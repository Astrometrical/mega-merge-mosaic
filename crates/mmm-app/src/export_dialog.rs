//! The Export dialog (spec §14): the session's one modal, for choosing
//! what to export, where and how. Owned by the session window, which opens
//! it from **Export…**, the Export step and `Ctrl+E`, and draws it before
//! anything else each frame so that it takes Esc first. Problems are never
//! reported here: an export that fails says so in a toast, like any job.
//!
//! The body, top to bottom:
//!
//! 1. **The counts**: "{n} groups · {m} composites".
//! 2. **The rows**: every group, then every composite, each a checkbox. A
//!    row can be ticked only while it is ready ([`readiness_of`]) and no
//!    merge of its group (of any of a composite's groups) runs or is
//!    queued; otherwise it is greyed with its reason as text ("not
//!    merged", "out of date", "maps no groups", "missing group {g}",
//!    "merging…"), which is also its accessible description. "Settings
//!    changed" never stops a row. On opening, every ready row is ticked but
//!    those the user unticked before ([`Choices::excluded`]): a group merged
//!    or a composite made since starts ticked too.
//! 3. **Folder**: typed, or picked with **Browse…** (a native folder dialog
//!    on a worker, [`FolderPicker`]).
//! 4. **Format**: FITS or TIFF, with a one-line note on what it holds.
//! 5. **File name pattern**: the tokens `{session}` (the session's name)
//!    and `{name}` (the group's or composite's); [`file_names`] makes the
//!    names safe for any file system.
//! 6. **The preview**: the exact file names, one per ticked row. Which of
//!    them exist already is looked up on a worker (it only looks: nothing
//!    is created in the folder), again whenever the folder, pattern,
//!    format or ticked rows change, and when an export ends (one lookup at
//!    a time). The folder must be a full path and there; on Unix, one
//!    whose permissions forbid writing is refused too (Windows' read-only
//!    mark on folders means nothing, so it is never believed: a folder
//!    that refuses the file fails the export, which says so in a toast).
//!    A file is marked "exists" when it is there, or when an export
//!    running or queued ([`JobRunner::export_destinations`]) is to write
//!    it. Until the lookup answers Export waits ("Checking the folder…"),
//!    and each row keeps the mark the last answer gave it, so nothing
//!    jumps; the status line keeps its room when empty for the same
//!    reason. Two rows that would write the same name (ignoring case, as
//!    Windows and macOS do) are marked "same name", and the collision
//!    message says how to part them: add `{name}` to the pattern, or, when
//!    it has it, rename one of them.
//! 7. **The overwrite line**, when files exist: "{n} files already exist
//!    and will be replaced".
//! 8. **The footer**: **Cancel**, and **Export**, which becomes **Overwrite
//!    and export** (Danger) while a ticked file exists. Export is disabled
//!    while nothing is ticked, the folder is empty, missing, not a full
//!    path or read-only, two names collide, the folder is being looked at,
//!    or an analysis runs; the reason is its tooltip, and shows in the
//!    body too (a disabled button takes no keyboard focus).
//!
//! **Export** remembers the folder, format, pattern and the rows unticked
//! in the session's `ui` under [`EXPORT_KEY`] (saved with the window's
//! next save), starts one export job per ticked row
//! ([`JobRunner::start_export`]) and closes. A job may replace its file
//! only when the dialog showed it as there; any other file there by the
//! time the export is written is left alone, and the job fails saying it
//! "appeared meanwhile and was not replaced". **Cancel**, Esc and a click
//! on the backdrop close the dialog, keeping nothing of what changed.
//! Closed from the keyboard (Esc, or **Cancel** or **Export** pressed with
//! Enter or Space), the focus goes back to what had it when the dialog
//! opened (**Export…**, the Export step, or whatever had it at `Ctrl+E`),
//! if it is still there ([`Dialog::close`]); closed with a click, the focus
//! stays where the click put it. A folder dialog that cannot be shown is
//! reported in a toast ([`ExportDialog::take_problem`]).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat};
use egui::{Align, Layout, TextStyle};
use mega_ui::theme::Tone;
use mega_ui::theme::text::{Type, text};
use mega_ui::theme::tokens::{CONTROL_HEIGHT, DIALOG_WIDTH, radius, space};
use mega_ui::widgets::{
    ButtonKind, Dialog, FooterButton, MegaButton, Segment, focus_ring, footer_with, segmented_with,
    text_input,
};
use mmm_core::export::ExportFormat;
use mmm_core::mosaic::grouping::is_reserved_device_name;
use mmm_core::session_summary::ResultState;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::jobs::{ExportTarget, JobRunner};
use crate::session::OpenSession;
use crate::session_window::{compositions_of, counted, out_of_date};
use crate::util::{Poll, Task, group_names, plain_path};

/// Key in `MosaicConfig.ui` of the remembered choices: `{ folder, format:
/// "fits" | "tiff", pattern, excluded: [ {group: name} | {composite: id} ]
/// }`, `excluded` being the rows the user unticked. A `selection` saved by
/// an earlier build (the rows exported) is not read, and goes with the next
/// save.
pub const EXPORT_KEY: &str = "export";
/// The file name pattern until another is chosen.
pub const DEFAULT_PATTERN: &str = "{session}_{name}";
/// The pattern's token for the session's name.
const SESSION_TOKEN: &str = "{session}";
/// The pattern's token for the group's or composite's name.
const NAME_TOKEN: &str = "{name}";
/// A file name the pattern leaves empty.
const FALLBACK_NAME: &str = "export";

/// Why Export waits for an analysis (and why the dialog cannot open).
pub const ANALYSING: &str = "Waiting for the analysis to finish.";
/// Why the dialog cannot open before anything is merged.
pub const NOTHING_MERGED: &str = "Nothing is merged yet.";
/// A row's reason while a merge of its group runs or is queued.
const MERGING: &str = "merging\u{2026}";
/// Why Export waits for the folder check.
const CHECKING: &str = "Checking the folder\u{2026}";
/// Why Export cannot go without a ticked row.
const NOTHING_TICKED: &str = "Tick a group or composite to export.";
/// Why Export cannot go without a folder.
const NO_FOLDER: &str = "Choose a folder.";
/// A folder typed relative to nothing in particular.
const NOT_FULL_PATH: &str = "Give the folder's full path";
/// A folder that is not there.
const MISSING_FOLDER: &str = "The folder doesn't exist";
/// A folder that says no file can be created in it (Unix permissions).
const UNWRITABLE_FOLDER: &str = "The folder can't be written to";
/// Two ticked rows would write the same file, the pattern lacking `{name}`.
const COLLISION_ADD_NAME: &str =
    "Two exports would have the same file name: add {name} to the pattern";
