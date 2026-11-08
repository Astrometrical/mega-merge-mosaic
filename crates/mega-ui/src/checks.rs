//! Checks list: problems and notes as tinted rows with a severity icon
//! and bar, passed checks collapsed into one expandable summary row.

use egui::{
    Align, Align2, Color32, CornerRadius, Frame, Layout, Margin, Pos2, Rect, RichText, Sense,
    TextStyle, Ui, Vec2, collapsing_header::CollapsingState,
};

use crate::icons;
use crate::theme::palette;
use crate::theme::tokens::{CONTROL_HEIGHT, radius, space};
use crate::theme::{Severity, Tone};
use crate::widgets::{ButtonKind, MegaButton};

/// Width of a row's severity bar, in points.
const BAR_WIDTH: f32 = 2.0;
/// Size of a row's severity icon, in points.
const ICON_SIZE: f32 = 15.0;

/// One check result to show.
#[derive(Clone, Debug, PartialEq)]
pub struct CheckItem {
    /// Problem, note or passed.
    pub severity: Severity,
    /// One-line summary.
    pub title: String,
    /// Explanation and remedy.
    pub detail: String,
    /// The user has accepted this problem.
    pub accepted: bool,
    /// The user may accept this problem.
    pub acceptable: bool,
}

/// A user action from [`checks_list`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckAction {
    /// Accept the problem at this index of the items given.
    Accept(usize),
}

/// Show `items` under `heading`: problems, then notes, as tinted rows with
/// a severity icon and bar (an "Accept" button on acceptable, unaccepted
/// problems), then the passed checks as one collapsed summary row that
/// expands to list them. With `show_all`, a "Show all" checkbox sits beside
/// the heading; the caller does the filtering it stands for. An empty
/// `heading` without `show_all` draws no heading row (the list sits under a
/// heading of the caller's). Returns the actions taken this frame.
pub fn checks_list(
    ui: &mut Ui,
    heading: &str,
    items: &[CheckItem],
    show_all: Option<&mut bool>,
) -> Vec<CheckAction> {
    let mut actions = Vec::new();
    if !heading.is_empty() || show_all.is_some() {
        ui.horizontal(|ui| {
            ui.label(RichText::new(heading).strong().color(palette::TEXT_STRONG));
            if let Some(all) = show_all {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.checkbox(all, "Show all");
                });
            }
        });
    }
    if items.is_empty() {
        ui.label(RichText::new("No checks yet").color(palette::TEXT_MUTED));
        return actions;
    }
    for severity in [Severity::Problem, Severity::Note] {
        for (i, item) in items.iter().enumerate() {
            if item.severity == severity {
                row(ui, i, item, &mut actions);
                ui.add_space(space::XS);
            }
        }
    }
    let passed: Vec<&CheckItem> = items
        .iter()
        .filter(|c| c.severity == Severity::Passed)
        .collect();
    if !passed.is_empty() {
        passed_section(ui, heading, &passed);
    }
    actions
}

/// Paint `glyph` with its top-left at `pos`.
fn paint_icon(ui: &Ui, pos: Pos2, glyph: char, color: Color32) {
    ui.painter()
        .text(pos, Align2::LEFT_TOP, glyph, icons::font(ICON_SIZE), color);
}

