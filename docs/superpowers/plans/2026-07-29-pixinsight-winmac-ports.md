# PixInsight Windows & macOS Native Ports Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the `continue-on-error` Windows/macOS placeholder jobs in `.github/workflows/module.yml` into required, green build+test jobs by porting the shm transport and the C++ host to macOS (build-only) and Windows (native port).

**Architecture:** macOS is a POSIX build-and-validate job — the existing Unix code compiles under clang; only per-platform PCL/module build scripts and a CMake link guard are new. Windows is a real native port: a `windows-sys` shm backend on the Rust side, a Win32 (`CreateProcessW`/`CreateFileMapping`/`HANDLE`) port of the C++ host behind `#ifdef _WIN32`, and an MSVC PCL + module build driven by a version-matched `PCL.vcxproj` lifted from the licensed install.

**Tech Stack:** Rust (`windows-sys` on `cfg(windows)`, `nix`/`libc` on `cfg(unix)`), C++20, CMake + CTest, GitHub Actions (ubuntu/macos/windows runners), MSVC v143, PowerShell (`pwsh`) for Windows CI drivers.

**Source spec:** `docs/superpowers/specs/2026-07-29-pixinsight-winmac-ports-design.md`

## Global Constraints

- **Wire protocol unchanged.** `IPC_PROTOCOL_VERSION = 2`; frame format and little-endian encoding in `crates/mmm-core/src/ipc/protocol.rs` and `integration/pixinsight/host/mmm_protocol.*` are **not** modified. `integration/pixinsight/PROTOCOL.md` stays authoritative.
- **PCL pin is the single source of truth:** `afea714e681853dfc21e70b5d53811ae41849e97` == PCL 2.10.4 / PixInsight 1.9.4, in `integration/pixinsight/ci/pcl-pin.env`. Every platform builds this exact commit.
- **`missing_docs` is warned in `mmm-core`:** every new public item needs a doc comment. Keep `cargo fmt`, `cargo clippy --all-targets`, and `cargo doc` warning-free.
- **Module builds must be warning-free** (`-Wall`/`-Wextra` on gcc/clang; `/W4` on MSVC is a binding gate, mirrored from `build-module-linux.sh`).
- **`main` stays green.** All work on branch `pixinsight-winmac-ports` (already created). The existing linux CI job must remain green after every task.
- **No signing in CI.** Workers ship unsigned; CI-green is the completion gate (spec §7). Do not add codesign/notarize/signtool steps.
- **macOS CI target is the runner's native arch (arm64).** Rust `aarch64-apple-darwin`, module `mmm-pxm.dylib`. Windows target `x86_64-pc-windows-msvc`, module `mmm-pxm.dll`, worker `mmm-ipc-worker.exe`.
- **shm name normalization (host↔worker contract):** POSIX `/name` maps to Windows `Local\name` by the identical rule on both the Rust and C++ sides. The name string carried in the JSON `Init` message is unchanged.

## Cross-platform verification model (read this first)

Implementers run on Linux/WSL and **cannot** compile MSVC C++, run Windows CTest, or run the macOS toolchain locally. Verification is therefore split:

- **Local gate (every task):** the Linux build + `cargo test -p mmm-core -p mmm-ipc-worker` + Linux host CTest must still pass; `cargo fmt --check`, `cargo clippy --all-targets`, and `cargo doc` must be clean. All new Windows C++ code is guarded by `#ifdef _WIN32` / `#else` so it is *additive* and cannot regress the POSIX build. For Rust Windows code, add the mingw cross target and run `cargo check --target x86_64-pc-windows-gnu -p mmm-core` (see Task 4 setup) as a compile-level proxy — `windows-sys` compiles for the gnu target, so this typechecks the `cfg(windows)` module even though the shipping target is msvc.
- **Authoritative gate (platform tasks):** the named GitHub Actions job (`build+test (macos-arm64)` / `build+test (windows-x64)`) going green. The workflow triggers on push to the paths this plan touches, so pushing the branch runs it. **The orchestrator pushes the branch after each platform task and confirms the target CI job's result before marking the task done**; a red job is fed back to a fresh fixer subagent with the CI log excerpt. Task "Expected" outcomes below name which CI job must be green.

## File Structure

**Created:**
- `integration/pixinsight/ci/build-pcl-macos.sh` — build `libPCL-pxi.a` from the pinned PCL on macOS (clang, arm64).
- `integration/pixinsight/ci/build-module-macos.sh` — build `mmm-pxm.dylib` + worker, stage payload.
- `integration/pixinsight/ci/win/PCL.vcxproj` — version-matched core PCL MSVC project (pinned artifact), lifted from the licensed install.
- `integration/pixinsight/ci/build-pcl-windows.ps1` — build `PCL-pxi.lib` from the pinned PCL on Windows (msbuild).
- `integration/pixinsight/ci/build-module-windows.ps1` — build `mmm-pxm.dll` + worker, stage payload.
- `integration/pixinsight/host/mmm_os.h` — the platform seam: `os_handle` typedef + invalid-handle constant + `os_read`/`os_write`/`os_close` helpers.
- `integration/pixinsight/host/CMakeLists.win.md` — (docs) not needed; see README updates instead.

**Modified:**
- `crates/mmm-core/src/ipc/shm.rs` — add `#[cfg(windows)]` `ShmSegment`; narrow the stub cfg; add `#[cfg(all(test, windows))]` tests.
- `crates/mmm-core/Cargo.toml` — add `[target.'cfg(windows)'.dependencies] windows-sys`.
- `integration/pixinsight/host/mmm_shm.cpp` / `mmm_shm.h` — Win32 shm branch.
- `integration/pixinsight/host/mmm_host.cpp` / `mmm_host.h` — Win32 spawn/pipe/wait branch; `os_handle` members.
- `integration/pixinsight/host/mmm_protocol.cpp` / `mmm_protocol.h` — `int fd` → `os_handle`.
- `integration/pixinsight/host/CMakeLists.txt` — `if(WIN32)`/`if(APPLE)` guards, MSVC flags, `.exe` worker default.
- `integration/pixinsight/host/test/*.cpp` — Windows fixture equivalents behind `#ifdef _WIN32`.
- `.github/workflows/module.yml` — replace the two placeholder jobs with real jobs.
- `integration/pixinsight/host/README.md`, `integration/pixinsight/module/README.md`, `integration/pixinsight/ci/`(new README) — document per-platform builds.

---

## Task 1: macOS PCL build script + macOS CI job (PCL only)

Establish the macOS runner and prove the pinned PCL builds under clang, cached like Linux.

**Files:**
- Create: `integration/pixinsight/ci/build-pcl-macos.sh`
- Modify: `.github/workflows/module.yml` (replace the `macos` placeholder job)

**Interfaces:**
- Produces: `build-pcl-macos.sh --out <prefix>` writes `<prefix>/lib/libPCL-pxi.a` and `<prefix>/include/` — identical output contract to `build-pcl.sh`, consumed by Task 2's `build-module-macos.sh`.

- [ ] **Step 1: Write `build-pcl-macos.sh`**

Create `integration/pixinsight/ci/build-pcl-macos.sh` (mode 0755):

