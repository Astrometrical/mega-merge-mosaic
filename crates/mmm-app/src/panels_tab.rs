//! The panel list (spec §5, §10; drawn by the session window in place of
//! its canvas while open): choosing the panel files and grouping them.
//! File cards, and with grouping on, groups as columns of cards with
//! "No group" first; never a spreadsheet.
//!
//! # Threads
//!
//! The UI thread reads no file. The file dialog, the expansion of a typed
//! path or pattern, and the panel headers (dimensions, channels, the
//! grouping rule's keyword, the checks' facts) all run on background
//! threads ([`Task`]); the cards and columns show the last known facts and
//! groups meanwhile. Each panel's header is read once per keyword and
//! cached, so regrouping after an edit is cheap.
//!
//! # Edits
//!
//! Every edit (add, remove, move, rule) builds a new panel list and
//! grouping, a [`PanelsEdit`], and [`PanelsEdit::apply`]s it with
//! [`set_inputs`]. While a job runs (it may hold a result store) the edit
//! cancels every job and waits; [`PanelsTab::poll`] cancels any job
//! started meanwhile too and applies the edit once the runner is idle, and
//! a newer edit replaces a waiting one (newer work wins). Picked, dropped
//! and typed paths are normalised on a worker before the duplicate check.
//!
//! Group colours ([`group_colors`]) are assigned when groups resolve and
//! kept in `MosaicConfig.ui` ([`GROUP_COLORS_KEY`]); they are saved with the
//! next `mosaic.json` save. The tab shows the waiting edit as if applied. `set_inputs` saves
//! `mosaic.json` and invalidates the stores' state tables on the UI thread:
//! the documented exception to "no file I/O on the UI thread".

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::text::{LayoutJob, TextWrapping};
use egui::{Color32, FontId, RichText, Sense, Stroke, StrokeKind, Vec2};
use mega_ui::groups::physical_color;
pub use mega_ui::groups::{GROUP_PALETTE, PHYSICAL_SLOTS};
use mega_ui::theme::palette;
use mmm_core::checks::{CheckCode, PanelFacts, input_checks, panel_facts};
use mmm_core::formats::InputPanel;
use mmm_core::mosaic::grouping::{Groups, header_value, resolve_groups};
use mmm_core::mosaic::{GroupRule, Grouping, MosaicConfig, PanelEntry, UNGROUPED, set_inputs};
use serde_json::Value;

use crate::jobs::JobRunner;
use crate::session::{OpenSession, normalize};
use crate::util::{Poll, Task};

