//! Named shared-memory segment management: creation, mapping, and the
//! fixed band-sized slot layout used to pass pixel data between processes.

use crate::{Error, Result};

/// Describes how a shared-memory segment is carved into fixed-size slots:
/// `input_slots` slots (filled by the host, read by the worker) followed by
/// `output_slots` slots (filled by the worker, read by the host).
///
/// Every slot is `slot_bytes` bytes, regardless of whether it holds an input
/// or output band; callers size `slot_bytes` for the largest band they will
/// ever transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotLayout {
    /// Size in bytes of a single slot.
    pub slot_bytes: u64,
    /// Number of input slots, placed at the start of the segment.
    pub input_slots: u32,
    /// Number of output slots, placed immediately after the input slots.
    pub output_slots: u32,
}

impl SlotLayout {
    /// Total size in bytes of the segment this layout describes: enough for
    /// every input slot plus every output slot.
    pub fn total_bytes(&self) -> u64 {
        self.slot_bytes * (self.input_slots as u64 + self.output_slots as u64)
    }

    /// Byte offset of input slot `slot` from the start of the segment.
    pub fn input_offset(&self, slot: u32) -> u64 {
        self.slot_bytes * slot as u64
    }

    /// Byte offset of output slot `slot` from the start of the segment.
    ///
    /// Output slots are laid out after all input slots.
    pub fn output_offset(&self, slot: u32) -> u64 {
        self.slot_bytes * self.input_slots as u64 + self.slot_bytes * slot as u64
    }
}

/// A named POSIX shared-memory segment mapped into this process.
///
/// The host process [`create`](ShmSegment::create)s the segment; worker
/// processes [`attach`](ShmSegment::attach) to the same name. Reads and
/// writes go through [`slice`](ShmSegment::slice) /
/// [`slice_mut`](ShmSegment::slice_mut), which index into the mapping by
/// byte offset (`offset`) and element count (`len`, in `f32`s); it is the
/// caller's responsibility — enforced in practice by [`SlotLayout`], which
/// assigns each slot a disjoint byte range — that concurrent accesses never
/// target overlapping ranges.
#[cfg(unix)]
#[derive(Debug)]
pub struct ShmSegment {
    name: String,
    is_creator: bool,
    map: memmap2::MmapMut,
}

#[cfg(unix)]
impl ShmSegment {
    /// Create a new shared-memory segment named `name`, sized
    /// `total_bytes`, and map it into this process.
    ///
    /// Any stale segment left behind under `name` (e.g. from a crashed
    /// previous run) is unlinked first so `create` never fails with
    /// "already exists".
    pub fn create(name: &str, total_bytes: u64) -> Result<ShmSegment> {
        use nix::errno::Errno;
        use nix::fcntl::OFlag;
        use nix::sys::mman::{shm_open, shm_unlink};
        use nix::sys::stat::Mode;

        match shm_unlink(name) {
            Ok(()) | Err(Errno::ENOENT) => {}
            Err(e) => {
                return Err(Error::compute(format!(
                    "shm_unlink({name}) failed while clearing a stale segment: {e}"
                )));
            }
        }

        let fd = shm_open(
            name,
            OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_RDWR,
            Mode::S_IRUSR | Mode::S_IWUSR,
        )
        .map_err(|e| Error::compute(format!("shm_open({name}) failed: {e}")))?;

        nix::unistd::ftruncate(&fd, total_bytes as libc::off_t).map_err(|e| {
            Error::compute(format!(
                "ftruncate({name}, {total_bytes} bytes) failed: {e}"
            ))
        })?;

        let file = std::fs::File::from(fd);
        let map = unsafe {
            memmap2::MmapMut::map_mut(&file).map_err(|e| Error::io(format!("shm:{name}"), e))?
        };

        Ok(ShmSegment {
            name: name.to_string(),
            is_creator: true,
            map,
        })
    }

