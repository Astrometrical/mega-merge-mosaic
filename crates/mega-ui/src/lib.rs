//! Generic egui components and the Astrometrical theme shared by the Mega
//! apps.
//!
//! This crate is deliberately generic: it must not depend on `mmm-core` (or
//! any app engine), so it can later be lifted into workflow-platform or
//! Photometrical. App-specific knowledge lives in the app crates.
#![warn(missing_docs)]

pub mod brand;
pub mod checks;
pub mod gallery;
pub mod groups;
pub mod hero;
pub mod icons;
pub mod inspector;
pub mod jobs;
pub mod motion;
pub mod plan;
pub mod starfield;
pub mod status;
pub mod table;
pub mod theme;
pub mod viewer;
pub mod widgets;

/// Shared test helpers.
#[cfg(test)]
pub(crate) mod testutil {
    /// Install the theme on `ui`'s context; `false` (draw nothing this
    /// frame) until its fonts are in use, which is one frame later. Keeps
    /// egui_kittest's deterministic settings (no animations, no cursor
    /// blink), which the theme's style would otherwise replace.
    pub fn themed(ui: &mut egui::Ui) -> bool {
        let ready = crate::theme::ensure_applied(ui.ctx());
        if !ready {
            ui.ctx().all_styles_mut(|s| {
                s.animation_time = 0.0;
                s.scroll_animation = egui::style::ScrollAnimation::none();
                s.visuals.text_cursor.blink = false;
            });
        }
        ready
    }
}
