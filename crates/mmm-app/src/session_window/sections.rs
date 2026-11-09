//! The inspector's sections, in their fixed order:
//!
//! - **Problems**: the checks' problems (and what could not be checked),
//!   each with **Accept** where it can be accepted, then **See all checks ›**
//!   for the notes and passed checks too. Its badge counts the unaccepted
//!   problems.
//! - **Panels**: "{n} panels · {g} groups · {canvas}", the previews'
//!   progress (with **Resume** once paused), and the selected panel's
//!   details and checks.
//! - **ROIs**: **Draw ROI** (also the `D` key, which reveals the section so
//!   its hint shows) arms the draw tool on the canvas shown, plan or mosaic
//!   (a crosshair over it while armed), then the ROI list
//!   ([`MosaicView::rois_ui`]). Picking an ROI frames it on the canvas
//!   shown ([`SessionWindow::go_to_roi`]); on a plan drawn in another frame
//!   than the analysed one nothing moves and a toast says why (the reason
//!   **Draw ROI** gives there: a toast rather than a tooltip, since the
//!   list's rows carry none and the click must not pass unexplained).
//! - **Merge settings** ([`MosaicView::settings_ui`]), then the health.
//! - **Composites**: the composites as an ordered list (pick, rename,
//!   reorder, delete with Undo); the picked one shows its channel editor.
//! - **Notes**: the session's notes, saved with the session (debounced).

use std::path::Path;
use std::time::Instant;

use mega_ui::checks::{CheckAction, CheckItem, ListMode, checks_list};
use mega_ui::inspector::{Badge, Section, inspector_with_shortcut};
use mega_ui::theme::text::{Type, text};
use mega_ui::theme::tokens::space;
use mega_ui::theme::{Severity as UiSeverity, Tone};
use mega_ui::widgets::ordered_list::{ListEvent, ListRow, ListState, ordered_list_with};
use mega_ui::widgets::{ButtonKind, MegaButton, NotesMode, notes_view};
use mmm_core::checks::{Check, Severity, is_accepted};
use mmm_core::mosaic::MosaicConfig;

use super::shortcuts;
use super::{
    COMPOSITES, CanvasMode, MERGE_SETTINGS, NOTES, PANELS, PROBLEMS, ROIS, SessionOutput,
    SessionWindow, Undo, WindowToast, compositions_of,
};
use crate::checks::{ChecksEngine, ChecksReport};
use crate::compositions::{
    Composition, ViewKind, channel_editor, composition_detail, save_compositions,
};
use crate::jobs::JobRunner;
use crate::mosaic_view::MosaicView;
use crate::pipeline::{Pipeline, Step};
use crate::plan_view::PlanView;
use crate::session::{OpenSession, notes, set_notes};
use crate::status::format_canvas;
use crate::util::{Debounce, group_names};

/// What to do with the draw tool armed.
const DRAW_HINT: &str = "Drag on the image to draw; Esc cancels.";

/// The notes text box's hint (as on the start screen).
const NOTES_HINT: &str = "Notes about this session";

/// What the sections asked of the window this frame.
#[derive(Default)]
struct Actions {
    mode: Option<CanvasMode>,
    /// A composite was deleted: the index it had, and it.
    deleted: Option<(usize, Composition)>,
    /// A composite row was picked: show it.
    view: Option<ViewKind>,
    /// A rename was refused: another composite has this name.
    refused: Option<String>,
    /// An ROI was picked: frame this canvas rect.
    go_to: Option<[u64; 4]>,
    /// **Draw ROI** was used: arm the draw tool on the canvas shown.
    draw_roi: bool,
}

