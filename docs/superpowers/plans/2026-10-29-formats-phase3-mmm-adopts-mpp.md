# Formats consolidation — Phase 3: mmm-core adopts mpp-core formats

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** delete mmm's own XISF/FITS code and read every panel through
`mpp-core`, so mmm accepts compressed (WBPP) XISF masters. A
non-mappable panel is decoded once into a cache file and memory-mapped,
with a prominent notice in the CLI and the PixInsight console.

**Architecture:**
- **Shared types and readers.** mmm re-exports mpp's value types, which
  are serde-identical (pinned by the wire golden). `XisfPanel` and
  `FitsPanel` become aliases of mpp's `XisfImage` and `FitsImage`.
  `InputPanel`, `PanelReader`, `FitsSink` and the astrometry glue stay in
  mmm as adapters.
- **Decoded panels reuse an existing backing.** A decoded panel is served
  through the existing `Backing::Cache` (raw planar f32 file, mmapped,
  full-canvas bbox), so the analyze scan, reprojection and blender are
  unchanged. `InputPanel::as_xisf()` returns only *mapped* images, so
  `align::reproject_panel` falls through to its existing `PanelReader`
  path for decoded ones.
- **The notice needs no protocol change.** It reaches PixInsight through
  the existing `WorkerMsg::Progress`, whose unknown stage names the host
  prints verbatim, so neither the C++ module nor the wire protocol
  changes.

**Tech Stack:** Rust 2024, `mpp-core` (path dependency, `default-features = false`).

**Spec:** roadmap `docs/superpowers/plans/2026-10-28-formats-to-mpp-xisf-rev2.md`
(§3 "Decoded panels in mmm", §4 Phase 3, Decisions). Phase 2 behaviour:
mpp `docs/DESIGN.md` "XISF conformance".

## Global Constraints

- Branches: mmm `feat/formats-to-mpp`, mpp `feat/formats-consolidation`.
  No push, no merge.
- The mmm → mpp dependency is a path dependency on the branch:
  `mpp-core = { path = "../mega-pixel-processor/crates/mpp-core", default-features = false }`.
  Moving to the pinned git dependency happens after mpp goes public (not
  in this plan).
- Each commit is green on `cargo fmt --all --check`,
  `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace`
  and `cargo doc --no-deps`, in each repo it touches.
- The Phase 0 goldens must pass **unchanged**: `wire_golden`,
  `decode_golden`, `wcs_golden`. Do not re-bless them in this plan. A
  failure is a behaviour change and needs a ledger ruling.
- **Decode notice text** (spec Decisions): name the panel, the cache path
  and its size, e.g.
  `Decoding compressed XISF panel <file> (zstd+sh) to <cache path> (2.03 GB) — decoded once per session, re-used after`.
  - CLI: on stderr, boxed by a line of `=` above and below.
  - PI: as a `WorkerMsg::Progress` stage string, `done = total = 1`.
- **Cache location:** `<session dir>/decoded/<16-hex key>.f32`. The key is
  FNV-1a 64 over the canonical path, file size, mtime (ns), image index and
  the block checksum attribute, when present. The cache is written to a
  `.tmp` file and renamed, so an interrupted decode never leaves a valid
  cache.

## Review Focus

1. **Source changed after caching:** a re-saved panel (new mtime or size)
   must not reuse a stale decode. Task 3.5,
   `stale_cache_is_not_reused`.
2. **Interrupted decode:** a leftover `.tmp`, or a cache file of the wrong
   length, is ignored and rewritten. Task 3.5,
   `truncated_cache_is_rebuilt`.
3. **Concurrent workers on one session** (two panels decoding at once)
   must not collide. Unique tmp names plus an atomic rename. Task 3.5,
   `parallel_decodes_do_not_collide`.
4. **Mapped XISF must not be copied:** a full-canvas uncompressed panel
   still reads from its own mmap, and no cache file appears. Task 3.5,
   `mapped_panel_creates_no_cache`.
5. **IPC wire stability:** an IPC session round-trips the same property
   JSON as before. This is pinned by `wire_golden` plus the existing ipc
   tests; Task 3.2 runs both.

---

### Task 3.1 (mpp): `testkit` feature exposing the test encoder

**Files:**
- `crates/mpp-core/Cargo.toml`: `testkit = []`.
- `crates/mpp-core/src/lib.rs`:
  `#[cfg(any(test, feature = "testkit"))] pub mod testkit;`, with the
  module renamed from `testsupport`. Its items become `pub` with docs, and
  `#![cfg_attr(not(feature = "convert"), allow(dead_code))]` stays.
- `crates/mpp-core/src/formats/xisf_codec.rs`: `compress` is gated by
  `#[cfg(any(test, feature = "testkit"))]` and becomes `pub`.
