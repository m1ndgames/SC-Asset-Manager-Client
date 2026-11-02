//! The PocketBase backend.
//!
//! Schema from POCKETBASE.md, field contract from INTEGRATIONS.md. Three things
//! differ from Firestore in ways that are easy to get wrong by analogy:
//!
//! 1. **The id is split in two.** PocketBase cannot hold a hyphenated UUID as a
//!    primary key, so a record carries `id = to_pb_id(uuid)` *and*
//!    `app_id = uuid`. Readers prefer `app_id`; `to_pb_id` is not reversible
//!    because the `r` prefix is conditional, which is why both are written.
//! 2. **`app_id` is the only snake_case name in the schema**, everything else
//!    camelCase as Firestore. PocketBase answers a write containing an unknown
//!    field with 200 and discards the field, so a casing slip does not fail —
//!    it stops recording data. Hence the field-by-field mapping tests.
//! 3. **The token has no `Bearer` prefix** (INTEGRATIONS.md §1).
//!
//! There is no upsert verb, so a write is `PATCH` falling back to `POST` on a
//! 404, keeping it idempotent on a deterministic id.

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use std::sync::Mutex;

use super::{BackendError, BackendResult, HealthReport, Session, SyncBackend};
use crate::config::{BackendKind, Config};
use crate::ids::to_pb_id;
use crate::model::{AssetRecord, TradeRecord};

pub const ASSETS: &str = "assets";
pub const TRADES: &str = "trades";
/// POCKETBASE.md §5: members are ordinary records in the `users` collection.
const AUTH_COLLECTION: &str = "users";

pub struct PocketbaseBackend {
    http: reqwest::Client,
    base: String,
    email: String,
    password: String,
    nickname: String,
    session: Mutex<Option<Session>>,
}

/// Hand-written for the same reason as the Firebase one: a derived `Debug`
/// would put the password into any panic or log line that formats the struct.
impl std::fmt::Debug for PocketbaseBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PocketbaseBackend")
            .field("base", &self.base)
            .field("email", &self.email)
            .field("password", &"<redacted>")
            .finish()
    }
}

impl PocketbaseBackend {
    pub fn from_config(cfg: &Config) -> BackendResult<Self> {
        let base = cfg.backend.pocketbase.url.trim().trim_end_matches('/');
        if base.is_empty() {
            return Err(BackendError::NotConfigured(
                "the PocketBase URL is empty".into(),
            ));
        }
        if cfg.backend.email.trim().is_empty() || cfg.backend.password.is_empty() {
            return Err(BackendError::NotConfigured(
                "a PocketBase email and password are required".into(),
            ));
        }
        Ok(Self {
            http: reqwest::Client::new(),
            base: base.to_string(),
            email: cfg.backend.email.trim().to_string(),
            password: cfg.backend.password.clone(),
            nickname: cfg.nickname.clone(),
            session: Mutex::new(None),
        })
    }

    fn records_url(&self, collection: &str) -> String {
        format!("{}/api/collections/{collection}/records", self.base)
    }

    /// A valid token, signing in again when it is close to expiring.
    ///
    /// PocketBase has an `auth-refresh` endpoint, but re-authenticating costs
    /// one request either way and avoids a code path only exercised after a
    /// long idle.
    async fn token(&self) -> BackendResult<String> {
        if let Some(s) = self.session.lock().unwrap().as_ref() {
            if !s.needs_refresh() {
                return Ok(s.token.clone());
            }
        }
        let s = self.sign_in().await?;
        let token = s.token.clone();
        *self.session.lock().unwrap() = Some(s);
        Ok(token)
    }

    async fn authed(
        &self,
        method: reqwest::Method,
        url: &str,
    ) -> BackendResult<reqwest::RequestBuilder> {
        let token = self.token().await?;
        // No `Bearer`. INTEGRATIONS.md §1: "Send the token as
        // `Authorization: <token>`."
        Ok(self.http.request(method, url).header("Authorization", token))
    }

    /// `PATCH`, falling back to `POST` when the record does not exist yet.
    ///
    /// Trying the update first means the common case — a retry of a write that
    /// already landed — costs one request.
    async fn upsert(&self, collection: &str, fields: Map<String, Value>) -> BackendResult<()> {
        let pb_id = fields
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| BackendError::Other("record has no PocketBase id".into()))?
            .to_string();

