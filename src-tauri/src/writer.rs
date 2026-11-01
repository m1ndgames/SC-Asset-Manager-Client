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
use crate::model::{now_iso, AssetRecord, TradeRecord, SOURCE};
use crate::queue::QueueItem;
use crate::refdata::RefData;
use crate::units::{self, CargoUnit};

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
    match backend.find_assets(&asset.item, Some(&asset.location)).await {
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
            "{} is a sell and must go through write_sell, which writes a trade \n             first and then mutates the asset",
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

// ---------------------------------------------------------------------------
// The sell path (INTEGRATIONS.md §8)
// ---------------------------------------------------------------------------

/// What happened to the asset a sell was applied to.
#[derive(Debug, Clone, PartialEq)]
pub enum AssetEffect {
    /// Partially sold. `remaining` is in the asset's own unit.
    Decremented { remaining: f64 },
    /// Sold out, so the asset was deleted (§8: delete when the result is <= 0).
    Deleted,
    /// No asset matched. The trade stands alone.
    Orphan,
}

/// What a sell produced.
#[derive(Debug, Clone, PartialEq)]
pub struct SellWriteOutcome {
    pub trade: TradeRecord,
    /// The asset sold from, as it was *before* the decrement.
    pub matched_asset: Option<AssetRecord>,
    pub asset_effect: AssetEffect,
    /// Other assets of the same item that were left alone. Shown to the user,
    /// never acted on: a sell is applied to exactly one asset.
    pub other_candidates: Vec<String>,
    /// How much of the sell the asset could not cover, in the asset's unit.
    /// Non-zero means the request was clamped — §8 forbids a negative amount.
    pub over_sold_by: f64,
}

/// Choose which asset a sell comes out of.
///
/// Oldest first, by `createdAt` then `id`. FIFO is the conventional inventory
/// rule and it is also the one that makes `buyPrice` meaningful: the oldest
/// holding is the one actually being shipped out. `id` breaks ties so the
/// choice is deterministic — a retry must land on the same asset, or a re-run
/// could decrement a different one.
fn choose_asset(mut candidates: Vec<AssetRecord>) -> Option<(AssetRecord, Vec<String>)> {
    candidates.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
    let chosen = candidates.first().cloned()?;
    let others = candidates.into_iter().skip(1).map(|a| a.id).collect();
    Some((chosen, others))
}

/// Write an approved sell.
///
/// ## Ordering
///
/// **The trade is written first, then the asset is mutated.** There is no
/// transaction, so one of the two happens first, and the failure modes are not
/// symmetric: a trade whose asset was not decremented is a visible discrepancy
/// the user can correct, whereas a decremented asset with no trade is cargo
/// that silently evaporated. §8 mandates this order.
///
/// ## Why the trade is denominated in the *asset's* unit
///
/// The web app computes profit as `(sellPrice - buyPrice) * amountSold` with
/// **no unit conversion**, so a trade where `unit != buyUnit` displays a wrong
/// number. `buyPrice` and `buyUnit` necessarily come from the asset, so the
/// only way to guarantee the two agree is to express the sell side in the
/// asset's unit as well. This client writes SCU, but the web app may have
/// written an asset in cSCU, and that must not silently produce a wrong profit.
pub async fn write_sell(
    backend: &dyn SyncBackend,
    item: &QueueItem,
    refdata: &RefData,
    nickname: &str,
) -> BackendResult<SellWriteOutcome> {
    if item.id.trim().is_empty() {
        return Err(BackendError::Other("sell has no id".into()));
    }
    let name = item
        .item_name(refdata)
        .ok_or_else(|| {
            BackendError::Other(format!(
                "{} has no item name and {} does not resolve",
                item.id, item.detected.line.resource_guid
            ))
        })?
        .to_string();
    let sell_location = item
        .location()
        .ok_or_else(|| BackendError::Other(format!("{} has no location", item.id)))?
        .to_string();

    // Match on item alone. An asset's `location` is where it was *bought* and
    // nothing ever moves it, so narrowing by the sell location would miss on
    // every run that buys in one place and sells in another — which is to say,
    // on trading. See CLAUDE.md.
    let candidates = backend.find_assets(&name, None).await?;
    let matched = choose_asset(candidates);

    let plan = plan_sell(PlanInput {
        id: &item.id,
        name: &name,
        sell_location: &sell_location,
        sold_scu: item.amount_scu(),
        price_per_scu: item.unit_price_per_scu(),
        sold_at: &item.detected.line.log_timestamp,
        nickname,
        matched: &matched,
    });

    // 1. The trade. If this fails nothing has been mutated, and a retry is
    //    clean because the id is deterministic: it overwrites, never duplicates.
    backend.upsert_trade(&plan.trade).await?;

    // 2. Only now the asset. A failure here leaves a trade whose asset was not
    //    decremented -- visible, fixable, and the safer of the two outcomes.
    if let Some((asset, applied)) = &plan.asset_update {
        if applied.delete_asset {
            backend.delete_asset(&asset.id).await?;
        } else {
            let mut updated = asset.clone();
            updated.amount = applied.remaining;
            // Every other field is carried over untouched -- §8: "A partial
            // sell leaves every other field untouched." `quality` and
            // `uexBuyId` are not in the update mask at all, so the web app's
            // values survive even though this client never sets them.
            backend.upsert_asset(&updated).await?;
        }
    }

    Ok(SellWriteOutcome {
        trade: plan.trade,
        matched_asset: matched.map(|(a, _)| a),
        asset_effect: plan.effect,
        other_candidates: plan.others,
        over_sold_by: plan.over_sold_by,
    })
}

/// Everything `plan_sell` needs, gathered so the signature stays readable.
struct PlanInput<'a> {
    id: &'a str,
    name: &'a str,
    sell_location: &'a str,
    sold_scu: f64,
    price_per_scu: f64,
    sold_at: &'a str,
    nickname: &'a str,
    matched: &'a Option<(AssetRecord, Vec<String>)>,
}

