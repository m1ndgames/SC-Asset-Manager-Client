//! Running the watcher: the thread, the events, and `state.json`.
//!
//! Deliberately thin, and deliberately outside `log/` — everything in there is
//! clock-injected and Tauri-free so it can be tested over a temp file. This is
//! the one place that knows about `AppHandle`, and it holds no logic worth
//! testing beyond wiring.
//!
//! **A thread, not a Tokio task.** `Tailer` does blocking `std::fs` reads, so
//! it would have to be `spawn_blocking`ed anyway, and the loop owns nothing
//! async. One named OS thread is easier to reason about and easier to see in a
//! debugger.
//!
//! ## Persistence ordering
//!
//! Per tick: emit the detections, *then* write `state.json`. The offset and the
//! duplicate-filter ids live in that one file, so a single atomic write cannot
//! leave them disagreeing. If the process dies between the emit and the write,
//! the offset has not advanced and those lines are re-read — and the
//! deterministic id collapses the duplicate. The reverse order loses trades.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter};

use crate::config::Config;
use crate::error::{AppError, AppResult};
use crate::log::watcher::{WatchStatus, Watcher};
use crate::watchstate;

/// Event names. Namespaced so the frontend can subscribe by area.
pub const EV_STATUS: &str = "watcher://status";
pub const EV_ROTATED: &str = "watcher://log-rotated";
pub const EV_DETECTED: &str = "tx://detected";
pub const EV_DISCARDED: &str = "tx://discarded";
pub const EV_MALFORMED: &str = "tx://malformed";

/// A running watcher. Dropping this does not stop the thread — call
/// [`WatcherHandle::stop`], which also lets the thread promote anything still
/// inside its grace window rather than dropping it.
pub struct WatcherHandle {
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<WatchStatus>>,
    join: Option<std::thread::JoinHandle<()>>,
    pub path: PathBuf,
}

impl WatcherHandle {
    pub fn status(&self) -> WatchStatus {
        self.status.lock().unwrap().clone()
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(j) = self.join.take() {
            // The thread wakes within one poll interval, drains the grace
            // window and persists before exiting.
            let _ = j.join();
        }
    }
}

/// Start watching `path`, resuming at `offset`.
///
/// `offset` comes from `log::watcher::plan_cold_start` — the stored mark for a
/// resume, the file's length for start-from-now, or 0 to read the lot.
pub fn start(app: AppHandle, cfg: &Config, path: PathBuf, offset: u64) -> AppResult<WatcherHandle> {
    let grace = Duration::from_millis(cfg.grace_window_ms.max(0) as u64);
    // A zero poll interval would spin a core; a very long one makes the app
    // feel broken. Clamp rather than trust the config file.
    let interval = Duration::from_millis(cfg.poll_interval_ms.clamp(200, 60_000));

    let state = watchstate::load();
    let mut watcher = Watcher::new(&path, grace, state, offset);

    let stop = Arc::new(AtomicBool::new(false));
    let status = Arc::new(Mutex::new(watcher.status().clone()));

    let thread_stop = Arc::clone(&stop);
    let thread_status = Arc::clone(&status);
    let thread_path = path.clone();

    let join = std::thread::Builder::new()
        .name("game-log-watcher".into())
        .spawn(move || {
            tracing::info!(path = %thread_path.display(), ?grace, ?interval, "watcher started");
            let _ = app.emit(EV_STATUS, watcher.status());

            while !thread_stop.load(Ordering::SeqCst) {
                match watcher.tick(Instant::now()) {
                    Ok(tick) => {
                        let quiet = tick.is_quiet();

                        if tick.rotated {
                            tracing::info!("Game.log was rotated or truncated");
                            let _ = app.emit(EV_ROTATED, watcher.status());
                        }
                        for t in &tick.promoted {
                            tracing::info!(id = %t.line.id, kind = ?t.line.kind, "trade detected");
                            let _ = app.emit(EV_DETECTED, t);
                        }
                        for d in &tick.discarded {
                            tracing::info!(
                                id = %d.trade.id,
                                result = %d.error.result,
                                delta_ms = d.delta_ms,
                                "detection retracted by a failure line"
                            );
                            let _ = app.emit(EV_DISCARDED, d);
                        }
                        for m in &tick.malformed {
                            // Loudly: this is how a game patch breaking the
                            // parser becomes visible instead of silent.
                            tracing::warn!(line = ?m, "class-matching line failed to parse");
                            let _ = app.emit(EV_MALFORMED, m);
                        }
                        for e in &tick.unmatched_errors {
                            tracing::warn!(result = %e.result, "failure line matched no request");
                        }

                        // See the module docs: emit first, then persist.
                        if tick.offset_advanced || !tick.promoted.is_empty() {
                            if let Err(e) = watchstate::save(watcher.state()) {
                                tracing::error!(error = %e, "could not persist state.json");
                            }
                        }
                        if !quiet {
                            *thread_status.lock().unwrap() = watcher.status().clone();
                            let _ = app.emit(EV_STATUS, watcher.status());
                        }
                    }
                    Err(e) => {
                        // A transient read failure — a share-mode conflict, a
                        // drive going away — must not kill the watcher.
                        tracing::warn!(error = %e, "poll failed; retrying");
                    }
                }
                std::thread::sleep(interval);
            }

            // Shutdown: anything still inside its window is promoted, not
            // dropped. The user can still reject it; losing it is worse.
            let final_tick = watcher.finish();
            for t in &final_tick.promoted {
                let _ = app.emit(EV_DETECTED, t);
            }
            if let Err(e) = watchstate::save(watcher.state()) {
                tracing::error!(error = %e, "could not persist state.json on shutdown");
            }
            *thread_status.lock().unwrap() = watcher.status().clone();
            let _ = app.emit(EV_STATUS, watcher.status());
            tracing::info!("watcher stopped");
        })
        .map_err(|e| AppError::Config(format!("cannot start the watcher thread: {e}")))?;

    Ok(WatcherHandle {
        stop,
        status,
        join: Some(join),
        path,
    })
}
