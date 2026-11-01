//! Process-wide state handed to Tauri via `.manage()`.

use std::sync::Mutex;

use crate::config::Config;
use crate::watcher_task::WatcherHandle;

pub struct AppState {
    /// The live config. Loaded once at startup, mutated only through
    /// `set_config`, which also persists it.
    pub config: Mutex<Config>,
    /// The running watcher, if any. `None` means not watching — which is the
    /// state the app starts in: nothing is read until the user says so, since
    /// a first run must not silently import a session.
    ///
    /// Only one may exist. Two watchers would detect every trade twice, which
    /// is also why `single-instance` is a plugin rather than a nicety.
    pub watcher: Mutex<Option<WatcherHandle>>,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        Self {
            config: Mutex::new(config),
            watcher: Mutex::new(None),
        }
    }

    pub fn config(&self) -> Config {
        self.config.lock().unwrap().clone()
    }
}
