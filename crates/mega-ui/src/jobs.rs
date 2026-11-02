//! Background job panel.

use egui::Ui;

/// Display snapshot of one running job.
pub struct JobView {
    /// Job id, echoed back in actions.
    pub id: u64,
    /// Short job title.
    pub label: String,
    /// Current stage description.
    pub stage: String,
    /// Units done.
    pub done: u64,
    /// Total units (0 = unknown).
    pub total: u64,
    /// Whether a Cancel button is offered.
    pub cancellable: bool,
}

/// User action raised by the job panel.
#[derive(Debug, PartialEq, Eq)]
pub enum JobAction {
    /// Cancel the job with this id.
    Cancel(u64),
}

/// Draw each job with a progress bar and optional Cancel button.
pub fn job_panel(ui: &mut Ui, jobs: &[JobView]) -> Vec<JobAction> {
    let mut actions = Vec::new();
    for job in jobs {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&job.label).color(crate::theme::palette::TEXT_STRONG));
            ui.label(egui::RichText::new(&job.stage).color(crate::theme::palette::TEXT_MUTED));
        });
        ui.horizontal(|ui| {
            let mut bar = if job.total > 0 {
                egui::ProgressBar::new((job.done as f32 / job.total as f32).clamp(0.0, 1.0))
            } else {
                egui::ProgressBar::new(0.0)
            }
            .fill(crate::theme::palette::ACCENT_500);
            if job.total > 0 {
                bar = bar.show_percentage();
            }
            // Leave room for the Cancel button (a bare bar fills the row).
            let room = if job.cancellable { 80.0 } else { 0.0 };
            ui.add(bar.desired_width((ui.available_width() - room).max(60.0)));
            if job.cancellable && ui.button("Cancel").clicked() {
                actions.push(JobAction::Cancel(job.id));
            }
        });
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    #[test]
    fn job_panel_cancel_returns_action() {
        let jobs = [JobView {
            id: 7,
            label: "Blend".into(),
            stage: "Seams".into(),
            done: 3,
            total: 10,
            cancellable: true,
        }];
        let mut actions = Vec::new();
        let mut harness = Harness::new_ui(|ui| actions.extend(job_panel(ui, &jobs)));
        harness.run();
        harness.get_by_label("Cancel").click();
        harness.run();
        drop(harness);
        assert_eq!(actions, vec![JobAction::Cancel(7)]);
    }
}
