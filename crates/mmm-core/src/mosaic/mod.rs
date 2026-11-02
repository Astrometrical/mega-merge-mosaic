//! The mosaic project: its persisted configuration (`mosaic.json`), panel
//! grouping, the names of the files in a mosaic directory, and the
//! orchestration that analyzes every group and blends a group into its tiled
//! result ([`Mosaic`], [`analyze_all`], [`blend_group`]).
//!
//! # Concurrency contract
//!
//! - One [`crate::tiles::TileStore`] writer per result file; nothing
//!   enforces it yet (a later plan adds an in-process guard). Readers must
//!   close before [`blend_group`] recreates a store (see [`crate::tiles`]).
//! - **Cancel and join a running [`blend_group`] or [`analyze_all`] before
//!   calling any `set_*`.** A `set_*` that lands while a blend writes
//!   invalidates the store, then the blend marks the tiles it writes
//!   afterwards valid although they were blended with the old settings:
//!   stale tiles marked valid.
//! - The job holding `&mut Mosaic` saves the configuration (fingerprints,
//!   `needs_analyze`, provenance). The app must not edit a clone of the
//!   configuration while such a job runs: whichever saves last wins.

pub mod grouping;
mod run;

pub use run::{
    Mosaic, analyze_all, blend_group, set_analyze_settings, set_blend_settings, set_group_override,
};

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::checks::CheckKey;
use crate::error::{Error, Result};
use crate::settings::{AnalyzeSettings, BlendSettings, GroupOverride};

/// Configuration file name inside a mosaic directory.
pub const CONFIG_FILE: &str = "mosaic.json";
/// Shared reference frame file name.
pub const FRAME_FILE: &str = "reference.mmm-frame.json";
/// Directory holding the per-group sessions.
pub const GROUPS_DIR: &str = "groups";
/// Tiled result store file name.
pub const RESULT_FILE: &str = "result.mmmt";
/// Highest `mosaic.json` format this build reads.
pub const CONFIG_FORMAT: u32 = 1;
/// Name of the processing group holding panels no rule could place.
pub const UNGROUPED: &str = "No group";
/// Group name used when grouping is off.
pub const SINGLE_GROUP: &str = "mosaic";

/// Cheap identity of a file: size and modification time.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Fingerprint {
    /// File length in bytes.
    pub len: u64,
    /// Modification time, milliseconds since the Unix epoch.
    pub modified_unix_ms: i64,
}

/// Fingerprint `path` from its filesystem metadata.
pub fn fingerprint(path: &Path) -> Result<Fingerprint> {
    let md = std::fs::metadata(path).map_err(|e| Error::io(path, e))?;
    let modified = md.modified().map_err(|e| Error::io(path, e))?;
    let modified_unix_ms = match modified.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(e) => -(e.duration().as_millis() as i64),
    };
    Ok(Fingerprint {
        len: md.len(),
        modified_unix_ms,
    })
}

/// One input panel in the mosaic.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PanelEntry {
    /// Path of the panel file.
    pub path: PathBuf,
    /// Fingerprint recorded when the panel was last analysed.
    #[serde(default)]
    pub fingerprint: Option<Fingerprint>,
    /// Group assigned by hand; wins over the grouping rule.
    #[serde(default)]
    pub manual_group: Option<String>,
}

/// How panels are assigned to groups automatically.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GroupRule {
    /// Group by the value of a header keyword (e.g. `FILTER`).
    Header {
        /// The keyword name.
        key: String,
    },
    /// Group by the file-name token following a keyword.
    FilenameToken {
        /// The keyword preceding the group token.
        keyword: String,
    },
}

impl Default for GroupRule {
    fn default() -> Self {
        GroupRule::Header {
            key: "FILTER".into(),
        }
    }
}

/// Grouping configuration.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Grouping {
    /// Whether panels are split into groups.
    pub enabled: bool,
    /// The automatic rule.
    #[serde(default)]
    pub rule: GroupRule,
}

