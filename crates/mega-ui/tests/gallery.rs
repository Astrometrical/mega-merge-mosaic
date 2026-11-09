//! The gallery renders every section of the Mega UI standard.

use std::collections::BTreeSet;

use egui::Vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mega_ui::gallery::{self, GalleryState};

#[test]
fn gallery_renders_every_section() {
    // Each page renders in full at its own size; every label is on one.
    let mut found = BTreeSet::new();
    let mut lockups = 0;
    let wanted: &[&str] = &[
        "TYPE SCALE",
        "TEXT ROLES",
        "SURFACES",
        "SEMANTIC COLOURS",
        "GROUP PALETTE",
        "BUTTONS",
        "INPUTS",
        "CHIPS",
        "STEP STRIP",
        "INSPECTOR",
        "TABLE",
        "ORDERED LIST",
        "Region 2",
        "Notes for the selected region show here.",
        "STATUS",
        "CHECK LIST",
        "CHECK LIST · ALL",
        "PROPERTY GRID",
        "PROGRESS",
        "TOASTS",
        "ICONS",
        "BRAND LOCKUP",
        "STARFIELD",
        "HERO STAGE",
        "HERO STAGE · PORTRAIT MOSAIC",
        "PLAN CANVAS",
        "SESSION CARDS",
        "CANVAS LABEL",
        "DIALOG",
        "Compare Trapezium",
        "Show B of Sword",
        "41 checks passed",
        "62% · 0:41 left",
    ];
    for page in 0..gallery::PAGES {
        let mut h = Harness::builder()
            .with_size(gallery::page_size(page))
            .build_ui_state(
                move |ui, s: &mut GalleryState| {
                    if mega_ui::theme::ensure_applied(ui.ctx()) {
                        gallery::show_page(ui, s, page);
                    }
                },
                GalleryState::default(),
            );
        // The indeterminate progress bar animates, so step a fixed number
        // of frames instead of running until idle.
        h.run_steps(4);
        for label in wanted {
            if h.query_by_label(label).is_some() {
                found.insert(*label);
            }
        }
        lockups += h.query_all_by_label("Mega Merge Mosaic").count();
    }
    for label in wanted {
        assert!(found.contains(label), "no page shows {label:?}");
    }
    // Both lockup sizes.
    assert_eq!(lockups, 2);
}

#[test]
fn the_whole_gallery_shows_in_one_scrolling_area() {
    let mut h = Harness::builder()
        .with_size(Vec2::new(1200.0, 900.0))
        .build_ui_state(
            |ui, s: &mut GalleryState| {
                if mega_ui::theme::ensure_applied(ui.ctx()) {
                    gallery::show(ui, s);
                }
            },
            GalleryState::default(),
        );
    h.run_steps(4);
    h.get_by_label("TYPE SCALE");
}
