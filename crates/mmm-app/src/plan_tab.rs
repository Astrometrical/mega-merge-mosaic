//! The Plan tab (spec §5.3): the plan canvas ([`PlanView`]) with a toolbar
//! and a side panel, until the session window replaces the tabs.
//!
//! - **Toolbar.** The view selector (each group, then each composition,
//!   and "+ composition"; with grouping off there is one view and no
//!   selector), Outlines and Numbers, the session's display stretch toggles
//!   Linked and Strong (saved in `MosaicConfig.ui` under
//!   [`crate::display_stretch::DISPLAY_STRETCH_KEY`]) and the previews'
//!   progress.
//! - **Side panel.** The composition being shown ([`composition_editor`]),
//!   the selected panel's details ([`PlanView::selection_ui`]) and the
//!   checks, filtered to the selected panel.
//! - **Saves.** A stretch toggle or composition change is a small
//!   `mosaic.json` write on the UI thread, rate-limited ([`Debounce`]; the
//!   documented exception).

use std::time::Instant;

use egui::RichText;
use mega_ui::checks::{CheckAction, CheckItem, checks_list};
use mega_ui::theme::tokens::space;
use mega_ui::theme::{Severity as UiSeverity, palette};
use mmm_core::checks::{Check, Severity, is_accepted};
use mmm_core::stretch::StretchParams;

use crate::compositions::{
    Composition, ViewKind, composition_editor, load_compositions, save_compositions,
    unique_composition_name,
};
use crate::display_stretch::DisplayStretch;
use crate::plan_view::{CanvasLook, ItemSnapshot, PlanFeed, PlanView};
use crate::session::OpenSession;
use crate::util::{Debounce, group_names};

/// Width of the side panel.
const SIDE_WIDTH: f32 = 300.0;

/// What the user asked of the shell in a frame.
#[derive(Debug, Default)]
pub struct PlanOutput {
    /// Run the (paused) previews again.
    pub resume_previews: bool,
    /// Accept these checks.
    pub accept: Vec<Check>,
}

/// The Plan tab's state for the open session.
pub struct PlanTab {
    plan: PlanView,
    show_outlines: bool,
    show_numbers: bool,
    show_all_checks: bool,
    save: Debounce,
}

impl Default for PlanTab {
    fn default() -> Self {
        Self::new()
    }
}

impl PlanTab {
    /// A tab showing the first group once drawn.
    pub fn new() -> Self {
        PlanTab {
            plan: PlanView::new(),
            show_outlines: true,
            show_numbers: true,
            show_all_checks: false,
            save: Debounce::default(),
        }
    }

    /// Show `view`.
    pub fn select_view(&mut self, view: ViewKind) {
        self.plan.select_view(view);
    }

    /// The view shown (once drawn).
    pub fn view(&self) -> Option<&ViewKind> {
        self.plan.view()
    }

    /// The selected panel.
    #[cfg(test)]
    pub fn selected(&self) -> Option<&std::path::Path> {
        self.plan.selected()
    }

    /// What the last frame drew, item by item.
    pub fn items(&self) -> &[ItemSnapshot] {
        self.plan.items()
    }

    /// The display stretch of the view, once known.
    pub fn stretch(&self) -> Option<&[StretchParams]> {
        self.plan.stretch_for_view()
    }

    /// See [`PlanView::release_if_hidden`]. Call every frame.
    pub fn release_if_hidden(&mut self, now: Instant) {
        self.plan.release_if_hidden(now);
    }

    /// The shared frame's canvas, once read.
    pub fn frame_canvas(&self) -> Option<(u64, u64)> {
        self.plan.frame_canvas()
    }

    /// See [`PlanView::is_settled`].
    pub fn is_settled(&self) -> bool {
        self.plan.is_settled()
    }

    /// The shown view's key (a group name, or a composition's key).
    pub fn view_key(&self) -> Option<&str> {
        self.plan.view_key()
    }

