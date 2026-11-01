//! `%APPDATA%\sc-asset-manager-client\config.toml` and the directory beside it.
//!
//! Written atomically (temp file + rename) so a crash mid-write cannot leave a
//! truncated config. Carries `schema = 1` so a future migration is not
//! guesswork.
//!
//! **The backend password is stored here in cleartext.** That is a deliberate
//! choice over an OS keyring, and it comes with obligations that are enforced
//! in code rather than left to discipline:
//!   - it is never returned to the webview — [`ConfigDto`] carries
//!     `password_set: bool` instead;
//!   - it is never logged — see [`Config::redacted`];
//!   - it is never put in an error message.
//!
//! Tokens derived from it stay in memory only.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

pub const SCHEMA: u32 = 1;

/// Which sync backend to write through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    #[default]
    Firebase,
    Pocketbase,
}

/// The web app's Firebase config object, field-for-field.
///
/// This is pasted wholesale from SC-Asset-Manager's own Settings, so the two
/// cannot disagree about which project they are talking to. Only `api_key` and
/// `project_id` are actually used; the rest is kept so a round-trip through
/// this client does not lose anything.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FirebaseConfig {
    pub api_key: String,
    pub auth_domain: String,
    pub project_id: String,
    pub storage_bucket: String,
    pub messaging_sender_id: String,
    pub app_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub measurement_id: Option<String>,
}

impl FirebaseConfig {
    pub fn is_usable(&self) -> bool {
        !self.api_key.trim().is_empty() && !self.project_id.trim().is_empty()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PocketbaseConfig {
    pub url: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BackendConfig {
    pub kind: BackendKind,
    /// Account used to sign in. An ordinary group member, never a service
    /// account.
    pub email: String,
    /// Cleartext. See the module docs.
    pub password: String,
    pub firebase: FirebaseConfig,
    pub pocketbase: PocketbaseConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub schema: u32,
    /// Must match the nickname in SC-Asset-Manager's own Settings. One
    /// character's difference makes every record this client writes unsellable
    /// for a `user`-role member.
    pub nickname: String,
    /// Where reference data is fetched from.
    pub app_url: String,
    /// Manual Game.log override; empty means auto-detect.
    pub log_path: String,
    pub poll_interval_ms: u64,
    pub grace_window_ms: i64,
    pub backend: BackendConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            nickname: String::new(),
            app_url: "https://sc-assetmanager.org".into(),
            log_path: String::new(),
            poll_interval_ms: 1000,
            grace_window_ms: crate::log::scan::DEFAULT_HOLD_MS,
            backend: BackendConfig::default(),
        }
    }
}

impl Config {
    /// A copy safe to log or put in a diagnostics dump.
    pub fn redacted(&self) -> Config {
        let mut c = self.clone();
        if !c.backend.password.is_empty() {
            c.backend.password = "<redacted>".into();
        }
        c
    }

    /// What the frontend is allowed to see.
    pub fn to_dto(&self) -> ConfigDto {
        ConfigDto {
            schema: self.schema,
            nickname: self.nickname.clone(),
            app_url: self.app_url.clone(),
            log_path: self.log_path.clone(),
            poll_interval_ms: self.poll_interval_ms,
            grace_window_ms: self.grace_window_ms,
            backend_kind: self.backend.kind,
            email: self.backend.email.clone(),
            password_set: !self.backend.password.is_empty(),
            firebase: self.backend.firebase.clone(),
            pocketbase: self.backend.pocketbase.clone(),
        }
    }

    /// Whether the configured backend has enough to attempt a sign-in.
    pub fn backend_ready(&self) -> bool {
        if self.backend.email.trim().is_empty() || self.backend.password.is_empty() {
            return false;
        }
        match self.backend.kind {
            BackendKind::Firebase => self.backend.firebase.is_usable(),
            BackendKind::Pocketbase => !self.backend.pocketbase.url.trim().is_empty(),
        }
    }
}

/// The config as the webview sees it. Note the absence of `password`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigDto {
    pub schema: u32,
    pub nickname: String,
    pub app_url: String,
    pub log_path: String,
    pub poll_interval_ms: u64,
    pub grace_window_ms: i64,
    pub backend_kind: BackendKind,
    pub email: String,
    /// True when a password is stored. The password itself never crosses here.
    pub password_set: bool,
    pub firebase: FirebaseConfig,
    pub pocketbase: PocketbaseConfig,
}

