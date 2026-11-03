//! The Blend tab's export dialog: write the view's groups' blended results
//! to FITS or TIFF files (`mmm_core::export`).
//!
//! The dialog chooses the groups (those of the current view), the format
//! and the destination: a typed path or the native save dialog
//! ([`SavePicker`], run on a background thread). With several groups the
//! destination names a pattern: each group goes to `<stem>-<group>.<ext>`.
//!
//! **Export** first counts each group's stale and missing tiles
//! ([`export_check`], on a background thread: it maps the stores), and
//! looks for destination files that exist already. It is disabled while a
//! blend of a chosen group is queued or running (the blend may recreate
//! the store the check maps). The groups, destinations and format are
//! taken when Export is pressed; later edits in the dialog do not change
//! that export. Existing destinations are listed and need an explicit
//! **Overwrite**. A complete result is then exported at once; otherwise
//! the dialog warns with the counts and offers **Blend whole mosaic
//! first** (blends the chosen groups over the whole canvas; export again
//! once it is done) or **Export anyway** (stale tiles as they are, missing
//! ones as zeros). The exports themselves run as jobs
//! ([`crate::jobs::JobRunner::start_export`]) with progress and cancel.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::RichText;
use mega_ui::theme::palette;
pub use mmm_core::export::ExportFormat;
use mmm_core::export::{ExportReport, export_check};

use crate::blend_tab::{Poll, Task};
use crate::session::OpenSession;

/// Asks where to save an export. Called on a background thread.
pub trait SavePicker: Send + Sync {
    /// Show a save dialog for a `format` file suggesting `name`; `None`
    /// when cancelled or when no dialog is available.
    fn pick_save(&self, format: ExportFormat, name: &str) -> Option<PathBuf>;
}

/// The native save dialog through `rfd`.
pub struct RfdSavePicker;

impl SavePicker for RfdSavePicker {
    fn pick_save(&self, format: ExportFormat, name: &str) -> Option<PathBuf> {
        let extensions: &[&str] = match format {
            ExportFormat::Fits => &["fits", "fit", "fts"],
            ExportFormat::Tiff => &["tif", "tiff"],
        };
        rfd::FileDialog::new()
            .set_title("Export the blended result")
            .add_filter(format.name(), extensions)
            .set_file_name(name)
            .save_file()
    }
}

/// What the user decided in the dialog.
#[derive(Clone, Debug, PartialEq)]
pub enum ExportAction {
    /// Blend these groups over the whole canvas (no export).
    BlendFirst(Vec<String>),
    /// Export each group to its file.
    Export(Vec<(String, PathBuf)>, ExportFormat),
}

/// What a check found: each chosen group's tile counts and the
/// destinations that exist already.
struct Checked {
    reports: Vec<(String, ExportReport)>,
    existing: Vec<PathBuf>,
}

/// What a check found, or why the results could not be read.
type CheckResult = Result<Checked, String>;

/// The export as chosen when Export was pressed: each group's destination
/// and the format.
#[derive(Clone, Debug)]
struct Snapshot {
    dests: Vec<(String, PathBuf)>,
    format: ExportFormat,
}

impl Snapshot {
    fn action(&self) -> ExportAction {
        ExportAction::Export(self.dests.clone(), self.format)
    }
}

/// The answer to the stale/missing warning or the overwrite question.
enum Choice {
    BlendFirst,
    ExportAnyway,
    Overwrite,
    Back,
}

/// Where the dialog is.
enum Step {
    /// Choosing groups, format and destination.
    Choose,
    /// Counting stale and missing tiles, and looking for existing files.
    Checking(Task<CheckResult>, Snapshot),
    /// Some destinations exist: overwrite them? Then the counts decide.
    Overwrite(Vec<PathBuf>, Vec<(String, ExportReport)>, Snapshot),
    /// The result is incomplete: the counts, per group.
    Warn(Vec<(String, ExportReport)>, Snapshot),
}

/// The export dialog's state.
pub struct ExportDialog {
    open: bool,
    /// The view's groups, and whether each is chosen.
    groups: Vec<(String, bool)>,
    format: ExportFormat,
    /// The destination, as typed (or picked).
    path: String,
    picking: Option<Task<Option<PathBuf>>>,
    step: Step,
    error: Option<String>,
    picker: Arc<dyn SavePicker>,
}

