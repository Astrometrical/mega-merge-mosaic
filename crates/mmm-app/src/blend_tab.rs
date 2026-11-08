//! The Blend tab: the merged mosaic canvas ([`MosaicView`]) with a toolbar
//! and a side panel, until the session window replaces the tabs.
//!
//! - **Toolbar.** The view selector (each group, then each composition),
//!   the session's display stretch toggles Linked and Strong (saved in
//!   `MosaicConfig.ui` under
//!   [`crate::display_stretch::DISPLAY_STRETCH_KEY`], shared with the Plan
//!   tab), the merge buttons (disabled with the view's reason,
//!   [`MosaicView::merge_block_reason`]), Export… and the running job.
//!   There is no Analyze button: the automatic pipeline analyses.
//! - **Side panel.** The merge settings, the ROI list and the health.
//! - **Saves.** A stretch toggle is a small `mosaic.json` write on the UI
//!   thread, rate-limited ([`Debounce`]; the documented exception).

use std::time::Instant;

use egui::RichText;
use mega_ui::theme::palette;
use mega_ui::theme::tokens::space;
use mmm_core::health::Health;
use mmm_core::settings::ChangeImpact;
use mmm_core::stretch::StretchParams;
use serde_json::Value;

use crate::compositions::{Composition, ViewKind, load_compositions};
use crate::display_stretch::DisplayStretch;
use crate::jobs::{JobEvent, JobRunner};
use crate::mosaic_view::MosaicView;
use crate::plan_view::CanvasLook;
use crate::rois::Roi;
use crate::session::OpenSession;
use crate::tile_source::MosaicTileSource;
use crate::util::{Debounce, group_names};

/// Width of the side panel.
const SIDE_WIDTH: f32 = 260.0;

/// The Blend tab's state for the open session.
pub struct BlendTab {
    mosaic: MosaicView,
    save: Debounce,
}

impl Default for BlendTab {
    fn default() -> Self {
        Self::new()
    }
}

impl BlendTab {
    /// A tab showing nothing yet; the first group opens on the first frame.
    pub fn new() -> Self {
        BlendTab {
            mosaic: MosaicView::new(),
            save: Debounce::default(),
        }
    }

    /// The group whose health is shown (in a composition view, one of its
    /// groups).
    pub fn group(&self) -> Option<&str> {
        self.mosaic.group()
    }

    /// The view shown.
    pub fn view(&self) -> Option<&ViewKind> {
        self.mosaic.view()
    }

    /// Show `view` from the next frame on.
    pub fn select_view(&mut self, view: ViewKind) {
        self.mosaic.select_view(view);
    }

    /// See [`MosaicView::source`].
    pub fn source(&self) -> Option<&std::sync::Arc<MosaicTileSource>> {
        self.mosaic.source()
    }

    /// See [`MosaicView::absent`].
    pub fn absent(&self) -> Option<&str> {
        self.mosaic.absent()
    }

    /// See [`MosaicView::health`].
    pub fn health(&self) -> Option<&Result<Health, String>> {
        self.mosaic.health()
    }

    /// The session's ROIs.
    pub fn rois(&self) -> &[Roi] {
        self.mosaic.rois()
    }

    /// See [`MosaicView::shown_text`].
    pub fn shown_text(&self) -> Option<String> {
        self.mosaic.shown_text()
    }

    /// See [`MosaicView::is_settled`].
    pub fn is_settled(&self) -> bool {
        self.mosaic.is_settled()
    }

    /// See [`MosaicView::view_key`].
    pub fn view_key(&self, ui: &Value) -> Option<String> {
        self.mosaic.view_key(ui)
    }

    /// See [`MosaicView::track_analysis`].
    pub fn track_analysis(&mut self, id: u64) {
        self.mosaic.track_analysis(id);
    }

    /// See [`MosaicView::on_event`].
    pub fn on_event(&mut self, event: &JobEvent) {
        self.mosaic.on_event(event);
    }

    /// See [`MosaicView::settings_editing`].
    pub fn settings_editing(&self, session: &OpenSession) -> bool {
        self.mosaic.settings_editing(session)
    }

    /// See [`MosaicView::poll`]. Call every frame, whichever tab is shown.
    pub fn poll(&mut self, ctx: &egui::Context, session: &mut OpenSession, jobs: &mut JobRunner) {
        self.mosaic.poll(ctx, session, jobs);
    }

    /// See [`MosaicView::poll_settings`]. Call every frame, whichever tab
    /// is shown.
    pub fn poll_settings(
        &mut self,
        ctx: &egui::Context,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
    ) -> Option<ChangeImpact> {
        self.mosaic.poll_settings(ctx, session, jobs)
    }

    /// Save a pending (rate-limited) stretch toggle or ROI change now (the
    /// window is closing). A small `mosaic.json` write.
    pub fn flush_save(&mut self, session: &OpenSession) {
        if self.save.pending() {
            self.save.saved(Instant::now());
            if let Err(e) = session.mosaic.save() {
                tracing::warn!("display stretch not saved: {e}");
            }
        }
        self.mosaic.flush_save(session);
    }

    /// Mark a change as unsaved (tests).
    #[cfg(test)]
    pub(crate) fn touch_save(&mut self) {
        self.save.touch();
    }

