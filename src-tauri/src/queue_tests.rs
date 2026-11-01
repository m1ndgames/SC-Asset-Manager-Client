//! The queue is the single source of truth, so these tests care about two
//! things above all: that a transition is on disk before it is reported, and
//! that nothing can be approved while the location — the one fact the log does
//! not contain — is still missing.

use super::*;
use crate::log::parse::{parse_line, ParsedLine};
use crate::log::scan::DetectedTrade;

const BUY: &str = "<2026-11-01T17:49:12.944Z> [Notice] <CEntityComponentCommodityUIProvider::SendCommodityBuyRequest> Sending SShopCommodityBuyRequest - playerId[214115750165] shopId[863043433450] shopName[SCShop_Admin_lt_base_g] kioskId[863043433446] price[50069.000000] shopPricePerCentiSCU[7.823200] resourceGUID[a11394c9-ad7c-404d-9209-3a5e57bb4aa4] autoLoading[1] quantity[6400.000000 cSCU] Cargo Box Data: boxSize[8.000000] | unitAmount[8] [Team_CoreGameplayFeatures][Shops][UI]";
const BUY2: &str = "<2026-11-01T09:42:57.034Z> [Notice] <CEntityComponentCommodityUIProvider::SendCommodityBuyRequest> Sending SShopCommodityBuyRequest - playerId[214115750165] shopId[863690742444] shopName[SCShop_Admin_lt_base_g] kioskId[863690742440] price[151296.000000] shopPricePerCentiSCU[3.152000] resourceGUID[0d41371c-3cde-4939-8583-cd718f5edd60] autoLoading[1] quantity[48000.000000 cSCU] Cargo Box Data: boxSize[8.000000] | unitAmount[60] [Team_CoreGameplayFeatures][Shops][UI]";

/// A real `ResourceContainer` sell of Ammonia - the GUID resolves, so once a
/// location is set this item has *no* blockers. That is the point: it isolates
/// the kind guard as the only remaining reason approval can fail.
const SELL: &str = "<2026-04-12T14:42:15.550Z> [Notice] <CEntityComponentCommodityUIProvider::SendCommoditySellRequest> Sending SShopCommoditySellRequest - playerId[000000000000] shopId[9831290495370] shopName[SCShop_Admin_lt_base_g] kioskId[9831290495369] amount[716880.000000] resourceGUID[a11394c9-ad7c-404d-9209-3a5e57bb4aa4] autoLoading[1] quantity[640] transactionMode[ResourceContainer] Cargo Box Data:  [boxSize[32] | unitAmount[20]] [Team_CoreGameplayFeatures][Shops][UI]";

fn detected(line: &str) -> DetectedTrade {
    let ParsedLine::Trade(t) = parse_line(line).expect("fixture parses") else {
        panic!("not a trade")
    };
    DetectedTrade {
        line: *t,
        location_id_hint: None,
    }
}

/// `a11394c9…` is Ammonia; `0d41371c…` is deliberately absent, to exercise the
/// unresolved-GUID path.
fn refdata() -> RefData {
    RefData {
        commodities: crate::refdata::parse_commodities(
            r#"{"a11394c9-ad7c-404d-9209-3a5e57bb4aa4":{"name":"Ammonia","key":"Ammonia"}}"#,
        )
        .unwrap(),
        locations: vec!["Port Tressler".into(), "Area18".into()],
        ..Default::default()
    }
}

fn queue() -> (tempfile::TempDir, Queue) {
    let dir = tempfile::tempdir().unwrap();
    let q = Queue::open(dir.path());
    (dir, q)
}

