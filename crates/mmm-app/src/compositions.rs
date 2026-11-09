//! Compositions (composites, as the UI calls them): named colour mixes of
//! groups, shown on the canvas like a group (plan or mosaic). Each of the
//! three output channels (R, G, B) takes a group with a weight, or nothing.
//! Stored in `MosaicConfig.ui` under [`COMPOSITIONS_KEY`].

use egui::RichText;
use mega_ui::theme::palette;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::stretch_panel::channel_name;

/// Key in `MosaicConfig.ui` of the compositions list.
pub const COMPOSITIONS_KEY: &str = "compositions";

/// A named mix of groups into the R, G and B channels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Composition {
    /// Stable identity: keys the view, its display stretch and the saved
    /// view, so renaming or reordering never orphans them. Unique in the
    /// list and never reused within it; 0 is "not assigned yet" and
    /// [`load_compositions`] gives such an entry a fresh one.
    #[serde(default)]
    pub id: u64,
    /// Shown name.
    pub name: String,
    /// Per output channel (R, G, B): the group shown and its weight.
    pub channels: [Option<(String, f32)>; 3],
}

impl Composition {
    /// An empty composition called `name`.
    pub fn new(name: impl Into<String>) -> Self {
        Composition {
            id: 0,
            name: name.into(),
            channels: [None, None, None],
        }
    }
}

/// What the canvas shows.
#[derive(Debug, Clone, PartialEq)]
pub enum ViewKind {
    /// One group by itself.
    Group(String),
    /// The composition with this [`Composition::id`].
    Composition(u64),
}

/// The compositions saved in `ui` (none when absent or not a list). Each
/// entry is read by itself: a malformed one, one without a name, or a
/// repeat of an earlier name is skipped, and the rest are kept. An entry
/// without an id (saved by an earlier mmm), or with a repeated one, gets
/// the next free id (the highest plus one); the next save writes them.
pub fn load_compositions(ui: &Value) -> Vec<Composition> {
    let Some(list) = ui.get(COMPOSITIONS_KEY).and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out: Vec<Composition> = Vec::new();
    for v in list {
        match serde_json::from_value::<Composition>(v.clone()) {
            Ok(c) if !c.name.trim().is_empty() && !out.iter().any(|o| o.name == c.name) => {
                out.push(c)
            }
            Ok(c) => tracing::warn!("composition {:?} skipped: no or a repeated name", c.name),
            Err(e) => tracing::warn!("a saved composition is unreadable and skipped: {e}"),
        }
    }
    let mut next = out.iter().map(|c| c.id).max().unwrap_or(0) + 1;
    let mut seen = std::collections::BTreeSet::new();
    for c in &mut out {
        if c.id == 0 || !seen.insert(c.id) {
            c.id = next;
            next += 1;
            seen.insert(c.id);
        }
    }
    out
}

/// Bring what an earlier mmm saved in `ui` up to the ids: if any saved
/// composition lacks an id (or repeats one), save the list with ids
/// ([`load_compositions`] assigns them) and move the plan display stretch
/// ([`crate::stretch_panel::PLAN_STRETCH_KEY`]) and the saved view
/// (`session_view.view`) keyed `comp:{name}` to `comp:{id}`. A key naming
/// no composite stays. Does nothing once every entry has an id, so a name
/// that looks like an id is never mistaken for one. True if `ui` changed.
pub fn migrate_composition_ids(ui: &mut Value) -> bool {
    let Some(raw) = ui.get(COMPOSITIONS_KEY).and_then(Value::as_array) else {
        return false;
    };
    let mut seen = std::collections::BTreeSet::new();
    let current = raw.iter().all(|v| {
        let id = v.get("id").and_then(Value::as_u64).unwrap_or(0);
        id != 0 && seen.insert(id)
    });
    if current {
        return false;
    }
    let list = load_compositions(ui);
    let moves: Vec<(String, String)> = list
        .iter()
        .map(|c| {
            (
                composition_stretch_key(&c.name),
                composition_stretch_key(c.id),
            )
        })
        .collect();
    save_compositions(ui, &list);
    if let Some(map) = ui
        .get_mut(crate::stretch_panel::PLAN_STRETCH_KEY)
        .and_then(Value::as_object_mut)
    {
        // Take every old key first so a new key never meets an old one.
        let taken: Vec<(String, Value)> = moves
            .iter()
            .filter_map(|(old, new)| Some((new.clone(), map.remove(old)?)))
            .collect();
        map.extend(taken);
    }
    if let Some(view) = ui
        .get_mut(crate::session_window::view_state::VIEW_STATE_KEY)
        .and_then(|v| v.get_mut("view"))
        && let Some(new) = view
            .as_str()
            .and_then(|k| moves.iter().find(|(old, _)| old == k))
            .map(|(_, new)| new.clone())
    {
        *view = Value::String(new);
    }
    true
}

