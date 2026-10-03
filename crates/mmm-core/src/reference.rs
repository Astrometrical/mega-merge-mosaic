//! Shared reference frame: the one piece of state several sessions adopt so
//! that their blended outputs land on a common pixel grid — one mosaic per
//! filter for a mono imager, combined afterwards (LRGB, Ha+RGB, …).
//!
//! A [`ReferenceFrame`] is persisted as `<name>.mmm-frame.json`, derived
//! header-only from *every* panel of *every* group by [`fn@derive`], and handed
//! to the analyze stage ([`crate::analyze::analyze_full`] with
//! `Some(&frame)`), which adopts it instead of choosing its own frame and
//! checks that the group fits it ([`check_footprints`], [`check_aligned`]).
//! Design: `docs/superpowers/specs/2026-10-03-shared-reference-frame-design.md`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::align::{MosaicFrame, choose_frame};
use crate::analyze::InputSelect;
use crate::astrometry::{LinearWcs, WcsModel, describe_unsolved, wcs_from_properties};
use crate::formats::InputPanel;
use crate::ipc::protocol::PanelDesc;
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
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
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
    let opened: Vec<InputPanel> = paths
        .iter()
        .map(|p| InputPanel::open(p))
        .collect::<Result<_>>()?;
    let items = opened
        .iter()
        .zip(paths)
        .map(|(x, p)| DeriveItem {
            label: p.display().to_string(),
            width: x.width(),
            height: x.height(),
            wcs: x.linear_wcs(),
            model: x.wcs_model(),
        })
        .collect();
    derive_items(items, input)
}

/// [`fn@derive`] over wire panel descriptors (a Views-mode host's `PanelDesc`s
/// with the astrometric properties attached): the same kind rule and frame
/// choice, so a frame derived from views equals one derived from the same
/// panels saved to disk. Labels in errors are `panel <id>`.
pub fn derive_from_descs(panels: &[PanelDesc], input: InputSelect) -> Result<ReferenceFrame> {
    let items = panels
        .iter()
        .map(|p| DeriveItem {
            label: format!("panel {}", p.panel_id),
            width: p.width,
            height: p.height,
            wcs: wcs_from_properties(&p.properties),
            model: WcsModel::from_properties(&p.properties, p.width, p.height)
                .ok_or_else(|| describe_unsolved(&p.properties)),
        })
        .collect();
    derive_items(items, input)
}

/// One panel's header-level facts for frame derivation, from a file or a
/// wire descriptor — the two sources must never drift, so both feed
/// [`derive_items`].
struct DeriveItem {
    label: String,
    width: u64,
    height: u64,
    wcs: Option<LinearWcs>,
    model: std::result::Result<WcsModel, String>,
}

fn derive_items(items: Vec<DeriveItem>, input: InputSelect) -> Result<ReferenceFrame> {
    if items.is_empty() {
        return Err(Error::compute("no input panels given"));
    }
    let same_geometry = items
        .iter()
        .all(|i| (i.width, i.height) == (items[0].width, items[0].height));
    let aligned = match input {
        InputSelect::Aligned => true,
        InputSelect::Solved => false,
        InputSelect::Auto => items.len() >= 2 && same_geometry,
    };
    if aligned {
        if let Some(k) = items
            .iter()
            .position(|i| (i.width, i.height) != (items[0].width, items[0].height))
        {
            return Err(Error::compute(format!(
                "aligned input needs one canvas geometry, but {} is {}x{} and {} is {}x{}",
                items[0].label,
                items[0].width,
                items[0].height,
                items[k].label,
                items[k].width,
                items[k].height
            )));
        }
        let first = items.into_iter().next().expect("non-empty");
        return Ok(ReferenceFrame::Aligned {
            width: first.width,
            height: first.height,
            wcs: first.wcs,
        });
    }
    let mut models: Vec<WcsModel> = Vec::with_capacity(items.len());
    let mut errors: Vec<String> = Vec::new();
    for item in items {
        match item.model {
            Ok(m) => models.push(m),
            Err(reason) => errors.push(format!("{}: {reason}", item.label)),
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
        let missing = ReferenceFrame::load(&dir.join("absent.json"))
            .unwrap_err()
            .to_string();
        assert!(missing.contains("absent.json"), "{missing}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

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
            [
                ("fits.xisf".to_string(), &m),
                ("P3_Ha.xisf".to_string(), &shifted),
            ],
            &frame,
        )
        .unwrap_err();
        assert!(err.contains("P3_Ha.xisf"), "{err}");
        assert!(!err.contains("fits.xisf"), "{err}");
        assert!(
            ["23 px", "24 px", "25 px"]
                .iter()
                .any(|px| err.contains(&format!("{px} beyond the bottom edge"))),
            "{err}"
        );
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
            err.contains(
                "align every group to one common reference, or process the groups separately"
            ),
            "{err}"
        );
    }

    #[test]
    fn aligned_check_refuses_displaced_wcs_quoting_pixels() {
        let reference = sample_wcs();
        let mut shifted = reference.clone();
        shifted.crpix[0] += 2.0; // the same sky lands 2 px to the right
        let err =
            check_aligned((240, 200), Some(&shifted), 240, 200, Some(&reference)).unwrap_err();
        assert!(err.contains("2.00 px"), "{err}");
        assert!(
            err.contains("align every group to one common reference"),
            "{err}"
        );
        // Within tolerance: a 0.01 px shift passes.
        let mut tiny = reference.clone();
        tiny.crpix[0] += 0.01;
        check_aligned((240, 200), Some(&tiny), 240, 200, Some(&reference)).unwrap();
    }
}
