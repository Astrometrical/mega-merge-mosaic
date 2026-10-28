# Formats consolidation into mpp-core + XISF Revision 2 compliance — roadmap

> Roadmap plan. Each phase below gets its own detailed (TDD, bite-sized)
> plan in the repo it lands in when it starts; this document fixes scope,
> order, decisions and acceptance gates.

**Goal:** one home for image-format handling — `mpp-core` (sibling repo
`~/dev/mega-pixel-processor`) — with `mmm-core` depending on it; and that one
XISF reader brought up to the XISF 1.0 spec, document version 1.02
(Revision 2, Oct 2026), so compressed XISF from PixInsight reads correctly.

**Trigger:** a user reports PixInsight now writes compressed XISF. Confirmed
2026-10-28: the user's files were written by **WBPP**, so compressed
*masters* (our main input) should now be expected to be common. Both
readers refuse any `compression` attribute outright
(mmm `formats/xisf.rs:648`, mpp `formats/xisf.rs:769`), so those files are
unreadable today in the CLI and in the PixInsight module's Files mode.

**Decisions (Daniel, 2026-10-28):**
- **mpp repo visibility:** it is private today. Make it public when mmm CI
  first needs the git dependency (Phase 3); clean up its README and so on
  soon after. No CI token plumbing.
- **Decoded panels:** cached in the session dir. Both the CLI and the
  PixInsight module console print a *prominent* message saying a compressed
  panel is being decoded to disk: the path and the size.
- **Checksum mismatch:** error by default, with an explicit override flag.
- **Phase 4 (astrometry rationalisation):** in scope. The more complete the
  refactor, the better.
- **Codec crates:** as proposed (`flate2`, `lz4_flex`, `zstd`, RustCrypto
  SHA crates).
- **Real samples:** PixInsight 1.9.5 is installed on WSL and on the Windows
  host. Generate every XISF permutation with a PJSR script in automation mode
  (§4 Phase 0.4). Generate them with the current 1.9.5 build *and* with the
  pending update (expected to carry the Rev 2 encoder), so both the Rev 1
  and Rev 2 encoder behaviours are covered.

**Execution (autonomous, from 2026-10-28):**
- Branches: mmm `feat/formats-to-mpp`; mpp `feat/formats-consolidation`.
- No pushing and no merging without Daniel's say-so.
- While on branches, mmm depends on mpp by **path**
  (`../mega-pixel-processor/crates/mpp-core`). The switch to the pinned git
  dependency is the last step, after mpp goes public. Until then, mmm's CI is
  not expected to pass on this branch.
- Work through Phases 0–4. Write each phase's detailed plan in
  `docs/superpowers/plans/` of the repo it lands in, then execute it
  test-first. Keep fmt, clippy and doc clean, and commit per task.
- Phase 5 (PixInsight GUI Files-mode smoke test, release) needs Daniel. Stop
  there.

---

## 1. Situation (audited 2026-10-28)

### Two near-identical XISF readers

mpp's reader was *ported* from mmm (mpp DESIGN.md:89-94, decided 2026-08-20:
"mmm-core will eventually depend on mpp-core for file-format support"). Both
are the same "monolithic minimal subset":

| Capability | mmm-core | mpp-core |
|---|---|---|
| Monolithic file, `XISF0100` preamble | yes | yes |
| Image location | `attachment:` only | `attachment:` only |
| Compression (any codec), subblocks, byte shuffle | **refused** | **refused** |
| Checksums | ignored | ignored |
| `byteOrder="big"` | refused | refused |
| `pixelStorage="Normal"` | refused | refused |
| Pixel sample formats | Float32 only (zero-copy mmap) | Float32 only (zero-copy mmap) |
| `bounds` | ignored | ignored |
| Multiple `<Image>` | first only | first only |
| Property scalars / all numeric vectors+matrices | yes | yes |
| Property `inline:base64/hex`, `attachment:` | yes | yes |
| Property `embedded`, compressed property blocks | no | no (lenient `Unread`) |
| Header-only read for all sample formats | — | `read_header` |
| Drops `PCL:AstrometricSolution:Grid:*` cache | — | yes |
| Format-neutral `Header` model | — | `header.rs` |
| Writer | test-only `synth` writers | none |