/// What a sell *would* do. Pure.
///
/// Split out of [`write_sell`] so the review card can show the user the
/// consequences — which asset is hit, what remains, whether it will be deleted,
/// whether they are over-selling — using the identical arithmetic that the
/// write will then perform. A preview computed by a second implementation
/// would eventually disagree with the write, and the disagreement would show up
/// as a surprise in a shared database.
struct SellPlan {
    trade: TradeRecord,
    asset_update: Option<(AssetRecord, units::SellOutcome)>,
    effect: AssetEffect,
    others: Vec<String>,
    over_sold_by: f64,
}

fn plan_sell(input: PlanInput<'_>) -> SellPlan {
    let PlanInput {
        id,
        name,
        sell_location,
        sold_scu,
        price_per_scu,
        sold_at,
        nickname,
        matched,
    } = input;

    let logged_by = Some(nickname.trim())
        .filter(|n| !n.is_empty())
        .map(str::to_string);

    let (trade, asset_update, effect, others, over_sold_by) = match matched {
        None => {
            // An orphan: cargo this client never saw bought. §8 says that is
            // safe and prescribes exactly this shape — `assetId` is the trade's
            // own id, and the buy-side fields are omitted so the Profit column
            // honestly shows an em dash instead of a number computed against a
            // missing buy price.
            let trade = TradeRecord {
                id: id.to_string(),
                asset_id: id.to_string(),
                item: name.to_string(),
                amount_sold: sold_scu,
                unit: Some(CargoUnit::Scu),
                buy_unit: None,
                sell_price: price_per_scu,
                buy_price: None,
                buy_location: None,
                sell_location: sell_location.to_string(),
                sold_at: sold_at.to_string(),
                logged_by: logged_by.clone(),
                source: Some(SOURCE.to_string()),
                uex_sell_id: None,
            };
            (trade, None, AssetEffect::Orphan, Vec::new(), 0.0)
        }
        Some((asset, others)) => {
            let asset_unit = asset.unit.unwrap_or(CargoUnit::Scu);
            let requested = units::convert(sold_scu, CargoUnit::Scu, asset_unit);
            let applied = units::apply_sell(asset.amount, requested);

            let trade = TradeRecord {
                id: id.to_string(),
                asset_id: asset.id.clone(),
                item: name.to_string(),
                // Clamped: §8 forbids writing more than existed.
                amount_sold: applied.sold,
                // Both sides in the asset's unit, so the web app's
                // conversion-free profit formula comes out right.
                unit: Some(asset_unit),
                buy_unit: Some(asset_unit),
                sell_price: units::convert_price(price_per_scu, CargoUnit::Scu, asset_unit),
                buy_price: Some(asset.buy_price),
                buy_location: Some(asset.location.clone()),
                sell_location: sell_location.to_string(),
                sold_at: sold_at.to_string(),
                logged_by: logged_by.clone(),
                source: Some(SOURCE.to_string()),
                uex_sell_id: None,
            };

            let effect = if applied.delete_asset {
                AssetEffect::Deleted
            } else {
                AssetEffect::Decremented {
                    remaining: applied.remaining,
                }
            };
            (
                trade,
                Some((asset.clone(), applied)),
                effect,
                others.clone(),
                applied.over_sold_by,
            )
        }
    };

    SellPlan {
        trade,
        asset_update,
        effect,
        others,
        over_sold_by,
    }
}

