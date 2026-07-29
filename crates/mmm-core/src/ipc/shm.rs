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

        // A job with 0 input slots and 0 output slots (a degenerate
        // test-only case; see `ipc::client::tests`) yields `total_bytes ==
        // 0`. macOS's `mmap` rejects a zero-length mapping with EINVAL, so
        // allocate (and map) a minimum of 1 byte that is never accessed —
        // `checked_range` below still bounds logical accesses against
        // `total_bytes`, not the allocation, so a zero-byte segment
        // continues to reject any nonzero-length `slice`/`slice_mut`.
        let alloc = total_bytes.max(1);

        if let Err(e) = nix::unistd::ftruncate(&fd, alloc as libc::off_t) {
            // We already created the (now zero-length) named object; leaving
            // it behind would only be cleaned up by a future `create` of the
            // same name unlinking it first. Unlink it now instead so a
            // failed `create` doesn't leak the shm object.
            let _ = nix::sys::mman::shm_unlink(name);
            return Err(Error::compute(format!(
                "ftruncate({name}, {alloc} bytes) failed: {e}"
            )));
        }

        let file = std::fs::File::from(fd);
        // Map an explicit length rather than the file's fstat-derived length:
        // macOS page-rounds a POSIX shm object's size up to the page size (16
        // KiB on Apple Silicon, vs. 4 KiB on Linux), so mapping "the whole
        // file" would give a mapping longer than `total_bytes` and drift the
        // segment's logical size (which `checked_range` bounds against) away
        // from what the caller asked for. An explicit length maps exactly
        // `alloc` (== `total_bytes`, except in the zero-byte case above) on
        // every platform.
        let map = unsafe {
            memmap2::MmapOptions::new()
                .len(alloc as usize)
                .map_mut(&file)
                .map_err(|e| Error::io(format!("shm:{name}"), e))?
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
    /// size. The underlying object must be *at least* `total_bytes` (on
    /// Linux the creator's `ftruncate` makes it exactly `total_bytes`; on
    /// macOS the OS page-rounds a shm object's size up, so it may be
    /// larger) — a too-small object is rejected rather than silently
    /// mapping a truncated segment. The mapping `attach` produces is always
    /// exactly `total_bytes`, regardless of the underlying object's actual
    /// (possibly page-rounded) size.
    pub fn attach(name: &str, total_bytes: u64) -> Result<ShmSegment> {
        use nix::fcntl::OFlag;
        use nix::sys::mman::shm_open;
        use nix::sys::stat::Mode;

        let fd = shm_open(name, OFlag::O_RDWR, Mode::S_IRUSR | Mode::S_IWUSR)
            .map_err(|e| Error::compute(format!("shm_open({name}) failed: {e}")))?;

        let file = std::fs::File::from(fd);

        // Validate the underlying object is large enough before mapping: an
        // exact-equality check here would fail spuriously on macOS, where
        // the creator's `ftruncate(total_bytes)` gets rounded up to the
        // page size by the OS.
        let actual = file
            .metadata()
            .map_err(|e| Error::io(format!("shm:{name}"), e))?
            .len();
        if actual < total_bytes {
            return Err(Error::compute(format!(
                "shm segment {name} is {actual} bytes, but attach expected at least {total_bytes}"
            )));
        }

        // See the matching comment in `create`: a degenerate 0-slot job
        // yields `total_bytes == 0`, and macOS's `mmap` rejects a
        // zero-length mapping, so map at least 1 byte (never accessed).
        let alloc = total_bytes.max(1);

        // Map an explicit length (see the matching comment in `create`) so
        // `self.map.len() == alloc` exactly, independent of the underlying
        // object's (possibly page-rounded) actual size.
        let map = unsafe {
            memmap2::MmapOptions::new()
                .len(alloc as usize)
                .map_mut(&file)
                .map_err(|e| Error::io(format!("shm:{name}"), e))?
        };

        Ok(ShmSegment {
            name: name.to_string(),
            is_creator: false,
            map,
        })
    }

    /// Bounds-check an `(offset_bytes, len_elements)` f32 slice request
    /// against the mapping, returning the validated byte range.
    ///
    /// Also enforces that `offset` is a multiple of `size_of::<f32>()`:
    /// both [`slice`](Self::slice) (via `bytemuck::cast_slice`) and
    /// [`slice_mut`](Self::slice_mut) (via a raw-pointer cast to `*mut
    /// f32`, in [`Self::slice_mut_raw`]) require a 4-byte-aligned start —
    /// for `slice_mut` a misaligned pointer would be immediate undefined
    /// behavior when the slice is constructed, not just on access, so this
    /// is the single chokepoint both paths go through to fail loudly
    /// instead.
    fn checked_range(&self, offset: u64, len: u64) -> Result<std::ops::Range<usize>> {
        if !offset.is_multiple_of(std::mem::size_of::<f32>() as u64) {
            return Err(Error::compute(format!(
                "slice offset {offset} is not a multiple of {} (f32 alignment)",
                std::mem::size_of::<f32>()
            )));
        }
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
    /// this delegates to after bounds- and alignment-checking. [`SlotLayout`]
    /// offsets are constructed so distinct slots never overlap.
    ///
    /// # Panics
    /// Panics if `[offset, offset + len * 4)` is out of bounds for the
    /// segment, or if `offset` is not a multiple of 4 (required for `f32`
    /// alignment).
    // This is the intentional public entry point for the interior-mutable
    // pattern described above and on `slice_mut_raw`'s `# Safety` section;
    // clippy's `mut_from_ref` lint fires on any `&self -> &mut` signature,
    // which is exactly the shared-memory shape this type needs.
    #[allow(clippy::mut_from_ref)]
    pub fn slice_mut(&self, offset: u64, len: u64) -> &mut [f32] {
        let range = self
            .checked_range(offset, len)
            .expect("ShmSegment::slice_mut out of bounds");
        // SAFETY: see `slice_mut_raw`'s `# Safety` section; `range` was just
        // bounds- and alignment-checked against the mapping above (`offset`
        // is a multiple of 4, so `range.start` is too).
        unsafe { self.slice_mut_raw(range.start, len as usize) }
    }

    /// Hand out a mutable f32 slice into the mapping from a shared
    /// (`&self`) reference.
    ///
    /// # Safety
    /// `MmapMut` normally requires `&mut self` to mutate. Shared memory
    /// exists precisely so a host and worker (or multiple threads in one
    /// process) can write disjoint slots of the *same* segment concurrently
    /// through shared references, so this bypasses that requirement via a
    /// raw pointer. Constructing a slice from a misaligned pointer is
    /// undefined behavior the moment the slice is built (not just on
    /// access), so the caller must guarantee:
    /// - `offset_bytes + len * 4 <= self.map.len()`, and
    /// - `offset_bytes` is a multiple of `size_of::<f32>()` (so
    ///   `base.add(offset_bytes) as *mut f32` is properly aligned — the
    ///   mapping's base address is page-aligned, so 4-byte alignment of the
    ///   base plus a 4-byte-aligned offset guarantees a 4-byte-aligned
    ///   result).
    ///
    /// Both of the above are established by `checked_range`, which every
    /// caller of this function (`slice_mut` above) runs first. The one
    /// invariant `checked_range` cannot check, and that remains the
    /// caller's obligation, is:
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

/// Windows named shared-memory segment (a pagefile-backed file mapping),
/// mirroring the POSIX [`ShmSegment`]: the host `create`s a named mapping and
/// the worker `attach`es to the same name. The name carried in the JSON `Init`
/// message is the POSIX-style `/name`; both sides normalize it identically to a
/// Windows object name (see [`win_object_name`]).
#[cfg(windows)]
#[derive(Debug)]
pub struct ShmSegment {
    name: String,
    is_creator: bool,
    handle: windows_sys::Win32::Foundation::HANDLE,
    base: *mut u8,
    size: usize,
}

// SAFETY: identical contract to the Unix impl (whose `MmapMut` is `Send+Sync`).
// The raw `base` pointer is only ever handed out as disjoint sub-slices via
// `SlotLayout`-derived offsets (see `slice_mut_raw`'s `# Safety`); the `HANDLE`
// is only closed on `Drop`. `HostLink` shares `ShmSegment` across its reader
// thread behind an `Arc`, so `Send + Sync` is required and upheld by that
// disjoint-access discipline.
#[cfg(windows)]
unsafe impl Send for ShmSegment {}
#[cfg(windows)]
unsafe impl Sync for ShmSegment {}

/// Normalize a POSIX-style shm name (`/mmm-foo`) to a Windows object name
/// (`Local\mmm-foo`). Applied identically here and in the C++ host so the
/// name string exchanged in the `Init` message round-trips.
#[cfg(windows)]
fn win_object_name(name: &str) -> Vec<u16> {
    let base = name.strip_prefix('/').unwrap_or(name);
    let full = format!("Local\\{base}");
    full.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
impl ShmSegment {
    /// Create a new named pagefile-backed segment of `total_bytes` and map it.
    pub fn create(name: &str, total_bytes: u64) -> Result<ShmSegment> {
        use windows_sys::Win32::Foundation::{GetLastError, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, FILE_MAP_ALL_ACCESS, MapViewOfFile, PAGE_READWRITE,
        };
        let wname = win_object_name(name);
        // A job with 0 input slots and 0 output slots (a degenerate
        // test-only case; see `ipc::client::tests`) yields `total_bytes ==
        // 0`. `CreateFileMappingW` rejects a zero-size mapping with
        // ERROR_INVALID_PARAMETER, so allocate (and map) a minimum of 1
        // byte that is never accessed — `self.size` stays `total_bytes`
        // (not `alloc`), so `checked_range` still rejects any nonzero
        // access to a logically zero-byte segment.
        let alloc = total_bytes.max(1);
        let hi = (alloc >> 32) as u32;
        let lo = (alloc & 0xFFFF_FFFF) as u32;
        // SAFETY: FFI; INVALID_HANDLE_VALUE requests a pagefile-backed mapping.
        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                std::ptr::null(),
                PAGE_READWRITE,
                hi,
                lo,
                wname.as_ptr(),
            )
        };
        if handle.is_null() {
            return Err(Error::compute(format!(
                "CreateFileMappingW({name}) failed: os error {}",
                unsafe { GetLastError() }
            )));
        }
        // SAFETY: FFI; map the whole allocation ([0, alloc)).
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, alloc as usize) };
        if view.Value.is_null() {
            let e = unsafe { GetLastError() };
            unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
            return Err(Error::compute(format!(
                "MapViewOfFile({name}) failed: os error {e}"
            )));
        }
        Ok(ShmSegment {
            name: name.to_string(),
            is_creator: true,
            handle,
            base: view.Value as *mut u8,
            size: total_bytes as usize,
        })
    }

    /// Attach to an existing named segment created by [`create`](Self::create).
    ///
    /// The mapping is opened by name and a view of `total_bytes` is mapped;
    /// requesting more than the creator allocated fails, so an oversized
    /// `total_bytes` is rejected. Unlike the POSIX impl, Windows offers no cheap
    /// exact-size assertion, so a *smaller* `total_bytes` within a larger
    /// segment is accepted — the creator's size is the contract.
    pub fn attach(name: &str, total_bytes: u64) -> Result<ShmSegment> {
        use windows_sys::Win32::Foundation::{CloseHandle, GetLastError};
        use windows_sys::Win32::System::Memory::{
            FILE_MAP_ALL_ACCESS, MapViewOfFile, OpenFileMappingW,
        };
        let wname = win_object_name(name);
        // SAFETY: FFI.
        let handle = unsafe { OpenFileMappingW(FILE_MAP_ALL_ACCESS, 0, wname.as_ptr()) };
        if handle.is_null() {
            return Err(Error::compute(format!(
                "OpenFileMappingW({name}) failed: os error {}",
                unsafe { GetLastError() }
            )));
        }
        // See the matching comment in `create`: a degenerate 0-slot job
        // yields `total_bytes == 0`, and `MapViewOfFile` rejects a
        // zero-size view, so map at least 1 byte (never accessed); `size`
        // (below) stays `total_bytes`, not `alloc`.
        let alloc = total_bytes.max(1);
        // SAFETY: FFI.
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, alloc as usize) };
        if view.Value.is_null() {
            let e = unsafe { GetLastError() };
            unsafe { CloseHandle(handle) };
            return Err(Error::compute(format!(
                "MapViewOfFile({name}, {total_bytes} bytes) failed: os error {e}"
            )));
        }
        Ok(ShmSegment {
            name: name.to_string(),
            is_creator: false,
            handle,
            base: view.Value as *mut u8,
            size: total_bytes as usize,
        })
    }

    /// Bounds/alignment-check an `(offset, len)` f32 request — identical logic
    /// to the Unix impl (see its doc); the single chokepoint for both `slice`
    /// and `slice_mut`.
    fn checked_range(&self, offset: u64, len: u64) -> Result<std::ops::Range<usize>> {
        if !offset.is_multiple_of(std::mem::size_of::<f32>() as u64) {
            return Err(Error::compute(format!(
                "slice offset {offset} is not a multiple of {} (f32 alignment)",
                std::mem::size_of::<f32>()
            )));
        }
        let byte_len = len.checked_mul(4).ok_or_else(|| {
            Error::compute(format!("slice len {len} (elements) overflows byte length"))
        })?;
        let end = offset.checked_add(byte_len).ok_or_else(|| {
            Error::compute(format!("slice offset {offset} + len {len} overflows"))
        })?;
        if end > self.size as u64 {
            return Err(Error::compute(format!(
                "slice range [{offset}, {end}) is out of bounds for segment of {} bytes",
                self.size
            )));
        }
        Ok(offset as usize..end as usize)
    }

    /// Read `len` f32 elements starting at byte `offset`. Panics if out of
    /// bounds or misaligned (see the Unix impl's doc for the contract).
    pub fn slice(&self, offset: u64, len: u64) -> &[f32] {
        let range = self
            .checked_range(offset, len)
            .expect("ShmSegment::slice out of bounds");
        // SAFETY: `range` is bounds/alignment-checked against the mapping; the
        // base pointer is page-aligned so a 4-byte-aligned offset stays aligned.
        unsafe {
            std::slice::from_raw_parts(self.base.add(range.start) as *const f32, len as usize)
        }
    }

    /// Write `len` f32 elements starting at byte `offset` from a shared
    /// reference — the interior-mutable shared-memory pattern; see the Unix
    /// impl's `# Safety`. Panics if out of bounds or misaligned.
    #[allow(clippy::mut_from_ref)]
    pub fn slice_mut(&self, offset: u64, len: u64) -> &mut [f32] {
        let range = self
            .checked_range(offset, len)
            .expect("ShmSegment::slice_mut out of bounds");
        // SAFETY: as `slice`, plus the caller's disjoint-range obligation
        // (upheld by `SlotLayout`), identical to the Unix `slice_mut_raw`.
        unsafe {
            std::slice::from_raw_parts_mut(self.base.add(range.start) as *mut f32, len as usize)
        }
    }
}

