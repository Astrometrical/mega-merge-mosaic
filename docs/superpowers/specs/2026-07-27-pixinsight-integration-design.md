# PixInsight Integration — Design

Status: **Plan 1 complete** (Rust IPC transport + `mmm-ipc-worker`, committed on
branch `pixinsight-integration`). **Plan 2 design approved** (2026-07-27): the
C++ PCL module + packaging, detailed in §14–§18 below. §10 (UI) and §12
(packaging) are updated from "TO VERIFY" to concrete decisions.
Scope owner: mmm-core + the `mmm-ipc-worker` crate + a C++ PCL module under
`integration/pixinsight/`.

## 1. Goal

Let a PixInsight user blend a set of mosaic panels with mmm **without writing
the panels to disk first**. The panels are typically the product of an
in-PixInsight workflow (e.g. load master lights → gradient removal → BlurX in
correct-only mode → colour correction), held as image **views/windows**.
Selecting those windows and blending them directly — rather than saving each
aligned view to disk to feed a file-only tool — is the headline UX win.

The integration must:

- Accept **in-memory views** *and* **files** as input (files are the trivial
  case — the worker runs the existing CLI path on paths).
- Support both panel modes:
  - **Aligned** — full-canvas frames (MosaicByCoordinates output, run
    in-memory). mmm needs only pixels + geometry.
  - **Solved** — raw panels carrying PixInsight astrometric solutions; mmm's
    phase-5 path reprojects them itself, so MosaicByCoordinates can be skipped
    entirely. The user chooses per their need for projection control.
- **Not double peak memory.** Inputs are ~24 GB of pixels that already exist as
  PixInsight views; the integration must not require a second full resident
  copy.
- **Isolate faults.** An mmm bug must never take down the user's PixInsight
  session or corrupt their views.
- **Scale to very large mosaics.** Where mmm controls a storage choice, prefer
  disk-backed streaming over holding pixel-scale data resident.

## 2. Chosen architecture (and rejected alternatives)

**Process boundary via a Rust worker driven over IPC.** A native C++ PCL module
lives inside PixInsight, owns the views, and supervises a separate
`mmm-ipc-worker` process that links `mmm-core` and does the compute. Pixels
cross via shared memory; commands/progress/data-requests cross via a control
channel.

Rejected, with reasons (see the conversation that produced this spec):

- **WASM in PixInsight's V8** — wasm32's 4 GB address space cannot hold even one
  2 GB panel plus working set, and a non-browser V8 embed provides no threads,
  which would forfeit mmm's rayon concurrency. Wrong tool for a 24 GB,
  multi-pass, multi-threaded workload.
- **stdin/socket single-stream to the CLI** — mmm is multi-pass and
  random-access (`the source file *is* the random-access store`); a one-shot
  sequential stream cannot serve it, and a "worker requests tiles back" protocol
  is just mmap reinvented over a pipe. (That pull idea *is* adopted below, but as
  shared memory, not a byte stream.)
- **In-process C ABI (`mmm-core` linked into the PCL module)** — gives true
  zero-copy but couples PixInsight's stability to Rust (a panic crashes PI) and
  provides no fault isolation. The process boundary the user wants is precisely
  what removes the need for the C ABI here.
- **A C ABI as a parallel deliverable** — **deferred.** It is only needed if a
  *non-Rust* host wants to embed `mmm-core` in-process (e.g. a future C++/Qt
  GUI). The PixInsight integration does not, because the worker is Rust and the
  boundary is a wire protocol, not a function-call ABI.
- **PJSR script front-end** — **dropped entirely.** A native Process +
  ProcessInterface gives a strictly better UX: PixInsight scripts are *modal*,
  so a mid-run adjustment (rename a view, apply a mask, run SCNR first) forces
  closing the script and starting over, whereas a process interface is
  non-modal. Registered processes are also automatically PJSR-scriptable, so
  power users keep batch-scripting for free without us shipping any JS.

## 3. Topology

