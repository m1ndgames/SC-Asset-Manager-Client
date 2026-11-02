//! The watcher's job is to make three correct components behave correctly
//! together. The tests that matter are the ones about the seam between the
//! offset and the grace window, because a bug there is silent in both
//! directions: a lost trade looks like a trade you never made, and a
//! duplicated one looks like a trade you made twice.

use std::io::Write;

use super::*;
use crate::ids::TradeKind;

const BUY: &str = "<2026-11-01T09:11:29.754Z> [Notice] <CEntityComponentCommodityUIProvider::SendCommodityBuyRequest> Sending SShopCommodityBuyRequest - playerId[214115750165] shopId[863690278779] shopName[SCShop_Admin_lt_base_g] kioskId[863690278778] price[190453.000000] shopPricePerCentiSCU[1.889409] resourceGUID[b999ef65-35be-45bf-908a-5eac6e06ba12] autoLoading[1] quantity[100800.000000 cSCU] Cargo Box Data: boxSize[16.000000] | unitAmount[63] [Team_CoreGameplayFeatures][Shops][UI]";
const BUY2: &str = "<2026-11-01T09:42:57.034Z> [Notice] <CEntityComponentCommodityUIProvider::SendCommodityBuyRequest> Sending SShopCommodityBuyRequest - playerId[214115750165] shopId[863690742444] shopName[SCShop_Admin_lt_base_g] kioskId[863690742440] price[151296.000000] shopPricePerCentiSCU[3.152000] resourceGUID[0d41371c-3cde-4939-8583-cd718f5edd60] autoLoading[1] quantity[48000.000000 cSCU] Cargo Box Data: boxSize[8.000000] | unitAmount[60] [Team_CoreGameplayFeatures][Shops][UI]";
const ERR_BUY: &str = "<2026-11-01T09:11:30.938Z> [Error] <CEntityComponentCommodityUIProvider::RmToken_CommodityTransactionResponse> Commodity Transaction Response Error - playerId[214115750165] result[CargoCreationFailed] type[Buying] [Team_CoreGameplayFeatures][Shops]";
const CHATTER: &str = "<2026-11-01T09:10:00.000Z> [Notice] <CVehicleMovementBase::Update> nothing to do with shops";
const MALFORMED: &str = "<2026-11-01T09:12:00.000Z> [Notice] <CEntityComponentCommodityUIProvider::SendCommodityBuyRequest> Sending SShopCommodityBuyRequest - playerId[214115750165] shopId[863690278779] [Team_CoreGameplayFeatures][Shops][UI]";

const GRACE: Duration = Duration::from_millis(3000);

struct Log {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

impl Log {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Game.log");
        Self { _dir: dir, path }
    }

    fn append(&self, line: &str) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .unwrap();
        writeln!(f, "{line}").unwrap();
        f.flush().unwrap();
    }

    fn truncate(&self) {
        std::fs::File::create(&self.path).unwrap();
    }

    fn len(&self) -> u64 {
        std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0)
    }

    fn watcher(&self, offset: u64) -> Watcher {
        Watcher::new(&self.path, GRACE, WatchState::default(), offset)
    }
}

// ---------------------------------------------------------------------------
// The seam: the offset must not pass a detection inside its window
// ---------------------------------------------------------------------------

/// The central invariant. While a trade is being held, the persisted offset
/// must still point *before* its line — otherwise a crash loses it.
#[test]
fn the_offset_is_held_back_while_a_detection_is_pending() {
    let log = Log::new();
    let mut w = log.watcher(0);
    let t0 = Instant::now();

    log.append(CHATTER);
    let tick = w.tick(t0).unwrap();
    assert!(tick.promoted.is_empty());
    let after_chatter = w.state().offset;
    assert_eq!(after_chatter, log.len(), "an unremarkable line resolves at once");

    // Now a trade, which starts a grace window.
    log.append(BUY);
    let tick = w.tick(t0 + Duration::from_millis(100)).unwrap();
    assert!(tick.promoted.is_empty(), "still inside the window");
    assert_eq!(w.status().pending, 1);
    assert_eq!(
        w.state().offset,
        after_chatter,
        "the persisted offset must still be before the trade's line"
    );
    assert_eq!(w.status().read_offset, log.len(), "but reading has moved on");

    // The window elapses.
    let tick = w.tick(t0 + Duration::from_millis(3200)).unwrap();
    assert_eq!(tick.promoted.len(), 1);
    assert!(tick.offset_advanced);
    assert_eq!(
        w.state().offset,
        log.len(),
        "once resolved, the offset catches up"
    );
}

