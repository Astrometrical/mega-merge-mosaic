use egui::Vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use super::*;
use crate::icons;

#[derive(Default)]
struct State {
    clicked: bool,
    rect: Option<egui::Rect>,
}

fn harness(add: impl Fn(&mut egui::Ui) -> egui::Response + 'static) -> Harness<'static, State> {
    Harness::builder()
        .with_size(Vec2::new(400.0, 200.0))
        .build_ui_state(
            move |ui, s: &mut State| {
                if !crate::testutil::themed(ui) {
                    return;
                }
                let r = add(ui);
                s.clicked |= r.clicked();
                s.rect = Some(r.rect);
            },
            State::default(),
        )
}

#[test]
fn button_click_reports_clicked() {
    let mut h = harness(|ui| ui.add(MegaButton::new(ButtonKind::Primary, "New mosaic")));
    h.run();
    h.get_by_label("New mosaic").click();
    h.run();
    assert!(h.state().clicked);
}

#[test]
fn icon_does_not_change_the_accessible_label() {
    let mut h =
        harness(|ui| ui.add(MegaButton::new(ButtonKind::Primary, "New mosaic").icon(icons::PLUS)));
    h.run();
    h.get_by_label("New mosaic").click();
    h.run();
    assert!(h.state().clicked);
}

#[test]
fn icon_button_is_square_and_labelled_by_tooltip() {
    let mut h = harness(|ui| ui.add(icon_button(icons::PIN, "Pin session")));
    h.run();
    h.get_by_label("Pin session");
    let r = h.state().rect.unwrap();
    assert!((r.width() - 30.0).abs() < 0.5, "{r:?}");
    assert!((r.height() - 30.0).abs() < 0.5, "{r:?}");
}

#[test]
fn large_button_is_38_high() {
    let mut h =
        harness(|ui| ui.add(MegaButton::new(ButtonKind::Primary, "Resume session").large()));
    h.run();
    let r = h.state().rect.unwrap();
    assert!((r.height() - 38.0).abs() < 0.5, "{r:?}");
}

#[test]
fn normal_button_is_30_high() {
    let mut h = harness(|ui| ui.add(MegaButton::new(ButtonKind::Secondary, "Open…")));
    h.run();
    let r = h.state().rect.unwrap();
    assert!((r.height() - 30.0).abs() < 0.5, "{r:?}");
}

#[test]
fn disabled_button_does_not_click() {
    let mut h =
        harness(|ui| ui.add_enabled(false, MegaButton::new(ButtonKind::Secondary, "Merge")));
    h.run();
    h.get_by_label("Merge").click();
    h.run();
    assert!(!h.state().clicked);
}

#[test]
fn icon_renders_in_the_icon_font() {
    let mut h =
        harness(|ui| ui.add(MegaButton::new(ButtonKind::Primary, "New mosaic").icon(icons::PLUS)));
    h.run();
    let mut found = false;
    for c in &h.output().shapes {
        if let egui::Shape::Text(t) = &c.shape {
            for section in &t.galley.job.sections {
                let r = &section.byte_range;
                let s = &t.galley.job.text[r.start.0..r.end.0];
                if s.contains(icons::PLUS) {
                    found = true;
                    assert_eq!(
                        section.format.font_id.family,
                        crate::theme::fonts::icon_family()
                    );
                }
            }
        }
    }
    assert!(found, "the plus glyph was painted");
}

#[test]
fn primary_button_text_meets_aa_in_every_state() {
    for state in looks(ButtonKind::Primary) {
        let c = crate::theme::contrast_ratio(state.text, state.fill);
        assert!(c >= 4.5, "{:?} on {:?}: {c:.2}:1", state.text, state.fill);
    }
}

/// Whether the last frame painted a 2 pt accent ring around `rect`.
fn has_focus_ring(h: &Harness<'static, State>, rect: egui::Rect) -> bool {
    h.output().shapes.iter().any(|c| match &c.shape {
        egui::Shape::Rect(r) => {
            r.stroke.width >= 2.0
                && r.stroke.color == crate::theme::palette::ACCENT_400
                && r.rect.contains_rect(rect)
        }
        _ => false,
    })
}

#[test]
fn keyboard_focus_shows_an_accent_ring() {
    for kind in [ButtonKind::Primary, ButtonKind::Quiet, ButtonKind::Ghost] {
        let mut h = harness(move |ui| ui.add(MegaButton::new(kind, "Merge")));
        h.run();
        let rect = h.state().rect.unwrap();
        assert!(!has_focus_ring(&h, rect), "{kind:?}: no ring before focus");
        h.get_by_label("Merge").focus();
        h.run();
        assert!(has_focus_ring(&h, rect), "{kind:?}: ring when focused");
    }
}
