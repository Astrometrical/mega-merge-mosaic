//! Filter bands: recognising a group as one of the usual imaging bands
//! (R, G, B, Ha, SII, OIII) from its name, and the composite presets
//! (RGB, SHO, HOO, HSO) that the recognised groups allow.
//!
//! Recognition looks at the group's name only. The FILTER values of the
//! group's panels live in the panel list's header facts, which are read on
//! a worker and are not reachable from the session without I/O; a group
//! made by FILTER is named by that value anyway, and one named by hand can
//! be named for its band.

use std::collections::BTreeMap;

use crate::compositions::Channels;
use crate::session::OpenSession;
use crate::util::group_names;

/// An imaging band the presets use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Band {
    /// Red.
    R,
    /// Green.
    G,
    /// Blue.
    B,
    /// Hydrogen alpha.
    Ha,
    /// Sulphur II.
    Sii,
    /// Oxygen III.
    Oiii,
}

/// A bandwidth such as "7nm" or "3.5nm".
fn is_bandwidth(token: &str) -> bool {
    token
        .strip_suffix("nm")
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit() || c == '.'))
}

/// The band `text` (a group name or filter value) names, if any. Case,
/// spaces, hyphens, underscores and a bandwidth ("7nm") do not matter:
/// "Ha", "H-alpha", "Halpha", "Ha 7nm" are Ha; "OIII", "O3", "O-III",
/// "OIII 3nm" are OIII; "SII", "S2", "S-II" are SII; "R"/"Red",
/// "G"/"Green", "B"/"Blue" are the colours.
pub fn recognise(text: &str) -> Option<Band> {
    let lower = text.to_lowercase();
    let mut joined = String::new();
    for token in lower.split(|c: char| c.is_whitespace() || c == '-' || c == '_') {
        if !token.is_empty() && !is_bandwidth(token) {
            joined.push_str(token);
        }
    }
    // A bandwidth stuck to the name: "ha7nm".
    if let Some(stem) = joined.strip_suffix("nm") {
        let stem = stem.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
        if !stem.is_empty() {
            joined = stem.to_string();
        }
    }
    match joined.as_str() {
        "r" | "red" => Some(Band::R),
        "g" | "green" => Some(Band::G),
        "b" | "blue" => Some(Band::B),
        "ha" | "halpha" | "h\u{3b1}" => Some(Band::Ha),
        "sii" | "s2" => Some(Band::Sii),
        "oiii" | "o3" => Some(Band::Oiii),
        _ => None,
    }
}

/// The group each band is, from the group `names`. A band claimed by two
/// groups is left out (ambiguous), as is a group that is no band.
pub fn bands_of(names: &[String]) -> BTreeMap<Band, String> {
    let mut found: BTreeMap<Band, Vec<&String>> = BTreeMap::new();
    for name in names {
        if let Some(band) = recognise(name) {
            found.entry(band).or_default().push(name);
        }
    }
    found
        .into_iter()
        .filter_map(|(band, groups)| match groups[..] {
            [only] => Some((band, only.clone())),
            _ => None,
        })
        .collect()
}

/// The group each band is in `session` ([`bands_of`] its group names).
pub fn group_bands(session: &OpenSession) -> BTreeMap<Band, String> {
    bands_of(&group_names(session))
}

/// A composite made of bands: its label (and the new composite's name) and
/// the band each of R, G and B shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preset {
    /// The preset's name.
    pub label: &'static str,
    /// The band of the R, G and B channels.
    pub channels: [Band; 3],
}

/// Every preset, in the order they are offered.
pub const PRESETS: [Preset; 4] = [
    Preset {
        label: "RGB",
        channels: [Band::R, Band::G, Band::B],
    },
    Preset {
        label: "SHO",
        channels: [Band::Sii, Band::Ha, Band::Oiii],
    },
    Preset {
        label: "HOO",
        channels: [Band::Ha, Band::Oiii, Band::Oiii],
    },
    Preset {
        label: "HSO",
        channels: [Band::Ha, Band::Sii, Band::Oiii],
    },
];

