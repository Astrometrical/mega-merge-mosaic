//! What a recent-session card shows, gathered off the UI thread: the
//! session's size, groups, state and notes, read without ever taking the
//! session lock.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

use mmm_core::mosaic::{MosaicConfig, is_mosaic_dir, session_busy};
use mmm_core::preview::PREVIEWS_DIR;
use mmm_core::session_summary::{ResultState, SessionSummary, read_summary};

use super::RecentEntry;
use crate::panels_tab::load_group_colors;
use crate::session::{notes, session_name};

/// Whether a recent session can be used right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Availability {
    /// Readable and not open elsewhere.
    Available,
    /// The folder is gone (deleted, or its drive is unplugged).
    Missing,
    /// Open in another window or process; its notes are read-only.
    Busy,
    /// `mosaic.json` is corrupt or from a newer mmm (the message says why).
    Unreadable(String),
}

/// How far a session has got, for the card's status chip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardState {
    /// No panels added yet.
    NoPanels,
    /// Analysed (previews exist) but not merged.
    PlanReady,
    /// Every group merged.
    Merged,
    /// Every group merged and exported since.
    MergedExported,
    /// A group's result is stale or partial.
    ReMergeNeeded,
    /// Nothing known (no summary, no previews).
    Unknown,
}

/// The card state from the panel count, whether previews exist and the
/// session summary (if any).
pub fn card_state(
    panels: usize,
    has_previews: bool,
    summary: Option<&SessionSummary>,
) -> CardState {
    if panels == 0 {
        return CardState::NoPanels;
    }
    if let Some(s) = summary {
        let groups = &s.groups;
        if groups
            .iter()
            .any(|g| matches!(g.result, ResultState::Stale | ResultState::Partial))
        {
            return CardState::ReMergeNeeded;
        }
        if !groups.is_empty() && groups.iter().all(|g| g.result == ResultState::Complete) {
            let exported = groups.iter().all(|g| match (g.blended_at, g.exported_at) {
                (Some(b), Some(e)) => e >= b,
                _ => false,
            });
            return if exported {
                CardState::MergedExported
            } else {
                CardState::Merged
            };
        }
    }
    if has_previews {
        CardState::PlanReady
    } else {
        CardState::Unknown
    }
}

/// Everything a card (and the hero) shows about one recent session.
#[derive(Clone, Debug)]
pub struct CardFacts {
    /// The session folder.
    pub path: PathBuf,
    /// Display name of the session.
    pub name: String,
    /// When it was last opened (unix seconds).
    pub last_opened: u64,
    /// Whether it can be used.
    pub availability: Availability,
    /// Panel count, when the configuration was read.
    pub panels: Option<usize>,
    /// Group names with their panel counts (from the summary; empty without).
    pub groups: Vec<(String, usize)>,
    /// The session's saved group palette indices.
    pub group_colors: BTreeMap<String, usize>,
    /// How far the session has got.
    pub state: CardState,
    /// Check problems, once checked.
    pub problems: Option<usize>,
    /// Check notes, once checked.
    pub notes_count: Option<usize>,
    /// The session's free-text notes.
    pub notes: String,
    /// The session summary, when present and readable.
    pub summary: Option<SessionSummary>,
}

/// Whether `previews/` of `dir` holds any `.mmmp` file.
fn has_previews(dir: &Path) -> bool {
    std::fs::read_dir(dir.join(PREVIEWS_DIR)).is_ok_and(|rd| {
        rd.flatten()
            .any(|e| e.path().extension().is_some_and(|x| x == "mmmp"))
    })
}

