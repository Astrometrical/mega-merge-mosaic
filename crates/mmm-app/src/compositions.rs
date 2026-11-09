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

/// What the R, G and B channels show: a group and its weight, or nothing.
pub type Channels = [Option<(String, f32)>; 3];

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
    pub channels: Channels,
}

impl Composition {
    /// An empty composition called `name` (without an id).
    #[cfg(test)]
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

/// Key in `MosaicConfig.ui` of the next composition id to hand out. Ids are
/// never reused, so a new composition never inherits a deleted one's saved
/// stretch or view (`comp:{id}`).
pub const NEXT_ID_KEY: &str = "composition_next_id";

/// The next id to hand out: the saved counter, or one past the highest id
/// in `list` if that is more.
fn next_id(ui: &Value, list: &[Composition]) -> u64 {
    let counter = ui.get(NEXT_ID_KEY).and_then(Value::as_u64).unwrap_or(0);
    counter.max(list.iter().map(|c| c.id).max().unwrap_or(0) + 1)
}

/// `base`, or "`base` 2", "`base` 3", … when `taken` has it: a name none of
/// `taken` has.
pub fn unique_name(base: &str, taken: &[String]) -> String {
    if !taken.iter().any(|t| t == base) {
        return base.to_string();
    }
    (2..)
        .map(|i| format!("{base} {i}"))
        .find(|n| !taken.contains(n))
        .unwrap_or_default()
}

/// Add a composition called `name` mapping `channels` (weights as given)
/// to the compositions in `ui`, with a new id; returns that id. The caller
/// has made `name` unique ([`unique_name`]). Not saved to disk (the caller
/// rate-limits that).
pub fn create_composition(ui: &mut Value, name: &str, channels: [Option<(String, f32)>; 3]) -> u64 {
    let mut list = load_compositions(ui);
    let id = next_id(ui, &list);
    list.push(Composition {
        id,
        name: name.to_string(),
        channels,
    });
    save_compositions(ui, &list);
    id
}

/// Put `c`, deleted from `index`, back there (the end, if the list is
/// shorter by now); returns its id. It keeps its id unless another
/// composition has it by now, and its name unless taken: then it gets a
/// fresh id ([`create_composition`]'s) or a [`unique_name`].
pub fn restore_composition(ui: &mut Value, index: usize, mut c: Composition) -> u64 {
    let mut list = load_compositions(ui);
    if list.iter().any(|o| o.id == c.id) || c.id == 0 {
        c.id = next_id(ui, &list);
    }
    let taken: Vec<String> = list.iter().map(|o| o.name.clone()).collect();
    c.name = unique_name(&c.name, &taken);
    let id = c.id;
    list.insert(index.min(list.len()), c);
    save_compositions(ui, &list);
    id
}

/// Store `c` as the compositions in `ui`, keeping everything else there (a
/// `ui` that is not a JSON object becomes one). The id counter
/// ([`NEXT_ID_KEY`]) moves past the highest id, so deleting the last
/// composition never frees its id.
pub fn save_compositions(ui: &mut Value, c: &[Composition]) {
    if !ui.is_object() {
        *ui = Value::Object(Default::default());
    }
    let next = next_id(ui, c);
    if let (Some(obj), Ok(v)) = (ui.as_object_mut(), serde_json::to_value(c)) {
        obj.insert(COMPOSITIONS_KEY.to_string(), v);
        obj.insert(NEXT_ID_KEY.to_string(), Value::from(next));
    }
}

/// What a composition's row says under its name: "R {g} · G {g} · B {g}"
/// ("None" for a channel without a group), or "missing group {g}" for the
/// first mapped group that is not in `groups`.
pub fn composition_detail(c: &Composition, groups: &[String]) -> String {
    let mapped = c.channels.iter().flatten().map(|(g, _)| g);
    if let Some(missing) = mapped.clone().find(|g| !groups.contains(g)) {
        return format!("missing group {missing}");
    }
    c.channels
        .iter()
        .enumerate()
        .map(|(i, slot)| {
            let g = slot.as_ref().map_or("None", |(g, _)| g.as_str());
            format!("{} {g}", channel_name(i, 3))
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Editor of one composition's channels (a group, or "None", for each of
/// R, G and B); true if changed. `groups` are the groups to choose from. A
/// mapped group that is not among them is kept and shown as missing. The
/// weights stay as they are (1) and are not shown.
pub fn channel_editor(ui: &mut egui::Ui, c: &mut Composition, groups: &[String]) -> bool {
    let mut changed = false;
    for (i, slot) in c.channels.iter_mut().enumerate() {
        ui.push_id(i, |ui| {
            ui.horizontal(|ui| {
                let name = channel_name(i, 3);
                let label = ui.label(RichText::new(name).color(palette::TEXT_BODY));
                let current = match slot {
                    None => "None".to_string(),
                    Some((g, _)) if groups.contains(g) => g.clone(),
                    Some((g, _)) => format!("{g} (missing)"),
                };
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
                    })
                    .response
                    .labelled_by(label.id);
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

    fn channels_of(groups: [Option<&str>; 3]) -> [Option<(String, f32)>; 3] {
        groups.map(|g| g.map(|g| (g.to_string(), 1.0)))
    }

    #[test]
    fn created_compositions_map_their_channels_with_fresh_ids() {
        let mut ui = serde_json::json!({ "other": 1 });
        let hoo = channels_of([Some("Ha"), Some("OIII"), Some("OIII")]);
        assert_eq!(create_composition(&mut ui, "HOO", hoo.clone()), 1);
        assert_eq!(
            create_composition(&mut ui, "Empty", channels_of([None, None, None])),
            2
        );
        let list = load_compositions(&ui);
        assert_eq!(list[0].name, "HOO");
        assert_eq!(list[0].channels, hoo);
        assert_eq!(list[0].id, 1);
        assert_eq!(ui["other"], 1);
        let taken = vec!["SHO".to_string(), "SHO 2".to_string()];
        assert_eq!(unique_name("SHO", &taken), "SHO 3");
        assert_eq!(unique_name("HOO", &taken), "HOO");
    }

    #[test]
    fn ids_are_never_reused() {
        let mut ui = serde_json::json!({});
        for name in ["A", "B", "C"] {
            create_composition(&mut ui, name, channels_of([Some("Ha"), None, None]));
        }
        // Delete the highest-id composition, as the list does.
        let mut list = load_compositions(&ui);
        let gone = list.pop().unwrap();
        assert_eq!(gone.id, 3);
        save_compositions(&mut ui, &list);
        // A new one is not 3: it would inherit C's saved stretch and view.
        let fresh = create_composition(&mut ui, "D", channels_of([None, None, None]));
        assert_eq!(fresh, 4);
        // Undo puts C back with its own id.
        assert_eq!(restore_composition(&mut ui, 2, gone.clone()), 3);
        let ids: Vec<u64> = load_compositions(&ui).iter().map(|c| c.id).collect();
        assert_eq!(ids, [1, 2, 3, 4], "restored at index 2, D after");
        // An id taken meanwhile means a fresh one (and a taken name another).
        let mut taken = gone;
        taken.id = 4;
        taken.name = "D".into();
        let id = restore_composition(&mut ui, 0, taken);
        assert_eq!(id, 5);
        let list = load_compositions(&ui);
        assert_eq!(list[0].id, 5);
        assert_eq!(list[0].name, "D 2");
        // The counter also guards a list saved without it.
        let mut old = serde_json::json!({ "compositions": [
            { "id": 7, "name": "X", "channels": [null, null, null] }
        ]});
        assert_eq!(
            create_composition(&mut old, "Y", channels_of([None, None, None])),
            8
        );
    }

    #[test]
    fn details_name_the_groups_or_the_missing_one() {
        let groups = vec!["Ha".to_string(), "OIII".to_string()];
        let mut c = Composition::new("HOO");
        c.channels = channels_of([Some("Ha"), Some("OIII"), None]);
        assert_eq!(composition_detail(&c, &groups), "R Ha · G OIII · B None");
        c.channels[2] = Some(("SII".into(), 1.0));
        assert_eq!(composition_detail(&c, &groups), "missing group SII");
    }

    #[test]
    fn channel_editor_edits_without_weights() {
        use egui_kittest::kittest::Queryable;

        struct State {
            c: Composition,
            changes: usize,
        }
        let groups = vec!["Ha".to_string(), "OIII".to_string()];
        let mut start = Composition::new("New");
        start.channels[1] = Some(("Ha".into(), 0.5));
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(500.0, 400.0))
            .build_ui_state(
                move |ui, s: &mut State| {
                    if channel_editor(ui, &mut s.c, &groups) {
                        s.changes += 1;
                    }
                },
                State {
                    c: start,
                    changes: 0,
                },
            );
        h.run();
        // A group picked for R gets weight 1.
        h.get_all_by_role(egui::accesskit::Role::ComboBox)
            .next()
            .unwrap()
            .click();
        h.run();
        h.get_by_label("OIII").click();
        h.run();
        assert_eq!(h.state().c.channels[0], Some(("OIII".to_string(), 1.0)));
        // G's weight is kept, not edited; and no weight control exists.
        assert_eq!(h.state().c.channels[1], Some(("Ha".to_string(), 0.5)));
        assert!(h.query_all_by_label("Weight").next().is_none());
        assert_eq!(h.state().changes, 1);
    }
}