/// Two ticked rows would write the same file although the pattern has
/// `{name}` (a group and a composite of one name, or names differing only
/// in case).
const COLLISION_RENAME: &str = "Two exports would have the same file name: rename one of them";
/// The toast when **Browse…** cannot show the folder dialog.
const PICKER_FAILED: &str = "The folder picker could not be shown.";
/// The mark of a name that exists already.
const EXISTS: &str = "exists";
/// The mark of a name another ticked row would write too.
const SAME_NAME: &str = "same name";

/// Whether a group or composite can be exported now, and if not, why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Readiness {
    /// Its mosaic (each of a composite's) is complete and up to date.
    Ready,
    /// A group with no result yet (for a composite: its first group not
    /// ready has none).
    NotMerged,
    /// A group whose result is not complete, needs a new analysis, or has a
    /// region merged with other settings ([`out_of_date`]).
    OutOfDate,
    /// A composite that maps no group to any channel.
    MapsNoGroups,
    /// A composite mapping a group the session no longer has (renamed or
    /// gone): the first such, in channel order.
    MissingGroup(String),
}

impl Readiness {
    /// What a row not ready says: "not merged", "out of date", "maps no
    /// groups", "missing group {g}"; `None` when ready.
    pub fn reason(&self) -> Option<String> {
        match self {
            Readiness::Ready => None,
            Readiness::NotMerged => Some("not merged".into()),
            Readiness::OutOfDate => Some("out of date".into()),
            Readiness::MapsNoGroups => Some("maps no groups".into()),
            Readiness::MissingGroup(g) => Some(format!("missing group {g}")),
        }
    }
}

/// Whether `target` can be exported. A group is ready when its result is
/// complete and not [`out_of_date`] ("Settings changed" does not matter);
/// with no result it is not merged, else out of date. A composite needs a
/// group in some channel, every group it maps still in the session, and
/// every one ready; else the reason of the first not ready, in channel
/// order.
pub fn readiness_of(session: &OpenSession, target: &ExportTarget) -> Readiness {
    match target {
        ExportTarget::Group(g) => group_readiness(session, g),
        ExportTarget::Composite { .. } => {
            let groups = target.groups();
            if groups.is_empty() {
                return Readiness::MapsNoGroups;
            }
            let known = group_names(session);
            if let Some(g) = groups.iter().find(|g| !known.iter().any(|k| k == *g)) {
                return Readiness::MissingGroup(g.to_string());
            }
            groups
                .iter()
                .map(|g| group_readiness(session, g))
                .find(|r| *r != Readiness::Ready)
                .unwrap_or(Readiness::Ready)
        }
    }
}

fn group_readiness(session: &OpenSession, group: &str) -> Readiness {
    match session.results.get(group) {
        None | Some(ResultState::None) => Readiness::NotMerged,
        Some(_) if out_of_date(session, group) => Readiness::OutOfDate,
        Some(_) => Readiness::Ready,
    }
}

/// The file name for each of `names` (groups' or composites'): `pattern`
/// with `{session}` and `{name}` filled in, made safe, then `.ext`. Safe:
/// trimmed of spaces, and of dots at the end; each path separator,
/// character Windows refuses (`<>:"/\|?*`) and control character made `_`;
/// `export` if nothing is left; and a Windows device name (`CON`, `nul`,
/// `COM1`, …) given a `_`. Two names may still come out the same: the
/// dialog looks for that.
pub fn file_names(pattern: &str, session: &str, names: &[&str], ext: &str) -> Vec<String> {
    names
        .iter()
        .map(|name| format!("{}.{ext}", safe_stem(&expand(pattern, session, name))))
        .collect()
}

/// `pattern` with its tokens filled in, in one pass (a name holding a
/// token's text is left as it is).
fn expand(pattern: &str, session: &str, name: &str) -> String {
    let mut out = String::new();
    let mut rest = pattern;
    while let Some(at) = rest.find('{') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        rest = if let Some(after) = tail.strip_prefix(SESSION_TOKEN) {
            out.push_str(session);
            after
        } else if let Some(after) = tail.strip_prefix(NAME_TOKEN) {
            out.push_str(name);
            after
        } else {
            out.push('{');
            &tail[1..]
        };
    }
    out.push_str(rest);
    out
}

/// `stem` safe as a file name's stem (see [`file_names`]).
fn safe_stem(stem: &str) -> String {
    let trimmed = stem
        .trim()
        .trim_end_matches(|c: char| c == '.' || c.is_whitespace());
    let mut safe: String = trimmed
        .chars()
        .map(|c| {
            let refused = matches!(c, '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*');
            if refused || c.is_control() { '_' } else { c }
        })
        .collect();
    if safe.is_empty() {
        safe = FALLBACK_NAME.into();
    }
    if is_reserved_device_name(&safe) {
        let at = safe.find('.').unwrap_or(safe.len());
        safe.insert(at, '_');
    }
    safe
}

/// A row of the dialog, as remembered among the exclusions: a group by name or
/// a composite by id (a composite may share a group's name).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Pick {
    /// A group, by name.
    Group(String),
    /// A composite, by [`crate::compositions::Composition::id`].
    Composite(u64),
}

/// The choices the session remembers ([`EXPORT_KEY`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Choices {
    /// The folder, as typed or picked.
    pub folder: String,
    /// The format.
    pub format: ExportFormat,
    /// The file name pattern.
    pub pattern: String,
    /// The rows the user unticked: they start unticked, and every other
    /// ready row starts ticked (one created or merged since, too).
    pub excluded: Vec<Pick>,
}

