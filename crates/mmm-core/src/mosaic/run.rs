//! Mosaic orchestration: analyze every processing group onto one shared
//! reference frame, blend a group into its tiled result store, and apply
//! settings edits, staling exactly the result stores they affect.
//!
//! Each job is split in three: a *plan* taken from the [`Mosaic`] on the
//! owner thread ([`plan_analyze`], [`plan_blend`]), a *run* on any thread
//! that needs only the plan ([`run_analyze`], [`run_blend`]), and an *apply*
//! back on the owner thread that records the outcome in the configuration
//! ([`Mosaic::apply_analyze`], [`Mosaic::apply_blend`]). [`analyze_all`]
//! and [`blend_group`] compose the three.
//!
//! On-disk layout of a mosaic directory:
//!
//! ```text
//! <dir>/mosaic.json                  configuration (MosaicConfig)
//! <dir>/reference.mmm-frame.json     shared reference frame
//! <dir>/groups/<group dir>/          one analyze session per group
//! <dir>/groups/<group dir>/result.mmmt   the group's tiled result
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::json;

use super::grouping::{Groups, allocate_group_dir, header_value, resolve_groups};
use super::{
    FRAME_FILE, Fingerprint, GROUPS_DIR, GroupRule, MosaicConfig, ProvenanceEntry, RESULT_FILE,
    fingerprint,
};
use crate::analyze::{InputSelect, analyze_ctl};
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

    /// Record a finished (or partly finished) [`run_analyze`]: remove the
    /// completed groups from [`MosaicConfig::needs_analyze`], record the
    /// fingerprints of their panels, append the outcome's provenance, and
    /// save. Groups that did not complete stay in `needs_analyze`, so
    /// [`plan_blend`] keeps refusing them until the next analysis.
    ///
    /// A completed group whose effective analyze settings changed since it
    /// was planned (a `set_*` landed during the run) also stays marked: it
    /// was analysed with superseded settings.
    pub fn apply_analyze(&mut self, o: &AnalyzeOutcome) -> Result<()> {
        let mut next = self.config.clone();
        for group in &o.completed {
            let current = next.settings_for(group).0;
            if o.analyzed.get(group) == Some(&current) {
                next.needs_analyze.remove(group);
            }
        }
        for (path, fp) in &o.fingerprints {
            for p in next.panels.iter_mut().filter(|p| &p.path == path) {
                p.fingerprint = Some(fp.clone());
            }
        }
        next.provenance.extend(o.provenance.iter().cloned());
        next.save(&self.dir)?;
        self.config = next;
        Ok(())
    }

    /// Record a finished [`run_blend`] of `plan` that blended `rect` (the
    /// aligned rect it returned): append a `"blend"` provenance entry
    /// (group, rect, settings) and save.
    ///
    /// If the group's effective blend settings no longer equal the plan's,
    /// or the group was marked [`MosaicConfig::needs_analyze`] since (a
    /// `set_*` landed between [`plan_blend`] and the run taking the store's
    /// writer lock), the tiles just written are stale: the group's store is
    /// invalidated, nothing is recorded, and an [`Error::Compute`] saying
    /// the settings changed during the blend is returned ([`Error::Busy`]
    /// if another writer holds the store).
    pub fn apply_blend(&mut self, plan: &BlendPlan, rect: [u64; 4]) -> Result<()> {
        let group = &plan.group;
        if self.config.settings_for(group).1 != plan.settings
            || self.config.needs_analyze.contains(group)
        {
            let mut stores = lock_stores(std::slice::from_ref(&plan.result_path))?;
            invalidate(&mut stores)?;
            return Err(Error::compute(format!(
                "settings for group '{group}' changed during the blend — its tiles were \
                 invalidated; blend again"
            )));
        }
        let mut next = self.config.clone();
        next.push_provenance(
            "blend",
            Some(&plan.group),
            json!({ "group": plan.group, "rect": rect, "settings": plan.settings }),
        );
        next.save(&self.dir)?;
        self.config = next;
        Ok(())
    }
}

/// One processing group of an [`AnalyzePlan`].
#[derive(Clone, Debug, PartialEq)]
pub struct AnalyzeGroup {
    /// The group name.
    pub name: String,
    /// The group's session directory (see [`Mosaic::group_session_dir`]).
    pub session_dir: PathBuf,
    /// The group's tiled result store (see [`Mosaic::result_path`]).
    pub result_path: PathBuf,
    /// The group's effective analyze settings.
    pub settings: AnalyzeSettings,
    /// The group's panels.
    pub paths: Vec<PathBuf>,
}

