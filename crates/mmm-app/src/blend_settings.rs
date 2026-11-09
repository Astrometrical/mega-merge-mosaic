//! The merge settings panel (spec §5.4; the session window's Merge
//! settings section, through [`crate::mosaic_view::MosaicView`]): the
//! mosaic-wide merge and analysis settings, with the CLI's value domains.
//!
//! - **Merge fields** (mode, feather, defect veto, flatten) edit a *draft*
//!   kept in `mosaic.json`'s `ui` ([`MERGE_DRAFT_KEY`], [`load_draft`]):
//!   at once, with no Apply or Revert. Nothing is invalidated and no job
//!   is cancelled; the session window saves the draft with its debounced
//!   save ([`SettingsPanel::take_draft_changed`]) and a merge takes it up
//!   (`mmm_core::mosaic::commit_blend_settings`). Typed feather text that
//!   is not a valid width keeps the last valid draft and shows its error;
//!   merging is blocked meanwhile ([`SettingsPanel::input_error`]).
//! - **Analysis fields** (input, surface, gain) apply at once
//!   ([`SettingsPanel::edit_analysis`]) with the pending-edit pattern of
//!   the Panels tab: the jobs in the way (all but exports) are cancelled
//!   and the edit waits until they have stopped; a store held elsewhere
//!   ([`mmm_core::Error::Busy`]) retries after [`BUSY_RETRY`]. The merge
//!   settings of the config are untouched. Each applied change leaves the
//!   previous values for the window's Undo toast
//!   ([`SettingsPanel::take_analysis_undo`]). Jobs cancelled for an edit
//!   count as cancelled *for an edit* ([`SettingsPanel::editing`]) until
//!   every one has been reported, so the automatic pipeline restarts its
//!   work instead of taking them for the user's cancels.
//! - **Changed since the last merge.** A merge field whose draft value
//!   (with a group's override) differs from that group's newest
//!   whole-canvas merge, or an analysis field differing from the group's
//!   last analysis, is marked with a dot in the accent colour; for a
//!   composite, any member group counts ([`changed_fields`]).
//! - **Per-group overrides** (set with the CLI) are respected but not
//!   edited here; the panel names the groups that have them.
//!
//! [`SettingsPanel::edit_analysis`] saves `mosaic.json` and flushes store
//! state tables on the UI thread: the documented exception to "no file I/O
//! on the UI thread" (see [`crate::jobs`]).

use std::time::{Duration, Instant};

use egui::RichText;
use mega_ui::theme::palette;
use mmm_core::analyze::InputSelect;
use mmm_core::blend::BlendMode;
use mmm_core::mosaic::{
    MosaicConfig, last_analyze_settings, last_merge_settings, set_analyze_settings,
};
use mmm_core::photometry::GainMode;
use mmm_core::settings::{AnalyzeSettings, BlendSettings, ChangeImpact, analyze_change};
use serde_json::Value;

use crate::jobs::JobRunner;
use crate::session::OpenSession;

/// Wait before retrying an edit refused because a store was busy.
pub const BUSY_RETRY: Duration = Duration::from_millis(500);
/// Marker of a field changed since the last merge.
pub const CHANGED_MARK: &str = "●";
/// Key in `MosaicConfig.ui` of the merge settings draft.
pub const MERGE_DRAFT_KEY: &str = "merge_draft";

/// The merge settings draft: `ui.merge_draft` if it parses and is valid,
/// else the config's own merge settings.
pub fn load_draft(cfg: &MosaicConfig) -> BlendSettings {
    cfg.ui
        .get(MERGE_DRAFT_KEY)
        .and_then(|v| serde_json::from_value::<BlendSettings>(v.clone()).ok())
        .filter(|b| b.validate().is_ok())
        .unwrap_or_else(|| cfg.blend.clone())
}

/// The draft as `group` would merge with it: [`load_draft`] with the
/// group's override applied (what a transient preview of the group is made
/// with, [`crate::previews`]).
pub fn draft_for(cfg: &MosaicConfig, group: &str) -> BlendSettings {
    let over = cfg
        .group_overrides
        .get(group)
        .map(|o| o.blend.clone())
        .unwrap_or_default();
    load_draft(cfg).with(&over)
}

/// Store `draft` in `ui` (`MosaicConfig.ui`), keeping everything else
/// there (a `ui` that is not a JSON object becomes one).
pub fn save_draft(ui: &mut Value, draft: &BlendSettings) {
    if !ui.is_object() {
        *ui = Value::Object(Default::default());
    }
    if let (Some(obj), Ok(v)) = (ui.as_object_mut(), serde_json::to_value(draft)) {
        obj.insert(MERGE_DRAFT_KEY.to_string(), v);
    }
}

