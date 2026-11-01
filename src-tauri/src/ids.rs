//! Deterministic record ids. INTEGRATIONS.md §6.
//!
//! Replaying the same log line must produce the same id, so that a crash, a
//! restart or a backfill collapses duplicates instead of creating them. That
//! makes every write an idempotent upsert — the property the whole queue design
//! leans on.

use uuid::Uuid;

/// `uuid5(NAMESPACE_DNS, "sc-assetmanager.org")`, per INTEGRATIONS.md §6.
///
/// Hardcoded so a typo in the derivation can never silently change every id;
/// `namespace_derivation_matches` asserts the two agree.
pub const NAMESPACE: Uuid = Uuid::from_bytes([
    0xc9, 0x9f, 0xd5, 0x89, 0xc7, 0x00, 0x51, 0xa5, 0xac, 0x3b, 0x10, 0x43, 0xbe, 0x61, 0xf4, 0x7f,
]);

/// The DNS name the namespace is derived from.
pub const NAMESPACE_DNS_NAME: &str = "sc-assetmanager.org";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TradeKind {
    Buy,
    Sell,
}

impl TradeKind {
    /// The literal token used in the id name string. Must stay lowercase
    /// `buy`/`sell` — it is part of the hash input.
    pub fn as_str(self) -> &'static str {
        match self {
            TradeKind::Buy => "buy",
            TradeKind::Sell => "sell",
        }
    }
}

/// Strip the trailing unit token and surrounding whitespace from a raw
/// `quantity[...]` value, leaving the numeric text **exactly** as the log wrote
/// it.
///
/// `"100800.000000 cSCU"` -> `"100800.000000"`, `"640"` -> `"640"`.
///
/// Critically this does *not* parse: `100800.000000` keeps its six decimal
/// zeroes, because the id is a hash over the literal text. Reformatting it
/// through `f64` would yield `100800` and a different, wrong id.
pub fn normalize_quantity_raw(raw: &str) -> &str {
    let trimmed = raw.trim();
    // Cut at the first character that cannot belong to the number itself.
    let end = trimmed
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
        .unwrap_or(trimmed.len());
    trimmed[..end].trim_end()
}

/// The `|`-joined name string hashed into the record id.
///
/// Every component is taken verbatim from the log (modulo
/// [`normalize_quantity_raw`]): UTF-8, no padding, no trailing separator.
pub fn id_name_string(
    kind: TradeKind,
    log_timestamp: &str,
    resource_guid: &str,
    shop_id: &str,
    quantity_raw: &str,
) -> String {
    format!(
        "{}|{}|{}|{}|{}",
        kind.as_str(),
        log_timestamp,
        resource_guid,
        shop_id,
        normalize_quantity_raw(quantity_raw)
    )
}

/// The deterministic record id for one parsed trade line.
pub fn trade_id(
    kind: TradeKind,
    log_timestamp: &str,
    resource_guid: &str,
    shop_id: &str,
    quantity_raw: &str,
) -> Uuid {
    let name = id_name_string(kind, log_timestamp, resource_guid, shop_id, quantity_raw);
    Uuid::new_v5(&NAMESPACE, name.as_bytes())
}

