//! The top bar (spec §5, §6, §15): **‹** (back to the start screen: the
//! shell closes the session; disabled while an export runs, its tooltip
//! says why), the session name, the step strip ([`steps`]) and, at the
//! right, the main button:
//! **Add panels…** while the session has no panels (it opens the panel
//! list and its file dialog), **Merge** while a merge is needed
//! ([`needs_merge`]) followed by the merge settings' summary ("Pyramid ·
//! feather 256 px", which opens Merge settings), else **Export…**.
//!
//! The steps act as navigation: Panels opens or closes the panel list (and
//! reads the previews again once they stopped short), Checks opens
//! Problems (or starts the analysis the pipeline left to the user), Merge
//! merges every group while it is to do (merging or merged, it shows the
//! mosaic), Export opens the Export dialog. A working step's **Cancel**
//! cancels its jobs. An ROI's merge belongs to its ROI: it neither holds
//! **Merge** back nor stops with the Merge step's Cancel.

use egui::{Align, Layout};
use mega_ui::icons;
use mega_ui::theme::text::{Type, text};
use mega_ui::theme::tokens::{CONTROL_HEIGHT, space};
use mega_ui::widgets::{
    ButtonKind, MegaButton, StepState, StripEvent, StripStep, icon_button_of, step_strip,
};

use super::steps::{
    CHECKS_STEP, EXPORT_STEP, MERGE_STEP, PANELS_STEP, is_analysis, is_previews, jobs_where,
    needs_merge, steps,
};
use super::{CanvasMode, MERGE_SETTINGS, PROBLEMS, SessionWindow, shortcuts};
use crate::blend_settings::{feather_text, mode_name};
use crate::checks::ChecksEngine;
use crate::jobs::{JobRunner, JobTarget};
use crate::pipeline::Pipeline;
use crate::session::OpenSession;

/// The ‹ button's tooltip and accessible label.
const BACK: &str = "Back to the start screen";
/// Why ‹ cannot be pressed while exporting.
const EXPORTING: &str = "Exporting: cancel it or wait to go back";
/// Why **Merge** cannot be pressed while merging.
const MERGING: &str = "Merging: the Merge step shows how far it is";

/// What the main button asked for.
enum Main {
    /// Open the panel list and its file dialog.
    AddPanels,
    /// Merge every group.
    Merge,
    /// Open Merge settings.
    Settings,
    /// Open the Export dialog.
    Export,
}

