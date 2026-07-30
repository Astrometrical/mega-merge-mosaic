# PixInsight module — CI build & distribution

> **⚠️ RELOCATING (2026-07-30).** These scripts are being **moved out of
> MergeMosaic** into a separate, private **"Astrometrical" website repository**
> that hosts the PixInsight update repository as static files on Cloudflare
> Pages. They are **kept here, annotated, as the tested starting point to lift
> into that project** — do not treat them as something to finish *in this repo*.
> The decision, the (favorable) repository trust findings, the corrected
> sign-then-package ordering, and the bundle contract are recorded in
> [`docs/superpowers/specs/2026-07-30-distribution-relocation-and-repo-trust-findings.md`](../../../docs/superpowers/specs/2026-07-30-distribution-relocation-and-repo-trust-findings.md).
> Key correction to the text below: a **local signing identity is accepted for
> the maintainer's own machines** — repository distribution is **not** blocked
> on CPD; CPD only makes the repo trusted by *other* people. The "dormant until
> CPD" framing below is superseded accordingly.

This directory holds the (originally dormant) PixInsight update-repository
pipeline (package + `updates.xri` + signing) for the `mmm-pxm` module. The
pipeline is built and CI-validated end to end. See the relocation spec above for
the current plan; the historical "dormant until CPD" details below are retained
for the lift-and-shift.

The full design (why these decisions, the package/XRI format, the signing
architecture, and open items) lives in
[`docs/superpowers/specs/2026-07-28-pixinsight-ci-and-distribution-design.md`](../../../docs/superpowers/specs/2026-07-28-pixinsight-ci-and-distribution-design.md).
That spec supersedes the forward-looking part of §12 of the original
[`2026-07-27-pixinsight-integration-design.md`](../../../docs/superpowers/specs/2026-07-27-pixinsight-integration-design.md).

Two things exist today:

1. **CI-driven build** ([`.github/workflows/module.yml`](../../../.github/workflows/module.yml)):
   builds `mmm-pxm.so` + `mmm-ipc-worker` against a pinned open-source PCL, with
   **no PixInsight install on the runner**. Linux x64 is the only live/green
   job; Windows/macOS are `continue-on-error` placeholders until the native
   ports (Plan 3a) land. The job also runs this directory's generator scripts
   (below) and validates their output with `xmllint`, but never publishes.
2. **This repository pipeline** (`gen-package.sh` → `gen-updates-xri.sh` →
   `sign-and-publish.sh`): assembles the per-platform package + `updates.xri`
   that a real PixInsight update repository serves, and would sign + publish
   them — except signing is gated on a Pleiades Certified-Developer (CPD)
   identity we don't have yet (see [CPD action item](#cpd-action-item)), so
   `sign-and-publish.sh` is intentionally a no-op until then.

