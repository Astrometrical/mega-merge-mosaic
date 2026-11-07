//! Session lifecycle: opening and creating a mosaic session off the UI
//! thread, and handing a second session to a new process (one session per
//! window, spec §3).

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use mmm_core::mosaic::grouping::Groups;
use mmm_core::mosaic::{
    CONFIG_FILE, FRAME_FILE, Mosaic, MosaicConfig, SessionLock, busy_message, is_mosaic_dir,
    lock_session,
};
use mmm_core::reference::ReferenceFrame;
use mmm_core::session_summary::{SessionSummary, read_summary};
use serde_json::Value;

/// A mosaic session open in this window.
#[derive(Debug)]
pub struct OpenSession {
    /// The mosaic: its directory and configuration (`mosaic.json`).
    pub mosaic: Mosaic,
    /// The analysed frame (`reference.mmm-frame.json`): the frame of the
    /// last analysis, read when opening and updated by the job runner after
    /// each analysis. Previews keep their own frame (see
    /// `mmm_core::preview::preview_frame_path`); ROIs and blends use this
    /// one.
    pub frame: Option<ReferenceFrame>,
    /// The groups as last resolved (when opening, off the UI thread, and by
    /// each analysis); `None` if resolution failed.
    pub groups: Option<Groups>,
    /// The cross-process session lock, held for the session's lifetime;
    /// `None` only for a check made to hand the session to a new process.
    #[allow(dead_code)] // held for its Drop, never read
    pub lock: Option<SessionLock>,
    /// The session's `summary.json` as found when opening (`None` if absent
    /// or unreadable); seeds the summary writer's blend and export stamps.
    pub summary: Option<SessionSummary>,
}

impl OpenSession {
    /// The display name: the folder name without `.mmm-session`.
    pub fn name(&self) -> String {
        session_name(&self.mosaic.dir)
    }

    /// Canvas `(width, height)` of the analysed frame, once an analysis has
    /// derived one.
    pub fn canvas(&self) -> Option<(u64, u64)> {
        self.frame.as_ref().map(ReferenceFrame::canvas)
    }

    /// The number of processing groups: from the resolved groups, else the
    /// groups known from analysis.
    pub fn group_count(&self) -> usize {
        match &self.groups {
            Some(g) => g.processing_groups().len(),
            None => self.mosaic.config.group_dirs.len(),
        }
    }
}

/// The extension of a session folder (`<name>.mmm-session`).
pub const SESSION_EXT: &str = "mmm-session";

/// The display name of the session folder `dir`: its file name without
/// `.mmm-session`.
pub fn session_name(dir: &Path) -> String {
    let file = dir
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string());
    match file.strip_suffix(&format!(".{SESSION_EXT}")) {
        Some(stem) if !stem.is_empty() => stem.to_string(),
        _ => file,
    }
}

/// What to do with a session folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Open an existing session.
    Open(PathBuf),
    /// Create a new, empty session.
    Create(PathBuf),
}

impl Request {
    /// The session folder the request is about.
    pub fn path(&self) -> &Path {
        match self {
            Request::Open(p) | Request::Create(p) => p,
        }
    }
}

/// `path` made absolute, so recent entries and the "already open" check
/// compare like with like. The longest existing prefix is canonicalised
/// (symlinks and `..` resolved); a path that cannot be resolved at all is
/// made absolute lexically. Blocking (file I/O): call off the UI thread.
pub fn normalize(path: &Path) -> PathBuf {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    if let Ok(c) = abs.canonicalize() {
        return c;
    }
    // Not there yet (a session being created): resolve the parent.
    match (abs.parent(), abs.file_name()) {
        (Some(parent), Some(name)) => match parent.canonicalize() {
            Ok(p) => p.join(name),
            Err(_) => abs,
        },
        _ => abs,
    }
}

/// Open the session in `dir`. Blocking (file I/O): call off the UI thread.
/// Errors are actionable messages for the start screen.
#[cfg(test)]
pub fn open_blocking(dir: &Path) -> Result<OpenSession, String> {
    open_with(dir, true)
}

