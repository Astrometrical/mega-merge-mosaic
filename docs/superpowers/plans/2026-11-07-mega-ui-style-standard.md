# Mega UI Style Standard Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn `mega-ui` into the shared Mega UI standard: colour, type, spacing, radius and motion tokens; Inter, JetBrains Mono and Lucide fonts; restyled and new components; the brand lockup; and a gallery with visual tests. `mmm-app` picks all of it up.

**Architecture:** `mega_ui::theme` becomes a module tree: palette, tokens, fonts, text and style. `theme::apply` installs a complete `egui::Style` and the font definitions, and forces the dark theme. Components live in `mega_ui::widgets` (new) beside the existing `checks`, `jobs` and `status` modules. They use only tokens and know nothing about mmm. A `mega_ui::gallery` module renders every token and component. The `gallery` example and the snapshot tests both use it.

**Tech Stack:** Rust 2024, egui/eframe/egui_kittest 0.36.2 (glow), Inter 4.1, JetBrains Mono 2.304, lucide-static 1.60.0, the `png` crate (workspace).

**Spec:** `docs/superpowers/specs/2026-11-07-ui-standard-and-start-screen-design.md`, §4 and the parts of §6 and §8 that cover the standard. Plan 2 (the start screen, spec §5) builds on this one: the starfield, hero stage and session card belong to that plan.

## Global Constraints

- egui, eframe and egui_kittest stay on the workspace 0.36 pins. No third-party egui crates.
- `mega-ui` must not depend on `mmm-core`.
- Tests must not read `test_data/`.
- Every public item has a doc comment. `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and `cargo doc` stay warning-free.
- New code takes colours, sizes, radii and durations from tokens. The only literals allowed are inside the token modules.
- The apps are dark-themed only.
- Token values, verbatim from the spec:
  - Surfaces: astro-900 `#050505`, astro-800 `#0e0e0e`, astro-700 `#1a1a1a`, astro-600 `#2a2a2a`, astro-500 `#444444`, astro-400 `#777777`.
  - Text: strong `#eeeeee`, emphasis `#d0d0d0`, body `#aaaaaa`, muted `#999999`, subtle `#8a8a8a`.
  - Accent: `#4ebcd4` / `#2a95ab` / `#1d7588`.
  - Green: `#4ed090` / `#2ab875`.
  - Hα: ha-500 `#c83050`, ha-400 `#e05a76`.
  - SII: sii-300 `#f5c76a`, sii-400 `#e8a838`, sii-600 `#a8741c`.
- Type scale (points): hero 32 semibold, title 20 semibold, heading 15 semibold, body 13, small 12, overline 10.5 semibold caps with +0.12em tracking, numeric mono 12.
- Spacing steps: 4 · 8 · 12 · 16 · 24 · 32. Radius: 4 for small controls, 6 for buttons and inputs, 8 for cards and panels, pill for chips. Controls are 30 px high; large buttons are 38 px. Hover fades take 100 ms.
- Text colours meet WCAG 2.2 AA (at least 4.5:1) on astro-900, -800 and -700. Severity is never shown by colour alone.

## Review Focus

1. **The OS is in light mode.** The app must still be dark, including if the OS switches theme while the app is running. Task 3 pins this down.
2. **Session names or paths with emoji or non-Latin characters.** Inter lacks those glyphs, so egui's default fallback fonts must stay in the chain. Task 2 pins this down.
3. **Existing sessions keep their group colours.** `ui.group_colors` in `mosaic.json` stores palette indices, so moving the palette must not reorder it. Task 10 pins this down.
4. **Long group names** (for example `Ha_3nm_2026-10-12_flat-corrected`) in chips must truncate, with the full name on hover, and never push the layout wider. Task 5 pins this down.
5. **A burst of notifications** (say ten exports finishing at once) shows at most three toasts. They never cover the window or pile up forever. Task 9 pins this down.

---

### Task 1: Colour tokens, tones, contrast

**Files:**
- Create: `crates/mega-ui/src/theme/mod.rs`, `crates/mega-ui/src/theme/palette.rs`. This replaces `crates/mega-ui/src/theme.rs`; move its contents.
- Modify: `crates/mega-ui/src/checks.rs`, `crates/mega-ui/src/status.rs` (imports only, if paths change). Every `mmm-app` file that passes `palette::HA_500` as a *text* colour.
- Test: the `#[cfg(test)]` modules in `theme/mod.rs` and `theme/palette.rs`.

**Interfaces:**
- Produces:
  - `mega_ui::theme::palette` keeps every existing constant name and value and adds:
    - `TEXT_EMPHASIS` `#d0d0d0`
    - `BORDER_STRONG` `#444444`
    - `FILL_MUTED` `#777777`
    - `HA_400` `#e05a76`
    - `SII_300` `#f5c76a`, `SII_400` `#e8a838`, `SII_600` `#a8741c`
    - `ON_ACCENT` (white text on accent fills)

    Each doc comment names the web token it mirrors (for example "astro-500").
  - `pub enum Tone { Success, Info, Warning, Problem, Neutral }` with:
    - `pub fn color(self) -> Color32`, the text and icon colour: `GREEN_300`, `ACCENT_400`, `SII_400`, `HA_400`, `TEXT_MUTED`;
    - `pub fn accent(self) -> Color32`, for borders and bars: `GREEN_400`, `ACCENT_500`, `SII_400`, `HA_500`, `BORDER_STRONG`;
    - `pub fn tint(self) -> Color32`, a background tint: `accent()` with alpha 20 (unmultiplied).
  - `impl From<Severity> for Tone`: Problem→Problem, Note→Warning, Passed→Success.
  - `severity_color(s)` returns `Tone::from(s).color()`. That changes Problem to `HA_400` and Note to `SII_400`.
  - `pub fn contrast_ratio(a: Color32, b: Color32) -> f32` (WCAG 2.x relative luminance; alpha ignored).
  - `pub fn legible_text(tint: Color32, bg: Color32) -> Color32`: returns `tint` if it already reaches 4.5:1 on `bg`. Otherwise it blends `tint` toward white in small steps until it does.

