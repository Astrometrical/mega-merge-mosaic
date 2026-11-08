//! The panel inputs and how edits to them apply: picking panel files, the
//! group colours kept with the session, and the edit model behind the
//! panel list ([`crate::panel_list`]).
//!
//! # Edits
//!
//! Every edit (add, remove, move, rule) builds a new panel list and
//! grouping, a [`PanelsEdit`], and [`PanelsEdit::apply_scoped`]s it with
//! [`set_inputs_affecting`], limited to the groups it touches (worked out on
//! a worker, [`PanelEdits::submit`]). While a job runs (it may hold a result store) the edit
//! cancels every job and waits ([`PanelEdits`]); [`PanelEdits::poll`]
//! cancels any job started meanwhile too and applies the edit once the
//! runner is idle, and a newer edit replaces a waiting one (newer work
//! wins). The list shows the waiting edit as if applied
//! ([`PanelEdits::shown`]). `set_inputs` saves `mosaic.json` and
//! invalidates the stores' state tables on the UI thread: the documented
//! exception to "no file I/O on the UI thread".
//!
//! Group colours ([`group_colors`]) are assigned when groups resolve and
//! kept in `MosaicConfig.ui` ([`GROUP_COLORS_KEY`]); they are saved with the
//! next `mosaic.json` save. So is whether the user has chosen how to group
//! ([`GROUPING_CHOSEN_KEY`]).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::Color32;
use mega_ui::groups::physical_color;
pub use mega_ui::groups::{GROUP_PALETTE, PHYSICAL_SLOTS};
use mmm_core::mosaic::grouping::Groups;
use mmm_core::mosaic::{
    Grouping, Mosaic, MosaicConfig, PanelEntry, UNGROUPED, affected_groups, set_inputs_affecting,
};
use mmm_core::reference::{self, ReferenceFrame};
use serde_json::Value;

use crate::jobs::JobRunner;
use crate::session::{OpenSession, normalize};
use crate::util::{Poll, Task};

/// File extensions accepted as panels (the dialog filter, drops and
/// patterns), compared case-insensitively.
pub const PANEL_EXTENSIONS: [&str; 4] = ["xisf", "fits", "fit", "fts"];
/// Wait before retrying an edit refused because a store was busy.
const BUSY_RETRY: Duration = Duration::from_millis(500);

/// Key in `MosaicConfig.ui` of the group colours: group name to an index
/// in [`GROUP_PALETTE`], assigned once (first seen) and then kept.
pub const GROUP_COLORS_KEY: &str = "group_colors";

/// Key in `MosaicConfig.ui` of whether the user has chosen how to group
/// (`true` once they pick in **Group by** or undo the automatic grouping):
/// the panels are then never grouped by FILTER by themselves.
pub const GROUPING_CHOSEN_KEY: &str = "grouping_chosen";

/// Whether `ui` (`MosaicConfig.ui`) records that the user has chosen how to
/// group ([`GROUPING_CHOSEN_KEY`]).
pub fn grouping_chosen(ui: &Value) -> bool {
    ui.get(GROUPING_CHOSEN_KEY).and_then(Value::as_bool) == Some(true)
}

/// Record in `ui` that the user has chosen how to group, keeping everything
/// else there (a `ui` that is not a JSON object becomes one). Whether that
/// changed `ui`.
pub fn set_grouping_chosen(ui: &mut Value) -> bool {
    if grouping_chosen(ui) {
        return false;
    }
    if !ui.is_object() {
        *ui = Value::Object(Default::default());
    }
    if let Some(obj) = ui.as_object_mut() {
        obj.insert(GROUPING_CHOSEN_KEY.to_string(), Value::Bool(true));
    }
    true
}

