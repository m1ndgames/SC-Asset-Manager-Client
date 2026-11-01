//! The queue is the single source of truth, so these tests care about two
//! things above all: that a transition is on disk before it is reported, and
//! that nothing can be approved while the location — the one fact the log does
//! not contain — is still missing.

use super::*;
use crate::log::parse::{parse_line, ParsedLine};
use crate::log::scan::DetectedTrade;

const BUY: &str = "<2026-11-01T17:49:12.944Z> [Notice] <CEntityComponentCommodityUIProvider::SendCommodityBuyRequest> Sending SShopCommodityBuyRequest - playerId[214115750165] shopId[863043433450] shopName[SCShop_Admin_lt_base_g] kioskId[863043433446] price[50069.000000] shopPricePerCentiSCU[7.823200] resourceGUID[a11394c9-ad7c-404d-9209-3a5e57bb4aa4] autoLoading[1] quantity[6400.000000 cSCU] Cargo Box Data: boxSize[8.000000] | unitAmount[8] [Team_CoreGameplayFeatures][Shops][UI]";
const BUY2: &str = "<2026-11-01T09:42:57.034Z> [Notice] <CEntityComponentCommodityUIProvider::SendCommodityBuyRequest> Sending SShopCommodityBuyRequest - playerId[214115750165] shopId[863690742444] shopName[SCShop_Admin_lt_base_g] kioskId[863690742440] price[151296.000000] shopPricePerCentiSCU[3.152000] resourceGUID[0d41371c-3cde-4939-8583-cd718f5edd60] autoLoading[1] quantity[48000.000000 cSCU] Cargo Box Data: boxSize[8.000000] | unitAmount[60] [Team_CoreGameplayFeatures][Shops][UI]";

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