/// One problem or note row; `index` is the item's index in the caller's
/// slice.
fn row(ui: &mut Ui, index: usize, item: &CheckItem, actions: &mut Vec<CheckAction>) {
    let tone = Tone::from(item.severity);
    let (bar, fill, icon_color) = if item.accepted {
        (
            palette::TEXT_SUBTLE,
            Color32::TRANSPARENT,
            palette::TEXT_SUBTLE,
        )
    } else {
        (tone.accent(), tone.tint(), tone.color())
    };
    let left = BAR_WIDTH + space::S + ICON_SIZE + space::S;
    let frame = Frame::new()
        .fill(fill)
        .corner_radius(CornerRadius {
            ne: radius::CONTROL,
            se: radius::CONTROL,
            ..CornerRadius::ZERO
        })
        .inner_margin(Margin {
            left: left as i8,
            right: space::S as i8,
            top: 6,
            bottom: 6,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(&item.title).color(palette::TEXT_EMPHASIS));
            if !item.detail.is_empty() {
                ui.label(
                    RichText::new(&item.detail)
                        .text_style(TextStyle::Small)
                        .color(palette::TEXT_MUTED),
                );
            }
            if item.severity == Severity::Problem {
                if item.accepted {
                    ui.label(
                        RichText::new("Accepted")
                            .text_style(TextStyle::Small)
                            .color(palette::TEXT_SUBTLE),
                    );
                } else if item.acceptable
                    && ui
                        .add(MegaButton::new(ButtonKind::Quiet, "Accept"))
                        .on_hover_text(
                            "Accept this problem: it stays listed but is no longer flagged",
                        )
                        .clicked()
                {
                    actions.push(CheckAction::Accept(index));
                }
            }
        });
    let r = frame.response.rect;
    ui.painter().rect_filled(
        Rect::from_min_max(r.min, Pos2::new(r.min.x + BAR_WIDTH, r.max.y)),
        CornerRadius::ZERO,
        bar,
    );
    if let Some(glyph) = tone.icon() {
        paint_icon(
            ui,
            Pos2::new(r.min.x + BAR_WIDTH + space::S, r.min.y + 7.0),
            glyph,
            icon_color,
        );
    }
}

