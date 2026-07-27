# PixInsight Plan 2a — Rust IPC changes + C++ host transport library

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the three Rust-side IPC changes Plan 2 needs (files-mode `InputSelect` override, a `--probe-frame` subcommand, `Progress` emission) and build a **PCL-free C++ host transport library** that drives the real `mmm-ipc-worker` over pipes + POSIX shared memory, proven **byte-identical** to the file-based blend by standalone tests that need no PixInsight.

**Architecture:** The C++ splits into a pure `host/` transport layer (shm, wire codec, spawn/supervise, serve bands, collect output) that includes **no PCL header** and is exercised by standalone CTest executables spawning the real worker — mirroring `crates/mmm-ipc-worker/tests/end_to_end.rs`. The separate PCL `module/` wrapper is **Plan 2b** and is out of scope here. The wire contract is fixed by `integration/pixinsight/PROTOCOL.md`; the Rust source of truth is `crates/mmm-core/src/ipc/{protocol.rs,shm.rs,client.rs,testhost.rs}`.

**Tech Stack:** Rust (mmm-core, mmm-ipc-worker); C++20 (g++ 13.3); CMake + CTest; POSIX shm (`shm_open`/`mmap`) and `posix_spawn`; vendored `nlohmann/json` single header (MIT) for JSON, so `host/` has no other third-party dependency.

## Global Constraints

- **Keep the branch green:** `cargo test --workspace` must pass after every Rust task. Do **not** create a new branch — work on `pixinsight-integration`.
- **`host/` includes no PCL header.** It links only the C++ stdlib, POSIX, pthreads, and the vendored `nlohmann/json`. This is what makes the standalone tests runnable without PixInsight.
- **Wire contract is authoritative:** every binary offset, tag number, JSON field name, and slot-layout formula must match `integration/pixinsight/PROTOCOL.md` **and** the Rust code it documents. Any wire change updates `PROTOCOL.md` in the same task.
- **Endianness:** all multi-byte integers on the control channel are **little-endian**; pixel `f32` slots are **native-endian** planar `index(c,r,x) = c*rows*width + r*width + x`.
- **C++ standard/flags:** C++20, `-fPIC -pthread -Wall`. No exceptions leak across the worker boundary as hangs — a dead worker surfaces as a clean error, never a hang (enforced by tests with a watchdog).
- **Protocol version:** `IPC_PROTOCOL_VERSION` becomes `2` in this plan (Task 1). The worker rejects any other version.
- Every new public item in `mmm-core` needs a doc comment (`missing_docs` is warned); keep `cargo fmt`, `cargo clippy --all-targets`, `cargo doc` warning-free.

---

## File Structure

**Rust (modify):**
- `crates/mmm-core/src/ipc/mod.rs` — bump `IPC_PROTOCOL_VERSION` 1 → 2.
- `crates/mmm-core/src/ipc/protocol.rs` — add `input_select` to `JobMode::Files`; add `InputSelectWire` enum.
- `crates/mmm-ipc-worker/src/main.rs` — map `JobMode::Files.input_select` → `InputSelect`; add `--probe-frame` mode.
- `crates/mmm-core/src/analyze.rs` — per-panel `Progress` in `analyze_ipc_aligned`/`analyze_ipc_solved`.
- `crates/mmm-core/src/ipc/sink.rs` — per-band `Progress` in `ShmRowSink`.
- `crates/mmm-ipc-worker/tests/end_to_end.rs` — new tests (files override, probe, progress).
- `integration/pixinsight/PROTOCOL.md` — document v2, `input_select`, `--probe-frame`, progress reality.

**Rust (create):**
- `crates/mmm-ipc-worker/examples/gen_fixtures.rs` — writes C++ test fixtures (XISF + raw `.bin` + `meta.json` + solved `props.json`) using `mmm_core::synth`.

**C++ (create):**
- `integration/pixinsight/host/third_party/json.hpp` — vendored nlohmann/json (single header).
- `integration/pixinsight/host/mmm_shm.{h,cpp}` — shm create/attach/unlink + `SlotLayout`.
- `integration/pixinsight/host/mmm_protocol.{h,cpp}` — frame codec, tags, binary layouts, Init JSON writer, worker-frame JSON reader.
- `integration/pixinsight/host/mmm_host.{h,cpp}` — spawn/supervise, service loop, `PanelSource`, output collector, fault isolation, cancel, solved probe.
- `integration/pixinsight/host/CMakeLists.txt` — `mmm_host` static lib + tests, CTest.
- `integration/pixinsight/host/test/test_util.h` — `CHECK` macro + fixture-path helpers.
- `integration/pixinsight/host/test/test_shm.cpp` — Task 4.
- `integration/pixinsight/host/test/test_protocol.cpp` — Task 5.
- `integration/pixinsight/host/test/test_golden_aligned.cpp` — Task 7.
- `integration/pixinsight/host/test/test_golden_solved.cpp` — Task 8.
- `integration/pixinsight/host/test/test_isolation.cpp` — Task 9.
- `integration/pixinsight/host/README.md` — build + run instructions (Task 10).

---

## Task 1: Files-mode `InputSelect` override + protocol version bump

**Files:**
- Modify: `crates/mmm-core/src/ipc/mod.rs:39`
- Modify: `crates/mmm-core/src/ipc/protocol.rs:157-161` (the `Files` variant) and add `InputSelectWire`
- Modify: `crates/mmm-ipc-worker/src/main.rs:75-78`
- Modify: `integration/pixinsight/PROTOCOL.md` (§4/§6/§10/§11)
- Test: `crates/mmm-core/src/ipc/protocol.rs` (unit test module at bottom)

**Interfaces:**
- Produces: `JobMode::Files { paths: Vec<String>, input_select: InputSelectWire }`; `enum InputSelectWire { Auto, Aligned, Solved }` (externally tagged, serializes as bare strings `"Auto"`/`"Aligned"`/`"Solved"`); `InputSelectWire::to_input_select(self) -> mmm_core::analyze::InputSelect`.
- Consumes: existing `mmm_core::analyze::InputSelect` (`Auto`/`Aligned`/`Solved`).

- [ ] **Step 1: Write the failing test** — add to the `#[cfg(test)] mod tests` in `protocol.rs`:

```rust
#[test]
fn files_mode_carries_input_select_and_round_trips() {
    use super::*;
    let m = JobMode::Files {
        paths: vec!["a.xisf".into(), "b.xisf".into()],
        input_select: InputSelectWire::Solved,
    };
    let js = serde_json::to_string(&m).unwrap();
    // externally-tagged: the enum value round-trips exactly.
    let back: JobMode = serde_json::from_str(&js).unwrap();
    assert_eq!(m, back);
    // InputSelectWire serializes as a bare string (unit variant).
    assert_eq!(serde_json::to_string(&InputSelectWire::Auto).unwrap(), "\"Auto\"");
}

#[test]
fn input_select_wire_maps_to_core() {
    use super::InputSelectWire as W;
    use crate::analyze::InputSelect as I;
    assert_eq!(W::Auto.to_input_select(), I::Auto);
    assert_eq!(W::Aligned.to_input_select(), I::Aligned);
    assert_eq!(W::Solved.to_input_select(), I::Solved);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `source ~/.cargo/env && cargo test -p mmm-core input_select 2>&1 | tail -20`
Expected: FAIL — `InputSelectWire` and the `input_select` field do not exist.

- [ ] **Step 3: Implement.** In `protocol.rs`, add the enum and the mapping, and extend `Files`:

```rust
/// Wire form of [`crate::analyze::InputSelect`] — the UI's `Auto/Aligned/Solved`
/// override, threaded to the worker only for [`JobMode::Files`] (views modes are
/// resolved host-side into `Aligned`/`Solved` directly). Serializes as a bare
/// JSON string via serde's externally-tagged unit-variant encoding.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum InputSelectWire {
    /// Detect aligned-vs-solved from the files (the default).
    #[default]
    Auto,
    /// Force the aligned full-canvas path.
    Aligned,
    /// Force the solved (reproject) path.
    Solved,
}