/// File extensions accepted as panels (the dialog filter, drops and
/// patterns), compared case-insensitively.
pub const PANEL_EXTENSIONS: [&str; 4] = ["xisf", "fits", "fit", "fts"];
/// Header keyword of a new header rule.
const DEFAULT_HEADER_KEY: &str = "FILTER";
/// Keyword of a new filename-token rule (`xxx_filter_Ha_yyy` → `Ha`).
const DEFAULT_TOKEN_KEYWORD: &str = "filter";
/// Wait before retrying an edit refused because a store was busy.
const BUSY_RETRY: Duration = Duration::from_millis(500);
/// Size of a file card.
const CARD_SIZE: Vec2 = Vec2::new(236.0, 52.0);
/// Width of a group column (card plus padding).
const COLUMN_WIDTH: f32 = 260.0;

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
/// view showing groups (Panels, Plan, Blend).
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
fn is_panel_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| PANEL_EXTENSIONS.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// The panel files `text` names: one path, or a pattern with one `*` in
/// its file name (`dir/*.xisf`) matched against the files of that one
/// directory, ASCII case-insensitively and sorted. Paths are made absolute.
/// Blocking (file I/O): call off the UI thread. Errors are for the user.
fn expand_typed(text: &str) -> Result<Vec<PathBuf>, String> {
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
    fn matches(&self, config: &MosaicConfig) -> bool {
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

/// What the headers say about one panel (cached by path).
#[derive(Clone, Debug, Default)]
struct PanelInfo {
    /// `[width, height, channels]`, or why the file could not be read;
    /// `None` until read.
    dims: Option<Result<[u64; 3], String>>,
    /// Facts for the input checks, when readable.
    facts: Option<PanelFacts>,
    /// Header values by upper-case keyword (`None`: absent or unreadable).
    headers: BTreeMap<String, Option<String>>,
}

/// A background resolution's outcome.
struct Resolved {
    /// The inputs resolved.
    inputs: PanelsEdit,
    /// What is known about their panels.
    infos: BTreeMap<PathBuf, PanelInfo>,
    groups: Groups,
    /// The "filters differ but grouping is off" check fired.
    filters_differ: bool,
}

/// Read what `infos` lacks for `inputs` (the headers of new panels, and
/// the rule's keyword), then resolve the groups and the input checks.
/// Blocking (header reads): runs on a worker.
fn resolve(inputs: PanelsEdit, mut infos: BTreeMap<PathBuf, PanelInfo>) -> Resolved {
    let key = match (&inputs.grouping.enabled, &inputs.grouping.rule) {
        (true, GroupRule::Header { key }) if !key.is_empty() => Some(key.to_ascii_uppercase()),
        _ => None,
    };
    for p in &inputs.panels {
        let info = infos.entry(p.path.clone()).or_default();
        let fresh = info.dims.is_none();
        let need_key = key.as_ref().is_some_and(|k| !info.headers.contains_key(k));
        if !fresh && !need_key {
            continue;
        }
        let keys: Vec<String> = fresh
            .then(|| DEFAULT_HEADER_KEY.to_string())
            .into_iter()
            .chain(key.clone())
            .collect();
        match InputPanel::open(&p.path) {
            Ok(panel) => {
                for k in keys {
                    let v = header_value(&panel, &k);
                    info.headers.insert(k, v);
                }
                if fresh {
                    info.dims = Some(Ok([panel.width(), panel.height(), panel.channels()]));
                    info.facts = panel_facts(&p.path).ok();
                }
            }
            Err(e) => {
                for k in keys {
                    info.headers.insert(k, None);
                }
                if fresh {
                    info.dims = Some(Err(e.to_string()));
                }
            }
        }
    }
    let mut cfg = MosaicConfig::new(Vec::new());
    cfg.panels = inputs.panels.clone();
    cfg.grouping = inputs.grouping.clone();
    let groups = resolve_groups(&cfg, &|path| {
        let k = key.as_ref()?;
        infos.get(path)?.headers.get(k).cloned().flatten()
    });
    let facts: Vec<PanelFacts> = inputs
        .panels
        .iter()
        .filter_map(|p| infos.get(&p.path)?.facts.clone())
        .collect();
    let filters_differ = input_checks(&cfg, &facts, &groups)
        .iter()
        .any(|c| c.code == CheckCode::FiltersDifferUngrouped);
    Resolved {
        inputs,
        infos,
        groups,
        filters_differ,
    }
}

/// The cards of a grouped view: the "No group" column, then each named
/// group in name order.
#[derive(Debug, Default, PartialEq)]
struct Columns {
    /// Panels no rule placed (or placed in [`UNGROUPED`] by hand), and
    /// those whose group is not resolved yet.
    ungrouped: Vec<PathBuf>,
    /// Named groups and their panels.
    groups: BTreeMap<String, Vec<PathBuf>>,
    /// Panels whose group is still being resolved.
    unsorted: BTreeSet<PathBuf>,
}

/// Place `shown`'s panels in columns: a manual group as set; otherwise the
/// group of the last resolution (`resolved`), when it used the same
/// grouping and placed the panel by the rule.
fn columns(shown: &PanelsEdit, resolved: Option<(&PanelsEdit, &Groups)>) -> Columns {
    let mut out = Columns::default();
    let mut placed: BTreeMap<&Path, Option<&str>> = BTreeMap::new();
    if let Some((inputs, groups)) = resolved
        && inputs.grouping == shown.grouping
    {
        let by_rule: BTreeSet<&Path> = inputs
            .panels
            .iter()
            .filter(|p| p.manual_group.is_none())
            .map(|p| p.path.as_path())
            .collect();
        for (name, paths) in &groups.groups {
            for p in paths.iter().filter(|p| by_rule.contains(p.as_path())) {
                placed.insert(p, Some(name));
            }
        }
        for p in groups
            .ungrouped
            .iter()
            .filter(|p| by_rule.contains(p.as_path()))
        {
            placed.insert(p, None);
        }
    }
    for p in &shown.panels {
        let group = match &p.manual_group {
            Some(g) => Some(g.as_str()),
            None => match placed.get(p.path.as_path()) {
                Some(g) => *g,
                None => {
                    out.unsorted.insert(p.path.clone());
                    None
                }
            },
        };
        match group {
            Some(g) if g != UNGROUPED => out
                .groups
                .entry(g.to_string())
                .or_default()
                .push(p.path.clone()),
            _ => out.ungrouped.push(p.path.clone()),
        }
    }
    out
}

/// The named groups of `groups` in the order their first panel appears in
/// `inputs` ([`UNGROUPED`] left out).
fn first_seen<'a>(inputs: &PanelsEdit, groups: &'a Groups) -> Vec<&'a str> {
    let by_path: BTreeMap<&Path, &str> = groups
        .groups
        .iter()
        .flat_map(|(n, ps)| ps.iter().map(move |p| (p.as_path(), n.as_str())))
        .collect();
    let mut seen = BTreeSet::new();
    inputs
        .panels
        .iter()
        .filter_map(|p| by_path.get(p.path.as_path()).copied())
        .filter(|n| *n != UNGROUPED && seen.insert(*n))
        .collect()
}

