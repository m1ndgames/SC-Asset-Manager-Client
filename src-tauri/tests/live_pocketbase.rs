//! The PocketBase round trip, against a real instance.
//!
//! Ignored by default. It needs `backend.kind = "pocketbase"` and a URL plus a
//! member account in `%APPDATA%\sc-asset-manager-client\config.toml`, and it
//! writes to and deletes from that instance.
//!
//!   cargo test --test live_pocketbase -- --ignored --nocapture
//!
//! **This has never been run.** The PocketBase backend was written against
//! POCKETBASE.md and INTEGRATIONS.md with no instance available to test
//! against, so everything below the mapping layer — the auth handshake, the
//! PATCH-then-POST upsert, the filter syntax, pagination — is unverified on
//! the wire. This test is how that gets settled; treat a failure here as the
//! expected first outcome rather than a surprise.
//!
//! It is also the only check of the two things most likely to be wrong:
//! whether PocketBase accepts `Authorization: <token>` without a `Bearer`
//! prefix, and whether it accepts a client-supplied `id` on create.

use sc_asset_manager_client_lib::backend::pocketbase::PocketbaseBackend;
use sc_asset_manager_client_lib::backend::SyncBackend;
use sc_asset_manager_client_lib::config::{self, BackendKind};
use sc_asset_manager_client_lib::model::AssetRecord;
use sc_asset_manager_client_lib::units::CargoUnit;

/// A uuid beginning with a digit, so the `r` prefix is exercised rather than
/// avoided — that is the case a real id usually hits.
const TEST_ID: &str = "00000000-0000-5000-8000-0000000c1ea7";

#[tokio::test]
#[ignore = "writes to the configured PocketBase instance"]
async fn pocketbase_round_trip() {
    let cfg = config::load().expect("config loads");
    if cfg.backend.kind != BackendKind::Pocketbase || !cfg.backend_ready() {
        eprintln!("PocketBase not configured; skipping");
        return;
    }
    let b = PocketbaseBackend::from_config(&cfg).expect("backend builds");

    // 1. Auth and read. Fails here => the token header or the auth collection
    //    name is wrong.
    let health = b.health_check().await.expect("health check");
    eprintln!("\n=== PocketBase health ===");
    eprintln!("user: {} ({})", health.email, health.user_id);
    eprintln!("assets: {:?}  trades: {:?}", health.asset_count, health.trade_count);
    for n in &health.notes {
        eprintln!("note: {n}");
    }
    assert!(health.ok, "health check failed: {:?}", health.notes);

    let asset = AssetRecord {
        id: TEST_ID.into(),
        item: "ZZZ-CLIENT-TEST".into(),
        amount: 1008.0,
        unit: Some(CargoUnit::Scu),
        // Deliberately unrounded: the contract requires it to survive exactly.
        buy_price: 188.9409,
        location: "ZZZ-TEST-LOCATION".into(),
        quality: None,
        created_at: "2026-11-01T09:11:29.754Z".into(),
        logged_by: Some(cfg.nickname.clone()),
        source: Some("log-watcher".into()),
        uex_buy_id: None,
    };

    // 2. Create. This is the POST fallback path, since nothing exists yet.
    //    Fails here => PocketBase is rejecting a client-supplied id.
    b.upsert_asset(&asset).await.expect("create");

    // 3. Read it back by the app uuid, not the mangled key.
    let got = b
        .get_asset(TEST_ID)
        .await
        .expect("get")
        .expect("the asset we just wrote");
    assert_eq!(got.id, TEST_ID, "app_id must come back as the record id");
    assert_eq!(got.buy_price, 188.9409, "an unrounded price must survive");
    assert_eq!(got.unit, Some(CargoUnit::Scu));
    assert_eq!(got.quality, None, "an absent quality must stay absent");

    // 4. The same write again: the PATCH path, and it must be idempotent.
    b.upsert_asset(&asset).await.expect("update");
    let again = b.get_asset(TEST_ID).await.expect("get").expect("still there");
    assert_eq!(again, got, "a repeated upsert must change nothing");

    // 5. Both filter shapes. `Some` is the buy merge rule, `None` is the sell
    //    lookup; a broken filter would return everything and look correct.
    let by_item = b.find_assets("ZZZ-CLIENT-TEST", None).await.expect("find by item");
    assert_eq!(by_item.len(), 1, "exactly our test asset");

    let here = b
        .find_assets("ZZZ-CLIENT-TEST", Some("ZZZ-TEST-LOCATION"))
        .await
        .expect("find by item and location");
    assert_eq!(here.len(), 1);

    let elsewhere = b
        .find_assets("ZZZ-CLIENT-TEST", Some("ZZZ-SOMEWHERE-ELSE"))
        .await
        .expect("find at the wrong place");
    assert!(
        elsewhere.is_empty(),
        "a location filter that matches nothing must return nothing, not everything"
    );

    // 6. Clean up, and confirm the delete actually happened.
    b.delete_asset(TEST_ID).await.expect("delete");
    assert!(
        b.get_asset(TEST_ID).await.expect("get after delete").is_none(),
        "the test asset is still there"
    );

    // 7. A delete of something absent is not an error — a retried sell must
    //    not fail because the asset is already gone.
    b.delete_asset(TEST_ID).await.expect("a second delete is a no-op");

    eprintln!("\nPocketBase round trip passed.\n");
}