Until CPD lands, getting the module onto a real PixInsight install (ours or
another licensed tester's) goes through the interim on-ramp below, **not**
through this repository pipeline.

## Interim on-ramp (now, our own licensed machines)

No signed repository exists yet, so install the CI-built module directly, the
same way local development already does:

1. Go to the GitHub Actions **`Module`** workflow, pick a run on the branch you
   want, and download the **`mmm-pxm-linux-x64`** artifact. It contains
   `stage/bin/mmm-pxm.so` + `stage/bin/mmm-ipc-worker` (unsigned) alongside the
   generated (also unsigned) `repo-out/` package + `updates.xri`.
2. Place `mmm-pxm.so` and `mmm-ipc-worker` **together** in one folder — the
   module resolves the worker's path relative to its own `.so` via `dladdr`, so
   they must sit side by side.
3. Sign the module locally with your PixInsight **local signing identity**
   (license-bound, not the CPD identity):
   ```sh
   make -f makefile-x64 sign XSSK_FILE=/path/to/your.xssk XSSK_PASSWORD='your-password'
   ```
   This is the same local-signing flow as native development builds; see
   [`integration/pixinsight/module/README.md`](../module/README.md#code-signing-required--pixinsight--19)
   for the one-time local-identity setup (`SigningKeys` script + **Script →
   Local Signing Identity…**) if you haven't done it on this machine before.
4. In PixInsight: **Process → Modules → Install Modules…**, point it at the
   folder from step 2, restart if prompted, and confirm **MosaicMerge** appears
   under the **Mosaic** category.

This on-ramp only works on machines holding a valid PixInsight license (the
local signing identity is license-bound) — it's not a path to distributing to
arbitrary third parties. That's what the repository pipeline below is for,
once CPD is live.

## The repository pipeline (dormant)

Three scripts, run in order, build what a PixInsight update repository serves.
None of them require a PixInsight install except the last one's signing step.

1. **[`gen-package.sh`](gen-package.sh)** `<os> <arch> <staging-dir> <out-dir>`
   — tars a staging tree (the install-root overlay) into a deterministic
   `<os>-<arch>-module.tar.gz`, and writes a matching `<os>-<arch>.meta`
   (`fileName`/`sha1`/`os`/`arch`) recording its SHA-1.

   The staging tree — and therefore the package's install-root overlay — is:
   ```
   bin/mmm-pxm.so            # (or mmm-pxm.dll / mmm-pxm.dylib on other OSes)
   bin/mmm-pxm.xsgn          # signature sidecar — added by sign-and-publish.sh, not this script
   bin/mmm-ipc-worker        # (or mmm-ipc-worker.exe) — sibling worker, allowed in bin/
   ```
   A PixInsight update repository has no manifest; where files land is
   determined purely by the archive's internal paths relative to the install
   root, so everything module-related goes under `bin/`.

2. **[`gen-updates-xri.sh`](gen-updates-xri.sh)** `<releaseDate YYYYMMDD>
   <versionRange> <title> <out> <meta...>` — emits a schema-valid
   `updates.xri` (namespace `http://www.pixinsight.com/xri`) with one
   `<platform os="…" arch="…">` block per `.meta` file passed in (macOS maps to
   PixInsight's `macosx` platform id), each wrapping a `<package
   type="module" fileName="…" sha1="…"/>`. CI validates the output with
   `xmllint --noout`.

3. **[`sign-and-publish.sh`](sign-and-publish.sh)** `[--dry-run] --stage-root
   <dir> --xri <file> [--out <dir>]` — the signing + publish driver. **Dormant
   until the `MMM_CPD_XSSK` environment variable is set**: without it, the
   script prints a `DORMANT: …` message and exits 0 without touching anything.
   With `MMM_CPD_XSSK` (+ `MMM_CPD_XSSK_PASSWORD`) set, it:
   1. finds every `mmm-pxm.{so,dll,dylib}` under `--stage-root` and signs each
      one with `PixInsight --automation-mode --sign-module-file=… --xssk-file=…
      --xssk-password=… --force-exit`, producing its `.xsgn`;
   2. signs the `--xri` file (see the
      [`--sign-xml-file` caveat](#the---sign-xml-file-caveat) below);
   3. if `--out` is given, copies the signed stage tree + `.xri` there for
      publishing (e.g. to a GitHub Pages checkout).

   **One PixInsight install signs every platform.** Module signing is a
   hash-and-sign over file bytes (SHA-512 + Ed25519); the signer only checks
   the file extension (`.so`/`.dylib`/`.dll`) and a `-pxm` basename — there's no
   host-OS branch or binary-format inspection. So a single Linux signer host,
   fed all three platforms' unsigned CI artifacts, can sign the whole matrix
   (and the `.xri`) in one headless pass with one `.xssk`. See the design
   spec's §6 for the primary-source evidence.

   `--dry-run` prints every command it would run (module signs + the `.xri`
   sign) without executing them or touching the filesystem — this is what CI
   and [`test/test-sign-and-publish.sh`](test/test-sign-and-publish.sh) use to
   exercise the dormant/gated/dry-run paths without a real `.xssk`.

## Going live (post-CPD)

Once the CPD certificate is in hand, switching this pipeline from dormant to
live is a config/secret change, not a rebuild:

1. Provide the CPD `.xssk` (+ its password) to whichever machine will act as
   the signer — a PixInsight-licensed, PixInsight-installed host (see the open
   verify item below about whether this can be a bare CI runner).
2. Set `MMM_CPD_XSSK` and `MMM_CPD_XSSK_PASSWORD` in that signer's environment
   (a CI secret if the signer is a self-hosted runner; a local env var if it's
   run by hand on a licensed machine).
3. Confirm the GitHub Pages hosting URL for the repository (currently assumed,
   not yet confirmed — see [spec §9](../../../docs/superpowers/specs/2026-07-28-pixinsight-ci-and-distribution-design.md#9-please-provide--decide-external-non-blocking)).
4. Run the pipeline: `gen-package.sh` for each platform → `gen-updates-xri.sh`
   → `sign-and-publish.sh --stage-root … --xri … --out <hosting-checkout>` →
   push/deploy the hosting checkout.
5. In PixInsight: **Resources → Updates → Manage Repositories…**, add the
   confirmed repository URL. From then on the module installs/updates through
   PixInsight's normal repository UI instead of the interim on-ramp above.

Resolve the [`--sign-xml-file` caveat](#the---sign-xml-file-caveat) as part of
this switch-on, before the first real (non-dry-run) publish.

## CPD action item

Applied for 2026-07-28; **awaiting Pleiades response**. Nothing in this
pipeline is blocked waiting on it — everything above is built and validated in
its dormant/dry-run form.

Steps to complete once a response arrives (or to (re-)initiate if needed):

1. Run the bundled **`SubmitCPD`** script (found alongside PixInsight's other
   signing scripts, e.g. `/opt/PixInsight/src/scripts/SubmitCPD/`): it uses the
   same `SigningKeys` machinery as the local-signing identity, but with a real
   **Developer id** instead of the local/license-bound option, generating a
   CPD-candidate `.xssk` and submitting its **public** key to Pleiades for
   certification.
2. Once Pleiades returns the certified identity, install it the same way a
   local identity is installed (**Script → Local/CPD Signing Identity…**, load
   the returned identity, make it persistent) so this signer host trusts and
   can sign with it.
3. Provide that `.xssk` to the pipeline per [Going live](#going-live-post-cpd)
   above.

## The `--sign-xml-file` caveat

`sign-and-publish.sh`'s `.xri`-signing step invokes
`PixInsight --automation-mode --sign-xml-file=<path> --xssk-file=… --xssk-password=… --force-exit`.
**That flag name is provisional, not confirmed.** PixInsight's own signing
scripts expose `.xri`/XML signing primarily through the in-app **CodeSign**
script (`Security.generateXMLSignature`); whether the same operation is also
reachable headlessly via a `--sign-xml-file` command-line switch — as opposed
to requiring the CodeSign script to be driven some other way in
`--automation-mode` — is **unverified**.

This is safe to leave unresolved today because the whole path is dormant:
`sign-and-publish.sh` never runs its real signing branch without
`MMM_CPD_XSSK` set, and
[`test/test-sign-and-publish.sh`](test/test-sign-and-publish.sh) only asserts
that *some* xri-signing command is emitted in `--dry-run` mode (it greps for
`codesign`/`--sign-xml`/`updates.xri`), not that the flag is the final one
PixInsight actually accepts.

**This must be settled when CPD signing is switched on** (first non-dry-run
run of `sign-and-publish.sh`): confirm the correct headless invocation against
a real PixInsight install + `.xssk`, and update the flag in
`sign-and-publish.sh` (and this note) if `--sign-xml-file` turns out to be
wrong — one place to change.
