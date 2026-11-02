# Mega Merge Mosaic (mmm)

Fast standalone merge/blend for pre-aligned astro mosaic panels (PixInsight
MosaicByCoordinates output). Read `docs/DESIGN.md` first — it records the
architecture, verified facts about the input data, and the algorithm choices.

## Layout

- `crates/mmm-core` — engine library (UI-agnostic; a GUI will reuse it)
- `../mega-pixel-processor` (sibling checkout, `mpp-core`) — all XISF/FITS
  reading and writing; mmm pins it as a git dependency by `rev`
  (`default-features = false`); a gitignored `.cargo/config.toml` `[patch]`
  points it at the sibling checkout (DESIGN.md "Dependency on mpp-core")
- `crates/mmm` — CLI frontend (thin)
- `crates/mega-ui` — generic egui components/theme/tile viewer; must not
  depend on `mmm-core` (liftable to other apps); egui/eframe pinned at 0.36
- `crates/mmm-app` — eframe desktop app (`mega-merge-mosaic`), glow renderer
- `integration/pixinsight/` — PCL module + `mmm-ipc-worker` host. **Development
  eased off (2026-11-02), shelving expected** under PixInsight CPD rules 1.0.1;
  no new feature work here (DESIGN.md "PixInsight module status")
- `docs/DESIGN.md` — architecture spec; keep it current when design changes
- `docs/superpowers/plans/` — implementation plans
- `test_data/` — gitignored multi-GB real panels:
  - `orion_mosaic/` — 12 registered full-canvas panels (9255×18310×3 Float32
    XISF, ~2 GB each); panels 3,4,7,8 cover M42
  - `orion_mosaic_raw_panels/` — the same stacks before registration (own WCS,
    legacy 1.9.4 `PCL:AstrometricSolution:*` block)
  - `orion_mosaic_raw_panels_195/` — the raw panels re-saved by PixInsight
    1.9.5 (XISF rev 1 `AstrometricSolution:*` standard block)
  - `Orion_Mosaic_Plan.jpg` — panel layout diagram

## Commands

```sh
cargo test                 # unit + integration tests (no big files needed)
cargo build --release      # required for real-data runs (2 GB frames)
target/release/mmm info test_data/orion_mosaic/*PANEL-4_*.xisf --stats
```

GUI runs: launch the app only via `--selftest` (it closes itself; never
kill a GUI on a timer). Selftest session: `cargo run -p mmm-app --example
make_selftest_session -- DIR`. Under WSL the app sets `GALLIUM_DRIVER=d3d12`.

Rust via rustup (`source ~/.cargo/env` if PATH lacks cargo).

## Conventions

- Linear data end-to-end; zero = no-data sentinel (all channels zero).
- Never process dense over the canvas — panels cover ~9% each; work in
  overlap bands / sparse structures.
- Per-channel math for OSC; seams (phase 2) shared across channels.
- Session dirs (`*.mmm-session/`) hold all cached analysis; stages re-runnable.
- Tests must not depend on `test_data/` (synthesize inputs); real-data runs
  are manual smoke tests. `blend.rs`'s unit tests live in the sibling
  `blend_tests.rs` (a `#[path]` child module).
- Every public item in `mmm-core` needs a doc comment (`missing_docs` is
  warned); keep `cargo fmt`, `cargo clippy --all-targets`, and `cargo doc`
  warning-free. `lib.rs` documents the intended public API surface.
