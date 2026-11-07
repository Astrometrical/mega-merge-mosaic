//! The type scale: named text styles and their role colours.

use egui::{RichText, TextStyle};

use super::palette;

/// Overline size in points (also in [`super::style::style`]).
pub(crate) const OVERLINE_SIZE: f32 = 10.5;

/// A step of the type scale.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Type {
    /// 32 pt semibold: the session name on the start screen.
    Hero,
    /// 20 pt semibold: view titles.
    Title,
    /// 15 pt semibold: section headings.
    Heading,
    /// 13 pt: default text and buttons.
    Body,
    /// 12 pt: metadata and secondary lines.
    Small,
    /// 10.5 pt semibold caps, tracked: section labels.
    Overline,
    /// 12 pt mono: live numbers, sizes, coordinates, paths.
    Numeric,
}

impl Type {
    /// The egui text style registered for this step.
    pub fn text_style(self) -> TextStyle {
        match self {
            Type::Hero => TextStyle::Name("hero".into()),
            Type::Title => TextStyle::Name("title".into()),
            Type::Heading => TextStyle::Heading,
            Type::Body => TextStyle::Body,
            Type::Small => TextStyle::Small,
            Type::Overline => TextStyle::Name("overline".into()),
            Type::Numeric => TextStyle::Name("numeric".into()),
        }
    }

    /// The text role colour this step uses by default.
    pub fn color(self) -> egui::Color32 {
        match self {
            Type::Hero | Type::Title | Type::Heading => palette::TEXT_STRONG,
            Type::Body => palette::TEXT_BODY,
            Type::Small => palette::TEXT_MUTED,
            Type::Overline => palette::TEXT_SUBTLE,
            Type::Numeric => palette::TEXT_EMPHASIS,
        }
    }
}

/// `s` in the style and role colour of `t`. Overlines are upper-cased and
/// tracked (+0.12em).
pub fn text(t: Type, s: impl Into<String>) -> RichText {
    let s: String = s.into();
    let r = match t {
        Type::Overline => {
            RichText::new(s.to_uppercase()).extra_letter_spacing(0.12 * OVERLINE_SIZE)
        }
        _ => RichText::new(s),
    };
    r.text_style(t.text_style()).color(t.color())
}
