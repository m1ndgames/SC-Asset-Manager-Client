//! Every `#[tauri::command]`. Thin by design: these marshal arguments and
//! delegate, so the logic stays testable without a Tauri runtime.

use std::path::PathBuf;

use tauri::State;

use crate::backend::firebase::FirebaseBackend;
use crate::backend::{BackendError, HealthReport, SyncBackend};
use crate::config::{self, BackendKind, ConfigDto, ConfigPatch, FirebaseConfig};
use crate::error::{AppError, AppResult};
use crate::drain;
use crate::log::watcher::{self, ColdStart, WatchStatus};
use crate::queue::{self, ItemPatch, ItemView};
use crate::refdata::{self, RefData};
use crate::log::{discover, scan};
use crate::state::AppState;
use crate::{watcher_task, watchstate};

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

// ---------------------------------------------------------------------------
// The watcher
// ---------------------------------------------------------------------------

/// Resolve the log path the way every watcher command resolves it: the
/// override if set, else detection.
fn watch_path(cfg: &config::Config) -> AppResult<PathBuf> {
    if !cfg.log_path.trim().is_empty() {
        return Ok(PathBuf::from(&cfg.log_path));
    }
    discover::detect().ok_or(AppError::LogNotFound)
}

/// What starting the watcher would do, without doing it.
///
/// The UI asks this first and, on `firstRun`, puts the choice to the user:
/// importing a session nobody asked to import is the one cold-start mistake
/// that cannot be undone quietly.
#[tauri::command]
pub fn plan_watch_start(state: State<'_, AppState>) -> AppResult<ColdStart> {
    let cfg = state.config();
    let path = watch_path(&cfg)?;
    Ok(cold_start_for(&path))
}

/// Start watching.
///
/// `offset` is the decision `plan_watch_start` offered: the stored mark for a
/// resume, the file length for start-from-now, or 0 to read the whole file.
/// The UI passes back what the user chose; omitting it takes the plan's own
/// answer, with start-from-now as the first-run default.
#[tauri::command]
pub fn start_watcher(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    offset: Option<u64>,
) -> AppResult<WatchStatus> {
    let cfg = state.config();
    let path = watch_path(&cfg)?;

    let mut guard = state.watcher.lock().unwrap();
    if let Some(h) = guard.as_ref() {
        // Already running on that path: report rather than start a second one.
        if h.path == path {
            return Ok(h.status());
        }
        // The path changed under us. Stop the old one first.
        if let Some(old) = guard.take() {
            old.stop();
        }
    }

    let offset = match offset {
        Some(o) => o,
        None => match cold_start_for(&path) {
            ColdStart::Resume { offset } => offset,
            // Default to start-from-now, never a silent import.
            ColdStart::FirstRun { end_offset, .. } => end_offset,
            ColdStart::Backfill { .. } => watchstate::load().offset,
            ColdStart::NoLog => 0,
        },
    };

    let handle = watcher_task::start(app, &cfg, path, offset)?;
    let status = handle.status();
    *guard = Some(handle);
    Ok(status)
}

/// The cold-start decision, with the trade count filled in only when a first
/// run makes one necessary.
fn cold_start_for(path: &std::path::Path) -> ColdStart {
    let st = watchstate::load();
    watcher::plan_cold_start(path, &st, |p| match std::fs::read(p) {
        Ok(bytes) => scan::scan(&String::from_utf8_lossy(&bytes), 0).trades.len(),
        Err(_) => 0,
    })
}

/// Stop watching. Anything still inside its grace window is promoted on the
/// way out rather than dropped.
#[tauri::command]
pub fn stop_watcher(state: State<'_, AppState>) -> AppResult<()> {
    let handle = state.watcher.lock().unwrap().take();
    if let Some(h) = handle {
        h.stop();
    }
    Ok(())
}

/// Current watcher status, or `None` when not watching.
#[tauri::command]
pub fn watcher_status(state: State<'_, AppState>) -> Option<WatchStatus> {
    state.watcher.lock().unwrap().as_ref().map(|h| h.status())
}

// ---------------------------------------------------------------------------
// The review queue
// ---------------------------------------------------------------------------

/// The whole queue as the UI renders it: a projection, never a handle.
///
/// The frontend calls a command and re-renders from what comes back. It never
/// edits an item locally, which is why there is no reconciliation logic and no
/// "unsaved edits" state anywhere in this app.
#[tauri::command]
pub fn get_queue(state: State<'_, AppState>) -> Vec<ItemView> {
    let rd = state.refdata();
    let q = state.queue.lock().unwrap();
    q.items().iter().map(|i| queue::view(i, &rd)).collect()
}

#[tauri::command]
pub fn get_history(state: State<'_, AppState>, limit: Option<usize>) -> Vec<ItemView> {
    let rd = state.refdata();
    let q = state.queue.lock().unwrap();
    // Newest first: history is read to check what just happened.
    let mut items: Vec<&crate::queue::QueueItem> = q.history().iter().collect();
    items.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    items
        .into_iter()
        .take(limit.unwrap_or(200))
        .map(|i| queue::view(i, &rd))
        .collect()
}

/// Apply a field edit. Persisted before this returns.
#[tauri::command]
pub fn update_item(
    state: State<'_, AppState>,
    id: String,
    patch: ItemPatch,
) -> AppResult<ItemView> {
    let rd = state.refdata();
    let mut q = state.queue.lock().unwrap();
    let item = q.update(&id, patch)?;
    Ok(queue::view(item, &rd))
}

/// Approve an item and write it.
///
/// Approval and writing are one user action, so they are one command: an
/// "Approved" card that then sat there doing nothing until some other event
/// fired would be indistinguishable from a bug.
#[tauri::command]
pub async fn approve_item(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> AppResult<drain::DrainReport> {
    {
        // Scoped: the guard must not be alive across the await below.
        let rd = state.refdata();
        let mut q = state.queue.lock().unwrap();
        q.approve(&id, &rd)?;
    }
    Ok(drain::drain(&app).await)
}

#[tauri::command]
pub fn reject_item(state: State<'_, AppState>, id: String) -> AppResult<()> {
    let mut q = state.queue.lock().unwrap();
    q.reject(&id)
}

/// Retry a failed write.
#[tauri::command]
pub async fn retry_item(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> AppResult<drain::DrainReport> {
    {
        let mut q = state.queue.lock().unwrap();
        q.retry(&id)?;
    }
    Ok(drain::drain(&app).await)
}

/// Write anything already approved. Called on startup, and available manually
/// after a backend problem is fixed.
#[tauri::command]
pub async fn drain_queue(app: tauri::AppHandle) -> drain::DrainReport {
    drain::drain(&app).await
}

// ---------------------------------------------------------------------------
// Reference data
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_refdata(state: State<'_, AppState>) -> RefData {
    state.refdata()
}

/// Fetch reference data and cache it.
///
/// On failure the cached copy is kept: losing every item name because the
/// network blinked would be worse than showing slightly stale ones.
#[tauri::command]
pub async fn refresh_refdata(state: State<'_, AppState>) -> AppResult<RefData> {
    let url = state.config().app_url;
    let fetched = refdata::fetch(&url).await?;
    if let Err(e) = refdata::save(&fetched) {
        // Worth saying, not worth failing: it is in memory and working.
        tracing::warn!(error = %e, "fetched reference data but could not cache it");
    }
    *state.refdata.write().unwrap() = fetched.clone();
    Ok(fetched)
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
