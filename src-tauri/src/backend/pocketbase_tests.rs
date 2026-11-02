//! Tests for the PocketBase mapping.
//!
//! Weighted towards names rather than HTTP plumbing: PocketBase discards a
//! field it has no column for and returns 200, so a casing slip writes a
//! record with the field missing. Asserting the key set against the documented
//! schema is the only way to catch that.
//!
//! The wire behaviour needs a live instance — see `tests/live_pocketbase.rs`.

use super::*;
use crate::units::CargoUnit;

fn cfg() -> Config {
    let mut c = Config::default();
    c.backend.kind = BackendKind::Pocketbase;
    c.backend.email = "a@b.c".into();
    c.backend.password = "SENTINEL-PASSWORD".into();
    c.backend.pocketbase.url = "https://pb.example".into();
    c.nickname = "m1nd".into();
    c
}

/// A uuid starting with a letter, so `to_pb_id` does not prefix it. Both
/// published vectors begin with a digit, so the prefixed case is tested below.
const UUID: &str = "ea78052a-11b2-484e-a7ee-d19ed906a037";
const PB_ID: &str = "ea78052a11b2484ea7eed19ed906a037";

fn asset() -> AssetRecord {
    AssetRecord {
        id: UUID.into(),
        item: "Ammonia".into(),
        amount: 1008.0,
        unit: Some(CargoUnit::Scu),
        buy_price: 188.9409,
        location: "Port Tressler".into(),
        quality: None,
        created_at: "2026-11-01T09:11:29.754Z".into(),
        logged_by: Some("m1nd".into()),
        source: Some("log-watcher".into()),
        uex_buy_id: None,
    }
}

fn trade() -> TradeRecord {
    TradeRecord {
        id: UUID.into(),
        asset_id: "a-different-uuid".into(),
        item: "Ammonia".into(),
        amount_sold: 8.0,
        unit: Some(CargoUnit::Scu),
        buy_unit: Some(CargoUnit::Scu),
        sell_price: 1064.66,
        buy_price: Some(188.9409),
        buy_location: Some("Port Tressler".into()),
        sell_location: "Baijini Point".into(),
        sold_at: "2026-11-01T19:44:48.000Z".into(),
        logged_by: Some("m1nd".into()),
        source: Some("log-watcher".into()),
        uex_sell_id: None,
    }
}

// ---------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------

#[test]
fn refuses_to_build_without_a_url() {
    let mut c = cfg();
    c.backend.pocketbase.url = String::new();
    let err = PocketbaseBackend::from_config(&c).unwrap_err();
    assert_eq!(err.kind(), "notConfigured");
}

#[test]
fn refuses_to_build_without_credentials() {
    let mut c = cfg();
    c.backend.password = String::new();
    let err = PocketbaseBackend::from_config(&c).unwrap_err();
    assert_eq!(err.kind(), "notConfigured");
}

/// A pasted URL with a trailing slash must not produce `//api/...`.
#[test]
fn a_trailing_slash_is_trimmed() {
    let mut c = cfg();
    c.backend.pocketbase.url = "https://pb.example/".into();
    let b = PocketbaseBackend::from_config(&c).unwrap();
    assert_eq!(
        b.records_url(ASSETS),
        "https://pb.example/api/collections/assets/records"
    );
}

/// The password must not be reachable through `Debug`, which is what a panic
/// message or an instrumented log line formats.
#[test]
fn debug_does_not_leak_the_password() {
    let b = PocketbaseBackend::from_config(&cfg()).unwrap();
    let s = format!("{b:?}");
    assert!(!s.contains("SENTINEL-PASSWORD"), "{s}");
    assert!(s.contains("redacted"), "{s}");
}

// ---------------------------------------------------------------------------
// The field contract
// ---------------------------------------------------------------------------

/// POCKETBASE.md's `assets` schema, verbatim, plus the primary key.
const ASSET_COLUMNS: &[&str] = &[
    "id",
    "app_id",
    "item",
    "amount",
    "unit",
    "buyPrice",
    "location",
    "quality",
    "createdAt",
    "loggedBy",
    "uexBuyId",
    "source",
];

/// POCKETBASE.md's `trades` schema, verbatim, plus the primary key.
const TRADE_COLUMNS: &[&str] = &[
    "id",
    "app_id",
    "assetId",
    "item",
    "amountSold",
    "unit",
    "buyUnit",
    "sellPrice",
    "sellLocation",
    "soldAt",
    "buyPrice",
    "buyLocation",
    "loggedBy",
    "uexSellId",
    "source",
];

