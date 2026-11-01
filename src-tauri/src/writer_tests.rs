//! `writer.rs` is one of the two highest-value test targets in the project
//! (the other is `parse.rs`), and like the parser it is tested with no network
//! and no filesystem — `backend/mock.rs` stands in.
//!
//! The field contract is what these assert. A wrong `unit`, a `quality` of 0
//! instead of absent, or a `uexBuyId` written at all are all silent: the write
//! succeeds and the damage shows up in someone else's browser.

use super::*;
use crate::backend::mock::{Call, Faults, MockBackend};
use crate::log::parse::{parse_line, ParsedLine};
use crate::log::scan::DetectedTrade;
use crate::queue::{ItemPatch, Queue};

const BUY: &str = "<2026-11-01T17:49:12.944Z> [Notice] <CEntityComponentCommodityUIProvider::SendCommodityBuyRequest> Sending SShopCommodityBuyRequest - playerId[214115750165] shopId[863043433450] shopName[SCShop_Admin_lt_base_g] kioskId[863043433446] price[50069.000000] shopPricePerCentiSCU[7.823200] resourceGUID[a11394c9-ad7c-404d-9209-3a5e57bb4aa4] autoLoading[1] quantity[6400.000000 cSCU] Cargo Box Data: boxSize[8.000000] | unitAmount[8] [Team_CoreGameplayFeatures][Shops][UI]";
const SELL: &str = "<2026-11-01T11:09:47.325Z> [Notice] <CEntityComponentCommodityUIProvider::SendCommoditySellRequest> Sending SShopCommoditySellRequest - playerId[214115750165] shopId[862401115218] shopName[SCShop_Admin_lt_base_g] kioskId[862401115214] amount[317120.000000] resourceGUID[a11394c9-ad7c-404d-9209-3a5e57bb4aa4] autoLoading[1] quantity[640] transactionMode[ResourceContainer] Cargo Box Data:  [boxSize[8] | unitAmount[80]] [Team_CoreGameplayFeatures][Shops][UI]";

fn refdata() -> RefData {
    RefData {
        commodities: crate::refdata::parse_commodities(
            r#"{"a11394c9-ad7c-404d-9209-3a5e57bb4aa4":{"name":"Ammonia","key":"Ammonia"}}"#,
        )
        .unwrap(),
        locations: vec!["Port Tressler".into()],
        ..Default::default()
    }
}

/// An approved item, built through the real queue so the states are honest.
fn approved(line: &str, location: &str) -> (tempfile::TempDir, QueueItem) {
    let dir = tempfile::tempdir().unwrap();
    let mut q = Queue::open(dir.path());
    let ParsedLine::Trade(t) = parse_line(line).unwrap() else {
        panic!("not a trade")
    };
    let id = q
        .enqueue(DetectedTrade {
            line: *t,
            location_id_hint: None,
        })
        .unwrap()
        .id
        .clone();
    q.update(
        &id,
        ItemPatch {
            location: Some(Some(location.into())),
            ..Default::default()
        },
    )
    .unwrap();
    q.approve(&id, &refdata()).unwrap();
    let item = q.claim_for_writing().unwrap().unwrap();
    (dir, item)
}

/// Like `approved`, but stopping before `approve` - which refuses a sell
/// outright (see `queue::approve`). `write_buy` does not read `state`, so this
/// still exercises the real refusal: that `write_buy` itself rejects a sell no
/// matter how one reached it. Keeping both guards tested matters, because they
/// defend different failures - the queue one stops a user approving a sell, and
/// this one stops a future caller writing one directly.
fn unapproved(line: &str, location: &str) -> (tempfile::TempDir, QueueItem) {
    let dir = tempfile::tempdir().unwrap();
    let mut q = Queue::open(dir.path());
    let ParsedLine::Trade(t) = parse_line(line).unwrap() else {
        panic!("not a trade")
    };
    let id = q
        .enqueue(DetectedTrade {
            line: *t,
            location_id_hint: None,
        })
        .unwrap()
        .id
        .clone();
    q.update(
        &id,
        ItemPatch {
            location: Some(Some(location.into())),
            ..Default::default()
        },
    )
    .unwrap();
    let item = q.get(&id).unwrap().clone();
    (dir, item)
}