/// A field of the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Field {
    /// Blend mode.
    Mode,
    /// Feather ramp length.
    Feather,
    /// Cross-panel defect veto.
    DefectVeto,
    /// Background flatten order.
    Flatten,
    /// Input interpretation.
    Input,
    /// Residual surface order.
    Surface,
    /// Photometric gain handling.
    Gain,
}

/// The fields that differ from what `groups` were last merged and analysed
/// with: the draft (through each group's override) against the group's
/// newest whole-canvas merge, the shown analysis settings against its last
/// analysis. A group never merged (or analysed) marks nothing.
pub fn changed_fields(
    cfg: &MosaicConfig,
    draft: &BlendSettings,
    analyze: &AnalyzeSettings,
    groups: &[String],
) -> Vec<Field> {
    let mut out = Vec::new();
    for g in groups {
        let over = cfg.group_overrides.get(g).cloned().unwrap_or_default();
        if let Some(last) = last_merge_settings(cfg, g) {
            let b = draft.with(&over.blend);
            for (f, differs) in [
                (Field::Mode, b.mode != last.mode),
                (Field::Feather, b.feather_px != last.feather_px),
                (Field::DefectVeto, b.defect_veto != last.defect_veto),
                (Field::Flatten, b.flatten != last.flatten),
            ] {
                if differs {
                    out.push(f);
                }
            }
        }
        if let Some(last) = last_analyze_settings(cfg, g) {
            let a = analyze.with(&over.analyze);
            for (f, differs) in [
                (Field::Input, a.input != last.input),
                (Field::Surface, a.surface != last.surface),
                (Field::Gain, a.gain != last.gain),
            ] {
                if differs {
                    out.push(f);
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// The merge mode's name, as the settings show it.
pub(crate) fn mode_name(m: BlendMode) -> &'static str {
    match m {
        BlendMode::Pyramid => "Pyramid",
        BlendMode::TwoBand => "TwoBand",
        BlendMode::Feather => "Feather",
    }
}

fn on_off(v: bool) -> &'static str {
    if v { "On" } else { "Off" }
}

fn flatten_name(f: Option<u32>) -> &'static str {
    match f {
        None => "Off",
        Some(1) => "1 (plane)",
        Some(2) => "2 (quadratic)",
        Some(_) => "Other",
    }
}

fn input_name(i: InputSelect) -> &'static str {
    match i {
        InputSelect::Auto => "Auto",
        InputSelect::Aligned => "Aligned",
        InputSelect::Solved => "Solved",
    }
}

fn surface_name(s: Option<u32>) -> &'static str {
    match s {
        None => "Off",
        Some(0) => "0 (constant)",
        Some(1) => "1 (plane)",
        Some(2) => "2 (quadratic)",
        Some(_) => "Other",
    }
}

fn gain_name(g: GainMode) -> &'static str {
    match g {
        GainMode::Fit => "Fit",
        GainMode::Unity => "Unity",
    }
}

/// Feather text as shown in the field.
pub(crate) fn feather_text(v: f32) -> String {
    format!("{v}")
}

/// The panel's state (owned by the merged mosaic view). The merge draft
/// lives in the config's `ui`, not here.
#[derive(Default)]
pub struct SettingsPanel {
    /// The feather field's text while it differs from the draft's value
    /// (being typed, or invalid).
    feather_text: Option<String>,
    /// Why the typed feather is refused.
    feather_error: Option<String>,
    /// An analysis edit waiting for the jobs to stop (or a busy store).
    pending: Option<AnalyzeSettings>,
    /// When to retry a pending edit refused as busy.
    retry_at: Option<Instant>,
    /// A failed edit's message, until the window takes it for a toast.
    problem: Option<String>,
    /// Jobs were cancelled for an analysis edit and have not all been
    /// reported yet (see the module docs).
    cancelled_for_edit: bool,
    /// The analysis settings before the last applied change, until the
    /// window takes them for its Undo toast.
    analysis_undo: Option<AnalyzeSettings>,
    /// The draft changed since the window last asked.
    draft_changed: bool,
}

