//! Pure `&str -> ParsedLine`. No I/O, no clock — so this module is testable
//! entirely from text fixtures, which is why it is the highest-value test
//! target in the project.
//!
//! Strategy is **classify, then sweep** (PLAN.md §3.5), never one monolithic
//! regex. The log already formats the same field differently between buy and
//! sell lines, and has stayed that way across ten months of game builds; a
//! rigid pattern would break on the next patch. A class-matching line we cannot
//! parse becomes [`ParsedLine::Malformed`] and surfaces in Diagnostics — it is
//! **never** silently dropped, because that is exactly how a game patch breaks
//! the watcher invisibly.
//!
//! One correction to PLAN.md §3.5, learned from the corpus: a line may carry
//! **several** `Cargo Box Data` groups (up to four observed), so the generic
//! field sweep alone is lossy — a map keeps only the last `boxSize`. The groups
//! are therefore extracted in order, as a list. See [`parity_of`].

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::ids::{self, TradeKind};
use crate::units::{self, CargoUnit};

/// Cheap rejection gate. ~19 000 of 20 000 lines fail this in nanoseconds.
const CLASS_TOKEN: &str = "CEntityComponentCommodityUIProvider::";

/// Generic `field[value]` sweep. Tolerates field reordering and new fields.
/// Field names may contain digits/underscores after the first letter; the value
/// may not contain brackets, which is what lets the nested
/// `[boxSize[8] | unitAmount[80]]` form fall out without special-casing.
///
/// Values are collapsed into a map for convenience, which is lossy for
/// repeated keys — hence [`BOX_GROUP`] below.
static FIELD_SWEEP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([A-Za-z][A-Za-z0-9_]*)\[([^\[\]]*)\]").unwrap());

/// One `boxSize[..] | unitAmount[..]` pair. Matched separately and in order,
/// because a single line can carry several and their contributions add up.
static BOX_GROUP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"boxSize\[([0-9.]+)\]\s*\|\s*unitAmount\[([0-9.]+)\]").unwrap()
});

/// One cargo box group from `Cargo Box Data`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CargoBoxGroup {
    pub box_size: f64,
    pub unit_amount: f64,
}

impl CargoBoxGroup {
    pub fn scu(self) -> f64 {
        self.box_size * self.unit_amount
    }
}

/// Whether `sum(boxSize_i * unitAmount_i) == quantity` on this line.
///
/// The early-warning system for log-format drift, so its
/// false-positive rate matters: a check that cries wolf trains the user to
/// ignore amber and stops being a signal at all.
///
/// Measured over 91 real trade lines spanning game builds 10989003 (Dec 2025)
/// to 12913470 (Oct 2026):
///
/// | line kind               | holds |
/// |-------------------------|-------|
/// | buy                     | 21/21 |
/// | sell, Location          | 21/21 |
/// | sell, ResourceContainer | 48/49 |
///
/// The sole exception is one `boxSize[16] | unitAmount[7]` line with
/// `quantity[576]` (576/16 = 36 boxes, so `unitAmount` undercounts). At 90/91
/// the check earns its place; a mismatch is worth showing.
///
/// Note it must be summed. Taking a single `boxSize`/`unitAmount` pair — as a
/// naive field map does — makes it appear to fail on ~11% of sells, because
/// mixed-cargo sells list one group per box size.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ParityCheck {
    Ok,
    #[serde(rename_all = "camelCase")]
    Mismatch { expected_scu: f64, actual_scu: f64 },
    #[serde(rename_all = "camelCase")]
    NotApplicable { reason: String },
}

impl ParityCheck {
    pub fn is_mismatch(&self) -> bool {
        matches!(self, ParityCheck::Mismatch { .. })
    }
}

/// One successfully parsed buy or sell request line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TradeLine {
    pub kind: TradeKind,
    /// The deterministic record id. INTEGRATIONS.md §6.
    pub id: String,
    /// Verbatim text between the first `<` and `>`. Kept as a string because
    /// the id hashes it unparsed.
    pub log_timestamp: String,
    pub resource_guid: String,
    pub shop_id: String,
    pub shop_name: Option<String>,
    pub kiosk_id: Option<String>,
    /// Verbatim `quantity[...]` text with the unit token stripped, e.g.
    /// `100800.000000`. Never reformatted — it is part of the id.
    pub quantity_raw: String,
    /// Quantity in SCU. Buy lines log cSCU, sell lines log SCU; this is
    /// normalised. `quantity` is always authoritative — never derive it from
    /// the cargo box groups.
    pub quantity_scu: f64,
    /// aUEC per SCU, never rounded.
    pub unit_price_per_scu: f64,
    /// Total aUEC for the transaction as the log states it.
    pub total_auec: f64,
    /// Every `boxSize | unitAmount` group, in order. Informational: the
    /// transaction total comes from `quantity`.
    pub box_groups: Vec<CargoBoxGroup>,
    /// Present on sell lines only: `ResourceContainer` or `Location`.
    pub transaction_mode: Option<String>,
    pub parity: ParityCheck,
    /// Buy lines only: relative disagreement between
    /// `shopPricePerCentiSCU x 100` and `price / quantityScu`. Within 0.1% on
    /// 21/21 real buys, so it is a second sound drift detector.
    pub price_cross_check: Option<f64>,
    /// Every swept `field[value]` pair, for Diagnostics. Lossy for repeated
    /// keys; use `box_groups` for cargo data.
    pub fields: BTreeMap<String, String>,
}