```bash
#!/usr/bin/env bash
# Build libPCL-pxi.a from the pinned open-source PCL on macOS (clang, arm64),
# out-of-tree, no PixInsight. Mirrors build-pcl.sh. Usage:
#   build-pcl-macos.sh --out <prefix-dir> [--work <clone-dir>]
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "$HERE/pcl-pin.env"

OUT="" ; WORK=""
while [ $# -gt 0 ]; do
  case "$1" in
    --out)  OUT="$2"; shift 2 ;;
    --work) WORK="$2"; shift 2 ;;
    --help) echo "usage: build-pcl-macos.sh --out <dir> [--work <dir>]"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done
[ -n "$OUT" ] || { echo "--out is required" >&2; exit 2; }
WORK="${WORK:-$(mktemp -d)}"
mkdir -p "$OUT/lib" "$OUT/include" "$WORK"

cd "$WORK"
if [ ! -d .git ]; then
  git init -q
  git remote add origin "$PCL_REPO_URL"
fi
git fetch -q --depth 1 origin "$PCL_SHA"
git checkout -q FETCH_HEAD
HEAD_SHA="$(git rev-parse HEAD)"
[ "$HEAD_SHA" = "$PCL_SHA" ] || { echo "PCL SHA mismatch: got $HEAD_SHA want $PCL_SHA" >&2; exit 1; }

# Soft version check (SHA is the hard gate) — same accessor grep as build-pcl.sh.
VER_CPP="src/pcl/Version.cpp"
if [ -f "$VER_CPP" ]; then
  check_ver_fn() { grep -A2 "^int Version::$1()" "$VER_CPP" | grep -qE "return[[:space:]]+$2[[:space:]]*;"; }
  check_ver_fn Major   "$PCL_VER_MAJOR"   || echo "warning: Version.cpp major != $PCL_VER_MAJOR" >&2
  check_ver_fn Minor   "$PCL_VER_MINOR"   || echo "warning: Version.cpp minor != $PCL_VER_MINOR" >&2
  check_ver_fn Release "$PCL_VER_RELEASE" || echo "warning: Version.cpp release != $PCL_VER_RELEASE" >&2
fi

# The upstream macosx makefile hardcodes an Xcode isysroot; retarget it to the
# runner's installed SDK for robustness. arm64 is the macos-latest arch.
MK="src/pcl/macosx/g++/makefile-arm64"
[ -f "$MK" ] || { echo "expected $MK in PCL tree" >&2; exit 1; }
SDK="$(xcrun --show-sdk-path)"
# Replace any hardcoded '-isysroot <path>' with the discovered SDK path.
/usr/bin/sed -i '' -E "s#-isysroot [^[:space:]]+#-isysroot ${SDK//#/\\#}#g" "$MK"

export PCLDIR="$WORK"
export PCLSRCDIR="$WORK/src"
export PCLINCDIR="$WORK/include"
export PCLLIBDIR64="$WORK/lib/macosx/x64"
export PCLBINDIR64="$WORK/bin"
mkdir -p "$PCLLIBDIR64" "$PCLBINDIR64"
( cd src/pcl/macosx/g++ && make -f makefile-arm64 -j"$(sysctl -n hw.ncpu)" )

LIB="$(find "$WORK/src/pcl" -name libPCL-pxi.a -print -quit)"
[ -n "$LIB" ] || { echo "libPCL-pxi.a not produced" >&2; exit 1; }
cp -f "$LIB" "$OUT/lib/libPCL-pxi.a"
cp -a "$WORK/include/." "$OUT/include/"
echo "PCL built: $OUT/lib/libPCL-pxi.a, headers in $OUT/include"
```

- [ ] **Step 2: Replace the `macos` placeholder job in `module.yml`**

Replace the entire `macos:` job (currently lines ~114-125) with a real job that builds+caches PCL only (module/tests come in Task 2). Keep it **without** `continue-on-error` from the start so a PCL build failure is visible:

```yaml
  macos:
    name: build+test (macos-arm64)
    runs-on: macos-latest
    steps:
      - uses: actions/checkout@v4

      - name: Install stable Rust
        uses: dtolnay/rust-toolchain@stable

      - name: Cache cargo build artifacts
        uses: Swatinem/rust-cache@v2

      - name: Read PCL pin
        id: pcl
        run: |
          . integration/pixinsight/ci/pcl-pin.env
          echo "sha=$PCL_SHA" >> "$GITHUB_OUTPUT"

      - name: Cache libPCL-pxi.a (keyed on PCL commit)
        id: pcl-cache
        uses: actions/cache/restore@v4
        with:
          path: pcl-ci
          key: pcl-${{ runner.os }}-${{ steps.pcl.outputs.sha }}

      - name: Build PCL (on cache miss)
        if: steps.pcl-cache.outputs.cache-hit != 'true'
        run: bash integration/pixinsight/ci/build-pcl-macos.sh --out "$PWD/pcl-ci"

      - name: Save libPCL-pxi.a cache (only after a successful build)
        if: steps.pcl-cache.outputs.cache-hit != 'true'
        uses: actions/cache/save@v4
        with:
          path: pcl-ci
          key: pcl-${{ runner.os }}-${{ steps.pcl.outputs.sha }}

      - name: Confirm PCL archive
        run: test -f "$PWD/pcl-ci/lib/libPCL-pxi.a"
```

- [ ] **Step 3: Local non-regression check**

Run: `bash -n integration/pixinsight/ci/build-pcl-macos.sh` (syntax) and `shellcheck integration/pixinsight/ci/build-pcl-macos.sh` if available.
Expected: no syntax errors. (The build itself cannot run locally.)

- [ ] **Step 4: Commit**

```bash
git add integration/pixinsight/ci/build-pcl-macos.sh .github/workflows/module.yml
git commit -m "feat(ci): macOS PCL build script + macos CI job (PCL build)"
```

- [ ] **Step 5: Push and confirm CI (orchestrator)**

