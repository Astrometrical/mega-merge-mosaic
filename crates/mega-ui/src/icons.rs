//! Lucide icon glyphs (lucide-static 1.60.0, ISC licence).
//!
//! Always draw these in the icon family ([`font`], [`rich`]), never in a
//! text font: Inter has its own glyphs in the private-use range Lucide
//! uses, so an icon in ordinary text shows the wrong symbol. Codepoints
//! come from the package's `font/codepoints.json`; they are not stable
//! across Lucide versions, so bump the font and this table together.

use egui::{FontId, RichText};

use crate::theme::fonts::icon_family;

/// The icon font at `size` points.
pub fn font(size: f32) -> FontId {
    FontId::new(size, icon_family())
}

/// `glyph` as rich text in the icon font at `size` points.
pub fn rich(glyph: char, size: f32) -> RichText {
    RichText::new(glyph).font(font(size))
}

/// `plus`
pub const PLUS: char = '\u{e13d}';
/// `pin`
pub const PIN: char = '\u{e259}';
/// `pin-off`
pub const PIN_OFF: char = '\u{e2b6}';
/// `circle-x`: problems.
pub const CIRCLE_X: char = '\u{e084}';
/// `triangle-alert`: warnings and notes.
pub const TRIANGLE_ALERT: char = '\u{e193}';
/// `check`: passed, done.
pub const CHECK: char = '\u{e06c}';
/// `info`
pub const INFO: char = '\u{e0f9}';
/// `chevron-right`
pub const CHEVRON_RIGHT: char = '\u{e06f}';
/// `chevron-left`
pub const CHEVRON_LEFT: char = '\u{e06e}';
/// `chevron-down`
pub const CHEVRON_DOWN: char = '\u{e06d}';
/// `chevron-up`
pub const CHEVRON_UP: char = '\u{e070}';
/// `arrow-left`
pub const ARROW_LEFT: char = '\u{e048}';
/// `search`
pub const SEARCH: char = '\u{e151}';
/// `folder-open`
pub const FOLDER_OPEN: char = '\u{e247}';
/// `folder-plus`
pub const FOLDER_PLUS: char = '\u{e0d9}';
/// `play`
pub const PLAY: char = '\u{e13c}';
/// `pause`
pub const PAUSE: char = '\u{e12e}';
/// `skip-forward`
pub const SKIP_FORWARD: char = '\u{e160}';
/// `x`: close, dismiss.
pub const X: char = '\u{e1b2}';
/// `pencil`
pub const PENCIL: char = '\u{e1f9}';
/// `copy`
pub const COPY: char = '\u{e09e}';
/// `external-link`
pub const EXTERNAL_LINK: char = '\u{e0b9}';
/// `zoom-in`
pub const ZOOM_IN: char = '\u{e1b6}';
/// `zoom-out`
pub const ZOOM_OUT: char = '\u{e1b7}';
/// `house`: view all / home.
pub const HOUSE: char = '\u{e0f5}';
/// `maximize-2`
pub const MAXIMIZE_2: char = '\u{e113}';

/// Every glyph above with its Lucide name, for tests and the gallery.
pub const ALL: &[(&str, char)] = &[
    ("plus", PLUS),
    ("pin", PIN),
    ("pin-off", PIN_OFF),
    ("circle-x", CIRCLE_X),
    ("triangle-alert", TRIANGLE_ALERT),
    ("check", CHECK),
    ("info", INFO),
    ("chevron-right", CHEVRON_RIGHT),
    ("chevron-left", CHEVRON_LEFT),
    ("chevron-down", CHEVRON_DOWN),
    ("arrow-left", ARROW_LEFT),
    ("search", SEARCH),
    ("folder-open", FOLDER_OPEN),
    ("folder-plus", FOLDER_PLUS),
    ("play", PLAY),
    ("pause", PAUSE),
    ("skip-forward", SKIP_FORWARD),
    ("x", X),
    ("pencil", PENCIL),
    ("copy", COPY),
    ("external-link", EXTERNAL_LINK),
    ("zoom-in", ZOOM_IN),
    ("zoom-out", ZOOM_OUT),
    ("house", HOUSE),
    ("maximize-2", MAXIMIZE_2),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::fonts::icon_family;

    #[test]
    fn icons_use_the_icon_family() {
        assert_eq!(font(14.0).family, icon_family());
        assert_eq!(font(14.0).size, 14.0);
        assert_eq!(rich(PLUS, 14.0).text(), PLUS.to_string());
    }
}
