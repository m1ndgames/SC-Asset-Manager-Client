//! SC-Asset-Manager-Client.
//!
//! Watches Star Citizen's Game.log for commodity trades, holds each one for
//! human review — the log has no location, which is why a human is in the loop
//! — and writes approved records into the user's SC-Asset-Manager backend.
//!
//! INTEGRATIONS.md in the main repo is the authoritative data contract;
//! PLAN.md here is the design of record.

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "sc_asset_manager_client_lib=info".into()),
        )
        .init();

    tauri::Builder::default()
        // Two tailers would double-write every trade. A second launch must
        // focus the existing window instead of starting a second watcher.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            use tauri::Manager;
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_focus();
                let _ = w.unminimize();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
