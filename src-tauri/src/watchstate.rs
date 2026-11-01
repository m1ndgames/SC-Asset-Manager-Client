//! `state.json` — the watcher's high-water mark.
//!
//! This is the file that decides, on every launch, whether a trade you made
//! while the client was closed is seen, seen twice, or lost. PLAN.md §3.6.
//!
//! Three things are persisted and each earns its place:
//!
//! - **`offset` + `log_created_unix_ms`** — where to resume, and the identity
//!   check that makes resuming safe. Length alone cannot tell "the same file,
//!   longer" from "a new session's log that happens to be longer".
//! - **`processed_ids`** — ids already *enqueued*, not ids already written.
//!   Writing them would mean a rejected trade comes back on the next backfill.
//!   Bounded to the last 500: deterministic ids make this a duplicate filter,
//!   not a ledger, and the queue and history are the real records.
//! - **`last_seen_log_ts`** — a cheap coarse filter for a backfill, from the
//!   log's own clock rather than ours.
//!
//! ## The ordering rule
//!
//! The offset is advanced only past lines whose detections have **left the
//! grace window**. A detection still being held is not persisted anywhere, so
//! its line must stay unread as far as `state.json` is concerned; a crash then
//! re-reads it, and the deterministic id collapses the duplicate. The reverse
//! — advancing on parse — loses trades outright.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::config;
use crate::error::{AppError, AppResult};

pub const SCHEMA: u32 = 1;

/// How many ids to keep. One trading session is a handful; 500 covers weeks.
const MAX_PROCESSED_IDS: usize = 500;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchState {
    pub schema: u32,
    /// Which file the rest of this record describes. A different path makes
    /// the offset meaningless.
    pub log_path: String,
    /// Creation time of that file, as unix milliseconds. `None` on a
    /// filesystem that does not report one — then only length can be checked.
    pub log_created_unix_ms: Option<u64>,
    /// A line boundary in that file, past only fully-resolved detections.
    pub offset: u64,
    /// The newest trade timestamp we have enqueued, from the log's own clock.
    pub last_seen_log_ts: Option<String>,
    /// Ids already enqueued. Oldest first.
    pub processed_ids: VecDeque<String>,
}

impl Default for WatchState {
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            log_path: String::new(),
            log_created_unix_ms: None,
            offset: 0,
            last_seen_log_ts: None,
            processed_ids: VecDeque::new(),
        }
    }
}

impl WatchState {
    /// Does this record describe the file now at `path`?
    ///
    /// A `None` creation time on either side is not a mismatch — it means the
    /// question cannot be answered, and the caller falls back to length.
    pub fn matches(&self, path: &Path, created: Option<SystemTime>) -> bool {
        if self.log_path != path.display().to_string() {
            return false;
        }
        match (self.log_created_unix_ms, created.map(to_unix_ms)) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        }
    }

    pub fn has_processed(&self, id: &str) -> bool {
        self.processed_ids.iter().any(|i| i == id)
    }

    /// Record an enqueued id, evicting the oldest beyond the bound.
    pub fn mark_processed(&mut self, id: impl Into<String>) {
        let id = id.into();
        if self.has_processed(&id) {
            return;
        }
        self.processed_ids.push_back(id);
        while self.processed_ids.len() > MAX_PROCESSED_IDS {
            self.processed_ids.pop_front();
        }
    }

    /// Advance the log-clock watermark, never backwards.
    ///
    /// Compared as strings on purpose: the log writes a fixed-width RFC 3339
    /// UTC timestamp, which is lexicographically ordered, and parsing here
    /// would make a format change silently reset the watermark to zero.
    pub fn observe_log_ts(&mut self, ts: &str) {
        let newer = match &self.last_seen_log_ts {
            Some(prev) => ts > prev.as_str(),
            None => true,
        };
        if newer {
            self.last_seen_log_ts = Some(ts.to_string());
        }
    }

    pub fn set_file(&mut self, path: &Path, created: Option<SystemTime>) {
        self.log_path = path.display().to_string();
        self.log_created_unix_ms = created.map(to_unix_ms);
    }

    /// A different file at the same path, or a new path: the position is gone,
    /// but the duplicate filter is still worth keeping.
    pub fn reset_position(&mut self) {
        self.offset = 0;
    }
}

fn to_unix_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis() as u64
}

pub fn state_path() -> AppResult<PathBuf> {
    Ok(config::config_dir()?.join("state.json"))
}

