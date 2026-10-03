# Shared Reference Frame Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let several groups of panels (one per filter) be merged onto one shared pixel grid so the outputs can be combined directly, via an explicit reference-frame file, an `analyze --frame` option, a canvas-wide blend extent, and a `mmm batch` front-end.

**Architecture:** A new `mmm_core::reference` module owns the `ReferenceFrame` type (JSON file, two kinds: `solved` wraps a `MosaicFrame`, `aligned` wraps a canvas geometry plus optional WCS), its header-only derivation over every panel of every group, and the two fit checks (footprint for solved input, canvas/WCS agreement for aligned input). `analyze_full` gains an optional reference it adopts instead of `choose_frame`; `Session` records `frame_imposed`; `BlendParams` gains an `Extent` whose default is the whole canvas for imposed-frame sessions and the content-bbox union otherwise, so existing sessions blend byte-identically. The CLI grows `mmm frame`, `--frame`, `--extent`, and `mmm batch`, which is pure orchestration over the single commands.

**Tech Stack:** Rust 2024 edition workspace; `serde`/`serde_json` (already dependencies) for the frame file; `clap` 4 derive plus the stable `ArgMatches::get_occurrences` for grouped `--group` values; tests synthesize inputs with `mmm_core::synth` (`write_xisf`, `write_xisf_solved`).

**Spec:** `docs/superpowers/specs/2026-10-03-shared-reference-frame-design.md`

## Global Constraints

- No new crate dependencies. `clap` stays on stable features only (no `unstable-v5`; grouped `--group` values come from `ArgMatches::get_occurrences`).
- Tests must not depend on `test_data/`; every input is synthesized (`mmm_core::synth`).
- Every public item in `mmm-core` carries a doc comment (`#![warn(missing_docs)]`); `cargo fmt --check`, `cargo clippy --all-targets`, and `cargo doc` stay warning-free.
- Zero is the no-data sentinel: output pixels outside every panel are exactly `0.0`.
- Existing sessions must blend byte-identically: the constants in `crates/mmm-core/tests/regression_guard.rs` do not change.
- Reference file format version is `1`; `ALIGNED_WCS_TOLERANCE_PX = 0.05`; `FRAME_MARGIN_PX` (16) is unchanged.
- Aligned-mismatch hint text, verbatim: `align every group to one common reference, or process the groups separately`.
- Footprint and aligned mismatches are hard errors (no clipping).
- Group names match `[A-Za-z0-9._-]+` and are unique within one `batch` run.
- Commit messages end with `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.

## Review Focus

1. A `session.json` written before this change (no `frame_imposed` field) must load and blend as `Union` — pinned in Task 5 (`session_json_without_frame_imposed_reads_false`).
2. `--frame` pointing at a missing or non-JSON file must fail naming the file before any pixel scan — pinned in Task 1 (`load_rejects_garbage_naming_the_file`) and Task 7 (`analyze_with_missing_frame_file_fails_naming_it`).
3. A reference derived over an OSC (3-channel) group plus a mono group must succeed; the channel rule is per group — pinned in Task 2 (`derive_allows_mixed_channel_counts`).
4. `batch` with a duplicate group name, or a name containing `/`, must refuse before doing any work — pinned in Task 8 (`batch_rejects_duplicate_and_unsafe_names`).
5. `--roi` combined with the canvas extent must not be clipped to the content union (the LRGB-with-ROI case) — pinned in Task 5 (`roi_intersects_the_active_extent`).

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/mmm-core/src/reference.rs` (new) | `ReferenceFrame` type, `save`/`load`, `derive`, `check_footprints`, `check_aligned`, unit tests |
| `crates/mmm-core/src/astrometry/mod.rs` | `LinearWcs` gains `Serialize`/`Deserialize` |
| `crates/mmm-core/src/align.rs` | `boundary_samples` becomes `pub(crate)` |
| `crates/mmm-core/src/analyze.rs` | `analyze_full(.., reference)` and the two path functions adopt/check the frame |
| `crates/mmm-core/src/session.rs` | `Session::frame_imposed` |
| `crates/mmm-core/src/blend.rs` + `blend_tests.rs` | `Extent`, `BlendParams::extent`, extent rule in `output_bbox` |
| `crates/mmm-core/src/ipc/protocol.rs`, `diag.rs`, tests | `extent: None` in every `BlendParams` literal |
| `crates/mmm-core/src/lib.rs` | `pub mod reference;` and the API map entry |
| `crates/mmm-core/tests/reference.rs` (new) | Integration tests: derive, analyze with a frame, two-group end-to-end |
| `crates/mmm/src/main.rs` | `AnalyzeOpts`/`BlendOpts`, `frame`, `--frame`, `--extent`, `batch` |
| `crates/mmm/tests/cli.rs` (new) | Binary-level tests through `CARGO_BIN_EXE_mmm` |
| `crates/mmm-ipc-worker/src/main.rs` | passes `None` for the new parameter |
| `docs/DESIGN.md` | New section, CLI surface, session layout |

---

### Task 1: `ReferenceFrame` type and JSON persistence

**Files:**
- Create: `crates/mmm-core/src/reference.rs`
- Modify: `crates/mmm-core/src/astrometry/mod.rs:214` (derive on `LinearWcs`)
- Modify: `crates/mmm-core/src/lib.rs:50-71` (module list + API map)

**Interfaces:**
- Consumes: `align::MosaicFrame` (serializable), `astrometry::LinearWcs`, `Error::{io, format}`.
- Produces:
  - `pub const REFERENCE_FRAME_VERSION: u32 = 1`
  - `pub enum ReferenceFrame { Solved { frame: MosaicFrame }, Aligned { width: u64, height: u64, wcs: Option<LinearWcs> } }` (serde internally tagged on `kind`, lowercase)
  - `impl ReferenceFrame { fn kind_name(&self) -> &'static str; fn canvas(&self) -> (u64, u64); fn describe(&self) -> String; fn save(&self, path: &Path) -> Result<()>; fn load(path: &Path) -> Result<ReferenceFrame> }`

- [ ] **Step 1: Make `LinearWcs` serializable**

In `crates/mmm-core/src/astrometry/mod.rs`, change the derive on `LinearWcs` (line 214) to:

```rust
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LinearWcs {
```

- [ ] **Step 2: Write the module with failing tests**

Create `crates/mmm-core/src/reference.rs`:

```rust
//! Shared reference frame: the one piece of state several sessions adopt so
//! that their blended outputs land on a common pixel grid — one mosaic per
//! filter for a mono imager, combined afterwards (LRGB, Ha+RGB, …).
//!
//! A [`ReferenceFrame`] is persisted as `<name>.mmm-frame.json`, derived
//! header-only from *every* panel of *every* group by [`derive`], and handed
//! to the analyze stage ([`crate::analyze::analyze_full`] with
//! `Some(&frame)`), which adopts it instead of choosing its own frame and
//! checks that the group fits it ([`check_footprints`], [`check_aligned`]).
//! Design: `docs/superpowers/specs/2026-10-03-shared-reference-frame-design.md`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::align::{MosaicFrame, choose_frame};
use crate::analyze::InputSelect;
use crate::astrometry::{LinearWcs, WcsModel};
use crate::formats::InputPanel;
use crate::{Error, Result};

/// On-disk format version of `*.mmm-frame.json` files this build writes and
/// reads.
pub const REFERENCE_FRAME_VERSION: u32 = 1;

/// A reference frame shared by several sessions so their outputs land on one
/// pixel grid. Persisted as `<name>.mmm-frame.json` (see [`ReferenceFrame::save`]).
///
/// No channel count is recorded: an OSC RGB mosaic and a mono Ha mosaic may
/// share a frame and be combined later.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ReferenceFrame {
    /// Solved input: the mosaic frame every group reprojects onto.
    Solved {
        /// The frame, exactly as `session.json` stores it for solved sessions.
        frame: MosaicFrame,
    },
    /// Aligned input: the shared canvas geometry and, when the panels carry
    /// one, the canvas WCS every group must agree with.
    Aligned {
        /// Canvas width in pixels.
        width: u64,
        /// Canvas height in pixels.
        height: u64,
        /// Canvas solution of the first panel the frame was derived from
        /// (top-down convention), `None` when that panel carried no WCS.
        wcs: Option<LinearWcs>,
    },
}

/// The file wrapper: a top-level `version` beside the frame's own fields.
#[derive(Serialize, Deserialize)]
struct ReferenceFrameFile {
    version: u32,
    #[serde(flatten)]
    frame: ReferenceFrame,
}

impl ReferenceFrame {
    /// The `kind` tag as written in the file: `"solved"` or `"aligned"`.
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Solved { .. } => "solved",
            Self::Aligned { .. } => "aligned",
        }
    }

    /// Canvas `(width, height)` in pixels that the frame prescribes.
    pub fn canvas(&self) -> (u64, u64) {
        match self {
            Self::Solved { frame } => (frame.width, frame.height),
            Self::Aligned { width, height, .. } => (*width, *height),
        }
    }

    /// One-line human summary for console output.
    pub fn describe(&self) -> String {
        match self {
            Self::Solved { frame } => format!(
                "solved frame {}x{} px, {:.3}\"/px, center RA {:.4} Dec {:+.4}, rotation {:.2}°",
                frame.width,
                frame.height,
                frame.scale_deg * 3600.0,
                frame.crval[0],
                frame.crval[1],
                frame.rotation_deg
            ),
            Self::Aligned { width, height, wcs } => format!(
                "aligned canvas {width}x{height} px{}",
                if wcs.is_some() {
                    " with canvas WCS"
                } else {
                    " (no canvas WCS)"
                }
            ),
        }
    }

    /// Write the frame as versioned, pretty-printed JSON to `path`, creating
    /// parent directories.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
            }
        }
        let file = ReferenceFrameFile {
            version: REFERENCE_FRAME_VERSION,
            frame: self.clone(),
        };
        let json = serde_json::to_string_pretty(&file)
            .map_err(|e| Error::format(path, format!("cannot serialize reference frame: {e}")))?;
        std::fs::write(path, json).map_err(|e| Error::io(path, e))
    }

    /// Read a frame written by [`ReferenceFrame::save`]. Malformed files and
    /// other format versions are refused with a message naming `path`.
    pub fn load(path: &Path) -> Result<ReferenceFrame> {
        let json = std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
        let file: ReferenceFrameFile = serde_json::from_str(&json)
            .map_err(|e| Error::format(path, format!("not a reference frame file: {e}")))?;
        if file.version != REFERENCE_FRAME_VERSION {
            return Err(Error::format(
                path,
                format!(
                    "reference frame version {} is not supported (this build reads version \
                     {REFERENCE_FRAME_VERSION})",
                    file.version
                ),
            ));
        }
        Ok(file.frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mmm-reference-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample_frame() -> MosaicFrame {
        MosaicFrame {
            crval: [66.0, 18.0],
            scale_deg: 1.0e-3,
            width: 400,
            height: 300,
            rotation_deg: 1.5,
        }
    }

    fn sample_wcs() -> LinearWcs {
        LinearWcs {
            crval: [66.0, 18.0],
            crpix: [120.5, 100.5],
            cd: [[-1.0e-3, 0.0], [0.0, 1.0e-3]],
            ctype: ["RA---TAN".into(), "DEC--TAN".into()],
            radesys: "ICRS".into(),
        }
    }

    #[test]
    fn solved_round_trips_through_json() {
        let dir = tmp("solved");
        let path = dir.join("ref.mmm-frame.json");
        let original = ReferenceFrame::Solved {
            frame: sample_frame(),
        };
        original.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"version\": 1"), "{text}");
        assert!(text.contains("\"kind\": \"solved\""), "{text}");
        assert_eq!(ReferenceFrame::load(&path).unwrap(), original);
        assert_eq!(original.kind_name(), "solved");
        assert_eq!(original.canvas(), (400, 300));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn aligned_round_trips_with_and_without_wcs() {
        let dir = tmp("aligned");
        for wcs in [Some(sample_wcs()), None] {
            let path = dir.join("ref.mmm-frame.json");
            let original = ReferenceFrame::Aligned {
                width: 240,
                height: 200,
                wcs,
            };
            original.save(&path).unwrap();
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(text.contains("\"kind\": \"aligned\""), "{text}");
            assert_eq!(ReferenceFrame::load(&path).unwrap(), original);
            assert_eq!(original.canvas(), (240, 200));
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn save_creates_parent_directories() {
        let dir = tmp("parents");
        let path = dir.join("nested").join("deeper").join("ref.mmm-frame.json");
        ReferenceFrame::Aligned {
            width: 1,
            height: 1,
            wcs: None,
        }
        .save(&path)
        .unwrap();
        assert!(path.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_rejects_other_versions() {
        let dir = tmp("version");
        let path = dir.join("ref.mmm-frame.json");
        std::fs::write(
            &path,
            r#"{"version": 2, "kind": "aligned", "width": 1, "height": 1, "wcs": null}"#,
        )
        .unwrap();
        let err = ReferenceFrame::load(&path).unwrap_err().to_string();
        assert!(err.contains("version 2"), "{err}");
        assert!(err.contains("ref.mmm-frame.json"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_rejects_garbage_naming_the_file() {
        let dir = tmp("garbage");
        let path = dir.join("nope.mmm-frame.json");
        std::fs::write(&path, "this is not json").unwrap();
        let err = ReferenceFrame::load(&path).unwrap_err().to_string();
        assert!(err.contains("not a reference frame file"), "{err}");
        assert!(err.contains("nope.mmm-frame.json"), "{err}");
        let missing = ReferenceFrame::load(&dir.join("absent.json")).unwrap_err().to_string();
        assert!(missing.contains("absent.json"), "{missing}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
```