/// Open `dir`; `lock` false skips the session lock, for checking a session
/// before handing it to a new process (which takes the lock itself).
fn open_with(dir: &Path, lock: bool) -> Result<OpenSession, String> {
    let dir = &normalize(dir);
    let shown = dir.display();
    if !dir.exists() {
        return Err(format!("{shown} does not exist."));
    }
    if !dir.is_dir() {
        return Err(format!(
            "{shown} is not a folder. Choose a .{SESSION_EXT} folder."
        ));
    }
    if !is_mosaic_dir(dir) {
        if dir.join("session.json").is_file() {
            return Err(format!(
                "{shown} is a single-group session from an earlier mmm (it has \
                 session.json but no {CONFIG_FILE}). Create a mosaic session from its \
                 panels with `mmm init PANELS… -s NAME.{SESSION_EXT}`, then open that."
            ));
        }
        return Err(format!(
            "{shown} is not a Mega Merge Mosaic session (no {CONFIG_FILE}). Choose a \
             .{SESSION_EXT} folder, or start a new one with New mosaic…"
        ));
    }
    let lock = if lock { Some(take_lock(dir)?) } else { None };
    let mosaic = Mosaic::open(dir).map_err(|e| format!("Cannot open {shown}: {e}"))?;
    // A missing or unreadable frame only means nothing analysed yet.
    let frame = ReferenceFrame::load(&dir.join(FRAME_FILE)).ok();
    // Reads panel headers (Header rule): fine here, off the UI thread.
    let groups = mosaic.groups().ok();
    let summary = read_summary(dir);
    Ok(OpenSession {
        mosaic,
        frame,
        groups,
        lock,
        summary,
    })
}

/// The session lock on `dir`, or an actionable message.
fn take_lock(dir: &Path) -> Result<SessionLock, String> {
    lock_session(dir).map_err(|e| {
        if e.is_busy() {
            busy_message(dir)
        } else {
            format!("Cannot lock {}: {e}", dir.display())
        }
    })
}

/// Create a new session in `dir` with an empty [`MosaicConfig`]. Blocking
/// (file I/O): call off the UI thread. Refuses an existing `dir`.
pub fn create_blocking(dir: &Path) -> Result<OpenSession, String> {
    let dir = &normalize(dir);
    let shown = dir.display();
    if dir.exists() {
        return Err(format!(
            "{shown} already exists. Choose another name, or Open… it."
        ));
    }
    let mosaic = Mosaic::create(dir, MosaicConfig::new(Vec::new()))
        .map_err(|e| format!("Cannot create {shown}: {e}"))?;
    let lock = Some(take_lock(dir)?);
    Ok(OpenSession {
        mosaic,
        frame: None,
        groups: Some(Groups::default()),
        lock,
        summary: None,
    })
}

/// A session being opened or created on a background thread.
pub struct Pending {
    /// What was asked for.
    pub request: Request,
    rx: Receiver<Result<OpenSession, String>>,
}

impl Pending {
    /// Start `request` on a background thread; `ctx` is repainted when it
    /// finishes.
    ///
    /// `keep_lock` is false when the session is only being checked before a
    /// new process opens it (the child takes the lock; ours would refuse it).
    pub fn spawn(request: Request, keep_lock: bool, ctx: &egui::Context) -> Self {
        let (tx, rx) = mpsc::channel();
        let req = request.clone();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("mmm-session-open".into())
            .spawn(move || {
                let outcome = match &req {
                    Request::Open(dir) => open_with(dir, keep_lock),
                    Request::Create(dir) => create_blocking(dir),
                };
                let _ = tx.send(outcome);
                ctx.request_repaint();
            })
            .map(|_| ())
            .unwrap_or_else(|e| tracing::error!("cannot start the session thread: {e}"));
        Pending { request, rx }
    }