/// The file name of `path`, for cards.
fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// The modifiers held when the pointer was last released this frame (the
/// click's), else the current ones.
fn click_modifiers(ui: &egui::Ui) -> egui::Modifiers {
    ui.input(|i| {
        i.events
            .iter()
            .rev()
            .find_map(|e| match e {
                egui::Event::PointerButton {
                    pressed: false,
                    modifiers,
                    ..
                } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or(i.modifiers)
    })
}

/// A selectable file card: name, a status line and, when grouped, the
/// group's colour. Painted (not laid out from labels) so its one
/// accessible node is the file name.
fn card(
    ui: &mut egui::Ui,
    path: &Path,
    status: (&str, Color32),
    color: Option<Color32>,
    selected: bool,
) -> egui::Response {
    let name = file_name(path);
    let (rect, response) = ui.allocate_exact_size(CARD_SIZE, Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &name)
    });
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let fill = if selected {
            palette::ACCENT_600.gamma_multiply(0.35)
        } else if response.hovered() {
            palette::BORDER
        } else {
            palette::SURFACE_700
        };
        let stroke = if selected {
            Stroke::new(2.0, palette::ACCENT_400)
        } else {
            Stroke::new(1.0, palette::BORDER)
        };
        painter.rect(rect, 6.0, fill, stroke, StrokeKind::Inside);
        let mut left = rect.left() + 12.0;
        if let Some(c) = color {
            painter.circle_filled(egui::pos2(left + 3.0, rect.center().y), 4.0, c);
            left += 14.0;
        }
        let width = rect.right() - 10.0 - left;
        let line = |text: &str, size: f32, color: Color32| {
            let mut job =
                LayoutJob::simple_singleline(text.to_string(), FontId::proportional(size), color);
            job.wrap = TextWrapping::truncate_at_width(width);
            ui.fonts_mut(|f| f.layout_job(job))
        };
        let title = line(&name, 14.0, palette::TEXT_STRONG);
        let sub = line(status.0, 12.0, status.1);
        painter.galley(
            egui::pos2(left, rect.top() + 8.0),
            title,
            palette::TEXT_STRONG,
        );
        painter.galley(egui::pos2(left, rect.top() + 29.0), sub, status.1);
    }
    response.on_hover_text(path.display().to_string())
}

/// What the action bar asked for.
enum Action {
    /// Set (`Some`) or clear (`None`: the rule decides) the selected
    /// panels' manual group.
    MoveTo(Option<String>),
    /// Prompt for a new group's name.
    NewGroup,
    /// Remove the selected panels.
    Remove,
    /// Clear the selection.
    Clear,
}

/// The Panels tab's state for the open session.
pub struct PanelsTab {
    picker: Arc<dyn FilesPicker>,
    /// The file dialog, while open (its paths come back normalised).
    picking: Option<Task<Option<Vec<PathBuf>>>>,
    /// Dropped files being normalised.
    normalizing: Vec<Task<Vec<PathBuf>>>,
    /// The typed-path field.
    path_text: String,
    /// A typed path being expanded.
    expanding: Option<Task<Result<Vec<PathBuf>, String>>>,
    /// The rule keyword being typed (committed on Enter or focus lost).
    rule_text: Option<String>,
    /// The last header key and token keyword used, for switching rules.
    header_key: String,
    token_keyword: String,
    selection: BTreeSet<PathBuf>,
    /// Where a Shift-click range starts.
    anchor: Option<PathBuf>,
    /// The cards in display order (last frame), for ranges.
    order: Vec<PathBuf>,
    /// The new group's name while prompting.
    new_group: Option<String>,
    /// An edit waiting for the jobs to stop (or a busy store).
    pending: Option<PanelsEdit>,
    /// When to retry a pending edit refused as busy.
    retry_at: Option<Instant>,
    /// The last error, for the user.
    error: Option<String>,
    /// Header facts by panel path.
    infos: BTreeMap<PathBuf, PanelInfo>,
    /// The background resolution, while running.
    resolving: Option<Task<Resolved>>,
    /// The inputs of the last resolution started.
    attempted: Option<PanelsEdit>,
    /// The last resolution's inputs and groups.
    resolved: Option<(PanelsEdit, Groups)>,
    /// The last resolution found different filters with grouping off.
    filters_differ: bool,
}

impl PanelsTab {
    /// A tab adding files with `picker`.
    pub fn new(picker: Arc<dyn FilesPicker>) -> Self {
        PanelsTab {
            picker,
            picking: None,
            normalizing: Vec::new(),
            path_text: String::new(),
            expanding: None,
            rule_text: None,
            header_key: DEFAULT_HEADER_KEY.into(),
            token_keyword: DEFAULT_TOKEN_KEYWORD.into(),
            selection: BTreeSet::new(),
            anchor: None,
            order: Vec::new(),
            new_group: None,
            pending: None,
            retry_at: None,
            error: None,
            infos: BTreeMap::new(),
            resolving: None,
            attempted: None,
            resolved: None,
            filters_differ: false,
        }
    }

