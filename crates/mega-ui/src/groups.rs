//! Group colours: the palette groups take their colour from, with
//! physical colours for well-known filter names. Shared by the Mega apps;
//! the assignment and persistence of colours stay with each app.

use egui::Color32;

use crate::theme::palette;

/// Sulphur II: a warm orange-red, clearly apart from Hα's crimson
/// ([`palette::HA_500`]) so SII and Hα groups never look alike.
pub const SII_COLOR: Color32 = Color32::from_rgb(0xe0, 0x70, 0x3a);

/// Group colours. The first six are physical colours that well-known
/// filter names get (see [`physical_color`]): Hα / red, green, OIII
/// cyan, SII, blue, luminance white. Other groups take the first of the
/// remaining entries no group uses yet, in first-seen order (cycling).
pub const GROUP_PALETTE: [Color32; 11] = [
    palette::HA_500,
    palette::GREEN_400,
    palette::ACCENT_400,
    SII_COLOR,
    palette::ACCENT_600,
    palette::TEXT_STRONG,
    Color32::from_rgb(0xd4, 0xa2, 0x2a), // amber
    Color32::from_rgb(0x8e, 0x6a, 0xd4), // violet
    Color32::from_rgb(0xc8, 0x4a, 0x9e), // magenta
    Color32::from_rgb(0x9c, 0xc2, 0x3a), // lime
    Color32::from_rgb(0x6a, 0x8e, 0xd4), // periwinkle
];

/// How many leading [`GROUP_PALETTE`] entries are physical colours, kept
/// for well-known filter names.
pub const PHYSICAL_SLOTS: usize = 6;

/// The physical [`GROUP_PALETTE`] index of a well-known filter name
/// (case-insensitive), if it is one.
pub fn physical_color(name: &str) -> Option<usize> {
    match name.to_ascii_lowercase().as_str() {
        "ha" | "h" | "halpha" | "h-alpha" | "r" | "red" => Some(0),
        "g" | "green" => Some(1),
        "oiii" | "o3" | "o" => Some(2),
        "sii" | "s2" | "s" => Some(3),
        "b" | "blue" => Some(4),
        "l" | "lum" => Some(5),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use egui::Color32;

    use super::*;

    #[test]
    fn palette_order_is_frozen() {
        // Saved sessions store indices into this array (`ui.group_colors`);
        // never reorder it.
        use crate::theme::palette::*;
        assert_eq!(
            GROUP_PALETTE[..6],
            [
                HA_500,
                GREEN_400,
                ACCENT_400,
                SII_COLOR,
                ACCENT_600,
                TEXT_STRONG
            ]
        );
        assert_eq!(SII_COLOR, Color32::from_rgb(0xe0, 0x70, 0x3a));
        assert_eq!(GROUP_PALETTE.len(), 11);
        assert_eq!(PHYSICAL_SLOTS, 6);
        assert_eq!(physical_color("SII"), Some(3));
        assert_eq!(physical_color("r"), Some(0));
        assert_eq!(physical_color("Lum"), Some(5));
        assert_eq!(physical_color("Foo"), None);
    }
}
