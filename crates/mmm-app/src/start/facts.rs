//! What a recent-session card shows, gathered off the UI thread: the
//! session's size, groups, state and notes, read without ever taking the
//! session lock.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, PoisonError};

use mmm_core::mosaic::{MosaicConfig, is_mosaic_dir, session_busy};
use mmm_core::preview::PREVIEWS_DIR;
use mmm_core::session_summary::{ResultState, SessionSummary, read_summary};

use super::RecentEntry;
use crate::panel_edits::load_group_colors;
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
    /// Some groups merged, the others without a result yet.
    PartlyMerged,
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
/// session summary (if any). A stale or partial group makes it
/// [`CardState::ReMergeNeeded`] whatever the others are; then every group
/// complete is merged, and some complete (the rest without a result)
/// [`CardState::PartlyMerged`].
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
        if groups.iter().any(|g| g.result == ResultState::Complete) {
            return CardState::PartlyMerged;
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

/// How the facts of one entry are read: [`load_facts`], or a stand-in in
/// tests (e.g. one that stalls like a dead network share).
pub type LoadFacts = fn(&RecentEntry) -> CardFacts;

/// How many threads load facts at once: a session on an unreachable path
/// (a dead network share can stall for tens of seconds) holds up one of
/// them, not the whole list.
const FACTS_WORKERS: usize = 4;

/// Loads [`CardFacts`] on a few background threads that take entries from
/// one queue, the hero session first.
pub struct FactsLoader {
    rx: Receiver<CardFacts>,
}

impl FactsLoader {
    /// Start loading `entries`: `first` (when it is among them) taken
    /// first, then the rest in order, requesting a repaint on `ctx` after
    /// each result. Results arrive as they finish, so a slow entry doesn't
    /// hold back the ones after it.
    pub fn spawn(entries: Vec<RecentEntry>, first: Option<PathBuf>, ctx: &egui::Context) -> Self {
        Self::spawn_with(entries, first, ctx, load_facts)
    }

    /// [`FactsLoader::spawn`], reading each entry's facts with `load`.
    pub fn spawn_with(
        entries: Vec<RecentEntry>,
        first: Option<PathBuf>,
        ctx: &egui::Context,
        load: LoadFacts,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let workers = FACTS_WORKERS.min(entries.len());
        let queue = Arc::new(Mutex::new(load_order(entries, first)));
        for _ in 0..workers {
            let (queue, tx, ctx) = (Arc::clone(&queue), tx.clone(), ctx.clone());
            std::thread::spawn(move || {
                loop {
                    let next = queue
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .pop_front();
                    let Some(entry) = next else { return };
                    if tx.send(load(&entry)).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            });
        }
        FactsLoader { rx }
    }

    /// The results that have arrived since the last call (non-blocking).
    pub fn poll(&mut self) -> Vec<CardFacts> {
        self.rx.try_iter().collect()
    }
}

/// `entries` in the order they are loaded: `first` (when it is among them)
/// at the front, then the rest as given.
fn load_order(mut entries: Vec<RecentEntry>, first: Option<PathBuf>) -> VecDeque<RecentEntry> {
    if let Some(first) = first
        && let Some(i) = entries.iter().position(|e| e.path == first)
    {
        let hero = entries.remove(i);
        entries.insert(0, hero);
    }
    entries.into()
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
        // Some groups merged and the rest not yet, previews or not.
        let unmerged = || group(ResultState::None, None, None);
        let partly = summary(vec![done(1, 2), unmerged()]);
        assert_eq!(card_state(3, true, Some(&partly)), PartlyMerged);
        assert_eq!(card_state(3, false, Some(&partly)), PartlyMerged);
        // A stale or partial group still comes first.
        let stale = group(ResultState::Stale, None, None);
        let partly_stale = summary(vec![done(1, 2), unmerged(), stale]);
        assert_eq!(card_state(3, true, Some(&partly_stale)), ReMergeNeeded);
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

    /// Facts of `entry`, stalling for files named `stuck*` (like a folder
    /// on a dead network share) and slow for `slow*` ones.
    fn stalling(entry: &RecentEntry) -> CardFacts {
        let name = entry.path.file_name().unwrap_or_default().to_string_lossy();
        if name.starts_with("stuck") {
            std::thread::sleep(std::time::Duration::from_secs(120));
        } else if name.starts_with("slow") {
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        load_facts(entry)
    }

    /// Poll `loader` until `n` results came, or `secs` passed.
    fn collect(loader: &mut FactsLoader, n: usize, secs: u64) -> Vec<CardFacts> {
        let mut got = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        while got.len() < n && std::time::Instant::now() < deadline {
            got.extend(loader.poll());
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        got
    }

    #[test]
    fn hero_candidate_is_dequeued_first() {
        let entries: Vec<RecentEntry> = (0..5)
            .map(|i| entry(Path::new(&format!("s{i}.mmm-session"))))
            .collect();
        let order: Vec<PathBuf> = load_order(entries.clone(), Some(entries[3].path.clone()))
            .into_iter()
            .map(|e| e.path)
            .collect();
        assert_eq!(order, [3, 0, 1, 2, 4].map(|i| entries[i].path.clone()));
    }

    #[test]
    fn hero_facts_arrive_first() {
        // Slow sessions ahead of the hero in the list don't keep it waiting.
        let root = temp_dir("hero_first");
        let entries: Vec<RecentEntry> = ["slow0", "slow1", "slow2", "hero", "slow4"]
            .iter()
            .map(|n| entry(&root.join(format!("{n}.mmm-session"))))
            .collect();
        let hero = entries[3].path.clone();
        let ctx = egui::Context::default();
        let mut loader = FactsLoader::spawn_with(entries, Some(hero.clone()), &ctx, stalling);
        let got = collect(&mut loader, 5, 10);
        assert_eq!(got.len(), 5, "timed out");
        assert_eq!(got[0].path, hero);
    }

    #[test]
    fn a_stuck_entry_does_not_hold_up_the_rest() {
        let root = temp_dir("facts_stuck");
        let entries: Vec<RecentEntry> = ["stuck", "s1", "s2", "s3", "s4"]
            .iter()
            .map(|n| entry(&root.join(format!("{n}.mmm-session"))))
            .collect();
        let stuck = entries[0].path.clone();
        let ctx = egui::Context::default();
        let mut loader = FactsLoader::spawn_with(entries, Some(stuck.clone()), &ctx, stalling);
        let got = collect(&mut loader, 4, 5);
        assert_eq!(got.len(), 4, "the others' facts arrive while one is stuck");
        assert!(got.iter().all(|f| f.path != stuck));
    }
}
