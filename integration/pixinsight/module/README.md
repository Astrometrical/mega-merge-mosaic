# mmm-pxm — MergeMosaic PixInsight module

The PCL `Process`/`ProcessInterface` wrapper around the [`../host`](../host)
transport library: a thin **Mosaic → MosaicMerge** process that drives
`mmm-ipc-worker` over shared memory from inside PixInsight, using the same
byte-exact analyze/blend pipeline as the `mmm` CLI. It is PixInsight-only code
(depends on `libPCL-pxi.a` and PCL headers); the transport it wraps
(`host/`) has no PCL dependency and is tested independently (see
`../host/README.md`).

Linux/WSL only for now — see [spec §12](../../../docs/superpowers/specs/2026-07-27-pixinsight-integration-design.md#12-packaging--distribution--concrete-linuxwsl-first)
for what Windows/macOS and signed-repository distribution still need.

## One-time prerequisite: build `libPCL-pxi.a`

The module links PixInsight's own PCL library, statically. On a stock
install (`/opt/PixInsight`), the PCL source tree
(`/opt/PixInsight/src/{pcl,3rdparty}`) is **root-owned**, so build it
out-of-tree into a writable directory instead of in place — no `sudo`
required:

```sh
mkdir -p ~/.local/pcl-build/lib
cp -r /opt/PixInsight/src/pcl      ~/.local/pcl-build/pcl
cp -r /opt/PixInsight/src/3rdparty ~/.local/pcl-build/3rdparty
mkdir -p ~/.local/pcl-build/pcl/linux/g++/x64/Release

cd ~/.local/pcl-build/pcl/linux/g++
PCLSRCDIR=~/.local/pcl-build \
PCLINCDIR=/opt/PixInsight/include \
PCLLIBDIR64=~/.local/pcl-build/lib \
make -f makefile-x64 -j$(nproc)
```

This produces `~/.local/pcl-build/lib/libPCL-pxi.a` (~45 MB). It builds clean
with g++ 13.3 and needs no extra packages beyond what a stock PixInsight +
build-essential install already provides. You only do this once per machine
(or PixInsight core update — see the ABI note at the end of this file).

## Building the module

```sh
cd integration/pixinsight/module
make
```

`Makefile` is a thin wrapper delegating to `makefile-x64`, which defaults to
the paths above:

```make
PCLINCDIR ?= /opt/PixInsight/include
PCLLIBDIR ?= $(HOME)/.local/pcl-build/lib
```

Override either if your PCL headers or `libPCL-pxi.a` live elsewhere, e.g.:

```sh
make PCLINCDIR=/opt/PixInsight/include PCLLIBDIR=/some/other/lib
```

The build compiles the module sources in this directory **and** the
`host/` transport objects into one shared object; `host/` itself needs no PCL
and is never built with cmake for this purpose (cmake is only needed for
`host/`'s own standalone CTest suite — see `../host/README.md` — not for
building the module). Output: `mmm-pxm.so`, warning-free with `-Wall`. The
module intentionally links with undefined PixInsight-core symbols (resolved
by the PixInsight core at load time), so `nm -D -u mmm-pxm.so` reporting many
undefined `PCL`-core symbols is expected; watch instead for any undefined
`mmm::`/`_ZN3mmm` symbols, which would mean the host objects failed to link
in.

```sh
make clean   # remove x64/Release/*.o and mmm-pxm.so
```

## Building and placing the worker

The module spawns `mmm-ipc-worker` as a child process and resolves its path
via `dladdr` relative to its own `.so` — the worker binary must sit **beside**
`mmm-pxm.so` in the same directory:

```sh
# from the repo root
source ~/.cargo/env   # if cargo isn't already on PATH
cargo build --release -p mmm-ipc-worker
cp target/release/mmm-ipc-worker integration/pixinsight/module/
```

If the worker binary is missing, `MosaicMerge` reports a clear PixInsight
error at execute time rather than failing silently or hanging.

## Code signing (required — PixInsight ≥ 1.9)

PixInsight **1.9 removed the "allow installation of unsigned modules" option**;
an unsigned module now fails to install with *"Required module signature not
found"*. For **local development you do NOT need a Pleiades certificate** —
PixInsight provides a *local signing identity* (tied to your license, valid only
on machines where you hold a valid PixInsight license). The Certified-Developer
certificate is only needed to distribute to *other* people (spec §12).

**1. Generate a local signing key — once (GUI, needs your license).** Run the
bundled **SigningKeys** script (open `/opt/PixInsight/src/scripts/SigningKeys/SigningKeys.js`
in the Script editor, or find it under the Script menu). Tick **"Local signing
identity"** (the Developer id field is ignored for a local identity — it's
derived from your license) and write a keys file, e.g. `~/mmm-dev.xssk`, with a
password. (Back this file up — losing it loses the identity.)