/// The passed checks: a summary row (tick, "N checks passed", chevron)
/// that expands to list each one.
fn passed_section(ui: &mut Ui, heading: &str, passed: &[&CheckItem]) {
    let n = passed.len();
    let summary = format!("{n} check{} passed", if n == 1 { "" } else { "s" });
    let id = ui.make_persistent_id(("mega-ui-checks-passed", heading));
    let mut state = CollapsingState::load_with_default_open(ui.ctx(), id, false);
    let header = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::XS;
            // As tall as the button beside it, the tick centred in it.
            let (icon, _) =
                ui.allocate_exact_size(Vec2::new(ICON_SIZE, CONTROL_HEIGHT), Sense::hover());
            ui.painter().text(
                icon.left_center(),
                Align2::LEFT_CENTER,
                icons::CHECK,
                icons::font(ICON_SIZE),
                palette::GREEN_300,
            );
            let glyph = if state.is_open() {
                icons::CHEVRON_DOWN
            } else {
                icons::CHEVRON_RIGHT
            };
            ui.add(MegaButton::new(ButtonKind::Quiet, &summary).trailing_icon(glyph))
        })
        .inner;
    if header.clicked() {
        state.toggle(ui);
    }
    state.show_body_unindented(ui, |ui| {
        for c in passed {
            ui.label(RichText::new(&c.title).color(palette::TEXT_BODY));
            if !c.detail.is_empty() {
                ui.label(
                    RichText::new(&c.detail)
                        .text_style(TextStyle::Small)
                        .color(palette::TEXT_MUTED),
                );
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    fn check(severity: Severity, title: &str, acceptable: bool, accepted: bool) -> CheckItem {
        CheckItem {
            severity,
            title: title.to_owned(),
            detail: format!("{title} detail"),
            accepted,
            acceptable,
        }
    }

    struct State {
        items: Vec<CheckItem>,
        show_all: bool,
        actions: Vec<CheckAction>,
    }

    fn harness(items: Vec<CheckItem>) -> Harness<'static, State> {
        Harness::builder()
            .with_size(egui::Vec2::new(400.0, 600.0))
            .build_ui_state(
                |ui, s: &mut State| {
                    if crate::testutil::themed(ui) {
                        let a = checks_list(ui, "Checks", &s.items, Some(&mut s.show_all));
                        s.actions.extend(a);
                    }
                },
                State {
                    items,
                    show_all: false,
                    actions: Vec::new(),
                },
            )
    }

    #[test]
    fn checks_list_accept_emits_action() {
        let mut h = harness(vec![
            check(Severity::Note, "A note", false, false),
            check(Severity::Problem, "Not acceptable", false, false),
            check(Severity::Problem, "Acceptable", true, false),
            check(Severity::Problem, "Already accepted", true, true),
            check(Severity::Passed, "Fine", false, false),
        ]);
        h.run();
        h.get_by_label("Acceptable");
        h.get_by_label("Acceptable detail");
        h.get_by_label("A note");
        // Exactly one Accept button: the acceptable, unaccepted problem.
        h.get_by_label("Accept").click();
        h.run();
        assert_eq!(h.state().actions, vec![CheckAction::Accept(2)]);

        h.get_by_label("Show all").click();
        h.run();
        assert!(h.state().show_all);
    }

    /// Every text run painted in the last frame.
    fn painted_text(h: &Harness<'static, State>) -> String {
        fn walk(shape: &egui::Shape, out: &mut String) {
            match shape {
                egui::Shape::Text(t) => out.push_str(t.galley.text()),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = String::new();
        for c in &h.output().shapes {
            walk(&c.shape, &mut out);
        }
        out
    }

    /// Every painted Lucide glyph uses the icon family.
    fn icon_fonts_ok(h: &Harness<'static, State>) -> bool {
        let icon = crate::theme::fonts::icon_family();
        let mut ok = true;
        for c in &h.output().shapes {
            if let egui::Shape::Text(t) = &c.shape {
                for section in &t.galley.job.sections {
                    let r = &section.byte_range;
                    let s = &t.galley.job.text[r.start.0..r.end.0];
                    let has_icon = crate::icons::ALL.iter().any(|(_, g)| s.contains(*g));
                    ok &= !has_icon || section.format.font_id.family == icon;
                }
            }
        }
        ok
    }

    #[test]
    fn severity_rows_carry_their_icon() {
        let mut h = harness(vec![
            check(Severity::Problem, "Broken", true, false),
            check(Severity::Note, "Worth knowing", false, false),
            check(Severity::Passed, "Fine", false, false),
        ]);
        h.run();
        let text = painted_text(&h);
        assert!(icon_fonts_ok(&h), "icons are drawn in the icon family");
        assert!(text.contains(crate::icons::CIRCLE_X), "problem icon");
        assert!(text.contains(crate::icons::TRIANGLE_ALERT), "note icon");
        assert!(text.contains(crate::icons::CHECK), "passed icon");
        h.get_by_label("Broken");
        h.get_by_label("1 check passed");
    }

    #[test]
    fn passed_summary_is_a_button() {
        let mut h = harness(vec![
            check(Severity::Passed, "Pass one", false, false),
            check(Severity::Passed, "Pass two", false, false),
        ]);
        h.run();
        h.get_by_role_and_label(egui::accesskit::Role::Button, "2 checks passed")
            .click();
        h.run();
        h.get_by_label("Pass one");
    }

    #[test]
    fn passed_tick_lines_up_with_its_summary() {
        let mut h = harness(vec![check(Severity::Passed, "Pass one", false, false)]);
        h.run();
        let button = h.get_by_label("1 check passed").rect();
        let tick = h
            .output()
            .shapes
            .iter()
            .find_map(|c| match &c.shape {
                egui::Shape::Text(t) if t.galley.text().contains(crate::icons::CHECK) => {
                    Some(t.visual_bounding_rect())
                }
                _ => None,
            })
            .expect("the tick is painted");
        assert!(
            (tick.center().y - button.center().y).abs() < 3.0,
            "tick {tick:?} beside summary {button:?}"
        );
    }

    #[test]
    fn an_empty_heading_draws_no_heading_row() {
        let mut h = Harness::builder()
            .with_size(egui::Vec2::new(400.0, 300.0))
            .build_ui_state(
                |ui, s: &mut State| {
                    if crate::testutil::themed(ui) {
                        let a = checks_list(ui, "", &s.items, None);
                        s.actions.extend(a);
                    }
                },
                State {
                    items: vec![check(Severity::Problem, "Broken", true, false)],
                    show_all: false,
                    actions: Vec::new(),
                },
            );
        h.run();
        h.get_by_label("Broken");
        let labels: Vec<String> = h
            .query_all_by_role(egui::accesskit::Role::Label)
            .filter_map(|n| n.value())
            .collect();
        assert!(!labels.iter().any(String::is_empty), "{labels:?}");
    }

    #[test]
    fn passed_checks_collapse() {
        let mut h = harness(vec![
            check(Severity::Passed, "Pass one", false, false),
            check(Severity::Problem, "Broken", true, false),
            check(Severity::Passed, "Pass two", false, false),
            check(Severity::Passed, "Pass three", false, false),
        ]);
        h.run();
        h.get_by_label("Broken");
        assert!(h.query_by_label("Pass one").is_none());
        h.get_by_label("3 checks passed").click();
        h.run();
        for t in ["Pass one", "Pass two", "Pass three"] {
            h.get_by_label(t);
        }
    }
}
