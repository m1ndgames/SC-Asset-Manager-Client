//! Draining approved items to the backend.
//!
//! Separate from `writer.rs` because the two have different jobs: `writer.rs`
//! owns the field contract and the §8 ordering, this owns the loop, the locks
//! and the events. Keeping them apart is what lets the contract be tested with
//! no runtime at all.
//!
//! ## The lock rule
//!
//! The queue lives behind a `std::sync::Mutex` and its guard is **never held
//! across an `.await`**. Each iteration takes the lock, claims one item,
//! releases it, does the network call, then takes it again to record the
//! result. Holding it across the await would serialise the whole UI behind a
//! network timeout, and a `std::sync` guard is not `Send` so it would not even
//! compile — which is a useful accident.
//!
//! ## Why a drain rather than a background worker
//!
//! It runs on startup and after each approval, which covers every way an item
//! can reach `Approved`. A permanently-running worker would need its own
//! wake-up channel and shutdown handling to achieve exactly the same thing.
//! Startup matters because `Approved` survives a restart: PLAN.md §6 requires
//! the writer to drain those before processing anything new, which is safe
//! precisely because every write is an idempotent upsert on a deterministic id.

use tauri::{AppHandle, Emitter, Manager};

use crate::backend::firebase::FirebaseBackend;
use crate::backend::{BackendError, SyncBackend};
use crate::config::BackendKind;
use crate::queue::ItemState;
use crate::state::AppState;

/// A write landed.
pub const EV_WRITTEN: &str = "tx://written";
/// A write failed; the item is now `Failed` and retryable.
pub const EV_WRITE_FAILED: &str = "tx://write-failed";
/// Anything changed in the queue. The Review view refetches on this.
pub const EV_QUEUE: &str = "queue://changed";

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DrainReport {
    pub written: usize,
    pub failed: usize,
    /// Ids that landed, for the event payload.
    pub written_ids: Vec<String>,
    /// Set when the drain stopped early — no backend configured, say. Not an
    /// error: approved items simply stay approved until it is.
    pub stopped: Option<String>,
}

/// Write every approved item, oldest first.
///
/// Never returns `Err` for an individual write failure: that is recorded on the
/// item and reported in the result, because one bad record must not block the
/// rest of the queue.
pub async fn drain(app: &AppHandle) -> DrainReport {
    let state = app.state::<AppState>();
    let mut report = DrainReport::default();

    // Nothing approved? Do not even construct a backend — that would make an
    // unconfigured install log a scary error on every launch.
    {
        let q = state.queue.lock().unwrap();
        if !q.items().iter().any(|i| i.state == ItemState::Approved) {
            return report;
        }
    }

    let cfg = state.config();
    let backend: Box<dyn SyncBackend> = match cfg.backend.kind {
        BackendKind::Firebase => match FirebaseBackend::from_config(&cfg) {
            Ok(b) => Box::new(b),
            Err(e) => {
                report.stopped = Some(e.to_string());
                tracing::warn!(error = %e, "approved items are waiting but the backend is not usable");
                return report;
            }
        },
        BackendKind::Pocketbase => {
            report.stopped = Some("PocketBase support lands in Phase 6".into());
            return report;
        }
    };

    let refdata = state.refdata();
    let nickname = cfg.nickname.clone();

    loop {
        // Lock, claim, unlock. `claim_for_writing` marks the item `Writing` on
        // disk before returning it, so an interruption from here on is
        // recoverable rather than invisible.
        let claimed = {
            let mut q = state.queue.lock().unwrap();
            match q.claim_for_writing() {
                Ok(c) => c,
                Err(e) => {
                    report.stopped = Some(e.to_string());
                    break;
                }
            }
        };
        let Some(item) = claimed else { break };

        // No lock held here.
        let result = crate::writer::write_buy(backend.as_ref(), &item, &refdata, &nickname).await;

        let mut q = state.queue.lock().unwrap();
        match result {
            Ok(outcome) => {
                if let Err(e) = q.mark_written(&item.id) {
                    tracing::error!(error = %e, id = %item.id, "wrote the record but could not update the queue");
                }
                report.written += 1;
                report.written_ids.push(item.id.clone());
                tracing::info!(
                    id = %item.id,
                    item = %outcome.asset.item,
                    amount = outcome.asset.amount,
                    location = %outcome.asset.location,
                    "asset written"
                );
                let _ = app.emit(EV_WRITTEN, &outcome.asset);
            }
            Err(e) => {
                let message = humanise(&e);
                if let Err(e2) = q.mark_failed(&item.id, &message) {
                    tracing::error!(error = %e2, id = %item.id, "could not record the failure");
                }
                report.failed += 1;
                tracing::warn!(id = %item.id, error = %e, "write failed");
                let _ = app.emit(
                    EV_WRITE_FAILED,
                    serde_json::json!({ "id": item.id, "message": message }),
                );
                // Keep going: a failure specific to one record (a bad location,
                // say) must not strand everything behind it. A systemic failure
                // will simply fail the rest in turn and they are all retryable.
            }
        }
    }

    if report.written > 0 || report.failed > 0 {
        let _ = app.emit(EV_QUEUE, ());
    }
    report
}

/// Backend errors reach the review card, so they have to read like something a
/// person can act on.
fn humanise(e: &BackendError) -> String {
    match e {
        BackendError::Auth(m) => {
            format!("Sign-in was rejected ({m}). Check the email and password in Settings.")
        }
        BackendError::Network(m) => {
            format!("Could not reach the backend ({m}). It will retry when you ask it to.")
        }
        BackendError::NotConfigured(m) => format!("The backend is not configured: {m}"),
        other => other.to_string(),
    }
}
