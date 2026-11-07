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
use std::time::Duration;

use egui_kittest::Harness;
use mmm_core::job::JobCtl;
use mmm_core::mosaic::{analyze_all, blend_group};
use mmm_core::session_summary::{result_state, write_summary};

use super::*;
use crate::start::tests::{StubPicker, entry_at, session, step_until, temp_dir};
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
    let root = temp_dir("start-snapshot");
    let merged_dir = merged(&root);
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
    let mut h = Harness::builder()
        .wgpu()
        .with_size(egui::vec2(1280.0, 800.0))
        .with_step_dt(0.02)
        .with_max_steps(64)
        .build_ui_state(
            |ui, s: &mut StartScreen| {
                if crate::testutil::themed(ui) {
                    let _ = s.ui(ui);
                }
            },
            screen,
        );
    // Facts, the reduced-motion probe and the hero's first tiles arrive on
    // background threads.
    step_until(&mut h, |s| {
        !s.checking_recent() && s.motion.is_none() && s.stage.first_image_drawn()
    });
    // Hold the stage still, then let the tiles settle at their final level.
    h.state_mut().stage.set_reduced_motion(true);
    for _ in 0..25 {
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    h.run();
    h.snapshot("start_screen");
    let _ = std::fs::remove_dir_all(&root);
}