- [ ] **Step 0: Commit the approved spec and this plan**

```bash
git add docs/superpowers/specs/2026-11-07-ui-standard-and-start-screen-design.md docs/superpowers/plans/2026-11-07-mega-ui-style-standard.md
git commit -m "docs: UI standard and start screen design, plan 1"
```

- [ ] **Step 1: Write the failing tests** in `theme/mod.rs`:

```rust
#[test]
fn text_roles_meet_aa_on_all_surfaces() {
    use palette::*;
    let texts = [TEXT_STRONG, TEXT_EMPHASIS, TEXT_BODY, TEXT_MUTED, TEXT_SUBTLE,
                 HA_400, SII_300, SII_400, GREEN_300, ACCENT_400];
    for bg in [SURFACE_900, SURFACE_800, SURFACE_700] {
        for t in texts {
            assert!(contrast_ratio(t, bg) >= 4.5, "{t:?} on {bg:?}: {}", contrast_ratio(t, bg));
        }
    }
}

#[test]
fn ha_500_fails_aa_as_text() {
    // Why HA_400 exists: ha-500 is ~3.7:1 on astro-800.
    assert!(contrast_ratio(palette::HA_500, palette::SURFACE_800) < 4.5);
}

#[test]
fn contrast_known_values() {
    assert!((contrast_ratio(Color32::WHITE, Color32::BLACK) - 21.0).abs() < 0.01);
    assert!((contrast_ratio(Color32::BLACK, Color32::BLACK) - 1.0).abs() < 1e-6);
}

#[test]
fn legible_text_keeps_hue_and_reaches_aa() {
    let t = legible_text(palette::HA_500, palette::SURFACE_700);
    assert!(contrast_ratio(t, palette::SURFACE_700) >= 4.5);
    assert!(t.r() > t.g() && t.r() > t.b(), "still red: {t:?}");
    assert_eq!(legible_text(palette::TEXT_STRONG, palette::SURFACE_700), palette::TEXT_STRONG);
}

#[test]
fn severities_map_to_tones() {
    assert_eq!(severity_color(Severity::Problem), palette::HA_400);
    assert_eq!(severity_color(Severity::Note), palette::SII_400);
    assert_eq!(severity_color(Severity::Passed), palette::GREEN_300);
}
```

  Also extend `palette_matches_brand` with the new constants and their hex values.

- [ ] **Step 2: Run them and confirm they fail.** Run `cargo test -p mega-ui theme`. Expected: compile errors for `contrast_ratio`, `legible_text`, `TEXT_EMPHASIS` and the rest.

- [ ] **Step 3: Implement the constants, `Tone`, `contrast_ratio`, `legible_text` and the new `severity_color` mapping.**

- [ ] **Step 4: Sweep Hα text.** In `crates/mmm-app/src` and `crates/mega-ui/src`, every `palette::HA_500` used as a text colour becomes `palette::HA_400`. That covers `RichText::color`, `.text_color`, painter text and label colours. Fills, strokes and bars stay `HA_500`. Find the candidates with `rg -n "HA_500" crates/mmm-app/src crates/mega-ui/src`.

- [ ] **Step 5: Run the tests and confirm they pass.** Run `cargo test --workspace`. Expected: PASS, with existing tests unchanged.

- [ ] **Step 6: Commit.** `git commit -am "feat(mega-ui): AA colour tokens, tones, SII gold, Hα text colour"`

---

### Task 2: Fonts and icons

**Files:**
- Create:
  - `crates/mega-ui/assets/fonts/`:
    - `Inter-Regular.ttf` and `Inter-SemiBold.ttf`, from `extras/ttf/` in `https://github.com/rsms/inter/releases/download/v4.1/Inter-4.1.zip`;
    - `LICENSE-Inter.txt`, from `LICENSE.txt` in the same zip;
    - `JetBrainsMono-Regular.ttf`, from `fonts/ttf/` in `https://github.com/JetBrains/JetBrainsMono/releases/download/v2.304/JetBrainsMono-2.304.zip`;
    - `LICENSE-JetBrainsMono.txt`, from `OFL.txt` in that zip;
    - `lucide.ttf`, from `package/font/` in `https://registry.npmjs.org/lucide-static/-/lucide-static-1.60.0.tgz`;
    - `LICENSE-Lucide.txt`, from `package/LICENSE` in that tarball.
  - `crates/mega-ui/src/theme/fonts.rs`, `crates/mega-ui/src/icons.rs`.
- Modify: `crates/mega-ui/src/lib.rs` (add `pub mod icons;`), `crates/mega-ui/src/theme/mod.rs` (add `Tone::icon`), `NOTICE`.

