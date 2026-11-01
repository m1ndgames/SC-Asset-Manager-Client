//! Every `#[tauri::command]`. Thin by design: these marshal arguments and
//! delegate, so the logic stays testable without a Tauri runtime.

use std::path::PathBuf;

use tauri::State;

use crate::backend::firebase::FirebaseBackend;
use crate::backend::{BackendError, HealthReport, SyncBackend};
use crate::config::{self, BackendKind, ConfigDto, ConfigPatch, FirebaseConfig};
use crate::error::{AppError, AppResult};
use crate::log::{discover, scan};
use crate::state::AppState;

/// Where the client thinks your Game.log is.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogLocation {
    pub path: Option<String>,
    pub candidates: Vec<String>,
    /// True when the path came from the config rather than detection.
    pub overridden: bool,
}

#[tauri::command]
pub fn detect_log_path(state: State<'_, AppState>) -> LogLocation {
    let cfg = state.config();
    let candidates = discover::candidates();
    let override_path = (!cfg.log_path.trim().is_empty()).then(|| cfg.log_path.clone());
    LogLocation {
        path: override_path
            .clone()
            .or_else(|| candidates.first().map(|p| p.display().to_string())),
        candidates: candidates.iter().map(|p| p.display().to_string()).collect(),
        overridden: override_path.is_some(),
    }
}

/// Parse a whole log and report what was found, without writing anything.
#[tauri::command]
pub fn rescan_log(
    state: State<'_, AppState>,
    path: Option<String>,
    hold_ms: Option<i64>,
) -> AppResult<scan::ScanReport> {
    let cfg = state.config();
    let path: PathBuf = match path.filter(|p| !p.trim().is_empty()) {
        Some(p) => PathBuf::from(p),
        None if !cfg.log_path.trim().is_empty() => PathBuf::from(&cfg.log_path),
        None => discover::detect().ok_or(AppError::LogNotFound)?,
    };

    // from_utf8_lossy: a stray byte in an entity name must never stop a scan.
    let bytes = std::fs::read(&path).map_err(|source| AppError::LogUnreadable {
        path: path.display().to_string(),
        source,
    })?;
    let contents = String::from_utf8_lossy(&bytes);

    Ok(scan::scan(
        &contents,
        hold_ms.unwrap_or(cfg.grace_window_ms),
    ))
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// The config as the webview may see it — never includes the password.
#[tauri::command]
pub fn get_config(state: State<'_, AppState>) -> ConfigDto {
    state.config().to_dto()
}

/// Where the config file lives, so Settings can show it.
#[tauri::command]
pub fn config_file_path() -> AppResult<String> {
    Ok(config::config_path()?.display().to_string())
}

/// Apply a patch and persist it atomically.
///
/// `password: None` leaves the stored password alone, which is how the UI
/// saves every other field without ever holding the secret.
#[tauri::command]
pub fn set_config(state: State<'_, AppState>, patch: ConfigPatch) -> AppResult<ConfigDto> {
    let mut guard = state.config.lock().unwrap();
    guard.apply(patch);
    config::save(&guard)?;
    tracing::info!(config = ?guard.redacted(), "configuration saved");
    Ok(guard.to_dto())
}

/// Parse a Firebase web config pasted straight out of SC-Asset-Manager's
/// Settings.
///
/// Accepts the bare JSON object, and also tolerates the `const firebaseConfig =
/// {...};` form people copy out of a snippet.
#[tauri::command]
pub fn parse_firebase_config(text: String) -> AppResult<FirebaseConfig> {
    let t = text.trim();
    let start = t
        .find('{')
        .ok_or_else(|| AppError::Config("that does not look like a Firebase config".into()))?;
    let end = t
        .rfind('}')
        .ok_or_else(|| AppError::Config("that does not look like a Firebase config".into()))?;
    if end <= start {
        return Err(AppError::Config("that does not look like a Firebase config".into()));
    }
    let body = &t[start..=end];

    let cfg: FirebaseConfig = serde_json::from_str(body)
        .map_err(|e| AppError::Config(format!("could not read the config: {e}")))?;
    if !cfg.is_usable() {
        return Err(AppError::Config(
            "the config is missing apiKey or projectId".into(),
        ));
    }
    Ok(cfg)
}

/// Settings "Test connection". Signs in and reads; mutates nothing.
#[tauri::command]
pub async fn test_backend(state: State<'_, AppState>) -> Result<HealthReport, BackendError> {
    let cfg = state.config();
    match cfg.backend.kind {
        BackendKind::Firebase => {
            let b = FirebaseBackend::from_config(&cfg)?;
            b.health_check().await
        }
        BackendKind::Pocketbase => Err(BackendError::NotConfigured(
            "PocketBase support is not built yet — it lands in Phase 6".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_bare_json_config() {
        let f = parse_firebase_config(
            r#"{"apiKey":"k","authDomain":"a","projectId":"p","storageBucket":"s",
                "messagingSenderId":"m","appId":"i"}"#
                .into(),
        )
        .unwrap();
        assert_eq!(f.project_id, "p");
    }

    /// People copy the snippet, not just the object.
    #[test]
    fn parses_the_const_snippet_form() {
        let f = parse_firebase_config(
            r#"const firebaseConfig = {
                 apiKey: "k",
                 "projectId": "p"
               };"#
            .into(),
        );
        // Unquoted keys are not JSON; this must fail cleanly rather than panic.
        assert!(f.is_err());

        let f = parse_firebase_config(
            r#"const firebaseConfig = {"apiKey":"k","projectId":"p"};"#.into(),
        )
        .unwrap();
        assert_eq!(f.api_key, "k");
    }

    #[test]
    fn rejects_a_config_missing_the_project() {
        let e = parse_firebase_config(r#"{"apiKey":"k"}"#.into()).unwrap_err();
        assert_eq!(e.kind(), "config");
    }

    #[test]
    fn rejects_nonsense() {
        assert!(parse_firebase_config("hello".into()).is_err());
        assert!(parse_firebase_config(String::new()).is_err());
    }
}
