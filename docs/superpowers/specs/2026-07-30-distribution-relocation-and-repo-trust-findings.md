# PixInsight Distribution — Repository Trust Findings & Relocation Decision

Status: **approved 2026-07-30** (brainstorming). This is a **decision record**,
not an implementation spec: the actual repository/website build moves to a
**separate, private "Astrometrical" website repository** (see §5). This document
captures what was investigated and decided in MergeMosaic so that (a) the new
project starts from solid ground, and (b) the dormant pipeline still living in
this repo is not mistaken for something to finish here.

Extends / partially supersedes
[`2026-07-28-pixinsight-ci-and-distribution-design.md`](2026-07-28-pixinsight-ci-and-distribution-design.md)
(the dormant repository pipeline) and the distribution parts of
[`2026-07-27-pixinsight-integration-design.md`](2026-07-27-pixinsight-integration-design.md) §12.

## 1. Goal of the phase (as framed)

Stand up a **real, hostable PixInsight update repository** that distributes the
current three-platform module **signed with the local signing identity**
(`~/astrometrical.xssk`), as a preparatory step **before the Pleiades
Certified-Developer (CPD) certificate arrives**. It only needs to be trusted on
the maintainer's own machines, and the local→CPD transition must be minimal
(ideally: point the sign step at a different key).

## 2. Critical finding — the repository trust model (RESOLVED, empirically)

**A repository signed with the local signing identity is accepted on the
maintainer's own machine. No CPD is required for this phase.** PixInsight signs
at two independent layers; both are satisfied by the local identity on a
licensed machine.

### 2.1 The `.xri` signing mechanism is identity-agnostic

- `--sign-xml-file=<file>` is a **real, documented core command-line flag**
  (verified in `PixInsight --help` on the local 1.9.4 install). This **retires
  the "`--sign-xml-file` caveat"** the dormant `repo/README.md` flagged as
  provisional — the flag name is correct.
- `/opt/PixInsight/src/scripts/CodeSign/CodeSignMain.js`'s `.xri` branch calls
  `Security.generateXMLSignature(filePath, keys.developerId, keys.publicKey,
  keys.privateKey)` using **whatever identity is in the `.xssk`**. There is **no
  CPD-specific code path** — local and CPD keys sign identically.

### 2.2 Empirical proof on the licensed PI (Linux/WSL, core 1.9.4 "Lockhart")

Signing a real generated `updates.xri` with the local key succeeded:

```
$ PixInsight --automation-mode -n \
    --sign-xml-file=<updates.xri> \
    --xssk-file=~/astrometrical.xssk --xssk-password='***' --force-exit
Secure signing keys file loaded: /home/dpaull/astrometrical.xssk
Developer id: 5845079579265752
1 XML signature(s) generated.        # exit 0
```

It appended a valid top-level signature element **after** `</xri>`:

```xml
<Signature developerId="5845079579265752" timestamp="2026-07-29T23:58:08.630Z"
           encoding="Base64">3WiBqKzW0Un…EdgaCg==</Signature>
```

That is exactly the repository-signature format PixInsight validates.

### 2.3 The local core is already configured to trust it

From `~/.PixInsight/core-001-pxi.settings`:

- `EnableLocalSigningIdentity = true`, with an installed
  `LocalSigningIdentity/PublicKey` → the local identity is trusted (this is
  **why the module `.xsgn` loads**), and the same trust store validates the
  `.xri` signature.
- `AllowUnsignedRepositories = true` → even an unsigned repo would be *addable*
  on this machine (a fallback, not the plan).
- `AllowInsecureRepositories = false` → repositories should be served over
  **HTTPS** (Cloudflare Pages satisfies this by default).
- `Repositories` is a plain list of URLs — nothing is certificate-gated at the
  storage layer; a repository is added by URL.

### 2.4 The two signing layers, settled

| Layer | Requirement | Status on our machines |
|---|---|---|
| Module `.so`/`.dll`/`.dylib` → `.xsgn` | Must be signed; local identity trusted where a license is held | ✅ Already validated (manual install, Linux + Windows). A repository install lays down the **same** `bin/` files + `.xsgn`, so trust is identical. |
| Repository `updates.xri` | Signed by a trusted identity (or unsigned if `AllowUnsignedRepositories`) | ✅ Local identity signs it (proven §2.2); trusted via the installed public key (§2.3). |

**Documentation corroboration** (PixInsight docs + a third-party analysis): a
local signing identity is explicitly sanctioned *"if this repository is only for
your own local testing"*; CPD is required **only** so *other people's* machines
trust the repository. Our requirement — trusted on our own machines — is exactly
the local-identity case.

## 3. Second finding — no standalone / cloud signer exists

- Signing happens **only** via the `PixInsight` binary (`--sign-module-file` /
  `--sign-xml-file`), which requires an **activated license**, which is
  **machine-bound**. There is **no standalone offline signer** and **no remote
  PixInsight signing service**.
