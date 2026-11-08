//! The panel list (spec §10.2): the session's panel files as a sectioned
//! table, drawn by the session window in a column at the left of the
//! canvas while open, so the plan stays beside it.
//!
//! - **Toolbar.** **Add files…** (the file dialog), **Add folder…**, a
//!   More menu (**Type a path…**: a file, or `folder/*.xisf`) and **Group
//!   by** with the rule's keyword. Files dropped anywhere on the window are
//!   added whatever shows ([`PanelList::poll`]).
//! - **Table** ([`sectioned_table`]): columns **PANEL**, **FILTER**,
//!   **INTEGRATION**, **SCALE** and **SIZE** from the panel headers ("—"
//!   when unknown; a file that can't be opened reads "Unreadable" with the
//!   error as its problem). A panel with an unaccepted problem in the checks
//!   report carries a ⚠ with the problem's text.
//! - **Sections.** With grouping on: "No group" first (never rolled up; its
//!   heading says why the rule placed nothing), then each group in name
//!   order, headed "{group} · {n}" (" · {total integration}" when every
//!   member's is known) in the group's colour, with "{d} fewer than
//!   {largest group}" when it has fewer panels than the largest. Group
//!   sections roll up to their first rows. With grouping off: one flat
//!   section.
//! - **Rows** carry an actions menu: **Remove** (a toast offers Undo).
//!
//! # Threads
//!
//! The UI thread reads no file. The file dialog, the expansion of a typed
//! path, the normalising of dropped paths and the panel headers all run on
//! background threads ([`Task`]); the table shows the last known facts and
//! groups meanwhile. Each panel's header is read once
//! ([`read_panel_header`]) and cached by path and file fingerprint, so
//! regrouping after an edit reads nothing new; a header rule on a keyword
//! other than FILTER reads that keyword once more per panel.
//!
//! Edits go through [`PanelEdits`] (queued behind running jobs; see
//! [`crate::panel_edits`]).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{Align, Color32, Label, Layout, RichText, Stroke, TextWrapMode};
use mega_ui::icons;
use mega_ui::table::{Column, Heading, Row, Section, TableEvent, TableState, sectioned_table};
use mega_ui::theme::text::{Type, text};
use mega_ui::theme::tokens::{CONTROL_HEIGHT, KEYWORD_INPUT_WIDTH, radius, space};
use mega_ui::theme::{Tone, palette};
use mega_ui::widgets::{ButtonKind, MegaButton, Toast, icon_button_of, text_input};
use mmm_core::checks::{CheckCode, PanelFacts, Severity, input_checks, is_accepted};
use mmm_core::formats::InputPanel;
use mmm_core::mosaic::grouping::{Groups, header_value, resolve_groups};
use mmm_core::mosaic::{Fingerprint, GroupRule, Grouping, MosaicConfig, UNGROUPED, fingerprint};
use mmm_core::panel_header::{PanelHeader, read_panel_header};

use crate::checks::ChecksReport;
use crate::jobs::JobRunner;
use crate::panel_edits::{
    FilesPicker, PanelEdits, PanelsEdit, assign_group_colors, expand_typed, group_colors,
    is_panel_file, load_group_colors, save_group_colors,
};
use crate::session::{OpenSession, normalize};
use crate::session_window::{Undo, WindowToast};
use crate::util::{Poll, Task};

/// Header keyword of a new header rule.
const DEFAULT_HEADER_KEY: &str = "FILTER";
/// Keyword of a new filename-token rule (`xxx_filter_Ha_yyy` → `Ha`).
const DEFAULT_TOKEN_KEYWORD: &str = "filter";
/// A cell whose fact is unknown.
const UNKNOWN: &str = "—";
/// The FILTER cell of a panel whose header is not read yet.
const READING: &str = "Reading…";
/// The FILTER cell of a panel that can't be opened.
const UNREADABLE: &str = "Unreadable";

/// The table's columns.
const COLUMNS: [Column<'static>; 5] = [
    Column {
        title: "PANEL",
        numeric: false,
    },
    Column {
        title: "FILTER",
        numeric: false,
    },
    Column {
        title: "INTEGRATION",
        numeric: true,
    },
    Column {
        title: "SCALE",
        numeric: true,
    },
    Column {
        title: "SIZE",
        numeric: true,
    },
];

