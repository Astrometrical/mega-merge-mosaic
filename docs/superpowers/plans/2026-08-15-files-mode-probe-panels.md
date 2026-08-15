# Files-Mode Worker-Side Metadata Probe Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move the PixInsight module's Files-mode per-file metadata pass (geometry + astrometric solution) off the GUI thread into the Rust worker, behind the same event-pumped wait loop `Host::run()` already uses, so an 80-panel run never freezes PixInsight.

**Architecture:** A new `mmm-ipc-worker --probe-panels` mode reads the panel headers (in parallel, via mmm-core's own header-only XISF reader) and returns per-panel geometry plus the solved mosaic frame in one JSON reply. The C++ host gains a shared pumped-probe helper (spawn worker, write stdin, drain stdout+stderr with `on_idle()` pumping and abort support) used by both the new `Host::probe_panels` and the existing `Host::probe_frame`. `RunFiles` in the PCL module then drops its PCL `FileFormatInstance` loop entirely.

**Tech Stack:** Rust (mmm-core, mmm-ipc-worker, serde/rayon), C++20 (mmm_host static lib, PCL module), CMake/ctest, cargo test.

**Spec:** No separate spec file — the design is recorded in this plan's Background section and in the conversation that produced it. PROTOCOL.md §11 is the normative wire contract and is updated in Task 2.

## Background (why)

`RunFiles` (integration/pixinsight/module/MmmExecution.cpp:675) currently opens every input file on the GUI thread via `FileFormatInstance::Open()` + `ReadImageProperties()` with no `ProcessEvents()` pumping and no abort path; `Host::probe_frame` is also fully blocking. The v1.2 `on_idle` pump only covers `Host::run()`. With 80 × 2 GB panels (cold cache, possible AV scanning) this phase freezes PixInsight for minutes. The fix moves all file reading into the worker process and pumps the GUI while waiting.

Key existing pieces reused:
- `mmm_core::formats::xisf::XisfPanel::open` — mmaps, parses XML header + resolves attached astrometry properties; never touches pixel data.
- `mmm_core::analyze::solved_frame(&[PanelDesc])` — builds WCS models + `choose_frame`; already shared with `--probe-frame`.
- `mmm::Host` pumped wait: `os_wait_readable(fd, kIdleWaitMs)` → `prog_->on_idle()`.
- Host slot sizing rule being preserved exactly: `width = max(max input width, solved frame width if the job can resolve to solved)`, `slot_bytes = width * ch0 * band_rows * 4`.

## Global Constraints

- Every public item in `mmm-core` needs a doc comment (`missing_docs` warned); `cargo fmt`, `cargo clippy --all-targets`, `cargo doc` warning-free.
- Tests must not depend on `test_data/` — synthesize inputs (`mmm_core::synth`).
- Any wire-format change must update `integration/pixinsight/PROTOCOL.md` in the same change (§12 rule).
- `blend.rs` convention n/a here; C++ host lib stays PCL-free (only the module links PCL).
- Windows and POSIX must both stay correct in `mmm_os.h` / `mmm_host.cpp` (module ships on Windows; only POSIX is compilable here — keep Windows code paths carefully mirrored).

---

### Task 1: mmm-core — probe wire types + `probe_panels()`

**Files:**
- Modify: `crates/mmm-core/src/ipc/protocol.rs` (add `PanelProbeRequest`, `PanelProbeGeom`, `PanelProbeReply` near `PanelDesc`)
- Modify: `crates/mmm-core/src/analyze.rs` (add `probe_panels` near `solved_frame`)
- Test: `crates/mmm-core/tests/probe_panels.rs` (new)

**Interfaces:**
- Produces (used by Task 2):
  - `mmm_core::ipc::protocol::PanelProbeRequest { paths: Vec<String>, input_select: InputSelectWire }` (Serialize, Deserialize; `input_select` defaults to `Auto` via `#[serde(default)]` like `JobMode::Files` does)
  - `mmm_core::ipc::protocol::PanelProbeGeom { width: u64, height: u64, channels: u64 }`
  - `mmm_core::ipc::protocol::PanelProbeReply { panels: Vec<PanelProbeGeom>, frame: Option<[u64; 3]> }`
  - `mmm_core::analyze::probe_panels(paths: &[PathBuf], input: InputSelect) -> Result<PanelProbeReply>`

**Semantics of `probe_panels`** (mirrors today's host logic exactly, plus fail-fast):
- Error on empty `paths`.
- Open every path with `XisfPanel::open` **in parallel** (`par_iter`, as `analyze_aligned`'s scan does); any open error propagates (its message already carries the path).
- Build a `PanelDesc` per panel (`panel_id = index`, header properties attached) solely to feed `solved_frame`.
- `frame`:
  - `InputSelect::Aligned` → `None` (host's old `canSolve == false`).
  - Else try `solved_frame(&descs)`: `Ok((_, frame, ch))` → `Some([frame.width, frame.height, ch])`.
  - `Err(reason)` with `InputSelect::Solved` → `Err(Error::compute(reason))` (fail fast; the run would fail with this same message later).
  - `Err(_)` with `InputSelect::Auto` → `None` (matches today: no probe → size by input widths; the aligned path proceeds, or analyze errors identically later).

- [ ] **Step 1: Write failing test** `crates/mmm-core/tests/probe_panels.rs`:

```rust
//! Tests for [`mmm_core::analyze::probe_panels`] — the Files-mode metadata
//! probe the IPC worker exposes as `--probe-panels`.

use std::path::PathBuf;

use mmm_core::analyze::{InputSelect, probe_panels, solved_frame};
use mmm_core::formats::xisf::XisfPanel;
use mmm_core::ipc::protocol::PanelDesc;
use mmm_core::synth::{SynthWcs, write_xisf, write_xisf_solved};

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mmm-probe-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Two small plate-solved raw panels (mirrors end_to_end.rs's fixture shape).
fn write_solved(dir: &PathBuf) -> Vec<PathBuf> {
    let scale_deg = 1.0e-3_f64;
    let mut paths = Vec::new();
    for (k, (w, h, crval)) in [
        (64u64, 48u64, [10.0, 0.0]),
        (60, 52, [10.0 + 64.0 * scale_deg * 0.55, 8.0 * scale_deg]),
    ]
    .into_iter()
    .enumerate()
    {
        let planes = vec![0.5f32; (w * h) as usize];
        let wcs = SynthWcs {
            crval,
            refimg: [w as f64 / 2.0, h as f64 / 2.0],
            cd: [[-scale_deg, 0.0], [0.0, scale_deg]],
        };
        let path = dir.join(format!("solved_{k}.xisf"));
        write_xisf_solved(&path, w, h, 1, &planes, &wcs).unwrap();
        paths.push(path);
    }
    paths
}

#[test]
fn solved_panels_report_geometry_and_frame() {
    let dir = tmpdir("solved");
    let paths = write_solved(&dir);

    // Expected frame straight from solved_frame over the file headers.
    let descs: Vec<PanelDesc> = paths
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let x = XisfPanel::open(p).unwrap();
            PanelDesc {
                panel_id: i as u32,
                width: x.width(),
                height: x.height(),
                channels: x.channels(),
                properties: x.header().properties.clone(),
            }
        })
        .collect();
    let (_, frame, ch) = solved_frame(&descs).unwrap();

    let reply = probe_panels(&paths, InputSelect::Auto).unwrap();
    assert_eq!(reply.panels.len(), 2);
    assert_eq!(
        (reply.panels[0].width, reply.panels[0].height, reply.panels[0].channels),
        (64, 48, 1)
    );
    assert_eq!((reply.panels[1].width, reply.panels[1].height), (60, 52));
    assert_eq!(reply.frame, Some([frame.width, frame.height, ch]));

    // Explicit Solved gives the same frame; explicit Aligned suppresses it.
    assert_eq!(probe_panels(&paths, InputSelect::Solved).unwrap().frame,
               reply.frame);
    assert_eq!(probe_panels(&paths, InputSelect::Aligned).unwrap().frame, None);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn unsolved_panels_have_no_frame_in_auto_and_fail_in_solved() {
    let dir = tmpdir("unsolved");
    let mut paths = Vec::new();
    for k in 0..2u64 {
        let path = dir.join(format!("plain_{k}.xisf"));
        write_xisf(&path, 8, 6, 1, &vec![0.25f32; 48]).unwrap();
        paths.push(path);
    }

    let reply = probe_panels(&paths, InputSelect::Auto).unwrap();
    assert_eq!(reply.frame, None);
    assert_eq!(reply.panels.len(), 2);
    assert_eq!(
        (reply.panels[0].width, reply.panels[0].height, reply.panels[0].channels),
        (8, 6, 1)
    );

    let err = probe_panels(&paths, InputSelect::Solved).unwrap_err();
    assert!(err.to_string().contains("astrometric"), "got: {err}");

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn missing_file_errors_with_path() {
    let dir = tmpdir("missing");
    let good = dir.join("good.xisf");
    write_xisf(&good, 4, 4, 1, &vec![0.1f32; 16]).unwrap();
    let bad = dir.join("nope.xisf");
    let err = probe_panels(&[good, bad.clone()], InputSelect::Auto).unwrap_err();
    assert!(err.to_string().contains("nope.xisf"), "got: {err}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn empty_paths_error() {
    assert!(probe_panels(&[], InputSelect::Auto).is_err());
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p mmm-core --test probe_panels` → compile error (`probe_panels` not found).

- [ ] **Step 3: Implement.** In `protocol.rs`, after `PanelDesc` (~line 207):

```rust
/// Request read by `mmm-ipc-worker --probe-panels` from stdin: a bare JSON
/// object (unframed, like `--probe-frame`). See PROTOCOL.md §11.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct PanelProbeRequest {
    /// Panel file paths, one per panel, in `panels` order.
    pub paths: Vec<String>,
    /// Aligned-vs-solved override, as in [`JobMode::Files`]. Defaults to
    /// `Auto` when omitted.
    #[serde(default)]
    pub input_select: InputSelectWire,
}

/// One panel's header geometry in a [`PanelProbeReply`].
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub struct PanelProbeGeom {
    /// Panel width in pixels.
    pub width: u64,
    /// Panel height in pixels.
    pub height: u64,
    /// Channel count.
    pub channels: u64,
}

/// Reply printed by `--probe-panels` on stdout as one bare JSON object.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct PanelProbeReply {
    /// Per-panel header geometry, in request `paths` order.
    pub panels: Vec<PanelProbeGeom>,
    /// `Some([w, h, ch])` — the worker's `choose_frame` result — when the
    /// job can resolve to solved mode (`input_select` is not `Aligned` and
    /// every panel carries a usable astrometric solution); `None` otherwise.
    /// Hosts size output slots by `max(max panel width, frame width)`.
    pub frame: Option<[u64; 3]>,
}
```

`InputSelectWire` needs `#[derive(Default)]` with `#[default] Auto` (check current derives; add `Default` if absent).

In `analyze.rs`, after `solved_frame` (~line 426):

```rust
/// Files-mode metadata probe (PROTOCOL.md §11, `--probe-panels`): read each
/// panel's header — geometry plus astrometric properties, never pixel data —
/// and report per-panel geometry along with the solved mosaic frame when the
/// job can resolve to solved mode. Lets a GUI host size shm slots for a
/// Files-mode run without opening any panel file on its own thread.
///
/// `frame` follows the same rule the PixInsight host previously implemented
/// itself: `input` = `Aligned` never probes a frame; `Solved` requires every
/// panel to solve (erroring otherwise, with the same message the analyze
/// stage would produce); `Auto` degrades to `None` when any panel lacks a
/// usable solution. Header reads run in parallel.
pub fn probe_panels(paths: &[PathBuf], input: InputSelect) -> Result<PanelProbeReply> {
    if paths.is_empty() {
        return Err(Error::compute("probe-panels: no input panels given"));
    }
    let descs: Vec<PanelDesc> = paths
        .par_iter()
        .enumerate()
        .map(|(id, path)| {
            let x = XisfPanel::open(path)?;
            Ok(PanelDesc {
                panel_id: id as u32,
                width: x.width(),
                height: x.height(),
                channels: x.channels(),
                properties: x.header().properties.clone(),
            })
        })
        .collect::<Result<_>>()?;

    let panels = descs
        .iter()
        .map(|d| PanelProbeGeom {
            width: d.width,
            height: d.height,
            channels: d.channels,
        })
        .collect();

    let frame = match input {
        InputSelect::Aligned => None,
        InputSelect::Solved => {
            let (_, frame, ch) = solved_frame(&descs).map_err(Error::compute)?;
            Some([frame.width, frame.height, ch])
        }
        InputSelect::Auto => solved_frame(&descs)
            .ok()
            .map(|(_, frame, ch)| [frame.width, frame.height, ch]),
    };

    Ok(PanelProbeReply { panels, frame })
}
```

Add the needed imports (`PanelProbeGeom`, `PanelProbeReply` from `crate::ipc::protocol`; `PanelDesc` is already imported for `solved_frame`).

- [ ] **Step 4: Run** `cargo test -p mmm-core --test probe_panels` → PASS; `cargo test -p mmm-core` → all green.

- [ ] **Step 5: Commit** — `feat(core): probe_panels metadata probe for Files-mode hosts`

---

### Task 2: worker `--probe-panels` mode + PROTOCOL.md §11

**Files:**
- Modify: `crates/mmm-ipc-worker/src/main.rs`
- Test: `crates/mmm-ipc-worker/tests/end_to_end.rs` (new test fn)
- Modify: `integration/pixinsight/PROTOCOL.md` §11 (same commit — §12 rule)

**Interfaces:**
- Consumes: `mmm_core::analyze::probe_panels`, `PanelProbeRequest`/`PanelProbeReply` (Task 1).
- Produces (used by Task 3): child process contract — `mmm-ipc-worker --probe-panels` reads one bare JSON `PanelProbeRequest` on stdin (to EOF), writes one bare JSON `PanelProbeReply` line on stdout, exit 0; on error writes a message to stderr and exits 1.

- [ ] **Step 1: Failing test** in `end_to_end.rs` (after `probe_frame_prints_choose_frame_geometry`):

```rust
/// `--probe-panels`: paths in, per-panel geometry + solved frame out —
/// the metadata pass a Files-mode PixInsight host runs instead of opening
/// panels itself (PROTOCOL.md §11).
#[test]
fn probe_panels_reports_geometry_and_frame() {
    use std::io::Write;
    let dir = tmpdir("probe-panels");
    let (paths, _planars) = write_two_solved_panels(&dir);

    // Expected frame, computed exactly as the worker will.
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

    let req = serde_json::json!({
        "paths": paths.iter().map(|p| p.to_str().unwrap()).collect::<Vec<_>>(),
        "input_select": "Auto",
    });

    let exe = env!("CARGO_BIN_EXE_mmm-ipc-worker");
    let mut child = std::process::Command::new(exe)
        .arg("--probe-panels")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(req.to_string().as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "probe exited nonzero: {}",
            String::from_utf8_lossy(&out.stderr));

    let reply: mmm_core::ipc::protocol::PanelProbeReply =
        serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(reply.panels.len(), 2);
    assert_eq!((reply.panels[0].width, reply.panels[0].height, reply.panels[0].channels),
               (headers[0].0, headers[0].1, headers[0].2));
    assert_eq!(reply.frame, Some([frame.width, frame.height, headers[0].2]));

    std::fs::remove_dir_all(&dir).unwrap();
}
```

Add `PanelProbeReply` to the existing `mmm_core::ipc::protocol` import list if the test references it unqualified.

- [ ] **Step 2: Verify failure** — `cargo test -p mmm-ipc-worker --test end_to_end probe_panels` → the worker treats `--probe-panels` as a normal run and fails (or hangs → the frame-read errors on JSON input); expect a failing/erroring test.

- [ ] **Step 3: Implement** in `main.rs`:

```rust
fn main() {
    let probe_frame_mode = std::env::args().any(|a| a == "--probe-frame");
    let probe_panels_mode = std::env::args().any(|a| a == "--probe-panels");
    let result = if probe_frame_mode {
        probe_frame()
    } else if probe_panels_mode {
        probe_panels()
    } else {
        run()
    };
    if let Err(e) = result {
        let _ = writeln!(std::io::stderr(), "mmm-ipc-worker: {e}");
        std::process::exit(1);
    }
}

/// `--probe-panels`: read a `PanelProbeRequest` JSON object on stdin, open
/// each panel's header via [`mmm_core::analyze::probe_panels`] (parallel,
/// header-only — no pixel reads), and print the `PanelProbeReply` JSON on
/// stdout. The Files-mode metadata pass a GUI host delegates to this
/// process so its own thread never touches the panel files (PROTOCOL.md §11).
fn probe_panels() -> mmm_core::Result<()> {
    let mut buf = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
        .map_err(|e| mmm_core::Error::compute(format!("reading probe JSON from stdin: {e}")))?;
    let req: mmm_core::ipc::protocol::PanelProbeRequest = serde_json::from_str(&buf)
        .map_err(|e| mmm_core::Error::compute(format!("parsing PanelProbeRequest JSON: {e}")))?;
    let paths: Vec<PathBuf> = req.paths.iter().map(PathBuf::from).collect();
    let reply = mmm_core::analyze::probe_panels(&paths, req.input_select.to_input_select())?;
    let text = serde_json::to_string(&reply)
        .map_err(|e| mmm_core::Error::compute(format!("encoding PanelProbeReply: {e}")))?;
    writeln!(std::io::stdout(), "{text}")
        .map_err(|e| mmm_core::Error::compute(format!("probe-panels: writing stdout: {e}")))
}
```

- [ ] **Step 4: Run** `cargo test -p mmm-ipc-worker --test end_to_end probe_panels` → PASS.

- [ ] **Step 5: Update PROTOCOL.md §11** — after the "Frame probe" paragraph add:

```markdown
**Panel probe (Files-mode metadata).** A GUI host running `Files` mode
must not open the panel files on its own (GUI) thread just to size
`slot_bytes` and build the `panels` array — with dozens of multi-GB files
that pass alone can freeze the host for minutes. Instead it invokes
`mmm-ipc-worker --probe-panels`: write a bare JSON object
`{"paths": ["/abs/panel1.xisf", ...], "input_select": "Auto"}` on stdin
(unframed; `input_select` as in `JobMode::Files`, defaulting to `"Auto"`
when omitted) and read back one JSON object on stdout:
`{"panels": [{"width": W, "height": H, "channels": C}, ...],
"frame": [FW, FH, FCH] | null}`. `panels` is in `paths` order; `frame` is
the worker's own `choose_frame` result and is non-null exactly when the job
can resolve to solved mode (`input_select` ≠ `"Aligned"` and every panel
carries a usable astrometric solution) — with `input_select` `"Solved"` a
panel without a solution makes the probe exit 1 with the analyze stage's
error message instead. Header reads are parallel and header-only (never
pixel data). The host sizes `slot_bytes` from
`max(max panel width, frame width) * ch * band_rows * 4`. Exit code 0 on
success; on any error the worker writes a message to stderr and exits 1.
Wire structs: `PanelProbeRequest` / `PanelProbeReply` in `protocol.rs`.
```

- [ ] **Step 6: Commit** — `feat(ipc-worker): --probe-panels metadata probe + PROTOCOL.md §11`

---

### Task 3: C++ host — pumped, stderr-capturing probe plumbing

**Files:**
- Modify: `integration/pixinsight/host/mmm_os.h` (add `os_wait_readable2`)
- Modify: `integration/pixinsight/host/mmm_host.h` (ProgressCallback::want_cancel, HostCancelled, ProbedPanel/PanelProbeResult, probe_panels decl, probe_frame gains prog param)
- Modify: `integration/pixinsight/host/mmm_host.cpp` (spawn stderr wiring, `run_probe_process`, rewrite `probe_frame`, add `probe_panels`)
- Test: `integration/pixinsight/host/test/test_probe_panels.cpp` (new)
- Modify: `integration/pixinsight/host/CMakeLists.txt` (add test target)

**Interfaces:**
- Consumes: Task 2's `--probe-panels` child contract; existing `os_wait_readable`, `Pipe`, spawn helpers.
- Produces (used by Task 4):

```cpp
// mmm_host.h
struct ProgressCallback {
  ...existing...
  /// Polled by the probe helpers after each on_idle(): return true to abort
  /// the probe (the child is killed and HostCancelled is thrown).
  virtual bool want_cancel() { return false; }
};

/// Thrown by Host::probe_frame / Host::probe_panels when the
/// ProgressCallback reported want_cancel(): a deliberate user stop, distinct
/// from HostError faults. Carries no message.
struct HostCancelled {};

/// One panel's header geometry from Host::probe_panels.
struct ProbedPanel {
  uint64_t width = 0, height = 0, channels = 0;
};

/// Result of Host::probe_panels (PROTOCOL.md §11 --probe-panels reply).
struct PanelProbeResult {
  std::vector<ProbedPanel> panels;   // in input-path order
  bool has_frame = false;            // true iff the job can resolve to solved
  uint64_t frame_w = 0, frame_h = 0, frame_ch = 0;
};

class Host {
  ...
  static void probe_frame(const std::string& worker_path, const nlohmann::json& init_obj,
                          uint64_t& w, uint64_t& h, uint64_t& ch,
                          ProgressCallback* prog = nullptr);
  static PanelProbeResult probe_panels(const std::string& worker_path,
                                       const std::vector<std::string>& paths_utf8,
                                       const std::string& input_select,
                                       ProgressCallback* prog = nullptr);
  ...
};
```

**Implementation notes (mmm_host.cpp):**

1. `os_wait_readable2(h1, h2, timeout_ms) -> int` in `mmm_os.h`: bitmask (bit 0 = h1 readable/EOF, bit 1 = h2), 0 = timeout; either handle may be `os_invalid_handle` (ignored). POSIX: one `poll()` with up to 2 fds. Windows: `PeekNamedPipe` both in ≤10 ms `Sleep` slices, mirroring `os_wait_readable`; a broken pipe reports readable so the read observes EOF. Keep doc comments in the same style as `os_wait_readable`.

2. Spawn plumbing gains a stderr leg **without changing `Host::run`'s behavior** (its worker keeps inherited stderr):
   - POSIX `spawn_worker(path, argv, child_stdin_rd, child_stdout_wr, child_stderr_wr /* -1 = inherit */, const std::vector<int>& parent_fds_to_close)` — dup2 stderr only when `>= 0`; close all listed parent ends in the child. Update `run()`'s call site to the new signature (`-1`, `{in_pipe.fd[1], out_pipe.fd[0]}`).
   - Windows `build_command_line_w(path, args, const std::string& extra_flag)` — replaces `bool probe`; appends the flag when non-empty. `spawn_worker_win(path, args, extra_flag, child_stdin_rd, child_stdout_wr, child_stderr_wr /* os_invalid_handle = inherit host stderr */)` — sets `si.hStdError` to the pipe when given.
   - POSIX probe spawns pass the flag through `argv` as today.

3. New file-local helper:

```cpp
// Spawns `worker_path <flag>`, writes `payload` to its stdin (the probe modes
// read stdin to EOF before writing anything, so a full blocking write cannot
// deadlock), closes stdin, then drains stdout AND stderr concurrently with the
// same pumped wait loop Host::run uses (os_wait_readable2 + prog->on_idle()).
// Reaps the child on every path. Returns captured stdout on exit 0. Throws
// HostCancelled when prog->want_cancel() during the drain (child killed
// first); throws HostError on nonzero exit, with trimmed captured stderr in
// the message (fallback text when stderr is empty).
static std::string run_probe_process(const std::string& worker_path, const std::string& flag,
                                     const std::string& payload, ProgressCallback* prog);
```

   Drain loop shape (both platforms):

```cpp
std::string out, err;
bool out_open = true, err_open = true;
while (out_open || err_open) {
  int r = os_wait_readable2(out_open ? out_fd : os_invalid_handle,
                            err_open ? err_fd : os_invalid_handle, Host::kIdleWaitMs);
  if (r == 0) {
    if (prog != nullptr) {
      prog->on_idle();
      if (prog->want_cancel()) { /* kill + reap child, close fds */ throw HostCancelled{}; }
    }
    continue;
  }
  char buf[4096];
  if ((r & 1) && out_open) {
    long n = os_read(out_fd, buf, sizeof buf);
    if (n > 0) out.append(buf, size_t(n)); else out_open = false;   // n<=0: EOF/err (EINTR retry on POSIX)
  }
  if ((r & 2) && err_open) {
    long n = os_read(err_fd, buf, sizeof buf);
    if (n > 0) err.append(buf, size_t(n)); else err_open = false;
  }
}
// reap; on nonzero exit: throw HostError("worker " + flag + " failed: " + (err trimmed, or "exited abnormally"))
```

   Kill+reap on cancel mirrors `run()`'s catch block (`TerminateProcess`/`SIGKILL` + wait + close handles). The existing probe_frame's "reaped" try/catch fault-isolation pattern carries over.

4. `Host::probe_frame` becomes a thin wrapper: `run_probe_process(worker_path, "--probe-frame", init_obj.dump(), prog)` + the existing sscanf parse. Signature gains the defaulted `prog` param (golden tests compile unchanged).

5. `Host::probe_panels`:

```cpp
PanelProbeResult Host::probe_panels(const std::string& worker_path,
                                    const std::vector<std::string>& paths_utf8,
                                    const std::string& input_select, ProgressCallback* prog) {
  nlohmann::json req;
  req["paths"] = paths_utf8;
  req["input_select"] = input_select;
  const std::string out = run_probe_process(worker_path, "--probe-panels", req.dump(), prog);
  PanelProbeResult res;
  try {
    nlohmann::json reply = nlohmann::json::parse(out);
    for (const auto& p : reply.at("panels")) {
      ProbedPanel pp;
      pp.width = p.at("width").get<uint64_t>();
      pp.height = p.at("height").get<uint64_t>();
      pp.channels = p.at("channels").get<uint64_t>();
      res.panels.push_back(pp);
    }
    const auto& frame = reply.at("frame");
    if (!frame.is_null()) {
      res.has_frame = true;
      res.frame_w = frame.at(0).get<uint64_t>();
      res.frame_h = frame.at(1).get<uint64_t>();
      res.frame_ch = frame.at(2).get<uint64_t>();
    }
  } catch (const nlohmann::json::exception& e) {
    throw HostError(std::string("probe-panels: could not parse worker reply: ") + e.what());
  }
  return res;
}
```

**Test** `test/test_probe_panels.cpp` (argv: `<fixtures_dir> <worker_path>`, like the golden tests):
- Load `solved_meta.json`; probe the two `solvedN.xisf` fixture paths with `"Auto"`: panels' geometry matches the meta; `has_frame` true; frame equals what `Host::probe_frame` returns for the same panels via `solved_props.json` (reuse `make_panels`-style splice or compare against `probe_frame` called as in test_golden_solved).
- Probe the aligned fixtures (`meta.json` panels, registered, no solutions) with `"Auto"`: geometry matches; `has_frame` false.
- Probe with a nonexistent path appended: expect `HostError` whose message contains the bogus filename (proves stderr capture).
- An `on_idle`-counting ProgressCallback on one call: assert it doesn't crash (idle count may be 0 for tiny files — no assertion on the count).

CMake: add `test_probe_panels` mirroring `test_golden_solved` (`gen_fixtures build_worker` deps, `${CMAKE_BINARY_DIR}/fixtures ${MMM_WORKER}` args).

- [ ] **Step 1:** Write `test_probe_panels.cpp` + CMake entry; build → link failure (`probe_panels` undefined).
- [ ] **Step 2:** Implement `os_wait_readable2`, spawn changes, `run_probe_process`, `probe_frame` rewrite, `probe_panels`.
- [ ] **Step 3:** `cmake --build && ctest` in a build dir — all host tests pass (including the pre-existing golden/isolation ones, proving the spawn refactor didn't regress `run()`).
- [ ] **Step 4: Commit** — `feat(host): pumped stderr-capturing probes; Host::probe_panels`

---

### Task 4: PCL module — RunFiles via probe_panels; pumped RunViews probe; v1.3.0

**Files:**
- Modify: `integration/pixinsight/module/MmmExecution.cpp`
- Modify: `integration/pixinsight/module/MmmVersion.h` (+ check `mmm.cpp` version derivation)

**Interfaces:**
- Consumes: `mmm::Host::probe_panels`, `mmm::Host::probe_frame(..., prog)`, `mmm::HostCancelled`, `ProgressCallback::want_cancel` (Task 3).

**Changes:**

1. `ConsoleProgress`: implement `want_cancel()`:
   - `PumpEventsAndPollAbort()` sets a new member `bool m_abortSeen = false;` when `AbortRequested()` fires (keep calling `m_console.Abort()` and `host->cancel()` when host is set).
   - `bool want_cancel() override { return m_abortSeen; }`
2. `DriveHost` signature becomes `DriveHost(..., mmm::PanelSource& source, ConsoleProgress& prog)`; it no longer constructs `ConsoleProgress`/calls `EnableAbort()` (callers do), still sets `prog.host = &host` and `s_activeHost`.
3. `RunViews`: construct `Console().EnableAbort(); ConsoleProgress prog;` up front; pass `&prog` to the solved-mode `probe_frame` call; pass `prog` to `DriveHost`. Wrap the probe in `try { ... } catch (const mmm::HostCancelled&) { throw ProcessAborted(); }`.
4. `RunFiles`: delete the whole `FileFormat`/`FileFormatInstance` metadata loop, `probe_panels` json, `allHaveSolution`, `canSolve`, and the probe-init build. New shape:

```cpp
void RunFiles( const Params& in, const std::string& worker_path )
{
   Console console;
   console.EnableAbort();
   ConsoleProgress prog;

   // Delegate the metadata pass to the worker (PROTOCOL.md §11
   // --probe-panels): header-only parallel reads in a child process, while
   // this GUI thread pumps events via prog.on_idle() — the module never
   // opens a panel file itself, so an 80-panel run cannot freeze the UI.
   std::vector<std::string> paths_utf8;
   paths_utf8.reserve( size_type( in.filePaths.Length() ) );
   for ( const String& path : in.filePaths )
      paths_utf8.push_back( std::string( path.ToUTF8().c_str() ) );

   console.WriteLn( String().Format( "<end><cbr>Reading metadata of %u panel files...",
                                     unsigned( paths_utf8.size() ) ) );
   mmm::PanelProbeResult probe;
   try
   {
      probe = mmm::Host::probe_panels( worker_path, paths_utf8,
                                       InputSelectWireString( in.inputSelect ), &prog );
   }
   catch ( const mmm::HostCancelled& )
   {
      throw ProcessAborted();
   }

   json panels = json::array();
   json file_paths = json::array();
   uint64_t max_w = 0;
   const uint64_t ch0 = probe.panels[0].channels;
   for ( size_t i = 0; i < probe.panels.size(); ++i )
   {
      const mmm::ProbedPanel& p = probe.panels[i];
      json pd;
      pd["panel_id"]   = uint32_t( i );
      pd["width"]      = p.width;
      pd["height"]     = p.height;
      pd["channels"]   = p.channels;
      pd["properties"] = json::array();
      panels.push_back( std::move( pd ) );
      file_paths.push_back( paths_utf8[i] );
      if ( p.width > max_w )
         max_w = p.width;
   }

   const uint64_t band_rows = uint64_t( uint32_t( in.bandRows ) );
   // Output-slot sizing (PROTOCOL.md §7/§11): the probe's frame is non-null
   // exactly when the run can resolve to solved mode, whose reprojected
   // frame can be wider than every input; otherwise the widest input file
   // is exact.
   uint64_t width = max_w;
   if ( probe.has_frame && probe.frame_w > width )
      width = probe.frame_w;
   const uint64_t slot_bytes = width * ch0 * band_rows * 4;
   ... // init_body build, shm name, layout, NullPanelSource — unchanged,
       // then DriveHost( worker_path, std::move( init_body ), shm_name, layout, source, prog );
}
```

   (Empty-panels guard: `run_blend` already enforces ≥ 2 files, and `probe_panels` errors on none — `probe.panels[0]` is safe; keep it that way by leaving the ≥ 2 validation in `run_blend`.)
5. Update the big "Progress + cooperative abort" comment block (MmmExecution.cpp:89-121) to note the probe phases are now pumped too and the Files metadata pass happens worker-side.
6. Includes: drop now-unused `<pcl/ImageDescription.h>`? — **no**: `ShowSeamMap` still uses `FileFormat`/`FileFormatInstance`/`ImageDescriptionArray`; `AstrometryProps.h` still used by `RunViews`. Leave includes alone.
7. `MmmVersion.h`: 1.2.0 → 1.3.0 (`MMM_VERSION_MINOR 3`, string "1.3.0"); confirm `mmm.cpp` consumes the macros (comment says it derives — verify, adjust if hand-duplicated).

- [ ] **Step 1:** Apply the edits.
- [ ] **Step 2:** Build the module if the local PCL toolchain allows (`integration/pixinsight/module/` Makefile or CMake); otherwise at minimum re-run the host-lib build+tests and `cargo test` (module compile then happens on the user's Windows/Linux build machine — call this out in the final report).
- [ ] **Step 3: Commit** — `feat(pixinsight): v1.3.0 - worker-side Files metadata probe, pumped probes, abort during scan`

---

### Task 5: Full verification sweep

- [ ] `source ~/.cargo/env; cargo fmt --check && cargo clippy --all-targets && cargo test && cargo doc --no-deps` — all clean.
- [ ] Host: fresh `cmake -S integration/pixinsight/host -B <build> && cmake --build <build> && ctest --test-dir <build>` — all pass.
- [ ] Module build attempt (as Task 4 Step 2).
- [ ] Re-read PROTOCOL.md §11 diff against the implemented wire structs (§12 rule).
- [ ] Real-data smoke note for the user: run Files mode on the 12-panel Orion set, then the 80-panel set on Windows; expect a live UI + working Cancel during "Reading metadata", and unchanged blend output.

## Self-Review

- Coverage: freeze root cause (unpumped RunFiles loop) → Task 4; unpumped probe_frame → Task 3; "PixInsight reading more than needed" → worker header-only reads (Tasks 1-2); cancellation during scan → want_cancel/HostCancelled (Tasks 3-4); parallel reads → rayon in Task 1; stderr visibility → run_probe_process capture (Task 3).
- Types consistent: `PanelProbeRequest/Reply` (Rust) ⇄ §11 JSON ⇄ `PanelProbeResult` (C++); `probe_frame` keeps defaulted `prog` so existing C++ tests compile.
- Slot-sizing parity: old `canSolve && allHaveSolution → probe frame; width = max(max_w, fw)` reproduced by worker-side `frame` nullability rule.
