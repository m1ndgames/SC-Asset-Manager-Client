//! Fixtures are real log lines, copied verbatim from
//! `E:\Roberts Space Industries\StarCitizen\LIVE\` — including their trailing
//! `[Team_CoreGameplayFeatures][Shops][UI]` tag blocks, which the samples in
//! INTEGRATIONS.md omit. `playerId` values are left as-is here because they are
//! this machine's own; the committed fixture files under `tests/fixtures/` are
//! redacted.

use super::*;

// ---------------------------------------------------------------------------
// Current session (build 12913470). This is the session INTEGRATIONS.md §6's
// published id vectors were taken from.
// ---------------------------------------------------------------------------

/// The buy that FAILED — its error arrives 1.184 s later. Also published id
/// vector 1.
const BUY_FAILED: &str = "<2026-11-01T09:11:29.754Z> [Notice] <CEntityComponentCommodityUIProvider::SendCommodityBuyRequest> Sending SShopCommodityBuyRequest - playerId[214115750165] shopId[863690278779] shopName[SCShop_Admin_lt_base_g] kioskId[863690278778] price[190453.000000] shopPricePerCentiSCU[1.889409] resourceGUID[b999ef65-35be-45bf-908a-5eac6e06ba12] autoLoading[1] quantity[100800.000000 cSCU] Cargo Box Data: boxSize[16.000000] | unitAmount[63] [Team_CoreGameplayFeatures][Shops][UI]";

const TX_ERROR: &str = "<2026-11-01T09:11:30.938Z> [Error] <CEntityComponentCommodityUIProvider::RmToken_CommodityTransactionResponse> Commodity Transaction Response Error - playerId[214115750165] result[CargoCreationFailed] type[Buying] [Team_CoreGameplayFeatures][Shops]";

/// Published id vector 2.
const SELL_OK: &str = "<2026-11-01T11:09:47.325Z> [Notice] <CEntityComponentCommodityUIProvider::SendCommoditySellRequest> Sending SShopCommoditySellRequest - playerId[214115750165] shopId[862401115218] shopName[SCShop_Admin_lt_base_g] kioskId[862401115214] amount[317120.000000] resourceGUID[ea78052a-11b2-484e-a7ee-d19ed906a037] autoLoading[1] quantity[640] transactionMode[ResourceContainer] Cargo Box Data:  [boxSize[8] | unitAmount[80]] [Team_CoreGameplayFeatures][Shops][UI]";

/// A location-bearing `shopName`, unlike the generic `SCShop_Admin_lt_base_g`.
const BUY_GRIMHEX: &str = "<2026-11-01T10:41:21.074Z> [Notice] <CEntityComponentCommodityUIProvider::SendCommodityBuyRequest> Sending SShopCommodityBuyRequest - playerId[214115750165] shopId[863690652870] shopName[SCShop_AdminOffice_GrimHex] kioskId[863690652868] price[206824.000000] shopPricePerCentiSCU[3.231615] resourceGUID[ea78052a-11b2-484e-a7ee-d19ed906a037] autoLoading[1] quantity[64000.000000 cSCU] Cargo Box Data: boxSize[8.000000] | unitAmount[80] [Team_CoreGameplayFeatures][Shops][UI]";

const LOCATION_INFO: &str = "<2026-11-01T09:10:39.576Z> [Notice] <CEntityComponentCommodityUIProvider::ClSetSelectedPlayerLocationInfo> Selecting location with item(s) for LocationId[910662854419] [Team_CoreGameplayFeatures][Shops]";

const LOCATION_INFO_NULL: &str = "<2026-11-01T09:10:39.576Z> [Notice] <CEntityComponentCommodityUIProvider::ClSetSelectedPlayerLocationInfo> Selecting location with item(s) for LocationId[0] [Team_CoreGameplayFeatures][Shops]";

const IGNORED_METHOD: &str = "<2026-11-01T10:13:32.767Z> [Notice] <CEntityComponentCommodityUIProvider::AddPlayerCommodityItem> AddingCommodityBox - playerId[214115750165] shopId[862783269855] shopName[SCShop_Admin_lt_base_g] commodityName[] Cargo Box Data:  [boxSize[8] | unitAmount[60]] [Team_CoreGameplayFeatures][Shops][UI]";

// ---------------------------------------------------------------------------
// Older builds — the adversarial cases that motivated summing box groups.
// ---------------------------------------------------------------------------