/// What a sell would do, for the review card to show before the user approves.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SellPreview {
    /// The asset this would come out of, or `None` for an orphan trade.
    pub matched_asset: Option<AssetRecord>,
    /// Same item, not chosen. Shown so the user can see FIFO picked one.
    pub other_candidates: Vec<AssetRecord>,
    /// What would actually be written, in `unit` — clamped.
    pub amount_sold: f64,
    pub unit: CargoUnit,
    pub sell_price: f64,
    pub buy_price: Option<f64>,
    /// What the asset would be left holding, in `unit`.
    pub remaining_after: Option<f64>,
    pub deletes_asset: bool,
    /// Non-zero means the holding cannot cover the sale and it will be clamped.
    pub over_sold_by: f64,
    /// `(sellPrice - buyPrice) * amountSold`, computed exactly as the web app
    /// does — including its lack of unit conversion, which is harmless here
    /// because this client always writes `unit == buyUnit`. `None` for an
    /// orphan, matching the em dash the web app will show.
    ///
    /// **Gross.** Auto-load fees are not in the log at all, so a real wallet
    /// profit is lower; see CLAUDE.md.
    pub profit: Option<f64>,
}

/// Compute what [`write_sell`] would do, without writing anything.
///
/// Shares `plan_sell` with the write, so the preview cannot drift from the
/// outcome. Read-only: one `find_assets` and nothing else.
pub async fn preview_sell(
    backend: &dyn SyncBackend,
    item: &QueueItem,
    refdata: &RefData,
    nickname: &str,
) -> BackendResult<SellPreview> {
    let name = item
        .item_name(refdata)
        .ok_or_else(|| {
            BackendError::Other(format!(
                "{} has no item name and {} does not resolve",
                item.id, item.detected.line.resource_guid
            ))
        })?
        .to_string();
    // A location is not required to preview — the user is probably still
    // choosing one, and the matched asset does not depend on it anyway.
    let sell_location = item.location().unwrap_or("").to_string();

    let candidates = backend.find_assets(&name, None).await?;
    let by_id: std::collections::BTreeMap<_, _> =
        candidates.iter().map(|a| (a.id.clone(), a.clone())).collect();
    let matched = choose_asset(candidates);

    let plan = plan_sell(PlanInput {
        id: &item.id,
        name: &name,
        sell_location: &sell_location,
        sold_scu: item.amount_scu(),
        price_per_scu: item.unit_price_per_scu(),
        sold_at: &item.detected.line.log_timestamp,
        nickname,
        matched: &matched,
    });

    let remaining_after = plan.asset_update.as_ref().map(|(_, a)| a.remaining);
    let deletes_asset = matches!(plan.effect, AssetEffect::Deleted);
    let profit = plan
        .trade
        .buy_price
        .map(|buy| (plan.trade.sell_price - buy) * plan.trade.amount_sold);

    Ok(SellPreview {
        matched_asset: matched.map(|(a, _)| a),
        other_candidates: plan
            .others
            .iter()
            .filter_map(|id| by_id.get(id).cloned())
            .collect(),
        amount_sold: plan.trade.amount_sold,
        unit: plan.trade.unit.unwrap_or(CargoUnit::Scu),
        sell_price: plan.trade.sell_price,
        buy_price: plan.trade.buy_price,
        remaining_after,
        deletes_asset,
        over_sold_by: plan.over_sold_by,
        profit,
    })
}
