//! The review queue — the single source of truth, and it lives in Rust.
//!
//! ```text
//! Detected ─(grace window)─> PendingReview ──approve──> Approved ──> Writing ──> Written
//!     │                           │                                      │
//!     └──> Discarded              └──reject──> Rejected                  └──> Failed ──retry──┘
//! ```
//!
//! `Detected` is deliberately absent from [`ItemState`]: it exists only in
//! `correlate.rs`'s memory and is never persisted, because the watcher's offset
//! has not advanced past its line yet. A crash there re-reads the line; a crash
//! after enqueueing is covered by `queue.json`.
//!
//! Every mutator persists before returning, per PLAN.md §6 — after every
//! transition and every user field edit. The watcher's offset advances once an
//! item is enqueued, so anything not on disk at that moment is gone.
//!
//! Svelte calls a command and re-renders from the returned projection, never
//! mutating locally. Hence no reconciliation logic and no "unsaved edits"
//! concept; preserve that.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config;
use crate::error::{AppError, AppResult};
use crate::log::scan::DetectedTrade;
use crate::model::now_iso;
use crate::refdata::RefData;
use crate::units::{self, CargoUnit};

pub const SCHEMA: u32 = 1;

/// Only the states that are persisted. See the module docs on `Detected`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ItemState {
    /// Waiting for a human. The log does not record the location, which is the
    /// entire reason this state exists.
    PendingReview,
    /// The user approved it; the writer has not picked it up yet.
    Approved,
    /// Handed to the writer. An item found here on load was interrupted
    /// mid-write and is retried — safe because every write is an idempotent
    /// upsert on a deterministic id.
    Writing,
    Written,
    Rejected,
    /// Retracted by a failure line before the user ever saw it.
    Discarded,
    /// The write failed. Retryable, and it keeps the error for display.
    Failed,
}

impl ItemState {
    /// Terminal states leave the queue for `history.json`.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            ItemState::Written | ItemState::Rejected | ItemState::Discarded
        )
    }
}

/// What the user may change. Every field is an override: `None` means "use what
/// the log said", so the log's own values are never overwritten in place and an
/// edit can always be reasoned about against them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemEdits {
    /// Set when the GUID did not resolve, or the user disagreed with the map.
    pub item_name: Option<String>,
    /// **Required to approve.** The log never records it.
    pub location: Option<String>,
    /// Amount in SCU, whatever unit the UI is displaying.
    pub amount_scu: Option<f64>,
    /// aUEC per SCU. Never rounded.
    pub unit_price_per_scu: Option<f64>,
    /// Display unit only — it rescales what the user sees, and amount and price
    /// are always stored per-SCU so the two cannot end up on different
    /// denominators. That is the easiest contract violation available and the
    /// data model should make it unrepresentable.
    #[serde(default)]
    pub display_unit: Option<CargoUnit>,
    /// Write this buy as its own asset row instead of merging it into an
    /// existing one. INTEGRATIONS.md section 8 offers both; merging is the
    /// default because it is what the web app itself does, and this is the
    /// escape hatch for keeping lots apart.
    #[serde(default)]
    pub separate_asset: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueItem {
    /// The deterministic trade id. Also the document id of the record written.
    pub id: String,
    pub state: ItemState,
    /// The immutable facts from the log. Never edited — edits live beside it.
    pub detected: DetectedTrade,
    pub edits: ItemEdits,
    /// Our clock, not the log's: when this entered the queue.
    pub created_at: String,
    pub updated_at: String,
    /// Last failure, for display and for History.
    pub error: Option<String>,
    pub attempts: u32,
    /// Phase 5. Carried now so a queue.json written today stays readable when
    /// the merge rule lands, rather than needing a schema bump then.
    #[serde(default)]
    pub merge_target_id: Option<String>,
    /// The one non-idempotent operation in the design: written *before* the
    /// backend call, so a resumed item that already applied its merge goes
    /// straight to `Written` instead of double-counting.
    #[serde(default)]
    pub merge_applied: bool,
}

impl QueueItem {
    fn new(detected: DetectedTrade) -> Self {
        let now = now_iso();
        Self {
            id: detected.line.id.clone(),
            state: ItemState::PendingReview,
            detected,
            edits: ItemEdits::default(),
            created_at: now.clone(),
            updated_at: now,
            error: None,
            attempts: 0,
            merge_target_id: None,
            merge_applied: false,
        }
    }

