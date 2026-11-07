//! Background job panel.

use egui::Ui;

use crate::widgets::{ButtonKind, MegaButton, progress};

/// Display snapshot of one running job.
#[derive(Debug, Clone, PartialEq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobAction {
    /// Cancel the job with this id.
    Cancel(u64),
}

/// Draw each job as a progress row (readout "N%" when the total is
/// known, else the stage), with an optional Cancel button.
pub fn job_panel(ui: &mut Ui, jobs: &[JobView]) -> Vec<JobAction> {
    let mut actions = Vec::new();
    for job in jobs {
        let (fraction, readout) = if job.total > 0 {
            let f = (job.done as f32 / job.total as f32).clamp(0.0, 1.0);
            (Some(f), format!("{}%", (f * 100.0).round() as u32))
        } else {
            (None, job.stage.clone())
        };
        ui.horizontal(|ui| {
            // Leave room for the Cancel button (a bare row fills the width).
            let room = if job.cancellable { 80.0 } else { 0.0 };
            let width = (ui.available_width() - room).max(60.0);
            ui.allocate_ui(egui::vec2(width, 0.0), |ui| {
                progress(ui, &job.label, fraction, &readout);
            });
            if job.cancellable
                && ui
                    .add(MegaButton::new(ButtonKind::Quiet, "Cancel"))
                    .clicked()
            {
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
    fn job_panel_readout_is_percentage() {
        let jobs = [JobView {
            id: 1,
            label: "Blend".into(),
            stage: "Seams".into(),
            done: 3,
            total: 10,
            cancellable: false,
        }];
        let mut harness = Harness::new_ui(|ui| {
            if crate::testutil::themed(ui) {
                job_panel(ui, &jobs);
            }
        });
        harness.run();
        harness.get_by_label("Blend");
        harness.get_by_label("30%");
    }

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
        let mut harness = Harness::new_ui(|ui| {
            if crate::testutil::themed(ui) {
                actions.extend(job_panel(ui, &jobs));
            }
        });
        harness.run();
        harness.get_by_label("Cancel").click();
        harness.run();
        drop(harness);
        assert_eq!(actions, vec![JobAction::Cancel(7)]);
    }
}