/// A failure response. Success is never logged, so a request line *is* the
/// trade — provisionally — and one of these inside the grace window retracts
/// it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TxErrorLine {
    pub log_timestamp: String,
    /// `CargoCreationFailed` etc.
    pub result: String,
    /// `Buying` or `Selling` — the only thing tying an error to a request,
    /// which is why correlation is FIFO-by-kind and inherently ambiguous.
    pub tx_type: Option<String>,
    pub player_id: Option<String>,
}

/// `ClSetSelectedPlayerLocationInfo` — the player selecting a shop location.
///
/// `LocationId` is an entity id, not a stable key: across 274 archived sessions
/// most values appear in one session only, and `LocationId[0]` is a null
/// sentinel. It is still useful, because it groups every kiosk at one place
/// within a session, turning the location question from once-per-trade into
/// roughly once-per-place.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocationInfoLine {
    pub log_timestamp: String,
    pub location_id: String,
}

impl LocationInfoLine {
    /// `LocationId[0]` is the null sentinel, not a place.
    pub fn is_null(&self) -> bool {
        self.location_id.trim() == "0"
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ParsedLine {
    /// Boxed: `parse_line` is called once per log line — ~20 000 per session —
    /// and an unboxed `TradeLine` would make every one of those returns carry a
    /// 312-byte stack slot, including the overwhelming majority that match
    /// nothing. The allocation happens only for the handful of real trades.
    Trade(Box<TradeLine>),
    TxError(TxErrorLine),
    LocationInfo(LocationInfoLine),
    /// Class-matching but a method this client does not act on.
    Ignored { method: String },
    /// Class-matching, recognised method, but required fields are missing.
    /// Surfaces loudly rather than being dropped.
    #[serde(rename_all = "camelCase")]
    Malformed {
        raw: String,
        method: String,
        missing: Vec<String>,
    },
}

/// Verbatim text between the first `<` and the following `>`.
fn extract_log_timestamp(line: &str) -> Option<&str> {
    let start = line.find('<')? + 1;
    let end = start + line[start..].find('>')?;
    Some(&line[start..end])
}

/// The `<Class::Method>` token's method part.
fn extract_method(line: &str) -> Option<&str> {
    let at = line.find(CLASS_TOKEN)? + CLASS_TOKEN.len();
    let rest = &line[at..];
    let end = rest.find('>')?;
    Some(&rest[..end])
}

fn sweep_fields(line: &str) -> BTreeMap<String, String> {
    FIELD_SWEEP
        .captures_iter(line)
        .map(|c| (c[1].to_string(), c[2].to_string()))
        .collect()
}

fn sweep_box_groups(line: &str) -> Vec<CargoBoxGroup> {
    BOX_GROUP
        .captures_iter(line)
        .filter_map(|c| {
            Some(CargoBoxGroup {
                box_size: c[1].parse().ok()?,
                unit_amount: c[2].parse().ok()?,
            })
        })
        .collect()
}

fn num(fields: &BTreeMap<String, String>, key: &str) -> Option<f64> {
    fields.get(key)?.trim().parse::<f64>().ok()
}

/// `sum(boxSize_i * unitAmount_i)` against the authoritative quantity.
pub fn parity_of(quantity_scu: f64, groups: &[CargoBoxGroup]) -> ParityCheck {
    if groups.is_empty() {
        return ParityCheck::NotApplicable {
            reason: "no Cargo Box Data on this line".into(),
        };
    }
    let expected: f64 = groups.iter().map(|g| g.scu()).sum();
    if (expected - quantity_scu).abs() < 1e-6 {
        ParityCheck::Ok
    } else {
        ParityCheck::Mismatch {
            expected_scu: expected,
            actual_scu: quantity_scu,
        }
    }
}

/// Parse one log line.
///
/// Returns `None` for the overwhelming majority of lines, which do not concern
/// this client at all.
pub fn parse_line(line: &str) -> Option<ParsedLine> {
    // 1. Cheap rejection.
    if !line.contains(CLASS_TOKEN) {
        return None;
    }
    // 2. Classify.
    let method = extract_method(line)?.to_string();
    let log_timestamp = extract_log_timestamp(line).unwrap_or_default().to_string();
    // 3./4. Sweep.
    let fields = sweep_fields(line);

    let kind = match method.as_str() {
        "SendCommodityBuyRequest" => TradeKind::Buy,
        "SendCommoditySellRequest" => TradeKind::Sell,
        "RmToken_CommodityTransactionResponse" => {
            let Some(result) = fields.get("result").cloned() else {
                return Some(ParsedLine::Malformed {
                    raw: line.to_string(),
                    method,
                    missing: vec!["result".into()],
                });
            };
            return Some(ParsedLine::TxError(TxErrorLine {
                log_timestamp,
                result,
                tx_type: fields.get("type").cloned(),
                player_id: fields.get("playerId").cloned(),
            }));
        }
        "ClSetSelectedPlayerLocationInfo" => {
            // This method has benign `[Error]`-level variants that announce no
            // location at all — "Non-vehicle Resource Container not found",
            // "entityId[...] was not found in location data!". They are normal
            // game chatter, not format drift, so they are ignored rather than
            // reported as malformed; treating them as malformed would bury the
            // Diagnostics list that exists to make real drift visible.
            let Some(location_id) = fields.get("LocationId").cloned() else {
                return Some(ParsedLine::Ignored { method });
            };
            return Some(ParsedLine::LocationInfo(LocationInfoLine {
                log_timestamp,
                location_id,
            }));
        }
        _ => return Some(ParsedLine::Ignored { method }),
    };

    // 5. Validate required keys per kind.
    let required: &[&str] = match kind {
        TradeKind::Buy => &[
            "shopId",
            "resourceGUID",
            "quantity",
            "price",
            "shopPricePerCentiSCU",
        ],
        TradeKind::Sell => &["shopId", "resourceGUID", "quantity", "amount"],
    };
    let mut missing: Vec<String> = required
        .iter()
        .filter(|k| !fields.contains_key(**k))
        .map(|k| (*k).to_string())
        .collect();
    if log_timestamp.is_empty() {
        missing.push("<timestamp>".into());
    }
    if !missing.is_empty() {
        return Some(ParsedLine::Malformed {
            raw: line.to_string(),
            method,
            missing,
        });
    }

    let quantity_field = fields.get("quantity").cloned().unwrap_or_default();
    let quantity_raw = ids::normalize_quantity_raw(&quantity_field).to_string();
    let Ok(quantity_logged) = quantity_raw.parse::<f64>() else {
        return Some(ParsedLine::Malformed {
            raw: line.to_string(),
            method,
            missing: vec!["quantity(not a number)".into()],
        });
    };

    let resource_guid = fields.get("resourceGUID").cloned().unwrap_or_default();
    let shop_id = fields.get("shopId").cloned().unwrap_or_default();
    let box_groups = sweep_box_groups(line);

    // Units: a buy logs cSCU, a sell logs SCU. INTEGRATIONS.md §3.
    let (quantity_scu, unit_price_per_scu, total_auec, price_cross_check) = match kind {
        TradeKind::Buy => {
            let scu = units::convert(quantity_logged, CargoUnit::CentiScu, CargoUnit::Scu);
            let per_centi = num(&fields, "shopPricePerCentiSCU").unwrap_or(0.0);
            let unit_price = units::buy_unit_price_per_scu(per_centi);
            let total = num(&fields, "price").unwrap_or(0.0);
            // Cross-check against the rounded `price` field.
            let cross = units::sell_unit_price_per_scu(total, scu)
                .filter(|_| unit_price != 0.0)
                .map(|from_price| (from_price - unit_price).abs() / unit_price);
            (scu, unit_price, total, cross)
        }
        TradeKind::Sell => {
            let scu = quantity_logged;
            let total = num(&fields, "amount").unwrap_or(0.0);
            let unit_price = units::sell_unit_price_per_scu(total, scu).unwrap_or(0.0);
            (scu, unit_price, total, None)
        }
    };

    let parity = parity_of(quantity_scu, &box_groups);

    let id = ids::trade_id(kind, &log_timestamp, &resource_guid, &shop_id, &quantity_field)
        .to_string();

    Some(ParsedLine::Trade(Box::new(TradeLine {
        kind,
        id,
        log_timestamp,
        resource_guid,
        shop_id,
        shop_name: fields.get("shopName").cloned(),
        kiosk_id: fields.get("kioskId").cloned(),
        quantity_raw,
        quantity_scu,
        unit_price_per_scu,
        total_auec,
        box_groups,
        transaction_mode: fields.get("transactionMode").cloned(),
        parity,
        price_cross_check,
        fields,
    })))
}

#[cfg(test)]
#[path = "parse_tests.rs"]
mod tests;
