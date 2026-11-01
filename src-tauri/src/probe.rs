//! Phase 1's decisive test (PLAN.md §11), run against the real backend.
//!
//! Two things can kill this project and neither involves tailing or UI: the
//! document-id/field contract being subtly wrong — producing records the web
//! app can display but never update or delete — and Firestore `PATCH`
//! semantics, which INTEGRATIONS.md explicitly flags as unverified.
//!
//! This settles both. It writes one clearly-labelled asset and **leaves it in
//! place**, because the point is for a human to edit and delete it in the web
//! app. The `PATCH` experiment uses a separate throwaway document and cleans
//! up after itself.

use serde_json::{json, Map, Value};
use tauri::State;

use crate::backend::firebase::FirebaseBackend;
use crate::backend::{BackendError, SyncBackend};
use crate::config::BackendKind;
use crate::model::{now_iso, AssetRecord, SOURCE};
use crate::state::AppState;
use crate::units::CargoUnit;

/// One observation from the probe.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    pub question: String,
    pub answer: String,
    /// True when the observed behaviour is the one this client is built on.
    pub as_assumed: bool,
}

#[derive(Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeReport {
    /// The asset left behind for you to edit and delete in the web app.
    pub test_asset_id: Option<String>,
    pub test_asset_item: Option<String>,
    pub idempotent: Option<bool>,
    pub findings: Vec<Finding>,
    pub errors: Vec<String>,
}

fn has_quality(m: &Option<Map<String, Value>>) -> bool {
    m.as_ref().and_then(|f| f.get("quality")).is_some()
}

/// Whether the probe commands may run.
///
/// `delete_test_asset` deletes any asset by id from the user's live group
/// database, and `phase1_probe` writes to it. Both exist to settle Firestore's
/// PATCH semantics against a scratch project during development, and nothing in
/// `src/` invokes either -- but both are registered in the shipped invoke
/// handler, so a release build exposes "delete any asset" to anything that can
/// reach `invoke`. Taking the flag as an argument keeps both branches testable.
fn probe_allowed_when(is_debug_build: bool) -> Result<(), BackendError> {
    if is_debug_build {
        Ok(())
    } else {
        Err(BackendError::Other(
            "the backend probe is available only in development builds".into(),
        ))
    }
}

fn probe_allowed() -> Result<(), BackendError> {
    probe_allowed_when(cfg!(debug_assertions))
}

#[tauri::command]
pub async fn phase1_probe(state: State<'_, AppState>) -> Result<ProbeReport, BackendError> {
    probe_allowed()?;
    run_probe(&state.config()).await
}

