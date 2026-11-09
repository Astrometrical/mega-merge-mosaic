//! Checks list: problems and notes as tinted rows with a severity icon
//! and bar, passed checks collapsed into one expandable summary row. A
//! row can lead somewhere (it is then a button: [`CheckAction::Open`]); in
//! [`ListMode::All`] the list is grouped into Problems, Notes and Passed
//! and an accepted problem can be taken back ([`CheckAction::Unaccept`]).

use egui::{
    Align2, Color32, CornerRadius, Frame, Margin, Pos2, Rect, RichText, Sense, Stroke, StrokeKind,
    TextStyle, Ui, UiBuilder, Vec2, WidgetInfo, WidgetType, collapsing_header::CollapsingState,
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
    /// The row leads somewhere (the place the check is about): it is a
    /// button, activated by a click or Enter, and reported as
    /// [`CheckAction::Open`].
    pub goes_somewhere: bool,
}

/// A user action from [`checks_list`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckAction {
    /// Accept the problem at this index of the items given.
    Accept(usize),
    /// Take back the acceptance of the problem at this index.
    Unaccept(usize),
    /// Go to what the item at this index is about.
    Open(usize),
}

/// What [`checks_list`] lists and how.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ListMode {
    /// A flat list: problems, then notes, then the passed checks as one
    /// collapsed summary row. The caller gives it the items to show.
    #[default]
    Problems,
    /// Everything, in sections headed Problems, Notes and Passed (the
    /// passed checks stay collapsed under theirs). An accepted problem
    /// shows "Accepted" with an Un-accept button.
    All,
}

/// Show `items` under `heading`: problems, then notes, as tinted rows with
/// a severity icon and bar (an "Accept" button on acceptable, unaccepted
/// problems; rows that [go somewhere](CheckItem::goes_somewhere) are
/// buttons), then the passed checks as one collapsed summary row that
/// expands to list them. In [`ListMode::All`] each group has its own
/// heading and an accepted problem can be un-accepted. An empty `heading`
/// draws no heading row (the list sits under a heading of the caller's).
/// Returns the actions taken this frame.
pub fn checks_list(
    ui: &mut Ui,
    heading: &str,
    items: &[CheckItem],
    mode: ListMode,
) -> Vec<CheckAction> {
    let mut actions = Vec::new();
    if !heading.is_empty() {
        ui.label(RichText::new(heading).strong().color(palette::TEXT_STRONG));
    }
    if items.is_empty() {
        ui.label(RichText::new("No checks yet").color(palette::TEXT_MUTED));
        return actions;
    }
    let grouped = mode == ListMode::All;
    for (severity, name) in [(Severity::Problem, "Problems"), (Severity::Note, "Notes")] {
        if grouped && items.iter().any(|c| c.severity == severity) {
            section_heading(ui, name);
        }
        for (i, item) in items.iter().enumerate() {
            if item.severity == severity {
                row(ui, i, item, mode, &mut actions);
                ui.add_space(space::XS);
            }
        }
    }
    let passed: Vec<&CheckItem> = items
        .iter()
        .filter(|c| c.severity == Severity::Passed)
        .collect();
    if !passed.is_empty() {
        if grouped {
            section_heading(ui, "Passed");
        }
        passed_section(ui, heading, &passed);
    }
    actions
}

/// The heading of one group of an [`ListMode::All`] list.
fn section_heading(ui: &mut Ui, name: &str) {
    ui.add_space(space::XS);
    ui.label(
        RichText::new(name)
            .text_style(TextStyle::Small)
            .strong()
            .color(palette::TEXT_MUTED),
    );
}

/// Paint `glyph` with its top-left at `pos`.
fn paint_icon(ui: &Ui, pos: Pos2, glyph: char, color: Color32) {
    ui.painter()
        .text(pos, Align2::LEFT_TOP, glyph, icons::font(ICON_SIZE), color);
}

/// One problem or note row; `index` is the item's index in the caller's
/// slice.
fn row(
    ui: &mut Ui,
    index: usize,
    item: &CheckItem,
    mode: ListMode,
    actions: &mut Vec<CheckAction>,
) {
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
    // A chevron at the right says the row leads somewhere.
    let right = if item.goes_somewhere {
        space::S + ICON_SIZE + space::S
    } else {
        space::S
    };
    let mut builder = UiBuilder::new().id_salt(("mega-ui-check-row", index));
    if item.goes_somewhere {
        // Registered before the row's content, so a button inside it
        // (Accept) is on top of the row and takes its own clicks.
        builder = builder.sense(Sense::click());
    }
    let scope = ui.scope_builder(builder, |ui| {
        Frame::new()
            .fill(fill)
            .corner_radius(CornerRadius {
                ne: radius::CONTROL,
                se: radius::CONTROL,
                ..CornerRadius::ZERO
            })
            .inner_margin(Margin {
                left: left as i8,
                right: right as i8,
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
                    accept_controls(ui, index, item, mode, actions);
                }
            })
            .response
            .rect
    });
    let r = scope.inner;
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
    if item.goes_somewhere {
        let response = scope.response;
        ui.painter().text(
            Pos2::new(r.max.x - space::S, r.center().y),
            Align2::RIGHT_CENTER,
            icons::CHEVRON_RIGHT,
            icons::font(ICON_SIZE),
            palette::TEXT_MUTED,
        );
        if response.hovered() {
            ui.painter().rect_stroke(
                r,
                CornerRadius::same(radius::CONTROL),
                Stroke::new(1.0, palette::BORDER),
                StrokeKind::Inside,
            );
        }
        crate::widgets::button::focus_ring(ui, &response, radius::CONTROL);
        let title = item.title.clone();
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, title.clone()));
        if response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            actions.push(CheckAction::Open(index));
        }
    }
}