/// Anything we send that is not a column is dropped
/// without an error, so every emitted key must be a known column.
#[test]
fn no_asset_field_is_sent_that_pocketbase_has_no_column_for() {
    let fields = to_pb_fields(&asset(), UUID, &[]).unwrap();
    for k in fields.keys() {
        assert!(
            ASSET_COLUMNS.contains(&k.as_str()),
            "`{k}` is not an assets column -- PocketBase would discard it silently"
        );
    }
}

#[test]
fn no_trade_field_is_sent_that_pocketbase_has_no_column_for() {
    let fields = to_pb_fields(&trade(), UUID, &trade_clearable()).unwrap();
    for k in fields.keys() {
        assert!(
            TRADE_COLUMNS.contains(&k.as_str()),
            "`{k}` is not a trades column -- PocketBase would discard it silently"
        );
    }
}

/// The required columns must actually arrive. The test above catches a typo
/// that invents a column; this one catches a rename that loses a real one.
#[test]
fn every_required_asset_column_is_present() {
    let fields = to_pb_fields(&asset(), UUID, &[]).unwrap();
    for required in ["app_id", "item", "amount", "buyPrice", "location", "createdAt"] {
        assert!(fields.contains_key(required), "missing `{required}`");
    }
}

#[test]
fn every_required_trade_column_is_present() {
    let fields = to_pb_fields(&trade(), UUID, &trade_clearable()).unwrap();
    for required in [
        "app_id",
        "item",
        "amountSold",
        "sellPrice",
        "sellLocation",
        "soldAt",
    ] {
        assert!(fields.contains_key(required), "missing `{required}`");
    }
}

/// `app_id` is the only snake_case name in the schema. Writing it as `appId`
/// would be silently discarded and leave every record unfindable by the web
/// app, which reads `app_id` in preference to the mangled key.
#[test]
fn app_id_is_snake_case_and_holds_the_real_uuid() {
    let fields = to_pb_fields(&asset(), UUID, &[]).unwrap();
    assert_eq!(fields["app_id"], json!(UUID));
    assert!(!fields.contains_key("appId"));
}

/// Both ids are always written. `to_pb_id` is not reversible, so `app_id` is
/// the only way back to the uuid the rest of the system uses.
#[test]
fn the_primary_key_is_the_mangled_id_and_both_are_written() {
    let fields = to_pb_fields(&asset(), UUID, &[]).unwrap();
    assert_eq!(fields["id"], json!(PB_ID));
    assert_eq!(fields["app_id"], json!(UUID));
    assert_ne!(fields["id"], fields["app_id"], "both, not one twice");
}

/// A uuid starting with a digit gets the `r` prefix, because a PocketBase id
/// may not begin with one.
#[test]
fn a_leading_digit_is_prefixed() {
    // The published buy vector, which begins with `4`.
    let digit_first = "4ed7a396-7398-590d-be1c-d3a012c0dcb6";
    let fields = to_pb_fields(&trade(), digit_first, &trade_clearable()).unwrap();
    assert_eq!(fields["id"], json!("r4ed7a3967398590dbe1cd3a012c0dcb6"));
    assert_eq!(fields["app_id"], json!(digit_first));
}

/// `quality` and the UEX ids are never written. `skip_serializing_if` keeps
/// them out of the body entirely, which is what stops a write from setting
/// `quality` to 0 or clearing a value the web app owns.
#[test]
fn fields_this_client_does_not_own_are_absent_rather_than_zero() {
    let a = to_pb_fields(&asset(), UUID, &[]).unwrap();
    assert!(!a.contains_key("quality"), "absent, never 0");
    assert!(!a.contains_key("uexBuyId"));
    let t = to_pb_fields(&trade(), UUID, &trade_clearable()).unwrap();
    assert!(!t.contains_key("uexSellId"));
}

/// An unrounded price must stay unrounded; it is a JSON number, not a string.
#[test]
fn prices_are_numbers_and_are_not_rounded() {
    let fields = to_pb_fields(&asset(), UUID, &[]).unwrap();
    assert_eq!(fields["buyPrice"].as_f64().unwrap(), 188.9409);
    assert!(fields["amount"].is_number());
}

/// The unit keeps its contract spelling.
#[test]
fn the_unit_keeps_its_contract_spelling() {
    let fields = to_pb_fields(&asset(), UUID, &[]).unwrap();
    assert_eq!(fields["unit"], json!("SCU"));
}

