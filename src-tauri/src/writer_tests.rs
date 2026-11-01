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
                // Some(..): the buy merge rule IS location-keyed, unlike a sell.
                location: Some("Port Tressler".into())
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
async fn write_buy_refuses_a_sell() {
    let (_d, item) = unapproved(SELL, "Port Tressler");
    let backend = MockBackend::new();
    let err = write_buy(&backend, &item, &refdata(), "m1nd")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("write_sell"), "{err}");
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

// ---------------------------------------------------------------------------
// The sell path
// ---------------------------------------------------------------------------
//
// The SELL fixture is 640 SCU at 495.5 aUEC/SCU (amount[317120] / quantity[640]).

/// An asset to sell out of. `created_at` matters: it decides FIFO order.
fn asset_of(id: &str, amount: f64, unit: CargoUnit, location: &str, created_at: &str) -> AssetRecord {
    AssetRecord {
        id: id.into(),
        item: "Ammonia".into(),
        amount,
        unit: Some(unit),
        buy_price: 300.0,
        location: location.into(),
        quality: None,
        created_at: created_at.into(),
        logged_by: Some("m1nd".into()),
        source: Some("log-watcher".into()),
        uex_buy_id: None,
    }
}

/// §8, the rule with real consequences: **the trade is written before the
/// asset is mutated.** Asserted on call order, which is the only way to see it.
#[tokio::test]
async fn a_sell_writes_the_trade_before_it_touches_the_asset() {
    let (_d, item) = approved(SELL, "Baijini Point");
    let backend =
        MockBackend::new().with_asset(asset_of("a1", 1000.0, CargoUnit::Scu, "Port Tressler", "2026-10-01T00:00:00.000Z"));

    let out = write_sell(&backend, &item, &refdata(), "m1nd").await.unwrap();

    let calls = backend.calls();
    let trade_at = calls.iter().position(|c| matches!(c, Call::UpsertTrade(_)));
    let asset_at = calls.iter().position(|c| matches!(c, Call::UpsertAsset(_)));
    assert!(trade_at.is_some() && asset_at.is_some(), "{calls:?}");
    assert!(
        trade_at < asset_at,
        "the trade must land first -- crashing between the two must leave an \
         un-decremented asset, never vanished cargo: {calls:?}"
    );
    assert_eq!(out.trade.asset_id, "a1");
}

/// The case this client exists for: cargo bought at one station and sold at
/// another. Matching on location would find nothing here and silently produce
/// an orphan with no profit.
#[tokio::test]
async fn a_sell_matches_an_asset_bought_somewhere_else() {
    let (_d, item) = approved(SELL, "Baijini Point");
    let backend = MockBackend::new().with_asset(asset_of(
        "bought-at-tressler",
        1000.0,
        CargoUnit::Scu,
        "Port Tressler",
        "2026-10-01T00:00:00.000Z",
    ));

    let out = write_sell(&backend, &item, &refdata(), "m1nd").await.unwrap();

    assert_eq!(out.trade.asset_id, "bought-at-tressler");
    assert_eq!(out.trade.buy_location.as_deref(), Some("Port Tressler"));
    assert_eq!(out.trade.sell_location, "Baijini Point");
    // The lookup must not be narrowed by location.
    assert!(
        backend.calls().contains(&Call::FindAssets {
            item: "Ammonia".into(),
            location: None
        }),
        "a sell must search every location: {:?}",
        backend.calls()
    );
}

/// A partial sell decrements and leaves everything else alone (§8).
#[tokio::test]
async fn a_partial_sell_decrements_and_preserves_the_other_fields() {
    let (_d, item) = approved(SELL, "Baijini Point");
    let mut existing = asset_of("a1", 1000.0, CargoUnit::Scu, "Port Tressler", "2026-10-01T00:00:00.000Z");
    // Fields this client never writes, owned by the web app. They must survive.
    existing.quality = Some(42.0);
    existing.uex_buy_id = Some(777);
    let created = existing.created_at.clone();
    let backend = MockBackend::new().with_asset(existing);

    let out = write_sell(&backend, &item, &refdata(), "m1nd").await.unwrap();

    assert_eq!(out.asset_effect, AssetEffect::Decremented { remaining: 360.0 });
    let after = backend.asset("a1").expect("still there");
    assert_eq!(after.amount, 360.0, "1000 - 640");
    assert_eq!(after.quality, Some(42.0), "the web app owns quality");
    assert_eq!(after.uex_buy_id, Some(777), "never clobber uexBuyId");
    assert_eq!(after.created_at, created, "a partial sell is not a new asset");
    assert_eq!(after.buy_price, 300.0);
    assert_eq!(backend.trade_count(), 1);
}

