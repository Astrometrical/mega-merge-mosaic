//! Serialisable analyze/blend settings, per-group overrides, and change
//! impact classification.
//!
//! Defaults match the CLI (`mmm analyze` / `mmm blend`). Analyze settings are
//! *global*: changing any reruns analysis and every result tile goes stale.
//! Blend settings are *blend-local*: analysis is kept, existing result tiles
//! go stale, and an ROI reblend shows the effect.

use serde::{Deserialize, Serialize};

use crate::analyze::InputSelect;
use crate::blend::{BlendMode, BlendParams};
use crate::error::{Error, Result};
use crate::photometry::GainMode;

/// Settings that drive analysis (global: a change reruns analysis). A field
/// missing from the JSON takes its default, so files written before a field
/// existed still load.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct AnalyzeSettings {
    /// How input panels are interpreted.
    pub input: InputSelect,
    /// Residual surface order (`None` = off, 0 constant, 1 plane, 2 quadratic).
    pub surface: Option<u32>,
    /// Photometric gain handling.
    pub gain: GainMode,
}

impl Default for AnalyzeSettings {
    fn default() -> Self {
        Self {
            input: InputSelect::Auto,
            surface: Some(2),
            gain: GainMode::Fit,
        }
    }
}

/// Settings that drive blending (blend-local: analysis is kept). A field
/// missing from the JSON takes its default, so files written before a field
/// existed still load.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct BlendSettings {
    /// Blend mode.
    pub mode: BlendMode,
    /// Feather ramp length in canvas pixels.
    pub feather_px: f32,
    /// Cross-panel defect veto in the TwoBand detail stage.
    pub defect_veto: bool,
    /// Global background flatten polynomial order (`None` = off).
    pub flatten: Option<u32>,
}

impl Default for BlendSettings {
    fn default() -> Self {
        let p = BlendParams::default();
        Self {
            mode: p.mode,
            feather_px: p.feather_px,
            defect_veto: p.defect_veto,
            flatten: p.flatten,
        }
    }
}

impl BlendSettings {
    /// Check the settings can drive a blend: the feather ramp must be finite
    /// and positive. Called by every settings edit and by
    /// [`crate::mosaic::MosaicConfig::save`], so invalid settings are never
    /// persisted (JSON cannot represent NaN or infinity).
    pub fn validate(&self) -> Result<()> {
        if !(self.feather_px.is_finite() && self.feather_px > 0.0) {
            return Err(Error::compute(format!(
                "feather must be a finite number of pixels greater than 0 (got {})",
                self.feather_px
            )));
        }
        Ok(())
    }

    /// Engine parameters for these settings; other fields at their defaults.
    pub fn to_params(&self) -> BlendParams {
        BlendParams {
            mode: self.mode,
            feather_px: self.feather_px,
            defect_veto: self.defect_veto,
            flatten: self.flatten,
            ..BlendParams::default()
        }
    }

    /// These settings with every field set in `o` replaced.
    pub fn with(&self, o: &BlendOverride) -> BlendSettings {
        BlendSettings {
            mode: o.mode.unwrap_or(self.mode),
            feather_px: o.feather_px.unwrap_or(self.feather_px),
            defect_veto: o.defect_veto.unwrap_or(self.defect_veto),
            flatten: o.flatten.unwrap_or(self.flatten),
        }
    }
}

impl AnalyzeSettings {
    /// These settings with every field set in `o` replaced.
    pub fn with(&self, o: &AnalyzeOverride) -> AnalyzeSettings {
        AnalyzeSettings {
            input: o.input.unwrap_or(self.input),
            surface: o.surface.unwrap_or(self.surface),
            gain: o.gain.unwrap_or(self.gain),
        }
    }
}

/// (De)serialiser keeping `Some(None)` (explicit null) distinct from `None`
/// (absent): absent -> `None`, null -> `Some(None)`, value -> `Some(Some(v))`.
mod double_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<Option<u32>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(inner) => inner.serialize(s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<u32>>, D::Error> {
        Option::<u32>::deserialize(d).map(Some)
    }
}

/// Per-group override of [`AnalyzeSettings`]; unset fields inherit.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct AnalyzeOverride {
    /// Override for [`AnalyzeSettings::input`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<InputSelect>,
    /// Override for [`AnalyzeSettings::surface`] (outer `Some` = set).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub surface: Option<Option<u32>>,
    /// Override for [`AnalyzeSettings::gain`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gain: Option<GainMode>,
}

/// Per-group override of [`BlendSettings`]; unset fields inherit.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct BlendOverride {
    /// Override for [`BlendSettings::mode`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<BlendMode>,
    /// Override for [`BlendSettings::feather_px`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feather_px: Option<f32>,
    /// Override for [`BlendSettings::defect_veto`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defect_veto: Option<bool>,
    /// Override for [`BlendSettings::flatten`] (outer `Some` = set).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub flatten: Option<Option<u32>>,
}

/// A group's analyze and blend overrides.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct GroupOverride {
    /// Analyze-setting overrides.
    #[serde(default)]
    pub analyze: AnalyzeOverride,
    /// Blend-setting overrides.
    #[serde(default)]
    pub blend: BlendOverride,
}

impl GroupOverride {
    /// True when nothing is overridden.
    pub fn is_empty(&self) -> bool {
        *self == GroupOverride::default()
    }
}

