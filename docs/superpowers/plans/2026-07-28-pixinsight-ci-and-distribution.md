# PixInsight Module — CI Build & Distribution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `mmm-pxm.so` + `mmm-ipc-worker` reproducibly in GitHub Actions (no PixInsight on the runner) and provide a validated-but-dormant PixInsight update-repository pipeline that goes live once a CPD certificate lands.

**Architecture:** The build logic lives in **standalone scripts** (`integration/pixinsight/ci/` and `integration/pixinsight/repo/`) that run locally *and* are called by a thin new workflow `.github/workflows/module.yml`. This keeps every step testable outside CI. CI builds the open-source PCL from `gitlab.com/pixinsight/PCL` pinned to a commit SHA (cached), links the module, runs the host CTest golden suite as the correctness gate, and uploads an **unsigned** package artifact. The repository generator assembles `tar.gz` packages + `updates.xri` and is validated in CI, but the signing + publish stage is **dormant** (gated on a CPD `.xssk`) because module signing requires the PixInsight binary and a Pleiades-trusted identity.

**Tech Stack:** GitHub Actions; POSIX `sh`/`bash`; `git`, `g++` (C++20), GNU `make`, `cmake`/`ctest`, `cargo`; `tar`/`sha1sum`; `python3` (stdlib `xml.etree` + `pyyaml`) for local test validation; `xmllint`/`actionlint` added in CI only.

**Spec:** `docs/superpowers/specs/2026-07-28-pixinsight-ci-and-distribution-design.md`

## Global Constraints

Every task's requirements implicitly include these (values copied verbatim from the spec):

- **Branch:** work on the existing `pixinsight-integration` branch. Do **not** create a new branch.
- **PCL pin:** `gitlab.com/pixinsight/PCL` commit **`afea714e681853dfc21e70b5d53811ae41849e97`** = **PCL 2.10.4 / PixInsight 1.9.4 "Lockhart"**. The pinned SHA is the single source of truth; a build must assert `git rev-parse HEAD` equals it.
- **Module build:** warning-free with `-Wall`; **0 undefined `mmm::`/`_ZN3mmm` symbols** in `mmm-pxm.so` (undefined PCL-core symbols are expected and fine).
- **Platforms:** the CI matrix is structured for `ubuntu-latest`, `windows-latest`, `macos-latest`. **Only ubuntu is live/required.** Windows/macOS jobs are `continue-on-error: true` placeholders (native ports are a later plan).
- **Distribution:** **no publishing until CPD.** The signing/publish stage is dormant. **No GitHub Releases** for the module. Interim on-ramp = CI artifact + local `make sign`.
- **Package format:** `tar.gz`; internal tree overlays the install root; binaries in `bin/`: `bin/mmm-pxm.<so|dll|dylib>`, `bin/mmm-pxm.xsgn` (post-sign only), `bin/mmm-ipc-worker[.exe]`. Module binaries **must keep the `-pxm` basename** (a hard signer requirement).
- **`updates.xri`:** XML, UTF-8, namespace `http://www.pixinsight.com/xri`; `<package type="module" …>`; `sha1` = lowercase hex SHA-1 of the archive; `releaseDate` = `YYYYMMDD` (passed as a parameter — never generated nondeterministically inside a generator that a test must reproduce). OS attribute mapping: linux→`linux`, windows→`windows`, macos→`macosx`; `arch="x64"`.
- **Untouched:** existing `.github/workflows/ci.yml` (Rust tests) and `release.yml` (CLI releases). All new work is additive.
- **Testing:** scripts must be verifiable **locally** (no dependency on `xmllint`/`actionlint`/`bats` being installed — use `python3` for validation in tests). `cargo test --workspace` and the host CTest suite must stay green. Never depend on `test_data/`.
- **Commits:** frequent, one per task step where indicated; commit messages end with the `Co-Authored-By` trailer used on this branch.

## File Structure

**Created:**
- `.github/workflows/module.yml` — new workflow: PCL build+cache, module/worker build, host CTest, repo-generator validation, artifact upload. Ubuntu live; Win/mac placeholders.
- `integration/pixinsight/ci/pcl-pin.env` — the pinned PCL SHA + expected version (single source of truth, sourced by scripts and the workflow).
- `integration/pixinsight/ci/build-pcl.sh` — clone PCL at the pinned SHA, assert SHA + version, build `libPCL-pxi.a`, populate an output prefix (`include/` + `lib/libPCL-pxi.a`).
- `integration/pixinsight/ci/build-module-linux.sh` — build `mmm-pxm.so` (against a PCL prefix) + `mmm-ipc-worker`, assert no undefined `mmm::` symbols, stage the unsigned package payload under `bin/`.
- `integration/pixinsight/repo/gen-package.sh` — tar.gz one platform's staged overlay tree; write `<os>-<arch>.meta` (fileName, sha1, os, arch).
- `integration/pixinsight/repo/gen-updates-xri.sh` — emit a schema-valid `updates.xri` from one or more `.meta` files.
- `integration/pixinsight/repo/sign-and-publish.sh` — **dormant** signing+publish driver: reports dormant + exits 0 without a CPD `.xssk`; `--dry-run` prints the exact `--sign-module-file`/CodeSign/publish commands.
- `integration/pixinsight/repo/test/test-gen-package.sh` — test for the package assembler.
- `integration/pixinsight/repo/test/test-gen-updates-xri.sh` — test for the XRI generator (python3-validated).
- `integration/pixinsight/repo/test/test-sign-and-publish.sh` — test for the dormant driver.
- `integration/pixinsight/repo/README.md` — interim artifact on-ramp, post-CPD repository flow, and the `SubmitCPD` action item.