impl ExportDialog {
    /// A closed dialog saving through `picker`.
    pub fn new(picker: Arc<dyn SavePicker>) -> Self {
        ExportDialog {
            open: false,
            groups: Vec::new(),
            format: ExportFormat::Fits,
            path: String::new(),
            picking: None,
            step: Step::Choose,
            error: None,
            picker,
        }
    }

    /// Save through `picker` from now on.
    #[cfg(test)]
    pub fn set_picker(&mut self, picker: Arc<dyn SavePicker>) {
        self.picker = picker;
    }

    /// Open the dialog for `groups` (all chosen). The destination keeps
    /// what was typed last; at first it suggests a file beside the session
    /// folder. No I/O.
    pub fn open(&mut self, session: &OpenSession, groups: Vec<String>) {
        if self.path.trim().is_empty() {
            let dir = session.mosaic.dir.parent().unwrap_or(&session.mosaic.dir);
            self.path = dir
                .join(suggested_name(&groups, self.format))
                .display()
                .to_string();
        }
        self.groups = groups.into_iter().map(|g| (g, true)).collect();
        self.step = Step::Choose;
        self.error = None;
        self.open = true;
    }

    /// Whether the dialog is shown.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The destination as typed.
    #[cfg(test)]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Set the destination as if typed.
    #[cfg(test)]
    pub fn set_path(&mut self, path: &str) {
        self.path = path.to_string();
    }

    /// Set the format as if chosen.
    #[cfg(test)]
    pub fn set_format(&mut self, format: ExportFormat) {
        self.format = format;
    }

    /// Whether the overwrite confirmation is shown.
    #[cfg(test)]
    pub fn overwrite_shown(&self) -> bool {
        matches!(self.step, Step::Overwrite(..))
    }

    /// Whether the stale/missing warning is shown.
    #[cfg(test)]
    pub fn warning_shown(&self) -> bool {
        matches!(self.step, Step::Warn(..))
    }

    /// Nothing pending (no save dialog or check running).
    pub fn idle(&self) -> bool {
        self.picking.is_none() && !matches!(self.step, Step::Checking(..))
    }

    /// The groups chosen, in order.
    fn chosen(&self) -> Vec<String> {
        self.groups
            .iter()
            .filter(|(_, on)| *on)
            .map(|(g, _)| g.clone())
            .collect()
    }

