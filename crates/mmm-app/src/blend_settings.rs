//! The Blend tab's **Blend settings** panel (spec §5.4): the mosaic-wide
//! analyze and blend settings, exactly the `mmm analyze` / `mmm blend`
//! options the session keeps ([`AnalyzeSettings`], [`BlendSettings`]), with
//! the CLI's value domains.
//!
//! - **Two kinds.** Mode, feather, defect veto and flatten are
//!   *blend-local*: analysis is kept, existing results go stale and a
//!   reblend (ROI or whole) shows the change. Input, surface and gain are
//!   *global*: analysis reruns (the automatic pipeline starts it) and every
//!   result goes stale. The panel captions each kind.
//! - **Staged edits.** Changes are a draft until **Apply**; **Revert**
//!   discards them. While a draft differs, the panel says what applying
//!   costs ("… (N of M tiles)"): N valid level-0 tiles of the group results
//!   that would go stale, of M. The counts are read on a worker
//!   ([`count_tiles`]), only while no job but an export runs; while a count
//!   runs no blend may start ([`SettingsPanel::idle`]), so the count's
//!   [`TileReader`] never maps a store a blend recreates.
//! - **Applying** uses [`set_settings`] with the pending-edit pattern of
//!   the Panels tab: the jobs in the way are cancelled and the edit waits
//!   until they have stopped — blends only for a blend-local change
//!   ([`set_settings`] needs just the stores' writer locks), every job but
//!   exports for a global one; a store held elsewhere
//!   ([`mmm_core::Error::Busy`]) retries after [`BUSY_RETRY`]. Jobs
//!   cancelled for a global edit count as cancelled *for an edit*
//!   ([`SettingsPanel::editing`]) until every one has been reported, even
//!   if the edit is reverted meanwhile, so the automatic pipeline restarts
//!   its work instead of taking them for the user's cancels.
//! - **Changed since the last blend.** A field whose shown value differs
//!   from the settings the last blend used (from provenance; see
//!   [`last_blend`]) is marked with a dot in the accent colour.
//! - **Per-group overrides** (set with the CLI) are respected but not
//!   edited here; the panel names the groups that have them.
//!
//! [`set_settings`] saves `mosaic.json` and flushes store state tables on
//! the UI thread: the documented exception to "no file I/O on the UI
//! thread" (see [`crate::jobs`]).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::RichText;
use mega_ui::theme::palette;
use mmm_core::analyze::InputSelect;
use mmm_core::blend::BlendMode;
use mmm_core::mosaic::{MosaicConfig, result_store_paths, set_settings};
use mmm_core::photometry::GainMode;
use mmm_core::settings::{
    AnalyzeSettings, BlendSettings, ChangeImpact, analyze_change, blend_change,
};
use mmm_core::tiles::TileReader;

use crate::jobs::{JobEvent, JobRunner};
use crate::session::OpenSession;
use crate::util::{Poll, Task};

/// Wait before retrying an edit refused because a store was busy.
pub const BUSY_RETRY: Duration = Duration::from_millis(500);
/// Marker of a field changed since the last blend.
pub const CHANGED_MARK: &str = "●";

/// A mosaic-wide settings edit: both kinds together, applied at once.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsEdit {
    /// Global (analysis) settings.
    pub analyze: AnalyzeSettings,
    /// Blend-local settings.
    pub blend: BlendSettings,
}

impl SettingsEdit {
    /// The mosaic-wide settings of `cfg`.
    pub fn of(cfg: &MosaicConfig) -> Self {
        SettingsEdit {
            analyze: cfg.analyze.clone(),
            blend: cfg.blend.clone(),
        }
    }