impl SessionWindow {
    /// The inspector filling `ui` (sized by the caller).
    pub(super) fn inspector_ui(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
        pipeline: &mut Pipeline,
        checks: &ChecksEngine,
        out: &mut SessionOutput,
    ) {
        let count = |n: usize, tone: Tone| {
            (n > 0).then(|| Badge {
                text: n.to_string(),
                tone,
            })
        };
        let problems = checks
            .counts(&session.mosaic.config)
            .map_or(0, |c| c.problems);
        let sections = [
            Section {
                id: PROBLEMS,
                title: "Problems",
                badge: count(problems, Tone::Warning),
            },
            Section {
                id: PANELS,
                title: "Panels",
                badge: count(session.mosaic.config.panels.len(), Tone::Neutral),
            },
            Section {
                id: ROIS,
                title: "ROIs",
                badge: count(self.mosaic.rois().len(), Tone::Neutral),
            },
            Section {
                id: MERGE_SETTINGS,
                title: "Merge settings",
                badge: None,
            },
            Section {
                id: COMPOSITES,
                title: "Composites",
                badge: count(compositions_of(session).len(), Tone::Neutral),
            },
            Section {
                id: NOTES,
                title: "Notes",
                badge: None,
            },
        ];
        let draw_block = self.draw_roi_block(session);
        let drawing = self.drawing_roi();
        let mut act = Actions::default();
        let SessionWindow {
            plan,
            mosaic,
            inspector: state,
            show_all_checks,
            notes_mode,
            save,
            composite_list,
            view,
            ..
        } = self;
        let fold = shortcuts::FOLD.symbol_or_name();
        inspector_with_shortcut(ui, state, &sections, fold, |ui, id| {
            // Each body under its own id: the inspector draws them all in
            // like-identified scopes.
            ui.push_id(id, |ui| match id {
                PROBLEMS => problems_ui(ui, session, checks, show_all_checks, &mut out.accept),
                PANELS => panels_ui(ui, session, plan, pipeline, checks, &mut out.accept),
                ROIS => rois_ui(ui, session, mosaic, (draw_block, drawing), &mut act),
                MERGE_SETTINGS => {
                    mosaic.settings_ui(ui, session, jobs);
                    ui.add_space(space::L);
                    mosaic.health_ui(ui);
                }
                COMPOSITES => {
                    composites_ui(ui, session, (composite_list, view.as_ref()), save, &mut act)
                }
                NOTES => notes_ui(ui, session, notes_mode, save),
                _ => {}
            });
        });
        if let Some(mode) = act.mode {
            self.set_mode(mode);
        }
        if let Some(rect) = act.go_to {
            self.go_to_roi(session, rect);
        }
        if act.draw_roi {
            self.start_drawing_roi(session);
        }
        if let Some(view) = act.view {
            self.select_view(view);
        }
        if let Some(name) = act.refused {
            self.push_toast(WindowToast {
                toast: mega_ui::widgets::Toast {
                    tone: Tone::Warning,
                    title: format!("Another composite is called {name}"),
                    detail: None,
                    action: None,
                },
                undo: None,
            });
        }
        if let Some((index, composite)) = act.deleted {
            self.composite_deleted(session, composite.id);
            self.push_toast(WindowToast {
                toast: mega_ui::widgets::Toast {
                    tone: Tone::Neutral,
                    title: format!("{} deleted", composite.name),
                    detail: None,
                    action: Some("Undo".into()),
                },
                undo: Some(Undo::CompositeDelete { index, composite }),
            });
        }
    }

    /// Composite `id` is gone: a view of it shows the first group.
    fn composite_deleted(&mut self, session: &OpenSession, id: u64) {
        if self.view == Some(ViewKind::Composition(id))
            && let Some(g) = group_names(session).first().cloned()
        {
            self.select_view(ViewKind::Group(g));
        }
    }
}

/// Previews are running, or about to start (no Resume is offered).
fn previews_busy(pipeline: &Pipeline) -> bool {
    pipeline.step() == Some(Step::Previews) || pipeline.waiting(Instant::now())
}

/// A muted secondary line.
fn muted(ui: &mut egui::Ui, s: impl Into<String>) {
    ui.label(text(Type::Small, s));
}