impl Choices {
    /// The choices saved in `ui` (the session's UI settings). Each field is
    /// read by itself: one missing or malformed (a blank folder or pattern,
    /// a format other than "fits" or "tiff") is its default: `folder`
    /// (where the session folder is; written without Windows' verbatim
    /// prefix, [`plain_path`]), FITS, [`DEFAULT_PATTERN`], nothing
    /// excluded. A malformed entry of `excluded` is skipped. A `selection`
    /// an earlier build saved (the rows exported, not those left out) is
    /// ignored: read as exclusions it would leave out every row made
    /// since, and every ready row starting ticked is the spec's rule.
    pub fn load(ui: &Value, folder: &Path) -> Self {
        let saved = ui.get(EXPORT_KEY).filter(|v| v.is_object());
        let text = |key: &str| {
            saved
                .and_then(|s| s.get(key))
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string)
        };
        let format = match saved.and_then(|s| s.get("format")).and_then(Value::as_str) {
            Some("tiff") => ExportFormat::Tiff,
            _ => ExportFormat::Fits,
        };
        let excluded = saved
            .and_then(|s| s.get("excluded"))
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|v| serde_json::from_value::<Pick>(v.clone()).ok())
                    .collect()
            })
            .unwrap_or_default();
        Choices {
            folder: text("folder").unwrap_or_else(|| plain_path(folder)),
            format,
            pattern: text("pattern").unwrap_or_else(|| DEFAULT_PATTERN.into()),
            excluded,
        }
    }

    /// Write them into `ui` (made an object if it is not), for the caller
    /// to save.
    pub fn save(&self, ui: &mut Value) {
        if !ui.is_object() {
            *ui = Value::Object(Default::default());
        }
        let mut saved = serde_json::Map::new();
        saved.insert("folder".into(), self.folder.clone().into());
        let format = match self.format {
            ExportFormat::Fits => "fits",
            ExportFormat::Tiff => "tiff",
        };
        saved.insert("format".into(), format.into());
        saved.insert("pattern".into(), self.pattern.clone().into());
        if let Ok(v) = serde_json::to_value(&self.excluded) {
            saved.insert("excluded".into(), v);
        }
        if let Some(obj) = ui.as_object_mut() {
            obj.insert(EXPORT_KEY.into(), Value::Object(saved));
        }
    }
}

/// Asks for the folder to export into. Called on a background thread.
pub trait FolderPicker: Send + Sync {
    /// Show a folder dialog, starting in `start` if given; `None` when
    /// cancelled or when no dialog is available.
    fn pick_folder(&self, start: Option<&Path>) -> Option<PathBuf>;
}

/// The native folder dialog through `rfd`.
pub struct RfdFolderPicker;

impl FolderPicker for RfdFolderPicker {
    fn pick_folder(&self, start: Option<&Path>) -> Option<PathBuf> {
        let mut dialog = rfd::FileDialog::new().set_title("Export into this folder");
        if let Some(start) = start.filter(|s| s.is_dir()) {
            dialog = dialog.set_directory(start);
        }
        dialog.pick_folder()
    }
}

/// The Export dialog's id (its modal's).
fn dialog_id() -> egui::Id {
    egui::Id::new("export-dialog")
}

/// One row: a group or a composite, how it stands, and what it exports.
struct Row {
    pick: Pick,
    target: ExportTarget,
    readiness: Readiness,
    /// A merge of its group (of one of a composite's groups) runs or is
    /// queued: its result is about to change.
    merging: bool,
}

impl Row {
    fn name(&self) -> &str {
        self.target.name()
    }

    /// Whether it can be ticked now.
    fn tickable(&self) -> bool {
        self.readiness == Readiness::Ready && !self.merging
    }

    /// Why it cannot be ticked, if it cannot: what is wrong with a
    /// composite's mapping first, then a merge on its way, then its
    /// result.
    fn reason(&self) -> Option<String> {
        match &self.readiness {
            r @ (Readiness::MapsNoGroups | Readiness::MissingGroup(_)) => r.reason(),
            _ if self.merging => Some(MERGING.into()),
            r => r.reason(),
        }
    }
}

/// The session's rows: its groups, then its composites.
fn rows(session: &OpenSession, jobs: &JobRunner) -> Vec<Row> {
    let groups = group_names(session)
        .into_iter()
        .map(|g| (Pick::Group(g.clone()), ExportTarget::Group(g)));
    let composites = compositions_of(session).into_iter().map(|c| {
        let target = ExportTarget::Composite {
            id: c.id,
            name: c.name,
            channels: c.channels.map(|ch| ch.map(|(g, _)| g)),
        };
        (Pick::Composite(c.id), target)
    });
    groups
        .chain(composites)
        .map(|(pick, target)| Row {
            readiness: readiness_of(session, &target),
            merging: target.groups().iter().any(|g| jobs.blend_pending(g)),
            pick,
            target,
        })
        .collect()
}

/// What a folder lookup looks at: the folder and the file names.
#[derive(Clone, Debug, PartialEq)]
struct Lookup {
    folder: PathBuf,
    names: Vec<String>,
}

/// What a folder lookup found.
#[derive(Clone, Debug)]
struct Found {
    /// Why the folder cannot take the files, if it is not a full path, not
    /// there, or (on Unix) read-only.
    problem: Option<&'static str>,
    /// The names that exist there already.
    existing: BTreeSet<String>,
}

/// Look in `lookup`'s folder (on a worker): is it a full path, there and
/// not read-only, and which of the names exist. Nothing is created there.
fn look(lookup: &Lookup) -> Found {
    let folder = &lookup.folder;
    let problem = if !folder.is_absolute() {
        Some(NOT_FULL_PATH)
    } else if !folder.is_dir() {
        Some(MISSING_FOLDER)
    } else if read_only(folder) {
        Some(UNWRITABLE_FOLDER)
    } else {
        None
    };
    let existing = match problem {
        Some(NOT_FULL_PATH | MISSING_FOLDER) => BTreeSet::new(),
        _ => lookup
            .names
            .iter()
            .filter(|n| folder.join(n).exists())
            .cloned()
            .collect(),
    };
    Found { problem, existing }
}

/// Whether `folder` says no file can be created in it. Only Unix's
/// permission bits mean that: Windows marks Documents, Pictures and
/// customised folders read-only and ignores the mark on folders, so there
/// it is never taken for a refusal. A folder that refuses the file anyway
/// fails the export, which says so in a toast.
fn read_only(folder: &Path) -> bool {
    #[cfg(unix)]
    {
        std::fs::metadata(folder).is_ok_and(|m| m.permissions().readonly())
    }
    #[cfg(not(unix))]
    {
        let _ = folder;
        false
    }
}