```
┌─────────────────────── PixInsight process ───────────────────────┐
│  mmm PCL module  (C++, new)                                       │
│   • Process (global context) + ProcessInterface (C++ UI)          │
│   • owns the selected views (ImageVariant pixel access)           │
│   • creates named shared-memory segment(s)                        │
│   • spawns + supervises mmm-ipc-worker                            │
│   • service thread: answers band-requests, memcpy view → shm      │
│   • assembles the streamed result into a new ImageWindow          │
└───────────────┬───────────────────────────────────────────────────┘
                │  control channel: worker stdin/stdout pipes
                │  bulk pixels:     named shared memory
┌───────────────▼──────── mmm-ipc-worker process (Rust, new) ───────┐
│   • links mmm-core directly as a Rust crate                       │
│   • drives analyze → blend exactly like the CLI                   │
│   • pixel input via a new IpcPanelReader backing                  │
│   • pixel output via a new shm RowSink                            │
└───────────────────────────────────────────────────────────────────┘
```

### Components / deliverables

1. **`crates/mmm-ipc-worker/`** — new Rust binary. Kept a **separate crate**
   (clean dependency boundary) rather than a hidden `mmm` subcommand.
2. **`mmm-core` additions** — `IpcPanelReader` (a third `PanelReader` backing),
   a shared-memory `RowSink`, and the shared protocol types (so worker and tests
   use one definition).
3. **`integration/pixinsight/`** — the C++ PCL module (Process +
   ProcessInterface + shm + supervision) and the protocol spec document.

Everything is a thin frontend over `mmm-core`; the algorithm code
(`analyze`, `blend`, `seam`, `pyramid`, …) is untouched.

## 4. Data flow

### Aligned mode (a)
Module holds full-canvas views. The worker's **analyze scan** and **blend
band-sweep** pull rows over IPC — two sequential sweeps of the big data. Result
bands stream back; the module builds the new window. IPC transport is used for
both passes.

### Solved mode (b)
Module holds raw views **plus each view's astrometric solution**, passed in the
init message (extracted from `ImageWindow.astrometricSolution` on the C++ side —
**not** re-parsed from a file). The worker's **reproject** stage pulls each raw
panel once, Lanczos-resamples to `panels/<id>/aligned.bin` in the session dir on
disk, then analyze/blend run entirely off those disk mmaps. **IPC transport is
touched only during reprojection here** — a single sequential pass, the cheapest
case.

### File mode
No shared memory at all: the module passes paths and the worker runs the
existing CLI path verbatim.

## 5. Transport (the crux)

- **Control channel = the worker's stdin/stdout pipes.** Length-prefixed
  messages. Universally available, auto-inherited by the child, no socket
  naming or permissions. The module runs a reader thread.
- **Encoding.** A **tiny fixed binary header** on the hot band-request/reply
  path; **JSON only** for the one-time init handshake (human-debuggable,
  negligible cost).
- **Bulk pixels = named shared memory**, created by the C++ module, name handed
  to the worker in the init message. Cross-platform via OS primitives
  (`shm_open`+`mmap` on Linux/macOS; `CreateFileMapping`+`MapViewOfFile` on
  Windows); the Rust side attaches by name.
- **Pipe messages are the synchronisation.** Every band transfer is
  request → fill → reply, so a request is not answered until the pixels are in
  shm. Therefore **no lock-free ring buffers or futexes** — shm is just a pool
  of band-sized slots. Request/reply carries `(panel_id, channel, y0, y1,
  slot_id)`.
- **The one approved memcpy:** module copies view rows → shm slot; worker reads
  them zero-copy from the mapped slot. The result path mirrors this: worker
  writes a result band → shm slot → module copies it into the new window.

### Concurrency
The worker is rayon-parallel (analyze across panels; blend across output bands),
so **multiple band-requests are in flight at once**. A fixed pool of shm slots
(≈ 2× worker threads) is used; each request claims a free slot by id, and the
module's service thread(s) fill and reply per request. This bounds shm to
`pool_size × band_bytes` (tens of MB) regardless of mosaic size — the property
that makes the pull design scale.

> **To verify during planning:** the exact concurrent row-access pattern in
> `blend.rs` / `analyze.rs`, to size band height and slot count and to confirm
> the per-reader band cache (below) satisfies the borrow contract. This is the
> one place the design leans on an unverified reading of the access pattern.

## 6. `IpcPanelReader` — the mmm-core insertion point

`PanelReader` is already the single choke-point for all pixel access, exposing a
uniform `row(c, y) -> Option<(u64, &[f32])>` over two backings today
(`Xisf` mmap, `Cache` mmap). Add a **third backing** that implements the same
contract by:

1. on a miss, issuing a band-request over the control channel;
2. receiving the shm slot;
3. copying the band into a **locally-owned band cache** and returning a slice
   into that cache (so the returned `&[f32]` outlives the shm slot, which is then
   recycled).

Because analyze/blend consume rows sequentially within a band across channels, a
small per-reader band cache (a few rows × all channels) satisfies the borrow
contract with minimal memory. **`analyze.rs` and `blend.rs` do not change** —
they only ever see `PanelReader`.

## 7. Output

The blend result is returned as a **new PixInsight window**, streamed
band-by-band through an output shm slot: the module allocates the result image
once (unavoidable — it is the product) and fills it from the streamed bands. No
doubling. The output canvas is the union content bbox, as today.

## 8. Session directory & intermediates

- **Analysis artifacts** (L8 summaries, overlap graph, photometry JSON) are
  tiny (a few MB) and live on disk in the session dir, preserving stage
  re-runnability. No reason to keep them in RAM.
- **Reprojection caches** (solved mode only) are pixel-scale (~8 GB for the
  Orion set) and are written to `panels/<id>/aligned.bin` on disk, **exactly as
  phase 5 does today** — near-zero new code, and disk-backed by deliberate
  choice for scalability.
- The **session dir is user-specified**; the user owns its lifetime. The module
  offers convenience cleanup (a command/button), but does not silently delete
  it.

## 9. Error handling & the isolation guarantee

The boundary exists for this, so it is a first-class requirement:

- **Worker crash/panic** → pipe closes / process exits non-zero → module detects
  the broken control channel, abandons the shm, reports a clean error to
  PixInsight, and **PixInsight keeps running with the user's views intact**. No
  partial window is created.
- **Worker builds with `panic = "abort"`** so a panic is a clean, observable
  process death rather than an unwind into an undefined state.
- **Cancellation** → module sends a cancel message (or closes the pipe); the
  worker checks a cancel flag between bands and exits promptly.
- **Protocol/version mismatch** → the init handshake exchanges a protocol
  version; a mismatch is refused with a clear message rather than misbehaving.

## 10. UI model (PixInsight side) — concrete

A blend consumes **many** source views, so the Process runs in the **global
context** (no single target view; its parameters carry the *list* of input view
ids), like ImageIntegration or StarAlignment's multi-view mode — not a per-view
process. Because it is a Process (not a script) the UI is **non-modal**: the
user can make a mid-run adjustment (rename a view, apply a mask, run SCNR)
without tearing down and restarting the tool. Registered processes are
PJSR-scriptable for free, so power users keep batch-scripting without us
shipping any JS. The full `ProcessInterface` UI is built in this pass (not a
minimal stub).

**`mmmProcess` parameters** (the scriptable/serializable state, all mapping to
the CLI/`InitJob`):

| parameter | maps to | notes |
|---|---|---|
| `inputViews` | `panels` | ordered list of view ids (aligned/solved modes) |
| `filePaths` | `JobMode::Files.paths` | ordered path list (files mode) |
| `mode` | `InitJob.mode` | `aligned` / `solved` / `files` |
| `sessionDir` | `InitJob.session_dir` | user-owned session directory |
| `feather` | `params.feather_px` | f32, canvas px |
| `blendMode` | `params.mode` | `feather` / `twoband` / `pyramid` |
| `flatten` | `params.flatten` | opt-in polynomial order, or off |
| `roi` | `params.roi` | optional `[x0,y0,x1,y1]`, or off |
| `downsample` | `params.downsample` | `1` full-res, `8` L8 preview |
| `defectVeto` | `params.defect_veto` | bool |
| `surfaceOrder` | `params.surface_order` | analyze surface-fit order |
| `bandRows` | `params.band_rows` | band granularity (sane default, advanced) |

**`mmmInterface` controls**: a multi-view selector with a files-list toggle, a
session-dir picker, a mode combo (aligned/solved/files), the blend-parameter
controls above, and a progress bar + cancel button fed by the worker's
`Progress` frames (§15). Enable/disable logic follows the mode (files list vs
view selector).

**Execution flow** (on Apply / global execute):

