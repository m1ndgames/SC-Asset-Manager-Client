//! The grace window: deciding, live, whether a detected trade actually
//! happened.
//!
//! **Success is never logged.** A completed transaction produces no
//! confirmation line at all; only failures are logged. So a request line *is*
//! the trade — provisionally. Each detection is held briefly, and discarded if
//! a matching failure arrives inside the window.
//!
//! Be honest about what this is: **correlation by proximity, not by
//! transaction id.** The failure line carries only `playerId`, `result` and
//! `type[Buying|Selling]` — no `resourceGUID`, no `shopId`. Matching is
//! therefore oldest-unmatched-of-that-kind. That is correct for every case in
//! the corpus, where transactions are minutes apart, but two buys fired within
//! the window at different kiosks could mis-attribute. The mitigation is
//! visibility, not correctness: every discard is reported with its raw line so
//! it can be restored.
//!
//! The clock is a parameter, never read from inside. That keeps the window
//! logic as testable as the parser.

use std::time::{Duration, Instant};

use super::parse::{ParsedLine, TradeLine, TxErrorLine};
use super::scan::{DetectedTrade, DiscardedTrade};
use crate::ids::TradeKind;

/// What came out of the correlator.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Survived the window. This is a real trade, ready for review.
    Promoted(Box<DetectedTrade>),
    /// Retracted by a failure line.
    Discarded(Box<DiscardedTrade>),
    /// A failure line that matched nothing. Surfaced rather than dropped — it
    /// means either a transaction we never saw requested, or a window too
    /// short.
    UnmatchedError(TxErrorLine),
}

#[derive(Debug)]
struct Pending {
    trade: TradeLine,
    detected_at: Instant,
    location_hint: Option<String>,
}

#[derive(Debug)]
pub struct Correlator {
    hold: Duration,
    pending: Vec<Pending>,
    /// Most recent non-null `LocationId`. A within-session grouping key: it
    /// turns the location question from once-per-trade into once-per-place.
    last_location: Option<String>,
}

impl Correlator {
    pub fn new(hold: Duration) -> Self {
        Self {
            hold,
            pending: Vec::new(),
            last_location: None,
        }
    }

    pub fn hold(&self) -> Duration {
        self.hold
    }

    /// How many detections are waiting out their window.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    pub fn last_location(&self) -> Option<&str> {
        self.last_location.as_deref()
    }

    /// Feed one parsed line.
    ///
    /// Returns immediately-resolved events — a failure line can discard a
    /// pending trade at once. Promotions come from [`Correlator::tick`].
    pub fn offer(&mut self, parsed: ParsedLine, now: Instant) -> Vec<Event> {
        match parsed {
            ParsedLine::Trade(t) => {
                self.pending.push(Pending {
                    trade: *t,
                    detected_at: now,
                    location_hint: self.last_location.clone(),
                });
                Vec::new()
            }
            ParsedLine::LocationInfo(l) => {
                if !l.is_null() {
                    self.last_location = Some(l.location_id);
                }
                Vec::new()
            }
            ParsedLine::TxError(e) => vec![self.resolve_error(e, now)],
            ParsedLine::Ignored { .. } | ParsedLine::Malformed { .. } => Vec::new(),
        }
    }

    fn resolve_error(&mut self, e: TxErrorLine, now: Instant) -> Event {
        let want = match e.tx_type.as_deref() {
            Some("Buying") => Some(TradeKind::Buy),
            Some("Selling") => Some(TradeKind::Sell),
            // A failure with no type can match either; still FIFO.
            _ => None,
        };

        // Oldest first, of the right kind, still inside its window.
        let hit = self.pending.iter().position(|p| {
            if let Some(k) = want {
                if p.trade.kind != k {
                    return false;
                }
            }
            now.duration_since(p.detected_at) <= self.hold
        });

        match hit {
            Some(i) => {
                let p = self.pending.remove(i);
                let delta_ms = delta_from_timestamps(&p.trade, &e);
                Event::Discarded(Box::new(DiscardedTrade {
                    trade: p.trade,
                    error: e,
                    delta_ms,
                }))
            }
            None => Event::UnmatchedError(e),
        }
    }

    /// Release everything whose window has elapsed.
    pub fn tick(&mut self, now: Instant) -> Vec<Event> {
        let hold = self.hold;
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.pending.len() {
            if now.duration_since(self.pending[i].detected_at) >= hold {
                let p = self.pending.remove(i);
                out.push(Event::Promoted(Box::new(DetectedTrade {
                    line: p.trade,
                    location_id_hint: p.location_hint,
                })));
            } else {
                i += 1;
            }
        }
        out
    }

    /// Release everything immediately, whatever its age.
    ///
    /// Used on shutdown: a detection still inside its window would otherwise
    /// be lost, and the offset has not advanced past it, so the alternative is
    /// re-reading it next launch. Promoting is the honest choice — the user
    /// can still reject it.
    pub fn drain(&mut self) -> Vec<Event> {
        self.pending
            .drain(..)
            .map(|p| {
                Event::Promoted(Box::new(DetectedTrade {
                    line: p.trade,
                    location_id_hint: p.location_hint,
                }))
            })
            .collect()
    }
}

