//! The panel list (spec §10.2): the session's panel files as a sectioned
//! table, drawn by the session window in a column at the left of the
//! canvas while open, so the plan stays beside it.
//!
//! - **Toolbar.** **Add files…** (the file dialog), **Add folder…**, a
//!   More menu (**Type a path…**: a file, or `folder/*.xisf`) and **Group
//!   by**. Files dropped anywhere on the window are added whatever shows
//!   ([`PanelList::poll`]). Adding that adds nothing says why in a toast: a
//!   folder with no panel at its top level (per-filter subfolders are
//!   dropped together instead), one that can't be read, or panels already
//!   in the session.
//! - **Group by** (spec §10.3) chooses the rule: **Don't group**, **FILTER
//!   header**, **Header keyword…** and **Path keyword…** (each with its
//!   keyword's field, FILTER at first, committed on Enter or clicking
//!   away, Esc reverting), or **By hand**. A legacy file-name token rule
//!   shows as **Path keyword…** with its keyword and keeps resolving as it
//!   did until the user edits the keyword or picks a choice. A key or
//!   keyword typed (or picked) in another case only is the same rule:
//!   nothing changes. Any choice notes in the session that the user has
//!   chosen ([`GROUPING_CHOSEN_KEY`](crate::panel_edits::GROUPING_CHOSEN_KEY)).
//! - **Automatic grouping.** Until the user has chosen, panels whose headers
//!   carry two or more FILTER values while grouping is off are grouped by
//!   **FILTER header** by themselves, once, in one edit, with a toast whose
//!   Undo puts back only the grouping (and counts as a choice; after
//!   another choice it changes nothing). Picking the choice already shown
//!   changes nothing. A session that kept its filters together before
//!   (the "filters differ" note accepted, or grouping off with another
//!   rule than FILTER header) is not grouped, nor is one opened with
//!   analysis results until panels are added or removed: the edit would
//!   throw the results away.
//! - **Table** ([`sectioned_table`]): columns **PANEL**, **FILTER**,
//!   **INTEGRATION**, **SCALE** and **SIZE** from the panel headers ("—"
//!   when unknown; a file that can't be opened reads "Unreadable" with the
//!   error as its problem). A panel with an unaccepted problem in the checks
//!   report carries a ⚠ with the problem's text. A click on a column
//!   heading sorts the rows within each section by it (integration, scale
//!   and size as numbers; "—", "Reading…" and "Unreadable" last), again
//!   the other way; the sort stays while the session is open.
//! - **Sections.** With grouping on: "No group" first (never rolled up; its
//!   heading says why the rule placed nothing), then each group in name
//!   order, headed "{group} · {n}" (" · {total integration}" when every
//!   member's is known) in the group's colour, with "{d} fewer than
//!   {largest group}" when it has fewer panels than the largest. Group
//!   sections roll up to their first rows. With grouping off: one flat
//!   section.
//! - **Selection.** A click selects a row, Shift-click the rows on screen
//!   from the last one clicked, Ctrl-click (Cmd-click) adds or takes away
//!   one. It follows the panels (a regrouped panel stays selected; a
//!   removed one, or one whose section is folded, leaves it). A plain
//!   click also selects the panel on the plan (showing its group there
//!   first if the plan shows another view), and a panel clicked there
//!   selects its row ([`PanelList::select_only`]); the window links the
//!   rows and the plan ([`crate::session_window`]), clears the selection on
//!   Esc and removes it on Delete ([`PanelList::remove_selection`]). The
//!   row the pointer is over is reported ([`PanelListOutput::hovered`]),
//!   and the row of the panel pointed at on the plan is highlighted
//!   ([`PanelList::set_highlight`]).
//! - **Rows** carry an actions menu, acting on the selection when the row
//!   is selected, else on the row: with grouping on, **Move to** with each
//!   group, **New group…** (a name typed in the menu; the group is made of
//!   the panels moved, groups having no existence of their own), **No
//!   group** and **Let the rule decide**; then **Remove** (a toast offers
//!   Undo, which puts back only the panels removed). Rows dragged onto a
//!   section heading move there too. A panel moved by hand (its manual
//!   group, saved in `mosaic.json`) is tagged "by hand"; its FILTER cell
//!   still reads its header. A panel already in the group it is moved to
//!   is left as it is.
//!
//! # Threads
//!
//! The UI thread reads no file. The file dialog, the expansion of a typed
//! path, the normalising of dropped paths and the panel headers all run on
//! background threads ([`Task`]); the table shows the last known facts and
//! groups meanwhile. Each panel's header is read once
//! ([`read_panel_header`]) and cached by path and file fingerprint, so
//! regrouping after an edit reads nothing new; a header rule on a keyword
//! other than FILTER reads that keyword once more per panel. A reader that
//! panics makes its panel "Unreadable"; a resolution that stops
//! unexpectedly is tried again for the same inputs after [`RESOLVE_RETRY`]
//! (doubling each time), never every frame.
//!
//! Edits go through [`PanelEdits`] (queued behind running jobs; see
//! [`crate::panel_edits`]).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Align, Color32, Key, Label, Layout, Modifiers, Popup, TextWrapMode};
use mega_ui::icons;
use mega_ui::table::{
    Column, Heading, Row, Section, SortKey, TableEvent, TableState, sectioned_table, shown_ids,
};
use mega_ui::theme::text::{Type, text};
use mega_ui::theme::tokens::{CONTROL_HEIGHT, KEYWORD_INPUT_WIDTH, space};
use mega_ui::theme::{Tone, palette};
use mega_ui::widgets::{ButtonKind, MegaButton, Toast, icon_button_of, submenu, text_input};
use mmm_core::checks::{CheckCode, CheckKey, Severity, is_accepted};
use mmm_core::formats::InputPanel;
use mmm_core::mosaic::grouping::{Groups, PathKeyword, header_value, path_keyword, resolve_groups};
use mmm_core::mosaic::{
    Fingerprint, GroupRule, Grouping, MosaicConfig, PanelEntry, UNGROUPED, fingerprint,
};
use mmm_core::panel_header::{PanelHeader, read_panel_header};

use crate::checks::ChecksReport;
use crate::jobs::JobRunner;
use crate::panel_edits::{
    FilesPicker, Found, PanelEdits, PanelsEdit, assign_group_colors, expand_typed, group_colors,
    grouping_chosen, load_group_colors, panel_files_in, save_group_colors, set_grouping_chosen,
};
use crate::session::{OpenSession, normalize};
use crate::session_window::{Undo, WindowToast};
use crate::util::{Poll, Task};

