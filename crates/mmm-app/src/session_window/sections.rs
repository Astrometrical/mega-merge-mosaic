//! The inspector's sections, in their fixed order:
//!
//! - **Problems**: the checks' problems (and what could not be checked),
//!   each with **Accept** where it can be accepted, then **See all checks ›**
//!   for the notes and passed checks too. Its badge counts the unaccepted
//!   problems.
//! - **Panels**: "{n} panels · {g} groups · {canvas}", the previews'
//!   progress (with **Resume** once paused), and the selected panel's
//!   details and checks.
//! - **ROIs**: the ROI list ([`MosaicView::rois_ui`]) and **Merge ROI**.
//!   Drawing an ROI needs the mosaic for now: starting to draw switches the
//!   canvas to it.
//! - **Merge settings** ([`MosaicView::settings_ui`]), then the health.
//! - **Composites**: each composite's editor and **Delete composite**.
//! - **Notes**: the session's notes, saved with the session (debounced).

use std::path::Path;
use std::time::Instant;

use mega_ui::checks::{CheckAction, CheckItem, checks_list};
use mega_ui::inspector::{Badge, Section, inspector};
use mega_ui::theme::text::{Type, text};
use mega_ui::theme::tokens::space;
use mega_ui::theme::{Severity as UiSeverity, Tone};
use mega_ui::widgets::{ButtonKind, MegaButton, NotesMode, notes_view};
use mmm_core::checks::{Check, Severity, is_accepted};
use mmm_core::mosaic::MosaicConfig;

use super::{
    COMPOSITES, CanvasMode, MERGE_SETTINGS, NOTES, PANELS, PROBLEMS, ROIS, SessionOutput,
    SessionWindow, compositions_of,
};
use crate::checks::{ChecksEngine, ChecksReport};
use crate::compositions::{ViewKind, composition_editor, save_compositions};
use crate::jobs::JobRunner;
use crate::mosaic_view::MosaicView;
use crate::pipeline::{Pipeline, Step};
use crate::plan_view::PlanView;
use crate::session::{OpenSession, notes, set_notes};
use crate::status::format_canvas;
use crate::util::{Debounce, group_names};

/// The notes text box's hint (as on the start screen).
const NOTES_HINT: &str = "Notes about this session";

/// What the sections asked of the window this frame.
#[derive(Default)]
struct Actions {
    mode: Option<CanvasMode>,
    /// The composite at this index was deleted.
    deleted: Option<usize>,
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
        let mode = self.mode();
        let mut act = Actions::default();
        let SessionWindow {
            plan,
            mosaic,
            inspector: state,
            show_all_checks,
            notes_mode,
            save,
            ..
        } = self;
        inspector(ui, state, &sections, |ui, id| {
            // Each body under its own id: the inspector draws them all in
            // like-identified scopes.
            ui.push_id(id, |ui| match id {
                PROBLEMS => problems_ui(ui, session, checks, show_all_checks, &mut out.accept),
                PANELS => panels_ui(ui, session, plan, pipeline, checks, &mut out.accept),
                ROIS => rois_ui(ui, session, jobs, mosaic, mode, &mut act),
                MERGE_SETTINGS => {
                    mosaic.settings_ui(ui, session, jobs);
                    ui.add_space(space::L);
                    mosaic.health_ui(ui);
                }
                COMPOSITES => composites_ui(ui, session, save, &mut act),
                NOTES => notes_ui(ui, session, notes_mode, save),
                _ => {}
            });
        });
        if let Some(mode) = act.mode {
            self.set_mode(mode);
        }
        if let Some(i) = act.deleted {
            self.composite_deleted(session, i);
        }
    }

    /// Composite `index` is gone: a view of it shows the first group, a view
    /// of a later one follows it down the list.
    fn composite_deleted(&mut self, session: &OpenSession, index: usize) {
        let next = match &self.view {
            Some(ViewKind::Composition(i)) if *i == index => {
                group_names(session).first().cloned().map(ViewKind::Group)
            }
            Some(ViewKind::Composition(i)) if *i > index => Some(ViewKind::Composition(i - 1)),
            _ => None,
        };
        if let Some(view) = next {
            self.select_view(view);
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
fn counted(n: usize, noun: &str) -> String {
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
    for CheckAction::Accept(i) in checks_list(ui, heading, items, None) {
        if let Some(Some(c)) = sources.get(i) {
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
    jobs: &mut JobRunner,
    mosaic: &mut MosaicView,
    mode: CanvasMode,
    act: &mut Actions,
) {
    let drawing = mosaic.drawing();
    mosaic.rois_ui(ui, session);
    // ROIs are drawn on the mosaic (until the plan takes them).
    if !drawing && mosaic.drawing() && mode == CanvasMode::Plan {
        act.mode = Some(CanvasMode::Mosaic);
    }
    ui.add_space(space::S);
    let why = mosaic.merge_roi_block_reason(session, jobs);
    if ui
        .add_enabled(
            why.is_none(),
            MegaButton::new(ButtonKind::Secondary, "Merge ROI"),
        )
        .on_hover_text("Merge the selected region of interest for this view's groups")
        .on_disabled_hover_text(why.unwrap_or_default())
        .clicked()
    {
        mosaic.merge_roi(session, jobs);
        act.mode = Some(CanvasMode::Mosaic);
    }
}

fn composites_ui(
    ui: &mut egui::Ui,
    session: &mut OpenSession,
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
    let mut next = list.clone();
    let mut changed = false;
    for (i, edited) in next.iter_mut().enumerate() {
        if i > 0 {
            ui.separator();
        }
        let others: Vec<String> = list
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, o)| o.name.clone())
            .collect();
        ui.push_id(("composite", i), |ui| {
            if composition_editor(ui, edited, &groups, &others) {
                changed = true;
            }
            if ui
                .add(MegaButton::new(ButtonKind::Danger, "Delete composite"))
                .clicked()
            {
                act.deleted = Some(i);
            }
        });
    }
    if let Some(i) = act.deleted {
        next.remove(i);
        changed = true;
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