    /// The location, or `None` if the user has not chosen one.
    pub fn location(&self) -> Option<&str> {
        self.edits
            .location
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    /// The item name: the user's override, else whatever the GUID resolved to.
    pub fn item_name<'a>(&'a self, refdata: &'a RefData) -> Option<&'a str> {
        self.edits
            .item_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or_else(|| refdata.name_of(&self.detected.line.resource_guid))
    }

    /// Amount in SCU, as it will be written.
    pub fn amount_scu(&self) -> f64 {
        self.edits
            .amount_scu
            .unwrap_or(self.detected.line.quantity_scu)
    }

    /// aUEC per SCU, as it will be written. Never rounded.
    pub fn unit_price_per_scu(&self) -> f64 {
        self.edits
            .unit_price_per_scu
            .unwrap_or(self.detected.line.unit_price_per_scu)
    }

    /// Whether the user asked for this buy to stay its own row.
    pub fn merge_opt_out(&self) -> bool {
        self.edits.separate_asset.unwrap_or(false)
    }

    pub fn display_unit(&self) -> CargoUnit {
        // SCU is the contract's preferred unit, so it is also the default view.
        self.edits.display_unit.unwrap_or(CargoUnit::Scu)
    }

    /// Why this cannot be approved yet, if it cannot.
    ///
    /// Returned as a list rather than a bool so the card can point at the
    /// offending field instead of just refusing.
    pub fn blockers(&self, refdata: &RefData) -> Vec<Blocker> {
        let mut out = Vec::new();
        if self.location().is_none() {
            out.push(Blocker::LocationMissing);
        }
        if self.item_name(refdata).is_none() {
            out.push(Blocker::ItemUnresolved {
                guid: self.detected.line.resource_guid.clone(),
            });
        }
        let amount = self.amount_scu();
        if !amount.is_finite() || amount <= 0.0 {
            out.push(Blocker::AmountInvalid);
        }
        let price = self.unit_price_per_scu();
        if !price.is_finite() || price < 0.0 {
            out.push(Blocker::PriceInvalid);
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Blocker {
    /// The whole reason a human is in the loop.
    LocationMissing,
    ItemUnresolved { guid: String },
    AmountInvalid,
    PriceInvalid,
}

/// Absent in JSON, `null`, and a value must be three different requests:
/// "leave it alone", "clear it", "set it". `Option<Option<T>>` with
/// `#[serde(default)]` gives exactly that — absent is `None`, `null` is
/// `Some(None)`, a value is `Some(Some(v))`.
///
/// The one-line deserializer is the standard trick, and cheaper than taking a
/// dependency for it.
fn double_option<'de, T, D>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Deserialize::deserialize(d).map(Some)
}

/// A patch from the UI. Absent fields are left alone; `null` clears.
///
/// The distinction matters: "the user cleared the location" and "the user did
/// not touch the location" must not arrive as the same request.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemPatch {
    #[serde(default, deserialize_with = "double_option")]
    pub item_name: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub location: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub amount_scu: Option<Option<f64>>,
    #[serde(default, deserialize_with = "double_option")]
    pub unit_price_per_scu: Option<Option<f64>>,
    #[serde(default)]
    pub display_unit: Option<CargoUnit>,
    #[serde(default)]
    pub separate_asset: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueData {
    pub schema: u32,
    pub items: Vec<QueueItem>,
}

impl Default for QueueData {
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            items: Vec::new(),
        }
    }
}

/// The queue, and the directory it persists to.
pub struct Queue {
    dir: PathBuf,
    data: QueueData,
    history: Vec<QueueItem>,
}

impl Queue {
    /// Load from `dir`, recovering anything interrupted mid-write.
    pub fn open(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let mut data: QueueData = read_json(&dir.join("queue.json")).unwrap_or_default();
        if data.schema != SCHEMA {
            tracing::warn!(
                found = data.schema,
                expected = SCHEMA,
                "queue.json schema differs; keeping the items as-is"
            );
            data.schema = SCHEMA;
        }

        // An item left in `Writing` was interrupted. Put it back in the write
        // queue rather than leaving it stuck: the write is an idempotent upsert
        // on a deterministic id, so repeating it is safe. The one exception is
        // a merge that already applied — Phase 5's guard — which is finished.
        let mut recovered = 0;
        for item in &mut data.items {
            if item.state == ItemState::Writing {
                if item.merge_applied {
                    item.state = ItemState::Written;
                } else {
                    item.state = ItemState::Approved;
                    recovered += 1;
                }
                item.updated_at = now_iso();
            }
        }
        if recovered > 0 {
            tracing::info!(recovered, "re-queued writes interrupted by a restart");
        }

        let history: Vec<QueueItem> = read_json(&dir.join("history.json")).unwrap_or_default();

        Self { dir, data, history }
    }