    /// Draw the tab. `plan_stretch` is the plan's stretch of the same view
    /// (see [`MosaicView::show`]).
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
        plan_stretch: Option<&[StretchParams]>,
    ) {
        let ctx = ui.ctx().clone();
        let groups = group_names(session);
        let compositions = load_compositions(&session.mosaic.config.ui);
        egui::Panel::top("blend-toolbar").show(ui, |ui| {
            self.toolbar(ui, session, jobs, &groups, &compositions);
        });
        egui::Panel::right("blend-side")
            .resizable(false)
            .exact_size(SIDE_WIDTH)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    self.mosaic.settings_ui(ui, session, jobs);
                    ui.add_space(space::L);
                    self.mosaic.rois_ui(ui, session);
                    ui.add_space(space::L);
                    self.mosaic.health_ui(ui);
                });
            });
        let look = CanvasLook {
            stretch: DisplayStretch::load(&session.mosaic.config.ui),
            ..CanvasLook::default()
        };
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(palette::SURFACE_900))
            .show(ui, |ui| {
                self.mosaic.show(ui, session, jobs, &look, plan_stretch);
            });
        self.mosaic.export_ui(&ctx, session, jobs);
        self.save_if_due(&ctx, session);
    }

    fn toolbar(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
        groups: &[String],
        compositions: &[Composition],
    ) {
        ui.horizontal(|ui| {
            if groups.len() > 1 || !compositions.is_empty() {
                let current = self.mosaic.view().cloned();
                let shown = match &current {
                    Some(ViewKind::Group(g)) => g.clone(),
                    Some(ViewKind::Composition(i)) => compositions
                        .get(*i)
                        .map(|c| c.name.clone())
                        .unwrap_or_default(),
                    None => String::new(),
                };
                let mut chosen = current.clone();
                egui::ComboBox::from_label("View")
                    .selected_text(shown)
                    .show_ui(ui, |ui| {
                        for g in groups {
                            ui.selectable_value(&mut chosen, Some(ViewKind::Group(g.clone())), g);
                        }
                        if !compositions.is_empty() {
                            ui.separator();
                        }
                        for (i, c) in compositions.iter().enumerate() {
                            let v = Some(ViewKind::Composition(i));
                            ui.selectable_value(&mut chosen, v, &c.name);
                        }
                    });
                if chosen != current
                    && let Some(v) = chosen
                {
                    self.mosaic.select_view(v);
                }
                ui.separator();
            }
            let ui_value = &mut session.mosaic.config.ui;
            let mut stretch = DisplayStretch::load(ui_value);
            let before = stretch;
            ui.checkbox(&mut stretch.linked, "Linked")
                .on_hover_text("One automatic stretch for every colour channel");
            ui.checkbox(&mut stretch.strong, "Strong")
                .on_hover_text("A stronger automatic stretch that brings out faint gradients");
            if stretch != before {
                stretch.save(ui_value);
                self.save.touch();
            }
            ui.separator();
            let why = self.mosaic.merge_block_reason(session, jobs);
            if ui
                .add_enabled(why.is_none(), egui::Button::new("Blend whole mosaic"))
                .on_hover_text("Blend every group over the whole canvas")
                .on_disabled_hover_text(why.unwrap_or_default())
                .clicked()
            {
                self.mosaic.merge_all(session, jobs);
            }
            let why = self.mosaic.merge_roi_block_reason(session, jobs);
            if ui
                .add_enabled(why.is_none(), egui::Button::new("Blend ROI"))
                .on_hover_text("Blend the selected region of interest for this view's groups")
                .on_disabled_hover_text(why.unwrap_or_default())
                .clicked()
            {
                self.mosaic.merge_roi(session, jobs);
            }
            if ui
                .add_enabled(self.mosaic.can_open_export(), egui::Button::new("Export…"))
                .on_hover_text("Export this view's blended groups to FITS or TIFF")
                .clicked()
            {
                self.mosaic.open_export(session);
            }
            let views = jobs.views();
            if ui
                .add_enabled(jobs.busy(), egui::Button::new("Cancel"))
                .clicked()
            {
                for v in &views {
                    jobs.cancel(v.id);
                }
            }
            if let Some(v) = views.first() {
                ui.spinner();
                let pct = (v.done * 100)
                    .checked_div(v.total)
                    .map(|p| format!(" {p}%"))
                    .unwrap_or_default();
                ui.label(
                    RichText::new(format!("{} — {}{pct}", v.label, v.stage))
                        .color(palette::TEXT_BODY),
                );
            }
        });
    }

    /// Save a pending stretch toggle once the rate limit allows (documented
    /// exception: a small `mosaic.json` write on the UI thread).
    fn save_if_due(&mut self, ctx: &egui::Context, session: &OpenSession) {
        let now = Instant::now();
        if self.save.due(now) {
            self.save.saved(now);
            if let Err(e) = session.mosaic.save() {
                tracing::warn!("display stretch not saved: {e}");
            }
        } else if let Some(wait) = self.save.wait(now) {
            ctx.request_repaint_after(wait);
        }
    }
}

#[cfg(test)]
#[path = "blend_tab_tests.rs"]
mod tests;