/// Load, or start fresh.
///
/// A corrupt or unreadable `state.json` must never stop the watcher: the worst
/// case of starting fresh is a backfill prompt, while refusing to start means
/// no trades are detected at all.
pub fn load_from(dir: &Path) -> WatchState {
    let path = dir.join("state.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return WatchState::default();
    };
    match serde_json::from_str::<WatchState>(&text) {
        Ok(mut s) => {
            if s.schema != SCHEMA {
                tracing::warn!(
                    found = s.schema,
                    expected = SCHEMA,
                    "state.json schema differs; keeping the duplicate filter, dropping the position"
                );
                s.schema = SCHEMA;
                s.reset_position();
            }
            s
        }
        Err(e) => {
            tracing::error!(error = %e, path = %path.display(), "state.json is unreadable; starting fresh");
            WatchState::default()
        }
    }
}

pub fn save_to(dir: &Path, s: &WatchState) -> AppResult<()> {
    std::fs::create_dir_all(dir)
        .map_err(|e| AppError::Config(format!("cannot create {}: {e}", dir.display())))?;
    let bytes = serde_json::to_vec_pretty(s)
        .map_err(|e| AppError::Config(format!("cannot serialise state: {e}")))?;
    config::write_atomic(&dir.join("state.json.tmp"), &dir.join("state.json"), &bytes)
}

pub fn load() -> WatchState {
    match config::config_dir() {
        Ok(d) => load_from(&d),
        Err(e) => {
            tracing::error!(error = %e, "no config directory; watcher state will not persist");
            WatchState::default()
        }
    }
}

pub fn save(s: &WatchState) -> AppResult<()> {
    save_to(&config::config_dir()?, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_a_default() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_from(dir.path()), WatchState::default());
    }

    #[test]
    fn a_round_trip_preserves_everything() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = WatchState {
            log_path: "E:\\x\\Game.log".into(),
            log_created_unix_ms: Some(1_762_000_000_000),
            offset: 4096,
            ..Default::default()
        };
        s.observe_log_ts("2026-11-01T09:11:29.754Z");
        s.mark_processed("4ed7a396-7398-590d-be1c-d3a012c0dcb6");
        save_to(dir.path(), &s).unwrap();
        assert_eq!(load_from(dir.path()), s);
    }

    /// Corrupt state must degrade to a fresh start, never to a crash.
    #[test]
    fn corrupt_state_starts_fresh() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("state.json"), "{ not json").unwrap();
        assert_eq!(load_from(dir.path()), WatchState::default());
    }

    /// An unknown schema keeps the duplicate filter but drops the position —
    /// the ids remain meaningful, the byte offset does not.
    #[test]
    fn a_future_schema_drops_the_position_but_keeps_the_ids() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = WatchState {
            schema: 99,
            offset: 9999,
            ..Default::default()
        };
        s.mark_processed("keep-me");
        std::fs::write(
            dir.path().join("state.json"),
            serde_json::to_vec(&s).unwrap(),
        )
        .unwrap();

        let got = load_from(dir.path());
        assert_eq!(got.schema, SCHEMA);
        assert_eq!(got.offset, 0);
        assert!(got.has_processed("keep-me"));
    }

    #[test]
    fn the_id_filter_is_bounded_and_evicts_the_oldest() {
        let mut s = WatchState::default();
        for i in 0..MAX_PROCESSED_IDS + 10 {
            s.mark_processed(format!("id-{i}"));
        }
        assert_eq!(s.processed_ids.len(), MAX_PROCESSED_IDS);
        assert!(!s.has_processed("id-0"));
        assert!(s.has_processed(&format!("id-{}", MAX_PROCESSED_IDS + 9)));
    }

    #[test]
    fn marking_the_same_id_twice_is_a_no_op() {
        let mut s = WatchState::default();
        s.mark_processed("x");
        s.mark_processed("x");
        assert_eq!(s.processed_ids.len(), 1);
    }

    #[test]
    fn the_log_watermark_never_moves_backwards() {
        let mut s = WatchState::default();
        s.observe_log_ts("2026-11-01T11:09:47.325Z");
        s.observe_log_ts("2026-11-01T09:11:29.754Z");
        assert_eq!(
            s.last_seen_log_ts.as_deref(),
            Some("2026-11-01T11:09:47.325Z")
        );
    }

    #[test]
    fn identity_rejects_a_different_path_or_creation_time() {
        let mut s = WatchState::default();
        let path = Path::new("E:\\x\\Game.log");
        let t = UNIX_EPOCH + Duration::from_millis(1_762_000_000_000);
        s.set_file(path, Some(t));

        assert!(s.matches(path, Some(t)));
        assert!(!s.matches(Path::new("E:\\y\\Game.log"), Some(t)));
        assert!(!s.matches(path, Some(t + Duration::from_secs(1))));
        // Unknown creation time: unanswerable, so not a mismatch — the caller
        // falls back to comparing length.
        assert!(s.matches(path, None));
    }
}
