//! Turning an approved queue item into backend writes.
//!
//! This module implements INTEGRATIONS.md §8 **once**, for both backends. It
//! takes a `&dyn SyncBackend` and knows nothing about Firestore or PocketBase —
//! the ordering rules are the valuable part and they must not be duplicated per
//! backend, because a rule implemented twice is a rule that will diverge.
//!
//! ## Phase 3 scope: one asset per log line
//!
//! The §8 merge rule — add to an existing asset with the same item and location
//! — is deliberately *not* implemented here yet. It is the one non-idempotent
//! operation in the whole design, so it gets its own phase and its own guard.
//!
//! This is not a gap in the contract: §8 explicitly offers the choice, "If you
//! would rather have strict replay safety than a tidy inventory, write one asset
//! per log line instead and let the user merge in the app." That is exactly what
//! this does, and it is the safer half of the trade-off. [`merge_candidates`]
//! exists so the UI can tell the user when the app would have merged, rather
//! than silently producing a second row.
//!
//! ## What a buy writes
//!
//! A buy creates an `Asset` and **no trade record** (§8). The id is the
//! deterministic trade id, which is also the document id — a mismatch there
//! produces a record the web app can display but never update or delete, so
//! [`build_asset`] is the one place that decides it and the backend derives the
//! document id from `record.id` rather than being passed one separately.

use crate::backend::{BackendError, BackendResult, SyncBackend};
use crate::model::{now_iso, AssetRecord, SOURCE};
use crate::queue::QueueItem;
use crate::refdata::RefData;
use crate::units::CargoUnit;

/// What a write produced, for the queue and for History.
#[derive(Debug, Clone, PartialEq)]
pub struct WriteOutcome {
    pub asset: AssetRecord,
    /// Assets that already existed with this item and location. Non-empty means
    /// the app's merge rule *would* have merged; Phase 3 writes a separate row
    /// and says so rather than deciding for the user.
    pub would_have_merged_into: Vec<String>,
}

/// Build the record a buy writes.
///
/// Pure, and separate from the call, because this is where the field contract
/// lives and it deserves to be tested without a backend at all.
pub fn build_asset(item: &QueueItem, refdata: &RefData, nickname: &str) -> BackendResult<AssetRecord> {
    let name = item.item_name(refdata).ok_or_else(|| {
        BackendError::Other(format!(
            "{} has no item name and {} does not resolve",
            item.id, item.detected.line.resource_guid
        ))
    })?;
    let location = item.location().ok_or_else(|| {
        // Should be unreachable: the queue refuses to approve without one.
        // Checked anyway, because writing a record with an empty location
        // produces a row the user cannot interpret and cannot easily find.
        BackendError::Other(format!("{} has no location", item.id))
    })?;

    Ok(AssetRecord {
        // The document id is derived from this field, never passed separately.
        id: item.id.clone(),
        item: name.to_string(),
        // `amount` and `buyPrice` are both denominated in `unit` (§4). We
        // always write SCU — the contract's preferred unit — so the queue's
        // per-SCU figures map straight across and the display unit, which is a
        // view concern only, never reaches the wire.
        amount: item.amount_scu(),
        unit: Some(CargoUnit::Scu),
        buy_price: item.unit_price_per_scu(),
        location: location.to_string(),
        // Never 0: the log has no quality and an absent field means "unknown",
        // while 0 means "worthless". §4.
        quality: None,
        created_at: now_iso(),
        logged_by: Some(nickname.trim())
            .filter(|n| !n.is_empty())
            .map(str::to_string),
        source: Some(SOURCE.to_string()),
        // Owned by the web app's UEX push. This client must never set it, and
        // `ASSET_FIELD_PATHS` leaves it out of the update mask so a write here
        // cannot delete a value the web app wrote.
        uex_buy_id: None,
    })
}

/// Which existing assets the app's merge rule would have targeted.
///
/// A failure here is not a write failure — it must not block the buy — so it
/// degrades to "unknown" and is logged.
pub async fn merge_candidates(
    backend: &dyn SyncBackend,
    asset: &AssetRecord,
) -> Vec<String> {
    match backend.find_assets(&asset.item, &asset.location).await {
        Ok(found) => found
            .into_iter()
            .map(|a| a.id)
            .filter(|id| id != &asset.id)
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e, "could not check for merge candidates; continuing");
            Vec::new()
        }
    }
}

/// Write an approved buy.
///
/// Idempotent: the id is deterministic and the upsert replaces, so a retry
/// after an ambiguous failure cannot produce a second record or leave stale
/// fields behind. That is what lets the queue re-run an interrupted write
/// without any reconciliation.
pub async fn write_buy(
    backend: &dyn SyncBackend,
    item: &QueueItem,
    refdata: &RefData,
    nickname: &str,
) -> BackendResult<WriteOutcome> {
    if item.detected.line.kind != crate::ids::TradeKind::Buy {
        return Err(BackendError::Other(format!(
            "{} is a sell; the sell path lands in Phase 4",
            item.id
        )));
    }

    let asset = build_asset(item, refdata, nickname)?;

    // Checked before the write, not after: a mismatch here is the failure mode
    // that produces a record the web app can show but never update or delete.
    if asset.id.trim().is_empty() {
        return Err(BackendError::Other(
            "refusing to write an asset with an empty id".into(),
        ));
    }
    if asset.id != item.id {
        return Err(BackendError::Other(format!(
            "record id {} does not match queue item {}",
            asset.id, item.id
        )));
    }

    // Informational, and gathered *before* the write so the count describes the
    // state the user was told about rather than including our own new row.
    let would_have_merged_into = merge_candidates(backend, &asset).await;

    backend.upsert_asset(&asset).await?;

    if !would_have_merged_into.is_empty() {
        tracing::info!(
            id = %asset.id,
            existing = ?would_have_merged_into,
            "wrote a separate asset where the app would have merged (Phase 5)"
        );
    }

    Ok(WriteOutcome {
        asset,
        would_have_merged_into,
    })
}

#[cfg(test)]
#[path = "writer_tests.rs"]
mod tests;