    /// An edit is waiting for the jobs to stop (or a busy store); the
    /// automatic pipeline starts nothing meanwhile.
    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// The inputs as shown: the waiting edit, else the session's.
    fn shown(&self, session: &OpenSession) -> PanelsEdit {
        self.pending
            .clone()
            .unwrap_or_else(|| PanelsEdit::of(session))
    }

    /// Apply `edit`, or keep it waiting (replacing any waiting one).
    fn submit(&mut self, edit: PanelsEdit, session: &mut OpenSession, jobs: &mut JobRunner) {
        self.pending = None;
        self.retry_at = None;
        self.error = None;
        match edit.apply(session, jobs) {
            Applied::Done => {}
            Applied::Queued(edit) => self.pending = Some(edit),
            Applied::Busy(edit) => {
                self.pending = Some(edit);
                self.retry_at = Some(Instant::now() + BUSY_RETRY);
            }
            Applied::Failed(message) => {
                tracing::warn!("{message}");
                self.error = Some(message);
            }
        }
    }

    /// Add `paths` not already in (duplicates ignored).
    fn add_paths(&mut self, paths: Vec<PathBuf>, session: &mut OpenSession, jobs: &mut JobRunner) {
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

    /// Take finished background work (dialog, typed path, dropped files,
    /// header reads), apply a waiting edit once the runner is idle, and
    /// start resolving the shown inputs if they changed. Call every frame,
    /// after the runner's poll, whether or not the list is shown (files
    /// dropped on the window are added whatever shows).
    pub fn poll(&mut self, ctx: &egui::Context, session: &mut OpenSession, jobs: &mut JobRunner) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| is_panel_file(p))
                .collect()
        });
        if !dropped.is_empty() {
            // Normalised (symlinks resolved) off the UI thread before the
            // duplicate check, as picked and typed paths are.
            let task = Task::spawn("mmm-drop-paths", ctx, move || {
                dropped.iter().map(|p| normalize(p)).collect()
            });
            self.normalizing.extend(task);
        }
        let mut normalized = Vec::new();
        self.normalizing.retain(|task| match task.poll() {
            Poll::Pending => true,
            Poll::Done(paths) => {
                normalized.push(paths);
                false
            }
            Poll::Lost => false,
        });
        for paths in normalized {
            self.add_paths(paths, session, jobs);
        }
        if let Some(task) = &self.picking {
            match task.poll() {
                Poll::Pending => {}
                Poll::Done(picked) => {
                    self.picking = None;
                    if let Some(paths) = picked {
                        self.add_paths(paths, session, jobs);
                    }
                }
                Poll::Lost => self.picking = None,
            }
        }
        if let Some(task) = &self.expanding {
            match task.poll() {
                Poll::Pending => {}
                Poll::Done(expanded) => {
                    self.expanding = None;
                    match expanded {
                        Ok(paths) => {
                            self.path_text.clear();
                            self.add_paths(paths, session, jobs);
                        }
                        Err(message) => self.error = Some(message),
                    }
                }
                Poll::Lost => self.expanding = None,
            }
        }
        if self.pending.is_some() && jobs.busy_except_exports() {
            // Newer work wins, also over jobs started after the edit was
            // queued (cancelling again is harmless); exports are spared.
            jobs.cancel_all_but_exports();
        } else if self.pending.is_some() {
            let now = Instant::now();
            match self.retry_at {
                Some(at) if at > now => ctx.request_repaint_after(at - now),
                _ => {
                    let edit = self.pending.take().expect("pending edit");
                    self.submit(edit, session, jobs);
                }
            }
        }
        self.poll_resolution(ctx, session);
    }

    /// Take a finished resolution (refreshing the session's cached groups
    /// when it is for the applied inputs) and start one for the shown
    /// inputs if they changed.
    fn poll_resolution(&mut self, ctx: &egui::Context, session: &mut OpenSession) {
        if let Some(task) = &self.resolving {
            match task.poll() {
                Poll::Pending => {}
                Poll::Done(r) => {
                    self.resolving = None;
                    self.infos.extend(r.infos);
                    self.filters_differ = r.filters_differ;
                    if r.inputs.grouping.enabled {
                        // Saved with the next edit or small save (the
                        // UI-thread exception); kept in memory until then.
                        let ui = &mut session.mosaic.config.ui;
                        let mut assigned = load_group_colors(ui);
                        if assign_group_colors(&mut assigned, first_seen(&r.inputs, &r.groups)) {
                            save_group_colors(ui, &assigned);
                        }
                    }
                    self.resolved = Some((r.inputs, r.groups));
                }
                Poll::Lost => {
                    // Try again on the next poll rather than leave cards
                    // "Sorting…" for good.
                    self.resolving = None;
                    self.attempted = None;
                    tracing::warn!("reading the panel headers stopped unexpectedly; retrying");
                    ctx.request_repaint();
                }
            }
        }
        if let Some((inputs, groups)) = &self.resolved
            && inputs.matches(&session.mosaic.config)
            && session.groups.as_ref() != Some(groups)
        {
            session.groups = Some(groups.clone());
        }
        let shown = self.shown(session);
        if self.resolving.is_none() && self.attempted.as_ref() != Some(&shown) {
            let infos: BTreeMap<PathBuf, PanelInfo> = shown
                .panels
                .iter()
                .filter_map(|p| Some((p.path.clone(), self.infos.get(&p.path)?.clone())))
                .collect();
            self.attempted = Some(shown.clone());
            self.resolving = Task::spawn("mmm-panels-resolve", ctx, move || resolve(shown, infos));
        }
    }

    /// Open the file dialog (on a worker; [`PanelsTab::poll`] adds what
    /// was picked), unless it is open already.
    pub(crate) fn pick_files(&mut self, ctx: &egui::Context) {
        if self.picking.is_some() {
            return;
        }
        let picker = self.picker.clone();
        self.picking = Task::spawn("mmm-pick-files", ctx, move || {
            let picked = picker.pick_files()?;
            Some(picked.iter().map(|p| normalize(p)).collect())
        });
    }

    /// The status line of `path`'s card.
    fn card_status(&self, path: &Path) -> (String, Color32) {
        match self.infos.get(path).and_then(|i| i.dims.as_ref()) {
            None => ("Reading header…".into(), palette::TEXT_SUBTLE),
            Some(Ok([w, h, c])) => {
                let ch = if *c == 1 {
                    "1 channel".to_string()
                } else {
                    format!("{c} channels")
                };
                (format!("{w} × {h} · {ch}"), palette::TEXT_BODY)
            }
            Some(Err(e)) => (format!("Unreadable: {e}"), palette::HA_400),
        }
    }

    /// A click on `path`'s card: select it alone; Ctrl/Cmd toggles it;
    /// Shift selects the range from the last clicked card.
    fn click(&mut self, path: &Path, mods: egui::Modifiers) {
        let at = |p: &Path| self.order.iter().position(|o| o == p);
        if mods.shift
            && let Some(a) = self.anchor.as_deref().and_then(at)
            && let Some(b) = at(path)
        {
            let (lo, hi) = (a.min(b), a.max(b));
            if !mods.command {
                self.selection.clear();
            }
            self.selection.extend(self.order[lo..=hi].iter().cloned());
            return;
        }
        if mods.command {
            if !self.selection.remove(path) {
                self.selection.insert(path.to_path_buf());
            }
        } else {
            self.selection = BTreeSet::from([path.to_path_buf()]);
        }
        self.anchor = Some(path.to_path_buf());
    }

    /// Draw the tab.
    pub fn ui(&mut self, ui: &mut egui::Ui, session: &mut OpenSession, jobs: &mut JobRunner) {
        let ctx = ui.ctx().clone();
        let shown = self.shown(session);
        let present: BTreeSet<&Path> = shown.panels.iter().map(|p| p.path.as_path()).collect();
        self.selection.retain(|p| present.contains(p.as_path()));
        let grouped = shown.grouping.enabled;
        let cols = grouped.then(|| columns(&shown, self.resolved.as_ref().map(|(i, g)| (i, g))));

        let assigned = load_group_colors(&session.mosaic.config.ui);
        let mut edit: Option<PanelsEdit> = None;
        egui::Panel::top("panels-toolbar").show(ui, |ui| {
            ui.add_space(4.0);
            self.toolbar(ui, &ctx, &shown, &mut edit);
            if let Some(e) = self.notices(ui, &shown) {
                edit = Some(e);
            }
            if !self.selection.is_empty() {
                let names: Vec<String> = cols
                    .as_ref()
                    .map(|c| c.groups.keys().cloned().collect())
                    .unwrap_or_default();
                if let Some(action) = self.action_bar(ui, grouped, &names) {
                    edit = self.act(action, &shown);
                }
            }
            ui.add_space(4.0);
        });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(palette::SURFACE_900)
                    .inner_margin(12.0),
            )
            .show(ui, |ui| {
                if shown.panels.is_empty() {
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            RichText::new(
                                "No panels yet. Add files…, drop panel files on the window, \
                                 or type a path above.",
                            )
                            .color(palette::TEXT_MUTED),
                        );
                    });
                    return;
                }
                match &cols {
                    Some(cols) => self.columns_ui(ui, cols, &assigned),
                    None => self.grid_ui(ui, &shown),
                }
            });
        if let Some(e) = self.new_group_prompt(&ctx, &shown) {
            edit = Some(e);
        }
        let typing = ctx.memory(|m| m.focused().is_some());
        if self.new_group.is_none() && !typing && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.selection.clear();
        }
        if let Some(e) = edit {
            self.submit(e, session, jobs);
        }
        self.drop_overlay(&ctx);
    }

    /// Add files, the typed path, and the grouping switch and rule.
    fn toolbar(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        shown: &PanelsEdit,
        edit: &mut Option<PanelsEdit>,
    ) {
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.picking.is_none(), egui::Button::new("Add files…"))
                .on_hover_text("Choose XISF or FITS panel files")
                .clicked()
            {
                self.pick_files(ctx);
            }
            let label = ui.label(RichText::new("Add path").color(palette::TEXT_MUTED));
            let field = ui
                .add(
                    egui::TextEdit::singleline(&mut self.path_text)
                        .hint_text("a file, or folder/*.xisf")
                        .desired_width(300.0),
                )
                .labelled_by(label.id);
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let can_add = self.expanding.is_none() && !self.path_text.trim().is_empty();
            let add = ui.add_enabled(can_add, egui::Button::new("Add")).clicked();
            if can_add && (enter || add) {
                let text = self.path_text.clone();
                self.expanding = Task::spawn("mmm-expand-path", ctx, move || expand_typed(&text));
            }
            if self.picking.is_some() || self.expanding.is_some() || !self.normalizing.is_empty() {
                ui.spinner();
            }
            ui.separator();
            let mut on = shown.grouping.enabled;
            if ui
                .checkbox(&mut on, "Grouping")
                .on_hover_text("Split the panels into groups, e.g. one per filter")
                .changed()
            {
                *edit = Some(PanelsEdit {
                    panels: shown.panels.clone(),
                    grouping: Grouping {
                        enabled: on,
                        rule: shown.grouping.rule.clone(),
                    },
                });
            }
            if on && let Some(rule) = self.rule_ui(ui, &shown.grouping.rule) {
                *edit = Some(PanelsEdit {
                    panels: shown.panels.clone(),
                    grouping: Grouping {
                        enabled: true,
                        rule,
                    },
                });
            }
        });
    }

    /// The rule picker; a new rule once the user commits one.
    fn rule_ui(&mut self, ui: &mut egui::Ui, rule: &GroupRule) -> Option<GroupRule> {
        let header = matches!(rule, GroupRule::Header { .. });
        let mut choose_header = header;
        egui::ComboBox::from_id_salt("panels-rule")
            .selected_text(if header {
                "Header keyword"
            } else {
                "Filename token"
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut choose_header, true, "Header keyword");
                ui.selectable_value(&mut choose_header, false, "Filename token");
            });
        if choose_header != header {
            self.rule_text = None;
            return Some(if choose_header {
                GroupRule::Header {
                    key: self.header_key.clone(),
                }
            } else {
                GroupRule::FilenameToken {
                    keyword: self.token_keyword.clone(),
                }
            });
        }
        let (current, field_label, hint) = match rule {
            GroupRule::Header { key } => (key, "Header key", "FILTER"),
            GroupRule::FilenameToken { keyword } => (keyword, "Token keyword", "filter"),
        };
        let label = ui.label(RichText::new(field_label).color(palette::TEXT_MUTED));
        let mut text = self.rule_text.clone().unwrap_or_else(|| current.clone());
        let field = ui
            .add(
                egui::TextEdit::singleline(&mut text)
                    .hint_text(hint)
                    .desired_width(120.0),
            )
            .labelled_by(label.id);
        if let GroupRule::FilenameToken { .. } = rule {
            ui.label(
                RichText::new(format!("…_{}_Ha_… → Ha", text.trim()))
                    .small()
                    .color(palette::TEXT_SUBTLE),
            );
        }
        if field.lost_focus() {
            self.rule_text = None;
            // Esc abandons the typing; Enter or clicking away commits.
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                return None;
            }
            let text = text.trim().to_string();
            if text != *current {
                return Some(match rule {
                    GroupRule::Header { .. } => {
                        self.header_key = text.clone();
                        GroupRule::Header { key: text }
                    }
                    GroupRule::FilenameToken { .. } => {
                        self.token_keyword = text.clone();
                        GroupRule::FilenameToken { keyword: text }
                    }
                });
            }
        } else if field.has_focus() {
            self.rule_text = Some(text);
        }
        None
    }

    /// Waiting edit, errors, and the grouping suggestion (grouping turned
    /// on when taken).
    fn notices(&mut self, ui: &mut egui::Ui, shown: &PanelsEdit) -> Option<PanelsEdit> {
        let mut edit = None;
        if self.pending.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                let text = if self.retry_at.is_some() {
                    "A result is in use by another program; the change applies when it is free…"
                } else {
                    "Waiting for the running job to stop; the change applies then…"
                };
                ui.label(RichText::new(text).color(palette::TEXT_BODY));
            });
        }
        if let Some(e) = self.error.clone() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(e).color(palette::HA_400));
                if ui.small_button("Dismiss").clicked() {
                    self.error = None;
                }
            });
        }
        let suggest = !shown.grouping.enabled
            && self.filters_differ
            && self.resolved.as_ref().is_some_and(|(i, _)| i == shown);
        if suggest {
            egui::Frame::new()
                .fill(palette::SURFACE_700)
                .stroke(Stroke::new(1.0, palette::ACCENT_600))
                .corner_radius(6.0)
                .inner_margin(8.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(
                                "These panels use different filters. Group them to blend each \
                                 filter separately.",
                            )
                            .color(palette::TEXT_STRONG),
                        );
                        if ui.button("Turn grouping on").clicked() {
                            edit = Some(PanelsEdit {
                                panels: shown.panels.clone(),
                                grouping: Grouping {
                                    enabled: true,
                                    rule: shown.grouping.rule.clone(),
                                },
                            });
                        }
                    });
                });
        }
        edit
    }

    /// The actions on the selection.
    fn action_bar(
        &mut self,
        ui: &mut egui::Ui,
        grouped: bool,
        groups: &[String],
    ) -> Option<Action> {
        let mut action = None;
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("{} selected", self.selection.len()))
                    .color(palette::ACCENT_400),
            );
            if grouped {
                ui.menu_button("Move to ▾", |ui| {
                    for g in groups {
                        if ui.button(g).clicked() {
                            action = Some(Action::MoveTo(Some(g.clone())));
                        }
                    }
                    if !groups.is_empty() {
                        ui.separator();
                    }
                    if ui.button(UNGROUPED).clicked() {
                        action = Some(Action::MoveTo(Some(UNGROUPED.to_string())));
                    }
                    if ui
                        .button("Let the rule decide")
                        .on_hover_text("Undo manual placement")
                        .clicked()
                    {
                        action = Some(Action::MoveTo(None));
                    }
                });
                if ui.button("Add to new group…").clicked() {
                    action = Some(Action::NewGroup);
                }
            }
            if ui.button("Remove").clicked() {
                action = Some(Action::Remove);
            }
            if ui.button("Clear selection").clicked() {
                action = Some(Action::Clear);
            }
        });
        action
    }

    /// Carry out `action` on the selection: the edit it makes, if any.
    fn act(&mut self, action: Action, shown: &PanelsEdit) -> Option<PanelsEdit> {
        let mut edit = shown.clone();
        match action {
            Action::MoveTo(group) => {
                for p in edit
                    .panels
                    .iter_mut()
                    .filter(|p| self.selection.contains(&p.path))
                {
                    p.manual_group = group.clone();
                }
            }
            Action::NewGroup => {
                self.new_group = Some(String::new());
                return None;
            }
            Action::Remove => {
                edit.panels.retain(|p| !self.selection.contains(&p.path));
                self.selection.clear();
                self.anchor = None;
            }
            Action::Clear => {
                self.selection.clear();
                return None;
            }
        }
        Some(edit)
    }

    /// The "Add to new group…" name prompt: the edit, once confirmed.
    fn new_group_prompt(&mut self, ctx: &egui::Context, shown: &PanelsEdit) -> Option<PanelsEdit> {
        let mut name = self.new_group.take()?;
        let mut open = true;
        let mut confirmed = false;
        let mut cancelled = false;
        egui::Window::new("Add to new group")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                let label = ui.label("Group name");
                let field = ui
                    .add(egui::TextEdit::singleline(&mut name).hint_text("e.g. Ha"))
                    .labelled_by(label.id);
                let valid = !name.trim().is_empty();
                let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.horizontal(|ui| {
                    confirmed = ui.add_enabled(valid, egui::Button::new("Create")).clicked()
                        || (valid && enter);
                    cancelled = ui.button("Cancel").clicked();
                });
            });
        if confirmed {
            return self.act(Action::MoveTo(Some(name.trim().to_string())), shown);
        }
        if open && !cancelled {
            self.new_group = Some(name);
        }
        None
    }

    /// The plain grid of cards (grouping off).
    fn grid_ui(&mut self, ui: &mut egui::Ui, shown: &PanelsEdit) {
        self.order = shown.panels.iter().map(|p| p.path.clone()).collect();
        let mut clicked = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(10.0, 10.0);
                ui.horizontal_wrapped(|ui| {
                    for path in &self.order {
                        let (text, color) = self.card_status(path);
                        let selected = self.selection.contains(path);
                        if card(ui, path, (&text, color), None, selected).clicked() {
                            clicked = Some((path.clone(), click_modifiers(ui)));
                        }
                    }
                });
            });
        if let Some((path, mods)) = clicked {
            self.click(&path, mods);
        }
    }

    /// Group columns: "No group" first, then each group in name order.
    fn columns_ui(
        &mut self,
        ui: &mut egui::Ui,
        cols: &Columns,
        assigned: &BTreeMap<String, usize>,
    ) {
        let colors = group_colors(assigned, cols.groups.keys().map(String::as_str));
        self.order = cols
            .ungrouped
            .iter()
            .chain(cols.groups.values().flatten())
            .cloned()
            .collect();
        let mut clicked = None;
        egui::ScrollArea::horizontal()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    if !cols.ungrouped.is_empty() {
                        let header = |ui: &mut egui::Ui| {
                            ui.label(
                                RichText::new(UNGROUPED)
                                    .strong()
                                    .color(palette::TEXT_STRONG),
                            );
                            ui.label(
                                RichText::new(format!("{} to sort", cols.ungrouped.len()))
                                    .color(palette::ACCENT_400),
                            );
                        };
                        if let Some(c) = self.column(
                            ui,
                            UNGROUPED,
                            header,
                            &cols.ungrouped,
                            None,
                            &cols.unsorted,
                        ) {
                            clicked = Some(c);
                        }
                    }
                    for (name, paths) in &cols.groups {
                        let color = colors.get(name).copied().unwrap_or(palette::TEXT_MUTED);
                        let header = |ui: &mut egui::Ui| {
                            let (dot, _) =
                                ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
                            ui.painter().circle_filled(dot.center(), 5.0, color);
                            ui.label(RichText::new(name).strong().color(palette::TEXT_STRONG));
                            ui.label(
                                RichText::new(paths.len().to_string()).color(palette::TEXT_MUTED),
                            );
                        };
                        if let Some(c) =
                            self.column(ui, name, header, paths, Some(color), &cols.unsorted)
                        {
                            clicked = Some(c);
                        }
                    }
                });
            });
        if let Some((path, mods)) = clicked {
            self.click(&path, mods);
        }
    }

    /// One group column of cards; the clicked card and its modifiers.
    fn column(
        &self,
        ui: &mut egui::Ui,
        id: &str,
        header: impl FnOnce(&mut egui::Ui),
        paths: &[PathBuf],
        color: Option<Color32>,
        unsorted: &BTreeSet<PathBuf>,
    ) -> Option<(PathBuf, egui::Modifiers)> {
        let mut clicked = None;
        let height = ui.available_height();
        ui.allocate_ui_with_layout(
            egui::vec2(COLUMN_WIDTH, height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                egui::Frame::new()
                    .fill(palette::SURFACE_800)
                    .stroke(Stroke::new(1.0, palette::BORDER))
                    .corner_radius(8.0)
                    .inner_margin(10.0)
                    .show(ui, |ui| {
                        ui.set_width(COLUMN_WIDTH - 22.0);
                        ui.set_min_height(height - 22.0);
                        ui.horizontal(header);
                        ui.add_space(6.0);
                        egui::ScrollArea::vertical()
                            .id_salt(("panels-column", id))
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.y = 8.0;
                                for path in paths {
                                    let (text, tone) = if unsorted.contains(path) {
                                        ("Sorting…".to_string(), palette::TEXT_SUBTLE)
                                    } else {
                                        self.card_status(path)
                                    };
                                    let selected = self.selection.contains(path);
                                    if card(ui, path, (&text, tone), color, selected).clicked() {
                                        clicked = Some((path.clone(), click_modifiers(ui)));
                                    }
                                }
                            });
                    });
            },
        );
        clicked
    }

    /// Show where files dragged over the window will go (drawn by
    /// [`PanelsTab::ui`]; call it while the tab is not drawn: the drop is
    /// taken whatever shows).
    pub(crate) fn drop_overlay(&self, ctx: &egui::Context) {
        if ctx.input(|i| i.raw.hovered_files.is_empty()) {
            return;
        }
        let screen = ctx.content_rect();
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("panels-drop"),
        ));
        painter.rect_filled(screen, 0.0, Color32::from_black_alpha(180));
        painter.text(
            screen.center(),
            egui::Align2::CENTER_CENTER,
            "Drop panel files to add them",
            FontId::proportional(22.0),
            palette::ACCENT_400,
        );
    }
}

#[cfg(test)]
impl PanelsTab {
    /// Nothing waiting: no edit pending and the groups resolved for the
    /// session's inputs.
    fn settled(&self, session: &OpenSession) -> bool {
        self.pending.is_none() && self.resolved_current(session)
    }

    /// The last resolution is for the session's inputs.
    fn resolved_current(&self, session: &OpenSession) -> bool {
        self.resolving.is_none()
            && self
                .resolved
                .as_ref()
                .is_some_and(|(i, _)| i.matches(&session.mosaic.config))
    }

    /// Files are being picked or a typed path expanded.
    fn adding(&self) -> bool {
        self.picking.is_some() || self.expanding.is_some() || !self.normalizing.is_empty()
    }

    fn filters_differ(&self) -> bool {
        self.filters_differ
    }

    fn card_text(&self, path: &Path) -> String {
        self.card_status(path).0
    }

    fn selected(&self) -> &BTreeSet<PathBuf> {
        &self.selection
    }

    fn pending(&self) -> Option<&PanelsEdit> {
        self.pending.as_ref()
    }
}

#[cfg(test)]
#[path = "panels_tab_tests.rs"]
mod tests;