impl SessionWindow {
    /// Why the session cannot be left now, if it cannot: an export is
    /// running or queued (its Cancel is in the step strip). Anything else
    /// running is cancelled on leaving.
    pub fn back_block_reason(jobs: &JobRunner) -> Option<&'static str> {
        jobs.run_of(JobTarget::Export).map(|_| EXPORTING)
    }

    /// The top bar, one row filling `ui`'s width: the main button at the
    /// right first, so it always shows; the rest from the left. Whether ‹
    /// was clicked.
    pub(super) fn top_bar(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
        pipeline: &mut Pipeline,
        checks: &ChecksEngine,
    ) -> bool {
        let strip = steps(session, jobs, pipeline, checks);
        let mut event = None;
        let mut main = None;
        let mut back = false;
        let row = egui::vec2(ui.available_width(), CONTROL_HEIGHT);
        ui.allocate_ui_with_layout(row, Layout::right_to_left(Align::Center), |ui| {
            main = self.main_button(ui, session, jobs);
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                let blocked = Self::back_block_reason(jobs);
                let button = icon_button_of(ButtonKind::Quiet, icons::ARROW_LEFT, BACK);
                back = ui
                    .add_enabled(blocked.is_none(), button)
                    .on_disabled_hover_text(blocked.unwrap_or_default())
                    .clicked();
                ui.add_space(space::XS);
                ui.label(text(Type::Heading, session.name()));
                ui.add_space(space::L);
                event = step_strip(ui, &strip);
            });
        });
        match main {
            Some(Main::AddPanels) => {
                self.panel_list_open = true;
                self.panels.pick_files(ui.ctx());
            }
            Some(Main::Merge) => self.merge(session, jobs),
            Some(Main::Settings) => self.inspector_mut().reveal(MERGE_SETTINGS),
            Some(Main::Export) => self.mosaic.open_export(session),
            None => {}
        }
        match event {
            Some(StripEvent::Open(i)) => self.open_step(i, &strip, session, jobs, pipeline),
            Some(StripEvent::Cancel(i)) => cancel_step(i, jobs),
            None => {}
        }
        back
    }

    /// Why the main button's **Merge** cannot be pressed now, if it cannot
    /// (it shows only while [`needs_merge`]): a whole-canvas merge running,
    /// or [`MosaicView::merge_block_reason`](crate::mosaic_view::MosaicView::merge_block_reason).
    /// An ROI's merge is no reason to wait (it may be cut short).
    fn merge_block(&self, session: &OpenSession, jobs: &JobRunner) -> Option<String> {
        if jobs.run_of(JobTarget::Blend).is_some() {
            Some(MERGING.to_string())
        } else {
            self.mosaic.merge_block_reason(session, jobs)
        }
    }

    /// Whether the main button offers **Merge** and it can be pressed
    /// (what `Ctrl+Enter` presses).
    pub(super) fn can_merge(&self, session: &OpenSession, jobs: &JobRunner) -> bool {
        !session.mosaic.config.panels.is_empty()
            && needs_merge(session)
            && self.merge_block(session, jobs).is_none()
    }

    /// The main button (and, with **Merge**, the settings' summary after
    /// it), laid out right to left; what was clicked.
    fn main_button(
        &self,
        ui: &mut egui::Ui,
        session: &OpenSession,
        jobs: &JobRunner,
    ) -> Option<Main> {
        let mut asked = None;
        if session.mosaic.config.panels.is_empty() {
            if ui
                .add(MegaButton::new(ButtonKind::Primary, "Add panels…"))
                .on_hover_text("Choose the panel files to merge")
                .clicked()
            {
                asked = Some(Main::AddPanels);
            }
        } else if needs_merge(session) {
            let b = &session.mosaic.config.blend;
            let summary = format!(
                "{} · feather {} px",
                mode_name(b.mode),
                feather_text(b.feather_px)
            );
            if ui
                .add(MegaButton::new(ButtonKind::Quiet, summary))
                .on_hover_text("Open Merge settings")
                .clicked()
            {
                asked = Some(Main::Settings);
            }
            let why = self.merge_block(session, jobs);
            let keys = ui.ctx().format_shortcut(&shortcuts::MERGE);
            if ui
                .add_enabled(why.is_none(), MegaButton::new(ButtonKind::Primary, "Merge"))
                .on_hover_text(format!("Merge every group over the whole canvas ({keys})"))
                .on_disabled_hover_text(why.unwrap_or_default())
                .clicked()
            {
                asked = Some(Main::Merge);
            }
        } else if ui
            .add_enabled(
                self.mosaic.can_open_export(),
                MegaButton::new(ButtonKind::Primary, "Export…"),
            )
            .on_hover_text("Export this view's merged groups to FITS or TIFF")
            .clicked()
        {
            asked = Some(Main::Export);
        }
        asked
    }

    /// Step `i` was clicked, `strip` showing.
    fn open_step(
        &mut self,
        i: usize,
        strip: &[StripStep; 4],
        session: &mut OpenSession,
        jobs: &mut JobRunner,
        pipeline: &mut Pipeline,
    ) {
        match i {
            PANELS_STEP => {
                if pipeline.previews_paused() {
                    pipeline.request_previews();
                }
                self.panel_list_open = !self.panel_list_open;
            }
            // Waiting for an analysis nothing will start by itself.
            CHECKS_STEP if strip[i].state == StepState::Next => {
                let id = jobs.start_analyze(session);
                self.track_analysis(id);
            }
            CHECKS_STEP => self.inspector_mut().reveal(PROBLEMS),
            // Merging already, or merged: a look at the mosaic. A status
            // pill never starts an hours-long merge of what is complete.
            MERGE_STEP if matches!(strip[i].state, StepState::Working { .. } | StepState::Done) => {
                self.set_mode(CanvasMode::Mosaic);
            }
            MERGE_STEP => self.merge(session, jobs),
            EXPORT_STEP if self.mosaic.can_open_export() => self.mosaic.open_export(session),
            _ => {}
        }
    }
}

/// Step `i`'s Cancel was clicked: cancel its jobs (running and queued). An
/// ROI's merge is not the Merge step's: it runs on.
fn cancel_step(i: usize, jobs: &mut JobRunner) {
    let views = jobs.views();
    let of = |target: JobTarget| {
        jobs_where(&views, |v| match jobs.target(v.id) {
            Some((JobTarget::Blend, _)) => target == JobTarget::Blend && jobs.whole_canvas(v.id),
            Some((t, _)) => t == target,
            None => false,
        })
    };
    let ids = match i {
        PANELS_STEP => jobs_where(&views, is_previews),
        CHECKS_STEP => jobs_where(&views, is_analysis),
        MERGE_STEP => of(JobTarget::Blend),
        EXPORT_STEP => of(JobTarget::Export),
        _ => Vec::new(),
    };
    for id in ids {
        jobs.cancel(id);
    }
}
