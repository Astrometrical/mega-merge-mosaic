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

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::json;

use super::grouping::{Groups, allocate_group_dir, header_value, resolve_groups};
use super::{
    FRAME_FILE, Fingerprint, GROUPS_DIR, GroupRule, Grouping, MosaicConfig, PanelEntry,
    ProvenanceEntry, RESULT_FILE, fingerprint,
};
use crate::analyze::{InputSelect, analyze_ctl};
use crate::error::{Error, Result};
use crate::formats::InputPanel;
use crate::job::JobCtl;
use crate::overlap::OverlapGraph;
use crate::photometry::Photometry;
use crate::reference::{self, ReferenceFrame};
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
            GroupRule::FilenameToken { .. } | GroupRule::PathKeyword { .. } | GroupRule::Manual => {
                None
            }
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
    /// The provenance records `plan.settings` (the settings the run used),
    /// whatever the committed settings are now: a later edit leaves the
    /// result out of date ([`settings_changed`]) but not wrong.
    ///
    /// If the group was marked [`MosaicConfig::needs_analyze`] since (a
    /// `set_*` landed between [`plan_blend`] and the run taking the store's
    /// writer lock), the tiles just written are stale: the group's store is
    /// invalidated, nothing is recorded, and an [`Error::Compute`] is
    /// returned ([`Error::Busy`] if another writer holds the store).
    pub fn apply_blend(&mut self, plan: &BlendPlan, rect: [u64; 4]) -> Result<()> {
        let group = &plan.group;
        if self.config.needs_analyze.contains(group) {
            let mut stores = lock_stores(std::slice::from_ref(&plan.result_path))?;
            invalidate(&mut stores)?;
            return Err(Error::compute(format!(
                "analyze settings for group '{group}' changed during the merge — its tiles were invalidated; analyze, then merge again"
            )));
        }
        let mut next = self.config.clone();
        next.push_provenance(
            "blend",
            Some(&plan.group),
            json!({
                "group": plan.group,
                "rect": rect,
                "settings": plan.settings,
                "whole": plan.rect.is_none(),
            }),
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
    /// The processing groups to analyze, in order. Empty for a
    /// [`AnalyzeScope::Needed`] plan when nothing needs analysing; running
    /// such a plan does nothing (no frame is derived, no provenance added).
    pub groups: Vec<AnalyzeGroup>,
    /// The saved analysed reference frame, set by a [`AnalyzeScope::Needed`]
    /// plan when a frame file exists: [`run_analyze`] refuses with
    /// [`Error::FrameMoved`] if the frame derived from the panels now
    /// differs, because the groups left out were analysed on the old one.
    pub expected_frame: Option<ReferenceFrame>,
}

/// Which groups a [`plan_analyze_scoped`] plan analyses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnalyzeScope {
    /// Every processing group.
    All,
    /// Only groups in [`MosaicConfig::needs_analyze`], plus resolved groups
    /// never analysed (no recorded directory).
    Needed,
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
/// [`plan_blend`]. Marks of groups that no longer resolve are dropped
/// (their `group_dirs` entries are kept). Then the configuration is saved.
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
    plan_analyze_scoped(m, resolved, AnalyzeScope::All)
}

