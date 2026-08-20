# Releasing mmm

A from-zero guide to publishing this repository on GitHub and cutting binary
releases with GitHub Actions. No prior Actions experience assumed. (For the
day-to-day build/test workflow CI enforces, see
[DEVELOPMENT.md](DEVELOPMENT.md).)

Two workflow files in `.github/workflows/` do all the work:

| File          | Trigger                          | What it does |
|---------------|----------------------------------|--------------|
| `ci.yml`      | every push / PR to `main`        | `cargo fmt --check`, `clippy -D warnings`, `cargo test` on Linux, Windows, and macOS |
| `release.yml` | pushing a tag starting with `v`  | builds the `mmm` binary for 4 targets, packages archives + `SHA256SUMS`, publishes a GitHub Release |

## 1. One-time setup: publish the repository

1. On <https://github.com/new>, create an empty repository (no README, no
   license — this repo already has them) named `mega-merge-mosaic` under the
   `Astrometrical` organisation.

2. Point this local repo at it and push:

   ```sh
   git remote add origin https://github.com/Astrometrical/mega-merge-mosaic.git
   git push -u origin main
   ```

   (If a remote named `origin` already exists, use
   `git remote set-url origin …` instead of `add`.)

3. The `repository` field in the root `Cargo.toml` (`[workspace.package]`)
   already points at this URL. It is metadata only, but it is what crates.io
   and tooling display, so keep it correct if the repo ever moves.

That is all. **GitHub Actions needs no enabling** — the moment `main` (with
`.github/workflows/` in it) lands on GitHub, the CI workflow runs
automatically on that push and on every future push and pull request.

## 2. Reading CI results

- The **Actions** tab of the repository lists every workflow run. Click a run,
  then a job (e.g. `test (ubuntu-latest)`), then a step to see its log.
- A red X on a commit or PR means some step exited non-zero; the failing step
  is highlighted in the job view. Fix, push again — a new run starts (and for
  PRs, the superseded run is auto-cancelled).
- On the very first push, watch the run to completion on all three platforms.
  Nothing in these workflows can be executed locally, so the first push *is*
  the validation run — see the checklist in the Troubleshooting section.

**Suggested: required checks.** Once CI is green, consider protecting `main`:
Settings → Branches (or Rules) → add a ruleset/branch protection rule for
`main` requiring the `test (...)` status checks to pass before merging. This
makes it impossible to merge a PR that fails CI. Optional, but cheap insurance
once collaborators or PRs appear.

## 3. Cutting a release, start to finish

Releases are driven entirely by git tags of the form `vX.Y.Z`.

1. **(Optional) bump the version.** Edit `version` under `[workspace.package]`
   in the root `Cargo.toml`, run `cargo build` once so `Cargo.lock` picks it
   up, and bump the same number in the three files that must stay in exact
   sync with it (enforced by `crates/mmm-ipc-worker/tests/version_sync.rs`,
   so CI goes red if one is missed):

   - `integration/pixinsight/module/MmmVersion.h` (`MMM_VERSION_*` and
     `MMM_VERSION_STRING`)
   - `integration/pixinsight/host/mmm_protocol.h` (`kExpectedWorkerVersion`)
   - `integration/pixinsight/doc/tools/MegaMergeMosaic/MegaMergeMosaic.html`
     (the "Version X.Y.Z" subtitle line)

   Then commit:

   ```sh
   git commit -am "chore: bump version to 0.1.0"
   git push
   ```

   Wait for CI to go green on that commit — never tag a red commit.

2. **Tag and push the tag:**

   ```sh
   git tag v0.1.0
   git push origin v0.1.0
   ```

   Pushing the tag is what triggers `release.yml`. (A plain `git push` does
   *not* push tags.)

3. **Watch the workflow** in the Actions tab (run named "Release"). Four
   `build (...)` jobs compile and package in parallel (~5–15 min each on a
   cold cache), then `publish release` collects the archives, writes
   `SHA256SUMS`, and creates the GitHub Release.

