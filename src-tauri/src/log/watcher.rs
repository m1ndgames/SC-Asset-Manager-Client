//! The watcher: tailer → parser → correlator → `state.json`.
//!
//! Everything here is Tauri-free and clock-injected, so the whole pipeline can
//! be driven over a temp file in a unit test. `driver.rs` is the thin shell
//! that runs it on a thread and emits events.
//!
//! ## The ordering rule this module exists to enforce
//!
//! The persisted offset must only ever advance past lines whose detections
//! have **left the grace window**. A detection still being held lives in
//! `Correlator`'s memory and nowhere else; if the offset moved past its line
//! and the process died, that trade would be gone for good. Holding the offset
//! back means a crash re-reads a handful of lines instead — and the
//! deterministic id collapses the duplicate.
//!
//! So `safe_offset` is not the tailer's offset. It is the tailer's offset as of
//! the last moment nothing was pending. `Tailer::offset()` is where *reading*
//! has got to; `safe_offset` is where *resolution* has got to, and only the
//! latter is written to disk.
//!
//! ## Cold start
//!
//! See [`plan_cold_start`]. The three cases differ in what they risk: resuming
//! wrongly duplicates, backfilling wrongly floods the queue with a session the
//! user already dealt with, and a first run that silently imports a whole
//! session is the worst of the three because the user never asked for it.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::correlate::{Correlator, Event};
use super::parse::{parse_line, ParsedLine};
use super::scan::{self, DetectedTrade, DiscardedTrade};
use super::tail::{self, Tailer, Tick};
use crate::watchstate::WatchState;

/// How many raw class-matching lines to keep for Diagnostics.
const RECENT_LINES: usize = 50;

// ---------------------------------------------------------------------------
// Cold start
// ---------------------------------------------------------------------------

/// What to do with the log that is already on disk when the watcher starts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ColdStart {
    /// Same file, and our offset is still inside it. The common case, and the
    /// only one that reprocesses nothing.
    Resume { offset: u64 },
    /// A session happened while the client was closed. The caller parses the
    /// whole file with `hold_ms = 0` — every error that will ever exist is
    /// already in it — and filters with [`backfill`].
    Backfill {
        /// Why, so Diagnostics can say so rather than just acting.
        reason: BackfillReason,
    },
    /// Never watched anything before. Do **not** import silently: report the
    /// count and let the user choose, defaulting to start-from-now.
    FirstRun {
        /// Trades found in the file as it stands.
        trades: usize,
        /// Where start-from-now would begin.
        end_offset: u64,
    },
    /// No file at that path yet. Normal — the game has not run.
    NoLog,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BackfillReason {
    /// A different file now sits at that path: the game rotated ours into
    /// `logbackups/` and started a new one.
    Replaced,
    /// Same file, but shorter than our offset. Truncated in place.
    Truncated,
    /// The configured path changed.
    DifferentPath,
}

/// Decide the cold-start case without reading the whole file.
///
/// `count_trades` is only called for the first-run case, because that is the
/// one where a number has to be shown to the user before anything happens.
pub fn plan_cold_start(
    path: &Path,
    state: &WatchState,
    count_trades: impl FnOnce(&Path) -> usize,
) -> ColdStart {
    let Ok(meta) = std::fs::metadata(path) else {
        return ColdStart::NoLog;
    };
    if !meta.is_file() {
        return ColdStart::NoLog;
    }
    let len = meta.len();
    // From the log's own first line, not from a creation time NTFS resurrects
    // on every rotation. See `tail::FileIdentity`.
    let identity = tail::identity_of(path);

    // Never watched anything: the offset of 0 is an absence, not a position.
    if state.log_path.is_empty() {
        return ColdStart::FirstRun {
            trades: count_trades(path),
            end_offset: len,
        };
    }

    if state.log_path != path.display().to_string() {
        return ColdStart::Backfill {
            reason: BackfillReason::DifferentPath,
        };
    }
    if !state.matches(path, identity) {
        return ColdStart::Backfill {
            reason: BackfillReason::Replaced,
        };
    }
    if state.offset > len {
        return ColdStart::Backfill {
            reason: BackfillReason::Truncated,
        };
    }
    ColdStart::Resume {
        offset: state.offset,
    }
}

