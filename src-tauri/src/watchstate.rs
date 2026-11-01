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
use crate::log::tail::FileIdentity;
use crate::error::{AppError, AppResult};

// Bumped to 2 when the header identity was added. A schema-1 record's offset
// was validated against a creation time that NTFS resurrects on every
// rotation, so it cannot be trusted; the loader drops the position and keeps
// the id filter.
pub const SCHEMA: u32 = 2;

/// How many ids to keep. One trading session is a handful; 500 covers weeks.
const MAX_PROCESSED_IDS: usize = 500;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchState {
    pub schema: u32,
    /// Which file the rest of this record describes. A different path makes
    /// the offset meaningless.
    pub log_path: String,
    /// That file's first line — the game's own session fingerprint, and the
    /// reliable half of the identity. Legible on purpose: you can read it and
    /// see which session the offset belongs to.
    #[serde(default)]
    pub log_header: Option<String>,
    /// Creation time as unix milliseconds. A fallback only, for a file with no
    /// usable first line — it is not trustworthy for `Game.log`.
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
            log_header: None,
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
    /// An unanswerable comparison is not a mismatch — it means the caller must
    /// fall back to length. Creation time is only
    /// consulted when neither side has a header, because NTFS tunneling makes
    /// a *matching* creation time meaningless, while a differing one is still
    /// conclusive.
    pub fn matches(&self, path: &Path, identity: Option<FileIdentity>) -> bool {
        if self.log_path != path.display().to_string() {
            return false;
        }
        let Some(now) = identity else {
            return true;
        };
        match now.differs_from(&self.identity()) {
            Some(differs) => !differs,
            None => true,
        }
    }

    /// The stored identity, in the form the tailer compares.
    pub fn identity(&self) -> FileIdentity {
        FileIdentity {
            header: self.log_header.clone(),
            created: self
                .log_created_unix_ms
                .map(|ms| UNIX_EPOCH + Duration::from_millis(ms)),
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

    pub fn set_file(&mut self, path: &Path, identity: Option<FileIdentity>) {
        self.log_path = path.display().to_string();
        let id = identity.unwrap_or_default();
        self.log_header = id.header;
        self.log_created_unix_ms = id.created.map(to_unix_ms);
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
    fn identity_rejects_a_different_path_or_a_different_session() {
        let mut s = WatchState::default();
        let path = Path::new("E:/x/Game.log");
        let session_a = FileIdentity {
            header: Some("<2026-11-01T17:29:49.066Z> BackupNameAttachment=\" Build(12913470) 01 Nov 26 (18 29 39)\"".into()),
            created: None,
        };
        let session_b = FileIdentity {
            header: Some("<2026-11-01T08:07:03.350Z> BackupNameAttachment=\" Build(12913470) 01 Nov 26 (09 06 54)\"".into()),
            created: None,
        };
        s.set_file(path, Some(session_a.clone()));

        assert!(s.matches(path, Some(session_a)));
        assert!(!s.matches(path, Some(session_b)), "a new session must not match");
        assert!(!s.matches(Path::new("E:/y/Game.log"), None));
        // Nothing to compare: unanswerable, so not a mismatch — the caller
        // falls back to length.
        assert!(s.matches(path, None));
    }

    /// The bug this identity exists for: NTFS resurrects a creation time on
    /// rotation, so two different sessions can carry the same one. The header
    /// must still tell them apart.
    #[test]
    fn a_resurrected_creation_time_does_not_make_two_sessions_match() {
        let path = Path::new("E:/x/Game.log");
        let tunneled = UNIX_EPOCH + Duration::from_millis(1_621_967_154_569);
        let mut s = WatchState::default();
        s.set_file(
            path,
            Some(FileIdentity {
                header: Some("session one".into()),
                created: Some(tunneled),
            }),
        );

        assert!(!s.matches(
            path,
            Some(FileIdentity {
                header: Some("session two".into()),
                // Identical, because NTFS tunneling preserved it.
                created: Some(tunneled),
            })
        ));
    }
}
