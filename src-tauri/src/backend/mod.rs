//! The sync backend abstraction.
//!
//! SC-Asset-Manager has no API server — the sync backend *is* the integration
//! point. This client signs in as an ordinary group member and writes to the
//! same shared collections the web app listens on.
//!
//! One trait, two implementations. The web app has no such abstraction (its two
//! backends are parallel implementations), but this client has exactly one
//! write pipeline, so `writer.rs` implements INTEGRATIONS.md §8 once.

pub mod firebase;
pub mod mock;
pub mod pocketbase;
pub mod value;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::config::{BackendKind, Config};
use crate::model::{AssetRecord, TradeRecord};

/// A signed-in session. Tokens live in memory only — restarting without
/// re-prompting works because the password is in the config file.
#[derive(Debug, Clone)]
pub struct Session {
    pub token: String,
    pub refresh_token: String,
    /// When the token stops being usable.
    pub expires_at: chrono::DateTime<chrono::Utc>,
    /// The backend's own id for the signed-in user.
    pub user_id: String,
}

impl Session {
    /// Refresh if expiring within a five-minute skew.
    pub fn needs_refresh(&self) -> bool {
        self.expires_at - chrono::Utc::now() < chrono::Duration::minutes(5)
    }
}

/// The one place a backend is chosen from config.
///
/// Every caller goes through this rather than matching on `BackendKind`
/// itself, so adding a third backend cannot leave a stale arm behind that
/// silently refuses to write.
pub fn open(cfg: &Config) -> BackendResult<Box<dyn SyncBackend>> {
    match cfg.backend.kind {
        BackendKind::Firebase => Ok(Box::new(firebase::FirebaseBackend::from_config(cfg)?)),
        BackendKind::Pocketbase => Ok(Box::new(pocketbase::PocketbaseBackend::from_config(cfg)?)),
    }
}

/// What `Test connection` reports.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthReport {
    pub ok: bool,
    pub backend: BackendKind,
    /// The account that signed in.
    pub email: String,
    pub user_id: String,
    /// Assets and trades currently visible to this account. A cheap proof that
    /// reads work, not just auth.
    pub asset_count: Option<usize>,
    pub trade_count: Option<usize>,
    /// The nickname configured locally. INTEGRATIONS.md §7 does not document a
    /// remote profile location, so this cannot be compared against the server
    /// — the UI shows it for the user to check by eye.
    pub configured_nickname: String,
    /// Distinct `loggedBy` values already present in the data. If the
    /// configured nickname is not among them and the list is non-empty, that is
    /// worth a second look before writing.
    pub known_nicknames: Vec<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("not configured: {0}")]
    NotConfigured(String),

    #[error("sign-in rejected: {0}")]
    Auth(String),

    #[error("network error: {0}")]
    Network(String),

    /// The server answered, but not with what the contract expects.
    #[error("unexpected response from {what}: {detail}")]
    Protocol { what: String, detail: String },

    #[error("{0}")]
    Other(String),
}

impl BackendError {
    pub fn kind(&self) -> &'static str {
        match self {
            BackendError::NotConfigured(_) => "notConfigured",
            BackendError::Auth(_) => "auth",
            BackendError::Network(_) => "network",
            BackendError::Protocol { .. } => "protocol",
            BackendError::Other(_) => "other",
        }
    }
}

impl serde::Serialize for BackendError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("BackendError", 2)?;
        st.serialize_field("kind", self.kind())?;
        st.serialize_field("message", &self.to_string())?;
        st.end()
    }
}

pub type BackendResult<T> = Result<T, BackendError>;

#[async_trait]
pub trait SyncBackend: Send + Sync {
    fn kind(&self) -> BackendKind;

    async fn sign_in(&self) -> BackendResult<Session>;

    /// Settings "Test connection". Must not mutate anything.
    async fn health_check(&self) -> BackendResult<HealthReport>;

    /// Whole-document upsert keyed by the record's `id`. MUST be idempotent:
    /// writing twice leaves one document, with no stale fields from a prior
    /// version.
    async fn upsert_asset(&self, asset: &AssetRecord) -> BackendResult<()>;

    /// `Ok(None)` when absent — not an error.
    async fn get_asset(&self, id: &str) -> BackendResult<Option<AssetRecord>>;

    async fn delete_asset(&self, id: &str) -> BackendResult<()>;

    /// Assets matching `item`, optionally narrowed to one `location`.
    ///
    /// The `Option` is the whole point, and the two callers want opposite
    /// things:
    ///
    /// - **`Some(location)`** — the §8 *buy* merge rule, which is explicitly
    ///   defined on "the same `item` and the same `location`".
    /// - **`None`** — the *sell* lookup. An asset's `location` is where it was
    ///   bought (§4) and nothing ever moves it, so matching a sell on location
    ///   would miss on every run that buys in one place and sells in another,
    ///   i.e. on trading. §8 never states a key for the sell side; this is our
    ///   choice, and it is deliberately not the buy side's. See CLAUDE.md.
    async fn find_assets(
        &self,
        item: &str,
        location: Option<&str>,
    ) -> BackendResult<Vec<AssetRecord>>;

    /// Whole-document upsert keyed by the record's `id`, as `upsert_asset`.
    ///
    /// Separate from the asset write because §8 requires the trade to land
    /// *first*: there is no transaction, and crashing between the two must
    /// leave a trade whose asset was not decremented, never a vanished asset.
    async fn upsert_trade(&self, trade: &TradeRecord) -> BackendResult<()>;
}