/// Header keyword of **FILTER header**, and the first of **Header
/// keyword…**.
const DEFAULT_HEADER_KEY: &str = "FILTER";
/// The first keyword of **Path keyword…** (`…/FILTER_Ha/…` → `Ha`).
const DEFAULT_PATH_KEYWORD: &str = "FILTER";
/// A cell whose fact is unknown.
const UNKNOWN: &str = "—";
/// The FILTER cell of a panel whose header is not read yet.
const READING: &str = "Reading…";
/// The FILTER cell of a panel that can't be opened.
const UNREADABLE: &str = "Unreadable";
/// Wait before resolving the same inputs again after a resolution
/// stopped unexpectedly (doubling each time, up to [`RESOLVE_RETRY_MAX`]).
const RESOLVE_RETRY: Duration = Duration::from_secs(2);
/// Longest wait between two tries of the same inputs.
const RESOLVE_RETRY_MAX: Duration = Duration::from_secs(60);
/// The tag of a row whose panel was moved by hand.
const BY_HAND: &str = "by hand";
/// Why a batch of files that are not panels added nothing.
const ONLY_PANELS: &str = "Only XISF and FITS panel files can be added";

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
    pub hovered: Option<PathBuf>,
    /// The panel whose row was clicked with no modifier (selected alone).
    pub clicked: Option<PathBuf>,
    /// The panels removed this frame.
    pub removed: Vec<PathBuf>,
    /// Toasts to show (an edit refused, a removal to undo).
    pub toasts: Vec<WindowToast>,
}

/// What a row's actions menu asked for.
#[derive(Clone, Debug, PartialEq)]
enum RowAction {
    /// Into this group by hand ([`UNGROUPED`]: No group), or back to the
    /// rule (`None`).
    MoveTo(Option<String>),
    /// Out of the session.
    Remove,
}

/// A new group's name being typed in a row's actions menu.
#[derive(Debug)]
struct Naming {
    /// The row whose menu it is in.
    row: u64,
    /// The name so far.
    name: String,
    /// Give the field the keyboard on the next frame.
    focus: bool,
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
    /// How many FILTER values the panels' headers carry (panels without
    /// one not counted).
    filters: usize,
}

/// The grouping the panels get by themselves: by the FILTER header.
fn by_filter() -> Grouping {
    Grouping {
        enabled: true,
        rule: GroupRule::Header {
            key: DEFAULT_HEADER_KEY.into(),
        },
    }
}

/// Whether `config` shows the user chose to keep different filters
/// together before they could say so in **Group by**: the "filters differ"
/// note accepted, or grouping turned off with a rule other than FILTER
/// header.
fn kept_ungrouped(config: &MosaicConfig) -> bool {
    let note = CheckKey {
        code: CheckCode::FiltersDifferUngrouped,
        panel: None,
    };
    config.accepted_checks.contains(&note)
        || (!config.grouping.enabled && config.grouping.rule != GroupRule::default())
}

/// Whether rules `a` and `b` group alike: equal, or header rules (or path
/// keyword rules) whose keys differ only in case, which match whatever
/// their case. Changing one into the other would only throw the analysis
/// away.
fn same_rule(a: &GroupRule, b: &GroupRule) -> bool {
    match (a, b) {
        (GroupRule::Header { key: x }, GroupRule::Header { key: y })
        | (GroupRule::PathKeyword { keyword: x }, GroupRule::PathKeyword { keyword: y }) => {
            x.trim().eq_ignore_ascii_case(y.trim())
        }
        _ => a == b,
    }
}

