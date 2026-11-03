//! The display stretch panel shared by the Blend and Plan tabs: Auto,
//! Linked, and black / white / midtones sliders per channel (one set when
//! linked or mono), plus the persistence of a view's manual stretch in
//! `MosaicConfig.ui`.
//!
//! The panel only draws and reports what the user did ([`StretchChange`]);
//! the tab owns the source, the automatic-stretch task and the debounced
//! save.

use egui::RichText;
use mega_ui::theme::palette;
use mmm_core::stretch::StretchParams;
use serde_json::Value;

/// Key in `MosaicConfig.ui` of the Blend view's display stretches, by group.
pub const BLEND_STRETCH_KEY: &str = "blend_stretch";
/// Key in `MosaicConfig.ui` of the Plan view's display stretches, by view
/// name (a group or a composition).
#[allow(dead_code)] // used by the Plan tab (next task)
pub const PLAN_STRETCH_KEY: &str = "plan_stretch";

/// What the panel shows and edits; the caller keeps it between frames.
#[derive(Debug, Clone, Default)]
pub struct StretchState {
    /// The stretch now applied, one entry per channel (empty with no
    /// result).
    pub stretch: Vec<StretchParams>,
    /// Channels of the view; 0 means there is no result to show.
    pub channels: usize,
    /// One set of sliders for every channel.
    pub linked: bool,
    /// The stretch was set by the user (otherwise automatic).
    pub manual: bool,
    /// An automatic stretch is being computed (Auto is disabled).
    pub auto_busy: bool,
}

/// What the user did in the panel this frame.
#[derive(Debug, Clone, PartialEq)]
pub enum StretchChange {
    /// Nothing.
    None,
    /// Auto was pressed: go back to the automatic stretch (with
    /// [`StretchState::linked`] as shown).
    Auto,
    /// The user set this stretch (sliders, or Linked switched on).
    Manual(Vec<StretchParams>),
}

/// The saved display stretch of view `name` under `key` in `ui`
/// (`MosaicConfig.ui`), if any: present means the user set it; absent means
/// automatic.
pub fn saved_stretch_at(ui: &Value, key: &str, name: &str) -> Option<Vec<StretchParams>> {
    serde_json::from_value(ui.get(key)?.get(name)?.clone()).ok()
}

/// Save (`Some`) or clear (`None`, back to automatic) view `name`'s display
/// stretch under `key` in `ui`, keeping everything else there. A `ui` that
/// is not a JSON object (e.g. `null`) becomes one.
pub fn save_stretch_at(ui: &mut Value, key: &str, name: &str, stretch: Option<&[StretchParams]>) {
    if !ui.is_object() {
        *ui = Value::Object(Default::default());
    }
    let Some(map) = ui.as_object_mut() else {
        return;
    };
    let entry = map
        .entry(key)
        .or_insert_with(|| Value::Object(Default::default()));
    if !entry.is_object() {
        *entry = Value::Object(Default::default());
    }
    let Some(views) = entry.as_object_mut() else {
        return;
    };
    match stretch.map(serde_json::to_value) {
        Some(Ok(v)) => {
            views.insert(name.to_string(), v);
        }
        Some(Err(e)) => tracing::warn!("display stretch not saved: {e}"),
        None => {
            views.remove(name);
        }
    }
}

