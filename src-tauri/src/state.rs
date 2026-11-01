//! Process-wide state handed to Tauri via `.manage()`.

use std::sync::Mutex;

use crate::config::Config;

pub struct AppState {
    /// The live config. Loaded once at startup, mutated only through
    /// `set_config`, which also persists it.
    pub config: Mutex<Config>,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        Self {
            config: Mutex::new(config),
        }
    }

    pub fn config(&self) -> Config {
        self.config.lock().unwrap().clone()
    }
}