/// What the list reports after a frame.
#[derive(Debug, Default)]
pub struct PanelListOutput {
    /// The panel whose row the pointer is over.
    // Read once rows and the plan are linked (the next step of the list).
    #[allow(dead_code)]
    pub hovered: Option<PathBuf>,
    /// The panel whose row was clicked.
    #[allow(dead_code)]
    pub clicked: Option<PathBuf>,
    /// Toasts to show (an edit refused, a removal to undo).
    pub toasts: Vec<WindowToast>,
}

/// What the headers say about one panel (cached by path).
#[derive(Clone, Debug)]
struct PanelInfo {
    /// The file's fingerprint when read (`None`: it could not be read).
    fingerprint: Option<Fingerprint>,
    /// The header facts, or why the file could not be opened.
    header: Result<PanelHeader, String>,
    /// Values of the header keywords read for a rule other than FILTER, by
    /// upper-case keyword (`None`: absent or unreadable).
    keys: BTreeMap<String, Option<String>>,
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

/// The upper-case header keyword `grouping` groups by, if it is an enabled
/// header rule.
fn rule_key(grouping: &Grouping) -> Option<String> {
    match (&grouping.enabled, &grouping.rule) {
        (true, GroupRule::Header { key }) if !key.trim().is_empty() => {
            Some(key.trim().to_ascii_uppercase())
        }
        _ => None,
    }
}

/// Read what `infos` lacks for `inputs` (the header of each panel new or
/// changed on disk since read, and the rule's keyword), then resolve the
/// groups and the input checks. Blocking (header reads): runs on a worker.
fn resolve(inputs: PanelsEdit, mut infos: BTreeMap<PathBuf, PanelInfo>) -> Resolved {
    let key = rule_key(&inputs.grouping);
    // FILTER is among the header facts; another keyword is read on its own.
    let extra = key.clone().filter(|k| k != DEFAULT_HEADER_KEY);
    for p in &inputs.panels {
        let fingerprint = fingerprint(&p.path).ok();
        let fresh = infos
            .get(&p.path)
            .is_none_or(|i| i.fingerprint != fingerprint);
        if fresh {
            let header = read_panel_header(&p.path).map_err(|e| e.to_string());
            let info = PanelInfo {
                fingerprint,
                header,
                keys: BTreeMap::new(),
            };
            infos.insert(p.path.clone(), info);
        }
        let Some(info) = infos.get_mut(&p.path) else {
            continue;
        };
        if let Some(k) = &extra
            && !info.keys.contains_key(k)
        {
            let value = match &info.header {
                Ok(_) => InputPanel::open(&p.path)
                    .ok()
                    .and_then(|panel| header_value(&panel, k)),
                Err(_) => None,
            };
            info.keys.insert(k.clone(), value);
        }
    }
    let mut cfg = MosaicConfig::new(Vec::new());
    cfg.panels = inputs.panels.clone();
    cfg.grouping = inputs.grouping.clone();
    let groups = resolve_groups(&cfg, &|path| {
        let k = key.as_ref()?;
        let info = infos.get(path)?;
        if *k == DEFAULT_HEADER_KEY {
            info.header.as_ref().ok()?.filter.clone()
        } else {
            info.keys.get(k).cloned().flatten()
        }
    });
    let facts: Vec<PanelFacts> = inputs
        .panels
        .iter()
        .filter_map(|p| {
            let h = infos.get(&p.path)?.header.as_ref().ok()?;
            Some(PanelFacts {
                path: p.path.clone(),
                channels: h.channels,
                filter: h.filter.clone(),
                solution: h.solution,
                scale_arcsec: h.scale_arcsec,
            })
        })
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

/// Where the panels of a grouped list go: "No group", then each named
/// group in name order.
#[derive(Debug, Default, PartialEq)]
struct Placement {
    /// Panels no rule placed (or placed in [`UNGROUPED`] by hand), and
    /// those whose group is not resolved yet.
    ungrouped: Vec<PathBuf>,
    /// Named groups and their panels.
    groups: BTreeMap<String, Vec<PathBuf>>,
    /// Panels whose group is still being resolved.
    unsorted: BTreeSet<PathBuf>,
}

/// Place `shown`'s panels: a manual group as set; otherwise the group of
/// the last resolution (`resolved`), when it used the same grouping and
/// placed the panel by the rule.
fn place(shown: &PanelsEdit, resolved: Option<(&PanelsEdit, &Groups)>) -> Placement {
    let mut out = Placement::default();
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

/// The table id of `path`'s row: from the path, so a row keeps its id (and
/// with it its focus, menu and selection) as other panels come and go.
fn row_id(path: &Path) -> u64 {
    egui::Id::new(("panel-row", path)).value()
}

/// The panel of `shown` whose row is `id`.
fn panel_of(shown: &PanelsEdit, id: u64) -> Option<&Path> {
    shown
        .panels
        .iter()
        .map(|p| p.path.as_path())
        .find(|p| row_id(p) == id)
}

/// The file name of `path`, for rows.
fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// An integration time: "3h 05m", or "45s" below a minute.
fn integration_text(seconds: f64) -> String {
    let whole = seconds.max(0.0).round() as u64;
    if whole < 60 {
        return format!("{whole}s");
    }
    let minutes = (seconds / 60.0).round() as u64;
    format!("{}h {:02}m", minutes / 60, minutes % 60)
}

/// A pixel scale: "1.21″".
fn scale_text(arcsec: f64) -> String {
    format!("{arcsec:.2}″")
}

/// Why `rule` placed a panel in no group: the "No group" heading's
/// warning.
fn no_group_reason(rule: &GroupRule) -> String {
    match rule {
        GroupRule::Header { key } if key.trim().is_empty() => "no header keyword chosen".into(),
        GroupRule::Header { key } => format!("{} missing", key.trim().to_ascii_uppercase()),
        GroupRule::FilenameToken { keyword } => format!("no {keyword} in the file name"),
        GroupRule::PathKeyword { keyword } => format!("no {keyword} in the path"),
        GroupRule::Manual => "not placed by hand".into(),
    }
}

/// "Removed 1 panel" / "Removed {n} panels".
fn removed_title(n: usize) -> String {
    if n == 1 {
        "Removed 1 panel".into()
    } else {
        format!("Removed {n} panels")
    }
}

/// A message for the user that something went wrong.
fn problem_toast(message: String) -> WindowToast {
    WindowToast {
        toast: Toast {
            tone: Tone::Problem,
            title: message,
            detail: None,
            action: None,
        },
        undo: None,
    }
}

/// The text of each panel's unaccepted problems in `report` (message, then
/// remedy), by panel.
fn panel_problems(
    report: Option<&ChecksReport>,
    cfg: &MosaicConfig,
) -> BTreeMap<PathBuf, Vec<String>> {
    let mut out: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    let Some(report) = report else {
        return out;
    };
    let problems = report
        .checks
        .iter()
        .filter(|c| c.severity == Severity::Problem && !is_accepted(cfg, c));
    for c in problems {
        let text = match &c.remedy {
            Some(remedy) => format!("{}\n{remedy}", c.message),
            None => c.message.clone(),
        };
        for p in &c.panels {
            out.entry(p.clone()).or_default().push(text.clone());
        }
    }
    out
}

/// One table row, owned (the table borrows it).
struct RowData {
    id: u64,
    cells: Vec<String>,
    problem: Option<String>,
}

/// One section heading, owned.
struct HeadingData {
    id: String,
    title: String,
    color: Option<Color32>,
    warning: Option<String>,
    rolls_up: bool,
}

/// One table section, owned.
struct SectionData {
    heading: Option<HeadingData>,
    rows: Vec<RowData>,
}

impl SectionData {
    /// The section for [`sectioned_table`], borrowing this one.
    fn section(&self) -> Section<'_> {
        Section {
            heading: self.heading.as_ref().map(|h| Heading {
                id: &h.id,
                title: h.title.clone(),
                color: h.color,
                warning: h.warning.clone(),
                rolls_up: h.rolls_up,
                drop_target: false,
            }),
            rows: self
                .rows
                .iter()
                .map(|r| Row {
                    id: r.id,
                    cells: r.cells.clone(),
                    problem: r.problem.as_deref(),
                    tag: None,
                    selected: false,
                    highlighted: false,
                })
                .collect(),
        }
    }
}

/// The **Group by** choices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GroupBy {
    Off,
    Header,
    Token,
    Path,
    Manual,
}

impl GroupBy {
    /// The choice `grouping` is.
    fn of(grouping: &Grouping) -> Self {
        if !grouping.enabled {
            return GroupBy::Off;
        }
        match grouping.rule {
            GroupRule::Header { .. } => GroupBy::Header,
            GroupRule::FilenameToken { .. } => GroupBy::Token,
            GroupRule::PathKeyword { .. } => GroupBy::Path,
            GroupRule::Manual => GroupBy::Manual,
        }
    }

