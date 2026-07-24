# Mega Merge Mosaic (mmm)

Fast standalone merge/blend for pre-aligned astro mosaic panels (PixInsight
MosaicByCoordinates output). Read `docs/DESIGN.md` first — it records the
architecture, verified facts about the input data, and the algorithm choices.

## Layout

- `crates/mmm-core` — engine library (UI-agnostic; a GUI will reuse it)
- `crates/mmm` — CLI frontend (thin)
- `docs/DESIGN.md` — architecture spec; keep it current when design changes
- `docs/superpowers/plans/` — implementation plans
- `test_data/` — gitignored multi-GB real panels:
  - `orion_mosaic/` — 12 registered full-canvas panels (9255×18310×3 Float32
    XISF, ~2 GB each); panels 3,4,7,8 cover M42
  - `orion_mosaic_raw_panels/` — the same stacks before registration (own WCS)
  - `Orion_Mosaic_Plan.jpg` — panel layout diagram

## Commands

```sh
cargo test                 # unit + integration tests (no big files needed)
cargo build --release      # required for real-data runs (2 GB frames)
target/release/mmm info test_data/orion_mosaic/*PANEL-4_*.xisf --stats
```

Rust via rustup (`source ~/.cargo/env` if PATH lacks cargo).

## Conventions

- Linear data end-to-end; zero = no-data sentinel (all channels zero).
- Never process dense over the canvas — panels cover ~9% each; work in
  overlap bands / sparse structures.
- Per-channel math for OSC; seams (phase 2) shared across channels.
- Session dirs (`*.mmm-session/`) hold all cached analysis; stages re-runnable.
- Tests must not depend on `test_data/` (synthesize inputs); real-data runs
  are manual smoke tests.