/// Gap between the request and its failure, from the log's own timestamps
/// rather than wall clock — the log is the authority on when things happened.
fn delta_from_timestamps(trade: &TradeLine, err: &TxErrorLine) -> i64 {
    use chrono::{DateTime, Utc};
    let a = trade.log_timestamp.parse::<DateTime<Utc>>();
    let b = err.log_timestamp.parse::<DateTime<Utc>>();
    match (a, b) {
        (Ok(a), Ok(b)) => (b - a).num_milliseconds(),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::parse::parse_line;

    const BUY: &str = "<2026-11-01T09:11:29.754Z> [Notice] <CEntityComponentCommodityUIProvider::SendCommodityBuyRequest> Sending SShopCommodityBuyRequest - playerId[214115750165] shopId[863690278779] shopName[SCShop_Admin_lt_base_g] kioskId[863690278778] price[190453.000000] shopPricePerCentiSCU[1.889409] resourceGUID[b999ef65-35be-45bf-908a-5eac6e06ba12] autoLoading[1] quantity[100800.000000 cSCU] Cargo Box Data: boxSize[16.000000] | unitAmount[63] [Team_CoreGameplayFeatures][Shops][UI]";
    const BUY2: &str = "<2026-11-01T09:42:57.034Z> [Notice] <CEntityComponentCommodityUIProvider::SendCommodityBuyRequest> Sending SShopCommodityBuyRequest - playerId[214115750165] shopId[863690742444] shopName[SCShop_Admin_lt_base_g] kioskId[863690742440] price[151296.000000] shopPricePerCentiSCU[3.152000] resourceGUID[0d41371c-3cde-4939-8583-cd718f5edd60] autoLoading[1] quantity[48000.000000 cSCU] Cargo Box Data: boxSize[8.000000] | unitAmount[60] [Team_CoreGameplayFeatures][Shops][UI]";
    const SELL: &str = "<2026-11-01T11:09:47.325Z> [Notice] <CEntityComponentCommodityUIProvider::SendCommoditySellRequest> Sending SShopCommoditySellRequest - playerId[214115750165] shopId[862401115218] shopName[SCShop_Admin_lt_base_g] kioskId[862401115214] amount[317120.000000] resourceGUID[ea78052a-11b2-484e-a7ee-d19ed906a037] autoLoading[1] quantity[640] transactionMode[ResourceContainer] Cargo Box Data:  [boxSize[8] | unitAmount[80]] [Team_CoreGameplayFeatures][Shops][UI]";
    const ERR_BUY: &str = "<2026-11-01T09:11:30.938Z> [Error] <CEntityComponentCommodityUIProvider::RmToken_CommodityTransactionResponse> Commodity Transaction Response Error - playerId[214115750165] result[CargoCreationFailed] type[Buying] [Team_CoreGameplayFeatures][Shops]";
    const ERR_SELL: &str = "<2026-11-01T11:09:48.000Z> [Error] <CEntityComponentCommodityUIProvider::RmToken_CommodityTransactionResponse> Commodity Transaction Response Error - playerId[214115750165] result[SomethingFailed] type[Selling] [Team_CoreGameplayFeatures][Shops]";
    const LOC: &str = "<2026-11-01T09:10:39.576Z> [Notice] <CEntityComponentCommodityUIProvider::ClSetSelectedPlayerLocationInfo> Selecting location with item(s) for LocationId[910662854419] [Team_CoreGameplayFeatures][Shops]";

    fn p(line: &str) -> ParsedLine {
        parse_line(line).expect("fixture parses")
    }

    fn correlator() -> (Correlator, Instant) {
        (Correlator::new(Duration::from_millis(3000)), Instant::now())
    }

    #[test]
    fn a_trade_with_no_error_is_promoted_after_the_window() {
        let (mut c, t0) = correlator();
        assert!(c.offer(p(BUY), t0).is_empty());

        // Too early.
        assert!(c.tick(t0 + Duration::from_millis(2999)).is_empty());
        assert_eq!(c.pending_len(), 1);

        let out = c.tick(t0 + Duration::from_millis(3000));
        assert_eq!(out.len(), 1);
        match &out[0] {
            Event::Promoted(d) => assert_eq!(d.line.kind, TradeKind::Buy),
            other => panic!("expected promotion, got {other:?}"),
        }
        assert_eq!(c.pending_len(), 0);
    }

    #[test]
    fn an_error_inside_the_window_discards_the_trade() {
        let (mut c, t0) = correlator();
        c.offer(p(BUY), t0);

        let out = c.offer(p(ERR_BUY), t0 + Duration::from_millis(1184));
        assert_eq!(out.len(), 1);
        match &out[0] {
            Event::Discarded(d) => {
                assert_eq!(d.error.result, "CargoCreationFailed");
                // Delta comes from the log's timestamps, not wall clock.
                assert_eq!(d.delta_ms, 1184);
            }
            other => panic!("expected discard, got {other:?}"),
        }
        // And it must never be promoted afterwards.
        assert!(c.tick(t0 + Duration::from_secs(10)).is_empty());
    }

    #[test]
    fn an_error_after_the_window_does_not_retract_a_promoted_trade() {
        let (mut c, t0) = correlator();
        c.offer(p(BUY), t0);
        let promoted = c.tick(t0 + Duration::from_millis(3000));
        assert_eq!(promoted.len(), 1);

        let out = c.offer(p(ERR_BUY), t0 + Duration::from_millis(3500));
        assert!(matches!(out[0], Event::UnmatchedError(_)));
    }

    /// The documented ambiguity: two buys inside one window, one failure.
    /// FIFO means the older one is discarded.
    #[test]
    fn two_requests_one_error_is_fifo() {
        let (mut c, t0) = correlator();
        c.offer(p(BUY), t0);
        c.offer(p(BUY2), t0 + Duration::from_millis(100));

        let out = c.offer(p(ERR_BUY), t0 + Duration::from_millis(200));
        let Event::Discarded(d) = &out[0] else {
            panic!("expected discard")
        };
        // BUY is the older of the two.
        assert_eq!(d.trade.id, {
            let ParsedLine::Trade(t) = p(BUY) else {
                unreachable!()
            };
            t.id
        });

        // The younger one still promotes.
        let later = c.tick(t0 + Duration::from_secs(5));
        assert_eq!(later.len(), 1);
    }

    #[test]
    fn an_error_matches_only_its_own_kind() {
        let (mut c, t0) = correlator();
        c.offer(p(BUY), t0);
        c.offer(p(SELL), t0 + Duration::from_millis(10));

        // A Selling failure must not take the buy.
        let out = c.offer(p(ERR_SELL), t0 + Duration::from_millis(20));
        let Event::Discarded(d) = &out[0] else {
            panic!("expected discard, got {out:?}")
        };
        assert_eq!(d.trade.kind, TradeKind::Sell);

        let promoted = c.tick(t0 + Duration::from_secs(5));
        assert_eq!(promoted.len(), 1);
        let Event::Promoted(p0) = &promoted[0] else {
            panic!()
        };
        assert_eq!(p0.line.kind, TradeKind::Buy);
    }

    #[test]
    fn an_error_with_no_request_is_surfaced() {
        let (mut c, t0) = correlator();
        let out = c.offer(p(ERR_BUY), t0);
        assert!(matches!(out[0], Event::UnmatchedError(_)));
    }

    #[test]
    fn location_info_attaches_to_later_trades_only() {
        let (mut c, t0) = correlator();
        c.offer(p(BUY), t0);
        c.offer(p(LOC), t0 + Duration::from_millis(1));
        c.offer(p(BUY2), t0 + Duration::from_millis(2));

        let out = c.tick(t0 + Duration::from_secs(5));
        assert_eq!(out.len(), 2);
        let hints: Vec<Option<String>> = out
            .iter()
            .map(|e| match e {
                Event::Promoted(d) => d.location_id_hint.clone(),
                _ => panic!(),
            })
            .collect();
        assert_eq!(hints[0], None, "the earlier trade predates the hint");
        assert_eq!(hints[1], Some("910662854419".into()));
    }

    #[test]
    fn null_location_sentinel_is_ignored() {
        let (mut c, t0) = correlator();
        let null_loc = LOC.replace("LocationId[910662854419]", "LocationId[0]");
        c.offer(p(&null_loc), t0);
        assert_eq!(c.last_location(), None);
    }

    #[test]
    fn malformed_and_ignored_lines_do_not_disturb_the_window() {
        let (mut c, t0) = correlator();
        c.offer(p(BUY), t0);
        c.offer(
            ParsedLine::Ignored {
                method: "AddPlayerCommodityItem".into(),
            },
            t0,
        );
        c.offer(
            ParsedLine::Malformed {
                raw: "x".into(),
                method: "SendCommodityBuyRequest".into(),
                missing: vec!["price".into()],
            },
            t0,
        );
        assert_eq!(c.pending_len(), 1);
        assert_eq!(c.tick(t0 + Duration::from_secs(5)).len(), 1);
    }

    /// On shutdown a detection still inside its window must not evaporate.
    #[test]
    fn drain_releases_everything_still_held() {
        let (mut c, t0) = correlator();
        c.offer(p(BUY), t0);
        c.offer(p(SELL), t0);
        assert_eq!(c.pending_len(), 2);
        let out = c.drain();
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|e| matches!(e, Event::Promoted(_))));
        assert_eq!(c.pending_len(), 0);
    }

    /// The whole current session, driven through the live correlator: 4 buys,
    /// 3 sells, 1 failure -> 6 trades.
    #[test]
    fn the_real_session_yields_six_trades() {
        let fixture = include_str!("../../tests/fixtures/session-12913470-current.log");
        let (mut c, t0) = correlator();
        let mut promoted = 0;
        let mut discarded = 0;

        // Feed every line at the same instant, then let the window elapse —
        // the failure arrives well inside 3 s of its buy in the real log.
        for (i, line) in fixture.lines().enumerate() {
            if let Some(parsed) = parse_line(line) {
                for ev in c.offer(parsed, t0 + Duration::from_millis(i as u64)) {
                    match ev {
                        Event::Discarded(_) => discarded += 1,
                        Event::UnmatchedError(e) => panic!("unmatched failure: {e:?}"),
                        Event::Promoted(_) => promoted += 1,
                    }
                }
            }
        }
        promoted += c.tick(t0 + Duration::from_secs(60)).len();

        assert_eq!(discarded, 1, "the failed buy");
        assert_eq!(promoted, 6, "6 trades, not 7");
    }
}
