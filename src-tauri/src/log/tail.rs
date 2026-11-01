//! Polling tailer for a file the game holds open and rewrites without warning.
//!
//! **Polling, not `notify`.** CryEngine appends with buffering. `notify` on
//! Windows wraps `ReadDirectoryChangesW`, which coalesces under load, can miss
//! events, needs its own debounce, and says *nothing* about truncation — we
//! would still have to stat and compare length. The latency budget is seconds
//! anyway, because every detection is held for a grace period. One
//! `fs::metadata` plus a seek-read handles append, truncation, deletion,
//! replacement and "file does not exist yet" with the same few branches.
//!
//! ## The invariant
//!
//! `offset` always points at a **line boundary**. A read advances it only as
//! far as the last `\n` actually seen; anything after that is simply not
//! consumed, and is re-read on the next tick. A half-written line is therefore
//! never parsed, and never half-parsed twice.
//!
//! PLAN.md §3.3 describes keeping the remainder in a `partial` buffer *and*
//! advancing only to the last newline. Those are two solutions to the same
//! problem and only one is needed. Not buffering is the safer half: `offset`
//! alone is sufficient state, so it is also sufficient to persist, and a crash
//! cannot leave a buffer and an offset disagreeing with each other.
//!
//! It also avoids a bug the buffered version invites. The remainder has to be
//! carried as **bytes**, not as a `String`: a multi-byte character split
//! across a read boundary would be mangled by a lossy decode on the way in and
//! could never be repaired. Here, only whole lines are ever decoded.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// How we tell "the same file, longer" from "a different file".
///
/// Creation time is the discriminator that matters: the game archives the old
/// log into `logbackups/` and starts a new one, which can easily be longer
/// than the old offset by the time we next look.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileIdentity {
    pub created: Option<SystemTime>,
}

impl FileIdentity {
    fn of(meta: &std::fs::Metadata) -> Self {
        Self {
            created: meta.created().ok(),
        }
    }
}

/// What one poll produced.
#[derive(Debug, Clone, PartialEq)]
pub enum Tick {
    /// No file at that path. Normal — the game has not launched.
    Missing,
    /// Nothing new since last time.
    Idle,
    /// Whole lines, in order.
    Lines {
        lines: Vec<String>,
        /// The file was truncated or replaced; everything before this came
        /// from a previous session.
        rotated: bool,
    },
}

#[derive(Debug)]
pub struct Tailer {
    path: PathBuf,
    identity: Option<FileIdentity>,
    /// Always a line boundary. See the module docs.
    offset: u64,
    /// Guards against an unterminated line growing without bound — a line this
    /// long means the format broke, not that the game is mid-write.
    max_line_bytes: usize,
}

/// 1 MiB. The longest real line observed is a few hundred bytes.
const DEFAULT_MAX_LINE: usize = 1024 * 1024;

impl Tailer {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            identity: None,
            offset: 0,
            max_line_bytes: DEFAULT_MAX_LINE,
        }
    }

    /// Resume from a persisted high-water mark.
    pub fn resume_at(mut self, offset: u64, created: Option<SystemTime>) -> Self {
        self.offset = offset;
        self.identity = Some(FileIdentity { created });
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn identity(&self) -> Option<&FileIdentity> {
        self.identity.as_ref()
    }

    /// Open with explicit Windows share flags.
    ///
    /// This is `std`, no crate, and it removes any chance of
    /// ERROR_SHARING_VIOLATION against the handle CryEngine holds open.
    /// CryEngine's exact share mode is unverified, so writing it this way from
    /// the start is free insurance.
    fn open(&self) -> std::io::Result<File> {
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_SHARE_READ: u32 = 0x1;
            const FILE_SHARE_WRITE: u32 = 0x2;
            const FILE_SHARE_DELETE: u32 = 0x4;
            std::fs::OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                .open(&self.path)
        }
        #[cfg(not(windows))]
        {
            File::open(&self.path)
        }
    }

    /// One poll.
    pub fn poll(&mut self) -> std::io::Result<Tick> {
        let meta = match std::fs::metadata(&self.path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Tick::Missing),
            Err(e) => return Err(e),
        };
        if !meta.is_file() {
            return Ok(Tick::Missing);
        }

        let len = meta.len();
        let identity = FileIdentity::of(&meta);

        // Truncation, or a different file at the same path. Either way,
        // everything we knew about position is meaningless.
        let replaced = match &self.identity {
            Some(prev) => {
                // Only treat a creation-time change as replacement when we
                // actually have both timestamps; some filesystems do not
                // report one.
                let created_changed = match (prev.created, identity.created) {
                    (Some(a), Some(b)) => a != b,
                    _ => false,
                };
                created_changed || len < self.offset
            }
            None => false,
        };

        let rotated = replaced;
        if replaced {
            self.offset = 0;
        }
        self.identity = Some(identity);

        if len == self.offset {
            return Ok(if rotated {
                Tick::Lines {
                    lines: Vec::new(),
                    rotated: true,
                }
            } else {
                Tick::Idle
            });
        }

        let mut f = self.open()?;
        f.seek(SeekFrom::Start(self.offset))?;
        let mut buf = Vec::with_capacity((len - self.offset).min(8 * 1024 * 1024) as usize);
        f.take(len - self.offset).read_to_end(&mut buf)?;

        // Consume only as far as the last newline. Everything after it is a
        // line still being written; leaving the offset before it means we
        // re-read it next tick, whole.
        let Some(last_nl) = buf.iter().rposition(|b| *b == b'\n') else {
            if buf.len() > self.max_line_bytes {
                // Not a partial line any more — something is wrong. Skip it so
                // the tailer cannot wedge forever on one pathological line.
                tracing::warn!(
                    bytes = buf.len(),
                    path = %self.path.display(),
                    "no newline in an oversized read; skipping forward"
                );
                self.offset += buf.len() as u64;
            }
            return Ok(if rotated {
                Tick::Lines {
                    lines: Vec::new(),
                    rotated: true,
                }
            } else {
                Tick::Idle
            });
        };

        let consumed = last_nl + 1;
        let complete = &buf[..consumed];
        self.offset += consumed as u64;

        // Only whole lines are decoded, so a multi-byte character can never be
        // split by a read boundary. `from_utf8_lossy` still guards against a
        // genuinely bad byte inside an entity name.
        let lines: Vec<String> = complete
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .map(|l| {
                let l = l.strip_suffix(b"\r").unwrap_or(l);
                String::from_utf8_lossy(l).into_owned()
            })
            .collect();

        Ok(Tick::Lines { lines, rotated })
    }
}

#[cfg(test)]
#[path = "tail_tests.rs"]
mod tests;
