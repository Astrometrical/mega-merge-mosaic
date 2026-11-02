//! Pre-flight and staleness checks. This module currently holds only the
//! stable identifiers; the checks themselves are added separately.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Identifies a kind of check. Persisted in `mosaic.json`: the serialised
/// names are stable and must never be renamed.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum CheckCode {
    /// A panel has no astrometric solution.
    SolutionPresent,
    /// A panel's distortion model is unsupported or inconsistent.
    DistortionModel,
    /// Panel pixel scales differ.
    ScaleMismatch,
    /// Panels mix colour and mono channels.
    MixedChannels,
    /// Groups hold different panel counts.
    GroupCountMismatch,
    /// Filters differ but grouping is off.
    FiltersDifferUngrouped,
    /// Some panels could not be placed in a group.
    Ungrouped,
    /// A panel file changed since it was added.
    FileChanged,
    /// A panel file is missing.
    FileMissing,
    /// A panel overlaps no other panel.
    NoOverlap,
    /// Panel sky levels differ strongly.
    SkyLevelMismatch,
}

/// A check result's identity: the check and, when panel-specific, the panel.
/// Used to remember which warnings the user has accepted.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CheckKey {
    /// Which check.
    pub code: CheckCode,
    /// The panel concerned, if any.
    pub panel: Option<PathBuf>,
}