**Interfaces:**
- Produces:
  - Font data names as constants: `fonts::INTER_REGULAR = "Inter-Regular"`, `INTER_SEMIBOLD = "Inter-SemiBold"`, `JETBRAINS_MONO = "JetBrainsMono-Regular"`, `LUCIDE = "lucide"`.
  - `pub fn semibold() -> FontFamily` returns `FontFamily::Name("semibold".into())`.
  - `pub fn icon_family() -> FontFamily` returns `FontFamily::Name("icons".into())`, which contains Lucide only.
  - `pub fn definitions() -> FontDefinitions`. Start from `FontDefinitions::default()` and keep all its fonts.
    - Proportional is `[Inter-Regular, lucide, …the defaults]`.
    - Monospace is `[JetBrainsMono-Regular, lucide, …the defaults]`.
    - `semibold` is `[Inter-SemiBold, lucide, …the Proportional defaults]`.
    - `icons` is `[lucide]`.
  - `mega_ui::icons`: `pub const NAME: char` for each glyph below. The codepoints come from lucide-static 1.60.0 `font/codepoints.json`; copy them from that file, not from memory. Values as checked:
    - PLUS `\u{e13d}`, PIN `\u{e259}`, PIN_OFF `\u{e2b6}`
    - CIRCLE_X `\u{e084}`, TRIANGLE_ALERT `\u{e193}`, CHECK `\u{e06c}`, INFO `\u{e0f9}`
    - CHEVRON_RIGHT `\u{e06f}`, CHEVRON_DOWN `\u{e06d}`, ARROW_LEFT `\u{e048}`
    - SEARCH `\u{e151}`, FOLDER_OPEN `\u{e247}`, FOLDER_PLUS `\u{e0d9}`
    - PLAY `\u{e13c}`, PAUSE `\u{e12e}`, SKIP_FORWARD `\u{e160}`
    - X `\u{e1b2}`, PENCIL `\u{e1f9}`, COPY `\u{e09e}`, EXTERNAL_LINK `\u{e0b9}`
    - ZOOM_IN `\u{e1b6}`, ZOOM_OUT `\u{e1b7}`, HOUSE `\u{e0f5}`, MAXIMIZE_2 `\u{e113}`

    Also `pub const ALL: &[(&str, char)]`, listing every constant with its Lucide name (used by the test and the gallery).
  - `Tone::icon(self) -> Option<char>`: Success→CHECK, Info→INFO, Warning→TRIANGLE_ALERT, Problem→CIRCLE_X, Neutral→None.

- [ ] **Step 1: Write the failing tests** in `fonts.rs`:

```rust
#[test]
fn families_put_our_fonts_first_and_keep_fallbacks() {
    let d = definitions();
    let p = &d.families[&FontFamily::Proportional];
    assert_eq!(&p[..2], &[INTER_REGULAR.to_owned(), LUCIDE.to_owned()]);
    assert!(p.iter().any(|f| f == "NotoEmoji-Regular"), "emoji fallback kept: {p:?}");
    assert_eq!(d.families[&FontFamily::Monospace][0], JETBRAINS_MONO);
    assert_eq!(d.families[&semibold()][0], INTER_SEMIBOLD);
    assert_eq!(d.families[&icon_family()], vec![LUCIDE.to_owned()]);
}

#[test]
fn every_icon_is_in_the_lucide_font() {
    let ctx = egui::Context::default();
    ctx.set_fonts(definitions());
    let _ = ctx.run_ui(Default::default(), |_| {}); // fonts load on the next frame
    ctx.fonts_mut(|f| {
        for (name, c) in crate::icons::ALL {
            assert!(f.has_glyph(&FontId::new(13.0, icon_family()), *c), "{name} {c:?}");
        }
    });
}
```

  If `Context::run_ui` has a different name in 0.36, use `ctx.run` or `egui_kittest::Harness` instead.

- [ ] **Step 2: Run them and confirm they fail.** Run `cargo test -p mega-ui fonts`. Expected: compile errors.

- [ ] **Step 3: Add the files and implement `definitions()`.** Embed the fonts with `include_bytes!` and `FontData::from_static`. Add the icon constants, `ALL` and `Tone::icon`. Append a "Third-party fonts" section to `NOTICE` naming each font, its version, its licence (OFL-1.1, OFL-1.1, ISC) and its file under `crates/mega-ui/assets/fonts/`.

- [ ] **Step 4: Run the tests and confirm they pass.** Run `cargo test -p mega-ui fonts`. Expected: PASS.

- [ ] **Step 5: Commit.** `git add crates/mega-ui NOTICE && git commit -m "feat(mega-ui): Inter, JetBrains Mono and Lucide fonts; icon constants"`

---

### Task 3: Type scale, layout tokens, full style, `apply`

**Files:**
- Create: `crates/mega-ui/src/theme/tokens.rs`, `crates/mega-ui/src/theme/text.rs`, `crates/mega-ui/src/theme/style.rs`.
- Modify: `crates/mega-ui/src/theme/mod.rs`, so that `apply` uses `style()` and `fonts::definitions()`.

