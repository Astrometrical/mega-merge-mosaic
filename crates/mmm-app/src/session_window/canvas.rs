//! The canvas and its view bar: **Plan** / **Mosaic**, a chip per group
//! (its colour, "{group} · {panels}") and per composite (dashed) then
//! **+** — chips only with two or more groups — and at the right the
//! **Outlines**, **ROIs**, **Linked** and **Strong** toggles (on a row of
//! their own when the bar is too narrow). **Mosaic** can be chosen once
//! something is merged or a merge has started. **Linked** is disabled for
//! a mono view ([`SessionWindow::is_mono`]). The stretch
//! toggles are the session's ([`DisplayStretch`] in `MosaicConfig.ui`):
//! both canvases follow them.
//!
//! **Marks and tags** (spec §11): a group chip (a composite's takes its
//! members' worst) shows a [`ChipMark`] (its tone's icon, and the state in
//! its hover text) when its mosaic is out of date ([`out_of_date`]) or its
//! merge settings changed ([`draft_changed`]). Its
//! context menu offers **Merge this group** (committing the draft and
//! merging that group only; off while it merges or merging is blocked). In
//! Mosaic mode a tag over the image's top-left says why a shown group is out
//! of date ([`out_of_date_reason`]) or, quieter, that its settings changed;
//! clicking it reveals Merge settings.

use std::path::Path;

use mega_ui::theme::tokens::space;
use mega_ui::widgets::{ChipMark, ChoiceChip, Segment, choice_chip, segmented_with};
use mmm_core::mosaic::{OutOfDateReason, out_of_date_reason};

use super::shortcuts::{self, keyed, views_hint};
use super::steps::{draft_changed, out_of_date};
use super::{COMPOSITES, CanvasMode, MERGE_SETTINGS, SessionWindow, compositions_of};
use crate::checks::ChecksEngine;
use crate::compositions::{ViewKind, add_composition};
use crate::display_stretch::DisplayStretch;
use crate::jobs::JobRunner;
use crate::mosaic_view::{CanvasTag, view_members};
use crate::panel_edits::{group_colors, load_group_colors};
use crate::pipeline::Pipeline;
use crate::plan_view::{PlanFeed, group_members};
use crate::preview_draw::{LayerSpec, PatchWant};
use crate::previews::Side;
use crate::session::OpenSession;
use crate::tile_source::channel_tints;
use crate::util::group_names;
use mmm_core::stretch::StretchParams;

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
    /// **Merge this group** was chosen on this group's chip.
    merge_group: Option<String>,
}

/// Why a mosaic is out of date, for the canvas tag (spec §11), from
/// [`out_of_date_reason`] (the provenance trail, which outlives
/// `needs_analyze`: the pipeline re-analyses by itself).
fn out_of_date_why(session: &OpenSession, group: &str) -> &'static str {
    match out_of_date_reason(&session.mosaic.config, group) {
        OutOfDateReason::AnalysisSettings => "Out of date — analysis settings changed",
        OutOfDateReason::Panels => "Out of date — panels changed",
        OutOfDateReason::RegionDiffers => "Out of date — region merged with other settings",
        OutOfDateReason::NotMerged => "Not merged yet",
        OutOfDateReason::MergeUnfinished => "Out of date — merge didn't finish",
    }
}

/// The chip mark of the groups `groups` (a composite's members): the
/// worst state among them.
fn mark_of(session: &OpenSession, groups: &[String]) -> Option<ChipMark> {
    if groups.iter().any(|g| out_of_date(session, g)) {
        Some(ChipMark::Attention)
    } else if groups.iter().any(|g| draft_changed(session, g)) {
        Some(ChipMark::Changed)
    } else {
        None
    }
}

/// The tag over the mosaic of the shown `members`: why the first one out
/// of date is, else that the settings changed since the merge.
fn canvas_tag(session: &OpenSession, members: &[String]) -> Option<CanvasTag> {
    if let Some(g) = members.iter().find(|g| out_of_date(session, g)) {
        return Some(CanvasTag {
            text: out_of_date_why(session, g).to_string(),
            quiet: false,
        });
    }
    members
        .iter()
        .any(|g| draft_changed(session, g))
        .then(|| CanvasTag {
            text: "Settings changed since this merge".to_string(),
            quiet: true,
        })
}

/// How a group's patch is drawn on the canvas shown (see `preview_wants`).
struct Recipe {
    group: String,
    tint: [f32; 3],
    /// Shown as the channel mean (a plan composition channel).
    mono: bool,
    stretch: Vec<StretchParams>,
    /// `tint` applies only to a patch of fewer than three channels (the plan
    /// draws colour panels in their own colours).
    mono_patch_only: bool,
}