/// One entry of the mosaic's audit trail.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ProvenanceEntry {
    /// When the action happened, milliseconds since the Unix epoch.
    pub unix_ms: i64,
    /// `mmm-core` version that performed it.
    pub mmm_version: String,
    /// What was done.
    pub action: String,
    /// The group concerned, if any.
    #[serde(default)]
    pub group: Option<String>,
    /// Free-form details.
    #[serde(default)]
    pub detail: serde_json::Value,
}

/// The persisted mosaic project (`mosaic.json`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct MosaicConfig {
    /// File format version ([`CONFIG_FORMAT`]).
    pub format: u32,
    /// Input panels.
    pub panels: Vec<PanelEntry>,
    /// Grouping configuration.
    #[serde(default)]
    pub grouping: Grouping,
    /// Global analyze settings.
    #[serde(default)]
    pub analyze: AnalyzeSettings,
    /// Global blend settings.
    #[serde(default)]
    pub blend: BlendSettings,
    /// Per-group setting overrides, by group name.
    #[serde(default)]
    pub group_overrides: BTreeMap<String, GroupOverride>,
    /// Group name to its directory under [`GROUPS_DIR`], assigned once by
    /// [`grouping::allocate_group_dir`] and never changed afterwards.
    #[serde(default)]
    pub group_dirs: BTreeMap<String, String>,
    /// Groups whose effective analyze settings changed since they were last
    /// analyzed: [`blend_group`] refuses them until [`analyze_all`] reruns.
    #[serde(default)]
    pub needs_analyze: BTreeSet<String>,
    /// Check results the user has accepted.
    #[serde(default)]
    pub accepted_checks: Vec<CheckKey>,
    /// Audit trail.
    #[serde(default)]
    pub provenance: Vec<ProvenanceEntry>,
    /// Opaque application state (compositions, stretches, ROIs, view state);
    /// stored but never interpreted by the core.
    #[serde(default)]
    pub ui: serde_json::Value,
}

impl MosaicConfig {
    /// A fresh configuration over `paths` with default settings.
    pub fn new(paths: Vec<PathBuf>) -> Self {
        MosaicConfig {
            format: CONFIG_FORMAT,
            panels: paths
                .into_iter()
                .map(|path| PanelEntry {
                    path,
                    fingerprint: None,
                    manual_group: None,
                })
                .collect(),
            grouping: Grouping::default(),
            analyze: AnalyzeSettings::default(),
            blend: BlendSettings::default(),
            group_overrides: BTreeMap::new(),
            group_dirs: BTreeMap::new(),
            needs_analyze: BTreeSet::new(),
            accepted_checks: Vec::new(),
            provenance: Vec::new(),
            ui: serde_json::Value::Null,
        }
    }

    /// Load `mosaic.json` from `dir`. A newer `format` than this build
    /// understands is rejected.
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join(CONFIG_FILE);
        let bytes = std::fs::read(&path).map_err(|e| Error::io(&path, e))?;
        let cfg: MosaicConfig = serde_json::from_slice(&bytes)
            .map_err(|e| Error::format(&path, format!("invalid mosaic configuration: {e}")))?;
        if cfg.format > CONFIG_FORMAT {
            return Err(Error::format(
                &path,
                format!(
                    "mosaic format {} is newer than this version of mmm supports ({CONFIG_FORMAT}); \
                     upgrade mmm to open it",
                    cfg.format
                ),
            ));
        }
        Ok(cfg)
    }

    /// Save to `dir/mosaic.json` atomically (temp file, then rename).
    ///
    /// Refuses (leaving the file untouched) a configuration whose base or
    /// any group's effective blend settings fail
    /// [`BlendSettings::validate`]: JSON cannot hold a NaN or infinite
    /// feather, and a file written with one would no longer load.
    pub fn save(&self, dir: &Path) -> Result<()> {
        self.blend.validate()?;
        for group in self.group_overrides.keys() {
            self.settings_for(group).1.validate()?;
        }
        let path = dir.join(CONFIG_FILE);
        let tmp = dir.join(format!("{CONFIG_FILE}.tmp"));
        let json = serde_json::to_vec_pretty(self)
            .map_err(|e| Error::format(&path, format!("cannot serialise configuration: {e}")))?;
        std::fs::write(&tmp, json).map_err(|e| Error::io(&tmp, e))?;
        std::fs::rename(&tmp, &path).map_err(|e| Error::io(&path, e))
    }

    /// The effective settings for `group`: the base settings with the
    /// group's override, if any, applied.
    pub fn settings_for(&self, group: &str) -> (AnalyzeSettings, BlendSettings) {
        match self.group_overrides.get(group) {
            Some(o) => (self.analyze.with(&o.analyze), self.blend.with(&o.blend)),
            None => (self.analyze.clone(), self.blend.clone()),
        }
    }

    /// Append a provenance entry stamped with the current time and version.
    pub fn push_provenance(
        &mut self,
        action: &str,
        group: Option<&str>,
        detail: serde_json::Value,
    ) {
        let unix_ms = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        self.provenance.push(ProvenanceEntry {
            unix_ms,
            mmm_version: env!("CARGO_PKG_VERSION").to_string(),
            action: action.to_string(),
            group: group.map(str::to_string),
            detail,
        });
    }
}