**1b. Install the local signing identity into the core — once.** Generating the
`.xssk` only creates the keypair; the core will still reject a module signed
with it (*"Unknown code signing identity '…'"*) until the identity's **public**
key is installed. Run **Script → Local Signing Identity…**, and in the *Manage
Local Signing Identity* dialog: select your `~/mmm-dev.xssk`, enter its
password, and enable the **"make persistent"** option (stores the encrypted
public key in global settings so it loads on every startup). After this the core
trusts modules/scripts signed with that identity.

**2. Sign the module — each build:**

```sh
make -f makefile-x64 sign \
  XSSK_FILE=$HOME/mmm-dev.xssk XSSK_PASSWORD='your-password'
```

which runs, roughly:

```sh
PixInsight --automation-mode -n \
  --sign-module-file="$PWD/mmm-pxm.so" \
  --xssk-file="$HOME/mmm-dev.xssk" --xssk-password='your-password' \
  --force-exit
```

That writes `mmm-pxm.xsgn` beside the `.so`. If PixInsight stays open after
signing on your version, override the exit behavior:
`make -f makefile-x64 sign … SIGN_EXIT=--force-exit` (or drop `-n`) — adjust once
for your build and it's stable thereafter.

## Installing into PixInsight (dev flow)

There is no signed update repository yet (spec §12) — install the freshly built,
**signed** module directly:

1. Ensure `mmm-pxm.so` **and its `mmm-pxm.xsgn`** (from the signing step above)
   are together in the folder, alongside `mmm-ipc-worker`.
2. In PixInsight: **Process → Modules → Install Modules…**
3. Point it at `integration/pixinsight/module/`, or select `mmm-pxm.so` directly.
4. Restart PixInsight if prompted.
5. Confirm **MosaicMerge** now appears under the **Mosaic** category in the
   Process menu.

A module signed with your local identity loads on your licensed machine(s); this
is the whole dev-distribution story for now.

## Manual smoke test

The module is PCL/PixInsight code, so its behavior can only be verified
inside the real GUI — cargo/ctest cannot drive it. This checklist is the one
used during development (see `.superpowers/sdd/2026-07-28-pixinsight-plan2b-pcl-module/task-5-report.md`
for the full narrative); repeat it after any module rebuild before trusting a
run:

1. **Install** as above; confirm MosaicMerge appears under Mosaic.
2. **Aligned views** — open ≥2 registered full-canvas panels
   (MosaicByCoordinates output) as views. Launch MosaicMerge, **Add Views…**,
   pick a session directory, **Input = Auto**, Apply → a new blended
   `ImageWindow` appears; the Console shows analyze/blend progress
   percentages.
3. **Solved views** — open raw plate-solved panels as views (differing
   geometry, each with an astrometric solution). `Input = Auto` (resolves to
   Solved) or forced `Input = Solved`, pick a session directory, Apply → new
   blended window. A view lacking a solution must yield a clear error and no
   window.
4. **Files mode** — switch to **Files**, **Add Files…** on-disk panels, pick a
   session directory, Apply → new blended window.
5. **Fault isolation** — start a run, then induce a worker failure (rename or
   delete `mmm-ipc-worker` before running, or kill it mid-run) → a clean
   PixInsight error dialog, **no partial window**, source views intact,
   PixInsight itself still alive.
6. **Cancel/progress** — during a longer run, watch the `Progress` label
   update and use the Console Abort (or the interface **Cancel** button) →
   the run stops with a clean "cancelled" error and no output window.

## Future work (not implemented — see spec §12)

- A signed PixInsight **update repository** for one-click install/auto-update
  (needs a code-signing certificate from the PixInsight team and a
  per-platform signed package matrix).
- **Windows/macOS** ports: Windows/macOS shared-memory transport
  (`CreateFileMapping`/`MapViewOfFile` — the Rust `shm.rs` already stubs
  non-Unix), macOS notarization of `mmm-ipc-worker` (else Gatekeeper blocks
  the `exec`), Windows Authenticode/SmartScreen.

## PCL ABI note

The module is built against a specific PixInsight core's PCL/SDK
(`/opt/PixInsight/include`, `libPCL-pxi.a`). A PixInsight core update can
change that ABI, so re-run the `libPCL-pxi.a` build and re-link the module
after updating PixInsight. `mmm-ipc-worker` is unaffected by PixInsight
updates — it only speaks the wire protocol (`../PROTOCOL.md`), never links
PCL, and never needs rebuilding for this reason.
