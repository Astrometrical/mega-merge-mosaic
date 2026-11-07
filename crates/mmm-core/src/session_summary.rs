//! The small `summary.json` a running app keeps in each session folder, so a
//! start screen can show a session's facts (panel and group counts,
//! merge/export state, check counts) without opening it.
//!
//! The file is advisory: [`read_summary`] returns `None` for anything it
//! cannot trust (absent, unreadable, corrupt, or written by a newer mmm).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::fsutil::write_atomic;
use crate::tiles::{FileTileReader, TileRead, TileState};

/// Name of the summary file at the session root.
pub const SUMMARY_FILE: &str = "summary.json";

/// The summary format this build writes and the newest it reads.
pub const SUMMARY_FORMAT: u32 = 1;

/// A session's facts, as last written by the app that had it open.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct SessionSummary {
    /// Format version of this file ([`SUMMARY_FORMAT`]).
    pub format: u32,
    /// When it was written, in unix seconds.
    pub written_at: u64,
    /// Number of panels in the session.
    pub panels: usize,
    /// Per-group facts.
    pub groups: Vec<GroupSummary>,
    /// Count of check problems; `None` when not checked yet.
    pub problems: Option<usize>,
    /// Count of check notes; `None` when not checked yet.
    pub notes: Option<usize>,
    /// Canvas size in pixels (width, height), once known.
    pub canvas: Option<(u64, u64)>,
    /// The view last shown (a free-form label chosen by the app).
    pub last_view: Option<String>,
}

/// One group's facts within a [`SessionSummary`].
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct GroupSummary {
    /// Group name.
    pub name: String,
    /// The group's panel files.
    pub panels: Vec<PathBuf>,
    /// State of the group's blended result.
    pub result: ResultState,
    /// When the group was last blended, in unix seconds.
    pub blended_at: Option<u64>,
    /// When the group was last exported, in unix seconds.
    pub exported_at: Option<u64>,
}

/// How much of a group's result store is current.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ResultState {
    /// No result: never blended, or nothing in the store is valid.
    #[default]
    None,
    /// Every tile is valid.
    Complete,
    /// At least one tile is stale (blended with earlier settings or inputs).
    Stale,
    /// Some tiles are valid and some were never blended.
    Partial,
}

/// Read `dir`'s [`SUMMARY_FILE`]. `None` when it is absent, unreadable,
/// unparsable, or from a newer format than [`SUMMARY_FORMAT`].
pub fn read_summary(dir: &Path) -> Option<SessionSummary> {
    let bytes = std::fs::read(dir.join(SUMMARY_FILE)).ok()?;
    let s: SessionSummary = serde_json::from_slice(&bytes).ok()?;
    (s.format <= SUMMARY_FORMAT).then_some(s)
}

/// Write `s` to `dir`'s [`SUMMARY_FILE`] atomically.
pub fn write_summary(dir: &Path, s: &SessionSummary) -> Result<()> {
    let path = dir.join(SUMMARY_FILE);
    let bytes = serde_json::to_vec_pretty(s)
        .map_err(|e| Error::compute(format!("{}: {e}", path.display())))?;
    write_atomic(&path, &bytes)
}

/// Classify the result store at `store` from its level-0 tile states, read
/// with [`FileTileReader`] (never memory-mapped, so a store recreated by
/// another process is safe to read). A missing or unreadable store is
/// [`ResultState::None`]; a stale tile makes the result [`ResultState::Stale`]
/// even if others are missing.
pub fn result_state(store: &Path) -> ResultState {
    let Ok(reader) = FileTileReader::open(store) else {
        return ResultState::None;
    };
    let (nx, ny) = reader.geometry().tiles(0);
    let (mut valid, mut stale, mut total) = (0usize, 0usize, 0usize);
    for ty in 0..ny {
        for tx in 0..nx {
            total += 1;
            match reader.state(0, tx, ty) {
                TileState::Valid => valid += 1,
                TileState::Stale => stale += 1,
                TileState::Missing => {}
            }
        }
    }
    if stale > 0 {
        ResultState::Stale
    } else if valid == 0 {
        ResultState::None
    } else if valid == total {
        ResultState::Complete
    } else {
        ResultState::Partial
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::{TileGeometry, TileStore};

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mmm-sumry-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample() -> SessionSummary {
        SessionSummary {
            format: SUMMARY_FORMAT,
            written_at: 1_700_000_000,
            panels: 4,
            groups: vec![GroupSummary {
                name: "L".into(),
                panels: vec![PathBuf::from("a.xisf"), PathBuf::from("b.xisf")],
                result: ResultState::Stale,
                blended_at: Some(1_699_999_000),
                exported_at: None,
            }],
            problems: Some(1),
            notes: None,
            canvas: Some((100, 200)),
            last_view: Some("L".into()),
        }
    }

    #[test]
    fn summary_roundtrips() {
        let dir = tmp("rt");
        let s = sample();
        write_summary(&dir, &s).unwrap();
        assert_eq!(read_summary(&dir), Some(s));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn summary_from_newer_mmm_is_ignored() {
        let dir = tmp("newer");
        let mut s = sample();
        s.format = SUMMARY_FORMAT + 1;
        write_summary(&dir, &s).unwrap();
        assert_eq!(read_summary(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_summary_is_ignored() {
        let dir = tmp("corrupt");
        assert_eq!(read_summary(&dir), None, "absent");
        std::fs::write(dir.join(SUMMARY_FILE), "{not json").unwrap();
        assert_eq!(read_summary(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn result_state_follows_the_store() {
        let dir = tmp("state");
        let path = dir.join("result.tiles");
        assert_eq!(result_state(&path), ResultState::None, "no file");

        // 2 x 2 tiles of 8 px.
        let g = TileGeometry {
            width: 16,
            height: 16,
            channels: 1,
            tile: 8,
        };
        let tile = vec![1.0f32; 64];
        {
            let _fresh = TileStore::create(&path, g).unwrap();
        }
        assert_eq!(result_state(&path), ResultState::None, "nothing written");
        {
            let mut store = TileStore::open(&path).unwrap();
            store.write_tile(0, 0, 0, &tile).unwrap();
            store.write_tile(0, 1, 0, &tile).unwrap();
        }
        assert_eq!(result_state(&path), ResultState::Partial, "half written");
        {
            let mut store = TileStore::open(&path).unwrap();
            store.write_tile(0, 0, 1, &tile).unwrap();
            store.write_tile(0, 1, 1, &tile).unwrap();
        }
        assert_eq!(result_state(&path), ResultState::Complete);
        {
            let mut store = TileStore::open(&path).unwrap();
            store.invalidate_rect([0, 0, 4, 4]);
        }
        assert_eq!(result_state(&path), ResultState::Stale);

        // Stale wins over missing: a recreated store with one tile written,
        // then invalidated, leaves stale and missing tiles together.
        let mixed = dir.join("mixed.tiles");
        {
            let mut store = TileStore::create(&mixed, g).unwrap();
            store.write_tile(0, 0, 0, &tile).unwrap();
            store.write_tile(0, 1, 0, &tile).unwrap();
            store.invalidate_rect([0, 0, 4, 4]);
        }
        assert_eq!(result_state(&mixed), ResultState::Stale);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