/// [`plan_analyze_with`] limited by `scope`. A [`AnalyzeScope::Needed`] plan
/// builds, locks, invalidates and marks only its groups (marks of vanished
/// groups are still dropped), and records the saved analysed frame in
/// [`AnalyzePlan::expected_frame`] (a missing or unreadable frame file gives
/// `None`). A plan with no groups is valid and does nothing when run.
pub fn plan_analyze_scoped(
    m: &mut Mosaic,
    resolved: Groups,
    scope: AnalyzeScope,
) -> Result<AnalyzePlan> {
    let mut next = m.clone();
    let never_analyzed: BTreeSet<String> = resolved
        .processing_groups()
        .into_iter()
        .map(|(n, _)| n)
        .filter(|n| !m.config.group_dirs.contains_key(n))
        .collect();
    next.assign_group_dirs(&resolved);
    let groups: Vec<AnalyzeGroup> = resolved
        .processing_groups()
        .into_iter()
        .filter(|(name, _)| {
            scope == AnalyzeScope::All
                || next.config.needs_analyze.contains(name)
                || never_analyzed.contains(name)
        })
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
    // Groups that no longer resolve (a rule or manual-group change) can
    // never be analysed: drop their marks. Their directories stay in
    // `group_dirs`, so caches stay stable if the name comes back.
    let resolved_names: BTreeSet<String> = resolved
        .processing_groups()
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    next.config
        .needs_analyze
        .retain(|name| resolved_names.contains(name));
    next.config
        .needs_analyze
        .extend(groups.iter().map(|g| g.name.clone()));
    invalidate(&mut stores)?;
    next.save()?;
    *m = next;
    let frame_path = m.dir.join(FRAME_FILE);
    let expected_frame = match scope {
        AnalyzeScope::All => None,
        AnalyzeScope::Needed => ReferenceFrame::load(&frame_path).ok(),
    };
    Ok(AnalyzePlan {
        dir: m.dir.clone(),
        frame_path,
        panels: m.config.panels.iter().map(|p| p.path.clone()).collect(),
        input: m.config.analyze.input,
        resolved,
        groups,
        expected_frame,
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
    if plan.groups.is_empty() {
        return Ok(());
    }
    let frame = reference::derive(&plan.panels, plan.input)?;
    if plan.expected_frame.as_ref().is_some_and(|e| *e != frame) {
        return Err(Error::FrameMoved);
    }
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
    /// The blend settings the run uses (recorded in the provenance).
    pub settings: BlendSettings,
    /// Level-0 canvas rect to blend; `None` = the whole canvas.
    pub rect: Option<[u64; 4]>,
    /// A whole-canvas merge whose settings differ from the group's last
    /// whole-canvas merge: [`run_blend`] invalidates the whole store before
    /// writing, so a stopped run leaves nothing that looks current. A
    /// region merge is never fresh (the sink invalidates just its rect;
    /// one with other settings makes the group out of date instead, see
    /// [`region_differs`]).
    ///
    /// The tile sink ([`crate::tiles::TileSink::new`]) also invalidates
    /// the whole aligned rect of any whole-canvas merge, fresh or not, so
    /// a finished whole merge always rewrites every tile. `fresh` is kept
    /// for durability: it invalidates and flushes the state table before
    /// [`crate::tiles::blend_tiles`] starts, whereas the sink's
    /// invalidation sits only in the mapping until the run ends, through
    /// the (possibly long) blend that reads every panel. A crash or cancel
    /// before the sink exists (or before its invalidation reaches the disk)
    /// still leaves the group out of date. Neither covers the other: keep
    /// both.
    pub fresh: bool,
}

/// Plan a blend of `rect` (level-0 canvas coordinates; `None` = the whole
/// canvas) of `group` (owner thread).
///
/// Refuses (with [`Error::Compute`]) a group that has never been analyzed
/// (no recorded directory) or whose analysis is out of date
/// ([`MosaicConfig::needs_analyze`]): run [`analyze_all`] first.
pub fn plan_blend(m: &Mosaic, group: &str, rect: Option<[u64; 4]>) -> Result<BlendPlan> {
    plan_blend_with(m, group, rect, m.config.settings_for(group).1)
}

/// [`plan_blend`] with explicit `settings` (those a queued merge was
/// queued with) instead of the group's committed ones. The plan is
/// [`BlendPlan::fresh`] when it is a whole-canvas merge and `settings`
/// differ from [`last_merge_settings`].
pub fn plan_blend_with(
    m: &Mosaic,
    group: &str,
    rect: Option<[u64; 4]>,
    settings: BlendSettings,
) -> Result<BlendPlan> {
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
        fresh: rect.is_none() && last_merge_settings(&m.config, group).as_ref() != Some(&settings),
        settings,
        rect,
    })
}

