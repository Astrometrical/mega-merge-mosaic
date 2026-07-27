# PixInsight IPC Transport + mmm-ipc-worker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the Rust half of the PixInsight integration — a shared-memory, pull-based IPC transport and a standalone `mmm-ipc-worker` binary that drives mmm-core's analyze→blend over that transport, verified byte-identical to the file-based path by a Rust mock host (no PixInsight needed).

**Architecture:** A host process (the C++ PCL module in Plan 2; a Rust mock host in tests) spawns `mmm-ipc-worker` and talks to it over the worker's stdin/stdout (a tagged message frame protocol) plus a named POSIX shared-memory segment carved into fixed band-sized slots. Pixel bands are pulled on demand: a worker thread requests a band, the host memcpy's it into a free slot and replies, the worker copies it into a per-thread buffer and frees the slot. A new `PanelReader::Ipc` backing makes this invisible to `analyze`/`blend`, which reach panel pixels only through an injected `PanelSource`.

**Tech Stack:** Rust (edition matching the workspace), `memmap2` (already a dep) for mapping shm fds, `nix` (new, unix-only) for `shm_open`/`ftruncate`/`shm_unlink`, `serde`/`serde_json` (already deps) for the JSON handshake, `rayon` (already a dep) for the existing parallelism, `bytemuck` (already a dep) for zero-copy f32/byte casts.

## Global Constraints

- Linear data end-to-end; zero = no-data sentinel (a pixel is covered iff all channels are nonzero). Copied verbatim from CLAUDE.md.
- Every public item in `mmm-core` needs a doc comment (`missing_docs` is warned). Keep `cargo fmt`, `cargo clippy --all-targets`, and `cargo doc` warning-free.
- Tests must not depend on `test_data/` — synthesize inputs via the `synth` module.
- `blend.rs`'s unit tests live in the sibling `blend_tests.rs` (a `#[path]` child module); add blend-related unit tests there, not inline.
- Per-channel math for OSC; the existing algorithm code (`analyze` scan loop, `blend`, `seam`, `pyramid`) must not change behaviour — IPC is a new *input/output path*, proven by byte-identical output.
- POSIX shm (Linux/macOS) is implemented and tested here; Windows shm is a documented follow-up in Plan 2 (the `shm` module isolates the platform code behind one interface).
- Protocol version constant `IPC_PROTOCOL_VERSION: u32 = 1`; a handshake mismatch is a hard error.
- All f32 pixel data crossing shm is native-endian planar (`channels × rows × width`, row-major, top-down) — the same layout `PanelReader`/`RowSink` already use.

---

## File Structure

**New crate `crates/mmm-ipc-worker/`:**
- `Cargo.toml` — binary crate depending on `mmm-core`.
- `src/main.rs` — parse the `Init` handshake, build the transport, run analyze→blend into a `ShmRowSink`, translate errors/cancellation to protocol messages. `panic = "abort"` in profile.

**New module tree `crates/mmm-core/src/ipc/`:**
- `mod.rs` — module docs + re-exports; the shared `IPC_PROTOCOL_VERSION`.
- `protocol.rs` — `HostMsg`/`WorkerMsg` enums, the tagged frame codec (`write_frame`/`read_frame`), and the fixed band-request/reply structs.
- `shm.rs` — `ShmSegment` (create/attach/unlink) and `SlotLayout` (offset math); POSIX impl behind `#[cfg(unix)]`.
- `client.rs` — `HostLink`: the worker-side client owning the shm attach, a stdin-reader/demux thread, the slot pool, and the `request_band`/`send_progress`/`is_cancelled`/output-slot API.
- `reader.rs` — `IpcBacking`: per-thread band buffers + the `row` implementation, wired into `PanelReader` as a new backing.
- `sink.rs` — `ShmRowSink`: a `RowSink` that streams bands back through output slots.
- `source.rs` — the `PanelSource` trait, `FileSource` (default), and `IpcSource`.
- `testhost.rs` — `#[cfg(test)]`-usable mock host used by client/reader/end-to-end tests (also compiled for integration tests via a small public test hook).

**Modified files:**
- `crates/mmm-core/src/lib.rs` — `pub mod ipc;`.
- `crates/mmm-core/src/panel_reader.rs` — add `Backing::Ipc(IpcBacking)` + `PanelReader::open_ipc(...)`; `PanelStorage::Ipc { panel_id }`.
- `crates/mmm-core/src/blend.rs` — route `open_readers` through a `&dyn PanelSource`; add `blend_with_source`, keep `blend` delegating with `FileSource`.
- `crates/mmm-core/src/analyze.rs` — add `analyze_source(...)` taking a `&dyn PanelSource`; existing `analyze_input` delegates with `FileSource`.
- `crates/mmm-core/Cargo.toml` — add `nix` (unix-only) and `libc` if needed.
- `Cargo.toml` (workspace) — add the new crate to `members`.

---

## Task 1: Workspace wiring + `ipc` module skeleton

**Files:**
- Modify: `Cargo.toml` (workspace `members`)
- Create: `crates/mmm-ipc-worker/Cargo.toml`
- Create: `crates/mmm-ipc-worker/src/main.rs`
- Create: `crates/mmm-core/src/ipc/mod.rs`
- Modify: `crates/mmm-core/src/lib.rs`
- Modify: `crates/mmm-core/Cargo.toml`

**Interfaces:**
- Produces: `mmm_core::ipc` module; `mmm_core::ipc::IPC_PROTOCOL_VERSION: u32`.

- [ ] **Step 1: Add the crate to the workspace and declare deps**

In workspace `Cargo.toml`, add `"crates/mmm-ipc-worker"` to `members`.

`crates/mmm-ipc-worker/Cargo.toml`:
```toml
[package]
name = "mmm-ipc-worker"
version = "0.1.0"
edition = "2021"

[dependencies]
mmm-core = { path = "../mmm-core" }
serde_json = "1"

[profile.release]
panic = "abort"
```

In `crates/mmm-core/Cargo.toml` add (below existing deps):
```toml
[target.'cfg(unix)'.dependencies]
nix = { version = "0.29", features = ["mman", "fs"] }
libc = "0.2"
```

- [ ] **Step 2: Create the module skeleton**

