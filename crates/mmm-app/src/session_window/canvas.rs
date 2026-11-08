//! The canvas and its view bar: **Plan** / **Mosaic**, a chip per group
//! (its colour, "{group} · {panels}") and per composite (dashed) then
//! **+** — chips only with two or more groups — and at the right the
//! **Outlines**, **ROIs**, **Linked** and **Strong** toggles. **Linked** is
//! disabled for a mono view ([`SessionWindow::is_mono`]). The stretch
//! toggles are the session's ([`DisplayStretch`] in `MosaicConfig.ui`):
//! both canvases follow them.

use std::path::Path;

use mega_ui::theme::tokens::space;
use mega_ui::widgets::{ChoiceChip, Segment, choice_chip, segmented_with};

use super::{COMPOSITES, CanvasMode, SessionWindow, compositions_of};
use crate::checks::ChecksEngine;
use crate::compositions::{ViewKind, add_composition};
use crate::display_stretch::DisplayStretch;
use crate::panels_tab::{group_colors, load_group_colors};
use crate::pipeline::Pipeline;
use crate::plan_view::{PlanFeed, group_members};
use crate::session::OpenSession;
use crate::util::group_names;

/// Why **Mosaic** cannot be chosen before anything is merged.
const NOTHING_MERGED: &str = "Nothing merged yet";
/// Why **Linked** cannot be chosen for a mono view.
const ONE_CHANNEL: &str = "One channel: nothing to link";

/// What the view bar's controls asked for this frame.
#[derive(Default)]
struct BarActions {
    mode: Option<CanvasMode>,
    view: Option<ViewKind>,
    add_composite: bool,
}

