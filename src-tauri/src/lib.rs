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
pub mod drain;
pub mod error;
pub mod ids;
pub mod log;
pub mod model;
pub mod probe;
pub mod queue;
pub mod refdata;
pub mod state;
pub mod units;
pub mod update;
pub mod writer;
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

    // The queue is the single source of truth and must load before the window
    // does, so a crash mid-review reappears rather than vanishing.
    let queue = queue::Queue::open_default().unwrap_or_else(|e| {
        tracing::error!(error = %e, "no config directory; the queue will not persist");
        queue::Queue::open(std::env::temp_dir().join("sc-asset-manager-client"))
    });
    tracing::info!(
        pending = queue.items().len(),
        history = queue.history().len(),
        "queue loaded"
    );

    // Cached reference data only. A refresh happens on request, and at startup
    // when there is nothing cached — the app must open with the network down.
    let refdata = refdata::load();

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
        .manage(state::AppState::new(cfg, queue, refdata))
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
            commands::get_queue,
            commands::get_history,
            commands::update_item,
            commands::approve_item,
            commands::reject_item,
            commands::retry_item,
            commands::restore_item,
            commands::drain_queue,
            commands::preview_sell,
            commands::preview_merge,
            commands::export_history_csv,
            commands::get_refdata,
            commands::refresh_refdata,
            update::check_for_update,
            update::install_update,
            probe::phase1_probe,
            probe::delete_test_asset,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                // Approved items survive a restart; PLAN.md §6 wants them
                // written before anything new is processed. Safe to repeat
                // because every write is an idempotent upsert.
                let report = drain::drain(&handle).await;
                if report.written > 0 || report.failed > 0 {
                    tracing::info!(?report, "drained approved items left from the last run");
                }

                // First run, or a cache that never populated.
                let needs_refdata = {
                    use tauri::Manager;
                    handle.state::<state::AppState>().refdata().is_empty()
                };
                if needs_refdata {
                    use tauri::Manager;
                    let url = handle.state::<state::AppState>().config().app_url;
                    match refdata::fetch(&url).await {
                        Ok(d) => {
                            let _ = refdata::save(&d);
                            tracing::info!(
                                commodities = d.commodities.len(),
                                locations = d.locations.len(),
                                "reference data fetched"
                            );
                            *handle.state::<state::AppState>().refdata.write().unwrap() = d;
                            let _ = tauri::Emitter::emit(&handle, "refdata://updated", ());
                        }
                        Err(e) => tracing::warn!(
                            error = %e,
                            "could not fetch reference data; item names will be unresolved until a refresh"
                        ),
                    }
                }

                // Autostart, last: reference data first means the trades it
                // finds can resolve their item names straight away.
                //
                // This delegates to the same `start_watcher` the button calls,
                // with no offset, which is what keeps the "nothing is read
                // until the user says so" invariant intact — that path defaults
                // a first run to start-from-now and never backfills silently.
                // Enabling autostart therefore cannot import history behind the
                // user's back; it only removes the click.
                {
                    use tauri::Manager;
                    let state = handle.state::<state::AppState>();
                    if state.config().autostart_watcher {
                        match commands::start_watcher(handle.clone(), state, None) {
                            Ok(s) => tracing::info!(
                                path = %s.path,
                                offset = s.safe_offset,
                                "watcher autostarted"
                            ),
                            // Never fatal. A missing log or a bad path must not
                            // stop the app opening — the user needs Settings to
                            // fix exactly that.
                            Err(e) => tracing::warn!(
                                error = ?e,
                                "autostart is on but the watcher could not start"
                            ),
                        }
                    }
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
