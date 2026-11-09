//! Keyboard shortcuts (spec §15), each named in its control's tooltip:
//!
//! | Key | Does | Control |
//! |---|---|---|
//! | `I` | fold or unfold the inspector | its Hide / Show button |
//! | `M` | plan or mosaic (once the mosaic can be shown) | **Plan** / **Mosaic** |
//! | `[` / `]` | previous / next group or composite, round the chips | the chips |
//! | `O` | **Outlines** | the toggle |
//! | `Shift+S` | **Seams** (plain `S` is Strong) | the toggle |
//! | `R` | **ROIs** | the toggle |
//! | `D` | **Draw ROI**: arm the draw tool on the canvas shown (a crosshair over it), revealing the ROIs section | the section's button |
//! | `L` | **Linked** (not for a mono view) | the toggle |
//! | `S` | **Strong** | the toggle |
//! | `Ctrl+Enter` (`Cmd+Enter` on macOS) | **Merge**, while the main button offers it | the main button |
//! | `Ctrl+E` (`Cmd+E` on macOS) | open the Export dialog, while it can open (something merged, no analysis running) | **Export…** |
//! | `Delete` (and `Backspace` on macOS) | remove the selected panels (a toast offers Undo), while the panel list is open | the row menu's **Remove** |
//! | `Esc` | clear the selection: a panel (in the panel list and on the plan), an overlap or a seam | |
//! | `Space`, held | comparing an ROI (spec §8.3): show the other side while held (Esc first if a control has the focus, as the toggle's tooltip says) | the **A** / **B** buttons |
//!
//! The letters, brackets, Delete and Space count with no modifier held
//! (`Shift+S` with Shift, the `Ctrl` ones with the command key). None
//! acts while a text field has the keyboard (notes, names, a path) or the
//! Export dialog is open, where Esc closes the dialog and nothing else (a
//! Space held since before still ends its peek when let go:
//! [`SessionWindow::follow_peek_key`]); a focused button or chip
//! (keyboard navigation) does not stop them, except Space, which presses
//! the focused control. The canvas always shows (the panel list sits beside
//! it), so the canvas's own (`M`, the brackets and the toggles) act with
//! the list open too. A key that is a shortcut is consumed, so no widget sees it
//! too, except Esc: an Esc that closes a menu or leaves a text field does
//! only that, keeping the selection.

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
/// Toggles **Seams** (`S` alone is [`STRONG`]).
pub(super) const SEAMS: KeyboardShortcut = KeyboardShortcut::new(Modifiers::SHIFT, Key::S);
/// Toggles **ROIs**.
pub(super) const ROIS: Key = Key::R;
/// Arms the draw tool for a new ROI, as **Draw ROI** does.
pub(super) const DRAW_ROI: Key = Key::D;
/// Toggles **Linked**.
pub(super) const LINKED: Key = Key::L;
/// Toggles **Strong**.
pub(super) const STRONG: Key = Key::S;
/// Merges, as the main button does.
pub(super) const MERGE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Enter);
/// Opens the Export dialog, as **Export…** does.
pub(super) const EXPORT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::E);
/// Removes the panels selected in the panel list.
pub(super) const REMOVE: Key = Key::Delete;
/// Removes them too on macOS, whose keyboards label Backspace "delete".
pub(super) const REMOVE_MAC: Key = Key::Backspace;
/// Clears the selection.
pub(super) const CLEAR: Key = Key::Escape;
/// Held while comparing an ROI: shows the side not selected.
pub(super) const PEEK: Key = Key::Space;

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
        let typing = ctx.text_edit_focused() || self.export.is_open();
        self.follow_peek_key(ctx, typing);
        if typing {
            return;
        }
        let pressed = |key: Key| {
            ctx.input_mut(|i| i.modifiers.is_none() && i.consume_key(Modifiers::NONE, key))
        };
        if self.panel_list_open {
            // Both keys are checked (and consumed) whatever the other did.
            let delete = pressed(REMOVE);
            let backspace = cfg!(target_os = "macos") && pressed(REMOVE_MAC);
            if delete || backspace {
                let removed = self.panels.remove_selection(session, jobs);
                self.panels_removed(&removed);
            }
        }
        // Not consumed: an open menu closes on it (and the selection
        // stays), as does a text field it leaves (egui has let go of the
        // field already: hence what the last frame ended with).
        let escape = ctx.input(|i| i.key_pressed(CLEAR));
        if escape && !self.was_typing && !egui::Popup::is_any_open(ctx) {
            self.clear_selection();
        }
        if pressed(FOLD) {
            self.inspector.folded = !self.inspector.folded;
        }
        if ctx.input_mut(|i| i.consume_shortcut(&MERGE)) && self.can_merge(session, jobs) {
            self.merge(session, jobs);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&EXPORT)) {
            self.open_export(session, jobs);
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
        // Before Strong: `pressed` wants no modifier, so Shift+S is never
        // Strong, and this consumes it.
        if ctx.input_mut(|i| i.consume_shortcut(&SEAMS)) {
            self.look.seams = !self.look.seams;
        }
        if pressed(ROIS) {
            self.look.rois = !self.look.rois;
        }
        if pressed(DRAW_ROI) {
            self.start_drawing_roi(session);
            // The section shows what the armed tool does and that Esc
            // cancels it, however it was left.
            if self.drawing_roi() {
                self.inspector.reveal(super::ROIS);
            }
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
        let composites = compositions_of(session);
        let views: Vec<ViewKind> = groups
            .into_iter()
            .map(ViewKind::Group)
            .chain(composites.iter().map(|c| ViewKind::Composition(c.id)))
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
