# PixInsight Module — CI Build & Distribution — Design

Status: **approved 2026-07-28** (brainstorming). Follows the
[integration design](2026-07-27-pixinsight-integration-design.md) (Plans 1/2a/2b,
complete on branch `pixinsight-integration`). This spec covers the **build
automation + distribution** phase and supersedes the forward-looking parts of
that document's §12. Scope owner: `.github/workflows/`, the PCL-in-CI build, and
new packaging/repository tooling under `integration/pixinsight/`.

## 1. Goal

Make the PixInsight module **easy to get onto test machines**, in service of
end-to-end integration testing — **not yet public distribution to arbitrary
users**. Two concrete outcomes:

1. **CI builds** `mmm-pxm.so` (the PCL module) and `mmm-ipc-worker` (the Rust
   sibling) reproducibly on GitHub Actions, producing installable artifacts,
   with **no PixInsight install on the runner**.
2. **A PixInsight update-repository pipeline** (`updates.xri` + per-platform
   `tar.gz` packages + signing + hosting) is **built and validated but dormant**
   — it publishes nothing until a Pleiades **Certified PixInsight Developer
   (CPD)** certificate is in hand (applied for 2026-07-28; awaiting response).

Until CPD lands, the on-ramp to our **own licensed test machines** is: download
the CI-built (unsigned) artifacts → sign locally with `make sign` (local signing
identity) → **Install Modules**. CI eliminates the painful per-machine
out-of-tree `libPCL-pxi.a` build; the tester's only manual step is the local
sign, which our own licensed machines can already do.

## 2. Decisions (locked in brainstorming, 2026-07-28)

| # | Decision | Rationale |
|---|---|---|
| Platform scope | Matrix is **structured for all three OSes**; **Linux x64 is the only live/green build**. Windows/macOS jobs are `continue-on-error` placeholders until the native ports land in a later plan. | The Win/mac **native ports** (shm + host transport rewrite, notarization) are substantial native engineering, separable from CI, and are the long pole — deferred. A CI matrix that "builds Windows" is useless until `shm.rs` and the C++ host support Windows. |
| PCL acquisition | Build the open-source PCL from **gitlab.com/pixinsight/PCL**, pinned to a **commit SHA** matching the target core, cached keyed on that SHA. | The GitLab repo is self-contained (headers + source + all 3rdparty), tiny (~56 MB shallow clone), builds with only g++≥12 + make (no Qt/X11 for the static lib), and current `master` == **PCL 2.10.4 / PixInsight 1.9.4 "Lockhart"** (`Version.cpp` dated 2026-06-21), matching the local core. There are **no release tags** — pin by SHA and verify `Version.cpp`. |
| Signing (CI) | CI produces **unsigned** artifacts. Signing happens **only** on a PixInsight-equipped machine (the signer), because the **only** module signer is the PixInsight binary itself (`--sign-module-file` / CodeSign) — there is no standalone signer. | A stock GitHub runner has no PixInsight install and no license. |
| Signing (cross-platform) | **One PixInsight install signs all three platforms' modules.** A single Linux signer signs `mmm-pxm.so`/`.dll`/`.dylib` (+ the `.xri`) in one headless `--automation-mode` pass with one `.xssk`. | Confirmed (§6): module signing is a hash-and-sign over file bytes (SHA-512 + Ed25519); the CodeSign path gates only on extension ∈ {.so,.dylib,.dll} and a `-pxm` basename — no host-OS branch, no binary-format inspection. |
| Distribution | Stand up the **full repository pipeline** (generator + signing + publish), but keep it **dormant until CPD**. No GitHub Releases in the interim; the interim on-ramp is CI **artifacts** + local `make sign`. | The module `.so`/`.dll`/`.dylib` **must be signed to load** in 1.9 regardless of delivery mechanism; a repository can't serve a loadable module without a CPD-trusted signature, and CPD is being pursued. |
| Sequencing | This phase = **Plan 3b (CI) + Plan 3c (repo pipeline, dormant)**, all-platform-structured with Linux live. **Plan 3a (native Win/mac ports)** is a later, separate plan that lights up the matrix. | Clean dependency order without a half-built matrix blocking progress. |