    /// The outcome, once the background work has finished.
    pub fn poll(&mut self) -> Option<Result<OpenSession, String>> {
        match self.rx.try_recv() {
            Ok(outcome) => Some(outcome),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(format!(
                "Opening {} failed unexpectedly (see the log).",
                self.request.path().display()
            ))),
        }
    }
}

/// Open `session` in a new window: a new process of this executable (one
/// session per window, spec §3); with `None` it starts on the start
/// screen. The child is reaped on a helper thread.
pub fn open_in_new_process(session: Option<&Path>) -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let mut command = std::process::Command::new(exe);
    command.args(session);
    let mut child = command.spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// The key in a session's `ui` JSON holding its free-text notes.
pub const NOTES_KEY: &str = "notes";

/// The notes stored in `ui` (empty when there are none).
pub fn notes(ui: &Value) -> String {
    ui.get(NOTES_KEY)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Store `text` as the notes in `ui`, keeping everything else there (a `ui`
/// that is not a JSON object becomes one).
pub fn set_notes(ui: &mut Value, text: &str) {
    if !ui.is_object() {
        *ui = Value::Object(Default::default());
    }
    if let Some(obj) = ui.as_object_mut() {
        obj.insert(NOTES_KEY.to_string(), Value::String(text.to_string()));
    }
}

/// Save `text` as the notes of the session in `dir`, without opening it:
/// takes the session lock for the write only. A session open elsewhere is
/// refused (`Err(busy_message)`) so its configuration is never overwritten.
/// Blocking file I/O: call from a background thread.
pub fn save_notes_blocking(dir: &Path, text: &str) -> Result<(), String> {
    let _lock = lock_session(dir).map_err(|e| {
        if e.is_busy() {
            busy_message(dir)
        } else {
            e.to_string()
        }
    })?;
    let mut cfg = MosaicConfig::load(dir).map_err(|e| e.to_string())?;
    set_notes(&mut cfg.ui, text);
    cfg.save(dir).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::start::tests::{relative_to_cwd, temp_dir, temp_dir_near_cwd};

    #[test]
    fn create_then_open_roundtrip() {
        let root = temp_dir("roundtrip");
        let dir = root.join("Orion.mmm-session");
        let created = create_blocking(&dir).unwrap();
        assert_eq!(created.name(), "Orion");
        assert!(dir.join("mosaic.json").is_file());
        drop(created);
        let opened = open_blocking(&dir).unwrap();
        assert!(opened.mosaic.config.panels.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn open_refused_while_locked() {
        let root = temp_dir("locked");
        let dir = root.join("L.mmm-session");
        let created = create_blocking(&dir).unwrap();
        let err = open_blocking(&dir).unwrap_err();
        assert!(err.contains("open in another window or process"), "{err}");
        drop(created);
        let opened = open_blocking(&dir).unwrap();
        // A check without the lock (for a hand-off) leaves it free.
        assert!(open_with(&dir, false).unwrap().lock.is_none());
        drop(opened);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn create_refuses_existing_folder() {
        let root = temp_dir("exists");
        let err = create_blocking(&root).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        assert!(!root.join("mosaic.json").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn legacy_session_suggests_mmm_init() {
        let root = temp_dir("session-legacy");
        std::fs::write(root.join("session.json"), "{}").unwrap();
        let err = open_blocking(&root).unwrap_err();
        assert!(err.contains("mmm init"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn relative_paths_become_absolute() {
        let root = temp_dir_near_cwd("relative");
        let dir = root.join("Rel.mmm-session");
        let rel = relative_to_cwd(&dir);
        assert!(rel.is_relative());
        assert_eq!(create_blocking(&rel).unwrap().mosaic.dir, dir);
        assert_eq!(open_blocking(&rel).unwrap().mosaic.dir, dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_folder_is_reported() {
        let root = temp_dir("missing");
        let err = open_blocking(&root.join("nope.mmm-session")).unwrap_err();
        assert!(err.contains("does not exist"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