/// `path` as compared with another: without Windows' verbatim prefix,
/// with its separators normalised, and ignoring case (one file on Windows
/// and macOS; elsewhere, taking two such names for one is the safe side).
fn path_key(path: &Path) -> PathBuf {
    PathBuf::from(plain_path(path).to_lowercase())
        .components()
        .collect()
}

/// Holds folder lookups back until its sender is dropped (tests).
#[cfg(test)]
type CheckHold = Arc<std::sync::Mutex<std::sync::mpsc::Receiver<()>>>;

/// A folder lookup running: what it looks at, for which rows, the ended
/// exports' destinations its answer accounts for, and the task.
struct Looking {
    lookup: Lookup,
    picks: Vec<Pick>,
    covers: BTreeSet<PathBuf>,
    task: Task<Found>,
}

/// The dialog while open: the choices being made, and the lookups.
struct Choosing {
    ticked: BTreeSet<Pick>,
    /// The rows the user unticked: the remembered ones, as ticked and
    /// unticked since.
    excluded: BTreeSet<Pick>,
    folder: String,
    format: ExportFormat,
    pattern: String,
    /// The folder dialog, while shown.
    picking: Option<Task<Option<PathBuf>>>,
    /// The folder lookup running.
    looking: Option<Looking>,
    /// The last lookup's answer, and what it looked at.
    found: Option<(Lookup, Found)>,
    /// Whether each row's file existed, as the last answer said: it stands
    /// for the row while the next lookup runs, so nothing jumps.
    last_marks: BTreeMap<Pick, bool>,
    /// What the choices ask to look at, as last drawn.
    wanted: Option<Lookup>,
    /// The destinations of the exports running or queued, as last seen.
    in_flight: Vec<PathBuf>,
    /// The destinations of exports that ended since the lookup in hand
    /// started: taken as there until the folder has been looked at again.
    ended: BTreeSet<PathBuf>,
    /// An export ended: the folder is to be looked at again.
    relook: bool,
}

/// What the user did in the dialog this frame.
enum Act {
    Cancel,
    Browse,
    Export,
}

/// The Export dialog (see the module docs).
pub struct ExportDialog {
    open: Option<Choosing>,
    picker: Arc<dyn FolderPicker>,
    /// A problem to report in a toast (the folder dialog could not show).
    problem: Option<String>,
    #[cfg(test)]
    hold: Option<CheckHold>,
}

impl ExportDialog {
    /// A closed dialog whose **Browse…** asks `picker`.
    pub fn new(picker: Arc<dyn FolderPicker>) -> Self {
        ExportDialog {
            open: None,
            picker,
            problem: None,
            #[cfg(test)]
            hold: None,
        }
    }

    /// **Browse…** asks `picker` from now on (tests).
    #[cfg(test)]
    pub(crate) fn set_picker(&mut self, picker: Arc<dyn FolderPicker>) {
        self.picker = picker;
    }

    /// Whether the dialog is open.
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// A problem since the last call, for a toast: the folder dialog could
    /// not be shown.
    pub fn take_problem(&mut self) -> Option<String> {
        self.problem.take()
    }

    /// Open the dialog on `session`'s remembered choices (else the
    /// defaults: beside the session folder, FITS, [`DEFAULT_PATTERN`]),
    /// every ready row ticked but those the user unticked
    /// ([`Choices::excluded`]). No I/O. Nothing if it is open already.
    pub fn open(&mut self, session: &OpenSession, jobs: &JobRunner) {
        if self.open.is_some() {
            return;
        }
        let dir = &session.mosaic.dir;
        let choices = Choices::load(&session.mosaic.config.ui, dir.parent().unwrap_or(dir));
        let excluded: BTreeSet<Pick> = choices.excluded.into_iter().collect();
        let ticked = rows(session, jobs)
            .into_iter()
            .filter(|r| r.tickable() && !excluded.contains(&r.pick))
            .map(|r| r.pick)
            .collect();
        self.open = Some(Choosing {
            ticked,
            excluded,
            folder: choices.folder,
            format: choices.format,
            pattern: choices.pattern,
            picking: None,
            looking: None,
            found: None,
            last_marks: BTreeMap::new(),
            wanted: None,
            in_flight: jobs.export_destinations(),
            ended: BTreeSet::new(),
            relook: false,
        });
    }

    /// Draw the dialog, if open, and act on it (see the module docs).
    /// Whether the session's `ui` changed (the caller saves it).
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
    ) -> bool {
        let Some(c) = &mut self.open else {
            return false;
        };
        if !c.poll() {
            self.problem = Some(PICKER_FAILED.into());
        }
        c.follow_exports(jobs.export_destinations());
        let rows = rows(session, jobs);
        let analysing = jobs.analyzing();
        let session_name = session.name();
        let shown = Shown::of(c, &rows, &session_name);
        let response = Dialog {
            title: "Export",
            width: DIALOG_WIDTH,
        }
        .show(ctx, dialog_id(), |ui| body(ui, c, &rows, &shown, analysing));
        let act = if response.close {
            Some(Act::Cancel)
        } else {
            response.inner
        };
        match act {
            Some(Act::Cancel) => {
                self.open = None;
                Dialog::close(ctx, dialog_id());
                return false;
            }
            Some(Act::Browse) => {
                let picker = self.picker.clone();
                let start = PathBuf::from(c.folder.trim());
                c.picking = Task::spawn("mmm-export-folder", ctx, move || {
                    picker.pick_folder(Some(&start))
                });
                if c.picking.is_none() {
                    self.problem = Some(PICKER_FAILED.into());
                }
            }
            Some(Act::Export) => {
                let changed = c.export(session, jobs, &rows, &shown);
                self.open = None;
                Dialog::close(ctx, dialog_id());
                return changed;
            }
            None => {}
        }
        // Look again at what the choices are now (after this frame's
        // clicks and typing), or once more when an export ended.
        let now = Shown::of(c, &rows, &session_name);
        #[cfg(test)]
        let hold = self.hold.clone();
        if let Some(lookup) = &now.lookup
            && c.looking.is_none()
            && (c.relook || c.found.as_ref().is_none_or(|(l, _)| l != lookup))
        {
            c.relook = false;
            let job = lookup.clone();
            let task = Task::spawn("mmm-export-lookup", ctx, move || {
                #[cfg(test)]
                if let Some(hold) = hold {
                    let _ = hold
                        .lock()
                        .map(|rx| rx.recv_timeout(crate::testutil::PATIENCE));
                }
                look(&job)
            });
            c.looking = task.map(|task| Looking {
                lookup: lookup.clone(),
                picks: now.chosen.iter().map(|&i| rows[i].pick.clone()).collect(),
                covers: c.ended.clone(),
                task,
            });
        }
        c.wanted = now.lookup;
        false
    }

    /// Hold the folder lookups until the returned sender is dropped (tests).
    #[cfg(test)]
    pub(crate) fn hold_checks(&mut self) -> std::sync::mpsc::Sender<()> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.hold = Some(Arc::new(std::sync::Mutex::new(rx)));
        tx
    }

    /// Open, with no folder dialog shown and the folder looked at for what
    /// the choices ask now (tests).
    #[cfg(test)]
    pub(crate) fn settled(&self) -> bool {
        self.open.as_ref().is_some_and(|c| {
            c.picking.is_none()
                && c.looking.is_none()
                && !c.relook
                && c.wanted
                    .as_ref()
                    .is_none_or(|w| c.found.as_ref().is_some_and(|(l, _)| l == w))
        })
    }

    /// The folder as typed (tests).
    #[cfg(test)]
    pub(crate) fn folder(&self) -> &str {
        self.open.as_ref().map_or("", |c| c.folder.as_str())
    }

    /// The format chosen (tests).
    #[cfg(test)]
    pub(crate) fn format(&self) -> ExportFormat {
        self.open.as_ref().map_or(ExportFormat::Fits, |c| c.format)
    }

    /// The pattern as typed (tests).
    #[cfg(test)]
    pub(crate) fn pattern(&self) -> &str {
        self.open.as_ref().map_or("", |c| c.pattern.as_str())
    }

    /// The rows ticked (some may not be tickable now), in order (tests).
    #[cfg(test)]
    pub(crate) fn ticked(&self) -> Vec<Pick> {
        self.open
            .as_ref()
            .map(|c| c.ticked.iter().cloned().collect())
            .unwrap_or_default()
    }
}