/// Everything a background analysis needs, detached from the [`Mosaic`]
/// (made by [`plan_analyze`], run by [`run_analyze`]).
#[derive(Clone, Debug, PartialEq)]
pub struct AnalyzePlan {
    /// The mosaic directory.
    pub dir: PathBuf,
    /// Where the shared reference frame is saved ([`FRAME_FILE`]).
    pub frame_path: PathBuf,
    /// Every panel, in configuration order; the reference frame is derived
    /// over all of them.
    pub panels: Vec<PathBuf>,
    /// Input interpretation for the reference frame (the base
    /// [`AnalyzeSettings::input`]).
    pub input: InputSelect,
    /// The groups as resolved when planning.
    pub resolved: Groups,
    /// The processing groups to analyze, in order.
    pub groups: Vec<AnalyzeGroup>,
}

/// What a [`run_analyze`] achieved, for [`Mosaic::apply_analyze`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AnalyzeOutcome {
    /// Groups whose analysis finished, in order.
    pub completed: Vec<String>,
    /// The analyze settings each completed group was analysed with (checked
    /// by [`Mosaic::apply_analyze`] against the settings current then).
    pub analyzed: BTreeMap<String, AnalyzeSettings>,
    /// Fingerprints of the completed groups' panels, taken after analysis.
    pub fingerprints: Vec<(PathBuf, Fingerprint)>,
    /// Provenance to append: one `"analyze"` entry recording each group's
    /// resolved settings when every group finished; empty otherwise.
    pub provenance: Vec<ProvenanceEntry>,
}

/// Plan an analysis of every processing group (owner thread).
///
/// Resolves the groups and records directories for new ones
/// ([`Mosaic::assign_group_dirs`]). Every processing group is marked in
/// [`MosaicConfig::needs_analyze`] and its result store, if any, is
/// invalidated — its tiles were blended from the analysis being replaced —
/// so an interrupted run leaves the groups it did not finish refused by
/// [`plan_blend`]. Then the configuration is saved.
///
/// If any group's store is held by a writer ([`Error::Busy`]), nothing is
/// changed (in memory or on disk) and the busy error is returned.
pub fn plan_analyze(m: &mut Mosaic) -> Result<AnalyzePlan> {
    let resolved = m.groups()?;
    plan_analyze_with(m, resolved)
}

/// [`plan_analyze`] with the groups already resolved (by
/// [`Mosaic::groups`], possibly on another thread over a copy of the
/// mosaic): no panel is read, so this is bookkeeping only — suitable for an
/// owner thread that must not do file reads proportional to the panel
/// count. `resolved` must come from the current panels and grouping.
pub fn plan_analyze_with(m: &mut Mosaic, resolved: Groups) -> Result<AnalyzePlan> {
    let mut next = m.clone();
    next.assign_group_dirs(&resolved);
    let groups: Vec<AnalyzeGroup> = resolved
        .processing_groups()
        .into_iter()
        .map(|(name, paths)| AnalyzeGroup {
            session_dir: next.group_session_dir(&name),
            result_path: next.result_path(&name),
            settings: next.config.settings_for(&name).0,
            paths,
            name,
        })
        .collect();
    let paths: Vec<PathBuf> = groups.iter().map(|g| g.result_path.clone()).collect();
    let mut stores = lock_stores(&paths)?;
    next.config
        .needs_analyze
        .extend(groups.iter().map(|g| g.name.clone()));
    invalidate(&mut stores)?;
    next.save()?;
    *m = next;
    Ok(AnalyzePlan {
        dir: m.dir.clone(),
        frame_path: m.dir.join(FRAME_FILE),
        panels: m.config.panels.iter().map(|p| p.path.clone()).collect(),
        input: m.config.analyze.input,
        resolved,
        groups,
    })
}

/// Run a planned analysis (any thread; no [`Mosaic`] needed).
///
/// Derives the reference frame over all panels and saves it, then analyzes
/// each group into its session directory with its effective settings.
/// Progress stages are prefixed with the group name (`"Ha: analyze"`).
///
/// Returns the outcome alongside the result: on cancellation or error the
/// outcome lists the groups that finished before it (and no provenance), so
/// [`Mosaic::apply_analyze`] keeps them.
pub fn run_analyze(plan: &AnalyzePlan, ctl: &JobCtl) -> (AnalyzeOutcome, Result<()>) {
    let mut out = AnalyzeOutcome::default();
    let res = run_analyze_into(plan, ctl, &mut out);
    (out, res)
}

