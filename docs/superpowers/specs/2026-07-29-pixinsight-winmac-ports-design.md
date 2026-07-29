# Plan 3a — PixInsight Windows & macOS native ports (design)

Status: approved for planning (2026-07-29)
Supersedes/extends: `2026-07-28-pixinsight-ci-and-distribution-design.md` §2
(native ports were the explicit non-goal deferred to this plan) and
`2026-07-27-pixinsight-integration-design.md` §5 (transport) / §12 (packaging).

## 1. Goal & scope

Turn the `continue-on-error` **windows** and **macos** placeholder jobs in
`.github/workflows/module.yml` into **real, required** build+test jobs by porting
the shared-memory transport and the C++ host to those platforms.

**In scope (completion gate = green CI on all three OSes):**

- A `#[cfg(windows)]` `ShmSegment` backend in `crates/mmm-core/src/ipc/shm.rs`.
- A Windows port of the C++ host (`integration/pixinsight/host/`): process spawn,
  pipes, shared memory, and the `int fd` → `HANDLE` seam.
- Per-platform PCL builds: a macOS build script and a Windows build script/driver,
  each producing the static PCL library the module links.
- Per-platform module + worker builds, staged as unsigned payloads.
- The host **CTest golden/isolation suite** and `cargo test` (ipc crates) green on
  linux-x64, macos (arm64), and windows-x64.

**Out of scope (explicit follow-ups):**

- Worker **notarization** (macOS) and **Authenticode / Azure Trusted Signing**
  (Windows). CI is green *unsigned* — see §7.
- **macOS GUI smoke test** (no Mac with a licensed PixInsight is available).
- **Windows GUI smoke test** is a *manual* post-merge step the maintainer runs
  (a licensed PixInsight is available on Windows), mirroring how Linux/WSL was
  validated in Plans 2b/3 — it is **not** a CI gate.