/// Filter a whole-file scan down to what the user has not already seen.
///
/// Two filters, deliberately both: the id set is exact but bounded, and the
/// log-clock watermark is coarse but unbounded. Together they cover a gap
/// longer than 500 trades without replaying a year of sessions.
///
/// `processed_ids` holds ids we *enqueued*, not ids we wrote — otherwise a
/// trade the user rejected would come back on every backfill.
pub fn backfill(report: &scan::ScanReport, state: &WatchState) -> Vec<DetectedTrade> {
    report
        .trades
        .iter()
        .filter(|t| !state.has_processed(&t.line.id))
        .filter(|t| match &state.last_seen_log_ts {
            Some(seen) => t.line.log_timestamp.as_str() > seen.as_str(),
            None => true,
        })
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// The watcher
// ---------------------------------------------------------------------------

/// A raw class-matching line kept for Diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeenLine {
    pub raw: String,
    /// `trade`, `error`, `location`, `ignored` or `malformed`.
    pub class: &'static str,
}

/// What Diagnostics shows about the watcher.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchStatus {
    pub path: String,
    pub running: bool,
    /// Present, and whether the last poll saw it at all.
    pub log_present: bool,
    /// Where reading has got to.
    pub read_offset: u64,
    /// Where resolution has got to — what gets persisted. Lower than
    /// `read_offset` exactly while a detection is inside its grace window.
    pub safe_offset: u64,
    /// Detections currently inside the grace window.
    pub pending: usize,
    pub grace_window_ms: u64,
    pub lines_seen: u64,
    pub class_matched: u64,
    pub promoted: u64,
    pub discarded: u64,
    pub malformed: u64,
    pub rotations: u64,
    pub last_log_timestamp: Option<String>,
}

/// One tick's worth of output.
#[derive(Debug, Default)]
pub struct WatchTick {
    /// Trades that have left the grace window. These are the real detections.
    pub promoted: Vec<DetectedTrade>,
    pub discarded: Vec<DiscardedTrade>,
    /// Class-matching lines that failed to parse. Never dropped: this is how a
    /// game patch breaking the watcher becomes visible.
    pub malformed: Vec<ParsedLine>,
    /// A failure line that matched no request.
    pub unmatched_errors: Vec<super::parse::TxErrorLine>,
    /// The log was truncated or replaced during this tick.
    pub rotated: bool,
    /// True when `safe_offset` moved, i.e. there is something new to persist.
    pub offset_advanced: bool,
}

impl WatchTick {
    pub fn is_quiet(&self) -> bool {
        self.promoted.is_empty()
            && self.discarded.is_empty()
            && self.malformed.is_empty()
            && self.unmatched_errors.is_empty()
            && !self.rotated
            && !self.offset_advanced
    }
}

pub struct Watcher {
    tailer: Tailer,
    correlator: Correlator,
    state: WatchState,
    /// See the module docs: resolution, not reading.
    safe_offset: u64,
    recent: VecDeque<SeenLine>,
    status: WatchStatus,
}

impl Watcher {
    /// Start watching, resuming at `offset`.
    ///
    /// `offset` comes from [`plan_cold_start`] — the stored mark for a resume,
    /// the file's length for start-from-now, or 0 to read everything.
    pub fn new(path: impl Into<PathBuf>, grace: Duration, mut state: WatchState, offset: u64) -> Self {
        let path = path.into();
        let identity = tail::identity_of(&path);
        state.set_file(&path, identity.clone());
        state.offset = offset;

        let mut status = WatchStatus {
            path: path.display().to_string(),
            running: true,
            read_offset: offset,
            safe_offset: offset,
            grace_window_ms: grace.as_millis() as u64,
            last_log_timestamp: state.last_seen_log_ts.clone(),
            ..Default::default()
        };
        status.log_present = identity.is_some();

        Self {
            tailer: Tailer::new(&path).resume_at(offset, identity.unwrap_or_default()),
            correlator: Correlator::new(grace),
            state,
            safe_offset: offset,
            recent: VecDeque::with_capacity(RECENT_LINES),
            status,
        }
    }

    pub fn status(&self) -> &WatchStatus {
        &self.status
    }

    pub fn state(&self) -> &WatchState {
        &self.state
    }

    pub fn recent_lines(&self) -> impl Iterator<Item = &SeenLine> {
        self.recent.iter()
    }

    pub fn path(&self) -> &Path {
        self.tailer.path()
    }