    /// Save a pending (rate-limited) display stretch or composition change
    /// now (the window is closing). A small `mosaic.json` write.
    pub fn flush_save(&mut self, session: &OpenSession) {
        if !self.save.pending() {
            return;
        }
        self.save.saved(Instant::now());
        if let Err(e) = session.mosaic.save() {
            tracing::warn!("Plan settings not saved: {e}");
        }
    }

    /// Draw the tab.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut OpenSession,
        feed: &PlanFeed<'_>,
    ) -> PlanOutput {
        let ctx = ui.ctx().clone();
        let mut out = PlanOutput::default();
        let grouping = session.mosaic.config.grouping.enabled;
        let groups = group_names(session);
        let compositions = if grouping {
            load_compositions(&session.mosaic.config.ui)
        } else {
            Vec::new()
        };

        egui::Panel::top("plan-toolbar").show(ui, |ui| {
            self.toolbar(
                ui,
                session,
                &groups,
                &compositions,
                grouping,
                feed,
                &mut out,
            );
        });
        egui::Panel::right("plan-side")
            .resizable(false)
            .exact_size(SIDE_WIDTH)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if let Some(ViewKind::Composition(i)) = self.plan.view().cloned()
                        && let Some(c) = compositions.get(i)
                    {
                        self.composition_ui(ui, session, &groups, &compositions, i, c);
                        ui.add_space(space::M);
                    }
                    self.plan.selection_ui(ui, session);
                    ui.add_space(space::M);
                    self.checks_ui(ui, session, feed, &mut out);
                });
            });
        let look = CanvasLook {
            outlines: self.show_outlines,
            numbers: self.show_numbers,
            stretch: DisplayStretch::load(&session.mosaic.config.ui),
        };
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(palette::SURFACE_900))
            .show(ui, |ui| self.plan.show(ui, session, feed, &look));
        self.save_if_due(&ctx, session);
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn toolbar(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut OpenSession,
        groups: &[String],
        compositions: &[Composition],
        grouping: bool,
        feed: &PlanFeed<'_>,
        out: &mut PlanOutput,
    ) {
        ui.horizontal(|ui| {
            if grouping {
                let current = self.plan.view().cloned();
                let shown = match &current {
                    Some(ViewKind::Group(g)) => g.clone(),
                    Some(ViewKind::Composition(i)) => compositions
                        .get(*i)
                        .map(|c| c.name.clone())
                        .unwrap_or_default(),
                    None => String::new(),
                };
                let mut chosen = current.clone();
                let mut add = false;
                egui::ComboBox::from_label("View")
                    .selected_text(shown)
                    .show_ui(ui, |ui| {
                        for g in groups {
                            let v = Some(ViewKind::Group(g.clone()));
                            ui.selectable_value(&mut chosen, v, g);
                        }
                        if !compositions.is_empty() {
                            ui.separator();
                        }
                        for (i, c) in compositions.iter().enumerate() {
                            let v = Some(ViewKind::Composition(i));
                            ui.selectable_value(&mut chosen, v, &c.name);
                        }
                        ui.separator();
                        add = ui.selectable_label(false, "+ composition").clicked();
                    });
                if add {
                    let i = add_composition(session, groups, compositions);
                    self.save.touch();
                    chosen = Some(ViewKind::Composition(i));
                }
                if chosen != current
                    && let Some(view) = chosen
                {
                    self.plan.select_view(view);
                }
                ui.separator();
            }
            ui.checkbox(&mut self.show_outlines, "Outlines")
                .on_hover_text("Panel outlines");
            ui.checkbox(&mut self.show_numbers, "Numbers")
                .on_hover_text("Panel numbers");
            ui.separator();
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
            let total = session.mosaic.config.panels.len();
            if total > 0 && !feed.complete {
                if feed.previews_busy {
                    ui.spinner();
                    ui.label(
                        RichText::new(format!("Previews {} of {total}", feed.previews.len()))
                            .color(palette::TEXT_BODY),
                    );
                } else {
                    ui.label(RichText::new("Previews paused —").color(palette::TEXT_BODY));
                    if ui
                        .button("Resume")
                        .on_hover_text("Build the remaining panel previews")
                        .clicked()
                    {
                        out.resume_previews = true;
                    }
                }
            }
            let unplaced = self.plan.unplaced();
            if unplaced > 0 {
                ui.label(
                    RichText::new(format!("{unplaced} not placed yet")).color(palette::TEXT_MUTED),
                );
            }
        });
    }

    fn composition_ui(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut OpenSession,
        groups: &[String],
        compositions: &[Composition],
        index: usize,
        c: &Composition,
    ) {
        ui.label(RichText::new("Composition").color(palette::TEXT_STRONG));
        let others: Vec<String> = compositions
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != index)
            .map(|(_, o)| o.name.clone())
            .collect();
        let mut edited = c.clone();
        let ui_value = &mut session.mosaic.config.ui;
        let mut list = compositions.to_vec();
        if ui
            .push_id(("composition", index), |ui| {
                composition_editor(ui, &mut edited, groups, &others)
            })
            .inner
        {
            list[index] = edited;
            save_compositions(ui_value, &list);
            self.save.touch();
        }
        if ui.button("Delete composition").clicked() {
            list.remove(index);
            save_compositions(ui_value, &list);
            self.save.touch();
            if let Some(g) = groups.first() {
                self.plan.select_view(ViewKind::Group(g.clone()));
            }
        }
    }

    fn checks_ui(
        &mut self,
        ui: &mut egui::Ui,
        session: &OpenSession,
        feed: &PlanFeed<'_>,
        out: &mut PlanOutput,
    ) {
        let cfg = &session.mosaic.config;
        let selected = self.plan.selected().map(std::path::Path::to_path_buf);
        let filter = selected.clone().filter(|_| !self.show_all_checks);
        let mut items = Vec::new();
        let mut sources: Vec<Option<&Check>> = Vec::new();
        if let Some(report) = feed.checks {
            if filter.is_none() {
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
            for c in &report.checks {
                if filter.as_ref().is_some_and(|p| !c.panels.contains(p)) {
                    continue;
                }
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
        }
        let heading = if selected.is_some() && !self.show_all_checks {
            "Checks for this panel"
        } else {
            "Checks"
        };
        let show_all = selected.is_some().then_some(&mut self.show_all_checks);
        for action in checks_list(ui, heading, &items, show_all) {
            let CheckAction::Accept(i) = action;
            if let Some(Some(c)) = sources.get(i) {
                out.accept.push((*c).clone());
            }
        }
    }

    /// Save a pending change once the rate limit allows (documented
    /// exception: a small `mosaic.json` write on the UI thread).
    fn save_if_due(&mut self, ctx: &egui::Context, session: &OpenSession) {
        let now = Instant::now();
        if self.save.due(now) {
            self.save.saved(now);
            if let Err(e) = session.mosaic.save() {
                tracing::warn!("Plan settings not saved: {e}");
            }
        } else if let Some(wait) = self.save.wait(now) {
            ctx.request_repaint_after(wait);
        }
    }
}

/// Add a new composition (HOO-like defaults from the first groups) to the
/// session's `ui`; returns its index. Not saved (the caller rate-limits).
fn add_composition(
    session: &mut OpenSession,
    groups: &[String],
    compositions: &[Composition],
) -> usize {
    let taken: Vec<String> = compositions.iter().map(|c| c.name.clone()).collect();
    let mut c = Composition::new(unique_composition_name(&taken));
    if !groups.is_empty() {
        let pick = |i: usize| Some((groups[i.min(groups.len() - 1)].clone(), 1.0));
        c.channels = [pick(0), pick(1), pick(2)];
    }
    let mut list = compositions.to_vec();
    list.push(c);
    save_compositions(&mut session.mosaic.config.ui, &list);
    list.len() - 1
}

#[cfg(test)]
#[path = "plan_tab_tests.rs"]
mod tests;