// ---------------------------------------------------------------------------
// The field contract
// ---------------------------------------------------------------------------

#[test]
fn the_record_matches_the_field_contract() {
    let (_d, item) = approved(BUY, "Port Tressler");
    let a = build_asset(&item, &refdata(), "m1nd").unwrap();

    assert_eq!(a.id, item.id);
    assert_eq!(a.item, "Ammonia");
    // 6400 cSCU normalised to SCU, and the price per SCU from
    // shopPricePerCentiSCU x 100 -- never price/quantity.
    assert_eq!(a.amount, 64.0);
    // Compared with a tolerance, not against 782.32, because 7.8232 x 100 in
    // f64 is 782.3199999999999 and that is the value we must write. Snapping
    // it to the prettier decimal would be rounding a price, which is the one
    // thing this codebase must not do.
    assert!((a.buy_price - 782.32).abs() < 1e-9, "{}", a.buy_price);
    assert_eq!(a.unit, Some(CargoUnit::Scu));
    assert_eq!(a.location, "Port Tressler");
    assert_eq!(a.logged_by.as_deref(), Some("m1nd"));
    assert_eq!(a.source.as_deref(), Some("log-watcher"));
    assert_eq!(a.quality, None);
    assert_eq!(a.uex_buy_id, None);
}

/// `quality` and `uexBuyId` must be *absent*, not null and not 0. An absent
/// quality means "unknown"; a 0 means "worthless", and the web app owns the
/// field.
#[test]
fn quality_and_uex_are_omitted_from_the_json_entirely() {
    let (_d, item) = approved(BUY, "Port Tressler");
    let a = build_asset(&item, &refdata(), "m1nd").unwrap();
    let json = serde_json::to_string(&a).unwrap();

    assert!(!json.contains("quality"), "{json}");
    assert!(!json.contains("uexBuyId"), "{json}");
    assert!(!json.contains("null"), "{json}");
    // And the fields that must be there, spelled as the contract spells them.
    for key in ["id", "item", "amount", "unit", "buyPrice", "location", "createdAt", "loggedBy", "source"] {
        assert!(json.contains(&format!("\"{key}\"")), "missing {key} in {json}");
    }
}

