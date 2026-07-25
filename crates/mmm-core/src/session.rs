//! Session directory: persistent home for all cached analysis artifacts.
//!
//! Layout (see `docs/DESIGN.md`):
//! ```text
//! <name>.mmm-session/
//!   session.json              # canvas geometry, panel list + metadata
//!   panels/<id>/summary.bin   # L8 summary planes
//!   analysis/…                # overlap graph, photometry (later stages)
//! ```
//! `session.json` holds everything needed to re-run any stage without
//! re-scanning inputs; summary paths are reconstructed from the directory.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::panel_reader::PanelStorage;
use crate::{Error, Result};

/// Per-panel metadata recorded by the analyze stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PanelMeta {
    pub id: usize,
    /// File the panel's pixel data is read from: the source XISF for
    /// full-canvas input, the `aligned.bin` reprojection cache otherwise
    /// (see [`PanelStorage`]).
    pub path: PathBuf,
    /// Content bounding box of covered pixels: `[x0, y0, x1, y1]`, exclusive.
    pub bbox: [u64; 4],
    /// Fraction of canvas pixels covered (all channels nonzero).
    pub nonzero_frac: f64,
    /// Per-channel min over covered pixels (0 if none).
    pub ch_min: Vec<f32>,
    /// Per-channel max over covered pixels (0 if none).
    pub ch_max: Vec<f32>,
    /// Per-channel mean over covered pixels (0 if none).
    pub ch_mean: Vec<f64>,
    /// How the pixel data at `path` is stored. Defaults to
    /// [`PanelStorage::FullCanvasXisf`] so pre-phase-5 `session.json` files
    /// (which lack the field) keep reading unchanged.
    #[serde(default)]
    pub storage: PanelStorage,
}

/// A mosaic session: canvas geometry plus the analyzed panel set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Session directory; not serialized — reconstructed by [`Session::open`].
    #[serde(skip)]
    pub dir: PathBuf,
    /// Canvas geometry `(width, height, channels)`, identical across panels.
    pub canvas: (u64, u64, u64),
    pub panels: Vec<PanelMeta>,
}

impl Session {
    /// Create the session directory (and parents) with an empty panel set.
    pub fn create(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        Ok(Self { dir: dir.to_path_buf(), canvas: (0, 0, 0), panels: Vec::new() })
    }

    /// Open an existing session by reading its `session.json`.
    pub fn open(dir: &Path) -> Result<Self> {
        let path = dir.join("session.json");
        let json = std::fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
        let mut session: Session = serde_json::from_str(&json)
            .map_err(|e| Error::format(&path, format!("bad session.json: {e}")))?;
        session.dir = dir.to_path_buf();
        Ok(session)
    }

    /// Persist `session.json` into the session directory.
    pub fn save(&self) -> Result<()> {
        let path = self.dir.join("session.json");
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| Error::format(&path, format!("serialize session: {e}")))?;
        std::fs::write(&path, json).map_err(|e| Error::io(&path, e))
    }

    /// Path of panel `id`'s L8 summary: `panels/<id>/summary.bin`.
    pub fn summary_path(&self, id: usize) -> PathBuf {
        self.dir.join("panels").join(id.to_string()).join("summary.bin")
    }

    /// Path of panel `id`'s reprojection cache: `panels/<id>/aligned.bin`.
    /// Only exists for solved (reprojected) input panels.
    pub fn aligned_path(&self, id: usize) -> PathBuf {
        self.dir.join("panels").join(id.to_string()).join("aligned.bin")
    }

    /// Path of the overlap graph: `analysis/overlap_graph.json`.
    pub fn overlap_graph_path(&self) -> PathBuf {
        self.dir.join("analysis").join("overlap_graph.json")
    }

    /// Path of the photometric solve results: `analysis/photometry.json`.
    pub fn photometry_path(&self) -> PathBuf {
        self.dir.join("analysis").join("photometry.json")
    }

    /// Path of the residual surface corrections: `analysis/surfaces.json`.
    /// Absent when analyze ran with surfaces disabled.
    pub fn surfaces_path(&self) -> PathBuf {
        self.dir.join("analysis").join("surfaces.json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_save_open_round_trip() {
        let dir = std::env::temp_dir().join(format!("mmm-session-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let mut session = Session::create(&dir).unwrap();
        session.canvas = (640, 480, 3);
        session.panels.push(PanelMeta {
            id: 0,
            path: PathBuf::from("/data/panel0.xisf"),
            bbox: [10, 20, 300, 400],
            nonzero_frac: 0.25,
            ch_min: vec![0.001, 0.002, 0.003],
            ch_max: vec![0.9, 0.8, 0.7],
            ch_mean: vec![0.01, 0.02, 0.03],
            storage: PanelStorage::CroppedCache { bbox: [10, 20, 300, 400] },
        });
        session.save().unwrap();

        let reopened = Session::open(&dir).unwrap();
        assert_eq!(reopened.dir, dir);
        assert_eq!(reopened.canvas, (640, 480, 3));
        assert_eq!(reopened.panels.len(), 1);
        let p = &reopened.panels[0];
        assert_eq!(p.id, 0);
        assert_eq!(p.bbox, [10, 20, 300, 400]);
        assert_eq!(p.ch_max, vec![0.9, 0.8, 0.7]);
        assert_eq!(p.storage, PanelStorage::CroppedCache { bbox: [10, 20, 300, 400] });
        assert_eq!(
            reopened.summary_path(0),
            dir.join("panels").join("0").join("summary.bin")
        );
        assert_eq!(
            reopened.aligned_path(0),
            dir.join("panels").join("0").join("aligned.bin")
        );
        assert_eq!(
            reopened.surfaces_path(),
            dir.join("analysis").join("surfaces.json")
        );

        // Later stages depend on these exact JSON field names.
        let json = std::fs::read_to_string(dir.join("session.json")).unwrap();
        for key in ["canvas", "panels", "id", "path", "bbox", "nonzero_frac", "ch_min", "ch_max", "ch_mean"] {
            assert!(json.contains(&format!("\"{key}\"")), "session.json missing key {key}");
        }
        // The directory itself must not be baked into the JSON.
        assert!(!json.contains("\"dir\""));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Pre-phase-5 `session.json` files carry no `storage` field: they must
    /// keep deserializing, defaulting to full-canvas XISF storage.
    #[test]
    fn session_without_storage_field_defaults_to_full_canvas() {
        let dir = std::env::temp_dir().join(format!("mmm-session-old-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("session.json"),
            r#"{
              "canvas": [640, 480, 3],
              "panels": [{
                "id": 0, "path": "/data/panel0.xisf", "bbox": [10, 20, 300, 400],
                "nonzero_frac": 0.25, "ch_min": [0.001], "ch_max": [0.9], "ch_mean": [0.01]
              }]
            }"#,
        )
        .unwrap();
        let session = Session::open(&dir).unwrap();
        assert_eq!(session.panels[0].storage, PanelStorage::FullCanvasXisf);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn open_missing_session_errors() {
        let dir = std::env::temp_dir().join(format!("mmm-session-miss-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(Session::open(&dir).is_err());
    }
}