**Modified:**
- `docs/superpowers/specs/2026-07-27-pixinsight-integration-design.md` — §12 cross-reference to the new spec (Task 7).

---

### Task 1: PCL build script + pin

**Files:**
- Create: `integration/pixinsight/ci/pcl-pin.env`
- Create: `integration/pixinsight/ci/build-pcl.sh`

**Interfaces:**
- Produces: `build-pcl.sh --out <DIR>` populates `<DIR>/include/` (the PCL headers) and `<DIR>/lib/libPCL-pxi.a`. Sources `pcl-pin.env` for `PCL_SHA`, `PCL_VER_MAJOR`, `PCL_VER_MINOR`, `PCL_VER_RELEASE`. Exit non-zero on SHA/version mismatch or build failure.

- [ ] **Step 1: Write `pcl-pin.env`**

```sh
# Pinned open-source PCL revision for CI module builds.
# Single source of truth; keep in sync with the target PixInsight core.
# gitlab.com/pixinsight/PCL @ this SHA == PCL 2.10.4 / PixInsight 1.9.4 "Lockhart".
PCL_REPO_URL="https://gitlab.com/pixinsight/PCL.git"
PCL_SHA="afea714e681853dfc21e70b5d53811ae41849e97"
PCL_VER_MAJOR=2
PCL_VER_MINOR=10
PCL_VER_RELEASE=4
```

- [ ] **Step 2: Write the failing test invocation (script absent)**

Run: `bash integration/pixinsight/ci/build-pcl.sh --help`
Expected: FAIL — "No such file or directory" (script not yet created).

- [ ] **Step 3: Write `build-pcl.sh`**

```sh
#!/usr/bin/env bash
# Build libPCL-pxi.a from the pinned open-source PCL, out-of-tree, no PixInsight.
# Usage: build-pcl.sh --out <prefix-dir> [--work <clone-dir>]
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "$HERE/pcl-pin.env"

OUT="" ; WORK=""
while [ $# -gt 0 ]; do
  case "$1" in
    --out)  OUT="$2"; shift 2 ;;
    --work) WORK="$2"; shift 2 ;;
    --help) echo "usage: build-pcl.sh --out <dir> [--work <dir>]"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done
[ -n "$OUT" ] || { echo "--out is required" >&2; exit 2; }
WORK="${WORK:-$(mktemp -d)}"
mkdir -p "$OUT/lib" "$OUT/include" "$WORK"

# Fetch exactly the pinned commit (GitLab allows fetch-by-SHA for reachable commits).
cd "$WORK"
if [ ! -d .git ]; then
  git init -q
  git remote add origin "$PCL_REPO_URL"
fi
git fetch -q --depth 1 origin "$PCL_SHA"
git checkout -q FETCH_HEAD
HEAD_SHA="$(git rev-parse HEAD)"
[ "$HEAD_SHA" = "$PCL_SHA" ] || { echo "PCL SHA mismatch: got $HEAD_SHA want $PCL_SHA" >&2; exit 1; }

# Soft version check against Version.cpp (belt-and-suspenders; SHA is the hard gate).
VER_CPP="src/pcl/Version.cpp"
if [ -f "$VER_CPP" ]; then
  if ! grep -qE "return[[:space:]]+$PCL_VER_MINOR[[:space:]]*;" "$VER_CPP"; then
    echo "warning: Version.cpp minor != $PCL_VER_MINOR (source may have shifted)" >&2
  fi
fi

# Build only the static PCL library (the module links -lPCL-pxi and nothing else;
# 3rdparty are header-only for this archive). CUDADevice.cpp compiles without the
# CUDA toolkit in this PCL version. If a future pin breaks here on cuda.h, drop
# that TU from SRC_FILES or `apt-get install nvidia-cuda-toolkit` for headers.
export PCLDIR="$WORK"
export PCLSRCDIR="$WORK/src"
export PCLINCDIR="$WORK/include"
export PCLLIBDIR64="$WORK/lib/linux/x64"
export PCLBINDIR64="$WORK/bin"
mkdir -p "$PCLLIBDIR64" "$PCLBINDIR64"
( cd src/pcl/linux/g++ && make -f makefile-x64 -j"$(nproc)" )

# Locate and publish the archive + headers to the output prefix.
LIB="$(find "$WORK/src/pcl" -name libPCL-pxi.a -print -quit)"
[ -n "$LIB" ] || { echo "libPCL-pxi.a not produced" >&2; exit 1; }
cp -f "$LIB" "$OUT/lib/libPCL-pxi.a"
cp -a "$WORK/include/." "$OUT/include/"
echo "PCL built: $OUT/lib/libPCL-pxi.a ($(du -h "$OUT/lib/libPCL-pxi.a" | cut -f1)), headers in $OUT/include"
```

Then `chmod +x integration/pixinsight/ci/build-pcl.sh`.

