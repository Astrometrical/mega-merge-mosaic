//! What the session window shows, saved with the session so reopening it
//! looks the same (spec §15): plan or mosaic, the group or composite, the
//! camera, the inspector's fold and open sections, whether the panel list
//! is open, and the Outlines, Seams and ROIs toggles. Kept in `MosaicConfig.ui`
//! under [`VIEW_STATE_KEY`]; the display stretch toggles keep their own key
//! ([`crate::display_stretch::DISPLAY_STRETCH_KEY`]).
//!
//! The window writes it into `ui` whenever any of it changes and saves it
//! with the rest of `ui` (rate-limited); it applies a saved one on its
//! first frame ([`super::SessionWindow`]'s module docs).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{CanvasMode, PROBLEMS};
use crate::camera::Camera;

/// Key in `MosaicConfig.ui` of the session window's view.
pub const VIEW_STATE_KEY: &str = "session_view";

/// The session window's view, as saved. A field missing from what was
/// saved takes its [`Default`] value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewState {
    /// Plan or mosaic.
    pub mode: CanvasMode,
    /// The group (its name) or composite (`comp:<id>`) shown.
    pub view: Option<String>,
    /// Where the canvas looks.
    pub camera: Option<Camera>,
    /// The inspector is folded away.
    pub inspector_folded: bool,
    /// The ids of the inspector sections open.
    pub inspector_open: Vec<String>,
    /// The panel list shows in the canvas's place.
    pub panel_list_open: bool,
    /// The panels' outlines (and numbers) show.
    pub outlines: bool,
    /// The seams show (off in a view saved before the toggle existed).
    #[serde(default)]
    pub seams: bool,
    /// The ROIs show.
    pub rois: bool,
}

impl Default for ViewState {
    /// A new window's view: the plan, Problems open, Outlines and ROIs on,
    /// Seams off.
    fn default() -> Self {
        ViewState {
            mode: CanvasMode::Plan,
            view: None,
            camera: None,
            inspector_folded: false,
            inspector_open: vec![PROBLEMS.to_string()],
            panel_list_open: false,
            outlines: true,
            seams: false,
            rois: true,
        }
    }
}

impl ViewState {
    /// Read from `ui`: `None` if nothing is saved there or what is cannot be
    /// read. A camera no canvas could look through (not finite, or a zoom
    /// that is not positive) is dropped.
    pub fn load(ui: &Value) -> Option<Self> {
        let mut state = Self::deserialize(ui.get(VIEW_STATE_KEY)?).ok()?;
        state.camera = state.camera.filter(usable);
        Some(state)
    }

    /// Store in `ui`, keeping everything else there (a `ui` that is not an
    /// object is replaced by one). A camera no canvas could look through is
    /// left out.
    pub fn save(&self, ui: &mut Value) {
        let state = ViewState {
            camera: self.camera.filter(usable),
            ..self.clone()
        };
        let value = match serde_json::to_value(&state) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("the session window's view is not saved: {e}");
                return;
            }
        };
        if !ui.is_object() {
            *ui = Value::Object(Default::default());
        }
        if let Some(obj) = ui.as_object_mut() {
            obj.insert(VIEW_STATE_KEY.to_string(), value);
        }
    }
}

/// Whether a canvas can look through `c`: finite, with a positive zoom.
fn usable(c: &Camera) -> bool {
    c.centre.0.is_finite() && c.centre.1.is_finite() && c.zoom.is_finite() && c.zoom > 0.0
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn state() -> ViewState {
        ViewState {
            mode: CanvasMode::Mosaic,
            view: Some("comp:SHO".into()),
            camera: Some(Camera {
                centre: (1234.5, 678.25),
                zoom: 0.125,
            }),
            inspector_folded: true,
            inspector_open: vec!["notes".into(), "rois".into()],
            panel_list_open: true,
            outlines: false,
            seams: true,
            rois: false,
        }
    }

    #[test]
    fn view_state_round_trips_keeping_other_keys() {
        let mut ui = json!({"display_stretch": {"linked": true, "strong": false}, "notes": "x"});
        let s = state();
        s.save(&mut ui);
        assert_eq!(ui["notes"], "x");
        assert_eq!(ui["display_stretch"]["linked"], true);
        assert!(ui[VIEW_STATE_KEY].is_object());
        assert_eq!(ViewState::load(&ui), Some(s.clone()));
        // A `ui` that is not an object becomes one.
        let mut not_obj = json!([1, 2]);
        s.save(&mut not_obj);
        assert_eq!(ViewState::load(&not_obj), Some(s));
    }

    #[test]
    fn view_state_loads_leniently() {
        assert_eq!(ViewState::load(&json!({})), None, "nothing saved");
        assert_eq!(ViewState::load(&json!(null)), None);
        assert_eq!(ViewState::load(&json!({VIEW_STATE_KEY: 3})), None);
        let bad_mode = json!({VIEW_STATE_KEY: {"mode": "sideways"}});
        assert_eq!(ViewState::load(&bad_mode), None, "malformed");
        // Fields missing take a new window's values.
        let some = ViewState::load(&json!({VIEW_STATE_KEY: {"mode": "mosaic"}})).unwrap();
        assert_eq!(some.mode, CanvasMode::Mosaic);
        assert_eq!(some.view, None);
        assert_eq!(some.camera, None);
        assert!(!some.inspector_folded);
        assert_eq!(some.inspector_open, ["problems"]);
        assert!(!some.panel_list_open);
        assert!(some.outlines && some.rois);
        assert!(!some.seams, "a view saved before Seams: off");
        // A camera that could not be looked through is dropped.
        let mut ui = json!({});
        let mut s = state();
        s.save(&mut ui);
        ui[VIEW_STATE_KEY]["camera"]["zoom"] = json!(0.0);
        s.camera = None;
        assert_eq!(ViewState::load(&ui), Some(s));
    }
}