impl InputSelectWire {
    /// Map to the `mmm-core` selector the file analyze path consumes.
    pub fn to_input_select(self) -> crate::analyze::InputSelect {
        match self {
            InputSelectWire::Auto => crate::analyze::InputSelect::Auto,
            InputSelectWire::Aligned => crate::analyze::InputSelect::Aligned,
            InputSelectWire::Solved => crate::analyze::InputSelect::Solved,
        }
    }
}
```

Change the `Files` variant to:

```rust
    Files {
        /// One path per panel, in `panels` order.
        paths: Vec<String>,
        /// The `Auto/Aligned/Solved` override (default `Auto`).
        #[serde(default)]
        input_select: InputSelectWire,
    },
```

In `mod.rs`, bump: `pub const IPC_PROTOCOL_VERSION: u32 = 2;`

In `main.rs`, update the match arm:

```rust
            JobMode::Files { paths, input_select } => {
                let paths: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
                analyze_input(&paths, &session_dir, surface_order, input_select.to_input_select())?
            }
```

- [ ] **Step 4: Run tests to verify they pass, and the workspace stays green**

Run: `source ~/.cargo/env && cargo test --workspace 2>&1 | grep -E "test result:|error" | tail -20`
Expected: all `ok`, 0 failures. (`#[serde(default)]` keeps any existing `Files` deserialization compatible; the only in-tree constructor is `main.rs`, updated above.)

- [ ] **Step 5: Update `PROTOCOL.md`.** In §10 bump the version note to `2`. In §11, change the `Files` bullet to document the new object shape `{"Files": {"paths": [...], "input_select": "Auto"}}` and that `input_select` defaults to `"Auto"`, mapping to the worker's `InputSelect`. Add a one-line note in §6 that `InitJob.protocol_version` must equal `2`.

- [ ] **Step 6: Verify fmt/clippy/doc clean**

Run: `source ~/.cargo/env && cargo fmt --check && cargo clippy --all-targets 2>&1 | grep -E "warning|error" | tail; cargo doc -p mmm-core --no-deps 2>&1 | grep -E "warning|error" | tail`
Expected: no warnings/errors.

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "feat(ipc): files-mode InputSelect override; bump protocol v1->v2

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: `mmm-ipc-worker --probe-frame` subcommand

**Files:**
- Modify: `crates/mmm-ipc-worker/src/main.rs` (arg handling + a `probe_frame` fn)
- Test: `crates/mmm-ipc-worker/tests/end_to_end.rs` (new `probe_frame_*` test)

**Interfaces:**
- Produces: CLI contract — `mmm-ipc-worker --probe-frame` reads one `InitJob`-shaped JSON document from **stdin** (a bare JSON object, **not** a length-prefixed frame — this path predates the transport), builds a `WcsModel` per panel from `PanelDesc.properties`, runs `mmm_core::align::choose_frame`, and prints exactly `"{w} {h} {ch}\n"` to stdout, exit 0. On any error (missing/invalid solution, empty panels) it prints a message to stderr and exits 1.
- Consumes: `mmm_core::align::choose_frame`, `mmm_core::astrometry::WcsModel::from_properties`, `InitJob` (deserialized from stdin JSON).

- [ ] **Step 1: Write the failing test** — add to `end_to_end.rs` (reuse `write_two_solved_panels`, `choose_frame`, `XisfPanel`, `WcsModel` already imported there):

```rust
#[test]
fn probe_frame_prints_choose_frame_geometry() {
    use std::io::Write;
    let dir = tmpdir("probe");
    let (paths, _planars) = write_two_solved_panels(&dir);

    // Build the expected frame the same way analyze_ipc_solved will.
    let headers: Vec<(u64, u64, u64, Vec<mmm_core::formats::XisfProperty>)> = paths
        .iter()
        .map(|p| {
            let xp = XisfPanel::open(p).unwrap();
            (xp.width(), xp.height(), xp.channels(), xp.header().properties.clone())
        })
        .collect();
    let models: Vec<WcsModel> = headers
        .iter()
        .map(|(w, h, _, props)| WcsModel::from_properties(props, *w, *h).unwrap())
        .collect();
    let frame = choose_frame(&models);
    let ch = headers[0].2;

    // Build the probe Init JSON (panels with properties; mode Solved).
    let panels: Vec<PanelDesc> = headers
        .iter()
        .enumerate()
        .map(|(id, (w, h, c, props))| PanelDesc {
            panel_id: id as u32,
            width: *w,
            height: *h,
            channels: *c,
            properties: props.clone(),
        })
        .collect();
    let job = InitJob {
        protocol_version: IPC_PROTOCOL_VERSION,
        shm_name: String::new(),
        slot_bytes: 0,
        input_slots: 0,
        output_slots: 0,
        canvas: [0, 0, ch],
        panels,
        mode: JobMode::Solved,
        session_dir: String::new(),
        params: BlendParamsWire {
            feather_px: 0.0, downsample: 1, band_rows: 8, mode: "pyramid".into(),
            roi: None, defect_veto: true, flatten: None, surface_order: Some(2),
        },
    };
    let json = serde_json::to_string(&job).unwrap();

    let exe = env!("CARGO_BIN_EXE_mmm-ipc-worker");
    let mut child = std::process::Command::new(exe)
        .arg("--probe-frame")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(json.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "probe exited nonzero: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).unwrap();
    assert_eq!(text.trim(), format!("{} {} {}", frame.width, frame.height, ch));

    std::fs::remove_dir_all(&dir).unwrap();
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `source ~/.cargo/env && cargo test -p mmm-ipc-worker probe_frame 2>&1 | tail -20`
Expected: FAIL — `--probe-frame` is unrecognized; the worker tries to read a length-prefixed `Init` frame and errors.

- [ ] **Step 3: Implement.** At the top of `run()` (or in `main`), branch on the arg before the transport handshake:

```rust
fn main() {
    let probe = std::env::args().any(|a| a == "--probe-frame");
    let result = if probe { probe_frame() } else { run() };
    if let Err(e) = result {
        let _ = writeln!(std::io::stderr(), "mmm-ipc-worker: {e}");
        std::process::exit(1);
    }
}