/// The persisted group colours in `ui` (`MosaicConfig.ui`); empty when
/// absent or unreadable.
pub fn load_group_colors(ui: &Value) -> BTreeMap<String, usize> {
    ui.get(GROUP_COLORS_KEY)
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

/// Store `map` as the group colours in `ui`, keeping everything else there
/// (a `ui` that is not a JSON object becomes one).
pub fn save_group_colors(ui: &mut Value, map: &BTreeMap<String, usize>) {
    if !ui.is_object() {
        *ui = Value::Object(Default::default());
    }
    if let (Some(obj), Ok(v)) = (ui.as_object_mut(), serde_json::to_value(map)) {
        obj.insert(GROUP_COLORS_KEY.to_string(), v);
    }
}

/// Give each of `names` (in first-seen order) without a colour in `map`
/// one: its physical colour if it is a well-known filter name, else the
/// first non-physical [`GROUP_PALETTE`] entry (after [`PHYSICAL_SLOTS`]) no
/// group uses, cycling through those once all are used.
/// Existing entries never change; [`UNGROUPED`] gets none. Whether `map`
/// changed.
pub fn assign_group_colors<'a>(
    map: &mut BTreeMap<String, usize>,
    names: impl IntoIterator<Item = &'a str>,
) -> bool {
    let mut changed = false;
    for name in names {
        if name == UNGROUPED || map.contains_key(name) {
            continue;
        }
        let index = physical_color(name).unwrap_or_else(|| {
            // Physical colours stay free for the filters they name.
            let used: BTreeSet<usize> = map.values().copied().collect();
            let others = GROUP_PALETTE.len() - PHYSICAL_SLOTS;
            let unknown = map.keys().filter(|n| physical_color(n).is_none()).count();
            (PHYSICAL_SLOTS..GROUP_PALETTE.len())
                .find(|i| !used.contains(i))
                .unwrap_or(PHYSICAL_SLOTS + unknown % others)
        });
        map.insert(name.to_string(), index);
        changed = true;
    }
    changed
}

/// The colour of each of `names` (as [`assign_group_colors`] would give
/// them on top of the persisted `map`); [`UNGROUPED`] has none. For every
/// view showing groups (the panel list, the plan, the view bar).
pub fn group_colors<'a>(
    map: &BTreeMap<String, usize>,
    names: impl IntoIterator<Item = &'a str>,
) -> BTreeMap<String, Color32> {
    let names: Vec<&str> = names.into_iter().filter(|n| *n != UNGROUPED).collect();
    let mut map = map.clone();
    assign_group_colors(&mut map, names.iter().copied());
    names
        .into_iter()
        .map(|n| (n.to_string(), GROUP_PALETTE[map[n] % GROUP_PALETTE.len()]))
        .collect()
}

/// Picks panel files with a native dialog. Called on a background thread.
pub trait FilesPicker: Send + Sync {
    /// Show a multi-file dialog for panels; `None` when cancelled or when no
    /// dialog is available.
    fn pick_files(&self) -> Option<Vec<PathBuf>>;

    /// Show a multi-folder dialog; `None` when cancelled or when no dialog
    /// is available.
    fn pick_folders(&self) -> Option<Vec<PathBuf>>;
}

/// The native dialog through `rfd`, filtered to [`PANEL_EXTENSIONS`].
pub struct RfdFilesPicker;

impl FilesPicker for RfdFilesPicker {
    fn pick_files(&self) -> Option<Vec<PathBuf>> {
        rfd::FileDialog::new()
            .set_title("Add panel files")
            .add_filter("Panels (XISF, FITS)", &PANEL_EXTENSIONS)
            .pick_files()
    }

    fn pick_folders(&self) -> Option<Vec<PathBuf>> {
        rfd::FileDialog::new()
            .set_title("Add folders of panels")
            .pick_folders()
    }
}

/// What a batch of paths to add stands for ([`panel_files_in`]): the panel
/// files, and the folders that gave none, so that a batch adding nothing
/// can say why.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Found {
    /// The panel files, normalised, in batch order.
    pub files: Vec<PathBuf>,
    /// Folders with no panel file at their top level.
    pub empty: Vec<PathBuf>,
    /// Folders that could not be read.
    pub unreadable: Vec<PathBuf>,
    /// Files that are not panel files (by extension), and paths that do
    /// not exist.
    pub skipped: Vec<PathBuf>,
}

impl Found {
    /// Panel files alone (picked, or a typed path's).
    pub fn of_files(files: Vec<PathBuf>) -> Self {
        Found {
            files,
            ..Found::default()
        }
    }
}

