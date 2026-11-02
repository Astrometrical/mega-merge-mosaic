//! Mosaic orchestration: analyze every processing group onto one shared
//! reference frame, blend a group into its tiled result store, and apply
//! settings edits, staling exactly the result stores they affect.
//!
//! On-disk layout of a mosaic directory:
//!
//! ```text
//! <dir>/mosaic.json                  configuration (MosaicConfig)
//! <dir>/reference.mmm-frame.json     shared reference frame
//! <dir>/groups/<group dir>/          one analyze session per group
//! <dir>/groups/<group dir>/result.mmmt   the group's tiled result
//! ```

use std::path::{Path, PathBuf};

use serde_json::json;

use super::grouping::{Groups, allocate_group_dir, header_value, resolve_groups};
use super::{FRAME_FILE, GROUPS_DIR, GroupRule, MosaicConfig, RESULT_FILE, fingerprint};
use crate::analyze::analyze_ctl;
use crate::error::{Error, Result};
use crate::formats::InputPanel;
use crate::job::JobCtl;
use crate::overlap::OverlapGraph;
use crate::photometry::Photometry;
use crate::reference;
use crate::session::Session;
use crate::settings::{
    AnalyzeSettings, BlendSettings, ChangeImpact, GroupOverride, analyze_change, blend_change,
};
use crate::surfaces::Surfaces;
use crate::tiles::{TILE, TileGeometry, TileStore, blend_tiles};

/// An open mosaic project: its directory and configuration.
#[derive(Clone, Debug, PartialEq)]
pub struct Mosaic {
    /// The mosaic directory.
    pub dir: PathBuf,
    /// The configuration, as last loaded or edited (see [`Mosaic::save`]).
    pub config: MosaicConfig,
}

impl Mosaic {
    /// Create `dir` (and parents) and save `config` into it.
    pub fn create(dir: &Path, config: MosaicConfig) -> Result<Self> {
        std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        let m = Mosaic {
            dir: dir.to_path_buf(),
            config,
        };
        m.save()?;
        Ok(m)
    }

    /// Open the mosaic in `dir` (loads `mosaic.json`).
    pub fn open(dir: &Path) -> Result<Self> {
        Ok(Mosaic {
            dir: dir.to_path_buf(),
            config: MosaicConfig::load(dir)?,
        })
    }

    /// Save the configuration to `mosaic.json` (atomically).
    pub fn save(&self) -> Result<()> {
        self.config.save(&self.dir)
    }

    /// Resolve the panels into groups with the configured rule. Header
    /// values are read header-only through [`InputPanel::open`]; a panel that
    /// fails to open has no header value (it lands in the ungrouped set
    /// rather than failing the call).
    pub fn groups(&self) -> Result<Groups> {
        let header = |p: &Path| match &self.config.grouping.rule {
            GroupRule::Header { key } => InputPanel::open(p)
                .ok()
                .and_then(|panel| header_value(&panel, key)),
            GroupRule::FilenameToken { .. } => None,
        };
        Ok(resolve_groups(&self.config, &header))
    }

    /// The session directory of `group`: `<dir>/groups/<dir name>`. The dir
    /// name is the group's persisted entry in [`MosaicConfig::group_dirs`];
    /// for a group without one it is the name
    /// [`Mosaic::assign_group_dirs`] would allocate (not recorded here).
    pub fn group_session_dir(&self, group: &str) -> PathBuf {
        let name = allocate_group_dir(&self.config.group_dirs, group);
        self.dir.join(GROUPS_DIR).join(name)
    }

    /// The tiled result store of `group` (inside its session directory).
    pub fn result_path(&self, group: &str) -> PathBuf {
        self.group_session_dir(group).join(RESULT_FILE)
    }

    /// Record a directory for every processing group in `groups` that has
    /// none yet (see [`allocate_group_dir`]); existing assignments never
    /// change. Returns whether anything was added. Does not save.
    pub fn assign_group_dirs(&mut self, groups: &Groups) -> bool {
        let mut added = false;
        for (name, _) in groups.processing_groups() {
            if !self.config.group_dirs.contains_key(&name) {
                let d = allocate_group_dir(&self.config.group_dirs, &name);
                self.config.group_dirs.insert(name, d);
                added = true;
            }
        }
        added
    }
}