/// `--probe-frame`: read an `InitJob`-shaped JSON object on stdin, build the
/// WCS models from each panel's `properties`, run `choose_frame`, and print
/// `"{w} {h} {ch}"`. Lets a host size the shm output slots for solved mode
/// without duplicating the frame math (see PROTOCOL.md §11 / spec §15).
fn probe_frame() -> mmm_core::Result<()> {
    use mmm_core::align::choose_frame;
    use mmm_core::astrometry::WcsModel;
    let mut buf = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
        .map_err(|e| mmm_core::Error::compute(format!("reading probe JSON from stdin: {e}")))?;
    let job: mmm_core::ipc::protocol::InitJob = serde_json::from_str(&buf)
        .map_err(|e| mmm_core::Error::compute(format!("parsing probe InitJob JSON: {e}")))?;
    if job.panels.is_empty() {
        return Err(mmm_core::Error::compute("probe-frame: no panels"));
    }
    let mut models = Vec::with_capacity(job.panels.len());
    for p in &job.panels {
        let m = WcsModel::from_properties(&p.properties, p.width, p.height).ok_or_else(|| {
            mmm_core::Error::compute(format!("probe-frame: panel {} lacks a usable solution", p.panel_id))
        })?;
        models.push(m);
    }
    let frame = choose_frame(&models);
    let ch = job.panels[0].channels;
    writeln!(std::io::stdout(), "{} {} {}", frame.width, frame.height, ch)
        .map_err(|e| mmm_core::Error::compute(format!("probe-frame: writing stdout: {e}")))?;
    Ok(())
}
```

Add `use std::io::Read;`/`Write` as needed (Write is already imported).

- [ ] **Step 4: Run test to verify it passes**

Run: `source ~/.cargo/env && cargo test -p mmm-ipc-worker probe_frame 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Document in `PROTOCOL.md`.** Add a short subsection under §11 (Job modes) titled "Frame probe (solved-mode sizing)" describing: invoke `mmm-ipc-worker --probe-frame`, write the `InitJob` JSON (unframed) to stdin, read `"{w} {h} {ch}"` from stdout; the host uses it to size `slot_bytes` before creating shm.

- [ ] **Step 6: Verify green + clean**

Run: `source ~/.cargo/env && cargo test --workspace 2>&1 | grep -E "test result:|error" | tail; cargo clippy --all-targets 2>&1 | grep -E "warning|error" | tail`
Expected: all pass, no warnings.

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "feat(worker): --probe-frame prints choose_frame geometry for solved sizing

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: `Progress` emission in the IPC analyze + blend paths

**Files:**
- Modify: `crates/mmm-core/src/analyze.rs` (`analyze_ipc_aligned` ~L339, `analyze_ipc_solved` ~L384)
- Modify: `crates/mmm-core/src/ipc/sink.rs` (`ShmRowSink`)
- Test: `crates/mmm-ipc-worker/tests/end_to_end.rs` (new `progress_frames_are_emitted` test)

**Interfaces:**
- Consumes: `HostLink::send_progress(&self, stage: &str, done: u64, total: u64)` (already exists, `client.rs:455`).
- Produces: `Progress { stage: "analyze", done, total }` frames during the IPC scan (one per panel completed) and `Progress { stage: "blend", done, total }` during the band sweep (one per band delivered). No signature changes to `blend.rs`.

- [ ] **Step 1: Write the failing test** — add to `end_to_end.rs`. It drives a full aligned run with a hand-written serve loop that counts `Progress` frames (the `MockHost` path hides them, so serve manually here):

```rust
#[test]
fn progress_frames_are_emitted() {
    with_watchdog(Duration::from_secs(20), || {
        let dir = tmpdir("progress");
        let (w, h, ch) = (96u64, 64u64, 1u64);
        let band_rows = 8u32;
        let (job, shm, panel_pixels) = aligned_two_panel_job(&dir, "progress", w, h, ch, band_rows);
        let layout = SlotLayout {
            slot_bytes: job.slot_bytes,
            input_slots: job.input_slots,
            output_slots: job.output_slots,
        };
        let (mut child, mut child_stdin, mut child_stdout) = spawn_worker_with_init(&job);

        let mut progress_count = 0usize;
        let mut saw_done = false;
        loop {
            match read_worker_frame(&mut child_stdout) {
                Ok(Some(WorkerMsg::BandRequest(req))) => {
                    let panel = &panel_pixels[req.panel_id as usize];
                    let desc = &job.panels[req.panel_id as usize];
                    let rows = req.y1 - req.y0;
                    let dst = shm.slice_mut(layout.input_offset(req.slot_id), desc.channels * rows * desc.width);
                    let plane_stride = desc.height * desc.width;
                    for c in 0..desc.channels {
                        let ss = (c * plane_stride + req.y0 * desc.width) as usize;
                        let sl = (rows * desc.width) as usize;
                        let ds = (c * rows * desc.width) as usize;
                        dst[ds..ds + sl].copy_from_slice(&panel[ss..ss + sl]);
                    }
                    write_frame(&mut child_stdin, &HostMsg::BandReply(mmm_core::ipc::protocol::BandReply {
                        request_id: req.request_id, slot_id: req.slot_id, status: 0,
                    })).unwrap();
                    child_stdin.flush().unwrap();
                }
                Ok(Some(WorkerMsg::Progress { .. })) => progress_count += 1,
                Ok(Some(WorkerMsg::Begin { .. })) => {}
                Ok(Some(WorkerMsg::OutputBand(ob))) => {
                    write_frame(&mut child_stdin, &HostMsg::OutputAck { request_id: ob.request_id }).unwrap();
                    child_stdin.flush().unwrap();
                }
                Ok(Some(WorkerMsg::Done)) => { saw_done = true; break; }
                Ok(Some(WorkerMsg::Error { message })) => panic!("worker error: {message}"),
                Ok(None) => break,
                Err(e) => panic!("read error: {e}"),
            }
        }
        child.wait().unwrap();
        assert!(saw_done, "run should complete");
        assert!(progress_count > 0, "expected at least one Progress frame, got {progress_count}");
        std::fs::remove_dir_all(&dir).unwrap();
    });
}
```

(Check `read_worker_frame`/`OutputBand`/`OutputAck` field names against `protocol.rs` when writing — match the exact `OutputBand` struct field `request_id`.)

- [ ] **Step 2: Run test to verify it fails**

Run: `source ~/.cargo/env && cargo test -p mmm-ipc-worker progress_frames 2>&1 | tail -20`
Expected: FAIL — `progress_count` is 0 (no driver emits `Progress`).

- [ ] **Step 3: Implement analyze progress.** In `analyze_ipc_aligned` and `analyze_ipc_solved`, wrap the per-panel scan with an atomic counter. For the aligned scan loop (around L354-371), change the `.map(|id| { ... scan_reader(meta, reader) })` closure to increment and report after each scan:

```rust
    use std::sync::atomic::{AtomicU64, Ordering};
    let done = AtomicU64::new(0);
    let total = n_panels as u64;
    let scans: Vec<PanelScan> = (0..n_panels)
        .into_par_iter()
        .map(|id| {
            let reader = PanelReader::open_ipc(link.clone(), id as u32, canvas, band_rows);
            let meta = /* unchanged */;
            let s = scan_reader(meta, reader)?;
            let d = done.fetch_add(1, Ordering::Relaxed) + 1;
            link.send_progress("analyze", d, total);
            Ok(s)
        })
        .collect::<Result<_>>()?;
```

