# Mega Merge Mosaic (mmm)

Fast, standalone merging of astrophotography mosaic panels into a seamless
mosaic — automatically, in seconds, and without pinched or doubled stars.

`mmm` takes linear XISF panels — either the registered full-canvas frames
produced by PixInsight's **MosaicByCoordinates**, or your raw plate-solved
panel stacks directly — and produces a photometrically matched, seam-blended
32-bit FITS mosaic with the astrometric solution carried into the output.

<!-- TODO: screenshot — full Orion 12-panel mosaic result, with a seam-map inset -->

```sh
cargo build --release
target/release/mmm analyze panels/*.xisf --session orion.mmm-session
target/release/mmm blend --session orion.mmm-session -o mosaic.fits
```

That's it: `analyze` detects whether the panels are pre-registered or raw
solved stacks (reprojecting the latter onto a fresh mosaic frame
automatically), and `blend` streams out the merged FITS. On a 12-panel
mosaic this takes seconds, not minutes ([numbers below](#performance)).

## Why

The existing options each fall short somewhere. GradientMergeMosaic averages
overlap regions, so sub-pixel misregistration pinches and doubles stars, and
gradient mismatches in signal-dominated overlaps can "dig holes" in
nebulosity. PhotometricMosaic produces excellent results but needs
per-overlap manual tuning and is slow on large mosaics. General panorama
stitchers assume stretched, nonlinear images.

`mmm` exploits the structure of the astro-mosaic problem — panels share one
projection and each covers a small fraction of the canvas — so all expensive
work happens in the overlap bands, never across the whole canvas, and the
things that must never be averaged (stars) never are.

## Features

- **Global photometric solve** — robust per-overlap linear fits plus a global
  per-panel gain/offset adjustment, per channel; panel background steps of 2×
  and more are absorbed automatically.
- **Signal-protected residual surfaces** — low-order per-panel corrections
  fitted to *background only*, globally consistent around loops; structurally
  prevented from absorbing real signal differences (no nebula hole-digging).
- **Star-avoiding seams with hard star ownership** — every star (including
  its diffraction spikes) comes from exactly one panel, so misregistration
  cannot pinch or double it; seams detour around stars and structure.
- **Laplacian-pyramid multiband base** — the star-free background blends at
  seam-transition widths matched to each spatial scale (Burt–Adelson), on a
  1/8-scale grid so it costs almost nothing.
- **Cross-panel defect veto** — cosmic-ray residue and satellite trails that
  survive in only one panel's overlap are suppressed, not shown full-strength.
- **Opt-in global background flatten** (`--flatten 1|2`) — halves varying
  background casts, provably cannot dig holes in nebulosity, and refuses to
  run on signal-dominated mosaics.
- **Unaligned input** — raw plate-solved panels are reprojected (Lanczos-3,
  including PixInsight spline distortion grids) onto a self-chosen mosaic
  frame; matches the MosaicByCoordinates-based result to 0.030 px median on
  real data.
- **Diagnostics** — seam/ownership map PNG, per-edge seam Δ report, per-edge
  photometric fit table with outlier warnings.
- **WCS in the output** — from the XISF astrometric solution (aligned input)
  or the self-chosen mosaic frame (solved input), catalog-star verified.
- **Fast and out-of-core** — mmap + streaming row bands; canvas size is
  bounded by disk, not RAM. CPU-parallel via rayon.
- **Validated** — 140 tests including synthetic ground-truth end-to-end runs
  and byte-exact regression guards; every quality mechanism above was
  user-validated on real mosaic data.

## How it works

**Sparse, overlap-band processing.** `analyze` streams each panel once,
building a 1/8-scale summary (per-channel means + coverage per 8×8 block),
coverage masks, and an overlap graph of panel pairs. Panels cover only a few
percent of the canvas each, so everything downstream — photometric fitting,
seam optimization, blend masks — runs on these small sparse structures;
full-resolution pixels are only touched in a single streaming blend pass.

**Photometric solve + residual surfaces.** Per overlap and channel, a robust
linear fit relates the two panels; a global least-squares solve then assigns
each panel one gain and offset per channel so all overlaps agree at once.
Remaining low-order mismatch is absorbed by per-panel polynomial surfaces
fitted to background cells only — guard-railed (robust clipping, ridge,
magnitude caps) so real signal differences can never steer them.

**Star-safe seam blending.** Each panel splits into a smooth star-free base
and a detail band holding all stars and fine structure. The base is blended
as a Laplacian pyramid across seams; the detail band is *owned* — a seam path
optimized through each overlap gives every pixel's detail to exactly one
panel, snapping hard at stars and ramping smoothly across extended structure.
Averaging never touches a star. Full details, results, and design history are
in [docs/DESIGN.md](docs/DESIGN.md).

## Install / build

From source (Rust via [rustup](https://rustup.rs)):

```sh
git clone https://github.com/Astrometrical/mega-merge-mosaic.git
cd mega-merge-mosaic
cargo build --release        # release build strongly recommended: inputs are GBs
target/release/mmm --help
```

Prebuilt binaries for Linux, Windows, and macOS will be attached to GitHub
releases once published (see [docs/RELEASING.md](docs/RELEASING.md) for how
releases are cut).

## CLI reference

All commands accept `-v` / `-vv` for debug/trace logging. `--session`
defaults to `mosaic.mmm-session` everywhere.

### `mmm info <panels…> [--stats]`

Print header metadata for panel files.

| Flag | Default | Meaning |
|---|---|---|
| `--stats` | off | Also scan pixel data for per-channel min/max/mean/nonzero-fraction (reads the whole file) |

### `mmm analyze <panels…> [options]`

Scan panels into a session directory: reprojection (if needed), 1/8-scale
summaries, overlap graph, photometric solve, residual surfaces.

| Flag | Default | Meaning |
|---|---|---|
| `-s, --session <DIR>` | `mosaic.mmm-session` | Session directory for cached analysis (created if missing) |
| `--surface off\|0\|1\|2` | `2` | Residual surface correction order: off, constant, plane, quadratic |
| `--input auto\|aligned\|solved` | `auto` | Input kind: auto-detect, registered full-canvas frames, or unaligned plate-solved panels |

### `mmm report [options]`

Print the overlap-graph edge table, per-edge photometric fits, seam Δ per
edge, and residual-surface magnitudes, with ⚠ flags on outliers.

| Flag | Default | Meaning |
|---|---|---|
| `-s, --session <DIR>` | `mosaic.mmm-session` | Session directory produced by `mmm analyze` |
| `--seam-png <PNG>` | — | Write a seam/ownership map: autostretched preview with owner regions tinted, seams drawn, panel ids labelled |

### `mmm blend -o <out.fits> [options]`

Blend the analyzed panels into a mosaic FITS (BITPIX=-32, planar channels).

| Flag | Default | Meaning |
|---|---|---|
| `-s, --session <DIR>` | `mosaic.mmm-session` | Session directory produced by `mmm analyze` |
| `-o, --output <FITS>` | required | Output FITS file |
| `--downsample <N>` | `1` | `1` = full resolution, `8` = fast preview from the 1/8-scale summaries (only 1 and 8 are supported) |
| `--feather <PX>` | `256` | Feather ramp length in canvas pixels |
| `--mode pyramid\|twoband\|feather` | `pyramid` | Blend mode: multiband base + star-safe seams (default), feathered base + star-safe seams, or plain feather |
| `--png <PNG>` | — | Also write an autostretched 8-bit PNG preview (downsampled runs only) |
| `--roi x,y,w,h` | — | Restrict output to a region of interest, in full-res canvas pixels |
| `--defect-veto on\|off` | `on` | Cross-panel defect veto in overlaps (seam modes): suppress cosmic-ray residue / satellite trails surviving in one panel |
| `--flatten off\|1\|2` | `off` | Opt-in global background flatten (plane / quadratic); refuses on signal-dominated mosaics |
| `--wcs-frame topdown\|flipped` | `topdown` | WCS card convention: PixInsight display-space (default), or reflected bottom-up for readers that mirror annotations |

## Worked examples

Registered panels (MosaicByCoordinates output) to a finished mosaic:

```sh
mmm analyze registered/*.xisf --session orion.mmm-session
mmm report  --session orion.mmm-session          # sanity-check fits & seams
mmm blend   --session orion.mmm-session -o orion.fits
```

Raw plate-solved panels — identical commands; `analyze` auto-detects solved
input and reprojects each panel onto a fresh mosaic frame first:

```sh
mmm analyze raw_solved/*.xisf --session orion.mmm-session
mmm blend   --session orion.mmm-session -o orion.fits
```

Fast preview (sub-second, 1/8 scale, with a stretched PNG for quick looks):

```sh
mmm blend --session orion.mmm-session -o preview.fits --downsample 8 --png preview.png
```

Iterate on a problem area at full resolution without re-blending everything:

```sh
mmm blend --session orion.mmm-session -o crop.fits --roi 5000,3500,2000,2000
```

Inspect where the seams went and how big each seam step is:

```sh
mmm report --session orion.mmm-session --seam-png seam_map.png
```

Compare with and without the global background flatten:

```sh
mmm blend --session orion.mmm-session -o orion_flat.fits --flatten 2
```

**Drizzle-scale inputs:** there is no drizzle option in `mmm` — drizzle
happens upstream when you integrate the panels. Drizzled solved panels go
through the same commands; the canvas (and therefore time and output size)
just scales accordingly — see the 2× drizzle row in the performance table.

## Input requirements & limitations

- **Input:** XISF only, uncompressed, Float32, monolithic (one file per
  panel) — exactly what PixInsight writes by default. FITS and compressed
  XISF input are not yet supported.
- **Output:** 32-bit float FITS (plus autostretched 8-bit PNG previews).
- **Linear data** is expected end-to-end; zero is the no-data sentinel
  (registered panels must keep their hard zero padding).
- **Unaligned mode** needs each panel to carry a PixInsight astrometric
  solution (`PCL:AstrometricSolution:*` properties, PixInsight ≥ 1.9.4
  verified; spline distortion grids are used when present).
- Channels are matched independently (per-channel photometry for OSC/RGB);
  seams are shared across channels so colour fringing cannot occur. RGB is
  the tested path.
- **Known open issue:** some PixInsight versions mirror *annotations*
  north–south on mmm output despite catalog-verified-correct WCS. If you see
  this, re-blend with `--wcs-frame flipped`. Details and status in
  [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md#open-issues).

## Performance

Measured on a 64-thread Threadripper (WSL2, warm file cache) with the
12-panel Orion test mosaic; your numbers will scale with cores and,
cold-cache, with disk throughput:

| Input | Canvas | Analyze (+align) | Full-res blend |
|---|---|---|---|
| 12 registered panels, 24 GB | 9255×18310 ×3ch | ~5 s | ~7 s |
| Same 12 as raw solved stacks, 3 GB | 9286×18341 ×3ch | ~11 s | ~7 s |
| Same 12, 2× drizzle, 12 GB | 18540×36649 ×3ch (679 Mpx) | 62 s | 34 s → 8.1 GB FITS |

Peak RAM for the 2× drizzle run was 15 GB; the pipeline is mmap/streaming
based, so canvas size is bounded by disk rather than memory. The raw-stack
result matches the registered-panel result to 0.030 px median star position.

## Status

Early — the CLI surface and formats may still change — but heavily
validated: 140 tests including synthetic ground-truth end-to-end pipelines
and byte-exact regression guards, and every quality mechanism was verified
on real mosaic data in PixInsight. Feedback, bug reports, and sample data
that breaks it are very welcome — please open a GitHub issue.
Contributors: start with [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).

## About Astrometrical

`mmm` is an [Astrometrical](https://github.com/Astrometrical) tool — built by
an astrophotographer making tooling for very large images. It is brand-neutral
and community-first: not tied to any platform, and useful on its own terms.

## Contributing

Contributions, bug reports, and frontend integrations are welcome — start with
[CONTRIBUTING.md](CONTRIBUTING.md) (and [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md)
for the build/test workflow and internals).

## Support / donations

`mmm` is free and open source, and always will be. If it saves you time and you
want to say thanks, donations are welcome at
[Ko-fi](https://ko-fi.com/astrometrical) — entirely optional. Contributions and
frontend integrations are valued just as highly.

## License

Apache-2.0 — see [LICENSE](LICENSE) and [NOTICE](NOTICE).

In plain terms: you are free to use, modify, and redistribute `mmm`, including
commercially, provided you keep the attribution and license notices. The
"Astrometrical" name and branding are a trademark of the project and are not
licensed for endorsement or promotional use (Apache License §6).