/// The display stretch panel. Switching Linked on replaces the stretch with
/// the channel mean (reported as [`StretchChange::Manual`]).
pub fn stretch_panel(ui: &mut egui::Ui, state: &mut StretchState) -> StretchChange {
    ui.label(RichText::new("Display stretch").color(palette::TEXT_STRONG));
    if state.channels == 0 {
        ui.label(RichText::new("No result to show.").color(palette::TEXT_MUTED));
        return StretchChange::None;
    }
    let channels = state.channels;
    let mut stretch = state.stretch.clone();
    let mut changed = false;
    let mut auto_pressed = false;
    ui.horizontal(|ui| {
        let auto = ui
            .add_enabled(!state.auto_busy, egui::Button::new("Auto"))
            .on_hover_text("Automatic stretch from the whole result's statistics");
        auto_pressed = auto.clicked();
        if channels > 1
            && ui
                .toggle_value(&mut state.linked, "Linked")
                .on_hover_text("One stretch for every channel")
                .changed()
            && state.linked
        {
            let n = stretch.len() as f32;
            let mean = StretchParams {
                black: stretch.iter().map(|p| p.black).sum::<f32>() / n,
                white: stretch.iter().map(|p| p.white).sum::<f32>() / n,
                midtones: stretch.iter().map(|p| p.midtones).sum::<f32>() / n,
            };
            stretch = vec![mean; channels];
            changed = true;
        }
        ui.label(
            RichText::new(if state.manual { "Manual" } else { "Automatic" })
                .small()
                .color(palette::TEXT_MUTED),
        );
    });
    let sets = if state.linked || channels == 1 {
        1
    } else {
        channels
    };
    for (c, params) in stretch.iter_mut().enumerate().take(sets) {
        if sets > 1 {
            ui.label(
                RichText::new(channel_name(c, channels))
                    .color(palette::TEXT_BODY)
                    .small(),
            );
        }
        changed |= params_ui(ui, params, c);
    }
    if auto_pressed {
        return StretchChange::Auto;
    }
    if changed {
        if sets == 1 {
            stretch = vec![stretch[0]; channels];
        }
        state.stretch = stretch.clone();
        state.manual = true;
        return StretchChange::Manual(stretch);
    }
    StretchChange::None
}

/// Black, white and midtones sliders for one stretch; true if changed.
fn params_ui(ui: &mut egui::Ui, p: &mut StretchParams, salt: usize) -> bool {
    let mut changed = false;
    ui.push_id(salt, |ui| {
        let white = p.white;
        changed |= ui
            .add(
                egui::Slider::new(&mut p.black, 0.0..=white.max(f32::EPSILON))
                    .logarithmic(true)
                    .text("Black"),
            )
            .changed();
        let black = p.black;
        changed |= ui
            .add(egui::Slider::new(&mut p.white, black..=1.0).text("White"))
            .changed();
        changed |= ui
            .add(
                egui::Slider::new(&mut p.midtones, 0.0001..=0.9999)
                    .logarithmic(true)
                    .text("Midtones"),
            )
            .changed();
    });
    changed
}

/// "R", "G", "B" for colour; "Channel n" otherwise.
pub(crate) fn channel_name(c: usize, channels: usize) -> String {
    match (channels, c) {
        (3.., 0) => "R".into(),
        (3.., 1) => "G".into(),
        (3.., 2) => "B".into(),
        _ => format!("Channel {}", c + 1),
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::kittest::Queryable;

    use super::*;

    fn p(black: f32) -> StretchParams {
        StretchParams {
            black,
            white: 1.0,
            midtones: 0.25,
        }
    }

    #[test]
    fn stretch_panel_auto_emits_change() {
        let mut state = StretchState {
            stretch: vec![p(0.01)],
            channels: 1,
            ..Default::default()
        };
        let mut change = StretchChange::None;
        let mut harness = egui_kittest::Harness::new_ui(|ui| {
            // Keep the first real change: later frames report nothing.
            let c = stretch_panel(ui, &mut state);
            if c != StretchChange::None {
                change = c;
            }
        });
        harness.run();
        harness.get_by_label("Auto").click();
        harness.run();
        drop(harness);
        assert_eq!(change, StretchChange::Auto);

        // Nothing pressed: no change.
        let mut idle = StretchChange::Auto;
        let mut harness = egui_kittest::Harness::new_ui(|ui| {
            idle = stretch_panel(ui, &mut state);
        });
        harness.run();
        drop(harness);
        assert_eq!(idle, StretchChange::None);
    }

    #[test]
    fn plan_and_blend_stretch_keys_are_separate() {
        let mut ui = serde_json::json!({ "other": 1 });
        save_stretch_at(&mut ui, BLEND_STRETCH_KEY, "Ha", Some(&[p(0.1)]));
        save_stretch_at(&mut ui, PLAN_STRETCH_KEY, "Ha", Some(&[p(0.2)]));
        assert_eq!(
            saved_stretch_at(&ui, BLEND_STRETCH_KEY, "Ha"),
            Some(vec![p(0.1)])
        );
        assert_eq!(
            saved_stretch_at(&ui, PLAN_STRETCH_KEY, "Ha"),
            Some(vec![p(0.2)])
        );
        assert_eq!(ui["other"], 1);
    }
}
