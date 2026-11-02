//! Checks list: problems and notes as cards with a severity bar, passed
//! checks collapsed into one green summary line.

use egui::{
    Align, CollapsingHeader, CornerRadius, Frame, Layout, Margin, Pos2, Rect, RichText, Stroke, Ui,
};

use crate::theme::{Severity, palette, severity_color};

/// Width of a card's severity bar, in points.
const BAR_WIDTH: f32 = 3.0;

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

/// Show `items` under `heading`: problems, then notes, as cards with a
/// severity bar (an "Accept" button on acceptable, unaccepted problems),
/// then the passed checks as one collapsed green summary line. With
/// `show_all`, a "Show all" checkbox sits beside the heading; the caller
/// does the filtering it stands for. Returns the actions taken this frame.
pub fn checks_list(
    ui: &mut Ui,
    heading: &str,
    items: &[CheckItem],
    show_all: Option<&mut bool>,
) -> Vec<CheckAction> {
    let mut actions = Vec::new();
    ui.horizontal(|ui| {
        ui.label(RichText::new(heading).strong().color(palette::TEXT_STRONG));
        if let Some(all) = show_all {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.checkbox(all, "Show all");
            });
        }
    });
    if items.is_empty() {
        ui.label(RichText::new("No checks yet").color(palette::TEXT_MUTED));
        return actions;
    }
    for severity in [Severity::Problem, Severity::Note] {
        for (i, item) in items.iter().enumerate() {
            if item.severity == severity {
                card(ui, i, item, &mut actions);
                ui.add_space(4.0);
            }
        }
    }
    let passed: Vec<&CheckItem> = items
        .iter()
        .filter(|c| c.severity == Severity::Passed)
        .collect();
    if !passed.is_empty() {
        let n = passed.len();
        let summary = format!("{n} check{} passed", if n == 1 { "" } else { "s" });
        let green = severity_color(Severity::Passed);
        CollapsingHeader::new(RichText::new(summary).color(green))
            .id_salt(("mega-ui-checks-passed", heading))
            .default_open(false)
            .show(ui, |ui| {
                for c in passed {
                    ui.label(RichText::new(&c.title).color(green));
                    if !c.detail.is_empty() {
                        ui.label(RichText::new(&c.detail).small().color(palette::TEXT_MUTED));
                    }
                }
            });
    }
    actions
}

/// One problem or note card; `index` is the item's index in the caller's
/// slice.
fn card(ui: &mut Ui, index: usize, item: &CheckItem, actions: &mut Vec<CheckAction>) {
    let bar = if item.accepted {
        palette::TEXT_SUBTLE
    } else {
        severity_color(item.severity)
    };
    let frame = Frame::new()
        .fill(palette::SURFACE_700)
        .stroke(Stroke::new(1.0, palette::BORDER))
        .corner_radius(4)
        .inner_margin(Margin {
            left: 12,
            right: 8,
            top: 6,
            bottom: 6,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new(&item.title)
                    .strong()
                    .color(palette::TEXT_STRONG),
            );
            if !item.detail.is_empty() {
                ui.label(RichText::new(&item.detail).color(palette::TEXT_BODY));
            }
            if item.severity == Severity::Problem {
                if item.accepted {
                    ui.label(
                        RichText::new("Accepted")
                            .small()
                            .color(palette::TEXT_SUBTLE),
                    );
                } else if item.acceptable
                    && ui
                        .button("Accept")
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
        CornerRadius {
            nw: 4,
            sw: 4,
            ..CornerRadius::ZERO
        },
        bar,
    );
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
                    let a = checks_list(ui, "Checks", &s.items, Some(&mut s.show_all));
                    s.actions.extend(a);
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