    pub fn open_default() -> AppResult<Self> {
        Ok(Self::open(config::config_dir()?))
    }

    pub fn items(&self) -> &[QueueItem] {
        &self.data.items
    }

    pub fn history(&self) -> &[QueueItem] {
        &self.history
    }

    pub fn get(&self, id: &str) -> Option<&QueueItem> {
        self.data.items.iter().find(|i| i.id == id)
    }

    /// Items awaiting review, oldest log timestamp first — the order they
    /// happened in, which is the order a human wants to confirm them.
    pub fn pending(&self) -> Vec<&QueueItem> {
        let mut v: Vec<&QueueItem> = self
            .data
            .items
            .iter()
            .filter(|i| i.state == ItemState::PendingReview)
            .collect();
        v.sort_by(|a, b| a.detected.line.log_timestamp.cmp(&b.detected.line.log_timestamp));
        v
    }

    /// Add a detection, or return the existing item untouched.
    ///
    /// Idempotent on the deterministic id. This is what makes the watcher's
    /// "persist the queue, then advance the offset" ordering safe: a crash
    /// in between re-reads the line, and this collapses the duplicate without
    /// discarding the edits the user may already have made.
    pub fn enqueue(&mut self, detected: DetectedTrade) -> AppResult<&QueueItem> {
        let id = detected.line.id.clone();
        if let Some(pos) = self.data.items.iter().position(|i| i.id == id) {
            return Ok(&self.data.items[pos]);
        }
        if self.history.iter().any(|i| i.id == id) {
            // Already dealt with. Returning an error would be wrong — nothing
            // failed — so report it as a no-op the caller can ignore.
            return Err(AppError::Queue(format!(
                "{id} was already written or rejected"
            )));
        }
        self.data.items.push(QueueItem::new(detected));
        self.persist()?;
        Ok(self.data.items.last().expect("just pushed"))
    }

    /// Apply a field edit. Persists, per PLAN.md §6.
    pub fn update(&mut self, id: &str, patch: ItemPatch) -> AppResult<&QueueItem> {
        let item = self.item_mut(id)?;
        if !matches!(item.state, ItemState::PendingReview | ItemState::Failed) {
            return Err(AppError::Queue(format!(
                "{id} is {:?} and can no longer be edited",
                item.state
            )));
        }

        if let Some(v) = patch.item_name {
            item.edits.item_name = v.filter(|s| !s.trim().is_empty());
        }
        if let Some(v) = patch.location {
            item.edits.location = v.filter(|s| !s.trim().is_empty());
        }
        if let Some(v) = patch.amount_scu {
            item.edits.amount_scu = v;
        }
        if let Some(v) = patch.unit_price_per_scu {
            item.edits.unit_price_per_scu = v;
        }
        if let Some(u) = patch.display_unit {
            item.edits.display_unit = Some(u);
        }
        if let Some(v) = patch.separate_asset {
            item.edits.separate_asset = Some(v);
        }
        item.updated_at = now_iso();
        self.persist()?;
        self.get(id).ok_or_else(|| missing(id))
    }

    /// Approve for writing. Refuses while anything is missing.
    pub fn approve(&mut self, id: &str, refdata: &RefData) -> AppResult<&QueueItem> {
        {
            let item = self.item_ref(id)?;
            if !matches!(item.state, ItemState::PendingReview | ItemState::Failed) {
                return Err(AppError::Queue(format!(
                    "{id} is {:?} and cannot be approved",
                    item.state
                )));
            }
            let blockers = item.blockers(refdata);
            if !blockers.is_empty() {
                return Err(AppError::NotReady { blockers });
            }
        }
        let item = self.item_mut(id)?;
        item.state = ItemState::Approved;
        item.error = None;
        item.updated_at = now_iso();
        self.persist()?;
        self.get(id).ok_or_else(|| missing(id))
    }

    pub fn reject(&mut self, id: &str) -> AppResult<()> {
        let item = self.item_mut(id)?;
        if item.state == ItemState::Writing {
            return Err(AppError::Queue(format!(
                "{id} is being written and cannot be rejected"
            )));
        }
        item.state = ItemState::Rejected;
        item.updated_at = now_iso();
        self.retire(id)
    }

    /// Take the oldest approved item for writing, marking it `Writing` on disk
    /// *before* the caller touches the network.
    pub fn claim_for_writing(&mut self) -> AppResult<Option<QueueItem>> {
        let Some(pos) = self
            .data
            .items
            .iter()
            .position(|i| i.state == ItemState::Approved)
        else {
            return Ok(None);
        };
        let item = &mut self.data.items[pos];
        item.state = ItemState::Writing;
        item.attempts += 1;
        item.updated_at = now_iso();
        let claimed = item.clone();
        self.persist()?;
        Ok(Some(claimed))
    }