Apply the equivalent to `analyze_ipc_solved`'s scan loop (report `("analyze", done, n_panels)` per completed panel). Keep the existing body otherwise; only add the counter + `send_progress`. `send_progress` is best-effort (`let _ =` internally), so it never fails the scan.

- [ ] **Step 4: Implement blend progress in `ShmRowSink`.** In `sink.rs`, store the canvas height on `begin()` and emit per-band progress in `band()`:

```rust
// add a field: `out_h: u64` (init 0)
fn begin(&mut self, w: u64, h: u64, ch: u64) -> Result<()> {
    self.out_h = h;
    self.link.begin_output(w, h, ch)
}
fn band(&mut self, y0: u64, rows: &[f32]) -> Result<()> {
    // ... existing send_output_band logic ...
    let band_h = rows.len() as u64 / (self.width * self.channels).max(1);
    self.link.send_progress("blend", (y0 + band_h).min(self.out_h), self.out_h);
    Ok(())
}
```

Match the existing `ShmRowSink` field names (`width`/`channels` may be derived differently — inspect the file and adapt; the essential change is: record `h` in `begin`, call `send_progress("blend", y0+band_rows, h)` after the output band is sent).

- [ ] **Step 5: Run test to verify it passes + workspace green**

Run: `source ~/.cargo/env && cargo test -p mmm-ipc-worker progress_frames 2>&1 | tail; cargo test --workspace 2>&1 | grep -E "test result:|error" | tail`
Expected: PASS; all workspace tests still `ok` (byte-identity tests unaffected — Progress frames are skipped by `MockHost` and `serve_n_band_requests`).

- [ ] **Step 6: Update `PROTOCOL.md` §6.** Replace the note under the `Progress` (tag 2) example that says the worker emits no Progress frames with the reality: the worker now emits `Progress { stage: "analyze", done, total }` (one per panel during the IPC scan) and `Progress { stage: "blend", done, total }` (one per band during the sweep). A host may display these; absence in file mode is still fine.

- [ ] **Step 7: Verify clean + commit**

Run: `source ~/.cargo/env && cargo fmt --check && cargo clippy --all-targets 2>&1 | grep -E "warning|error" | tail`
```bash
git add -A && git commit -m "feat(ipc): emit Progress frames from IPC analyze scan and blend sweep

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: C++ `mmm_shm` — POSIX shm + `SlotLayout`

**Files:**
- Create: `integration/pixinsight/host/mmm_shm.h`, `integration/pixinsight/host/mmm_shm.cpp`
- Create: `integration/pixinsight/host/CMakeLists.txt`
- Create: `integration/pixinsight/host/third_party/json.hpp` (vendored nlohmann/json v3.11.x single header — download from github.com/nlohmann/json releases; MIT)
- Create: `integration/pixinsight/host/test/test_util.h`, `integration/pixinsight/host/test/test_shm.cpp`

**Interfaces:**
- Produces:
  ```cpp
  namespace mmm {
  struct SlotLayout {
    uint64_t slot_bytes; uint32_t input_slots; uint32_t output_slots;
    uint64_t total_bytes() const { return slot_bytes * (uint64_t(input_slots) + output_slots); }
    uint64_t input_offset(uint32_t i)  const { return slot_bytes * i; }
    uint64_t output_offset(uint32_t i) const { return slot_bytes * input_slots + slot_bytes * i; }
  };
  // RAII: creator ftruncates+mmaps and unlinks on destruction.
  class ShmSegment {
  public:
    static ShmSegment create(const std::string& name, uint64_t total_bytes); // throws std::runtime_error
    ~ShmSegment();                       // munmap + shm_unlink (creator only)
    ShmSegment(ShmSegment&&) noexcept; ShmSegment& operator=(ShmSegment&&) noexcept;
    ShmSegment(const ShmSegment&) = delete;
    uint8_t* base() const;               // mapped pointer
    uint64_t size() const;
    float* slot_floats(uint64_t byte_offset) const; // base()+offset as float* (offset%4==0 asserted)
    const std::string& name() const;
  };
  } // namespace mmm
  ```
  Matches PROTOCOL.md §7 offset math and `crates/mmm-core/src/ipc/shm.rs`'s `SlotLayout`.

- [ ] **Step 1: Write `test_util.h`** (dependency-free check macro):

```cpp
#pragma once
#include <cstdio>
#include <cstdlib>
#define CHECK(cond) do { if(!(cond)) { \
  std::fprintf(stderr, "CHECK failed: %s (%s:%d)\n", #cond, __FILE__, __LINE__); \
  std::exit(1); } } while(0)
```

- [ ] **Step 2: Write the failing test** `test_shm.cpp`:

```cpp
#include "../mmm_shm.h"
#include "test_util.h"
#include <cstring>
int main() {
  using namespace mmm;
  SlotLayout L{ /*slot_bytes*/ 256, /*input*/ 4, /*output*/ 2 };
  CHECK(L.total_bytes() == 256ull * 6);
  CHECK(L.input_offset(0) == 0);
  CHECK(L.input_offset(3) == 256ull * 3);
  CHECK(L.output_offset(0) == 256ull * 4);
  CHECK(L.output_offset(1) == 256ull * 5);

  auto seg = ShmSegment::create("/mmm-cpp-test-shm", L.total_bytes());
  CHECK(seg.size() == L.total_bytes());
  // write floats into input slot 2, read them back
  float* p = seg.slot_floats(L.input_offset(2));
  for (int i = 0; i < 5; ++i) p[i] = float(i) + 0.5f;
  float* q = seg.slot_floats(L.input_offset(2));
  for (int i = 0; i < 5; ++i) CHECK(q[i] == float(i) + 0.5f);
  std::printf("test_shm OK\n");
  return 0;
}
```

- [ ] **Step 3: Write `CMakeLists.txt`** (builds the lib + registers tests as they are added):

```cmake
cmake_minimum_required(VERSION 3.16)
project(mmm_host CXX)
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
add_compile_options(-Wall -Wextra -fPIC)
find_package(Threads REQUIRED)

add_library(mmm_host STATIC mmm_shm.cpp mmm_protocol.cpp mmm_host.cpp)
target_include_directories(mmm_host PUBLIC ${CMAKE_CURRENT_SOURCE_DIR} ${CMAKE_CURRENT_SOURCE_DIR}/third_party)
target_link_libraries(mmm_host PUBLIC Threads::Threads rt)

enable_testing()
# Path to the debug worker binary, overridable; default assumes cargo target dir.
set(MMM_WORKER "${CMAKE_CURRENT_SOURCE_DIR}/../../../target/debug/mmm-ipc-worker" CACHE FILEPATH "worker binary")
set(MMM_CARGO_MANIFEST "${CMAKE_CURRENT_SOURCE_DIR}/../../../Cargo.toml")

foreach(t test_shm test_protocol)
  add_executable(${t} test/${t}.cpp)
  target_link_libraries(${t} PRIVATE mmm_host)
  add_test(NAME ${t} COMMAND ${t})
