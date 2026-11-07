//! Embedded fonts: Inter (UI), JetBrains Mono (numbers) and Lucide
//! (icons). Licences are in `assets/fonts/` and listed in `NOTICE`.

use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily};

/// Font data name of Inter Regular (4.1, OFL-1.1).
pub const INTER_REGULAR: &str = "Inter-Regular";
/// Font data name of Inter SemiBold (4.1, OFL-1.1).
pub const INTER_SEMIBOLD: &str = "Inter-SemiBold";
/// Font data name of JetBrains Mono Regular (2.304, OFL-1.1).
pub const JETBRAINS_MONO: &str = "JetBrainsMono-Regular";
/// Font data name of the Lucide icon font (lucide-static 1.60.0, ISC).
pub const LUCIDE: &str = "lucide";

/// The semibold UI family (egui has no font-weight property, so each
/// weight is its own family).
pub fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

/// A family holding only the Lucide icon font.
pub fn icon_family() -> FontFamily {
    FontFamily::Name("icons".into())
}

/// egui's default fonts plus ours: Inter first in Proportional (and the
/// semibold family), JetBrains Mono first in Monospace, the defaults
/// (emoji and more) kept after them, and Lucide alone in
/// [`icon_family`]. Lucide is in no text chain: Inter has glyphs in the
/// private-use range Lucide uses, and Lucide has digits.
pub fn definitions() -> FontDefinitions {
    let mut d = FontDefinitions::default();
    let add = |d: &mut FontDefinitions, name: &str, bytes: &'static [u8]| {
        d.font_data
            .insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
    };
    add(
        &mut d,
        INTER_REGULAR,
        include_bytes!("../../assets/fonts/Inter-Regular.ttf"),
    );
    add(
        &mut d,
        INTER_SEMIBOLD,
        include_bytes!("../../assets/fonts/Inter-SemiBold.ttf"),
    );
    add(
        &mut d,
        JETBRAINS_MONO,
        include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf"),
    );
    add(
        &mut d,
        LUCIDE,
        include_bytes!("../../assets/fonts/lucide.ttf"),
    );

    let defaults_prop = d
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let defaults_mono = d
        .families
        .get(&FontFamily::Monospace)
        .cloned()
        .unwrap_or_default();
    let chain = |first: &str, rest: &[String]| {
        let mut v = vec![first.to_owned()];
        v.extend(rest.iter().cloned());
        v
    };
    d.families.insert(
        FontFamily::Proportional,
        chain(INTER_REGULAR, &defaults_prop),
    );
    d.families
        .insert(FontFamily::Monospace, chain(JETBRAINS_MONO, &defaults_mono));
    d.families
        .insert(semibold(), chain(INTER_SEMIBOLD, &defaults_prop));
    d.families.insert(icon_family(), vec![LUCIDE.to_owned()]);
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{FontFamily, FontId};

    #[test]
    fn families_put_our_fonts_first_and_keep_fallbacks() {
        let d = definitions();
        let p = &d.families[&FontFamily::Proportional];
        assert_eq!(p[0], INTER_REGULAR);
        // Inter has private-use glyphs where Lucide keeps its icons, and
        // Lucide has digits: icons must never come from a text chain.
        for family in [FontFamily::Proportional, FontFamily::Monospace, semibold()] {
            assert!(
                !d.families[&family].iter().any(|f| f == LUCIDE),
                "{family:?} must not fall back to Lucide"
            );
        }
        assert!(
            p.iter().any(|f| f == "NotoEmoji-Regular"),
            "emoji fallback kept: {p:?}"
        );
        assert_eq!(d.families[&FontFamily::Monospace][0], JETBRAINS_MONO);
        assert_eq!(d.families[&semibold()][0], INTER_SEMIBOLD);
        assert_eq!(d.families[&icon_family()], vec![LUCIDE.to_owned()]);
    }

    #[test]
    fn every_icon_is_in_the_lucide_font() {
        let ctx = egui::Context::default();
        ctx.set_fonts(definitions());
        // Fonts load on the next frame; this test has no renderer to take
        // the frame's texture updates.
        ctx.run_ui(Default::default(), |_| {})
            .textures_delta
            .clear();
        ctx.fonts_mut(|f| {
            // The icon family is Lucide alone: it has no Latin letters.
            assert!(!f.has_glyph(&FontId::new(13.0, icon_family()), 'A'));
            for (name, c) in crate::icons::ALL {
                assert!(
                    f.has_glyph(&FontId::new(13.0, icon_family()), *c),
                    "{name} {c:?}"
                );
            }
        });
    }
}
