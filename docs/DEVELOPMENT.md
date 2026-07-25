# Developing mmm

Contributor guide: repo layout, build/test workflow, testing philosophy, and
the current open-issues list. For the architecture, algorithms, and the
verified facts about real input data, read [DESIGN.md](DESIGN.md) first — it
is the internal source of truth and is kept current as the design changes.
CI and the release process are documented in [RELEASING.md](RELEASING.md).

## Repo layout

```
crates/mmm-core/     engine library (UI-agnostic; a future GUI reuses it)
  src/               pipeline stages as public modules — see the API map
                     in src/lib.rs
  tests/             integration tests (e2e, photometry, regression guard)
crates/mmm/          CLI frontend (thin: arg parsing + table printing only)
docs/DESIGN.md       architecture spec + per-phase results and saga notes
docs/RELEASING.md    GitHub publishing, CI, and binary-release guide
docs/superpowers/plans/  historical implementation plans (frozen)
test_data/           gitignored multi-GB real panels (see below); optional
```

## Build & test

Rust via [rustup](https://rustup.rs) (`source ~/.cargo/env` if PATH lacks
cargo).

```sh
cargo test                  # full suite, no big files needed (~seconds)
cargo build --release       # required for real-data runs (2 GB frames)
cargo fmt --all --check     # CI enforces formatting
cargo clippy --all-targets  # CI runs with -D warnings on Linux/Windows/macOS
```

`mmm-core` has `#![warn(missing_docs)]` — every public item needs a doc
comment, and CI's clippy `-D warnings` turns lint debt into a red build.
Run all three commands before pushing.

## Testing philosophy

**Tests never touch `test_data/`.** CI machines don't have it, and multi-GB
fixtures don't belong in tests. Everything a test needs is synthesized.

- **Synthetic ground-truth harness** (`mmm_core::synth`): generates a known
  sky (gradient background + Gaussian stars with spikes + noise + optional
  defects and per-panel misregistration), cuts it into overlapping panels
  with per-panel gain/offset/gradient perturbations, and writes real XISF
  files. End-to-end tests run the full pipeline on these and assert
  recovered gains and RMSE against the noiseless truth — plus targeted
  guarantees (stars match exactly ONE panel under misregistration, spikes
  never kink, defects vetoed, deep single-coverage untouched). `synth` is
  public so downstream tools and benchmarks can use it too.

- **Byte-exact regression guard**
  (`crates/mmm-core/tests/regression_guard.rs`): a deterministic synthetic
  session is analyzed and blended, and FNV-1a hashes of every analysis
  artifact and of the blended row streams are asserted against captured
  constants. Any byte of drift in the aligned pipeline fails the build.

  **Legitimate hash change?** If a change is *supposed* to alter output
  (an algorithm fix or improvement): (1) confirm every other test — and,
  for behaviour changes, a real-data smoke run — supports the change;
  (2) run the guard with `--nocapture` and copy the printed
  `META_HASH:`/`BLEND_*_HASH:` values into the constants; (3) append a
  bullet to the doc comment above the constant saying *what* changed the
  bytes and *why*, including the previous value — the comment there is the
  audit trail (see its existing entries for the expected form). Never
  recapture to silence a failure you can't explain.

- **Real-data tests are `#[ignore]`d** and act as manual smoke tests. They
  need the gitignored `test_data/` sets: `orion_mosaic/` (12 registered
  full-canvas panels, ~2 GB each) and `orion_mosaic_raw_panels/` (the same
  stacks before registration, with PixInsight astrometric solutions). Run
  them explicitly, in release mode:

  ```sh
  cargo test -p mmm-core --release -- --ignored --nocapture
  ```

  They cover the WCS model against real PixInsight solutions, real
  reprojection, and the raw-vs-registered acceptance comparison. Don't add
  a non-ignored test that touches `test_data/` — a plausible-looking path
  dependency is the most common way CI breaks (see RELEASING.md).

## Development history

Each phase is recorded in full (numbers, deviations, post-mortems) in
[DESIGN.md](DESIGN.md); the short version:

| Phase | What landed |
|---|---|
| 1 (POC) | Streaming XISF ingest via mmap, 1/8-scale summaries, overlap graph, robust per-edge photometric fits + global gain/offset solve, feather blend, streamed FITS output. Validated on the real 12-panel Orion mosaic. |
| 2 | Signal-protected residual surface correction; two-band blend with star-avoiding seams (hard star ownership); WCS passthrough from XISF properties; `--roi`. |
| 3 | Connected star masks (diffraction-spike-safe seams); cross-panel defect veto; seam-map PNG + per-edge seam Δ diagnostics; opt-in `--flatten`. |
| 4 | Laplacian-pyramid base blend (new default mode); WCS north–south mirror resolved (see the saga in DESIGN.md); seam-through-structure and dark-streak fixes. |
| 5 | Unaligned plate-solved input: full PixInsight WCS model incl. spline distortion grids, automatic mosaic-frame choice + Lanczos-3 reprojection, `--input` auto-detect. Raw-input output matches the registered-input result to 0.030 px median. |

Current: 140 tests, clippy clean.

## Conventions

- **Linear data end-to-end**; zero = no-data sentinel (a pixel is covered
  iff all channels are nonzero).
- **Never process dense over the canvas** — panels cover a few percent
  each; work in overlap bands / sparse L8-grid structures.
- **Per-channel math for OSC**; seams are shared across channels (single
  owner map) to avoid colour fringing.
- **Session dirs** (`*.mmm-session/`) hold all cached analysis; every stage
  is individually re-runnable and `blend` never re-analyzes.
- `mmm-core` stays UI-agnostic; the CLI is a thin frontend so a GUI can
  reuse everything.
- Keep [DESIGN.md](DESIGN.md) current when the design changes.

## Open issues

Seed list at open-sourcing time (file GitHub issues as these are picked up):

- **PixInsight annotation mirror.** Some PixInsight annotation runs mirror
  north–south on mmm output even though the WCS cards are catalog-star
  verified (see the "WCS north–south mirror" saga in DESIGN.md).
  `--wcs-frame flipped` is the workaround. Needs a PixInsight-side A/B test
  (same file, both conventions, annotation screenshots) to pin down the
  reader behaviour across PI versions.
- **`Error` enum needs a non-file variant before 1.0.** Both variants carry
  a `PathBuf`; validation errors with no natural file currently borrow a
  context path (`crates/mmm-core/src/error.rs`).
- **FITS input not yet supported** — XISF only (FITS is output-only). The
  `mmm info` help string still says "FITS/XISF"; fix alongside.
- **Compressed XISF input not supported** — the reader mmaps the
  uncompressed monolithic attachment directly; compressed inputs will need
  a one-time decompress into a tile cache.
- **SampleFormat: Float32 only** — UInt16/Float64 XISF inputs are rejected.
- **GPU (wgpu) path** — planned; CPU/rayon only today.
- **GUI** — planned; `mmm-core` is structured for it (see the API map in
  `crates/mmm-core/src/lib.rs`).
