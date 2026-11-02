//! Bottom status bar.

use egui::{Color32, Ui};

/// One piece of status text.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusItem {
    /// Text shown.
    pub text: String,
    /// Optional colour override (default: muted text).
    pub color: Option<Color32>,
}

/// Draw a status row: `left` items left-aligned, `right` items right-aligned.
pub fn status_bar(ui: &mut Ui, left: &[StatusItem], right: &[StatusItem]) {
    let item = |ui: &mut Ui, it: &StatusItem| {
        let color = it.color.unwrap_or(crate::theme::palette::TEXT_MUTED);
        ui.label(egui::RichText::new(&it.text).small().color(color));
    };
    ui.horizontal(|ui| {
        for it in left {
            item(ui, it);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Right-to-left layout: iterate reversed so the slice reads left to right.
            for it in right.iter().rev() {
                item(ui, it);
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    #[test]
    fn status_bar_renders_items() {
        let left = [StatusItem {
            text: "Ready".into(),
            color: None,
        }];
        let right = [StatusItem {
            text: "12 GB free".into(),
            color: Some(crate::theme::palette::HA_500),
        }];
        let harness = Harness::new_ui(|ui| status_bar(ui, &left, &right));
        harness.get_by_label("Ready");
        harness.get_by_label("12 GB free");
    }
}