/// Build 10989003, a TDD kiosk: `transactionMode[Location]` with TWO cargo box
/// groups. 2x1 + 8x1 = 10 = quantity. Taking only the last pair gives 8 and a
/// bogus mismatch — this line is why `box_groups` is a list.
const SELL_TDD_TWO_GROUPS: &str = "<2025-12-25T20:35:27.785Z> [Notice] <CEntityComponentCommodityUIProvider::SendCommoditySellRequest> Sending SShopCommoditySellRequest - playerId[1849960029409] shopId[8516886087744] shopName[TDD_SCShop-001] kioskId[8516886087804] amount[20558.000000] resourceGUID[07570c9f-fdf6-4bca-a56b-c42809ec0e01] autoLoading[0] quantity[10] transactionMode[Location] Cargo Box Data:  [boxSize[2] | unitAmount[1]] [boxSize[8] | unitAmount[1]] [Team_CoreGameplayFeatures][Shops][UI]";

/// THREE groups: 1x2 + 2x1 + 4x1 = 8 = quantity.
const SELL_TDD_THREE_GROUPS: &str = "<2025-12-25T20:35:58.231Z> [Notice] <CEntityComponentCommodityUIProvider::SendCommoditySellRequest> Sending SShopCommoditySellRequest - playerId[1849960029409] shopId[8516886087744] shopName[TDD_SCShop-001] kioskId[8516886087804] amount[146952.000000] resourceGUID[999e3149-fd10-49ac-914f-8911e61c6122] autoLoading[0] quantity[8] transactionMode[Location] Cargo Box Data:  [boxSize[1] | unitAmount[2]] [boxSize[2] | unitAmount[1]] [boxSize[4] | unitAmount[1]] [Team_CoreGameplayFeatures][Shops][UI]";

/// The one genuine parity anomaly in 91 lines: 16x7 = 112, but quantity is 576
/// (= 36 boxes of 16). Exactly what the drift warning is for.
const SELL_PARITY_ANOMALY: &str = "<2026-04-10T21:41:23.264Z> [Notice] <CEntityComponentCommodityUIProvider::SendCommoditySellRequest> Sending SShopCommoditySellRequest - playerId[1849960029409] shopId[9829906682927] shopName[SCShop_Admin_lt_base_g] kioskId[9829906682935] amount[3027708.000000] resourceGUID[d5506a24-5729-4354-81fb-0959173357c4] autoLoading[0] quantity[576] transactionMode[ResourceContainer] Cargo Box Data:  [boxSize[16] | unitAmount[7]] [Team_CoreGameplayFeatures][Shops][UI]";

/// Build 11576750, `autoLoading[0]` and a location-bearing shop.
const BUY_RAYARI: &str = "<2026-04-06T14:14:28.003Z> [Notice] <CEntityComponentCommodityUIProvider::SendCommodityBuyRequest> Sending SShopCommodityBuyRequest - playerId[1849960029409] shopId[9776280029894] shopName[SCShop_ht_delta_rayari_m_store] kioskId[9776280029893] price[188328.000000] shopPricePerCentiSCU[39.234837] resourceGUID[d5506a24-5729-4354-81fb-0959173357c4] autoLoading[0] quantity[4800.000000 cSCU] Cargo Box Data: boxSize[16.000000] | unitAmount[3] [Team_CoreGameplayFeatures][Shops][UI]";

