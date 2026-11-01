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
/// **Creation time cannot do this job, and the reason is worth knowing.** NTFS
/// has a behaviour called *file-system tunneling*: delete or rename a file and
/// recreate one with the same name in the same directory within ~15 seconds,
/// and the new file inherits the old one's creation timestamp. That is exactly
/// what the game does on every launch — move `Game.log` into `logbackups/`,
/// create a fresh one — so the timestamp is resurrected every time. Verified on
/// the real install: a `Game.log` created minutes earlier reported a
/// CreationTime of **May 2021**, frozen there across years of rotations.
///
/// The NTFS file reference number would work, but `Metadata::file_index` is
/// still unstable, and reaching for `windows-sys` means picking one of the
/// three conflicting versions already in the tree.
///
/// So the discriminator is the log's **own first line**, which the game writes
/// before anything else:
///
/// ```text
/// <2026-11-01T17:29:49.066Z> BackupNameAttachment=" Build(12913470) 01 Nov 26 (18 29 39)"  -- used by backup system
/// ```
///
/// A millisecond-resolution session start plus the exact name the file will be
/// archived under. Two sessions cannot share it, it is immutable while the file
/// is appended to, and it is legible in `state.json` — which a file index would
/// not be. Creation time is kept only as a fallback for a file with no usable
/// first line, and length as the last resort.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileIdentity {
    /// The first line, verbatim and without its terminator. `None` when the
    /// file is empty or its first line has not been terminated yet.
    pub header: Option<String>,
    /// Unreliable for `Game.log` — see above. Fallback only.
    pub created: Option<SystemTime>,
}

/// A first line longer than this is pathological; treat it as no header rather
/// than storing an unbounded string.
const MAX_HEADER_BYTES: usize = 512;

impl FileIdentity {
    /// Read the identity from an already-open handle. Leaves the cursor
    /// wherever it likes — callers seek before reading content.
    fn of(f: &mut File, meta: &std::fs::Metadata) -> std::io::Result<Self> {
        Ok(Self {
            header: read_header(f)?,
            created: meta.created().ok(),
        })
    }

    /// Is this a *different* file from `prev`?
    ///
    /// `None` means the question cannot be answered from identity alone and the
    /// caller must fall back to comparing length. Deliberately three-valued:
    /// collapsing "same file" and "cannot tell" into one boolean is precisely
    /// how the creation-time bug stayed invisible.
    pub fn differs_from(&self, prev: &Self) -> Option<bool> {
        if let (Some(a), Some(b)) = (prev.header.as_deref(), self.header.as_deref()) {
            return Some(a != b);
        }
        if let (Some(a), Some(b)) = (prev.created, self.created) {
            return Some(a != b);
        }
        None
    }
}

/// The first line of `f`, if it has one yet.
fn read_header(f: &mut File) -> std::io::Result<Option<String>> {
    f.seek(SeekFrom::Start(0))?;
    let mut buf = vec![0u8; MAX_HEADER_BYTES];
    let n = read_up_to(f, &mut buf)?;
    let buf = &buf[..n];
    // Only a *terminated* line counts: a partial one would change under us as
    // the game finishes writing it, and look like a rotation.
    let Some(nl) = buf.iter().position(|b| *b == b'\n') else {
        return Ok(None);
    };
    let line = &buf[..nl];
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    Ok(Some(String::from_utf8_lossy(line).into_owned()))
}

/// `Read::read` may return fewer bytes than asked for without being at EOF.
fn read_up_to(f: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match f.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// Open for reading with explicit Windows share flags.
///
/// `std` only, no crate, and it removes any chance of
/// ERROR_SHARING_VIOLATION against the handle CryEngine holds open. CryEngine's
/// exact share mode is unverified, so writing it this way from the start is
/// free insurance.
pub(crate) fn open_shared(path: &Path) -> std::io::Result<File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x1;
        const FILE_SHARE_WRITE: u32 = 0x2;
        const FILE_SHARE_DELETE: u32 = 0x4;
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(path)
    }
    #[cfg(not(windows))]
    {
        File::open(path)
    }
}

/// The identity of whatever is at `path` right now, if anything.
pub fn identity_of(path: &Path) -> Option<FileIdentity> {
    let mut f = open_shared(path).ok()?;
    let meta = f.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    FileIdentity::of(&mut f, &meta).ok()
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
    ///
    /// `identity` is what the mark was recorded against. If it no longer
    /// describes the file at this path, the first poll reports a rotation and
    /// starts over — which is the whole point of persisting it.
    pub fn resume_at(mut self, offset: u64, identity: FileIdentity) -> Self {
        self.offset = offset;
        self.identity = Some(identity);
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

    /// One poll.
    pub fn poll(&mut self) -> std::io::Result<Tick> {
        // Opened rather than stat-ed: a file index is only obtainable from a
        // handle, and it is the only sound way to notice a rotation. See
        // `FileIdentity`.
        let mut f = match open_shared(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Tick::Missing),
            Err(e) => return Err(e),
        };
        let meta = f.metadata()?;
        if !meta.is_file() {
            return Ok(Tick::Missing);
        }

        let len = meta.len();
        let identity = FileIdentity::of(&mut f, &meta)?;

        // Truncation, or a different file at the same path. Either way,
        // everything we knew about position is meaningless.
        let replaced = match &self.identity {
            Some(prev) => match identity.differs_from(prev) {
                // A different file. Length proves nothing here — a new log can
                // already be longer than our old offset.
                Some(true) => true,
                // Same file, so only shrinkage can mean it was rewritten.
                Some(false) => len < self.offset,
                // Identity is unavailable; length is all we have left.
                None => len < self.offset,
            },
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