    /// Attach to an existing shared-memory segment named `name`, sized
    /// `total_bytes`, and map it into this process.
    ///
    /// The segment must already exist (created by
    /// [`create`](ShmSegment::create) in another process); `attach` never
    /// creates, resizes, or unlinks it — only the creator owns the segment's
    /// size. `total_bytes` must match the size the creator established;
    /// mismatches are rejected rather than silently truncating someone
    /// else's mapping.
    pub fn attach(name: &str, total_bytes: u64) -> Result<ShmSegment> {
        use nix::fcntl::OFlag;
        use nix::sys::mman::shm_open;
        use nix::sys::stat::Mode;

        let fd = shm_open(name, OFlag::O_RDWR, Mode::S_IRUSR | Mode::S_IWUSR)
            .map_err(|e| Error::compute(format!("shm_open({name}) failed: {e}")))?;

        let file = std::fs::File::from(fd);
        let map = unsafe {
            memmap2::MmapMut::map_mut(&file).map_err(|e| Error::io(format!("shm:{name}"), e))?
        };

        if map.len() as u64 != total_bytes {
            return Err(Error::compute(format!(
                "shm segment {name} is {} bytes, but attach expected {total_bytes}",
                map.len()
            )));
        }

        Ok(ShmSegment {
            name: name.to_string(),
            is_creator: false,
            map,
        })
    }

    /// Bounds-check an `(offset_bytes, len_elements)` f32 slice request
    /// against the mapping, returning the validated byte range.
    fn checked_range(&self, offset: u64, len: u64) -> Result<std::ops::Range<usize>> {
        let byte_len = len.checked_mul(4).ok_or_else(|| {
            Error::compute(format!("slice len {len} (elements) overflows byte length"))
        })?;
        let end = offset.checked_add(byte_len).ok_or_else(|| {
            Error::compute(format!("slice offset {offset} + len {len} overflows"))
        })?;
        if end > self.map.len() as u64 {
            return Err(Error::compute(format!(
                "slice range [{offset}, {end}) is out of bounds for segment of {} bytes",
                self.map.len()
            )));
        }
        Ok(offset as usize..end as usize)
    }

    /// Read `len` f32 elements starting at byte `offset` into the mapping.
    ///
    /// # Panics
    /// Panics if `[offset, offset + len * 4)` is out of bounds for the
    /// segment. Use [`SlotLayout`] to compute valid, non-overlapping
    /// offsets.
    pub fn slice(&self, offset: u64, len: u64) -> &[f32] {
        let range = self
            .checked_range(offset, len)
            .expect("ShmSegment::slice out of bounds");
        bytemuck::cast_slice(&self.map[range])
    }

    /// Write `len` f32 elements starting at byte `offset` into the mapping.
    ///
    /// Multiple callers may hold `&ShmSegment` and call `slice_mut`
    /// concurrently as long as their `(offset, len)` byte ranges are
    /// disjoint — see the `# Safety` note on the private `slice_mut_raw`, which
    /// this delegates to after bounds-checking. [`SlotLayout`] offsets are
    /// constructed so distinct slots never overlap.
    ///
    /// # Panics
    /// Panics if `[offset, offset + len * 4)` is out of bounds for the
    /// segment.
    // This is the intentional public entry point for the interior-mutable
    // pattern described above and on `slice_mut_raw`'s `# Safety` section;
    // clippy's `mut_from_ref` lint fires on any `&self -> &mut` signature,
    // which is exactly the shared-memory shape this type needs.
    #[allow(clippy::mut_from_ref)]
    pub fn slice_mut(&self, offset: u64, len: u64) -> &mut [f32] {
        let range = self
            .checked_range(offset, len)
            .expect("ShmSegment::slice_mut out of bounds");
        let start = range.start;
        let elems = range.len() / 4;
        // SAFETY: see `slice_mut_raw`'s `# Safety` section; `range` was just
        // bounds-checked against the mapping above.
        unsafe { self.slice_mut_raw(start, elems) }
    }