**Non-goals this phase** (explicit): the Windows/macOS **native ports**
(`CreateFileMapping`/`MapViewOfFile` in `crates/mmm-core/src/ipc/shm.rs`; the C++
host's Windows spawn/pipe/handle equivalents; macOS worker **notarization**);
any live/published repository; GitHub Releases for the module; obtaining the CPD
cert (an external action item, tracked in docs). Existing `ci.yml` (Rust tests)
and `release.yml` (CLI releases) are **untouched** — the module work is additive.

## 3. Deliverables

1. **`.github/workflows/module.yml`** — a new workflow that builds `libPCL-pxi.a`
   from pinned open-source PCL (cached), builds `mmm-pxm.so` + `mmm-ipc-worker`,
   runs the host CTest golden suite as the correctness gate, and uploads an
   unsigned per-platform package as an artifact. Linux live; Win/mac scaffolded
   `continue-on-error`.
2. **`integration/pixinsight/repo/`** — the repository/packaging pipeline:
   - a **package assembler** (per-platform `tar.gz` with the correct
     install-root overlay layout),
   - an **`updates.xri` generator** (schema-valid XML; SHA-1 per package),
   - a **signing driver** (headless `--sign-module-file` for every `mmm-pxm.*`
     + CodeSign for the `.xri`) — parameterized on a CPD `.xssk`, **gated off**,
   - a **publish target** (GitHub Pages) — **gated off** until CPD,
   - a `README.md` documenting the interim artifact on-ramp, the post-CPD
     repository flow, and the **CPD `SubmitCPD` submission** action item.
3. **Doc updates** — mark the superseded parts of the integration spec §12 as
   resolved by this document; keep `integration/pixinsight/module/README.md`
   consistent (the local-sign on-ramp already documented there).

## 4. CI build workflow (`.github/workflows/module.yml`)

**Triggers.** `push`/`pull_request` touching `integration/pixinsight/**`,
`crates/mmm-core/src/ipc/**`, `crates/mmm-ipc-worker/**`, or the workflow file
itself; plus manual `workflow_dispatch`. (Scoped paths keep the Rust-only
`ci.yml` as the general gate and avoid double-running on unrelated changes.)

**Matrix.** `os: [ubuntu-latest, windows-latest, macos-latest]`.
`fail-fast: false`. **ubuntu required**; windows/macos carry
`continue-on-error: true` until Plan 3a lands (so the matrix is present and
visible but does not block the branch on the not-yet-ported platforms).

**Ubuntu job steps (the live build):**

1. **Resolve + cache PCL.** Resolve the pinned PCL commit SHA (a value stored in
   the workflow / a small pinned file). Restore `libPCL-pxi.a` + the needed
   3rdparty archives from `actions/cache` keyed on that SHA. On a miss:
   shallow-clone `gitlab.com/pixinsight/PCL` at the SHA (optionally
   sparse/filtered to skip `src/modules`, the only bulk), set
   `PCLDIR/PCLSRCDIR/PCLINCDIR/PCLLIBDIR64`, build the required 3rdparty libs
   (`src/3rdparty/linux/make-3rdparty.sh` — at minimum RFC6234; cminpack if PSF
   paths are reachable), then `make -f makefile-x64 && make -f makefile-x64
   install` in `src/pcl/linux/g++`, and save the cache.
   - **Known risk (verify on first build):** `CUDADevice.cpp` references
     `cuda.h`. The current makefile adds no CUDA include path (likely guarded),
     but if the build breaks here the fallback is to drop that one TU from
     `SRC_FILES` or `apt install nvidia-cuda-toolkit` for headers. Resolve once
     and document.
2. **Build the module.** `make` in `integration/pixinsight/module` with
   `PCLINCDIR` → the cloned repo's `include/`, `PCLLIBDIR` → the built `.a`'s
   dir. Assert warning-free and **0 undefined `mmm::` symbols** (the host objects
   linked in), as the module README documents.
3. **Build the worker.** `cargo build --release -p mmm-ipc-worker`.
4. **Correctness gate.** Build + run the **host CTest golden suite**
   (`integration/pixinsight/host`, CMake/CTest) — the byte-identity + fault-
   isolation tests that need **no PixInsight**. This is the CI correctness gate
   for the module transport (approved). Because `ci.yml` fires only on
   pushes/PRs to `main`, feature-branch pushes would otherwise run no Rust
   tests; so `module.yml` also runs `cargo test -p mmm-core -p mmm-ipc-worker`
   (the ipc-relevant crates) to cover branch development. The full
   `cargo test --workspace` stays in `ci.yml`, not duplicated here.
5. **Assemble + upload.** Stage the **unsigned** package payload (§5) and upload
   it as a build artifact — the interim download for licensed test machines.

**Windows/macOS jobs** exist with the analogous shape but are
`continue-on-error` placeholders (e.g. a documented `exit`/skip with a clear
"native port pending — Plan 3a" message) so the matrix is ready to be filled in
without a workflow rewrite.

## 5. Repository & package format (built, validated, dormant)

Grounded in PixInsight's repository reference and a real-world `updates.xri`.

**Package = a plain compressed archive overlaying the install root.** Format:
**`tar.gz`** (universal, recommended). There is **no manifest**; destination is
determined **purely by the archive's internal directory tree**, interpreted
relative to the PixInsight install root. Third parties may write only to
sanctioned dirs; binaries go in **`bin/`**.

Per-platform archive contents:

```
bin/mmm-pxm.so            # (or mmm-pxm.dll / mmm-pxm.dylib)
bin/mmm-pxm.xsgn          # the module signature sidecar (post-sign; dormant until CPD)
bin/mmm-ipc-worker        # (or mmm-ipc-worker.exe) — sibling worker, allowed & expected in bin/
```

Shipping the non-module worker binary in `bin/` is explicitly permitted (arbitrary
files are laid down by path); it lands beside the module exactly as the module's
`dladdr` sibling-resolution expects.

**`updates.xri`** — strict XML (UTF-8, namespace `http://www.pixinsight.com/xri`).
Structure the generator emits:

- root `<xri version="1.0">` with a `<description>`;
- one `<platform os="linux|windows|macosx" arch="x64" version="from:to">`
  wrapper per platform (version = target core range, e.g. `1.9.4:1.9.4` or a
  suitable span);
- one `<package type="module" fileName="…" sha1="…" releaseDate="YYYYMMDD"
  metadata="…"/>` per platform, `sha1` = lowercase hex SHA-1 of the archive
  (verified by the core on download);
- shared `<metadata id="…" releaseDate="…"><title/><description/></metadata>`
  blocks referenced by the packages (one description serving all platforms).

**CI validation without publishing:** the generator runs in CI to produce the
archives + `updates.xri` as artifacts and **`xmllint`-validates** the XML; it
does **not** publish. Only `type` values `module` (and later `script`/`documentation`)
are permitted for third parties — we use `module`.

**No official packaging tool exists** (no `--make-package`); the generator is
ours (assemble tree → `tar czf` → `sha1sum` → emit `updates.xri`), matching how
established third-party repos are built.

## 6. Signing architecture

**Fact 1 — signing requires the PixInsight binary.** Modules are signed only by
`PixInsight --sign-module-file=<path> --xssk-file=<keys> …` (headless,
`--automation-mode`), and the `.xri` by the in-app **CodeSign** script /
`Security.generateXMLSignature`. There is **no standalone signer**. Therefore the
signing + publish stage runs on a **PixInsight-equipped signer**, never on a
stock GitHub runner.

**Fact 2 — one signer covers all platforms.** Confirmed from primary sources
(§ evidence below): a single PixInsight install signs Linux/Windows/macOS module
binaries alike. The signer stage ingests all three platforms' **unsigned** CI
artifacts and, in one headless pass with one `.xssk`:

1. `--sign-module-file` each `bin/mmm-pxm.{so,dll,dylib}` → its `.xsgn`
   (each binary keeps its `-pxm` basename — a hard requirement of the signer);
2. re-pack each `tar.gz` including the `.xsgn`;
3. compute each archive's SHA-1;
4. generate + **CodeSign** `updates.xri`;
5. publish to GitHub Pages.

**Gating.** Steps that need a signing identity are **off** until CPD:
- **Local identity (now):** works only on machines holding our license — usable
  for the interim `make sign` on our own test machines, but **not** for a
  repository other machines consume, and cannot run on CI (license-bound).
- **CPD identity (post-approval):** globally trusted; its `.xssk` is provided to
  the signer (from a secret on a self-hosted/licensed signer, or a verified
  headless-signing runner). Only then does the pipeline publish. The repository
  URL is stable across the transition, so flipping CPD on is a config change,
  not a rebuild.

**Evidence for cross-platform signing** (recorded so it need not be
re-researched):
- Juan Conejero, "Module Signatures Required" (2024-04-22): the CodeSign script
  "supports the generation of signatures for binary module files on **all
  supported platforms**" since 1.8.9-2 build 1604.
- `/opt/PixInsight/src/scripts/CodeSign/CodeSignMain.js`: the module branch gates
  only on extension ∈ {.so,.dylib,.dll} and a `-pxm` basename, then calls
  `Security.generateModuleSignatureFile(...)` — no host-OS branch, no format
  inspection.
- `--sign-module-file` help text has no platform qualifier; the native signer's
  only failure strings concern the developer identity/keys, never binary format.
- Our own `makefile-x64:sign` already invokes this headlessly for `.so`.
- **Unverified:** an end-to-end sign-on-Linux / load-on-Windows runtime proof
  (needs the `.xssk` + a Windows install). A 5-minute local check —
  `cp mmm-pxm.so test-pxm.dll` then `--sign-module-file=…/test-pxm.dll`, expect a
  `test-pxm.xsgn` rather than a format error — would clinch it locally.

**Open verify item:** whether `PixInsight --automation-mode --sign-module-file`
runs on a **CI runner** (headless, no interactive license). If not, the signer is
a licensed/self-hosted machine even post-CPD. This does not block Plans 3b/3c
(which stop at unsigned artifacts + a dormant generator); it is resolved when CPD
signing is switched on.

## 7. Testing & verification

- **CI is the correctness gate.** `module.yml` runs the **host CTest golden
  suite** (byte-identity Files-vs-Aligned-vs-Solved + worker-crash + cancel,
  no PixInsight needed). Rust tests stay green: `module.yml` runs
  `cargo test -p mmm-core -p mmm-ipc-worker` (covers feature-branch pushes),
  and `ci.yml` runs the full `cargo test --workspace` on PRs/pushes to `main`.
- **Repository generator** is validated in CI by `xmllint` on the emitted
  `updates.xri` and by asserting each `tar.gz` has the exact `bin/…` layout and a
  correct SHA-1 — all without publishing.
- **PixInsight-requiring checks stay manual** (the module runtime, and any actual
  signed-install / add-repository test) — the existing GUI smoke-test checklist
  in the module README remains the gate for anything touching module runtime.
- **PCL build reproducibility** is proven by a clean cache-miss build in CI plus
  the `Version.cpp` == target-core assertion.

## 8. Risks & open items

- **CUDADevice.cpp / `cuda.h`** — the single most likely PCL-in-CI break; verify
  on first build, fall back to dropping the TU or installing CUDA headers.
- **PCL has no release tags** — pin by commit SHA; add a documented "how to bump
  the pin" note and a `Version.cpp` assertion so a drift from the target core is
  caught in CI.
- **Headless signing on CI** (§6 open verify item) — deferred to CPD switch-on;
  interim artifacts are unsigned by design.
- **CPD turnaround is unknown** — applied 2026-07-28; the dormant pipeline means
  no work is blocked waiting, and switch-on is a config/secret change.
- **Non-module worker binary security checks** — the package format permits a
  plain `mmm-ipc-worker` in `bin/`; whether 1.9 subjects a non-`-pxm` helper to
  any signature/Gatekeeper check on install/exec is unverified on non-Linux
  (matters only when the Win/mac ports + macOS notarization land — Plan 3a).

## 9. Please provide / decide (external, non-blocking)

- **CPD certificate** — applied for (2026-07-28); switch on repository publishing
  when approved and the `.xssk` is available to the signer.
- **Repository hosting URL** — GitHub Pages assumed; confirm the exact URL/repo
  when publishing goes live (does not block building the generator).
