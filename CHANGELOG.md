# Changelog

## 1.7.0 — 2026-10-29

CLI, PixInsight module and worker. Requires PixInsight ≥ 1.9.5, as 1.6.0 did.

### Added

- **Compressed XISF input.** XISF 1.0 (spec revision 1.02) compressed images
  and properties are read: zlib, LZ4, LZ4HC and Zstandard, each with or
  without byte shuffling, in one block or in subblocks. This covers the
  compressed masters that WeightedBatchPreprocessing now writes; previously
  they were refused.
- **XISF checksums** (SHA-1, SHA-256, SHA-512, SHA3-256, SHA3-512) are
  verified, and a mismatch is an error.
- **More XISF sample formats:** UInt8/16/32 and Float64 alongside Float32,
  plus big-endian and planar/normal pixel storage and multi-image files (the
  first image is used).
- **Decode cache.** A compressed panel is decompressed once into
  `<session>/decoded/` (the uncompressed size: about 2 GB per full-canvas
  colour panel) and reused by later runs. A notice on stderr (CLI) or in the
  process console (PixInsight) says when and how much. Without a session,
  for example `mmm info --stats`, panels decode into memory instead.

### Changed

- **FITS output headers** follow the FITS standard more strictly: exponents
  use an uppercase `E`, `COMMENT`/`HISTORY` cards have no `= `, and long
  string values pass through as `CONTINUE` cards. Pixel data is unchanged.
- **Non-gnomonic linear-only astrometric solutions** are now refused for
  solved-mode alignment, with a reason. They were previously treated as TAN.
- **Float XISF panels** whose `bounds` aren't `0:1`, or that carry an
  `offset`, are rescaled to [0, 1] on read.
- XISF, FITS and astrometric-solution decoding now come from the
  [Mega Pixel Processor](https://github.com/Astrometrical/mega-pixel-processor)
  `mpp-core` library. On real data the analyze and blend results are
  byte-identical to 1.6.0.

### Fixed

- Hostile or corrupt XISF headers that declare huge block sizes are an error,
  not an out-of-memory abort.
- A full disk while caching a decoded panel is reported as an error, not a
  crash.

### Build

- The PixInsight module is compiled against the pinned PCL 2.10.8 headers,
  not the installed core's. PixInsight 1.9.5 build 1706 ships PCL 2.11.0
  headers, and a module built against them failed to load with
  `undefined symbol: _ZN3pcl…`. On Linux the build now refuses to produce
  such a module.

## 1.6.0 — 2026-10-12

See the [v1.6.0 release](https://github.com/Astrometrical/mega-merge-mosaic/releases/tag/v1.6.0).