- [ ] **Step 4: Run the script locally and verify output**

Run: `bash integration/pixinsight/ci/build-pcl.sh --out /tmp/pcl-ci`
Expected: PASS — prints "PCL built: …", and:
```sh
test -f /tmp/pcl-ci/lib/libPCL-pxi.a && \
test "$(stat -c%s /tmp/pcl-ci/lib/libPCL-pxi.a)" -gt 40000000 && \
test -f /tmp/pcl-ci/include/pcl/Version.h && echo OK
```
must print `OK` (archive > 40 MB; headers present). If the sandbox has no network, skip the run and note it; CI is the authoritative run — but attempt it, since it is the core de-risking of this plan.

- [ ] **Step 5: Commit**

```bash
git add integration/pixinsight/ci/pcl-pin.env integration/pixinsight/ci/build-pcl.sh
git commit -m "feat(ci): reproducible open-source PCL build script, pinned by SHA

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Module + worker build/package script

**Files:**
- Create: `integration/pixinsight/ci/build-module-linux.sh`

**Interfaces:**
- Consumes: a PCL prefix dir from Task 1 (`<prefix>/include`, `<prefix>/lib/libPCL-pxi.a`).
- Produces: `build-module-linux.sh --pcl <prefix> --stage <dir>` builds `mmm-pxm.so` + `mmm-ipc-worker`, asserts no undefined `mmm::` symbols, and populates `<dir>/bin/{mmm-pxm.so,mmm-ipc-worker}`. Exit non-zero on build failure or undefined `mmm::` symbols.

- [ ] **Step 1: Write the failing test invocation**

Run: `bash integration/pixinsight/ci/build-module-linux.sh --help`
Expected: FAIL — script does not exist.

- [ ] **Step 2: Write `build-module-linux.sh`**

```sh
#!/usr/bin/env bash
# Build the PCL module + worker on Linux and stage the unsigned package payload.
# Usage: build-module-linux.sh --pcl <prefix> --stage <dir>
set -euo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

PCL="" ; STAGE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --pcl)   PCL="$2"; shift 2 ;;
    --stage) STAGE="$2"; shift 2 ;;
    --help)  echo "usage: build-module-linux.sh --pcl <prefix> --stage <dir>"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done
[ -n "$PCL" ] && [ -n "$STAGE" ] || { echo "--pcl and --stage are required" >&2; exit 2; }

MODULE_DIR="$REPO_ROOT/integration/pixinsight/module"
make -C "$MODULE_DIR" clean
make -C "$MODULE_DIR" PCLINCDIR="$PCL/include" PCLLIBDIR="$PCL/lib"

SO="$MODULE_DIR/mmm-pxm.so"
[ -f "$SO" ] || { echo "mmm-pxm.so not built" >&2; exit 1; }
# The host objects must be linked in: any undefined mmm:: symbol means they weren't.
if nm -D -u "$SO" | grep -E '_ZN3mmm|mmm::' ; then
  echo "ERROR: undefined mmm:: symbols in mmm-pxm.so (host objects not linked)" >&2
  exit 1
fi

( cd "$REPO_ROOT" && cargo build --release -p mmm-ipc-worker )
WORKER="$REPO_ROOT/target/release/mmm-ipc-worker"
[ -f "$WORKER" ] || { echo "mmm-ipc-worker not built" >&2; exit 1; }

mkdir -p "$STAGE/bin"
cp -f "$SO" "$STAGE/bin/mmm-pxm.so"
cp -f "$WORKER" "$STAGE/bin/mmm-ipc-worker"
echo "staged unsigned payload in $STAGE/bin: $(ls "$STAGE/bin")"
```

Then `chmod +x`.

- [ ] **Step 3: Run against the existing local PCL build and verify**

Run (uses the machine's existing `~/.local/pcl-build` as the prefix shape — it has `lib/libPCL-pxi.a`; point include at the install headers):
```bash
mkdir -p /tmp/pcl-local/lib /tmp/pcl-local/include
cp ~/.local/pcl-build/lib/libPCL-pxi.a /tmp/pcl-local/lib/
cp -a /opt/PixInsight/include/. /tmp/pcl-local/include/
bash integration/pixinsight/ci/build-module-linux.sh --pcl /tmp/pcl-local --stage /tmp/stage
test -f /tmp/stage/bin/mmm-pxm.so && test -f /tmp/stage/bin/mmm-ipc-worker && echo OK
```
Expected: PASS — build warning-free, no undefined `mmm::` symbols, `OK` printed.

- [ ] **Step 4: Commit**

```bash
git add integration/pixinsight/ci/build-module-linux.sh
git commit -m "feat(ci): module + worker build/stage script (asserts host link)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: `module.yml` CI workflow

**Files:**
- Create: `.github/workflows/module.yml`

**Interfaces:**
- Consumes: `build-pcl.sh`, `build-module-linux.sh` (Tasks 1–2). Repo-generator validation step is added in Task 5.
- Produces: a workflow whose ubuntu job builds+tests the module and uploads an unsigned artifact `mmm-pxm-linux-x64`.

- [ ] **Step 1: Write `module.yml`**