/// Restarting at the persisted offset after a crash mid-window must re-read
/// the trade's line and detect it again — the same id, so no duplicate record.
#[test]
fn a_crash_inside_the_window_re_reads_the_line() {
    let log = Log::new();
    let t0 = Instant::now();

    let (saved_offset, id) = {
        let mut w = log.watcher(0);
        log.append(BUY);
        w.tick(t0).unwrap();
        assert_eq!(w.status().pending, 1);
        // Process dies here. Only state.offset survives.
        let ParsedLine::Trade(t) = parse_line(BUY).unwrap() else {
            unreachable!()
        };
        (w.state().offset, t.id.clone())
    };
    assert_eq!(saved_offset, 0, "nothing had resolved, so nothing persisted");

    let mut w = log.watcher(saved_offset);
    // The window runs from when the line is *read*, not from its log
    // timestamp, so the re-read trade is held once more before promotion —
    // which is correct: a retracting error line could be sitting right after
    // it, and this is the only ordering that gives it a chance to be seen.
    let tick = w.tick(t0 + Duration::from_millis(4000)).unwrap();
    assert!(tick.promoted.is_empty());
    assert_eq!(w.status().pending, 1, "held again, not dropped");

    let tick = w.tick(t0 + Duration::from_millis(7100)).unwrap();
    assert_eq!(tick.promoted.len(), 1, "the trade is recovered, not lost");
    assert_eq!(tick.promoted[0].line.id, id, "and it is the same record");
}

/// The other direction: a resolved trade's line must never be read twice.
#[test]
fn resuming_after_a_clean_resolution_reads_nothing_twice() {
    let log = Log::new();
    let t0 = Instant::now();

    let saved = {
        let mut w = log.watcher(0);
        log.append(BUY);
        w.tick(t0).unwrap();
        let tick = w.tick(t0 + GRACE).unwrap();
        assert_eq!(tick.promoted.len(), 1);
        w.state().offset
    };
    assert_eq!(saved, log.len());

    let mut w = log.watcher(saved);
    let tick = w.tick(t0 + Duration::from_secs(10)).unwrap();
    assert!(tick.promoted.is_empty(), "the buy must not reappear");
}

/// A discard resolves its line too; it must not come back as a fresh
/// detection on the next launch.
#[test]
fn a_discarded_trade_resolves_its_line_and_is_remembered() {
    let log = Log::new();
    let mut w = log.watcher(0);
    let t0 = Instant::now();

    log.append(BUY);
    log.append(ERR_BUY);
    let tick = w.tick(t0).unwrap();
    assert_eq!(tick.discarded.len(), 1);
    assert!(tick.promoted.is_empty());
    assert_eq!(w.status().pending, 0);
    assert_eq!(w.state().offset, log.len(), "resolved, so the offset moves");

    let ParsedLine::Trade(t) = parse_line(BUY).unwrap() else {
        unreachable!()
    };
    assert!(
        w.state().has_processed(&t.id),
        "a discard must be remembered, or a backfill resurrects it"
    );
}

/// Two trades, the second arriving while the first is still held: the floor
/// must stay at the oldest unresolved line, not jump to the newest.
#[test]
fn the_floor_holds_at_the_oldest_pending_line() {
    let log = Log::new();
    let mut w = log.watcher(0);
    let t0 = Instant::now();

    log.append(BUY);
    w.tick(t0).unwrap();
    let floor = w.state().offset;
    assert_eq!(floor, 0);

    log.append(BUY2);
    w.tick(t0 + Duration::from_millis(500)).unwrap();
    assert_eq!(w.status().pending, 2);
    assert_eq!(w.state().offset, floor, "still before the first buy");

    // The first matures, the second has not.
    let tick = w.tick(t0 + Duration::from_millis(3100)).unwrap();
    assert_eq!(tick.promoted.len(), 1);
    assert_eq!(w.status().pending, 1);
    assert!(
        w.state().offset < log.len(),
        "one is still held, so the offset cannot reach the end"
    );

    let tick = w.tick(t0 + Duration::from_millis(3700)).unwrap();
    assert_eq!(tick.promoted.len(), 1);
    assert_eq!(w.state().offset, log.len());
}

// ---------------------------------------------------------------------------
// Rotation, shutdown, diagnostics
// ---------------------------------------------------------------------------