impl Choosing {
    /// Take the folder dialog's and the lookup's answers; `false` when the
    /// folder dialog could not be shown (its thread ended without one).
    fn poll(&mut self) -> bool {
        let mut shown = true;
        if let Some(task) = &self.picking {
            match task.poll() {
                Poll::Pending => {}
                Poll::Done(picked) => {
                    self.picking = None;
                    if let Some(p) = picked {
                        self.folder = plain_path(&p);
                    }
                }
                Poll::Lost => {
                    self.picking = None;
                    shown = false;
                }
            }
        }
        if let Some(looking) = &self.looking {
            match looking.task.poll() {
                Poll::Pending => {}
                Poll::Done(found) => {
                    let Looking {
                        lookup,
                        picks,
                        covers,
                        ..
                    } = self.looking.take().expect("a lookup");
                    self.last_marks = picks
                        .into_iter()
                        .zip(&lookup.names)
                        .map(|(p, n)| (p, found.existing.contains(n)))
                        .collect();
                    self.ended.retain(|p| !covers.contains(p));
                    self.found = Some((lookup, found));
                }
                Poll::Lost => {
                    // Unknown: never taken for an answer; looked up again.
                    self.looking = None;
                    self.found = None;
                }
            }
        }
        shown
    }

    /// Note the exports running or queued (`busy`, their destinations): one
    /// that ended since last seen has written its file (or not), so the
    /// folder is looked at again, its file taken as there meanwhile.
    fn follow_exports(&mut self, busy: Vec<PathBuf>) {
        for p in &self.in_flight {
            if !busy.contains(p) {
                self.ended.insert(p.clone());
                self.relook = true;
            }
        }
        self.in_flight = busy;
    }

    /// **Export**: remember the choices in `session`'s `ui`, then start one
    /// export job per ticked row, allowed to replace its file only when
    /// the dialog showed it as there (existing, or an export's on its way).
    /// Whether `ui` changed (it did).
    fn export(
        &self,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
        rows: &[Row],
        shown: &Shown,
    ) -> bool {
        let folder = PathBuf::from(self.folder.trim());
        // The rows unticked, of those the session still has.
        let excluded = rows
            .iter()
            .filter(|r| self.excluded.contains(&r.pick))
            .map(|r| r.pick.clone())
            .collect();
        let choices = Choices {
            folder: self.folder.trim().to_string(),
            format: self.format,
            pattern: self.pattern.clone(),
            excluded,
        };
        choices.save(&mut session.mosaic.config.ui);
        for ((&i, name), &replace) in shown.chosen.iter().zip(&shown.names).zip(&shown.exists) {
            let target = rows[i].target.clone();
            jobs.start_export(session, target, folder.join(name), self.format, replace);
        }
        true
    }
}

/// What the dialog shows for its choices this frame: the rows exported,
/// their file names and marks, the lookup they need, and what is known.
struct Shown {
    /// Indices into the rows of those ticked and tickable, in order.
    chosen: Vec<usize>,
    /// Their file names.
    names: Vec<String>,
    /// Whether each file is there: found there, or an export on its way
    /// writes it. While the folder is being looked at again, the last
    /// answer stands for each row.
    exists: Vec<bool>,
    /// What to look up for them (`None` without a folder).
    lookup: Option<Lookup>,
    /// The names (lowercase) more than one row would write.
    colliding: BTreeSet<String>,
    /// What to say about them, if any collide.
    collision: Option<&'static str>,
    /// The folder is being looked at: no answer yet for these choices, or
    /// a lookup still runs.
    checking: bool,
    /// Why the folder cannot take the files, as known: none chosen, not a
    /// full path, not there, or read-only (the last answer's while the
    /// folder is being looked at again).
    folder_problem: Option<&'static str>,
}

