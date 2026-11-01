//! Every `#[tauri::command]`. Thin by design: these marshal arguments and
//! delegate, so the logic stays testable without a Tauri runtime.

use std::path::PathBuf;

use crate::error::{AppError, AppResult};
use crate::log::{discover, scan};

/// Where the client thinks your Game.log is.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogLocation {
    pub path: Option<String>,
    pub candidates: Vec<String>,
}

#[tauri::command]
pub fn detect_log_path() -> LogLocation {
    let candidates = discover::candidates();
    LogLocation {
        path: candidates.first().map(|p| p.display().to_string()),
        candidates: candidates.iter().map(|p| p.display().to_string()).collect(),
    }
}

/// Parse a whole log and report what was found, without writing anything.
///
/// Phase 1's proving ground: it exercises extraction, unit derivation, the
/// deterministic ids and the grace window against real data, with no tailer
/// and no backend involved.
#[tauri::command]
pub fn rescan_log(path: Option<String>, hold_ms: Option<i64>) -> AppResult<scan::ScanReport> {
    let path: PathBuf = match path.filter(|p| !p.trim().is_empty()) {
        Some(p) => PathBuf::from(p),
        None => discover::detect().ok_or(AppError::LogNotFound)?,
    };

    // from_utf8_lossy: a stray byte in an entity name must never stop a scan.
    let bytes = std::fs::read(&path).map_err(|source| AppError::LogUnreadable {
        path: path.display().to_string(),
        source,
    })?;
    let contents = String::from_utf8_lossy(&bytes);

    Ok(scan::scan(&contents, hold_ms.unwrap_or(scan::DEFAULT_HOLD_MS)))
}