#[test]
fn rotation_resets_the_position_but_keeps_the_duplicate_filter() {
    let log = Log::new();
    let mut w = log.watcher(0);
    let t0 = Instant::now();

    log.append(BUY);
    w.tick(t0).unwrap();
    let tick = w.tick(t0 + GRACE).unwrap();
    assert_eq!(tick.promoted.len(), 1);
    let ParsedLine::Trade(t) = parse_line(BUY).unwrap() else {
        unreachable!()
    };

    // The game relaunched.
    log.truncate();
    log.append(CHATTER);
    let tick = w.tick(t0 + Duration::from_secs(10)).unwrap();
    assert!(tick.rotated);
    assert_eq!(w.status().rotations, 1);
    assert!(
        w.state().has_processed(&t.id),
        "the id filter must survive rotation"
    );
    assert_eq!(w.state().offset, log.len());
}

/// A tick that rotates *and* holds a detection must floor the safe offset at
/// the start of the new file, not at where the batch began.
///
/// `offset_before_batch` is a position in the previous file. Persisting it
/// beside the new file's identity makes `plan_cold_start` see a matching header
/// and an offset within bounds, so the next launch `Resume`s past everything
/// before it. Reachable whenever the poll thread stalls (a sleep/resume) across
/// a game relaunch and the first tick back carries both the rotation and a
/// trade line.
#[test]
fn a_rotation_that_also_holds_a_detection_floors_the_offset_at_zero() {
    let log = Log::new();
    let mut w = log.watcher(0);
    let t0 = Instant::now();

    // Build up a real offset in the pre-rotation file.
    for _ in 0..20 {
        log.append(CHATTER);
    }
    w.tick(t0).unwrap();
    let before_rotation = w.state().offset;
    assert!(before_rotation > 0, "need a non-zero offset to regress on");

    // The game relaunched and traded before we next looked: one tick, both
    // the rotation and a line that goes into the grace window.
    log.truncate();
    log.append(BUY);
    let tick = w.tick(t0 + Duration::from_secs(10)).unwrap();

    assert!(tick.rotated, "the rotation must be seen");
    assert_eq!(tick.promoted.len(), 0, "and the buy is still held");
    assert_eq!(
        w.state().offset,
        0,
        "the floor must be the new file's start, not {before_rotation} from the old one"
    );
}

#[test]
fn shutdown_promotes_what_is_still_held_rather_than_dropping_it() {
    let log = Log::new();
    let mut w = log.watcher(0);
    let t0 = Instant::now();

    log.append(BUY);
    w.tick(t0).unwrap();
    assert_eq!(w.status().pending, 1);

    let out = w.finish();
    assert_eq!(out.promoted.len(), 1);
    assert_eq!(w.status().pending, 0);
    assert!(!w.status().running);
}

#[test]
fn a_missing_log_is_quiet_not_an_error() {
    let log = Log::new();
    let mut w = log.watcher(0);
    let tick = w.tick(Instant::now()).unwrap();
    assert!(tick.is_quiet());
    assert!(!w.status().log_present);
}

/// A class-matching line that fails to parse must surface, never vanish.
#[test]
fn a_malformed_line_surfaces_and_does_not_enter_the_window() {
    let log = Log::new();
    let mut w = log.watcher(0);
    let t0 = Instant::now();

    log.append(MALFORMED);
    let tick = w.tick(t0).unwrap();
    assert_eq!(tick.malformed.len(), 1);
    assert_eq!(w.status().malformed, 1);
    assert_eq!(w.status().pending, 0, "it is not a trade");
    assert_eq!(w.state().offset, log.len());
}

#[test]
fn only_class_matching_lines_are_kept_for_diagnostics() {
    let log = Log::new();
    let mut w = log.watcher(0);
    log.append(CHATTER);
    log.append(BUY);
    log.append(ERR_BUY);
    w.tick(Instant::now()).unwrap();

    let classes: Vec<&str> = w.recent_lines().map(|l| l.class).collect();
    assert_eq!(classes, vec!["trade", "error"]);
    assert_eq!(w.status().lines_seen, 3);
    assert_eq!(w.status().class_matched, 2);
}