/// Whether header keyword `key` is FILTER (keywords match whatever their
/// case).
fn is_filter(key: &str) -> bool {
    key.trim().eq_ignore_ascii_case(DEFAULT_HEADER_KEY)
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

/// `path`'s header facts, or why they can't be read: a reader that panics
/// makes the panel "Unreadable" (the rest still group) rather than end the
/// resolution. Blocking (one open).
fn read_header(path: &Path) -> Result<PanelHeader, String> {
    std::panic::catch_unwind(|| {
        #[cfg(test)]
        read_hooks::reading(path);
        read_panel_header(path).map_err(|e| e.to_string())
    })
    .unwrap_or_else(|_| Err("reading the file failed unexpectedly".into()))
}

/// The value of header keyword `key` in `path` (`None`: absent, or the file
/// unreadable, a reader that panics included). Blocking (one open).
fn read_key(path: &Path, key: &str) -> Option<String> {
    std::panic::catch_unwind(|| {
        #[cfg(test)]
        read_hooks::reading(path);
        InputPanel::open(path)
            .ok()
            .and_then(|panel| header_value(&panel, key))
    })
    .ok()
    .flatten()
}

/// Read what `infos` lacks for `inputs` (the header of each panel new or
/// changed on disk since read, and the rule's keyword), then resolve the
/// groups and count the FILTER values. Blocking (header reads): runs on a
/// worker.
fn resolve(inputs: PanelsEdit, mut infos: BTreeMap<PathBuf, PanelInfo>) -> Resolved {
    #[cfg(test)]
    read_hooks::resolving(&inputs);
    let key = rule_key(&inputs.grouping);
    // FILTER is among the header facts; another keyword is read on its own.
    let extra = key.clone().filter(|k| k != DEFAULT_HEADER_KEY);
    for p in &inputs.panels {
        let fingerprint = fingerprint(&p.path).ok();
        let fresh = infos
            .get(&p.path)
            .is_none_or(|i| i.fingerprint != fingerprint);
        if fresh {
            let header = read_header(&p.path);
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
                Ok(_) => read_key(&p.path, k),
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
    // Counted as FILTER header would group them.
    let filters = inputs
        .panels
        .iter()
        .filter_map(|p| infos.get(&p.path)?.header.as_ref().ok()?.filter.as_deref())
        .collect::<BTreeSet<&str>>()
        .len();
    Resolved {
        inputs,
        infos,
        groups,
        filters,
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

impl Placement {
    /// Each placed panel's section: its group, or [`UNGROUPED`] (panels
    /// whose group is still being resolved left out).
    fn sections(&self) -> BTreeMap<&Path, &str> {
        let named = self
            .groups
            .iter()
            .flat_map(|(g, ps)| ps.iter().map(move |p| (p.as_path(), g.as_str())));
        let ungrouped = self
            .ungrouped
            .iter()
            .filter(|p| !self.unsorted.contains(*p))
            .map(|p| (p.as_path(), UNGROUPED));
        named.chain(ungrouped).collect()
    }
}

/// The edit putting `targets` of `shown` in group `to` by hand
/// ([`UNGROUPED`]: No group), or with `to` `None` giving them back to the
/// rule; `None` when nothing changes. A panel `placed` in `to` already
/// stays as it is (it does not move).
fn moved(
    shown: &PanelsEdit,
    placed: &Placement,
    targets: &BTreeSet<PathBuf>,
    to: Option<&str>,
) -> Option<PanelsEdit> {
    let sections = placed.sections();
    let mut edit = shown.clone();
    let mut changed = false;
    for p in edit.panels.iter_mut().filter(|p| targets.contains(&p.path)) {
        let group = match to {
            Some(g) if sections.get(p.path.as_path()) == Some(&g) => continue,
            Some(g) => Some(g.to_string()),
            None => None,
        };
        if p.manual_group != group {
            p.manual_group = group;
            changed = true;
        }
    }
    changed.then_some(edit)
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

/// Why `rule` placed the panels `left_out` in no group: the "No group"
/// heading's warning. A path keyword found with values that differ reads
/// "no single {keyword} value in the path" (both reasons when some paths
/// lack the keyword).
fn no_group_reason<'p>(rule: &GroupRule, left_out: impl IntoIterator<Item = &'p Path>) -> String {
    match rule {
        GroupRule::Header { key } if key.trim().is_empty() => "no header keyword chosen".into(),
        GroupRule::Header { key } => format!("{} missing", key.trim().to_ascii_uppercase()),
        GroupRule::FilenameToken { keyword } => format!("no {keyword} in the file name"),
        GroupRule::PathKeyword { keyword } => {
            let (mut missing, mut conflicting) = (false, false);
            for p in left_out {
                match path_keyword(p, keyword) {
                    PathKeyword::Conflicting => conflicting = true,
                    PathKeyword::Missing | PathKeyword::Value(_) => missing = true,
                }
            }
            match (missing, conflicting) {
                (false, true) => format!("no single {keyword} value in the path"),
                (true, true) => format!("no {keyword} in the path, or no single value"),
                _ => format!("no {keyword} in the path"),
            }
        }
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

/// The toast saying why adding `found` added nothing, every reason in one
/// toast, the worst first: folders that couldn't be read, folders with no
/// panel at their top level, panels already in the session, then files
/// that are not panels (or not there). None for an empty batch. For a
/// batch that added nothing.
fn nothing_added(found: &Found) -> Option<WindowToast> {
    let names = |paths: &[PathBuf]| -> String {
        let names: Vec<String> = paths.iter().map(|p| file_name(p)).collect();
        names.join(", ")
    };
    let mut reasons = Vec::new();
    if !found.unreadable.is_empty() {
        reasons.push(format!("Couldn't read {}", names(&found.unreadable)));
    }
    if !found.empty.is_empty() {
        reasons.push(format!(
            "No panels at the top level of {}",
            names(&found.empty)
        ));
    }
    if !found.files.is_empty() {
        reasons.push("Already in the session".to_string());
    }
    if !found.skipped.is_empty() {
        reasons.push(ONLY_PANELS.to_string());
    }
    let mut reasons = reasons.into_iter();
    let title = reasons.next()?;
    let rest: Vec<String> = reasons.collect();
    let tone = if found.unreadable.is_empty() {
        Tone::Info
    } else {
        Tone::Problem
    };
    Some(WindowToast {
        toast: Toast {
            tone,
            title,
            detail: (!rest.is_empty()).then(|| rest.join("\n")),
            action: None,
        },
        undo: None,
    })
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

/// The actions menu of row `id`: with grouping on (`groups`: the groups
/// to move to), **Move to** with each group, **New group…**, **No group**
/// and **Let the rule decide**; then **Remove**. `naming` holds a new
/// group's name while it is typed.
fn row_menu(
    ui: &mut egui::Ui,
    id: u64,
    groups: Option<&[String]>,
    naming: &mut Option<Naming>,
) -> Option<RowAction> {
    let mut action = None;
    if let Some(groups) = groups {
        submenu(ui, "Move to", |ui| {
            for g in groups {
                if ui.button(g).clicked() {
                    action = Some(RowAction::MoveTo(Some(g.clone())));
                }
            }
            if !groups.is_empty() {
                ui.separator();
            }
            match naming.as_mut().filter(|n| n.row == id) {
                Some(n) => {
                    if let Some(name) = new_group_field(ui, n) {
                        action = Some(RowAction::MoveTo(Some(name)));
                    }
                }
                None => {
                    let new = ui
                        .button("New group…")
                        .on_hover_text("Make a group of these panels");
                    if new.clicked() {
                        *naming = Some(Naming {
                            row: id,
                            name: String::new(),
                            focus: true,
                        });
                    }
                }
            }
            if ui.button(UNGROUPED).clicked() {
                action = Some(RowAction::MoveTo(Some(UNGROUPED.into())));
            }
            let rule = ui
                .button("Let the rule decide")
                .on_hover_text("Back to where Group by places these panels");
            if rule.clicked() {
                action = Some(RowAction::MoveTo(None));
            }
            if action.is_some() {
                ui.close();
            }
        });
        ui.separator();
    }
    let remove = ui.button("Remove").on_hover_text(format!(
        "Remove from the session ({})",
        Key::Delete.symbol_or_name()
    ));
    if remove.clicked() {
        action = Some(RowAction::Remove);
    }
    action
}

/// The new group's name field in a row's menu, with **Create**: the name
/// (trimmed) once committed with Enter or Create; none while empty.
fn new_group_field(ui: &mut egui::Ui, naming: &mut Naming) -> Option<String> {
    let label = ui.label(text(Type::Small, "New group name"));
    let mut commit = false;
    ui.horizontal(|ui| {
        let field = ui
            .add(
                egui::TextEdit::singleline(&mut naming.name)
                    .hint_text("Group name")
                    .desired_width(KEYWORD_INPUT_WIDTH),
            )
            .labelled_by(label.id);
        if std::mem::take(&mut naming.focus) {
            field.request_focus();
        }
        let enter = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        let ready = !naming.name.trim().is_empty();
        let create = ui
            .add_enabled(ready, MegaButton::new(ButtonKind::Secondary, "Create"))
            .clicked();
        commit = ready && (enter || create);
    });
    commit.then(|| naming.name.trim().to_string())
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
    /// [`BY_HAND`] for a panel moved by hand (grouping on).
    tag: Option<&'static str>,
    selected: bool,
    highlighted: bool,
    /// How each cell sorts (numbers for the numeric columns).
    sort_keys: Vec<SortKey>,
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
            // Rows dragged onto a heading move into its section.
            heading: self.heading.as_ref().map(|h| Heading {
                id: &h.id,
                title: h.title.clone(),
                color: h.color,
                warning: h.warning.clone(),
                rolls_up: h.rolls_up,
                drop_target: true,
            }),
            rows: self
                .rows
                .iter()
                .map(|r| Row {
                    id: r.id,
                    cells: r.cells.clone(),
                    problem: r.problem.as_deref(),
                    tag: r.tag,
                    selected: r.selected,
                    highlighted: r.highlighted,
                    sort_keys: r.sort_keys.clone(),
                })
                .collect(),
        }
    }
}

/// The **Group by** choices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GroupBy {
    /// One mosaic.
    Off,
    /// The FILTER header.
    Filter,
    /// Another header keyword.
    Header,
    /// A keyword in the file path (or, shown so, a legacy file-name token).
    Path,
    /// Groups made by hand.
    Manual,
}

impl GroupBy {
    /// The choices, in the dropdown's order.
    const ALL: [GroupBy; 5] = [
        GroupBy::Off,
        GroupBy::Filter,
        GroupBy::Header,
        GroupBy::Path,
        GroupBy::Manual,
    ];

    /// The choice `grouping` is; a FILTER header rule shows as **Header
    /// keyword…** while the user has it open (`custom_header`).
    fn of(grouping: &Grouping, custom_header: bool) -> Self {
        if !grouping.enabled {
            return GroupBy::Off;
        }
        match &grouping.rule {
            GroupRule::Header { key } if is_filter(key) && !custom_header => GroupBy::Filter,
            GroupRule::Header { .. } => GroupBy::Header,
            GroupRule::PathKeyword { .. } | GroupRule::FilenameToken { .. } => GroupBy::Path,
            GroupRule::Manual => GroupBy::Manual,
        }
    }

    fn label(self) -> &'static str {
        match self {
            GroupBy::Off => "Don't group",
            GroupBy::Filter => "FILTER header",
            GroupBy::Header => "Header keyword…",
            GroupBy::Path => "Path keyword…",
            GroupBy::Manual => "By hand",
        }
    }

    /// The choice's tooltip.
    fn hint(self) -> &'static str {
        match self {
            GroupBy::Off => "Merge every panel into one mosaic",
            GroupBy::Filter => "A group for each filter the panel headers name",
            GroupBy::Header => "A group for each value of a header keyword you type",
            GroupBy::Path => {
                "A group for each value after a keyword in the file path, like Ha \
                 in FILTER_Ha (as WBPP groups)"
            }
            GroupBy::Manual => "Groups you make with Move to",
        }
    }
}

/// The panel list's state for the open session.
pub struct PanelList {
    picker: Arc<dyn FilesPicker>,
    /// The file or folder dialog, while open (its paths come back
    /// normalised).
    picking: Option<Task<Option<Found>>>,
    /// Dropped files being normalised.
    normalizing: Vec<Task<Found>>,
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
    /// The last header key and path keyword used, for switching rules.
    header_key: String,
    path_keyword: String,
    /// **Header keyword…** was picked: a FILTER header rule shows as it
    /// (with its field) rather than as **FILTER header**.
    custom_header: bool,
    /// Edits on their way into the session.
    edits: PanelEdits,
    /// Header facts by panel path.
    infos: BTreeMap<PathBuf, PanelInfo>,
    /// The background resolution, while running.
    resolving: Option<Task<Resolved>>,
    /// The inputs of the last resolution started.
    attempted: Option<PanelsEdit>,
    /// When to try `attempted` again (its resolution stopped unexpectedly),
    /// and the wait before the try after that.
    retry: Option<Instant>,
    retry_wait: Duration,
    /// The last resolution's inputs and groups.
    resolved: Option<(PanelsEdit, Groups)>,
    /// How many FILTER values the last resolution found.
    filters: usize,
    /// The panels were grouped by FILTER by themselves (or tried to be):
    /// never again for this session.
    auto_grouped: bool,
    /// The session has been polled once ([`PanelList::poll`]).
    opened: bool,
    /// The panels of a session opened with analysis results, as opened:
    /// no automatic grouping (which would throw the results away) while
    /// they are the panels shown.
    analysed_panels: Option<BTreeSet<PathBuf>>,
    /// The session's `ui` changed with no edit to save it (the user's
    /// grouping choice); see [`PanelList::take_ui_changed`].
    ui_changed: bool,
    /// The table's folded and opened sections.
    table: TableState,
    /// The selected panels.
    selection: BTreeSet<PathBuf>,
    /// The row last clicked (plainly or with Ctrl): where a Shift-click
    /// range starts.
    anchor: Option<PathBuf>,
    /// The panel whose row is highlighted (pointed at on the plan).
    highlight: Option<PathBuf>,
    /// A new group's name being typed in a row's menu.
    naming: Option<Naming>,
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
            path_keyword: DEFAULT_PATH_KEYWORD.into(),
            custom_header: false,
            edits: PanelEdits::default(),
            infos: BTreeMap::new(),
            resolving: None,
            attempted: None,
            retry: None,
            retry_wait: RESOLVE_RETRY,
            resolved: None,
            filters: 0,
            auto_grouped: false,
            opened: false,
            analysed_panels: None,
            ui_changed: false,
            table: TableState::default(),
            selection: BTreeSet::new(),
            anchor: None,
            highlight: None,
            naming: None,
            toasts: Vec::new(),
        }
    }

    /// Select panel `path` alone (it was clicked on the plan); `reveal`
    /// brings its row into view the next time the list is drawn.
    pub fn select_only(&mut self, path: &Path, reveal: bool) {
        self.selection = BTreeSet::from([path.to_path_buf()]);
        self.anchor = Some(path.to_path_buf());
        if reveal {
            self.table.reveal(row_id(path));
        }
    }

    /// Select nothing.
    pub fn clear_selection(&mut self) {
        self.selection.clear();
        self.anchor = None;
    }

    /// Highlight panel `path`'s row (or none): it is pointed at on the plan.
    /// Whether that changed what the list shows.
    pub fn set_highlight(&mut self, path: Option<PathBuf>) -> bool {
        let changed = self.highlight != path;
        self.highlight = path;
        changed
    }

    /// Remove the selected panels (a toast offers to put them back) and
    /// select nothing; the panels removed (none with nothing selected).
    pub fn remove_selection(
        &mut self,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
    ) -> Vec<PathBuf> {
        let shown = self.edits.shown(session);
        let targets = std::mem::take(&mut self.selection);
        self.anchor = None;
        let Some((edit, removed)) = self.remove(&shown, &targets) else {
            return Vec::new();
        };
        self.edits.submit(edit, session, jobs);
        self.collect_errors();
        removed
    }

    /// Put back panels removed earlier (an Undo): each as it was, at its
    /// index from before (or the end), into the inputs as they are now; one
    /// already back is left alone. Applied as any edit (queued behind
    /// running jobs).
    pub(crate) fn readd(
        &mut self,
        mut removed: Vec<(usize, PanelEntry)>,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
    ) {
        let mut edit = self.edits.shown(session);
        let before = edit.panels.len();
        removed.sort_by_key(|(at, _)| *at);
        for (at, entry) in removed {
            if edit.panels.iter().any(|p| p.path == entry.path) {
                continue;
            }
            let at = at.min(edit.panels.len());
            edit.panels.insert(at, entry);
        }
        if edit.panels.len() > before {
            self.edits.submit(edit, session, jobs);
            self.collect_errors();
        }
    }

    /// Put back `grouping` (an Undo of the automatic grouping) into the
    /// inputs as they are now (the panels untouched), noting that the user
    /// has chosen how to group; nothing more once the user has chosen
    /// another way to group than **FILTER header** (their choice stays).
    /// Applied as any edit (queued behind running jobs).
    pub(crate) fn regroup(
        &mut self,
        grouping: Grouping,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
    ) {
        self.note_choice(session);
        let mut edit = self.edits.shown(session);
        if edit.grouping != by_filter() || self.custom_header || edit.grouping == grouping {
            return;
        }
        self.rule_text = None;
        edit.grouping = grouping;
        self.edits.submit(edit, session, jobs);
        self.collect_errors();
    }

    /// Whether the session's `ui` changed since last asked with no edit to
    /// save it (the user's grouping choice): the window saves it with its
    /// own small, rate-limited saves.
    pub fn take_ui_changed(&mut self) -> bool {
        std::mem::take(&mut self.ui_changed)
    }

    /// Note in the session that the user has chosen how to group (saved
    /// with the edit that follows, else by the window).
    fn note_choice(&mut self, session: &mut OpenSession) {
        if set_grouping_chosen(&mut session.mosaic.config.ui) {
            self.ui_changed = true;
        }
    }

    /// Group by FILTER by themselves (once) panels whose headers carry two
    /// or more FILTER values while grouping is off, until the user has
    /// chosen how to group: one edit of the inputs as shown, once their
    /// resolution is in, with a toast whose Undo puts back the grouping
    /// from before. Not for a session that shows the user kept its filters
    /// together ([`kept_ungrouped`]), nor for one opened with analysis
    /// results while it has the panels it was opened with: the edit would
    /// throw the results away (adding or removing panels does anyway).
    fn auto_group(&mut self, session: &mut OpenSession, jobs: &mut JobRunner) {
        let config = &session.mosaic.config;
        if self.auto_grouped
            || self.filters < 2
            || grouping_chosen(&config.ui)
            || kept_ungrouped(config)
        {
            return;
        }
        // The filters were counted for the resolved inputs: only for those.
        let Some((resolved, _)) = &self.resolved else {
            return;
        };
        if resolved.grouping.enabled {
            return;
        }
        let shown = self.edits.shown(session);
        if *resolved != shown {
            return;
        }
        if let Some(opened) = &self.analysed_panels
            && shown.panels.len() == opened.len()
            && shown.panels.iter().all(|p| opened.contains(&p.path))
        {
            return;
        }
        self.auto_grouped = true;
        self.custom_header = false;
        self.rule_text = None;
        let previous = shown.grouping.clone();
        let edit = PanelsEdit {
            panels: shown.panels,
            grouping: by_filter(),
        };
        self.edits.submit(edit, session, jobs);
        self.collect_errors();
        if !self.edits.shown(session).grouping.enabled {
            // Refused: its error toast says why.
            return;
        }
        self.toasts.push(WindowToast {
            toast: Toast {
                tone: Tone::Info,
                title: format!("Grouped by FILTER: {} filters", self.filters),
                detail: None,
                action: Some("Undo".into()),
            },
            undo: Some(Undo::Grouping(previous)),
        });
    }

    /// Add `found`'s panel files not in yet; a batch adding none says why
    /// in a toast.
    fn add_found(&mut self, found: Found, session: &mut OpenSession, jobs: &mut JobRunner) {
        let why = nothing_added(&found);
        if self.edits.add_paths(found.files, session, jobs) == 0 {
            self.toasts.extend(why);
        }
    }

    /// Update the selection for a click on panel `path`'s row with
    /// `modifiers` held, `order` being the panels of the rows on screen:
    /// Shift selects the rows from the anchor to it (added to the selection
    /// with Ctrl or Cmd held too), Ctrl or Cmd adds or takes it away, a
    /// plain click selects it alone. Whether it was a plain click.
    fn click(&mut self, path: &Path, modifiers: Modifiers, order: &[PathBuf]) -> bool {
        let add = modifiers.command || modifiers.ctrl;
        if modifiers.shift {
            let at = |p: &Path| order.iter().position(|o| o == p);
            let range = self.anchor.as_deref().and_then(at).zip(at(path));
            if let Some((a, b)) = range {
                if !add {
                    self.selection.clear();
                }
                let (from, to) = (a.min(b), a.max(b));
                self.selection.extend(order[from..=to].iter().cloned());
                return false;
            }
        } else if add {
            if !self.selection.remove(path) {
                self.selection.insert(path.to_path_buf());
            }
            self.anchor = Some(path.to_path_buf());
            return false;
        }
        self.select_only(path, false);
        true
    }

    /// The panels a row's action on row `id` acts on: the selection (as
    /// `shown` has it) if the row is selected, else the row's panel.
    fn targets(&self, shown: &PanelsEdit, id: u64) -> BTreeSet<PathBuf> {
        let Some(path) = panel_of(shown, id) else {
            return BTreeSet::new();
        };
        if self.selection.contains(path) {
            self.selection.clone()
        } else {
            BTreeSet::from([path.to_path_buf()])
        }
    }

    /// Forget selected panels `shown` no longer has.
    fn keep_selection(&mut self, shown: &PanelsEdit) {
        if self.selection.is_empty() && self.anchor.is_none() {
            return;
        }
        let present: BTreeSet<&Path> = shown.panels.iter().map(|p| p.path.as_path()).collect();
        self.selection.retain(|p| present.contains(p.as_path()));
        if self.anchor.as_deref().is_some_and(|a| !present.contains(a)) {
            self.anchor = None;
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
    /// header reads), apply a waiting edit once the runner is idle, start
    /// resolving the shown inputs if they changed, and group the panels by
    /// FILTER by themselves when due. Call every frame,
    /// after the runner's poll, whether or not the list is shown (files
    /// dropped on the window are added whatever shows).
    pub fn poll(&mut self, ctx: &egui::Context, session: &mut OpenSession, jobs: &mut JobRunner) {
        if !std::mem::replace(&mut self.opened, true) {
            let config = &session.mosaic.config;
            if !config.group_dirs.is_empty() {
                let panels = config.panels.iter().map(|p| p.path.clone());
                self.analysed_panels = Some(panels.collect());
            }
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if !dropped.is_empty() {
            // Folders expanded and paths normalised (symlinks resolved) off
            // the UI thread before the duplicate check.
            let task = Task::spawn("mmm-drop-paths", ctx, move || panel_files_in(dropped));
            self.normalizing.extend(task);
        }
        let mut normalized = Vec::new();
        self.normalizing.retain(|task| match task.poll() {
            Poll::Pending => true,
            Poll::Done(found) => {
                normalized.push(found);
                false
            }
            Poll::Lost => false,
        });
        for found in normalized {
            self.add_found(found, session, jobs);
        }
        if let Some(task) = &self.picking {
            match task.poll() {
                Poll::Pending => {}
                Poll::Done(picked) => {
                    self.picking = None;
                    if let Some(found) = picked {
                        self.add_found(found, session, jobs);
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
                            self.add_found(Found::of_files(paths), session, jobs);
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
        self.auto_group(session, jobs);
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
                    self.filters = r.filters;
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
                    self.retry_wait = RESOLVE_RETRY;
                }
                Poll::Lost => {
                    // Try again later rather than leave rows "Reading…"
                    // for good, or try again every frame.
                    self.resolving = None;
                    let wait = self.retry_wait;
                    self.retry = Some(Instant::now() + wait);
                    self.retry_wait = (wait * 2).min(RESOLVE_RETRY_MAX);
                    tracing::warn!(
                        "reading the panel headers stopped unexpectedly; trying again in {wait:?}"
                    );
                }
            }
        }
        if let Some((inputs, groups)) = &self.resolved
            && inputs.matches(&session.mosaic.config)
            && session.groups.as_ref() != Some(groups)
        {
            session.groups = Some(groups.clone());
        }
        if self.resolving.is_some() {
            return;
        }
        let shown = self.edits.shown(session);
        // The same inputs again only once their retry is due.
        let now = Instant::now();
        let again = match self.retry {
            Some(at) if at > now => {
                ctx.request_repaint_after(at - now);
                false
            }
            Some(_) => true,
            None => false,
        };
        if again || self.attempted.as_ref() != Some(&shown) {
            self.retry = None;
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
            Some(Found::of_files(
                picked.iter().map(|p| normalize(p)).collect(),
            ))
        });
    }

    /// Open the folder dialog (on a worker; [`PanelList::poll`] adds the
    /// panel files at the top level of what was picked), unless a dialog is
    /// open already.
    pub fn pick_folders(&mut self, ctx: &egui::Context) {
        if self.picking.is_some() {
            return;
        }
        let picker = self.picker.clone();
        self.picking = Task::spawn("mmm-pick-folders", ctx, move || {
            Some(panel_files_in(picker.pick_folders()?))
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
        if !Popup::is_any_open(&ctx) {
            // The menu it was typed in closed.
            self.naming = None;
        }
        let shown = self.edits.shown(session);
        self.keep_selection(&shown);
        let mut out = PanelListOutput::default();
        let mut edit: Option<PanelsEdit> = None;
        if let Some(grouping) = self.toolbar(ui, &ctx, &shown) {
            // Noted first, so the edit's save carries it.
            self.note_choice(session);
            if grouping != shown.grouping {
                edit = Some(PanelsEdit {
                    panels: shown.panels.clone(),
                    grouping,
                });
            }
        }
        self.notices(ui);
        self.typed_row(ui, &ctx);
        ui.add_space(space::S);
        if shown.panels.is_empty() {
            ui.label(text(
                Type::Body,
                "No panels yet: add files or folders, or drop them anywhere on the window.",
            ));
        } else if let Some(e) = self.table_ui(ui, &shown, session, report, &mut out) {
            edit = Some(e);
        }
        if let Some(e) = edit {
            self.edits.submit(e, session, jobs);
        }
        for p in &out.removed {
            self.selection.remove(p);
        }
        out.toasts = self.take_toasts();
        out
    }

    /// The table of `shown`'s panels, and what was done in it (reported in
    /// `out`): the selection follows clicks; a row's menu or rows dropped
    /// on a heading give the edit returned.
    fn table_ui(
        &mut self,
        ui: &mut egui::Ui,
        shown: &PanelsEdit,
        session: &OpenSession,
        report: Option<&ChecksReport>,
        out: &mut PanelListOutput,
    ) -> Option<PanelsEdit> {
        let placed = shown
            .grouping
            .enabled
            .then(|| place(shown, self.resolved.as_ref().map(|(i, g)| (i, g))));
        let data = self.sections(shown, placed.as_ref(), session, report);
        let sections: Vec<Section<'_>> = data.iter().map(SectionData::section).collect();
        // The rows on screen as the table is drawn (for a Shift range).
        let order = shown_ids(&self.table, &sections);
        // Groups to move to, with grouping on.
        let groups: Option<Vec<String>> =
            placed.as_ref().map(|p| p.groups.keys().cloned().collect());
        let mut action: Option<(u64, RowAction)> = None;
        let folded_before = self.table.folded.clone();
        let (table, naming) = (&mut self.table, &mut self.naming);
        let events = egui::ScrollArea::vertical()
            .id_salt("panel-list-rows")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                sectioned_table(ui, table, &COLUMNS, &sections, |ui, id| {
                    if let Some(a) = row_menu(ui, id, groups.as_deref(), naming) {
                        action = Some((id, a));
                    }
                })
            })
            .inner;
        let path = |id: u64| panel_of(shown, id).map(Path::to_path_buf);
        // Rows folded away leave the selection (Delete and Move to act on
        // what shows).
        let folded_now = sections.iter().filter(|s| {
            s.heading
                .as_ref()
                .is_some_and(|h| self.table.is_folded(h.id) && !folded_before.contains(h.id))
        });
        for row in folded_now.flat_map(|s| &s.rows) {
            if let Some(p) = path(row.id) {
                self.selection.remove(&p);
            }
        }
        let mut edit = None;
        for event in events {
            match event {
                TableEvent::Hover(id) => out.hovered = path(id),
                TableEvent::Click { id, modifiers } => {
                    let Some(p) = path(id) else { continue };
                    let order: Vec<PathBuf> = order.iter().filter_map(|&i| path(i)).collect();
                    if self.click(&p, modifiers, &order) {
                        out.clicked = Some(p);
                    }
                }
                TableEvent::Drop { ids, section } => {
                    let targets = ids.iter().filter_map(|&i| path(i)).collect();
                    if let Some(placed) = &placed
                        && let Some(e) = moved(shown, placed, &targets, Some(&section))
                    {
                        edit = Some(e);
                    }
                }
            }
        }
        let Some((id, action)) = action else {
            return edit;
        };
        let targets = self.targets(shown, id);
        match action {
            RowAction::MoveTo(to) => placed
                .as_ref()
                .and_then(|placed| moved(shown, placed, &targets, to.as_deref()))
                .or(edit),
            RowAction::Remove => match self.remove(shown, &targets) {
                Some((e, removed)) => {
                    out.removed = removed;
                    Some(e)
                }
                None => edit,
            },
        }
    }

    /// The edit removing `targets` from `shown`, with a toast whose Undo
    /// puts back just those (each where it was), and the panels removed;
    /// `None` if `shown` has none of them.
    fn remove(
        &mut self,
        shown: &PanelsEdit,
        targets: &BTreeSet<PathBuf>,
    ) -> Option<(PanelsEdit, Vec<PathBuf>)> {
        let removed: Vec<(usize, PanelEntry)> = shown
            .panels
            .iter()
            .enumerate()
            .filter(|(_, p)| targets.contains(&p.path))
            .map(|(at, p)| (at, p.clone()))
            .collect();
        if removed.is_empty() {
            return None;
        }
        let mut edit = shown.clone();
        edit.panels.retain(|p| !targets.contains(&p.path));
        let paths = removed.iter().map(|(_, p)| p.path.clone()).collect();
        self.toasts.push(WindowToast {
            toast: Toast {
                tone: Tone::Neutral,
                title: removed_title(removed.len()),
                detail: None,
                action: Some("Undo".into()),
            },
            undo: Some(Undo::Readd(removed)),
        });
        Some((edit, paths))
    }

    /// Add files, Add folder, More (Type a path…), and Group by with the
    /// rule's keyword; the grouping the user chose in Group by, if they did.
    fn toolbar(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        shown: &PanelsEdit,
    ) -> Option<Grouping> {
        // Two rows, each fitting the narrowest list: adding, then grouping.
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::S;
            let add = MegaButton::new(ButtonKind::Secondary, "Add files…")
                .hover_text("Choose XISF or FITS panel files");
            if ui.add_enabled(self.picking.is_none(), add).clicked() {
                self.pick_files(ctx);
            }
            let folder = MegaButton::new(ButtonKind::Secondary, "Add folder…")
                .hover_text("Choose folders of panels (their top level only)");
            if ui.add_enabled(self.picking.is_none(), folder).clicked() {
                self.pick_folders(ctx);
            }
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
            self.group_by(ui, shown)
        })
        .inner
    }

    /// The **Group by** dropdown and, for a rule with one, its keyword
    /// field; the grouping the user chose (picked, perhaps the one in use,
    /// or with a new keyword committed).
    fn group_by(&mut self, ui: &mut egui::Ui, shown: &PanelsEdit) -> Option<Grouping> {
        let rule = &shown.grouping.rule;
        // The session's keywords, for switching back to their rules.
        match rule {
            GroupRule::Header { key } if !is_filter(key) => self.header_key = key.clone(),
            GroupRule::PathKeyword { keyword } | GroupRule::FilenameToken { keyword } => {
                self.path_keyword = keyword.clone();
            }
            _ => {}
        }
        let current = GroupBy::of(&shown.grouping, self.custom_header);
        let mut chosen = None;
        let label =
            ui.add(Label::new(text(Type::Small, "Group by")).wrap_mode(TextWrapMode::Extend));
        let combo = egui::ComboBox::from_id_salt("panel-list-group-by")
            .selected_text(current.label())
            .show_ui(ui, |ui| {
                for c in GroupBy::ALL {
                    let item = ui
                        .selectable_label(c == current, c.label())
                        .on_hover_text(c.hint());
                    if item.clicked() {
                        chosen = Some(c);
                    }
                }
            });
        let _ = combo.response.labelled_by(label.id);
        if let Some(c) = chosen {
            self.rule_text = None;
            if c == current {
                // Nothing changes (a legacy file-name token rule stays one).
                return Some(shown.grouping.clone());
            }
            self.custom_header = c == GroupBy::Header;
            let rule = match c {
                GroupBy::Off => rule.clone(),
                GroupBy::Filter => GroupRule::Header {
                    key: DEFAULT_HEADER_KEY.into(),
                },
                GroupBy::Header => GroupRule::Header {
                    key: self.header_key.clone(),
                },
                // A legacy file-name token rule becomes a path keyword one.
                GroupBy::Path => GroupRule::PathKeyword {
                    keyword: self.path_keyword.clone(),
                },
                GroupBy::Manual => GroupRule::Manual,
            };
            let grouping = Grouping {
                enabled: c != GroupBy::Off,
                rule,
            };
            if grouping.enabled == shown.grouping.enabled
                && same_rule(&grouping.rule, &shown.grouping.rule)
            {
                // Nothing changes (a key in another case groups alike).
                return Some(shown.grouping.clone());
            }
            return Some(grouping);
        }
        if !matches!(current, GroupBy::Header | GroupBy::Path) {
            return None;
        }
        self.rule_field(ui, rule).map(|rule| Grouping {
            enabled: true,
            rule,
        })
    }

    /// The rule's keyword field; the new rule once the user commits a new
    /// keyword (Enter or clicking away; Esc, or an empty field, abandons
    /// the typing). A new keyword for a legacy file-name token rule makes
    /// it a path keyword rule.
    fn rule_field(&mut self, ui: &mut egui::Ui, rule: &GroupRule) -> Option<GroupRule> {
        let (current, field_label, hint) = match rule {
            GroupRule::Header { key } => (key, "Header key", DEFAULT_HEADER_KEY),
            GroupRule::PathKeyword { keyword } | GroupRule::FilenameToken { keyword } => {
                (keyword, "Path keyword", DEFAULT_PATH_KEYWORD)
            }
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
            // Header keys and path keywords match whatever their case; a
            // legacy file-name token rule becomes a path keyword one.
            let same = match rule {
                GroupRule::FilenameToken { .. } => typed == *current,
                _ => typed.eq_ignore_ascii_case(current.trim()),
            };
            if typed.is_empty() || same {
                return None;
            }
            return Some(match rule {
                GroupRule::Header { .. } => {
                    self.header_key = typed.clone();
                    GroupRule::Header { key: typed }
                }
                _ => {
                    self.path_keyword = typed.clone();
                    GroupRule::PathKeyword { keyword: typed }
                }
            });
        }
        if field.has_focus() {
            self.rule_text = Some(typed);
        }
        None
    }

    /// The waiting edit, if any.
    fn notices(&mut self, ui: &mut egui::Ui) {
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

    /// The table's sections for `shown`, its panels `placed` in groups with
    /// grouping on (see the module docs).
    fn sections(
        &self,
        shown: &PanelsEdit,
        placed: Option<&Placement>,
        session: &OpenSession,
        report: Option<&ChecksReport>,
    ) -> Vec<SectionData> {
        let cfg = &session.mosaic.config;
        let problems = panel_problems(report, cfg);
        // Panels moved by hand (which counts with grouping on only).
        let manual: BTreeSet<&Path> = shown
            .panels
            .iter()
            .filter(|p| p.manual_group.is_some())
            .map(|p| p.path.as_path())
            .collect();
        let row = |path: &Path| {
            let mut row = self.row(path, problems.get(path));
            row.selected = self.selection.contains(path);
            row.highlighted = self.highlight.as_deref() == Some(path);
            if placed.is_some() && manual.contains(path) {
                row.tag = Some(BY_HAND);
            }
            row
        };
        let Some(placed) = placed else {
            let rows = shown.panels.iter().map(|p| row(&p.path)).collect();
            return vec![SectionData {
                heading: None,
                rows,
            }];
        };
        let mut out = Vec::new();
        if !placed.ungrouped.is_empty() {
            // A reason only for panels the rule left out (not those still
            // being read, nor those put there by hand).
            let left_out: Vec<&Path> = placed
                .ungrouped
                .iter()
                .filter(|p| !placed.unsorted.contains(*p) && !manual.contains(p.as_path()))
                .map(PathBuf::as_path)
                .collect();
            out.push(SectionData {
                heading: Some(HeadingData {
                    id: UNGROUPED.into(),
                    title: format!("{UNGROUPED} · {}", placed.ungrouped.len()),
                    color: None,
                    warning: (!left_out.is_empty())
                        .then(|| no_group_reason(&shown.grouping.rule, left_out)),
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
    /// yet or at all), how they sort (integration in seconds, scale in
    /// arcseconds, size in pixels; "Reading…" and "Unreadable" last), and
    /// its problems (the read error, then `problems`).
    fn row(&self, path: &Path, problems: Option<&Vec<String>>) -> RowData {
        let name = file_name(path);
        let mut sort_keys = vec![SortKey::Text(name.clone())];
        let mut cells = vec![name];
        let mut texts = Vec::new();
        let number = |n: Option<f64>| n.map_or(SortKey::Unknown, SortKey::Number);
        match self.infos.get(path).map(|i| &i.header) {
            None => cells.extend([READING, UNKNOWN, UNKNOWN, UNKNOWN].map(String::from)),
            Some(Ok(h)) => {
                cells.extend([
                    h.filter.clone().unwrap_or_else(|| UNKNOWN.into()),
                    h.integration_s()
                        .map_or_else(|| UNKNOWN.into(), integration_text),
                    h.scale_arcsec.map_or_else(|| UNKNOWN.into(), scale_text),
                    format!("{} × {}", h.width, h.height),
                ]);
                sort_keys.extend([
                    h.filter.clone().map_or(SortKey::Unknown, SortKey::Text),
                    number(h.integration_s()),
                    number(h.scale_arcsec),
                    SortKey::Number(h.width as f64 * h.height as f64),
                ]);
            }
            Some(Err(e)) => {
                cells.extend([UNREADABLE, UNKNOWN, UNKNOWN, UNKNOWN].map(String::from));
                texts.push(format!("{UNREADABLE}: {e}"));
            }
        }
        // Not read (yet, or at all): unknown in every column but the name.
        sort_keys.resize(cells.len(), SortKey::Unknown);
        texts.extend(problems.into_iter().flatten().cloned());
        RowData {
            id: row_id(path),
            cells,
            problem: (!texts.is_empty()).then(|| texts.join("\n\n")),
            tag: None,
            selected: false,
            highlighted: false,
            sort_keys,
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
            "Drop panel files or folders to add them",
            Type::Title.text_style().resolve(&ctx.global_style()),
            palette::ACCENT_400,
        );
    }
}

/// Test-only hooks into the background resolution: readers that panic for
/// chosen panels, a resolution that panics outside them, and how many
/// resolutions each panel took part in.
#[cfg(test)]
pub(crate) mod read_hooks {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use crate::panel_edits::PanelsEdit;

    static PANIC_READING: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());
    static PANIC_RESOLVING: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());
    static RESOLVES: Mutex<BTreeMap<PathBuf, usize>> = Mutex::new(BTreeMap::new());

    /// Reading `path`'s header panics from now on.
    pub(crate) fn panic_reading(path: &Path) {
        PANIC_READING.lock().unwrap().insert(path.to_path_buf());
    }

    /// A resolution with `path` among its inputs panics (outside any
    /// reader) from now on, or (`on: false`) no longer.
    pub(crate) fn panic_resolving(path: &Path, on: bool) {
        let mut set = PANIC_RESOLVING.lock().unwrap();
        if on {
            set.insert(path.to_path_buf());
        } else {
            set.remove(path);
        }
    }

    /// How many resolutions `path` was among the inputs of.
    pub(crate) fn resolves(path: &Path) -> usize {
        RESOLVES.lock().unwrap().get(path).copied().unwrap_or(0)
    }

    /// A reader opens `path`.
    pub(super) fn reading(path: &Path) {
        if PANIC_READING.lock().unwrap().contains(path) {
            panic!("test: reading {} panics", path.display());
        }
    }

    /// A resolution of `inputs` starts.
    pub(super) fn resolving(inputs: &PanelsEdit) {
        let mut panics = false;
        {
            let mut counts = RESOLVES.lock().unwrap();
            let set = PANIC_RESOLVING.lock().unwrap();
            for p in &inputs.panels {
                *counts.entry(p.path.clone()).or_default() += 1;
                panics |= set.contains(&p.path);
            }
        }
        if panics {
            panic!("test: resolving panics");
        }
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

    /// The selected panels.
    pub(crate) fn selection(&self) -> &BTreeSet<PathBuf> {
        &self.selection
    }

    /// Apply `edit` from outside the list: as any edit, queued behind
    /// running jobs.
    pub(crate) fn submit_edit(
        &mut self,
        edit: PanelsEdit,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
    ) {
        self.edits.submit(edit, session, jobs);
        self.collect_errors();
    }
}

#[cfg(test)]
#[path = "panel_list_tests.rs"]
mod tests;
