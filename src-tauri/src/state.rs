//! Process-wide state handed to Tauri via `.manage()`.

use std::sync::{Mutex, RwLock};

use crate::config::Config;
use crate::queue::Queue;
use crate::refdata::RefData;
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
    /// The review queue — the single source of truth. The frontend holds only
    /// a projection of this and never mutates it.
    ///
    /// A `std::sync::Mutex`, not Tokio's: every operation on it is a short
    /// in-memory edit plus one atomic file write, with no `.await` inside. The
    /// corollary is that the guard must never be held across an await — the
    /// write drain scopes each lock deliberately for that reason.
    pub queue: Mutex<Queue>,
    /// Reference data. Read on every render of the review list and written only
    /// by a refresh, so a read-write lock is the right shape.
    pub refdata: RwLock<RefData>,
}

impl AppState {
    pub fn new(config: Config, queue: Queue, refdata: RefData) -> Self {
        Self {
            config: Mutex::new(config),
            watcher: Mutex::new(None),
            queue: Mutex::new(queue),
            refdata: RwLock::new(refdata),
        }
    }

    pub fn config(&self) -> Config {
        self.config.lock().unwrap().clone()
    }

    pub fn refdata(&self) -> RefData {
        self.refdata.read().unwrap().clone()
    }
}
