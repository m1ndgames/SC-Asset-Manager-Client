//! Firebase: Identity Toolkit for auth, Firestore REST for documents.
//!
//! Signs in as an ordinary group member, never a service account —
//! `firestore.rules` lets any authenticated member write `/assets` and
//! `/trades`.

use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use super::value::{decode_fields, encode_fields, id_from_name};
use super::{BackendError, BackendResult, HealthReport, Session, SyncBackend};
use crate::config::{BackendKind, Config};
use crate::model::{AssetRecord, TradeRecord};

/// Fields this client owns on an asset document.
///
/// Sent as `updateMask.fieldPaths` on every write. Two consequences, both
/// deliberate:
///   - a field listed here but absent from the body is **deleted**, so an
///     upsert leaves no stale values from a previous version;
///   - a field *not* listed is left entirely alone, which is how `quality` and
///     `uexBuyId` — both owned by the web app and never produced by the log —
///     survive a merge instead of being wiped.
///
/// INTEGRATIONS.md flags `PATCH` semantics as unverified and PLAN.md risk #1
/// says to make the question moot by always sending an explicit mask. That is
/// what this is.
const ASSET_FIELD_PATHS: &[&str] = &[
    "id",
    "item",
    "amount",
    "unit",
    "buyPrice",
    "location",
    "createdAt",
    "loggedBy",
    "source",
];

/// The same contract for `/trades`, and `uexSellId` is left out for the same
/// reason `uexBuyId` is: the web app owns it, and listing it would delete the
/// user's value on every write.
///
/// `buyPrice`, `buyUnit` and `buyLocation` *are* listed even though they are
/// omitted for an orphan trade — that is the point. Listed-but-absent deletes,
/// so an orphan cannot inherit stale buy-side values from a previous version of
/// the same document, and the web app's Profit column correctly shows `—`.
const TRADE_FIELD_PATHS: &[&str] = &[
    "id",
    "assetId",
    "item",
    "amountSold",
    "unit",
    "buyUnit",
    "sellPrice",
    "buyPrice",
    "buyLocation",
    "sellLocation",
    "soldAt",
    "loggedBy",
    "source",
];

pub struct FirebaseBackend {
    http: reqwest::Client,
    api_key: String,
    project_id: String,
    email: String,
    password: String,
    nickname: String,
    session: Mutex<Option<Session>>,
}

/// Hand-written so the password can never reach a log line, a panic message
/// or a test failure. Deriving `Debug` here would undo the guarantee the
/// config module makes.
impl std::fmt::Debug for FirebaseBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FirebaseBackend")
            .field("project_id", &self.project_id)
            .field("email", &self.email)
            .field("nickname", &self.nickname)
            .field("api_key", &"<redacted>")
            .field("password", &"<redacted>")
            .field("signed_in", &self.session.lock().unwrap().is_some())
            .finish()
    }
}

