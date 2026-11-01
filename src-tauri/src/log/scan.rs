//! A whole-file pass over a Game.log.
//!
//! This is the offline twin of the live watcher: every error line that will
//! ever exist is already in the file, so correlation runs with a zero hold and
//! the result is deterministic. Phase 1 uses it to prove parsing against real
//! logs before any tailer exists; PLAN.md §3.6 reuses it for backfill.
//!
//! Correlation is **by proximity, not by transaction id** — the error line
//! carries only `playerId`, `result` and `type[Buying|Selling]`, no
//! `resourceGUID` and no `shopId`. Matching is therefore oldest-unmatched of
//! that kind within the window. Correct for every case observed, where
//! transactions are minutes apart; two buys inside the window at different
//! kiosks could mis-attribute. The mitigation is visibility, not correctness:
//! every discard is reported with its raw line so it can be restored.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::parse::{parse_line, ParsedLine, TradeLine, TxErrorLine};
use crate::ids::TradeKind;

/// Default grace period. A request line is provisionally a trade; an error
/// within this window retracts it.
pub const DEFAULT_HOLD_MS: i64 = 3_000;

/// A trade that survived correlation, plus the hints gathered around it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedTrade {
    #[serde(flatten)]
    pub line: TradeLine,
    /// The most recent non-null `LocationId` seen before this trade. A
    /// within-session grouping key, not a stable identifier: see
    /// parse::LocationInfoLine.
    pub location_id_hint: Option<String>,
}

/// A trade that was retracted by a following error line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscardedTrade {
    pub trade: TradeLine,
    pub error: TxErrorLine,
    pub delta_ms: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    pub total_lines: usize,
    /// Lines containing the class token — the ones this client looks at.
    pub class_matched: usize,
    pub buys: usize,
    pub sells: usize,
    pub errors: usize,
    /// Trades that survived the grace window, in log order.
    pub trades: Vec<DetectedTrade>,
    /// Trades retracted by an error line.
    pub discarded: Vec<DiscardedTrade>,
    /// Class-matching lines that could not be parsed. Must stay empty; a
    /// non-empty list is the signal that a game patch changed the format.
    pub malformed: Vec<ParsedLine>,
    /// Error lines that matched no pending request.
    pub unmatched_errors: Vec<TxErrorLine>,
    /// Distinct non-null `LocationId`s seen.
    pub location_ids: Vec<String>,
    /// Distinct `shopName`s seen on trade lines.
    pub shop_names: Vec<String>,
    /// Trades whose summed cargo-box parity disagrees with `quantity`.
    pub parity_mismatches: usize,
    /// Buys whose price cross-check drifts by more than 0.1%.
    pub price_drift: usize,
}

fn ts(s: &str) -> Option<DateTime<Utc>> {
    s.parse::<DateTime<Utc>>().ok()
}