`crates/mmm-core/src/ipc/mod.rs`:
```rust
//! Shared-memory IPC transport for driving the pipeline from a host process
//! (the PixInsight PCL module) without writing panel pixels to disk.
//!
//! The host spawns `mmm-ipc-worker` and talks to it over the worker's
//! stdin/stdout ([`protocol`]) plus a named shared-memory segment ([`shm`])
//! carved into fixed band-sized slots. Panel pixels are pulled on demand: a
//! worker thread asks for a band, the host fills a slot and replies, the
//! worker copies the band into a per-thread buffer ([`reader`]) and frees the
//! slot. Blended output streams back the same way ([`sink`]). The pipeline is
//! reached through a [`source::PanelSource`], so `analyze`/`blend` never learn
//! whether pixels came from a file or the host.
pub mod client;
pub mod protocol;
pub mod reader;
pub mod shm;
pub mod sink;
pub mod source;

#[cfg(test)]
pub mod testhost;

/// Wire-protocol version exchanged in the init handshake; a mismatch aborts
/// the run rather than risking a misinterpretation of later frames.
pub const IPC_PROTOCOL_VERSION: u32 = 1;
```

Add `pub mod ipc;` to `crates/mmm-core/src/lib.rs` (after `pub mod formats;`, keeping alphabetical order). Add empty `pub` items or `mod` stubs as needed so the tree compiles: for now create each submodule file with only a `//! ...` doc line and a `#![allow(unused)]`-free minimal body. To keep it compiling before later tasks fill them, give each a doc comment and no items.

- [ ] **Step 3: Verify it compiles**

Run: `source ~/.cargo/env; cargo build -p mmm-core -p mmm-ipc-worker`
Expected: builds (the worker `main` can be an empty `fn main() {}` for now).

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml crates/mmm-ipc-worker crates/mmm-core/Cargo.toml crates/mmm-core/src/lib.rs crates/mmm-core/src/ipc
git commit -m "chore: scaffold mmm-ipc-worker crate and mmm-core::ipc module"
```

---

## Task 2: Frame protocol (`ipc/protocol.rs`)

**Files:**
- Modify: `crates/mmm-core/src/ipc/protocol.rs`
- Test: inline `#[cfg(test)]` in the same file.

**Interfaces:**
- Produces:
  - `enum WorkerMsg { BandRequest(BandRequest), Progress{stage:String,done:u64,total:u64}, Begin{w:u64,h:u64,ch:u64}, OutputBand(OutputBand), Done, Error{message:String} }`
  - `enum HostMsg { Init(InitJob), BandReply(BandReply), OutputAck{request_id:u32}, Cancel }`
  - `struct BandRequest { request_id:u32, panel_id:u32, y0:u64, y1:u64, slot_id:u32 }`
  - `struct BandReply { request_id:u32, slot_id:u32, status:u8 }` (`status` 0=ok, 1=error)
  - `struct OutputBand { request_id:u32, y0:u64, rows:u64, slot_id:u32 }`
  - `struct InitJob { protocol_version:u32, shm_name:String, slot_bytes:u64, input_slots:u32, output_slots:u32, canvas:[u64;3], panels:Vec<PanelDesc>, mode:JobMode, session_dir:String, params:BlendParamsWire }` (serde JSON)
  - `struct PanelDesc { panel_id:u32, width:u64, height:u64, channels:u64, properties:Vec<mmm_core::formats::XisfProperty> }` (`properties` empty except solved mode)
  - `enum JobMode { Aligned, Solved, Files{paths:Vec<String>} }`
  - `struct BlendParamsWire { feather_px:f32, downsample:u32, band_rows:u32, mode:String, roi:Option<[u64;4]>, defect_veto:bool, flatten:Option<u32>, surface_order:Option<u32> }` with `fn to_params(&self)->mmm_core::blend::BlendParams`.
  - `fn write_frame<W:Write>(w:&mut W, msg:&impl FrameBody)->io::Result<()>`
  - `fn read_worker_frame<R:Read>(r:&mut R)->io::Result<Option<WorkerMsg>>` and `read_host_frame` (None on clean EOF).