- All `crate::testsupport::` uses become `crate::testkit::`.

- [ ] **Step 1:** RED. `cargo check -p mpp-core --features testkit` with a
  scratch `use mpp_core::testkit::XisfBuilder;` in a doctest fails.
- [ ] **Step 2:** Implement. GREEN. Clippy is clean with and without
  `testkit`, in lean and default mode.
- [ ] **Step 3: Commit:** `build(core): testkit feature exposes the XISF/FITS test encoders`.

### Task 3.2 (mmm): depend on mpp-core; error bridge; shared value types

**Files:**
- `Cargo.toml` (workspace): `mpp-core` path dependency.
- `crates/mmm-core/Cargo.toml`: dependency plus
  `[dev-dependencies] mpp-core = { …, features = ["testkit"] }`.
- `crates/mmm-core/src/error.rs`:
  `impl From<mpp_core::Error> for Error`.
  - `Io` → `Io`.
  - `Format` → `Format`.
  - `Astrometry { reason }` → `Format { path: "", reason: "unusable astrometric solution: …" }`.
  - `Invalid { reason }` → `Compute { reason }`.
- `crates/mmm-core/src/formats/mod.rs`: delete mmm's `FitsKeyword`,
  `XisfProperty`, `PropertyValue`, `SampleFormat`, `RowOrder`,
  `card_number`, `card_string`, `FileFormat` and `sniff_format`, and
  `pub use` them from `mpp_core::formats`. `sniff_format` becomes the mpp
  one. A `Tiff` result is refused by `InputPanel::open` and
  `PanelReader::open_file` with
  `"TIFF panels are not supported (XISF or FITS expected)"`.

- [ ] **Step 1:** Make the change. Run `cargo test -p mmm-core --test wire_golden`,
  the ipc tests and the full suite. Expected: PASS with no fixture change.
- [ ] **Step 2:** Add `mpp_error_converts_keeping_path_and_reason` in
  error.rs.
- [ ] **Step 3: Commit:** `refactor(core): value types and format sniffing come from mpp-core`.

### Task 3.3 (mmm): FITS through mpp

**Files:**
- `crates/mmm-core/src/formats/fits.rs`: becomes
  `pub use mpp_core::formats::fits::{FitsHeader, FitsImage as FitsPanel};`
  plus a doc comment. mpp's `FitsHeader.keywords` replaces
  `fits_keywords`, so update every use (`grep -rn fits_keywords crates`).
- `crates/mmm-core/src/output/fits.rs`: `FitsSink` is a `RowSink` over
  `mpp_core::formats::fits_write::FitsWriter`, and `keywords_for_output`
  is re-exported. Delete the duplicated tests that mpp now owns. Keep one
  `fits_sink_streams_through_row_sink` test.
- Synth: `synth::write_fits` stays (it writes test input).

- [ ] **Step 1:** Make the change. Run the whole suite. `decode_golden`
  and `wcs_golden` must pass unchanged.
- [ ] **Step 2:** Fix any mmm test that asserted the old card format
  (lowercase exponents, `COMMENT =`). Each such edit gets a ledger
  ruling: it is the DESIGN.md follow-up being fixed.
- [ ] **Step 3: Commit:** `refactor(core): FITS reading and writing come from mpp-core`.

### Task 3.4 (mmm): XISF through mpp

**Files:**
- `crates/mmm-core/src/formats/xisf.rs`: becomes
  `pub use mpp_core::formats::xisf::{XisfHeader, XisfImage as XisfPanel, XisfOpenOptions};`.
- `crates/mmm-core/src/formats/mod.rs`: `InputPanel::as_xisf()` returns
  `Some` only when `is_mapped()`. Its doc says so, and that decoded panels
  read through `PanelReader`.
- `crates/mmm-core/src/panel_reader.rs`: no behaviour change yet. A
  non-mapped XISF panel errors with
  `"compressed/non-mappable XISF panels need the decode cache"`, which is
  replaced in 3.5.
- `crates/mmm/src/main.rs`: `info` prints the codec, subblock count,
  checksum algorithm and `mapped`/`decoded` for XISF.

- [ ] **Step 1:** Make the change. Run the whole suite. All three goldens
  must pass unchanged.
- [ ] **Step 2: Commit:** `refactor(core): XISF reading comes from mpp-core`.

### Task 3.5 (mmm): decoded-panel cache

**Files:**
- Create: `crates/mmm-core/src/decoded.rs`.
- Modify: `crates/mmm-core/src/lib.rs` and `crates/mmm-core/src/panel_reader.rs`.
  The `FullCanvasXisf` arm of `open` and the XISF arm of `open_file` use
  the cache when the image is not mapped, giving `Backing::Cache` with the
  full-canvas bbox.