fn located(loc: &str) -> ItemPatch {
    ItemPatch {
        location: Some(Some(loc.into())),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Enqueue and persistence
// ---------------------------------------------------------------------------

#[test]
fn an_enqueued_item_is_pending_review_and_on_disk() {
    let (dir, mut q) = queue();
    let id = q.enqueue(detected(BUY)).unwrap().id.clone();

    assert_eq!(q.get(&id).unwrap().state, ItemState::PendingReview);
    assert!(dir.path().join("queue.json").exists(), "must persist at once");

    // A fresh process sees the same thing.
    let reopened = Queue::open(dir.path());
    assert_eq!(reopened.items().len(), 1);
    assert_eq!(reopened.get(&id).unwrap().state, ItemState::PendingReview);
}

/// The property that makes the watcher's "persist queue, then advance offset"
/// ordering safe: a re-read line must collapse into the same item, keeping any
/// edits already made.
#[test]
fn enqueueing_the_same_trade_twice_is_idempotent_and_keeps_edits() {
    let (_d, mut q) = queue();
    let id = q.enqueue(detected(BUY)).unwrap().id.clone();
    q.update(&id, located("Port Tressler")).unwrap();

    q.enqueue(detected(BUY)).unwrap();
    assert_eq!(q.items().len(), 1, "no duplicate");
    assert_eq!(
        q.get(&id).unwrap().location(),
        Some("Port Tressler"),
        "a re-read must not wipe what the user typed"
    );
}

#[test]
fn an_already_written_trade_is_not_re_enqueued() {
    let (_d, mut q) = queue();
    let id = q.enqueue(detected(BUY)).unwrap().id.clone();
    q.update(&id, located("Area18")).unwrap();
    q.approve(&id, &refdata()).unwrap();
    q.claim_for_writing().unwrap();
    q.mark_written(&id).unwrap();

    let err = q.enqueue(detected(BUY)).unwrap_err();
    assert_eq!(err.kind(), "queue");
    assert!(q.items().is_empty());
}

#[test]
fn every_edit_is_persisted_immediately() {
    let (dir, mut q) = queue();
    let id = q.enqueue(detected(BUY)).unwrap().id.clone();
    q.update(
        &id,
        ItemPatch {
            location: Some(Some("Port Tressler".into())),
            amount_scu: Some(Some(70.0)),
            ..Default::default()
        },
    )
    .unwrap();

    // Read it back from disk, not from memory.
    let reopened = Queue::open(dir.path());
    let item = reopened.get(&id).unwrap();
    assert_eq!(item.location(), Some("Port Tressler"));
    assert_eq!(item.amount_scu(), 70.0);
}

// ---------------------------------------------------------------------------
// The location gate — the reason a human is in the loop at all
// ---------------------------------------------------------------------------

#[test]
fn approval_is_refused_without_a_location() {
    let (_d, mut q) = queue();
    let id = q.enqueue(detected(BUY)).unwrap().id.clone();

    let err = q.approve(&id, &refdata()).unwrap_err();
    assert_eq!(err.kind(), "notReady");
    match &err {
        AppError::NotReady { blockers } => {
            assert!(blockers.contains(&Blocker::LocationMissing));
        }
        other => panic!("expected NotReady, got {other:?}"),
    }
    assert_eq!(
        q.get(&id).unwrap().state,
        ItemState::PendingReview,
        "a refused approval must not move the item"
    );
}

/// Whitespace is not a location.
#[test]
fn a_blank_location_does_not_count() {
    let (_d, mut q) = queue();
    let id = q.enqueue(detected(BUY)).unwrap().id.clone();
    q.update(&id, located("   ")).unwrap();
    assert_eq!(q.get(&id).unwrap().location(), None);
    assert!(q.approve(&id, &refdata()).is_err());
}

/// An unresolved GUID must be named by the user, not guessed at.
#[test]
fn an_unresolved_item_blocks_approval_until_named() {
    let (_d, mut q) = queue();
    let id = q.enqueue(detected(BUY2)).unwrap().id.clone();
    q.update(&id, located("Area18")).unwrap();

    let err = q.approve(&id, &refdata()).unwrap_err();
    match err {
        AppError::NotReady { blockers } => assert!(blockers.iter().any(|b| matches!(
            b,
            Blocker::ItemUnresolved { guid } if guid == "0d41371c-3cde-4939-8583-cd718f5edd60"
        ))),
        other => panic!("expected NotReady, got {other:?}"),
    }

    q.update(
        &id,
        ItemPatch {
            item_name: Some(Some("Quantanium".into())),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(q.approve(&id, &refdata()).unwrap().state, ItemState::Approved);
}

#[test]
fn a_nonsensical_amount_or_price_blocks_approval() {
    let (_d, mut q) = queue();
    let id = q.enqueue(detected(BUY)).unwrap().id.clone();
    q.update(&id, located("Area18")).unwrap();

    for (patch, want) in [
        (
            ItemPatch {
                amount_scu: Some(Some(0.0)),
                ..Default::default()
            },
            Blocker::AmountInvalid,
        ),
        (
            ItemPatch {
                amount_scu: Some(Some(-5.0)),
                ..Default::default()
            },
            Blocker::AmountInvalid,
        ),
        (
            ItemPatch {
                amount_scu: Some(Some(f64::NAN)),
                ..Default::default()
            },
            Blocker::AmountInvalid,
        ),
        (
            ItemPatch {
                amount_scu: Some(Some(64.0)),
                unit_price_per_scu: Some(Some(-1.0)),
                ..Default::default()
            },
            Blocker::PriceInvalid,
        ),
    ] {
        q.update(&id, patch).unwrap();
        match q.approve(&id, &refdata()).unwrap_err() {
            AppError::NotReady { blockers } => assert!(
                blockers.contains(&want),
                "expected {want:?}, got {blockers:?}"
            ),
            other => panic!("expected NotReady, got {other:?}"),
        }
    }
}

/// A price of zero is legitimate — salvage and some missions genuinely cost
/// nothing — so it must not be rejected along with the nonsense.
#[test]
fn a_zero_price_is_allowed() {
    let (_d, mut q) = queue();
    let id = q.enqueue(detected(BUY)).unwrap().id.clone();
    q.update(
        &id,
        ItemPatch {
            location: Some(Some("Area18".into())),
            unit_price_per_scu: Some(Some(0.0)),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(q.approve(&id, &refdata()).is_ok());
}

// ---------------------------------------------------------------------------
// The patch protocol
// ---------------------------------------------------------------------------

/// Absent, null and a value must be three distinct requests, or "clear the
/// location" and "leave the location alone" become the same call.
#[test]
fn absent_null_and_a_value_are_three_different_requests() {
    let (_d, mut q) = queue();
    let id = q.enqueue(detected(BUY)).unwrap().id.clone();
    q.update(&id, located("Port Tressler")).unwrap();

    // Absent: untouched.
    let p: ItemPatch = serde_json::from_str("{}").unwrap();
    q.update(&id, p).unwrap();
    assert_eq!(q.get(&id).unwrap().location(), Some("Port Tressler"));

    // A value: set.
    let p: ItemPatch = serde_json::from_str(r#"{"location":"Area18"}"#).unwrap();
    q.update(&id, p).unwrap();
    assert_eq!(q.get(&id).unwrap().location(), Some("Area18"));

    // Null: cleared.
    let p: ItemPatch = serde_json::from_str(r#"{"location":null}"#).unwrap();
    q.update(&id, p).unwrap();
    assert_eq!(q.get(&id).unwrap().location(), None);
}

#[test]
fn an_approved_item_can_no_longer_be_edited() {
    let (_d, mut q) = queue();
    let id = q.enqueue(detected(BUY)).unwrap().id.clone();
    q.update(&id, located("Area18")).unwrap();
    q.approve(&id, &refdata()).unwrap();

    let err = q.update(&id, located("Somewhere else")).unwrap_err();
    assert_eq!(err.kind(), "queue");
    assert_eq!(q.get(&id).unwrap().location(), Some("Area18"));
}

// ---------------------------------------------------------------------------
// The write path
// ---------------------------------------------------------------------------

#[test]
fn claiming_marks_writing_on_disk_before_the_caller_acts() {
    let (dir, mut q) = queue();
    let id = q.enqueue(detected(BUY)).unwrap().id.clone();
    q.update(&id, located("Area18")).unwrap();
    q.approve(&id, &refdata()).unwrap();

    let claimed = q.claim_for_writing().unwrap().expect("one approved item");
    assert_eq!(claimed.id, id);
    assert_eq!(claimed.state, ItemState::Writing);
    assert_eq!(claimed.attempts, 1);

    // On disk, so an interrupted write is recoverable.
    let reopened = Queue::open(dir.path());
    assert_eq!(
        reopened.get(&id).unwrap().state,
        ItemState::Approved,
        "reopening must re-queue an interrupted write"
    );
    assert_eq!(q.claim_for_writing().unwrap(), None, "nothing else pending");
}

/// A crash mid-write must leave something retryable, never something stuck.
#[test]
fn an_interrupted_write_is_requeued_on_load() {
    let (dir, mut q) = queue();
    let id = q.enqueue(detected(BUY)).unwrap().id.clone();
    q.update(&id, located("Area18")).unwrap();
    q.approve(&id, &refdata()).unwrap();
    q.claim_for_writing().unwrap();
    drop(q);

    let mut reopened = Queue::open(dir.path());
    assert_eq!(reopened.get(&id).unwrap().state, ItemState::Approved);
    assert!(reopened.claim_for_writing().unwrap().is_some());
}

/// Phase 5's guard, carried now: a merge that already applied must not be
/// replayed, because adding quantity is the one non-idempotent operation here.
#[test]
fn an_interrupted_merge_that_already_applied_goes_straight_to_written() {
    let (dir, mut q) = queue();
    let id = q.enqueue(detected(BUY)).unwrap().id.clone();
    q.update(&id, located("Area18")).unwrap();
    q.approve(&id, &refdata()).unwrap();
    q.claim_for_writing().unwrap();

    // Simulate the writer recording that the merge landed.
    {
        let item = q.item_mut(&id).unwrap();
        item.merge_target_id = Some("existing-asset".into());
        item.merge_applied = true;
        q.persist().unwrap();
    }
    drop(q);

    let reopened = Queue::open(dir.path());
    assert_eq!(
        reopened.get(&id).unwrap().state,
        ItemState::Written,
        "replaying an applied merge would double-count the quantity"
    );
}

#[test]
fn a_written_item_moves_to_history() {
    let (dir, mut q) = queue();
    let id = q.enqueue(detected(BUY)).unwrap().id.clone();
    q.update(&id, located("Area18")).unwrap();
    q.approve(&id, &refdata()).unwrap();
    q.claim_for_writing().unwrap();
    q.mark_written(&id).unwrap();

    assert!(q.items().is_empty(), "no longer in the queue");
    assert_eq!(q.history().len(), 1);
    assert_eq!(q.history()[0].state, ItemState::Written);

    let reopened = Queue::open(dir.path());
    assert!(reopened.items().is_empty());
    assert_eq!(reopened.history().len(), 1);
}

#[test]
fn a_failed_write_keeps_its_error_and_can_be_retried() {
    let (_d, mut q) = queue();
    let id = q.enqueue(detected(BUY)).unwrap().id.clone();
    q.update(&id, located("Area18")).unwrap();
    q.approve(&id, &refdata()).unwrap();
    q.claim_for_writing().unwrap();

    let item = q.mark_failed(&id, "401 Unauthorized").unwrap();
    assert_eq!(item.state, ItemState::Failed);
    assert_eq!(item.error.as_deref(), Some("401 Unauthorized"));

    // Still editable while failed — the failure may have been a bad location.
    q.update(&id, located("Port Tressler")).unwrap();

    let item = q.retry(&id).unwrap();
    assert_eq!(item.state, ItemState::Approved);
    assert_eq!(item.error, None, "a retry must not show a stale error");
    assert_eq!(q.claim_for_writing().unwrap().unwrap().attempts, 2);
}

#[test]
fn rejecting_moves_to_history_and_a_write_in_flight_cannot_be_rejected() {
    let (_d, mut q) = queue();
    let a = q.enqueue(detected(BUY)).unwrap().id.clone();
    q.reject(&a).unwrap();
    assert!(q.items().is_empty());
    assert_eq!(q.history()[0].state, ItemState::Rejected);

    let b = q.enqueue(detected(BUY2)).unwrap().id.clone();
    q.update(&b, located("Area18")).unwrap();
    q.update(
        &b,
        ItemPatch {
            item_name: Some(Some("Quantanium".into())),
            ..Default::default()
        },
    )
    .unwrap();
    q.approve(&b, &refdata()).unwrap();
    q.claim_for_writing().unwrap();
    assert!(q.reject(&b).is_err(), "cannot reject a write in flight");
}

#[test]
fn a_discarded_detection_is_recorded_in_history_only() {
    let (dir, mut q) = queue();
    q.record_discarded(detected(BUY), "CargoCreationFailed".into())
        .unwrap();
    assert!(q.items().is_empty(), "it never needed review");
    assert_eq!(q.history().len(), 1);
    assert_eq!(q.history()[0].state, ItemState::Discarded);
    assert_eq!(Queue::open(dir.path()).history().len(), 1);
}

#[test]
fn pending_items_are_ordered_by_when_they_happened() {
    let (_d, mut q) = queue();
    // BUY is 17:49, BUY2 is 09:42 — enqueued in the wrong order on purpose.
    q.enqueue(detected(BUY)).unwrap();
    q.enqueue(detected(BUY2)).unwrap();
    let order: Vec<&str> = q
        .pending()
        .iter()
        .map(|i| i.detected.line.log_timestamp.as_str())
        .collect();
    assert_eq!(
        order,
        vec!["2026-11-01T09:42:57.034Z", "2026-11-01T17:49:12.944Z"]
    );
}

#[test]
fn an_unknown_id_is_an_error_not_a_panic() {
    let (_d, mut q) = queue();
    assert!(q.update("nope", located("Area18")).is_err());
    assert!(q.approve("nope", &refdata()).is_err());
    assert!(q.reject("nope").is_err());
    assert!(q.retry("nope").is_err());
    assert!(q.mark_written("nope").is_err());
}

/// A corrupt queue.json must not stop the app — but it must be loud, and the
/// file must be left alone for recovery by hand.
#[test]
fn a_corrupt_queue_file_starts_empty_rather_than_refusing_to_run() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("queue.json"), "{ truncated").unwrap();
    let q = Queue::open(dir.path());
    assert!(q.items().is_empty());
    assert!(
        dir.path().join("queue.json").exists(),
        "the damaged file must not be deleted"
    );
}

// ---------------------------------------------------------------------------
// The projection the UI renders
// ---------------------------------------------------------------------------

/// The toggle must rescale amount and price *together*. Letting them land on
/// different denominators is the easiest contract violation available.
#[test]
fn the_display_unit_rescales_amount_and_price_together() {
    let (_d, mut q) = queue();
    let id = q.enqueue(detected(BUY)).unwrap().id.clone();
    let rd = refdata();

    let v = view(q.get(&id).unwrap(), &rd);
    assert_eq!(v.display_unit, CargoUnit::Scu);
    assert_eq!(v.display_amount, 64.0);
    assert!((v.display_unit_price - 782.32).abs() < 1e-9);
    // The product is what must stay invariant.
    let total = v.display_amount * v.display_unit_price;

    for unit in [CargoUnit::CentiScu, CargoUnit::MicroScu, CargoUnit::Scu] {
        q.update(
            &id,
            ItemPatch {
                display_unit: Some(unit),
                ..Default::default()
            },
        )
        .unwrap();
        let v = view(q.get(&id).unwrap(), &rd);
        assert_eq!(v.display_unit, unit);
        assert!(
            (v.display_amount * v.display_unit_price - total).abs() < 1e-6,
            "{unit:?}: amount x price drifted to {}",
            v.display_amount * v.display_unit_price
        );
        // Whatever the view says, what gets written stays in SCU.
        assert_eq!(v.effective_amount_scu, 64.0);
    }
}

#[test]
fn the_view_reports_the_resolved_name_and_the_log_total() {
    let (_d, mut q) = queue();
    let id = q.enqueue(detected(BUY)).unwrap().id.clone();
    let v = view(q.get(&id).unwrap(), &refdata());

    assert_eq!(v.resolved_item_name.as_deref(), Some("Ammonia"));
    // From the log, never amount x price: `price` is rounded to whole aUEC,
    // and 64 x 782.32 = 50068.48, not the logged 50069.
    assert_eq!(v.total_auec, 50069.0);
    assert!(!v.can_approve, "no location yet");
    assert_eq!(v.blockers, vec![Blocker::LocationMissing]);
}

// ---------------------------------------------------------------------------
// The sell guard
// ---------------------------------------------------------------------------

/// Phase 4 landed, so a sell is approvable on exactly the same terms as a buy:
/// a location, a resolvable item, sane figures. The queue does not care which
/// kind it is — `drain` routes to `write_buy` or `write_sell`, and that routing
/// is what used to be guarded here.
///
/// The property that still matters is that a sell needs a **location** like
/// anything else. It is the sell location, and the log does not record it.
#[test]
fn a_sell_is_approvable_once_it_has_a_location() {
    let (_dir, mut q) = queue();
    let rd = refdata();
    let id = q.enqueue(detected(SELL)).unwrap().id.clone();

    let err = q
        .approve(&id, &rd)
        .expect_err("no location yet, so not approvable");
    assert!(
        matches!(err, AppError::NotReady { ref blockers } if blockers.contains(&Blocker::LocationMissing)),
        "got {err:?}"
    );

    q.update(&id, located("Baijini Point")).unwrap();
    q.approve(&id, &rd).expect("a located sell approves");
    assert_eq!(q.get(&id).unwrap().state, ItemState::Approved);
}

/// The mirror case, so the guard cannot be "fixed" by rejecting everything.
#[test]
fn a_buy_is_still_approvable() {
    let (_dir, mut q) = queue();
    let rd = refdata();
    let id = q.enqueue(detected(BUY)).unwrap().id.clone();
    q.update(&id, located("Port Tressler")).unwrap();

    q.approve(&id, &rd).expect("a located, resolved buy approves");
    assert_eq!(q.get(&id).unwrap().state, ItemState::Approved);
}

// ---------------------------------------------------------------------------
// The merge crash guard, from the queue's side
// ---------------------------------------------------------------------------

/// The recovery rule that makes merging survivable. A `Writing` item is
/// normally returned to `Approved` and retried, because every other write is
/// an idempotent upsert. A merge is not: retrying adds the quantity again. So
/// a journalled merge is promoted straight to `Written` instead.
#[test]
fn a_crashed_merge_is_never_retried() {
    let dir = tempfile::tempdir().unwrap();
    let rd = refdata();
    let id = {
        let mut q = Queue::open(dir.path());
        let id = q.enqueue(detected(BUY)).unwrap().id.clone();
        q.update(&id, located("Port Tressler")).unwrap();
        q.approve(&id, &rd).unwrap();
        q.claim_for_writing().unwrap().unwrap();
        // Journalled, then the process dies before the backend answers.
        q.mark_merging(&id, "some-older-asset").unwrap();
        id
    };

    let reopened = Queue::open(dir.path());
    let item = reopened.get(&id).expect("still there");
    assert_eq!(
        item.state,
        ItemState::Written,
        "a journalled merge must not be retried -- repeating it double-counts"
    );
    assert_eq!(item.merge_target_id.as_deref(), Some("some-older-asset"));
    assert!(item.merge_applied);
}

/// The contrast, so the rule above is not just "never retry anything": an
/// ordinary interrupted write goes back to `Approved` and is retried, which is
/// safe because the id is deterministic and the write is an upsert.
#[test]
fn a_crashed_plain_write_is_retried() {
    let dir = tempfile::tempdir().unwrap();
    let rd = refdata();
    let id = {
        let mut q = Queue::open(dir.path());
        let id = q.enqueue(detected(BUY)).unwrap().id.clone();
        q.update(&id, located("Port Tressler")).unwrap();
        q.approve(&id, &rd).unwrap();
        q.claim_for_writing().unwrap().unwrap();
        id
    };

    let reopened = Queue::open(dir.path());
    assert_eq!(reopened.get(&id).unwrap().state, ItemState::Approved);
}

/// CLAUDE.md: a full rescan must not resurrect a merged buy. It cannot, and
/// this is why — `enqueue` refuses an id that is already in history, so
/// re-reading the log from byte 0 offers nothing that was already applied.
/// Without this, a replay would add the same quantity to the same asset again.
#[test]
fn a_rescan_cannot_resurrect_a_merged_buy() {
    let dir = tempfile::tempdir().unwrap();
    let rd = refdata();
    let mut q = Queue::open(dir.path());

    let id = q.enqueue(detected(BUY)).unwrap().id.clone();
    q.update(&id, located("Port Tressler")).unwrap();
    q.approve(&id, &rd).unwrap();
    q.claim_for_writing().unwrap().unwrap();
    q.mark_merging(&id, "older").unwrap();
    q.mark_written(&id).unwrap(); // retires into history itself

    // The rescan re-reads the very same line.
    let err = q
        .enqueue(detected(BUY))
        .expect_err("a written merge must not come back");
    assert!(err.to_string().contains("already written"), "{err}");
    assert!(q.items().is_empty(), "nothing re-queued");

    // And it survives a restart, since history is on disk.
    let mut reopened = Queue::open(dir.path());
    assert!(reopened.enqueue(detected(BUY)).is_err());
}

/// The opt-out round-trips like any other edit, and does not disturb the rest.
#[test]
fn the_separate_asset_opt_out_persists() {
    let dir = tempfile::tempdir().unwrap();
    let mut q = Queue::open(dir.path());
    let id = q.enqueue(detected(BUY)).unwrap().id.clone();
    q.update(&id, located("Port Tressler")).unwrap();

    assert!(!q.get(&id).unwrap().merge_opt_out(), "merging is the default");

    q.update(
        &id,
        ItemPatch {
            separate_asset: Some(true),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(q.get(&id).unwrap().merge_opt_out());
    assert_eq!(
        q.get(&id).unwrap().location(),
        Some("Port Tressler"),
        "an unrelated edit must survive"
    );

    let reopened = Queue::open(dir.path());
    assert!(reopened.get(&id).unwrap().merge_opt_out());
}

// ---------------------------------------------------------------------------
// Restoring from history
// ---------------------------------------------------------------------------

/// The case the feature exists for: the user rejected a trade by mistake. It
/// comes back for review, and the edits they had already made survive.
#[test]
fn a_rejected_trade_can_be_restored_with_its_edits() {
    let dir = tempfile::tempdir().unwrap();
    let mut q = Queue::open(dir.path());
    let id = q.enqueue(detected(BUY)).unwrap().id.clone();
    q.update(&id, located("Port Tressler")).unwrap();
    q.reject(&id).unwrap();

    assert!(q.get(&id).is_none(), "rejected, so out of the queue");
    assert_eq!(q.history().len(), 1);

    let restored = q.restore(&id).unwrap().clone();
    assert_eq!(restored.state, ItemState::PendingReview);
    assert_eq!(
        restored.location(),
        Some("Port Tressler"),
        "the location the user already typed must not be thrown away"
    );
    assert!(restored.error.is_none());
    assert!(q.history().is_empty(), "no longer in history");
    assert_eq!(q.items().len(), 1, "and back in the queue exactly once");

    // And it survives a restart in the new position, not the old one.
    let reopened = Queue::open(dir.path());
    assert_eq!(reopened.items().len(), 1);
    assert!(reopened.history().is_empty());
}

/// A discard comes from the game's own failure line, so restoring one is
/// normally wrong — but the 3 s grace window can attribute an error to the
/// wrong detection when two trades land together, and that is unrecoverable
/// otherwise. So it is allowed, and the UI is what makes it look different
/// from restoring a rejection.
#[test]
fn a_discarded_trade_can_be_restored() {
    let dir = tempfile::tempdir().unwrap();
    let mut q = Queue::open(dir.path());
    q.record_discarded(detected(BUY), "a failure line arrived".into())
        .unwrap();
    let id = q.history()[0].id.clone();

    let restored = q.restore(&id).unwrap();
    assert_eq!(restored.state, ItemState::PendingReview);
    assert!(
        restored.error.is_none(),
        "the discard reason no longer describes it"
    );
}

/// The refusal that matters most. A written sell cannot be restored, because
/// writing it again decrements the asset a second time — data loss, not a
/// duplicate. The same action is offered for buys and sells, so it has to be
/// refused for both rather than being safe only sometimes.
#[test]
fn a_written_trade_cannot_be_restored() {
    let dir = tempfile::tempdir().unwrap();
    let rd = refdata();
    let mut q = Queue::open(dir.path());
    let id = q.enqueue(detected(SELL)).unwrap().id.clone();
    q.update(&id, located("Baijini Point")).unwrap();
    q.approve(&id, &rd).unwrap();
    q.claim_for_writing().unwrap().unwrap();
    q.mark_written(&id).unwrap();

    let err = q.restore(&id).expect_err("must refuse");
    assert!(err.to_string().contains("already written"), "{err}");
    assert_eq!(q.history().len(), 1, "still in history, untouched");
    assert!(q.items().is_empty(), "and not back in the queue");
}

/// A journalled merge is refused even when it did **not** end up `Written`,
/// which is the case the written-check cannot catch.
///
/// The route there is real: the merge is journalled, the backend call then
/// fails, the item becomes `Failed`, and the user rejects it rather than
/// retrying — so it reaches history as `Rejected` while `merge_applied` is
/// true and the quantity may already have been added. Restoring and writing
/// that again would add it twice.
#[test]
fn a_merged_buy_cannot_be_restored_even_when_it_was_not_written() {
    let dir = tempfile::tempdir().unwrap();
    let rd = refdata();
    let mut q = Queue::open(dir.path());
    let id = q.enqueue(detected(BUY)).unwrap().id.clone();
    q.update(&id, located("Port Tressler")).unwrap();
    q.approve(&id, &rd).unwrap();
    q.claim_for_writing().unwrap().unwrap();
    q.mark_merging(&id, "older-asset").unwrap();
    // The backend call fails after the journal was written.
    q.mark_failed(&id, "connection reset").unwrap();
    // The user gives up on it rather than retrying.
    q.reject(&id).unwrap();

    let in_history = &q.history()[0];
    assert_eq!(in_history.state, ItemState::Rejected);
    assert!(in_history.merge_applied, "the journal survived");

    let err = q.restore(&id).expect_err("must refuse a journalled merge");
    assert!(err.to_string().contains("merged into"), "{err}");
    assert!(q.items().is_empty(), "and it stays out of the queue");
}

/// Restoring twice must not produce two queue entries. The second call finds
/// it already in the queue and reports it rather than erroring, because the
/// user double-clicking a button is not a failure.
#[test]
fn restoring_twice_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let mut q = Queue::open(dir.path());
    let id = q.enqueue(detected(BUY)).unwrap().id.clone();
    q.reject(&id).unwrap();

    q.restore(&id).unwrap();
    let again = q.restore(&id).expect("a second restore is not an error");
    assert_eq!(again.state, ItemState::PendingReview);
    assert_eq!(q.items().len(), 1, "exactly one queue entry");
    assert!(q.history().is_empty());
}

/// An id that was never seen is an error, not a silent empty restore.
#[test]
fn restoring_an_unknown_id_fails() {
    let dir = tempfile::tempdir().unwrap();
    let mut q = Queue::open(dir.path());
    assert!(q.restore("no-such-id").is_err());
}

/// A restored item is a normal queue item: it can be approved and written
/// like any other, which is the whole point of putting it back.
#[test]
fn a_restored_trade_can_then_be_approved() {
    let dir = tempfile::tempdir().unwrap();
    let rd = refdata();
    let mut q = Queue::open(dir.path());
    let id = q.enqueue(detected(BUY)).unwrap().id.clone();
    q.reject(&id).unwrap();
    q.restore(&id).unwrap();

    q.update(&id, located("Port Tressler")).unwrap();
    q.approve(&id, &rd).unwrap();
    assert_eq!(q.get(&id).unwrap().state, ItemState::Approved);
}