endforeach()
```

(The golden/isolation tests and their fixture wiring are added in Tasks 7–9; `mmm_protocol.cpp`/`mmm_host.cpp` are created in Tasks 5–6 — until then, compile `mmm_host` with just `mmm_shm.cpp`. To keep this task self-contained, temporarily list only `mmm_shm.cpp` in `add_library` and only `test_shm` in the foreach; later tasks extend both.)

- [ ] **Step 4: Implement `mmm_shm.h`/`.cpp`.** Header declares the interfaces above. In `.cpp`: `create` = `shm_unlink(name)` (ignore ENOENT) then `shm_open(name, O_CREAT|O_RDWR|O_EXCL, 0600)`, `ftruncate(fd, total_bytes)`, `mmap(NULL, total_bytes, PROT_READ|PROT_WRITE, MAP_SHARED, fd, 0)`, `close(fd)`; throw `std::runtime_error` with `strerror(errno)` on any failure. Destructor: `munmap` then `shm_unlink(name)` if this instance is the creator (guard a moved-from instance with a null base). `slot_floats`: `assert(byte_offset % 4 == 0)`, return `reinterpret_cast<float*>(base_ + byte_offset)`.

- [ ] **Step 5: Configure + build + run the test**

Run:
```bash
cd integration/pixinsight/host && cmake -S . -B build -DCMAKE_BUILD_TYPE=Debug >/dev/null && cmake --build build --target test_shm 2>&1 | tail && ctest --test-dir build -R test_shm --output-on-failure
```
Expected: `test_shm OK`, CTest 1/1 passed.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(pxhost): POSIX shm segment + SlotLayout (mirrors shm.rs)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: C++ `mmm_protocol` — frame codec + binary layouts + Init JSON

**Files:**
- Create: `integration/pixinsight/host/mmm_protocol.h`, `integration/pixinsight/host/mmm_protocol.cpp`
- Modify: `integration/pixinsight/host/CMakeLists.txt` (add `mmm_protocol.cpp` to lib; add `test_protocol`)
- Create: `integration/pixinsight/host/test/test_protocol.cpp`

**Interfaces:**
- Produces (all little-endian on the wire; match PROTOCOL.md §3–§6):
  ```cpp
  namespace mmm {
  enum class WorkerTag : uint8_t { BandRequest=1, Progress=2, Begin=3, OutputBand=4, Done=5, Error=6 };
  enum class HostTag   : uint8_t { Init=128, BandReply=129, OutputAck=130, Cancel=131 };

  struct BandRequest { uint32_t request_id, panel_id; uint64_t y0, y1; uint32_t slot_id; }; // 28B
  struct BandReply   { uint32_t request_id, slot_id; uint8_t status; };                     // 9B
  struct OutputBand  { uint32_t request_id; uint64_t y0, rows; uint32_t slot_id; };         // 24B

  // A decoded worker->host frame (binary or JSON) as a variant-ish struct:
  struct WorkerFrame {
    WorkerTag tag;
    BandRequest band_request;         // valid iff tag==BandRequest
    OutputBand  output_band;          // valid iff tag==OutputBand
    std::string progress_stage; uint64_t progress_done=0, progress_total=0; // Progress
    uint64_t begin_w=0, begin_h=0, begin_ch=0; // Begin
    std::string error_message;        // Error
  };

  // Low-level frame IO over fds (blocking). Return false on clean EOF before a
  // frame; throw std::runtime_error on a truncated/short frame or unknown tag.
  bool read_worker_frame(int fd, WorkerFrame& out);        // reads tag+len+payload
  void write_frame_raw(int fd, uint8_t tag, const uint8_t* payload, uint32_t len); // mutex-guarded by caller

  // Host->worker encoders (append into a byte buffer, then write_frame_raw):
  std::vector<uint8_t> encode_band_reply(const BandReply&);   // 9B
  std::vector<uint8_t> encode_output_ack(uint32_t request_id);// JSON {"OutputAck":{"request_id":N}}
  std::vector<uint8_t> encode_cancel();                       // JSON bare string "Cancel"

  // Init JSON. `panels_json` is a pre-built nlohmann array (opaque properties);
  // throws if any float reachable in the doc is non-finite (PROTOCOL.md §6).
  struct InitParams { /* mirrors BlendParamsWire: feather_px(f64), downsample(u32),
      band_rows(u32), mode(string), roi(optional array<4>), defect_veto(bool),
      flatten(optional u32), surface_order(optional u32) */ };
  std::vector<uint8_t> encode_init(const nlohmann::json& init_obj); // validates finiteness, returns framed bytes
  } // namespace mmm
  ```
- Consumes: `nlohmann/json` (vendored). Byte offsets exactly per PROTOCOL.md §5.

- [ ] **Step 1: Write the failing test** `test_protocol.cpp` — assert the exact byte layouts and a round trip:

```cpp
#include "../mmm_protocol.h"
#include "test_util.h"
#include <unistd.h>
#include <vector>
using namespace mmm;

static void test_band_reply_bytes() {
  BandReply r{ /*request_id*/ 0x11223344u, /*slot_id*/ 0x55667788u, /*status*/ 1 };
  auto b = encode_band_reply(r);
  CHECK(b.size() == 9);
  // little-endian request_id, slot_id, then status
  CHECK(b[0]==0x44 && b[1]==0x33 && b[2]==0x22 && b[3]==0x11);
  CHECK(b[4]==0x88 && b[5]==0x77 && b[6]==0x66 && b[7]==0x55);
  CHECK(b[8]==1);
}

static void test_read_bandrequest_roundtrip() {
  // Build a 28-byte BandRequest payload framed as tag=1,len=28, write to a pipe,
  // read it back via read_worker_frame.
  int fds[2]; CHECK(pipe(fds)==0);
  std::vector<uint8_t> frame;
  frame.push_back(1);                       // tag BandRequest
  uint32_t len=28; for(int i=0;i<4;i++) frame.push_back((len>>(8*i))&0xff);
  auto put32=[&](uint32_t v){ for(int i=0;i<4;i++) frame.push_back((v>>(8*i))&0xff); };
  auto put64=[&](uint64_t v){ for(int i=0;i<8;i++) frame.push_back((v>>(8*i))&0xff); };
  put32(7); put32(2); put64(16); put64(48); put32(3); // req,panel,y0,y1,slot
  CHECK(write(fds[1], frame.data(), frame.size()) == (ssize_t)frame.size());
  close(fds[1]);
  WorkerFrame wf;
  CHECK(read_worker_frame(fds[0], wf));
  CHECK(wf.tag == WorkerTag::BandRequest);
  CHECK(wf.band_request.request_id==7 && wf.band_request.panel_id==2);
  CHECK(wf.band_request.y0==16 && wf.band_request.y1==48 && wf.band_request.slot_id==3);
  close(fds[0]);
}

static void test_init_rejects_nonfinite() {
  nlohmann::json j; j["Init"]["params"]["feather_px"] = std::nan("");
  bool threw=false;
  try { encode_init(j); } catch(const std::exception&) { threw=true; }
  CHECK(threw);
}

int main(){ test_band_reply_bytes(); test_read_bandrequest_roundtrip(); test_init_rejects_nonfinite();
  std::printf("test_protocol OK\n"); return 0; }