1. Resolve the selection into ordered `PanelDesc`s. In **solved** mode, extract
   each view's astrometric solution from its `ImageWindow` via
   `AstrometricMetadata` and carry it verbatim as `PanelDesc.properties` — a C++
   read of the in-memory solution, **not** a file re-parse.
2. In **solved** mode, run the worker's `--probe-frame` (§15) once to obtain the
   output frame `w×h×ch` and size `slot_bytes` from it (§7 hazard resolved by
   making the worker the single source of truth). In **aligned**/**files** mode
   the canvas width is known from the panels, so slots are sized directly.
3. Create the shm segment, spawn + supervise the worker, and drive the blend via
   the pure host library (§14), pumping `Progress` into the progress bar and
   offering cancel.
4. Assemble the streamed output bands into **one new `ImageWindow`** (allocated
   once — §7, no doubling) and show it. Any host/worker error → a clean PI
   message box, **no partial window**, the user's source views intact (§9).

## 11. Testing

- **The whole protocol + worker is testable without PixInsight.** A Rust
  integration test plays the module role: it spawns the worker, serves bands
  from a `synth`-generated in-memory mosaic, and asserts the streamed result is
  **byte-identical to the file-based blend** of the same panels. This
  golden-identity check anchors correctness and fits the repo's existing
  hash-regression culture.
- **`IpcPanelReader` unit tests** run against a mock control channel.
- **The C++ PCL module** requires PixInsight → manual smoke test, like the
  existing real-data runs.

## 12. Packaging & distribution — concrete (Linux/WSL first)

Verified against the local install (`/opt/PixInsight`, core dated 2026-06-21,
PCL headers + full PCL source present, Qt 6.8.7 bundled). Decisions:

- A **Module** is the deployment container — on Linux the file is
  **`mmm-pxm.so`** (the `-pxm.so` suffix is the PixInsight module convention,
  matching the bundled `*-pxm.so` files). The core loads it and registers the
  **Process** + **ProcessInterface** it contains via `InstallPixInsightModule`.
- The **`mmm-ipc-worker` binary ships beside the module** in one folder; the
  module resolves its own on-disk path (`dladdr`) and spawns the sibling worker
  — no separate install or `PATH` setup. The pure host library takes the worker
  path as a parameter so the standalone test can point at the debug build.
- **Now — dev distribution (implemented this pass):** build `mmm-pxm.so`, place
  `mmm-ipc-worker` next to it, and load via PixInsight's manual **Install
  Modules** (unsigned local modules load fine for development). One-time
  prerequisite: build **`libPCL-pxi.a`** from the bundled source
  (`/opt/PixInsight/src/pcl/linux/g++`, `make`), output directed to a
  **project-local** lib dir so no writes to the root-owned install and no sudo.
  Documented in `integration/pixinsight/README.md`.
- **Later — repository distribution (planned, not implemented):** a PixInsight
  **update repository** (user adds a URL once; auto-install + auto-update). Costs:
  a **code-signing certificate from the PixInsight team** and a **per-platform
  signed package matrix** (Win/Linux/macOS). Cross-platform work this defers:
  Windows/macOS **shm ports** (`CreateFileMapping`/`MapViewOfFile`; the Rust
  `shm.rs` already stubs non-Unix), macOS **notarization** of `mmm-ipc-worker`
  (else Gatekeeper blocks the `exec`), and Windows Authenticode/SmartScreen.
- The PCL module must be built against a matching PixInsight PCL/SDK version and
  re-validated on PI ABI updates; the worker is ABI-independent (it only talks
  the wire protocol), so a PI update never requires rebuilding the worker.

## 13. Explicitly out of scope

- A C ABI for `mmm-core` (deferred; only needed for a future non-Rust in-process
  host).
- WASM.
- Any JavaScript / PJSR in the data path or as the UI.
- Pure no-disk reprojection intermediates (shm-resident aligned caches) — a
  possible later optimisation, not v1.
- Freeing/consuming the user's source views to save memory — unnecessary given
  the pull-based transport.

---

# Plan 2 — C++ host + PCL module (implementation design)

Plan 1 delivered the Rust IPC transport and `mmm-ipc-worker` (byte-identical to
the file-based blend, with cancel + crash-isolation e2e tests). Plan 2 builds the
**production host**: the PixInsight-side C++ that drives that worker. The wire
contract is fixed by `integration/pixinsight/PROTOCOL.md` and must be verified
against the Rust code (`ipc/protocol.rs`, `shm.rs`, `client.rs`, `testhost.rs`)
during implementation.

## 14. Host/module split (the testability boundary)

The C++ is split so the protocol implementation is testable **without**
PixInsight, isolating the PixInsight-only parts to a manual smoke test.

```
integration/pixinsight/
  PROTOCOL.md                 # the wire contract (exists)
  README.md                   # build + one-time libPCL-pxi.a setup + dev install
  host/                       # PURE transport layer — NO PCL dependency
    mmm_shm.{h,cpp}           #   shm_open/ftruncate/mmap; SlotLayout math (mirrors shm.rs §7)
    mmm_protocol.{h,cpp}      #   frame codec, tag table, binary band layouts, Init JSON writer
    mmm_host.{h,cpp}          #   spawn+supervise worker, service loop, band serving, output collect
  module/                     # THIN PCL wrapper — depends on host/ + PCL
    mmmModule.cpp             #   MetaModule entry (InstallPixInsightModule)
    mmmProcess.{h,cpp}        #   global-context MetaProcess + parameters (§10)
    mmmInterface.{h,cpp}      #   ProcessInterface UI (§10)
    Makefile, makefile-x64    #   PI MakefileGenerator-style; links libPCL-pxi.a + host/ objs
  test/
    host_golden_test.cpp      #   standalone; mirrors end_to_end.rs; NO PixInsight
    CMakeLists.txt            #   builds host/ + this test via CMake
```

**Invariant: `host/` includes no PCL header.** It compiles and links against
nothing but the C++ stdlib + POSIX, so `test/` exercises the whole protocol
against the real `mmm-ipc-worker` with g++/CMake alone. `module/` is the only
PCL-linked code and stays thin — it adapts PixInsight objects to the `host/`
library's interfaces and back.

### 14.1 `host/` — the pure transport library

Mirrors `testhost.rs`'s `run_host` serving loop, as a reusable library:

- **`mmm_shm`** — creates the named POSIX segment (`shm_open`+`ftruncate`+`mmap`),
  carved into `input_slots` then `output_slots` fixed `slot_bytes` slots with the
  exact `input_offset`/`output_offset` math of PROTOCOL §7. Name is
  **deterministic** (`/mmm-<pid>-<counter>`, no RNG). The creator unlinks on
  teardown and defensively at the start of a create under the same name (crash
  cleanup). Enforces the 4-byte-alignment discipline (§7).
- **`mmm_protocol`** — the frame codec (`tag:u8 | len:u32 LE | payload`), the tag
  table (worker→host 1–6, host→worker 128–131), the three binary band layouts
  (`BandRequest` 28 B, `BandReply` 9 B, `OutputBand` 24 B; all LE), and the
  `Init` JSON writer. The writer enforces the **finite-float precondition**
  (PROTOCOL §6): reject the write if any reachable float (`feather_px`, panel
  `properties` F64/F64Vec/F64Mat data) is non-finite, since JSON has no NaN/Inf.
- **`mmm_host`** — owns spawn/supervision + the service loop:
  - **Spawn**: `posix_spawn` (or fork+exec) the worker, its stdin/stdout piped,
    stderr inherited for human diagnostics. Worker path is a **constructor
    parameter** (module passes the `dladdr`-resolved sibling; test passes
    `target/debug/mmm-ipc-worker`).
  - **Service loop**: one reader thread decodes worker→host frames.
    `BandRequest` → look up `panel_id` geometry, memcpy rows `[y0,y1)` of every
    channel into the slot in **planar, native-endian** order (§7), then
    `BandReply{status:0}` (or `status:1` on an out-of-range request).
    `Begin{w,h,ch}` → allocate the output collector once. `OutputBand` → copy the
    band out of the slot into the collector at row `y0`, then `OutputAck`.
    `Progress` → forwarded to a caller callback. `Done`/`Error` → terminate. All
    host→worker writes are serialized behind one mutex (PROTOCOL §9).
  - **`PanelSource` interface** — abstracts *where input pixels come from*: the
    test supplies in-memory buffers; the module supplies `ImageVariant` rows via
    an adapter. `host/` never knows about PixInsight.
  - **Output collector** — the module supplies a sink (writes into the new
    `ImageWindow`); the test supplies a buffer for byte comparison.
  - **Fault isolation (§9, PROTOCOL §10)**: stdout EOF **without** a prior
    `Done`/`Error`, or a non-zero worker exit, → a clean `HostError` to the
    caller, shm unlinked, **no output surfaced**. `cancel()` sends `Cancel` (tag
    131) and drains. Protocol-version mismatch is refused before the run.

## 15. Rust-side changes (worker + mmm-core)

Two additive changes; file-mode CLI behaviour and existing tests are unchanged;
`cargo test --workspace` stays green.

1. **`mmm-ipc-worker --probe-frame`.** Reads an `Init`-shaped JSON (panels with
   solved-mode `properties`, `mode`) on stdin, builds the WCS models, runs
   `align::choose_frame`, prints the resulting output frame as `w h ch` to
   stdout, and exits — no shm, no compute. The host calls this once in **solved**
   mode to size `slot_bytes`, keeping the worker the single source of truth for
   frame geometry (resolves the §7 solved-mode sizing hazard without duplicating
   WCS math in C++). Rust-unit-tested against a known solved fixture.
2. **`Progress` emission.** The `Progress { stage, done, total }` frame already
   exists in the protocol but no driver sends it. Wire minimal emission points
   into the IPC analyze scan (`analyze_ipc_aligned`/`analyze_ipc_solved`) and the
   blend band-sweep (`blend_with_source`'s IPC sink path) so the UI shows real
   percentages. Emitted only over the `HostLink`; the file CLI path is untouched.

## 16. Build system

- **Module `.so`** — a PixInsight MakefileGenerator-style makefile (the
  `makefile-x64` format found in `/opt/PixInsight/src/pcl/linux/g++`: g++,
  `-std=c++20 -fPIC -pthread -D__PCL_LINUX`, AVX2/FMA, `-fvisibility=hidden`,
  output `mmm-pxm.so`), linking `libPCL-pxi.a` + the compiled `host/` objects.
  Hand-authored in that format; the README documents regenerating it via PI's
  MakefileGenerator if the project definition changes.
- **`host/` + `test/`** — CMake (no PCL): a `mmm_host` static lib target + the
  `host_golden_test` executable, registered with CTest.
- **One-time prerequisite** — `make` `libPCL-pxi.a` from the bundled PCL source
  with `PCLSRCDIR`/`PCLINCDIR`/`PCLLIBDIR64` pointed so the archive lands in a
  **project-local** dir (no writes to `/opt/PixInsight`, no sudo). README step.

## 17. Testing

- **Standalone C++ golden-identity test** (`test/host_golden_test.cpp`, CTest —
  the C++ equivalent of `crates/mmm-ipc-worker/tests/end_to_end.rs`, needs **no
  PixInsight**):
  - Generate synthetic panels on disk (e.g. via `mmm synth`).
  - **Aligned**: run the worker in **Files** mode → golden output; run it in
    **Aligned** mode with the `host/` library serving the same pixels from
    memory over shm → assert **byte-identical** to the golden.
  - **Solved**: `--probe-frame` → size slots → serve → assert identical to the
    file-based solved blend.
  - **Isolation**: a worker-crash case (assert clean error, no output) and a
    cancel case (assert prompt stop) — mirroring the Rust e2e tests.
- **Rust**: unit tests for `--probe-frame` output and `Progress` emission;
  `cargo test --workspace` stays green throughout.
- **PCL module**: a manual smoke test inside PixInsight (like the existing
  real-data runs) is the only PixInsight-requiring verification — load the
  module, blend a few views in each mode, confirm the new window + a clean error
  on an induced worker failure.

## 18. Fault-isolation contract (host obligations)

Restating §9 / PROTOCOL §10 as guarantees the `host/` library provides and the
standalone test asserts:

- Worker crash/panic (stdout EOF with no prior `Done`/`Error`, or non-zero exit)
  → clean caller error, shm unlinked, **no partial `ImageWindow`**, PixInsight
  and the user's views intact.
- Cancel → `Cancel` frame; worker unwinds and exits promptly; treated as an
  intentional stop, not a crash.
- Protocol-version mismatch → refused before the run with a clear message.
- The host, as shm creator, is solely responsible for unlinking the segment once
  the worker is confirmed gone.