/// What a problem row offers about acceptance: Accept on an acceptable
/// one; "Accepted" on an accepted one, with Un-accept in
/// [`ListMode::All`].
fn accept_controls(
    ui: &mut Ui,
    index: usize,
    item: &CheckItem,
    mode: ListMode,
    actions: &mut Vec<CheckAction>,
) {
    if item.accepted {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Accepted")
                    .text_style(TextStyle::Small)
                    .color(palette::TEXT_SUBTLE),
            );
            if mode == ListMode::All
                && ui
                    .add(MegaButton::new(ButtonKind::Quiet, "Un-accept"))
                    .on_hover_text("Flag this problem again")
                    .clicked()
            {
                actions.push(CheckAction::Unaccept(index));
            }
        });
    } else if item.acceptable
        && ui
            .add(MegaButton::new(ButtonKind::Quiet, "Accept"))
            .on_hover_text("Accept this problem: it stays listed but is no longer flagged")
            .clicked()
    {
        actions.push(CheckAction::Accept(index));
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
            goes_somewhere: false,
        }
    }

    struct State {
        items: Vec<CheckItem>,
        mode: ListMode,
        actions: Vec<CheckAction>,
    }

    fn harness(items: Vec<CheckItem>) -> Harness<'static, State> {
        Harness::builder()
            .with_size(egui::Vec2::new(400.0, 600.0))
            .build_ui_state(
                |ui, s: &mut State| {
                    if crate::testutil::themed(ui) {
                        let a = checks_list(ui, "Checks", &s.items, s.mode);
                        s.actions.extend(a);
                    }
                },
                State {
                    items,
                    mode: ListMode::Problems,
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
                        let a = checks_list(ui, "", &s.items, s.mode);
                        s.actions.extend(a);
                    }
                },
                State {
                    items: vec![check(Severity::Problem, "Broken", true, false)],
                    mode: ListMode::Problems,
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

    fn goes(mut item: CheckItem) -> CheckItem {
        item.goes_somewhere = true;
        item
    }

    fn row_button<'a>(h: &'a Harness<'static, State>, title: &'a str) -> egui_kittest::Node<'a> {
        h.get_by_role_and_label(egui::accesskit::Role::Button, title)
    }

    #[test]
    fn rows_that_go_somewhere_open() {
        let mut h = harness(vec![
            goes(check(Severity::Problem, "Gap in coverage", true, false)),
            check(Severity::Note, "Stays put", false, false),
            goes(check(Severity::Note, "Noisy panel", false, false)),
        ]);
        h.run();
        // Only the rows that lead somewhere are buttons.
        assert!(
            h.query_by_role_and_label(egui::accesskit::Role::Button, "Stays put")
                .is_none()
        );
        row_button(&h, "Gap in coverage").click();
        h.run();
        assert_eq!(h.state().actions, vec![CheckAction::Open(0)]);

        // Reachable by keyboard: focus and Enter.
        h.state_mut().actions.clear();
        row_button(&h, "Noisy panel").focus();
        h.run();
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().actions, vec![CheckAction::Open(2)]);

        // The row's own Accept button is not a click on the row.
        h.state_mut().actions.clear();
        h.get_by_label("Accept").click();
        h.run();
        assert_eq!(h.state().actions, vec![CheckAction::Accept(0)]);
    }

    #[test]
    fn all_mode_groups_by_severity_with_unaccept() {
        let mut h = harness(vec![
            check(Severity::Passed, "Pass one", false, false),
            check(Severity::Problem, "Accepted gap", true, true),
            check(Severity::Note, "Noisy", false, false),
            check(Severity::Problem, "Open gap", true, false),
        ]);
        // Problems mode: no sections, nothing to un-accept.
        h.run();
        assert!(h.query_by_label("Notes").is_none());
        assert!(h.query_by_label("Un-accept").is_none());

        h.state_mut().mode = ListMode::All;
        h.run();
        let y = |h: &Harness<'static, State>, t: &str| h.get_by_label(t).rect().min.y;
        let (problems, notes, passed) = (y(&h, "Problems"), y(&h, "Notes"), y(&h, "Passed"));
        assert!(
            problems < notes && notes < passed,
            "{problems} {notes} {passed}"
        );
        assert!(y(&h, "Accepted gap") > problems && y(&h, "Accepted gap") < notes);
        assert!(y(&h, "Noisy") > notes && y(&h, "Noisy") < passed);
        h.get_by_label("Accepted");
        h.get_by_label("Un-accept").click();
        h.run();
        assert_eq!(h.state().actions, vec![CheckAction::Unaccept(1)]);
        h.state_mut().actions.clear();
        h.get_by_label("Accept").click();
        h.run();
        assert_eq!(h.state().actions, vec![CheckAction::Accept(3)]);
        // Passed stays collapsible.
        h.get_by_label("1 check passed").click();
        h.run();
        h.get_by_label("Pass one");
    }
}
