//! The gallery renders every section of the Mega UI standard.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mega_ui::gallery::{self, GalleryState};

#[test]
fn gallery_renders_every_section() {
    let mut h = Harness::builder()
        .with_size(egui::vec2(1200.0, 4000.0))
        .build_ui_state(
            |ui, s: &mut GalleryState| {
                if mega_ui::theme::ensure_applied(ui.ctx()) {
                    gallery::show(ui, s);
                }
            },
            GalleryState::default(),
        );
    // The indeterminate progress bar animates, so step a fixed number of
    // frames instead of running until idle.
    h.run_steps(4);
    for label in [
        "TYPE SCALE",
        "TEXT ROLES",
        "SURFACES",
        "SEMANTIC COLOURS",
        "GROUP PALETTE",
        "BUTTONS",
        "INPUTS",
        "CHIPS",
        "STATUS",
        "CHECK LIST",
        "PROPERTY GRID",
        "PROGRESS",
        "TOASTS",
        "ICONS",
        "BRAND LOCKUP",
        "STARFIELD",
        "HERO STAGE",
        "SESSION CARDS",
        "41 checks passed",
        "62% · 0:41 left",
    ] {
        h.get_by_label(label);
    }
    // Both lockup sizes.
    assert_eq!(h.query_all_by_label("Mega Merge Mosaic").count(), 2);
}