    fn label(self) -> &'static str {
        match self {
            GroupBy::Off => "Don't group",
            GroupBy::Header => "Header keyword…",
            GroupBy::Token => "Filename token…",
            GroupBy::Path => "Path keyword…",
            GroupBy::Manual => "By hand",
        }
    }
}

/// The panel list's state for the open session.
pub struct PanelList {
    picker: Arc<dyn FilesPicker>,
    /// The file dialog, while open (its paths come back normalised).
    picking: Option<Task<Option<Vec<PathBuf>>>>,
    /// Dropped files being normalised.
    normalizing: Vec<Task<Vec<PathBuf>>>,
    /// The typed-path row shows (opened from More → Type a path…).
    typing: bool,
    /// Focus the typed-path field on the next frame.
    focus_typed: bool,
    /// The typed-path field.
    path_text: String,
    /// A typed path being expanded.
    expanding: Option<Task<Result<Vec<PathBuf>, String>>>,
    /// The rule keyword being typed (committed on Enter or focus lost).
    rule_text: Option<String>,
    /// The last header key and token keyword used, for switching rules.
    header_key: String,
    token_keyword: String,
    /// Edits on their way into the session.
    edits: PanelEdits,
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
    /// The table's folded and opened sections.
    table: TableState,
    /// Toasts raised and not yet handed to the window.
    toasts: Vec<WindowToast>,
}

