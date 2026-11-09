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
//!    changed" never stops a row. On opening, the ready rows are ticked:
//!    all of them, or with a remembered selection, the remembered ones.
//! 3. **Folder**: typed, or picked with **Browse…** (a native folder dialog
//!    on a worker, [`FolderPicker`]).
//! 4. **Format**: FITS or TIFF, with a one-line note on what it holds.
//! 5. **File name pattern**: the tokens `{session}` (the session's name)
//!    and `{name}` (the group's or composite's); [`file_names`] makes the
//!    names safe for any file system.
//! 6. **The preview**: the exact file names, one per ticked row. Which of
//!    them exist already is looked up on a worker, again whenever the
//!    folder, pattern, format or ticked rows change (one lookup at a time).
//!    The folder's writability is tried once per folder, with a file
//!    created and removed at once, and not while the folder field has the
//!    keyboard (not in every folder on the way to the one being typed).
//!    Until the lookup answers the names show unmarked and Export waits
//!    ("Checking the folder…"). An existing file
//!    is marked "exists"; two rows that would write the same name
//!    (ignoring case, as Windows and macOS do) are marked "same name" and
//!    the collision message says how to fix it.
//! 7. **The overwrite line**, when files exist: "{n} files already exist
//!    and will be replaced".
//! 8. **The footer**: **Cancel**, and **Export**, which becomes **Overwrite
//!    and export** (Danger) while a ticked file exists. Export is disabled
//!    while nothing is ticked, the folder is empty, missing, not a full
//!    path or not writable, two names collide, the folder is being looked
//!    at, or an analysis runs; the reason is its tooltip, and shows in the
//!    body too (a disabled button takes no keyboard focus).
//!
//! **Export** remembers the folder, format, pattern and the rows exported
//! in the session's `ui` under [`EXPORT_KEY`] (saved with the window's
//! next save), starts one export job per ticked row
//! ([`JobRunner::start_export`]) and closes. **Cancel**, Esc and a click on
//! the backdrop close it, keeping nothing of what changed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat};
use egui::{Align, Layout, TextStyle};
use mega_ui::theme::Tone;
use mega_ui::theme::text::{Type, text};
use mega_ui::theme::tokens::{CONTROL_HEIGHT, DIALOG_WIDTH, space};
use mega_ui::widgets::{
    ButtonKind, Dialog, FooterButton, MegaButton, Segment, footer_with, segmented_with, text_input,
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
/// "fits" | "tiff", pattern, selection: [ {group: name} | {composite: id} ] }`.
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
/// A folder where no file can be created.
const UNWRITABLE_FOLDER: &str = "The folder can't be written to";
/// Two ticked rows would write the same file.
const COLLISION: &str = "Two exports would have the same file name: add {name} to the pattern";
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

/// A row of the dialog, as remembered in the selection: a group by name or
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
    /// The rows exported last; `None` before any export (every ready row
    /// starts ticked then).
    pub selection: Option<Vec<Pick>>,
}