/// Run a planned blend (any thread; no [`Mosaic`] needed); returns the
/// tile-aligned rect blended (see [`blend_tiles`]). Once blending has
/// started, the store is flushed before returning: wholly on success, its
/// state table only on an error or cancellation (so a cancel stays quick).
///
/// Loads the group's session, overlap graph, photometry and (when present)
/// residual surfaces, as `mmm blend` does. An existing result store is
/// opened under its writer lock first, before anything is loaded; it is
/// (re)created when missing, unparseable ([`Error::Format`]), or of a
/// different geometry. A store held by another writer fails with
/// [`Error::Busy`]; any other error opening it is returned as is (the file
/// is never truncated for it). While no store is locked (before a missing
/// one is created, or while a mismatched one is recreated) a `set_*` can
/// still land; [`Mosaic::apply_blend`] catches it. Callers must not let the
/// recreation happen while readers ([`crate::tiles::TileReader`]) still
/// have the store mapped: recreating truncates the file under their
/// mapping. An existing store of matching geometry is never recreated.
pub fn run_blend(plan: &BlendPlan, ctl: &JobCtl) -> Result<[u64; 4]> {
    let path = &plan.result_path;
    // Take an existing store's writer lock before anything else, so a
    // `set_*` from here on is refused busy rather than racing the blend.
    let existing = open_existing(path)?;
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
    // Durability, not redundancy: `TileSink::new` (inside `blend_tiles`)
    // also invalidates the whole aligned rect of any whole-canvas merge,
    // but only in the mapping, unflushed until the run ends, and
    // `blend_tiles` can fail before the sink exists. Invalidating and
    // flushing here, before the (possibly long) blend reads the panels,
    // means a crash or cancel at any point still leaves the group out of
    // date rather than its old-settings tiles looking current. Keep both.
    if plan.fresh {
        store.invalidate_all();
        store.flush_state()?;
    }
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
    finish_store(&store, blended)
}