impl Shown {
    fn of(c: &Choosing, rows: &[Row], session: &str) -> Self {
        let chosen: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.tickable() && c.ticked.contains(&r.pick))
            .map(|(i, _)| i)
            .collect();
        let row_names: Vec<&str> = chosen.iter().map(|&i| rows[i].name()).collect();
        let names = file_names(&c.pattern, session, &row_names, c.format.extension());
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for n in &names {
            *counts.entry(n.to_lowercase()).or_default() += 1;
        }
        let colliding: BTreeSet<String> = counts
            .into_iter()
            .filter(|(_, k)| *k > 1)
            .map(|(n, _)| n)
            .collect();
        let collision = (!colliding.is_empty()).then(|| {
            if c.pattern.contains(NAME_TOKEN) {
                COLLISION_RENAME
            } else {
                COLLISION_ADD_NAME
            }
        });
        let folder = c.folder.trim();
        let lookup = (!folder.is_empty()).then(|| Lookup {
            folder: PathBuf::from(folder),
            names: names.clone(),
        });
        let found = c
            .found
            .as_ref()
            .filter(|(l, _)| Some(l) == lookup.as_ref())
            .map(|(_, f)| f);
        let coming: BTreeSet<PathBuf> = c
            .in_flight
            .iter()
            .chain(&c.ended)
            .map(|p| path_key(p))
            .collect();
        let exists = chosen
            .iter()
            .zip(&names)
            .map(|(&i, n)| {
                let on_its_way = lookup
                    .as_ref()
                    .is_some_and(|l| coming.contains(&path_key(&l.folder.join(n))));
                on_its_way
                    || match found {
                        Some(f) => f.existing.contains(n),
                        None => c.last_marks.get(&rows[i].pick).copied().unwrap_or(false),
                    }
            })
            .collect();
        let checking = lookup.is_some() && (found.is_none() || c.looking.is_some() || c.relook);
        let folder_problem = match &lookup {
            None => Some(NO_FOLDER),
            Some(_) => match found {
                Some(f) => f.problem,
                None => c.found.as_ref().and_then(|(_, f)| f.problem),
            },
        };
        Shown {
            chosen,
            names,
            exists,
            lookup,
            colliding,
            collision,
            checking,
            folder_problem,
        }
    }

    /// How many of the files are there (or on their way), as shown.
    fn existing(&self) -> usize {
        self.exists.iter().filter(|&&e| e).count()
    }

    /// Why Export cannot be pressed, if it cannot.
    fn block(&self, analysing: bool) -> Option<&'static str> {
        if analysing {
            Some(ANALYSING)
        } else if self.chosen.is_empty() {
            Some(NOTHING_TICKED)
        } else if self.lookup.is_none() {
            Some(NO_FOLDER)
        } else if let Some(collision) = self.collision {
            Some(collision)
        } else if self.checking {
            Some(CHECKING)
        } else {
            self.folder_problem
        }
    }
}

/// The dialog's body (see the module docs); what was pressed. All but the
/// footer scrolls when the window is too short for it, so **Cancel** and
/// **Export** always show.
fn body(
    ui: &mut egui::Ui,
    c: &mut Choosing,
    rows: &[Row],
    shown: &Shown,
    analysing: bool,
) -> Option<Act> {
    let mut act = None;
    egui::ScrollArea::vertical()
        .id_salt("export-body")
        .max_height(body_max_height(ui))
        .auto_shrink([false, true])
        .show(ui, |ui| {
            rows_ui(ui, rows, &mut c.ticked, &mut c.excluded);
            ui.add_space(space::M);
            if settings_ui(ui, c, shown) {
                act = Some(Act::Browse);
            }
            ui.add_space(space::M);
            ui.label(text(Type::Body, "Files"));
            preview_ui(ui, shown);
            let existing = shown.existing();
            if existing > 0 {
                let line = if existing == 1 {
                    "1 file already exists and will be replaced".to_string()
                } else {
                    format!("{existing} files already exist and will be replaced")
                };
                ui.add_space(space::S);
                ui.label(text(Type::Body, line).color(Tone::Warning.color()));
            }
            if analysing {
                ui.add_space(space::S);
                ui.label(text(Type::Small, ANALYSING));
            }
        });
    let blocked = shown.block(analysing);
    let (kind, label) = if shown.existing() > 0 {
        (ButtonKind::Danger, "Overwrite and export")
    } else {
        (ButtonKind::Primary, "Export")
    };
    let pressed = footer_with(
        ui,
        &[
            FooterButton {
                kind: ButtonKind::Quiet,
                label: "Cancel",
                disabled: None,
            },
            FooterButton {
                kind,
                label,
                disabled: blocked,
            },
        ],
    );
    match pressed {
        Some(0) => Some(Act::Cancel),
        Some(_) if blocked.is_none() => Some(Act::Export),
        _ => act,
    }
}

/// The most the scrolling part of the body may take: the window's height
/// less the title, the footer, the card's margins and as much again around
/// the card.
fn body_max_height(ui: &egui::Ui) -> f32 {
    let title = ui.text_style_height(&Type::Title.text_style()) + space::L;
    let footer = space::L + CONTROL_HEIGHT;
    let margins = 2.0 * space::XL + 2.0 * space::XL;
    let gaps = 3.0 * ui.spacing().item_spacing.y;
    (ui.ctx().content_rect().height() - title - footer - margins - gaps).max(CONTROL_HEIGHT)
}

/// The counts, then the rows: the groups, then the composites, each part
/// under its heading.
fn rows_ui(
    ui: &mut egui::Ui,
    rows: &[Row],
    ticked: &mut BTreeSet<Pick>,
    excluded: &mut BTreeSet<Pick>,
) {
    let groups = rows
        .iter()
        .filter(|r| matches!(r.pick, Pick::Group(_)))
        .count();
    let composites = rows.len() - groups;
    ui.label(text(
        Type::Body,
        format!(
            "{} \u{b7} {}",
            counted(groups, "group"),
            counted(composites, "composite")
        ),
    ));
    let (group_rows, composite_rows) = rows.split_at(groups);
    for (heading, part) in [("Groups", group_rows), ("Composites", composite_rows)] {
        if part.is_empty() {
            continue;
        }
        ui.add_space(space::S);
        ui.label(text(Type::Overline, heading));
        for r in part {
            row_ui(ui, r, ticked, excluded);
        }
    }
}

/// A row: its checkbox (disabled unless it can be ticked) and, when it
/// cannot, its reason as text, also its checkbox's description. Unticked,
/// the row is `excluded` (it starts unticked next time); ticked, no more.
fn row_ui(
    ui: &mut egui::Ui,
    row: &Row,
    ticked: &mut BTreeSet<Pick>,
    excluded: &mut BTreeSet<Pick>,
) {
    ui.horizontal(|ui| {
        let tickable = row.tickable();
        let mut on = tickable && ticked.contains(&row.pick);
        let r = ui.add_enabled(tickable, egui::Checkbox::new(&mut on, row.name()));
        focus_ring(ui, &r, radius::SMALL);
        if let Some(why) = row.reason() {
            ui.ctx()
                .accesskit_node_builder(r.id, |node| node.set_description(why.as_str()));
            ui.label(text(Type::Small, why));
        }
        if r.changed() && tickable {
            if on {
                ticked.insert(row.pick.clone());
                excluded.remove(&row.pick);
            } else {
                ticked.remove(&row.pick);
                excluded.insert(row.pick.clone());
            }
        }
    });
}