impl Choices {
    /// The choices saved in `ui` (the session's UI settings). Each field is
    /// read by itself: one missing or malformed (a blank folder or pattern,
    /// a format other than "fits" or "tiff") is its default: `folder`
    /// (where the session folder is; written without Windows' verbatim
    /// prefix, [`plain_path`]), FITS, [`DEFAULT_PATTERN`], no selection. A
    /// malformed entry of the selection is skipped.
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
        let selection = saved
            .and_then(|s| s.get("selection"))
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|v| serde_json::from_value::<Pick>(v.clone()).ok())
                    .collect()
            });
        Choices {
            folder: text("folder").unwrap_or_else(|| plain_path(folder)),
            format,
            pattern: text("pattern").unwrap_or_else(|| DEFAULT_PATTERN.into()),
            selection,
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
        if let Some(selection) = &self.selection
            && let Ok(v) = serde_json::to_value(selection)
        {
            saved.insert("selection".into(), v);
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
    /// Why the folder cannot take the files, if it is not a full path or
    /// not there.
    problem: Option<&'static str>,
    /// The names that exist there already.
    existing: BTreeSet<String>,
    /// Whether a file could be created there, if that was tried.
    writable: Option<bool>,
}

/// Look in `lookup`'s folder (on a worker): is it there (and, with
/// `try_write`, can a file be created in it), and which of the names
/// exist.
fn look(lookup: &Lookup, try_write: bool) -> Found {
    let folder = &lookup.folder;
    let problem = if !folder.is_absolute() {
        Some(NOT_FULL_PATH)
    } else if !folder.is_dir() {
        Some(MISSING_FOLDER)
    } else {
        None
    };
    if problem.is_some() {
        return Found {
            problem,
            existing: BTreeSet::new(),
            writable: None,
        };
    }
    Found {
        problem,
        existing: lookup
            .names
            .iter()
            .filter(|n| folder.join(n).exists())
            .cloned()
            .collect(),
        writable: try_write.then(|| writable(folder)),
    }
}

/// Whether a file can be created in `folder`: one is, and removed at once.
fn writable(folder: &Path) -> bool {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let probe = folder.join(format!(
        ".mmm-export-check-{}-{nanos}.tmp",
        std::process::id()
    ));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(file) => {
            drop(file);
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(e) => e.kind() == std::io::ErrorKind::AlreadyExists,
    }
}

/// Holds folder lookups back until its sender is dropped (tests).
#[cfg(test)]
type CheckHold = Arc<std::sync::Mutex<std::sync::mpsc::Receiver<()>>>;

/// The dialog while open: the choices being made, and the lookups.
struct Choosing {
    ticked: BTreeSet<Pick>,
    folder: String,
    format: ExportFormat,
    pattern: String,
    /// The folder dialog, while shown.
    picking: Option<Task<Option<PathBuf>>>,
    /// The folder lookup running, and what it looks at.
    looking: Option<(Lookup, Task<Found>)>,
    /// The last lookup's answer, and what it looked at.
    found: Option<(Lookup, Found)>,
    /// What the choices ask to look at, as last drawn.
    wanted: Option<Lookup>,
    /// The folder field has the keyboard: what it holds is being typed, so
    /// its writability is not tried yet (not in every folder on the way).
    editing_folder: bool,
    /// The last folder whose writability a lookup was asked to try.
    write_tried: Option<PathBuf>,
    /// The last folder whose writability is known, and whether a file
    /// could be created there.
    writable: Option<(PathBuf, bool)>,
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
    #[cfg(test)]
    hold: Option<CheckHold>,
}

impl ExportDialog {
    /// A closed dialog whose **Browse…** asks `picker`.
    pub fn new(picker: Arc<dyn FolderPicker>) -> Self {
        ExportDialog {
            open: None,
            picker,
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

    /// Open the dialog on `session`'s remembered choices (else the
    /// defaults: beside the session folder, FITS, [`DEFAULT_PATTERN`]), its
    /// ready rows ticked (with a remembered selection, those of it). No
    /// I/O. Nothing if it is open already.
    pub fn open(&mut self, session: &OpenSession, jobs: &JobRunner) {
        if self.open.is_some() {
            return;
        }
        let dir = &session.mosaic.dir;
        let choices = Choices::load(&session.mosaic.config.ui, dir.parent().unwrap_or(dir));
        let ticked = rows(session, jobs)
            .into_iter()
            .filter(|r| r.tickable())
            .filter(|r| {
                choices
                    .selection
                    .as_ref()
                    .is_none_or(|chosen| chosen.contains(&r.pick))
            })
            .map(|r| r.pick)
            .collect();
        self.open = Some(Choosing {
            ticked,
            folder: choices.folder,
            format: choices.format,
            pattern: choices.pattern,
            picking: None,
            looking: None,
            found: None,
            wanted: None,
            editing_folder: false,
            write_tried: None,
            writable: None,
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
        c.poll();
        let rows = rows(session, jobs);
        let analysing = jobs.analyzing();
        let session_name = session.name();
        let shown = Shown::of(c, &rows, &session_name);
        let response = Dialog {
            title: "Export",
            width: DIALOG_WIDTH,
        }
        .show(ctx, egui::Id::new("export-dialog"), |ui| {
            body(ui, c, &rows, &shown, analysing)
        });
        let act = if response.close {
            Some(Act::Cancel)
        } else {
            response.inner
        };
        match act {
            Some(Act::Cancel) => {
                self.open = None;
                return false;
            }
            Some(Act::Browse) => {
                let picker = self.picker.clone();
                let start = PathBuf::from(c.folder.trim());
                c.picking = Task::spawn("mmm-export-folder", ctx, move || {
                    picker.pick_folder(Some(&start))
                });
            }
            Some(Act::Export) => {
                let changed = c.export(session, jobs, &rows, &shown);
                self.open = None;
                return changed;
            }
            None => {}
        }
        // Look again at what the choices are now (after this frame's
        // clicks and typing).
        let wanted = Shown::of(c, &rows, &session_name).lookup;
        #[cfg(test)]
        let hold = self.hold.clone();
        // The folder's writability is tried once per folder, once it is
        // no longer being typed.
        let try_write =
            |lookup: &Lookup| !c.editing_folder && c.write_tried.as_ref() != Some(&lookup.folder);
        if let Some(lookup) = &wanted
            && c.looking.is_none()
            && (c.found.as_ref().is_none_or(|(l, _)| l != lookup) || try_write(lookup))
        {
            let try_write = try_write(lookup);
            if try_write {
                c.write_tried = Some(lookup.folder.clone());
            }
            let job = lookup.clone();
            let task = Task::spawn("mmm-export-lookup", ctx, move || {
                #[cfg(test)]
                if let Some(hold) = hold {
                    let _ = hold
                        .lock()
                        .map(|rx| rx.recv_timeout(crate::testutil::PATIENCE));
                }
                look(&job, try_write)
            });
            c.looking = task.map(|t| (lookup.clone(), t));
        }
        c.wanted = wanted;
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
    /// the choices ask now, its writability tried unless it is being typed
    /// (tests).
    #[cfg(test)]
    pub(crate) fn settled(&self) -> bool {
        self.open.as_ref().is_some_and(|c| {
            c.picking.is_none()
                && c.looking.is_none()
                && c.wanted.as_ref().is_none_or(|w| {
                    c.found.as_ref().is_some_and(|(l, _)| l == w)
                        && (c.editing_folder || c.write_tried.as_ref() == Some(&w.folder))
                })
        })
    }

    /// The last folder whose writability a lookup tried (tests).
    #[cfg(test)]
    pub(crate) fn write_tried(&self) -> Option<&Path> {
        self.open.as_ref()?.write_tried.as_deref()
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
    /// Take the folder dialog's and the lookup's answers.
    fn poll(&mut self) {
        if let Some(task) = &self.picking {
            match task.poll() {
                Poll::Pending => {}
                Poll::Done(picked) => {
                    self.picking = None;
                    if let Some(p) = picked {
                        self.folder = plain_path(&p);
                    }
                }
                Poll::Lost => self.picking = None,
            }
        }
        if let Some((lookup, task)) = &self.looking {
            match task.poll() {
                Poll::Pending => {}
                Poll::Done(found) => {
                    if let Some(w) = found.writable {
                        self.writable = Some((lookup.folder.clone(), w));
                    }
                    self.found = Some((lookup.clone(), found));
                    self.looking = None;
                }
                Poll::Lost => {
                    // Unknown: never taken for an answer; looked up again.
                    self.found = None;
                    self.looking = None;
                }
            }
        }
    }

    /// **Export**: remember the choices in `session`'s `ui`, then start one
    /// export job per ticked row. Whether `ui` changed (it did).
    fn export(
        &self,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
        rows: &[Row],
        shown: &Shown,
    ) -> bool {
        let folder = PathBuf::from(self.folder.trim());
        let choices = Choices {
            folder: self.folder.trim().to_string(),
            format: self.format,
            pattern: self.pattern.clone(),
            selection: Some(shown.chosen.iter().map(|&i| rows[i].pick.clone()).collect()),
        };
        choices.save(&mut session.mosaic.config.ui);
        for (&i, name) in shown.chosen.iter().zip(&shown.names) {
            let target = rows[i].target.clone();
            jobs.start_export(session, target, folder.join(name), self.format);
        }
        true
    }
}

/// What the dialog shows for its choices this frame: the rows exported,
/// their file names, the lookup they need, and what is known of them.
struct Shown {
    /// Indices into the rows of those ticked and tickable, in order.
    chosen: Vec<usize>,
    /// Their file names.
    names: Vec<String>,
    /// What to look up for them (`None` without a folder).
    lookup: Option<Lookup>,
    /// The names (lowercase) more than one row would write.
    colliding: BTreeSet<String>,
    /// The lookup's answer for exactly these choices, once known.
    found: Option<Found>,
    /// The folder is being looked at: no answer yet for these choices, or
    /// another lookup (its writability) still runs.
    checking: bool,
    /// Why the folder cannot take the files, as known: none chosen, not a
    /// full path, not there, or no file can be created in it.
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
        let colliding = counts
            .into_iter()
            .filter(|(_, k)| *k > 1)
            .map(|(n, _)| n)
            .collect();
        let folder = c.folder.trim();
        let lookup = (!folder.is_empty()).then(|| Lookup {
            folder: PathBuf::from(folder),
            names: names.clone(),
        });
        let found = c
            .found
            .as_ref()
            .filter(|(l, _)| Some(l) == lookup.as_ref())
            .map(|(_, f)| f.clone());
        let checking = lookup.is_some() && (found.is_none() || c.looking.is_some());
        let folder_problem = match &lookup {
            None => Some(NO_FOLDER),
            Some(l) => found.as_ref().and_then(|f| f.problem).or_else(|| {
                c.writable
                    .as_ref()
                    .is_some_and(|(f, w)| *f == l.folder && !w)
                    .then_some(UNWRITABLE_FOLDER)
            }),
        };
        Shown {
            chosen,
            names,
            lookup,
            colliding,
            found,
            checking,
            folder_problem,
        }
    }

    /// The names that exist already, as known.
    fn existing(&self) -> usize {
        self.found.as_ref().map_or(0, |f| f.existing.len())
    }

    /// Why Export cannot be pressed, if it cannot.
    fn block(&self, analysing: bool) -> Option<&'static str> {
        if analysing {
            Some(ANALYSING)
        } else if self.chosen.is_empty() {
            Some(NOTHING_TICKED)
        } else if self.lookup.is_none() {
            Some(NO_FOLDER)
        } else if !self.colliding.is_empty() {
            Some(COLLISION)
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
            rows_ui(ui, rows, &mut c.ticked);
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
fn rows_ui(ui: &mut egui::Ui, rows: &[Row], ticked: &mut BTreeSet<Pick>) {
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
            row_ui(ui, r, ticked);
        }
    }
}

/// A row: its checkbox (disabled unless it can be ticked) and, when it
/// cannot, its reason as text, also its checkbox's description.
fn row_ui(ui: &mut egui::Ui, row: &Row, ticked: &mut BTreeSet<Pick>) {
    ui.horizontal(|ui| {
        let tickable = row.tickable();
        let mut on = tickable && ticked.contains(&row.pick);
        let r = ui.add_enabled(tickable, egui::Checkbox::new(&mut on, row.name()));
        if let Some(why) = row.reason() {
            ui.ctx()
                .accesskit_node_builder(r.id, |node| node.set_description(why.as_str()));
            ui.label(text(Type::Small, why));
        }
        if r.changed() && tickable {
            if on {
                ticked.insert(row.pick.clone());
            } else {
                ticked.remove(&row.pick);
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
                let r = text_input(ui, &mut c.folder, "Where the files go").labelled_by(label);
                c.editing_folder = r.has_focus();
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
/// there is none; then the collision message and the lookup's progress.
fn preview_ui(ui: &mut egui::Ui, shown: &Shown) {
    if shown.chosen.is_empty() {
        ui.label(text(Type::Small, NOTHING_TICKED));
        return;
    }
    let existing = shown.found.as_ref().map(|f| &f.existing);
    for name in &shown.names {
        let mut marks = Vec::new();
        if shown.colliding.contains(&name.to_lowercase()) {
            marks.push((SAME_NAME, Tone::Problem));
        }
        if existing.is_some_and(|e| e.contains(name)) {
            marks.push((EXISTS, Tone::Warning));
        }
        ui.label(file_line(ui, name, &marks));
    }
    if !shown.colliding.is_empty() {
        ui.label(text(Type::Small, COLLISION).color(Tone::Problem.color()));
    } else if shown.checking {
        ui.label(text(Type::Small, CHECKING));
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
            selection: None,
        };
        // Nothing saved, or not an object.
        assert_eq!(Choices::load(&json!({}), folder), defaults);
        assert_eq!(Choices::load(&json!({EXPORT_KEY: 3}), folder), defaults);
        // Each field by itself: a bad one is its default.
        let saved = json!({EXPORT_KEY: {
            "folder": 3,
            "format": "xisf",
            "pattern": " ",
            "selection": [{"group": "Ha"}, {"bad": 1}, {"composite": 2}, "x"],
        }});
        assert_eq!(
            Choices::load(&saved, folder),
            Choices {
                selection: Some(vec![Pick::Group("Ha".into()), Pick::Composite(2)]),
                ..defaults.clone()
            }
        );
        // What is saved loads back.
        let chosen = Choices {
            folder: "/out".into(),
            format: ExportFormat::Tiff,
            pattern: "{name}".into(),
            selection: Some(vec![Pick::Composite(4), Pick::Group("OIII".into())]),
        };
        let mut ui = json!({"other": 1});
        chosen.save(&mut ui);
        assert_eq!(
            ui[EXPORT_KEY],
            json!({
                "folder": "/out",
                "format": "tiff",
                "pattern": "{name}",
                "selection": [{"composite": 4}, {"group": "OIII"}],
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
        let found = look(&lookup(root.clone()), true);
        assert_eq!(found.problem, None);
        assert_eq!(found.existing, BTreeSet::from(["a.fits".to_string()]));
        assert_eq!(found.writable, Some(true));
        // Writability is tried only when asked.
        assert_eq!(look(&lookup(root.clone()), false).writable, None);
        let missing = look(&lookup(root.join("nope")), true);
        assert_eq!(missing.problem, Some(MISSING_FOLDER));
        assert!(missing.existing.is_empty());
        assert_eq!(missing.writable, None);
        let relative = look(&lookup(PathBuf::from("out")), true);
        assert_eq!(relative.problem, Some(NOT_FULL_PATH));
        // The writability check leaves nothing behind.
        let left: Vec<_> = std::fs::read_dir(&root).unwrap().collect();
        assert_eq!(left.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }
}