impl FirebaseBackend {
    pub fn from_config(cfg: &Config) -> BackendResult<Self> {
        let fb = &cfg.backend.firebase;
        if !fb.is_usable() {
            return Err(BackendError::NotConfigured(
                "paste the Firebase web config (apiKey and projectId) in Settings".into(),
            ));
        }
        if cfg.backend.email.trim().is_empty() || cfg.backend.password.is_empty() {
            return Err(BackendError::NotConfigured(
                "a member account email and password are required".into(),
            ));
        }
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .map_err(|e| BackendError::Network(e.to_string()))?,
            api_key: fb.api_key.trim().to_string(),
            project_id: fb.project_id.trim().to_string(),
            email: cfg.backend.email.trim().to_string(),
            password: cfg.backend.password.clone(),
            nickname: cfg.nickname.clone(),
            session: Mutex::new(None),
        })
    }


    /// Raw `PATCH` against one document, with full control over the mask.
    ///
    /// Exists for the Phase 1 `PATCH`-semantics probe. `mask: None` sends no
    /// `updateMask` at all, which is the case INTEGRATIONS.md flags as
    /// unverified.
    pub async fn patch_raw(
        &self,
        collection: &str,
        id: &str,
        fields: &Map<String, Value>,
        mask: Option<&[&str]>,
    ) -> BackendResult<()> {
        let mut url = format!("{}/{collection}/{id}", self.docs_root());
        if let Some(paths) = mask {
            let q: Vec<String> = paths
                .iter()
                .map(|f| format!("updateMask.fieldPaths={f}"))
                .collect();
            url.push('?');
            url.push_str(&q.join("&"));
        }
        let token = self.token().await?;
        let res = self
            .http
            .patch(&url)
            .bearer_auth(token)
            .json(&json!({ "fields": encode_fields(fields) }))
            .send()
            .await
            .map_err(|e| BackendError::Network(e.to_string()))?;
        if res.status().is_success() {
            return Ok(());
        }
        let body: Value = res.json().await.unwrap_or(Value::Null);
        Err(firestore_error("patch_raw", &body))
    }

    /// Raw document read, returning the decoded plain-JSON fields.
    pub async fn get_raw(
        &self,
        collection: &str,
        id: &str,
    ) -> BackendResult<Option<Map<String, Value>>> {
        let url = format!("{}/{collection}/{id}", self.docs_root());
        Ok(self.get_json(&url).await?.and_then(|doc| {
            doc.get("fields")
                .and_then(Value::as_object)
                .map(decode_fields)
        }))
    }

    /// Delete any document, not just a well-formed asset.
    pub async fn delete_raw(&self, collection: &str, id: &str) -> BackendResult<()> {
        let url = format!("{}/{collection}/{id}", self.docs_root());
        let token = self.token().await?;
        let res = self
            .http
            .delete(&url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| BackendError::Network(e.to_string()))?;
        if res.status().is_success() || res.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        let body: Value = res.json().await.unwrap_or(Value::Null);
        Err(firestore_error("delete_raw", &body))
    }

    /// The field paths this client writes, exposed for diagnostics.
    pub fn asset_field_paths() -> &'static [&'static str] {
        ASSET_FIELD_PATHS
    }

    pub fn trade_field_paths() -> &'static [&'static str] {
        TRADE_FIELD_PATHS
    }

    /// One masked `PATCH` upsert, shared by assets and trades.
    ///
    /// Shared deliberately: the document id must equal the record's `id` field
    /// for *both* collections — a mismatch yields a record the web app can
    /// display but never update or delete — and that guard is worth having in
    /// exactly one place rather than written out twice and drifting.
    async fn upsert_doc<T: serde::Serialize>(
        &self,
        collection: &str,
        id: &str,
        record: &T,
        field_paths: &[&str],
        what: &'static str,
    ) -> BackendResult<()> {
        if id.trim().is_empty() {
            return Err(BackendError::Other(format!("{what}: record has no id")));
        }

        let plain = serde_json::to_value(record).map_err(|e| BackendError::Other(e.to_string()))?;
        let Value::Object(map) = plain else {
            return Err(BackendError::Other(format!(
                "{what}: record did not serialise to an object"
            )));
        };
        // The id carried in the body must be the one addressing the document.
        if map.get("id").and_then(Value::as_str) != Some(id) {
            return Err(BackendError::Other(format!(
                "{what}: document id {id} disagrees with the record's own id"
            )));
        }

        let mask: String = field_paths
            .iter()
            .map(|f| format!("updateMask.fieldPaths={f}"))
            .collect::<Vec<_>>()
            .join("&");
        let url = format!("{}/{collection}/{id}?{mask}", self.docs_root());

        let token = self.token().await?;
        let res = self
            .http
            .patch(&url)
            .bearer_auth(token)
            .json(&json!({ "fields": encode_fields(&map) }))
            .send()
            .await
            .map_err(|e| BackendError::Network(e.to_string()))?;

        if res.status().is_success() {
            return Ok(());
        }
        let body: Value = res.json().await.unwrap_or(Value::Null);
        Err(firestore_error(what, &body))
    }

    fn docs_root(&self) -> String {
        format!(
            "https://firestore.googleapis.com/v1/projects/{}/databases/(default)/documents",
            self.project_id
        )
    }

    /// A valid token, refreshing or signing in as needed.
    async fn token(&self) -> BackendResult<String> {
        if let Some(s) = self.session.lock().unwrap().clone() {
            if !s.needs_refresh() {
                return Ok(s.token);
            }
        }
        let s = self.sign_in().await?;
        Ok(s.token)
    }

    async fn get_json(&self, url: &str) -> BackendResult<Option<Value>> {
        let token = self.token().await?;
        let res = self
            .http
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| BackendError::Network(e.to_string()))?;
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let status = res.status();
        let body: Value = res
            .json()
            .await
            .map_err(|e| BackendError::Network(e.to_string()))?;
        if !status.is_success() {
            return Err(firestore_error("GET", &body));
        }
        Ok(Some(body))
    }

    /// Every document in a collection, following `nextPageToken`.
    ///
    /// PLAN.md risk #2: an `item == X && location == Y` query needs a
    /// composite index the user's project almost certainly lacks, which fails
    /// with a 400 and a "create this index" URL. Fetch-all-and-filter needs no
    /// setup and is one request at a few hundred assets. Revisit if the
    /// collection grows.
    async fn list_collection(&self, name: &str) -> BackendResult<Vec<Value>> {
        let mut out = Vec::new();
        let mut page: Option<String> = None;
        loop {
            let mut url = format!("{}/{name}?pageSize=300", self.docs_root());
            if let Some(t) = &page {
                url.push_str(&format!("&pageToken={t}"));
            }
            let Some(body) = self.get_json(&url).await? else {
                break;
            };
            if let Some(docs) = body.get("documents").and_then(Value::as_array) {
                out.extend(docs.iter().cloned());
            }
            match body.get("nextPageToken").and_then(Value::as_str) {
                Some(t) if !t.is_empty() => page = Some(t.to_string()),
                _ => break,
            }
        }
        Ok(out)
    }
}