/// An id that is not a uuid has no PocketBase id, and guessing one would
/// produce a record the web app can show but never match.
#[test]
fn a_non_uuid_id_is_refused_rather_than_mangled() {
    let mut a = asset();
    a.id = "not-a-uuid".into();
    let err = to_pb_fields(&a, &a.id, &[]).unwrap_err();
    assert!(err.to_string().contains("not a uuid"), "{err}");
}

// ---------------------------------------------------------------------------
// Reading back
// ---------------------------------------------------------------------------

/// A read prefers `app_id`, so the record's `id` is the uuid the queue,
/// the writer and the web app all use — never the mangled key.
#[test]
fn a_read_record_carries_the_uuid_not_the_pocketbase_key() {
    let row = json!({
        "id": PB_ID,
        "app_id": UUID,
        "item": "Ammonia",
        "amount": 1008.0,
        "unit": "SCU",
        "buyPrice": 188.9409,
        "location": "Port Tressler",
        "createdAt": "2026-11-01T09:11:29.754Z",
        "loggedBy": "m1nd",
        "source": "log-watcher",
        // PocketBase sends these on every record; they are not ours.
        "collectionId": "abc123",
        "collectionName": "assets",
        "created": "2026-11-01 09:11:29.754Z",
        "updated": "2026-11-01 09:11:29.754Z",
    });
    let a = asset_from_pb(&row).expect("parsed");
    assert_eq!(a.id, UUID, "the uuid, not the mangled key");
    assert_eq!(a.item, "Ammonia");
    assert_eq!(a.buy_price, 188.9409);
    assert_eq!(a.unit, Some(CargoUnit::Scu));
}

/// A round trip through both directions must be lossless for the fields this
/// client owns, or a merge would write back something subtly different from
/// what it read.
#[test]
fn an_asset_round_trips() {
    let before = asset();
    let mut row = to_pb_fields(&before, &before.id, &[]).unwrap();
    // What the server adds on the way back.
    row.insert("collectionName".into(), json!("assets"));
    let after = asset_from_pb(&Value::Object(row)).expect("parsed");
    assert_eq!(after, before);
}

/// PocketBase returns `""` for an unset optional Text column. An empty `unit`
/// is not a unit, and letting it through fails to deserialise — so the whole
/// record would silently disappear from a merge lookup.
#[test]
fn an_empty_optional_unit_is_treated_as_absent() {
    let row = json!({
        "id": PB_ID,
        "app_id": UUID,
        "item": "Ammonia",
        "amount": 1008.0,
        "unit": "",
        "buyPrice": 188.9409,
        "location": "Port Tressler",
        "createdAt": "2026-11-01T09:11:29.754Z",
    });
    let a = asset_from_pb(&row).expect("an older instance has no unit column");
    assert_eq!(a.unit, None);
}

/// A row with no `app_id` cannot be mapped back, and must not be guessed at
/// from the primary key — `to_pb_id` has no inverse.
#[test]
fn a_row_without_app_id_is_skipped_rather_than_guessed() {
    let row = json!({
        "id": PB_ID,
        "item": "Ammonia",
        "amount": 1.0,
        "buyPrice": 1.0,
        "location": "x",
        "createdAt": "x",
    });
    assert!(asset_from_pb(&row).is_none());
}

// ---------------------------------------------------------------------------
// Filters
// ---------------------------------------------------------------------------

/// A location with an apostrophe is ordinary in this game. An unescaped one
/// would end the string literal and change what the filter matches.
#[test]
fn a_quote_in_a_value_is_escaped_not_dropped() {
    assert_eq!(filter_literal("Orison"), "'Orison'");
    assert_eq!(filter_literal("Jack's Kiosk"), r"'Jack\'s Kiosk'");
    assert_eq!(filter_literal(r"back\slash"), r"'back\\slash'");
}

/// The filter travels in a query string, so every byte a filter contains
/// — quotes, spaces, `=`, `&` — must be escaped. A half-escaped filter
/// returns the wrong rows rather than an error.
#[test]
fn a_filter_is_fully_percent_encoded() {
    let encoded = urlencode("item='Ammonia' && location='Port Tressler'");
    for raw in ["'", " ", "=", "&"] {
        assert!(
            !encoded.contains(raw),
            "`{raw}` survived encoding: {encoded}"
        );
    }
    assert!(encoded.contains("item%3D%27Ammonia%27"), "{encoded}");
}