    /// Journal that a merge is about to be attempted, and fsync before
    /// returning.
    ///
    /// Must be called *before* the backend is touched: the dangerous window is
    /// "the merge landed but we never recorded it", where a resumed item adds
    /// the same quantity again into a shared inventory. Recording first makes
    /// the opposite failure the possible one — believing a merge applied when
    /// it did not, which costs a missing buy that is visible and correctable by
    /// hand. `Queue::open` promotes a `Writing` item with `merge_applied`
    /// straight to `Written` rather than retrying it.
    pub fn mark_merging(&mut self, id: &str, target_id: &str) -> AppResult<()> {
        let item = self.item_mut(id)?;
        item.merge_target_id = Some(target_id.to_string());
        item.merge_applied = true;
        item.updated_at = now_iso();
        self.persist()
    }

    pub fn mark_written(&mut self, id: &str) -> AppResult<()> {
        let item = self.item_mut(id)?;
        item.state = ItemState::Written;
        item.error = None;
        item.updated_at = now_iso();
        self.retire(id)
    }

    pub fn mark_failed(&mut self, id: &str, error: impl Into<String>) -> AppResult<&QueueItem> {
        let item = self.item_mut(id)?;
        item.state = ItemState::Failed;
        item.error = Some(error.into());
        item.updated_at = now_iso();
        self.persist()?;
        self.get(id).ok_or_else(|| missing(id))
    }

    /// Return a failed write to `Approved` so the drain picks it up again.
    ///
    /// Refuses a journalled merge for the same reason [`Self::restore`] does:
    /// the write may have landed before the error was reported, and repeating
    /// it adds the quantity to the target asset a second time. `Queue::open`
    /// already promotes such an item rather than retrying it, so letting the
    /// Retry button do what the crash path refuses to would defeat that.
    pub fn retry(&mut self, id: &str) -> AppResult<&QueueItem> {
        let item = self.item_mut(id)?;
        if item.state != ItemState::Failed {
            return Err(AppError::Queue(format!(
                "{id} is {:?}, not failed",
                item.state
            )));
        }
        if item.merge_applied {
            let target = item.merge_target_id.clone().unwrap_or_default();
            return Err(AppError::Queue(format!(
                "{id} was already merged into {target}, so retrying it could add the                  same quantity twice. Check that asset in SC-Asset-Manager and reject                  this item once you have."
            )));
        }
        item.state = ItemState::Approved;
        item.error = None;
        item.updated_at = now_iso();
        self.persist()?;
        self.get(id).ok_or_else(|| missing(id))
    }

    /// Bring a retired item back into review.
    ///
    /// The only path that moves an item out of history, so the only place a
    /// write could be made to happen twice. Two refusals:
    ///
    /// - A **written** item stays written. Re-writing a sell decrements the
    ///   asset again and re-writing a merged buy adds the quantity again. A
    ///   plain buy would be a harmless upsert, but a History row does not say
    ///   which of the three it is, so all three are refused.
    /// - **`merge_applied`** is refused in any state: the non-idempotent write
    ///   may already have landed.
    ///
    /// What remains is the recoverable cases — a rejection the user regrets,
    /// and a discard the grace window got wrong.
    pub fn restore(&mut self, id: &str) -> AppResult<&QueueItem> {
        let Some(pos) = self.history.iter().position(|i| i.id == id) else {
            // Already back in the queue is not a failure worth an error page.
            if self.data.items.iter().any(|i| i.id == id) {
                return self.get(id).ok_or_else(|| missing(id));
            }
            return Err(missing(id));
        };

        let item = &self.history[pos];
        if item.state == ItemState::Written {
            return Err(AppError::Queue(format!(
                "{id} was already written to the database. Restoring it would write it                  a second time, which for a sell means decrementing the asset twice.                  Fix it in SC-Asset-Manager instead."
            )));
        }
        if item.merge_applied {
            return Err(AppError::Queue(format!(
                "{id} was merged into an existing asset, which is the one operation                  this client cannot repeat safely. Fix it in SC-Asset-Manager instead."
            )));
        }

        let mut item = self.history[pos].clone();
        item.state = ItemState::PendingReview;
        // The reason it was discarded or rejected is no longer true of it, but
        // the user's field edits are, so those are kept.
        item.error = None;
        item.updated_at = now_iso();

        // The reverse of `retire`'s ordering, for the same reason: add before
        // removing, so a crash in between leaves the item in both files and
        // merely shows it twice, rather than in neither and losing it.
        self.data.items.push(item);
        self.persist()?;
        self.history.remove(pos);
        self.persist_history()?;

        self.get(id).ok_or_else(|| missing(id))
    }