/// PocketBase id mangling, INTEGRATIONS.md §6.
///
/// PocketBase ids must start with a letter, so a uuid beginning with a digit
/// gains an `r` prefix. Records carry both `id = to_pb_id(uuid)` and
/// `app_id = uuid`; readers prefer `app_id`.
pub fn to_pb_id(uuid: &Uuid) -> String {
    let s: String = uuid
        .as_hyphenated()
        .to_string()
        .chars()
        .filter(|c| *c != '-')
        .collect();
    match s.chars().next() {
        Some(c) if c.is_ascii_alphabetic() => s,
        _ => format!("r{s}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hardcoded namespace must equal its documented derivation.
    #[test]
    fn namespace_derivation_matches() {
        let derived = Uuid::new_v5(&Uuid::NAMESPACE_DNS, NAMESPACE_DNS_NAME.as_bytes());
        assert_eq!(derived, NAMESPACE);
        assert_eq!(NAMESPACE.to_string(), "c99fd589-c700-51a5-ac3b-1043be61f47f");
    }

    /// Published test vector 1 — INTEGRATIONS.md §6.
    #[test]
    fn published_vector_buy() {
        let name = id_name_string(
            TradeKind::Buy,
            "2026-11-01T09:11:29.754Z",
            "b999ef65-35be-45bf-908a-5eac6e06ba12",
            "863690278779",
            "100800.000000 cSCU",
        );
        assert_eq!(
            name,
            "buy|2026-11-01T09:11:29.754Z|b999ef65-35be-45bf-908a-5eac6e06ba12|863690278779|100800.000000"
        );
        assert_eq!(
            trade_id(
                TradeKind::Buy,
                "2026-11-01T09:11:29.754Z",
                "b999ef65-35be-45bf-908a-5eac6e06ba12",
                "863690278779",
                "100800.000000 cSCU",
            )
            .to_string(),
            "4ed7a396-7398-590d-be1c-d3a012c0dcb6"
        );
    }

    /// Published test vector 2 — INTEGRATIONS.md §6.
    #[test]
    fn published_vector_sell() {
        let name = id_name_string(
            TradeKind::Sell,
            "2026-11-01T11:09:47.325Z",
            "ea78052a-11b2-484e-a7ee-d19ed906a037",
            "862401115218",
            "640",
        );
        assert_eq!(
            name,
            "sell|2026-11-01T11:09:47.325Z|ea78052a-11b2-484e-a7ee-d19ed906a037|862401115218|640"
        );
        assert_eq!(
            trade_id(
                TradeKind::Sell,
                "2026-11-01T11:09:47.325Z",
                "ea78052a-11b2-484e-a7ee-d19ed906a037",
                "862401115218",
                "640",
            )
            .to_string(),
            "41604931-7f71-55f3-8059-77b46c7475b4"
        );
    }

    /// `quantityRaw` must never round-trip through a float. PLAN.md §4.
    ///
    /// If someone "tidies" the parser by storing quantity as f64 and
    /// reformatting it here, this is the test that catches it.
    #[test]
    fn quantity_raw_is_never_reformatted() {
        assert_eq!(normalize_quantity_raw("100800.000000 cSCU"), "100800.000000");
        assert_eq!(normalize_quantity_raw("  100800.000000 cSCU  "), "100800.000000");
        assert_eq!(normalize_quantity_raw("640"), "640");
        assert_eq!(normalize_quantity_raw("4800.000000 cSCU"), "4800.000000");

        // The trap, stated explicitly: the float round-trip loses the zeroes
        // and would produce a different id.
        let raw = "100800.000000";
        let reformatted = raw.parse::<f64>().unwrap().to_string();
        assert_eq!(reformatted, "100800");
        assert_ne!(
            raw, reformatted,
            "quantityRaw must stay literal text, not a parsed number"
        );
        assert_ne!(
            trade_id(TradeKind::Buy, "t", "g", "s", raw),
            trade_id(TradeKind::Buy, "t", "g", "s", &reformatted),
            "reformatting quantityRaw changes the id"
        );
    }

    #[test]
    fn to_pb_id_prefixes_leading_digit() {
        // Vector 1 starts with '4' -> prefixed.
        let u: Uuid = "4ed7a396-7398-590d-be1c-d3a012c0dcb6".parse().unwrap();
        let pb = to_pb_id(&u);
        assert_eq!(pb, "r4ed7a3967398590dbe1cd3a012c0dcb6");
        assert_eq!(pb.len(), 33);
        assert!(pb.chars().next().unwrap().is_ascii_alphabetic());
    }

    #[test]
    fn to_pb_id_leaves_leading_letter_alone() {
        let u: Uuid = "ea78052a-11b2-484e-a7ee-d19ed906a037".parse().unwrap();
        let pb = to_pb_id(&u);
        assert_eq!(pb, "ea78052a11b2484ea7eed19ed906a037");
        assert_eq!(pb.len(), 32);
    }

    #[test]
    fn ids_are_stable_and_kind_sensitive() {
        let args = ("2026-11-01T09:11:29.754Z", "guid", "shop", "100");
        let buy = trade_id(TradeKind::Buy, args.0, args.1, args.2, args.3);
        let sell = trade_id(TradeKind::Sell, args.0, args.1, args.2, args.3);
        assert_ne!(buy, sell, "kind must participate in the hash");
        assert_eq!(
            buy,
            trade_id(TradeKind::Buy, args.0, args.1, args.2, args.3),
            "same input must always give the same id"
        );
    }
}