**Conclusion:** mpp's reader is a strict superset of mmm's. It is the base;
the mmm copy is deleted. Nothing of mmm's XISF reader needs porting except
any behavioural detail a golden test shows differs.

### FITS

- **mmm** has the real FITS pixel reader (`formats/fits.rs`, 606 lines: BITPIX
  8/16/32/−32/−64, BZERO/BSCALE/BLANK, NaN→0, ROWORDER, NAXIS 2/3) and the
  production writer (`output/fits.rs` `FitsSink`, streaming band writes).
- **mpp** has a header-only reader (`formats/fits.rs`, 212 lines) and says so:
  "mpp-core has no FITS pixel reader yet."
- **FITS WCS** (TAN / TAN-SIP, CD/PC/CDELT/CROTA2) lives only in mmm
  (`astrometry/fits_wcs.rs`, `sip.rs`). mpp has an uninhabited
  `WcsSolution` enum reserved for exactly this (mpp `astrometry/mod.rs:74`).

### Astrometric-solution decoders are also duplicated

mmm `astrometry/{standard,legacy,spline}.rs` (~2.3k lines) and mpp
`astrometry/{pixinsight,legacy,distortion}.rs` (~2.7k lines) both decode the
rev 1 `AstrometricSolution:*` block and the legacy `PCL:` block. mpp's is the
newer one (layers 1–4, direct evaluation, `#[non_exhaustive]`, built on
`Header`). mmm's `WcsModel` adds grid acceleration for reprojection, which is
mmm-specific and stays in mmm.

### Coupling in mmm that must stay behind as adapters

- `InputPanel::storage()` → `panel_reader::PanelStorage`
- `InputPanel::wcs_model()` / `linear_wcs()` → mmm `astrometry`
- `FitsSink` → `blend::RowSink`
- **IPC wire:** `XisfProperty`, `PropertyValue`, `FitsKeyword` are serde types
  inside the PixInsight host ↔ worker protocol (`ipc/protocol.rs:16,207,504`).
  Their serialised form must not change, or the C++ side and older module
  builds break.

### Dependency weight

mpp-core does not link libvips (it shells out to the `vips` CLI), but it
unconditionally pulls `tiff` (+flate2, weezl, zune-jpeg) and the
convert/publish/viewer modules. mmm wants only formats + astrometry.

### Build plumbing

mmm CI (`ci.yml`, `module.yml`, `release.yml`) checks out only the mmm repo,
so a `path = "../mega-pixel-processor/..."` dependency would break all CI.

---

## 2. XISF compliance gap list (spec 1.02)

The target is **baseline decoder** conformance (spec §7.2) at Revision 2,
plus the rest of what PixInsight actually writes. Spec facts below were
checked against the published spec on 2026-10-28.

### Must-have (baseline decoder, §7.2 + Rev 1/Rev 2)

1. **Compression, all standard codecs** (§10.6), on image *and* property
   blocks: `zlib`, `lz4`, `lz4hc`, `zstd` and their `+sh` variants.
   - Syntax: `compression="<codec>:<uncompressed-size>[:<item-size>]"`
     (item size only for `+sh`).
   - zlib = RFC 1950 zlib stream (with header), not raw deflate. lz4/lz4hc =
     LZ4 *block* format (no frame). zstd = one zstd *frame* per subblock.
   - Rev 1: zstd support is required for Rev 1 conformance.
   - Unsupported codec → that block is unavailable; the rest of the unit
     stays readable.
2. **Subblocks:** `subblocks="c1,u1:c2,u2:…"`. Stored contiguously, each
   compressed independently (so parallel decode is possible). They are
   required above the codec input limit (zlib 2³²−1), so large panels will
   have them.