impl PanelList {
    /// A list adding files with `picker`.
    pub fn new(picker: Arc<dyn FilesPicker>) -> Self {
        PanelList {
            picker,
            picking: None,
            normalizing: Vec::new(),
            typing: false,
            focus_typed: false,
            path_text: String::new(),
            expanding: None,
            rule_text: None,
            header_key: DEFAULT_HEADER_KEY.into(),
            token_keyword: DEFAULT_TOKEN_KEYWORD.into(),
            edits: PanelEdits::default(),
            infos: BTreeMap::new(),
            resolving: None,
            attempted: None,
            resolved: None,
            filters_differ: false,
            table: TableState::default(),
            toasts: Vec::new(),
        }
    }

    /// An edit is waiting for the jobs to stop (or a busy store); the
    /// automatic pipeline starts nothing meanwhile.
    pub fn has_pending(&self) -> bool {
        self.edits.has_pending()
    }

    /// Apply the waiting edit now (the session is being left); see
    /// [`PanelEdits::apply_waiting`].
    pub fn apply_waiting(&mut self, session: &mut OpenSession, jobs: &mut JobRunner) {
        self.edits.apply_waiting(session, jobs);
    }

    /// Apply `edit` from outside the list (an Undo toast): as any edit,
    /// queued behind running jobs.
    pub(crate) fn submit_edit(
        &mut self,
        edit: PanelsEdit,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
    ) {
        self.edits.submit(edit, session, jobs);
        self.collect_errors();
    }

    /// The toasts raised since last asked (an edit refused while the list
    /// is closed); [`PanelList::ui`] hands them over while it is open.
    pub fn take_toasts(&mut self) -> Vec<WindowToast> {
        self.collect_errors();
        std::mem::take(&mut self.toasts)
    }

