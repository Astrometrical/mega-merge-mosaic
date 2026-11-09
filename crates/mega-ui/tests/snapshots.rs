//! Image snapshots of the gallery, one per page and scale. They need a GPU,
//! so they are ignored by default (CI has none); run them with
//! `cargo test -p mega-ui --test snapshots -- --ignored`, and refresh the
//! references with `UPDATE_SNAPSHOTS=1` after an intended visual change.
//! If wgpu can't create a device (DX12 fails on some Windows machines), set
//! `WGPU_BACKEND=vulkan`; the references were made with Vulkan.

use egui_kittest::Harness;
use mega_ui::gallery::{self, GalleryState};

/// Snapshot gallery `page` (0-based) at `pixels_per_point` as
/// `gallery_p{page + 1}_{scale}x`.
fn snapshot_page(page: usize, pixels_per_point: f32) {
    let mut h = Harness::builder()
        .wgpu()
        .with_size(gallery::page_size(page))
        .with_pixels_per_point(pixels_per_point)
        .build_ui_state(
            move |ui, s: &mut GalleryState| {
                if mega_ui::theme::ensure_applied(ui.ctx()) {
                    gallery::show_page(ui, s, page);
                }
            },
            GalleryState::default(),
        );
    h.run_steps(4);
    h.snapshot(format!(
        "gallery_p{}_{}x",
        page + 1,
        pixels_per_point as u32
    ));
}

#[test]
#[ignore = "image snapshot: needs a GPU; run with --ignored"]
fn gallery_snapshot_p1_1x() {
    snapshot_page(0, 1.0);
}

#[test]
#[ignore = "image snapshot: needs a GPU; run with --ignored"]
fn gallery_snapshot_p2_1x() {
    snapshot_page(1, 1.0);
}

#[test]
#[ignore = "image snapshot: needs a GPU; run with --ignored"]
fn gallery_snapshot_p3_1x() {
    snapshot_page(2, 1.0);
}

#[test]
#[ignore = "image snapshot: needs a GPU; run with --ignored"]
fn gallery_snapshot_p1_2x() {
    snapshot_page(0, 2.0);
}

#[test]
#[ignore = "image snapshot: needs a GPU; run with --ignored"]
fn gallery_snapshot_p2_2x() {
    snapshot_page(1, 2.0);
}

#[test]
#[ignore = "image snapshot: needs a GPU; run with --ignored"]
fn gallery_snapshot_p3_2x() {
    snapshot_page(2, 2.0);
}