```yaml
# Builds the PixInsight module (mmm-pxm) + mmm-ipc-worker against the pinned
# open-source PCL, with NO PixInsight on the runner. Linux is the live build;
# Windows/macOS are scaffolded placeholders until the native ports land.
name: Module

on:
  push:
    paths:
      - "integration/pixinsight/**"
      - "crates/mmm-core/src/ipc/**"
      - "crates/mmm-ipc-worker/**"
      - ".github/workflows/module.yml"
  pull_request:
    paths:
      - "integration/pixinsight/**"
      - "crates/mmm-core/src/ipc/**"
      - "crates/mmm-ipc-worker/**"
      - ".github/workflows/module.yml"
  workflow_dispatch:

concurrency:
  group: module-${{ github.ref }}
  cancel-in-progress: ${{ github.event_name == 'pull_request' }}

jobs:
  linux:
    name: build+test (linux-x64)
    runs-on: ubuntu-latest
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
        uses: actions/cache@v4
        with:
          path: pcl-ci
          key: pcl-${{ runner.os }}-${{ steps.pcl.outputs.sha }}

      - name: Build PCL (on cache miss)
        if: steps.pcl-cache.outputs.cache-hit != 'true'
        run: bash integration/pixinsight/ci/build-pcl.sh --out "$PWD/pcl-ci"

      - name: Build module + worker, stage payload
        run: |
          bash integration/pixinsight/ci/build-module-linux.sh \
            --pcl "$PWD/pcl-ci" --stage "$PWD/stage"

      - name: Rust tests (ipc crates; ci.yml owns full workspace on main)
        run: cargo test -p mmm-core -p mmm-ipc-worker

      - name: Host golden CTest suite (no PixInsight)
        run: |
          cmake -S integration/pixinsight/host -B integration/pixinsight/host/ci-build
          cmake --build integration/pixinsight/host/ci-build
          ctest --test-dir integration/pixinsight/host/ci-build --output-on-failure

      - name: Upload unsigned module artifact
        uses: actions/upload-artifact@v4
        with:
          name: mmm-pxm-linux-x64
          path: stage/bin/**
          if-no-files-found: error

  windows:
    name: build+test (windows-x64) — placeholder
    runs-on: windows-latest
    continue-on-error: true
    steps:
      - uses: actions/checkout@v4
      - name: Native port pending (Plan 3a)
        shell: bash
        run: |
          echo "Windows shm + host transport port not yet implemented (Plan 3a)."
          echo "This job is a scaffolded placeholder; it will build the module"
          echo "once crates/mmm-core/src/ipc/shm.rs and host/ support Windows."
          exit 1

  macos:
    name: build+test (macos-x64) — placeholder
    runs-on: macos-latest
    continue-on-error: true
    steps:
      - uses: actions/checkout@v4
      - name: Native port pending (Plan 3a)
        run: |
          echo "macOS validation + worker notarization not yet implemented (Plan 3a)."
          echo "This job is a scaffolded placeholder."
          exit 1
```

- [ ] **Step 2: Validate the workflow YAML locally with python3**

Run:
```bash
python3 -c "import yaml,sys; d=yaml.safe_load(open('.github/workflows/module.yml')); \
assert 'linux' in d['jobs'] and d['jobs']['windows'].get('continue-on-error') is True \
and d['jobs']['macos'].get('continue-on-error') is True; \
assert True in d[True] or 'push' in d['on'] if False else True; print('module.yml parses; win/mac are continue-on-error')"
```
Expected: PASS — prints the confirmation. (PyYAML parses `on:` as the boolean key `True`; the assertion only checks the jobs, which is what matters.)

- [ ] **Step 3: Sanity-check job/script wiring by inspection**

Confirm each `run:` that calls a script references a path created in Tasks 1–2, the cache `key` uses the PCL SHA, and the artifact path is `stage/bin/**`. (No execution — GitHub Actions cannot run locally here; the called scripts are already verified in Tasks 1–2.)

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/module.yml
git commit -m "feat(ci): module.yml — build+test module on Linux, win/mac scaffolded

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Repository package assembler

**Files:**
- Create: `integration/pixinsight/repo/gen-package.sh`
- Test: `integration/pixinsight/repo/test/test-gen-package.sh`

**Interfaces:**
- Produces: `gen-package.sh <os> <arch> <staging-dir> <out-dir>` writes `<out-dir>/<os>-<arch>-module.tar.gz` (the gzip of the staging tree, whose top level is the install-root overlay, e.g. `bin/…`) and `<out-dir>/<os>-<arch>.meta` containing `fileName=`, `sha1=`, `os=`, `arch=` lines. `sha1` is the lowercase-hex SHA-1 of the tarball.

- [ ] **Step 1: Write the failing test**