/// Analyze every processing group onto one shared reference frame.
///
/// Resolves the groups, derives the reference frame over **all** panels
/// (with the base [`AnalyzeSettings::input`]) and saves it to
/// [`FRAME_FILE`], then analyzes each processing group into its session
/// directory with the group's effective analyze settings. Progress stages
/// are prefixed with the group name (`"Ha: analyze"`). Afterwards every
/// panel's fingerprint is refreshed, an `"analyze"` provenance entry
/// recording each group's resolved settings is appended, and the
/// configuration is saved.
///
/// New groups get their directories recorded ([`Mosaic::assign_group_dirs`])
/// and saved before any analysis runs. Each group is removed from
/// [`MosaicConfig::needs_analyze`] (and the configuration saved) as soon as
/// its analysis succeeds, so a cancelled or failed run keeps the groups it
/// finished; it returns the error without adding provenance or refreshing
/// fingerprints.
pub fn analyze_all(m: &mut Mosaic, ctl: &JobCtl) -> Result<Groups> {
    ctl.check()?;
    let groups = m.groups()?;
    if m.assign_group_dirs(&groups) {
        m.save()?;
    }
    let all: Vec<PathBuf> = m.config.panels.iter().map(|p| p.path.clone()).collect();
    let frame = reference::derive(&all, m.config.analyze.input)?;
    frame.save(&m.dir.join(FRAME_FILE))?;

    let mut detail = serde_json::Map::new();
    for (group, paths) in groups.processing_groups() {
        ctl.check()?;
        let settings = m.config.settings_for(&group).0;
        let prefixed = |stage: &str, done: u64, total: u64| {
            // Cancellation is observed through the shared token below.
            let _ = ctl.report(&format!("{group}: {stage}"), done, total);
        };
        let group_ctl = JobCtl::new(ctl.cancel_token().clone(), Some(&prefixed));
        let dir = m.group_session_dir(&group);
        analyze_ctl(&paths, &dir, &settings, Some(&frame), &group_ctl)?;
        if m.config.needs_analyze.remove(&group) {
            m.save()?;
        }
        detail.insert(group.clone(), json!(settings));
    }

    for p in &mut m.config.panels {
        p.fingerprint = Some(fingerprint(&p.path)?);
    }
    m.config
        .push_provenance("analyze", None, json!({ "groups": detail }));
    m.save()?;
    Ok(groups)
}

/// Blend `rect` (level-0 canvas coordinates; `None` = the whole canvas) of
/// `group` into its tiled result store; returns the tile-aligned rect
/// blended (see [`blend_tiles`]).
///
/// Loads the group's session, overlap graph, photometry and (when present)
/// residual surfaces, as `mmm blend` does. The result store is opened if it
/// exists with the canvas geometry; it is (re)created when missing,
/// unreadable, or of a different geometry. Callers must not let that
/// recreation happen while readers ([`crate::tiles::TileReader`]) still have
/// the store mapped: recreating truncates the file under their mapping. An
/// existing store of matching geometry is never recreated.
///
/// Refuses (with [`Error::Compute`]) a group that has never been analyzed
/// (no recorded directory) or whose analyze settings changed since its
/// analysis ([`MosaicConfig::needs_analyze`]): run [`analyze_all`] first.
///
/// Appends a `"blend"` provenance entry (group, aligned rect, settings) and
/// saves the configuration.
pub fn blend_group(
    m: &mut Mosaic,
    group: &str,
    rect: Option<[u64; 4]>,
    ctl: &JobCtl,
) -> Result<[u64; 4]> {
    if !m.config.group_dirs.contains_key(group) {
        return Err(Error::compute(format!(
            "group '{group}' has not been analyzed yet — analyze first"
        )));
    }
    if m.config.needs_analyze.contains(group) {
        return Err(Error::compute(format!(
            "analyze settings for group '{group}' changed since it was analyzed — analyze first"
        )));
    }
    let session = Session::open(&m.group_session_dir(group))?;
    let graph = OverlapGraph::load(&session.overlap_graph_path())?;
    let phot = Photometry::load(&session.photometry_path())?;
    let surfaces = if session.surfaces_path().exists() {
        Some(Surfaces::load(&session.surfaces_path())?)
    } else {
        None
    };
    let (width, height, channels) = session.canvas;
    let geometry = TileGeometry {
        width,
        height,
        channels: channels as u32,
        tile: TILE,
    };
    let path = m.result_path(group);
    // A mismatched or unreadable store is dropped (unmapped) before `create`
    // truncates the same file.
    let existing = TileStore::open(&path)
        .ok()
        .filter(|s| s.geometry() == geometry);
    let mut store = match existing {
        Some(s) => s,
        None => TileStore::create(&path, geometry)?,
    };
    let settings = m.config.settings_for(group).1;
    let aligned = blend_tiles(
        &session,
        &phot,
        surfaces.as_ref(),
        &graph,
        &settings,
        rect,
        &mut store,
        ctl,
    )?;
    store.flush()?;
    m.config.push_provenance(
        "blend",
        Some(group),
        json!({ "group": group, "rect": aligned, "settings": settings }),
    );
    m.save()?;
    Ok(aligned)
}