```

- [ ] **Step 2: Add to CMake + run to verify it fails**

Add `mmm_protocol.cpp` to the `add_library` sources and `test_protocol` to the test `foreach`. Run:
```bash
cd integration/pixinsight/host && cmake --build build --target test_protocol 2>&1 | tail
```
Expected: FAIL — `mmm_protocol.h` / symbols not defined (link/compile error).

- [ ] **Step 3: Implement `mmm_protocol.{h,cpp}`.** Little-endian read/write helpers (`put_u32_le`, `get_u32_le`, `get_u64_le`). `read_worker_frame`: read 1 tag byte (clean EOF → return false); read 4-byte LE len (EOF mid-len → throw "truncated frame"); read `len` payload bytes (short read → throw). Dispatch on tag: 1 → parse 28-byte `BandRequest`; 4 → parse 24-byte `OutputBand`; 2/3/5/6 → parse payload as JSON with nlohmann (`{"Progress":{...}}`, `{"Begin":{...}}`, bare `"Done"`, `{"Error":{...}}`); any other tag → throw "unknown tag". `encode_band_reply`: 9 bytes LE. `encode_output_ack`/`encode_cancel`: build JSON, frame with the right host tag. `encode_init`: recursively walk `init_obj` — for every `is_number_float()` node, `CHECK`/throw if `!std::isfinite(value)`; then `dump()` to a string and frame under tag 128. `write_frame_raw`: write tag, 4-byte LE len, payload; single `write`/`writev` (caller holds the stdin mutex).

- [ ] **Step 4: Build + run the test to verify it passes**

Run: `cd integration/pixinsight/host && cmake --build build --target test_protocol 2>&1 | tail && ctest --test-dir build -R test_protocol --output-on-failure`
Expected: `test_protocol OK`, passed.

- [ ] **Step 5: Cross-check against Rust.** Re-read `crates/mmm-core/src/ipc/protocol.rs` and confirm: `BandRequest` field order/offsets (req, panel, y0, y1, slot), `BandReply` (req, slot, status), `OutputBand` (req, y0, rows, slot), tag numbers, and that `Done`/`Cancel` serialize as bare JSON strings. Fix any mismatch and re-run.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(pxhost): wire codec, binary band layouts, Init JSON (finite-checked)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: C++ `mmm_host` — spawn, serve loop, collector, fault isolation

**Files:**
- Create: `integration/pixinsight/host/mmm_host.h`, `integration/pixinsight/host/mmm_host.cpp`
- Modify: `integration/pixinsight/host/CMakeLists.txt` (add `mmm_host.cpp` to lib)

**Interfaces:**
- Produces:
  ```cpp
  namespace mmm {
  // Where input pixels come from (the module supplies ImageVariant rows; tests
  // supply memory). Fill rows [y0,y1) of every channel of panel `panel_id`
  // into `dst` in planar native-endian order: index(c,r,x)=c*rows*w+r*w+x.
  // Return false to signal an out-of-range/failed fill (host replies status=1).
  struct PanelSource {
    virtual ~PanelSource() = default;
    virtual bool fill_band(uint32_t panel_id, uint64_t y0, uint64_t y1, float* dst) = 0;
  };
  // Receives output geometry + bands (module writes into ImageWindow; tests buffer).
  struct OutputCollector {
    virtual ~OutputCollector() = default;
    virtual void begin(uint64_t w, uint64_t h, uint64_t ch) = 0;
    virtual void band(uint64_t y0, uint64_t rows, const float* planar, uint64_t width, uint64_t ch) = 0;
  };
  struct ProgressCallback { virtual ~ProgressCallback()=default;
    virtual void on_progress(const std::string& stage, uint64_t done, uint64_t total) {} };

  struct HostConfig {
    std::string worker_path;        // absolute path to mmm-ipc-worker
    nlohmann::json init;            // the full {"Init":{...}} object (panels, mode, params, ...)
    SlotLayout layout;              // must match init.shm sizing
    std::string shm_name;
  };
  // Drives one job to completion. Throws HostError on worker crash/exit-without-Done,
  // protocol error, or a worker Error frame. Returns normally on Done.
  struct HostError : std::runtime_error { using std::runtime_error::runtime_error; };
  class Host {
  public:
    Host(HostConfig cfg, PanelSource& src, OutputCollector& out, ProgressCallback* prog=nullptr);
    void run();                     // blocking; performs the whole handshake+serve
    void cancel();                  // thread-safe; sends Cancel
    // Solved-mode helper: spawn `worker_path --probe-frame`, write `init_for_probe`
    // JSON to its stdin, parse "w h ch" from stdout. Static, no shm.
    static void probe_frame(const std::string& worker_path, const nlohmann::json& init_obj,
                            uint64_t& w, uint64_t& h, uint64_t& ch);
  };
  } // namespace mmm
  ```
- Consumes: `mmm_shm`, `mmm_protocol`. Mirrors the serve loop in `crates/mmm-core/src/ipc/testhost.rs` (`run_host`) and the handshake in `end_to_end.rs`.

- [ ] **Step 1: Implement first (this task's test IS the golden test in Task 7).** Because `Host` cannot be meaningfully unit-tested without the worker, implement it here and let Task 7's `test_golden_aligned` be its first executable proof. Write `mmm_host.{h,cpp}`:
  - **Create shm** from `cfg.layout`/`cfg.shm_name` (Task 4).
  - **Spawn** `cfg.worker_path` via `posix_spawn` with stdin/stdout redirected to pipes (`posix_spawn_file_actions_adddup2`), stderr inherited. Keep the worker pid.
  - **Send Init**: `encode_init(cfg.init)` → `write_frame_raw` to the worker's stdin (hold a `std::mutex` for all stdin writes).
  - **Reader loop** (on the calling thread is fine for the test; or a dedicated thread): `read_worker_frame(stdout_fd, wf)`:
    - `BandRequest`: look up panel width/height/channels from `cfg.init["Init"]["panels"]`; `float* dst = shm.slot_floats(layout.input_offset(slot_id))`; `bool ok = src.fill_band(panel_id, y0, y1, dst)`; reply `encode_band_reply({request_id, slot_id, ok?0:1})`.
    - `Begin`: `out.begin(w,h,ch)`; remember `ch`/width for output copies.
    - `OutputBand`: `const float* p = shm.slot_floats(layout.output_offset(slot_id))`; `out.band(y0, rows, p, out_w, out_ch)`; reply `encode_output_ack(request_id)`.
    - `Progress`: `if (prog) prog->on_progress(stage, done, total)`.
    - `Done`: break; mark success.
    - `Error`: throw `HostError(message)`.
    - **Clean EOF (`read_worker_frame` returns false) before `Done`** → throw `HostError("worker exited before Done")`.
  - **After the loop**: `waitpid` the worker. If it exited nonzero and we did **not** see `Done`, throw `HostError`. On any throw, ensure the shm is unlinked (RAII via `ShmSegment` destructor) and **no partial output is surfaced** (the collector's buffer is the caller's; the caller discards it on exception — document this).
  - **`cancel()`**: set an atomic flag and `write_frame_raw` a `Cancel` frame under the stdin mutex; the reader loop then expects an `Error{"cancelled"}` (treated as intentional — `run()` may translate a post-cancel `HostError("cancelled")` into a normal return or a distinct `Cancelled` signal; document the choice and assert it in Task 9).
  - **`probe_frame()`**: `posix_spawn` `worker_path` with arg `--probe-frame`, stdin/stdout piped; write `init_obj.dump()`; read all stdout; `sscanf`/parse three integers; `waitpid`; throw on nonzero exit.

- [ ] **Step 2: Add `mmm_host.cpp` to the CMake library, build the lib**

Run: `cd integration/pixinsight/host && cmake --build build --target mmm_host 2>&1 | tail`
Expected: compiles cleanly (no test yet — proven in Task 7).

- [ ] **Step 3: Commit the library (unproven until Task 7, but compiles)**

```bash
git add -A && git commit -m "feat(pxhost): Host driver — spawn, serve loop, collector, probe, isolation

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 7: Fixture generator + aligned byte-identity golden test