`integration/pixinsight/repo/test/test-gen-package.sh`:
```sh
#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GEN="$HERE/../gen-package.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# Fixture staging tree: the install-root overlay.
mkdir -p "$TMP/stage/bin"
echo "fake-module" > "$TMP/stage/bin/mmm-pxm.so"
echo "fake-worker" > "$TMP/stage/bin/mmm-ipc-worker"

bash "$GEN" linux x64 "$TMP/stage" "$TMP/out"

TARBALL="$TMP/out/linux-x64-module.tar.gz"
META="$TMP/out/linux-x64.meta"
[ -f "$TARBALL" ] || { echo "FAIL: tarball missing"; exit 1; }
[ -f "$META" ]    || { echo "FAIL: meta missing"; exit 1; }

# Layout: overlay paths present.
tar tzf "$TARBALL" | grep -qx "bin/mmm-pxm.so"    || { echo "FAIL: bin/mmm-pxm.so not in tar"; exit 1; }
tar tzf "$TARBALL" | grep -qx "bin/mmm-ipc-worker" || { echo "FAIL: bin/mmm-ipc-worker not in tar"; exit 1; }

# sha1 matches sha1sum, lowercase hex.
WANT="$(sha1sum "$TARBALL" | cut -d' ' -f1)"
GOT="$(grep '^sha1=' "$META" | cut -d= -f2)"
[ "$GOT" = "$WANT" ] || { echo "FAIL: sha1 $GOT != $WANT"; exit 1; }
echo "$GOT" | grep -qE '^[0-9a-f]{40}$' || { echo "FAIL: sha1 not lowercase hex"; exit 1; }
grep -qx "fileName=linux-x64-module.tar.gz" "$META" || { echo "FAIL: fileName wrong"; exit 1; }
grep -qx "os=linux" "$META" || { echo "FAIL: os wrong"; exit 1; }
grep -qx "arch=x64" "$META" || { echo "FAIL: arch wrong"; exit 1; }
echo "PASS: gen-package"
```
Then `chmod +x` the test.

- [ ] **Step 2: Run the test to verify it fails**

Run: `bash integration/pixinsight/repo/test/test-gen-package.sh`
Expected: FAIL — `gen-package.sh` does not exist.

- [ ] **Step 3: Write `gen-package.sh`**

```sh
#!/usr/bin/env bash
# Assemble one platform's update package (tar.gz overlay) + its .meta.
# Usage: gen-package.sh <os> <arch> <staging-dir> <out-dir>
set -euo pipefail
OS="${1:?os}"; ARCH="${2:?arch}"; STAGE="${3:?staging-dir}"; OUT="${4:?out-dir}"
mkdir -p "$OUT"
FILE="${OS}-${ARCH}-module.tar.gz"
TARBALL="$OUT/$FILE"

# Tar the staging tree's contents so archive paths are install-root-relative
# (e.g. bin/mmm-pxm.so), deterministically (sorted, no owner/time noise).
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime='UTC 2020-01-01' \
    -C "$STAGE" -czf "$TARBALL" .

SHA1="$(sha1sum "$TARBALL" | cut -d' ' -f1)"
{
  echo "fileName=$FILE"
  echo "sha1=$SHA1"
  echo "os=$OS"
  echo "arch=$ARCH"
} > "$OUT/${OS}-${ARCH}.meta"
echo "packaged $TARBALL sha1=$SHA1"
```
Then `chmod +x`.

- [ ] **Step 4: Run the test to verify it passes**

Run: `bash integration/pixinsight/repo/test/test-gen-package.sh`
Expected: PASS — prints `PASS: gen-package`.

- [ ] **Step 5: Commit**

```bash
git add integration/pixinsight/repo/gen-package.sh integration/pixinsight/repo/test/test-gen-package.sh
git commit -m "feat(repo): package assembler (tar.gz overlay + sha1 meta)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: `updates.xri` generator + CI validation wiring

**Files:**
- Create: `integration/pixinsight/repo/gen-updates-xri.sh`
- Test: `integration/pixinsight/repo/test/test-gen-updates-xri.sh`
- Modify: `.github/workflows/module.yml` (add a repo-generator validation step)

**Interfaces:**
- Consumes: `.meta` files from Task 4.
- Produces: `gen-updates-xri.sh <release-date YYYYMMDD> <version-range> <title> <out-file> <meta...>` writes a schema-valid `updates.xri` with one shared `<metadata>` block and one `<platform>/<package>` per meta. OS mapping linux→linux, windows→windows, macos→macosx.

- [ ] **Step 1: Write the failing test**

`integration/pixinsight/repo/test/test-gen-updates-xri.sh`:
```sh
#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GEN="$HERE/../gen-updates-xri.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

for p in "linux x64" "windows x64" "macos x64"; do
  set -- $p; os="$1"; arch="$2"
  printf 'fileName=%s-%s-module.tar.gz\nsha1=%040d\nos=%s\narch=%s\n' \
    "$os" "$arch" 1 "$os" "$arch" > "$TMP/$os-$arch.meta"
done

bash "$GEN" 20260728 "1.9.4:1.9.4" "MergeMosaic module" "$TMP/updates.xri" \
  "$TMP/linux-x64.meta" "$TMP/windows-x64.meta" "$TMP/macos-x64.meta"

XRI="$TMP/updates.xri"
[ -f "$XRI" ] || { echo "FAIL: updates.xri missing"; exit 1; }

# Well-formed XML (stdlib, no xmllint needed).
python3 - "$XRI" <<'PY'
import sys, xml.etree.ElementTree as ET
root = ET.parse(sys.argv[1]).getroot()
assert root.tag.endswith('xri'), root.tag
xml = open(sys.argv[1]).read()
for os_attr in ('os="linux"','os="windows"','os="macosx"'):
    assert os_attr in xml, os_attr