/// The key of composition `id` for its Plan display stretch (under
/// [`crate::stretch_panel::PLAN_STRETCH_KEY`] in `MosaicConfig.ui`) and for
/// the saved view; prefixed so it never collides with a group's, which is
/// keyed by the group name.
pub fn composition_stretch_key(id: impl std::fmt::Display) -> String {
    format!("comp:{id}")
}

/// A name for a new composition that none of `taken` has:
/// "Composite 1", "Composite 2", ….
pub fn unique_composition_name(taken: &[String]) -> String {
    (1..)
        .map(|i| format!("Composite {i}"))
        .find(|n| !taken.contains(n))
        .unwrap_or_default()
}

/// Add a new composition to the compositions in `ui`, named by
/// [`unique_composition_name`] and mapping the first of `groups` to R, G
/// and B in turn (the last one repeated, HOO-like); returns its id. Not
/// saved to disk (the caller rate-limits that).
pub fn add_composition(ui: &mut Value, groups: &[String]) -> u64 {
    let mut list = load_compositions(ui);
    let taken: Vec<String> = list.iter().map(|c| c.name.clone()).collect();
    let mut c = Composition::new(unique_composition_name(&taken));
    c.id = list.iter().map(|o| o.id).max().unwrap_or(0) + 1;
    if let Some(last) = groups.len().checked_sub(1) {
        let pick = |i: usize| Some((groups[i.min(last)].clone(), 1.0));
        c.channels = [pick(0), pick(1), pick(2)];
    }
    let id = c.id;
    list.push(c);
    save_compositions(ui, &list);
    id
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

/// Editor of one composition (name, and a group and weight per channel);
/// true if changed. `groups` are the groups to choose from; `others` are
/// the names of the other compositions. The name is edited in a draft kept
/// in egui memory and taken only when it is non-empty (trimmed) and none of
/// `others`; otherwise the editor says why and the name stays as it was.
pub fn composition_editor(
    ui: &mut egui::Ui,
    c: &mut Composition,
    groups: &[String],
    others: &[String],
) -> bool {
    let mut changed = false;
    let id = ui.id().with("mmm-composition-name");
    // (the name the draft was made for, the draft)
    let mut draft: (String, String) = ui
        .data(|d| d.get_temp(id))
        .filter(|(for_name, _): &(String, String)| *for_name == c.name)
        .unwrap_or_else(|| (c.name.clone(), c.name.clone()));
    ui.horizontal(|ui| {
        let label = ui.label(RichText::new("Name").color(palette::TEXT_BODY));
        if ui
            .text_edit_singleline(&mut draft.1)
            .labelled_by(label.id)
            .changed()
        {
            let name = draft.1.trim();
            if !name.is_empty() && name != c.name && !others.iter().any(|o| o == name) {
                c.name = name.to_string();
                draft.0 = c.name.clone();
                changed = true;
            }
        }
    });
    let name = draft.1.trim();
    let problem = if name.is_empty() {
        Some("A composite needs a name.")
    } else if others.iter().any(|o| o == name) {
        Some("Another composite has this name.")
    } else {
        None
    };
    if let Some(p) = problem {
        ui.label(RichText::new(p).small().color(palette::HA_400));
    }
    ui.data_mut(|d| d.insert_temp(id, draft));
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
        a.id = 1;
        a.channels = [
            Some(("SII".into(), 1.0)),
            Some(("Ha".into(), 0.5)),
            Some(("OIII".into(), 1.25)),
        ];
        let mut b = Composition::new("Ha only");
        b.id = 2;
        b.channels[0] = Some(("Ha".into(), 1.0));
        save_compositions(&mut cfg.ui, &[a.clone(), b.clone()]);
        let dir = temp_dir("compositions");
        cfg.save(&dir).unwrap();
        let back = MosaicConfig::load(&dir).unwrap();
        assert_eq!(load_compositions(&back.ui), vec![a.clone(), b]);
        assert_eq!(back.ui["other"], 1);
        // A non-object `ui` becomes one; unreadable data reads as none.
        let mut null = Value::Null;
        save_compositions(&mut null, &[a]);
        assert_eq!(load_compositions(&null).len(), 1);
        assert!(load_compositions(&serde_json::json!({ "compositions": 3 })).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compositions_load_leniently() {
        let ui = serde_json::json!({ "compositions": [
            { "name": "HOO", "channels": [["Ha", 1.0], ["OIII", 1.0], ["OIII", 1.0]] },
            { "name": "broken", "channels": 7 },
            42,
            { "name": "  ", "channels": [null, null, null] },
            { "name": "HOO", "channels": [null, null, null] },
            { "name": "Ha only", "channels": [["Ha", 0.5], null, null] }
        ]});
        let names: Vec<String> = load_compositions(&ui).into_iter().map(|c| c.name).collect();
        assert_eq!(names, ["HOO", "Ha only"]);
        assert_eq!(composition_stretch_key(7), "comp:7");
        assert_ne!(
            composition_stretch_key(7),
            "7",
            "a composition's stretch key never equals a group's"
        );
        let taken = vec!["Composite 1".to_string(), "Composite 3".to_string()];
        assert_eq!(unique_composition_name(&taken), "Composite 2");
    }

    #[test]
    fn ids_are_assigned_once_and_migration_runs_once() {
        let mut ui = serde_json::json!({
            "compositions": [
                { "id": 5, "name": "A", "channels": [null, null, null] },
                { "name": "2", "channels": [null, null, null] },
                { "id": 5, "name": "C", "channels": [null, null, null] },
            ],
            "plan_stretch": { "comp:2": 1, "comp:C": 2, "Ha": 3 },
            "session_view": { "view": "comp:2" },
        });
        let ids: Vec<u64> = load_compositions(&ui).iter().map(|c| c.id).collect();
        assert_eq!(
            ids,
            [5, 6, 7],
            "after the highest; a repeat gets a fresh one"
        );
        assert!(migrate_composition_ids(&mut ui));
        // "2" is B's name, now id 6; C is id 7; nothing else moves.
        assert_eq!(
            ui["plan_stretch"],
            serde_json::json!({ "comp:6": 1, "comp:7": 2, "Ha": 3 })
        );
        assert_eq!(ui["session_view"]["view"], "comp:6");
        // Every entry has an id now: a second pass must not read the new
        // keys as names.
        let before = ui.clone();
        assert!(!migrate_composition_ids(&mut ui));
        assert_eq!(ui, before);
    }

    #[test]
    fn added_composition_maps_the_first_groups() {
        let mut ui = serde_json::json!({ "other": 1 });
        let groups = vec!["Ha".to_string(), "OIII".to_string()];
        assert_eq!(add_composition(&mut ui, &groups), 1);
        assert_eq!(add_composition(&mut ui, &[]), 2);
        let list = load_compositions(&ui);
        assert_eq!(list[0].name, "Composite 1");
        let mapped: Vec<Option<&str>> = list[0]
            .channels
            .iter()
            .map(|c| c.as_ref().map(|(g, _)| g.as_str()))
            .collect();
        assert_eq!(mapped, [Some("Ha"), Some("OIII"), Some("OIII")]);
        assert_eq!(list[0].id, 1);
        let mut second = Composition::new("Composite 2");
        second.id = 2;
        assert_eq!(list[1], second);
        assert_eq!(ui["other"], 1);
    }

    #[test]
    fn composition_editor_edits_and_refuses_bad_names() {
        use egui_kittest::kittest::Queryable;

        struct State {
            c: Composition,
            changes: usize,
        }
        let groups = vec!["Ha".to_string(), "OIII".to_string()];
        let others = vec!["SHO".to_string()];
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(500.0, 400.0))
            .build_ui_state(
                move |ui, s: &mut State| {
                    if composition_editor(ui, &mut s.c, &groups, &others) {
                        s.changes += 1;
                    }
                },
                State {
                    c: Composition::new("New"),
                    changes: 0,
                },
            );
        h.run();
        let name_field = |h: &mut egui_kittest::Harness<'_, State>| {
            h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Name")
                .focus();
            h.step();
            h.key_combination_modifiers(egui::Modifiers::COMMAND, &[egui::Key::A]);
            h.step();
        };
        // A unique name is taken.
        name_field(&mut h);
        h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Name")
            .type_text("HOO");
        h.run();
        assert_eq!(h.state().c.name, "HOO");
        // Another composition's name is refused, with a reason.
        name_field(&mut h);
        h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Name")
            .type_text("SHO");
        h.run();
        assert_eq!(h.state().c.name, "HOO");
        h.get_by_label("Another composite has this name.");
        // An empty name too.
        name_field(&mut h);
        h.key_press(egui::Key::Backspace);
        h.run();
        assert_eq!(h.state().c.name, "HOO");
        h.get_by_label("A composite needs a name.");

        // A group picked for R gets weight 1.
        h.get_all_by_role(egui::accesskit::Role::ComboBox)
            .next()
            .unwrap()
            .click();
        h.run();
        h.get_by_label("OIII").click();
        h.run();
        assert_eq!(h.state().c.channels[0], Some(("OIII".to_string(), 1.0)));
        assert!(h.state().c.channels[1].is_none());
        assert!(h.query_all_by_label("Weight").next().is_some());
        assert!(h.state().changes >= 2);
    }
}