/// The panel files `paths` stand for, normalised: a directory contributes
/// the panel files at its top level (never its subfolders), sorted; a file
/// with a panel extension is kept; anything else, or a path that does not
/// exist, is dropped (and noted). Folders giving no file are noted.
/// Blocking (file I/O): call off the UI thread.
pub(crate) fn panel_files_in(paths: Vec<PathBuf>) -> Found {
    let mut out = Found::default();
    for path in paths {
        if path.is_dir() {
            let Ok(entries) = std::fs::read_dir(&path) else {
                out.unreadable.push(path);
                continue;
            };
            let mut files: Vec<PathBuf> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| is_panel_file(p) && p.is_file())
                .map(|p| normalize(&p))
                .collect();
            if files.is_empty() {
                out.empty.push(path);
            }
            files.sort();
            out.files.extend(files);
        } else if is_panel_file(&path) && path.is_file() {
            out.files.push(normalize(&path));
        } else {
            out.skipped.push(path);
        }
    }
    out
}

/// Whether `path` has a panel file extension.
pub(crate) fn is_panel_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| PANEL_EXTENSIONS.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// The panel files `text` names: one path, or a pattern with one `*` in
/// its file name (`dir/*.xisf`) matched against the files of that one
/// directory, ASCII case-insensitively and sorted. Paths are made absolute.
/// Blocking (file I/O): call off the UI thread. Errors are for the user.
pub(crate) fn expand_typed(text: &str) -> Result<Vec<PathBuf>, String> {
    let path = PathBuf::from(text.trim());
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let Some((prefix, suffix)) = name.split_once('*') else {
        if path.is_file() {
            return Ok(vec![normalize(&path)]);
        }
        return Err(format!("{} is not a file.", path.display()));
    };
    if suffix.contains('*') {
        return Err("Only one * is supported, in the file name.".into());
    }
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let (prefix, suffix) = (prefix.to_ascii_lowercase(), suffix.to_ascii_lowercase());
    let entries =
        std::fs::read_dir(&dir).map_err(|e| format!("Cannot read {}: {e}", dir.display()))?;
    let mut out: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter(|e| {
            let n = e.file_name().to_string_lossy().to_ascii_lowercase();
            n.len() >= prefix.len() + suffix.len() && n.starts_with(&prefix) && n.ends_with(&suffix)
        })
        .map(|e| normalize(&e.path()))
        .collect();
    out.sort();
    if out.is_empty() {
        return Err(format!("No files match {}.", path.display()));
    }
    Ok(out)
}

/// New panel inputs: the panel list and grouping an edit leads to.
#[derive(Clone, Debug, PartialEq)]
pub struct PanelsEdit {
    /// The panels, in order.
    pub panels: Vec<PanelEntry>,
    /// The grouping switch and rule.
    pub grouping: Grouping,
}

/// What became of [`PanelsEdit::apply_scoped`].
#[derive(Debug)]
pub enum Applied {
    /// Applied and saved.
    Done,
    /// Jobs (other than exports) were running: they were cancelled, and
    /// the edit waits for them to end.
    Queued(PanelsEdit),
    /// A result store was held (by another program): try again later.
    Busy(PanelsEdit),
    /// Refused; a message for the user.
    Failed(String),
}

impl PanelsEdit {
    /// The session's current inputs.
    pub fn of(session: &OpenSession) -> Self {
        let c = &session.mosaic.config;
        PanelsEdit {
            panels: c.panels.clone(),
            grouping: c.grouping.clone(),
        }
    }

    /// Whether these are `config`'s inputs.
    pub fn matches(&self, config: &MosaicConfig) -> bool {
        self.panels == config.panels && self.grouping == config.grouping
    }

    /// [`PanelsEdit::apply_scoped`] over every group (nothing known of
    /// which the edit touches).
    #[cfg(test)]
    pub fn apply(self, session: &mut OpenSession, jobs: &mut JobRunner) -> Applied {
        self.apply_scoped(session, jobs, None)
    }