assert xml.count('type="module"') == 3, "expected 3 module packages"
assert 'releaseDate="20260728"' in xml
assert 'arch="x64"' in xml
assert 'version="1.9.4:1.9.4"' in xml
print("xri structure OK")
PY
echo "PASS: gen-updates-xri"
```
Then `chmod +x`.

- [ ] **Step 2: Run the test to verify it fails**

Run: `bash integration/pixinsight/repo/test/test-gen-updates-xri.sh`
Expected: FAIL — `gen-updates-xri.sh` does not exist.

- [ ] **Step 3: Write `gen-updates-xri.sh`**

```sh
#!/usr/bin/env bash
# Emit a schema-valid PixInsight updates.xri from one or more package .meta files.
# Usage: gen-updates-xri.sh <releaseDate YYYYMMDD> <versionRange> <title> <out> <meta...>
set -euo pipefail
DATE="${1:?releaseDate}"; VER="${2:?versionRange}"; TITLE="${3:?title}"; OUT="${4:?out}"
shift 4
[ $# -ge 1 ] || { echo "at least one .meta required" >&2; exit 2; }

os_to_xri() { case "$1" in macos) echo macosx ;; *) echo "$1" ;; esac; }

MID="${DATE}-mmm"
{
  echo '<?xml version="1.0" encoding="UTF-8"?>'
  echo '<xri version="1.0" xmlns="http://www.pixinsight.com/xri">'
  echo "   <description><p>${TITLE}.</p></description>"
  echo "   <metadata id=\"${MID}\" releaseDate=\"${DATE}\">"
  echo "      <title>${TITLE}</title>"
  echo "      <description><p>${TITLE}. MosaicMerge process for PixInsight.</p></description>"
  echo "   </metadata>"
  for META in "$@"; do
    # shellcheck disable=SC1090
    fileName=""; sha1=""; os=""; arch=""
    while IFS='=' read -r k v; do
      case "$k" in fileName) fileName="$v";; sha1) sha1="$v";; os) os="$v";; arch) arch="$v";; esac
    done < "$META"
    xos="$(os_to_xri "$os")"
    echo "   <platform os=\"${xos}\" arch=\"${arch}\" version=\"${VER}\">"
    echo "      <package fileName=\"${fileName}\" sha1=\"${sha1}\" type=\"module\" releaseDate=\"${DATE}\" metadata=\"${MID}\"/>"
    echo "   </platform>"
  done
  echo '</xri>'
} > "$OUT"
echo "wrote $OUT"
```
Then `chmod +x`.

- [ ] **Step 4: Run the test to verify it passes**

Run: `bash integration/pixinsight/repo/test/test-gen-updates-xri.sh`
Expected: PASS — prints `xri structure OK` then `PASS: gen-updates-xri`.

- [ ] **Step 5: Wire repo-generator validation into `module.yml`**

In `.github/workflows/module.yml`, in the `linux` job, add a step **after** "Build module + worker, stage payload" and **before** the artifact upload:

```yaml
      - name: Generate + validate repository package and updates.xri (no publish)
        run: |
          sudo apt-get update && sudo apt-get install -y libxml2-utils
          bash integration/pixinsight/repo/gen-package.sh linux x64 "$PWD/stage" "$PWD/repo-out"
          bash integration/pixinsight/repo/gen-updates-xri.sh \
            "$(date -u +%Y%m%d)" "1.9.4:1.9.4" "MergeMosaic module" \
            "$PWD/repo-out/updates.xri" "$PWD/repo-out/linux-x64.meta"
          xmllint --noout "$PWD/repo-out/updates.xri"
          echo "repository artifacts generated + validated (dormant: not published)"

      - name: Run repo generator tests
        run: |
          bash integration/pixinsight/repo/test/test-gen-package.sh
          bash integration/pixinsight/repo/test/test-gen-updates-xri.sh