impl SessionWindow {
    /// The view bar across the top of the canvas area. The choices (Plan /
    /// Mosaic and the chips) and the toggles share one row when they fit
    /// side by side (as measured the frame before); otherwise, and before
    /// anything is measured, the choices wrap onto as many rows as they need
    /// and the toggles take a row of their own, right-aligned. Nothing
    /// overlaps.
    pub(super) fn view_bar(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
        pipeline: &Pipeline,
    ) {
        let groups = group_names(session);
        let mono = self.is_mono(session, pipeline);
        // The mosaic can be shown once something is merged, or a merge
        // has started (its result fills in as it runs).
        let mosaic_ok = session.merged || jobs.blending();
        let mut act = BarActions::default();
        let margin = egui::Margin::symmetric(space::M as i8, space::S as i8);
        egui::Frame::NONE.inner_margin(margin).show(ui, |ui| {
            let one_row = self
                .bar_widths
                .is_some_and(|(left, right)| left + space::L + right <= ui.available_width());
            let mut widths = (0.0, 0.0);
            if one_row {
                ui.horizontal(|ui| {
                    widths.0 = self.choices(ui, session, jobs, &groups, mosaic_ok, &mut act);
                    // Right to left: Strong ends the row.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        widths.1 = self.toggles(ui, session, mono);
                    });
                });
            } else {
                ui.horizontal_wrapped(|ui| {
                    widths.0 = self.choices(ui, session, jobs, &groups, mosaic_ok, &mut act);
                });
                ui.add_space(space::XS);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        widths.1 = self.toggles(ui, session, mono);
                    });
                });
            }
            if self.bar_widths != Some(widths) {
                // Lay the bar out again with what it measured.
                self.bar_widths = Some(widths);
                ui.ctx().request_repaint();
            }
        });
        if let Some(mode) = act.mode {
            self.set_mode(mode);
        }
        if let Some(view) = act.view {
            self.select_view(view);
        }
        if let Some(g) = act.merge_group {
            self.merge_group(session, jobs, &g);
        }
        if act.add_composite {
            let i = add_composition(&mut session.mosaic.config.ui, &groups);
            self.save.touch();
            self.select_view(ViewKind::Composition(i));
            self.inspector.reveal(COMPOSITES);
        }
    }

    /// Plan / Mosaic, then (with two or more groups) a chip per group and
    /// per composite and **+**; what was clicked goes to `act`. Returns the
    /// width they take in one row.
    fn choices(
        &self,
        ui: &mut egui::Ui,
        session: &OpenSession,
        jobs: &JobRunner,
        groups: &[String],
        mosaic_ok: bool,
        act: &mut BarActions,
    ) -> f32 {
        let mut widths = Vec::new();
        let mut extra = 0.0;
        let mut mode = self.mode();
        let switch = keyed("Switch between plan and mosaic", shortcuts::PLAN_MOSAIC);
        let options = [
            Segment {
                value: CanvasMode::Plan,
                label: "Plan",
                disabled: None,
                tooltip: Some(&switch),
            },
            Segment {
                value: CanvasMode::Mosaic,
                label: "Mosaic",
                disabled: (!mosaic_ok).then_some(NOTHING_MERGED),
                tooltip: Some(&switch),
            },
        ];
        let r = segmented_with(ui, &mut mode, &options);
        widths.push(r.rect.width());
        if r.changed() {
            act.mode = Some(mode);
        }
        if groups.len() >= 2 {
            ui.add_space(space::S);
            extra += space::S;
            let colors = group_colors(
                &load_group_colors(&session.mosaic.config.ui),
                groups.iter().map(String::as_str),
            );
            let members = group_members(session, groups);
            let hint = views_hint();
            let mut chip = |ui: &mut egui::Ui, chip: ChoiceChip<'_>| {
                let r = choice_chip(ui, &chip);
                widths.push(r.rect.width());
                r
            };
            for g in groups {
                let n = members.get(g).map_or(0, Vec::len);
                let label = format!("{g} · {n}");
                let view = ViewKind::Group(g.clone());
                let c = ChoiceChip {
                    label: &label,
                    color: colors.get(g).copied(),
                    selected: self.view.as_ref() == Some(&view),
                    dashed: false,
                    mark: mark_of(session, std::slice::from_ref(g)),
                };
                let r = chip(ui, c).on_hover_text(&hint);
                if r.clicked() {
                    act.view = Some(view);
                }
                let why = self.mosaic.merge_group_block_reason(session, jobs, g);
                egui::Popup::context_menu(&r).show(|ui| {
                    let item = ui.add_enabled(why.is_none(), egui::Button::new("Merge this group"));
                    if item
                        .on_hover_text(
                            "Merge this group over the whole canvas with the Merge settings",
                        )
                        .on_disabled_hover_text(why.unwrap_or_default())
                        .clicked()
                    {
                        act.merge_group = Some(g.clone());
                        ui.close();
                    }
                });
            }
            for (i, comp) in compositions_of(session).iter().enumerate() {
                let view = ViewKind::Composition(i);
                let c = ChoiceChip {
                    label: &comp.name,
                    color: None,
                    selected: self.view.as_ref() == Some(&view),
                    dashed: true,
                    mark: mark_of(session, &view_members(&view, &compositions_of(session))),
                };
                if chip(ui, c).on_hover_text(&hint).clicked() {
                    act.view = Some(view);
                }
            }
            let plus = ChoiceChip {
                label: "+",
                color: None,
                selected: false,
                dashed: false,
                mark: None,
            };
            if chip(ui, plus).on_hover_text("New composite").clicked() {
                act.add_composite = true;
            }
        }
        one_row_width(ui, &widths) + extra
    }

    /// The overlay and stretch toggles, laid out right to left. Returns
    /// the width they take.
    fn toggles(&mut self, ui: &mut egui::Ui, session: &mut OpenSession, mono: bool) -> f32 {
        let ui_value = &mut session.mosaic.config.ui;
        let mut stretch = DisplayStretch::load(ui_value);
        let before = stretch;
        let mut widths = Vec::new();
        let mut toggle = |ui: &mut egui::Ui, label: &str, on: bool| {
            let chip = ChoiceChip {
                label,
                color: None,
                selected: on,
                dashed: false,
                mark: None,
            };
            let r = choice_chip(ui, &chip);
            widths.push(r.rect.width());
            r
        };
        let strong_tip = keyed(
            "A stronger automatic stretch that brings out faint gradients",
            shortcuts::STRONG,
        );
        if toggle(ui, "Strong", stretch.strong)
            .on_hover_text(strong_tip)
            .clicked()
        {
            stretch.strong = !stretch.strong;
        }
        let linked = ui
            .add_enabled_ui(!mono, |ui| toggle(ui, "Linked", stretch.linked))
            .inner
            .on_hover_text(keyed(
                "One automatic stretch for every colour channel",
                shortcuts::LINKED,
            ))
            .on_disabled_hover_text(ONE_CHANNEL);
        if linked.clicked() {
            stretch.linked = !stretch.linked;
        }
        ui.add_space(space::S);
        if toggle(ui, "ROIs", self.look.rois)
            .on_hover_text(keyed("Show the regions of interest", shortcuts::ROIS))
            .clicked()
        {
            self.look.rois = !self.look.rois;
        }
        if toggle(ui, "Outlines", self.look.outlines)
            .on_hover_text(keyed(
                "Show the panels' outlines and numbers",
                shortcuts::OUTLINES,
            ))
            .clicked()
        {
            self.look.outlines = !self.look.outlines;
            self.look.numbers = self.look.outlines;
        }
        if stretch != before {
            stretch.save(ui_value);
            self.save.touch();
        }
        one_row_width(ui, &widths) + space::S
    }

    /// The layers each visible ROI's patch is drawn from on the canvas shown
    /// (see `preview_draw`): per ROI in view, its live preview (B) with one
    /// layer per group of the view that has a patch, converted with the
    /// stretch of the canvas beneath (read only); the compared ROI has its
    /// side A too, through the same stretch, and only the side shown is
    /// drawn. Nothing while the ROIs are hidden, the plan draws in another
    /// frame than the analysed one, the canvas has not been drawn, or the
    /// canvas has no stretch yet.
    fn preview_wants(&self, session: &OpenSession) -> Vec<PatchWant> {
        let plan = self.mode() == CanvasMode::Plan;
        if !self.look.rois || (plan && !self.plan.frame_matches(session)) {
            return Vec::new();
        }
        let Some((view, _)) = self.visible_canvas() else {
            return Vec::new();
        };
        let Some(kind) = &self.view else {
            return Vec::new();
        };
        // Per layer: the group, its tint, whether it shows as the channel
        // mean, and the canvas's stretch for it.
        let mut recipe: Vec<Recipe> = Vec::new();
        match kind {
            ViewKind::Group(g) => {
                let stretch = if plan {
                    let same = self.plan.view_key().map(str::to_string)
                        == self.view_key(&session.mosaic.config.ui);
                    self.plan
                        .stretch_for_view()
                        .filter(|_| same)
                        .map(<[_]>::to_vec)
                } else {
                    self.mosaic.member_stretch(g)
                };
                if let Some(s) = stretch {
                    // The plan draws a mono group view in its group colour
                    // (grouping on); the patch beside it must match.
                    let tint = if plan && session.mosaic.config.grouping.enabled {
                        let names = group_names(session);
                        let colours = group_colors(
                            &load_group_colors(&session.mosaic.config.ui),
                            names.iter().map(String::as_str),
                        );
                        colours.get(g).map_or([1.0; 3], |c| {
                            [c.r(), c.g(), c.b()].map(|v| f32::from(v) / 255.0)
                        })
                    } else {
                        [1.0; 3]
                    };
                    recipe.push(Recipe {
                        group: g.clone(),
                        tint,
                        mono: false,
                        stretch: s,
                        mono_patch_only: true,
                    });
                }
            }
            ViewKind::Composition(i) => {
                let Some(c) = compositions_of(session).get(*i).cloned() else {
                    return Vec::new();
                };
                if plan {
                    // The plan stretches each composition channel (its group's
                    // channel mean) on its own.
                    let same = self.plan.view_key().map(str::to_string)
                        == self.view_key(&session.mosaic.config.ui);
                    let Some(stretch) = self.plan.stretch_for_view().filter(|_| same) else {
                        return Vec::new();
                    };
                    for (slot, ch) in c.channels.iter().enumerate().take(3) {
                        let mut only = vec![None, None, None];
                        only[slot] = ch.clone();
                        for (g, tint) in channel_tints(&only, |a, b| a == b) {
                            if let Some(s) = stretch.get(slot) {
                                recipe.push(Recipe {
                                    group: g,
                                    tint,
                                    mono: true,
                                    stretch: vec![*s],
                                    mono_patch_only: false,
                                });
                            }
                        }
                    }
                } else {
                    for (g, tint) in channel_tints(&c.channels, |a, b| a == b) {
                        if let Some(s) = self.mosaic.member_stretch(&g) {
                            recipe.push(Recipe {
                                group: g,
                                tint,
                                mono: false,
                                stretch: s,
                                mono_patch_only: false,
                            });
                        }
                    }
                }
            }
        }
        // A recipe's layer of `patch`.
        let layer = |r: &Recipe, patch: std::sync::Arc<mmm_core::mosaic::PreviewPatch>| {
            // A colour patch keeps its own colours.
            let tint = if r.mono_patch_only && patch.channels >= 3 {
                [1.0; 3]
            } else {
                r.tint
            };
            LayerSpec {
                patch,
                stretch: r.stretch.clone(),
                tint,
                mono: r.mono,
            }
        };
        let compared = self.previews.compare().map(|c| c.roi);
        let mut out = Vec::new();
        for roi in self.mosaic.rois() {
            let clip = roi.canvas.map(|v| v as f64);
            let c = clip;
            if c[0] >= view[2] || c[2] <= view[0] || c[1] >= view[3] || c[3] <= view[1] {
                continue;
            }
            let shown = if compared == Some(roi.id) {
                self.previews.shown_side()
            } else {
                Side::B
            };
            let layers = recipe
                .iter()
                .filter_map(|r| Some(layer(r, self.previews.patch(roi.id, &r.group)?.1)))
                .collect();
            out.push(PatchWant {
                roi: roi.id,
                side: Side::B,
                clip,
                layers,
                shown: shown == Side::B,
            });
            if compared == Some(roi.id) {
                let layers = recipe
                    .iter()
                    .filter_map(|r| Some(layer(r, self.previews.a_patch(roi.id, &r.group)?)))
                    .collect();
                out.push(PatchWant {
                    roi: roi.id,
                    side: Side::A,
                    clip,
                    layers,
                    shown: shown == Side::A,
                });
            }
        }
        out
    }

    /// Hand the canvas shown the patches of its visible ROIs; the other
    /// canvas drops its textures. While the ROIs are not synced with the
    /// current frame (between an analysis applying and their reprojection
    /// landing) it is handed none, a patch could be drawn at an old rect;
    /// its textures are kept meanwhile ([`PatchTextures::withhold`]).
    fn give_patches(&mut self, ctx: &egui::Context, session: &OpenSession) {
        let synced = self.mosaic.rois_synced();
        let wants = self.preview_wants(session);
        match self.mode() {
            CanvasMode::Plan => {
                self.mosaic_patches.patches(ctx, Vec::new());
                let patches = if synced {
                    self.plan_patches.patches(ctx, wants)
                } else {
                    self.plan_patches.withhold()
                };
                self.plan.set_patches(patches);
            }
            CanvasMode::Mosaic => {
                self.plan_patches.patches(ctx, Vec::new());
                let patches = if synced {
                    self.mosaic_patches.patches(ctx, wants)
                } else {
                    self.mosaic_patches.withhold()
                };
                self.mosaic.set_patches(patches);
            }
        }
    }

    /// The canvas filling the rest of `ui`: the plan or the mosaic.
    pub(super) fn canvas_ui(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut OpenSession,
        jobs: &JobRunner,
        pipeline: &Pipeline,
        checks: &ChecksEngine,
    ) {
        self.look.stretch = DisplayStretch::load(&session.mosaic.config.ui);
        self.give_patches(ui.ctx(), session);
        match self.mode() {
            CanvasMode::Plan => {
                let feed = PlanFeed {
                    previews: pipeline.previews(),
                    generation: pipeline.generation(),
                    complete: pipeline.previews_complete(),
                    checks: checks.report(),
                };
                // The ROIs are anchored in the analysed frame: not while the
                // plan draws in another (outlines would be offset, a drawn
                // rect misplaced for good).
                let matches = self.plan.frame_matches(session);
                let overlays = if self.look.rois && matches {
                    self.mosaic.roi_overlays()
                } else {
                    Vec::new()
                };
                self.plan.set_overlays(overlays);
                // A click on the compared preview is its peek, no selection;
                // not while the plan draws in another frame (it shows no ROI
                // or patch there).
                let quiet = self
                    .previews
                    .compare()
                    .and_then(|c| c.rect())
                    .filter(|_| matches);
                self.plan.set_quiet(quiet);
                self.plan.show(ui, session, &feed, &self.look);
                if let Some((rect, canvas)) = self.plan.take_drawn()
                    && matches
                {
                    self.mosaic.on_drawn(session, rect, canvas);
                }
                if !matches {
                    self.plan.set_drawing(false);
                }
                // The plan's selection changed (a click, Esc): a panel just
                // selected opens its details in the inspector; the list
                // follows (drawn already: from the next frame).
                let selected = self.plan.selected().map(Path::to_path_buf);
                if selected != self.selected {
                    match &selected {
                        Some(p) => {
                            self.inspector.reveal(super::PANELS);
                            self.panels.select_only(p, self.panel_list_open);
                        }
                        None => self.panels.clear_selection(),
                    }
                    ui.ctx().request_repaint();
                }
                self.selected = selected;
                // The panel pointed at lights its row, from the next frame.
                let hovered = self.plan.hovered().map(Path::to_path_buf);
                if self.panels.set_highlight(hovered) {
                    ui.ctx().request_repaint();
                }
            }
            CanvasMode::Mosaic => {
                // No plan to point at.
                if self.panels.set_highlight(None) {
                    ui.ctx().request_repaint();
                }
                // The plan's stretch holds a first merge of the view it was
                // worked out for.
                let same_view = self.plan.view_key().map(str::to_string)
                    == self.mosaic.view_key(&session.mosaic.config.ui);
                let plan_stretch = self.plan.stretch_for_view().filter(|_| same_view);
                // A whole-canvas merge of a shown group running says it
                // all: no tag meanwhile (other merges leave it).
                let merging = self
                    .mosaic
                    .members()
                    .iter()
                    .any(|g| jobs.whole_blend_pending(g));
                let tag = if merging {
                    None
                } else {
                    canvas_tag(session, self.mosaic.members())
                };
                self.mosaic.set_tag(tag);
                self.mosaic.show(ui, session, &self.look, plan_stretch);
                if self.mosaic.take_reveal_settings() {
                    self.inspector.reveal(MERGE_SETTINGS);
                }
            }
        }
        // Over the canvas: the ROI labels with controls (Compare A / B).
        self.roi_labels(ui, session);
    }

    /// Whether the view shows one channel, so **Linked** has nothing to
    /// link: a group view whose panels' previews all have one channel or,
    /// with no preview in yet, whose merged result has one. A composite
    /// mixes colour; a view nothing has been read of yet counts as colour.
    pub(super) fn is_mono(&self, session: &OpenSession, pipeline: &Pipeline) -> bool {
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

/// The width of controls `widths` wide side by side in `ui`'s spacing.
fn one_row_width(ui: &egui::Ui, widths: &[f32]) -> f32 {
    let gaps = widths.len().saturating_sub(1) as f32;
    widths.iter().sum::<f32>() + gaps * ui.spacing().item_spacing.x
}
