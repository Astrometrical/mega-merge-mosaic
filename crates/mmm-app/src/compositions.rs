//! Compositions: named colour mixes of groups for the Plan view. Each of
//! the three output channels (R, G, B) takes a group with a weight, or
//! nothing. Stored in `MosaicConfig.ui` under [`COMPOSITIONS_KEY`].

use std::collections::BTreeMap;

use egui::{Color32, RichText};
use mega_ui::theme::palette;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::panels_tab::{group_colors, load_group_colors};
use crate::stretch_panel::channel_name;

/// Key in `MosaicConfig.ui` of the compositions list.
pub const COMPOSITIONS_KEY: &str = "compositions";

/// A named mix of groups into the R, G and B channels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Composition {
    /// Shown name; also keys the view's display stretch.
    pub name: String,
    /// Per output channel (R, G, B): the group shown and its weight.
    pub channels: [Option<(String, f32)>; 3],
}

impl Composition {
    /// An empty composition called `name`.
    pub fn new(name: impl Into<String>) -> Self {
        Composition {
            name: name.into(),
            channels: [None, None, None],
        }
    }
}

/// What the Plan view shows.
#[derive(Debug, Clone, PartialEq)]
pub enum ViewKind {
    /// One group by itself.
    Group(String),
    /// The composition at this index of the compositions list.
    Composition(usize),
}

/// The compositions saved in `ui` (none when absent or unreadable).
pub fn load_compositions(ui: &Value) -> Vec<Composition> {
    ui.get(COMPOSITIONS_KEY)
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

/// Store `c` as the compositions in `ui`, keeping everything else there (a
/// `ui` that is not a JSON object becomes one).
pub fn save_compositions(ui: &mut Value, c: &[Composition]) {
    if !ui.is_object() {
        *ui = Value::Object(Default::default());
    }
    if let (Some(obj), Ok(v)) = (ui.as_object_mut(), serde_json::to_value(c)) {
        obj.insert(COMPOSITIONS_KEY.to_string(), v);
    }
}

/// The default tint of each channel: its group's colour (see
/// [`group_colors`]), `None` for an empty channel.
pub fn channel_tints(ui: &Value, c: &Composition) -> [Option<Color32>; 3] {
    let colors: BTreeMap<String, Color32> = group_colors(
        &load_group_colors(ui),
        c.channels.iter().flatten().map(|(g, _)| g.as_str()),
    );
    std::array::from_fn(|i| {
        c.channels[i]
            .as_ref()
            .and_then(|(g, _)| colors.get(g).copied())
    })
}

/// Editor of one composition (name, and a group and weight per channel);
/// true if changed. `groups` are the groups to choose from.
pub fn composition_editor(ui: &mut egui::Ui, c: &mut Composition, groups: &[String]) -> bool {
    let mut changed = ui.text_edit_singleline(&mut c.name).changed();
    for (i, slot) in c.channels.iter_mut().enumerate() {
        ui.push_id(i, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(channel_name(i, 3)).color(palette::TEXT_BODY));
                let current = slot.as_ref().map_or("None", |(g, _)| g.as_str());
                egui::ComboBox::from_id_salt("group")
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(slot.is_none(), "None").clicked() && slot.is_some() {
                            *slot = None;
                            changed = true;
                        }
                        for g in groups {
                            let picked = slot.as_ref().is_some_and(|(s, _)| s == g);
                            if ui.selectable_label(picked, g).clicked() && !picked {
                                let w = slot.as_ref().map_or(1.0, |(_, w)| *w);
                                *slot = Some((g.clone(), w));
                                changed = true;
                            }
                        }
                    });
                if let Some((_, w)) = slot {
                    changed |= ui
                        .add(egui::Slider::new(w, 0.0..=2.0).text("Weight"))
                        .changed();
                }
            });
        });
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::start::tests::temp_dir;
    use mmm_core::mosaic::MosaicConfig;

    #[test]
    fn compositions_roundtrip() {
        let mut cfg = MosaicConfig::new(Vec::new());
        cfg.ui = serde_json::json!({ "other": 1 });
        assert!(load_compositions(&cfg.ui).is_empty());
        let mut a = Composition::new("SHO");
        a.channels = [
            Some(("SII".into(), 1.0)),
            Some(("Ha".into(), 0.5)),
            Some(("OIII".into(), 1.25)),
        ];
        let mut b = Composition::new("Ha only");
        b.channels[0] = Some(("Ha".into(), 1.0));
        save_compositions(&mut cfg.ui, &[a.clone(), b.clone()]);
        let dir = temp_dir("compositions");
        cfg.save(&dir).unwrap();
        let back = MosaicConfig::load(&dir).unwrap();
        assert_eq!(load_compositions(&back.ui), vec![a.clone(), b]);
        assert_eq!(back.ui["other"], 1);
        // Tints follow the group colours; an empty channel has none.
        let tints = channel_tints(&back.ui, &a);
        assert!(tints.iter().all(Option::is_some));
        assert_ne!(tints[0], tints[1]);
        // A non-object `ui` becomes one; unreadable data reads as none.
        let mut null = Value::Null;
        save_compositions(&mut null, &[a]);
        assert_eq!(load_compositions(&null).len(), 1);
        assert!(load_compositions(&serde_json::json!({ "compositions": 3 })).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