    /// Turn refused edits into toasts.
    fn collect_errors(&mut self) {
        let errors = self.edits.take_errors();
        self.toasts.extend(errors.into_iter().map(problem_toast));
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
            self.edits.add_paths(paths, session, jobs);
        }
        if let Some(task) = &self.picking {
            match task.poll() {
                Poll::Pending => {}
                Poll::Done(picked) => {
                    self.picking = None;
                    if let Some(paths) = picked {
                        self.edits.add_paths(paths, session, jobs);
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
                            self.edits.add_paths(paths, session, jobs);
                        }
                        Err(message) => self.toasts.push(problem_toast(message)),
                    }
                }
                Poll::Lost => self.expanding = None,
            }
        }
        self.edits.poll(ctx, session, jobs);
        self.collect_errors();
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
                    // Try again on the next poll rather than leave rows
                    // "Reading…" for good.
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
        let shown = self.edits.shown(session);
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

    /// Open the file dialog (on a worker; [`PanelList::poll`] adds what
    /// was picked), unless it is open already.
    pub fn pick_files(&mut self, ctx: &egui::Context) {
        if self.picking.is_some() {
            return;
        }
        let picker = self.picker.clone();
        self.picking = Task::spawn("mmm-pick-files", ctx, move || {
            let picked = picker.pick_files()?;
            Some(picked.iter().map(|p| normalize(p)).collect())
        });
    }

    /// Draw the list filling `ui`: the toolbar, notices, and the table of
    /// panels. `report` is the last checks run's, for the rows' problems.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
        report: Option<&ChecksReport>,
    ) -> PanelListOutput {
        let ctx = ui.ctx().clone();
        // Fill the column, whatever shows: a resizable panel keeps the width
        // its contents take, so a short hint would shrink it for good.
        ui.set_min_width(ui.available_width());
        let shown = self.edits.shown(session);
        let mut out = PanelListOutput::default();
        let mut edit: Option<PanelsEdit> = None;
        self.toolbar(ui, &ctx, &shown, &mut edit);
        if let Some(e) = self.notices(ui, &shown) {
            edit = Some(e);
        }
        self.typed_row(ui, &ctx);
        ui.add_space(space::S);
        if shown.panels.is_empty() {
            ui.label(text(
                Type::Body,
                "No panels yet: add files, or drop panel files anywhere on the window.",
            ));
        } else {
            let data = self.sections(&shown, session, report);
            let sections: Vec<Section<'_>> = data.iter().map(SectionData::section).collect();
            let mut remove: Option<u64> = None;
            let table = &mut self.table;
            let events = egui::ScrollArea::vertical()
                .id_salt("panel-list-rows")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    sectioned_table(ui, table, &COLUMNS, &sections, |ui, id| {
                        if ui.button("Remove").clicked() {
                            remove = Some(id);
                        }
                    })
                })
                .inner;
            let path = |id: u64| panel_of(&shown, id).map(|p| p.to_path_buf());
            for event in events {
                match event {
                    TableEvent::Hover(id) => out.hovered = path(id),
                    TableEvent::Click { id, .. } => out.clicked = path(id),
                    TableEvent::Drop { .. } => {}
                }
            }
            if let Some(id) = remove
                && let Some(e) = self.remove(&shown, id)
            {
                edit = Some(e);
            }
        }
        if let Some(e) = edit {
            self.edits.submit(e, session, jobs);
        }
        out.toasts = self.take_toasts();
        out
    }

    /// The edit removing row `id`'s panel from `shown`, with a toast that
    /// puts it back.
    fn remove(&mut self, shown: &PanelsEdit, id: u64) -> Option<PanelsEdit> {
        let at = shown.panels.iter().position(|p| row_id(&p.path) == id)?;
        let mut edit = shown.clone();
        edit.panels.remove(at);
        self.toasts.push(WindowToast {
            toast: Toast {
                tone: Tone::Neutral,
                title: removed_title(1),
                detail: None,
                action: Some("Undo".into()),
            },
            undo: Some(Undo::Panels(shown.clone())),
        });
        Some(edit)
    }

    /// Add files, Add folder, More (Type a path…), and Group by with the
    /// rule's keyword.
    fn toolbar(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        shown: &PanelsEdit,
        edit: &mut Option<PanelsEdit>,
    ) {
        // Two rows, each fitting the narrowest list: adding, then grouping.
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::S;
            let add = MegaButton::new(ButtonKind::Secondary, "Add files…")
                .hover_text("Choose XISF or FITS panel files");
            if ui.add_enabled(self.picking.is_none(), add).clicked() {
                self.pick_files(ctx);
            }
            // Folders come next; the button holds its place meanwhile.
            ui.add_enabled(false, MegaButton::new(ButtonKind::Secondary, "Add folder…"));
            let more = ui.add(icon_button_of(ButtonKind::Quiet, icons::ELLIPSIS, "More"));
            egui::Popup::menu(&more).show(|ui| {
                if ui.button("Type a path…").clicked() {
                    self.typing = true;
                    self.focus_typed = true;
                }
            });
            if self.picking.is_some() || self.expanding.is_some() || !self.normalizing.is_empty() {
                ui.spinner();
            }
        });
        ui.add_space(space::XS);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::S;
            if let Some(grouping) = self.group_by(ui, shown) {
                *edit = Some(PanelsEdit {
                    panels: shown.panels.clone(),
                    grouping,
                });
            }
        });
    }

    /// The **Group by** dropdown and, for a rule with one, its keyword
    /// field; the new grouping once the user commits one.
    fn group_by(&mut self, ui: &mut egui::Ui, shown: &PanelsEdit) -> Option<Grouping> {
        let current = GroupBy::of(&shown.grouping);
        let mut chosen = current;
        let label =
            ui.add(Label::new(text(Type::Small, "Group by")).wrap_mode(TextWrapMode::Extend));
        let combo = egui::ComboBox::from_id_salt("panel-list-group-by")
            .selected_text(current.label())
            .show_ui(ui, |ui| {
                for c in [GroupBy::Off, GroupBy::Header, GroupBy::Token] {
                    ui.selectable_value(&mut chosen, c, c.label());
                }
                // Path keywords and by hand are shown, not offered yet.
                if matches!(current, GroupBy::Path | GroupBy::Manual) {
                    ui.selectable_value(&mut chosen, current, current.label());
                }
            });
        let _ = combo.response.labelled_by(label.id);
        let rule = &shown.grouping.rule;
        if chosen != current {
            self.rule_text = None;
            let rule = match chosen {
                GroupBy::Off => rule.clone(),
                GroupBy::Header => match rule {
                    GroupRule::Header { .. } => rule.clone(),
                    _ => GroupRule::Header {
                        key: self.header_key.clone(),
                    },
                },
                GroupBy::Token => match rule {
                    GroupRule::FilenameToken { .. } => rule.clone(),
                    _ => GroupRule::FilenameToken {
                        keyword: self.token_keyword.clone(),
                    },
                },
                GroupBy::Path | GroupBy::Manual => return None,
            };
            return Some(Grouping {
                enabled: chosen != GroupBy::Off,
                rule,
            });
        }
        if !shown.grouping.enabled {
            return None;
        }
        self.rule_field(ui, rule).map(|rule| Grouping {
            enabled: true,
            rule,
        })
    }

    /// The rule's keyword field; the new rule once the user commits a new
    /// keyword (Enter or clicking away; Esc abandons the typing).
    fn rule_field(&mut self, ui: &mut egui::Ui, rule: &GroupRule) -> Option<GroupRule> {
        let (current, field_label, hint) = match rule {
            GroupRule::Header { key } => (key, "Header key", DEFAULT_HEADER_KEY),
            GroupRule::FilenameToken { keyword } => {
                (keyword, "Token keyword", DEFAULT_TOKEN_KEYWORD)
            }
            GroupRule::PathKeyword { keyword } => (keyword, "Path keyword", DEFAULT_HEADER_KEY),
            GroupRule::Manual => return None,
        };
        let label =
            ui.add(Label::new(text(Type::Small, field_label)).wrap_mode(TextWrapMode::Extend));
        // As wide as a keyword needs, or what the row has left (the field's
        // frame adds its margin).
        let width = (ui.available_width() - space::S).clamp(0.0, KEYWORD_INPUT_WIDTH);
        let mut typed = self.rule_text.clone().unwrap_or_else(|| current.clone());
        let field = ui
            .add(
                egui::TextEdit::singleline(&mut typed)
                    .hint_text(hint)
                    .desired_width(width),
            )
            .labelled_by(label.id);
        if field.lost_focus() {
            self.rule_text = None;
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                return None;
            }
            let typed = typed.trim().to_string();
            if typed == *current {
                return None;
            }
            return Some(match rule {
                GroupRule::Header { .. } => {
                    self.header_key = typed.clone();
                    GroupRule::Header { key: typed }
                }
                GroupRule::FilenameToken { .. } => {
                    self.token_keyword = typed.clone();
                    GroupRule::FilenameToken { keyword: typed }
                }
                GroupRule::PathKeyword { .. } => GroupRule::PathKeyword { keyword: typed },
                GroupRule::Manual => return None,
            });
        }
        if field.has_focus() {
            self.rule_text = Some(typed);
        }
        None
    }

    /// The waiting edit, and the grouping suggestion (grouping turned on
    /// when taken).
    fn notices(&mut self, ui: &mut egui::Ui, shown: &PanelsEdit) -> Option<PanelsEdit> {
        let mut edit = None;
        if self.edits.has_pending() {
            ui.horizontal(|ui| {
                ui.spinner();
                let waiting = if self.edits.waits_for_store() {
                    "A result is in use by another program; the change applies when it is free…"
                } else {
                    "Waiting for the running job to stop; the change applies then…"
                };
                ui.label(text(Type::Body, waiting));
            });
        }
        let suggest = !shown.grouping.enabled
            && self.filters_differ
            && self.resolved.as_ref().is_some_and(|(i, _)| i == shown);
        if suggest {
            ui.add_space(space::XS);
            egui::Frame::new()
                .fill(palette::SURFACE_700)
                .stroke(Stroke::new(1.0, palette::ACCENT_600))
                .corner_radius(radius::CONTROL)
                .inner_margin(space::S)
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(
                                "These panels use different filters. Group them to merge each \
                                 filter separately.",
                            )
                            .color(palette::TEXT_STRONG),
                        );
                        if ui
                            .add(MegaButton::new(ButtonKind::Secondary, "Turn grouping on"))
                            .clicked()
                        {
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

    /// The typed-path row, once opened from More → Type a path….
    fn typed_row(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if !self.typing {
            return;
        }
        ui.add_space(space::XS);
        let label = ui.label(text(Type::Small, "Add path"));
        let can_add = self.expanding.is_none() && !self.path_text.trim().is_empty();
        let mut add = false;
        ui.horizontal(|ui| {
            // Room for the Add button after the field.
            let reserve = 2.0 * CONTROL_HEIGHT + space::S;
            let size = egui::vec2((ui.available_width() - reserve).max(0.0), CONTROL_HEIGHT);
            let field = ui
                .allocate_ui_with_layout(size, Layout::left_to_right(Align::Center), |ui| {
                    text_input(ui, &mut self.path_text, "a file, or folder/*.xisf")
                })
                .inner
                .labelled_by(label.id);
            if std::mem::take(&mut self.focus_typed) {
                field.request_focus();
            }
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let clicked = ui
                .add_enabled(can_add, MegaButton::new(ButtonKind::Secondary, "Add"))
                .clicked();
            add = can_add && (enter || clicked);
        });
        if add {
            let typed = self.path_text.clone();
            self.expanding = Task::spawn("mmm-expand-path", ctx, move || expand_typed(&typed));
        }
    }

    /// The table's sections for `shown` (see the module docs).
    fn sections(
        &self,
        shown: &PanelsEdit,
        session: &OpenSession,
        report: Option<&ChecksReport>,
    ) -> Vec<SectionData> {
        let cfg = &session.mosaic.config;
        let problems = panel_problems(report, cfg);
        let row = |path: &Path| self.row(path, problems.get(path));
        if !shown.grouping.enabled {
            let rows = shown.panels.iter().map(|p| row(&p.path)).collect();
            return vec![SectionData {
                heading: None,
                rows,
            }];
        }
        let placed = place(shown, self.resolved.as_ref().map(|(i, g)| (i, g)));
        let mut out = Vec::new();
        if !placed.ungrouped.is_empty() {
            let manual: BTreeSet<&Path> = shown
                .panels
                .iter()
                .filter(|p| p.manual_group.is_some())
                .map(|p| p.path.as_path())
                .collect();
            // A reason only for panels the rule left out (not those still
            // being read, nor those put there by hand).
            let by_rule = placed
                .ungrouped
                .iter()
                .any(|p| !placed.unsorted.contains(p) && !manual.contains(p.as_path()));
            out.push(SectionData {
                heading: Some(HeadingData {
                    id: UNGROUPED.into(),
                    title: format!("{UNGROUPED} · {}", placed.ungrouped.len()),
                    color: None,
                    warning: by_rule.then(|| no_group_reason(&shown.grouping.rule)),
                    rolls_up: false,
                }),
                rows: placed.ungrouped.iter().map(|p| row(p)).collect(),
            });
        }
        let assigned = load_group_colors(&cfg.ui);
        let colors = group_colors(&assigned, placed.groups.keys().map(String::as_str));
        // The group with the most panels (the first by name on a tie).
        let mut largest: Option<(&str, usize)> = None;
        for (name, paths) in &placed.groups {
            if largest.is_none_or(|(_, n)| paths.len() > n) {
                largest = Some((name, paths.len()));
            }
        }
        for (name, paths) in &placed.groups {
            let n = paths.len();
            let mut title = format!("{name} · {n}");
            if let Some(total) = self.total_integration(paths) {
                title.push_str(&format!(" · {}", integration_text(total)));
            }
            let warning = largest
                .filter(|(_, most)| n < *most)
                .map(|(big, most)| format!("{} fewer than {big}", most - n));
            out.push(SectionData {
                heading: Some(HeadingData {
                    id: name.clone(),
                    title,
                    color: colors.get(name).copied(),
                    warning,
                    rolls_up: true,
                }),
                rows: paths.iter().map(|p| row(p)).collect(),
            });
        }
        out
    }

    /// The summed integration of `paths`, when every one's is known.
    fn total_integration(&self, paths: &[PathBuf]) -> Option<f64> {
        paths.iter().try_fold(0.0, |sum, p| {
            let h = self.infos.get(p)?.header.as_ref().ok()?;
            Some(sum + h.integration_s()?)
        })
    }

    /// `path`'s row: its cells from its header ("—" for what is not known,
    /// yet or at all), and its problems (the read error, then `problems`).
    fn row(&self, path: &Path, problems: Option<&Vec<String>>) -> RowData {
        let mut cells = vec![file_name(path)];
        let mut texts = Vec::new();
        match self.infos.get(path).map(|i| &i.header) {
            None => cells.extend([READING, UNKNOWN, UNKNOWN, UNKNOWN].map(String::from)),
            Some(Ok(h)) => cells.extend([
                h.filter.clone().unwrap_or_else(|| UNKNOWN.into()),
                h.integration_s()
                    .map_or_else(|| UNKNOWN.into(), integration_text),
                h.scale_arcsec.map_or_else(|| UNKNOWN.into(), scale_text),
                format!("{} × {}", h.width, h.height),
            ]),
            Some(Err(e)) => {
                cells.extend([UNREADABLE, UNKNOWN, UNKNOWN, UNKNOWN].map(String::from));
                texts.push(format!("{UNREADABLE}: {e}"));
            }
        }
        texts.extend(problems.into_iter().flatten().cloned());
        RowData {
            id: row_id(path),
            cells,
            problem: (!texts.is_empty()).then(|| texts.join("\n\n")),
        }
    }

    /// Show where files dragged over the window will go: drawn over
    /// everything whether or not the list shows (the drop is taken
    /// whatever shows).
    pub fn drop_overlay(&self, ctx: &egui::Context) {
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
            Type::Title.text_style().resolve(&ctx.global_style()),
            palette::ACCENT_400,
        );
    }
}

#[cfg(test)]
impl PanelList {
    /// Nothing waiting: no edit pending and the groups resolved for the
    /// session's inputs.
    fn settled(&self, session: &OpenSession) -> bool {
        !self.edits.has_pending() && self.resolved_current(session)
    }

    /// The last resolution is for the session's inputs.
    fn resolved_current(&self, session: &OpenSession) -> bool {
        self.resolving.is_none()
            && self
                .resolved
                .as_ref()
                .is_some_and(|(i, _)| i.matches(&session.mosaic.config))
    }

    /// Files are being picked, dropped or a typed path expanded.
    fn adding(&self) -> bool {
        self.picking.is_some() || self.expanding.is_some() || !self.normalizing.is_empty()
    }

    fn pending(&self) -> Option<&PanelsEdit> {
        self.edits.pending()
    }
}

#[cfg(test)]
#[path = "panel_list_tests.rs"]
mod tests;
