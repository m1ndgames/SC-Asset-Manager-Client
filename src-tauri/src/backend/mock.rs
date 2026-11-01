//! An in-memory `SyncBackend`, so `writer.rs` is tested without a network.
//!
//! `writer.rs` implements the INTEGRATIONS.md §8 ordering rules once, for both
//! backends, and those rules are exactly the kind of thing that is painful to
//! test against a live service: the interesting cases are a failure *between*
//! two writes, and a retry landing on a document that already exists.
//!
//! So this mock can be told to fail on command, and it records every call in
//! order. A test asserts on the sequence, not just the end state — "the trade
//! was written before the asset was mutated" is a statement about order and
//! cannot be checked any other way.
//!
//! Test-only, but compiled normally rather than behind `#[cfg(test)]`: it is
//! also what the `writer` integration test uses, and a second compilation mode
//! is a source of bugs nobody sees until CI.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::{BackendError, BackendResult, HealthReport, Session, SyncBackend};
use crate::config::BackendKind;
use crate::model::AssetRecord;

/// Every call the writer made, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    UpsertAsset(String),
    GetAsset(String),
    DeleteAsset(String),
    FindAssets { item: String, location: String },
    SignIn,
    HealthCheck,
}

/// What to fail, and how. Set before the call under test.
#[derive(Debug, Clone, Default)]
pub struct Faults {
    /// Fail `upsert_asset` for this id, once, then succeed. Models the retry
    /// case: the first attempt dies, the second must land cleanly.
    pub fail_upsert_once: Option<String>,
    /// Fail every `upsert_asset`, forever.
    pub fail_all_upserts: bool,
    pub fail_find: bool,
}

#[derive(Debug, Default)]
struct Inner {
    assets: BTreeMap<String, AssetRecord>,
    calls: Vec<Call>,
    faults: Faults,
}

#[derive(Debug, Clone, Default)]
pub struct MockBackend {
    inner: Arc<Mutex<Inner>>,
}

impl MockBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_asset(self, asset: AssetRecord) -> Self {
        self.inner
            .lock()
            .unwrap()
            .assets
            .insert(asset.id.clone(), asset);
        self
    }

    pub fn with_faults(self, faults: Faults) -> Self {
        self.inner.lock().unwrap().faults = faults;
        self
    }

    pub fn calls(&self) -> Vec<Call> {
        self.inner.lock().unwrap().calls.clone()
    }

    pub fn assets(&self) -> Vec<AssetRecord> {
        self.inner.lock().unwrap().assets.values().cloned().collect()
    }

    pub fn asset(&self, id: &str) -> Option<AssetRecord> {
        self.inner.lock().unwrap().assets.get(id).cloned()
    }

    pub fn asset_count(&self) -> usize {
        self.inner.lock().unwrap().assets.len()
    }
}

#[async_trait]
impl SyncBackend for MockBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Firebase
    }

    async fn sign_in(&self) -> BackendResult<Session> {
        self.inner.lock().unwrap().calls.push(Call::SignIn);
        Ok(Session {
            token: "mock-token".into(),
            refresh_token: "mock-refresh".into(),
            expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            user_id: "mock-user".into(),
        })
    }

    async fn health_check(&self) -> BackendResult<HealthReport> {
        let mut g = self.inner.lock().unwrap();
        g.calls.push(Call::HealthCheck);
        Ok(HealthReport {
            ok: true,
            backend: BackendKind::Firebase,
            email: "mock@example.com".into(),
            user_id: "mock-user".into(),
            asset_count: Some(g.assets.len()),
            trade_count: Some(0),
            configured_nickname: String::new(),
            known_nicknames: Vec::new(),
            notes: Vec::new(),
        })
    }

    async fn upsert_asset(&self, asset: &AssetRecord) -> BackendResult<()> {
        let mut g = self.inner.lock().unwrap();
        g.calls.push(Call::UpsertAsset(asset.id.clone()));

        if g.faults.fail_all_upserts {
            return Err(BackendError::Network("mock: upsert always fails".into()));
        }
        if g.faults.fail_upsert_once.as_deref() == Some(asset.id.as_str()) {
            g.faults.fail_upsert_once = None;
            return Err(BackendError::Network("mock: upsert failed once".into()));
        }

        // The contract's idempotency rule: writing twice leaves one document
        // with no stale fields, so this replaces rather than merges.
        g.assets.insert(asset.id.clone(), asset.clone());
        Ok(())
    }

    async fn get_asset(&self, id: &str) -> BackendResult<Option<AssetRecord>> {
        let mut g = self.inner.lock().unwrap();
        g.calls.push(Call::GetAsset(id.to_string()));
        Ok(g.assets.get(id).cloned())
    }

    async fn delete_asset(&self, id: &str) -> BackendResult<()> {
        let mut g = self.inner.lock().unwrap();
        g.calls.push(Call::DeleteAsset(id.to_string()));
        g.assets.remove(id);
        Ok(())
    }

    async fn find_assets(&self, item: &str, location: &str) -> BackendResult<Vec<AssetRecord>> {
        let mut g = self.inner.lock().unwrap();
        g.calls.push(Call::FindAssets {
            item: item.to_string(),
            location: location.to_string(),
        });
        if g.faults.fail_find {
            return Err(BackendError::Network("mock: find failed".into()));
        }
        Ok(g.assets
            .values()
            .filter(|a| a.item == item && a.location == location)
            .cloned()
            .collect())
    }
}