- The "cloud code-signing" services found in research (SSL.com et al.) produce
  **Authenticode / notarization** signatures — they **cannot** produce
  PixInsight's proprietary SHA-512 + Ed25519 module/`.xri` signatures.
- **Consequence:** installing PixInsight on an ephemeral cloud CI runner to sign
  is blocked at license activation. Signing must run on a machine that holds the
  license — i.e. a **manual local step** or a **self-hosted runner**. We chose
  manual (§4).

## 4. Decisions locked in brainstorming (2026-07-30)

| # | Decision | Rationale |
|---|---|---|
| Hosting | **Cloudflare Pages** (final target from the start). | The maintainer named it as the permanent home; standing it up now keeps the repository URL added in PixInsight **stable** — no later migration. HTTPS by default (satisfies §2.3). |
| Platforms | **Publish all three** (`linux-x64`, `windows-x64`, `macos-arm64`); **GUI-validate Linux/WSL + Windows** only. | All three build green in CI and all three sign from one Linux host (confirmed cross-platform sign). No Mac is available to validate, so macOS ships *present but unvalidated* — it "just appears" once a Mac exists. |
| Where signing runs | **Manual local `sign+publish` step** on the maintainer's licensed Linux/WSL box. **No self-hosted runner, no custom signing daemon** this phase. | §3: cloud signing is impossible; a manual step is robust, needs zero standing infra, and keeps the key entirely on the maintainer's box. Releases are rare, so the manual cost is low. |
| CPD swap path | **Single documented key input** (an env var, e.g. `MMM_XSSK` / `MMM_XSSK_PASSWORD`), defaulting to the local key. | Local→CPD becomes "point the sign step at a different `.xssk`" — one path change, no rebuild, stable URL. |
| **Structure** | **Relocate distribution to a separate, private "Astrometrical" website repository** (see §5). MergeMosaic keeps only engine + CI + the dev-sign flow. | Cleanly separates the engine (this repo) from distribution/website (private); lets one deploy ship both a feature announcement and the updated signed module; scales as "mmm is one of many tools." |

## 5. Relocation decision — the "Astrometrical" website repository

**The PixInsight update repository and its publishing pipeline move OUT of
MergeMosaic into a new, private website repository.** That repo:

1. Is the website for Astrometrical processing tools (mmm is the first of
   several), deployed via **Cloudflare Pages**.
2. **Includes the PixInsight update repository as static files** (`updates.xri`
   + per-platform `.tar.gz` packages) served at a stable URL.
3. Ships a release as **one deploy**: website updates (feature announcements) +
   the newly signed module package + regenerated `updates.xri`. A script
   automates **signing + assembling the update repository from a downloaded CI
   build bundle**.
4. Can offer a **beta repository** — a second `updates.xri` under `/beta/` that
   testers add as a separate repository URL (no version-range hacks).

**Why separate:** keeps mmm cleanly decoupled from the website; the website is
private; distribution concerns (Cloudflare, signing keys, announcements) do not
belong in the public engine repo.

### 5.1 What stays in MergeMosaic vs. relocates

- **Stays here:** `.github/workflows/module.yml` (builds all three platforms,
  uploads **unsigned** per-platform bundles); the module's **local dev-sign
  flow** (`make sign`) for manual dev installs; the specs.
- **Relocates to the website repo:** package assembly, `updates.xri`
  generation, the **sign + publish** driver, and Cloudflare deployment. The
  existing `integration/pixinsight/repo/` scripts (`gen-package.sh`,
  `gen-updates-xri.sh`, `sign-and-publish.sh`, `test/`) are a working, tested
  **starting point to lift into the new project**.
- **Decision — keep and annotate:** the `repo/` scripts stay in place, annotated
  as *relocating* (not deleted), so the tested starting point isn't lost before
  the new repo exists. They are revisited/removed once the website is up.

## 6. The bundle contract (the one interface between the two repos)

MergeMosaic's CI is the producer; the website repo's publish script is the
consumer. Pin this contract so the two evolve independently:

- **Producer:** `module.yml` uploads one artifact per platform —
  `mmm-pxm-linux-x64`, `mmm-pxm-windows-x64`, `mmm-pxm-macos-arm64`.
- **Layout (per artifact):** `stage/bin/mmm-pxm.{so,dll,dylib}` +
  `stage/bin/mmm-ipc-worker[.exe]` (the worker sits beside the module, as
  `dladdr` sibling-resolution expects).
- **Provenance:** the consumer records, per published release, the **MergeMosaic
  git SHA and CI run id** the binaries came from (traceability).
- **Version string:** the module/core version range published in `updates.xri`
  (`1.9.4:1.9.4` today) — sourced from the target core, not the bundle.

*(Optional producer improvement, deferred to the new project's discretion: a
dependent CI job that gathers the three artifacts into a single
`mmm-pxm-repo-unsigned` bundle so the manual download is one file, not three.)*

## 7. Sign + publish flow (for the new project) — with a correctness fix

The current dormant `sign-and-publish.sh` **packages before signing**, so the
`.xsgn` is not inside the tarball and the `updates.xri` `sha1` is stale. The new
project must **sign first, then package**:

1. **Sign** each `mmm-pxm.{so,dll,dylib}` → `.xsgn` with the one `--xssk-file`
   (`MMM_XSSK`, password from `MMM_XSSK_PASSWORD`); one Linux host signs all
   three platforms in one pass.
2. **Package** each platform: `tar.gz` overlay containing
   `bin/{mmm-pxm.*, mmm-pxm.xsgn, mmm-ipc-worker[.exe]}` — `.xsgn` **inside** the
   archive.
3. **Hash** each archive (`sha1`).
4. **Generate** `updates.xri` (all three platforms, correct `sha1`s).
5. **Sign** `updates.xri` (`--sign-xml-file`, same key).
6. **Assemble** the deploy tree (website + `updates.xri` + packages) and
   **deploy to Cloudflare Pages**.

### 7.1 Publishing mechanism — keep binaries out of git

The Windows `.dll` links the full PCL closure and is not small; three platforms
× every release kept in git history would bloat a long-lived repo. **Recommended:
Wrangler direct upload** — the publish script assembles the full deploy tree and
`wrangler pages deploy`s it; git holds website source + the text `updates.xri` +
a small manifest, **not** the binaries. (If Git-integrated Pages is preferred for
simplicity, use **Git LFS** for the packages.)

## 8. End-to-end validation plan (for the new project)

- **Linux/WSL:** add the Cloudflare Pages repository URL in PixInsight
  (Resources → Updates → Manage Repositories) → it fetches `updates.xri` →
  validates the local-identity signature (trusted) → offers `linux-x64` →
  install → module loads (`.xsgn` trusted) → **MosaicMerge** appears. Then bump
  the version, re-publish, and confirm **auto-update** detects + installs it.
- **Windows** (licensed PI available): **one-time**, install the same
  `astrometrical.xssk` public key on the Windows core (**Script → Local Signing
  Identity…**, make persistent) so it trusts the identity. Then add the same URL
  → install `windows-x64` → module loads → MosaicMerge. This validates the
  `.dll` path end-to-end via a repository.
- **macOS:** published but **unvalidated** (no Mac). Known caveat: a
  repository-**downloaded** macOS worker carries `com.apple.quarantine`, so
  Gatekeeper may block the worker `exec` until `mmm-ipc-worker` is **notarized**
  (the deferred worker-signing item — a source build, incl. CI, is unaffected).
  Windows: an unsigned downloaded `.exe` spawned by PixInsight runs (SmartScreen
  only warns on *download* of the package).

## 9. CPD swap (unchanged, still one input)

When the CPD `.xssk` arrives: point the publish script's `MMM_XSSK` /
`MMM_XSSK_PASSWORD` at the CPD identity instead of `~/astrometrical.xssk`, and
re-publish. Same scripts, same flags, **same repository URL**. A CPD-signed repo
is trusted by *everyone*; the local-signed repo is trusted only on our own
machines (§2, §10).

## 10. Known caveats to document for testers

- **Trust scope:** a local-identity-signed repo on a public URL is
  world-*reachable* but **only trusted on the maintainer's own machines**. Other
  people's PixInsight will flag developer id `5845079579265752` as untrusted
  until re-signed with CPD. Expected — not a bug.
- **Repository URL convention:** confirm against the PixInsight Repository
  Reference whether the added URL is the **base directory** containing
  `updates.xri` (package `fileName`s resolved relative to it) vs. an explicit
  path, when standing up Cloudflare Pages in the new project.
- **PCL ABI:** the module is built against a specific core's PCL/SDK; a core
  update requires a rebuild + re-sign (the worker is ABI-independent).

## 11. Please provide / decide (in the new project)

- Cloudflare **account + Pages project name + custom domain** (or accept the
  `*.pages.dev` URL) — the stable repository URL.
- **Publish mechanism:** Wrangler direct upload (recommended, §7.1) vs.
  Git-integrated Pages + Git LFS.
- **CPD certificate** — still pending from Pleiades (applied 2026-07-28); flips
  trust from "our machines only" to "everyone" via the §9 key swap.

## Sources

- Local empirical evidence: `/opt/PixInsight` (core 1.9.4 "Lockhart"),
  `PixInsight --help`, `src/scripts/CodeSign/CodeSignMain.js`,
  `~/.PixInsight/core-001-pxi.settings`, and a real `--sign-xml-file` run with
  `~/astrometrical.xssk` (developer id `5845079579265752`).
- [PixInsight Update Repositories reference](https://pixinsight.com/doc/docs/PIRepositoryReference/PIRepositoryReference.html)
- [The PixInsight Script Code Signing System](https://pixinsight.com/doc/docs/ScriptCodeSigning/ScriptCodeSigning.html)
- [Third-party repository signing analysis (SIGNING_PLAN.md)](https://github.com/cosgrovescosmos/astro-color-mixer-pixinsight/blob/main/SIGNING_PLAN.md)
</content>
</invoke>