/// Gather the facts of `entry`. Blocking file I/O: only call from a
/// background thread. Never takes the session lock and never panics on a
/// corrupt session.
pub fn load_facts(entry: &RecentEntry) -> CardFacts {
    let dir = &entry.path;
    let mut f = CardFacts {
        path: dir.clone(),
        name: session_name(dir),
        last_opened: entry.last_opened,
        availability: Availability::Available,
        panels: None,
        groups: Vec::new(),
        group_colors: BTreeMap::new(),
        state: CardState::Unknown,
        problems: None,
        notes_count: None,
        notes: String::new(),
        summary: None,
    };
    if !is_mosaic_dir(dir) {
        f.availability = Availability::Missing;
        return f;
    }
    let busy = session_busy(dir);
    let cfg = match MosaicConfig::load(dir) {
        Ok(cfg) => cfg,
        Err(e) => {
            f.availability = Availability::Unreadable(e.to_string());
            return f;
        }
    };
    if busy {
        f.availability = Availability::Busy;
    }
    let summary = read_summary(dir);
    f.panels = Some(cfg.panels.len());
    f.group_colors = load_group_colors(&cfg.ui);
    f.notes = notes(&cfg.ui);
    if let Some(s) = &summary {
        f.groups = s
            .groups
            .iter()
            .map(|g| (g.name.clone(), g.panels.len()))
            .collect();
        f.problems = s.problems;
        f.notes_count = s.notes;
    }
    f.state = card_state(cfg.panels.len(), has_previews(dir), summary.as_ref());
    f.summary = summary;
    f
}

/// Loads [`CardFacts`] on one background thread, the hero session first.
pub struct FactsLoader {
    rx: Receiver<CardFacts>,
}