/// True when `dir` holds a mosaic configuration.
pub fn is_mosaic_dir(dir: &Path) -> bool {
    dir.join(CONFIG_FILE).is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{BlendOverride, GroupOverride};

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mmm-mosaic-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn config_roundtrip_and_atomic_save() {
        let dir = tmp("rt");
        let mut cfg = MosaicConfig::new(vec!["a.xisf".into(), "b.xisf".into()]);
        cfg.grouping.enabled = true;
        cfg.panels[0].manual_group = Some("Ha".into());
        cfg.ui = serde_json::json!({"zoom": 2});
        cfg.push_provenance("analyze", Some("Ha"), serde_json::json!({"n": 1}));
        assert!(!is_mosaic_dir(&dir));
        cfg.save(&dir).unwrap();
        assert!(is_mosaic_dir(&dir));
        assert!(!dir.join("mosaic.json.tmp").exists());
        assert_eq!(MosaicConfig::load(&dir).unwrap(), cfg);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn future_format_rejected() {
        let dir = tmp("future");
        let mut cfg = MosaicConfig::new(vec![]);
        cfg.format = CONFIG_FORMAT + 1;
        cfg.save(&dir).unwrap();
        let err = MosaicConfig::load(&dir).unwrap_err();
        assert!(matches!(err, Error::Format { .. }));
        assert!(err.to_string().contains("upgrade"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_refuses_invalid_settings() {
        let dir = tmp("invalid");
        let cfg = MosaicConfig::new(vec![]);
        cfg.save(&dir).unwrap();
        let before = std::fs::read(dir.join(CONFIG_FILE)).unwrap();
        let mut bad = cfg.clone();
        bad.blend.feather_px = f32::NAN;
        assert!(bad.save(&dir).unwrap_err().to_string().contains("feather"));
        let mut bad = cfg.clone();
        bad.group_overrides.insert(
            "Ha".into(),
            GroupOverride {
                blend: BlendOverride {
                    feather_px: Some(-3.0),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        assert!(bad.save(&dir).unwrap_err().to_string().contains("feather"));
        assert_eq!(std::fs::read(dir.join(CONFIG_FILE)).unwrap(), before);
        assert_eq!(MosaicConfig::load(&dir).unwrap(), cfg);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn settings_for_applies_override() {
        let mut cfg = MosaicConfig::new(vec![]);
        let base = cfg.blend.feather_px;
        cfg.group_overrides.insert(
            "Ha".into(),
            GroupOverride {
                blend: BlendOverride {
                    feather_px: Some(base + 7.0),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        assert_eq!(cfg.settings_for("Ha").1.feather_px, base + 7.0);
        assert_eq!(cfg.settings_for("OIII").1.feather_px, base);
        assert_eq!(cfg.settings_for("Ha").0, cfg.analyze);
    }

    #[test]
    fn fingerprint_reports_length() {
        let dir = tmp("fp");
        let p = dir.join("f.bin");
        std::fs::write(&p, [0u8; 5]).unwrap();
        let fp = fingerprint(&p).unwrap();
        assert_eq!(fp.len, 5);
        assert!(fp.modified_unix_ms > 0);
        std::fs::remove_dir_all(&dir).ok();
    }
}