/// What the frontend may change. `password: None` means "leave the stored one
/// alone", which is how the UI can save other fields without ever holding the
/// secret.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigPatch {
    pub nickname: Option<String>,
    pub app_url: Option<String>,
    pub log_path: Option<String>,
    pub poll_interval_ms: Option<u64>,
    pub grace_window_ms: Option<i64>,
    pub backend_kind: Option<BackendKind>,
    pub email: Option<String>,
    pub password: Option<String>,
    pub firebase: Option<FirebaseConfig>,
    pub pocketbase: Option<PocketbaseConfig>,
}

impl Config {
    pub fn apply(&mut self, p: ConfigPatch) {
        if let Some(v) = p.nickname {
            self.nickname = v.trim().to_string();
        }
        if let Some(v) = p.app_url {
            self.app_url = v.trim().trim_end_matches('/').to_string();
        }
        if let Some(v) = p.log_path {
            self.log_path = v.trim().to_string();
        }
        if let Some(v) = p.poll_interval_ms {
            self.poll_interval_ms = v.clamp(250, 60_000);
        }
        if let Some(v) = p.grace_window_ms {
            self.grace_window_ms = v.clamp(0, 60_000);
        }
        if let Some(v) = p.backend_kind {
            self.backend.kind = v;
        }
        if let Some(v) = p.email {
            self.backend.email = v.trim().to_string();
        }
        // Only overwrite the password when one was actually supplied; an empty
        // string is a deliberate clear.
        if let Some(v) = p.password {
            self.backend.password = v;
        }
        if let Some(v) = p.firebase {
            self.backend.firebase = v;
        }
        if let Some(v) = p.pocketbase {
            self.backend.pocketbase = v;
        }
    }
}

/// `%APPDATA%\sc-asset-manager-client`.
///
/// Deliberately not Tauri's `app_config_dir()`, which would give
/// `%APPDATA%\<bundle-identifier>\`, and deliberately not the `directories`
/// crate — we need exactly one path.
pub fn config_dir() -> AppResult<PathBuf> {
    let appdata = std::env::var("APPDATA")
        .map_err(|_| AppError::Config("APPDATA is not set".into()))?;
    Ok(PathBuf::from(appdata).join("sc-asset-manager-client"))
}

pub fn config_path() -> AppResult<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

pub fn load() -> AppResult<Config> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(Config::default());
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| AppError::Config(format!("cannot read config: {e}")))?;
    let mut cfg: Config = toml::from_str(&text)
        .map_err(|e| AppError::Config(format!("config.toml is not valid: {e}")))?;
    if cfg.schema != SCHEMA {
        // Nothing to migrate yet; record the fact rather than silently
        // accepting a shape we do not understand.
        tracing::warn!(found = cfg.schema, expected = SCHEMA, "config schema differs");
        cfg.schema = SCHEMA;
    }
    Ok(cfg)
}

/// Atomic write: temp file in the same directory, then rename over the target.
pub fn save(cfg: &Config) -> AppResult<()> {
    let dir = config_dir()?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| AppError::Config(format!("cannot create {}: {e}", dir.display())))?;
    let path = config_path()?;
    let tmp = dir.join("config.toml.tmp");

    let text = toml::to_string_pretty(cfg)
        .map_err(|e| AppError::Config(format!("cannot serialise config: {e}")))?;
    write_atomic(&tmp, &path, text.as_bytes())
}