/// How much work a settings edit invalidates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChangeImpact {
    /// Nothing changed.
    None,
    /// Analysis kept; existing result tiles go stale (blend-local).
    Reblend,
    /// Analysis reruns and every result tile goes stale (global).
    Reanalyze,
}

/// Impact of changing analyze settings (global): any difference reanalyzes.
pub fn analyze_change(old: &AnalyzeSettings, new: &AnalyzeSettings) -> ChangeImpact {
    if old == new {
        ChangeImpact::None
    } else {
        ChangeImpact::Reanalyze
    }
}

/// Impact of changing blend settings (blend-local): any difference reblends.
pub fn blend_change(old: &BlendSettings, new: &BlendSettings) -> ChangeImpact {
    if old == new {
        ChangeImpact::None
    } else {
        ChangeImpact::Reblend
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_cli() {
        let p = BlendSettings::default().to_params();
        let d = BlendParams::default();
        assert_eq!(p.mode, d.mode);
        assert_eq!(p.feather_px, d.feather_px);
        assert_eq!(p.defect_veto, d.defect_veto);
        assert_eq!(p.flatten, d.flatten);
        let a = AnalyzeSettings::default();
        assert_eq!(a.input, InputSelect::Auto);
        assert_eq!(a.surface, Some(2));
        assert_eq!(a.gain, GainMode::Fit);
    }

    #[test]
    fn serde_names_match_cli() {
        let a = AnalyzeSettings {
            input: InputSelect::Solved,
            surface: None,
            gain: GainMode::Unity,
        };
        let s = serde_json::to_string(&a).unwrap();
        assert_eq!(s, r#"{"input":"solved","surface":null,"gain":"unity"}"#);
        assert_eq!(serde_json::from_str::<AnalyzeSettings>(&s).unwrap(), a);
        assert_eq!(
            serde_json::to_string(&BlendMode::TwoBand).unwrap(),
            "\"twoband\""
        );
    }

    #[test]
    fn override_applies_only_set_fields() {
        let base = BlendSettings::default();
        let o = BlendOverride {
            mode: Some(BlendMode::Feather),
            ..Default::default()
        };
        let got = base.with(&o);
        assert_eq!(
            got,
            BlendSettings {
                mode: BlendMode::Feather,
                ..base.clone()
            }
        );
        let a = AnalyzeSettings::default().with(&AnalyzeOverride {
            surface: Some(None),
            ..Default::default()
        });
        assert_eq!(a.surface, None);
        assert_eq!(a.gain, GainMode::Fit);
    }

    #[test]
    fn populated_overrides_roundtrip() {
        for surface in [None, Some(None), Some(Some(1))] {
            let o = AnalyzeOverride {
                surface,
                ..Default::default()
            };
            let s = serde_json::to_string(&o).unwrap();
            assert_eq!(
                serde_json::from_str::<AnalyzeOverride>(&s).unwrap(),
                o,
                "{s}"
            );
        }
        for flatten in [None, Some(None), Some(Some(1))] {
            let o = BlendOverride {
                flatten,
                ..Default::default()
            };
            let s = serde_json::to_string(&o).unwrap();
            assert_eq!(serde_json::from_str::<BlendOverride>(&s).unwrap(), o, "{s}");
        }
    }

    #[test]
    fn empty_override_serialises_to_empty_object() {
        let g = GroupOverride::default();
        assert!(g.is_empty());
        assert_eq!(
            serde_json::to_string(&g).unwrap(),
            r#"{"analyze":{},"blend":{}}"#
        );
    }

    #[test]
    fn empty_object_deserialises_to_defaults() {
        assert_eq!(
            serde_json::from_str::<AnalyzeSettings>("{}").unwrap(),
            AnalyzeSettings::default()
        );
        assert_eq!(
            serde_json::from_str::<BlendSettings>("{}").unwrap(),
            BlendSettings::default()
        );
        // A partial object keeps the fields given.
        let b: BlendSettings = serde_json::from_str(r#"{"feather_px": 12.5}"#).unwrap();
        assert_eq!(b.feather_px, 12.5);
        assert_eq!(b.mode, BlendSettings::default().mode);
    }

    #[test]
    fn validate_rejects_bad_feather() {
        assert!(BlendSettings::default().validate().is_ok());
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0, -1.0] {
            let b = BlendSettings {
                feather_px: bad,
                ..BlendSettings::default()
            };
            let err = b.validate().unwrap_err().to_string();
            assert!(err.contains("feather"), "{bad}: {err}");
        }
    }

    #[test]
    fn change_impact_classifies() {
        let b = BlendSettings::default();
        let b2 = BlendSettings {
            feather_px: 100.0,
            ..b.clone()
        };
        assert_eq!(blend_change(&b, &b2), ChangeImpact::Reblend);
        assert_eq!(blend_change(&b, &b), ChangeImpact::None);
        let a = AnalyzeSettings::default();
        let a2 = AnalyzeSettings {
            gain: GainMode::Unity,
            ..a.clone()
        };
        assert_eq!(analyze_change(&a, &a2), ChangeImpact::Reanalyze);
        assert_eq!(analyze_change(&a, &a), ChangeImpact::None);
        assert!(ChangeImpact::Reanalyze > ChangeImpact::Reblend);
    }
}