**Files:**
- Create: `crates/mmm-ipc-worker/examples/gen_fixtures.rs`
- Create: `integration/pixinsight/host/test/test_golden_aligned.cpp`
- Modify: `integration/pixinsight/host/CMakeLists.txt` (fixture generation + `test_golden_aligned`)

**Interfaces:**
- Produces: `gen_fixtures <out_dir>` writes, for a small deterministic 2-panel aligned scene: `panel0.xisf`, `panel1.xisf` (via `mmm_core::synth::write_xisf`), `panel0.bin`, `panel1.bin` (raw planar `f32` native-endian, the same pixels), and `meta.json` = `{ "canvas":[w,h,ch], "panels":[{"id":0,"w":..,"h":..,"ch":..},...], "band_rows":16, "feather_px":16.0 }`. Reuses the exact fixture shape as `end_to_end.rs::synth_two_panels`.
- The C++ test consumes `meta.json` + `panelN.bin` (memory `PanelSource`) + `panelN.xisf` (worker Files-mode golden).

- [ ] **Step 1: Write `gen_fixtures.rs`.** Mirror `synth_two_panels` (128×64×1, A=0.2 over [8,80)×[8,56), B=0.4 over [48,120)×[16,64)). Write each panel to `.xisf` (`write_xisf`) and to `.bin` (`std::fs::write` of the planar `f32` bytes via `bytemuck`/`to_le`... use native-endian: write `&[f32]` as bytes with `slice::align_to` or a manual loop). Write `meta.json` with `serde_json`. Keep it dependency-light (serde_json is already a dep).

- [ ] **Step 2: Write the failing test** `test_golden_aligned.cpp`. Two runs of the real worker via the C++ `Host`:
  1. **Files-mode golden**: build `init` with `mode = {"Files":{"paths":[panel0.xisf,panel1.xisf],"input_select":"Auto"}}`, empty-ish panels (still list `panel_id/width/height/channels` from meta), size `slot_bytes = w*ch*band_rows*4`, a `PanelSource` that never gets called (files read by worker), an `OutputCollector` capturing into `std::vector<float> golden`.
  2. **Aligned-mode**: `mode="Aligned"`, a memory `PanelSource` reading `panelN.bin`, capture into `std::vector<float> got`.
  Assert `got == golden` element-for-element, and that `golden` is non-empty and non-constant (guard against vacuous pass).

```cpp
// sketch of the memory PanelSource
struct MemSource : mmm::PanelSource {
  std::vector<std::vector<float>> panels; std::vector<uint64_t> w,h,ch;
  bool fill_band(uint32_t id, uint64_t y0, uint64_t y1, float* dst) override {
    const auto& p = panels[id]; uint64_t W=w[id], H=h[id], C=ch[id], rows=y1-y0;
    for (uint64_t c=0;c<C;c++) for (uint64_t r=0;r<rows;r++)
      std::memcpy(dst + (c*rows+r)*W, &p[(c*H + y0 + r)*W], W*sizeof(float));
    return true;
  }
};
struct BufCollector : mmm::OutputCollector {
  uint64_t W=0,H=0,C=0; std::vector<float> data;
  void begin(uint64_t w,uint64_t h,uint64_t c) override { W=w;H=h;C=c; data.assign(w*h*c,0.f); }
  void band(uint64_t y0,uint64_t rows,const float* p,uint64_t width,uint64_t c) override {
    for (uint64_t ch=0;ch<c;ch++) for (uint64_t r=0;r<rows;r++)
      std::memcpy(&data[(ch*H + y0 + r)*width], p + (ch*rows+r)*width, width*sizeof(float));
  }
};
```

(Use the same `BlendParamsWire` values as `end_to_end.rs::blend_params`: `feather_px=16`, `mode="pyramid"`, `downsample=1`, `defect_veto=true`, `surface_order=2`. Give each run its own `session_dir` under a temp dir, and a unique `shm_name` per run.)

- [ ] **Step 3: Wire fixtures + test into CMake.** Add a custom command that runs the generator into `${CMAKE_BINARY_DIR}/fixtures` before the test, and register the test with that dir + the worker path:

```cmake
add_custom_target(gen_fixtures
  COMMAND cargo run --quiet --manifest-path ${MMM_CARGO_MANIFEST} -p mmm-ipc-worker --example gen_fixtures -- ${CMAKE_BINARY_DIR}/fixtures
  BYPRODUCTS ${CMAKE_BINARY_DIR}/fixtures/meta.json)
add_executable(test_golden_aligned test/test_golden_aligned.cpp)
target_link_libraries(test_golden_aligned PRIVATE mmm_host)
add_dependencies(test_golden_aligned gen_fixtures)
add_test(NAME test_golden_aligned COMMAND test_golden_aligned ${CMAKE_BINARY_DIR}/fixtures ${MMM_WORKER})
```

(The test reads `argv[1]`=fixtures dir, `argv[2]`=worker path.)

- [ ] **Step 4: Build the worker (release-independent debug is fine) + run the test**

Run:
```bash
source ~/.cargo/env && cargo build -p mmm-ipc-worker 2>&1 | tail -3
cd integration/pixinsight/host && cmake --build build --target test_golden_aligned 2>&1 | tail && ctest --test-dir build -R test_golden_aligned --output-on-failure
```
Expected: `test_golden_aligned` passes — aligned IPC output byte-identical to the files-mode golden.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "test(pxhost): aligned byte-identity golden vs files-mode worker

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 8: Solved-mode golden test (probe → size → serve)

**Files:**
- Modify: `crates/mmm-ipc-worker/examples/gen_fixtures.rs` (also emit solved fixtures)
- Create: `integration/pixinsight/host/test/test_golden_solved.cpp`
- Modify: `integration/pixinsight/host/CMakeLists.txt`

**Interfaces:**
- Produces (fixtures): `solved0.xisf`,`solved1.xisf` (via `synth::write_xisf_solved`), `solved0.bin`,`solved1.bin` (raw planar pixels), `solved_props.json` = the two panels' `properties` arrays (serialize `XisfPanel::open(p).header().properties` with serde_json), and `solved_meta.json` (each panel `w/h/ch`, `band_rows`, `feather_px`, `ch`). Reuse `end_to_end.rs::write_two_solved_panels`' geometry/WCS.
- Consumes: `Host::probe_frame` to size output slots.

