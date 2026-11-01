//! Field-for-field mirrors of SC-Asset-Manager's `src/lib/types.ts`.
//!
//! camelCase on the wire, `skip_serializing_if` on every optional, because
//! INTEGRATIONS.md is explicit that absent and zero are different: writing
//! `quality: 0` when the log gave us nothing would claim the commodity is
//! worthless rather than unknown.

use serde::{Deserialize, Serialize};

use crate::units::CargoUnit;

/// Marks records written by this client. The web app renders a `LOG` badge.
pub const SOURCE: &str = "log-watcher";

/// INTEGRATIONS.md §4.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetRecord {
    pub id: String,
    pub item: String,
    /// Denominated in `unit`.
    pub amount: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<CargoUnit>,
    /// aUEC per `unit` — not per SCU.
    pub buy_price: f64,
    pub location: String,
    /// 0–1000, commodities only. **The log does not provide it — omit the
    /// field rather than writing 0.**
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<f64>,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logged_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// **Never set this.** Owned by the web app's UEX push. Present only so a
    /// read-modify-write does not drop a value the web app wrote.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uex_buy_id: Option<i64>,
}

/// INTEGRATIONS.md §5.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TradeRecord {
    pub id: String,
    /// The asset sold from. For an orphan trade this is the trade's own id.
    pub asset_id: String,
    pub item: String,
    /// Denominated in `unit`.
    pub amount_sold: f64,
    /// The sell unit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<CargoUnit>,
    /// The unit `buy_price` is denominated in. Set both or neither.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub buy_unit: Option<CargoUnit>,
    /// aUEC per `unit`.
    pub sell_price: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub buy_price: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub buy_location: Option<String>,
    pub sell_location: String,
    pub sold_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logged_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// **Never set this.**
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uex_sell_id: Option<i64>,
}

/// An ISO 8601 timestamp byte-identical to what the web app's
/// `new Date().toISOString()` produces.
pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset() -> AssetRecord {
        AssetRecord {
            id: "4ed7a396-7398-590d-be1c-d3a012c0dcb6".into(),
            item: "Titanium".into(),
            amount: 1008.0,
            unit: Some(CargoUnit::Scu),
            buy_price: 188.9409,
            location: "Area18".into(),
            quality: None,
            created_at: now_iso(),
            logged_by: Some("m1nd".into()),
            source: Some(SOURCE.into()),
            uex_buy_id: None,
        }
    }

    /// Absent is not zero. An omitted `quality` must not appear at all.
    #[test]
    fn optional_fields_are_omitted_not_nulled() {
        let json = serde_json::to_value(asset()).unwrap();
        let obj = json.as_object().unwrap();
        assert!(!obj.contains_key("quality"), "quality must be absent: {json}");
        assert!(!obj.contains_key("uexBuyId"), "uexBuyId must never be written");
        assert!(obj.contains_key("buyPrice"));
    }

    #[test]
    fn field_names_are_camel_case() {
        let json = serde_json::to_value(asset()).unwrap();
        for key in ["buyPrice", "createdAt", "loggedBy", "source"] {
            assert!(json.get(key).is_some(), "missing {key} in {json}");
        }
        assert!(json.get("buy_price").is_none());
    }

    #[test]
    fn unit_serialises_with_the_contract_spelling() {
        let mut a = asset();
        a.unit = Some(CargoUnit::MicroScu);
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains("\"\u{03BC}SCU\""), "{json}");
    }

    /// chrono must reproduce exactly what JS `toISOString()` writes:
    /// milliseconds, trailing Z, no offset.
    #[test]
    fn timestamp_matches_javascript_to_iso_string() {
        let s = now_iso();
        assert!(s.ends_with('Z'), "{s}");
        assert_eq!(s.len(), 24, "YYYY-MM-DDTHH:MM:SS.mmmZ is 24 chars: {s}");
        assert_eq!(s.as_bytes()[10], b'T');
        assert_eq!(s.as_bytes()[19], b'.');
        // And it must round-trip.
        let _: chrono::DateTime<chrono::Utc> = s.parse().unwrap();
    }

    #[test]
    fn trade_round_trips() {
        let t = TradeRecord {
            id: "41604931-7f71-55f3-8059-77b46c7475b4".into(),
            asset_id: "4ed7a396-7398-590d-be1c-d3a012c0dcb6".into(),
            item: "Titanium".into(),
            amount_sold: 640.0,
            unit: Some(CargoUnit::Scu),
            buy_unit: Some(CargoUnit::Scu),
            sell_price: 495.5,
            buy_price: Some(188.9409),
            buy_location: Some("Area18".into()),
            sell_location: "Lorville".into(),
            sold_at: now_iso(),
            logged_by: Some("m1nd".into()),
            source: Some(SOURCE.into()),
            uex_sell_id: None,
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: TradeRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }
}