fn trade(line: &str) -> TradeLine {
    match parse_line(line) {
        Some(ParsedLine::Trade(t)) => *t,
        other => panic!("expected a trade, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------

#[test]
fn irrelevant_lines_are_rejected_cheaply() {
    assert_eq!(parse_line(""), None);
    assert_eq!(parse_line("<2026-11-01T09:00:00.000Z> [Notice] <CFoo::Bar> hi"), None);
    // The class token must be present; a near-miss is not enough.
    assert_eq!(parse_line("CEntityComponentCommodityUIProvider hello"), None);
}

#[test]
fn recognises_each_line_kind() {
    assert!(matches!(parse_line(BUY_FAILED), Some(ParsedLine::Trade(_))));
    assert!(matches!(parse_line(SELL_OK), Some(ParsedLine::Trade(_))));
    assert!(matches!(parse_line(TX_ERROR), Some(ParsedLine::TxError(_))));
    assert!(matches!(
        parse_line(LOCATION_INFO),
        Some(ParsedLine::LocationInfo(_))
    ));
    assert!(matches!(
        parse_line(IGNORED_METHOD),
        Some(ParsedLine::Ignored { .. })
    ));
}

#[test]
fn timestamp_is_the_verbatim_first_bracket() {
    let t = trade(BUY_FAILED);
    assert_eq!(t.log_timestamp, "2026-11-01T09:11:29.754Z");
    // Not reformatted, not parsed, no timezone normalisation.
    assert!(t.log_timestamp.ends_with('Z'));
}

// ---------------------------------------------------------------------------
// Ids — the published vectors, reached through the parser rather than by hand
// ---------------------------------------------------------------------------

#[test]
fn parsed_buy_reproduces_published_id_vector() {
    let t = trade(BUY_FAILED);
    assert_eq!(t.id, "4ed7a396-7398-590d-be1c-d3a012c0dcb6");
    assert_eq!(t.quantity_raw, "100800.000000");
}

#[test]
fn parsed_sell_reproduces_published_id_vector() {
    let t = trade(SELL_OK);
    assert_eq!(t.id, "41604931-7f71-55f3-8059-77b46c7475b4");
    assert_eq!(t.quantity_raw, "640");
}

// ---------------------------------------------------------------------------
// Units and derived figures
// ---------------------------------------------------------------------------

#[test]
fn buy_quantity_is_centi_scu_and_sell_is_scu() {
    assert_eq!(trade(BUY_FAILED).quantity_scu, 1008.0);
    assert_eq!(trade(SELL_OK).quantity_scu, 640.0);
}

#[test]
fn buy_unit_price_comes_from_centi_scu_not_rounded_price() {
    let t = trade(BUY_FAILED);
    assert!((t.unit_price_per_scu - 188.9409).abs() < 1e-9);
    assert_eq!(t.total_auec, 190_453.0);
    // The rounded `price` field disagrees slightly, which is the point.
    let drift = t.price_cross_check.expect("buys carry a cross-check");
    assert!(drift > 0.0 && drift < 0.001, "drift was {drift}");
}

#[test]
fn sell_unit_price_comes_from_the_total() {
    let t = trade(SELL_OK);
    assert_eq!(t.total_auec, 317_120.0);
    assert_eq!(t.unit_price_per_scu, 495.5);
    assert_eq!(t.price_cross_check, None, "sells have no price field");
}

/// The cross-check must hold on every real buy line in the fixtures — it is the
/// drift detector that replaces naive parity.
#[test]
fn buy_price_cross_check_holds_on_all_fixtures() {
    for line in [BUY_FAILED, BUY_GRIMHEX, BUY_RAYARI] {
        let t = trade(line);
        let drift = t.price_cross_check.expect("buy");
        assert!(drift < 0.001, "drift {drift} on {}", t.id);
    }
}

// ---------------------------------------------------------------------------
// Parity — summed over groups
// ---------------------------------------------------------------------------

#[test]
fn parity_holds_on_single_group_lines() {
    assert_eq!(trade(BUY_FAILED).parity, ParityCheck::Ok); // 16 x 63 = 1008
    assert_eq!(trade(SELL_OK).parity, ParityCheck::Ok); // 8 x 80 = 640
    assert_eq!(trade(BUY_GRIMHEX).parity, ParityCheck::Ok); // 8 x 80 = 640
    assert_eq!(trade(BUY_RAYARI).parity, ParityCheck::Ok); // 16 x 3 = 48
}

/// The regression this whole list-of-groups design exists for.
#[test]
fn parity_sums_multiple_cargo_box_groups() {
    let t = trade(SELL_TDD_TWO_GROUPS);
    assert_eq!(
        t.box_groups,
        vec![
            CargoBoxGroup { box_size: 2.0, unit_amount: 1.0 },
            CargoBoxGroup { box_size: 8.0, unit_amount: 1.0 },
        ]
    );
    assert_eq!(t.quantity_scu, 10.0);
    assert_eq!(
        t.parity,
        ParityCheck::Ok,
        "2x1 + 8x1 = 10; taking only the last group would give 8 and a false mismatch"
    );

    let t3 = trade(SELL_TDD_THREE_GROUPS);
    assert_eq!(t3.box_groups.len(), 3);
    assert_eq!(t3.quantity_scu, 8.0);
    assert_eq!(t3.parity, ParityCheck::Ok, "1x2 + 2x1 + 4x1 = 8");
}

#[test]
fn parity_reports_the_one_real_anomaly() {
    let t = trade(SELL_PARITY_ANOMALY);
    assert_eq!(
        t.parity,
        ParityCheck::Mismatch { expected_scu: 112.0, actual_scu: 576.0 }
    );
    assert!(t.parity.is_mismatch());
    // Quantity stays authoritative even when parity disagrees.
    assert_eq!(t.quantity_scu, 576.0);
}

#[test]
fn parity_is_not_applicable_without_cargo_box_data() {
    let line = "<2026-11-01T09:11:29.754Z> [Notice] <CEntityComponentCommodityUIProvider::SendCommoditySellRequest> Sending x - shopId[1] resourceGUID[g] amount[100.000000] quantity[5] transactionMode[Location] [Team_CoreGameplayFeatures][Shops][UI]";
    assert!(matches!(
        trade(line).parity,
        ParityCheck::NotApplicable { .. }
    ));
}

#[test]
fn transaction_mode_is_captured_for_sells_only() {
    assert_eq!(
        trade(SELL_OK).transaction_mode.as_deref(),
        Some("ResourceContainer")
    );
    assert_eq!(
        trade(SELL_TDD_TWO_GROUPS).transaction_mode.as_deref(),
        Some("Location")
    );
    assert_eq!(trade(BUY_FAILED).transaction_mode, None);
}

// ---------------------------------------------------------------------------
// Location hints
// ---------------------------------------------------------------------------

#[test]
fn location_info_is_extracted_and_null_sentinel_detected() {
    let Some(ParsedLine::LocationInfo(l)) = parse_line(LOCATION_INFO) else {
        panic!("expected location info");
    };
    assert_eq!(l.location_id, "910662854419");
    assert!(!l.is_null());

    let Some(ParsedLine::LocationInfo(n)) = parse_line(LOCATION_INFO_NULL) else {
        panic!("expected location info");
    };
    assert!(n.is_null(), "LocationId[0] is a sentinel, not a place");
}

#[test]
fn shop_name_distinguishes_generic_from_location_bearing() {
    assert_eq!(
        trade(BUY_FAILED).shop_name.as_deref(),
        Some("SCShop_Admin_lt_base_g")
    );
    assert_eq!(
        trade(BUY_GRIMHEX).shop_name.as_deref(),
        Some("SCShop_AdminOffice_GrimHex")
    );
    assert_eq!(
        trade(SELL_TDD_TWO_GROUPS).shop_name.as_deref(),
        Some("TDD_SCShop-001")
    );
}

// ---------------------------------------------------------------------------
// Error lines
// ---------------------------------------------------------------------------

#[test]
fn error_line_carries_result_and_type() {
    let Some(ParsedLine::TxError(e)) = parse_line(TX_ERROR) else {
        panic!("expected error");
    };
    assert_eq!(e.result, "CargoCreationFailed");
    assert_eq!(e.tx_type.as_deref(), Some("Buying"));
    assert_eq!(e.log_timestamp, "2026-11-01T09:11:30.938Z");
    // The error carries no resourceGUID and no shopId; correlation can only be
    // FIFO by kind. This assertion documents that limitation.
    assert!(!TX_ERROR.contains("resourceGUID"));
    assert!(!TX_ERROR.contains("shopId"));
}

/// The error follows its buy by 1.184 s, comfortably inside a 3 s window.
#[test]
fn error_follows_its_buy_within_the_grace_window() {
    use chrono::{DateTime, Utc};
    let buy: DateTime<Utc> = trade(BUY_FAILED).log_timestamp.parse().unwrap();
    let Some(ParsedLine::TxError(e)) = parse_line(TX_ERROR) else {
        panic!()
    };
    let err: DateTime<Utc> = e.log_timestamp.parse().unwrap();
    let delta = (err - buy).num_milliseconds();
    assert_eq!(delta, 1184);
    assert!(delta < 3000);
}

// ---------------------------------------------------------------------------
// Malformed — never silently dropped
// ---------------------------------------------------------------------------

#[test]
fn missing_required_fields_become_malformed() {
    let line = "<2026-11-01T09:11:29.754Z> [Notice] <CEntityComponentCommodityUIProvider::SendCommodityBuyRequest> Sending x - shopId[1] quantity[100.000000 cSCU] [Shops]";
    let Some(ParsedLine::Malformed { missing, method, .. }) = parse_line(line) else {
        panic!("a class-matching line must never be silently dropped");
    };
    assert_eq!(method, "SendCommodityBuyRequest");
    assert!(missing.contains(&"resourceGUID".to_string()));
    assert!(missing.contains(&"price".to_string()));
    assert!(missing.contains(&"shopPricePerCentiSCU".to_string()));
}

#[test]
fn unparseable_quantity_becomes_malformed() {
    let line = "<2026-11-01T09:11:29.754Z> [Notice] <CEntityComponentCommodityUIProvider::SendCommoditySellRequest> x - shopId[1] resourceGUID[g] amount[1.0] quantity[lots]";
    let Some(ParsedLine::Malformed { missing, .. }) = parse_line(line) else {
        panic!("expected malformed");
    };
    assert!(missing.iter().any(|m| m.starts_with("quantity")));
}

/// `ClSetSelectedPlayerLocationInfo` has `[Error]`-level variants carrying no
/// `LocationId`. They are ordinary game chatter and must not pollute the
/// malformed list, which exists to make genuine format drift visible.
#[test]
fn location_info_error_variants_are_ignored_not_malformed() {
    for line in [
        "<2026-04-06T17:24:12.934Z> [Error] <CEntityComponentCommodityUIProvider::ClSetSelectedPlayerLocationInfo> entityId[9816428777188] was not found in location data! We should always find a location when explicitly looking for one [Team_CoreGameplayFeatures][Shops]",
        "<2026-04-10T21:40:28.614Z> [Error] <CEntityComponentCommodityUIProvider::ClSetSelectedPlayerLocationInfo> Non-vehicle Resource Container not found [Team_CoreGameplayFeatures][Shops]",
    ] {
        match parse_line(line) {
            Some(ParsedLine::Ignored { method }) => {
                assert_eq!(method, "ClSetSelectedPlayerLocationInfo")
            }
            other => panic!("expected Ignored, got {other:?}"),
        }
    }
}

#[test]
fn unknown_method_is_ignored_not_malformed() {
    let Some(ParsedLine::Ignored { method }) = parse_line(IGNORED_METHOD) else {
        panic!("expected ignored");
    };
    assert_eq!(method, "AddPlayerCommodityItem");
}

// ---------------------------------------------------------------------------
// Robustness
// ---------------------------------------------------------------------------

#[test]
fn crlf_is_tolerated() {
    let with_cr = format!("{BUY_FAILED}\r");
    let t = trade(&with_cr);
    assert_eq!(t.id, "4ed7a396-7398-590d-be1c-d3a012c0dcb6");
}

#[test]
fn truncated_line_does_not_panic() {
    // Every prefix of a real line must either parse or be rejected/malformed —
    // never panic. A half-written line reaches the parser only if the tailer's
    // invariant is broken, but it must still be safe.
    for cut in 0..BUY_FAILED.len() {
        if !BUY_FAILED.is_char_boundary(cut) {
            continue;
        }
        let _ = parse_line(&BUY_FAILED[..cut]);
    }
}

#[test]
fn lossy_utf8_does_not_stop_parsing() {
    // A stray byte in an entity name must never stop the tailer; by the time a
    // line reaches here it has been through from_utf8_lossy, so it carries the
    // replacement character.
    let mangled = BUY_FAILED.replace("SCShop_Admin_lt_base_g", "SCShop_Admin_\u{FFFD}_base_g");
    let t = trade(&mangled);
    // The id does not depend on shopName, so it is unchanged.
    assert_eq!(t.id, "4ed7a396-7398-590d-be1c-d3a012c0dcb6");
    assert!(t.shop_name.unwrap().contains('\u{FFFD}'));
}

#[test]
fn field_sweep_tolerates_reordering_and_new_fields() {
    let reordered = "<2026-11-01T11:09:47.325Z> [Notice] <CEntityComponentCommodityUIProvider::SendCommoditySellRequest> x - quantity[640] futureField[whatever] resourceGUID[ea78052a-11b2-484e-a7ee-d19ed906a037] shopId[862401115218] amount[317120.000000] transactionMode[ResourceContainer] Cargo Box Data:  [boxSize[8] | unitAmount[80]] [Shops]";
    let t = trade(reordered);
    // Same id as the canonical line: the id depends on values, not on order.
    assert_eq!(t.id, "41604931-7f71-55f3-8059-77b46c7475b4");
    assert_eq!(t.fields.get("futureField").map(String::as_str), Some("whatever"));
    assert_eq!(t.parity, ParityCheck::Ok);
}

#[test]
fn trailing_tag_block_is_not_mistaken_for_a_field() {
    let t = trade(BUY_FAILED);
    // `[Team_CoreGameplayFeatures][Shops][UI]` has no `name[` prefix, so the
    // sweep must not invent fields from it.
    assert!(!t.fields.contains_key("Team_CoreGameplayFeatures"));
    assert!(!t.fields.contains_key("Shops"));
    assert!(!t.fields.contains_key("UI"));
}