    /// One poll. `now` is injected so the grace window is testable.
    pub fn tick(&mut self, now: Instant) -> std::io::Result<WatchTick> {
        let mut out = WatchTick::default();
        let offset_before_batch = self.tailer.offset();
        let had_pending = self.correlator.pending_len() > 0;

        match self.tailer.poll()? {
            Tick::Missing => {
                self.status.log_present = false;
            }
            Tick::Idle => {
                self.status.log_present = true;
            }
            Tick::Lines { lines, rotated } => {
                self.status.log_present = true;
                if rotated {
                    // The position is meaningless now, but the duplicate
                    // filter is not: the same trade must not reappear just
                    // because the game relaunched.
                    out.rotated = true;
                    self.status.rotations += 1;
                    self.safe_offset = 0;
                    self.state.reset_position();
                    let path = self.tailer.path().to_path_buf();
                    self.state.set_file(&path, tail::identity_of(&path));
                }
                for line in lines {
                    self.consume(&line, now, &mut out);
                }
            }
        }

        // Promotions are what make an offset safe to persist, so they are
        // collected before the offset is recomputed.
        for ev in self.correlator.tick(now) {
            self.record(ev, &mut out);
        }

        let before = self.safe_offset;
        if self.correlator.pending_len() == 0 {
            // Everything read has fully resolved.
            self.safe_offset = self.tailer.offset();
        } else if !had_pending {
            // Something is now being held. The floor is where this batch
            // started — before any line that produced a pending detection.
            self.safe_offset = offset_before_batch;
        }
        // While pending stays non-empty across ticks the floor simply holds.
        out.offset_advanced = self.safe_offset != before;

        self.state.offset = self.safe_offset;
        self.status.read_offset = self.tailer.offset();
        self.status.safe_offset = self.safe_offset;
        self.status.pending = self.correlator.pending_len();
        Ok(out)
    }

    /// Promote everything still held, for shutdown. See `Correlator::drain`.
    pub fn finish(&mut self) -> WatchTick {
        let mut out = WatchTick::default();
        for ev in self.correlator.drain() {
            self.record(ev, &mut out);
        }
        self.safe_offset = self.tailer.offset();
        self.state.offset = self.safe_offset;
        self.status.safe_offset = self.safe_offset;
        self.status.pending = 0;
        self.status.running = false;
        out.offset_advanced = true;
        out
    }

    fn consume(&mut self, line: &str, now: Instant, out: &mut WatchTick) {
        self.status.lines_seen += 1;
        let Some(parsed) = parse_line(line) else {
            return;
        };
        self.status.class_matched += 1;

        let class = match &parsed {
            ParsedLine::Trade(_) => "trade",
            ParsedLine::TxError(_) => "error",
            ParsedLine::LocationInfo(_) => "location",
            ParsedLine::Ignored { .. } => "ignored",
            ParsedLine::Malformed { .. } => "malformed",
        };
        self.remember(line, class);

        if let ParsedLine::Malformed { .. } = &parsed {
            self.status.malformed += 1;
            out.malformed.push(parsed);
            return;
        }

        for ev in self.correlator.offer(parsed, now) {
            self.record(ev, out);
        }
    }

    fn record(&mut self, ev: Event, out: &mut WatchTick) {
        match ev {
            Event::Promoted(d) => {
                self.status.promoted += 1;
                // Marked processed at promotion, which is also when it would
                // be enqueued — not when it is written. A rejected trade must
                // not return on the next backfill.
                self.state.mark_processed(d.line.id.clone());
                self.state.observe_log_ts(&d.line.log_timestamp);
                self.status.last_log_timestamp = self.state.last_seen_log_ts.clone();
                out.promoted.push(*d);
            }
            Event::Discarded(d) => {
                self.status.discarded += 1;
                // A discard is a resolution too: its line must not be re-read
                // and re-detected on the next launch.
                self.state.mark_processed(d.trade.id.clone());
                self.state.observe_log_ts(&d.trade.log_timestamp);
                out.discarded.push(*d);
            }
            Event::UnmatchedError(e) => out.unmatched_errors.push(e),
        }
    }

    fn remember(&mut self, line: &str, class: &'static str) {
        if self.recent.len() == RECENT_LINES {
            self.recent.pop_front();
        }
        self.recent.push_back(SeenLine {
            raw: line.to_string(),
            class,
        });
    }
}

#[cfg(test)]
#[path = "watcher_tests.rs"]
mod tests;
