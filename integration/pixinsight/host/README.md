# mmm PixInsight host library

A PCL-free C++ implementation of the host side of the `mmm` IPC protocol
(see [`../PROTOCOL.md`](../PROTOCOL.md)): shared-memory transport, the
stdin/stdout frame codec, and the `Host` driver that spawns
`mmm-ipc-worker`, serves its band requests, and collects blended output.

It depends on nothing but the C++ standard library, POSIX (`mmap`,
`shm_open`, `fork`/`exec`), and vendored `nlohmann/json` — **no PixInsight
PCL headers, no PixInsight installation** — so it builds and its full test
suite runs on a bare Linux box. This is deliberate: it's the transport core
that a later PCL `Process`/`ProcessInterface` module (**Plan 2b**, not yet
started) will link against and wrap with the actual PixInsight UI and
`ImageVariant` I/O. This library only proves the transport and the byte-exact
blend results; the PixInsight module, its packaging, and the UI are future
work.

## Layout

- `mmm_shm.{h,cpp}` — `SlotLayout` + `ShmSegment`: POSIX shared-memory
  segment carved into fixed-size input/output slots. Mirrors
  `crates/mmm-core/src/ipc/shm.rs`.
- `mmm_protocol.{h,cpp}` — the wire codec: frame read/write, the
  `BandRequest`/`BandReply`/`OutputBand` fixed binary layouts, and the JSON
  messages (`Init`, `Progress`, `Begin`, `Done`, `Error`, `OutputAck`,
  `Cancel`). Mirrors `crates/mmm-core/src/ipc/protocol.rs`.
- `mmm_host.{h,cpp}` — `Host`: spawns the worker, creates the shm segment,
  runs the request/output serve loop to completion, and enforces fault
  isolation (a crashed or nonzero-exit worker always surfaces as a clean
  `HostError`, never a hang or silent partial output). Also
  `Host::probe_frame`, the solved-mode output-size probe. Mirrors the
  reference host in `crates/mmm-core/src/ipc/testhost.rs`.
- `third_party/json.hpp` — vendored [nlohmann/json](https://github.com/nlohmann/json)
  v3.11.3, MIT licensed, single header.
- `test/` — the test suite (see below).

## Building

Requirements: **cmake ≥ 3.16**, a **C++20 compiler** (g++ 13 has been used),
and **cargo/rustc** on `PATH` (the tests build and drive the real
`mmm-ipc-worker` and generate fixtures with `cargo run`). If cmake isn't
already on your system, install it and make sure it's on `PATH` — it is not
part of a stock Ubuntu install.

Build the worker first (from the repo root), then configure and build the
host library:

```sh
source ~/.cargo/env   # if cargo isn't already on PATH
cargo build -p mmm-ipc-worker

cd integration/pixinsight/host
cmake -S . -B build
cmake --build build
```

## Running the tests

```sh
ctest --test-dir build --output-on-failure
```

The default worker path is `<repo>/target/debug/mmm-ipc-worker` (a debug
build, matching the `cargo build -p mmm-ipc-worker` above). Override it with:

```sh
cmake -S . -B build -DMMM_WORKER=/path/to/mmm-ipc-worker
```

Fixture generation (`gen_fixtures`, a `cargo run --example gen_fixtures`
invocation wired in as a CMake custom target) runs automatically as a
dependency of the golden tests, so cargo must be reachable at build/test
time, not just when the worker was built.

### The 5 tests

| Test | Proves |
|---|---|
| `test_shm` | `SlotLayout` offset arithmetic and `ShmSegment` create/map/read/write round-trip through a real POSIX shm segment. |
| `test_protocol` | Frame codec: `BandReply`'s exact 9-byte wire layout, decoding a `BandRequest` frame over a real pipe, and `encode_init` rejecting a non-finite float (the finite-float precondition). |
| `test_golden_aligned` | Byte-identity: driving the real worker in `Files` mode (it reads the panels itself) vs. `Aligned` shm mode (our `PanelSource` serves the same pixels over shared memory) produces bit-identical blended output — proving the C++ `Host` serve loop reproduces the Rust reference host (`testhost.rs`) exactly. |
| `test_golden_solved` | Byte-identity for the solved (unaligned/reprojected) path: `Files(Solved)` mode vs. `Solved` shm mode with `Host::probe_frame` sizing the output slots up front, as a real PixInsight host must. Also guards against output-slot mis-sizing, which would silently corrupt the shm bands. |
| `test_isolation` | Fault isolation: a worker that exits immediately without ever touching the protocol (`/bin/false` in place of the worker) surfaces as a prompt `HostError`, never a hang; and a mid-run `Host::cancel()` from a second thread stops the run promptly with no partial-output confusion. |

## License note

`third_party/json.hpp` is nlohmann/json v3.11.3, MIT licensed; see the
license header at the top of that file for the full text.