**Interfaces:**
- Produces:
  - `tokens`:
    - `pub mod space { XS=4.0, S=8.0, M=12.0, L=16.0, XL=24.0, XXL=32.0 }` (f32)
    - `pub mod radius { SMALL=4, CONTROL=6, CARD=8 }` (u8). Pills use half the height at paint time.
    - `CONTROL_HEIGHT: f32 = 30.0`, `CONTROL_HEIGHT_LARGE: f32 = 38.0`
    - `HOVER_FADE_S: f32 = 0.1`, `DISABLED_ALPHA: f32 = 0.4`
  - `text`:
    - `pub enum Type { Hero, Title, Heading, Body, Small, Overline, Numeric }`.
    - `pub fn text_style(self) -> TextStyle`. Hero, Title, Overline and Numeric are `TextStyle::Name("hero" | "title" | "overline" | "numeric")`. Heading, Body and Small are the built-in styles.
    - `pub fn text(t: Type, s: impl Into<String>) -> RichText` applies the style and its role colour: Hero, Title and Heading get `TEXT_STRONG`; Body gets `TEXT_BODY`; Small gets `TEXT_MUTED`; Numeric gets `TEXT_EMPHASIS`; Overline gets `TEXT_SUBTLE`, in upper case, with `extra_letter_spacing(0.12 * 10.5)`.
  - `style`: `pub fn style() -> egui::Style`, built on `Style::default()` with dark visuals:
    - `text_styles`:
      - Hero 32 and Title 20, in `semibold()`;
      - Heading 15, in `semibold()`;
      - Body 13, Small 12 and Button 13, Proportional;
      - Overline 10.5, in `semibold()`;
      - Numeric and Monospace 12, Monospace.
    - `spacing`:
      - item_spacing (8, 8) and button_padding (12, 6);
      - interact_size (40, `CONTROL_HEIGHT`);
      - window_margin 16, menu_margin 8, indent 16.
    - `animation_time`: `HOVER_FADE_S`.
    - `visuals`: the current `apply` settings, plus:
      - widget `corner_radius` `radius::CONTROL`; `window_corner_radius` `radius::CARD`; `menu_corner_radius` `radius::CONTROL`;
      - `text_edit_bg_color` `Some(SURFACE_900)`;
      - `warn_fg_color` `SII_400`, `error_fg_color` `HA_400`;
      - `disabled_alpha` `DISABLED_ALPHA`;
      - noninteractive `bg_stroke` `BORDER`;
      - `selection.stroke` 2 px `ACCENT_500`, which egui uses for the focused text-edit outline (the spec's accent border with a 2 px focus ring). If 0.36 draws that outline from a different field, set that one instead.
  - `pub fn apply(ctx: &egui::Context)`: `ctx.set_fonts(fonts::definitions())`, `ctx.set_theme(egui::Theme::Dark)`, then `ctx.set_style_of(egui::Theme::Dark, style())`.

- [ ] **Step 1: Write the failing tests** in `style.rs`:

```rust
#[test]
fn type_scale_matches_spec() {
    let s = style();
    let size = |t: Type| s.text_styles[&t.text_style()].size;
    assert_eq!([size(Type::Hero), size(Type::Title), size(Type::Heading), size(Type::Body),
                size(Type::Small), size(Type::Overline), size(Type::Numeric)],
               [32.0, 20.0, 15.0, 13.0, 12.0, 10.5, 12.0]);
    assert_eq!(s.text_styles[&Type::Hero.text_style()].family, fonts::semibold());
    assert_eq!(s.text_styles[&Type::Numeric.text_style()].family, FontFamily::Monospace);
}

#[test]
fn density_and_radius_match_spec() {
    let s = style();
    assert_eq!(s.spacing.interact_size.y, 30.0);
    assert_eq!(s.visuals.widgets.inactive.corner_radius, CornerRadius::same(6));
    assert_eq!(s.visuals.window_corner_radius, CornerRadius::same(8));
    assert_eq!(s.animation_time, 0.1);
}

#[test]
fn apply_forces_dark_even_when_the_os_is_light() {
    let ctx = egui::Context::default();
    ctx.options_mut(|o| o.fallback_theme = egui::Theme::Light);
    apply(&ctx);
    assert_eq!(ctx.theme(), egui::Theme::Dark);
    assert_eq!(ctx.style().visuals.panel_fill, palette::SURFACE_800);
}

#[test]
fn overline_is_tracked_caps() {
    let job = text(Type::Overline, "Recent sessions");
    assert_eq!(job.text(), "RECENT SESSIONS");
}
```

  To also cover an OS theme change while running, feed `RawInput.system_theme = Some(Theme::Light)` through one frame and re-assert `ctx.theme() == Theme::Dark`.

- [ ] **Step 2: Run them and confirm they fail.** Run `cargo test -p mega-ui style`. Expected: compile errors.

- [ ] **Step 3: Implement `tokens`, `text`, `style()` and the new `apply`.**

- [ ] **Step 4: Run the whole workspace.** Run `cargo test --workspace`. Expected: PASS. The `mmm-app` kittest tests find widgets by label, so the font change mustn't break them. If one breaks on size alone, fix the harness size, not the theme.

- [ ] **Step 5: Smoke test.** Run `cargo run -p mmm-app --example make_selftest_session -- <dir>`, then `cargo run -p mmm-app -- --selftest <dir>`. Expected: it exits cleanly.

- [ ] **Step 6: Commit.** `git commit -am "feat(mega-ui): type scale, layout tokens, full egui style, forced dark theme"`

---

### Task 4: Buttons

**Files:**
- Create: `crates/mega-ui/src/widgets/mod.rs`, `crates/mega-ui/src/widgets/button.rs`, `crates/mega-ui/src/widgets/button_tests.rs` (a `#[path]` child module, following the repo's pattern).
- Modify: `crates/mega-ui/src/lib.rs` (`pub mod widgets;`).

**Interfaces:**
- Consumes: `palette`, `tokens`, `icons`.
- Produces:
  - `pub enum ButtonKind { Primary, Secondary, Quiet, Danger, Ghost }`.
  - `pub struct MegaButton` with:
    - `pub fn new(kind: ButtonKind, text: impl Into<String>) -> Self`;
    - `pub fn large(self) -> Self`, which is 38 px high with 14 pt semibold text;
    - `pub fn icon(self, glyph: char) -> Self`, which puts the glyph before the text;
    - `impl egui::Widget`.
  - `pub fn icon_button(glyph: char, tooltip: &str) -> MegaButton`: a Secondary button, square, `CONTROL_HEIGHT` on each side, with `tooltip` on hover and as its accessible label.
- Approach: inside `ui.scope`, override `visuals.widgets.{inactive, hovered, active}` (`bg_fill`, `weak_bg_fill`, `bg_stroke`, `fg_stroke`) for the kind. Then add an `egui::Button` whose minimum height is the control height and whose corner radius is `radius::CONTROL`. egui's interaction, focus and accessibility come for free. Per-kind colours:
  - Primary: fill `ACCENT_500`, `ACCENT_400` on hover, `ACCENT_600` when pressed; text `ON_ACCENT`.
  - Secondary: `SURFACE_700` with a `BORDER` border; on hover, an `ACCENT_500` border and `TEXT_STRONG` text.
  - Quiet: no fill and no border, `TEXT_MUTED` text; on hover, `SURFACE_700` fill and `TEXT_STRONG` text.
  - Danger: no fill; border `HA_500` at alpha 120, text `HA_400`; on hover, `Tone::Problem.tint()` fill.
  - Ghost: white at alpha 31 with a white border at alpha 41, `TEXT_STRONG` text; white at alpha 51 on hover.
- The accessible label is the text without the glyph. Override it with `Response::widget_info` after adding the button.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn button_click_reports_clicked()
// Harness::new_ui_state with a MegaButton::new(Primary, "New mosaic"): click get_by_label("New mosaic") → state flag set.

#[test] fn icon_does_not_change_the_accessible_label()
// MegaButton::new(Primary, "New mosaic").icon(icons::PLUS): get_by_label("New mosaic") finds it.

#[test] fn icon_button_is_square_and_labelled_by_tooltip()
// icon_button(icons::PIN, "Pin session"): node labelled "Pin session"; its rect is 30×30 (±0.5).

#[test] fn large_button_is_38_high()
// MegaButton::new(Primary, "Resume session").large(): response.rect.height() == 38.0 (±0.5).

#[test] fn disabled_button_does_not_click()
// ui.add_enabled(false, MegaButton::new(Secondary, "Merge")): click → flag stays false.
```

- [ ] **Step 2: Run them and confirm they fail.** Run `cargo test -p mega-ui button`.
- [ ] **Step 3: Implement `MegaButton` and `icon_button`.**
- [ ] **Step 4: Run them and confirm they pass.** Run `cargo test -p mega-ui button`. Expected: PASS.
- [ ] **Step 5: Commit.** `git add crates/mega-ui && git commit -m "feat(mega-ui): button kinds and icon buttons"`

---

### Task 5: Display widgets: chip, status, overline, property grid

**Files:**
- Create: `crates/mega-ui/src/widgets/chip.rs`, `crates/mega-ui/src/widgets/status_line.rs` (the status *bar* stays in `status.rs`), `crates/mega-ui/src/widgets/property.rs`, `crates/mega-ui/src/widgets/display_tests.rs`.
- Modify: `crates/mega-ui/src/widgets/mod.rs`.

**Interfaces:**
- Consumes: `Tone`, `legible_text`, `text::{Type, text}`, `tokens`.
- Produces:
  - `pub const CHIP_MAX_WIDTH: f32 = 160.0;`
  - `pub fn chip(ui: &mut Ui, label: &str, tint: Option<Color32>) -> Response`: a pill 22 px high, with horizontal padding 9, Small text, fill `SURFACE_700`.
    - Neutral: a `BORDER` border and `TEXT_BODY` text.
    - Tinted: a border of `tint` at alpha 110, and text `legible_text(tint, SURFACE_700)`.
    - Text wider than the maximum is truncated with "…". The full label is then on hover, and the accessible label is always the full text.
  - `pub fn status(ui: &mut Ui, tone: Tone, label: &str) -> Response`: a 7 px dot in `tone.color()`, then Small text in the same colour.
  - `pub fn overline(ui: &mut Ui, label: &str) -> Response`: a label showing `text(Type::Overline, label)`.
  - `pub fn property_grid<R>(ui: &mut Ui, id_salt: impl std::hash::Hash, add_rows: impl FnOnce(&mut Ui) -> R) -> R`: an `egui::Grid` with spacing (16, 7).
  - Rows, each of which ends the grid row:
    - `pub fn property_row(ui: &mut Ui, label: &str, value: &str)` shows the label in `TEXT_MUTED` and the value in `TEXT_EMPHASIS`;
    - `pub fn property_row_numeric(ui: &mut Ui, label: &str, value: &str)` shows the value as `Type::Numeric`;
    - `pub fn property_row_ui(ui: &mut Ui, label: &str, add_value: impl FnOnce(&mut Ui))`.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn chip_shows_its_label()           // chip(ui, "18 panels", None) → get_by_label("18 panels")
#[test] fn long_chip_truncates_but_keeps_full_label()
// chip(ui, "Ha_3nm_2026-10-12_flat-corrected_master_light", Some(palette::HA_500)):
// response.rect.width() <= CHIP_MAX_WIDTH + 0.5, and get_by_label(<full text>) still finds it.
#[test] fn status_shows_label()             // status(ui, Tone::Warning, "Re-merge needed") → label present
#[test] fn overline_is_upper_case()         // overline(ui, "Notes") → get_by_label("NOTES")
#[test] fn property_rows_render_label_and_value()
// property_grid with property_row("Canvas", "24 310 × 15 870 px") → both labels present
```

- [ ] **Step 2: Run them and confirm they fail.** Run `cargo test -p mega-ui display`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run them and confirm they pass.**
- [ ] **Step 5: Commit.** `git commit -am "feat(mega-ui): chips, status lines, overlines, property grid"`

---

### Task 6: Input widgets: segmented control and notes field

**Files:**
- Create: `crates/mega-ui/src/widgets/segmented.rs`, `crates/mega-ui/src/widgets/notes.rs`, `crates/mega-ui/src/widgets/input_tests.rs`.
- Modify: `crates/mega-ui/src/widgets/mod.rs`.

**Interfaces:**
- Produces:
  - `pub fn segmented<T: PartialEq + Clone>(ui: &mut Ui, value: &mut T, options: &[(T, &str)]) -> Response`:
    - one joined control, `CONTROL_HEIGHT - 2` high, with a `BORDER` outline and `radius::CONTROL`;
    - the selected segment has a `BORDER` fill and `TEXT_STRONG` text, the others `TEXT_MUTED`;
    - each segment is exposed as a selectable item with its label;
    - the returned response is the union, with `mark_changed()` when the value changes.
  - `pub fn notes_field(ui: &mut Ui, text: &mut String, hint: &str, enabled: bool) -> Response`:
    - multi-line `TextEdit`, the full available width, 4 rows, with `hint` as the hint text;
    - read-only (not interactive) when `!enabled`;
    - saving is the caller's job, on `response.changed()`.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn segmented_click_changes_value()
// options [(0,"Composite"),(1,"Hα"),(2,"OIII")], value 0; click "OIII" → value == 2 and response.changed().
#[test] fn notes_field_edits_text()
// focus the field (click it), harness.type_text(" more") → state string ends with " more".
#[test] fn disabled_notes_field_is_read_only()
// enabled=false; click + type → string unchanged.
```

- [ ] **Step 2: Run them and confirm they fail.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run them and confirm they pass.**
- [ ] **Step 5: Commit.** `git commit -am "feat(mega-ui): segmented control and notes field"`

---

### Task 7: Restyle the check list

**Files:**
- Modify: `crates/mega-ui/src/checks.rs`.

**Interfaces:**
- Consumes: `Tone`, `icons`, `text`.
- Produces: the same public API: `CheckItem`, `CheckAction`, `checks_list`.
- New look, per spec §4.7:
  - Each problem or note row: the `Tone::from(severity).icon()` glyph in `tone.color()`, the title in `TEXT_EMPHASIS`, and the detail as `Type::Small` in `TEXT_MUTED`.
  - The row has a `tone.tint()` fill and a 2 px `tone.accent()` bar on its left edge. Only the right-hand corners are rounded, at `radius::CONTROL`.
  - Accepted problems use a `TEXT_SUBTLE` bar and no tint.
  - **Accept** becomes `MegaButton::new(Quiet, "Accept")`.
  - The passed summary row shows a green CHECK glyph, the text "N checks passed" in `TEXT_BODY`, and a chevron. It stays collapsed by default and expands to show each passed check.
  - The glyphs are painted as separate shapes, so the accessible labels stay exactly as today.

- [ ] **Step 1: Write the failing test** (the existing tests stay as they are):

```rust
#[test]
fn severity_rows_carry_their_icon()
// Render one Problem and one Note; assert the painted shapes contain text runs with
// icons::CIRCLE_X and icons::TRIANGLE_ALERT (inspect harness.output().shapes for TextShape galleys
// containing the glyph), and get_by_label("Broken") still resolves.
```

- [ ] **Step 2: Run it and confirm it fails.** Run `cargo test -p mega-ui checks`.
- [ ] **Step 3: Restyle.**
- [ ] **Step 4: Run the tests and confirm they pass, including `checks_list_accept_emits_action` and `passed_checks_collapse` unchanged.** Run `cargo test --workspace`.
- [ ] **Step 5: Commit.** `git commit -am "feat(mega-ui): check list in the new style"`

---

### Task 8: Progress and the job panel

**Files:**
- Create: `crates/mega-ui/src/widgets/progress.rs`.
- Modify: `crates/mega-ui/src/jobs.rs`, `crates/mega-ui/src/widgets/mod.rs`.

**Interfaces:**
- Produces:
  - `pub fn progress_bar(ui: &mut Ui, fraction: Option<f32>) -> Response`: the full available width, 2 px high, a `SURFACE_700` track and an `ACCENT_500` fill.
    - `None` means indeterminate: a segment a quarter of the width sweeps across, and the widget requests a repaint while visible.
    - Exposed to accessibility as a progress indicator, with its value when known.
  - `pub fn progress(ui: &mut Ui, label: &str, fraction: Option<f32>, readout: &str) -> Response`: one row with `label` on the left in `TEXT_EMPHASIS` and `readout` on the right as `Type::Numeric` in `TEXT_MUTED`, with the bar below.
  - `job_panel` keeps its signature and `JobView`. Each job is drawn with `progress(…)`. The readout is "N%" when the total is known and the stage otherwise. Cancel becomes a Quiet `MegaButton`.

- [ ] **Step 1: Write the failing test** (`job_panel_cancel_returns_action` stays):

```rust
#[test] fn progress_shows_label_and_readout()
// progress(ui, "Merging OIII", Some(0.62), "62% · 0:41 left") → both labels present.
#[test] fn job_panel_readout_is_percentage()
// JobView{done:3,total:10,..} → get_by_label("30%").
```

- [ ] **Step 2: Run them and confirm they fail.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run them and confirm they pass.** Run `cargo test --workspace`.
- [ ] **Step 5: Commit.** `git commit -am "feat(mega-ui): thin progress bar; job panel uses it"`

---

### Task 9: Toasts

**Files:**
- Create: `crates/mega-ui/src/widgets/toast.rs`, `crates/mega-ui/src/widgets/toast_tests.rs`.
- Modify: `crates/mega-ui/src/widgets/mod.rs`.

**Interfaces:**
- Produces:
  - `pub struct Toast { pub tone: Tone, pub title: String, pub detail: Option<String>, pub action: Option<String> }`.
  - `pub type ToastId = u64;`
  - `pub enum ToastEvent { Action(ToastId), Dismissed(ToastId) }`.
  - `pub const TOAST_LIFETIME_S: f64 = 6.0; pub const MAX_TOASTS: usize = 3;`
  - `#[derive(Default)] pub struct Toasts` with:
    - `pub fn push(&mut self, toast: Toast, now: f64) -> ToastId`;
    - `pub fn show(&mut self, ctx: &egui::Context, now: f64) -> Vec<ToastEvent>`.
- Behaviour:
  - Toasts stack in an `egui::Area` anchored at the bottom right, 16 px from the edges, newest at the bottom.
  - Each is 340 px wide, with a `SURFACE_700` fill, a `BORDER` border, `radius::CARD` and a soft shadow. It shows the tone icon, the title in `TEXT_EMPHASIS`, the detail as `Type::Small`, and an optional Quiet action button in `ACCENT_400` text.
  - At most `MAX_TOASTS` are visible. Pushing more dismisses the oldest, with a `Dismissed` event.
  - A toast expires `TOAST_LIFETIME_S` after it was pushed. The clock pauses while it's hovered.
  - Clicking the action emits `Action(id)` and dismisses the toast.
  - `show` requests a repaint for the next expiry. Toasts are never modal.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn toast_shows_and_expires()
// push at now=0 → visible "Export finished"; show at now=6.1 → gone, events contain Dismissed(id).
#[test] fn action_emits_event_and_dismisses()
// push with action "Show"; click "Show" → [Action(id)], toast gone next frame.
#[test] fn burst_keeps_three()
// push 10 at now=0 → exactly 3 titles visible (the last three pushed); 7 Dismissed events.
```

- [ ] **Step 2: Run them and confirm they fail.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run them and confirm they pass.**
- [ ] **Step 5: Commit.** `git commit -am "feat(mega-ui): non-modal toasts"`

---

### Task 10: Move the group palette into `mega-ui`

**Files:**
- Create: `crates/mega-ui/src/groups.rs`.
- Modify: `crates/mega-ui/src/lib.rs`, `crates/mmm-app/src/panels_tab.rs` (lines 62–106: remove `SII_COLOR`, `GROUP_PALETTE`, `PHYSICAL_SLOTS` and `physical_color`, and import them from `mega_ui::groups`), `crates/mmm-app/src/panels_tab_tests.rs` (imports), and any other user (`rg -n "GROUP_PALETTE|SII_COLOR|PHYSICAL_SLOTS" crates`).

**Interfaces:**
- Produces: `mega_ui::groups::{SII_COLOR, GROUP_PALETTE, PHYSICAL_SLOTS}`, with the same values and order as today, and `pub fn physical_color(name: &str) -> Option<usize>`, which was private. `assign_group_colors`, `load_group_colors` and `save_group_colors` stay in `panels_tab.rs`, because they use `UNGROUPED` and the session's `ui` JSON.

- [ ] **Step 1: Write the failing test** in `groups.rs`:

```rust
#[test]
fn palette_order_is_frozen() {
    // Saved sessions store indices into this array (`ui.group_colors`); never reorder.
    use crate::theme::palette::*;
    assert_eq!(GROUP_PALETTE[..6], [HA_500, GREEN_400, ACCENT_400, SII_COLOR, ACCENT_600, TEXT_STRONG]);
    assert_eq!(SII_COLOR, Color32::from_rgb(0xe0, 0x70, 0x3a));
    assert_eq!(GROUP_PALETTE.len(), 11);
    assert_eq!(physical_color("SII"), Some(3));
    assert_eq!(physical_color("r"), Some(0));
    assert_eq!(physical_color("Foo"), None);
}
```

- [ ] **Step 2: Run it and confirm it fails.** Run `cargo test -p mega-ui groups`.
- [ ] **Step 3: Move the code.**
- [ ] **Step 4: Run the tests and confirm they pass.** Run `cargo test --workspace`. The `panels_tab_tests` group colour tests must pass unchanged apart from imports.
- [ ] **Step 5: Commit.** `git commit -am "refactor: group palette lives in mega-ui"`

---

### Task 11: Brand lockup

**Files:**
- Create:
  - `crates/mega-ui/assets/brand/`:
    - `logo-white.png` and `wordmark-white.png`, `git mv`'d from `crates/mmm-app/assets/`;
    - `name-mega-merge-mosaic.png`, generated;
    - `make-name.sh`, which documents how the name image is generated.
  - `crates/mega-ui/src/brand.rs`.
- Modify:
  - `crates/mega-ui/Cargo.toml` (add `png.workspace = true`) and `crates/mega-ui/src/lib.rs`;
  - `crates/mmm-app/src/start.rs`: `header()` uses the lockup; delete `Brand`, `LOGO_PNG` and `WORDMARK_PNG` there;
  - `crates/mmm-app/src/main.rs:116`: the window icon uses `mega_ui::brand::LOGO_PNG`.

**Interfaces:**
- Produces:
  - `pub const LOGO_PNG: &[u8]`, `WORDMARK_PNG` and `MEGA_MERGE_MOSAIC_PNG`.
  - `pub struct BrandImages { … }` with `pub fn load(ctx: &egui::Context, app_name_png: &[u8], app_name: &str) -> Self`. It decodes with the `png` crate into RGBA and keeps `app_name` as the name image's accessible label. Textures use `TextureOptions { mipmap_mode: Some(TextureFilter::Linear), ..TextureOptions::LINEAR }`.
  - `pub enum LockupSize { Compact, Large }`.
  - `pub fn lockup(ui: &mut Ui, images: &BrandImages, size: LockupSize) -> Response`:
    - the logo; to its right, the name image, and under it a row with "BY" in `Type::Overline` and the wordmark;
    - Compact has a 34 px logo and a 12 px name image; Large has a 72 px logo and a 22 px name image;
    - the accessible labels are "Astrometrical logo", the `app_name` given to `load`, and "Astrometrical".
- The name image is made by `make-name.sh ANGORA_OTF` with exactly: `magick -background none -fill white -font "$1" -pointsize 96 -kerning 31 label:"MEGA MERGE MOSAIC" -trim +repage PNG32:name-mega-merge-mosaic.png`. That gives white RGBA about 1986×70; kerning 31 at 96 pt is the +0.32em tracking. The font file is not committed (spec §4.3).

- [ ] **Step 1: Write the failing tests** in `brand.rs`:

```rust
#[test] fn embedded_images_decode()
// BrandImages::load(&ctx, MEGA_MERGE_MOSAIC_PNG, "Mega Merge Mosaic") on a default Context: no panic;
// the three textures are non-zero size and the name texture is wider than tall by > 20×.
#[test] fn lockup_has_accessible_labels()
// Harness: lockup(ui, &images, Compact) → labels "Astrometrical logo", "Mega Merge Mosaic", "Astrometrical".
```

- [ ] **Step 2: Run them and confirm they fail.**
- [ ] **Step 3: Generate the name image, move the assets and implement `brand`.** Wire up `start.rs` and `main.rs`.
- [ ] **Step 4: Run the tests and confirm they pass.** Run `cargo test --workspace`, including the `mmm-app` start screen tests.
- [ ] **Step 5: Commit.** `git add -A crates && git commit -m "feat(mega-ui): brand lockup with pre-rendered Angora app name"`

---

### Task 12: Gallery, snapshots and docs

**Files:**
- Create: `crates/mega-ui/src/gallery.rs`, `crates/mega-ui/examples/gallery.rs`, `crates/mega-ui/tests/gallery.rs` (semantic, always runs), `crates/mega-ui/tests/snapshots.rs` (image snapshots, `#[ignore]`d), `crates/mega-ui/tests/snapshots/` (committed reference PNGs).
- Modify:
  - `crates/mega-ui/Cargo.toml`: dev-deps `eframe.workspace = true`, and `egui_kittest = { workspace = true, features = ["snapshot", "wgpu"] }`;
  - `crates/mega-ui/src/lib.rs`;
  - `CLAUDE.md`: Commands gets the gallery and snapshot commands; Conventions gets "UI code takes colours, sizes, radii and durations from `mega_ui::theme` tokens";
  - `docs/superpowers/specs/2026-11-02-standalone-app-design.md`: a one-line "Superseded by `2026-11-07-ui-standard-and-start-screen-design.md`" under the §4 and §5.1 headings;
  - `docs/DESIGN.md`: in its app or UI section, a short pointer to the standard and the gallery.

**Interfaces:**
- Produces:
  - `#[derive(Default)] pub struct GalleryState` (segmented value, notes text, toasts, lazily loaded `BrandImages`).
  - `pub fn show(ui: &mut Ui, state: &mut GalleryState)` renders these sections, each headed with `overline`:
    - Type scale;
    - Text roles;
    - Surfaces;
    - Semantic colours;
    - Group palette;
    - Buttons: every kind, large, icon, disabled, and Ghost over a gradient;
    - Inputs: a text edit, the segmented control, the notes field;
    - Chips: neutral, palette-tinted, and one long chip;
    - Status: all tones;
    - Check list: the spec's example rows plus 41 passed checks;
    - Property grid;
    - Progress: determinate and indeterminate;
    - Toasts: a button that pushes one;
    - Icons: every `icons::ALL` entry with its name;
    - Brand lockup: both sizes.

    It's all inside a `ScrollArea`.
  - `examples/gallery.rs`: an eframe app that calls `theme::apply`, then `gallery::show` in a `CentralPanel`.

- [ ] **Step 1: Write the failing tests:**

```rust
// tests/gallery.rs
#[test] fn gallery_renders_every_section()
// Harness 1200×4000 with theme::apply; run; get_by_label for one marker per section, e.g.
// "TYPE SCALE", "BUTTONS", "41 checks passed", "Mega Merge Mosaic", "62% · 0:41 left".

// tests/snapshots.rs
#[test] #[ignore = "image snapshot: needs a GPU; run with --ignored"]
fn gallery_snapshot_1x()   // Harness::builder().wgpu().with_size((1200., 4000.)); snapshot("gallery_1x")
#[test] #[ignore = "image snapshot: needs a GPU; run with --ignored"]
fn gallery_snapshot_2x()   // same with .with_pixels_per_point(2.0); snapshot("gallery_2x")
```

- [ ] **Step 2: Run the semantic test and confirm it fails.** Run `cargo test -p mega-ui --test gallery`.
- [ ] **Step 3: Implement `gallery`, the example and the doc edits.**
- [ ] **Step 4: Run the semantic test and confirm it passes.** Run `cargo test --workspace`. Expected: PASS, with the snapshot tests reported as ignored.
- [ ] **Step 5: Create the reference snapshots** on a machine with a GPU. Run `UPDATE_SNAPSHOTS=1 cargo test -p mega-ui --test snapshots -- --ignored`, inspect `crates/mega-ui/tests/snapshots/gallery_1x.png` and `gallery_2x.png` by eye against `.superpowers/brainstorm/2566-1794055103/content/style-sheet.html`, then run the same command without `UPDATE_SNAPSHOTS`. Expected: PASS.
- [ ] **Step 6: Check the gallery by hand.** Run `cargo run -p mega-ui --example gallery`. Check the hover states and the keyboard focus ring (Tab through the buttons), which still images can't show.
- [ ] **Step 7: Run the full check.** Run `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo doc --no-deps -p mega-ui && cargo test --workspace`. Expected: all clean.
- [ ] **Step 8: Commit.** `git add -A crates/mega-ui CLAUDE.md docs && git commit -m "feat(mega-ui): gallery, image snapshots, docs"`