        let url = format!("{}/{}", self.records_url(collection), pb_id);
        let res = self
            .authed(reqwest::Method::PATCH, &url)
            .await?
            .json(&Value::Object(fields.clone()))
            .send()
            .await
            .map_err(|e| BackendError::Network(e.to_string()))?;

        if res.status().is_success() {
            return Ok(());
        }
        if res.status() != reqwest::StatusCode::NOT_FOUND {
            return Err(pb_error(collection, res).await);
        }

        // Not there yet: create it, id included.
        let res = self
            .authed(reqwest::Method::POST, &self.records_url(collection))
            .await?
            .json(&Value::Object(fields))
            .send()
            .await
            .map_err(|e| BackendError::Network(e.to_string()))?;
        if res.status().is_success() {
            return Ok(());
        }
        Err(pb_error(collection, res).await)
    }

    /// Every record in a collection, following pagination.
    async fn list(&self, collection: &str, filter: Option<&str>) -> BackendResult<Vec<Value>> {
        let mut out = Vec::new();
        let mut page = 1;
        loop {
            let mut url = format!("{}?perPage=200&page={page}", self.records_url(collection));
            if let Some(f) = filter {
                url.push_str(&format!("&filter={}", urlencode(f)));
            }
            let res = self
                .authed(reqwest::Method::GET, &url)
                .await?
                .send()
                .await
                .map_err(|e| BackendError::Network(e.to_string()))?;
            if !res.status().is_success() {
                return Err(pb_error(collection, res).await);
            }
            let body: Value = res
                .json()
                .await
                .map_err(|e| BackendError::Protocol {
                    what: collection.to_string(),
                    detail: e.to_string(),
                })?;
            let items = body
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            out.extend(items);

            let total_pages = body.get("totalPages").and_then(Value::as_i64).unwrap_or(1);
            if page >= total_pages {
                break;
            }
            page += 1;
        }
        Ok(out)
    }
}

/// Percent-encode a PocketBase filter expression for a query string.
///
/// Escapes anything not unreserved: a half-escaped filter silently matches the
/// wrong rows rather than erroring.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Escape a value for embedding in a PocketBase filter string literal.
fn filter_literal(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// Columns POCKETBASE.md says were added after the first release, so they may
/// be missing on an early instance. Losing `unit` is not cosmetic: an asset of
/// unknown unit cannot be merged or sold against correctly.
const LATE_ASSET_COLUMNS: &[&str] = &["unit", "quality", "source"];
const LATE_TRADE_COLUMNS: &[&str] = &["unit", "buyUnit", "source"];

/// Detect a missing column from a returned record.
///
/// PocketBase returns every column including unset ones (`""` for Text, `0` for
/// Number), so a key absent from a record means the collection has no such
/// column — the only cheap way to see this without an admin token. Needs one
/// existing record; reports nothing for an empty collection rather than
/// guessing.
fn missing_columns(rows: &[Value], expected: &[&str]) -> Vec<String> {
    let Some(obj) = rows.first().and_then(Value::as_object) else {
        return Vec::new();
    };
    expected
        .iter()
        .filter(|c| !obj.contains_key(**c))
        .map(|c| (*c).to_string())
        .collect()
}

async fn pb_error(what: &str, res: reqwest::Response) -> BackendError {
    let status = res.status();
    let body: Value = res.json().await.unwrap_or(Value::Null);
    let message = body
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("no message")
        .to_string();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return BackendError::Auth(format!("{what}: {message}"));
    }
    BackendError::Protocol {
        what: what.to_string(),
        detail: format!("HTTP {status}: {message}"),
    }
}

/// Turn a record into the PocketBase body: the app's `id` becomes `app_id`, and
/// `id` becomes the mangled primary key. §6 requires both.
/// Buy-side columns on a trade that this client owns and may need to *clear*.
///
/// Firestore handles this with `updateMask`: the paths are listed, so a field
/// absent from the body is deleted and an orphan trade ends up with no
/// buy-side values. PocketBase has no mask and a `PATCH` cannot delete a
/// column, so `skip_serializing_if` would simply leave the previous values in
/// place -- a trade re-written as an orphan would keep a `buyPrice` from an
/// asset that no longer applies, and the web app's Profit column would show a
/// number computed against it instead of nothing.
///
/// Sent as the column's empty value rather than `null`, which is what a record
/// written without them gets anyway, so a re-write matches a fresh write.
/// Unverified against a live instance.
///
/// `quality`, `uexBuyId` and `uexSellId` are deliberately absent: the web app
/// owns them, and omitting them from a `PATCH` is what leaves them untouched.
fn trade_clearable() -> Vec<(&'static str, Value)> {
    vec![
        ("buyPrice", json!(0)),
        ("buyUnit", json!("")),
        ("buyLocation", json!("")),
    ]
}