/// Open the store at `path` under its writer lock: `None` if it is
/// missing or not a readable store (wrong header or size —
/// [`Error::Format`]), which callers may (re)create. Every other error —
/// busy ([`Error::Busy`]), permissions, I/O — is returned: the file may
/// hold a valid result that must not be truncated.
fn open_existing(path: &Path) -> Result<Option<TileStore>> {
    match TileStore::open(path) {
        Ok(s) => Ok(Some(s)),
        Err(Error::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(Error::Format { .. }) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Flush `store` after [`blend_tiles`] returned `blended`, then return it.
///
/// On success the whole mapping is flushed (tile data included), here on
/// the worker, so it never waits for write-back on the owner thread. On
/// an error or cancellation only the state table is flushed: writing back
/// a big blend's tile data could take long, and a cancel must end within
/// about a second (spec §8); the kernel writes the data back on its own.
fn finish_store(store: &TileStore, blended: Result<[u64; 4]>) -> Result<[u64; 4]> {
    match blended {
        Ok(aligned) => {
            store.flush()?;
            Ok(aligned)
        }
        Err(e) => {
            if let Err(f) = store.flush_state() {
                tracing::warn!("result state not flushed after a failed blend: {f}");
            }
            Err(e)
        }
    }
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

/// Mark `groups` in [`MosaicConfig::needs_analyze`] (owner thread), e.g.
/// when the panels' frame no longer matches the analysed frame (files
/// changed on disk). Each newly marked group's result store is invalidated
/// (its tiles were blended from the analysis being replaced) and the
/// configuration is saved once. Groups already marked are left alone; when
/// all are, nothing is done.
///
/// Refused with nothing changed (in memory or on disk) with
/// [`Error::Busy`] when a store to invalidate is held by a writer.
pub fn mark_needs_analyze(m: &mut Mosaic, groups: &[String]) -> Result<()> {
    let new: Vec<&String> = groups
        .iter()
        .filter(|g| !m.config.needs_analyze.contains(*g))
        .collect();
    if new.is_empty() {
        return Ok(());
    }
    let paths: Vec<PathBuf> = new.iter().map(|g| m.result_path(g)).collect();
    let mut stores = lock_stores(&paths)?;
    let mut next = m.config.clone();
    next.needs_analyze.extend(new.into_iter().cloned());
    invalidate(&mut stores)?;
    next.save(&m.dir)?;
    m.config = next;
    Ok(())
}

/// Every existing group result store under `<dir>/groups`.
fn all_result_stores(m: &Mosaic) -> Result<Vec<PathBuf>> {
    result_store_paths(&m.dir)
}

/// Every existing group result store under `<dir>/groups` of the mosaic
/// directory `dir`: exactly the stores [`set_settings`] and [`set_inputs`]
/// invalidate. Reads the directory (blocking).
pub fn result_store_paths(dir: &Path) -> Result<Vec<PathBuf>> {
    let root = dir.join(GROUPS_DIR);
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
/// A missing or unparseable store is skipped ([`open_existing`]):
/// [`run_blend`] recreates it before use anyway. A store held by another writer fails the whole call
/// with [`Error::Busy`] (as does any other I/O error), with every lock taken
/// so far released.
fn lock_stores(paths: &[PathBuf]) -> Result<Vec<TileStore>> {
    let mut out = Vec::new();
    for p in paths {
        out.extend(open_existing(p)?);
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

/// Commit new global blend settings without invalidating anything: the
/// settings are validated like [`set_settings`] (the base and every group's
/// effective settings), stored and saved once. No store lock is taken, no
/// tiles are invalidated and [`MosaicConfig::needs_analyze`] is untouched;
/// the next merge uses the new settings, and [`settings_changed`] tells a
/// frontend which groups' merged tiles no longer match.
///
/// Refused with nothing changed (in memory or on disk) when any resolved
/// settings fail [`BlendSettings::validate`].
pub fn commit_blend_settings(m: &mut Mosaic, blend: BlendSettings) -> Result<()> {
    validate_blend(&m.config, &blend)?;
    let mut next = m.config.clone();
    next.blend = blend;
    next.save(&m.dir)?;
    m.config = next;
    Ok(())
}

/// Whether `e` is a `"blend"` entry of `group`.
fn is_merge_of(e: &ProvenanceEntry, group: &str) -> bool {
    e.action == "blend" && e.group.as_deref() == Some(group)
}

/// Whether a `"blend"` entry was a whole-canvas merge: `whole` is not
/// false (entries without the key predate it and count as whole).
fn is_whole(e: &ProvenanceEntry) -> bool {
    e.detail.get("whole").and_then(|w| w.as_bool()) != Some(false)
}

/// The settings a `"blend"` entry records (`None` if they do not parse).
fn merge_settings(e: &ProvenanceEntry) -> Option<BlendSettings> {
    serde_json::from_value(e.detail.get("settings")?.clone()).ok()
}

/// Where `group`'s newest whole-canvas merge is in the provenance trail.
fn last_whole_merge(cfg: &MosaicConfig, group: &str) -> Option<usize> {
    cfg.provenance
        .iter()
        .rposition(|e| is_merge_of(e, group) && is_whole(e))
}

/// The blend settings of `group`'s newest whole-canvas merge, from the
/// provenance trail (`None` if never merged, or if that entry's settings do
/// not parse: an older entry never answers for it). Region merges (`whole` false)
/// are skipped; entries without a `whole` key predate it and count as whole.
pub fn last_merge_settings(cfg: &MosaicConfig, group: &str) -> Option<BlendSettings> {
    last_whole_merge(cfg, group).and_then(|i| merge_settings(&cfg.provenance[i]))
}

/// Whether a region of `group` was merged, since its newest whole-canvas
/// merge, with settings different from that merge's
/// ([`last_merge_settings`]): some `"blend"` entry of the group with
/// `whole` false, later in the provenance trail than the newest whole
/// entry, records other settings. Its tiles then sit in the group's store
/// beside the whole merge's, so the store is complete but not of one
/// merge: a frontend treats the group as out of date (a whole merge
/// rewrites it) rather than as merely having changed settings. A later
/// region merge with the whole merge's settings does not clear it (the
/// other tiles may remain outside it); only a whole merge does. `false`
/// when no whole merge is recorded (the group is not merged at all).
pub fn region_differs(cfg: &MosaicConfig, group: &str) -> bool {
    let Some(whole) = last_whole_merge(cfg, group) else {
        return false;
    };
    let merged = merge_settings(&cfg.provenance[whole]);
    cfg.provenance[whole + 1..]
        .iter()
        .filter(|e| is_merge_of(e, group))
        .any(|e| merge_settings(e) != merged)
}

/// The analyze settings `group` was last analyzed with, from the newest
/// `"analyze"` provenance entry that includes it (`None` if never analyzed).
pub fn last_analyze_settings(cfg: &MosaicConfig, group: &str) -> Option<AnalyzeSettings> {
    cfg.provenance
        .iter()
        .rev()
        .filter(|e| e.action == "analyze")
        .find_map(|e| serde_json::from_value(e.detail.get("groups")?.get(group)?.clone()).ok())
}

/// Why a group's mosaic is out of date, as far as the provenance trail
/// can tell (see [`out_of_date_reason`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutOfDateReason {
    /// The group's analysis settings differ from those it was analysed with.
    AnalysisSettings,
    /// The panels changed.
    Panels,
    /// No whole-canvas merge is recorded.
    NotMerged,
    /// Nothing explains it: a merge that did not finish.
    MergeUnfinished,
    /// A region was merged with other settings since the whole-canvas
    /// merge ([`region_differs`]).
    RegionDiffers,
}

/// Why `group`'s mosaic is out of date, first match of: it needs analysis
/// and its effective analysis settings differ from [`last_analyze_settings`]
/// ([`OutOfDateReason::AnalysisSettings`]); it needs analysis
/// ([`OutOfDateReason::Panels`]); a region was merged with other settings
/// since the whole merge ([`region_differs`]:
/// [`OutOfDateReason::RegionDiffers`]); no whole merge is recorded
/// ([`OutOfDateReason::NotMerged`]); an `"inputs"` entry newer than the
/// group's last whole merge touches it (no `affected` key, or it lists the
/// group: `Panels`); an `"analyze"` entry listing the group, newer than
/// that merge, has different settings for it from the newest one before
/// the merge (`AnalysisSettings`: the pipeline already re-analysed, so
/// `needs_analyze` is clear); else [`OutOfDateReason::MergeUnfinished`].
/// Reads only the config, so it says nothing of whether the group *is* out
/// of date.
pub fn out_of_date_reason(cfg: &MosaicConfig, group: &str) -> OutOfDateReason {
    if cfg.needs_analyze.contains(group) {
        let changed =
            last_analyze_settings(cfg, group).is_some_and(|last| last != cfg.settings_for(group).0);
        return if changed {
            OutOfDateReason::AnalysisSettings
        } else {
            OutOfDateReason::Panels
        };
    }
    if region_differs(cfg, group) {
        return OutOfDateReason::RegionDiffers;
    }
    let Some(merge) = last_whole_merge(cfg, group) else {
        return OutOfDateReason::NotMerged;
    };
    let after = &cfg.provenance[merge + 1..];
    let touches = |e: &ProvenanceEntry| match e.detail.get("affected").and_then(|a| a.as_array()) {
        None => true,
        Some(list) => list.iter().any(|g| g.as_str() == Some(group)),
    };
    if after.iter().any(|e| e.action == "inputs" && touches(e)) {
        return OutOfDateReason::Panels;
    }
    let of = |e: &ProvenanceEntry| -> Option<AnalyzeSettings> {
        serde_json::from_value(e.detail.get("groups")?.get(group)?.clone()).ok()
    };
    let before = cfg.provenance[..merge]
        .iter()
        .rev()
        .filter(|e| e.action == "analyze")
        .find_map(of);
    let changed = after
        .iter()
        .filter(|e| e.action == "analyze")
        .filter_map(of)
        .any(|s| before.as_ref().is_some_and(|b| *b != s));
    if changed {
        OutOfDateReason::AnalysisSettings
    } else {
        OutOfDateReason::MergeUnfinished
    }
}

/// Whether `draft` global blend settings, resolved through `group`'s
/// override, differ from those of the group's last whole-canvas merge.
/// `false` when the group was never merged.
pub fn settings_changed(cfg: &MosaicConfig, group: &str, draft: &BlendSettings) -> bool {
    let over = cfg
        .group_overrides
        .get(group)
        .map(|o| o.blend.clone())
        .unwrap_or_default();
    last_merge_settings(cfg, group).is_some_and(|s| s != draft.with(&over))
}

/// Replace the input panel list and grouping rule. Unchanged inputs are a
/// no-op. Otherwise every existing group result store is invalidated, every
/// group with a recorded directory is marked in
/// [`MosaicConfig::needs_analyze`] (group membership may have changed), and
/// an `"inputs"` provenance entry with the panel and group counts is
/// appended. One save. A [`PanelEntry`]'s `manual_group` is part of the entry
/// and so survives a rule change.
///
/// Refused with nothing changed (in memory or on disk) with [`Error::Busy`]
/// when any store is held by a writer.
pub fn set_inputs(m: &mut Mosaic, panels: Vec<PanelEntry>, grouping: Grouping) -> Result<()> {
    set_inputs_affecting(m, panels, grouping, None)
}

/// [`set_inputs`] limited to `affected` groups (see [`super::grouping::affected_groups`]):
/// only their stores are locked and invalidated and only they (those with a
/// recorded directory) are marked in [`MosaicConfig::needs_analyze`]; other
/// groups stay valid and unmarked. `None` means every group, exactly
/// [`set_inputs`]. The `"inputs"` provenance entry gains an `"affected"`
/// list of names when `affected` is `Some` (absent = all).
pub fn set_inputs_affecting(
    m: &mut Mosaic,
    panels: Vec<PanelEntry>,
    grouping: Grouping,
    affected: Option<&BTreeSet<String>>,
) -> Result<()> {
    if panels == m.config.panels && grouping == m.config.grouping {
        return Ok(());
    }
    let mut stores = match affected {
        None => lock_stores(&all_result_stores(m)?)?,
        Some(a) => {
            let paths: Vec<PathBuf> = a.iter().map(|g| m.result_path(g)).collect();
            lock_stores(&paths)?
        }
    };
    let mut next = m.config.clone();
    match affected {
        None => next.needs_analyze.extend(next.group_dirs.keys().cloned()),
        Some(a) => {
            let marked: Vec<String> = a
                .iter()
                .filter(|g| next.group_dirs.contains_key(*g))
                .cloned()
                .collect();
            next.needs_analyze.extend(marked);
        }
    }
    let mut detail = json!({ "panels": panels.len(), "groups": next.group_dirs.len() });
    if let Some(a) = affected {
        detail["affected"] = json!(a);
    }
    next.push_provenance("inputs", None, detail);
    next.panels = panels;
    next.grouping = grouping;
    invalidate(&mut stores)?;
    next.save(&m.dir)?;
    m.config = next;
    Ok(())
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

    fn geometry() -> TileGeometry {
        TileGeometry {
            width: 600,
            height: 420,
            channels: 3,
            tile: TILE,
        }
    }

    #[test]
    fn failed_blend_flushes_only_the_state_table() {
        let d = std::env::temp_dir().join(format!("mmm-run-finish-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let store = TileStore::create(&d.join(RESULT_FILE), geometry()).unwrap();
        let err = finish_store(&store, Err(Error::Cancelled)).unwrap_err();
        assert!(err.is_cancelled(), "{err}");
        assert_eq!(store.full_flushes(), 0, "whole mapping flushed on cancel");
        // Success keeps the full flush (tile data durable).
        let rect = [0, 0, 600, 420];
        assert_eq!(finish_store(&store, Ok(rect)).unwrap(), rect);
        assert_eq!(store.full_flushes(), 1);
        drop(store);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn only_missing_or_unparseable_store_is_recreated() {
        let d = std::env::temp_dir().join(format!("mmm-run-open-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        // Missing: create it.
        assert!(open_existing(&d.join("missing")).unwrap().is_none());
        // Not a store: recreate it.
        let junk = d.join("junk");
        std::fs::write(&junk, b"not a tile store").unwrap();
        assert!(open_existing(&junk).unwrap().is_none());
        // A valid store opens.
        let good = d.join(RESULT_FILE);
        drop(TileStore::create(&good, geometry()).unwrap());
        assert!(open_existing(&good).unwrap().is_some());
        // Any other error (here: the path is a directory) is propagated,
        // never answered by truncating the file.
        let dir = d.join("adir");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(open_existing(&dir).is_err());
        // A store held by another writer is busy.
        let held = TileStore::open(&good).unwrap();
        assert!(open_existing(&good).err().is_some_and(|e| e.is_busy()));
        drop(held);
        let _ = std::fs::remove_dir_all(&d);
    }

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