    /// Apply with [`set_inputs_affecting`] (`scope`: the groups the edit
    /// touches, `None` for every group), or, while any job runs, cancel
    /// every job and hand the edit back to wait for the runner to be idle
    /// (newer work wins). Exports are spared and not waited for: they read
    /// the result without a writer lock and write stale tiles as they are.
    /// Owner (UI) thread.
    pub fn apply_scoped(
        self,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
        scope: Option<&BTreeSet<String>>,
    ) -> Applied {
        if jobs.busy_except_exports() {
            jobs.cancel_all_but_exports();
            return Applied::Queued(self);
        }
        match set_inputs_affecting(
            &mut session.mosaic,
            self.panels.clone(),
            self.grouping.clone(),
            scope,
        ) {
            Ok(()) => Applied::Done,
            Err(e) if e.is_busy() => Applied::Busy(self),
            Err(e) => Applied::Failed(format!("Could not change the panels: {e}")),
        }
    }
}

/// The groups an edit touches, decided on a worker (it reads panel
/// headers): `None` (every group) when the groups as last resolved or the
/// analysed frame are unknown, when the new panels do not resolve or derive
/// a frame, or when the frame moves (the unaffected groups were analysed on
/// the old canvas); else the groups whose panels differ.
///
/// `m` is the session's mosaic with the edit's panels and grouping.
fn edit_scope(
    m: Mosaic,
    old: Option<Groups>,
    frame: Option<ReferenceFrame>,
) -> Option<BTreeSet<String>> {
    let (old, frame) = (old?, frame?);
    let paths: Vec<PathBuf> = m.config.panels.iter().map(|p| p.path.clone()).collect();
    let derived = reference::derive(&paths, m.config.analyze.input).ok()?;
    if derived != frame {
        return None;
    }
    Some(affected_groups(&old, &m.groups().ok()?))
}

/// Where an edit stands in finding out which groups it touches.
#[derive(Debug)]
enum Scope {
    /// Being worked out on a worker.
    Scoping(Task<Option<BTreeSet<String>>>),
    /// Known (`None`: every group).
    Known(Option<BTreeSet<String>>),
}

/// Edits on their way into the session: at most one waiting (for the groups
/// it touches to be worked out, for the jobs to stop, or for a store held
/// elsewhere), and the messages of edits refused since last asked.
#[derive(Debug, Default)]
pub struct PanelEdits {
    /// An edit waiting to be scoped, for the jobs to stop (or a busy store).
    pending: Option<PanelsEdit>,
    /// Which groups `pending` touches; `None` with no edit pending.
    scope: Option<Scope>,
    /// For starting scope workers (set by [`PanelEdits::poll`]); without
    /// one an edit touches every group.
    ctx: Option<egui::Context>,
    /// When to retry a pending edit refused as busy.
    retry_at: Option<Instant>,
    /// Refused edits' messages, for the user.
    errors: Vec<String>,
}

impl PanelEdits {
    /// An edit is waiting (to be scoped, for the jobs to stop, or a busy
    /// store); the automatic pipeline starts nothing meanwhile.
    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// The waiting edit, if any (tests).
    #[cfg(test)]
    pub fn pending(&self) -> Option<&PanelsEdit> {
        self.pending.as_ref()
    }

    /// The waiting edit was refused because a result store is held by
    /// another program (rather than waiting for this one's jobs).
    pub fn waits_for_store(&self) -> bool {
        self.pending.is_some() && self.retry_at.is_some()
    }

    /// The inputs as shown: the waiting edit, else the session's.
    pub fn shown(&self, session: &OpenSession) -> PanelsEdit {
        self.pending
            .clone()
            .unwrap_or_else(|| PanelsEdit::of(session))
    }