fn run_analyze_into(plan: &AnalyzePlan, ctl: &JobCtl, out: &mut AnalyzeOutcome) -> Result<()> {
    ctl.check()?;
    let frame = reference::derive(&plan.panels, plan.input)?;
    frame.save(&plan.frame_path)?;
    let mut detail = serde_json::Map::new();
    for g in &plan.groups {
        ctl.check()?;
        let prefixed = |stage: &str, done: u64, total: u64| {
            // Cancellation is observed through the shared token below.
            let _ = ctl.report(&format!("{}: {stage}", g.name), done, total);
        };
        let group_ctl = JobCtl::new(ctl.cancel_token().clone(), Some(&prefixed));
        analyze_ctl(
            &g.paths,
            &g.session_dir,
            &g.settings,
            Some(&frame),
            &group_ctl,
        )?;
        let fps = g
            .paths
            .iter()
            .map(|p| Ok((p.clone(), fingerprint(p)?)))
            .collect::<Result<Vec<_>>>()?;
        out.fingerprints.extend(fps);
        out.completed.push(g.name.clone());
        out.analyzed.insert(g.name.clone(), g.settings.clone());
        detail.insert(g.name.clone(), json!(g.settings));
    }
    out.provenance.push(ProvenanceEntry::new(
        "analyze",
        None,
        json!({ "groups": detail }),
    ));
    Ok(())
}

/// Analyze every processing group onto one shared reference frame:
/// [`plan_analyze`], [`run_analyze`] and [`Mosaic::apply_analyze`] in turn,
/// on the calling thread. Returns the resolved groups.
///
/// A cancelled or failed run keeps the groups it finished, leaves every
/// other group refused by [`blend_group`] until the next `analyze_all`, and
/// returns the error without adding provenance.
pub fn analyze_all(m: &mut Mosaic, ctl: &JobCtl) -> Result<Groups> {
    ctl.check()?;
    let plan = plan_analyze(m)?;
    let (outcome, res) = run_analyze(&plan, ctl);
    let applied = m.apply_analyze(&outcome);
    res?;
    applied?;
    Ok(plan.resolved)
}

/// Everything a background blend of one group needs, detached from the
/// [`Mosaic`] (made by [`plan_blend`], run by [`run_blend`]).
#[derive(Clone, Debug, PartialEq)]
pub struct BlendPlan {
    /// The group name.
    pub group: String,
    /// The group's session directory.
    pub session_dir: PathBuf,
    /// The group's tiled result store.
    pub result_path: PathBuf,
    /// The group's effective blend settings.
    pub settings: BlendSettings,
    /// Level-0 canvas rect to blend; `None` = the whole canvas.
    pub rect: Option<[u64; 4]>,
}

/// Plan a blend of `rect` (level-0 canvas coordinates; `None` = the whole
/// canvas) of `group` (owner thread).
///
/// Refuses (with [`Error::Compute`]) a group that has never been analyzed
/// (no recorded directory) or whose analysis is out of date
/// ([`MosaicConfig::needs_analyze`]): run [`analyze_all`] first.
pub fn plan_blend(m: &Mosaic, group: &str, rect: Option<[u64; 4]>) -> Result<BlendPlan> {
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
    Ok(BlendPlan {
        group: group.to_string(),
        session_dir: m.group_session_dir(group),
        result_path: m.result_path(group),
        settings: m.config.settings_for(group).1,
        rect,
    })
}