impl FactsLoader {
    /// Start loading `entries`: `first` (when it is among them) before the
    /// rest, in order, requesting a repaint on `ctx` after each result.
    pub fn spawn(entries: Vec<RecentEntry>, first: Option<PathBuf>, ctx: &egui::Context) -> Self {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        let mut entries = entries;
        if let Some(first) = first
            && let Some(i) = entries.iter().position(|e| e.path == first)
        {
            let hero = entries.remove(i);
            entries.insert(0, hero);
        }
        std::thread::spawn(move || {
            for entry in entries {
                if tx.send(load_facts(&entry)).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
        });
        FactsLoader { rx }
    }

    /// The results that have arrived since the last call (non-blocking).
    pub fn poll(&mut self) -> Vec<CardFacts> {
        self.rx.try_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{notes, save_notes_blocking};
    use crate::start::tests::temp_dir;
    use mmm_core::mosaic::{CONFIG_FILE, MosaicConfig, lock_session};
    use mmm_core::session_summary::{GroupSummary, ResultState, SessionSummary};
    use std::path::Path;

    fn entry(path: &Path) -> RecentEntry {
        RecentEntry {
            path: path.to_path_buf(),
            last_opened: 7,
            pinned: false,
        }
    }

    fn group(result: ResultState, blended: Option<u64>, exported: Option<u64>) -> GroupSummary {
        GroupSummary {
            name: "g".into(),
            panels: vec![],
            result,
            blended_at: blended,
            exported_at: exported,
        }
    }

    fn summary(groups: Vec<GroupSummary>) -> SessionSummary {
        SessionSummary {
            panels: 2,
            groups,
            ..SessionSummary::default()
        }
    }

    #[test]
    fn facts_of_a_session() {
        let root = temp_dir("facts_ok");
        let s = crate::testutil::two_group_session(&root);
        let dir = s.mosaic.dir.clone();
        let f = load_facts(&entry(&dir));
        assert_eq!(f.availability, Availability::Available);
        assert_eq!(f.panels, Some(s.mosaic.config.panels.len()));
        assert_eq!(f.notes, "");
        assert_eq!(f.last_opened, 7);
        assert_eq!(f.name, "m");
    }

    #[test]
    fn missing_session_is_missing() {
        let root = temp_dir("facts_missing");
        let f = load_facts(&entry(&root.join("nope.mmm-session")));
        assert_eq!(f.availability, Availability::Missing);
        assert_eq!(f.panels, None);
    }

    #[test]
    fn busy_session_is_busy() {
        let root = temp_dir("facts_busy");
        let s = crate::testutil::two_group_session(&root);
        let dir = s.mosaic.dir.clone();
        let _lock = lock_session(&dir).unwrap();
        let f = load_facts(&entry(&dir));
        assert_eq!(f.availability, Availability::Busy);
        assert_eq!(f.panels, Some(s.mosaic.config.panels.len()));
    }

    #[test]
    fn unreadable_session_says_so() {
        let root = temp_dir("facts_bad");
        let s = crate::testutil::two_group_session(&root);
        let dir = s.mosaic.dir.clone();
        std::fs::write(dir.join(CONFIG_FILE), "{").unwrap();
        let f = load_facts(&entry(&dir));
        assert!(matches!(f.availability, Availability::Unreadable(_)));
    }

    #[test]
    fn card_state_rules() {
        use CardState::*;
        let done = |b, e| group(ResultState::Complete, Some(b), Some(e));
        assert_eq!(card_state(0, true, None), NoPanels);
        assert_eq!(
            card_state(0, true, Some(&summary(vec![done(1, 2)]))),
            NoPanels
        );
        let stale = summary(vec![done(1, 2), group(ResultState::Stale, None, None)]);
        assert_eq!(card_state(3, true, Some(&stale)), ReMergeNeeded);
        let partial = summary(vec![group(ResultState::Partial, None, None)]);
        assert_eq!(card_state(3, false, Some(&partial)), ReMergeNeeded);
        let exported = summary(vec![done(1, 2), done(5, 5)]);
        assert_eq!(card_state(3, true, Some(&exported)), MergedExported);
        let merged = summary(vec![done(1, 2), done(5, 4)]);
        assert_eq!(card_state(3, true, Some(&merged)), Merged);
        let unexported = summary(vec![group(ResultState::Complete, Some(1), None)]);
        assert_eq!(card_state(3, false, Some(&unexported)), Merged);
        let none = summary(vec![group(ResultState::None, None, None)]);
        assert_eq!(card_state(3, true, Some(&none)), PlanReady);
        assert_eq!(card_state(3, true, None), PlanReady);
        assert_eq!(card_state(3, false, None), Unknown);
        assert_eq!(card_state(3, false, Some(&none)), Unknown);
    }

    #[test]
    fn notes_roundtrip() {
        let root = temp_dir("notes_rt");
        let s = crate::testutil::two_group_session(&root);
        let dir = s.mosaic.dir.clone();
        let mut cfg = MosaicConfig::load(&dir).unwrap();
        cfg.ui = serde_json::json!({ "other": 5 });
        cfg.save(&dir).unwrap();
        save_notes_blocking(&dir, "first light\nsecond line").unwrap();
        assert_eq!(load_facts(&entry(&dir)).notes, "first light\nsecond line");
        let cfg = MosaicConfig::load(&dir).unwrap();
        assert_eq!(cfg.ui["other"], 5);
        assert_eq!(notes(&cfg.ui), "first light\nsecond line");
    }

    #[test]
    fn notes_save_refused_while_busy() {
        let root = temp_dir("notes_busy");
        let s = crate::testutil::two_group_session(&root);
        let dir = s.mosaic.dir.clone();
        let before = std::fs::read(dir.join(CONFIG_FILE)).unwrap();
        let _lock = lock_session(&dir).unwrap();
        let err = save_notes_blocking(&dir, "x").unwrap_err();
        assert!(err.contains("open in another window"), "{err}");
        assert_eq!(std::fs::read(dir.join(CONFIG_FILE)).unwrap(), before);
    }

    #[test]
    fn hero_facts_arrive_first() {
        let root = temp_dir("hero_first");
        let entries: Vec<RecentEntry> = (0..5)
            .map(|i| entry(&root.join(format!("s{i}.mmm-session"))))
            .collect();
        let mut loader = FactsLoader::spawn(
            entries.clone(),
            Some(entries[3].path.clone()),
            &egui::Context::default(),
        );
        let mut got = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while got.len() < 5 {
            assert!(std::time::Instant::now() < deadline, "timed out");
            got.extend(loader.poll());
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(got[0].path, entries[3].path);
    }
}