The `use` lines for `choose_frame`, `InputSelect`, `WcsModel`, and `InputPanel` are consumed by Tasks 2 and 3; until then they produce unused-import warnings, which is acceptable inside one task sequence but **not at the end of Task 3** (clippy must be clean).

- [ ] **Step 3: Register the module in `lib.rs`**

In `crates/mmm-core/src/lib.rs`, add after `pub mod pyramid;`:

```rust
pub mod reference;
```

and in the API map (the `//!` block), after the `[`astrometry`] + [`align`]` bullet, add:

```rust
//! - [`reference`] — the shared [`reference::ReferenceFrame`] several
//!   sessions adopt so their outputs share one pixel grid (multi-filter
//!   mosaics): derivation over every group's panels, persistence as
//!   `*.mmm-frame.json`, and the fit checks analyze applies.
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p mmm-core reference::tests`
Expected: 5 passed. (If `serde(flatten)` over the internally tagged enum fails to round-trip, replace the wrapper with a `version: u32` field on *each* variant plus a manual check in `load`; the tests above stay as the acceptance criterion.)

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add crates/mmm-core/src/reference.rs crates/mmm-core/src/lib.rs crates/mmm-core/src/astrometry/mod.rs
git commit -m "feat(reference): ReferenceFrame type with versioned JSON persistence

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 2: `reference::derive` (header-only frame derivation)

**Files:**
- Modify: `crates/mmm-core/src/reference.rs`
- Create: `crates/mmm-core/tests/reference.rs`

**Interfaces:**
- Consumes: `InputPanel::{open, width, height, channels, wcs_model, linear_wcs}`, `align::choose_frame`, `analyze::InputSelect`.
- Produces: `pub fn derive(paths: &[PathBuf], input: InputSelect) -> Result<ReferenceFrame>`.
- Produces (test fixture, reused by Tasks 4 and 6): the helper functions in `tests/reference.rs` below.

- [ ] **Step 1: Write the failing integration tests**

Create `crates/mmm-core/tests/reference.rs`:

```rust
//! Integration tests for the shared reference frame: header-only derivation,
//! analyze adopting an imposed frame (and refusing groups that do not fit),
//! and the two-group end-to-end guarantee that outputs share one pixel grid.

use std::path::{Path, PathBuf};

use mmm_core::Result;
use mmm_core::analyze::{InputSelect, analyze_full};
use mmm_core::astrometry::LinearWcs;
use mmm_core::blend::{BlendMode, BlendParams, RowSink, blend, union_bbox};
use mmm_core::overlap::OverlapGraph;
use mmm_core::photometry::{GainMode, Photometry};
use mmm_core::reference::{ReferenceFrame, derive};
use mmm_core::session::{InputKind, Session};
use mmm_core::surfaces::Surfaces;
use mmm_core::synth::{SynthWcs, write_xisf, write_xisf_solved};

/// Pixel scale of every synthetic solution, degrees per pixel (3.6″).
const S: f64 = 1.0e-3;
/// Sky position (RA, Dec) of the planted star — also the layout centre.
const STAR: [f64; 2] = [66.0, 18.0];

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mmm-reference-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Noiseless analytic sky: flat background 0.05 plus one Gaussian star
/// (σ = 2 px, amplitude 0.8) at [`STAR`]. Distances are measured in pixels
/// of scale [`S`] on the tangent plane about the star.
fn sky(ra: f64, dec: f64) -> f32 {
    let dx = (ra - STAR[0]) * STAR[1].to_radians().cos() / S;
    let dy = (dec - STAR[1]) / S;
    (0.05 + 0.8 * (-(dx * dx + dy * dy) / 8.0).exp()) as f32
}

/// Sky position `(dx, dy)` pixels from [`STAR`] (RA offset corrected for cos δ).
fn offset_px(dx: f64, dy: f64) -> [f64; 2] {
    [
        STAR[0] + dx * S / STAR[1].to_radians().cos(),
        STAR[1] + dy * S,
    ]
}

/// A north-up linear solution centred on `crval` for a `w`×`h` panel, in both
/// the FITS form the test samples with and the form the XISF writer takes.
fn north_up(crval: [f64; 2], w: u64, h: u64) -> (LinearWcs, SynthWcs) {
    let cd = [[-S, 0.0], [0.0, S]];
    let refimg = [w as f64 / 2.0, h as f64 / 2.0];
    (
        LinearWcs {
            crval,
            crpix: [refimg[0] + 0.5, refimg[1] + 0.5],
            cd,
            ctype: ["RA---TAN".into(), "DEC--TAN".into()],
            radesys: "ICRS".into(),
        },
        SynthWcs { crval, refimg, cd },
    )
}

/// Write one raw plate-solved XISF panel (`ch` channels, all identical) of
/// `w`×`h` px centred on `crval`; pixel (i, j) carries the sky at FITS
/// coordinate (i + 1, j + 1) of its own solution.
fn write_raw_panel(path: &Path, w: u64, h: u64, ch: u64, crval: [f64; 2]) {
    let (lin, synth) = north_up(crval, w, h);
    let mut plane = vec![0.0f32; (w * h) as usize];
    for j in 0..h {
        for i in 0..w {
            let (ra, dec) = lin.pixel_to_sky(i as f64 + 1.0, j as f64 + 1.0);
            plane[(j * w + i) as usize] = sky(ra, dec);
        }
    }
    let planes: Vec<f32> = (0..ch).flat_map(|_| plane.iter().copied()).collect();
    write_xisf_solved(path, w, h, ch, &planes, &synth).unwrap();
}

/// A group of two overlapping raw mono panels around [`STAR`]: 160×120 at
/// −60 px and `w2`×120 at +60 px (≈ 40 px overlap containing the star).
/// `shift` displaces the whole group on the sky, in pixels, so two groups can
/// have different footprints.
fn write_solved_group(dir: &Path, tag: &str, shift: (f64, f64), w2: u64) -> Vec<PathBuf> {
    std::fs::create_dir_all(dir).unwrap();
    let a = dir.join(format!("{tag}_00.xisf"));
    let b = dir.join(format!("{tag}_01.xisf"));
    write_raw_panel(&a, 160, 120, 1, offset_px(-60.0 + shift.0, shift.1));
    write_raw_panel(&b, w2, 120, 1, offset_px(60.0 + shift.0, shift.1));
    vec![a, b]
}

/// Canvas geometry of every synthetic aligned (registered) frame.
const CANVAS: (u64, u64) = (240, 200);

/// Write one full-canvas mono frame carrying the canvas WCS centred on
/// `canvas_crval`, with `value` inside `window` `[x0, y0, x1, y1]` and zeros
/// elsewhere (a MosaicByCoordinates-style registered panel).
fn write_canvas_panel(path: &Path, canvas_crval: [f64; 2], window: [u64; 4], value: f32) {
    let (w, h) = CANVAS;
    let (_, synth) = north_up(canvas_crval, w, h);
    let mut plane = vec![0.0f32; (w * h) as usize];
    for y in window[1]..window[3] {
        for x in window[0]..window[2] {
            plane[(y * w + x) as usize] = value;
        }
    }
    write_xisf_solved(path, w, h, 1, &plane, &synth).unwrap();
}

/// A group of two registered 100×120 panels overlapping by 30 px, placed at
/// `(dx, dy)` on the shared canvas (coverage 25% each, so auto-detect reads
/// the set as aligned).
fn write_aligned_group(dir: &Path, tag: &str, canvas_crval: [f64; 2], dx: u64, dy: u64) -> Vec<PathBuf> {
    std::fs::create_dir_all(dir).unwrap();
    let a = dir.join(format!("{tag}_00.xisf"));
    let b = dir.join(format!("{tag}_01.xisf"));
    write_canvas_panel(&a, canvas_crval, [dx, dy, dx + 100, dy + 120], 0.4);
    write_canvas_panel(&b, canvas_crval, [dx + 70, dy, dx + 170, dy + 120], 0.4);
    vec![a, b]
}

// ---------------------------------------------------------------------------
// derive

#[test]
fn derive_mixed_geometries_is_solved_over_all_panels() {
    let dir = tempdir("derive-solved");
    let a = write_solved_group(&dir.join("A"), "a", (0.0, 0.0), 150);
    let b = write_solved_group(&dir.join("B"), "b", (40.0, 25.0), 140);
    let all: Vec<PathBuf> = a.iter().chain(&b).cloned().collect();
    let r = derive(&all, InputSelect::Auto).unwrap();
    let ReferenceFrame::Solved { frame } = &r else {
        panic!("expected a solved reference, got {r:?}");
    };
    // Union of both groups: x spans [−140, 170] px (310) plus 16 px margins.
    assert!(frame.width >= 310 + 32 && frame.width <= 310 + 36, "width {}", frame.width);
    assert!((frame.scale_deg - S).abs() < 1e-12);
    // Only the first group fits inside a frame derived from it alone.
    let only_a = derive(&a, InputSelect::Solved).unwrap();
    assert!(only_a.canvas().0 < frame.width);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn derive_equal_geometries_is_aligned_with_canvas_wcs() {
    let dir = tempdir("derive-aligned");
    let a = write_aligned_group(&dir.join("A"), "a", STAR, 10, 20);
    let b = write_aligned_group(&dir.join("B"), "b", STAR, 30, 40);
    let all: Vec<PathBuf> = a.iter().chain(&b).cloned().collect();
    let r = derive(&all, InputSelect::Auto).unwrap();
    match r {
        ReferenceFrame::Aligned { width, height, wcs: Some(wcs) } => {
            assert_eq!((width, height), CANVAS);
            assert!((wcs.crval[0] - STAR[0]).abs() < 1e-9 && (wcs.crval[1] - STAR[1]).abs() < 1e-9);
        }
        other => panic!("expected an aligned reference with WCS, got {other:?}"),
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn derive_honours_input_override_both_ways() {
    let dir = tempdir("derive-override");
    // Same-geometry raw panels: auto reads them as aligned (documented), solved forces a frame.
    let same = write_solved_group(&dir.join("same"), "s", (0.0, 0.0), 160);
    assert!(matches!(derive(&same, InputSelect::Auto).unwrap(), ReferenceFrame::Aligned { width: 160, height: 120, .. }));
    assert!(matches!(derive(&same, InputSelect::Solved).unwrap(), ReferenceFrame::Solved { .. }));
    // Registered canvases carry solutions too: solved forces a fresh frame around the canvas.
    let canvases = write_aligned_group(&dir.join("canv"), "c", STAR, 10, 20);
    let forced = derive(&canvases, InputSelect::Solved).unwrap();
    assert!(matches!(forced, ReferenceFrame::Solved { .. }));
    let w = forced.canvas().0;
    assert!(w >= CANVAS.0 + 32 && w <= CANVAS.0 + 33, "canvas plus margins, got {w}");
    assert!(matches!(derive(&canvases, InputSelect::Aligned).unwrap(), ReferenceFrame::Aligned { .. }));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn derive_aligned_override_needs_one_geometry() {
    let dir = tempdir("derive-aligned-mismatch");
    let mixed = write_solved_group(&dir.join("m"), "m", (0.0, 0.0), 150);
    let err = derive(&mixed, InputSelect::Aligned).unwrap_err().to_string();
    assert!(err.contains("160x120") && err.contains("150x120"), "{err}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn derive_without_solution_lists_every_unsolved_file() {
    let dir = tempdir("derive-unsolved");
    let solved = write_solved_group(&dir.join("s"), "s", (0.0, 0.0), 150);
    let plain = dir.join("plain.xisf");
    write_xisf(&plain, 100, 80, 1, &vec![0.3f32; 100 * 80]).unwrap();
    let mut all = solved;
    all.push(plain);
    let err = derive(&all, InputSelect::Auto).unwrap_err().to_string();
    assert!(err.contains("astrometric solution in every panel"), "{err}");
    assert!(err.contains("plain.xisf"), "{err}");
    assert!(!err.contains("s_00.xisf"), "solved files must not be listed: {err}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn derive_allows_mixed_channel_counts() {
    let dir = tempdir("derive-channels");
    let mono = write_solved_group(&dir.join("mono"), "m", (0.0, 0.0), 150);
    let rgb = dir.join("rgb.xisf");
    write_raw_panel(&rgb, 140, 110, 3, offset_px(30.0, 10.0));
    let mut all = mono;
    all.push(rgb);
    let r = derive(&all, InputSelect::Auto).unwrap();
    assert!(matches!(r, ReferenceFrame::Solved { .. }), "{r:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn derive_refuses_empty_input() {
    let err = derive(&[], InputSelect::Auto).unwrap_err().to_string();
    assert!(err.contains("no input panels"), "{err}");
}
```

