//! Image snapshots of the gallery. They need a GPU, so they are ignored by
//! default (CI has none); run them with
//! `cargo test -p mega-ui --test snapshots -- --ignored`, and refresh the
//! references with `UPDATE_SNAPSHOTS=1` after an intended visual change.
//! If wgpu can't create a device (DX12 fails on some Windows machines), set
//! `WGPU_BACKEND=vulkan`; the references were made with Vulkan.

use egui_kittest::Harness;
use mega_ui::gallery::{self, GalleryState};

fn gallery_harness(pixels_per_point: f32) -> Harness<'static, GalleryState> {
    Harness::builder()
        .wgpu()
        .with_size(gallery::FULL_SIZE)
        .with_pixels_per_point(pixels_per_point)
        .build_ui_state(
            |ui, s: &mut GalleryState| {
                if mega_ui::theme::ensure_applied(ui.ctx()) {
                    gallery::show(ui, s);
                }
            },
            GalleryState::default(),
        )
}

#[test]
#[ignore = "image snapshot: needs a GPU; run with --ignored"]
fn gallery_snapshot_1x() {
    let mut h = gallery_harness(1.0);
    h.run_steps(4);
    h.snapshot("gallery_1x");
}

#[test]
#[ignore = "image snapshot: needs a GPU; run with --ignored"]
fn gallery_snapshot_2x() {
    let mut h = gallery_harness(2.0);
    h.run_steps(4);
    h.snapshot("gallery_2x");
}
