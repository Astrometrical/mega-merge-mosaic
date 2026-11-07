//! Astrometrical brand palette.
//!
//! Mirrors the web app's tokens (`apps/web/src/styles/app.css` in the
//! Astrometrical repo); each constant names the web token it mirrors.

use egui::Color32;

/// Darkest surface, the window background (`#050505`, web `astro-900`).
pub const SURFACE_900: Color32 = Color32::from_rgb(0x05, 0x05, 0x05);
/// Panel surface (`#0e0e0e`, web `astro-800`).
pub const SURFACE_800: Color32 = Color32::from_rgb(0x0e, 0x0e, 0x0e);
/// Raised surface: secondary buttons, chips (`#1a1a1a`, web `astro-700`).
pub const SURFACE_700: Color32 = Color32::from_rgb(0x1a, 0x1a, 0x1a);
/// Borders (`#2a2a2a`, web `astro-600`).
pub const BORDER: Color32 = Color32::from_rgb(0x2a, 0x2a, 0x2a);
/// Strong borders (`#444444`, web `astro-500`).
pub const BORDER_STRONG: Color32 = Color32::from_rgb(0x44, 0x44, 0x44);
/// Muted fills only; fails AA as text (`#777777`, web `astro-400`).
pub const FILL_MUTED: Color32 = Color32::from_rgb(0x77, 0x77, 0x77);
/// OIII accent, light (`#4EBCD4`, web `accent-400`).
pub const ACCENT_400: Color32 = Color32::from_rgb(0x4e, 0xbc, 0xd4);
/// OIII accent, primary (`#2A95AB`, web `accent-500`).
pub const ACCENT_500: Color32 = Color32::from_rgb(0x2a, 0x95, 0xab);
/// OIII accent, dark (`#1D7588`, web `accent-600`). The primary button
/// fill: white text on it is ~5.3:1.
pub const ACCENT_600: Color32 = Color32::from_rgb(0x1d, 0x75, 0x88);
/// Primary button hover (`#1F7F93`, new in the Mega apps): lighter than
/// [`ACCENT_600`] yet still AA with white text (~4.6:1), which the web's
/// `accent-400` hover is not.
pub const ACCENT_550: Color32 = Color32::from_rgb(0x1f, 0x7f, 0x93);
/// Primary button pressed (`#175F6E`, new in the Mega apps).
pub const ACCENT_700: Color32 = Color32::from_rgb(0x17, 0x5f, 0x6e);
/// Selection fill: selected tabs, highlighted text (`#123E48`, new in the
/// Mega apps); [`ACCENT_400`] text on it is ~5.2:1.
pub const ACCENT_800: Color32 = Color32::from_rgb(0x12, 0x3e, 0x48);
/// H-alpha red, for problem fills, borders and bars (`#C83050`, web
/// `ha-500`). Fails AA as small text: use [`HA_400`] for text.
pub const HA_500: Color32 = Color32::from_rgb(0xc8, 0x30, 0x50);
/// H-alpha red for text: problem messages and counts (`#E05A76`, new in
/// the Mega apps; ~5.4:1 on [`SURFACE_800`]).
pub const HA_400: Color32 = Color32::from_rgb(0xe0, 0x5a, 0x76);
/// SII gold, light: hover, light warning text (`#F5C76A`, new in the Mega
/// apps).
pub const SII_300: Color32 = Color32::from_rgb(0xf5, 0xc7, 0x6a);
/// SII gold: warning text and icons (`#E8A838`, new in the Mega apps;
/// ~9:1 on [`SURFACE_800`]).
pub const SII_400: Color32 = Color32::from_rgb(0xe8, 0xa8, 0x38);
/// SII gold, dark: warning fills and borders (`#A8741C`, new in the Mega
/// apps).
pub const SII_600: Color32 = Color32::from_rgb(0xa8, 0x74, 0x1c);
/// OIII green, light, for success (`#4ED090`, web `green-300`).
pub const GREEN_300: Color32 = Color32::from_rgb(0x4e, 0xd0, 0x90);
/// OIII green, primary (`#2AB875`, web `green-400`).
pub const GREEN_400: Color32 = Color32::from_rgb(0x2a, 0xb8, 0x75);
/// Strong text: headings, primary values (`#eeeeee`, web
/// `content-strong`).
pub const TEXT_STRONG: Color32 = Color32::from_rgb(0xee, 0xee, 0xee);
/// Emphasis text: lead text, values in property rows (`#d0d0d0`, web
/// `content-emphasis`).
pub const TEXT_EMPHASIS: Color32 = Color32::from_rgb(0xd0, 0xd0, 0xd0);
/// Body text, the default (`#aaaaaa`, web `content`).
pub const TEXT_BODY: Color32 = Color32::from_rgb(0xaa, 0xaa, 0xaa);
/// Muted text: secondary text, labels (`#999999`, web `content-muted`).
pub const TEXT_MUTED: Color32 = Color32::from_rgb(0x99, 0x99, 0x99);
/// Subtle text: captions, placeholders (`#8a8a8a`, web `content-subtle`).
pub const TEXT_SUBTLE: Color32 = Color32::from_rgb(0x8a, 0x8a, 0x8a);
/// Text on accent fills, e.g. primary buttons (white, as on the web).
pub const ON_ACCENT: Color32 = Color32::WHITE;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_matches_brand() {
        let rgb = |c: egui::Color32| (c.r(), c.g(), c.b());
        assert_eq!(rgb(SURFACE_900), (0x05, 0x05, 0x05));
        assert_eq!(rgb(SURFACE_800), (0x0e, 0x0e, 0x0e));
        assert_eq!(rgb(SURFACE_700), (0x1a, 0x1a, 0x1a));
        assert_eq!(rgb(BORDER), (0x2a, 0x2a, 0x2a));
        assert_eq!(rgb(BORDER_STRONG), (0x44, 0x44, 0x44));
        assert_eq!(rgb(FILL_MUTED), (0x77, 0x77, 0x77));
        assert_eq!(rgb(ACCENT_400), (0x4e, 0xbc, 0xd4));
        assert_eq!(rgb(ACCENT_500), (0x2a, 0x95, 0xab));
        assert_eq!(rgb(ACCENT_600), (0x1d, 0x75, 0x88));
        assert_eq!(rgb(ACCENT_550), (0x1f, 0x7f, 0x93));
        assert_eq!(rgb(ACCENT_700), (0x17, 0x5f, 0x6e));
        assert_eq!(rgb(ACCENT_800), (0x12, 0x3e, 0x48));
        assert_eq!(rgb(HA_500), (0xc8, 0x30, 0x50));
        assert_eq!(rgb(HA_400), (0xe0, 0x5a, 0x76));
        assert_eq!(rgb(SII_300), (0xf5, 0xc7, 0x6a));
        assert_eq!(rgb(SII_400), (0xe8, 0xa8, 0x38));
        assert_eq!(rgb(SII_600), (0xa8, 0x74, 0x1c));
        assert_eq!(rgb(GREEN_300), (0x4e, 0xd0, 0x90));
        assert_eq!(rgb(GREEN_400), (0x2a, 0xb8, 0x75));
        assert_eq!(rgb(TEXT_STRONG), (0xee, 0xee, 0xee));
        assert_eq!(rgb(TEXT_EMPHASIS), (0xd0, 0xd0, 0xd0));
        assert_eq!(rgb(TEXT_BODY), (0xaa, 0xaa, 0xaa));
        assert_eq!(rgb(TEXT_MUTED), (0x99, 0x99, 0x99));
        assert_eq!(rgb(TEXT_SUBTLE), (0x8a, 0x8a, 0x8a));
        assert_eq!(ON_ACCENT, Color32::WHITE);
    }
}
