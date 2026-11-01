//! The update check.
//!
//! In Rust rather than via `@tauri-apps/plugin-updater`, because the webview
//! makes no outbound requests — that is what keeps the CSP at
//! `default-src 'self'`. Using the JS API would mean the renderer fetching the
//! manifest and installer, forcing the CSP open to GitHub.
//!
//! Nothing checks automatically. The user asks, in Settings.

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_updater::UpdaterExt;

use crate::error::{AppError, AppResult};
use crate::queue::ItemState;
use crate::state::AppState;

/// What the Settings panel shows after a check.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub available: bool,
    /// The version running now, from `tauri.conf.json`.
    pub current_version: String,
    /// The version offered, when there is one.
    pub version: Option<String>,
    /// The release notes, as published in `latest.json`.
    pub notes: Option<String>,
    /// The publish date, RFC 3339, or `None` when the manifest omits it.
    pub date: Option<String>,
}

/// Progress, emitted on `update://progress` while an install runs.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProgress {
    pub downloaded: u64,
    /// `None` when the server sends no `Content-Length`.
    pub total: Option<u64>,
    pub done: bool,
}

fn updater(app: &AppHandle) -> AppResult<tauri_plugin_updater::Updater> {
    app.updater()
        .map_err(|e| AppError::Update(format!("the updater is not available: {e}")))
}

/// Ask the endpoint whether there is a newer release. Mutates nothing.
#[tauri::command]
pub async fn check_for_update(app: AppHandle) -> AppResult<UpdateInfo> {
    let current = app.package_info().version.to_string();

    let found = updater(&app)?.check().await.map_err(|e| {
        // The most likely causes are being offline and there being no release
        // published yet, and neither is worth an alarming message.
        AppError::Update(format!("could not check for updates: {e}"))
    })?;

    Ok(match found {
        Some(u) => UpdateInfo {
            available: true,
            current_version: current,
            version: Some(u.version.clone()),
            notes: u.body.clone(),
            // Converted through the unix timestamp rather than `time`'s own
            // formatter, so this file does not pull in a second date crate
            // (PLAN.md §2 chose chrono deliberately) and so the string is
            // spelled the same way as every other timestamp in the app.
            date: u.date.and_then(|d| {
                chrono::DateTime::from_timestamp(d.unix_timestamp(), 0).map(|t| {
                    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                })
            }),
        },
        None => UpdateInfo {
            available: false,
            current_version: current,
            version: None,
            notes: None,
            date: None,
        },
    })
}

/// Download and install, then relaunch.
///
/// An interrupted write is recoverable by the same machinery that handles a
/// crash, since every transition is fsynced and a journalled merge is promoted
/// to `Written` on reopen. Even so this refuses while anything is waiting to be
/// written, because losing a part-finished review is not recoverable.
#[tauri::command]
pub async fn install_update(app: AppHandle, state: State<'_, AppState>) -> AppResult<()> {
    let writing = {
        let q = state.queue.lock().unwrap();
        q.items()
            .iter()
            .filter(|i| matches!(i.state, ItemState::Writing | ItemState::Approved))
            .count()
    };
    if writing > 0 {
        return Err(AppError::Update(format!(
            "{writing} trade{} still waiting to be written. Let the queue finish, then update.",
            if writing == 1 { " is" } else { "s are" }
        )));
    }

    let update = updater(&app)?
        .check()
        .await
        .map_err(|e| AppError::Update(format!("could not check for updates: {e}")))?
        .ok_or_else(|| AppError::Update("there is no update to install".into()))?;

    let mut downloaded: u64 = 0;
    let progress_app = app.clone();
    let done_app = app.clone();

    update
        .download_and_install(
            move |chunk, total| {
                downloaded += chunk as u64;
                let _ = progress_app.emit(
                    "update://progress",
                    UpdateProgress {
                        downloaded,
                        total,
                        done: false,
                    },
                );
            },
            move || {
                let _ = done_app.emit(
                    "update://progress",
                    UpdateProgress {
                        downloaded: 0,
                        total: None,
                        done: true,
                    },
                );
            },
        )
        .await
        .map_err(|e| AppError::Update(format!("the update could not be installed: {e}")))?;

    // On Windows the installer replaces the running exe, so the process exits
    // here and the relaunch is the installer's job.
    app.restart();
}

#[cfg(test)]
mod tests {

    /// If these diverge, an update either never offers or offers forever.
    #[test]
    fn the_crate_version_matches_the_bundle_version() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(
            conf["version"].as_str().unwrap(),
            env!("CARGO_PKG_VERSION"),
            "tauri.conf.json and Cargo.toml disagree about the version"
        );
    }

    /// `requireSignedVersion` must stay on: it is the only guard against a
    /// crafted manifest pairing a new version with the signature of an older,
    /// genuinely signed release. Turning it on later is not equivalent — until
    /// it is on the attack works, and existing releases would need re-signing.
    #[test]
    fn signed_versions_are_required() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(
            conf["plugins"]["updater"]["requireSignedVersion"],
            serde_json::json!(true),
            "requireSignedVersion must not be turned off"
        );
    }

    /// Updater artifacts are off by default, and a build without them looks
    /// fine: installers are produced, signing secrets are accepted, and
    /// `latest.json` points at files that were never created.
    #[test]
    fn updater_artifacts_are_produced() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(
            conf["bundle"]["createUpdaterArtifacts"],
            serde_json::json!(true),
            "without this the build produces no .sig files and the updater never works"
        );
    }

    /// The pubkey is the `.key.pub` file's contents verbatim — already base64.
    /// Encoding it twice produces a key that parses but verifies nothing.
    #[test]
    fn the_update_pubkey_is_configured() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let pubkey = conf["plugins"]["updater"]["pubkey"].as_str().unwrap();
        assert!(!pubkey.is_empty());
        let decoded = String::from_utf8(
            base64_decode(pubkey).expect("the pubkey must be valid base64"),
        )
        .expect("and must decode to text");
        assert!(
            decoded.starts_with("untrusted comment:"),
            "the pubkey does not look like a minisign key -- it may be double-encoded: {decoded}"
        );
    }

    /// Avoids pulling in a base64 crate the app does not otherwise need.
    fn base64_decode(s: &str) -> Option<Vec<u8>> {
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut acc: u32 = 0;
        let mut bits = 0;
        let mut out = Vec::new();
        for c in s.bytes().filter(|c| !c.is_ascii_whitespace()) {
            if c == b'=' {
                break;
            }
            let v = A.iter().position(|a| *a == c)? as u32;
            acc = (acc << 6) | v;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
            }
        }
        Some(out)
    }
}
