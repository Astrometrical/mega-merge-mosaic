# PixInsight Integration — Design

Status: **approved design, pre-implementation** (2026-07-27)
Scope owner: mmm-core + a new `mmm-ipc-worker` crate + a C++ PCL module.

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

## 10. UI model (PixInsight side)

- A blend consumes **many** source views, so the Process runs in the **global
  context** (no single target view; its parameters carry the *list* of input
  view ids), like ImageIntegration or StarAlignment's multi-view mode — not a
  per-view process.
- The **ProcessInterface** (C++ UI) offers: a multi-view selector (or a
  files-list toggle), a session-dir picker, a mode control
  (aligned / solved / files), the blend parameters that map to the CLI
  (`--mode`, `--feather`, `--flatten`, `--roi`, downsample preview, …), and a
  progress + cancel area fed by worker progress messages.
- Because it is a Process (not a script) the UI is **non-modal**: the user can
  make a mid-run adjustment (rename a view, apply a mask, run SCNR) without
  tearing down and restarting the tool.

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

## 12. Packaging & distribution — TO VERIFY

Deferred and to be worked out during implementation; none of it is
insurmountable. Working understanding (specifics to confirm against current
PixInsight docs, as they may have drifted):

- A **Module** is the deployment container (`mmm-pxm.{dll,so,dylib}`), loaded by
  the PixInsight core, which registers the **Process** and **ProcessInterface**
  it contains.
- The **`mmm-ipc-worker` binary ships beside the module** in one folder; the
  module resolves the worker's absolute path from its own on-disk location and
  spawns it. No separate install or `PATH` setup for the worker.
- **Dev distribution:** manual Install Modules (unsigned local modules are
  loadable for development).
- **Long-term distribution:** a **PixInsight update repository** (user adds a URL
  once; auto-install + auto-update). Costs: a **code-signing certificate from
  the PixInsight team**, and a **per-platform signed package matrix**
  (Win/Linux/macOS).
- **Gotchas from shipping a spawned helper binary:** macOS **notarization** of
  `mmm-ipc-worker` (else Gatekeeper blocks the `exec`); Windows SmartScreen/AV
  (Authenticode signing mitigates); the PCL module must be built against a
  matching PixInsight PCL/SDK version and re-validated on PI ABI updates (the
  worker is ABI-independent — it only talks the wire protocol).

## 13. Explicitly out of scope

- A C ABI for `mmm-core` (deferred; only needed for a future non-Rust in-process
  host).
- WASM.
- Any JavaScript / PJSR in the data path or as the UI.
- Pure no-disk reprojection intermediates (shm-resident aligned caches) — a
  possible later optimisation, not v1.
- Freeing/consuming the user's source views to save memory — unnecessary given
  the pull-based transport.
