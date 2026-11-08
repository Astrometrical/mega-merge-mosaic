//! The session's display stretch: automatic only, with two toggles, kept in
//! the session's `ui` object under [`DISPLAY_STRETCH_KEY`].

use mmm_core::stretch::{ChannelStats, Strength, StretchParams, auto_stretch_with};
use serde_json::{Value, json};

/// Key in `MosaicConfig.ui` of the display stretch.
pub const DISPLAY_STRETCH_KEY: &str = "display_stretch";

/// Which automatic stretch the views show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DisplayStretch {
    /// Colour channels share one stretch.
    pub linked: bool,
    /// Exaggerate faint gradients.
    pub strong: bool,
}

impl DisplayStretch {
    /// Read from `ui`; absent or malformed gives the default.
    pub fn load(ui: &Value) -> Self {
        let Some(v) = ui.get(DISPLAY_STRETCH_KEY) else {
            return Self::default();
        };
        let flag = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
        Self {
            linked: flag("linked"),
            strong: flag("strong"),
        }
    }

    /// Store in `ui`, keeping everything else there (a `ui` that is not an
    /// object is replaced by one).
    pub fn save(self, ui: &mut Value) {
        if !ui.is_object() {
            *ui = Value::Object(Default::default());
        }
        if let Some(obj) = ui.as_object_mut() {
            obj.insert(
                DISPLAY_STRETCH_KEY.to_string(),
                json!({"linked": self.linked, "strong": self.strong}),
            );
        }
    }

    /// The core stretch strength.
    pub fn strength(self) -> Strength {
        if self.strong {
            Strength::Strong
        } else {
            Strength::Normal
        }
    }

    /// Stretch parameters for the channels' `stats`.
    pub fn params(self, stats: &[ChannelStats]) -> Vec<StretchParams> {
        auto_stretch_with(stats, self.linked, self.strength())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mmm_core::stretch::{ChannelStats, apply};
    use serde_json::json;

    #[test]
    fn display_stretch_defaults_to_unlinked_normal() {
        let d = DisplayStretch::default();
        assert!(!d.linked && !d.strong);
        assert_eq!(DisplayStretch::load(&json!({})), d);
        assert_eq!(DisplayStretch::load(&json!({"display_stretch": 3})), d);
        assert_eq!(DisplayStretch::load(&json!(null)), d);
    }

    #[test]
    fn display_stretch_round_trips() {
        let mut ui = json!({"other": 1});
        let d = DisplayStretch {
            linked: true,
            strong: true,
        };
        d.save(&mut ui);
        assert_eq!(ui["other"], 1);
        assert_eq!(
            ui["display_stretch"],
            json!({"linked": true, "strong": true})
        );
        assert_eq!(DisplayStretch::load(&ui), d);
        let mut not_obj = json!(5);
        d.save(&mut not_obj);
        assert_eq!(DisplayStretch::load(&not_obj), d);
    }

    #[test]
    fn params_follow_the_toggles() {
        let stats = [
            ChannelStats {
                median: 0.1,
                mad: 0.01,
            },
            ChannelStats {
                median: 0.2,
                mad: 0.02,
            },
        ];
        let linked = DisplayStretch {
            linked: true,
            strong: false,
        }
        .params(&stats);
        assert_eq!(linked[0], linked[1]);
        let strong = DisplayStretch {
            linked: false,
            strong: true,
        }
        .params(&stats);
        assert!((apply(&strong[0], 0.1) - 0.45).abs() < 1e-4);
        assert!((apply(&strong[1], 0.2) - 0.45).abs() < 1e-4);
    }
}