/// Run a planned blend (any thread; no [`Mosaic`] needed); returns the
/// tile-aligned rect blended (see [`blend_tiles`]). Once blending has
/// started, the store is flushed before returning on every path (success,
/// error or cancellation).
///
/// Loads the group's session, overlap graph, photometry and (when present)
/// residual surfaces, as `mmm blend` does. An existing result store is
/// opened under its writer lock first, before anything is loaded; it is
/// (re)created when missing, unreadable, or of a different geometry. A
/// store held by another writer fails with [`Error::Busy`]. While no store
/// is locked (before a missing one is created, or while a mismatched one is
/// recreated) a `set_*` can still land; [`Mosaic::apply_blend`] catches it. Callers must not let the
/// recreation happen while readers ([`crate::tiles::TileReader`]) still have
/// the store mapped: recreating truncates the file under their mapping. An
/// existing store of matching geometry is never recreated.
pub fn run_blend(plan: &BlendPlan, ctl: &JobCtl) -> Result<[u64; 4]> {
    let path = &plan.result_path;
    // Take an existing store's writer lock before anything else, so a
    // `set_*` from here on is refused busy rather than racing the blend.
    let existing = match TileStore::open(path) {
        Ok(s) => Some(s),
        Err(e) if e.is_busy() => return Err(e),
        Err(_) => None,
    };
    let session = Session::open(&plan.session_dir)?;
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
    // A mismatched store is dropped (unmapped, unlocked) before `create`
    // truncates the same file.
    let existing = existing.filter(|s| s.geometry() == geometry);
    let mut store = match existing {
        Some(s) => s,
        None => TileStore::create(path, geometry)?,
    };
    let blended = blend_tiles(
        &session,
        &phot,
        surfaces.as_ref(),
        &graph,
        &plan.settings,
        plan.rect,
        &mut store,
        ctl,
    );
    // Flush here on the worker whatever happened (success, error, cancel),
    // so the tiles written so far never wait for write-back on the owner
    // thread (whose invalidation flushes only the state table).
    let flushed = store.flush();
    let aligned = blended?;
    flushed?;
    Ok(aligned)
}

/// Blend `rect` (level-0 canvas coordinates; `None` = the whole canvas) of
/// `group` into its tiled result store: [`plan_blend`], [`run_blend`] and
/// [`Mosaic::apply_blend`] in turn, on the calling thread. Returns the
/// tile-aligned rect blended.
pub fn blend_group(
    m: &mut Mosaic,
    group: &str,
    rect: Option<[u64; 4]>,
    ctl: &JobCtl,
) -> Result<[u64; 4]> {
    let plan = plan_blend(m, group, rect)?;
    let aligned = run_blend(&plan, ctl)?;
    m.apply_blend(&plan, aligned)?;
    Ok(aligned)
}