    /// What applying this edit to `cfg` invalidates.
    pub fn impact(&self, cfg: &MosaicConfig) -> ChangeImpact {
        analyze_change(&cfg.analyze, &self.analyze).max(blend_change(&cfg.blend, &self.blend))
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

/// The settings the last blend used, from provenance: the blend settings
/// it recorded, and the analyze settings of the last analysis before it
/// (`None` if that is not recorded). Only blends of groups without
/// overrides count (theirs are the mosaic-wide settings); `None` when there
/// is no such blend.
pub fn last_blend(cfg: &MosaicConfig) -> Option<(Option<AnalyzeSettings>, BlendSettings)> {
    let prov = &cfg.provenance;
    let (at, group, blend) = prov.iter().enumerate().rev().find_map(|(i, p)| {
        if p.action != "blend" {
            return None;
        }
        let group = p.group.clone()?;
        if cfg
            .group_overrides
            .get(&group)
            .is_some_and(|o| !o.is_empty())
        {
            return None;
        }
        let blend: BlendSettings = serde_json::from_value(p.detail["settings"].clone()).ok()?;
        Some((i, group, blend))
    })?;
    let analyze = prov[..at]
        .iter()
        .rev()
        .find(|p| p.action == "analyze")
        .and_then(|p| serde_json::from_value(p.detail["groups"][&group].clone()).ok());
    Some((analyze, blend))
}

/// The fields of `shown` that differ from the last blend's settings
/// (nothing when nothing was blended).
pub fn changed_fields(cfg: &MosaicConfig, shown: &SettingsEdit) -> Vec<Field> {
    let Some((analyze, blend)) = last_blend(cfg) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let b = &shown.blend;
    for (f, differs) in [
        (Field::Mode, b.mode != blend.mode),
        (Field::Feather, b.feather_px != blend.feather_px),
        (Field::DefectVeto, b.defect_veto != blend.defect_veto),
        (Field::Flatten, b.flatten != blend.flatten),
    ] {
        if differs {
            out.push(f);
        }
    }
    if let Some(a) = analyze {
        let s = &shown.analyze;
        for (f, differs) in [
            (Field::Input, s.input != a.input),
            (Field::Surface, s.surface != a.surface),
            (Field::Gain, s.gain != a.gain),
        ] {
            if differs {
                out.push(f);
            }
        }
    }
    out
}

/// Valid and total level-0 tiles over the stores at `paths` (blocking:
/// maps each store; run on a worker). Missing or unreadable stores count
/// as nothing.
pub fn count_tiles(paths: &[PathBuf]) -> (usize, usize) {
    let mut valid = 0;
    let mut total = 0;
    for p in paths {
        let Ok(r) = TileReader::open(p) else { continue };
        let (nx, ny) = r.geometry().tiles(0);
        for ty in 0..ny {
            for tx in 0..nx {
                total += 1;
                if r.state(0, tx, ty) == mmm_core::tiles::TileState::Valid {
                    valid += 1;
                }
            }
        }
    }
    (valid, total)
}

/// What applying an edit costs, for the user. `auto`: automatic analysis
/// is on (a global change then reruns it by itself).
pub fn cost_text(impact: ChangeImpact, counts: Option<(usize, usize)>, auto: bool) -> String {
    let tiles = match counts {
        None => " (counting tiles…)".to_string(),
        Some((_, 0)) => String::new(),
        Some((n, m)) => format!(" ({n} of {m} tiles)"),
    };
    match impact {
        ChangeImpact::None => String::new(),
        ChangeImpact::Reblend => {
            format!("Existing results go stale; reblend to see the change{tiles}.")
        }
        ChangeImpact::Reanalyze if auto => {
            format!("Analysis reruns and every result goes stale{tiles}.")
        }
        ChangeImpact::Reanalyze => {
            format!("Analysis will need to rerun and every result goes stale{tiles}.")
        }
    }
}

fn mode_name(m: BlendMode) -> &'static str {
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
fn feather_text(v: f32) -> String {
    format!("{v}")
}

/// The panel's state (owned by the Blend tab).
#[derive(Default)]
pub struct SettingsPanel {
    /// Staged edits (`None`: the session's settings are shown).
    draft: Option<SettingsEdit>,
    /// The feather field's text while it differs from the draft's value
    /// (being typed, or invalid).
    feather_text: Option<String>,
    /// Why the typed feather is refused.
    feather_error: Option<String>,
    /// An applied edit waiting for the jobs to stop (or a busy store).
    pending: Option<SettingsEdit>,
    /// When to retry a pending edit refused as busy.
    retry_at: Option<Instant>,
    /// The last error, for the user.
    error: Option<String>,
    /// The last applied change, until the next edit (for a short note).
    applied: Option<ChangeImpact>,
    /// Valid and total level-0 tiles of the results, when counted.
    counts: Option<(usize, usize)>,
    count_task: Option<Task<(usize, usize)>>,
    /// The counts are out of date.
    stale_counts: bool,
    /// Jobs were cancelled for a global edit and have not all been
    /// reported yet (kept across a Revert; see the module docs).
    cancelled_for_edit: bool,
}

impl SettingsPanel {
    /// A panel showing the session's settings.
    pub fn new() -> Self {
        SettingsPanel {
            stale_counts: true,
            ..Default::default()
        }
    }

    /// An applied edit waits for the jobs to stop (or a busy store); no
    /// blend may start meanwhile.
    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// For the automatic pipeline: a global edit waits to apply, or jobs
    /// cancelled for one (even one reverted since) have not all been
    /// reported. The pipeline starts nothing meanwhile and restarts what
    /// the edit cancelled rather than taking it for a user's cancel.
    pub fn editing(&self, cfg: &MosaicConfig) -> bool {
        self.cancelled_for_edit
            || self
                .pending
                .as_ref()
                .is_some_and(|e| e.impact(cfg) == ChangeImpact::Reanalyze)
    }

    /// Cancel the jobs in the way of `edit`: every blend for a blend-local
    /// edit, every job but exports for a global one. True if any had to be
    /// (the edit then waits for them to stop).
    fn cancel_for(
        &mut self,
        edit: &SettingsEdit,
        cfg: &MosaicConfig,
        jobs: &mut JobRunner,
    ) -> bool {
        if edit.impact(cfg) == ChangeImpact::Reanalyze {
            if jobs.busy_except_exports() {
                jobs.cancel_all_but_exports();
                self.cancelled_for_edit = true;
                return true;
            }
        } else if jobs.blending() {
            jobs.cancel_blends();
            return true;
        }
        false
    }

    /// No tile count is running (a blend may start: see the module docs).
    pub fn idle(&self) -> bool {
        self.count_task.is_none()
    }

    /// The settings shown: the pending edit, else the draft, else the
    /// session's.
    pub fn shown(&self, cfg: &MosaicConfig) -> SettingsEdit {
        self.pending
            .clone()
            .or_else(|| self.draft.clone())
            .unwrap_or_else(|| SettingsEdit::of(cfg))
    }

    /// The staged edits, if any.
    #[cfg(test)]
    pub fn draft(&self) -> Option<&SettingsEdit> {
        self.draft.as_ref()
    }

    /// The tile counts, once counted.
    #[cfg(test)]
    pub fn counts(&self) -> Option<(usize, usize)> {
        self.counts
    }

    /// Why the typed feather is refused, if it is.
    #[cfg(test)]
    pub fn feather_error(&self) -> Option<&str> {
        self.feather_error.as_deref()
    }

    /// Stage `edit` (a draft equal to the session's settings is dropped).
    pub fn stage(&mut self, edit: SettingsEdit, cfg: &MosaicConfig) {
        self.applied = None;
        self.error = None;
        self.draft = (edit != SettingsEdit::of(cfg)).then_some(edit);
    }

    /// Discard the staged edits and any edit waiting to apply.
    pub fn revert(&mut self) {
        self.draft = None;
        self.pending = None;
        self.retry_at = None;
        self.feather_text = None;
        self.feather_error = None;
        self.error = None;
    }

    /// A job ended: the results (and so the counts) may have changed.
    pub fn on_event(&mut self, event: &JobEvent) {
        if !matches!(event, JobEvent::Preview(_)) {
            self.stale_counts = true;
        }
    }

    /// Apply the draft (see [`SettingsPanel::submit`]).
    pub fn apply(
        &mut self,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
    ) -> Option<ChangeImpact> {
        if self.feather_error.is_some() {
            return None;
        }
        let edit = self.draft.take()?;
        self.submit(edit, session, jobs)
    }

    /// Apply `edit` with [`set_settings`], or, while jobs in its way run,
    /// cancel them and keep it waiting (newer work wins; see
    /// [`SettingsPanel::cancel_for`]). The impact when it was applied now.
    fn submit(
        &mut self,
        edit: SettingsEdit,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
    ) -> Option<ChangeImpact> {
        self.pending = None;
        self.retry_at = None;
        self.error = None;
        if self.cancel_for(&edit, &session.mosaic.config, jobs) {
            self.pending = Some(edit);
            return None;
        }
        match set_settings(
            &mut session.mosaic,
            edit.analyze.clone(),
            edit.blend.clone(),
        ) {
            Ok(impact) => {
                self.draft = None;
                self.feather_text = None;
                self.applied = Some(impact);
                self.stale_counts = true;
                Some(impact)
            }
            Err(e) if e.is_busy() => {
                self.pending = Some(edit);
                self.retry_at = Some(Instant::now() + BUSY_RETRY);
                None
            }
            Err(e) => {
                let message = format!("Could not change the settings: {e}");
                tracing::warn!("{message}");
                self.draft = Some(edit);
                self.error = Some(message);
                None
            }
        }
    }

    /// Take a finished tile count, start one when needed, and apply a
    /// waiting edit once the runner is idle. Call every frame after the
    /// runner's poll, whichever tab is shown. The impact when an edit was
    /// applied.
    pub fn poll(
        &mut self,
        ctx: &egui::Context,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
    ) -> Option<ChangeImpact> {
        if let Some(task) = &self.count_task {
            match task.poll() {
                Poll::Pending => {}
                Poll::Done(c) => {
                    self.count_task = None;
                    self.counts = Some(c);
                }
                Poll::Lost => self.count_task = None,
            }
        }
        // Every job cancelled for an edit has been reported (by the
        // runner's poll this frame, before the shell read `editing`).
        if self.cancelled_for_edit && !jobs.busy_except_exports() && !jobs.has_undelivered() {
            self.cancelled_for_edit = false;
        }
        let mut applied = None;
        if let Some(edit) = self.pending.clone() {
            // Newer work wins, also over jobs started after the edit was
            // queued; exports are spared.
            if !self.cancel_for(&edit, &session.mosaic.config, jobs) {
                let now = Instant::now();
                match self.retry_at {
                    Some(at) if at > now => ctx.request_repaint_after(at - now),
                    _ => {
                        self.pending = None;
                        applied = self.submit(edit, session, jobs);
                    }
                }
            }
        }
        // Counted only while nothing but exports runs (no blend can
        // recreate a store under the count's mapping) and only when a
        // draft needs the cost. The stores are listed on the worker: the
        // ones `set_settings` invalidates.
        if self.stale_counts
            && self.draft.is_some()
            && self.count_task.is_none()
            && !jobs.busy_except_exports()
        {
            self.stale_counts = false;
            let dir = session.mosaic.dir.clone();
            self.count_task = Task::spawn("mmm-settings-count", ctx, move || {
                count_tiles(&result_store_paths(&dir).unwrap_or_default())
            });
        }
        applied
    }

    /// Draw the panel; the impact when an edit was applied now.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
    ) -> Option<ChangeImpact> {
        let mut applied = None;
        egui::CollapsingHeader::new(RichText::new("Blend settings").color(palette::TEXT_STRONG))
            .id_salt("blend-settings")
            .default_open(true)
            .show(ui, |ui| {
                applied = self.body(ui, session, jobs);
            });
        applied
    }

    fn body(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
    ) -> Option<ChangeImpact> {
        let cfg = &session.mosaic.config;
        let shown = self.shown(cfg);
        let changed = changed_fields(cfg, &shown);
        let mut edit = shown.clone();
        let editable = self.pending.is_none();
        let caption = |ui: &mut egui::Ui, t: &str| {
            ui.label(RichText::new(t).small().color(palette::TEXT_SUBTLE));
        };

        caption(ui, "BLEND · reblend only");
        ui.add_enabled_ui(editable, |ui| {
            egui::Grid::new("blend-settings-blend")
                .num_columns(3)
                .spacing([6.0, 4.0])
                .show(ui, |ui| {
                    let modes = [BlendMode::Pyramid, BlendMode::TwoBand, BlendMode::Feather];
                    combo_row(
                        ui,
                        "Mode",
                        changed.contains(&Field::Mode),
                        &mut edit.blend.mode,
                        &modes,
                        mode_name,
                        "Pyramid: multiband base + star-safe seams. TwoBand: feathered base \
                         + star-safe seams. Feather: plain weighted average.",
                    );
                    self.feather_row(ui, changed.contains(&Field::Feather), &mut edit.blend);
                    combo_row(
                        ui,
                        "Defect veto",
                        changed.contains(&Field::DefectVeto),
                        &mut edit.blend.defect_veto,
                        &[true, false],
                        on_off,
                        "Suppress cosmic-ray residue and satellite trails that survive in \
                         one panel of an overlap (TwoBand and Pyramid)",
                    );
                    combo_row(
                        ui,
                        "Flatten",
                        changed.contains(&Field::Flatten),
                        &mut edit.blend.flatten,
                        &[None, Some(1), Some(2)],
                        flatten_name,
                        "Global background flatten of the merged mosaic (refused on \
                         nebula-heavy mosaics)",
                    );
                });
        });
        ui.add_space(6.0);
        caption(ui, "ANALYSIS · reruns analysis, every tile goes stale");
        ui.add_enabled_ui(editable, |ui| {
            egui::Grid::new("blend-settings-analyze")
                .num_columns(3)
                .spacing([6.0, 4.0])
                .show(ui, |ui| {
                    combo_row(
                        ui,
                        "Input",
                        changed.contains(&Field::Input),
                        &mut edit.analyze.input,
                        &[InputSelect::Auto, InputSelect::Aligned, InputSelect::Solved],
                        input_name,
                        "How the panels are read: detected, registered full-canvas frames, \
                         or unaligned panels with astrometric solutions",
                    );
                    combo_row(
                        ui,
                        "Surface",
                        changed.contains(&Field::Surface),
                        &mut edit.analyze.surface,
                        &[None, Some(0), Some(1), Some(2)],
                        surface_name,
                        "Residual surface correction order",
                    );
                    combo_row(
                        ui,
                        "Gain",
                        changed.contains(&Field::Gain),
                        &mut edit.analyze.gain,
                        &[GainMode::Fit, GainMode::Unity],
                        gain_name,
                        "Fit per-panel gains from the overlaps, or pin every gain at 1 \
                         (same rig and exposure)",
                    );
                });
        });
        if editable && edit != shown {
            self.stage(edit, &session.mosaic.config);
        }

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
            if ui.small_button("Revert").clicked() {
                self.revert();
            }
        } else if let Some(draft) = &self.draft {
            let impact = draft.impact(&session.mosaic.config);
            let counts = if self.stale_counts || self.count_task.is_some() {
                None
            } else {
                self.counts
            };
            ui.add_space(4.0);
            ui.label(
                RichText::new(cost_text(
                    impact,
                    counts,
                    crate::pipeline::auto_analyze(&session.mosaic.config),
                ))
                .small()
                .color(palette::TEXT_BODY),
            );
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(self.feather_error.is_none(), egui::Button::new("Apply"))
                    .on_hover_text("Apply the changed settings")
                    .on_disabled_hover_text("Fix the feather first")
                    .clicked()
                {
                    applied = self.apply(session, jobs);
                }
                if ui.button("Revert").clicked() {
                    self.revert();
                }
            });
        } else if self.feather_error.is_some() {
            if ui.small_button("Revert").clicked() {
                self.revert();
            }
        } else if let Some(impact) = self.applied {
            let note = match impact {
                ChangeImpact::Reblend => "Applied. Blend again to see the change.",
                ChangeImpact::Reanalyze => "Applied. Analysis reruns, then blend again.",
                ChangeImpact::None => "",
            };
            if !note.is_empty() {
                ui.label(RichText::new(note).small().color(palette::TEXT_MUTED));
            }
        }
        if let Some(e) = self.error.clone() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(e).small().color(palette::HA_400));
                if ui.small_button("Dismiss").clicked() {
                    self.error = None;
                }
            });
        }
        applied
    }

    /// The feather row: a text field, validated as typed (a refused value
    /// keeps the text and an error, and Apply is disabled).
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
            // Not being typed: follow the value (e.g. after Revert).
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

/// The changed-since-last-blend dot (or the room it takes).
fn mark(ui: &mut egui::Ui, changed: bool) {
    if changed {
        ui.label(
            RichText::new(CHANGED_MARK)
                .small()
                .color(palette::ACCENT_400),
        )
        .on_hover_text("Changed since the last blend");
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
