//! An image snapshot of the real start screen, on synthetic sessions: a
//! merged one (the hero shows its blended tiles), an empty one and a
//! missing one. Ignored by default because it needs a GPU; run it with
//!
//! ```text
//! WGPU_BACKEND=vulkan cargo test -p mmm-app start::snapshot_tests -- --ignored
//! ```
//!
//! and refresh the reference in `tests/snapshots/` with `UPDATE_SNAPSHOTS=1`
//! after an intended visual change. The stage is held still (reduced motion,
//! no drift or crossfade) so the image is stable.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use egui_kittest::Harness;
use mmm_core::job::JobCtl;
use mmm_core::mosaic::{analyze_all, blend_group};
use mmm_core::session_summary::{result_state, write_summary};

use super::hero::Thumb;
use super::*;
use crate::start::tests::{StubPicker, entry_at, session, step_until};
use crate::summary_writer::summarize;
use crate::testutil::{CANVAS, two_group_session};

/// A two-group session `Orion Nebula` in `root`, analysed and blended, with
/// its summary written.
fn merged(root: &Path) -> PathBuf {
    let mut m = two_group_session(root).mosaic;
    let groups = analyze_all(&mut m, &JobCtl::none()).unwrap();
    for g in ["Ha", "OIII"] {
        blend_group(&mut m, g, None, &JobCtl::none()).unwrap();
    }
    let results = groups
        .processing_groups()
        .into_iter()
        .map(|(g, _)| {
            let state = result_state(&m.result_path(&g));
            (g, state)
        })
        .collect();
    let summary = summarize(
        &m.config,
        Some(&groups),
        None,
        Some(CANVAS),
        None,
        &results,
        &BTreeMap::new(),
    );
    write_summary(&m.dir, &summary).unwrap();
    // A name worth reading on the card (the synthetic session is `m`).
    let named = root.join("Orion Nebula.mmm-session");
    std::fs::rename(&m.dir, &named).unwrap();
    named
}

#[test]
#[ignore = "image snapshot: needs a GPU; run with --ignored"]
fn start_screen_snapshot() {
    // A fixed folder (no process id): the screen shows the session's path.
    let root = std::env::temp_dir().join("mmm-start-snapshot");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let merged_dir = merged(&root);
    let notes =
        "Shooting the 4th row next new moon.\nPanel 7: gradient in the lower left, redo DBE.";
    crate::session::save_notes_blocking(&merged_dir, notes).unwrap();
    let planned = session(&root, "Veil Nebula");
    let gone = root.join("Rosette.mmm-session");
    // Opened a few days ago (the screen says "2 days ago", whatever the
    // time of day).
    let day = 24 * 3600;
    let ago = |days: u64| unix_now() - days * day - day / 2;
    let recent = vec![
        entry_at(&merged_dir, ago(2)),
        entry_at(&planned, ago(9)),
        entry_at(&gone, ago(30)),
    ];
    let screen = StartScreen::new(recent, Arc::new(StubPicker(None)));
    // Edge to edge, as the app draws it (no panel margin): the harness
    // insets its ui by 8 points, so draw into the whole screen instead.
    let mut h = Harness::builder()
        .wgpu()
        .with_size(egui::vec2(1280.0, 800.0))
        .with_step_dt(0.02)
        .with_max_steps(64)
        .build_ui_state(
            |ui, s: &mut StartScreen| {
                let screen = ui.ctx().content_rect();
                ui.scope_builder(egui::UiBuilder::new().max_rect(screen), |ui| {
                    ui.set_clip_rect(screen);
                    if crate::testutil::themed(ui) {
                        ui.painter()
                            .rect_filled(screen, 0.0, mega_ui::theme::palette::SURFACE_900);
                        let _ = s.ui(ui);
                    }
                });
            },
            screen,
        );
    // Wait for everything the image shows: the facts, the reduced-motion
    // probe, the hero's first tiles with no more loading, and the merged
    // session's card thumbnail. Then hold the stage still.
    step_until(&mut h, |s| {
        !s.checking_recent()
            && s.motion.is_none()
            && s.stage.first_image_drawn()
            && !s.stage.is_loading()
            && matches!(s.thumbs.get(&merged_dir), Some(Thumb::Ready(Some(_))))
    });
    h.state_mut().stage.set_reduced_motion(true);
    h.run();
    h.snapshot("start_screen");
    let _ = std::fs::remove_dir_all(&root);
}