**Interfaces:**
- Produces:
  - `pub fn set_cache_dir(dir: PathBuf)` and
    `pub fn cache_dir() -> PathBuf`, process-wide via a `RwLock`. The
    default is `std::env::temp_dir().join("mmm-decoded")`.
  - `pub struct DecodeNotice { pub source: PathBuf, pub cache: PathBuf, pub bytes: u64, pub codec: String }`.
  - `pub fn set_notifier(f: Box<dyn Fn(&DecodeNotice) + Send + Sync>)`.
    The default notifier is `tracing::warn!`.
  - `pub fn decoded_planes(img: &XisfPanel) -> Result<Mmap>`:
    - It computes the key. A valid cache (exact length
      `w·h·c·4`) is mapped and returned without a notice.
    - Otherwise it calls the notifier, creates `<key>.<pid>.<counter>.tmp`,
      sizes it, maps it with `MmapMut`, and runs `img.decode_into` through
      `bytemuck::cast_slice_mut`.
    - It then flushes, renames the tmp file to `<key>.f32` (if another
      process won the rename, it removes its own tmp file and keeps the
      winner's), and maps the result read-only.

- [ ] **Step 1: Failing tests** (`decoded.rs` unit tests plus
  `panel_reader` tests). Inputs are compressed panels written with
  `mpp_core::testkit::XisfBuilder` (zstd+sh Float32 RGB, 40×30).
  - `compressed_panel_rows_match_uncompressed`: `PanelReader::open_file`
    on the compressed copy gives rows bit-identical to the uncompressed
    copy.
  - `cache_is_reused_and_notified_once`: a notifier counter shows 1 after
    two opens.
  - `stale_cache_is_not_reused`: rewrite the source with different pixels
    and a new mtime; the second open returns the new pixels and notifies
    again.
  - `truncated_cache_is_rebuilt`.
  - `parallel_decodes_do_not_collide`: 4 threads open the same panel and
    all get identical rows.
  - `mapped_panel_creates_no_cache`.
- [ ] **Step 2:** RED. Implement. GREEN.
- [ ] **Step 3: Commit:** `feat(core): decode compressed XISF panels once into a mapped session cache`.

### Task 3.6 (mmm): where the cache lives, and the notices

**Files:**
- `crates/mmm/src/main.rs`: when a command has a session dir (`analyze`,
  `align`, `blend`, …), call `decoded::set_cache_dir(session/decoded)`.
  Install a notifier that prints the boxed notice to stderr.
- `crates/mmm-ipc-worker/src/*`: in its job setup, set the cache dir to
  the job's session dir (or the worker's work dir) and install a notifier
  that sends `WorkerMsg::Progress { stage: <notice text>, done: 1, total: 1 }`
  on the link. Find where the worker sends `Progress` today and reuse that
  sender.
- `crates/mmm-core/src/session.rs`: no schema change. `decoded/` is a
  cache, deletable at any time.

- [ ] **Step 1:** Tests:
  - `notice_text_names_file_cache_and_size` (a pure formatter in
    `decoded.rs`).
  - A CLI integration test in `crates/mmm/tests/cli.rs`:
    `analyze_reads_compressed_xisf_and_says_so`. It writes compressed
    synthetic panels and runs `mmm analyze`. Expected: success, the notice
    on stderr, and `<session>/decoded/*.f32` present.
- [ ] **Step 2:** RED. Implement. GREEN.
- [ ] **Step 3: Commit:** `feat: decode notices in the CLI and the PixInsight console`.

### Task 3.7 (mmm): delete the rest, docs, real-data smoke

- [ ] **Step 1:** Remove dead code left in `formats/` (old parsers and
  tests now covered in mpp). `formats/mod.rs` keeps `InputPanel` and its
  mmm-specific helpers.
- [ ] **Step 2: Docs:**
  - `docs/DESIGN.md`: update the "verified facts" XISF bullet; mark
    "compressed XISF ingest" done; add a "Formats come from mpp-core"
    section (decode cache, notice, path-dependency note).
  - `CLAUDE.md` Layout: mention mpp-core and the sibling checkout.
- [ ] **Step 3: Real-data smoke** (manual; record the results in the
  commit body):
  - Generate zstd+sh copies of registered panels 3, 4, 7 and 8
    (`test_data/orion_mosaic/`), using `tools/pixinsight/gen_xisf_samples.js`
    `panel` mode per panel into `test_data/orion_mosaic_zstd/`.
  - Run `mmm analyze` + `blend` on the original and the compressed sets
    into separate sessions.
  - Expected: identical output FITS pixel data (`cmp` past the header).
    The decode notice is printed once per panel, and a re-run reuses the
    cache with no notice. Record the timings.
- [ ] **Step 4: Commit:** `docs: formats come from mpp-core; compressed XISF smoke results`.