impl SettingsPanel {
    /// A panel showing the session's settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// An analysis edit waits for the jobs to stop (or a busy store); no
    /// merge may start meanwhile.
    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// For the automatic pipeline: an analysis edit waits to apply, or jobs
    /// cancelled for one (even one put back since) have not all been
    /// reported. The pipeline starts nothing meanwhile and restarts what
    /// the edit cancelled rather than taking it for a user's cancel.
    pub fn editing(&self, cfg: &MosaicConfig) -> bool {
        self.cancelled_for_edit
            || self
                .pending
                .as_ref()
                .is_some_and(|a| analyze_change(&cfg.analyze, a) == ChangeImpact::Reanalyze)
    }

    /// Why the typed feather is refused, if it is.
    #[cfg(test)]
    pub fn feather_error(&self) -> Option<&str> {
        self.feather_error.as_deref()
    }

    /// Pretend the feather text is invalid (tests of the merge guard).
    #[cfg(test)]
    pub fn force_feather_error(&mut self) {
        self.feather_error = Some("Feather must be a number of pixels greater than 0.".into());
    }

    /// Take back [`SettingsPanel::force_feather_error`] (tests).
    #[cfg(test)]
    pub fn clear_feather_error(&mut self) {
        self.feather_error = None;
    }

    /// Why merging is blocked by what is typed, if it is (an invalid
    /// feather width).
    pub fn input_error(&self) -> Option<&'static str> {
        self.feather_error
            .is_some()
            .then_some("Feather width must be a number above 0")
    }

    /// Raise `message` as a problem (the window's Problem toast).
    pub fn set_problem(&mut self, message: String) {
        self.problem = Some(message);
    }

    /// The analysis settings from before the last applied change, once
    /// (for the window's Undo toast).
    pub fn take_analysis_undo(&mut self) -> Option<AnalyzeSettings> {
        self.analysis_undo.take()
    }

    /// Why the last analysis edit failed, or a merge could not commit the
    /// draft ([`SettingsPanel::set_problem`]), once (for the window's
    /// Problem toast).
    pub fn take_problem(&mut self) -> Option<String> {
        self.problem.take()
    }

    /// Whether the draft changed since the last call (the window then
    /// saves `mosaic.json`, rate-limited).
    pub fn take_draft_changed(&mut self) -> bool {
        std::mem::take(&mut self.draft_changed)
    }

    /// Make `draft` the merge settings draft in `cfg.ui` (nothing is
    /// invalidated, nothing is cancelled); the window saves it later.
    pub fn edit_draft(&mut self, cfg: &mut MosaicConfig, draft: BlendSettings) {
        if draft.validate().is_err() || draft == load_draft(cfg) {
            return;
        }
        save_draft(&mut cfg.ui, &draft);
        self.draft_changed = true;
    }

    /// Put `draft` back as the merge settings draft (compare's **Use A**):
    /// feather text being typed, valid or not, gives way to it, so the field
    /// shows its width and nothing typed blocks merging; then as
    /// [`SettingsPanel::edit_draft`].
    pub fn restore_draft(&mut self, cfg: &mut MosaicConfig, draft: BlendSettings) {
        self.feather_text = None;
        self.feather_error = None;
        self.edit_draft(cfg, draft);
    }

    /// Cancel the jobs in the way of `new`: every job but exports when it
    /// reruns the analysis. True if any had to be (the edit then waits for
    /// them to stop).
    fn cancel_for(
        &mut self,
        new: &AnalyzeSettings,
        cfg: &MosaicConfig,
        jobs: &mut JobRunner,
    ) -> bool {
        if analyze_change(&cfg.analyze, new) == ChangeImpact::Reanalyze
            && jobs.busy_except_exports()
        {
            jobs.cancel_all_but_exports();
            self.cancelled_for_edit = true;
            return true;
        }
        false
    }

    /// Apply the analysis settings `new` now with [`set_analyze_settings`],
    /// or, while jobs in its way run, cancel them and keep it waiting
    /// (newer work wins; see [`SettingsPanel::cancel_for`]). The impact
    /// when a change was applied now.
    pub fn edit_analysis(
        &mut self,
        new: AnalyzeSettings,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
    ) -> Option<ChangeImpact> {
        self.pending = None;
        self.retry_at = None;
        self.problem = None;
        if self.cancel_for(&new, &session.mosaic.config, jobs) {
            self.pending = Some(new);
            return None;
        }
        let previous = session.mosaic.config.analyze.clone();
        match set_analyze_settings(&mut session.mosaic, new.clone()) {
            Ok(ChangeImpact::None) => None,
            Ok(impact) => {
                self.analysis_undo = Some(previous);
                Some(impact)
            }
            Err(e) if e.is_busy() => {
                self.pending = Some(new);
                self.retry_at = Some(Instant::now() + BUSY_RETRY);
                None
            }
            Err(e) => {
                let message = format!("Could not change the settings: {e}");
                tracing::warn!("{message}");
                self.problem = Some(message);
                None
            }
        }
    }

    /// Apply the waiting edit now, without waiting for its retry (the
    /// session is being left); call only while no job but an export runs.
    /// One refused again because a store is held elsewhere stays waiting.
    /// The impact when it was applied.
    pub fn apply_waiting(
        &mut self,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
    ) -> Option<ChangeImpact> {
        let edit = self.pending.take()?;
        let applied = self.edit_analysis(edit, session, jobs);
        if self.pending.is_some() {
            tracing::warn!("the settings edit waits: a result is in use elsewhere");
        }
        applied
    }

    /// Apply a waiting edit once the runner is idle. Call every frame after
    /// the runner's poll, whichever tab is shown. The impact when an edit
    /// was applied.
    pub fn poll(
        &mut self,
        ctx: &egui::Context,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
    ) -> Option<ChangeImpact> {
        // Every job cancelled for an edit has been reported (by the
        // runner's poll this frame, before the shell read `editing`).
        if self.cancelled_for_edit && !jobs.busy_except_exports() && !jobs.has_undelivered() {
            self.cancelled_for_edit = false;
        }
        let edit = self.pending.clone()?;
        // Newer work wins, also over jobs started after the edit was
        // queued; exports are spared.
        if self.cancel_for(&edit, &session.mosaic.config, jobs) {
            return None;
        }
        let now = Instant::now();
        match self.retry_at {
            Some(at) if at > now => {
                ctx.request_repaint_after(at - now);
                None
            }
            _ => self.edit_analysis(edit, session, jobs),
        }
    }

    /// Draw the panel (no heading or fold of its own: the inspector's
    /// Merge settings section holds it) for the view showing `groups`; the
    /// impact when an analysis edit was applied now.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
        groups: &[String],
    ) -> Option<ChangeImpact> {
        let cfg = &session.mosaic.config;
        let draft = load_draft(cfg);
        let shown = self.pending.clone().unwrap_or_else(|| cfg.analyze.clone());
        let changed = changed_fields(cfg, &draft, &shown, groups);
        let cost = session.merged;
        let mut edit = draft.clone();
        let mut edit_analyze = shown.clone();
        let caption = |ui: &mut egui::Ui, t: &str| {
            ui.label(RichText::new(t).small().color(palette::TEXT_SUBTLE));
        };

        caption(ui, "MERGE · re-merge only");
        egui::Grid::new("blend-settings-blend")
            .num_columns(3)
            .spacing([6.0, 4.0])
            .show(ui, |ui| {
                let modes = [BlendMode::Pyramid, BlendMode::TwoBand, BlendMode::Feather];
                combo_row(
                    ui,
                    "Mode",
                    changed.contains(&Field::Mode),
                    &mut edit.mode,
                    &modes,
                    mode_name,
                    "Pyramid: multiband base + star-safe seams. TwoBand: feathered base \
                     + star-safe seams. Feather: plain weighted average.",
                );
                self.feather_row(ui, changed.contains(&Field::Feather), &mut edit);
                combo_row(
                    ui,
                    "Defect veto",
                    changed.contains(&Field::DefectVeto),
                    &mut edit.defect_veto,
                    &[true, false],
                    on_off,
                    "Suppress cosmic-ray residue and satellite trails that survive in \
                     one panel of an overlap (TwoBand and Pyramid)",
                );
                combo_row(
                    ui,
                    "Flatten",
                    changed.contains(&Field::Flatten),
                    &mut edit.flatten,
                    &[None, Some(1), Some(2)],
                    flatten_name,
                    "Global background flatten of the merged mosaic (refused on \
                     nebula-heavy mosaics)",
                );
            });
        ui.add_space(6.0);
        caption(ui, "ANALYSIS · reruns analysis, every tile goes stale");
        {
            egui::Grid::new("blend-settings-analyze")
                .num_columns(3)
                .spacing([6.0, 4.0])
                .show(ui, |ui| {
                    combo_row(
                        ui,
                        "Input",
                        changed.contains(&Field::Input),
                        &mut edit_analyze.input,
                        &[InputSelect::Auto, InputSelect::Aligned, InputSelect::Solved],
                        input_name,
                        "How the panels are read: detected, registered full-canvas frames, \
                         or unaligned panels with astrometric solutions",
                    );
                    combo_row(
                        ui,
                        "Surface",
                        changed.contains(&Field::Surface),
                        &mut edit_analyze.surface,
                        &[None, Some(0), Some(1), Some(2)],
                        surface_name,
                        "Residual surface correction order",
                    );
                    combo_row(
                        ui,
                        "Gain",
                        changed.contains(&Field::Gain),
                        &mut edit_analyze.gain,
                        &[GainMode::Fit, GainMode::Unity],
                        gain_name,
                        "Fit per-panel gains from the overlaps, or pin every gain at 1 \
                         (same rig and exposure)",
                    );
                });
        }
        if cost {
            ui.label(
                RichText::new("Re-runs the analysis; the mosaic goes out of date")
                    .small()
                    .color(palette::TEXT_MUTED),
            );
        }
        self.edit_draft(&mut session.mosaic.config, edit);

        for (group, o) in &session.mosaic.config.group_overrides {
            if !o.is_empty() {
                ui.label(
                    RichText::new(format!("{group} has its own overrides (CLI)"))
                        .small()
                        .color(palette::TEXT_MUTED),
                );
            }
        }
        if let Some(e) = &self.feather_error {
            ui.label(RichText::new(e).small().color(palette::HA_400));
        }

        let mut applied = None;
        if edit_analyze != shown {
            applied = self.edit_analysis(edit_analyze, session, jobs);
        }
        if self.pending.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                let text = if self.retry_at.is_some() {
                    "A result is in use by another program; the settings apply when it is free…"
                } else {
                    "Waiting for the running job to stop; the settings apply then…"
                };
                ui.label(RichText::new(text).small().color(palette::TEXT_BODY));
            });
        }
        applied
    }

    /// The feather row: a text field, validated as typed (a refused value
    /// keeps the text and an error, and the draft its last valid width).
    fn feather_row(&mut self, ui: &mut egui::Ui, changed: bool, blend: &mut BlendSettings) {
        mark(ui, changed);
        let label = ui.label(RichText::new("Feather (px)").color(palette::TEXT_BODY));
        let mut text = self
            .feather_text
            .clone()
            .unwrap_or_else(|| feather_text(blend.feather_px));
        let r = ui
            .add(egui::TextEdit::singleline(&mut text).desired_width(80.0))
            .labelled_by(label.id)
            .on_hover_text("Feather ramp length in canvas pixels (greater than 0)");
        if r.changed() {
            let parsed = text.trim().parse::<f32>().ok().map(|v| BlendSettings {
                feather_px: v,
                ..blend.clone()
            });
            match parsed.as_ref().map(BlendSettings::validate) {
                Some(Ok(())) => {
                    blend.feather_px = parsed.expect("parsed").feather_px;
                    self.feather_error = None;
                }
                Some(Err(e)) => self.feather_error = Some(capitalise(&e.to_string())),
                None => {
                    self.feather_error =
                        Some("Feather must be a number of pixels greater than 0.".into())
                }
            }
            self.feather_text = Some(text);
        } else if !r.has_focus() && self.feather_error.is_none() {
            // Not being typed: follow the value.
            self.feather_text = None;
        }
        ui.end_row();
    }
}

/// `s` with its first letter upper case.
fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// The changed-since-last-merge dot (or the room it takes).
fn mark(ui: &mut egui::Ui, changed: bool) {
    if changed {
        ui.label(
            RichText::new(CHANGED_MARK)
                .small()
                .color(palette::ACCENT_400),
        )
        .on_hover_text("Changed since the last merge");
    } else {
        ui.label("");
    }
}

/// One `label  [value ▾]` row choosing among `options`.
#[allow(clippy::too_many_arguments)]
fn combo_row<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    name: &str,
    changed: bool,
    value: &mut T,
    options: &[T],
    show: fn(T) -> &'static str,
    hover: &str,
) {
    mark(ui, changed);
    let label = ui.label(RichText::new(name).color(palette::TEXT_BODY));
    let r = egui::ComboBox::from_id_salt(("blend-settings", name))
        .selected_text(show(*value))
        .width(120.0)
        .show_ui(ui, |ui| {
            for o in options {
                ui.selectable_value(value, *o, show(*o));
            }
        });
    r.response.labelled_by(label.id).on_hover_text(hover);
    ui.end_row();
}

#[cfg(test)]
#[path = "blend_settings_tests.rs"]
mod tests;