/// The folder (with **Browse…**), the format and the file name pattern,
/// each named in a column of its own. Whether **Browse…** was pressed.
fn settings_ui(ui: &mut egui::Ui, c: &mut Choosing, shown: &Shown) -> bool {
    const FOLDER: &str = "Folder";
    const FORMAT: &str = "Format";
    const PATTERN: &str = "File name pattern";
    const BROWSE: &str = "Browse\u{2026}";
    let names = [FOLDER, FORMAT, PATTERN].map(|n| text_width(ui, text(Type::Body, n)));
    let names_width = names.into_iter().fold(0.0, f32::max);
    let mut browse = false;
    setting(ui, names_width, FOLDER, |ui, label| {
        ui.horizontal(|ui| {
            let button = text_width(
                ui,
                egui::RichText::new(BROWSE).text_style(TextStyle::Button),
            ) + 2.0 * ui.spacing().button_padding.x;
            let field = ui.available_width() - button - ui.spacing().item_spacing.x;
            ui.allocate_ui(egui::vec2(field, CONTROL_HEIGHT), |ui| {
                text_input(ui, &mut c.folder, "Where the files go").labelled_by(label);
            });
            let button = MegaButton::new(ButtonKind::Secondary, BROWSE);
            browse = ui.add_enabled(c.picking.is_none(), button).clicked();
        });
        if let Some(why) = shown.folder_problem {
            ui.label(text(Type::Small, why).color(Tone::Problem.color()));
        }
    });
    ui.add_space(space::S);
    setting(ui, names_width, FORMAT, |ui, label| {
        let options = [ExportFormat::Fits, ExportFormat::Tiff].map(|f| Segment {
            value: f,
            label: f.name(),
            disabled: None,
            tooltip: None,
        });
        segmented_with(ui, &mut c.format, &options).labelled_by(label);
        ui.label(text(
            Type::Small,
            match c.format {
                ExportFormat::Fits => "32-bit float, linear, with WCS and provenance.",
                ExportFormat::Tiff => "32-bit float, linear (values are not clamped).",
            },
        ));
    });
    ui.add_space(space::S);
    setting(ui, names_width, PATTERN, |ui, label| {
        text_input(ui, &mut c.pattern, DEFAULT_PATTERN).labelled_by(label);
        ui.label(text(Type::Small, "Tokens: {session} and {name}"));
    });
    browse
}

/// One setting: `name` in a column `names_width` wide, level with the first
/// control, then what `add` draws below one another, given the name's id
/// (to label its control with).
fn setting(
    ui: &mut egui::Ui,
    names_width: f32,
    name: &str,
    add: impl FnOnce(&mut egui::Ui, egui::Id),
) {
    ui.horizontal_top(|ui| {
        let size = egui::vec2(names_width, CONTROL_HEIGHT);
        let label = ui
            .allocate_ui_with_layout(size, Layout::left_to_right(Align::Center), |ui| {
                ui.set_min_size(size);
                ui.label(text(Type::Body, name))
            })
            .inner;
        ui.vertical(|ui| add(ui, label.id));
    });
}

/// How wide `t` is on one line.
fn text_width(ui: &egui::Ui, t: impl Into<egui::WidgetText>) -> f32 {
    t.into()
        .into_galley(
            ui,
            Some(egui::TextWrapMode::Extend),
            f32::INFINITY,
            TextStyle::Body,
        )
        .size()
        .x
}

/// The preview: each file name, marked "same name" or "exists", or why
/// there is none; then a status line: the collision message, the lookup
/// in progress, or nothing (its room kept, so the footer never jumps as a
/// lookup starts and ends).
fn preview_ui(ui: &mut egui::Ui, shown: &Shown) {
    if shown.chosen.is_empty() {
        ui.label(text(Type::Small, NOTHING_TICKED));
        return;
    }
    for (name, &exists) in shown.names.iter().zip(&shown.exists) {
        let mut marks = Vec::new();
        if shown.colliding.contains(&name.to_lowercase()) {
            marks.push((SAME_NAME, Tone::Problem));
        }
        if exists {
            marks.push((EXISTS, Tone::Warning));
        }
        ui.label(file_line(ui, name, &marks));
    }
    if let Some(collision) = shown.collision {
        ui.label(text(Type::Small, collision).color(Tone::Problem.color()));
    } else if shown.checking {
        ui.label(text(Type::Small, CHECKING));
    } else {
        let line = egui::WidgetText::from(text(Type::Small, CHECKING)).into_galley(
            ui,
            Some(egui::TextWrapMode::Wrap),
            ui.available_width(),
            TextStyle::Small,
        );
        ui.allocate_exact_size(egui::vec2(0.0, line.size().y), egui::Sense::hover());
    }
}