/// The probe itself, free of Tauri so it can be driven from an integration
/// test against the real backend.
pub async fn run_probe(cfg: &crate::config::Config) -> Result<ProbeReport, BackendError> {
    if cfg.backend.kind != BackendKind::Firebase {
        return Err(BackendError::NotConfigured(
            "the probe only supports Firebase".into(),
        ));
    }
    let b = FirebaseBackend::from_config(cfg)?;
    let mut r = ProbeReport::default();

    // ---- 1. A real, well-formed asset, obviously disposable. ------------
    let id = uuid::Uuid::new_v5(
        &crate::ids::NAMESPACE,
        b"sc-asset-manager-client/phase1-probe/asset",
    )
    .to_string();
    let item = "PROBE - safe to delete".to_string();
    let asset = AssetRecord {
        id: id.clone(),
        item: item.clone(),
        amount: 1008.0,
        unit: Some(CargoUnit::Scu),
        // Deliberately unrounded: if this returns as 188.94, the encoding is
        // losing precision and every cost basis would be subtly wrong.
        buy_price: 188.9409,
        location: "PROBE".into(),
        quality: None,
        created_at: now_iso(),
        logged_by: (!cfg.nickname.trim().is_empty()).then(|| cfg.nickname.clone()),
        source: Some(SOURCE.into()),
        uex_buy_id: None,
    };

    b.upsert_asset(&asset).await?;
    r.test_asset_id = Some(id.clone());
    r.test_asset_item = Some(item);

    match b.get_asset(&id).await? {
        Some(back) => {
            let precise = back.buy_price == 188.9409;
            r.findings.push(Finding {
                question: "Does an unrounded buyPrice survive the round trip?".into(),
                answer: format!("wrote 188.9409, read back {}", back.buy_price),
                as_assumed: precise,
            });
            let id_matches = back.id == id;
            r.findings.push(Finding {
                question: "Do the document id and the id field agree?".into(),
                answer: if id_matches {
                    "yes".into()
                } else {
                    format!("NO - the field says {}", back.id)
                },
                as_assumed: id_matches,
            });
            let unit_ok = back.unit == Some(CargoUnit::Scu);
            r.findings.push(Finding {
                question: "Does the unit survive as a contract-spelled string?".into(),
                answer: format!("{:?}", back.unit),
                as_assumed: unit_ok,
            });
            let quality_absent = back.quality.is_none();
            r.findings.push(Finding {
                question: "Is quality absent rather than zero? (writing 0 would claim \
                           the commodity is worthless rather than unmeasured)"
                    .into(),
                answer: if quality_absent {
                    "absent".into()
                } else {
                    format!("present: {:?}", back.quality)
                },
                as_assumed: quality_absent,
            });
        }
        None => r
            .errors
            .push("wrote the asset but could not read it back".into()),
    }

    // An upsert must be idempotent: writing twice leaves one document.
    b.upsert_asset(&asset).await?;
    let twice = b.get_asset(&id).await?;
    r.idempotent = Some(twice.as_ref().is_some_and(|a| *a == asset));

    // ---- 2. PATCH semantics, on a throwaway document. -------------------
    // Firestore rules only permit writes to /assets and /trades, so the
    // throwaway has to live in /assets. It is deleted at the end.
    // v5 over the current instant rather than v4: the `uuid` crate carries
    // only the `v5` feature (PLAN.md §2), and uniqueness per run is all this
    // needs.
    let probe_id = format!(
        "probe-{}",
        uuid::Uuid::new_v5(
            &crate::ids::NAMESPACE,
            format!("phase1-probe/{}", now_iso()).as_bytes()
        )
        .simple()
    );
    let base = |with_quality: bool| -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("id".into(), json!(probe_id.clone()));
        m.insert("item".into(), json!("PROBE - patch semantics"));
        m.insert("amount".into(), json!(1.0));
        m.insert("buyPrice".into(), json!(1.0));
        m.insert("location".into(), json!("PROBE"));
        m.insert("createdAt".into(), json!(now_iso()));
        if with_quality {
            m.insert("quality".into(), json!(500.0));
        }
        m
    };
    let full_mask: &[&str] = &[
        "id",
        "item",
        "amount",
        "buyPrice",
        "location",
        "createdAt",
        "quality",
    ];

    let experiment = async {
        // Seed it with a `quality`, the way the web app would.
        b.patch_raw("assets", &probe_id, &base(true), Some(full_mask))
            .await?;
        if !has_quality(&b.get_raw("assets", &probe_id).await?) {
            return Err(BackendError::Other(
                "could not seed the probe document with a quality value".into(),
            ));
        }

        // Q1 — a masked write that never mentions `quality`.
        let mut only_item = Map::new();
        only_item.insert("item".into(), json!("PROBE - changed"));
        b.patch_raw("assets", &probe_id, &only_item, Some(&["item"]))
            .await?;
        let survived = has_quality(&b.get_raw("assets", &probe_id).await?);

        // Q2 — `quality` listed in the mask but absent from the body.
        b.patch_raw("assets", &probe_id, &Map::new(), Some(&["quality"]))
            .await?;
        let deleted_by_mask = !has_quality(&b.get_raw("assets", &probe_id).await?);

        // Q3 — no updateMask at all, body without `quality`.
        b.patch_raw("assets", &probe_id, &base(true), Some(&["quality"]))
            .await?;
        b.patch_raw("assets", &probe_id, &base(false), None).await?;
        let no_mask_wipes = !has_quality(&b.get_raw("assets", &probe_id).await?);

        Ok::<_, BackendError>((survived, deleted_by_mask, no_mask_wipes))
    }
    .await;

    match experiment {
        Ok((survived, deleted_by_mask, no_mask_wipes)) => {
            r.findings.push(Finding {
                question: "Does a field left OUT of updateMask survive a write? \
                           (this is what protects a quality you set in the web app \
                           when a merged buy rewrites the asset)"
                    .into(),
                answer: if survived {
                    "yes - untouched".into()
                } else {
                    "NO - it was removed".into()
                },
                as_assumed: survived,
            });
            r.findings.push(Finding {
                question: "Is a field listed in updateMask but absent from the body \
                           deleted? (this is what gives an upsert its no-stale-fields \
                           property)"
                    .into(),
                answer: if deleted_by_mask {
                    "yes - deleted".into()
                } else {
                    "NO - it was kept".into()
                },
                as_assumed: deleted_by_mask,
            });
            r.findings.push(Finding {
                question: "Is a PATCH with NO updateMask a whole-document write?".into(),
                answer: if no_mask_wipes {
                    "yes - omitted fields are dropped".into()
                } else {
                    "no - it merges, leaving omitted fields in place".into()
                },
                // Either answer is survivable, because this client always
                // sends a mask. This records which reading of the docs is
                // right, so the question stops being open.
                as_assumed: true,
            });
        }
        Err(e) => r.errors.push(format!("patch experiment: {e}")),
    }

    if let Err(e) = b.delete_raw("assets", &probe_id).await {
        r.errors.push(format!("could not clean up {probe_id}: {e}"));
    }

    Ok(r)
}

/// Remove the probe asset once you have finished poking at it in the web app.
#[tauri::command]
pub async fn delete_test_asset(state: State<'_, AppState>, id: String) -> Result<(), BackendError> {
    probe_allowed()?;
    let cfg = state.config();
    let b = FirebaseBackend::from_config(&cfg)?;
    b.delete_asset(&id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A release build must not expose the probe. `delete_test_asset` deletes
    /// any asset by id from the user's live group database and nothing in the
    /// UI calls it, so in a shipped build it is reachable capability with no
    /// purpose.
    #[test]
    fn the_probe_is_refused_in_a_release_build() {
        let err = probe_allowed_when(false).expect_err("must refuse");
        assert!(err.to_string().contains("development builds"), "{err}");
    }

    #[test]
    fn the_probe_is_allowed_in_a_debug_build() {
        assert!(probe_allowed_when(true).is_ok());
    }

    /// Tests run with `debug_assertions`, so the live gate must agree.
    #[test]
    fn the_live_gate_follows_the_build_profile() {
        assert_eq!(probe_allowed().is_ok(), cfg!(debug_assertions));
    }
}