    /// Take `edit` (replacing any waiting one, scoped or not): cancel the
    /// jobs, work out on a worker which groups it touches, and apply it once
    /// that is known and the runner is idle (see [`PanelEdits::poll`]).
    /// Applied at once when nothing needs working out (no analysed frame or
    /// resolved groups, or no repaint context yet) and the runner is idle.
    pub fn submit(&mut self, edit: PanelsEdit, session: &mut OpenSession, jobs: &mut JobRunner) {
        self.retry_at = None;
        if jobs.busy_except_exports() {
            jobs.cancel_all_but_exports();
        }
        let task = self.ctx.as_ref().and_then(|ctx| {
            let (old, frame) = (session.groups.clone()?, session.frame.clone()?);
            let mut m = session.mosaic.clone();
            m.config.panels = edit.panels.clone();
            m.config.grouping = edit.grouping.clone();
            Task::spawn("mmm-edit-scope", ctx, move || {
                edit_scope(m, Some(old), Some(frame))
            })
        });
        self.scope = Some(match task {
            Some(t) => Scope::Scoping(t),
            None => Scope::Known(None),
        });
        self.pending = Some(edit);
        self.step(session, jobs);
    }

    /// Move the waiting edit along: take a finished scope, and apply it once
    /// the runner is idle and any busy retry is due.
    fn step(&mut self, session: &mut OpenSession, jobs: &mut JobRunner) {
        if self.pending.is_none() {
            return;
        }
        if let Some(Scope::Scoping(task)) = &self.scope {
            match task.poll() {
                Poll::Pending => {
                    if jobs.busy_except_exports() {
                        jobs.cancel_all_but_exports();
                    }
                    return;
                }
                Poll::Done(scope) => self.scope = Some(Scope::Known(scope)),
                Poll::Lost => self.scope = Some(Scope::Known(None)),
            }
        }
        if jobs.busy_except_exports() {
            // Cancelling again is harmless.
            jobs.cancel_all_but_exports();
            return;
        }
        if self.retry_at.is_some_and(|at| at > Instant::now()) {
            return;
        }
        self.retry_at = None;
        let edit = self.pending.take().expect("pending edit");
        let scope = match self.scope.take() {
            Some(Scope::Known(s)) => s,
            _ => None,
        };
        match edit.apply_scoped(session, jobs, scope.as_ref()) {
            Applied::Done => {}
            Applied::Queued(edit) => {
                self.pending = Some(edit);
                self.scope = Some(Scope::Known(scope));
            }
            Applied::Busy(edit) => {
                self.pending = Some(edit);
                self.scope = Some(Scope::Known(scope));
                self.retry_at = Some(Instant::now() + BUSY_RETRY);
            }
            Applied::Failed(message) => {
                tracing::warn!("{message}");
                self.errors.push(message);
            }
        }
    }

    /// Add `paths` not already in the shown inputs (duplicates, also within
    /// `paths`, ignored), after the others; nothing when none is new. How
    /// many were new.
    pub fn add_paths(
        &mut self,
        paths: Vec<PathBuf>,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
    ) -> usize {
        let mut edit = self.shown(session);
        let mut known: BTreeSet<PathBuf> = edit.panels.iter().map(|p| p.path.clone()).collect();
        let before = edit.panels.len();
        for path in paths {
            if known.insert(path.clone()) {
                edit.panels.push(PanelEntry {
                    path,
                    fingerprint: None,
                    manual_group: None,
                });
            }
        }
        let added = edit.panels.len() - before;
        if added > 0 {
            self.submit(edit, session, jobs);
        }
        added
    }

    /// Cancel jobs started after the waiting edit was queued (newer work
    /// wins; exports are spared), or apply it once its groups are known, the
    /// runner is idle and any busy retry is due. Call every frame, after the
    /// runner's poll.
    pub fn poll(&mut self, ctx: &egui::Context, session: &mut OpenSession, jobs: &mut JobRunner) {
        if self.ctx.is_none() {
            self.ctx = Some(ctx.clone());
        }
        if self.pending.is_none() {
            return;
        }
        self.step(session, jobs);
        if let Some(at) = self.retry_at
            && self.pending.is_some()
        {
            ctx.request_repaint_after(at.saturating_duration_since(Instant::now()));
        }
    }

    /// Apply the waiting edit now, without waiting for its retry or its
    /// scope (every group is then assumed touched; the session is being
    /// left); call only while no job but an export runs. One refused again
    /// because a store is held elsewhere stays waiting.
    pub fn apply_waiting(&mut self, session: &mut OpenSession, jobs: &mut JobRunner) {
        if let Some(edit) = self.pending.take() {
            self.retry_at = None;
            self.pending = Some(edit);
            self.scope = Some(Scope::Known(None));
            self.step(session, jobs);
            if self.pending.is_some() {
                tracing::warn!("the panel edit waits: a result is in use elsewhere");
            }
        }
    }