impl SessionWindow {
    /// The view bar across the top of the canvas area.
    pub(super) fn view_bar(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut OpenSession,
        pipeline: &Pipeline,
    ) {
        let groups = group_names(session);
        let compositions = compositions_of(session);
        let mono = self.is_mono(session, pipeline);
        let mut act = BarActions::default();
        let margin = egui::Margin::symmetric(space::M as i8, space::S as i8);
        egui::Frame::NONE.inner_margin(margin).show(ui, |ui| {
            ui.horizontal(|ui| {
                let mut mode = self.mode();
                let options = [
                    Segment {
                        value: CanvasMode::Plan,
                        label: "Plan",
                        disabled: None,
                    },
                    Segment {
                        value: CanvasMode::Mosaic,
                        label: "Mosaic",
                        disabled: (!session.merged).then_some(NOTHING_MERGED),
                    },
                ];
                if segmented_with(ui, &mut mode, &options).changed() {
                    act.mode = Some(mode);
                }
                if groups.len() >= 2 {
                    ui.add_space(space::S);
                    let colors = group_colors(
                        &load_group_colors(&session.mosaic.config.ui),
                        groups.iter().map(String::as_str),
                    );
                    let members = group_members(session, &groups);
                    for g in &groups {
                        let n = members.get(g).map_or(0, Vec::len);
                        let label = format!("{g} · {n}");
                        let view = ViewKind::Group(g.clone());
                        let chip = ChoiceChip {
                            label: &label,
                            color: colors.get(g).copied(),
                            selected: self.view.as_ref() == Some(&view),
                            dashed: false,
                        };
                        if choice_chip(ui, &chip).clicked() {
                            act.view = Some(view);
                        }
                    }
                    for (i, c) in compositions.iter().enumerate() {
                        let view = ViewKind::Composition(i);
                        let chip = ChoiceChip {
                            label: &c.name,
                            color: None,
                            selected: self.view.as_ref() == Some(&view),
                            dashed: true,
                        };
                        if choice_chip(ui, &chip).clicked() {
                            act.view = Some(view);
                        }
                    }
                    let plus = ChoiceChip {
                        label: "+",
                        color: None,
                        selected: false,
                        dashed: false,
                    };
                    if choice_chip(ui, &plus)
                        .on_hover_text("New composite")
                        .clicked()
                    {
                        act.add_composite = true;
                    }
                }
                // Right to left: Strong ends the row.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    self.toggles(ui, session, mono);
                });
            });
        });
        if let Some(mode) = act.mode {
            self.set_mode(mode);
        }
        if let Some(view) = act.view {
            self.select_view(view);
        }
        if act.add_composite {
            let i = add_composition(&mut session.mosaic.config.ui, &groups);
            self.save.touch();
            self.select_view(ViewKind::Composition(i));
            self.inspector.reveal(COMPOSITES);
        }
    }

    /// The overlay and stretch toggles, laid out right to left.
    fn toggles(&mut self, ui: &mut egui::Ui, session: &mut OpenSession, mono: bool) {
        let ui_value = &mut session.mosaic.config.ui;
        let mut stretch = DisplayStretch::load(ui_value);
        let before = stretch;
        let toggle = |ui: &mut egui::Ui, label: &str, on: bool| {
            let chip = ChoiceChip {
                label,
                color: None,
                selected: on,
                dashed: false,
            };
            choice_chip(ui, &chip)
        };
        if toggle(ui, "Strong", stretch.strong)
            .on_hover_text("A stronger automatic stretch that brings out faint gradients")
            .clicked()
        {
            stretch.strong = !stretch.strong;
        }
        let linked = ui
            .add_enabled_ui(!mono, |ui| toggle(ui, "Linked", stretch.linked))
            .inner
            .on_hover_text("One automatic stretch for every colour channel")
            .on_disabled_hover_text(ONE_CHANNEL);
        if linked.clicked() {
            stretch.linked = !stretch.linked;
        }
        ui.add_space(space::S);
        if toggle(ui, "ROIs", self.look.rois)
            .on_hover_text("Show the regions of interest")
            .clicked()
        {
            self.look.rois = !self.look.rois;
        }
        if toggle(ui, "Outlines", self.look.outlines)
            .on_hover_text("Show the panels' outlines and numbers")
            .clicked()
        {
            self.look.outlines = !self.look.outlines;
            self.look.numbers = self.look.outlines;
        }
        if stretch != before {
            stretch.save(ui_value);
            self.save.touch();
        }
    }

    /// The canvas filling the rest of `ui`: the plan or the mosaic.
    pub(super) fn canvas_ui(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut OpenSession,
        pipeline: &Pipeline,
        checks: &ChecksEngine,
    ) {
        self.look.stretch = DisplayStretch::load(&session.mosaic.config.ui);
        match self.mode() {
            CanvasMode::Plan => {
                let feed = PlanFeed {
                    previews: pipeline.previews(),
                    generation: pipeline.generation(),
                    complete: pipeline.previews_complete(),
                    checks: checks.report(),
                };
                self.plan.show(ui, session, &feed, &self.look);
                // A panel just selected: its details open in the inspector.
                let selected = self.plan.selected().map(Path::to_path_buf);
                if selected.is_some() && selected != self.selected {
                    self.inspector.reveal(super::PANELS);
                    ui.ctx().request_repaint();
                }
                self.selected = selected;
            }
            CanvasMode::Mosaic => {
                // The plan's stretch holds a first merge of the view it was
                // worked out for.
                let same_view = self.plan.view_key().map(str::to_string)
                    == self.mosaic.view_key(&session.mosaic.config.ui);
                let plan_stretch = self.plan.stretch_for_view().filter(|_| same_view);
                self.mosaic.show(ui, session, &self.look, plan_stretch);
            }
        }
    }

    /// Whether the view shows one channel, so **Linked** has nothing to
    /// link: a group view whose panels' previews all have one channel or,
    /// with no preview in yet, whose merged result has one. A composite
    /// mixes colour; a view nothing has been read of yet counts as colour.
    fn is_mono(&self, session: &OpenSession, pipeline: &Pipeline) -> bool {
        let Some(ViewKind::Group(g)) = &self.view else {
            return false;
        };
        let members = group_members(session, std::slice::from_ref(g));
        let previews = pipeline.previews();
        let channels: Vec<u32> = members
            .get(g)
            .into_iter()
            .flatten()
            .filter_map(|p| previews.get(p))
            .map(|p| p.channels)
            .collect();
        if !channels.is_empty() {
            return channels.iter().all(|&c| c == 1);
        }
        self.mosaic.group() == Some(g.as_str())
            && self
                .mosaic
                .source()
                .is_some_and(|s| s.tile_geometry().channels == 1)
    }
}