/// Non-ASCII must be encoded as UTF-8 bytes, not dropped.
#[test]
fn non_ascii_is_encoded_as_utf8_bytes() {
    assert_eq!(urlencode("μ"), "%CE%BC");
}

// ---------------------------------------------------------------------------
// Schema drift
// ---------------------------------------------------------------------------

/// An instance set up before `unit` was added accepts a write containing it,
/// returns 200, and discards the field. The only cheap way to see this is that
/// PocketBase returns every column of a record even when it is unset, so an
/// absent key means an absent column.
#[test]
fn a_missing_column_is_detected_from_a_returned_record() {
    let old_instance = json!({
        "id": PB_ID,
        "app_id": UUID,
        "item": "Ammonia",
        "amount": 1008.0,
        "buyPrice": 188.9409,
        "location": "Port Tressler",
        "createdAt": "2026-11-01T09:11:29.754Z",
        // no `unit`, no `quality`, no `source`
    });
    assert_eq!(
        missing_columns(&[old_instance], LATE_ASSET_COLUMNS),
        vec!["unit", "quality", "source"]
    );
}

/// An unset column is still a column: PocketBase sends `""` for an empty Text
/// and `0` for an unset Number, so neither must be read as missing.
#[test]
fn an_unset_column_is_not_a_missing_column() {
    let present_but_empty = json!({
        "app_id": UUID,
        "unit": "",
        "quality": 0,
        "source": "",
    });
    assert!(missing_columns(&[present_but_empty], LATE_ASSET_COLUMNS).is_empty());
}

/// With no records there is nothing to infer from, and a false warning on a
/// brand-new instance would be worse than silence.
#[test]
fn an_empty_collection_yields_no_schema_warning() {
    assert!(missing_columns(&[], LATE_ASSET_COLUMNS).is_empty());
}

// ---------------------------------------------------------------------------
// Clearing what a PATCH cannot delete
// ---------------------------------------------------------------------------

/// An orphan sell has no buy side. Firestore deletes those fields via
/// `updateMask`; PocketBase has no mask and a PATCH cannot delete a column, so
/// they must be sent explicitly or a trade re-written as an orphan keeps a
/// `buyPrice` from an asset that no longer applies -- and the web app's Profit
/// column shows a number instead of nothing.
#[test]
fn an_orphan_trade_clears_the_buy_side_rather_than_leaving_it_stale() {
    let mut orphan = trade();
    orphan.buy_price = None;
    orphan.buy_unit = None;
    orphan.buy_location = None;

    let fields = to_pb_fields(&orphan, UUID, &trade_clearable()).unwrap();
    assert_eq!(fields["buyPrice"], json!(0), "must be sent, not omitted");
    assert_eq!(fields["buyUnit"], json!(""));
    assert_eq!(fields["buyLocation"], json!(""));
}

/// A matched sell must still send its real buy-side values, not the blanks.
#[test]
fn a_matched_trade_keeps_its_real_buy_side() {
    let fields = to_pb_fields(&trade(), UUID, &trade_clearable()).unwrap();
    assert_eq!(fields["buyPrice"].as_f64().unwrap(), 188.9409);
    assert_eq!(fields["buyUnit"], json!("SCU"));
    assert_eq!(fields["buyLocation"], json!("Port Tressler"));
}

/// The clearing list must never touch the fields the web app owns: omitting
/// them from a PATCH is exactly what leaves them alone.
#[test]
fn clearing_never_reaches_web_app_owned_fields() {
    for (k, _) in trade_clearable() {
        assert!(
            !["quality", "uexBuyId", "uexSellId"].contains(&k),
            "`{k}` is owned by the web app and must not be written"
        );
    }
    let fields = to_pb_fields(&trade(), UUID, &trade_clearable()).unwrap();
    assert!(!fields.contains_key("uexSellId"));
}

/// PocketBase returns `""` for an unset Text column, so an absent check alone
/// lets an empty `app_id` through -- producing an asset with `id: ""` that can
/// be picked as a sell or merge target and then fails every id-based call on
/// the uuid parse.
#[test]
fn an_empty_app_id_is_rejected_like_a_missing_one() {
    for bad in ["", "   "] {
        let row = json!({
            "id": PB_ID,
            "app_id": bad,
            "item": "Ammonia",
            "amount": 1.0,
            "buyPrice": 1.0,
            "location": "x",
            "createdAt": "x",
        });
        assert!(
            asset_from_pb(&row).is_none(),
            "an app_id of {bad:?} must not produce a record"
        );
    }
}
