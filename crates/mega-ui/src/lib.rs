//! Generic egui components and the Astrometrical theme shared by the Mega
//! apps.
//!
//! This crate is deliberately generic: it must not depend on `mmm-core` (or
//! any app engine), so it can later be lifted into workflow-platform or
//! Photometrical. App-specific knowledge lives in the app crates.
#![warn(missing_docs)]

pub mod brand;
pub mod canvas_layers;
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

    /// Whether any shape strokes a line with `color` at `width`.
    pub fn has_stroke(
        shapes: &[egui::epaint::ClippedShape],
        color: egui::Color32,
        width: f32,
    ) -> bool {
        use egui::epaint::ColorMode;
        fn hit(s: &egui::Shape, color: egui::Color32, width: f32) -> bool {
            match s {
                egui::Shape::Path(p) => {
                    matches!(&p.stroke.color, ColorMode::Solid(c) if *c == color)
                        && (p.stroke.width - width).abs() < 1e-3
                }
                egui::Shape::LineSegment { stroke, .. } => {
                    stroke.color == color && (stroke.width - width).abs() < 1e-3
                }
                egui::Shape::Vec(v) => v.iter().any(|s| hit(s, color, width)),
                _ => false,
            }
        }
        shapes.iter().any(|c| hit(&c.shape, color, width))
    }
}
