use egui::Vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

use super::chip::{CHIP_MAX_WIDTH, chip};
use super::property::{property_grid, property_row, property_row_numeric};
use super::status_line::{overline, status};
use crate::theme::{Tone, palette};

fn harness(
    add: impl Fn(&mut egui::Ui) -> egui::Response + 'static,
) -> Harness<'static, Option<egui::Rect>> {
    Harness::builder()
        .with_size(Vec2::new(600.0, 200.0))
        .build_ui_state(
            move |ui, rect: &mut Option<egui::Rect>| {
                if crate::testutil::themed(ui) {
                    *rect = Some(add(ui).rect);
                }
            },
            None,
        )
}

const LONG: &str = "Ha_3nm_2026-10-12_flat-corrected_master_light";

#[test]
fn chip_shows_its_label() {
    let mut h = harness(|ui| chip(ui, "18 panels", None));
    h.run();
    h.get_by_label("18 panels");
}

#[test]
fn long_chip_truncates_but_keeps_full_label() {
    let mut h = harness(|ui| chip(ui, LONG, Some(palette::HA_500)));
    h.run();
    let r = h.state().unwrap();
    assert!(r.width() <= CHIP_MAX_WIDTH + 0.5, "{r:?}");
    h.get_by_label(LONG);
}

#[test]
fn status_shows_label() {
    let mut h = harness(|ui| status(ui, Tone::Warning, "Re-merge needed"));
    h.run();
    h.get_by_label("Re-merge needed");
}

#[test]
fn overline_is_upper_case() {
    let mut h = harness(|ui| overline(ui, "Notes"));
    h.run();
    h.get_by_label("NOTES");
}

#[test]
fn property_rows_render_label_and_value() {
    let mut h = harness(|ui| {
        property_grid(ui, "props", |ui| {
            property_row(ui, "Canvas", "24 310 × 15 870 px");
            property_row_numeric(ui, "Scale", "1.62″/px");
        });
        ui.label("end")
    });
    h.run();
    for t in ["Canvas", "24 310 × 15 870 px", "Scale", "1.62″/px"] {
        h.get_by_label(t);
    }
}

#[test]
fn choice_chip_reports_selected_state() {
    use super::chip::{ChoiceChip, choice_chip};
    for selected in [true, false] {
        let mut h = harness(move |ui| {
            choice_chip(
                ui,
                &ChoiceChip {
                    label: "Hα",
                    color: Some(palette::HA_500),
                    selected,
                    dashed: false,
                    mark: None,
                },
            )
        });
        h.run();
        let node = h.get_by_label("Hα");
        let want = if selected {
            egui::accesskit::Toggled::True
        } else {
            egui::accesskit::Toggled::False
        };
        assert_eq!(node.accesskit_node().toggled(), Some(want));
    }
}

#[test]
fn choice_chip_click_is_reported() {
    use super::chip::{ChoiceChip, choice_chip};
    use std::{cell::Cell, rc::Rc};
    let clicked = Rc::new(Cell::new(false));
    let seen = clicked.clone();
    let mut h = harness(move |ui| {
        let r = choice_chip(
            ui,
            &ChoiceChip {
                label: "Hα",
                color: None,
                selected: false,
                dashed: true,
                mark: None,
            },
        );
        if r.clicked() {
            seen.set(true);
        }
        r
    });
    h.run();
    assert!(!clicked.get());
    h.get_by_label("Hα").click();
    h.run();
    assert!(clicked.get());
}

#[test]
fn choice_chip_draws_its_mark() {
    use super::chip::{ChipMark, ChoiceChip, choice_chip};
    use std::{cell::Cell, rc::Rc};
    let widths = Rc::new(Cell::new([0.0_f32; 3]));
    let seen = widths.clone();
    let mut h = harness(move |ui| {
        let mut w = [0.0_f32; 3];
        let mut last = None;
        for (i, mark) in [None, Some(ChipMark::Attention), Some(ChipMark::Changed)]
            .into_iter()
            .enumerate()
        {
            let r = choice_chip(
                ui,
                &ChoiceChip {
                    label: "Same",
                    color: None,
                    selected: false,
                    dashed: false,
                    mark,
                },
            )
            .on_hover_text("Caller's hint");
            w[i] = r.rect.width();
            last = Some(r);
        }
        seen.set(w);
        last.unwrap()
    });
    h.run();
    // The glyph takes room, and the state is said in words.
    let [plain, stale, edited] = widths.get();
    assert!(stale > plain && edited > plain, "{plain} {stale} {edited}");
    let said: Vec<Option<String>> = h
        .query_all_by_label("Same")
        .map(|n| n.accesskit_node().description())
        .collect();
    assert_eq!(
        said,
        [
            None,
            Some("out of date".to_string()),
            Some("settings changed".to_string())
        ]
    );
    // Not colour alone: each mark is its tone's icon, in the icon font and
    // the tone's colour, drawn once (the plain chip draws none).
    let glyphs = |h: &Harness<'static, Option<egui::Rect>>, tone: Tone| {
        let g = tone.icon().unwrap();
        h.output()
            .shapes
            .iter()
            .filter(|c| match &c.shape {
                egui::Shape::Text(t) => {
                    t.galley.text().contains(g)
                        && t.galley.job.sections[0].format.font_id.family
                            == crate::theme::fonts::icon_family()
                        && t.galley.job.sections[0].format.color == tone.color()
                }
                _ => false,
            })
            .count()
    };
    assert_eq!(glyphs(&h, Tone::Warning), 1, "the out-of-date glyph");
    assert_eq!(glyphs(&h, Tone::Info), 1, "the settings-changed glyph");
    assert_ne!(ChipMark::Attention.color(), ChipMark::Changed.color());
    // Resting on a marked chip shows its state in words, with the caller's
    // hover text; the plain chip shows only the caller's.
    let at: Vec<egui::Pos2> = h
        .query_all_by_label("Same")
        .map(|n| n.rect().center())
        .collect();
    let rest = |h: &mut Harness<'static, Option<egui::Rect>>, p: egui::Pos2| {
        h.hover_at(egui::pos2(590.0, 190.0));
        h.run_steps(80);
        h.hover_at(p);
        h.run_steps(80);
    };
    for (p, state) in at
        .iter()
        .zip([None, Some("Out of date"), Some("Settings changed")])
    {
        rest(&mut h, *p);
        h.get_by_label("Caller's hint");
        for s in ["Out of date", "Settings changed"] {
            assert_eq!(
                h.query_by_label(s).is_some(),
                state == Some(s),
                "{s} while resting on the chip marked {state:?}"
            );
        }
    }
}