Push the branch. Expected: the **build+test (macos-arm64)** job reaches "Confirm PCL archive" green (PCL builds under clang; `CUDADevice.cpp` compiles without the CUDA toolkit — if it fails on `cuda.h`, drop that TU in the script per the Linux script's documented fallback). Feed any failure to a fixer subagent with the log.

---

## Task 2: macOS module build + host CMake portability; macOS job required

Build the `.dylib` + worker on macOS and run the full test suite, making the macOS job a real gate.

**Files:**
- Create: `integration/pixinsight/ci/build-module-macos.sh`
- Modify: `integration/pixinsight/host/CMakeLists.txt`, `integration/pixinsight/module/makefile-x64` (add an APPLE branch) and/or `integration/pixinsight/module/Makefile`, `.github/workflows/module.yml`

**Interfaces:**
- Consumes: `build-pcl-macos.sh` output prefix (`lib/`, `include/`).
- Produces: `build-module-macos.sh --pcl <prefix> --stage <dir>` writes `<dir>/bin/{mmm-pxm.dylib,mmm-ipc-worker}`.

- [ ] **Step 1: Guard the Linux-only `rt` link in host CMake**

In `integration/pixinsight/host/CMakeLists.txt`, the `mmm_host` target links `Threads::Threads` and `rt`. `rt` exists only on Linux (macOS provides `shm_open` in libc). Change the link line to guard `rt`:

```cmake
target_link_libraries(mmm_host PUBLIC Threads::Threads)
if(NOT WIN32 AND NOT APPLE)
  target_link_libraries(mmm_host PUBLIC rt)
endif()
```

- [ ] **Step 2: Verify Linux host still builds + CTest passes (local gate)**

Run:
```bash
cmake -S integration/pixinsight/host -B integration/pixinsight/host/ci-build
cmake --build integration/pixinsight/host/ci-build
ctest --test-dir integration/pixinsight/host/ci-build --output-on-failure
```
Expected: PASS (the `rt` guard is a no-op on Linux — `NOT WIN32 AND NOT APPLE` is true, so `rt` still links).

- [ ] **Step 3: Add a macOS branch to the module build**

The module build (`integration/pixinsight/module/makefile-x64`, invoked via `Makefile`) is gcc/`-shared`/`.so`. Add an APPLE branch producing a `.dylib`. In `makefile-x64`, detect the platform and set:

```make
UNAME_S := $(shell uname -s)
ifeq ($(UNAME_S),Darwin)
  CXX ?= clang++
  MODULE_EXT := dylib
  SHARED_FLAGS := -dynamiclib -install_name @rpath/mmm-pxm.dylib
else
  CXX ?= g++
  MODULE_EXT := so
  SHARED_FLAGS := -shared
endif
```

Replace the hardcoded `mmm-pxm.so` target/output and the `-shared` link flag with `mmm-pxm.$(MODULE_EXT)` and `$(SHARED_FLAGS)`. Keep `-Wall -Wextra` and the existing `PCLINCDIR`/`PCLLIBDIR` variables. (Read the current `makefile-x64` first and preserve its include/link structure; only the compiler, extension, and shared-link flag are platform-conditional.)

- [ ] **Step 4: Write `build-module-macos.sh`**

Create `integration/pixinsight/ci/build-module-macos.sh` (mode 0755), mirroring `build-module-linux.sh` but for `.dylib` and the `aarch64-apple-darwin` worker:

```bash
#!/usr/bin/env bash
# Build the PCL module (.dylib) + worker on macOS, stage the unsigned payload.
# Usage: build-module-macos.sh --pcl <prefix> --stage <dir>
set -euo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

PCL="" ; STAGE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --pcl)   PCL="$2"; shift 2 ;;
    --stage) STAGE="$2"; shift 2 ;;
    --help)  echo "usage: build-module-macos.sh --pcl <prefix> --stage <dir>"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done
[ -n "$PCL" ] && [ -n "$STAGE" ] || { echo "--pcl and --stage are required" >&2; exit 2; }

MODULE_DIR="$REPO_ROOT/integration/pixinsight/module"
make -C "$MODULE_DIR" clean

build_log="$(mktemp)"
trap 'rm -f "$build_log"' EXIT
make -C "$MODULE_DIR" PCLINCDIR="$PCL/include" PCLLIBDIR="$PCL/lib" 2>&1 | tee "$build_log"

if grep -q 'warning:' "$build_log"; then
  echo "ERROR: warnings in module build" >&2
  grep 'warning:' "$build_log" >&2
  exit 1
fi

DYLIB="$MODULE_DIR/mmm-pxm.dylib"
[ -f "$DYLIB" ] || { echo "mmm-pxm.dylib not built" >&2; exit 1; }
# Host objects must be linked in: any undefined mmm:: symbol means they weren't.
if nm -u "$DYLIB" | grep -E ' _?_ZN3mmm' ; then
  echo "ERROR: undefined mmm:: symbols in mmm-pxm.dylib (host objects not linked)" >&2
  exit 1
fi

( cd "$REPO_ROOT" && cargo build --release -p mmm-ipc-worker )
WORKER="$REPO_ROOT/target/release/mmm-ipc-worker"
[ -f "$WORKER" ] || { echo "mmm-ipc-worker not built" >&2; exit 1; }

mkdir -p "$STAGE/bin"
cp -f "$DYLIB" "$STAGE/bin/mmm-pxm.dylib"
cp -f "$WORKER" "$STAGE/bin/mmm-ipc-worker"
echo "staged unsigned payload in $STAGE/bin: $(ls "$STAGE/bin")"
```

- [ ] **Step 5: Extend the macOS CI job**

After the "Confirm PCL archive" step from Task 1, append module build + tests (mirroring the linux job's test steps):

```yaml
      - name: Build module + worker, stage payload
        run: |
          bash integration/pixinsight/ci/build-module-macos.sh \
            --pcl "$PWD/pcl-ci" --stage "$PWD/stage"

      - name: Rust tests (ipc crates)
        run: cargo test -p mmm-core -p mmm-ipc-worker

      - name: Host golden CTest suite (no PixInsight)
        run: |
          cmake -S integration/pixinsight/host -B integration/pixinsight/host/ci-build
          cmake --build integration/pixinsight/host/ci-build
          ctest --test-dir integration/pixinsight/host/ci-build --output-on-failure

      - name: Upload unsigned module artifacts
        uses: actions/upload-artifact@v4
        with:
          name: mmm-pxm-macos-arm64
          path: stage/bin/**
          if-no-files-found: error
```

- [ ] **Step 6: Commit**

```bash
git add integration/pixinsight/ci/build-module-macos.sh \
        integration/pixinsight/host/CMakeLists.txt \
        integration/pixinsight/module/makefile-x64 \
        .github/workflows/module.yml
git commit -m "feat(ci): macOS module+worker build, host CMake rt guard; macos job required"
```

- [ ] **Step 7: Push and confirm CI (orchestrator)**

Expected: **build+test (macos-arm64)** fully green (PCL cached from Task 1, `.dylib` builds warning-free, `cargo test` green — the Unix shm path runs unchanged on macOS — host CTest green). Also confirm **build+test (linux-x64)** stayed green. Feed failures to a fixer subagent.

---

## Task 3: Windows PCL project pin + `build-pcl-windows.ps1` + Windows PCL CI

Resolve the missing-upstream-vcxproj blocker and build `PCL-pxi.lib` on the Windows runner.

**Files:**
- Create: `integration/pixinsight/ci/win/PCL.vcxproj` (pinned artifact)
- Create: `integration/pixinsight/ci/build-pcl-windows.ps1`
- Modify: `.github/workflows/module.yml` (replace the `windows` placeholder job)

**Interfaces:**
- Produces: `build-pcl-windows.ps1 -Out <prefix>` writes `<prefix>/lib/PCL-pxi.lib` and `<prefix>/include/` — analogous to the Unix scripts, consumed by Task 6's module build.

- [ ] **Step 1: Pin the version-matched `PCL.vcxproj`**

Copy the version-matched project (PCL 2.10.4, matches the pin) from the licensed install into the repo:

```bash
mkdir -p integration/pixinsight/ci/win
cp /opt/PixInsight/src/pcl/windows/vc17/PCL.vcxproj integration/pixinsight/ci/win/PCL.vcxproj
```

Then verify it is the expected self-contained project (StaticLibrary, `v143`, `TargetName=PCL-pxi`, uses `$(PCLINCDIR)`/`$(PCLSRCDIR)`/`$(PCLLIBDIR64)`, imports only `Microsoft.Cpp.*`):

```bash
grep -E 'StaticLibrary|<PlatformToolset>|TargetName|PCLLIBDIR64|Microsoft.Cpp' \
  integration/pixinsight/ci/win/PCL.vcxproj
```

Expected: shows `StaticLibrary`, `v143`, `PCL-pxi`, `$(PCLLIBDIR64)\PCL-pxi.lib`, and only `Microsoft.Cpp.Default.props`/`Microsoft.Cpp.props`/`Microsoft.Cpp.targets` imports. Add a 3-line provenance header comment inside the file (below the existing generator banner) recording: lifted from PixInsight 1.9.4 install `src/pcl/windows/vc17/`, matches PCL pin `afea714e`, open-source PCL build file.

- [ ] **Step 2: Write `build-pcl-windows.ps1`**

Create `integration/pixinsight/ci/build-pcl-windows.ps1`:

```powershell
# Build PCL-pxi.lib from the pinned open-source PCL on Windows (msbuild/MSVC).
# The upstream commit omits src/pcl/windows/vc17/PCL.vcxproj, so we drop the
# repo-pinned, version-matched project into the fetched tree before building.
#   build-pcl-windows.ps1 -Out <prefix-dir> [-Work <clone-dir>]
[CmdletBinding()]
param(
  [Parameter(Mandatory=$true)][string]$Out,
  [string]$Work
)
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path

# Read the pin (shell env file: KEY="value" lines).
$pin = @{}
Get-Content (Join-Path $here 'pcl-pin.env') | ForEach-Object {
  if ($_ -match '^\s*([A-Z_]+)="?([^"]*)"?\s*$') { $pin[$Matches[1]] = $Matches[2] }
}
$sha = $pin['PCL_SHA']; $repo = $pin['PCL_REPO_URL']
if (-not $sha) { throw 'PCL_SHA not found in pcl-pin.env' }

if (-not $Work) { $Work = Join-Path $env:RUNNER_TEMP ("pcl-" + $sha.Substring(0,8)) }
New-Item -ItemType Directory -Force -Path $Work, "$Out/lib", "$Out/include" | Out-Null

Push-Location $Work
if (-not (Test-Path .git)) { git init -q; git remote add origin $repo }
git fetch -q --depth 1 origin $sha
git checkout -q FETCH_HEAD
$head = (git rev-parse HEAD).Trim()
if ($head -ne $sha) { throw "PCL SHA mismatch: got $head want $sha" }

# Drop the pinned core project into the (upstream-absent) windows build dir.
New-Item -ItemType Directory -Force -Path 'src/pcl/windows/vc17' | Out-Null
Copy-Item (Join-Path $here 'win/PCL.vcxproj') 'src/pcl/windows/vc17/PCL.vcxproj' -Force

# Guard against source-list drift: every ClCompile in the pinned project must
# exist in the fetched tree, and no new src/pcl/*.cpp should be missing from it.
$proj = [xml](Get-Content 'src/pcl/windows/vc17/PCL.vcxproj')
$ns = @{ m = 'http://schemas.microsoft.com/developer/msbuild/2003' }
$listed = Select-Xml -Xml $proj -Namespace $ns -XPath '//m:ClCompile/@Include' |
  ForEach-Object { Split-Path $_.Node.Value -Leaf } | Sort-Object -Unique
$actual = Get-ChildItem 'src/pcl' -Filter *.cpp | ForEach-Object { $_.Name } | Sort-Object -Unique
$missing = $listed | Where-Object { $_ -notin $actual }
$extra   = $actual | Where-Object { $_ -notin $listed }
if ($missing) { throw "PCL.vcxproj lists sources absent from the pinned tree: $($missing -join ', ')" }
if ($extra)   { throw "pinned PCL tree has sources not in PCL.vcxproj (pin bump?): $($extra -join ', ')" }

$env:PCLDIR = $Work
$env:PCLSRCDIR = Join-Path $Work 'src'
$env:PCLINCDIR = Join-Path $Work 'include'
$env:PCLLIBDIR64 = Join-Path $Work 'lib\windows\x64'
$env:PCLBINDIR64 = Join-Path $Work 'bin'
New-Item -ItemType Directory -Force -Path $env:PCLLIBDIR64, $env:PCLBINDIR64 | Out-Null

msbuild 'src\pcl\windows\vc17\PCL.vcxproj' /t:Build /m `
  /p:Configuration=Release /p:Platform=x64
if ($LASTEXITCODE -ne 0) { throw "msbuild failed ($LASTEXITCODE)" }

$lib = Get-ChildItem $Work -Recurse -Filter 'PCL-pxi.lib' | Select-Object -First 1
if (-not $lib) { throw 'PCL-pxi.lib not produced' }
Copy-Item $lib.FullName "$Out/lib/PCL-pxi.lib" -Force
Copy-Item "$Work/include/*" "$Out/include/" -Recurse -Force
Pop-Location
Write-Host "PCL built: $Out/lib/PCL-pxi.lib, headers in $Out/include"
```

- [ ] **Step 3: Replace the `windows` placeholder job (PCL build only)**

Replace the `windows:` job in `module.yml` with a real job that builds+caches `PCL-pxi.lib`. Keep `continue-on-error: true` **for now** (removed in Task 6, after the module build lands), so intermediate Windows tasks can iterate without failing the required checks:

```yaml
  windows:
    name: build+test (windows-x64)
    runs-on: windows-latest
    continue-on-error: true
    steps:
      - uses: actions/checkout@v4

      - name: Install stable Rust (msvc)
        uses: dtolnay/rust-toolchain@stable

      - name: Cache cargo build artifacts
        uses: Swatinem/rust-cache@v2

      - name: Add MSBuild to PATH
        uses: microsoft/setup-msbuild@v2

      - name: Read PCL pin
        id: pcl
        shell: bash
        run: |
          . integration/pixinsight/ci/pcl-pin.env
          echo "sha=$PCL_SHA" >> "$GITHUB_OUTPUT"

      - name: Cache PCL-pxi.lib (keyed on PCL commit)
        id: pcl-cache
        uses: actions/cache/restore@v4
        with:
          path: pcl-ci
          key: pcl-${{ runner.os }}-${{ steps.pcl.outputs.sha }}

      - name: Build PCL (on cache miss)
        if: steps.pcl-cache.outputs.cache-hit != 'true'
        shell: pwsh
        run: integration/pixinsight/ci/build-pcl-windows.ps1 -Out "$PWD/pcl-ci"

      - name: Save PCL-pxi.lib cache (only after a successful build)
        if: steps.pcl-cache.outputs.cache-hit != 'true'
        uses: actions/cache/save@v4
        with:
          path: pcl-ci
          key: pcl-${{ runner.os }}-${{ steps.pcl.outputs.sha }}

      - name: Confirm PCL archive
        shell: pwsh
        run: if (-not (Test-Path "$PWD/pcl-ci/lib/PCL-pxi.lib")) { exit 1 }
```

- [ ] **Step 4: Commit**

```bash
git add integration/pixinsight/ci/win/PCL.vcxproj \
        integration/pixinsight/ci/build-pcl-windows.ps1 \
        .github/workflows/module.yml
git commit -m "feat(ci): pin PCL.vcxproj + build-pcl-windows.ps1; windows PCL CI"
```

- [ ] **Step 5: Push and confirm CI (orchestrator)**

Expected: the **build+test (windows-x64)** job reaches "Confirm PCL archive" green — `PCL-pxi.lib` builds under MSVC and the source-list drift check passes. (Because the job is `continue-on-error`, watch the job's own conclusion, not just the overall check.) Feed failures to a fixer subagent.

---

## Task 4: Rust Windows shm backend

Add the `windows-sys` shared-memory implementation so `mmm-core`/`mmm-ipc-worker` compile and the shm unit tests pass on Windows.

**Files:**
- Modify: `crates/mmm-core/Cargo.toml`
- Modify: `crates/mmm-core/src/ipc/shm.rs`
- Modify: `.github/workflows/module.yml` (add cargo test to the windows job)

**Interfaces:**
- Produces: `#[cfg(windows)] ShmSegment` with the **same** public API as the Unix impl — `create(name: &str, total_bytes: u64) -> Result<ShmSegment>`, `attach(name, total_bytes) -> Result<ShmSegment>`, `slice(&self, offset: u64, len: u64) -> &[f32]`, `slice_mut(&self, offset: u64, len: u64) -> &mut [f32]`, `Drop`. Consumed unchanged by `client.rs` (`HostLink { shm: ShmSegment, ... }`, shared across threads via `Arc`).

- [ ] **Step 1: (Setup) add the mingw cross target for local `cargo check`**

Run:
```bash
rustup target add x86_64-pc-windows-gnu
sudo apt-get update && sudo apt-get install -y gcc-mingw-w64-x86-64
```
This enables `cargo check --target x86_64-pc-windows-gnu` as the local compile proxy (`windows-sys` builds for gnu). Not committed — environment setup only.

- [ ] **Step 2: Add the `windows-sys` dependency**

In `crates/mmm-core/Cargo.toml`, after the existing `[target.'cfg(unix)'.dependencies]` block, add:

```toml
[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.59", features = [
    "Win32_Foundation",
    "Win32_System_Memory",
] }
```

- [ ] **Step 3: Write the failing Windows shm tests**

In `crates/mmm-core/src/ipc/shm.rs`, add a Windows test module mirroring the Unix `#[cfg(all(test, unix))]` tests (names use a `mmm-shm-*` base without a leading slash; normalization adds `Local\`):

```rust
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
}
```

- [ ] **Step 4: Narrow the non-platform stub and implement the Windows backend**

In `shm.rs`, change the stub cfg from `#[cfg(not(unix))]` to `#[cfg(all(not(unix), not(windows)))]` on **both** the stub `struct ShmSegment` and its `impl` block. Then add the Windows implementation. Insert after the Unix `impl Drop` block:

```rust
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
            CreateFileMappingW, MapViewOfFile, FILE_MAP_ALL_ACCESS, PAGE_READWRITE,
        };
        let wname = win_object_name(name);
        let hi = (total_bytes >> 32) as u32;
        let lo = (total_bytes & 0xFFFF_FFFF) as u32;
        // SAFETY: FFI; INVALID_HANDLE_VALUE requests a pagefile-backed mapping.
        let handle = unsafe {
            CreateFileMappingW(INVALID_HANDLE_VALUE, std::ptr::null(), PAGE_READWRITE, hi, lo, wname.as_ptr())
        };
        if handle.is_null() {
            return Err(Error::compute(format!(
                "CreateFileMappingW({name}) failed: os error {}",
                unsafe { GetLastError() }
            )));
        }
        // SAFETY: FFI; map the whole segment ([0, total_bytes)).
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, total_bytes as usize) };
        if view.Value.is_null() {
            let e = unsafe { GetLastError() };
            unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
            return Err(Error::compute(format!("MapViewOfFile({name}) failed: os error {e}")));
        }
        Ok(ShmSegment { name: name.to_string(), is_creator: true, handle, base: view.Value as *mut u8, size: total_bytes as usize })
    }

    /// Attach to an existing named segment created by [`create`](Self::create).
    ///
    /// The mapping is opened by name and a view of `total_bytes` is mapped;
    /// requesting more than the creator allocated fails, so an oversized
    /// `total_bytes` is rejected. Unlike the POSIX impl, Windows offers no cheap
    /// exact-size assertion, so a *smaller* `total_bytes` within a larger
    /// segment is accepted — the creator's size is the contract.
    pub fn attach(name: &str, total_bytes: u64) -> Result<ShmSegment> {
        use windows_sys::Win32::Foundation::{GetLastError, CloseHandle};
        use windows_sys::Win32::System::Memory::{
            OpenFileMappingW, MapViewOfFile, FILE_MAP_ALL_ACCESS,
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
        // SAFETY: FFI.
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, total_bytes as usize) };
        if view.Value.is_null() {
            let e = unsafe { GetLastError() };
            unsafe { CloseHandle(handle) };
            return Err(Error::compute(format!(
                "MapViewOfFile({name}, {total_bytes} bytes) failed: os error {e}"
            )));
        }
        Ok(ShmSegment { name: name.to_string(), is_creator: false, handle, base: view.Value as *mut u8, size: total_bytes as usize })
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
        let range = self.checked_range(offset, len).expect("ShmSegment::slice out of bounds");
        // SAFETY: `range` is bounds/alignment-checked against the mapping; the
        // base pointer is page-aligned so a 4-byte-aligned offset stays aligned.
        unsafe { std::slice::from_raw_parts(self.base.add(range.start) as *const f32, len as usize) }
    }

    /// Write `len` f32 elements starting at byte `offset` from a shared
    /// reference — the interior-mutable shared-memory pattern; see the Unix
    /// impl's `# Safety`. Panics if out of bounds or misaligned.
    #[allow(clippy::mut_from_ref)]
    pub fn slice_mut(&self, offset: u64, len: u64) -> &mut [f32] {
        let range = self.checked_range(offset, len).expect("ShmSegment::slice_mut out of bounds");
        // SAFETY: as `slice`, plus the caller's disjoint-range obligation
        // (upheld by `SlotLayout`), identical to the Unix `slice_mut_raw`.
        unsafe { std::slice::from_raw_parts_mut(self.base.add(range.start) as *mut f32, len as usize) }
    }
}

#[cfg(windows)]
impl Drop for ShmSegment {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Memory::{MapViewOfFile, UnmapViewOfFile, MEMORY_MAPPED_VIEW_ADDRESS};
        let _ = MapViewOfFile; // (import kept local to Drop is unnecessary; see note)
        // SAFETY: `base`/`handle` came from a successful map in create/attach.
        // A Windows file mapping is refcounted by open handles and vanishes when
        // the last handle closes — there is no `shm_unlink` analog, so both the
        // creator and attachers simply unmap + close. `is_creator` is retained
        // for symmetry/debugging but drives no special teardown.
        let _ = self.is_creator;
        unsafe {
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: self.base as *mut core::ffi::c_void });
            CloseHandle(self.handle);
        }
    }
}
```

> Implementer note: remove the two `let _ = ...;` lines and unused imports if `clippy` flags them — they are written defensively for readers, but `clippy` cleanliness (Global Constraints) wins. The `MEMORY_MAPPED_VIEW_ADDRESS` wrapper is how `windows-sys` 0.59 types `MapViewOfFile`/`UnmapViewOfFile`; if the pinned `windows-sys` version returns a bare pointer, adapt to that signature (verify against the version resolved in `Cargo.lock`).

- [ ] **Step 5: Local compile proxy**

Run:
```bash
cargo check --target x86_64-pc-windows-gnu -p mmm-core
cargo check --target x86_64-pc-windows-gnu -p mmm-ipc-worker
```
Expected: PASS (the `cfg(windows)` module typechecks). Fix signature mismatches against the resolved `windows-sys` version until clean.

- [ ] **Step 6: Local non-regression + lint**

Run:
```bash
cargo test -p mmm-core -p mmm-ipc-worker
cargo fmt --check
cargo clippy --all-targets
cargo doc --no-deps -p mmm-core
```
Expected: all PASS/clean on Linux (the `cfg(windows)` code is inert here; the stub cfg change doesn't affect unix).

- [ ] **Step 7: Add `cargo test` to the windows CI job**

After "Confirm PCL archive" in the windows job, add (this runs the `win_tests` shm suite on real Windows):

```yaml
      - name: Rust tests (ipc crates)
        run: cargo test -p mmm-core -p mmm-ipc-worker