/// §8: delete the asset when the result is <= 0.
#[tokio::test]
async fn selling_the_last_of_an_asset_deletes_it() {
    let (_d, item) = approved(SELL, "Baijini Point");
    let backend = MockBackend::new().with_asset(asset_of(
        "a1", 640.0, CargoUnit::Scu, "Port Tressler", "2026-10-01T00:00:00.000Z",
    ));

    let out = write_sell(&backend, &item, &refdata(), "m1nd").await.unwrap();

    assert_eq!(out.asset_effect, AssetEffect::Deleted);
    assert!(backend.asset("a1").is_none(), "sold out, so removed");
    assert_eq!(backend.trade_count(), 1, "the trade still stands");
    assert!(backend.calls().contains(&Call::DeleteAsset("a1".into())));
}

/// Over-selling is clamped, never written negative (§8).
#[tokio::test]
async fn an_over_sell_is_clamped_and_reported() {
    let (_d, item) = approved(SELL, "Baijini Point");
    let backend = MockBackend::new().with_asset(asset_of(
        "a1", 100.0, CargoUnit::Scu, "Port Tressler", "2026-10-01T00:00:00.000Z",
    ));

    let out = write_sell(&backend, &item, &refdata(), "m1nd").await.unwrap();

    assert_eq!(out.trade.amount_sold, 100.0, "clamped to what existed");
    assert_eq!(out.over_sold_by, 540.0, "640 requested, 100 available");
    assert_eq!(out.asset_effect, AssetEffect::Deleted);
    assert!(
        backend.assets().iter().all(|a| a.amount >= 0.0),
        "never write a negative amount"
    );
}

/// An orphan: §8 prescribes the trade's own id as `assetId`, and no buy-side
/// fields, so the web app's Profit column honestly shows an em dash.
#[tokio::test]
async fn a_sell_with_no_matching_asset_is_an_orphan_trade() {
    let (_d, item) = approved(SELL, "Baijini Point");
    let backend = MockBackend::new();

    let out = write_sell(&backend, &item, &refdata(), "m1nd").await.unwrap();

    assert_eq!(out.asset_effect, AssetEffect::Orphan);
    assert_eq!(out.trade.asset_id, out.trade.id, "assetId is its own id");
    assert_eq!(out.trade.buy_price, None);
    assert_eq!(out.trade.buy_location, None);
    assert_eq!(out.trade.buy_unit, None);
    assert_eq!(backend.asset_count(), 0, "an orphan creates no asset");

    // And the omissions must be absent from the JSON, not null.
    let json = serde_json::to_value(&out.trade).unwrap();
    let obj = json.as_object().unwrap();
    for k in ["buyPrice", "buyLocation", "buyUnit", "uexSellId"] {
        assert!(!obj.contains_key(k), "{k} must be absent: {json}");
    }
}

/// The profit-correctness property. The web app computes
/// `(sellPrice - buyPrice) * amountSold` with **no unit conversion**, so a
/// trade whose `unit` and `buyUnit` disagree displays a wrong number. Writing
/// the sell side in the asset's unit is what guarantees they agree.
#[tokio::test]
async fn the_trade_is_denominated_in_the_assets_unit() {
    let (_d, item) = approved(SELL, "Baijini Point");
    // An asset the web app wrote in cSCU, not our preferred SCU.
    let mut existing = asset_of("a1", 100_000.0, CargoUnit::CentiScu, "Port Tressler", "2026-10-01T00:00:00.000Z");
    existing.buy_price = 3.0; // aUEC per cSCU
    let backend = MockBackend::new().with_asset(existing);

    let out = write_sell(&backend, &item, &refdata(), "m1nd").await.unwrap();

    assert_eq!(out.trade.unit, Some(CargoUnit::CentiScu));
    assert_eq!(out.trade.buy_unit, Some(CargoUnit::CentiScu));
    assert_eq!(
        out.trade.unit, out.trade.buy_unit,
        "unit and buyUnit must agree or the web app's profit is wrong"
    );
    // 640 SCU == 64 000 cSCU, and 495.5/SCU == 4.955/cSCU.
    assert_eq!(out.trade.amount_sold, 64_000.0);
    assert!((out.trade.sell_price - 4.955).abs() < 1e-9, "{}", out.trade.sell_price);
    assert_eq!(backend.asset("a1").unwrap().amount, 36_000.0);
}