    /// Draw the dialog (if open); the user's decision, once made.
    /// `can_blend` enables "Blend whole mosaic first"; `blending` are the
    /// groups with a blend queued or running (Export waits for them).
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        session: &OpenSession,
        can_blend: bool,
        blending: &BTreeSet<String>,
    ) -> Option<ExportAction> {
        if !self.open {
            return None;
        }
        self.poll_picker();
        let mut action = self.poll_check();
        let mut open = true;
        egui::Window::new("Export")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                let decided = match &self.step {
                    Step::Warn(reports, snap) => {
                        let (reports, snap) = (reports.clone(), snap.clone());
                        match warn_ui(ui, &reports, can_blend) {
                            Some(Choice::BlendFirst) => Some(ExportAction::BlendFirst(
                                reports.into_iter().map(|(g, _)| g).collect(),
                            )),
                            Some(Choice::ExportAnyway) => Some(snap.action()),
                            Some(Choice::Back) => {
                                self.step = Step::Choose;
                                None
                            }
                            Some(Choice::Overwrite) | None => None,
                        }
                    }
                    Step::Overwrite(existing, reports, snap) => match overwrite_ui(ui, existing) {
                        Some(Choice::Overwrite) => {
                            let (reports, snap) = (reports.clone(), snap.clone());
                            self.decide(reports, snap)
                        }
                        Some(Choice::Back) => {
                            self.step = Step::Choose;
                            None
                        }
                        Some(Choice::BlendFirst | Choice::ExportAnyway) | None => None,
                    },
                    Step::Choose | Step::Checking(..) => {
                        self.choose_ui(ui, session, blending);
                        None
                    }
                };
                action = action.take().or(decided);
            });
        if !open {
            self.close();
        }
        if action.is_some() {
            self.close();
        }
        action
    }

    fn close(&mut self) {
        self.open = false;
        self.step = Step::Choose;
    }

    /// Take the save dialog's answer.
    fn poll_picker(&mut self) {
        let Some(task) = &self.picking else {
            return;
        };
        match task.poll() {
            Poll::Pending => {}
            Poll::Done(picked) => {
                self.picking = None;
                if let Some(p) = picked {
                    if let Some(f) = format_of(&p) {
                        self.format = f;
                    }
                    self.path = p.display().to_string();
                }
            }
            Poll::Lost => {
                self.picking = None;
                self.error = Some("The save dialog could not be shown.".into());
            }
        }
    }

    /// Take the check's answer: confirm overwriting existing files first,
    /// then [`ExportDialog::decide`].
    fn poll_check(&mut self) -> Option<ExportAction> {
        let Step::Checking(task, snap) = &self.step else {
            return None;
        };
        match task.poll() {
            Poll::Pending => None,
            Poll::Done(Ok(checked)) => {
                let snap = snap.clone();
                if checked.existing.is_empty() {
                    self.decide(checked.reports, snap)
                } else {
                    self.step = Step::Overwrite(checked.existing, checked.reports, snap);
                    None
                }
            }
            Poll::Done(Err(e)) => {
                self.step = Step::Choose;
                self.error = Some(e);
                None
            }
            Poll::Lost => {
                self.step = Step::Choose;
                self.error = Some("Checking the results failed (see the log).".into());
                None
            }
        }
    }

    /// Export at once when every result is complete, else warn.
    fn decide(
        &mut self,
        reports: Vec<(String, ExportReport)>,
        snap: Snapshot,
    ) -> Option<ExportAction> {
        if reports.iter().all(|(_, r)| r.is_complete()) {
            self.step = Step::Choose;
            Some(snap.action())
        } else {
            self.step = Step::Warn(reports, snap);
            None
        }
    }

    /// Groups, format, destination and the Export button (which starts
    /// the check; disabled while a chosen group in `blending` blends).
    fn choose_ui(&mut self, ui: &mut egui::Ui, session: &OpenSession, blending: &BTreeSet<String>) {
        let checking = matches!(self.step, Step::Checking(..));
        ui.label(RichText::new("Groups").strong());
        for (g, on) in &mut self.groups {
            ui.checkbox(on, g.as_str());
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Format").strong());
            let before = self.format;
            ui.radio_value(&mut self.format, ExportFormat::Fits, "FITS");
            ui.radio_value(&mut self.format, ExportFormat::Tiff, "TIFF");
            if self.format != before {
                self.path = swap_extension(&self.path, before, self.format);
            }
        });
        ui.label(
            RichText::new(match self.format {
                ExportFormat::Fits => "32-bit float, linear, with WCS and provenance.",
                ExportFormat::Tiff => "32-bit float, linear (values are not clamped).",
            })
            .small()
            .color(palette::TEXT_MUTED),
        );
        ui.add_space(8.0);
        let label = ui.label(RichText::new("Destination").strong());
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.path)
                    .desired_width(420.0)
                    .hint_text("/path/to/mosaic.fits"),
            )
            .labelled_by(label.id);
            if ui
                .add_enabled(self.picking.is_none(), egui::Button::new("Browse…"))
                .clicked()
            {
                let picker = self.picker.clone();
                let format = self.format;
                let name = Path::new(self.path.trim())
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| suggested_name(&self.chosen(), format));
                self.picking = Task::spawn("mmm-export-picker", ui.ctx(), move || {
                    picker.pick_save(format, &name)
                });
            }
        });
        let chosen = self.chosen();
        if chosen.len() > 1 {
            ui.label(
                RichText::new("Each group is written to <name>-<group>.<extension>.")
                    .small()
                    .color(palette::TEXT_MUTED),
            );
        }
        if let Some(e) = &self.error {
            ui.label(RichText::new(e).color(palette::HA_500));
        }
        let busy: Vec<&String> = chosen.iter().filter(|g| blending.contains(*g)).collect();
        if !busy.is_empty() {
            let names: Vec<&str> = busy.iter().map(|g| g.as_str()).collect();
            ui.label(
                RichText::new(format!(
                    "Waiting for the blend of {} to end.",
                    names.join(", ")
                ))
                .color(palette::TEXT_MUTED),
            );
        }
        ui.add_space(8.0);
        let ready =
            !chosen.is_empty() && !self.path.trim().is_empty() && !checking && busy.is_empty();
        let mut clicked = false;
        ui.horizontal(|ui| {
            clicked = ui.add_enabled(ready, egui::Button::new("Export")).clicked();
            if checking {
                ui.spinner();
                ui.label(RichText::new("Checking the results…").color(palette::TEXT_MUTED));
            }
        });
        if clicked && ready {
            self.start_check(ui.ctx(), session, chosen);
        }
    }

    /// Take the chosen groups, destinations and format, then count the
    /// groups' stale and missing tiles on a worker (it maps the stores);
    /// the destination folder and existing destination files are checked
    /// there too.
    fn start_check(&mut self, ctx: &egui::Context, session: &OpenSession, chosen: Vec<String>) {
        self.error = None;
        let config = &session.mosaic.config;
        if let Some(g) = chosen.iter().find(|g| !config.group_dirs.contains_key(*g)) {
            self.error = Some(format!("{g} has not been analysed and blended yet."));
            return;
        }
        let snap = Snapshot {
            dests: targets(
                Path::new(self.path.trim()),
                &chosen,
                self.format,
                &config.group_dirs,
            ),
            format: self.format,
        };
        let results: Vec<(String, PathBuf)> = chosen
            .iter()
            .map(|g| (g.clone(), session.mosaic.result_path(g)))
            .collect();
        let folder = Path::new(self.path.trim())
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let dests: Vec<PathBuf> = snap.dests.iter().map(|(_, p)| p.clone()).collect();
        let task = Task::spawn("mmm-export-check", ctx, move || {
            if !folder.as_os_str().is_empty() && !folder.is_dir() {
                return Err(format!("The folder {} does not exist.", folder.display()));
            }
            let reports = results
                .into_iter()
                .map(|(g, path)| {
                    export_check(&path)
                        .map(|r| (g.clone(), r))
                        .map_err(|e| format!("{g}: {e}"))
                })
                .collect::<Result<Vec<_>, String>>()?;
            let existing = dests.into_iter().filter(|p| p.exists()).collect();
            Ok(Checked { reports, existing })
        });
        match task {
            Some(t) => self.step = Step::Checking(t, snap),
            None => self.error = Some("Cannot start a worker thread.".into()),
        }
    }
}