/// A file name in the numeric (mono) type, followed by its marks as words
/// in their tone: "m_Ha.fits exists". One label, read as one.
fn file_line(ui: &egui::Ui, name: &str, marks: &[(&str, Tone)]) -> LayoutJob {
    let style = ui.style();
    let mut job = LayoutJob::default();
    let numeric = Type::Numeric.text_style().resolve(style);
    job.append(
        name,
        0.0,
        TextFormat::simple(numeric, Type::Numeric.color()),
    );
    let small = Type::Small.text_style().resolve(style);
    for (i, (mark, tone)) in marks.iter().enumerate() {
        let sep = if i == 0 { " " } else { " \u{b7} " };
        let format = TextFormat::simple(small.clone(), tone.color());
        job.append(&format!("{sep}{mark}"), 0.0, format);
    }
    job
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn file_names_are_sanitised() {
        let names = |pattern: &str, session: &str, names: &[&str], ext: &str| {
            file_names(pattern, session, names, ext)
        };
        // The tokens, and the format's extension.
        assert_eq!(
            names(DEFAULT_PATTERN, "m", &["Ha", "OIII"], "fits"),
            ["m_Ha.fits", "m_OIII.fits"]
        );
        assert_eq!(
            names("{name} ({session})", "Orion", &["H a"], "tif"),
            ["H a (Orion).tif"]
        );
        // A name holding a token's text is not expanded again.
        assert_eq!(
            names("{name}", "s", &["{session}"], "fits"),
            ["{session}.fits"]
        );
        // No path separators, nor any character Windows refuses.
        assert_eq!(
            names("{name}", "m", &["a/b\\c", "<>:\"|?*"], "fits"),
            ["a_b_c.fits", "_______.fits"]
        );
        assert_eq!(names("../{name}", "m", &["Ha"], "fits"), [".._Ha.fits"]);
        // No control characters.
        assert_eq!(names("{name}", "m", &["H\ta\n"], "fits"), ["H_a.fits"]);
        // Trimmed of spaces, and of dots at the end.
        assert_eq!(names(" {name}. ", "m", &["Ha"], "fits"), ["Ha.fits"]);
        assert_eq!(names("{name}", "m", &["Ha..."], "fits"), ["Ha.fits"]);
        // Never a device name, whatever its case.
        assert_eq!(
            names(
                "{name}",
                "m",
                &["CON", "nul", "Com1", "lpt9.x", "CONSOLE"],
                "fits"
            ),
            [
                "CON_.fits",
                "nul_.fits",
                "Com1_.fits",
                "lpt9_.x.fits",
                "CONSOLE.fits"
            ]
        );
        assert_eq!(names("{session}", "aux", &["Ha"], "fits"), ["aux_.fits"]);
        assert_eq!(
            names("{name}", "m", &["COM0", "conin$.x", "LPT²"], "fits"),
            ["COM0_.fits", "conin$_.x.fits", "LPT²_.fits"]
        );
        // Never empty.
        assert_eq!(names("", "m", &["Ha"], "fits"), ["export.fits"]);
        assert_eq!(
            names("{name}", "m", &["...", " "], "tif"),
            ["export.tif", "export.tif"]
        );
        // Other braces are kept as typed.
        assert_eq!(names("{x}{name}", "m", &["Ha"], "fits"), ["{x}Ha.fits"]);
    }

    #[test]
    fn saved_choices_load_leniently() {
        let folder = Path::new("/somewhere");
        let defaults = Choices {
            folder: "/somewhere".into(),
            format: ExportFormat::Fits,
            pattern: DEFAULT_PATTERN.into(),
            excluded: Vec::new(),
        };
        // Nothing saved, or not an object.
        assert_eq!(Choices::load(&json!({}), folder), defaults);
        assert_eq!(Choices::load(&json!({EXPORT_KEY: 3}), folder), defaults);
        // Each field by itself: a bad one is its default.
        let saved = json!({EXPORT_KEY: {
            "folder": 3,
            "format": "xisf",
            "pattern": " ",
            "excluded": [{"group": "Ha"}, {"bad": 1}, {"composite": 2}, "x"],
        }});
        assert_eq!(
            Choices::load(&saved, folder),
            Choices {
                excluded: vec![Pick::Group("Ha".into()), Pick::Composite(2)],
                ..defaults.clone()
            }
        );
        // The rows an earlier build saved as exported are not read: every
        // ready row starts ticked.
        let old = json!({EXPORT_KEY: {"selection": [{"group": "Ha"}]}});
        assert_eq!(Choices::load(&old, folder), defaults);
        // What is saved loads back.
        let chosen = Choices {
            folder: "/out".into(),
            format: ExportFormat::Tiff,
            pattern: "{name}".into(),
            excluded: vec![Pick::Composite(4), Pick::Group("OIII".into())],
        };
        let mut ui = json!({"other": 1});
        chosen.save(&mut ui);
        assert_eq!(
            ui[EXPORT_KEY],
            json!({
                "folder": "/out",
                "format": "tiff",
                "pattern": "{name}",
                "excluded": [{"composite": 4}, {"group": "OIII"}],
            })
        );
        assert_eq!(ui["other"], 1, "the rest of ui is kept");
        assert_eq!(Choices::load(&ui, folder), chosen);
        // A ui that is no object becomes one.
        let mut ui = Value::Null;
        chosen.save(&mut ui);
        assert_eq!(Choices::load(&ui, folder), chosen);
    }

    #[test]
    fn a_folder_is_looked_at_on_its_own() {
        let root = crate::start::tests::temp_dir("export-look");
        std::fs::write(root.join("a.fits"), b"x").unwrap();
        let lookup = |folder: PathBuf| Lookup {
            folder,
            names: vec!["a.fits".into(), "b.fits".into()],
        };
        let found = look(&lookup(root.clone()));
        assert_eq!(found.problem, None);
        assert_eq!(found.existing, BTreeSet::from(["a.fits".to_string()]));
        let missing = look(&lookup(root.join("nope")));
        assert_eq!(missing.problem, Some(MISSING_FOLDER));
        assert!(missing.existing.is_empty());
        let relative = look(&lookup(PathBuf::from("out")));
        assert_eq!(relative.problem, Some(NOT_FULL_PATH));
        // Nothing is created there.
        let left: Vec<_> = std::fs::read_dir(&root).unwrap().collect();
        assert_eq!(left.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Unix's permission bits are believed; Windows' read-only mark on a
    /// folder never is (it marks Documents and customised folders so).
    #[test]
    fn a_read_only_folder_says_so_on_unix_only() {
        let root = crate::start::tests::temp_dir("export-read-only");
        let lookup = Lookup {
            folder: root.clone(),
            names: vec!["a.fits".into()],
        };
        let mut perms = std::fs::metadata(&root).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&root, perms.clone()).unwrap();
        let found = look(&lookup);
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(&root, perms).unwrap();
        let want = if cfg!(unix) {
            Some(UNWRITABLE_FOLDER)
        } else {
            None
        };
        assert_eq!(found.problem, want);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn paths_compare_as_the_file_system_does() {
        let a = Path::new("/Astro/Out/m.fits");
        assert_eq!(path_key(a), path_key(Path::new("/astro/out/M.FITS")));
        assert_eq!(path_key(a), path_key(Path::new("/Astro/Out/./m.fits")));
        assert_ne!(path_key(a), path_key(Path::new("/Astro/m.fits")));
        #[cfg(windows)]
        assert_eq!(
            path_key(Path::new(r"\\?\C:\Astro\m.fits")),
            path_key(Path::new("C:/Astro/m.fits"))
        );
    }
}
