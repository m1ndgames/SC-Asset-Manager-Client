//! Finding the Star Citizen install and its Game.log.
//!
//! The RSI launcher's `%APPDATA%\rsilauncher\launcher store.json` is
//! encrypted/binary, not readable JSON — verified — so it cannot be used to
//! locate the library. Resolution order is therefore: config override, then a
//! fixed-drive scan, then Program Files.
//!
//! A bounded number of `fs::metadata` calls; a drive whose root does not stat
//! is skipped. Never run this on the UI thread.

use std::path::{Path, PathBuf};

/// Where a LIVE install keeps its log, relative to a drive root.
const SUFFIX: &str = "Roberts Space Industries/StarCitizen/LIVE/Game.log";

/// Drive letters worth probing. Deliberately fixed and bounded: enumerating
/// network and removable drives can block for seconds apiece.
const DRIVES: &[char] = &[
    'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R', 'S', 'T', 'U',
    'V', 'W', 'X', 'Y', 'Z',
];

/// Where the game archives the previous session's log on each launch.
///
/// Rotation preserves rather than destroys: the live `Game.log` is moved here
/// when the game next starts. Useful for backfill, and the reason a missed
/// session is recoverable.
pub fn logbackups_dir(game_log: &Path) -> Option<PathBuf> {
    Some(game_log.parent()?.join("logbackups"))
}

/// All plausible Game.log locations, best first.
pub fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();

    for d in DRIVES {
        let root = PathBuf::from(format!("{d}:\\"));
        // Skip a drive that does not exist; this is the cheap gate.
        if std::fs::metadata(&root).is_err() {
            continue;
        }
        let p = root.join(SUFFIX.replace('/', "\\"));
        if p.is_file() {
            out.push(p);
        }
    }

    for var in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Ok(base) = std::env::var(var) {
            let p = PathBuf::from(base).join(SUFFIX.replace('/', "\\"));
            if p.is_file() && !out.contains(&p) {
                out.push(p);
            }
        }
    }

    out
}

/// The first plausible Game.log, or `None`.
pub fn detect() -> Option<PathBuf> {
    candidates().into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Discovery must never panic or hang, whatever this machine looks like.
    #[test]
    fn candidates_is_safe_to_call() {
        let found = candidates();
        for p in &found {
            assert!(p.is_file());
            assert!(p.ends_with("Game.log"));
        }
    }

    #[test]
    fn logbackups_sits_beside_the_log() {
        let p = Path::new("E:\\Roberts Space Industries\\StarCitizen\\LIVE\\Game.log");
        let b = logbackups_dir(p).unwrap();
        assert!(b.ends_with("logbackups"));
        assert_eq!(b.parent(), p.parent());
    }
}