- [ ] **Step 1: Extend `gen_fixtures.rs`** to also write the solved fixtures (mirror `write_two_solved_panels`), including `solved_props.json` (the `properties` from each written panel's header — reopen via `XisfPanel::open` to get exactly what the worker will parse) and `solved_meta.json`.

- [ ] **Step 2: Write the failing test** `test_golden_solved.cpp`:
  1. **Files(Solved) golden**: `mode={"Files":{"paths":[solved0.xisf,solved1.xisf],"input_select":"Solved"}}`; panels list each raw `w/h/ch` (empty properties are fine for Files — the worker reads the files). Size `slot_bytes` for the golden run using the same rule as step 3 (probe). Capture golden output.
  2. **Solved (shm) run**: build `init` panels with `properties` spliced from `solved_props.json` (load with nlohmann, assign into each panel object). Call `mmm::Host::probe_frame(worker, init, fw, fh, fch)`. Size `slot_bytes = max(max_panel_width, fw) * ch * band_rows * 4`. `mode="Solved"`. A `MemSource` serves `solvedN.bin` (raw panel pixels, each its own `w/h`). Capture output.
  3. Assert `got == golden`, non-empty, non-constant.

- [ ] **Step 3: Wire into CMake** (same pattern as Task 7; depends on `gen_fixtures`; pass fixtures dir + worker path as argv).

- [ ] **Step 4: Build + run**

Run: `cd integration/pixinsight/host && cmake --build build --target test_golden_solved 2>&1 | tail && ctest --test-dir build -R test_golden_solved --output-on-failure`
Expected: PASS — solved IPC output byte-identical to the files(solved) golden; confirms `probe_frame` sized the slots correctly (an undersized slot would corrupt and fail the byte compare).

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "test(pxhost): solved-mode golden via --probe-frame sizing

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 9: Fault-isolation tests — crash + cancel

**Files:**
- Create: `integration/pixinsight/host/test/test_isolation.cpp`
- Modify: `integration/pixinsight/host/CMakeLists.txt`

**Interfaces:**
- Consumes: `Host::run`, `Host::cancel`, `HostError`. Mirrors `end_to_end.rs::worker_crash_is_observable` and `cancel_midrun_stops_promptly`.

- [ ] **Step 1: Write the failing test** `test_isolation.cpp` with a wall-clock watchdog (a background thread that `std::abort`s the process after e.g. 20s so a hang fails loudly rather than hanging CI):
  - **crash case**: a `PanelSource` whose `fill_band` returns after the test has `kill`ed the worker — or simpler, point `Host` at a `worker_path` that is a tiny script exiting immediately / send `SIGKILL` to the child right after spawn. Assert `run()` throws `HostError` **promptly** (within the watchdog) and the `OutputCollector` never received a `begin`/`band` (no partial output). Since `Host` owns the pid, expose a test seam: either a `worker_path` pointing at `/bin/false` (exits nonzero immediately, no frames) to prove "exit before Done → HostError", or add a `Host` test hook to kill the child mid-run. Prefer the `/bin/false` variant for the "exit without Done" guarantee, plus a variant using the real worker killed via an injected `PanelSource` that calls a supplied "kill the worker" callback on the first `fill_band`.
  - **cancel case**: real worker, a `MemSource` serving a handful of bands; from a second thread, call `host.cancel()` after the first few `fill_band` calls; assert `run()` returns/raises the documented cancelled outcome promptly, no full output was produced (collector saw no `Done`-completing band set), and the worker process is reaped.

- [ ] **Step 2: Add to CMake + run to verify it fails** (test not yet linked / behavior unproven).

Run: `cd integration/pixinsight/host && cmake --build build --target test_isolation 2>&1 | tail`
Expected: FAIL to compile/link until the test file + any needed `Host` seam exist.

- [ ] **Step 3: Implement** any minimal `Host` seam the tests need (e.g. a `std::function<void()> on_first_fill` hook is cleaner kept in the test's `PanelSource` — it can capture the worker pid if `Host` exposes `worker_pid()` for tests, or the test triggers cancel instead of kill). Keep production `Host` API unchanged where possible; prefer driving crash via `/bin/false` and mid-run death via `cancel()` + a genuinely killed child only if a seam is truly needed. Make the tests pass.

- [ ] **Step 4: Run both isolation cases**

Run: `cd integration/pixinsight/host && cmake --build build --target test_isolation 2>&1 | tail && ctest --test-dir build -R test_isolation --output-on-failure`
Expected: PASS — crash → prompt `HostError`, no partial output; cancel → prompt stop, worker reaped. Neither hangs.

- [ ] **Step 5: Full CTest sweep**

Run: `cd integration/pixinsight/host && ctest --test-dir build --output-on-failure`
Expected: all of `test_shm`, `test_protocol`, `test_golden_aligned`, `test_golden_solved`, `test_isolation` pass.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "test(pxhost): worker crash + cancel isolation (no hang, no partial output)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 10: Host README + full verification

**Files:**
- Create: `integration/pixinsight/host/README.md`

- [ ] **Step 1: Write `README.md`** documenting: what the host library is (the PCL-free transport layer; Plan 2b adds the PCL module on top), the vendored `nlohmann/json` (version + license), how to build (`cargo build -p mmm-ipc-worker`, then `cmake -S . -B build && cmake --build build`), how to run tests (`ctest --test-dir build --output-on-failure`), the `-DMMM_WORKER=` override, and that the tests spawn the real worker + generate fixtures via `cargo run --example gen_fixtures` (so cargo must be on `PATH`).

- [ ] **Step 2: Final green sweep (Rust + C++)**

Run:
```bash
source ~/.cargo/env && cargo test --workspace 2>&1 | grep -E "test result:|error" | tail
cd integration/pixinsight/host && rm -rf build && cmake -S . -B build >/dev/null && cmake --build build 2>&1 | tail -3 && ctest --test-dir build --output-on-failure
```
Expected: Rust workspace all `ok`; all 5 C++ CTests pass from a clean build.

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "docs(pxhost): host library README + build/test instructions

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review notes (for the executor)

- **Spec coverage:** Task 1 ↔ spec §15.3 + §10.1 override; Task 2 ↔ §15.1 (probe) + §7/§11 sizing hazard; Task 3 ↔ §15.2 (progress); Tasks 4–6 ↔ §14.1 (`mmm_shm`/`mmm_protocol`/`mmm_host`, PCL-free invariant); Tasks 7–9 ↔ §17 (aligned/solved golden identity, crash, cancel) mirroring `end_to_end.rs`; Task 10 ↔ §16 build + §11 testing. **Plan 2b** covers §10 UI, §12 packaging, §16 module makefile — not this plan.
- **Type consistency:** `InputSelectWire` (Task 1) is the only new Rust type and is used only by `main.rs`. C++ names (`SlotLayout`, `ShmSegment`, `WorkerFrame`, `BandRequest/BandReply/OutputBand`, `PanelSource`, `OutputCollector`, `Host`, `HostError`, `probe_frame`) are defined in Tasks 4–6 and consumed unchanged in Tasks 7–9.
- **Wire cross-checks:** Tasks 5 Step 5 and 7/8 re-verify byte layouts and the byte-identity property against the Rust source, the same guarantee `end_to_end.rs` gives in-language.
- **Known judgement calls left to the executor:** the exact `ShmRowSink` field names (Task 3 Step 4) and the cancel-outcome representation in `Host` (Task 6/9) — both call out "inspect and adapt / document the choice."