#[test]
fn the_unit_keeps_its_contract_spelling() {
    let (_d, item) = approved(BUY, "Port Tressler");
    let a = build_asset(&item, &refdata(), "m1nd").unwrap();
    let json = serde_json::to_string(&a).unwrap();
    assert!(json.contains(r#""unit":"SCU""#), "{json}");
}

/// The display unit is a view concern. Whatever the user is looking at, SCU is
/// what goes on the wire — otherwise amount and price could reach the backend
/// on different denominators.
#[test]
fn the_display_unit_never_reaches_the_record() {
    let dir = tempfile::tempdir().unwrap();
    let mut q = Queue::open(dir.path());
    let ParsedLine::Trade(t) = parse_line(BUY).unwrap() else {
        panic!()
    };
    let id = q
        .enqueue(DetectedTrade {
            line: *t,
            location_id_hint: None,
        })
        .unwrap()
        .id
        .clone();
    q.update(
        &id,
        ItemPatch {
            location: Some(Some("Port Tressler".into())),
            display_unit: Some(CargoUnit::MicroScu),
            ..Default::default()
        },
    )
    .unwrap();
    q.approve(&id, &refdata()).unwrap();
    let item = q.claim_for_writing().unwrap().unwrap();

    let a = build_asset(&item, &refdata(), "m1nd").unwrap();
    assert_eq!(a.unit, Some(CargoUnit::Scu));
    assert_eq!(a.amount, 64.0);
    assert!((a.buy_price - 782.32).abs() < 1e-9);
}

/// An unrounded price must survive exactly — rounding it is the thing this
/// codebase must never do.
#[test]
fn an_unrounded_price_is_written_verbatim() {
    let (_d, item) = approved(BUY, "Port Tressler");
    let mut item = item;
    item.edits.unit_price_per_scu = Some(188.9409);
    let a = build_asset(&item, &refdata(), "m1nd").unwrap();
    assert_eq!(a.buy_price, 188.9409);
    assert!(
        serde_json::to_string(&a).unwrap().contains("188.9409"),
        "the price must not be reformatted on the way out"
    );
}

#[test]
fn an_empty_nickname_omits_logged_by_rather_than_writing_blank() {
    let (_d, item) = approved(BUY, "Port Tressler");
    for nick in ["", "   "] {
        let a = build_asset(&item, &refdata(), nick).unwrap();
        assert_eq!(a.logged_by, None);
        assert!(!serde_json::to_string(&a).unwrap().contains("loggedBy"));
    }
}

#[test]
fn a_user_override_beats_the_resolved_name() {
    let (_d, mut item) = approved(BUY, "Port Tressler");
    item.edits.item_name = Some("Ammonia (hand-corrected)".into());
    let a = build_asset(&item, &refdata(), "m1nd").unwrap();
    assert_eq!(a.item, "Ammonia (hand-corrected)");
}

#[test]
fn an_unresolvable_item_is_refused_rather_than_guessed() {
    let (_d, item) = approved(BUY, "Port Tressler");
    let empty = RefData::default();
    let err = build_asset(&item, &empty, "m1nd").unwrap_err();
    assert_eq!(err.kind(), "other");
}

// ---------------------------------------------------------------------------
// The write
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_buy_writes_one_asset_and_no_trade() {
    let (_d, item) = approved(BUY, "Port Tressler");
    let backend = MockBackend::new();

    let out = write_buy(&backend, &item, &refdata(), "m1nd").await.unwrap();

    assert_eq!(backend.asset_count(), 1);
    assert_eq!(out.asset.item, "Ammonia");
    assert!(out.would_have_merged_into.is_empty());
    // One find (the merge check) then one upsert. A buy writes no trade: §8.
    assert_eq!(
        backend.calls(),
        vec![
            Call::FindAssets {
                item: "Ammonia".into(),
                location: "Port Tressler".into()
            },
            Call::UpsertAsset(item.id.clone()),
        ]
    );
}

/// The property the queue's recovery depends on: writing twice leaves one
/// document, unchanged.
#[tokio::test]
async fn writing_twice_is_idempotent() {
    let (_d, item) = approved(BUY, "Port Tressler");
    let backend = MockBackend::new();

    let first = write_buy(&backend, &item, &refdata(), "m1nd").await.unwrap();
    let second = write_buy(&backend, &item, &refdata(), "m1nd").await.unwrap();

    assert_eq!(backend.asset_count(), 1, "no second document");
    // `createdAt` is our clock, so it may differ; everything the contract
    // keys on must not.
    assert_eq!(first.asset.id, second.asset.id);
    assert_eq!(first.asset.amount, second.asset.amount);
    assert_eq!(first.asset.buy_price, second.asset.buy_price);
    assert_eq!(backend.asset(&item.id).unwrap().amount, 64.0);
}

/// A failed write must leave nothing behind, and the retry must succeed.
#[tokio::test]
async fn a_failed_write_writes_nothing_and_a_retry_lands_cleanly() {
    let (_d, item) = approved(BUY, "Port Tressler");
    let backend = MockBackend::new().with_faults(Faults {
        fail_upsert_once: Some(item.id.clone()),
        ..Default::default()
    });

    let err = write_buy(&backend, &item, &refdata(), "m1nd")
        .await
        .unwrap_err();
    assert_eq!(err.kind(), "network");
    assert_eq!(backend.asset_count(), 0, "a failure must not half-write");

    write_buy(&backend, &item, &refdata(), "m1nd").await.unwrap();
    assert_eq!(backend.asset_count(), 1);
    assert_eq!(backend.asset(&item.id).unwrap().amount, 64.0);
}

/// Phase 3 writes one asset per log line. INTEGRATIONS.md §8 sanctions this
/// ("strict replay safety ... rather than a tidy inventory"), but the user has
/// to be told, so the outcome names what would have been merged.
#[tokio::test]
async fn an_existing_asset_is_reported_not_merged_into() {
    let (_d, item) = approved(BUY, "Port Tressler");
    let existing = AssetRecord {
        id: "an-older-asset".into(),
        item: "Ammonia".into(),
        amount: 100.0,
        unit: Some(CargoUnit::Scu),
        buy_price: 500.0,
        location: "Port Tressler".into(),
        quality: None,
        created_at: "2026-10-01T00:00:00.000Z".into(),
        logged_by: Some("m1nd".into()),
        source: Some("log-watcher".into()),
        uex_buy_id: None,
    };
    let backend = MockBackend::new().with_asset(existing);

    let out = write_buy(&backend, &item, &refdata(), "m1nd").await.unwrap();

    assert_eq!(out.would_have_merged_into, vec!["an-older-asset".to_string()]);
    assert_eq!(backend.asset_count(), 2, "a separate row, not a merge");
    // The existing asset must be untouched — no quantity added, no price
    // overwritten. Phase 5 is what changes this.
    let older = backend.asset("an-older-asset").unwrap();
    assert_eq!(older.amount, 100.0);
    assert_eq!(older.buy_price, 500.0);
}

/// A different location is a different asset, so there is nothing to report.
#[tokio::test]
async fn an_asset_elsewhere_is_not_a_merge_candidate() {
    let (_d, item) = approved(BUY, "Port Tressler");
    let backend = MockBackend::new().with_asset(AssetRecord {
        id: "asset-at-area18".into(),
        item: "Ammonia".into(),
        amount: 100.0,
        unit: Some(CargoUnit::Scu),
        buy_price: 500.0,
        location: "Area18".into(),
        quality: None,
        created_at: "2026-10-01T00:00:00.000Z".into(),
        logged_by: None,
        source: None,
        uex_buy_id: None,
    });

    let out = write_buy(&backend, &item, &refdata(), "m1nd").await.unwrap();
    assert!(out.would_have_merged_into.is_empty());
}

/// Checking for merge candidates is informational. If it fails, the buy must
/// still be written — refusing would mean a network blip loses the trade.
#[tokio::test]
async fn a_failed_merge_check_does_not_block_the_write() {
    let (_d, item) = approved(BUY, "Port Tressler");
    let backend = MockBackend::new().with_faults(Faults {
        fail_find: true,
        ..Default::default()
    });

    let out = write_buy(&backend, &item, &refdata(), "m1nd").await.unwrap();
    assert!(out.would_have_merged_into.is_empty());
    assert_eq!(backend.asset_count(), 1);
}

#[tokio::test]
async fn a_sell_is_refused_until_phase_4() {
    let (_d, item) = unapproved(SELL, "Port Tressler");
    let backend = MockBackend::new();
    let err = write_buy(&backend, &item, &refdata(), "m1nd")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Phase 4"), "{err}");
    assert_eq!(backend.asset_count(), 0);
}

/// The guard that matters most: a record whose `id` differs from its document
/// id can be displayed by the web app but never updated or deleted.
///
/// The structural defence is that exactly one expression decides it —
/// `build_asset` copies `item.id`, and the backend derives the document id from
/// `record.id` rather than taking one as a separate argument. So the two
/// *cannot* diverge, which this asserts directly; the runtime check in
/// `write_buy` is belt-and-braces for a future caller that builds a record some
/// other way.
#[test]
fn the_record_id_is_always_the_queue_item_id() {
    let (_d, mut item) = approved(BUY, "Port Tressler");
    item.id = "any-other-id".into();
    let a = build_asset(&item, &refdata(), "m1nd").unwrap();
    assert_eq!(a.id, "any-other-id", "the id comes from one place only");
}

/// The reachable half of that guard: an empty id must never be written, since
/// it would land as a document the user can neither find nor remove.
#[tokio::test]
async fn an_empty_id_is_refused_before_any_write() {
    let (_d, mut item) = approved(BUY, "Port Tressler");
    let backend = MockBackend::new();

    item.id = "   ".into();
    let err = write_buy(&backend, &item, &refdata(), "m1nd")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("empty id"), "{err}");
    assert_eq!(backend.asset_count(), 0, "nothing may be written");
}