- Universal / Intel-macOS binaries (macOS CI targets the runner's native arm64);
  repository *publishing* (the dormant pipeline stays dormant).

## 2. Platform order: macOS first, then Windows

macOS is a build-and-validate job — the POSIX code already compiles (`shm.rs`
only stubs *non-unix*; the host is flat POSIX that clang accepts) and upstream
carries the macOS makefiles. Landing it first proves the Unix path in CI and
lights up two thirds of the matrix before the heavier Windows native port.

## 3. Background: verified facts

Established by inspection (2026-07-29):

- **Rust worker transport is portable except shm.** `mmm-ipc-worker` talks the
  wire protocol over `std::io::stdin/stdout` (portable) and moves pixels over
  `ShmSegment`. `nix`/`libc` are already `[target.'cfg(unix)'.dependencies]`, and
  `shm.rs` stubs only `#[cfg(not(unix))]`. So the macOS (Unix) Rust path compiles
  as-is; only Windows needs a new shm backend.
- **`memmap2` 0.9 maps real file handles only** — it cannot express a Windows
  named *pagefile-backed* segment (the true POSIX-shm analog). The Windows shm
  backend is therefore hand-rolled on `windows-sys`.
- **The C++ host is flat POSIX with zero platform abstraction.** A tree-wide
  grep for `_WIN32`/`#ifdef` finds nothing in the host source. The port surface
  concentrates in `mmm_shm.cpp` and `mmm_host.cpp`, with an `int fd` → `HANDLE`
  seam through `mmm_protocol.cpp`.
- **The shm name is a host↔worker wire contract** carried inside the JSON `Init`
  message, not the shm API. Both sides must agree on the exact name string, so
  any Windows name normalization must be applied identically in C++ and Rust.
- **The Windows PCL core project is absent from the pinned open-source commit**
  (`afea714e`): upstream `src/pcl/` ships only `linux/` and `macosx/` build trees;
  `src/pcl/windows/vc17/PCL.vcxproj` returns 404. It **is** present in the local
  licensed install and is version-matched — see §5.
- **CI needs no signing** (§7).

## 4. macOS job

Runner `macos-latest` (Apple Silicon / arm64). Targets: Rust
`aarch64-apple-darwin`; module `mmm-pxm.dylib`; worker arm64.

- **`integration/pixinsight/ci/build-pcl-macos.sh`** (mirrors `build-pcl.sh`):
  fetch the pinned commit, verify SHA + `Version.cpp`, then
  `cd src/pcl/macosx/g++ && make -f makefile-arm64 -j$(sysctl -n hw.ncpu)`,
  producing `libPCL-pxi.a`. The upstream makefile hardcodes an Xcode `isysroot`
  path; the script rewrites it to `$(xcrun --show-sdk-path)` for runner
  robustness. Publishes `lib/libPCL-pxi.a` + `include/` to the output prefix,
  same shape as the Linux script.
- **`integration/pixinsight/ci/build-module-macos.sh`** (mirrors
  `build-module-linux.sh`): build `mmm-pxm.dylib` (the module makefile/CMake grows
  a macOS branch: `-dynamiclib`, `.dylib` suffix, **no `-lrt`** — macOS provides
  `shm_open` in libc), enforce warning-free, verify host symbols are linked, build
  the worker, stage `bin/{mmm-pxm.dylib,mmm-ipc-worker}`.
- **Host CMake**: guard the Linux-only `rt` link (`if(NOT WIN32 AND NOT APPLE)`,
  or `target_link_libraries` conditioned appropriately). No source changes — the
  POSIX host compiles under clang.
- **CI gate**: host CTest suite + `cargo test -p mmm-core -p mmm-ipc-worker` on the
  runner. Unsigned (source build ⇒ no `com.apple.quarantine`, so Gatekeeper does
  not gate the spawned worker in CI).

## 5. Windows job

Runner `windows-latest` (VS 2022 / MSVC v143). Targets: Rust
`x86_64-pc-windows-msvc`; module `mmm-pxm.dll`; worker `mmm-ipc-worker.exe`.

### 5.1 PCL build (blocker resolved)

The version-matched `PCL.vcxproj` is lifted from the local licensed install at
`/opt/PixInsight/src/pcl/windows/vc17/PCL.vcxproj` (generated by PixInsight
Makefile Generator v1.147 for PCL 2.10.4 — matches the `afea714e` pin) and pinned
in-repo under `integration/pixinsight/ci/win/PCL.vcxproj`. It is self-contained:
`StaticLibrary`, `v143`, `stdcpp20`, `/permissive- /Zc:__cplusplus`, AVX2/FMA,
`MultiThreadedDLL`, `TargetName=PCL-pxi`; it imports only standard
`Microsoft.Cpp.*` targets (no custom `.props` to also lift) and references the core
`..\..\*.cpp` sources by relative path. (The `.filters` file is IDE-only and not
required for the build.)

**`integration/pixinsight/ci/build-pcl-windows.ps1`**: fetch the pinned commit,
verify SHA + `Version.cpp`, **create `src/pcl/windows/vc17/` and drop the pinned
`PCL.vcxproj` in** (upstream omits this dir), set `PCLINCDIR`/`PCLSRCDIR`/
`PCLLIBDIR64`, run `msbuild PCL.vcxproj /t:Build /p:Configuration=Release
/p:Platform=x64 -m`, and publish `PCL-pxi.lib` + `include/` to the output prefix.

**Validation gate:** because the pinned `.vcxproj` carries a fixed source-file
list, the script diffs that list against the fetched commit's `src/pcl/*.cpp`; any
add/remove fails loudly rather than silently omitting a translation unit. (Both are
2.10.4, so this should be a no-op — it guards a future pin bump.)

Licensing note: PCL build files are part of the open-source PCL distribution; pinning
the generated `.vcxproj` in-repo is consistent with that license. Record provenance
in the file header / a sibling README.

### 5.2 Rust shm backend

A new `#[cfg(windows)]` `ShmSegment` in `shm.rs` (replacing the current
`#[cfg(not(unix))]` stub with `#[cfg(all(not(unix), not(windows)))]` for genuinely
unsupported targets), built on a `[target.'cfg(windows)'.dependencies]` on
`windows-sys`:

- `create(name, total_bytes)`: `CreateFileMappingW(INVALID_HANDLE_VALUE,
  PAGE_READWRITE, hi, lo, name)` (pagefile-backed named mapping) + `MapViewOfFile`.
  Store the base pointer, size, `HANDLE`, and `is_creator`.
- `attach(name, total_bytes)`: `OpenFileMappingW(FILE_MAP_ALL_ACCESS, …, name)` +
  `MapViewOfFile`; verify mapped size matches `total_bytes`.
- `slice`/`slice_mut`/`slice_mut_raw`/`checked_range`: **unchanged logic** — same
  bounds/alignment checks and the same raw-pointer interior-mutability pattern over
  the mapped base pointer.
- `Drop`: `UnmapViewOfFile` + `CloseHandle`. No `shm_unlink` analog — a Windows
  named mapping is refcounted by open handles and vanishes when the last handle
  closes. The creator/attacher distinction that mattered for POSIX unlink ordering
  becomes moot; document this divergence explicitly.
- Public API surface (`create`/`attach`/`slice`/`slice_mut`/`Drop`) is identical
  to the Unix impl. `#[cfg(all(test, windows))]` unit tests mirror the Unix tests
  (packed offsets, misaligned rejection, create-write / attach-read round-trip).

### 5.3 Name normalization (host↔worker contract)

POSIX shm names use a leading slash (`/mmm-…`). Windows named-mapping objects use
a different namespace and forbid backslashes in the base name. A single
normalization rule — strip a leading `/`, prefix `Local\`, e.g. `/mmm-foo` →
`Local\mmm-foo` — is applied **identically** in the Rust `shm.rs` Windows path and
the C++ `mmm_shm.cpp` Windows path, so the name string carried in the JSON `Init`
message round-trips unchanged (each side normalizes the same input the same way).

### 5.4 C++ host port

Concentrated, header interfaces mostly stable:

- **`mmm_shm.cpp`**: reimplement `ShmSegment` create/map/unmap on
  `CreateFileMappingW`/`OpenFileMappingW` + `MapViewOfFile`/`UnmapViewOfFile`/
  `CloseHandle`; size is passed to `CreateFileMapping` (no `ftruncate`); no
  `shm_unlink` (handle-refcount lifetime); Win32 error text via
  `GetLastError`/`FormatMessage`. The `mmm_shm.h` interface (`create`, `base()`,
  `size()`, `slot_floats()`, `name()`) is OS-neutral and does not change.
- **`mmm_host.cpp`**: `posix_spawn`+file-actions → `CreateProcessW` with
  `STARTF_USESTDHANDLES`; `pipe` → `CreatePipe` (+ `SetHandleInformation` for the
  child-inherited ends only); `waitpid`/`WIF*` → `WaitForSingleObject` +
  `GetExitCodeProcess`; `kill(SIGKILL)` → `TerminateProcess`; `read`/`write` →
  `ReadFile`/`WriteFile`. Drop `SIGPIPE` handling (no analog); remap the EPIPE
  detection in the write path to `ERROR_BROKEN_PIPE`/`ERROR_NO_DATA`. Member types
  `int fd`/`pid_t` become `HANDLE`, with `INVALID_HANDLE_VALUE` (not `-1`) sentinels.
- **`mmm_protocol.cpp`/`.h`**: change the `int fd` params of `read_worker_frame`/
  `write_frame_raw`/`full_read`/`full_write` to `HANDLE`; swap `::read`/`::write`
  for `ReadFile`/`WriteFile`; drop `<unistd.h>`. Wire format/endianness unchanged.
- **CMake**: `if(WIN32)` guards — drop `rt`/pthreads link, MSVC `/W4` in place of
  `-Wall -Wextra -fPIC`, `.exe` suffix on the `MMM_WORKER` default and cargo target
  path (`target/debug/mmm-ipc-worker.exe`).
- **Tests**: `test_isolation.cpp` crash-worker `/bin/false` → a Windows immediate-
  exit equivalent (`cmd /c exit 1`); `getpid` → `GetCurrentProcessId`; shm-name
  literals adjusted for the normalization rule in §5.3; `test_protocol.cpp`'s raw
  `pipe`/`write`/`close` round-trip → `CreatePipe`/`WriteFile`/`CloseHandle`.

### 5.5 Windows module build

The module `Makefile`/`makefile-x64` is GNU-make + g++ and does not port. Add a
Windows build path — the pragmatic choice is a small CMake target (or a pinned
`.vcxproj`) that compiles the module sources + the `host/` objects, links
`PCL-pxi.lib` and the required Windows import libs, and emits `mmm-pxm.dll`.
`build-module-windows.ps1` drives it, enforces a warning-free build, verifies the
host symbols are present, builds the worker (`cargo build --release
--target x86_64-pc-windows-msvc -p mmm-ipc-worker`), and stages
`bin/{mmm-pxm.dll,mmm-ipc-worker.exe}`. CI gate: host CTest + `cargo test`.

## 6. Packaging (unchanged)

`gen-package.sh` (GNU-tar-only) stays on the Linux signer host and bundles
pre-built per-OS artifacts. Windows/macOS runners build and **upload** their
`.dll`/`.dylib`/`.exe`; packaging/signing remains a Linux-host concern. No
BSD/macOS-tar port is needed.

## 7. Signing/notarization is orthogonal to CI-green

CI builds from source, so:

- **macOS**: no `com.apple.quarantine` xattr is applied to a source-built binary,
  so Gatekeeper does not gate the spawned worker on the runner. Notarization
  matters only for a *downloaded/quarantined* worker at end-user install time
  (deferred; pkg-install and build-from-source both escape the quarantine block).
- **Windows**: an unsigned spawned `.exe` runs; SmartScreen only warns on
  *download* of the distributed package. Authenticode/Trusted Signing deferred.

Therefore all three CI jobs go green **unsigned**, and signing is a separate
follow-up plan gated on an Apple Developer account / code-signing certificate.

## 8. Testing strategy (TDD)

Each port task is test-first against existing specs: the CTest golden/isolation
suite and the `cargo test` shm tests are the behavioral contract and must pass on
the new OS (modulo the platform name/fixture tweaks in §4–§5). The Windows shm
backend adds `#[cfg(all(test, windows))]` unit tests mirroring the Unix ones. A
task is "done" only when its target platform's CTest + cargo tests are green in CI.

## 9. CI wiring (`.github/workflows/module.yml`)

Replace the two placeholder jobs with real jobs mirroring the linux job's shape:
checkout → toolchain → PCL cache (keyed `pcl-${{ runner.os }}-<sha>`) → build PCL
on miss → build module+worker+stage → `cargo test` → host CTest → upload artifacts.
Remove `continue-on-error`. The PCL cache key already includes `runner.os`, so the
three platforms cache independently. macOS uses bash; Windows steps use
`pwsh`/`shell: pwsh` for the `.ps1` drivers.

## 10. Execution plan

New branch `pixinsight-winmac-ports` off `main`. subagent-driven-development: a
`.superpowers/sdd/` ledger, a fresh implementer per task, scoped review + fix loop
per task, `main` kept green. Rough task order (finalized in the implementation
plan):

1. macOS PCL build script + CI job green (build + CTest + cargo tests).
2. macOS module build script + host CMake `rt`/dylib branch; macOS job required.
3. Windows: pin `PCL.vcxproj` + `build-pcl-windows.ps1` + PCL cache; produce
   `PCL-pxi.lib` in CI.
4. Windows Rust shm backend (`windows-sys`) + `#[cfg(windows)]` tests; `cargo test`
   green on windows.
5. Windows C++ host port (shm, spawn/pipes, protocol `HANDLE` seam, CMake, tests);
   host CTest green on windows.
6. Windows module build (`build-module-windows.ps1`) + CI job; remove
   `continue-on-error`; windows job required.
7. Docs: update `integration/pixinsight/*/README.md` and the CI/distribution spec
   cross-refs; note the manual Windows GUI smoke-test procedure.

## 11. Risks & open items

- **Source-list drift** between the pinned `.vcxproj` and a future PCL pin —
  guarded by the §5.1 diff check.
- **`CUDADevice.cpp`** compiles without the CUDA toolkit on Linux; confirm the same
  on macOS/MSVC (the Linux script already documents the drop-TU fallback).
- **`ExceptionHandling=Async` + `/EHa`** and `MultiThreadedDLL` (`/MD`) must match
  between `PCL-pxi.lib`, the host objects, and the module DLL to avoid CRT/ABI
  mismatches — the module Windows build must use the same runtime + EH model as the
  pinned `.vcxproj`.
- **Windows worker cargo target** differs (`target/x86_64-pc-windows-msvc/release/
  mmm-ipc-worker.exe`); the module's sibling-worker path resolution
  (`dladdr`-equivalent → `GetModuleFileNameW`) must find the `.exe` beside the DLL.