- [ ] **Step 1: Write failing tests for the framing round-trip**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_request_round_trips_through_a_pipe_buffer() {
        let req = BandRequest { request_id: 7, panel_id: 3, y0: 256, y1: 512, slot_id: 2 };
        let mut buf = Vec::new();
        write_frame(&mut buf, &WorkerMsg::BandRequest(req.clone())).unwrap();
        let mut cur = std::io::Cursor::new(buf);
        match read_worker_frame(&mut cur).unwrap().unwrap() {
            WorkerMsg::BandRequest(got) => assert_eq!(got, req),
            other => panic!("wrong variant: {other:?}"),
        }
        // A second read on the exhausted cursor is a clean EOF.
        assert!(read_worker_frame(&mut cur).unwrap().is_none());
    }

    #[test]
    fn init_job_json_round_trips() {
        let job = InitJob {
            protocol_version: crate::ipc::IPC_PROTOCOL_VERSION,
            shm_name: "/mmm-test".into(),
            slot_bytes: 1 << 20,
            input_slots: 8,
            output_slots: 2,
            canvas: [100, 80, 3],
            panels: vec![PanelDesc { panel_id: 0, width: 100, height: 80, channels: 3, properties: vec![] }],
            mode: JobMode::Aligned,
            session_dir: "/tmp/x.mmm-session".into(),
            params: BlendParamsWire::default(),
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &HostMsg::Init(job.clone())).unwrap();
        let mut cur = std::io::Cursor::new(buf);
        match read_host_frame(&mut cur).unwrap().unwrap() {
            HostMsg::Init(got) => assert_eq!(got, job),
            other => panic!("wrong variant: {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p mmm-core ipc::protocol::tests -- --nocapture`
Expected: FAIL (types/functions not defined).

- [ ] **Step 3: Implement the protocol**

Frame layout: `[u8 tag][u32 le payload_len][payload bytes]`. Band frames use a fixed little-endian binary payload; all other frames use a JSON payload (serde_json). Tags: `1=BandRequest, 2=Progress, 3=Begin, 4=OutputBand, 5=Done, 6=Error` (worker→host); `128=Init, 129=BandReply, 130=OutputAck, 131=Cancel` (host→worker). Implement `#[derive(Serialize,Deserialize,Clone,PartialEq,Debug)]` on all types; give `BlendParamsWire` a `Default` matching `blend::BlendParams::default()` and a `to_params`. `write_frame` matches the message to a `(tag, payload)`; band structs encode via explicit `to_le_bytes`; JSON messages via `serde_json::to_vec`. `read_*_frame` reads the tag (returning `Ok(None)` on EOF before any byte), then the length, then dispatches by tag. Keep `FrameBody` a small sealed trait implemented for `WorkerMsg` and `HostMsg` exposing `fn encode(&self)->(u8, Vec<u8>)`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p mmm-core ipc::protocol::tests`
Expected: PASS.

- [ ] **Step 5: Doc + lint + commit**

Run: `cargo doc -p mmm-core --no-deps 2>&1 | grep -i warning; cargo clippy -p mmm-core --all-targets`
Expected: no warnings.
```bash
git add crates/mmm-core/src/ipc/protocol.rs
git commit -m "feat(ipc): tagged frame protocol with binary band frames + JSON control"
```

---

## Task 3: Shared-memory segment (`ipc/shm.rs`)

**Files:**
- Modify: `crates/mmm-core/src/ipc/shm.rs`
- Test: inline `#[cfg(test)]`.

**Interfaces:**
- Produces:
  - `struct SlotLayout { slot_bytes:u64, input_slots:u32, output_slots:u32 }` with `fn total_bytes(&self)->u64`, `fn input_offset(&self,slot:u32)->u64`, `fn output_offset(&self,slot:u32)->u64`.
  - `struct ShmSegment { /* owns the mapping */ }` with:
    - `fn create(name:&str, total_bytes:u64)->Result<ShmSegment>` (host side; unlinks any stale name first)
    - `fn attach(name:&str, total_bytes:u64)->Result<ShmSegment>` (worker side)
    - `fn slice(&self, offset:u64, len:u64)->&[f32]` and `fn slice_mut(&self, offset:u64, len:u64)->&mut [f32]` (interior-mutable; disjoint-offset invariant is the caller's — documented `# Safety` on a private helper)
    - `impl Drop` unlinks on the creator only.

- [ ] **Step 1: Write failing tests (Linux/macOS)**

```rust
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn layout_offsets_are_packed_and_non_overlapping() {
        let l = SlotLayout { slot_bytes: 4096, input_slots: 3, output_slots: 2 };
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
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p mmm-core ipc::shm::tests`
Expected: FAIL (undefined).

- [ ] **Step 3: Implement POSIX shm**

`create`: `nix::sys::mman::shm_unlink(name)` (ignore `ENOENT`), then `shm_open(name, O_CREAT|O_EXCL|O_RDWR, 0600)`, `nix::unistd::ftruncate(fd, total as off_t)`, wrap the `OwnedFd` in a `std::fs::File` (`File::from(owned_fd)`), `unsafe { memmap2::MmapMut::map_mut(&file) }`. Store `name`, `is_creator=true`, the `MmapMut`. `attach`: `shm_open(name, O_RDWR, 0600)` then map; `is_creator=false`. `slice`/`slice_mut`: `bytemuck::cast_slice(&self.map[off..off+len*4])` — expose `slice_mut` via a private `unsafe fn map_mut_ptr` and `#[allow(clippy::mut_from_ref)]` with a `# Safety` note that callers guarantee disjoint offsets per concurrent access. `Drop`: unmap (automatic), and if `is_creator` call `shm_unlink(name)`. Gate the whole impl with `#[cfg(unix)]`; add a `#[cfg(not(unix))]` stub whose `create`/`attach` return `Err(Error::compute("shared memory is not yet supported on this platform"))` so the crate still compiles on Windows.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p mmm-core ipc::shm::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mmm-core/src/ipc/shm.rs
git commit -m "feat(ipc): POSIX shared-memory segment with packed slot layout"
```

---

## Task 4: Worker-side host link (`ipc/client.rs`)

**Files:**
- Modify: `crates/mmm-core/src/ipc/client.rs`
- Create: `crates/mmm-core/src/ipc/testhost.rs` (the mock host used from here on)
- Test: inline `#[cfg(test)]` in `client.rs`.

**Interfaces:**
- Consumes: everything from Tasks 2–3.
- Produces:
  - `struct HostLink` (`Send + Sync`) built by `HostLink::start(init:InitJob, input:Box<dyn Read+Send>, output:Box<dyn Write+Send>)->Result<Arc<HostLink>>`.
  - `fn request_band(&self, panel_id:u32, y0:u64, y1:u64, dst:&mut [f32])->Result<()>` — blocks; copies the served band into `dst` (length must equal `channels*(y1-y0)*width`), frees the slot.
  - `fn send_progress(&self, stage:&str, done:u64, total:u64)`
  - `fn is_cancelled(&self)->bool`
  - `fn begin_output(&self, w:u64, h:u64, ch:u64)->Result<()>`
  - `fn send_output_band(&self, y0:u64, rows:u64, planar:&[f32])->Result<()>` — copies into a free output slot, sends `OutputBand`, waits `OutputAck`.
  - `fn finish_ok(&self)->Result<()>` / `fn finish_err(&self, msg:&str)`
  - `fn canvas(&self)->[u64;3]`, `fn panels(&self)->&[PanelDesc]`, `fn mode(&self)->&JobMode`, `fn slot_layout(&self)->SlotLayout`.
- `testhost.rs` Produces: `struct MockHost` with `fn spawn(job, panel_pixels: Vec<Vec<f32>>)->(HostSide, Box<dyn Read+Send>, Box<dyn Write+Send>)` serving bands from in-memory planar panels on its own thread, collecting `Begin`/`OutputBand` into a result buffer, and honouring a cancel flag; used by Tasks 4/5/6 and the end-to-end test.

- [ ] **Step 1: Write the failing concurrency test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::testhost::MockHost;
    use rayon::prelude::*;

    #[test]
    fn concurrent_band_requests_return_correct_pixels() {
        // Two panels, 8×16×1, value = panel*1000 + y*10 + x.
        let (w, h, ch) = (8u64, 16u64, 1u64);
        let mk = |p: u64| (0..h).flat_map(|y| (0..w).map(move |x| (p*1000 + y*10 + x) as f32)).collect::<Vec<_>>();
        let pixels = vec![mk(0), mk(1)];
        let job = MockHost::aligned_job(w, h, ch, /*panels*/2, /*input_slots*/4, /*slot_bytes*/ (w*8*4));
        let (host, r, wr) = MockHost::spawn(job.clone(), pixels.clone());
        let link = HostLink::start(job, r, wr).unwrap();

        // Hammer request_band from many rayon threads over both panels/bands.
        (0..2u32).into_par_iter().for_each(|panel| {
            for y0 in (0..h).step_by(8) {
                let y1 = (y0 + 8).min(h);
                let mut dst = vec![0f32; ((y1 - y0) * w) as usize];
                link.request_band(panel, y0, y1, &mut dst).unwrap();
                for (i, v) in dst.iter().enumerate() {
                    let yy = y0 + (i as u64) / w;
                    let xx = (i as u64) % w;
                    assert_eq!(*v, (panel as u64 * 1000 + yy*10 + xx) as f32);
                }
            }
        });
        link.finish_ok().unwrap();
        host.join();
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p mmm-core ipc::client::tests::concurrent_band_requests_return_correct_pixels`
Expected: FAIL (undefined).

- [ ] **Step 3: Implement `HostLink`**

State: `Arc` inner holding `ShmSegment`, `SlotLayout`, a `Mutex<Box<dyn Write>>` for the outbound pipe, an atomic `next_request_id`, a `Mutex<HashMap<u32, Arc<ReplySlot>>>` of pending requests (each `ReplySlot` = `Mutex<Option<Reply>>` + `Condvar`), a `Semaphore`-like free-slot pool for input slots and another for output slots (implement with `Mutex<Vec<u32>>` + `Condvar`), an `AtomicBool cancelled`, and the parsed `InitJob`. `start` spawns a reader thread that loops `read_host_frame`; on `BandReply`/`OutputAck` it stores the reply and notifies the matching `ReplySlot`; on `Cancel` it sets `cancelled`. `request_band`: acquire a free input slot id, allocate a request id + register a `ReplySlot`, `write_frame(BandRequest{...})`, wait on the condvar for the reply, on ok `dst.copy_from_slice(seg.slice(input_offset(slot), dst.len()))`, release the slot, return. `send_output_band`: acquire a free output slot, `seg.slice_mut(output_offset(slot), planar.len()).copy_from_slice(planar)`, send `OutputBand`, wait for `OutputAck`, release. Errors (reply status=1, or pipe closed → reader thread records a shutdown) surface as `Error::compute(...)`.

- [ ] **Step 4: Implement `MockHost` in testhost.rs**

`spawn` creates an in-process byte pipe pair for each direction (use `std::sync::mpsc`-backed `Read`/`Write` shims, or `os_pipe`; prefer a simple in-memory `PipeReader/PipeWriter` built on `Arc<Mutex<VecDeque<u8>>>+Condvar` to avoid a new dep). It also `ShmSegment::attach`es the same name the job carries (host uses `create`; test asserts the worker also attaches — for the in-process test the MockHost is the *creator*, so `MockHost::spawn` calls `ShmSegment::create` and the `HostLink` calls `attach`). The host thread loops reading `WorkerMsg`; on `BandRequest` it copies the requested rows of the named panel's planar buffer into `input_offset(slot)` and replies `BandReply{status:0}`; on `Begin` it records geometry; on `OutputBand` it copies the slot out into a result vector and replies `OutputAck`; on `Done`/`Error` it stops. `aligned_job` builds a matching `InitJob`. Expose `HostSide::join()` and `HostSide::result()->(geom, Vec<f32>)`.

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p mmm-core ipc::client::tests`
Expected: PASS. Also run with `--test-threads=1` and default to shake out races.

- [ ] **Step 6: Commit**

```bash
git add crates/mmm-core/src/ipc/client.rs crates/mmm-core/src/ipc/testhost.rs
git commit -m "feat(ipc): worker-side HostLink with request multiplexing + mock host"
```

---

## Task 5: `IpcBacking` panel reader (`ipc/reader.rs` + panel_reader.rs)

**Files:**
- Modify: `crates/mmm-core/src/ipc/reader.rs`
- Modify: `crates/mmm-core/src/panel_reader.rs` (new `Backing::Ipc`, `open_ipc`, `PanelStorage::Ipc`)
- Test: inline `#[cfg(test)]` in `reader.rs`.

**Interfaces:**
- Consumes: `HostLink` (Task 4).
- Produces:
  - `struct IpcBacking` with `fn new(link:Arc<HostLink>, panel_id:u32, canvas:(u64,u64,u64), band_rows:usize)->IpcBacking` and `fn row(&self, c:u64, canvas_y:u64)->Option<(u64,&[f32])>`.
  - `PanelReader::open_ipc(link:Arc<HostLink>, panel_id:u32, canvas:(u64,u64,u64), band_rows:usize)->PanelReader`.
  - `PanelStorage::Ipc { panel_id: u32 }` (serde-tagged; never persisted for real runs — sessions from IPC keep pixel access transient, so blend re-derives it from the source, see Task 8).

- [ ] **Step 1: Write the failing test — IPC reader matches a synth XISF byte-for-byte under rayon**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::client::HostLink;
    use crate::ipc::testhost::MockHost;
    use crate::panel_reader::PanelReader;
    use crate::synth::write_xisf;
    use crate::formats::xisf::XisfPanel;
    use rayon::prelude::*;

    #[test]
    fn ipc_rows_match_the_source_under_concurrent_access() {
        let (w, h, ch) = (37u64, 91u64, 3u64); // non-multiples of band_rows on purpose
        let planes: Vec<f32> = (0..w*h*ch).map(|i| (i as f32) * 0.5 + 1.0).collect();
        let dir = std::env::temp_dir().join(format!("mmm-ipc-reader-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("p.xisf");
        write_xisf(&path, w, h, ch, &planes).unwrap();
        let src = XisfPanel::open(&path).unwrap();

        let job = MockHost::aligned_job(w, h, ch, 1, 8, w*ch*32*4);
        let (host, r, wr) = MockHost::spawn(job.clone(), vec![planes.clone()]);
        let link = HostLink::start(job, r, wr).unwrap();
        let reader = PanelReader::open_ipc(link.clone(), 0, (w, h, ch), 32);

        (0..h).into_par_iter().for_each(|y| {
            for c in 0..ch {
                let (x0, got) = reader.row(c, y).unwrap();
                assert_eq!(x0, 0);
                assert_eq!(got, src.row(c, y), "mismatch at c={c} y={y}");
            }
        });
        link.finish_ok().unwrap();
        host.join();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p mmm-core ipc::reader::tests`
Expected: FAIL.

- [ ] **Step 3: Implement `IpcBacking` with per-thread band buffers**

Fields: `link: Arc<HostLink>`, `panel_id`, `canvas:(w,h,ch)`, `band_rows`, and `cells: Vec<UnsafeCell<ThreadBand>>` sized `rayon::current_num_threads() + 1` (index `num_threads` is the fallback for calls outside any rayon pool). `ThreadBand { y0: u64, valid: bool, buf: Vec<f32> }` where `buf` holds `ch*band_rows*w` f32. `row(c, canvas_y)`: pick `idx = rayon::current_thread_index().unwrap_or(num_threads)`; `let band = unsafe { &mut *cells[idx].get() }`; compute `by0 = (canvas_y / band_rows) * band_rows`; if `!band.valid || band.y0 != by0`, fetch: `by1 = (by0 + band_rows).min(h)`; `link.request_band(panel_id, by0, by1, &mut band.buf[..(ch*(by1-by0)*w) as usize])?` (on error, panic is unacceptable — but `row` returns `Option`; store the error into the link's failure latch and return `None`… instead: make fetch failures set a `poisoned` flag on the band and the reader exposes `fn take_error()`; blend/analyze already propagate the first `None`-driven emptiness. Simpler and honest: fetch errors are turned into a process-level abort via `link.finish_err` + `std::process::abort()` is wrong under test. Use: cache the `Result` — `row` stays `Option`, and store the last transport error in an `Arc<Mutex<Option<Error>>>` on the backing that `PanelReader` exposes via `fn ipc_error(&self)`; Task 8/9 check it after each stage.) Set `band.y0=by0; band.valid=true`. Return `Some((0, &band.buf[slice for channel c, row (canvas_y-by0)]))`. Add `unsafe impl Sync for IpcBacking {}` with a SAFETY comment: each thread only ever dereferences its own `cells[idx]`, so the `&mut` aliases are disjoint; the returned `&[f32]` borrows the cell and remains valid until the same thread's next `row` call, matching how `analyze`/`blend` consume rows (all channels of one row, then advance).

- [ ] **Step 4: Wire into `PanelReader`**

Add `Ipc(IpcBacking)` to `enum Backing`; in `row`, add the arm delegating to `IpcBacking::row`. Add `PanelReader::open_ipc(...)` constructing `PanelReader { backing: Backing::Ipc(IpcBacking::new(...)), bbox:[0,0,w,h], canvas }`. In `advise_sequential`, the `Ipc` arm is a no-op. Add `PanelStorage::Ipc { panel_id: u32 }` and make `PanelReader::open` reject it (`Error::compute("IPC panels are opened via open_ipc, not open")`) so the enum is exhaustive and misuse is loud.

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p mmm-core ipc::reader::tests`
Expected: PASS.

- [ ] **Step 6: Miri-style race check (best effort) + lint + commit**

Run: `cargo test -p mmm-core ipc::reader::tests -- --test-threads=8` (a few times).
Run: `cargo clippy -p mmm-core --all-targets`
Expected: PASS, no warnings.
```bash
git add crates/mmm-core/src/ipc/reader.rs crates/mmm-core/src/panel_reader.rs
git commit -m "feat(ipc): IpcBacking PanelReader with per-thread band buffers"
```

---

## Task 6: `ShmRowSink` (`ipc/sink.rs`)

**Files:**
- Modify: `crates/mmm-core/src/ipc/sink.rs`
- Test: inline `#[cfg(test)]`.

**Interfaces:**
- Consumes: `HostLink` (Task 4), `blend::RowSink` (existing).
- Produces: `struct ShmRowSink { link:Arc<HostLink>, next_y0:u64 }` with `fn new(link:Arc<HostLink>)->ShmRowSink` and `impl RowSink`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::blend::RowSink;
    use crate::ipc::client::HostLink;
    use crate::ipc::testhost::MockHost;

    #[test]
    fn streamed_bands_reassemble_on_the_host() {
        let (w, h, ch) = (5u64, 7u64, 2u64);
        let job = MockHost::output_job(w, h, ch, /*output_slots*/2, w*ch*4*4);
        let (host, r, wr) = MockHost::spawn(job.clone(), vec![]);
        let link = HostLink::start(job, r, wr).unwrap();
        let mut sink = ShmRowSink::new(link.clone());
        sink.begin(w, h, ch).unwrap();
        // Two bands of 4 and 3 rows; value = c*100 + y*10 + x.
        let band = |y0: u64, rows: u64| (0..ch).flat_map(|c| (0..rows).flat_map(move |ry| (0..w).map(move |x| (c*100 + (y0+ry)*10 + x) as f32))).collect::<Vec<_>>();
        sink.band(0, &band(0, 4)).unwrap();
        sink.band(4, &band(4, 3)).unwrap();
        link.finish_ok().unwrap();
        host.join();
        let (geom, out) = host.result();
        assert_eq!(geom, (w, h, ch));
        for c in 0..ch { for y in 0..h { for x in 0..w {
            assert_eq!(out[((c*h + y)*w + x) as usize], (c*100 + y*10 + x) as f32);
        }}}
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p mmm-core ipc::sink::tests`
Expected: FAIL.

- [ ] **Step 3: Implement `ShmRowSink`**

`begin`: `self.link.begin_output(w,h,ch)`. `band(y0, rows)`: derive `rows_count = rows.len() as u64 / (ch*w)`; `self.link.send_output_band(y0, rows_count, rows)`; the host reassembles by `y0`. No `finish` in the trait (check the trait — if there is a `finish`, forward to `link.finish_ok`). Keep the sink dumb; ordering is already guaranteed by `blend`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p mmm-core ipc::sink::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mmm-core/src/ipc/sink.rs
git commit -m "feat(ipc): ShmRowSink streaming blended bands back to the host"
```

---

## Task 7: `PanelSource` injection into blend

**Files:**
- Modify: `crates/mmm-core/src/ipc/source.rs`
- Modify: `crates/mmm-core/src/blend.rs` (`open_readers`, `blend`, `blend_full`, `blend_twoband`/`_impl`)
- Test: `crates/mmm-core/src/blend_tests.rs`

**Interfaces:**
- Produces:
  - `trait PanelSource: Sync { fn open_reader(&self, meta:&PanelMeta, canvas:(u64,u64,u64))->Result<PanelReader>; }`
  - `struct FileSource;` (opens via `PanelReader::open` — current behaviour).
  - `struct IpcSource { link:Arc<HostLink>, band_rows:usize }` — for `PanelStorage::Ipc{panel_id}` opens via `open_ipc`; for `FullCanvasXisf`/`CroppedCache` (solved-mode disk caches) falls back to `PanelReader::open` (those legitimately live on disk).
  - `blend::blend_with_source(session, phot, surfaces, graph, params, source:&dyn PanelSource, sink)->Result<()>`; `blend::blend(...)` delegates with `&FileSource`.

- [ ] **Step 1: Write the failing test (behaviour-preserving)**

In `blend_tests.rs`:
```rust
#[test]
fn blend_with_file_source_matches_plain_blend() {
    // Build a tiny 2-panel session via the synth harness (reuse an existing
    // helper in this file that produces a Session + Photometry + graph).
    let fixture = super::tests_support::two_panel_session(); // existing/added helper
    let params = crate::blend::BlendParams { downsample: 8, ..Default::default() };
    let mut a = crate::output::VecSink::default();
    crate::blend::blend(&fixture.session, &fixture.phot, None, &fixture.graph, &params, &mut a).unwrap();
    let mut b = crate::output::VecSink::default();
    crate::blend::blend_with_source(&fixture.session, &fixture.phot, None, &fixture.graph, &params, &crate::ipc::source::FileSource, &mut b).unwrap();
    assert_eq!(a.rows, b.rows, "injecting the default FileSource must not change output");
}
```
(If no `VecSink` exists in `output`, add a trivial `#[derive(Default)] struct VecSink { geom:(u64,u64,u64), rows:Vec<f32> }` implementing `RowSink` in `output/mod.rs` — it is broadly useful for tests. If `two_panel_session` does not exist, add it to a `tests_support` submodule in `blend_tests.rs` using `synth`.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p mmm-core blend_with_file_source_matches_plain_blend`
Expected: FAIL (functions/types undefined).

- [ ] **Step 3: Implement the source and thread it through blend**

Add `source.rs` with the trait + `FileSource` + `IpcSource`. Change `open_readers(session)` to `open_readers(session, source: &dyn PanelSource)` — replace the `PanelReader::open(p, session.canvas)?` line with `source.open_reader(p, session.canvas)?`. Add `blend_with_source(...)` as the real body of the current `blend`, taking `source`; thread `source` into `blend_full` and `blend_twoband_impl` (the two `open_readers` callers). Make `blend(...)` a thin wrapper calling `blend_with_source(..., &FileSource, sink)`. `blend_l8` reads summaries, not panels — it takes no source. Keep the public `blend` signature unchanged so the CLI and every existing test still compile.

- [ ] **Step 4: Run to verify pass + full suite**

Run: `cargo test -p mmm-core`
Expected: PASS — every pre-existing blend test still green (behaviour preserved), plus the new equivalence test.

- [ ] **Step 5: Commit**

```bash
git add crates/mmm-core/src/ipc/source.rs crates/mmm-core/src/blend.rs crates/mmm-core/src/blend_tests.rs crates/mmm-core/src/output/mod.rs
git commit -m "refactor(blend): inject PanelSource; add IPC source (behaviour-identical)"
```

---

## Task 8: Analyze over a `PanelSource`

**Files:**
- Modify: `crates/mmm-core/src/analyze.rs`
- Modify: `crates/mmm-core/src/ipc/source.rs` (helper to build panel metas for IPC aligned input)
- Test: `crates/mmm-core/src/analyze.rs` inline `#[cfg(test)]`.

**Interfaces:**
- Produces:
  - `analyze::analyze_ipc_aligned(link:Arc<HostLink>, session_dir:&Path, band_rows:usize, surface_order:Option<u32>)->Result<Session>` — scans each host panel through an `IpcPanelReader`, writing the same session artifacts as `analyze_aligned`, and records `PanelStorage::Ipc{panel_id}` in each `PanelMeta`.
  - `analyze::analyze_ipc_solved(link, session_dir, band_rows, surface_order)->Result<Session>` — reprojects each raw host panel (read via IPC) into `panels/<id>/aligned.bin` using the existing `align::reproject_from_reader` (added below), then scans the caches from disk exactly like `analyze_solved`.
  - `align::reproject_from_reader(reader:&PanelReader, model:&WcsModel, frame:&MosaicFrame, out_dir:&Path)->Result<AlignedPanel>` — the existing `reproject_panel` body generalised to read source rows through a `PanelReader` instead of an `XisfPanel` (mechanical: replace `panel.row(c,y)` calls, which already have the same shape).

- [ ] **Step 1: Write the failing test (aligned IPC analyze == file analyze)**

```rust
#[test]
fn ipc_aligned_analyze_matches_file_analyze() {
    use crate::ipc::client::HostLink;
    use crate::ipc::testhost::MockHost;
    // Two synth full-canvas panels on disk → file analyze (reference).
    let f = synth_two_full_canvas_panels(); // helper: returns dir + paths + planar per panel
    let ref_dir = f.dir.join("ref.mmm-session");
    let ref_sess = crate::analyze::analyze_opts(&f.paths, &ref_dir, Some(2)).unwrap();

    // Same pixels via IPC → ipc analyze.
    let (w, h, ch) = ref_sess.canvas;
    let job = MockHost::aligned_job(w, h, ch, f.paths.len() as u32, 8, w*ch*32*4);
    let (host, r, wr) = MockHost::spawn(job.clone(), f.planar.clone());
    let link = HostLink::start(job, r, wr).unwrap();
    let ipc_dir = f.dir.join("ipc.mmm-session");
    let ipc_sess = crate::analyze::analyze_ipc_aligned(link.clone(), &ipc_dir, 32, Some(2)).unwrap();
    link.finish_ok().unwrap();
    host.join();

    // Summaries, photometry, and per-panel stats must be byte-identical.
    assert_eq!(ref_sess.canvas, ipc_sess.canvas);
    for id in 0..f.paths.len() {
        assert_eq!(std::fs::read(ref_sess.summary_path(id)).unwrap(),
                   std::fs::read(ipc_sess.summary_path(id)).unwrap(), "summary {id}");
    }
    assert_eq!(std::fs::read(ref_sess.photometry_path()).unwrap(),
               std::fs::read(ipc_sess.photometry_path()).unwrap());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p mmm-core ipc_aligned_analyze_matches_file_analyze`
Expected: FAIL.

- [ ] **Step 3: Implement `analyze_ipc_aligned`**

Mirror `analyze_aligned` but build readers from the link: for `id in 0..link.panels().len()`, `let reader = PanelReader::open_ipc(link.clone(), id as u32, canvas, band_rows);` then reuse the existing `scan_reader(meta, reader)` with `meta.storage = PanelStorage::Ipc{ panel_id: id as u32 }` and `meta.path = PathBuf::new()`. Canvas = `link.canvas()`. Run the scans with `rayon` exactly as the file path does (the IPC reader is `Sync`). After scanning, check `reader`/link for a latched transport error and propagate it. Call the shared `finish_session`. For solved: implement `analyze_ipc_solved` reusing `WcsModel::from_properties(&panel_desc.properties, w, h)`, `choose_frame`, and `reproject_from_reader` writing `aligned.bin`, then scan the caches from disk (`PanelStorage::CroppedCache`) — identical to `analyze_solved`'s tail.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p mmm-core ipc_aligned_analyze_matches_file_analyze`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mmm-core/src/analyze.rs crates/mmm-core/src/align.rs crates/mmm-core/src/ipc/source.rs
git commit -m "feat(ipc): analyze aligned/solved panels streamed from the host"
```

---

## Task 9: The `mmm-ipc-worker` binary

**Files:**
- Modify: `crates/mmm-ipc-worker/src/main.rs`

**Interfaces:**
- Consumes: `analyze_ipc_aligned`/`analyze_ipc_solved`, `analyze_input` (files mode), `blend_with_source` + `IpcSource`, `ShmRowSink`, the protocol `read_host_frame` for `Init`.
- Produces: the runnable worker. Contract: reads one `HostMsg::Init` from stdin, does the job, streams the result via `ShmRowSink`, sends `Done` or `Error`, exits 0 on success / non-zero on failure.

- [ ] **Step 1: Write the failing integration test**

Create `crates/mmm-ipc-worker/tests/end_to_end.rs`:
```rust
// Spawns the real worker binary as a child, plays the host over real OS pipes
// + real shm, and asserts the returned mosaic is byte-identical to the
// file-based blend of the same synth panels.
#[test]
fn worker_blend_is_byte_identical_to_file_blend() {
    // 1. Synthesize N full-canvas panels, write them to disk, run the CLI
    //    path (analyze_opts + blend to a VecSink) to get the reference bytes.
    // 2. Spawn target/debug/mmm-ipc-worker with piped stdin/stdout.
    // 3. Create a real ShmSegment, send Init(Aligned, shm_name, ...), serve
    //    BandRequests from the in-memory panels, collect Begin/OutputBand.
    // 4. assert_eq!(reference_rows, streamed_rows).
    // Uses mmm_core::ipc::testhost helpers exposed for integration tests.
}
```
Because integration tests are a separate crate, expose the host-serving helper from `mmm-core` behind a `pub mod testkit` (a non-`#[cfg(test)]` module gated by a `testkit` feature) so both the unit tests and this integration test share one implementation. Add `[features] testkit = []` to mmm-core and `mmm-core = { path = "../mmm-core", features = ["testkit"] }` under `[dev-dependencies]` of the worker crate. Move the reusable `MockHost` serving loop into `ipc::testkit`; keep `testhost` a thin `#[cfg(test)]` re-export.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p mmm-ipc-worker`
Expected: FAIL (worker `main` is empty; helpers not yet public).

- [ ] **Step 3: Implement `main`**

```rust
fn main() {
    if let Err(e) = run() {
        // Best-effort error frame, then non-zero exit.
        let _ = writeln!(std::io::stderr(), "mmm-ipc-worker: {e}");
        std::process::exit(1);
    }
}

fn run() -> mmm_core::Result<()> {
    use mmm_core::ipc::{protocol::{read_host_frame, HostMsg, JobMode}, client::HostLink, sink::ShmRowSink, source::IpcSource};
    let mut stdin = std::io::stdin().lock();
    let init = match read_host_frame(&mut stdin)? {
        Some(HostMsg::Init(job)) => job,
        _ => return Err(mmm_core::Error::compute("expected Init as the first frame")),
    };
    if init.protocol_version != mmm_core::ipc::IPC_PROTOCOL_VERSION {
        return Err(mmm_core::Error::compute("protocol version mismatch"));
    }
    let band_rows = init.params.band_rows as usize;
    let session_dir = std::path::PathBuf::from(&init.session_dir);
    let params = init.params.to_params();
    let mode = init.mode.clone();
    // stdin is now owned by the reader thread inside HostLink.
    let link = HostLink::start(init, Box::new(std::io::stdin()), Box::new(std::io::stdout()))?;

    let session = match &mode {
        JobMode::Aligned => mmm_core::analyze::analyze_ipc_aligned(link.clone(), &session_dir, band_rows, params.flatten.map(|_| 2).or(Some(2)))?,
        JobMode::Solved  => mmm_core::analyze::analyze_ipc_solved(link.clone(), &session_dir, band_rows, Some(2))?,
        JobMode::Files{paths} => {
            let pb: Vec<_> = paths.iter().map(std::path::PathBuf::from).collect();
            mmm_core::analyze::analyze_input(&pb, &session_dir, Some(2), mmm_core::analyze::InputSelect::Auto)?
        }
    };
    if link.is_cancelled() { link.finish_err("cancelled"); return Ok(()); }

    let phot = mmm_core::photometry::Photometry::load(&session.photometry_path())?;
    let graph = mmm_core::overlap::OverlapGraph::load(&session.overlap_graph_path())?;
    let surfaces = mmm_core::surfaces::Surfaces::load(&session.surfaces_path()).ok();
    let source = IpcSource::new(link.clone(), band_rows);
    let mut sink = ShmRowSink::new(link.clone());
    mmm_core::blend::blend_with_source(&session, &phot, surfaces.as_ref(), &graph, &params, &source, &mut sink)?;
    link.finish_ok()
}
```
(Confirm the exact loader function names — `Photometry::load`, `OverlapGraph::load`, `Surfaces::load` — against the source; if a name differs, use the real one. `use std::io::Write;` for the stderr line. Note `HostLink::start` is given a fresh `std::io::stdin()`/`stdout()` handle; ensure `read_host_frame` for `Init` happened on the same underlying stdin before the reader thread takes over — acquire the `Init` bytes first, then hand the rest to the thread. If interleaving is fragile, have `HostLink::start` accept the already-locked reader and read `Init` inside it, returning `(link, init)`.)

- [ ] **Step 4: Run to verify pass**

Run: `cargo build -p mmm-ipc-worker; cargo test -p mmm-ipc-worker`
Expected: PASS — byte-identical mosaic.

- [ ] **Step 5: Commit**

```bash
git add crates/mmm-ipc-worker crates/mmm-core/Cargo.toml crates/mmm-core/src/ipc/mod.rs crates/mmm-core/src/ipc/testkit.rs crates/mmm-core/src/ipc/testhost.rs
git commit -m "feat: mmm-ipc-worker driving analyze+blend over IPC, byte-identical e2e"
```

---

## Task 10: Cancellation, worker-crash, and solved-mode e2e

**Files:**
- Modify: `crates/mmm-ipc-worker/tests/end_to_end.rs`
- Modify: `crates/mmm-core/src/ipc/client.rs` (cancel checks between bands, error latch)

**Interfaces:**
- Consumes: everything above.
- Produces: proven cancellation and fault semantics.

- [ ] **Step 1: Write failing tests**

Add three integration tests: (a) `cancel_midrun_stops_promptly` — host sends `Cancel` after the first few `BandRequest`s; assert the worker exits without producing a full result and without hanging (join within a timeout). (b) `worker_crash_is_observable` — spawn the worker, send `Init`, then kill it (`child.kill()`); assert the host's next `send_output_band`/read returns a clean error rather than blocking forever (the `HostLink` reader thread must detect pipe EOF and fail pending waiters). (c) `solved_mode_reprojection_matches_file` — build 2 raw solved synth panels with astrometric properties (reuse the synth solved-panel helper used by the phase-5 tests), run file-based `analyze_input(Solved)` + blend as reference, then the IPC solved path; assert the `aligned.bin` caches and the blended output match.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p mmm-ipc-worker cancel_ worker_crash_ solved_mode_`
Expected: FAIL.

- [ ] **Step 3: Implement the semantics**

In `HostLink`: the reader thread, on `read_host_frame` returning `Ok(None)` (EOF) or `Err`, sets a `shutdown` flag and notifies *all* pending `ReplySlot`s + slot-pool condvars so blocked `request_band`/`send_output_band` calls wake and return `Error::compute("host link closed")`. Add `check_cancel` points: `blend`/`analyze` already loop over bands/rows; the cancel signal is surfaced by making `request_band` return an error when `cancelled` is set, which propagates up as a normal `Result` error and unwinds the stage cleanly. In `main::run`, map a cancelled error to `finish_err("cancelled")` + exit 0. Verify the worker's `request_band` failure path in mid-blend does not leave a partial band delivered as final (the host discards results on any non-`Done`终) — the test asserts no full mosaic is emitted.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p mmm-ipc-worker`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mmm-ipc-worker/tests/end_to_end.rs crates/mmm-core/src/ipc/client.rs
git commit -m "feat(ipc): cancellation + crash-observable link semantics; solved-mode e2e"
```

---

## Task 11: CLI smoke subcommand + docs

**Files:**
- Modify: `crates/mmm/src/main.rs` (optional `mmm ipc-selftest` hidden subcommand that runs the worker against synth panels in-process for a manual smoke check)
- Modify: `docs/DESIGN.md` (add an "IPC transport" subsection pointing at the spec)
- Create: `integration/pixinsight/PROTOCOL.md` (the wire protocol reference, generated from the Task 2 types — this is the contract the Plan 2 C++ module implements)

**Interfaces:**
- Produces: human-facing protocol doc + a one-command smoke check.

- [ ] **Step 1: Write `PROTOCOL.md`**

Document the frame format (`[u8 tag][u32 le len][payload]`), every tag, the binary band-frame field layout, the JSON schemas for `Init`/`Progress`/etc., the shm slot layout (`input_offset`/`output_offset` math), the request→fill→reply and output→ack handshakes, the concurrency model (≥ `input_slots` = worker threads), and the cancellation/EOF semantics. This file is the source of truth for the C++ side.

- [ ] **Step 2: Add the DESIGN.md subsection**

One paragraph under a new `## IPC transport (PixInsight)` heading summarising the worker/host split and linking `docs/superpowers/specs/2026-07-27-pixinsight-integration-design.md` and `integration/pixinsight/PROTOCOL.md`.

- [ ] **Step 3: (Optional) `mmm ipc-selftest`**

A hidden subcommand that synthesizes 2 panels, spawns `mmm-ipc-worker`, plays the host in-process, and prints OK / a diff summary — a manual smoke check that mirrors the integration test without `cargo test`. Skip if time-boxed; the integration test already covers correctness.

- [ ] **Step 4: Full verification**

Run: `cargo test; cargo clippy --all-targets; cargo fmt --check; cargo doc --no-deps 2>&1 | grep -i warning`
Expected: all green, no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/mmm/src/main.rs docs/DESIGN.md integration/pixinsight/PROTOCOL.md
git commit -m "docs(ipc): protocol reference, DESIGN.md subsection, smoke subcommand"
```

---

## Self-Review

**Spec coverage** (against `2026-07-27-pixinsight-integration-design.md`):
- §3 topology (worker links mmm-core; module supervises) — Tasks 1, 9. C++ module deferred to Plan 2 (explicit).
- §4 aligned/solved/files data flow — Tasks 8, 9, 10.
- §5 transport: stdin/stdout control channel, named shm, request→fill→reply, binary hot path + JSON handshake, slot pool sized to worker threads — Tasks 2, 3, 4.
- §6 IpcPanelReader as a third backing, per-thread band buffers, no change to analyze/blend row loops — Task 5 (+ §refinement: per-thread sharded buffers, Task 5 Step 3).
- §7 output = streamed new window — Task 6.
- §8 session/reprojection on disk in a user-specified dir — Task 8 (solved reuses phase-5 `aligned.bin`).
- §9 crash/cancel/version isolation — Tasks 2 (version), 10 (crash/cancel); `panic=abort` — Task 1.
- §11 golden byte-identity test without PixInsight — Tasks 7, 8, 9, 10.
- §10 UI, §12 packaging — Plan 2 (out of scope here, by design).

**Placeholder scan:** the only intentionally-open items are Task 9 Step 3's "confirm exact loader names" (mechanical, verified against source during execution) and Task 11 Step 3 (explicitly optional). No `TODO`/`TBD` requirements; every code step carries real code.

**Type consistency:** `HostLink` (not `HostTransport`) used throughout Tasks 4–10; `IpcBacking`/`open_ipc`/`PanelStorage::Ipc{panel_id}` consistent Tasks 5–8; `PanelSource`/`FileSource`/`IpcSource`/`blend_with_source` consistent Tasks 7–9; `ShmRowSink::new(link)` consistent Tasks 6, 9; `request_band(panel_id,y0,y1,dst)` consistent Tasks 4, 5. `MockHost`/`testkit` split introduced in Task 4, made integration-visible in Task 9.

**Risks flagged for the executor:**
- The `unsafe impl Sync for IpcBacking` disjoint-access invariant (Task 5) is the sharpest correctness point — the concurrency test hammers it, but review the SAFETY note carefully.
- `HostLink::start` stdin ownership vs. reading `Init` first (Task 9 Step 3) — prefer the `start` returning `(link, init)` variant if interleaving proves fragile.