#[cfg(windows)]
impl Drop for ShmSegment {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Memory::{MEMORY_MAPPED_VIEW_ADDRESS, UnmapViewOfFile};
        // SAFETY: `base`/`handle` came from a successful map in create/attach.
        // A Windows file mapping is refcounted by open handles and vanishes when
        // the last handle closes — there is no `shm_unlink` analog, so both the
        // creator and attachers simply unmap + close. `is_creator` (and `name`)
        // are retained for symmetry with the Unix impl and for this debug trace;
        // they drive no special teardown.
        tracing::trace!(
            name = %self.name,
            is_creator = self.is_creator,
            "dropping ShmSegment"
        );
        unsafe {
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                Value: self.base as *mut core::ffi::c_void,
            });
            CloseHandle(self.handle);
        }
    }
}

/// A named shared-memory segment.
///
/// Shared memory is a POSIX/Windows-only transport; this platform stub
/// exists only so `mmm-core` compiles on other targets. Every operation
/// fails with [`Error::Compute`].
#[cfg(all(not(unix), not(windows)))]
#[derive(Debug)]
pub struct ShmSegment {
    _unused: (),
}

#[cfg(all(not(unix), not(windows)))]
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

#[cfg(all(test, windows))]
mod win_tests {
    use super::*;

    #[test]
    fn create_write_attach_read_same_bytes() {
        let name = format!("mmm-shm-wtest-{}", std::process::id());
        let total = 4096u64;
        let host = ShmSegment::create(&name, total).unwrap();
        host.slice_mut(0, 4).copy_from_slice(&[1.0, 2.0, 3.0, 4.0]);
        let worker = ShmSegment::attach(&name, total).unwrap();
        assert_eq!(worker.slice(0, 4), &[1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    #[should_panic(expected = "not a multiple of")]
    fn slice_mut_panics_on_misaligned_offset() {
        let name = format!("mmm-shm-wtest-align-{}", std::process::id());
        let host = ShmSegment::create(&name, 4096).unwrap();
        let _ = host.slice_mut(1, 1);
    }

    #[test]
    fn slice_roundtrip_at_aligned_offset() {
        let name = format!("mmm-shm-wtest-ok-{}", std::process::id());
        let host = ShmSegment::create(&name, 4096).unwrap();
        host.slice_mut(4, 2).copy_from_slice(&[5.0, 6.0]);
        assert_eq!(host.slice(4, 2), &[5.0, 6.0]);
    }

    #[test]
    fn zero_byte_segment_creates_and_slices_empty() {
        // A degenerate 0-input/0-output-slot job yields `total_bytes == 0`;
        // `CreateFileMappingW` rejects a zero-size mapping, so `create`
        // clamps its OS allocation to a minimum of 1 byte. The segment
        // still reports (and enforces) a logical size of 0.
        let name = format!("mmm-shm-wtest-zero-{}", std::process::id());
        let host = ShmSegment::create(&name, 0).unwrap();
        assert_eq!(host.slice(0, 0), &[] as &[f32]);
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
    fn misaligned_offset_is_rejected_by_checked_range() {
        let name = format!("/mmm-shm-test-align-{}", std::process::id());
        let host = ShmSegment::create(&name, 4096).unwrap();
        // `slice_mut`'s public signature is pinned to `-> &mut [f32]` (no
        // `Result`), so misalignment surfaces as a panic via `.expect()`
        // rather than a value the caller can match on; `checked_range` is
        // the private chokepoint both `slice` and `slice_mut` run through,
        // and is where the alignment invariant is actually enforced.
        assert!(host.checked_range(1, 1).is_err());
        assert!(host.checked_range(3, 1).is_err());
        // A 4-byte-aligned offset is accepted.
        assert!(host.checked_range(4, 1).is_ok());
    }

    #[test]
    #[should_panic(expected = "not a multiple of")]
    fn slice_mut_panics_on_misaligned_offset() {
        let name = format!("/mmm-shm-test-align-panic-{}", std::process::id());
        let host = ShmSegment::create(&name, 4096).unwrap();
        let _ = host.slice_mut(1, 1);
    }

    #[test]
    fn slice_and_slice_mut_work_at_a_valid_aligned_offset() {
        let name = format!("/mmm-shm-test-align-ok-{}", std::process::id());
        let host = ShmSegment::create(&name, 4096).unwrap();
        // Offset 4 (one f32 in) is 4-byte aligned and in bounds.
        let w = host.slice_mut(4, 2);
        w.copy_from_slice(&[5.0, 6.0]);
        assert_eq!(host.slice(4, 2), &[5.0, 6.0]);
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

    #[test]
    fn zero_byte_segment_creates_and_slices_empty() {
        // A degenerate 0-input/0-output-slot job yields `total_bytes == 0`;
        // macOS's `mmap` rejects a zero-length mapping, so `create` clamps
        // its OS allocation to a minimum of 1 byte. The segment still
        // reports (and enforces, via `checked_range`) a logical size of 0.
        let name = format!("/mmm-shm-test-zero-{}", std::process::id());
        let host = ShmSegment::create(&name, 0).unwrap();
        assert_eq!(host.slice(0, 0), &[] as &[f32]);
    }
}