fn to_pb_fields<T: serde::Serialize>(
    record: &T,
    app_id: &str,
    clearable: &[(&str, Value)],
) -> BackendResult<Map<String, Value>> {
    let Value::Object(mut map) = serde_json::to_value(record)
        .map_err(|e| BackendError::Other(e.to_string()))?
    else {
        return Err(BackendError::Other("record is not an object".into()));
    };

    let uuid = uuid::Uuid::parse_str(app_id).map_err(|_| {
        BackendError::Other(format!("{app_id} is not a uuid, so it has no PocketBase id"))
    })?;
    map.remove("id");
    map.insert("app_id".into(), json!(app_id));
    map.insert("id".into(), json!(to_pb_id(&uuid)));
    for (k, empty) in clearable {
        map.entry((*k).to_string()).or_insert_with(|| empty.clone());
    }
    Ok(map)
}

/// The reverse: prefer `app_id` over the PocketBase primary key, because
/// `to_pb_id` cannot be inverted.
fn asset_from_pb(v: &Value) -> Option<AssetRecord> {
    let obj = v.as_object()?;
    let mut m = obj.clone();
    // An unset Text column comes back as `""`, so absent is not the only way
    // `app_id` can be missing. An empty one would produce an AssetRecord with
    // `id: ""`, which can be picked as a sell or merge target and then fails
    // every id-based call on the uuid parse -- leaving a sell's asset
    // undecremented, or aborting a merge after the journal was written.
    let app_id = m
        .get("app_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_string();
    m.insert("id".into(), json!(app_id));
    m.remove("app_id");
    // PocketBase returns "" for an unset optional Text column, and 0 for an
    // unset Number. An absent `unit` must stay absent rather than becoming an
    // empty string that fails to parse as a CargoUnit.
    if m.get("unit").and_then(Value::as_str) == Some("") {
        m.remove("unit");
    }
    serde_json::from_value(Value::Object(m)).ok()
}

#[async_trait]
impl SyncBackend for PocketbaseBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Pocketbase
    }

    async fn sign_in(&self) -> BackendResult<Session> {
        let url = format!(
            "{}/api/collections/{AUTH_COLLECTION}/auth-with-password",
            self.base
        );
        let res = self
            .http
            .post(&url)
            .json(&json!({ "identity": self.email, "password": self.password }))
            .send()
            .await
            .map_err(|e| BackendError::Network(e.to_string()))?;

        if !res.status().is_success() {
            let status = res.status();
            let body: Value = res.json().await.unwrap_or(Value::Null);
            let msg = body
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("sign-in failed");
            return Err(BackendError::Auth(format!("{msg} (HTTP {status})")));
        }

        let body: Value = res.json().await.map_err(|e| BackendError::Protocol {
            what: "auth-with-password".into(),
            detail: e.to_string(),
        })?;
        let token = body
            .get("token")
            .and_then(Value::as_str)
            .ok_or_else(|| BackendError::Protocol {
                what: "auth-with-password".into(),
                detail: "no token in the response".into(),
            })?
            .to_string();
        let user_id = body
            .get("record")
            .and_then(|r| r.get("id"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        Ok(Session {
            token,
            // PocketBase issues no refresh token; re-authenticating is one
            // request, so there is nothing to hold here.
            refresh_token: String::new(),
            // Its tokens are long-lived, but a conservative window costs one
            // cheap sign-in and avoids reasoning about the JWT's clock.
            expires_at: chrono::Utc::now() + chrono::Duration::minutes(50),
            user_id,
        })
    }

    async fn health_check(&self) -> BackendResult<HealthReport> {
        let mut notes = Vec::new();
        let session = self.sign_in().await?;
        *self.session.lock().unwrap() = Some(session.clone());

        let assets = self.list(ASSETS, None).await;
        let trades = self.list(TRADES, None).await;

        let asset_count = match &assets {
            Ok(v) => Some(v.len()),
            Err(e) => {
                notes.push(format!("could not read assets: {e}"));
                None
            }
        };
        let trade_count = match &trades {
            Ok(v) => Some(v.len()),
            Err(e) => {
                notes.push(format!("could not read trades: {e}"));
                None
            }
        };

        // A missing column is the one way this backend loses data without
        // reporting an error, so it is surfaced here rather than discovered
        // later as an asset with no unit.
        for (rows, expected, what) in [
            (assets.as_ref().ok(), LATE_ASSET_COLUMNS, ASSETS),
            (trades.as_ref().ok(), LATE_TRADE_COLUMNS, TRADES),
        ] {
            let Some(rows) = rows else { continue };
            let missing = missing_columns(rows, expected);
            if !missing.is_empty() {
                notes.push(format!(
                    "the `{what}` collection has no {} column{} -- PocketBase accepts a write                      containing an unknown field and silently discards it, so add {} before                      writing anything",
                    missing.join(", no "),
                    if missing.len() == 1 { "" } else { "s" },
                    if missing.len() == 1 { "it" } else { "them" },
                ));
            }
        }

        let mut known_nicknames: Vec<String> = assets
            .iter()
            .flatten()
            .chain(trades.iter().flatten())
            .filter_map(|v| v.get("loggedBy").and_then(Value::as_str))
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        known_nicknames.sort();
        known_nicknames.dedup();

        Ok(HealthReport {
            ok: asset_count.is_some() && trade_count.is_some(),
            backend: BackendKind::Pocketbase,
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
        if asset.id.trim().is_empty() {
            return Err(BackendError::Other("asset has no id".into()));
        }
        let fields = to_pb_fields(asset, &asset.id, &[])?;
        self.upsert(ASSETS, fields).await
    }

    async fn upsert_trade(&self, trade: &TradeRecord) -> BackendResult<()> {
        if trade.id.trim().is_empty() {
            return Err(BackendError::Other("trade has no id".into()));
        }
        let fields = to_pb_fields(trade, &trade.id, &trade_clearable())?;
        self.upsert(TRADES, fields).await
    }

    async fn get_asset(&self, id: &str) -> BackendResult<Option<AssetRecord>> {
        let Ok(uuid) = uuid::Uuid::parse_str(id) else {
            return Ok(None);
        };
        let url = format!("{}/{}", self.records_url(ASSETS), to_pb_id(&uuid));
        let res = self
            .authed(reqwest::Method::GET, &url)
            .await?
            .send()
            .await
            .map_err(|e| BackendError::Network(e.to_string()))?;
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !res.status().is_success() {
            return Err(pb_error(ASSETS, res).await);
        }
        let body: Value = res.json().await.map_err(|e| BackendError::Protocol {
            what: ASSETS.into(),
            detail: e.to_string(),
        })?;
        Ok(asset_from_pb(&body))
    }

    async fn delete_asset(&self, id: &str) -> BackendResult<()> {
        let Ok(uuid) = uuid::Uuid::parse_str(id) else {
            return Err(BackendError::Other(format!("{id} is not a uuid")));
        };
        let url = format!("{}/{}", self.records_url(ASSETS), to_pb_id(&uuid));
        let res = self
            .authed(reqwest::Method::DELETE, &url)
            .await?
            .send()
            .await
            .map_err(|e| BackendError::Network(e.to_string()))?;
        if res.status().is_success() || res.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        Err(pb_error(ASSETS, res).await)
    }

    async fn find_assets(
        &self,
        item: &str,
        location: Option<&str>,
    ) -> BackendResult<Vec<AssetRecord>> {
        let mut filter = format!("item={}", filter_literal(item));
        if let Some(loc) = location {
            filter.push_str(&format!(" && location={}", filter_literal(loc)));
        }
        let rows = self.list(ASSETS, Some(&filter)).await?;
        // Filtered again locally: the server-side filter is the optimisation,
        // the local one is the guarantee. A filter syntax change would
        // otherwise return everything and look like a correct merge target.
        Ok(rows
            .iter()
            .filter_map(asset_from_pb)
            .filter(|a| a.item == item)
            .filter(|a| location.is_none_or(|loc| a.location == loc))
            .collect())
    }
}

#[cfg(test)]
#[path = "pocketbase_tests.rs"]
mod tests;