3. **Rev 2, uncompressed subblocks** (§10.6.1): `cᵢ = uᵢ` means the subblock
   is stored raw and copied as is. A block with no `subblocks` attribute is
   one subblock under the same rule.
   - Legacy tolerance: a `cᵢ = uᵢ` subblock from a Rev 1 encoder may be
     compressed. Try to decompress it; use the result only if it yields
     exactly `uᵢ` bytes, otherwise read it raw.
4. **Rev 2, length verification:** every compressed subblock must decompress
   to exactly `uᵢ` bytes, or the block is invalid. The total must equal the
   declared uncompressed size.
5. **Byte shuffling** (§10.6.3): unshuffle is applied once to the whole
   concatenated block after all subblocks are decompressed. Bytes of equal
   significance are grouped ascending, and trailing bytes that do not form a
   whole item are stored unshuffled at the end.
6. **Checksums** (§10.5): `checksum="<alg>:<lowercase-hex>"`.
   - Algorithms: `sha-1|sha1`, `sha-256|sha256`, `sha-512|sha512` (required);
     `sha3-256`, `sha3-512` (optional, implement anyway, it's cheap).
   - The digest covers the **stored (compressed) bytes**. For inline and
     embedded blocks it covers the decoded binary.
   - Verify before decompressing; on mismatch, do not decompress.
   - Policy: error by default, with an explicit "ignore checksum" escape
     hatch. The spec allows verification on demand; batch tools should not
     load a failing unit.
7. **`byteOrder="big"`** (data blocks; applies to the uncompressed data).
8. **`pixelStorage="Normal"`** (interleaved) as well as Planar (the default).
9. **Sample formats UInt8, UInt16, Float32** at minimum; also UInt32 and
   Float64 (cheap). UInt64 and complex: refuse by name.
10. **Multiple `<Image>` elements:** read all headers. The consumer selects
    one (first by default, or by `id`). Ignore `<Thumbnail>`.
11. **Data block locations:**
    - Image: `attachment:` and `embedded` (an Image cannot be inline).
    - Properties: also `inline:`.
    - `url(...)` and `path(...)` (distributed units / XISB) are *not*
      baseline-required. Refuse them by name.
12. **Robust text forms (Rev 1):**
    - Booleans `true`/`false`/`1`/`0`.
    - Non-finite floats `NaN`, `+Inf`, `-Inf`, `nan`, `-nan`, `inf`, `-inf`.
    - Ignore leading and trailing whitespace on scalars.
    - Unknown elements, attributes and properties are ignored (§7).
    - Empty vectors and matrices as zero-length inline blocks.

### Semantics to honour (not just parse)

- **`bounds`** is mandatory for float images. mmm assumes [0,1]: if bounds
  are not `0:1`, either rescale to [0,1] on decode or refuse. Integer images
  normalise by the type range (matches mmm's FITS path).
- **`offset`**: subtract it if present (rare).
- **`orientation`**: the spec says *not* to apply it for processing that
  depends on physical pixel layout, which is ours. Ignore it, with a warning.
- **`colorSpace`**: Gray or RGB; refuse CIELab.

### Out of scope here (record in DESIGN.md as deferred)

- Distributed units (`.xish` + XISB).
- XML signatures.
- Complex and UInt64 pixels.
- An XISF *writer*: mmm outputs FITS only. A future `zstd+sh` XISF output
  would reuse the codec layer built here.
- FITS tile compression (fpack).

---

## 3. Architecture

### Crate shape

`mpp-core` gains Cargo features. The default keeps today's full mpp build, and
mmm depends on the core with `default-features = false`:

```toml
[features]
default = ["tiff", "convert", "publish"]
tiff = ["dep:tiff"]          # tiff_read / tiff_write
convert = ["tiff"]           # convert.rs, wcs sidecar
publish = ["convert"]        # publish.rs, viewer.rs, vips.rs
# always on: formats::{xisf, fits, codec}, header, astrometry, wcs
```

The alternative, a separate `mpp-formats` crate, is only worth it if feature
gating gets messy. Decide in Phase 1, task 1 after a trial split.

### Reader API (mpp `formats`)

- `sniff_format(&Path)`: moves from mmm. Detection by signature, never by
  extension. mpp's extension-based `read_header` switches to it.
- `xisf::XisfFile::open(path)` → parsed header (all images, properties,
  keywords) plus the file mmap. `image(i)` → `XisfImageRef`, whose pixel
  access is one of:
  - `PixelSource::Mapped(&[f32] per channel)`: the zero-copy fast path when
    the block is attached, uncompressed, planar, little-endian, Float32,
    4-byte-aligned and has `0:1` bounds. This is today's behaviour, unchanged.
  - `PixelSource::Decode`: everything else. The caller supplies the
    destination: `decode_into(&mut impl Write + Seek)` streams the
    planar-f32 result, and `decode_rows(c, y0, n, out)` is for small images.
    The decode pipeline: verify checksum → per-subblock decompress (parallel)
    → length-check → concatenate → unshuffle → byte order → storage-model
    de-interleave → sample conversion + bounds normalisation.
- `xisf::codec`: codec trait, the four codecs, shuffle/unshuffle and subblock
  handling. It is pure and unit-testable, and also used for property blocks.
- `fits::FitsFile`: mmm's pixel reader moved over (`decode_rows`, RowOrder)
  and merged with mpp's header reader.
- `fits::write`: the format-pure half of `FitsSink` (card formatting,
  2880-byte padding, positional band writes, `keywords_for_output` filtering).
  The `RowSink` adapter stays in mmm. Fix the known follow-ups while moving
  (DESIGN.md: lowercase exponents, `COMMENT =` cards).
- Value types `FitsKeyword`, `XisfProperty`, `PropertyValue`, `SampleFormat`,
  `RowOrder`, `card_number`/`card_string` are canonical in mpp. mmm
  re-exports them (`pub use mpp_core::formats::{…}`), so wire types keep
  their names and serde form.

### Decoded panels in mmm

Panels are ~2 GB (12+ per mosaic), so decoded pixels cannot live in RAM.
Decode once into the session dir (`*.mmm-session/decoded/<panel-hash>.f32`,
planar f32, page-aligned), then mmap it. Every downstream consumer keeps its
zero-copy `&[f32]` channel slices, and `align.rs`'s `as_xisf().channel(c)`
path keeps working through an `InputPanel` accessor. DESIGN.md:19-22 already
anticipates "decompress into a tile cache once".

- The cache key is the path plus size plus mtime plus the block checksum when
  present.
- Stages stay re-runnable (the cache is reused).
- A one-shot `mmm` run without a session uses a temp file next to the output.

### Crate choices (pure Rust where it costs nothing)

| Need | Crate | Note |
|---|---|---|
| zlib | `flate2` (zlib-rs or miniz backend) | already transitively present via `tiff` |
| lz4 / lz4hc decode | `lz4_flex` (block API) | pure Rust; lz4hc decodes with the same block decoder |
| zstd | `zstd` (C, via `zstd-sys`) | fastest; builds on all three CI OSes. Fallback: `ruzstd` (pure Rust, decode-only, slower) if Windows/macOS module builds object |
| SHA-1/2/3 | `sha1`, `sha2`, `sha3` (RustCrypto) | |

Parallel subblock decode uses std scoped threads (mpp has no rayon). This is
I/O-bound anyway.

### Dependency wiring mmm → mpp

```toml
# mmm workspace Cargo.toml
mpp-core = { git = "https://github.com/Astrometrical/mega-pixel-processor",
             rev = "<sha>", default-features = false }
```

- CI works from the pinned rev. The mpp repo goes public first (see
  Decisions).
- Local development against the sibling checkout uses an *untracked* override
  in `~/.cargo/config.toml`:
  `[patch."https://github.com/Astrometrical/mega-pixel-processor"] mpp-core = { path = "/home/dpaull/dev/mega-pixel-processor/crates/mpp-core" }`.
- Bump `rev` deliberately, one commit per bump.
- Edition: mpp is on 2024, mmm on its own edition. That is fine across
  crates, but mmm's toolchain must meet mpp's effective MSRV (≥ 1.88: let
  chains, `as_chunks`). Pin `rust-version` in mpp-core so mismatches fail
  loudly.

---

## 4. Phases

Each phase ends green: `cargo fmt`, `clippy --all-targets -D warnings`,
`cargo test`, `cargo doc` in the repo touched, and mmm untouched until
Phase 3.

### Phase 0: Safety nets (mmm, small)

1. **Golden wire test:** serialise a fixture set of `XisfProperty`,
   `PropertyValue` and `FitsKeyword` values (every variant) to JSON. Check
   them in under `crates/mmm-core/tests/fixtures/`. Phase 3 must reproduce
   them byte-for-byte through the mpp types.
2. **Golden decode fixtures:** for the synth XISF/FITS test files, record
   pixel checksums and decoded header/property dumps from the *current* mmm
   readers.
3. **Golden WCS fixtures** (used in Phase 4): `pixel_to_sky` at a grid of
   points for each solution kind (legacy, rev 1 standard, FITS TAN, TAN-SIP).
   Record them from mmm's decoders.
4. Generate real samples with a PJSR script run headless:
   `PixInsight.sh -n --automation-mode -r=gen.js --force-exit`.
   - Scope: one small, deterministic image per sample format (UInt8, UInt16,
     Float32, Float64; Gray and RGB). Save every codec (none, zlib, lz4,
     lz4hc, zstd, each ± shuffle) × checksum (none, sha-1, sha-256, sha-512,
     sha3-*). Also save a large image that forces subblocks, and a
     poorly-compressible one that forces raw subblocks under the Rev 2
     encoder.
   - Also make one solved-panel copy (an M42 panel) saved `zstd+sh` +
     sha-256 for end-to-end runs.
   - Output: `test_data/xisf_samples/<pi-build>/`, with a manifest recording
     each file's settings and the uncompressed reference pixels' SHA-256.
   - The script lives in-repo (`tools/pixinsight/gen_xisf_samples.js`) so
     sample sets can be regenerated against new PI builds.
   - Manual smoke tests only (tests never depend on `test_data/`).
   - **Done 2026-10-28 for PI 1.9.5-1702** (pre-update):
     `test_data/xisf_samples/pi-1.9.5-pre-update/`, 130 small files plus
     the big and panel sets. Run it again after the PI update into a new
     directory.
   - Observed behaviour of the PI 1.9.5-1702 encoder (from its headers and
     the bundled PCL source, `/opt/PixInsight/src/pcl/{Compression,XISFWriter}.cpp`).
     The reader must handle all of these:
     - Hints:
       - `compression-codec <id>` (all 8 codecs)
       - `compression-level n`
       - `no-compression`
       - `checksums sha1|sha256|sha512` (no SHA3 on write)
       - `no-checksums`
       - `block-alignment n`
       - `image-ids a,b`
     - Always little-endian and Planar. Big-endian and Normal storage come
       only from synthesized tests.
     - Uncompressed blocks are 4096-aligned. Compressed blocks are *not*
       aligned at all (e.g. `attachment:1157:…`).
     - 8-bit `+sh` is written as the plain codec, because shuffling is a
       no-op for bytes.
     - A block that does not shrink at all is written uncompressed, with no
       `compression` attribute (not as a `c = u` subblock).
     - PI sets no subblock size, so subblocks appear only above a codec's
       input limit (LZ4 `0x7E000000` ≈ 2.11 GB; zlib 2³²−1; zstd
       effectively unlimited).
     - Within a multi-subblock block, PCL already stores incompressible
       subblocks raw (`c = u`). This is the Rev 2 rule, present in 1.9.5's
       PCL.
     - The `subblocks` attribute is written only when there are more than
       one.
     - Multi-image units work (`image-ids`).
     - Registered M42 canvases (2.03 GB Float32) sit just *under* the LZ4
       limit: a single subblock.
     - Mixed subblock list captured: `big_f32_rgb_mixed__lz4__none.xisf`
       has `subblocks="26796292,2113929216:238070784,238070784"`, i.e. a
       compressed subblock followed by a raw one. `big_…_tailnoise__lz4-sh`
       has 2 compressed subblocks.
     - Re-saved solved panel (`panel_…PANEL-4…__zstd-sh__sha256.xisf`):
       property blocks are compressed and checksummed as well, e.g.
       `AstrometricSolution:ControlPoints:Celestial` is an F64Matrix with
       `compression="zstd+sh:138384:8"` plus a `checksum` and an
       `attachment:` location. The reader must therefore decompress property
       blocks too, or the astrometric solution is lost.
     - The set is 14 GB in total.
   - **PI 1.9.5-1706** (installed 2026-10-28): small set regenerated into
     `test_data/xisf_samples/pi-1.9.5-1706/`.
     - Same XISF module (1.1.3). All 130 header summaries are identical to
       1702's (codecs, attributes, compressed sizes). The files differ only
       in `XISF:CreationTime`. The encoder is therefore unchanged, and the
       1702 big and panel sets stand for 1706 too.
     - `defaults` mode (empty hints) writes *uncompressed*: 4096-aligned, no
       checksum. Compression is not a new PI default. A user's compressed
       files come from their own XISF output preferences or from a
       process/script that sets hints, so expect them in the wild with any
       codec.

### Phase 1: mpp-core restructure and FITS consolidation (mpp repo)

1. Feature-gate tiff/convert/publish as above; confirm a
   `--no-default-features` build pulls no `tiff`/vips modules.
2. Move `sniff_format` and `FileFormat` from mmm; `read_header` dispatches on
   signature.
3. Move mmm's FITS pixel reader into `formats::fits` and merge it with the
   header reader. Port its 7 tests.
4. Move the format-pure half of the FITS writer (`fits::write`) with its tests
   and fix the two DESIGN.md follow-ups (uppercase `E` exponents; `COMMENT`
   without `=`). Verify with astropy `fitsverify`-style checks in a manual
   smoke test.
5. Reconcile value types. mpp's are a verbatim port, but diff
   field-by-field and serde-attribute-by-attribute against mmm, and adopt
   mmm's Phase 0 golden JSON as an mpp test.
6. Add `XisfFile` (multi-image header) and the `PixelSource` split, keeping
   `XisfImage` as a thin compatibility wrapper for mpp's convert/publish.
7. Set `rust-version`.

### Phase 2: XISF Rev 2 compliance (mpp repo)

TDD against synthesized files: extend `testsupport::synth_xisf` into a small
test *encoder* that can emit every codec, shuffle, subblocks (including
`c=u` raw ones), checksums, big-endian, Normal storage, each sample format,
multiple images and embedded/inline blocks. It needs the compressors as
dev-dependencies; `flate2`, `lz4_flex` and `zstd` all compress.

1. `codec` module: shuffle/unshuffle (port the spec's reference routine,
   including the trailing-bytes rule); zlib, lz4, lz4hc, zstd; subblock
   splitting; Rev 2 raw-subblock and legacy-retry rules; exact-length
   verification.
2. Checksums: all five algorithms, the stored-bytes rule, and the
   inline/embedded-decoded rule. The mismatch policy is an error with an
   opt-out flag on the open options.
3. Decode pipeline for image blocks (`PixelSource::Decode`): byte order,
   Normal→planar, UInt8/16/32/Float64→f32, `bounds` normalisation, `offset`.
4. Property blocks: compression, checksum and `embedded` location now
   supported. Complex types stay `Unread`.
5. Text-form robustness: booleans, non-finite floats, whitespace, unknown
   elements and attributes ignored; `Thumbnail` skipped; `url()`/`path()`
   refused by name.
6. Error taxonomy: "unsupported (named feature)" vs "corrupt (what failed
   where)", and per-block unavailability without failing the whole header
   read (spec §10.6).
7. Performance check: decode a synthesized 2 GB `zstd+sh` Float32 block;
   target is disk-bound, not CPU-bound, with parallel subblocks.
8. Manual smoke test on the Phase 0 real PixInsight samples: pixels must be
   identical to an uncompressed save of the same image.
9. mpp DESIGN.md: an XISF conformance section (what is supported, what is
   refused, and why).

### Phase 3: mmm-core adopts mpp-core formats (mmm repo)

1. Add the git dependency (pinned rev) and the local `[patch]` override doc in
   CLAUDE.md "Commands".
2. Re-export the value types from mpp. The Phase 0 golden wire JSON must pass
   unchanged.
3. `InputPanel` stays in mmm as the adapter. It wraps `mpp::XisfFile` /
   `mpp::FitsFile` and owns `storage()`, `wcs_model()`, `linear_wcs()`,
   `filter_name()`.
4. Decoded-panel cache in the session dir, mmapped. Print a prominent notice,
   in the CLI on stderr and in the PI module via the worker's console
   channel, naming each panel being decoded, the cache path and its size.
   `PanelStorage` and
   `panel_reader` gain the "decoded XISF" case; `align.rs` channel access
   goes through `InputPanel`, not `as_xisf()`.
5. `FitsSink` becomes a `RowSink` adapter over `mpp::fits::write`.
6. Delete `formats/xisf.rs`, `formats/fits.rs` and most of `formats/mod.rs`.
   Port surviving tests. The `synth::write_xisf*`/`write_fits*` test writers
   stay in mmm (they encode mmm-specific fixtures); later they can call mpp's
   test encoder.
7. `mmm info`: report compression codec, subblocks, checksum status, sample
   format and storage model.
8. Phase 0 golden pixel and header fixtures must pass. Real-data smoke test:
   the M42 panels (uncompressed, so the fast path, no regression in timing),
   plus a compressed save of panels 3/4/7/8 producing an identical blend.
9. DESIGN.md: update the "verified facts" and the phase 3 "compressed XISF
   ingest" deferral (now done), and point to mpp for format details.

### Phase 4: Astrometry rationalisation (both repos)

Scoped separately because it is the riskiest part and not needed to unblock
compressed XISF. Recommended, so the duplicate decoders don't drift.

1. **mpp:** move mmm's `fits_wcs.rs` + `sip.rs` into mpp `astrometry` as
   `WcsSolution::{Tan, TanSip}`, with the SIP oracle fixture.
2. **mpp:** confirm mpp's PixInsight decoders meet the Phase 0 WCS goldens
   to ≤ 1e-6 px (the spec's interoperability tolerance) for legacy and
   standard solutions.
3. **mmm:** `WcsModel` is built *from* `mpp::AstrometricSolution`. Grid
   acceleration, `describe_unsolved`, `wcs_cards*` and reprojection stay in
   mmm. Delete mmm `standard.rs`, `legacy.rs`, `spline.rs`, `fits_wcs.rs`,
   `sip.rs`.
4. Real-data parity: an `analyze`/`align` run on `orion_mosaic_raw_panels`
   and `_195` must give identical (≤ 1e-6 px) placement.

### Phase 5: Ship (mmm)

- Rebuild the PixInsight module worker. Files-mode smoke test with
  compressed panels in PI 1.9.5.
- Release notes: "reads compressed XISF (zlib/lz4/lz4hc/zstd, byte-shuffled,
  checksummed; XISF 1.0 Rev 2)". Bump version.
- Reply to the reporting user.

---

## 5. Risks

- **Wire drift:** a serde change in the moved types silently breaks the PI
  host. Mitigated by the Phase 0 golden JSON, enforced in both repos.
- **Disk for decoded panels:** a 12-panel mosaic is ~24 GB of decoded cache.
  Document it; `mmm` reports it; the cache can be cleared per session.
  (Alternative later: band-wise on-demand decode. Subblock boundaries don't
  align to rows, and shuffle spans the whole block, so random access needs
  the full decode.)
- **zstd C build on Windows/macOS module CI:** Phase 2 adds it in mpp first.
  Before Phase 3, run mmm's `module.yml` matrix once on a branch. Fallback:
  `ruzstd`.
- **Repo access** for the mmm CI git dependency: resolved by making mpp
  public before Phase 3 lands.
- **Astrometry parity** (Phase 4) may surface real disagreements between the
  two decoders. The Phase 0 goldens decide which is right; it is not settled
  by assumption.