/// The overwrite question: the existing files and the user's choice.
fn overwrite_ui(ui: &mut egui::Ui, existing: &[PathBuf]) -> Option<Choice> {
    ui.label(
        RichText::new(if existing.len() == 1 {
            "This file exists already:"
        } else {
            "These files exist already:"
        })
        .strong()
        .color(palette::HA_500),
    );
    for p in existing {
        ui.label(p.display().to_string());
    }
    ui.label(RichText::new("Exporting replaces them.").color(palette::TEXT_MUTED));
    ui.add_space(8.0);
    let mut choice = None;
    ui.horizontal(|ui| {
        if ui.button("Overwrite").clicked() {
            choice = Some(Choice::Overwrite);
        }
        if ui.button("Back").clicked() {
            choice = Some(Choice::Back);
        }
    });
    choice
}

/// The stale/missing warning and the user's choice.
fn warn_ui(
    ui: &mut egui::Ui,
    reports: &[(String, ExportReport)],
    can_blend: bool,
) -> Option<Choice> {
    ui.label(
        RichText::new("The blended result is not up to date.")
            .strong()
            .color(palette::HA_500),
    );
    for (g, r) in reports.iter().filter(|(_, r)| !r.is_complete()) {
        ui.label(counts_text(g, r));
    }
    ui.label(
        RichText::new("Stale tiles are exported as they are; missing tiles are exported as zeros.")
            .color(palette::TEXT_MUTED),
    );
    ui.add_space(8.0);
    let mut choice = None;
    ui.horizontal(|ui| {
        if ui
            .add_enabled(can_blend, egui::Button::new("Blend whole mosaic first"))
            .on_hover_text("Blend these groups over the whole canvas, then export again")
            .on_disabled_hover_text("Blending is not possible right now")
            .clicked()
        {
            choice = Some(Choice::BlendFirst);
        }
        if ui.button("Export anyway").clicked() {
            choice = Some(Choice::ExportAnyway);
        }
        if ui.button("Back").clicked() {
            choice = Some(Choice::Back);
        }
    });
    choice
}