/// Scan a whole log.
///
/// `hold_ms` is the grace window. Pass [`DEFAULT_HOLD_MS`] to mirror live
/// behaviour; 0 still works here because the whole file is present, but a
/// non-zero value is what a live tailer would apply.
pub fn scan(contents: &str, hold_ms: i64) -> ScanReport {
    let mut r = ScanReport::default();
    // Pending requests awaiting their grace window, oldest first, per kind.
    let mut pending: Vec<(TradeLine, Option<String>)> = Vec::new();
    let mut discarded_idx: Vec<usize> = Vec::new();
    let mut last_location: Option<String> = None;
    let mut seen_locations: Vec<String> = Vec::new();
    let mut seen_shops: Vec<String> = Vec::new();
    let mut errors_for: HashMap<usize, TxErrorLine> = HashMap::new();

    for line in contents.lines() {
        r.total_lines += 1;
        let Some(parsed) = parse_line(line) else {
            continue;
        };
        r.class_matched += 1;

        match parsed {
            ParsedLine::Trade(t) => {
                match t.kind {
                    TradeKind::Buy => r.buys += 1,
                    TradeKind::Sell => r.sells += 1,
                }
                if let Some(name) = &t.shop_name {
                    if !seen_shops.iter().any(|s| s == name) {
                        seen_shops.push(name.clone());
                    }
                }
                if t.parity.is_mismatch() {
                    r.parity_mismatches += 1;
                }
                if t.price_cross_check.is_some_and(|d| d > 0.001) {
                    r.price_drift += 1;
                }
                pending.push((*t, last_location.clone()));
            }
            ParsedLine::TxError(e) => {
                r.errors += 1;
                // FIFO: oldest unmatched request of the same kind still inside
                // the window.
                let want = match e.tx_type.as_deref() {
                    Some("Buying") => Some(TradeKind::Buy),
                    Some("Selling") => Some(TradeKind::Sell),
                    _ => None,
                };
                let err_at = ts(&e.log_timestamp);
                let hit = pending.iter().enumerate().position(|(i, (t, _))| {
                    if discarded_idx.contains(&i) {
                        return false;
                    }
                    if let Some(k) = want {
                        if t.kind != k {
                            return false;
                        }
                    }
                    match (ts(&t.log_timestamp), err_at) {
                        (Some(a), Some(b)) => {
                            let d = (b - a).num_milliseconds();
                            d >= 0 && d <= hold_ms
                        }
                        // Without parseable timestamps, fall back to order.
                        _ => true,
                    }
                });
                match hit {
                    Some(i) => {
                        let delta = match (ts(&pending[i].0.log_timestamp), err_at) {
                            (Some(a), Some(b)) => (b - a).num_milliseconds(),
                            _ => 0,
                        };
                        discarded_idx.push(i);
                        errors_for.insert(i, e);
                        let _ = delta;
                    }
                    None => r.unmatched_errors.push(e),
                }
            }
            ParsedLine::LocationInfo(l) => {
                if !l.is_null() {
                    if !seen_locations.contains(&l.location_id) {
                        seen_locations.push(l.location_id.clone());
                    }
                    last_location = Some(l.location_id);
                }
            }
            ParsedLine::Ignored { .. } => {}
            m @ ParsedLine::Malformed { .. } => r.malformed.push(m),
        }
    }

    for (i, (t, loc)) in pending.into_iter().enumerate() {
        if discarded_idx.contains(&i) {
            let error = errors_for.remove(&i).expect("recorded with the index");
            let delta_ms = match (ts(&t.log_timestamp), ts(&error.log_timestamp)) {
                (Some(a), Some(b)) => (b - a).num_milliseconds(),
                _ => 0,
            };
            r.discarded.push(DiscardedTrade {
                trade: t,
                error,
                delta_ms,
            });
        } else {
            r.trades.push(DetectedTrade {
                line: t,
                location_id_hint: loc,
            });
        }
    }

    r.location_ids = seen_locations;
    r.shop_names = seen_shops;
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUY: &str = "<2026-11-01T09:11:29.754Z> [Notice] <CEntityComponentCommodityUIProvider::SendCommodityBuyRequest> Sending SShopCommodityBuyRequest - playerId[214115750165] shopId[863690278779] shopName[SCShop_Admin_lt_base_g] kioskId[863690278778] price[190453.000000] shopPricePerCentiSCU[1.889409] resourceGUID[b999ef65-35be-45bf-908a-5eac6e06ba12] autoLoading[1] quantity[100800.000000 cSCU] Cargo Box Data: boxSize[16.000000] | unitAmount[63] [Team_CoreGameplayFeatures][Shops][UI]";
    const ERR: &str = "<2026-11-01T09:11:30.938Z> [Error] <CEntityComponentCommodityUIProvider::RmToken_CommodityTransactionResponse> Commodity Transaction Response Error - playerId[214115750165] result[CargoCreationFailed] type[Buying] [Team_CoreGameplayFeatures][Shops]";
    /// Same buy, but far later, so the same error cannot retract it.
    const BUY_LATE: &str = "<2026-11-01T09:40:00.000Z> [Notice] <CEntityComponentCommodityUIProvider::SendCommodityBuyRequest> Sending SShopCommodityBuyRequest - playerId[214115750165] shopId[863690278779] shopName[SCShop_Admin_lt_base_g] kioskId[863690278778] price[190453.000000] shopPricePerCentiSCU[1.889409] resourceGUID[b999ef65-35be-45bf-908a-5eac6e06ba12] autoLoading[1] quantity[100800.000000 cSCU] Cargo Box Data: boxSize[16.000000] | unitAmount[63] [Team_CoreGameplayFeatures][Shops][UI]";
    const LOC: &str = "<2026-11-01T09:10:39.576Z> [Notice] <CEntityComponentCommodityUIProvider::ClSetSelectedPlayerLocationInfo> Selecting location with item(s) for LocationId[910662854419] [Team_CoreGameplayFeatures][Shops]";

    #[test]
    fn error_inside_the_window_discards_the_trade() {
        let r = scan(&format!("{BUY}\n{ERR}\n"), DEFAULT_HOLD_MS);
        assert_eq!(r.buys, 1);
        assert_eq!(r.errors, 1);
        assert!(r.trades.is_empty(), "the failed buy must not survive");
        assert_eq!(r.discarded.len(), 1);
        assert_eq!(r.discarded[0].delta_ms, 1184);
    }

    #[test]
    fn error_outside_the_window_leaves_the_trade() {
        let r = scan(&format!("{BUY_LATE}\n{ERR}\n"), DEFAULT_HOLD_MS);
        assert_eq!(r.trades.len(), 1);
        assert_eq!(r.unmatched_errors.len(), 1);
    }

    #[test]
    fn error_with_no_request_is_recorded_not_dropped() {
        let r = scan(ERR, DEFAULT_HOLD_MS);
        assert_eq!(r.unmatched_errors.len(), 1);
        assert!(r.trades.is_empty());
    }

    #[test]
    fn location_hint_attaches_to_a_following_trade() {
        let r = scan(&format!("{LOC}\n{BUY_LATE}\n"), DEFAULT_HOLD_MS);
        assert_eq!(r.trades.len(), 1);
        assert_eq!(
            r.trades[0].location_id_hint.as_deref(),
            Some("910662854419")
        );
        assert_eq!(r.location_ids, vec!["910662854419".to_string()]);
    }

    #[test]
    fn scanning_the_same_line_twice_yields_the_same_id() {
        let r = scan(&format!("{BUY_LATE}\n{BUY_LATE}\n"), DEFAULT_HOLD_MS);
        assert_eq!(r.trades.len(), 2);
        assert_eq!(
            r.trades[0].line.id, r.trades[1].line.id,
            "deterministic ids collapse replays into one record downstream"
        );
    }
}