(The `analyze_full`, `blend`, `Session`, etc. imports are used from Task 4 onward; leave them in place.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p mmm-core --test reference`
Expected: compile error, `derive` not found in `mmm_core::reference`.

- [ ] **Step 3: Implement `derive`**

Append to `crates/mmm-core/src/reference.rs` (before the `#[cfg(test)]` module):

```rust
/// Derive the shared reference frame from the headers of `paths` — every
/// panel of every group — without scanning pixels.
///
/// Kind selection follows the cheap half of analyze's auto-detect rule:
/// `aligned` when `input` forces it, or with `Auto` when there are ≥ 2 panels
/// and every panel has the same `(width, height)`; otherwise `solved`, where
/// every panel must yield a [`WcsModel`] and the frame is
/// [`choose_frame`] over all of them. The coverage half of the rule (≥ 50 %
/// covered re-dispatches to solved) needs a scan and is deliberately not
/// applied: a same-geometry raw-panel set must pass `InputSelect::Solved`,
/// exactly as analyze requires.
///
/// Channel counts may differ across `paths`; groups are checked for channel
/// uniformity individually by analyze.
pub fn derive(paths: &[PathBuf], input: InputSelect) -> Result<ReferenceFrame> {
    if paths.is_empty() {
        return Err(Error::compute("no input panels given"));
    }
    let opened: Vec<InputPanel> = paths.iter().map(|p| InputPanel::open(p)).collect::<Result<_>>()?;
    let geoms: Vec<(u64, u64)> = opened.iter().map(|x| (x.width(), x.height())).collect();
    let same_geometry = geoms.iter().all(|g| *g == geoms[0]);
    let aligned = match input {
        InputSelect::Aligned => true,
        InputSelect::Solved => false,
        InputSelect::Auto => paths.len() >= 2 && same_geometry,
    };
    if aligned {
        if let Some(k) = geoms.iter().position(|g| *g != geoms[0]) {
            return Err(Error::compute(format!(
                "aligned input needs one canvas geometry, but {} is {}x{} and {} is {}x{}",
                paths[0].display(),
                geoms[0].0,
                geoms[0].1,
                paths[k].display(),
                geoms[k].0,
                geoms[k].1
            )));
        }
        return Ok(ReferenceFrame::Aligned {
            width: geoms[0].0,
            height: geoms[0].1,
            wcs: opened[0].linear_wcs(),
        });
    }
    let mut models: Vec<WcsModel> = Vec::with_capacity(paths.len());
    let mut errors: Vec<String> = Vec::new();
    for (x, path) in opened.iter().zip(paths) {
        match x.wcs_model() {
            Ok(m) => models.push(m),
            Err(reason) => errors.push(format!("{}: {reason}", path.display())),
        }
    }
    if !errors.is_empty() {
        return Err(Error::compute(format!(
            "solved input requires an astrometric solution in every panel:\n  {}",
            errors.join("\n  ")
        )));
    }
    Ok(ReferenceFrame::Solved {
        frame: choose_frame(&models),
    })
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p mmm-core --test reference`
Expected: the 7 `derive_*` tests pass. If `derive_mixed_geometries_is_solved_over_all_panels` fails on the width bound, print `frame.width`: the union is 310 px (panels at centres −60, +60, −20, +100 with widths 160, 150, 160, 140) plus 32 px of margin, so anything outside 342–346 is a fixture bug, not a bound to widen.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add crates/mmm-core/src/reference.rs crates/mmm-core/tests/reference.rs
git commit -m "feat(reference): header-only derivation of a shared frame over every group

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 3: Fit checks — `check_footprints` and `check_aligned`

**Files:**
- Modify: `crates/mmm-core/src/reference.rs`
- Modify: `crates/mmm-core/src/align.rs:143` (`fn boundary_samples` → `pub(crate) fn boundary_samples`)

**Interfaces:**
- Consumes: `align::boundary_samples(w, h) -> Vec<(f64, f64)>`, `WcsModel::{width, height, pixel_to_sky}`, `MosaicFrame::linear_wcs()`, `LinearWcs::{pixel_to_sky, sky_to_pixel}`.
- Produces:
  - `pub const ALIGNED_WCS_TOLERANCE_PX: f64 = 0.05`
  - `pub fn check_footprints<'a, I: IntoIterator<Item = (String, &'a WcsModel)>>(panels: I, frame: &MosaicFrame) -> std::result::Result<(), String>`
  - `pub fn check_aligned(canvas: (u64, u64), panel_wcs: Option<&LinearWcs>, width: u64, height: u64, reference_wcs: Option<&LinearWcs>) -> std::result::Result<(), String>`

- [ ] **Step 1: Make `boundary_samples` crate-visible**

In `crates/mmm-core/src/align.rs` line 143: `pub(crate) fn boundary_samples(w: f64, h: f64) -> Vec<(f64, f64)> {`.

- [ ] **Step 2: Write the failing unit tests**

Add inside the `mod tests` of `reference.rs`:

```rust
    use crate::astrometry::WcsModel;

    /// A north-up linear model of `w`×`h` px centred on `crval` at 1e-3 °/px.
    fn model(crval: [f64; 2], w: u64, h: u64) -> WcsModel {
        let lin = LinearWcs {
            crval,
            crpix: [w as f64 / 2.0 + 0.5, h as f64 / 2.0 + 0.5],
            cd: [[-1.0e-3, 0.0], [0.0, 1.0e-3]],
            ctype: ["RA---TAN".into(), "DEC--TAN".into()],
            radesys: "ICRS".into(),
        };
        WcsModel::linear_only(lin, w, h)
    }

    #[test]
    fn footprints_inside_the_frame_pass() {
        let m = model([66.0, 18.0], 200, 150);
        let frame = choose_frame(std::slice::from_ref(&m));
        check_footprints([("p0".to_string(), &m)], &frame).unwrap();
    }

    #[test]
    fn footprint_beyond_an_edge_is_named_with_side_and_overshoot() {
        let m = model([66.0, 18.0], 200, 150);
        let frame = choose_frame(std::slice::from_ref(&m));
        // 40 px north of the frame centre: exceeds the 16 px margin by ~24 px
        // on one side (y grows north with cd[1][1] > 0 → the bottom edge in
        // top-down rows).
        let shifted = model([66.0, 18.0 + 40.0e-3], 200, 150);
        let err = check_footprints(
            [("fits.xisf".to_string(), &m), ("P3_Ha.xisf".to_string(), &shifted)],
            &frame,
        )
        .unwrap_err();
        assert!(err.contains("P3_Ha.xisf"), "{err}");
        assert!(!err.contains("fits.xisf"), "{err}");
        assert!(err.contains("24 px beyond the bottom edge"), "{err}");
        assert!(err.contains("mmm frame"), "{err}");
    }

    #[test]
    fn aligned_check_accepts_matching_canvas_and_wcs() {
        let wcs = sample_wcs();
        check_aligned((240, 200), Some(&wcs), 240, 200, Some(&wcs)).unwrap();
        check_aligned((240, 200), None, 240, 200, Some(&wcs)).unwrap();
        check_aligned((240, 200), Some(&wcs), 240, 200, None).unwrap();
    }

    #[test]
    fn aligned_check_refuses_other_canvas_with_the_hint() {
        let err = check_aligned((230, 200), None, 240, 200, None).unwrap_err();
        assert!(err.contains("230x200"), "{err}");
        assert!(err.contains("240x200"), "{err}");
        assert!(
            err.contains("align every group to one common reference, or process the groups separately"),
            "{err}"
        );
    }

    #[test]
    fn aligned_check_refuses_displaced_wcs_quoting_pixels() {
        let reference = sample_wcs();
        let mut shifted = reference.clone();
        shifted.crpix[0] += 2.0; // the same sky lands 2 px to the right
        let err = check_aligned((240, 200), Some(&shifted), 240, 200, Some(&reference)).unwrap_err();
        assert!(err.contains("2.00 px"), "{err}");
        assert!(err.contains("align every group to one common reference"), "{err}");
        // Within tolerance: a 0.01 px shift passes.
        let mut tiny = reference.clone();
        tiny.crpix[0] += 0.01;
        check_aligned((240, 200), Some(&tiny), 240, 200, Some(&reference)).unwrap();
    }
```

`WcsModel::linear_only(linear, width, height)` is the existing `pub(crate)` constructor at `crates/mmm-core/src/astrometry/mod.rs:434`; unit tests inside the crate can call it directly.

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p mmm-core reference::tests`
Expected: compile errors for `check_footprints`, `check_aligned`, `ALIGNED_WCS_TOLERANCE_PX`.

- [ ] **Step 4: Implement the checks**

Append to `reference.rs` (before `#[cfg(test)]`):

```rust
/// Largest displacement, in canvas pixels over the canvas corners and
/// centre, allowed between a group's canvas WCS and the reference frame's
/// before aligned input is refused ([`check_aligned`]).
pub const ALIGNED_WCS_TOLERANCE_PX: f64 = 0.05;

/// The hint every aligned-input refusal ends with.
const ALIGNED_HINT: &str =
    "align every group to one common reference, or process the groups separately";

/// Footprint check for solved input against an imposed frame: every panel's
/// boundary, mapped through its own model and then into `frame`, must lie
/// inside the frame's canvas. Violations are aggregated into one message,
/// one line per panel naming it, the side(s) and the overshoot in whole
/// pixels (rounded up). No clipping: a hard refusal by design.
pub fn check_footprints<'a, I>(panels: I, frame: &MosaicFrame) -> std::result::Result<(), String>
where
    I: IntoIterator<Item = (String, &'a WcsModel)>,
{
    let lin = frame.linear_wcs();
    let (w, h) = (frame.width as f64, frame.height as f64);
    let mut lines: Vec<String> = Vec::new();
    for (label, m) in panels {
        // Overshoot per side (left, right, top, bottom), pixels; the canvas
        // spans FITS coordinates [0.5, w + 0.5] × [0.5, h + 0.5].
        let mut over = [0.0f64; 4];
        for (px, py) in crate::align::boundary_samples(m.width as f64, m.height as f64) {
            let (ra, dec) = m.pixel_to_sky(px, py);
            let (fx, fy) = lin.sky_to_pixel(ra, dec);
            over[0] = over[0].max(0.5 - fx);
            over[1] = over[1].max(fx - (w + 0.5));
            over[2] = over[2].max(0.5 - fy);
            over[3] = over[3].max(fy - (h + 0.5));
        }
        let sides = ["left", "right", "top", "bottom"];
        let parts: Vec<String> = over
            .iter()
            .zip(sides)
            .filter(|(o, _)| **o > 0.0)
            .map(|(o, side)| format!("{} px beyond the {side} edge", o.ceil() as u64))
            .collect();
        if !parts.is_empty() {
            lines.push(format!(
                "{label} extends {} of the reference frame ({}x{} px)",
                parts.join(" and "),
                frame.width,
                frame.height
            ));
        }
    }
    if lines.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{}\n  re-derive the frame over the full panel set with `mmm frame`, or exclude the panel",
        lines.join("\n  ")
    ))
}

/// Aligned-input check against an imposed frame: the group's `canvas`
/// `(width, height)` must equal the reference's, and when both the group's
/// first panel and the reference carry a canvas WCS, mapping the canvas
/// corners and centre through the panel's solution and back through the
/// reference's must move no point by more than
/// [`ALIGNED_WCS_TOLERANCE_PX`]. A missing WCS on either side passes on
/// geometry alone.
pub fn check_aligned(
    canvas: (u64, u64),
    panel_wcs: Option<&LinearWcs>,
    width: u64,
    height: u64,
    reference_wcs: Option<&LinearWcs>,
) -> std::result::Result<(), String> {
    if canvas != (width, height) {
        return Err(format!(
            "canvas {}x{} of this group does not match the reference frame ({width}x{height}): \
             {ALIGNED_HINT}",
            canvas.0, canvas.1
        ));
    }
    if let (Some(p), Some(r)) = (panel_wcs, reference_wcs) {
        let (w, h) = (width as f64, height as f64);
        let samples = [
            (1.0, 1.0),
            (w, 1.0),
            (1.0, h),
            (w, h),
            ((w + 1.0) / 2.0, (h + 1.0) / 2.0),
        ];
        let mut worst = 0.0f64;
        for (x, y) in samples {
            let (ra, dec) = p.pixel_to_sky(x, y);
            let (rx, ry) = r.sky_to_pixel(ra, dec);
            worst = worst.max((rx - x).hypot(ry - y));
        }
        if worst > ALIGNED_WCS_TOLERANCE_PX {
            return Err(format!(
                "canvas WCS of this group is displaced up to {worst:.2} px from the reference \
                 frame's (tolerance {ALIGNED_WCS_TOLERANCE_PX} px): {ALIGNED_HINT}"
            ));
        }
    }
    Ok(())
}
```

- [ ] **Step 5: Run the tests, then clippy**

Run: `cargo test -p mmm-core reference::tests && cargo clippy -p mmm-core --all-targets`
Expected: 10 tests pass; no warnings (all `use` lines in `reference.rs` are now consumed). If the footprint overshoot in `footprint_beyond_an_edge_is_named_with_side_and_overshoot` prints 23 or 25 instead of 24 (TAN curvature at 40 px is negligible, but `ceil` of `24.0 ± 1e-9` may land either way), change the test to accept `"23 px"`, `"24 px"` or `"25 px"` on the `bottom` edge rather than touching the implementation.

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add crates/mmm-core/src/reference.rs crates/mmm-core/src/align.rs crates/mmm-core/src/astrometry/mod.rs
git commit -m "feat(reference): footprint and aligned-canvas fit checks against an imposed frame

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 4: `Session::frame_imposed` and `analyze_full(.., reference)`

**Files:**
- Modify: `crates/mmm-core/src/session.rs:67-112` (field + `create`)
- Modify: `crates/mmm-core/src/analyze.rs:146-215` (wrappers), `:216-262` (`analyze_full`), `:267-335` (`analyze_aligned`), `:338-456` (`analyze_solved`)
- Modify: `crates/mmm/src/main.rs:259`, `crates/mmm-ipc-worker/src/main.rs:185` (pass `None`)
- Modify: `crates/mmm-core/tests/reference.rs` (tests)

**Interfaces:**
- Consumes: `reference::{ReferenceFrame, check_footprints, check_aligned}`.
- Produces:
  - `Session { ..., #[serde(default)] pub frame_imposed: bool }`
  - `pub fn analyze_full(paths, session_dir, surface_order, gain, input, progress, reference: Option<&ReferenceFrame>) -> Result<Session>` (new trailing parameter; `analyze_gain` and `analyze_input_progress` pass `None`).

- [ ] **Step 1: Write the failing tests**

Append to `crates/mmm-core/tests/reference.rs`:

```rust
// ---------------------------------------------------------------------------
// analyze with an imposed frame

fn analyze(paths: &[PathBuf], dir: &Path, input: InputSelect, reference: Option<&ReferenceFrame>) -> Result<Session> {
    analyze_full(paths, dir, Some(2), GainMode::Fit, input, None, reference)
}

#[test]
fn solved_group_adopts_the_imposed_frame() {
    let dir = tempdir("adopt-solved");
    let a = write_solved_group(&dir.join("A"), "a", (0.0, 0.0), 150);
    let b = write_solved_group(&dir.join("B"), "b", (40.0, 25.0), 140);
    let all: Vec<PathBuf> = a.iter().chain(&b).cloned().collect();
    let reference = derive(&all, InputSelect::Auto).unwrap();
    let ReferenceFrame::Solved { frame } = &reference else { unreachable!() };

    let sa = analyze(&a, &dir.join("a.mmm-session"), InputSelect::Solved, Some(&reference)).unwrap();
    assert_eq!(sa.input, InputKind::Solved);
    assert!(sa.frame_imposed);
    assert_eq!(sa.frame.as_ref(), Some(frame));
    assert_eq!(sa.canvas, (frame.width, frame.height, 1));
    // Persisted and read back.
    let reopened = Session::open(&dir.join("a.mmm-session")).unwrap();
    assert!(reopened.frame_imposed);
    assert_eq!(reopened.frame.as_ref(), Some(frame));

    // Without a reference the group gets its own, smaller frame.
    let own = analyze(&a, &dir.join("own.mmm-session"), InputSelect::Solved, None).unwrap();
    assert!(!own.frame_imposed);
    assert!(own.canvas.0 < sa.canvas.0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn panel_outside_the_imposed_frame_is_refused_by_name() {
    let dir = tempdir("footprint");
    let a = write_solved_group(&dir.join("A"), "a", (0.0, 0.0), 150);
    let b = write_solved_group(&dir.join("B"), "b", (40.0, 25.0), 140);
    let only_a = derive(&a, InputSelect::Solved).unwrap();
    let err = analyze(&b, &dir.join("b.mmm-session"), InputSelect::Solved, Some(&only_a))
        .unwrap_err()
        .to_string();
    assert!(err.contains("b_01.xisf"), "{err}");
    assert!(err.contains("beyond the"), "{err}");
    assert!(err.contains("mmm frame"), "{err}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn aligned_group_adopts_a_matching_canvas() {
    let dir = tempdir("adopt-aligned");
    let a = write_aligned_group(&dir.join("A"), "a", STAR, 10, 20);
    let reference = derive(&a, InputSelect::Auto).unwrap();
    let s = analyze(&a, &dir.join("a.mmm-session"), InputSelect::Auto, Some(&reference)).unwrap();
    assert_eq!(s.input, InputKind::Aligned);
    assert!(s.frame_imposed);
    assert!(s.frame.is_none(), "aligned sessions keep passthrough WCS");
    assert_eq!(s.canvas, (CANVAS.0, CANVAS.1, 1));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn aligned_group_on_another_canvas_is_refused_with_hint() {
    let dir = tempdir("aligned-size");
    let a = write_aligned_group(&dir.join("A"), "a", STAR, 10, 20);
    let smaller = ReferenceFrame::Aligned { width: CANVAS.0 - 10, height: CANVAS.1, wcs: None };
    let err = analyze(&a, &dir.join("a.mmm-session"), InputSelect::Auto, Some(&smaller))
        .unwrap_err()
        .to_string();
    assert!(err.contains("does not match the reference frame"), "{err}");
    assert!(err.contains("align every group to one common reference"), "{err}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn aligned_group_with_shifted_wcs_is_refused_quoting_pixels() {
    let dir = tempdir("aligned-wcs");
    let a = write_aligned_group(&dir.join("A"), "a", STAR, 10, 20);
    // Same canvas, but this group's registration put the sky 2 px further north.
    let b = write_aligned_group(&dir.join("B"), "b", offset_px(0.0, 2.0), 10, 20);
    let reference = derive(&a, InputSelect::Auto).unwrap();
    let err = analyze(&b, &dir.join("b.mmm-session"), InputSelect::Auto, Some(&reference))
        .unwrap_err()
        .to_string();
    assert!(err.contains("displaced up to 2.00 px"), "{err}");
    assert!(err.contains("align every group to one common reference"), "{err}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn reference_kind_must_match_the_input_kind() {
    let dir = tempdir("kind");
    let raw = write_solved_group(&dir.join("raw"), "r", (0.0, 0.0), 150);
    let registered = write_aligned_group(&dir.join("reg"), "g", STAR, 10, 20);
    let solved_ref = derive(&raw, InputSelect::Solved).unwrap();
    let aligned_ref = derive(&registered, InputSelect::Auto).unwrap();

    let err = analyze(&registered, &dir.join("x.mmm-session"), InputSelect::Auto, Some(&solved_ref))
        .unwrap_err()
        .to_string();
    assert!(err.contains("solved mosaic frame") && err.contains("--input solved"), "{err}");

    let err = analyze(&raw, &dir.join("y.mmm-session"), InputSelect::Solved, Some(&aligned_ref))
        .unwrap_err()
        .to_string();
    assert!(err.contains("aligned canvas") && err.contains("--input aligned"), "{err}");
    std::fs::remove_dir_all(&dir).unwrap();
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p mmm-core --test reference`
Expected: compile errors (`frame_imposed` missing; `analyze_full` takes 6 arguments).

- [ ] **Step 3: Add `frame_imposed` to `Session`**

In `crates/mmm-core/src/session.rs`, after the `frame` field:

```rust
    /// `true` when the analyze stage adopted a shared
    /// [`crate::reference::ReferenceFrame`] instead of deriving its own
    /// frame / taking the input canvas as given. The blender then defaults
    /// to the whole canvas as its output extent so every session sharing
    /// the frame yields identically sized, co-registered output. Sessions
    /// from before the field read as `false`.
    #[serde(default)]
    pub frame_imposed: bool,
```

and in `Session::create` add `frame_imposed: false,` after `frame: None,`.

- [ ] **Step 4: Thread the reference through `analyze.rs`**

Add to the imports: `use crate::reference::ReferenceFrame;`.

`analyze_full`: add the trailing parameter `reference: Option<&ReferenceFrame>` and extend its doc comment with:

```rust
/// With `reference = Some(frame)` the session adopts that shared
/// [`ReferenceFrame`] instead of deriving its own frame (solved input) or
/// taking the canvas as given (aligned input), after checking the group fits
/// it — see [`crate::reference`]. The session then records `frame_imposed`.
```

Pass `reference` in each of its three dispatch arms (`analyze_aligned(.., progress, reference)`, `analyze_solved(.., progress, reference)`). Update the two internal callers (`analyze_gain` at line 176 and `analyze_input_progress` at line 203) to pass `None` as the final argument.

`analyze_aligned`: add the parameter `reference: Option<&ReferenceFrame>`; pass it on in the auto re-dispatch `return analyze_solved(paths, session_dir, surface_order, gain, progress, reference);`. Immediately before `session.canvas = canvas;` insert:

```rust
    if let Some(r) = reference {
        match r {
            ReferenceFrame::Aligned { width, height, wcs } => {
                let panel_wcs = InputPanel::open(&paths[0])?.linear_wcs();
                crate::reference::check_aligned(
                    (canvas.0, canvas.1),
                    panel_wcs.as_ref(),
                    *width,
                    *height,
                    wcs.as_ref(),
                )
                .map_err(|reason| Error::format(session_dir, reason))?;
                session.frame_imposed = true;
            }
            ReferenceFrame::Solved { .. } => {
                return Err(Error::format(
                    session_dir,
                    "the reference frame is a solved mosaic frame but these panels were read as \
                     aligned full-canvas frames: pass `--input solved` if they are raw \
                     plate-solved panels, or derive an aligned reference from the registered \
                     canvases",
                ));
            }
        }
    }
```

`analyze_solved`: add the parameter `reference: Option<&ReferenceFrame>`; replace `let frame = choose_frame(&models);` with:

```rust
    let frame = match reference {
        None => choose_frame(&models),
        Some(ReferenceFrame::Solved { frame }) => {
            crate::reference::check_footprints(
                panels
                    .iter()
                    .map(|(p, m)| (p.path().display().to_string(), m)),
                frame,
            )
            .map_err(|reason| Error::format(session_dir, reason))?;
            frame.clone()
        }
        Some(ReferenceFrame::Aligned { .. }) => {
            return Err(Error::format(
                session_dir,
                "the reference frame is an aligned canvas but these panels were read as solved \
                 raw panels: pass `--input aligned` if they are registered full-canvas frames, \
                 or derive a solved reference from the raw panels",
            ));
        }
    };
```

and next to `session.frame = Some(frame);` add `session.frame_imposed = reference.is_some();`.

- [ ] **Step 5: Fix the external callers**

`crates/mmm/src/main.rs:259`: append `, None` to the `analyze_full(...)` call.
`crates/mmm-ipc-worker/src/main.rs:185-192`: add `None,` after `Some(&progress),`.

- [ ] **Step 6: Build everything and run the tests**

Run: `cargo build --all-targets && cargo test -p mmm-core --test reference && cargo test -p mmm-core --test e2e solved`
Expected: all `reference` tests pass (13 so far); the three solved e2e gates still pass (frame choice unchanged when `reference` is `None`).

- [ ] **Step 7: Commit**

```bash
cargo fmt
git add crates/mmm-core/src/session.rs crates/mmm-core/src/analyze.rs crates/mmm/src/main.rs crates/mmm-ipc-worker/src/main.rs crates/mmm-core/tests/reference.rs
git commit -m "feat(analyze): adopt an imposed reference frame and record frame_imposed

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 5: Blend extent (`Extent`, `BlendParams::extent`, default rule)

**Files:**
- Modify: `crates/mmm-core/src/blend.rs:165-215` (`BlendParams`, `Default`, `output_bbox`)
- Modify: every `BlendParams { .. }` literal without `..Default::default()`: `crates/mmm-core/src/ipc/protocol.rs:323`, `crates/mmm-core/tests/regression_guard.rs:197`, `crates/mmm-core/src/blend_tests.rs` (≈22 literals), `crates/mmm-core/tests/e2e.rs` (≈14 literals), `crates/mmm-ipc-worker/tests/end_to_end.rs:127`
- Modify: `crates/mmm-core/src/blend_tests.rs` (new tests), `crates/mmm-core/src/session.rs` (one test)

**Interfaces:**
- Consumes: `Session::frame_imposed`.
- Produces: `pub enum Extent { Union, Canvas }` (`Debug, Clone, Copy, PartialEq, Eq`), `BlendParams::extent: Option<Extent>` (default `None`), unchanged `output_bbox(session, params)` signature.

- [ ] **Step 1: Write the failing unit tests**

Append to `crates/mmm-core/src/blend_tests.rs`:

```rust
#[test]
fn extent_defaults_to_union_and_canvas_widens_with_zero_fill() {
    let dir = tmpdir("extent");
    let (session, graph) = make_panels(&dir);
    let phot = identity_phot(2, 1);
    let base = BlendParams {
        feather_px: 16.0,
        downsample: 1,
        band_rows: 16,
        mode: BlendMode::Feather,
        roi: None,
        defect_veto: true,
        flatten: None,
        extent: None,
    };
    assert_eq!(output_bbox(&session, &base).unwrap(), [8, 8, 120, 64]);
    let canvas = BlendParams {
        extent: Some(Extent::Canvas),
        ..base.clone()
    };
    assert_eq!(output_bbox(&session, &canvas).unwrap(), [0, 0, 128, 64]);

    let mut union_sink = MemSink::new();
    blend(&session, &phot, None, &graph, &base, &mut union_sink).unwrap();
    let mut wide = MemSink::new();
    blend(&session, &phot, None, &graph, &canvas, &mut wide).unwrap();
    assert_eq!((wide.w, wide.h), (128, 64));
    assert!(wide.data.iter().all(|v| v.is_finite()), "no NaN/Inf in zero-filled bands");
    for y in 0..8 {
        for x in 0..128 {
            assert_eq!(wide.at(0, x, y), 0.0, "rows above the content must be zero ({x},{y})");
        }
    }
    for y in 0..64 {
        for x in 0..8 {
            assert_eq!(wide.at(0, x, y), 0.0, "columns left of the content must be zero ({x},{y})");
        }
    }
    for y in 0..union_sink.h {
        for x in 0..union_sink.w {
            let (a, b) = (wide.at(0, x + 8, y + 8), union_sink.at(0, x, y));
            assert!((a - b).abs() < 1e-6, "canvas extent must reproduce the union blend at ({x},{y}): {a} vs {b}");
        }
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn imposed_frame_defaults_to_canvas_extent() {
    let dir = tmpdir("extent-imposed");
    let (mut session, _) = make_panels(&dir);
    let params = BlendParams {
        feather_px: 16.0,
        downsample: 1,
        band_rows: 16,
        mode: BlendMode::Feather,
        roi: None,
        defect_veto: true,
        flatten: None,
        extent: None,
    };
    session.frame_imposed = true;
    assert_eq!(output_bbox(&session, &params).unwrap(), [0, 0, 128, 64]);
    let union = BlendParams {
        extent: Some(Extent::Union),
        ..params.clone()
    };
    assert_eq!(output_bbox(&session, &union).unwrap(), [8, 8, 120, 64], "explicit union still wins");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn roi_intersects_the_active_extent() {
    let dir = tmpdir("extent-roi");
    let (session, _) = make_panels(&dir);
    // The ROI pokes outside the content union but stays inside the canvas.
    let params = BlendParams {
        feather_px: 16.0,
        downsample: 1,
        band_rows: 16,
        mode: BlendMode::Feather,
        roi: Some([0, 0, 64, 32]),
        defect_veto: true,
        flatten: None,
        extent: None,
    };
    assert_eq!(output_bbox(&session, &params).unwrap(), [8, 8, 64, 32]);
    let canvas = BlendParams {
        extent: Some(Extent::Canvas),
        ..params.clone()
    };
    assert_eq!(output_bbox(&session, &canvas).unwrap(), [0, 0, 64, 32]);
    std::fs::remove_dir_all(&dir).unwrap();
}
```

And in `crates/mmm-core/src/session.rs`, add (or extend) a unit-test module at the end of the file:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_json_without_frame_imposed_reads_false() {
        let json = r#"{"canvas":[128,64,1],"panels":[]}"#;
        let s: Session = serde_json::from_str(json).unwrap();
        assert!(!s.frame_imposed);
        assert!(s.frame.is_none());
        assert_eq!(s.input, InputKind::Aligned);
    }
}
```

(If `session.rs` already has a `mod tests`, add only the function to it.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p mmm-core extent`
Expected: compile errors (`extent` field / `Extent` type missing).

- [ ] **Step 3: Implement `Extent` and the rule**

In `crates/mmm-core/src/blend.rs`, before `pub struct BlendParams`:

```rust
/// Which region of the canvas the blend writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Extent {
    /// The union of the panels' content bboxes (cropped — the historical
    /// behaviour).
    Union,
    /// The whole session canvas: for a solved session the mosaic frame
    /// including its margin, for an aligned session the full input canvas.
    /// Pixels outside every panel are written as `0.0` (no-data). Sessions
    /// sharing one [`crate::reference::ReferenceFrame`] blend to identical
    /// geometry this way.
    Canvas,
}
```

Add the field to `BlendParams` (after `flatten`):

```rust
    /// Output extent. `None` picks the session default: [`Extent::Canvas`]
    /// when the session's frame was imposed from a shared reference
    /// ([`crate::session::Session::frame_imposed`]), else [`Extent::Union`],
    /// so pre-existing sessions blend exactly as before.
    pub extent: Option<Extent>,
```

and `extent: None,` in `impl Default for BlendParams`.

Rewrite `output_bbox`:

```rust
/// The blend's output bbox: the active [`Extent`] (see
/// [`BlendParams::extent`] for the default rule), intersected with the ROI
/// when one is set. Errors if the intersection is empty.
pub fn output_bbox(session: &Session, params: &BlendParams) -> Result<[u64; 4]> {
    let extent = params.extent.unwrap_or(if session.frame_imposed {
        Extent::Canvas
    } else {
        Extent::Union
    });
    let u = match extent {
        Extent::Union => union_bbox(session)?,
        Extent::Canvas => [0, 0, session.canvas.0, session.canvas.1],
    };
    let Some(r) = params.roi else { return Ok(u) };
    let b = [
        u[0].max(r[0]),
        u[1].max(r[1]),
        u[2].min(r[2]),
        u[3].min(r[3]),
    ];
    if b[0] >= b[2] || b[1] >= b[3] {
        return Err(Error::format(
            &session.dir,
            "ROI does not intersect the mosaic content",
        ));
    }
    Ok(b)
}
```

Update the module docs at `blend.rs:4-5` from "Output canvas = union of panel content bboxes (cropped — never the full mosaic canvas)" to: "Output extent = union of panel content bboxes by default (cropped), or the whole canvas for sessions on a shared reference frame / on request ([`Extent`])."

- [ ] **Step 4: Add `extent: None` to every struct literal**

Run `cargo build --all-targets 2>&1 | grep -c "missing field \`extent\`"` and fix each site by adding `extent: None,` after `flatten: ...,`. Known sites: `ipc/protocol.rs:323` (`to_params`), `regression_guard.rs:197`, `blend_tests.rs` (all literals that list `flatten:` without `..`), `e2e.rs` (same), `mmm-ipc-worker/tests/end_to_end.rs:127`. Also add `extent: None` explicitly to `main.rs`'s `params_for_bbox` (it uses `..Default::default()` so it compiles either way; Task 7 wires the CLI value in).

- [ ] **Step 5: Run the tests**

Run: `cargo test -p mmm-core extent && cargo test -p mmm-core session_json && cargo test -p mmm-core --test regression_guard && cargo test --workspace`
Expected: the 4 new tests pass; the regression guard passes with its existing constants (proves the default is byte-identical); whole workspace green. If `extent_defaults_to_union_and_canvas_widens_with_zero_fill` finds a NaN in empty rows, the blend loop divides by a zero weight sum somewhere other than the guarded `if w > 0.0` at `blend.rs:1641`; find the unguarded division in `blend_full`'s row loop and guard it to write `0.0`.

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add -A crates/mmm-core/src crates/mmm-core/tests crates/mmm-ipc-worker/tests crates/mmm/src/main.rs
git commit -m "feat(blend): Extent::{Union,Canvas} with canvas default for imposed-frame sessions

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 6: Two-group end-to-end tests (solved and aligned paths)

**Files:**
- Modify: `crates/mmm-core/tests/reference.rs`

**Interfaces:**
- Consumes: everything from Tasks 2–5.

- [ ] **Step 1: Write the tests**

Append to `crates/mmm-core/tests/reference.rs`:

```rust
// ---------------------------------------------------------------------------
// two groups → one grid

/// In-memory sink collecting the whole (small) blended output, planar.
struct MemSink {
    w: usize,
    h: usize,
    ch: usize,
    data: Vec<f32>,
}

impl MemSink {
    fn new() -> Self {
        Self { w: 0, h: 0, ch: 0, data: Vec::new() }
    }
    fn at(&self, c: usize, x: usize, y: usize) -> f32 {
        self.data[(c * self.h + y) * self.w + x]
    }
}

impl RowSink for MemSink {
    fn begin(&mut self, w: u64, h: u64, ch: u64) -> Result<()> {
        self.w = w as usize;
        self.h = h as usize;
        self.ch = ch as usize;
        self.data = vec![f32::NAN; self.w * self.h * self.ch];
        Ok(())
    }
    fn band(&mut self, y0: u64, rows: &[f32]) -> Result<()> {
        let band_rows = rows.len() / (self.ch * self.w);
        for c in 0..self.ch {
            for r in 0..band_rows {
                let src = &rows[(c * band_rows + r) * self.w..][..self.w];
                let off = (c * self.h + y0 as usize + r) * self.w;
                self.data[off..off + self.w].copy_from_slice(src);
            }
        }
        Ok(())
    }
    fn finish(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Blend a session with the given mode and the session-default extent.
fn blend_session(session: &Session, mode: BlendMode) -> MemSink {
    let phot = Photometry::load(&session.photometry_path()).unwrap();
    let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();
    let surf = Surfaces::load(&session.surfaces_path()).unwrap();
    let params = BlendParams {
        feather_px: 24.0,
        downsample: 1,
        band_rows: 64,
        mode,
        roi: None,
        defect_veto: true,
        flatten: None,
        extent: None,
    };
    let mut sink = MemSink::new();
    blend(session, &phot, Some(&surf), &graph, &params, &mut sink).unwrap();
    sink
}

/// Background-subtracted intensity-weighted centroid of the brightest spot
/// (±4 px window), channel 0, in output pixel coordinates.
fn star_centroid(sink: &MemSink) -> (f64, f64) {
    let mut best = (0usize, 0usize, f32::MIN);
    for y in 0..sink.h {
        for x in 0..sink.w {
            let v = sink.at(0, x, y);
            if v > best.2 {
                best = (x, y, v);
            }
        }
    }
    let (bx, by) = (best.0 as i64, best.1 as i64);
    let (mut sx, mut sy, mut sw) = (0.0f64, 0.0f64, 0.0f64);
    for dy in -4..=4 {
        for dx in -4..=4 {
            let (x, y) = (bx + dx, by + dy);
            if x < 0 || y < 0 || x >= sink.w as i64 || y >= sink.h as i64 {
                continue;
            }
            let w = (f64::from(sink.at(0, x as usize, y as usize)) - 0.05).max(0.0);
            sx += w * x as f64;
            sy += w * y as f64;
            sw += w;
        }
    }
    (sx / sw, sy / sw)
}

#[test]
fn two_solved_groups_share_one_grid() {
    let dir = tempdir("two-solved");
    let a = write_solved_group(&dir.join("A"), "a", (0.0, 0.0), 150);
    let b = write_solved_group(&dir.join("B"), "b", (40.0, 25.0), 140);

    // Sanity: left alone the groups land on different frames (the bug).
    let own_a = analyze(&a, &dir.join("own_a.mmm-session"), InputSelect::Solved, None).unwrap();
    let own_b = analyze(&b, &dir.join("own_b.mmm-session"), InputSelect::Solved, None).unwrap();
    assert_ne!(own_a.frame, own_b.frame, "fixture must make independently chosen frames differ");

    let all: Vec<PathBuf> = a.iter().chain(&b).cloned().collect();
    let reference = derive(&all, InputSelect::Auto).unwrap();
    let sa = analyze(&a, &dir.join("a.mmm-session"), InputSelect::Solved, Some(&reference)).unwrap();
    let sb = analyze(&b, &dir.join("b.mmm-session"), InputSelect::Solved, Some(&reference)).unwrap();
    assert_eq!(sa.frame, sb.frame);
    assert_eq!(sa.canvas, sb.canvas);
    assert_ne!(union_bbox(&sa).unwrap(), union_bbox(&sb).unwrap(), "coverage differs, grid must not");

    for mode in [BlendMode::Feather, BlendMode::Pyramid] {
        let out_a = blend_session(&sa, mode);
        let out_b = blend_session(&sb, mode);
        assert_eq!((out_a.w, out_a.h), (out_b.w, out_b.h), "{mode:?}");
        assert_eq!((out_a.w as u64, out_a.h as u64), (sa.canvas.0, sa.canvas.1), "{mode:?}: canvas extent");
        assert!(out_a.data.iter().chain(&out_b.data).all(|v| v.is_finite()), "{mode:?}");
        assert_eq!(out_a.at(0, 0, 0), 0.0, "{mode:?}: frame margin is zero-filled");
        let ca = star_centroid(&out_a);
        let cb = star_centroid(&out_b);
        assert!(
            (ca.0 - cb.0).abs() < 0.1 && (ca.1 - cb.1).abs() < 0.1,
            "{mode:?}: star at {ca:?} in group A vs {cb:?} in group B"
        );
        // The same WCS cards would be emitted: identical frames.
        assert_eq!(sa.frame.as_ref().unwrap().linear_wcs(), sb.frame.as_ref().unwrap().linear_wcs());
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn two_aligned_groups_share_the_full_canvas() {
    let dir = tempdir("two-aligned");
    let a = write_aligned_group(&dir.join("A"), "a", STAR, 10, 20);
    let b = write_aligned_group(&dir.join("B"), "b", STAR, 30, 40);
    let all: Vec<PathBuf> = a.iter().chain(&b).cloned().collect();
    let reference = derive(&all, InputSelect::Auto).unwrap();
    let sa = analyze(&a, &dir.join("a.mmm-session"), InputSelect::Auto, Some(&reference)).unwrap();
    let sb = analyze(&b, &dir.join("b.mmm-session"), InputSelect::Auto, Some(&reference)).unwrap();
    assert_eq!(sa.input, InputKind::Aligned);
    assert_ne!(union_bbox(&sa).unwrap(), union_bbox(&sb).unwrap());
    let out_a = blend_session(&sa, BlendMode::Feather);
    let out_b = blend_session(&sb, BlendMode::Feather);
    assert_eq!((out_a.w as u64, out_a.h as u64), CANVAS);
    assert_eq!((out_b.w as u64, out_b.h as u64), CANVAS);
    // Content sits where the windows were, zero elsewhere.
    assert!((out_a.at(0, 50, 80) - 0.4).abs() < 1e-3);
    assert_eq!(out_a.at(0, 5, 5), 0.0);
    assert!((out_b.at(0, 70, 100) - 0.4).abs() < 1e-3);
    assert_eq!(out_b.at(0, 5, 5), 0.0);
    std::fs::remove_dir_all(&dir).unwrap();
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p mmm-core --test reference`
Expected: all 15 tests pass. If `two_solved_groups_share_one_grid` fails because `Surfaces::load` finds no file, replace `Surfaces::load(..).unwrap()` in `blend_session` with `session.surfaces_path().exists().then(|| Surfaces::load(&session.surfaces_path()).unwrap())` and pass `surf.as_ref()`. If the Pyramid pass fails on this small fixture with a size-related panic from `crate::pyramid`, keep the Feather assertions and restrict the Pyramid loop iteration to the finiteness and dimension checks, noting the reason in a comment — do **not** drop Pyramid entirely (it is the default mode).

- [ ] **Step 3: Commit**

```bash
cargo fmt
git add crates/mmm-core/tests/reference.rs
git commit -m "test(reference): two groups on one shared frame produce co-registered outputs

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 7: CLI — shared option structs, `mmm frame`, `--frame`, `--extent`

**Files:**
- Modify: `crates/mmm/src/main.rs` (whole command surface, `analyze_cmd`, `blend_cmd`)
- Create: `crates/mmm/tests/cli.rs`

**Interfaces:**
- Consumes: `mmm_core::reference::{ReferenceFrame, derive}`, `mmm_core::blend::Extent`, `analyze_full(.., reference)`.
- Produces (used by Task 8):
  - `struct AnalyzeOpts` (clap `Args`) with `fn resolve(&self) -> anyhow::Result<AnalyzeConfig>`; `struct AnalyzeConfig { surface_order: Option<u32>, input: InputSelect, gain: GainMode }`
  - `struct BlendOpts` (clap `Args`) with `fn resolve(&self) -> anyhow::Result<BlendConfig>`; `struct BlendConfig { downsample: u32, feather: f32, mode: BlendMode, roi: Option<[u64; 4]>, defect_veto: bool, flatten: Option<u32>, wcs_flip: bool, extent: Option<Extent> }`
  - `fn analyze_cmd(panels: &[PathBuf], session: &Path, cfg: &AnalyzeConfig, reference: Option<&ReferenceFrame>) -> anyhow::Result<Session>`
  - `fn blend_cmd(session_dir: &Path, output: &Path, png: Option<&Path>, cfg: &BlendConfig) -> anyhow::Result<()>`
  - `fn frame_cmd(panels: &[PathBuf], output: &Path, input: &str) -> anyhow::Result<()>`

- [ ] **Step 1: Write the failing binary-level tests**

Create `crates/mmm/tests/cli.rs`:

```rust
//! Binary-level tests of the `mmm` CLI around the shared reference frame:
//! `frame` → `analyze --frame` → `blend` yields co-registered FITS outputs,
//! and `batch` does the same in one command.

use std::path::{Path, PathBuf};
use std::process::Command;

use mmm_core::astrometry::LinearWcs;
use mmm_core::formats::InputPanel;
use mmm_core::reference::ReferenceFrame;
use mmm_core::synth::{SynthWcs, write_xisf_solved};

const S: f64 = 1.0e-3;
const STAR: [f64; 2] = [66.0, 18.0];

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mmm-cli-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn sky(ra: f64, dec: f64) -> f32 {
    let dx = (ra - STAR[0]) * STAR[1].to_radians().cos() / S;
    let dy = (dec - STAR[1]) / S;
    (0.05 + 0.8 * (-(dx * dx + dy * dy) / 8.0).exp()) as f32
}

fn offset_px(dx: f64, dy: f64) -> [f64; 2] {
    [STAR[0] + dx * S / STAR[1].to_radians().cos(), STAR[1] + dy * S]
}

fn write_raw_panel(path: &Path, w: u64, h: u64, crval: [f64; 2]) {
    let cd = [[-S, 0.0], [0.0, S]];
    let refimg = [w as f64 / 2.0, h as f64 / 2.0];
    let lin = LinearWcs {
        crval,
        crpix: [refimg[0] + 0.5, refimg[1] + 0.5],
        cd,
        ctype: ["RA---TAN".into(), "DEC--TAN".into()],
        radesys: "ICRS".into(),
    };
    let mut plane = vec![0.0f32; (w * h) as usize];
    for j in 0..h {
        for i in 0..w {
            let (ra, dec) = lin.pixel_to_sky(i as f64 + 1.0, j as f64 + 1.0);
            plane[(j * w + i) as usize] = sky(ra, dec);
        }
    }
    write_xisf_solved(path, w, h, 1, &plane, &SynthWcs { crval, refimg, cd }).unwrap();
}

/// Two overlapping raw panels; `shift` moves the group on the sky (px), `w2`
/// sets the second panel's width so groups have different footprints.
fn write_group(dir: &Path, tag: &str, shift: (f64, f64), w2: u64) -> Vec<PathBuf> {
    std::fs::create_dir_all(dir).unwrap();
    let a = dir.join(format!("{tag}_00.xisf"));
    let b = dir.join(format!("{tag}_01.xisf"));
    write_raw_panel(&a, 160, 120, offset_px(-60.0 + shift.0, shift.1));
    write_raw_panel(&b, w2, 120, offset_px(60.0 + shift.0, shift.1));
    vec![a, b]
}

fn mmm() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_mmm"));
    c.arg("--no-banner");
    c
}

fn run(cmd: &mut Command) -> String {
    let out = cmd.output().expect("mmm must start");
    assert!(
        out.status.success(),
        "mmm failed ({}):\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn geometry_and_wcs(fits: &Path) -> ((u64, u64), LinearWcs) {
    let p = InputPanel::open(fits).unwrap();
    ((p.width(), p.height()), p.linear_wcs().expect("blend output carries WCS cards"))
}

#[test]
fn frame_analyze_blend_yield_co_registered_outputs() {
    let dir = tempdir("pipeline");
    let a = write_group(&dir.join("A"), "a", (0.0, 0.0), 150);
    let b = write_group(&dir.join("B"), "b", (40.0, 25.0), 140);
    let frame = dir.join("ref.mmm-frame.json");

    let out = run(mmm().arg("frame").args(&a).args(&b).arg("-o").arg(&frame));
    assert!(out.contains("solved frame"), "{out}");
    assert!(matches!(ReferenceFrame::load(&frame).unwrap(), ReferenceFrame::Solved { .. }));

    for (tag, panels) in [("A", &a), ("B", &b)] {
        let session = dir.join(format!("{tag}.mmm-session"));
        let out = run(mmm()
            .arg("analyze")
            .args(panels)
            .arg("-s")
            .arg(&session)
            .arg("--input")
            .arg("solved")
            .arg("--frame")
            .arg(&frame));
        assert!(out.contains("imposed"), "analyze must say the frame was imposed:\n{out}");
        run(mmm()
            .arg("blend")
            .arg("-s")
            .arg(&session)
            .arg("-o")
            .arg(dir.join(format!("{tag}.fits")))
            .arg("--mode")
            .arg("feather")
            .arg("--feather")
            .arg("24"));
    }
    let (ga, wa) = geometry_and_wcs(&dir.join("A.fits"));
    let (gb, wb) = geometry_and_wcs(&dir.join("B.fits"));
    assert_eq!(ga, gb);
    assert_eq!(wa, wb);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn blend_extent_flag_overrides_the_default() {
    let dir = tempdir("extent-flag");
    let a = write_group(&dir.join("A"), "a", (0.0, 0.0), 150);
    let session = dir.join("A.mmm-session");
    run(mmm().arg("analyze").args(&a).arg("-s").arg(&session).arg("--input").arg("solved"));
    run(mmm().arg("blend").arg("-s").arg(&session).arg("-o").arg(dir.join("union.fits")).arg("--mode").arg("feather"));
    run(mmm().arg("blend").arg("-s").arg(&session).arg("-o").arg(dir.join("canvas.fits")).arg("--mode").arg("feather").arg("--extent").arg("canvas"));
    let (gu, _) = geometry_and_wcs(&dir.join("union.fits"));
    let (gc, _) = geometry_and_wcs(&dir.join("canvas.fits"));
    assert!(gc.0 > gu.0 && gc.1 > gu.1, "canvas {gc:?} must exceed union {gu:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn analyze_with_missing_frame_file_fails_naming_it() {
    let dir = tempdir("missing-frame");
    let a = write_group(&dir.join("A"), "a", (0.0, 0.0), 150);
    let out = mmm()
        .arg("analyze")
        .args(&a)
        .arg("-s")
        .arg(dir.join("A.mmm-session"))
        .arg("--frame")
        .arg(dir.join("absent.mmm-frame.json"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("absent.mmm-frame.json"), "{err}");
    assert!(!dir.join("A.mmm-session").join("session.json").exists(), "must fail before analyzing");
    std::fs::remove_dir_all(&dir).unwrap();
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p mmm --test cli`
Expected: `frame_analyze_blend_yield_co_registered_outputs` fails (unknown subcommand `frame`), the others fail on the unknown `--frame` / `--extent` flags.

- [ ] **Step 3: Restructure the CLI**

In `crates/mmm/src/main.rs`:

Add the option structs and typed configs:

```rust
/// Analyze options shared by `analyze` and `batch`.
#[derive(clap::Args, Clone, Debug)]
struct AnalyzeOpts {
    /// Residual surface correction: off, 0 (constant), 1 (plane), 2 (quadratic)
    #[arg(long, default_value = "2")]
    surface: String,

    /// Input kind: auto (detect), aligned (registered full-canvas
    /// frames), solved (unaligned panels with astrometric solutions)
    #[arg(long, default_value = "auto")]
    input: String,

    /// Photometric gain handling: fit (measure per-panel gains from
    /// overlaps), unity (pin every gain at 1, match levels with offsets
    /// only — for same-rig/same-exposure mosaics)
    #[arg(long, default_value = "fit")]
    gain: String,
}

/// Parsed [`AnalyzeOpts`].
struct AnalyzeConfig {
    surface_order: Option<u32>,
    input: mmm_core::analyze::InputSelect,
    gain: mmm_core::photometry::GainMode,
}

fn parse_input(input: &str) -> anyhow::Result<mmm_core::analyze::InputSelect> {
    Ok(match input {
        "auto" => mmm_core::analyze::InputSelect::Auto,
        "aligned" => mmm_core::analyze::InputSelect::Aligned,
        "solved" => mmm_core::analyze::InputSelect::Solved,
        other => anyhow::bail!("--input must be auto, aligned or solved (got {other})"),
    })
}

impl AnalyzeOpts {
    fn resolve(&self) -> anyhow::Result<AnalyzeConfig> {
        let surface_order = match self.surface.as_str() {
            "off" => None,
            "0" => Some(0),
            "1" => Some(1),
            "2" => Some(2),
            other => anyhow::bail!("--surface must be off, 0, 1 or 2 (got {other})"),
        };
        let gain = match self.gain.as_str() {
            "fit" => mmm_core::photometry::GainMode::Fit,
            "unity" => mmm_core::photometry::GainMode::Unity,
            other => anyhow::bail!("--gain must be fit or unity (got {other})"),
        };
        Ok(AnalyzeConfig {
            surface_order,
            input: parse_input(&self.input)?,
            gain,
        })
    }
}

/// Blend options shared by `blend` and `batch`.
#[derive(clap::Args, Clone, Debug)]
struct BlendOpts {
    /// Downsample factor: 1 = full resolution, 8 = fast preview from L8 summaries
    #[arg(long, default_value_t = 1)]
    downsample: u32,

    /// Feather ramp length in canvas pixels
    #[arg(long, default_value_t = 256.0)]
    feather: f32,

    /// Blend mode: pyramid (multiband base + star-safe seams, default),
    /// twoband (feathered base + star-safe seams) or feather (phase-1)
    #[arg(long, default_value = "pyramid")]
    mode: String,

    /// Region of interest in full-res canvas pixels: x,y,w,h
    #[arg(long)]
    roi: Option<String>,

    /// Cross-panel defect veto in overlaps (twoband mode): suppresses
    /// cosmic-ray residue and satellite trails that survive in one panel
    #[arg(long, default_value = "on")]
    defect_veto: String,

    /// Opt-in global background flatten: off (default), 1 (plane) or
    /// 2 (quadratic). Fits the merged mosaic's background and subtracts
    /// its varying part, preserving the central level; refuses on
    /// signal-dominated (nebula-heavy) mosaics
    #[arg(long, default_value = "off")]
    flatten: String,

    /// WCS card convention: topdown (PI display-space, default) or
    /// flipped (reflected bottom-up) for readers that mirror annotations
    #[arg(long, default_value = "topdown")]
    wcs_frame: String,

    /// Output extent: auto (canvas for sessions on a shared reference
    /// frame, else the content union), union (crop to the panels'
    /// content), canvas (the whole canvas, zero outside coverage)
    #[arg(long, default_value = "auto")]
    extent: String,
}

/// Parsed [`BlendOpts`].
struct BlendConfig {
    downsample: u32,
    feather: f32,
    mode: mmm_core::blend::BlendMode,
    roi: Option<[u64; 4]>,
    defect_veto: bool,
    flatten: Option<u32>,
    wcs_flip: bool,
    extent: Option<mmm_core::blend::Extent>,
}

impl BlendOpts {
    fn resolve(&self) -> anyhow::Result<BlendConfig> {
        let mode = match self.mode.as_str() {
            "feather" => mmm_core::blend::BlendMode::Feather,
            "twoband" => mmm_core::blend::BlendMode::TwoBand,
            "pyramid" => mmm_core::blend::BlendMode::Pyramid,
            other => anyhow::bail!("--mode must be pyramid, twoband or feather (got {other})"),
        };
        let defect_veto = match self.defect_veto.as_str() {
            "on" => true,
            "off" => false,
            other => anyhow::bail!("--defect-veto must be on or off (got {other})"),
        };
        let flatten = match self.flatten.as_str() {
            "off" => None,
            "1" => Some(1),
            "2" => Some(2),
            other => anyhow::bail!("--flatten must be off, 1 or 2 (got {other})"),
        };
        let wcs_flip = match self.wcs_frame.as_str() {
            "topdown" => false,
            "flipped" => true,
            other => anyhow::bail!("--wcs-frame must be topdown or flipped (got {other})"),
        };
        let extent = match self.extent.as_str() {
            "auto" => None,
            "union" => Some(mmm_core::blend::Extent::Union),
            "canvas" => Some(mmm_core::blend::Extent::Canvas),
            other => anyhow::bail!("--extent must be auto, union or canvas (got {other})"),
        };
        Ok(BlendConfig {
            downsample: self.downsample,
            feather: self.feather,
            mode,
            roi: self.roi.as_deref().map(parse_roi).transpose()?,
            defect_veto,
            flatten,
            wcs_flip,
            extent,
        })
    }
}
```

Replace the `Analyze` and `Blend` variants and add `Frame`:

```rust
    /// Derive a shared reference frame from the headers of every panel of
    /// every group, so each group analyzed with `--frame` lands on one grid
    Frame {
        /// Every panel of every group (XISF or FITS); headers only are read
        #[arg(required = true)]
        panels: Vec<std::path::PathBuf>,

        /// Output reference frame file (`<name>.mmm-frame.json`)
        #[arg(short, long)]
        output: std::path::PathBuf,

        /// Input kind: auto (detect), aligned (registered full-canvas
        /// frames), solved (unaligned panels with astrometric solutions)
        #[arg(long, default_value = "auto")]
        input: String,
    },

    /// Analyze panels: build tiled cache, coverage masks, and the overlap graph
    Analyze {
        /// Input panel files (XISF or FITS): pre-aligned full-canvas frames, or
        /// unaligned plate-solved panels (reprojected automatically)
        #[arg(required = true)]
        panels: Vec<std::path::PathBuf>,

        /// Session directory for cached analysis (created if missing)
        #[arg(short, long, default_value = "mosaic.mmm-session")]
        session: std::path::PathBuf,

        /// Shared reference frame from `mmm frame` to adopt instead of
        /// deriving this set's own; the blend then covers the whole frame
        #[arg(long)]
        frame: Option<std::path::PathBuf>,

        #[command(flatten)]
        opts: AnalyzeOpts,
    },

    /// Blend the analyzed panels into a mosaic FITS (and optional PNG preview)
    Blend {
        /// Session directory produced by `mmm analyze`
        #[arg(short, long, default_value = "mosaic.mmm-session")]
        session: std::path::PathBuf,

        /// Output FITS file (BITPIX=-32, planar channels)
        #[arg(short, long)]
        output: std::path::PathBuf,

        /// Also write an autostretched 8-bit PNG preview (downsampled runs only)
        #[arg(long)]
        png: Option<std::path::PathBuf>,

        #[command(flatten)]
        opts: BlendOpts,
    },
```

Dispatch in `main`:

```rust
        Command::Frame { panels, output, input } => frame_cmd(&panels, &output, &input),
        Command::Analyze { panels, session, frame, opts } => {
            let cfg = opts.resolve()?;
            let reference = frame
                .as_deref()
                .map(mmm_core::reference::ReferenceFrame::load)
                .transpose()?;
            analyze_cmd(&panels, &session, &cfg, reference.as_ref()).map(|_| ())
        }
        Command::Blend { session, output, png, opts } => {
            let cfg = opts.resolve()?;
            blend_cmd(&session, &output, png.as_deref(), &cfg)
        }
```

`frame_cmd`:

```rust
/// `mmm frame`: header-only derivation of a shared reference frame.
fn frame_cmd(panels: &[std::path::PathBuf], output: &std::path::Path, input: &str) -> anyhow::Result<()> {
    let input = parse_input(input)?;
    let frame = mmm_core::reference::derive(panels, input)?;
    frame.save(output)?;
    println!("reference: {} ({} panels)", frame.describe(), panels.len());
    println!("written: {}", output.display());
    Ok(())
}
```

`analyze_cmd` becomes `fn analyze_cmd(panels, session, cfg: &AnalyzeConfig, reference: Option<&ReferenceFrame>) -> anyhow::Result<mmm_core::session::Session>`: drop its own option parsing, call `analyze_full(panels, session, cfg.surface_order, cfg.gain, cfg.input, None, reference)`, and change the "input:" line to distinguish imposed frames:

```rust
    match (&s.frame, s.align_secs) {
        (Some(f), align_secs) => println!(
            "input: solved panels — {} reprojected onto {} {}x{} frame \
             ({:.3}\"/px, center RA {:.4} Dec {:+.4}) in {:.2}s",
            s.panels.len(),
            if s.frame_imposed { "the imposed" } else { "a fresh" },
            f.width,
            f.height,
            f.scale_deg * 3600.0,
            f.crval[0],
            f.crval[1],
            align_secs.unwrap_or(0.0),
        ),
        _ if s.frame_imposed => println!("input: aligned full-canvas frames (canvas matches the imposed reference)"),
        _ => println!("input: aligned full-canvas frames"),
    }
```

and `Ok(s)` at the end instead of `Ok(())`.

`blend_cmd` becomes `fn blend_cmd(session_dir, output, png: Option<&Path>, cfg: &BlendConfig) -> anyhow::Result<()>`: replace the individual parameters with `cfg.*` reads and set `extent: cfg.extent` in the `BlendParams` literal(s). Remove the `#[allow(clippy::too_many_arguments)]`.

- [ ] **Step 4: Run the tests and lints**

Run: `cargo test -p mmm && cargo clippy -p mmm --all-targets && cargo run -p mmm -- --help && cargo run -p mmm -- analyze --help`
Expected: the three `cli` tests and the existing `main.rs` unit tests pass; no warnings; `analyze --help` shows `--frame`, `--surface`, `--input`, `--gain`; `blend --help` shows `--extent`.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add crates/mmm/src/main.rs crates/mmm/tests/cli.rs
git commit -m "feat(cli): mmm frame, analyze --frame, blend --extent; shared option structs

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 8: `mmm batch`

**Files:**
- Modify: `crates/mmm/src/main.rs`
- Modify: `crates/mmm/tests/cli.rs`

**Interfaces:**
- Consumes: Task 7's `AnalyzeOpts`, `BlendOpts`, `analyze_cmd`, `blend_cmd`; `clap::ArgMatches::get_occurrences`.
- Produces: `Command::Batch`, `fn group_specs(m: &clap::ArgMatches) -> anyhow::Result<Vec<(String, Vec<PathBuf>)>>`, `fn batch_cmd(groups, session_dir, out_dir, png: bool, analyze: &AnalyzeConfig, blend: &BlendConfig) -> anyhow::Result<()>`.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` in `crates/mmm/src/main.rs`:

```rust
    fn batch_matches(args: &[&str]) -> clap::ArgMatches {
        let mut full = vec!["mmm", "batch"];
        full.extend_from_slice(args);
        <Cli as clap::CommandFactory>::command()
            .try_get_matches_from(full)
            .unwrap()
            .subcommand_matches("batch")
            .unwrap()
            .clone()
    }

    #[test]
    fn batch_groups_keep_occurrence_boundaries() {
        let m = batch_matches(&[
            "--group", "L", "l1.xisf", "l2.xisf", "--group", "R", "r1.xisf", "-s", "out", "-o", "fits",
        ]);
        let g = group_specs(&m).unwrap();
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].0, "L");
        assert_eq!(g[0].1, vec![std::path::PathBuf::from("l1.xisf"), std::path::PathBuf::from("l2.xisf")]);
        assert_eq!(g[1].0, "R");
        assert_eq!(g[1].1, vec![std::path::PathBuf::from("r1.xisf")]);
    }

    #[test]
    fn batch_rejects_duplicate_and_unsafe_names() {
        let dup = batch_matches(&["--group", "L", "a.xisf", "--group", "L", "b.xisf", "-s", "s", "-o", "o"]);
        let err = group_specs(&dup).unwrap_err().to_string();
        assert!(err.contains("used twice"), "{err}");
        let slash = batch_matches(&["--group", "L/R", "a.xisf", "-s", "s", "-o", "o"]);
        let err = group_specs(&slash).unwrap_err().to_string();
        assert!(err.contains("[A-Za-z0-9._-]+"), "{err}");
    }
```

Append to `crates/mmm/tests/cli.rs`:

```rust
#[test]
fn batch_merges_every_group_onto_one_grid() {
    let dir = tempdir("batch");
    let a = write_group(&dir.join("A"), "a", (0.0, 0.0), 150);
    let b = write_group(&dir.join("B"), "b", (40.0, 25.0), 140);
    let sessions = dir.join("sessions");
    let out_dir = dir.join("out");
    let out = run(mmm()
        .arg("batch")
        .arg("--group")
        .arg("L")
        .args(&a)
        .arg("--group")
        .arg("Ha")
        .args(&b)
        .arg("-s")
        .arg(&sessions)
        .arg("-o")
        .arg(&out_dir)
        .arg("--input")
        .arg("solved")
        .arg("--mode")
        .arg("feather")
        .arg("--feather")
        .arg("24"));
    assert!(out.contains("group L"), "{out}");
    assert!(out.contains("group Ha"), "{out}");
    assert!(matches!(
        ReferenceFrame::load(&sessions.join("reference.mmm-frame.json")).unwrap(),
        ReferenceFrame::Solved { .. }
    ));
    assert!(sessions.join("L.mmm-session").join("session.json").exists());
    assert!(sessions.join("Ha.mmm-session").join("session.json").exists());
    let (gl, wl) = geometry_and_wcs(&out_dir.join("L.fits"));
    let (gh, wh) = geometry_and_wcs(&out_dir.join("Ha.fits"));
    assert_eq!(gl, gh);
    assert_eq!(wl, wh);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn batch_refuses_bad_group_names_before_any_work() {
    let dir = tempdir("batch-names");
    let a = write_group(&dir.join("A"), "a", (0.0, 0.0), 150);
    let out = mmm()
        .arg("batch")
        .arg("--group")
        .arg("L")
        .args(&a)
        .arg("--group")
        .arg("L")
        .args(&a)
        .arg("-s")
        .arg(dir.join("sessions"))
        .arg("-o")
        .arg(dir.join("out"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("used twice"));
    assert!(!dir.join("sessions").exists(), "no work before validation");
    std::fs::remove_dir_all(&dir).unwrap();
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p mmm`
Expected: compile errors (`group_specs` missing; no `batch` subcommand).

- [ ] **Step 3: Implement `batch`**

Add the variant:

```rust
    /// Merge several panel groups (one per filter) onto one shared frame:
    /// derives the frame from every panel, analyzes each group into
    /// DIR/<name>.mmm-session and blends it to OUTDIR/<name>.fits, so the
    /// outputs can be combined directly (LRGB, Ha+RGB, …)
    Batch {
        /// A group: its name followed by its panel files. Repeat per group,
        /// e.g. `--group L L/*.xisf --group R R/*.xisf`
        #[arg(long = "group", num_args = 2.., action = clap::ArgAction::Append,
              value_names = ["NAME", "PANELS"], required = true)]
        group: Vec<String>,

        /// Directory for the reference frame and the per-group sessions
        #[arg(short, long)]
        session: std::path::PathBuf,

        /// Directory for the per-group output FITS files (<name>.fits)
        #[arg(short, long)]
        output: std::path::PathBuf,

        /// Also write an autostretched PNG preview per group (downsampled runs only)
        #[arg(long)]
        png: bool,

        #[command(flatten)]
        analyze: AnalyzeOpts,

        #[command(flatten)]
        blend: BlendOpts,
    },
```

Change the start of `main` so the raw matches are available:

```rust
    let matches = <Cli as clap::CommandFactory>::command().get_matches();
    let cli = <Cli as clap::FromArgMatches>::from_arg_matches(&matches)
        .unwrap_or_else(|e| e.exit());
```

and dispatch:

```rust
        Command::Batch { group: _, session, output, png, analyze, blend } => {
            let sub = matches.subcommand_matches("batch").expect("batch was matched");
            let groups = group_specs(sub)?;
            batch_cmd(groups, &session, &output, png, &analyze.resolve()?, &blend.resolve()?)
        }
```

Add the two functions:

```rust
/// The `--group NAME PANELS...` occurrences of a `batch` invocation, in
/// order. Names must be unique and match `[A-Za-z0-9._-]+` (they become
/// directory and file names).
fn group_specs(m: &clap::ArgMatches) -> anyhow::Result<Vec<(String, Vec<std::path::PathBuf>)>> {
    let mut groups: Vec<(String, Vec<std::path::PathBuf>)> = Vec::new();
    for values in m.get_occurrences::<String>("group").into_iter().flatten() {
        let mut it = values.cloned();
        let name = it.next().expect("num_args >= 2 guarantees a name");
        let panels: Vec<std::path::PathBuf> = it.map(std::path::PathBuf::from).collect();
        anyhow::ensure!(
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)),
            "group name '{name}' must match [A-Za-z0-9._-]+"
        );
        anyhow::ensure!(!panels.is_empty(), "group '{name}' has no panels");
        anyhow::ensure!(
            !groups.iter().any(|(n, _)| n == &name),
            "group name '{name}' is used twice"
        );
        groups.push((name, panels));
    }
    anyhow::ensure!(!groups.is_empty(), "batch needs at least one --group");
    Ok(groups)
}

/// `mmm batch`: one reference frame over every group, then analyze and blend
/// each group with it. Pure orchestration over `analyze_cmd` / `blend_cmd`.
fn batch_cmd(
    groups: Vec<(String, Vec<std::path::PathBuf>)>,
    session_dir: &std::path::Path,
    out_dir: &std::path::Path,
    png: bool,
    analyze: &AnalyzeConfig,
    blend: &BlendConfig,
) -> anyhow::Result<()> {
    use anyhow::Context;
    std::fs::create_dir_all(session_dir)
        .with_context(|| format!("cannot create {}", session_dir.display()))?;
    std::fs::create_dir_all(out_dir).with_context(|| format!("cannot create {}", out_dir.display()))?;

    let all: Vec<std::path::PathBuf> = groups.iter().flat_map(|(_, p)| p.iter().cloned()).collect();
    let reference = mmm_core::reference::derive(&all, analyze.input)?;
    let frame_path = session_dir.join("reference.mmm-frame.json");
    reference.save(&frame_path)?;
    println!(
        "reference: {} over {} panels in {} groups → {}",
        reference.describe(),
        all.len(),
        groups.len(),
        frame_path.display()
    );

    let session_of = |name: &str| session_dir.join(format!("{name}.mmm-session"));
    for (name, panels) in &groups {
        println!("\n== group {name}: analyze ({} panels)", panels.len());
        analyze_cmd(panels, &session_of(name), analyze, Some(&reference))
            .with_context(|| format!("group {name}: analyze failed"))?;
    }
    let mut outputs = Vec::with_capacity(groups.len());
    for (name, _) in &groups {
        println!("\n== group {name}: blend");
        let out = out_dir.join(format!("{name}.fits"));
        let png_path = png.then(|| out_dir.join(format!("{name}.png")));
        blend_cmd(&session_of(name), &out, png_path.as_deref(), blend)
            .with_context(|| format!("group {name}: blend failed"))?;
        outputs.push((name.clone(), out));
    }

    let (w, h) = reference.canvas();
    println!("\nbatch: {} outputs, all on the shared {w}x{h} grid:", outputs.len());
    for (name, out) in &outputs {
        println!("  {name}: {}", out.display());
    }
    Ok(())
}
```

- [ ] **Step 4: Run the tests and lints**

Run: `cargo test -p mmm && cargo clippy -p mmm --all-targets && cargo run -p mmm -- batch --help`
Expected: all `mmm` tests pass (two new unit tests, two new CLI tests); help shows `--group <NAME> <PANELS>...` and both option groups.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add crates/mmm/src/main.rs crates/mmm/tests/cli.rs
git commit -m "feat(cli): mmm batch merges named panel groups onto one shared frame

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 9: Documentation and final verification

**Files:**
- Modify: `docs/DESIGN.md` (new section after the "PixInsight 1.9.5" section or at the end of the phase log; CLI surface at lines 229-236; session directory at lines 216-226)
- Modify: `docs/superpowers/specs/2026-10-03-shared-reference-frame-design.md` (status line)

- [ ] **Step 1: Update `docs/DESIGN.md`**

In the "Session directory" block add the line:

```
  session.json        # … + frame_imposed when a shared reference frame was adopted
<name>.mmm-frame.json # shared reference frame (mmm frame / batch), outside any session
```

In the "CLI surface" block replace the `analyze` and `blend` lines and add `frame` and `batch`:

```
mmm frame <all panels…> -o F.mmm-frame.json [--input auto|aligned|solved]
mmm analyze <panels…> --session S [--frame F] [--surface off|0|1|2]
            [--input auto|aligned|solved] [--gain fit|unity]
mmm report --session S [--seam-png P]   # graph + fit/seam tables, ⚠ on outliers
mmm blend --session S -o out.fits [--downsample 1|8] [--feather PX]
          [--mode pyramid|twoband|feather] [--png P] [--roi x,y,w,h]
          [--defect-veto on|off] [--flatten off|1|2] [--wcs-frame topdown|flipped]
          [--extent auto|union|canvas]
mmm batch --group NAME <panels…> [--group …] -s DIR -o OUTDIR [--png]
          [analyze options] [blend options]
```

Add a new section (before "## IPC transport (PixInsight)"):

```markdown
## Shared reference frame across groups (2026-10-03)

Mono imagers merge one mosaic per filter and combine afterwards, which needs
every output on one pixel grid. Two mechanisms broke that: `choose_frame`
derives the frame from the panels it is given (per-filter pointings differ by
fractions of a pixel), and the blend crops to the union of content bboxes
(per-filter coverage differs). Spec:
[shared reference frame design](superpowers/specs/2026-10-03-shared-reference-frame-design.md).

- **`reference::ReferenceFrame`** (`*.mmm-frame.json`, version 1): `solved`
  wraps a `MosaicFrame`; `aligned` wraps the canvas geometry plus the first
  panel's canvas WCS. No channel count — OSC and mono groups may share one.
  `reference::derive` is header-only over *every* panel of *every* group
  (cheap half of the auto-detect rule; same-geometry raw panels still need
  `--input solved`).
- **`analyze --frame F`** adopts the frame instead of choosing one. Solved
  input: footprint check — every panel's boundary samples must land inside
  the frame, else a hard error naming the panel, side and overshoot
  (`check_footprints`; no clipping by design). Aligned input: the canvas must
  equal the frame's and, when both carry a WCS, corners and centre must
  agree within `ALIGNED_WCS_TOLERANCE_PX = 0.05` px (`check_aligned`); the
  hint is tool-neutral ("align every group to one common reference, or
  process the groups separately"). Kind mismatch (solved frame, aligned
  set or vice versa) is an error. The session records `frame_imposed`.
- **Blend extent** (`blend::Extent`): `Union` (historical crop) or `Canvas`
  (whole canvas, zero outside coverage). Default: `Canvas` when
  `frame_imposed`, else `Union` — pre-existing sessions are byte-identical
  (regression guard unchanged). `--roi` intersects the active extent.
- **`mmm batch`** is CLI-only orchestration: derive once over all groups
  into `DIR/reference.mmm-frame.json`, analyze each group into
  `DIR/<name>.mmm-session`, blend each to `OUTDIR/<name>.fits`. Groups are
  `--group NAME panels…` occurrences (recovered through
  `ArgMatches::get_occurrences`; clap's grouped `Vec<Vec<T>>` derive is
  unstable).
- **Aligned-input caveat for users**: filters registered separately get
  separate canvases; either align all against one common reference or feed
  the raw solved panels.
- **PixInsight (later stage)**: `InitJob` gains optional `frame` and
  `extent`; the worker passes them into the same entry points. Not in this
  stage.
```

- [ ] **Step 2: Mark the spec implemented**

In the spec's `Status:` line, replace `approved design 2026-10-03, not yet implemented.` with `implemented 2026-10-03 (stage 1: mmm-core + CLI).`.

- [ ] **Step 3: Full verification**

Run:

```bash
cargo fmt --check
cargo clippy --all-targets --workspace
cargo doc --no-deps -p mmm-core 2>&1 | grep -i warn ; true
cargo test --workspace
```

Expected: no diffs, no warnings, no doc warnings, all tests pass (including `regression_guard` with unchanged constants and the IPC worker end-to-end tests).

- [ ] **Step 4: Commit**

```bash
git add docs/DESIGN.md docs/superpowers/specs/2026-10-03-shared-reference-frame-design.md
git commit -m "docs: shared reference frame — DESIGN.md section, CLI surface, session layout

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

## Self-review notes

- **Spec coverage**: §1 → Task 1; §2 → Task 2; §3 (footprint, aligned check, kind mismatch, `frame_imposed`) → Tasks 3–4; §4 → Task 5; §5 → Tasks 7–8 (the `--png` flag is a boolean on `batch`, a path on `blend`, as the spec implies); §6 → Tasks 1–8 tests (round-trip, version, derive detection, footprint, aligned mismatch, kind mismatch, extent default via regression guard and unit tests, two-group e2e both paths, batch e2e); §7 → Task 9; §8 is explicitly deferred.
- **Type consistency**: `analyze_full`'s new trailing `Option<&ReferenceFrame>` is used identically in Tasks 4, 6, 7; `Extent`/`extent` names match between Tasks 5 and 7; `AnalyzeConfig`/`BlendConfig` fields match between Tasks 7 and 8.
- **Known uncertainty, handled in-step**: `serde(flatten)` over the tagged enum in Task 1 (fallback given), Pyramid on the tiny fixture in Task 6 (fallback given, Pyramid never dropped).