/// The file name suggested for `groups`: the group's name for one, else
/// `mosaic`.
fn suggested_name(groups: &[String], format: ExportFormat) -> String {
    let stem = match groups {
        [g] => g
            .chars()
            .map(|c| {
                if matches!(c, '/' | '\\' | ':') {
                    '_'
                } else {
                    c
                }
            })
            .collect(),
        _ => "mosaic".to_string(),
    };
    format!("{stem}.{}", format.extension())
}

/// The format a file name's extension names, if any.
fn format_of(path: &Path) -> Option<ExportFormat> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "fits" | "fit" | "fts" => Some(ExportFormat::Fits),
        "tif" | "tiff" | "btf" | "tf8" => Some(ExportFormat::Tiff),
        _ => None,
    }
}

/// `path` with its extension changed to `to`'s when it is one of `from`'s.
fn swap_extension(path: &str, from: ExportFormat, to: ExportFormat) -> String {
    let p = Path::new(path.trim());
    if format_of(p) == Some(from) {
        p.with_extension(to.extension()).display().to_string()
    } else {
        path.to_string()
    }
}

/// Each group's destination: `path` itself for one group, else
/// `<stem>-<group dir>.<ext>` beside it. A path without an extension gets
/// the format's.
fn targets(
    path: &Path,
    groups: &[String],
    format: ExportFormat,
    group_dirs: &std::collections::BTreeMap<String, String>,
) -> Vec<(String, PathBuf)> {
    let path = if path.extension().is_some() {
        path.to_path_buf()
    } else {
        path.with_extension(format.extension())
    };
    if let [g] = groups {
        return vec![(g.clone(), path)];
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    groups
        .iter()
        .map(|g| {
            let tag = group_dirs.get(g).unwrap_or(g);
            (
                g.clone(),
                path.with_file_name(format!("{stem}-{tag}.{ext}")),
            )
        })
        .collect()
}

/// "Ha: 2 stale tile(s), 5 missing tile(s)" (non-zero counts only).
fn counts_text(group: &str, r: &ExportReport) -> String {
    let mut parts = Vec::new();
    if r.stale_tiles > 0 {
        parts.push(format!("{} stale tile(s)", r.stale_tiles));
    }
    if r.missing_tiles > 0 {
        parts.push(format!("{} missing tile(s)", r.missing_tiles));
    }
    format!("{group}: {}", parts.join(", "))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn destinations_per_group_and_extension_follows_format() {
        let dirs = BTreeMap::from([("H a".to_string(), "H_a".to_string())]);
        let one = targets(
            Path::new("/x/out"),
            &["Ha".into()],
            ExportFormat::Tiff,
            &dirs,
        );
        assert_eq!(one, [("Ha".to_string(), PathBuf::from("/x/out.tif"))]);
        let two = targets(
            Path::new("/x/m.fits"),
            &["H a".into(), "OIII".into()],
            ExportFormat::Fits,
            &dirs,
        );
        assert_eq!(
            two,
            [
                ("H a".to_string(), PathBuf::from("/x/m-H_a.fits")),
                ("OIII".to_string(), PathBuf::from("/x/m-OIII.fits")),
            ]
        );
        assert_eq!(
            swap_extension("/x/m.fits", ExportFormat::Fits, ExportFormat::Tiff),
            "/x/m.tif"
        );
        assert_eq!(
            swap_extension("/x/m.dat", ExportFormat::Fits, ExportFormat::Tiff),
            "/x/m.dat"
        );
        assert_eq!(format_of(Path::new("a.BTF")), Some(ExportFormat::Tiff));
        assert_eq!(
            suggested_name(&["a/b".into()], ExportFormat::Fits),
            "a_b.fits"
        );
        let r = ExportReport {
            stale_tiles: 2,
            missing_tiles: 5,
            width: 1,
            height: 1,
        };
        assert_eq!(
            counts_text("Ha", &r),
            "Ha: 2 stale tile(s), 5 missing tile(s)"
        );
    }
}
