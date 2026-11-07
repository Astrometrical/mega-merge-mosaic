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
pub mod icons;
pub mod jobs;
pub mod plan;
pub mod status;
pub mod theme;
pub mod viewer;
pub mod widgets;

/// Shared test helpers.
#[cfg(test)]
pub(crate) mod testutil {
    /// Install the theme on `ui`'s context; `false` (draw nothing this
    /// frame) until its fonts are in use, which is one frame later.
    pub fn themed(ui: &mut egui::Ui) -> bool {
        crate::theme::ensure_applied(ui.ctx())
    }
}
