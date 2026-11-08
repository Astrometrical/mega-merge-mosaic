//! The display stretches earlier versions saved per view in
//! `MosaicConfig.ui` (set by hand in the old Blend and Plan stretch
//! panels), and channel names. The session views ignore these stretches
//! now (their stretch is always automatic, [`crate::display_stretch`]); the
//! start screen still shows a session through them.

use mmm_core::stretch::StretchParams;
use serde_json::Value;

/// Key in `MosaicConfig.ui` of the merged results' display stretches, by
/// group.
pub const BLEND_STRETCH_KEY: &str = "blend_stretch";
/// Key in `MosaicConfig.ui` of the plan's display stretches, by view name
/// (a group or a composition).
pub const PLAN_STRETCH_KEY: &str = "plan_stretch";

/// The saved display stretch of view `name` under `key` in `ui`
/// (`MosaicConfig.ui`), if any: present means the user set it; absent means
/// automatic.
pub fn saved_stretch_at(ui: &Value, key: &str, name: &str) -> Option<Vec<StretchParams>> {
    serde_json::from_value(ui.get(key)?.get(name)?.clone()).ok()
}

/// The display stretch saved for `group`'s merged result in `ui`
/// ([`BLEND_STRETCH_KEY`]), if any.
pub fn saved_stretch(ui: &Value, group: &str) -> Option<Vec<StretchParams>> {
    saved_stretch_at(ui, BLEND_STRETCH_KEY, group)
}

/// Save (`Some`) or clear (`None`, back to automatic) view `name`'s display
/// stretch under `key` in `ui`, keeping everything else there. A `ui` that
/// is not a JSON object (e.g. `null`) becomes one. Tests write what earlier
/// versions saved with it.
#[cfg(test)]
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
    use mmm_core::mosaic::MosaicConfig;

    use super::*;
    use crate::start::tests::temp_dir;

    fn p(black: f32) -> StretchParams {
        StretchParams {
            black,
            white: 1.0,
            midtones: 0.25,
        }
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

    #[test]
    fn stretch_persisted_per_group_in_ui() {
        let (a, b) = (p(0.01), p(0.02));
        let mut cfg = MosaicConfig::new(Vec::new());
        cfg.ui = serde_json::json!({ "other": 1 });
        save_stretch_at(&mut cfg.ui, BLEND_STRETCH_KEY, "Ha", Some(&[a]));
        save_stretch_at(&mut cfg.ui, BLEND_STRETCH_KEY, "OIII", Some(&[a, b, b]));
        let dir = temp_dir("blend-stretch");
        cfg.save(&dir).unwrap();
        let back = MosaicConfig::load(&dir).unwrap();
        assert_eq!(saved_stretch(&back.ui, "Ha"), Some(vec![a]));
        assert_eq!(saved_stretch(&back.ui, "OIII"), Some(vec![a, b, b]));
        assert_eq!(saved_stretch(&back.ui, "SII"), None);
        assert!(back.ui["blend_stretch"]["Ha"].is_array());
        assert_eq!(back.ui["other"], 1);

        // Back to automatic: the group's entry goes, the rest stays.
        let mut ui = back.ui;
        save_stretch_at(&mut ui, BLEND_STRETCH_KEY, "Ha", None);
        assert_eq!(saved_stretch(&ui, "Ha"), None);
        assert_eq!(saved_stretch(&ui, "OIII"), Some(vec![a, b, b]));
        assert_eq!(ui["other"], 1);
        // A non-object `ui` (e.g. null from an older file) is replaced.
        let mut null = serde_json::Value::Null;
        save_stretch_at(&mut null, BLEND_STRETCH_KEY, "Ha", Some(&[b]));
        assert_eq!(saved_stretch(&null, "Ha"), Some(vec![b]));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
