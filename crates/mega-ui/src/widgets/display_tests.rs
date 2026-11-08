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