4. **Polish the notes.** The release is published immediately (not a draft)
   with auto-generated notes (the commit/PR list since the previous tag). Go
   to the repository's **Releases** page → the new release → **Edit** to
   reword the notes or add highlights. The attached assets are:

   - `mmm-v0.1.0-x86_64-linux.tar.gz`
   - `mmm-v0.1.0-x86_64-windows.zip`
   - `mmm-v0.1.0-x86_64-macos.tar.gz`
   - `mmm-v0.1.0-aarch64-macos.tar.gz`
   - `SHA256SUMS`

   Each archive contains the `mmm` binary, `README.md`, `LICENSE`, and
   `NOTICE` (the Apache-2.0 attribution notice).

## 4. Fixing a botched release

If a release built from the wrong commit, or the workflow failed halfway:

1. Delete the GitHub Release (Releases page → the release → Delete), if it
   was created.
2. Delete the tag both remotely and locally:

   ```sh
   git push --delete origin v0.1.0
   git tag -d v0.1.0
   ```

3. Fix whatever was wrong, then retag the correct commit and push the tag
   again (step 3.2 above). The workflow reruns from scratch.

Re-releasing the *same* version number is fine before anyone has downloaded
it; after that, prefer bumping to the next patch version instead.

If only the *workflow* failed (e.g. a flaky runner) and the tag itself is
fine, open the failed run in the Actions tab and use **Re-run failed jobs** —
no retagging needed.

## 5. Costs

GitHub Actions is **free with unlimited minutes for public repositories**,
including the macOS and Windows runners used here. (Private repos get a
limited free monthly quota where macOS minutes count 10x; this only matters
if the repo stays private.)

## 6. Troubleshooting

First-push checklist — the workflows cannot be tested before they reach
GitHub, so on the first push verify, in the Actions tab:

- [ ] The "CI" run appears at all (if not: files must be at exactly
      `.github/workflows/*.yml` on the default branch).
- [ ] All three `test (...)` jobs go green.
- [ ] After the first tag: all four `build (...)` jobs and `publish release`
      go green, and the Release page shows 5 assets.

Common first-run failures:

- **`cargo fmt --all --check` fails.** CI enforces formatting that local
  builds do not. Fix with `cargo fmt --all`, commit, push. Run
  `cargo fmt --all --check` locally before pushing to catch this early.

- **Clippy fails on a platform you did not develop on.** CI treats every
  clippy warning as an error (`-D warnings`) on Linux, Windows, *and* macOS —
  a lint can fire on one platform only (e.g. around path or size types).
  Read the job log for the exact lint name; fix the code, or in a justified
  case add a targeted `#[allow(lint_name)]` with a comment.

- **Release job fails with HTTP 403 / "Resource not accessible by
  integration".** Creating a Release needs write permission. `release.yml`
  already sets this at the top:

  ```yaml
  permissions:
    contents: write
  ```

  If it still 403s, check Settings → Actions → General → "Workflow
  permissions" — restrictive org/repo defaults are fine *as long as* the
  workflow-level `permissions:` block above is present, so make sure it was
  not removed.

- **A macOS build job dies with "runner label unknown" or queues forever.**
  GitHub retires old macOS images over time (this repo already uses
  `macos-15-intel` because `macos-13` was retired). Check
  <https://github.com/actions/runner-images> for current labels. If Intel
  macOS runners disappear entirely, change that matrix entry's `runner:` to
  `macos-latest` and keep `target: x86_64-apple-darwin` — Apple toolchains
  build Intel binaries on arm64 without extra setup.

- **Tests pass locally but fail in CI.** CI machines have no `test_data/`;
  tests must stay self-contained (multi-GB tests remain `#[ignore]`d). A CI
  failure here usually means a test grew an accidental dependency on local
  files or paths.

- **Editing workflows.** After changing a file under `.github/workflows/`,
  lint it locally with [actionlint](https://github.com/rhysd/actionlint)
  (`docker run --rm -v "$PWD:/repo" -w /repo rhysd/actionlint:latest`) before
  pushing — it catches label typos, bad `${{ }}` expressions, and shell
  mistakes that YAML validation alone misses.