/// `n` and `noun`, plural unless one.
pub(crate) fn counted(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// The checks of `report` that `keep` passes as list items, with the check
/// each stands for; with `errors`, what could not be checked first (as
/// problems that cannot be accepted, standing for no check).
fn check_items<'r>(
    report: &'r ChecksReport,
    cfg: &MosaicConfig,
    errors: bool,
    keep: impl Fn(&Check) -> bool,
) -> (Vec<CheckItem>, Vec<Option<&'r Check>>) {
    let mut items = Vec::new();
    let mut sources = Vec::new();
    if errors {
        for e in &report.errors {
            items.push(CheckItem {
                severity: UiSeverity::Problem,
                title: e.clone(),
                detail: String::new(),
                accepted: false,
                acceptable: false,
                goes_somewhere: false,
            });
            sources.push(None);
        }
    }
    for c in report.checks.iter().filter(|c| keep(c)) {
        items.push(CheckItem {
            severity: match c.severity {
                Severity::Problem => UiSeverity::Problem,
                Severity::Note => UiSeverity::Note,
                Severity::Passed => UiSeverity::Passed,
            },
            title: c.message.clone(),
            detail: c.remedy.clone().unwrap_or_default(),
            accepted: is_accepted(cfg, c),
            acceptable: c.severity != Severity::Passed,
            goes_somewhere: false,
        });
        sources.push(Some(c));
    }
    (items, sources)
}

/// `items` under `heading` (none: an empty string); an accepted one's check
/// goes to `accept`.
fn list(
    ui: &mut egui::Ui,
    heading: &str,
    items: &[CheckItem],
    sources: &[Option<&Check>],
    accept: &mut Vec<Check>,
) {
    // The flat list for now (the old "See all checks" toggle filters it).
    // Un-accept and Open have no handler yet: no row sets `goes_somewhere`
    // and the grouped `ListMode::All` (which offers Un-accept) is not used.
    for action in checks_list(ui, heading, items, ListMode::Problems) {
        if let CheckAction::Accept(i) = action
            && let Some(Some(c)) = sources.get(i)
        {
            accept.push((*c).clone());
        }
    }
}

fn problems_ui(
    ui: &mut egui::Ui,
    session: &OpenSession,
    checks: &ChecksEngine,
    show_all: &mut bool,
    accept: &mut Vec<Check>,
) {
    match checks.report() {
        None if checks.running() => muted(ui, "Checking…"),
        None => muted(ui, "Not checked yet."),
        Some(report) => {
            let all = *show_all;
            let (items, sources) = check_items(report, &session.mosaic.config, true, |c| {
                all || c.severity == Severity::Problem
            });
            if items.is_empty() {
                muted(
                    ui,
                    if all {
                        "No checks."
                    } else {
                        "No problems found."
                    },
                );
            } else {
                list(ui, "", &items, &sources, accept);
            }
        }
    }
    let label = if *show_all {
        "Problems only"
    } else {
        "See all checks ›"
    };
    if ui.add(MegaButton::new(ButtonKind::Quiet, label)).clicked() {
        *show_all = !*show_all;
    }
}

fn panels_ui(
    ui: &mut egui::Ui,
    session: &OpenSession,
    plan: &mut PlanView,
    pipeline: &mut Pipeline,
    checks: &ChecksEngine,
    accept: &mut Vec<Check>,
) {
    let cfg = &session.mosaic.config;
    let n = cfg.panels.len();
    let mut summary = format!(
        "{} · {}",
        counted(n, "panel"),
        counted(group_names(session).len(), "group")
    );
    if let Some((w, h)) = session.canvas().or(plan.frame_canvas()) {
        summary.push_str(&format!(" · {}", format_canvas(w, h)));
    }
    ui.label(text(Type::Body, summary));
    if n > 0 && !pipeline.previews_complete() {
        ui.horizontal(|ui| {
            if previews_busy(pipeline) {
                ui.spinner();
                muted(
                    ui,
                    format!("Reading previews {} / {n}", pipeline.previews().len()),
                );
            } else {
                muted(ui, "Previews paused");
                if ui
                    .add(MegaButton::new(ButtonKind::Quiet, "Resume"))
                    .on_hover_text("Read the remaining panel previews")
                    .clicked()
                {
                    pipeline.request_previews();
                }
            }
        });
    }
    let unplaced = plan.unplaced();
    if unplaced > 0 {
        muted(ui, format!("{unplaced} not placed yet"));
    }
    let Some(path) = plan.selected().map(Path::to_path_buf) else {
        return;
    };
    ui.add_space(space::M);
    plan.selection_ui(ui, session);
    if let Some(report) = checks.report() {
        let (items, sources) = check_items(report, cfg, false, |c| c.panels.contains(&path));
        if !items.is_empty() {
            ui.add_space(space::M);
            list(ui, "Checks for this panel", &items, &sources, accept);
        }
    }
}