    /// The messages of edits refused since last asked.
    pub fn take_errors(&mut self) -> Vec<String> {
        std::mem::take(&mut self.errors)
    }
}

#[cfg(test)]
mod scope_tests {
    use std::time::Duration;

    use super::*;
    use crate::jobs::tests::{analyzed, drain};

    /// `session`'s first panel of group `group` (by file-name prefix),
    /// copied to a new panel entry.
    fn copy_of(session: &OpenSession, group: &str, name: &str) -> PanelEntry {
        let first = session
            .mosaic
            .config
            .panels
            .iter()
            .find(|p| {
                p.path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with(group))
            })
            .unwrap()
            .path
            .clone();
        let path = first.with_file_name(name);
        std::fs::copy(&first, &path).unwrap();
        PanelEntry {
            path,
            fingerprint: None,
            manual_group: None,
        }
    }

    fn poll_until_applied(
        edits: &mut PanelEdits,
        s: &mut OpenSession,
        r: &mut JobRunner,
        ctx: &egui::Context,
    ) {
        let deadline = Instant::now() + crate::testutil::PATIENCE;
        while edits.has_pending() {
            assert!(Instant::now() < deadline, "the edit never applied");
            r.poll(s);
            edits.poll(ctx, s, r);
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn marked(s: &OpenSession) -> Vec<String> {
        s.mosaic.config.needs_analyze.iter().cloned().collect()
    }

    #[test]
    fn an_unresolvable_edit_falls_back_to_every_group() {
        let (root, mut s, mut r) = analyzed("edit-unresolvable");
        let mut edit = PanelsEdit::of(&s);
        edit.panels.push(PanelEntry {
            path: root.join("missing.xisf"),
            fingerprint: None,
            manual_group: None,
        });
        // The new panels derive no frame: nothing is known of what the
        // edit touches.
        let mut m = s.mosaic.clone();
        m.config.panels = edit.panels.clone();
        assert_eq!(edit_scope(m, s.groups.clone(), s.frame.clone()), None);

        let ctx = egui::Context::default();
        let mut edits = PanelEdits::default();
        edits.poll(&ctx, &mut s, &mut r);
        edits.submit(edit, &mut s, &mut r);
        poll_until_applied(&mut edits, &mut s, &mut r, &ctx);
        assert!(edits.take_errors().is_empty());
        assert_eq!(marked(&s), ["Ha", "OIII"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_newer_edit_replaces_one_being_scoped() {
        let (root, mut s, mut r) = analyzed("edit-replaced");
        let to_ha = copy_of(&s, "Ha", "Ha_extra.xisf");
        let to_oiii = copy_of(&s, "OIII", "OIII_extra.xisf");
        let ctx = egui::Context::default();
        let mut edits = PanelEdits::default();
        edits.poll(&ctx, &mut s, &mut r);

        // A running job keeps the edits waiting while they are scoped.
        let gate = r.hold();
        r.start_analyze(&mut s);
        let mut first = PanelsEdit::of(&s);
        first.panels.push(to_ha.clone());
        edits.submit(first, &mut s, &mut r);
        assert!(edits.has_pending(), "being scoped");
        let mut second = PanelsEdit::of(&s);
        second.panels.push(to_oiii.clone());
        edits.submit(second.clone(), &mut s, &mut r);
        assert_eq!(edits.pending(), Some(&second));
        gate.open();
        poll_until_applied(&mut edits, &mut s, &mut r, &ctx);
        drain(&mut r, &mut s);

        let paths: Vec<&Path> = s
            .mosaic
            .config
            .panels
            .iter()
            .map(|p| p.path.as_path())
            .collect();
        assert!(paths.contains(&to_oiii.path.as_path()));
        assert!(
            !paths.contains(&to_ha.path.as_path()),
            "the first edit was replaced"
        );
        assert_eq!(marked(&s), ["OIII"], "scoped by the newer edit only");
        let _ = std::fs::remove_dir_all(&root);
    }
}
