//! The recent-sessions library: what the start screen lists, with pins.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How many recent sessions are remembered.
pub const MAX_RECENT: usize = 100;

/// A recently opened session, persisted through eframe storage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentEntry {
    /// The session folder.
    pub path: PathBuf,
    /// When it was last opened, in seconds since the Unix epoch.
    pub last_opened: u64,
    /// Pinned sessions lead the shelf and survive the list limit.
    #[serde(default)]
    pub pinned: bool,
}

/// The remembered sessions. Entries are kept as recorded (most recently
/// opened first); the persisted form is a plain `Vec<RecentEntry>`.
#[derive(Clone, Debug, Default)]
pub struct Library {
    entries: Vec<RecentEntry>,
}

impl Library {
    /// A library over stored `entries`, trimmed to [`MAX_RECENT`].
    pub fn new(entries: Vec<RecentEntry>) -> Self {
        let mut library = Library { entries };
        library.trim();
        library
    }

    /// Entries for the shelf: pinned first, then newest opened first.
    #[allow(dead_code)] // used by the start screen rewrite
    pub fn shelf_order(&self) -> Vec<&RecentEntry> {
        let mut v: Vec<&RecentEntry> = self.entries.iter().collect();
        v.sort_by_key(|e| (!e.pinned, std::cmp::Reverse(e.last_opened)));
        v
    }

    /// Entries newest opened first, ignoring pins (for choosing the hero).
    #[allow(dead_code)] // used by the start screen rewrite
    pub fn by_recency(&self) -> Vec<&RecentEntry> {
        let mut v: Vec<&RecentEntry> = self.entries.iter().collect();
        v.sort_by_key(|e| std::cmp::Reverse(e.last_opened));
        v
    }

    /// Put `path` at the top, opened at `now` (seconds since the Unix
    /// epoch). An existing entry keeps its pin.
    pub fn record_opened(&mut self, path: &Path, now: u64) {
        let pinned = self
            .entries
            .iter()
            .find(|e| e.path == path)
            .is_some_and(|e| e.pinned);
        self.entries.retain(|e| e.path != path);
        self.entries.insert(
            0,
            RecentEntry {
                path: path.to_path_buf(),
                last_opened: now,
                pinned,
            },
        );
        self.trim();
    }

    /// Pin or unpin `path`, if listed.
    #[allow(dead_code)] // used by the start screen rewrite
    pub fn set_pinned(&mut self, path: &Path, pinned: bool) {
        if let Some(e) = self.entries.iter_mut().find(|e| e.path == path) {
            e.pinned = pinned;
        }
    }

    /// Forget `path`.
    pub fn remove(&mut self, path: &Path) {
        self.entries.retain(|e| e.path != path);
    }

    /// Point the entry for `old` at `new`, keeping its pin and time. Any
    /// other entry already listed at `new` is dropped.
    #[allow(dead_code)] // used by the start screen rewrite
    pub fn relocate(&mut self, old: &Path, new: &Path) {
        if old == new || !self.entries.iter().any(|e| e.path == old) {
            return;
        }
        self.entries.retain(|e| e.path != new);
        if let Some(e) = self.entries.iter_mut().find(|e| e.path == old) {
            e.path = new.to_path_buf();
        }
    }

    /// The entries as stored, for persisting.
    pub fn entries(&self) -> &[RecentEntry] {
        &self.entries
    }

    /// Over the limit, drop the oldest unpinned entries.
    fn trim(&mut self) {
        while self.entries.len() > MAX_RECENT {
            let Some(i) = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| !e.pinned)
                .min_by_key(|(_, e)| e.last_opened)
                .map(|(i, _)| i)
            else {
                break;
            };
            self.entries.remove(i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, t: u64, pinned: bool) -> RecentEntry {
        RecentEntry {
            path: PathBuf::from(name),
            last_opened: t,
            pinned,
        }
    }

    #[test]
    fn old_storage_without_pins_loads() {
        let v: Vec<RecentEntry> =
            serde_json::from_str(r#"[{"path":"a","last_opened":5}]"#).unwrap();
        assert_eq!(v, vec![e("a", 5, false)]);
    }

    #[test]
    fn pinned_sessions_lead_the_shelf() {
        let lib = Library::new(vec![
            e("new", 9, false),
            e("old", 1, true),
            e("mid", 5, false),
        ]);
        let shelf: Vec<_> = lib.shelf_order().iter().map(|x| x.path.clone()).collect();
        assert_eq!(shelf, ["old", "new", "mid"].map(PathBuf::from));
        assert_eq!(lib.by_recency()[0].path, PathBuf::from("new"));
    }

    #[test]
    fn limit_drops_oldest_unpinned() {
        let mut v = vec![e("pinned-oldest", 1, true), e("unpinned-oldest", 2, false)];
        v.extend((3..=101).map(|i| e(&format!("s{i}"), i, false)));
        assert_eq!(v.len(), 101);
        let lib = Library::new(v);
        assert_eq!(lib.entries().len(), MAX_RECENT);
        assert!(
            lib.entries()
                .iter()
                .any(|x| x.path == Path::new("pinned-oldest"))
        );
        assert!(
            !lib.entries()
                .iter()
                .any(|x| x.path == Path::new("unpinned-oldest"))
        );
    }

    #[test]
    fn relocate_keeps_pin_and_time() {
        let mut lib = Library::new(vec![e("a", 7, true), e("b", 3, false)]);
        lib.relocate(Path::new("a"), Path::new("c"));
        assert_eq!(lib.entries(), &[e("c", 7, true), e("b", 3, false)]);
    }
}