fn rois_ui(
    ui: &mut egui::Ui,
    session: &mut OpenSession,
    mosaic: &mut MosaicView,
    (draw_block, drawing): (Option<&str>, bool),
    act: &mut Actions,
) {
    if ui
        .add_enabled(
            draw_block.is_none() && !drawing,
            MegaButton::new(ButtonKind::Secondary, "Draw ROI"),
        )
        .on_hover_text(shortcuts::keyed("Draw ROI", shortcuts::DRAW_ROI))
        .on_disabled_hover_text(if drawing {
            DRAW_HINT
        } else {
            draw_block.unwrap_or_default()
        })
        .clicked()
    {
        act.draw_roi = true;
    }
    if drawing {
        muted(ui, DRAW_HINT);
    }
    ui.add_space(space::S);
    act.go_to = mosaic.rois_ui(ui, session);
}

fn composites_ui(
    ui: &mut egui::Ui,
    session: &mut OpenSession,
    (state, shown): (&mut ListState, Option<&ViewKind>),
    save: &mut Debounce,
    act: &mut Actions,
) {
    let groups = group_names(session);
    let list = compositions_of(session);
    if list.is_empty() {
        muted(
            ui,
            if groups.len() < 2 {
                "Composites need two or more groups."
            } else {
                "None yet: + after the group chips makes one."
            },
        );
        return;
    }
    let details: Vec<String> = list
        .iter()
        .map(|c| composition_detail(c, &groups))
        .collect();
    let rows: Vec<ListRow> = list
        .iter()
        .zip(&details)
        .map(|(c, detail)| ListRow {
            id: c.id,
            label: &c.name,
            detail: Some(detail),
            selected: shown == Some(&ViewKind::Composition(c.id)),
        })
        .collect();
    let mut next = list.clone();
    let mut changed = false;
    let events = ordered_list_with(ui, "composites", &rows, state, |ui, id| {
        if let Some(c) = next.iter_mut().find(|c| c.id == id) {
            changed |= channel_editor(ui, c, &groups);
        }
    });
    for event in events {
        match event {
            ListEvent::Select(id) | ListEvent::Activate(id) => {
                act.view = Some(ViewKind::Composition(id));
            }
            ListEvent::Rename(id, name) => {
                if next.iter().any(|c| c.id != id && c.name == name) {
                    act.refused = Some(name);
                } else if let Some(c) = next.iter_mut().find(|c| c.id == id) {
                    c.name = name;
                    changed = true;
                }
            }
            ListEvent::Move { id, to } => {
                if let Some(from) = next.iter().position(|c| c.id == id) {
                    let c = next.remove(from);
                    next.insert(to.min(next.len()), c);
                    changed = true;
                }
            }
            ListEvent::Delete(id) => {
                if let Some(at) = next.iter().position(|c| c.id == id) {
                    act.deleted = Some((at, next.remove(at)));
                    changed = true;
                }
            }
        }
    }
    if changed {
        save_compositions(&mut session.mosaic.config.ui, &next);
        save.touch();
    }
}

fn notes_ui(
    ui: &mut egui::Ui,
    session: &mut OpenSession,
    mode: &mut NotesMode,
    save: &mut Debounce,
) {
    let mut text = notes(&session.mosaic.config.ui);
    let r = notes_view(ui, &mut text, mode, NOTES_HINT, true);
    if r.changed() {
        set_notes(&mut session.mosaic.config.ui, &text);
        save.touch();
    }
}
