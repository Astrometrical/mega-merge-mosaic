//! The panel inputs and how edits to them apply: picking panel files, the
//! group colours kept with the session, and the edit model behind the
//! panel list ([`crate::panel_list`]).
//!
//! # Edits
//!
//! Every edit (add, remove, move, rule) builds a new panel list and
//! grouping, a [`PanelsEdit`], and [`PanelsEdit::apply`]s it with
//! [`set_inputs`]. While a job runs (it may hold a result store) the edit
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
//! next `mosaic.json` save.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::Color32;
use mega_ui::groups::physical_color;
pub use mega_ui::groups::{GROUP_PALETTE, PHYSICAL_SLOTS};
use mmm_core::mosaic::{Grouping, MosaicConfig, PanelEntry, UNGROUPED, set_inputs};
use serde_json::Value;

use crate::jobs::JobRunner;
use crate::session::{OpenSession, normalize};

/// File extensions accepted as panels (the dialog filter, drops and
/// patterns), compared case-insensitively.
pub const PANEL_EXTENSIONS: [&str; 4] = ["xisf", "fits", "fit", "fts"];
/// Wait before retrying an edit refused because a store was busy.
const BUSY_RETRY: Duration = Duration::from_millis(500);

/// Key in `MosaicConfig.ui` of the group colours: group name to an index
/// in [`GROUP_PALETTE`], assigned once (first seen) and then kept.
pub const GROUP_COLORS_KEY: &str = "group_colors";

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

/// What became of [`PanelsEdit::apply`].
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

    /// Apply with [`set_inputs`], or, while any job runs, cancel every job
    /// and hand the edit back to wait for the runner to be idle (newer work
    /// wins). Exports are spared and not waited for: they read the result
    /// without a writer lock and write stale tiles as they are. Owner (UI)
    /// thread.
    pub fn apply(self, session: &mut OpenSession, jobs: &mut JobRunner) -> Applied {
        if jobs.busy_except_exports() {
            jobs.cancel_all_but_exports();
            return Applied::Queued(self);
        }
        match set_inputs(
            &mut session.mosaic,
            self.panels.clone(),
            self.grouping.clone(),
        ) {
            Ok(()) => Applied::Done,
            Err(e) if e.is_busy() => Applied::Busy(self),
            Err(e) => Applied::Failed(format!("Could not change the panels: {e}")),
        }
    }
}

/// Edits on their way into the session: at most one waiting (for the jobs
/// to stop, or for a store held elsewhere), and the messages of edits
/// refused since last asked.
#[derive(Debug, Default)]
pub struct PanelEdits {
    /// An edit waiting for the jobs to stop (or a busy store).
    pending: Option<PanelsEdit>,
    /// When to retry a pending edit refused as busy.
    retry_at: Option<Instant>,
    /// Refused edits' messages, for the user.
    errors: Vec<String>,
}

impl PanelEdits {
    /// An edit is waiting for the jobs to stop (or a busy store); the
    /// automatic pipeline starts nothing meanwhile.
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

    /// Apply `edit`, or keep it waiting (replacing any waiting one).
    pub fn submit(&mut self, edit: PanelsEdit, session: &mut OpenSession, jobs: &mut JobRunner) {
        self.pending = None;
        self.retry_at = None;
        match edit.apply(session, jobs) {
            Applied::Done => {}
            Applied::Queued(edit) => self.pending = Some(edit),
            Applied::Busy(edit) => {
                self.pending = Some(edit);
                self.retry_at = Some(Instant::now() + BUSY_RETRY);
            }
            Applied::Failed(message) => {
                tracing::warn!("{message}");
                self.errors.push(message);
            }
        }
    }

    /// Add `paths` not already in the shown inputs (duplicates, also within
    /// `paths`, ignored), after the others; nothing when none is new.
    pub fn add_paths(
        &mut self,
        paths: Vec<PathBuf>,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
    ) {
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
        if edit.panels.len() > before {
            self.submit(edit, session, jobs);
        }
    }

    /// Cancel jobs started after the waiting edit was queued (newer work
    /// wins; exports are spared), or apply it once the runner is idle and
    /// any busy retry is due. Call every frame, after the runner's poll.
    pub fn poll(&mut self, ctx: &egui::Context, session: &mut OpenSession, jobs: &mut JobRunner) {
        if self.pending.is_none() {
            return;
        }
        if jobs.busy_except_exports() {
            // Cancelling again is harmless.
            jobs.cancel_all_but_exports();
            return;
        }
        let now = Instant::now();
        match self.retry_at {
            Some(at) if at > now => ctx.request_repaint_after(at - now),
            _ => {
                let edit = self.pending.take().expect("pending edit");
                self.submit(edit, session, jobs);
            }
        }
    }

    /// Apply the waiting edit now, without waiting for its retry (the
    /// session is being left); call only while no job but an export runs.
    /// One refused again because a store is held elsewhere stays waiting.
    pub fn apply_waiting(&mut self, session: &mut OpenSession, jobs: &mut JobRunner) {
        if let Some(edit) = self.pending.take() {
            self.submit(edit, session, jobs);
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
