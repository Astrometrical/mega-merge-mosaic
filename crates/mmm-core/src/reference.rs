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
}