/// Every existing group result store under `<dir>/groups`.
fn all_result_stores(m: &Mosaic) -> Result<Vec<PathBuf>> {
    let root = m.dir.join(GROUPS_DIR);
    let entries = match std::fs::read_dir(&root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::io(&root, e)),
    };
    let mut out = Vec::new();
    for entry in entries {
        let p = entry
            .map_err(|e| Error::io(&root, e))?
            .path()
            .join(RESULT_FILE);
        if p.is_file() {
            out.push(p);
        }
    }
    Ok(out)
}

/// Mark every tile of the store at `path` invalid. A missing or unreadable
/// store is skipped: [`blend_group`] recreates it before use anyway.
fn invalidate_store(path: &Path) -> Result<()> {
    if let Ok(mut store) = TileStore::open(path) {
        store.invalidate_all();
        store.flush()?;
    }
    Ok(())
}

fn invalidate_all_stores(m: &Mosaic) -> Result<()> {
    for p in all_result_stores(m)? {
        invalidate_store(&p)?;
    }
    Ok(())
}

/// Replace the global blend settings. On [`ChangeImpact::Reblend`] every
/// existing group result store is invalidated. Saves the configuration.
pub fn set_blend_settings(m: &mut Mosaic, new: BlendSettings) -> Result<ChangeImpact> {
    let impact = blend_change(&m.config.blend, &new);
    m.config.blend = new;
    if impact == ChangeImpact::Reblend {
        invalidate_all_stores(m)?;
    }
    m.save()?;
    Ok(impact)
}

/// Replace the global analyze settings. On [`ChangeImpact::Reanalyze`]
/// every existing group result store is invalidated and every analyzed
/// group (each with a recorded directory) is marked in
/// [`MosaicConfig::needs_analyze`], so [`blend_group`] refuses it until the
/// next [`analyze_all`] reruns analysis. Saves the configuration.
pub fn set_analyze_settings(m: &mut Mosaic, new: AnalyzeSettings) -> Result<ChangeImpact> {
    let impact = analyze_change(&m.config.analyze, &new);
    m.config.analyze = new;
    if impact == ChangeImpact::Reanalyze {
        invalidate_all_stores(m)?;
        let analyzed: Vec<String> = m.config.group_dirs.keys().cloned().collect();
        m.config.needs_analyze.extend(analyzed);
    }
    m.save()?;
    Ok(impact)
}

/// Set `group`'s setting override (an empty override removes the entry).
/// The impact is that of the change in the group's effective settings; on
/// anything but [`ChangeImpact::None`] only that group's result store is
/// invalidated, and on [`ChangeImpact::Reanalyze`] the group is marked in
/// [`MosaicConfig::needs_analyze`]. Saves the configuration.
pub fn set_group_override(m: &mut Mosaic, group: &str, o: GroupOverride) -> Result<ChangeImpact> {
    let (old_a, old_b) = m.config.settings_for(group);
    if o.is_empty() {
        m.config.group_overrides.remove(group);
    } else {
        m.config.group_overrides.insert(group.to_string(), o);
    }
    let (new_a, new_b) = m.config.settings_for(group);
    let impact = analyze_change(&old_a, &new_a).max(blend_change(&old_b, &new_b));
    if impact != ChangeImpact::None {
        invalidate_store(&m.result_path(group))?;
    }
    if impact == ChangeImpact::Reanalyze {
        m.config.needs_analyze.insert(group.to_string());
    }
    m.save()?;
    Ok(impact)
}