    /// Hand out a mutable f32 slice into the mapping from a shared
    /// (`&self`) reference.
    ///
    /// # Safety
    /// `MmapMut` normally requires `&mut self` to mutate. Shared memory
    /// exists precisely so a host and worker (or multiple threads in one
    /// process) can write disjoint slots of the *same* segment concurrently
    /// through shared references, so this bypasses that requirement via a
    /// raw pointer. The caller must guarantee:
    /// - `offset_bytes + len * 4 <= self.map.len()` (both public callers
    ///   above check this via `checked_range` before reaching here), and
    /// - the byte range `[offset_bytes, offset_bytes + len * 4)` does not
    ///   overlap any other `slice`/`slice_mut` byte range in concurrent use
    ///   (guaranteed by callers using [`SlotLayout`]-derived offsets, which
    ///   partition the segment into disjoint slots).
    #[allow(clippy::mut_from_ref)]
    unsafe fn slice_mut_raw(&self, offset_bytes: usize, len: usize) -> &mut [f32] {
        let base = self.map.as_ptr() as *mut u8;
        unsafe { std::slice::from_raw_parts_mut(base.add(offset_bytes) as *mut f32, len) }
    }
}

#[cfg(unix)]
impl Drop for ShmSegment {
    fn drop(&mut self) {
        // The `MmapMut` field unmaps itself when it is dropped right after
        // this. Only the creator removes the underlying named object so
        // that an attached worker's drop doesn't yank the segment out from
        // under a still-running host (or a second worker).
        if self.is_creator {
            let _ = nix::sys::mman::shm_unlink(self.name.as_str());
        }
    }
}

/// A named shared-memory segment.
///
/// Shared memory is a POSIX-only transport; this platform stub exists only
/// so `mmm-core` compiles on non-Unix targets. Every operation fails with
/// [`Error::Compute`].
#[cfg(not(unix))]
#[derive(Debug)]
pub struct ShmSegment {
    _unused: (),
}

#[cfg(not(unix))]
impl ShmSegment {
    /// Always fails: shared memory is not supported on this platform.
    pub fn create(_name: &str, _total_bytes: u64) -> Result<ShmSegment> {
        Err(Error::compute(
            "shared memory is not yet supported on this platform",
        ))
    }

    /// Always fails: shared memory is not supported on this platform.
    pub fn attach(_name: &str, _total_bytes: u64) -> Result<ShmSegment> {
        Err(Error::compute(
            "shared memory is not yet supported on this platform",
        ))
    }

    /// Unreachable: no `ShmSegment` can be constructed on this platform.
    pub fn slice(&self, _offset: u64, _len: u64) -> &[f32] {
        unreachable!("ShmSegment cannot be constructed on non-unix platforms")
    }

    /// Unreachable: no `ShmSegment` can be constructed on this platform.
    #[allow(clippy::mut_from_ref)]
    pub fn slice_mut(&self, _offset: u64, _len: u64) -> &mut [f32] {
        unreachable!("ShmSegment cannot be constructed on non-unix platforms")
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn layout_offsets_are_packed_and_non_overlapping() {
        let l = SlotLayout {
            slot_bytes: 4096,
            input_slots: 3,
            output_slots: 2,
        };
        assert_eq!(l.total_bytes(), 4096 * 5);
        assert_eq!(l.input_offset(0), 0);
        assert_eq!(l.input_offset(2), 8192);
        assert_eq!(l.output_offset(0), 4096 * 3);
        assert_eq!(l.output_offset(1), 4096 * 4);
    }

    #[test]
    fn create_write_attach_read_same_bytes() {
        let name = format!("/mmm-shm-test-{}", std::process::id());
        let total = 4096u64;
        let host = ShmSegment::create(&name, total).unwrap();
        let w = host.slice_mut(0, 4);
        w.copy_from_slice(&[1.0, 2.0, 3.0, 4.0]);
        let worker = ShmSegment::attach(&name, total).unwrap();
        assert_eq!(worker.slice(0, 4), &[1.0, 2.0, 3.0, 4.0]);
        // Cleanup happens on host drop (unlink); attach drop just unmaps.
    }
}
