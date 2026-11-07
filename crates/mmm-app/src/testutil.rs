//! Test helpers: small synthetic mosaics (see [`crate::synth_mosaic`]),
//! never `test_data/`.

use std::path::Path;

use crate::session::OpenSession;
pub(crate) use crate::synth_mosaic::CANVAS;

/// A grouped Ha + OIII mosaic session in `root/m.mmm-session`, grouped by
/// the `FILTER` header, not yet analyzed.
pub(crate) fn two_group_session(root: &Path) -> OpenSession {
    let mosaic = crate::synth_mosaic::two_group_mosaic(root);
    let groups = mosaic.groups().ok();
    OpenSession {
        mosaic,
        frame: None,
        groups,
        lock: None,
        summary: None,
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