```

Update the artifact-upload step to also include the repo output:
```yaml
      - name: Upload unsigned module + repo artifacts
        uses: actions/upload-artifact@v4
        with:
          name: mmm-pxm-linux-x64
          path: |
            stage/bin/**
            repo-out/**
          if-no-files-found: error
```

- [ ] **Step 6: Re-validate `module.yml` parses**

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/module.yml')); print('ok')"`
Expected: PASS — `ok`.

- [ ] **Step 7: Commit**

```bash
git add integration/pixinsight/repo/gen-updates-xri.sh \
        integration/pixinsight/repo/test/test-gen-updates-xri.sh \
        .github/workflows/module.yml
git commit -m "feat(repo): updates.xri generator + CI validation (no publish)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: Dormant signing + publish driver

**Files:**
- Create: `integration/pixinsight/repo/sign-and-publish.sh`
- Test: `integration/pixinsight/repo/test/test-sign-and-publish.sh`

**Interfaces:**
- Produces: `sign-and-publish.sh [--dry-run] --stage-root <dir> --xri <file> [--out <dir>]`. Without `MMM_CPD_XSSK` set it prints a `DORMANT:` message and exits 0. With `--dry-run` it prints (never executes) the exact `PixInsight --sign-module-file` command for each `mmm-pxm.*` under `<stage-root>` and a CodeSign command for the `.xri`. Only with a real `MMM_CPD_XSSK` (and no `--dry-run`) would it execute — that path is documented, not exercised.

- [ ] **Step 1: Write the failing test**

`integration/pixinsight/repo/test/test-sign-and-publish.sh`:
```sh
#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SP="$HERE/../sign-and-publish.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/linux/bin" "$TMP/windows/bin" "$TMP/macos/bin"
: > "$TMP/linux/bin/mmm-pxm.so"
: > "$TMP/windows/bin/mmm-pxm.dll"
: > "$TMP/macos/bin/mmm-pxm.dylib"
: > "$TMP/updates.xri"

# (a) No CPD identity -> dormant, exit 0.
out="$(unset MMM_CPD_XSSK; bash "$SP" --stage-root "$TMP" --xri "$TMP/updates.xri")"
echo "$out" | grep -q "DORMANT" || { echo "FAIL: not dormant without CPD key"; exit 1; }

# (b) Dry-run with a fake key -> prints sign commands for all three binaries + xri, executes nothing.
out="$(MMM_CPD_XSSK=/fake.xssk MMM_CPD_XSSK_PASSWORD=x bash "$SP" --dry-run \
        --stage-root "$TMP" --xri "$TMP/updates.xri")"
for b in mmm-pxm.so mmm-pxm.dll mmm-pxm.dylib; do
  echo "$out" | grep -q -- "--sign-module-file=.*$b" || { echo "FAIL: no sign cmd for $b"; exit 1; }
done
echo "$out" | grep -qi "codesign\|--sign-xml\|updates.xri" || { echo "FAIL: no xri sign cmd"; exit 1; }
# No signatures were actually produced.
[ -z "$(find "$TMP" -name '*.xsgn')" ] || { echo "FAIL: dry-run produced a signature"; exit 1; }
echo "PASS: sign-and-publish"
```
Then `chmod +x`.

- [ ] **Step 2: Run the test to verify it fails**

Run: `bash integration/pixinsight/repo/test/test-sign-and-publish.sh`
Expected: FAIL — `sign-and-publish.sh` does not exist.

- [ ] **Step 3: Write `sign-and-publish.sh`**

```sh
#!/usr/bin/env bash
# DORMANT repository signing + publish driver.
#
# Module signing requires the PixInsight binary (there is no standalone signer)
# AND a globally trusted CPD identity. Until a CPD .xssk is provided via
# MMM_CPD_XSSK, this script does nothing but report that it is dormant. One
# PixInsight install signs all platforms' module binaries (confirmed: signing is
# a hash-and-sign over file bytes), so this drives every mmm-pxm.* + the .xri in
# one pass on a single signer host.
#
# Usage: sign-and-publish.sh [--dry-run] --stage-root <dir> --xri <file> [--out <dir>]
# Env:   MMM_CPD_XSSK, MMM_CPD_XSSK_PASSWORD  (the CPD identity; unset => dormant)
#        PIXINSIGHT (path to the PixInsight binary; default: PixInsight)
set -euo pipefail

DRY=0; STAGE_ROOT=""; XRI=""; OUT=""
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) DRY=1; shift ;;
    --stage-root) STAGE_ROOT="$2"; shift 2 ;;
    --xri) XRI="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done
[ -n "$STAGE_ROOT" ] && [ -n "$XRI" ] || { echo "--stage-root and --xri are required" >&2; exit 2; }

if [ -z "${MMM_CPD_XSSK:-}" ]; then
  echo "DORMANT: repository signing/publishing is gated on a CPD identity."
  echo "         Set MMM_CPD_XSSK (and MMM_CPD_XSSK_PASSWORD) once the Pleiades"
  echo "         Certified-Developer certificate is in hand. Nothing signed/published."
  exit 0
fi

PIXINSIGHT="${PIXINSIGHT:-PixInsight}"
run() { if [ "$DRY" -eq 1 ]; then echo "[dry-run] $*"; else "$@"; fi; }

# 1) Sign every module binary (any platform) from this one host.
find "$STAGE_ROOT" -type f \( -name 'mmm-pxm.so' -o -name 'mmm-pxm.dll' -o -name 'mmm-pxm.dylib' \) \
| while read -r BIN; do
  run "$PIXINSIGHT" --automation-mode -n \
    "--sign-module-file=$BIN" \
    "--xssk-file=$MMM_CPD_XSSK" "--xssk-password=${MMM_CPD_XSSK_PASSWORD:-}" \
    --force-exit
done

# 2) Sign the repository index (CodeSign / --sign-xml-file).
run "$PIXINSIGHT" --automation-mode -n \
  "--sign-xml-file=$XRI" \
  "--xssk-file=$MMM_CPD_XSSK" "--xssk-password=${MMM_CPD_XSSK_PASSWORD:-}" \
  --force-exit

# 3) Publish (GitHub Pages). Left as an explicit manual/documented step until the
#    hosting URL is confirmed; see repo/README.md.
if [ -n "$OUT" ]; then
  run cp -r "$STAGE_ROOT" "$XRI" "$OUT/"
fi
echo "signing complete${DRY:+ (dry-run)}"
```
Then `chmod +x`.

Note for the implementer: the exact core option for signing an `.xri` from the
command line may be `--sign-xml-file` or require the in-app CodeSign script; the
test only asserts the driver *emits an xri-signing command*, and the whole path
is dormant, so the precise flag is confirmed when CPD signing is switched on.
Keep the flag name in one place and note it in `repo/README.md`.

- [ ] **Step 4: Run the test to verify it passes**

Run: `bash integration/pixinsight/repo/test/test-sign-and-publish.sh`
Expected: PASS — prints `PASS: sign-and-publish`.

- [ ] **Step 5: Commit**

```bash
git add integration/pixinsight/repo/sign-and-publish.sh \
        integration/pixinsight/repo/test/test-sign-and-publish.sh
git commit -m "feat(repo): dormant CPD signing + publish driver (gated, dry-runnable)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 7: Documentation capstone

**Files:**
- Create: `integration/pixinsight/repo/README.md`
- Modify: `docs/superpowers/specs/2026-07-27-pixinsight-integration-design.md` (§12 cross-reference)

**Interfaces:**
- Consumes: all prior tasks (documents the whole pipeline).

- [ ] **Step 1: Write `integration/pixinsight/repo/README.md`**

Content must cover, concretely:
- **What this is:** the CI-driven build + the dormant repository pipeline; pointer to the spec.
- **Interim on-ramp (now, our own licensed machines):** download the CI artifact `mmm-pxm-linux-x64` from a `Module` workflow run → place `mmm-pxm.so` + `mmm-ipc-worker` together → `make -f makefile-x64 sign XSSK_FILE=… XSSK_PASSWORD=…` (local identity) → PixInsight **Install Modules**. Cross-link `integration/pixinsight/module/README.md`.
- **The repository pipeline (dormant):** `gen-package.sh` → `gen-updates-xri.sh` → `sign-and-publish.sh`; what each produces; the `bin/` overlay layout; that `sign-and-publish.sh` is dormant until `MMM_CPD_XSSK` is set; the single-host cross-platform signing fact.
- **Going live (post-CPD):** provide the CPD `.xssk` to the signer, set `MMM_CPD_XSSK`/`MMM_CPD_XSSK_PASSWORD`, confirm the GitHub Pages hosting URL, run the pipeline, add the repository URL in PixInsight → Resources → Updates → Manage Repositories.
- **CPD action item:** applied 2026-07-28; awaiting Pleiades response. Steps to complete: run the bundled `SubmitCPD` script (generate `.xssk` via `SigningKeys` with a real Developer id, submit public key), then install the returned identity.
- **The `--sign-xml-file` caveat** from Task 6 (confirm the exact `.xri`-signing invocation when switching CPD on).

- [ ] **Step 2: Add the §12 cross-reference to the integration spec**

In `docs/superpowers/specs/2026-07-27-pixinsight-integration-design.md`, at the end of the §12 "Later — repository distribution (planned, not implemented)" bullet, append:

```markdown
  - **Superseded by** `2026-07-28-pixinsight-ci-and-distribution-design.md`: CI
    now builds the module against pinned open-source PCL and a dormant
    repository pipeline (packages + `updates.xri` + CPD signing) is in place,
    gated on the CPD certificate (applied 2026-07-28).
```

- [ ] **Step 3: Verify links resolve**

Run:
```bash
test -f integration/pixinsight/repo/README.md && \
grep -q "2026-07-28-pixinsight-ci-and-distribution" docs/superpowers/specs/2026-07-27-pixinsight-integration-design.md && \
echo OK
```
Expected: PASS — `OK`.

- [ ] **Step 4: Commit**

```bash
git add integration/pixinsight/repo/README.md \
        docs/superpowers/specs/2026-07-27-pixinsight-integration-design.md
git commit -m "docs(repo): CI/distribution README + integration spec §12 cross-ref

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage:**
- Spec §3.1 CI PCL build (pinned, cached) → Tasks 1, 3. ✔
- Spec §3.2 module build job + host CTest gate + unsigned artifact → Tasks 2, 3. ✔
- Spec §3.3 repo generator (package + updates.xri) validated dormant → Tasks 4, 5. ✔
- Spec §4 workflow (triggers, matrix, cache, ipc tests, placeholders) → Task 3. ✔
- Spec §5 package `tar.gz` overlay + `updates.xri` schema + CI validation → Tasks 4, 5. ✔
- Spec §6 signing architecture (single host, dormant, gated) → Task 6. ✔
- Spec §7 testing (host CTest gate, xmllint/python validation, Rust green) → Tasks 3, 4, 5. ✔
- Spec §3 docs + §12 cross-ref + CPD action item → Task 7. ✔

**Placeholder scan:** No "TBD/TODO/handle appropriately" in steps; every code step has real content. The one genuine uncertainty (the exact `.xri`-signing flag) is on a **dormant** path, is called out explicitly with a test that only asserts a command is emitted, and is resolved at CPD switch-on — not a plan gap.

**Type/name consistency:** `--pcl`/`--stage`/`--out` script flags, the `<os>-<arch>.meta` keys (`fileName`/`sha1`/`os`/`arch`), and the OS mapping (macos→macosx) are used identically across Tasks 1, 2, 4, 5, 6. The artifact name `mmm-pxm-linux-x64` and paths `stage/bin/**` + `repo-out/**` match between Task 3 and Task 5's upload edit.

**Scope:** Focused on Plan 3b (CI) + Plan 3c (dormant repo). The Win/mac native ports (Plan 3a) are explicitly out and represented only by placeholder jobs. Each task ends with an independently testable deliverable + commit.
