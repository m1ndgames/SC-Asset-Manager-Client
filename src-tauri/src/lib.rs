//! SC-Asset-Manager-Client.
//!
//! Watches Star Citizen's Game.log for commodity trades, holds each one for
//! human review — the log has no location, which is why a human is in the loop
//! — and writes approved records into the user's SC-Asset-Manager backend.
//!
//! INTEGRATIONS.md in the main repo is the authoritative data contract;
//! PLAN.md here is the design of record.

pub mod backend;
pub mod commands;
pub mod config;
pub mod error;
pub mod ids;
pub mod log;
pub mod model;
pub mod probe;
pub mod state;
pub mod units;
pub mod watcher_task;
pub mod watchstate;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "sc_asset_manager_client_lib=info".into()),
        )
        .init();

    // A broken config must not stop the app from starting — the user needs
    // Settings in order to fix it.
    let cfg = config::load().unwrap_or_else(|e| {
        tracing::error!(error = %e, "could not load config; starting with defaults");
        config::Config::default()
    });

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
        .manage(state::AppState::new(cfg))
        .invoke_handler(tauri::generate_handler![
            commands::detect_log_path,
            commands::rescan_log,
            commands::get_config,
            commands::set_config,
            commands::config_file_path,
            commands::parse_firebase_config,
            commands::test_backend,
            commands::plan_watch_start,
            commands::start_watcher,
            commands::stop_watcher,
            commands::watcher_status,
            probe::phase1_probe,
            probe::delete_test_asset,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