/// Mark every tile of `group`'s result store invalid (owner thread), e.g.
/// when the app discards an unapplied blend whose settings were superseded
/// meanwhile (see the module's concurrency contract). A missing or
/// unreadable store is left alone; the configuration is not changed.
///
/// Fails with [`Error::Busy`], invalidating nothing, while another writer
/// holds the store.
pub fn invalidate_group(m: &Mosaic, group: &str) -> Result<()> {
    let mut stores = lock_stores(&[m.result_path(group)])?;
    invalidate(&mut stores)
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

/// Open (taking the writer lock) every store in `paths`, for invalidation.
/// A missing or unparseable store is skipped: [`run_blend`] recreates it
/// before use anyway. A store held by another writer fails the whole call
/// with [`Error::Busy`] (as does any other I/O error), with every lock taken
/// so far released.
fn lock_stores(paths: &[PathBuf]) -> Result<Vec<TileStore>> {
    let mut out = Vec::new();
    for p in paths {
        match TileStore::open(p) {
            Ok(s) => out.push(s),
            Err(Error::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(Error::Format { .. }) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

/// Mark every tile of every store invalid and flush the state tables only
/// (never a whole-mapping flush: this runs on the owner — UI — thread, and
/// writers flush their own tile data, see [`run_blend`]).
fn invalidate(stores: &mut [TileStore]) -> Result<()> {
    for s in stores {
        s.invalidate_all();
        s.flush_state()?;
    }
    Ok(())
}

/// Every blend setting resolved from `base` (the base and each group's
/// override) passes [`BlendSettings::validate`].
fn validate_blend(cfg: &MosaicConfig, base: &BlendSettings) -> Result<()> {
    base.validate()?;
    for o in cfg.group_overrides.values() {
        base.with(&o.blend).validate()?;
    }
    Ok(())
}

/// Replace the global analyze and blend settings together: one validation,
/// one invalidation pass and one save. Returns the larger of the two
/// impacts.
///
/// On any impact every existing group result store is invalidated; on
/// [`ChangeImpact::Reanalyze`] every analyzed group (each with a recorded
/// directory) is also marked in [`MosaicConfig::needs_analyze`], so
/// [`blend_group`] refuses it until the next [`analyze_all`].
///
/// Refused with nothing changed (in memory or on disk) when any resolved
/// blend settings fail [`BlendSettings::validate`], or — with
/// [`Error::Busy`] — when any affected store is held by a writer.
pub fn set_settings(
    m: &mut Mosaic,
    analyze: AnalyzeSettings,
    blend: BlendSettings,
) -> Result<ChangeImpact> {
    validate_blend(&m.config, &blend)?;
    let ia = analyze_change(&m.config.analyze, &analyze);
    let impact = ia.max(blend_change(&m.config.blend, &blend));
    let mut stores = match impact {
        ChangeImpact::None => Vec::new(),
        _ => lock_stores(&all_result_stores(m)?)?,
    };
    let mut next = m.config.clone();
    next.analyze = analyze;
    next.blend = blend;
    if ia == ChangeImpact::Reanalyze {
        next.needs_analyze.extend(next.group_dirs.keys().cloned());
    }
    invalidate(&mut stores)?;
    next.save(&m.dir)?;
    m.config = next;
    Ok(impact)
}

/// Replace the global blend settings ([`set_settings`] with the analyze
/// settings unchanged). On [`ChangeImpact::Reblend`] every existing group
/// result store is invalidated. Saves the configuration.
///
/// Refused with nothing changed (in memory or on disk) when settings fail
/// [`BlendSettings::validate`] or a store is busy ([`Error::Busy`]).
pub fn set_blend_settings(m: &mut Mosaic, new: BlendSettings) -> Result<ChangeImpact> {
    let analyze = m.config.analyze.clone();
    set_settings(m, analyze, new)
}

/// Replace the global analyze settings ([`set_settings`] with the blend
/// settings unchanged). On [`ChangeImpact::Reanalyze`] every existing group
/// result store is invalidated and every analyzed group is marked in
/// [`MosaicConfig::needs_analyze`]. Saves the configuration.
///
/// Refused with nothing changed when a store is busy ([`Error::Busy`]).
pub fn set_analyze_settings(m: &mut Mosaic, new: AnalyzeSettings) -> Result<ChangeImpact> {
    let blend = m.config.blend.clone();
    set_settings(m, new, blend)
}

/// Set `group`'s setting override (an empty override removes the entry).
/// The impact is that of the change in the group's effective settings; on
/// anything but [`ChangeImpact::None`] only that group's result store is
/// invalidated, and on [`ChangeImpact::Reanalyze`] the group is marked in
/// [`MosaicConfig::needs_analyze`]. Saves the configuration.
///
/// Refused with nothing changed (in memory or on disk) when the resolved
/// blend settings fail [`BlendSettings::validate`] or the group's store is
/// busy ([`Error::Busy`]).
pub fn set_group_override(m: &mut Mosaic, group: &str, o: GroupOverride) -> Result<ChangeImpact> {
    m.config.blend.with(&o.blend).validate()?;
    let mut next = m.config.clone();
    if o.is_empty() {
        next.group_overrides.remove(group);
    } else {
        next.group_overrides.insert(group.to_string(), o);
    }
    let (old_a, old_b) = m.config.settings_for(group);
    let (new_a, new_b) = next.settings_for(group);
    let impact = analyze_change(&old_a, &new_a).max(blend_change(&old_b, &new_b));
    let mut stores = match impact {
        ChangeImpact::None => Vec::new(),
        _ => lock_stores(&[m.result_path(group)])?,
    };
    if impact == ChangeImpact::Reanalyze {
        next.needs_analyze.insert(group.to_string());
    }
    invalidate(&mut stores)?;
    next.save(&m.dir)?;
    m.config = next;
    Ok(impact)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalidate_flushes_only_the_state_table() {
        let d = std::env::temp_dir().join(format!("mmm-run-invalidate-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let g = TileGeometry {
            width: 600,
            height: 420,
            channels: 3,
            tile: TILE,
        };
        let mut store = TileStore::create(&d.join(RESULT_FILE), g).unwrap();
        let n = (g.channels * g.tile * g.tile) as usize;
        store.write_tile(0, 0, 0, &vec![1.0; n]).unwrap();
        let mut stores = vec![store];
        invalidate(&mut stores).unwrap();
        assert_eq!(stores[0].valid_count(0), 0);
        // No whole-mapping msync: only the state table is written back.
        assert_eq!(stores[0].full_flushes(), 0);
        drop(stores);
        let _ = std::fs::remove_dir_all(&d);
    }
}