/// Atomic write, shared with `watchstate`: temp file in the same directory,
/// fsynced, then renamed over the target.
pub fn write_atomic(tmp: &Path, target: &Path, bytes: &[u8]) -> AppResult<()> {
    use std::io::Write;
    {
        let mut f = std::fs::File::create(tmp)
            .map_err(|e| AppError::Config(format!("cannot open temp file: {e}")))?;
        f.write_all(bytes)
            .map_err(|e| AppError::Config(format!("cannot write temp file: {e}")))?;
        f.sync_all()
            .map_err(|e| AppError::Config(format!("cannot flush temp file: {e}")))?;
    }
    // Windows rename fails if the target exists, so remove it first. The window
    // between the two is why the temp file is written and synced beforehand.
    if target.exists() {
        let _ = std::fs::remove_file(target);
    }
    std::fs::rename(tmp, target)
        .map_err(|e| AppError::Config(format!("cannot replace config: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled() -> Config {
        Config {
            nickname: "m1nd".into(),
            backend: BackendConfig {
                email: "someone@example.com".into(),
                password: "hunter2".into(),
                firebase: FirebaseConfig {
                    api_key: "AIza-test".into(),
                    project_id: "star-citizen-asset-manager".into(),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// The password must not be reachable from anything the webview receives.
    #[test]
    fn dto_never_carries_the_password() {
        let dto = filled().to_dto();
        assert!(dto.password_set);
        let json = serde_json::to_string(&dto).unwrap();
        assert!(!json.contains("hunter2"), "password leaked into the DTO: {json}");
        assert!(json.contains("passwordSet"));
    }

    #[test]
    fn redacted_hides_the_password_but_keeps_shape() {
        let r = filled().redacted();
        assert_eq!(r.backend.password, "<redacted>");
        assert_eq!(r.backend.email, "someone@example.com");
        let text = toml::to_string_pretty(&r).unwrap();
        assert!(!text.contains("hunter2"));
    }

    #[test]
    fn redacting_an_empty_password_adds_nothing() {
        let c = Config::default();
        assert_eq!(c.redacted().backend.password, "");
    }

    #[test]
    fn round_trips_through_toml() {
        let c = filled();
        let text = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(c, back);
        assert_eq!(back.schema, SCHEMA);
    }

    /// A config written by an older build, missing newer keys, must still load.
    #[test]
    fn missing_keys_fall_back_to_defaults() {
        let c: Config = toml::from_str("schema = 1\nnickname = \"x\"\n").unwrap();
        assert_eq!(c.nickname, "x");
        assert_eq!(c.app_url, "https://sc-assetmanager.org");
        assert_eq!(c.poll_interval_ms, 1000);
        assert_eq!(c.grace_window_ms, 3000);
    }

    #[test]
    fn patch_without_password_keeps_the_stored_one() {
        let mut c = filled();
        c.apply(ConfigPatch {
            nickname: Some("other".into()),
            app_url: None,
            log_path: None,
            poll_interval_ms: None,
            grace_window_ms: None,
            backend_kind: None,
            email: None,
            password: None,
            firebase: None,
            pocketbase: None,
        });
        assert_eq!(c.nickname, "other");
        assert_eq!(c.backend.password, "hunter2", "password must survive a patch");
    }

    #[test]
    fn patch_trims_and_clamps() {
        let mut c = Config::default();
        c.apply(ConfigPatch {
            nickname: Some("  m1nd  ".into()),
            app_url: Some("https://example.org/".into()),
            log_path: None,
            poll_interval_ms: Some(1),
            grace_window_ms: Some(999_999),
            backend_kind: None,
            email: Some("  a@b.c ".into()),
            password: None,
            firebase: None,
            pocketbase: None,
        });
        assert_eq!(c.nickname, "m1nd");
        assert_eq!(c.app_url, "https://example.org", "trailing slash removed");
        assert_eq!(c.email_for_test(), "a@b.c");
        assert_eq!(c.poll_interval_ms, 250, "clamped up from 1ms");
        assert_eq!(c.grace_window_ms, 60_000, "clamped down");
    }

    #[test]
    fn backend_ready_requires_credentials_and_project() {
        let mut c = Config::default();
        assert!(!c.backend_ready());
        c.backend.email = "a@b.c".into();
        c.backend.password = "x".into();
        assert!(!c.backend_ready(), "still no Firebase project");
        c.backend.firebase.api_key = "k".into();
        c.backend.firebase.project_id = "p".into();
        assert!(c.backend_ready());

        c.backend.kind = BackendKind::Pocketbase;
        assert!(!c.backend_ready(), "PocketBase needs a url");
        c.backend.pocketbase.url = "https://pb.example".into();
        assert!(c.backend_ready());
    }

    /// Mirrors the shape the user pastes out of the web app's Settings.
    #[test]
    fn parses_the_web_app_firebase_config_json() {
        let json = r#"{
            "apiKey": "AIzaSyExample",
            "authDomain": "star-citizen-asset-manager.firebaseapp.com",
            "projectId": "star-citizen-asset-manager",
            "storageBucket": "star-citizen-asset-manager.firebasestorage.app",
            "messagingSenderId": "66347412400",
            "appId": "1:66347412400:web:abc",
            "measurementId": "G-XYZ"
        }"#;
        let f: FirebaseConfig = serde_json::from_str(json).unwrap();
        assert_eq!(f.project_id, "star-citizen-asset-manager");
        assert_eq!(f.measurement_id.as_deref(), Some("G-XYZ"));
        assert!(f.is_usable());
    }

    impl Config {
        fn email_for_test(&self) -> &str {
            &self.backend.email
        }
    }
}
