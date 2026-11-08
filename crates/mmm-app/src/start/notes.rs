//! The session block's notes: read as text, edited in place, saved on a
//! background thread a pause after the last keystroke or at once when
//! editing ends (or the text box loses focus), one save per session at a
//! time (a later one waits its turn).

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use mega_ui::widgets::NotesMode;

use super::StartScreen;
use crate::session::save_notes_blocking;

/// Seconds after the last keystroke before the notes are saved.
const NOTES_SAVE_DELAY_S: f64 = 1.0;

/// How long the app's exit waits for each notes save.
const EXIT_SAVE_WAIT: Duration = Duration::from_secs(2);

/// The notes shown in the session block.
#[derive(Default)]
pub(super) struct NotesEditor {
    /// The session they belong to.
    pub(super) path: Option<PathBuf>,
    /// The notes as edited.
    pub(super) text: String,
    /// The text came from the session's facts (read-only until then).
    pub(super) loaded: bool,
    /// When the last unsaved edit was made (egui time).
    pub(super) edited_at: Option<f64>,
    /// Save without waiting (editing ended, or the text box lost focus).
    pub(super) save_now: bool,
    /// Read (collapsed or expanded) or edited.
    pub(super) mode: NotesMode,
}

/// A notes save running on a background thread.
pub(super) struct NotesSave {
    path: PathBuf,
    text: String,
    rx: Receiver<Result<(), String>>,
}

impl StartScreen {
    /// Save notes still waiting for their pause, and wait (a little) for
    /// saves under way. For the app's exit only: it blocks.
    pub fn flush_notes(&mut self) {
        if self.notes.edited_at.take().is_some()
            && let Some(path) = self.notes.path.clone()
        {
            self.queued_saves.retain(|(p, _)| *p != path);
            self.queued_saves.push((path, self.notes.text.clone()));
        }
        for save in self.saves.drain(..) {
            let _ = save.rx.recv_timeout(EXIT_SAVE_WAIT);
        }
        for (path, text) in self.queued_saves.drain(..) {
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(save_notes_blocking(&path, &text));
            });
            if let Ok(Err(e)) = rx.recv_timeout(EXIT_SAVE_WAIT) {
                tracing::warn!("notes not saved: {e}");
            }
        }
    }

    /// Point the notes editor at `target`: save edits of the session it
    /// showed, then take the new session's notes from its facts (once
    /// they arrive).
    pub(super) fn sync_notes(&mut self, ctx: &egui::Context, target: Option<&Path>) {
        if self.notes.path.as_deref() != target {
            if self.notes.edited_at.is_some()
                && let Some(path) = self.notes.path.clone()
            {
                let text = std::mem::take(&mut self.notes.text);
                self.save_notes(ctx, path, text);
            }
            self.notes = NotesEditor {
                path: target.map(Path::to_path_buf),
                ..NotesEditor::default()
            };
        }
        if !self.notes.loaded
            && let Some(f) = target.and_then(|t| self.facts.get(t))
        {
            self.notes.text = f.notes.clone();
            self.notes.loaded = true;
        }
    }

    /// Save edited notes a pause after the last edit, or at once after the
    /// field lost focus.
    pub(super) fn save_notes_when_due(&mut self, ctx: &egui::Context, now: f64) {
        let Some(at) = self.notes.edited_at else {
            return;
        };
        let wait = NOTES_SAVE_DELAY_S - (now - at);
        if !self.notes.save_now && wait > 0.0 {
            ctx.request_repaint_after(Duration::from_secs_f64(wait));
            return;
        }
        self.notes.edited_at = None;
        self.notes.save_now = false;
        if let Some(path) = self.notes.path.clone() {
            let text = self.notes.text.clone();
            self.save_notes(ctx, path, text);
        }
    }

    /// Save `text` as the notes of `path` on a background thread, after any
    /// save of the same session still running.
    fn save_notes(&mut self, ctx: &egui::Context, path: PathBuf, text: String) {
        if self.saves.iter().any(|s| s.path == path) {
            self.queued_saves.retain(|(p, _)| *p != path);
            self.queued_saves.push((path, text));
            return;
        }
        let (tx, rx) = mpsc::channel();
        let (p, t, ctx) = (path.clone(), text.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(save_notes_blocking(&p, &t));
            ctx.request_repaint();
        });
        self.saves.push(NotesSave { path, text, rx });
    }

    pub(super) fn poll_saves(&mut self, ctx: &egui::Context) {
        let mut done = Vec::new();
        self.saves.retain(|s| {
            let result = match s.rx.try_recv() {
                Ok(r) => r,
                Err(TryRecvError::Disconnected) => Err("the save failed (see the log)".into()),
                Err(TryRecvError::Empty) => return true,
            };
            done.push((s.path.clone(), s.text.clone(), result));
            false
        });
        for (path, text, result) in done {
            match result {
                Ok(()) => {
                    if let Some(f) = self.facts.get_mut(&path) {
                        f.notes = text;
                    }
                }
                Err(e) => {
                    tracing::warn!("notes of {} not saved: {e}", path.display());
                    self.set_error(Some(format!("Notes not saved: {e}")));
                }
            }
            if let Some(i) = self.queued_saves.iter().position(|(p, _)| *p == path) {
                let (path, text) = self.queued_saves.remove(i);
                self.save_notes(ctx, path, text);
            }
        }
    }

    /// Whether notes of `path` are edited but unsaved, or being saved.
    pub(super) fn notes_pending(&self, path: &Path) -> bool {
        (self.notes.path.as_deref() == Some(path) && self.notes.edited_at.is_some())
            || self.saves.iter().any(|s| s.path == path)
            || self.queued_saves.iter().any(|(p, _)| p == path)
    }
}