/// The presets whose every band is a group in `bands` (a band without one,
/// or ambiguous, drops the presets that use it).
pub fn presets_for(bands: &BTreeMap<Band, String>) -> Vec<Preset> {
    PRESETS
        .into_iter()
        .filter(|p| p.channels.iter().all(|b| bands.contains_key(b)))
        .collect()
}

impl Preset {
    /// The channels this preset maps in `bands`, each at weight 1.
    pub fn mapping(&self, bands: &BTreeMap<Band, String>) -> Channels {
        self.channels
            .map(|b| bands.get(&b).map(|g| (g.clone(), 1.0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognise_filter_spellings() {
        let table = [
            ("Ha", Some(Band::Ha)),
            ("H-alpha", Some(Band::Ha)),
            ("Halpha", Some(Band::Ha)),
            ("Ha 7nm", Some(Band::Ha)),
            ("ha7nm", Some(Band::Ha)),
            ("H_alpha", Some(Band::Ha)),
            ("OIII", Some(Band::Oiii)),
            ("O3", Some(Band::Oiii)),
            ("O-III", Some(Band::Oiii)),
            ("OIII 3nm", Some(Band::Oiii)),
            ("o3 3nm", Some(Band::Oiii)),
            ("SII", Some(Band::Sii)),
            ("S2", Some(Band::Sii)),
            ("S-II", Some(Band::Sii)),
            ("sii 6.5nm", Some(Band::Sii)),
            ("R", Some(Band::R)),
            ("Red", Some(Band::R)),
            ("G", Some(Band::G)),
            ("green", Some(Band::G)),
            ("B", Some(Band::B)),
            ("BLUE", Some(Band::B)),
            ("  Ha  ", Some(Band::Ha)),
            ("L", None),
            ("Lum", None),
            ("", None),
            ("7nm", None),
            ("Baader Ha", None),
            ("Panel 1", None),
            ("H", None),
        ];
        for (text, want) in table {
            assert_eq!(recognise(text), want, "{text:?}");
        }
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn labels(bands: &BTreeMap<Band, String>) -> Vec<&'static str> {
        presets_for(bands).iter().map(|p| p.label).collect()
    }

    #[test]
    fn presets_follow_the_recognised_groups() {
        // Ha and OIII: HOO only.
        let ho = bands_of(&names(&["Ha", "O3"]));
        assert_eq!(labels(&ho), ["HOO"]);
        assert_eq!(
            PRESETS[2].mapping(&ho),
            [
                Some(("Ha".to_string(), 1.0)),
                Some(("O3".to_string(), 1.0)),
                Some(("O3".to_string(), 1.0))
            ],
            "one OIII group for G and B"
        );
        // All three narrowband: SHO, HOO and HSO.
        let all = bands_of(&names(&["S2", "Ha", "OIII"]));
        assert_eq!(labels(&all), ["SHO", "HOO", "HSO"]);
        assert_eq!(
            PRESETS[1].mapping(&all)[0],
            Some(("S2".to_string(), 1.0)),
            "SHO puts SII in R"
        );
        // RGB wants all three colours.
        assert_eq!(
            labels(&bands_of(&names(&["Red", "Green", "Blue"]))),
            ["RGB"]
        );
        assert!(labels(&bands_of(&names(&["Red", "Green"]))).is_empty());
        // Unknown groups are no band, and a lone Ha offers nothing.
        assert!(labels(&bands_of(&names(&["Ha", "Lum"]))).is_empty());
        // A band two groups claim is ambiguous: it drops its presets.
        let dup = bands_of(&names(&["Ha", "Ha 7nm", "OIII", "SII"]));
        assert!(!dup.contains_key(&Band::Ha));
        assert!(labels(&dup).is_empty());
        // An ambiguous OIII leaves the colour presets alone.
        let mixed = bands_of(&names(&["R", "G", "B", "OIII", "O3"]));
        assert_eq!(labels(&mixed), ["RGB"]);
    }
}