fn firestore_error(what: &str, body: &Value) -> BackendError {
    let msg = body
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("unknown error");
    BackendError::Protocol {
        what: what.to_string(),
        detail: msg.to_string(),
    }
}

fn doc_to_asset(doc: &Value) -> Option<AssetRecord> {
    let fields = doc.get("fields")?.as_object()?;
    let mut plain = decode_fields(fields);
    // Fall back to the document id when the `id` field is missing, so a
    // malformed record is still addressable rather than invisible.
    if !plain.contains_key("id") {
        if let Some(name) = doc.get("name").and_then(Value::as_str) {
            if let Some(id) = id_from_name(name) {
                plain.insert("id".into(), Value::String(id.to_string()));
            }
        }
    }
    serde_json::from_value(Value::Object(plain)).ok()
}

#[async_trait]
impl SyncBackend for FirebaseBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Firebase
    }

    async fn sign_in(&self) -> BackendResult<Session> {
        let url = format!(
            "https://identitytoolkit.googleapis.com/v1/accounts:signInWithPassword?key={}",
            self.api_key
        );
        let res = self
            .http
            .post(&url)
            .json(&json!({
                "email": self.email,
                "password": self.password,
                "returnSecureToken": true,
            }))
            .send()
            .await
            .map_err(|e| BackendError::Network(e.to_string()))?;

        let status = res.status();
        let body: Value = res
            .json()
            .await
            .map_err(|e| BackendError::Network(e.to_string()))?;

        if !status.is_success() {
            // Identity Toolkit returns machine-readable codes like
            // INVALID_LOGIN_CREDENTIALS. Pass them through — but never echo
            // the password, which is why only `message` is read.
            let msg = body
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("sign-in failed");
            return Err(BackendError::Auth(humanise_auth(msg)));
        }

        let token = body
            .get("idToken")
            .and_then(Value::as_str)
            .ok_or_else(|| BackendError::Protocol {
                what: "signInWithPassword".into(),
                detail: "no idToken in response".into(),
            })?
            .to_string();
        let expires_in: i64 = body
            .get("expiresIn")
            .and_then(Value::as_str)
            .and_then(|s| s.parse().ok())
            .unwrap_or(3600);

        let session = Session {
            token,
            refresh_token: body
                .get("refreshToken")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            expires_at: chrono::Utc::now() + chrono::Duration::seconds(expires_in),
            user_id: body
                .get("localId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        };
        *self.session.lock().unwrap() = Some(session.clone());
        Ok(session)
    }

    async fn health_check(&self) -> BackendResult<HealthReport> {
        let session = self.sign_in().await?;

        let mut notes = Vec::new();
        let assets = self.list_collection("assets").await;
        let trades = self.list_collection("trades").await;

        let (asset_count, known_nicknames) = match &assets {
            Ok(docs) => {
                let mut names: Vec<String> = docs
                    .iter()
                    .filter_map(doc_to_asset)
                    .filter_map(|a| a.logged_by)
                    .filter(|s| !s.is_empty())
                    .collect();
                names.sort();
                names.dedup();
                (Some(docs.len()), names)
            }
            Err(e) => {
                notes.push(format!("could not read assets: {e}"));
                (None, Vec::new())
            }
        };
        let trade_count = match &trades {
            Ok(docs) => Some(docs.len()),
            Err(e) => {
                notes.push(format!("could not read trades: {e}"));
                None
            }
        };

        if self.nickname.trim().is_empty() {
            notes.push(
                "No nickname set. Records written without one cannot be attributed to you \
                 in the web app."
                    .into(),
            );
        } else if !known_nicknames.is_empty()
            && !known_nicknames.iter().any(|n| n == &self.nickname)
        {
            // INTEGRATIONS.md §7 documents no remote profile to read, so this
            // is the closest honest check available: compare against the
            // nicknames already present in the data.
            notes.push(format!(
                "Nickname '{}' does not match any already in the database ({}). \
                 If that is a typo, every record this client writes will be unsellable \
                 for a user-role member.",
                self.nickname,
                known_nicknames.join(", ")
            ));
        }

        Ok(HealthReport {
            ok: true,
            backend: BackendKind::Firebase,
            email: self.email.clone(),
            user_id: session.user_id,
            asset_count,
            trade_count,
            configured_nickname: self.nickname.clone(),
            known_nicknames,
            notes,
        })
    }

    async fn upsert_asset(&self, asset: &AssetRecord) -> BackendResult<()> {
        self.upsert_doc("assets", &asset.id, asset, ASSET_FIELD_PATHS, "upsert_asset")
            .await
    }

    async fn upsert_trade(&self, trade: &TradeRecord) -> BackendResult<()> {
        self.upsert_doc("trades", &trade.id, trade, TRADE_FIELD_PATHS, "upsert_trade")
            .await
    }

    async fn get_asset(&self, id: &str) -> BackendResult<Option<AssetRecord>> {
        let url = format!("{}/assets/{id}", self.docs_root());
        match self.get_json(&url).await? {
            None => Ok(None),
            Some(doc) => Ok(doc_to_asset(&doc)),
        }
    }

    async fn delete_asset(&self, id: &str) -> BackendResult<()> {
        let url = format!("{}/assets/{id}", self.docs_root());
        let token = self.token().await?;
        let res = self
            .http
            .delete(&url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| BackendError::Network(e.to_string()))?;
        if res.status().is_success() || res.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        let body: Value = res.json().await.unwrap_or(Value::Null);
        Err(firestore_error("delete_asset", &body))
    }

    /// Fetch-all-and-filter, client side. PLAN.md risk #2: a server-side
    /// `item == X && location == Y` query needs a Firestore composite index the
    /// user would have to create by hand, and this client must work against an
    /// unmodified project. The collections are small enough that it does not
    /// matter — and the sell lookup filters on one field anyway.
    async fn find_assets(
        &self,
        item: &str,
        location: Option<&str>,
    ) -> BackendResult<Vec<AssetRecord>> {
        let docs = self.list_collection("assets").await?;
        Ok(docs
            .iter()
            .filter_map(doc_to_asset)
            .filter(|a| a.item == item)
            .filter(|a| location.is_none_or(|loc| a.location == loc))
            .collect())
    }
}

/// Identity Toolkit codes are not meant for humans.
fn humanise_auth(code: &str) -> String {
    match code.split(':').next().unwrap_or(code).trim() {
        "INVALID_LOGIN_CREDENTIALS" | "INVALID_PASSWORD" | "EMAIL_NOT_FOUND" => {
            "email or password is wrong".into()
        }
        "USER_DISABLED" => "that account is disabled".into(),
        "TOO_MANY_ATTEMPTS_TRY_LATER" => "too many attempts; try again shortly".into(),
        "API_KEY_INVALID" | "INVALID_API_KEY" => {
            "the apiKey in the Firebase config is not valid".into()
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FirebaseConfig;

    fn cfg() -> Config {
        let mut c = Config::default();
        c.backend.email = "a@b.c".into();
        c.backend.password = "SENTINEL-PASSWORD".into();
        c.backend.firebase = FirebaseConfig {
            api_key: "SENTINEL-APIKEY".into(),
            project_id: "proj".into(),
            ..Default::default()
        };
        c
    }

    #[test]
    fn refuses_to_build_without_a_project() {
        let mut c = cfg();
        c.backend.firebase.project_id = String::new();
        let err = FirebaseBackend::from_config(&c).unwrap_err();
        assert_eq!(err.kind(), "notConfigured");
    }

    #[test]
    fn refuses_to_build_without_credentials() {
        let mut c = cfg();
        c.backend.password = String::new();
        let err = FirebaseBackend::from_config(&c).unwrap_err();
        assert_eq!(err.kind(), "notConfigured");
    }

    #[test]
    fn document_urls_follow_the_contract() {
        let b = FirebaseBackend::from_config(&cfg()).unwrap();
        assert_eq!(
            b.docs_root(),
            "https://firestore.googleapis.com/v1/projects/proj/databases/(default)/documents"
        );
    }

    /// The mask must never list `quality` or `uexBuyId`: both are owned by the
    /// web app, and listing them would delete the user's values on every write.
    #[test]
    fn update_mask_leaves_web_app_owned_fields_alone() {
        assert!(!ASSET_FIELD_PATHS.contains(&"quality"));
        assert!(!ASSET_FIELD_PATHS.contains(&"uexBuyId"));
        assert!(ASSET_FIELD_PATHS.contains(&"amount"));
        assert!(ASSET_FIELD_PATHS.contains(&"buyPrice"));
        assert!(ASSET_FIELD_PATHS.contains(&"id"));
    }

    /// Debug output is a common accidental leak path: a panic in a test, a
    /// tracing line, an error formatted with {:?}.
    #[test]
    fn debug_output_never_contains_the_password() {
        let b = FirebaseBackend::from_config(&cfg()).unwrap();
        let s = format!("{b:?}");
        assert!(
            !s.contains("SENTINEL-PASSWORD"),
            "password leaked into Debug: {s}"
        );
        assert!(
            !s.contains("SENTINEL-APIKEY"),
            "api key leaked into Debug: {s}"
        );
        // The harmless identifiers are still there, so the output stays useful.
        assert!(s.contains("proj"));
        assert!(s.contains("a@b.c"));
    }

    #[test]
    fn auth_codes_become_readable() {
        assert_eq!(humanise_auth("INVALID_LOGIN_CREDENTIALS"), "email or password is wrong");
        assert_eq!(
            humanise_auth("API_KEY_INVALID"),
            "the apiKey in the Firebase config is not valid"
        );
        assert_eq!(humanise_auth("WEIRD_NEW_CODE"), "WEIRD_NEW_CODE");
    }

    /// A document missing its `id` field still resolves, via the path.
    #[test]
    fn doc_without_id_field_falls_back_to_the_path() {
        let doc = json!({
            "name": "projects/p/databases/(default)/documents/assets/abc-123",
            "fields": {
                "item": { "stringValue": "Titanium" },
                "amount": { "doubleValue": 10.0 },
                "buyPrice": { "doubleValue": 1.5 },
                "location": { "stringValue": "Area18" },
                "createdAt": { "stringValue": "2026-11-01T09:11:29.754Z" }
            }
        });
        let a = doc_to_asset(&doc).expect("should decode");
        assert_eq!(a.id, "abc-123");
        assert_eq!(a.item, "Titanium");
    }

    #[test]
    fn session_refresh_window_is_five_minutes() {
        let mut s = Session {
            token: "t".into(),
            refresh_token: "r".into(),
            expires_at: chrono::Utc::now() + chrono::Duration::minutes(10),
            user_id: "u".into(),
        };
        assert!(!s.needs_refresh());
        s.expires_at = chrono::Utc::now() + chrono::Duration::minutes(4);
        assert!(s.needs_refresh());
    }
}