```

- [ ] **Step 8: Commit**

```bash
git add crates/mmm-core/Cargo.toml crates/mmm-core/src/ipc/shm.rs Cargo.lock .github/workflows/module.yml
git commit -m "feat(ipc): Windows named-mapping shm backend (windows-sys)"
```

- [ ] **Step 9: Push and confirm CI (orchestrator)**

Expected: the windows job's **Rust tests** step green (the `win_tests` create/attach round-trip and alignment tests pass on real Windows). linux + macos jobs stay green. Feed failures to a fixer subagent.

---

## Task 5: C++ host Windows port

Port the PCL-free C++ host to Win32 behind `#ifdef _WIN32`, so the host static lib compiles under MSVC and the golden/isolation CTest suite passes on Windows. All POSIX code stays under `#else`.

**Files:**
- Create: `integration/pixinsight/host/mmm_os.h`
- Modify: `mmm_shm.cpp`, `mmm_host.cpp`, `mmm_host.h`, `mmm_protocol.cpp`, `mmm_protocol.h`, `CMakeLists.txt`, `test/test_protocol.cpp`, `test/test_isolation.cpp`, `test/test_golden_aligned.cpp`, `test/test_golden_solved.cpp`, `test/test_shm.cpp`

**Interfaces:**
- Consumes: Task 4's Rust shm name normalization (`/name` → `Local\name`) — the C++ shm must apply the **same** rule so the worker attaches to the host-created mapping.
- Produces: an OS-neutral `mmm::os_handle` type + `mmm::os_read/os_write/os_close/os_invalid_handle`; `mmm_protocol`/`mmm_host` use these instead of raw `int fd`. `ShmSegment` and `Host` public headers keep their current signatures (only member types change).

