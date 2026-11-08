//! Keyboard shortcuts (spec §15), each named in its control's tooltip:
//!
//! | Key | Does | Control |
//! |---|---|---|
//! | `I` | fold or unfold the inspector | its Hide / Show button |
//! | `M` | plan or mosaic (once the mosaic can be shown) | **Plan** / **Mosaic** |
//! | `[` / `]` | previous / next group or composite, round the chips | the chips |
//! | `O` | **Outlines** | the toggle |
//! | `R` | **ROIs** | the toggle |
//! | `L` | **Linked** (not for a mono view) | the toggle |
//! | `S` | **Strong** | the toggle |
//! | `Ctrl+Enter` (`Cmd+Enter` on macOS) | **Merge**, while the main button offers it | the main button |
//!
//! The letters and brackets count with no modifier held. None acts while a
//! text field has the keyboard (notes, names, a path) or the Export dialog
//! is open; a focused button or chip (keyboard navigation) does not stop
//! them. The canvas always shows (the panel list sits beside it), so the
//! canvas's own (`M`, the brackets and the toggles) act with the list open
//! too. A key that is a shortcut is consumed, so no widget sees it too.

use egui::{Key, KeyboardShortcut, Modifiers};

use super::{CanvasMode, SessionWindow, compositions_of};
use crate::compositions::ViewKind;
use crate::display_stretch::DisplayStretch;
use crate::jobs::JobRunner;
use crate::pipeline::Pipeline;
use crate::session::OpenSession;
use crate::util::group_names;

/// Folds and unfolds the inspector.
pub(super) const FOLD: Key = Key::I;
/// Switches between plan and mosaic.
pub(super) const PLAN_MOSAIC: Key = Key::M;
/// Shows the previous group or composite.
pub(super) const PREVIOUS_VIEW: Key = Key::OpenBracket;
/// Shows the next group or composite.
pub(super) const NEXT_VIEW: Key = Key::CloseBracket;
/// Toggles **Outlines**.
pub(super) const OUTLINES: Key = Key::O;
/// Toggles **ROIs**.
pub(super) const ROIS: Key = Key::R;
/// Toggles **Linked**.
pub(super) const LINKED: Key = Key::L;
/// Toggles **Strong**.
pub(super) const STRONG: Key = Key::S;
/// Merges, as the main button does.
pub(super) const MERGE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Enter);

/// `text` naming its shortcut `key`: "Outlines (O)".
pub(super) fn keyed(text: &str, key: Key) -> String {
    format!("{text} ({})", key.symbol_or_name())
}

/// The chips' tooltip: what `[` and `]` do.
pub(super) fn views_hint() -> String {
    format!(
        "Previous / next group or composite ({} / {})",
        PREVIOUS_VIEW.symbol_or_name(),
        NEXT_VIEW.symbol_or_name()
    )
}

impl SessionWindow {
    /// Act on this frame's shortcuts (see the module docs). Call before
    /// drawing, so the frame shows what they did.
    pub(super) fn shortcuts(
        &mut self,
        ctx: &egui::Context,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
        pipeline: &Pipeline,
    ) {
        if ctx.text_edit_focused() || self.mosaic.export_open() {
            return;
        }
        let pressed = |key: Key| {
            ctx.input_mut(|i| i.modifiers.is_none() && i.consume_key(Modifiers::NONE, key))
        };
        if pressed(FOLD) {
            self.inspector.folded = !self.inspector.folded;
        }
        if ctx.input_mut(|i| i.consume_shortcut(&MERGE)) && self.can_merge(session, jobs) {
            self.merge(session, jobs);
        }
        // As the segment: the plan always, the mosaic once it can show.
        if pressed(PLAN_MOSAIC) {
            match self.mode() {
                CanvasMode::Mosaic => self.set_mode(CanvasMode::Plan),
                CanvasMode::Plan if session.merged || jobs.blending() => {
                    self.set_mode(CanvasMode::Mosaic);
                }
                CanvasMode::Plan => {}
            }
        }
        let step = pressed(NEXT_VIEW) as isize - pressed(PREVIOUS_VIEW) as isize;
        if step != 0 {
            self.step_view(session, step);
        }
        if pressed(OUTLINES) {
            self.look.outlines = !self.look.outlines;
            self.look.numbers = self.look.outlines;
        }
        if pressed(ROIS) {
            self.look.rois = !self.look.rois;
        }
        let mut stretch = DisplayStretch::load(&session.mosaic.config.ui);
        let before = stretch;
        if pressed(LINKED) && !self.is_mono(session, pipeline) {
            stretch.linked = !stretch.linked;
        }
        if pressed(STRONG) {
            stretch.strong = !stretch.strong;
        }
        if stretch != before {
            stretch.save(&mut session.mosaic.config.ui);
            self.save.touch();
        }
    }

    /// Show the view `step` places along the chips (groups, then
    /// composites) from the one shown, round from the last to the first;
    /// nothing without chips (fewer than two groups).
    fn step_view(&mut self, session: &OpenSession, step: isize) {
        let groups = group_names(session);
        if groups.len() < 2 {
            return;
        }
        let composites = compositions_of(session).len();
        let views: Vec<ViewKind> = groups
            .into_iter()
            .map(ViewKind::Group)
            .chain((0..composites).map(ViewKind::Composition))
            .collect();
        let at = self
            .view
            .as_ref()
            .and_then(|v| views.iter().position(|w| w == v))
            .unwrap_or(0);
        let next = (at as isize + step).rem_euclid(views.len() as isize) as usize;
        self.select_view(views[next].clone());
    }
}