/// If the trade cannot be written, the asset must be left completely alone.
/// This is the half of the ordering rule that a happy-path test cannot show.
#[tokio::test]
async fn a_failed_trade_write_leaves_the_asset_untouched() {
    let (_d, item) = approved(SELL, "Baijini Point");
    let backend = MockBackend::new()
        .with_asset(asset_of("a1", 1000.0, CargoUnit::Scu, "Port Tressler", "2026-10-01T00:00:00.000Z"))
        .with_faults(Faults {
            fail_all_trades: true,
            ..Default::default()
        });

    let err = write_sell(&backend, &item, &refdata(), "m1nd").await.unwrap_err();

    assert!(err.to_string().contains("trade upsert"), "{err}");
    assert_eq!(backend.asset("a1").unwrap().amount, 1000.0, "untouched");
    assert!(
        !backend.calls().iter().any(|c| matches!(c, Call::UpsertAsset(_) | Call::DeleteAsset(_))),
        "the asset must not be touched at all: {:?}",
        backend.calls()
    );
}

/// The reverse: the trade landed but the asset mutation failed. The write
/// reports an error so the item is retryable, and the trade stays -- that is
/// the discrepancy §8 deliberately prefers.
#[tokio::test]
async fn a_failed_asset_mutation_still_leaves_the_trade() {
    let (_d, item) = approved(SELL, "Baijini Point");
    let backend = MockBackend::new()
        .with_asset(asset_of("a1", 640.0, CargoUnit::Scu, "Port Tressler", "2026-10-01T00:00:00.000Z"))
        .with_faults(Faults {
            fail_delete: true,
            ..Default::default()
        });

    let err = write_sell(&backend, &item, &refdata(), "m1nd").await.unwrap_err();

    assert!(err.to_string().contains("delete"), "{err}");
    assert_eq!(backend.trade_count(), 1, "the trade survives the failure");
    assert_eq!(
        backend.asset("a1").unwrap().amount,
        640.0,
        "un-decremented, which is the safe direction"
    );
}

/// FIFO, and deterministic: a retry must pick the same asset, or a re-run
/// decrements a different one.
#[tokio::test]
async fn the_oldest_matching_asset_is_sold_first() {
    let (_d, item) = approved(SELL, "Baijini Point");
    let backend = MockBackend::new()
        .with_asset(asset_of("newer", 1000.0, CargoUnit::Scu, "Area18", "2026-10-05T00:00:00.000Z"))
        .with_asset(asset_of("older", 1000.0, CargoUnit::Scu, "Port Tressler", "2026-10-01T00:00:00.000Z"));

    let out = write_sell(&backend, &item, &refdata(), "m1nd").await.unwrap();

    assert_eq!(out.trade.asset_id, "older");
    assert_eq!(out.other_candidates, vec!["newer".to_string()]);
    assert_eq!(backend.asset("newer").unwrap().amount, 1000.0, "untouched");
}

/// `soldAt` comes from the log, not the clock: the item may have sat in the
/// queue for an hour before the user reviewed it.
#[tokio::test]
async fn sold_at_is_the_log_timestamp() {
    let (_d, item) = approved(SELL, "Baijini Point");
    let backend = MockBackend::new();
    let out = write_sell(&backend, &item, &refdata(), "m1nd").await.unwrap();
    assert_eq!(out.trade.sold_at, "2026-11-01T11:09:47.325Z");
}

/// The document id and the record's own `id` must agree, for trades exactly as
/// for assets -- a mismatch is displayable but not updatable or deletable.
#[tokio::test]
async fn the_trade_id_is_the_queue_item_id() {
    let (_d, item) = approved(SELL, "Baijini Point");
    let backend = MockBackend::new();
    let out = write_sell(&backend, &item, &refdata(), "m1nd").await.unwrap();
    assert_eq!(out.trade.id, item.id);
    assert!(backend.calls().contains(&Call::UpsertTrade(item.id.clone())));
}

/// A sell writes no asset of its own -- only a trade, plus a mutation of one
/// that already existed. Writing an asset here would mean recording that the
/// user owns what they just sold.
#[tokio::test]
async fn a_sell_never_creates_an_asset() {
    let (_d, item) = approved(SELL, "Baijini Point");
    let backend = MockBackend::new();
    write_sell(&backend, &item, &refdata(), "m1nd").await.unwrap();
    assert_eq!(backend.asset_count(), 0);
    assert!(
        !backend.calls().iter().any(|c| matches!(c, Call::UpsertAsset(_))),
        "{:?}",
        backend.calls()
    );
}