Reference inventory (call sites to port, from recon) — all in `integration/pixinsight/host/`:
- `mmm_shm.cpp`: `shm_unlink` L29/41/52/67; `shm_open` L33; `ftruncate` L38; `close` L40/57; `mmap` L47; `munmap` L65/85.
- `mmm_host.cpp`: `signal(SIGPIPE)` L33; `write` L42; `pipe` L54; `close` L60/66; `posix_spawn_file_actions_*`/`posix_spawn` L81-96; `kill` L229/337; `waitpid` L231/259/323/338; `WIF*` L270-274; `read` L312.
- `mmm_protocol.cpp`: `read` L42; `write` L57 (in `full_read`/`full_write`).

- [ ] **Step 1: Create `mmm_os.h` (the platform seam)**

```cpp
// mmm_os.h — minimal OS handle/IO seam so the transport code is written once.
// POSIX uses int fds; Windows uses HANDLEs. Everything else stays portable.
#pragma once
#include <cstddef>

#ifdef _WIN32
  #ifndef WIN32_LEAN_AND_MEAN
    #define WIN32_LEAN_AND_MEAN
  #endif
  #include <windows.h>
namespace mmm {
using os_handle = HANDLE;
inline const os_handle os_invalid_handle = INVALID_HANDLE_VALUE;

// Blocking read/write of up to n bytes; returns bytes transferred, 0 on EOF
// (read) / -1 on error. Broken-pipe on write surfaces as 0 written + a broken
// flag the caller maps to the POSIX EPIPE behavior.
inline long os_read(os_handle h, void* buf, size_t n) {
    DWORD got = 0;
    if (!ReadFile(h, buf, (DWORD)n, &got, nullptr)) {
        if (GetLastError() == ERROR_BROKEN_PIPE) return 0; // peer closed == EOF
        return -1;
    }
    return (long)got;
}
inline long os_write(os_handle h, const void* buf, size_t n) {
    DWORD put = 0;
    if (!WriteFile(h, buf, (DWORD)n, &put, nullptr)) {
        DWORD e = GetLastError();
        if (e == ERROR_BROKEN_PIPE || e == ERROR_NO_DATA) return -1; // == EPIPE
        return -1;
    }
    return (long)put;
}
inline void os_close(os_handle h) { if (h && h != os_invalid_handle) CloseHandle(h); }
} // namespace mmm
#else
  #include <unistd.h>
namespace mmm {
using os_handle = int;
inline constexpr os_handle os_invalid_handle = -1;
inline long os_read(os_handle h, void* buf, size_t n) { return (long)::read(h, buf, n); }
inline long os_write(os_handle h, const void* buf, size_t n) { return (long)::write(h, buf, n); }
inline void os_close(os_handle h) { if (h >= 0) ::close(h); }
} // namespace mmm
#endif
```

