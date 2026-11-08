//! Test helpers: small synthetic mosaics (see [`crate::synth_mosaic`]),
//! never `test_data/`.

use std::path::Path;

use crate::session::OpenSession;
pub(crate) use crate::synth_mosaic::CANVAS;

/// How long a test waits for background work (worker threads, jobs, tile
/// loads) to reach the state it checks: generous, so a slow, loaded machine
/// never trips it. Every wait ends as soon as its condition holds.
pub(crate) const PATIENCE: std::time::Duration = std::time::Duration::from_secs(120);

/// A grouped Ha + OIII mosaic session in `root/m.mmm-session`, grouped by
/// the `FILTER` header, not yet analyzed.
pub(crate) fn two_group_session(root: &Path) -> OpenSession {
    open(crate::synth_mosaic::two_group_mosaic(root))
}

/// [`two_group_session`] in `root/{name}.mmm-session` (a session is
/// named after its folder).
pub(crate) fn two_group_session_named(root: &Path, name: &str) -> OpenSession {
    let made = crate::synth_mosaic::two_group_mosaic(root);
    let dir = root.join(format!("{name}.mmm-session"));
    std::fs::rename(&made.dir, &dir).unwrap();
    open(mmm_core::mosaic::Mosaic::open(&dir).unwrap())
}

/// [`two_group_session`] with mono (one-channel) panels.
pub(crate) fn two_group_mono_session(root: &Path) -> OpenSession {
    open(crate::synth_mosaic::two_group_mosaic_of(root, 1))
}

/// `mosaic` as opened, its groups resolved, without a lock.
pub(crate) fn open(mosaic: mmm_core::mosaic::Mosaic) -> OpenSession {
    let groups = mosaic.groups().ok();
    OpenSession {
        mosaic,
        frame: None,
        groups,
        lock: None,
        summary: None,
        merged: false,
        results: Default::default(),
    }
}

/// Install the Mega theme on `ui`'s context, as the app does before its
/// first frame; `false` (draw nothing this frame) until its fonts are in
/// use, one frame later. Keeps egui_kittest's deterministic settings (no
/// animations, no cursor blink), which the theme's style would replace.
pub(crate) fn themed(ui: &mut egui::Ui) -> bool {
    let ready = mega_ui::theme::ensure_applied(ui.ctx());
    if !ready {
        ui.ctx().all_styles_mut(|s| {
            s.animation_time = 0.0;
            s.scroll_animation = egui::style::ScrollAnimation::none();
            s.visuals.text_cursor.blink = false;
        });
    }
    ready
}

/// A solved (plate-solved, header-only WCS) two-group mosaic session in
/// `root/s.mmm-session`: groups `Ha` and `OIII` of two overlapping 200x150
/// panels each (manual groups), to be analysed from the solutions
/// (`InputSelect::Solved`); nothing is analysed yet. Also a third `Ha` panel,
/// not in the session, overlapping the others and extending the canvas to
/// the left: adding it moves the reference frame.
pub(crate) fn solved_two_group_session(root: &Path) -> (OpenSession, std::path::PathBuf) {
    use mmm_core::synth::{SynthWcs, write_xisf_solved};
    let (w, h) = (200u64, 150u64);
    // Pixel values depend on the sky position only, so panels agree where
    // they overlap.
    let sky = |x: f64, y: f64, seed: f64| -> f32 {
        let hash = ((x * 12.9898 + y * 78.233 + seed).sin() * 43758.5453)
            .fract()
            .abs();
        (0.2 + 0.001 * x + 0.0005 * y + 0.05 * hash) as f32
    };
    let write = |name: &str, refimg: [f64; 2], seed: f64| -> std::path::PathBuf {
        let path = root.join(name);
        let wcs = SynthWcs {
            crval: [83.8, -5.4],
            refimg,
            cd: [[-1e-3, 0.0], [0.0, 1e-3]],
        };
        let planes: Vec<f32> = (0..w * h)
            .map(|i| {
                let (px, py) = ((i % w) as f64, (i / w) as f64);
                sky(px - refimg[0], py - refimg[1], seed)
            })
            .collect();
        write_xisf_solved(&path, w, h, 1, &planes, &wcs).unwrap();
        path
    };
    let panels: Vec<(std::path::PathBuf, &str)> = vec![
        (write("ha_0.xisf", [100.0, 75.0], 1.0), "Ha"),
        (write("ha_1.xisf", [40.0, 45.0], 1.0), "Ha"),
        (write("oiii_0.xisf", [100.0, 75.0], 2.0), "OIII"),
        (write("oiii_1.xisf", [40.0, 45.0], 2.0), "OIII"),
    ];
    let extra = write("ha_2.xisf", [250.0, 75.0], 1.0);
    let mut cfg = mmm_core::mosaic::MosaicConfig::new(panels.iter().map(|p| p.0.clone()).collect());
    cfg.analyze.input = mmm_core::analyze::InputSelect::Solved;
    cfg.grouping = mmm_core::mosaic::Grouping {
        enabled: true,
        rule: mmm_core::mosaic::GroupRule::Manual,
    };
    for (entry, (_, group)) in cfg.panels.iter_mut().zip(&panels) {
        entry.manual_group = Some((*group).into());
    }
    let mosaic = mmm_core::mosaic::Mosaic::create(&root.join("s.mmm-session"), cfg).unwrap();
    (open(mosaic), extra)
}