    /// Record a detection that a failure line retracted, straight into history
    /// so Diagnostics and History can show it was seen and dropped.
    pub fn record_discarded(&mut self, detected: DetectedTrade, reason: String) -> AppResult<()> {
        let mut item = QueueItem::new(detected);
        item.state = ItemState::Discarded;
        item.error = Some(reason);
        self.history.push(item);
        self.persist_history()
    }

    // -- internals ---------------------------------------------------------

    /// Move a terminal item out of the queue and into history, persisting both.
    ///
    /// History first: an item present in neither file is lost, while an item
    /// present in both is merely shown twice until the next queue write lands.
    fn retire(&mut self, id: &str) -> AppResult<()> {
        let Some(pos) = self.data.items.iter().position(|i| i.id == id) else {
            return Err(missing(id));
        };
        if !self.data.items[pos].state.is_terminal() {
            return self.persist();
        }
        let item = self.data.items[pos].clone();
        self.history.push(item);
        self.persist_history()?;
        self.data.items.remove(pos);
        self.persist()
    }

    fn item_ref(&self, id: &str) -> AppResult<&QueueItem> {
        self.get(id).ok_or_else(|| missing(id))
    }

    fn item_mut(&mut self, id: &str) -> AppResult<&mut QueueItem> {
        self.data
            .items
            .iter_mut()
            .find(|i| i.id == id)
            .ok_or_else(|| missing(id))
    }

    fn persist(&self) -> AppResult<()> {
        write_json(&self.dir, "queue.json", &self.data)
    }

    fn persist_history(&self) -> AppResult<()> {
        write_json(&self.dir, "history.json", &self.history)
    }
}

fn missing(id: &str) -> AppError {
    AppError::Queue(format!("no queue item {id}"))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str(&text) {
        Ok(v) => Some(v),
        Err(e) => {
            // Refusing to start would strand the user's pending reviews with no
            // way to see them. Loudly starting empty is the lesser evil, and
            // the file is left on disk to be recovered by hand.
            tracing::error!(error = %e, path = %path.display(), "unreadable; starting empty");
            None
        }
    }
}

fn write_json<T: Serialize>(dir: &Path, name: &str, value: &T) -> AppResult<()> {
    std::fs::create_dir_all(dir)
        .map_err(|e| AppError::Queue(format!("cannot create {}: {e}", dir.display())))?;
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| AppError::Queue(format!("cannot serialise {name}: {e}")))?;
    config::write_atomic(&dir.join(format!("{name}.tmp")), &dir.join(name), &bytes)
        .map_err(|e| AppError::Queue(e.to_string()))
}

/// The projection the frontend renders. Computed here so the UI never has to
/// re-derive a figure the backend will write.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemView {
    #[serde(flatten)]
    pub item: QueueItem,
    /// Resolved name, or `None` when the GUID is unknown and the user must say.
    pub resolved_item_name: Option<String>,
    /// What will actually be written, after edits.
    pub effective_amount_scu: f64,
    pub effective_unit_price_per_scu: f64,
    /// Amount and price in the chosen display unit, rescaled *together*.
    pub display_amount: f64,
    pub display_unit_price: f64,
    pub display_unit: CargoUnit,
    pub total_auec: f64,
    pub blockers: Vec<Blocker>,
    pub can_approve: bool,
}

pub fn view(item: &QueueItem, refdata: &RefData) -> ItemView {
    let unit = item.display_unit();
    let amount = item.amount_scu();
    let price = item.unit_price_per_scu();
    let blockers = item.blockers(refdata);
    ItemView {
        resolved_item_name: refdata
            .name_of(&item.detected.line.resource_guid)
            .map(str::to_string),
        effective_amount_scu: amount,
        effective_unit_price_per_scu: price,
        display_amount: units::convert(amount, CargoUnit::Scu, unit),
        display_unit_price: units::convert_price(price, CargoUnit::Scu, unit),
        display_unit: unit,
        // From the log, not amount x price: `price` is rounded to whole aUEC
        // and the unit price is not derived from it.
        total_auec: item.detected.line.total_auec,
        can_approve: blockers.is_empty(),
        blockers,
        item: item.clone(),
    }
}

#[cfg(test)]
#[path = "queue_tests.rs"]
mod tests;