/// The whole current session, driven through the live watcher a line at a
/// time, as it would arrive from the game.
#[test]
fn the_real_session_through_the_live_watcher_yields_six_trades() {
    let fixture = include_str!("../../tests/fixtures/session-12913470-current.log");
    let log = Log::new();
    let mut w = log.watcher(0);
    let t0 = Instant::now();

    let mut promoted = Vec::new();
    let mut discarded = 0;
    for (i, line) in fixture.lines().enumerate() {
        log.append(line);
        let tick = w.tick(t0 + Duration::from_millis(i as u64)).unwrap();
        promoted.extend(tick.promoted);
        discarded += tick.discarded.len();
        assert!(tick.malformed.is_empty(), "unparseable line: {line}");
    }
    // Let every window elapse.
    let tick = w.tick(t0 + Duration::from_secs(600)).unwrap();
    promoted.extend(tick.promoted);

    assert_eq!(discarded, 1, "the failed buy");
    assert_eq!(promoted.len(), 6, "6 trades, not 7");
    assert_eq!(
        promoted.iter().filter(|t| t.line.kind == TradeKind::Buy).count(),
        3,
        "4 buy lines, one of which failed"
    );
    assert_eq!(w.state().offset, log.len(), "everything resolved");
    assert_eq!(w.state().processed_ids.len(), 7, "6 promoted + 1 discarded");
}

// ---------------------------------------------------------------------------
// Cold start
// ---------------------------------------------------------------------------

fn no_count(_: &Path) -> usize {
    panic!("count_trades must only be called for a first run")
}

#[test]
fn cold_start_with_no_log_is_not_an_error() {
    let log = Log::new();
    assert_eq!(
        plan_cold_start(&log.path, &WatchState::default(), no_count),
        ColdStart::NoLog
    );
}

/// The first run must never silently import a session. It reports a count and
/// where start-from-now would begin, and lets the user choose.
#[test]
fn a_first_run_reports_a_count_instead_of_importing() {
    let log = Log::new();
    log.append(BUY);
    let got = plan_cold_start(&log.path, &WatchState::default(), |p| {
        let text = std::fs::read_to_string(p).unwrap();
        scan::scan(&text, 0).trades.len()
    });
    assert_eq!(
        got,
        ColdStart::FirstRun {
            trades: 1,
            end_offset: log.len()
        }
    );
}

#[test]
fn the_common_case_resumes_without_reprocessing() {
    let log = Log::new();
    log.append(BUY);
    let mut state = WatchState::default();
    state.set_file(&log.path, tail::identity_of(&log.path));
    state.offset = log.len();

    assert_eq!(
        plan_cold_start(&log.path, &state, no_count),
        ColdStart::Resume { offset: log.len() }
    );
}

#[test]
fn a_shorter_file_than_our_offset_means_backfill() {
    let log = Log::new();
    log.append(BUY);
    let mut state = WatchState::default();
    state.set_file(&log.path, tail::identity_of(&log.path));
    state.offset = log.len() + 1_000;

    assert_eq!(
        plan_cold_start(&log.path, &state, no_count),
        ColdStart::Backfill {
            reason: BackfillReason::Truncated
        }
    );
}

#[test]
fn a_changed_path_means_backfill() {
    let log = Log::new();
    log.append(BUY);
    let mut state = WatchState::default();
    state.set_file(Path::new("E:\\somewhere\\else\\Game.log"), None);
    state.offset = 10;

    assert_eq!(
        plan_cold_start(&log.path, &state, no_count),
        ColdStart::Backfill {
            reason: BackfillReason::DifferentPath
        }
    );
}

/// A session that happened while we were closed is recovered, and the trades
/// already dealt with are not offered again.
#[test]
fn backfill_filters_by_id_and_by_the_log_watermark() {
    let text = format!("{BUY}\n{BUY2}\n");
    let report = scan::scan(&text, 0);
    assert_eq!(report.trades.len(), 2);

    // Nothing known: both are new.
    assert_eq!(backfill(&report, &WatchState::default()).len(), 2);

    // The first was already enqueued.
    let mut state = WatchState::default();
    state.mark_processed(report.trades[0].line.id.clone());
    let got = backfill(&report, &state);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].line.id, report.trades[1].line.id);

    // The watermark alone covers the case where the id set has rolled over.
    let mut state = WatchState::default();
    state.observe_log_ts(&report.trades[0].line.log_timestamp);
    let got = backfill(&report, &state);
    assert_eq!(got.len(), 1, "the watermark is exclusive of what was seen");
    assert_eq!(got[0].line.id, report.trades[1].line.id);

    // Both seen.
    let mut state = WatchState::default();
    state.observe_log_ts(&report.trades[1].line.log_timestamp);
    assert!(backfill(&report, &state).is_empty());
}