- [ ] **Step 2: Port `mmm_protocol.*` to `os_handle`**

In `mmm_protocol.h`, change the `int fd` parameters of `read_worker_frame`, `write_frame_raw` (and any `full_read`/`full_write` declarations) to `mmm::os_handle` and `#include "mmm_os.h"`. In `mmm_protocol.cpp`, replace the `::read`/`::write` EINTR loops with `mmm::os_read`/`mmm::os_write`. Keep the EINTR retry only under `#ifndef _WIN32` (Windows `ReadFile`/`WriteFile` don't return `EINTR`); on Windows a single `os_read`/`os_write` call is the loop body. Remove the now-unnecessary `<unistd.h>` include (it comes via `mmm_os.h` on POSIX). Wire format bytes are untouched.

- [ ] **Step 3: Port `mmm_shm.*` (Win32 branch)**

`mmm_shm.h`: interface unchanged (`create`, `base()`, `size()`, `slot_floats()`, `name()`), but the member that holds the fd/handle becomes platform-conditional (store `HANDLE hMap_` on Windows; keep `void* base_`, `size_t size_`, `std::string name_`, `bool is_creator_`). In `mmm_shm.cpp`, wrap the POSIX body in `#ifndef _WIN32 ... #else ... #endif`. The Windows branch of `ShmSegment::create`:

```cpp
#ifdef _WIN32
// Normalize the POSIX-style name (/mmm-foo) to a Windows object name
// (Local\mmm-foo) — identical rule to crates/mmm-core/src/ipc/shm.rs.
static std::wstring win_object_name(const std::string& name) {
    std::string base = (!name.empty() && name[0] == '/') ? name.substr(1) : name;
    std::wstring w = L"Local\\";
    w.reserve(w.size() + base.size());
    for (char c : base) w.push_back((wchar_t)(unsigned char)c);
    return w;
}

ShmSegment ShmSegment::create(const std::string& name, size_t total_bytes) {
    std::wstring wname = win_object_name(name);
    ULARGE_INTEGER sz; sz.QuadPart = total_bytes;
    HANDLE h = CreateFileMappingW(INVALID_HANDLE_VALUE, nullptr, PAGE_READWRITE,
                                  sz.HighPart, sz.LowPart, wname.c_str());
    if (h == nullptr)
        throw std::runtime_error("CreateFileMappingW(" + name + ") failed: " + last_error());
    void* base = MapViewOfFile(h, FILE_MAP_ALL_ACCESS, 0, 0, total_bytes);
    if (base == nullptr) { std::string e = last_error(); CloseHandle(h);
        throw std::runtime_error("MapViewOfFile(" + name + ") failed: " + e); }
    ShmSegment s; s.name_ = name; s.is_creator_ = true; s.hMap_ = h; s.base_ = base; s.size_ = total_bytes;
    return s;
}
#endif
```

Add a matching Windows `attach`/open path if the host uses one (the host only `create`s; the worker attaches on the Rust side, so a C++ `attach` may be unused — port it only if it exists). The Windows destructor: `if (base_) UnmapViewOfFile(base_); if (hMap_) CloseHandle(hMap_);` — **no `shm_unlink`** (handle-refcounted lifetime; document this). Add a small `last_error()` helper (`FormatMessageA` on `GetLastError()`), or inline a numeric `GetLastError()` string. Keep `slot_floats()` alignment assert unchanged.

- [ ] **Step 4: Port `mmm_host.*` (spawn/pipe/wait branch)**

`mmm_host.h`: change `int stdin_fd_` and the `Pipe` fd members to `mmm::os_handle` (init to `mmm::os_invalid_handle`), store the child process as `HANDLE` on Windows / `pid_t` on POSIX (a `#ifdef`-conditional member, e.g. `#ifdef _WIN32 HANDLE child_ = nullptr; #else pid_t pid_ = -1; #endif`). `#include "mmm_os.h"`.

`mmm_host.cpp`: wrap each POSIX syscall site in `#ifdef _WIN32`/`#else`:
- **SIGPIPE**: `ignore_sigpipe_once()` becomes a no-op on Windows (`#ifdef _WIN32 /* no SIGPIPE */ #else ::signal(...) #endif`). Broken-pipe is handled by `os_write` returning -1.
- **Pipe creation** (`Pipe`): `CreatePipe(&rd, &wr, &sa, 0)` with `SECURITY_ATTRIBUTES{ bInheritHandle = TRUE }`; then `SetHandleInformation(<host-side end>, HANDLE_FLAG_INHERIT, 0)` so only the child-inherited ends are inheritable.
- **Spawn** (`spawn_worker`): replace `posix_spawn`+file-actions with:

```cpp
#ifdef _WIN32
    STARTUPINFOW si; ZeroMemory(&si, sizeof si); si.cb = sizeof si;
    si.dwFlags = STARTF_USESTDHANDLES;
    si.hStdInput  = child_stdin_rd;   // child reads its stdin
    si.hStdOutput = child_stdout_wr;  // child writes its stdout
    si.hStdError  = GetStdHandle(STD_ERROR_HANDLE); // inherit stderr
    PROCESS_INFORMATION pi; ZeroMemory(&pi, sizeof pi);
    // Build a mutable UTF-16 command line: "\"<worker>\" [--probe-frame]".
    std::wstring cmd = build_command_line_w(worker_path, probe);
    BOOL ok = CreateProcessW(nullptr, cmd.data(), nullptr, nullptr,
                             TRUE /*inherit handles*/, 0, nullptr, nullptr, &si, &pi);
    if (!ok) throw std::runtime_error("CreateProcessW failed: " + last_error());
    CloseHandle(pi.hThread);
    child_ = pi.hProcess;
    // Close the child-side ends in the parent (they were inherited by the child).
    mmm::os_close(child_stdin_rd);
    mmm::os_close(child_stdout_wr);
#else
    /* existing posix_spawn body */
#endif
```

- **Reap/classify**: `#ifdef _WIN32` use `WaitForSingleObject(child_, INFINITE)` + `GetExitCodeProcess(child_, &code)`; map `code != 0` to the same error path the POSIX `WEXITSTATUS != 0` branch produces; there is no signal-vs-exit distinction (Windows has no `WIFSIGNALED`), so a non-zero/`TerminateProcess`d child is reported as an abnormal exit with its code. `#else` keep `waitpid`/`WIF*`.
- **Force-kill**: `#ifdef _WIN32 TerminateProcess(child_, 1); WaitForSingleObject(child_, INFINITE); #else ::kill(pid,SIGKILL); waitpid(...) #endif`.
- **fd I/O**: replace `full_write_fd`'s `::write` and the probe drain `::read` with `mmm::os_write`/`mmm::os_read`; drop the POSIX `EINTR` loop on Windows (guard with `#ifndef _WIN32`).

Add the `build_command_line_w(const std::string& worker_path, bool probe)` helper (quote the path, append `--probe-frame` when `probe`).

- [ ] **Step 5: Port CMake for Windows/macOS + MSVC flags**

In `CMakeLists.txt`:
- Compiler flags: `if(MSVC) target_compile_options(mmm_host PRIVATE /W4 /EHsc) else() ... -Wall -Wextra -fPIC endif()`. (The `/EHsc` model for the host is fine — it does not link PCL; the module DLL matches PCL's `/EHa` separately in Task 6.)
- Link: from Task 2, `rt` is already guarded to Linux; on Windows link nothing extra (`kernel32` is implicit). Guard `Threads::Threads` with `if(NOT WIN32)` if `find_package(Threads)` isn't desired on MSVC (std::thread works without it on MSVC).
- `MMM_WORKER` default: `if(WIN32)` use `../../../target/debug/mmm-ipc-worker.exe` else the current default.

- [ ] **Step 6: Port test fixtures behind `#ifdef _WIN32`**

- `test_protocol.cpp`: replace the raw `pipe`/`write`/`close` round-trip with `CreatePipe`/`mmm::os_write`/`mmm::os_close` on Windows (or use `mmm_os.h` helpers on both). `#include "mmm_os.h"`.
- `test_isolation.cpp`: crash-worker path `"/bin/false"` → `#ifdef _WIN32 "cmd" with args "/c","exit","1"` (or `where cmd`), i.e. a program that exits non-zero immediately; `::getpid()` → `#ifdef _WIN32 GetCurrentProcessId() #else ::getpid() #endif`; `::_exit(1)` watchdog → `#ifdef _WIN32 _exit(1)` (available via `<process.h>`) or `TerminateProcess(GetCurrentProcess(),1)`.
- `test_golden_aligned.cpp` / `test_golden_solved.cpp` / `test_shm.cpp`: `getpid` shim as above; shm name literals keep the `/mmm-...` form (normalization handles the rest) — no change needed beyond the `getpid` shim.

- [ ] **Step 7: Local non-regression (POSIX build must be untouched)**

Run:
```bash
rm -rf integration/pixinsight/host/ci-build
cmake -S integration/pixinsight/host -B integration/pixinsight/host/ci-build
cmake --build integration/pixinsight/host/ci-build
ctest --test-dir integration/pixinsight/host/ci-build --output-on-failure
```
Expected: PASS — every Windows branch is under `#ifdef _WIN32`, so the Linux build/tests are unchanged. This is the primary local proof of correctness for this task.

- [ ] **Step 8: Add host CTest to the windows CI job**

After "Rust tests" in the windows job, add the CTest steps (bash is available on windows runners; use `shell: bash` so the CMake invocation matches linux/macos). CMake will pick the MSVC generator by default:

```yaml
      - name: Host golden CTest suite (no PixInsight)
        shell: bash
        run: |
          cmake -S integration/pixinsight/host -B integration/pixinsight/host/ci-build
          cmake --build integration/pixinsight/host/ci-build --config Release
          ctest --test-dir integration/pixinsight/host/ci-build --build-config Release --output-on-failure
```

- [ ] **Step 9: Commit**

```bash
git add integration/pixinsight/host/ .github/workflows/module.yml
git commit -m "feat(host): Windows Win32 port (shm, spawn/pipe/wait, HANDLE seam)"
```

- [ ] **Step 10: Push and confirm CI (orchestrator)**

Expected: the windows job's **Host golden CTest suite** step green — the host builds under MSVC and the golden/isolation tests pass against the real worker over the Win32 shm + pipes. linux/macos stay green. Feed failures (with the CTest/`msbuild` log excerpt) to a fresh fixer subagent — this is the highest-risk task; expect an iteration or two.

---

## Task 6: Windows module build; make the windows job required

Build `mmm-pxm.dll` and flip the windows job to required.

**Files:**
- Create: `integration/pixinsight/ci/build-module-windows.ps1`
- Create: `integration/pixinsight/module/CMakeLists.txt` (Windows module build) — or a pinned `mmm-pxm.vcxproj`; CMake is preferred for maintainability.
- Modify: `.github/workflows/module.yml` (add module build + remove `continue-on-error`)

**Interfaces:**
- Consumes: `PCL-pxi.lib` (Task 3), the ported host sources (Task 5), the worker (`cargo`, Task 4).
- Produces: `build-module-windows.ps1 -Pcl <prefix> -Stage <dir>` writes `<dir>/bin/{mmm-pxm.dll,mmm-ipc-worker.exe}`.

- [ ] **Step 1: Add a CMake module build for Windows**

Create `integration/pixinsight/module/CMakeLists.txt` that builds the module DLL from the module sources (`mmm.cpp`, `Mmm*.cpp`, `AstrometryProps.cpp`, `ImageWindowCollector.cpp`, `ViewPanelSource.cpp`) plus the `host/` objects, links `PCL-pxi.lib`, and matches PCL's ABI settings. Critical ABI-match flags (from the pinned `PCL.vcxproj`): `/std:c++20 /permissive- /Zc:__cplusplus /EHa /MD` and `PlatformToolset v143`, plus the AVX2 arch. Skeleton:

```cmake
cmake_minimum_required(VERSION 3.20)
project(mmm_pxm CXX)
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# PCL prefix passed in: -DPCL_PREFIX=<dir with lib/PCL-pxi.lib and include/>
add_library(mmm-pxm SHARED
  mmm.cpp MmmProcess.cpp MmmInterface.cpp MmmParameters.cpp MmmExecution.cpp
  AstrometryProps.cpp ImageWindowCollector.cpp ViewPanelSource.cpp
  ${CMAKE_CURRENT_SOURCE_DIR}/../host/mmm_shm.cpp
  ${CMAKE_CURRENT_SOURCE_DIR}/../host/mmm_protocol.cpp
  ${CMAKE_CURRENT_SOURCE_DIR}/../host/mmm_host.cpp)
set_target_properties(mmm-pxm PROPERTIES PREFIX "" OUTPUT_NAME "mmm-pxm" SUFFIX ".dll")
target_include_directories(mmm-pxm PRIVATE
  ${PCL_PREFIX}/include ${CMAKE_CURRENT_SOURCE_DIR}/../host
  ${CMAKE_CURRENT_SOURCE_DIR}/../host/third_party)
target_compile_options(mmm-pxm PRIVATE /std:c++20 /permissive- /Zc:__cplusplus /EHa /MD /W4
  /arch:AVX2 /DWIN32 /DWIN64 /D_WINDOWS /DUNICODE /D__PCL_WINDOWS /D__PCL_AVX2 /D__PCL_FMA)
target_link_directories(mmm-pxm PRIVATE ${PCL_PREFIX}/lib)
target_link_libraries(mmm-pxm PRIVATE PCL-pxi)
```

> Implementer note: read `integration/pixinsight/module/makefile-x64` first for the exact source list, extra PCL preprocessor defines, and any Qt/other link deps it references; carry all of them into this CMake so the DLL has the same inputs as the Linux `.so`. The module is a `-pxm` DLL loaded by PixInsight — do **not** add a `main`. If `makefile-x64` links additional PCL/Qt import libs, add them to `target_link_libraries`.

- [ ] **Step 2: Write `build-module-windows.ps1`**

```powershell
# Build mmm-pxm.dll + worker on Windows, stage the unsigned payload.
#   build-module-windows.ps1 -Pcl <prefix> -Stage <dir>
[CmdletBinding()]
param(
  [Parameter(Mandatory=$true)][string]$Pcl,
  [Parameter(Mandatory=$true)][string]$Stage
)
$ErrorActionPreference = 'Stop'
$repo = Resolve-Path (Join-Path $PSScriptRoot '../../..')
$moduleDir = Join-Path $repo 'integration/pixinsight/module'
$build = Join-Path $moduleDir 'win-build'

cmake -S $moduleDir -B $build -DPCL_PREFIX="$Pcl" | Tee-Object -Variable cfgLog
$buildLog = cmake --build $build --config Release 2>&1 | Tee-Object -Variable _bl
if ($LASTEXITCODE -ne 0) { throw "module build failed" }
# Warning-free gate.
if ($buildLog | Select-String -Pattern 'warning C') {
  $buildLog | Select-String -Pattern 'warning C'
  throw "ERROR: MSVC warnings in module build"
}

$dll = Get-ChildItem $build -Recurse -Filter 'mmm-pxm.dll' | Select-Object -First 1
if (-not $dll) { throw 'mmm-pxm.dll not built' }

Push-Location $repo
cargo build --release --target x86_64-pc-windows-msvc -p mmm-ipc-worker
Pop-Location
$worker = Join-Path $repo 'target/x86_64-pc-windows-msvc/release/mmm-ipc-worker.exe'
if (-not (Test-Path $worker)) { throw 'mmm-ipc-worker.exe not built' }

New-Item -ItemType Directory -Force -Path (Join-Path $Stage 'bin') | Out-Null
Copy-Item $dll.FullName (Join-Path $Stage 'bin/mmm-pxm.dll') -Force
Copy-Item $worker (Join-Path $Stage 'bin/mmm-ipc-worker.exe') -Force
Write-Host "staged unsigned payload in $Stage/bin"
```

- [ ] **Step 3: Finalize the windows CI job**

Add the module build + artifact upload after the CTest step, and **remove `continue-on-error: true`** from the windows job:

```yaml
      - name: Build module + worker, stage payload
        shell: pwsh
        run: integration/pixinsight/ci/build-module-windows.ps1 -Pcl "$PWD/pcl-ci" -Stage "$PWD/stage"

      - name: Upload unsigned module artifacts
        uses: actions/upload-artifact@v4
        with:
          name: mmm-pxm-windows-x64
          path: stage/bin/**
          if-no-files-found: error
```

- [ ] **Step 4: Commit**

```bash
git add integration/pixinsight/ci/build-module-windows.ps1 \
        integration/pixinsight/module/CMakeLists.txt \
        .github/workflows/module.yml
git commit -m "feat(ci): Windows module (mmm-pxm.dll) build; windows job required"
```

- [ ] **Step 5: Push and confirm CI (orchestrator)**

Expected: all three jobs — **linux-x64**, **macos-arm64**, **windows-x64** — green and required (no `continue-on-error` remaining). This is the plan's completion gate. The likely failure here is a PCL/host ABI mismatch (`/MD` vs `/MT`, `/EHa` vs `/EHsc`) surfacing as LNK errors — reconcile the module DLL's runtime/EH model with the pinned `PCL.vcxproj` per §11 of the spec. Feed failures to a fixer subagent.

---

## Task 7: Documentation

Bring the READMEs and CI/distribution cross-refs in line with the three-platform reality.

**Files:**
- Create: `integration/pixinsight/ci/README.md` (per-platform build scripts overview)
- Modify: `integration/pixinsight/host/README.md`, `integration/pixinsight/module/README.md`
- Modify: `docs/superpowers/specs/2026-07-28-pixinsight-ci-and-distribution-design.md` (note the ports are now implemented; §2 non-goal is discharged)

- [ ] **Step 1: Write `integration/pixinsight/ci/README.md`**

Document the per-platform build scripts (`build-pcl-{linux,macos,windows}`, `build-module-{linux,macos,windows}`), the pinned `win/PCL.vcxproj` provenance and the source-list drift guard, the shm-name normalization contract, and the "CI-green is unsigned; notarization/Authenticode are deferred follow-ups" scope note (spec §7).

- [ ] **Step 2: Update host + module READMEs**

In `host/README.md`: note the port is no longer POSIX-only — the transport is `#ifdef _WIN32`-branched via `mmm_os.h`; the Windows shm has no `shm_unlink` (handle-refcounted). In `module/README.md`: document the macOS `.dylib` and Windows `.dll` builds and the ABI-match flags (`/MD /EHa /std:c++20 /arch:AVX2`) required against PCL.

- [ ] **Step 3: Note the manual Windows GUI smoke test**

In `module/README.md`, add a short "Windows GUI validation (manual)" section: build artifacts from CI (or `build-module-windows.ps1`), sign the module via the existing Linux `PixInsight --sign-module-file` flow (cross-platform per Plan 3b), install in PixInsight on Windows, and confirm MosaicMerge loads + runs — mirroring the Linux/WSL validation from Plans 2b/3. This is a maintainer step, not a CI gate.

- [ ] **Step 4: Update the CI/distribution spec cross-ref**

In `docs/superpowers/specs/2026-07-28-pixinsight-ci-and-distribution-design.md`, add a note that §2's deferred native ports are implemented by `2026-07-29-pixinsight-winmac-ports-design.md` (all three OSes build+test in CI; worker signing/notarization remains the outstanding follow-up).

- [ ] **Step 5: Commit**

```bash
git add integration/pixinsight/ci/README.md \
        integration/pixinsight/host/README.md \
        integration/pixinsight/module/README.md \
        docs/superpowers/specs/2026-07-28-pixinsight-ci-and-distribution-design.md
git commit -m "docs(pixinsight): document Windows/macOS ports + per-platform builds"
```

---

## Final verification (orchestrator, before finishing the branch)

- [ ] All three CI jobs green and required on the branch head (no `continue-on-error`).
- [ ] Linux job unchanged in behavior (still builds module + repo artifacts + runs all suites).
- [ ] `cargo fmt --check`, `cargo clippy --all-targets`, `cargo doc` clean locally.
- [ ] Spec §1 out-of-scope items (notarization, GUI smoke test) confirmed *not* added to CI.
- [ ] Then use `superpowers:finishing-a-development-branch` to integrate.

## Self-review notes (traceability to spec)

- Spec §2 (order) → Tasks 1–2 (macOS) precede 3–6 (Windows). ✓
- Spec §4 (macOS job) → Tasks 1, 2 (PCL script, module script, `rt` guard, CI). ✓
- Spec §5.1 (Windows PCL, vcxproj lift, drift guard) → Task 3. ✓
- Spec §5.2 (Rust shm, `windows-sys`, Send/Sync, no shm_unlink) → Task 4. ✓
- Spec §5.3 (name normalization, both sides) → Task 4 Step 4 (`win_object_name` in Rust) + Task 5 Step 3 (`win_object_name` in C++). ✓
- Spec §5.4 (C++ host: shm/spawn/pipe/wait/protocol/CMake/tests) → Task 5. ✓
- Spec §5.5 (Windows module build) → Task 6. ✓
- Spec §6 (packaging unchanged) → no task needed; linux job retains `gen-package.sh`. ✓
- Spec §7 (no signing in CI) → Global Constraints + Task 7 docs. ✓
- Spec §8 (TDD/CI-as-gate) → Cross-platform verification model + per-task CI gates. ✓
- Spec §9 (CI wiring) → Tasks 1,2,3,4,5,6 incrementally edit `module.yml`. ✓
- Spec §11 risks (source drift, CUDADevice, ABI `/MD`/`/EHa`, worker path) → Task 3 drift guard, Task 1 CUDA note, Task 6 ABI note, Task 6/§ worker `.exe` path. ✓
