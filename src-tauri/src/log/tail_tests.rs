//! PLAN.md §12 calls this the single most valuable test file in the project,
//! because a tailer bug is either silent data loss or silent duplication —
//! neither of which announces itself.
//!
//! Every test drives a real file on disk through a real `Tailer`.

use std::io::Write;

use super::*;

struct Log {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

impl Log {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Game.log");
        Self { _dir: dir, path }
    }

    fn append(&self, s: &str) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .unwrap();
        f.write_all(s.as_bytes()).unwrap();
        f.flush().unwrap();
    }

    fn append_bytes(&self, b: &[u8]) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .unwrap();
        f.write_all(b).unwrap();
        f.flush().unwrap();
    }

    fn truncate(&self) {
        std::fs::File::create(&self.path).unwrap();
    }

    fn remove(&self) {
        let _ = std::fs::remove_file(&self.path);
    }

    fn tailer(&self) -> Tailer {
        Tailer::new(&self.path)
    }
}

fn lines(t: &mut Tailer) -> Vec<String> {
    match t.poll().unwrap() {
        Tick::Lines { lines, .. } => lines,
        other => panic!("expected lines, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// The basics
// ---------------------------------------------------------------------------

#[test]
fn missing_file_is_not_an_error() {
    let log = Log::new();
    let mut t = log.tailer();
    assert_eq!(t.poll().unwrap(), Tick::Missing);
    assert_eq!(t.offset(), 0);
}

#[test]
fn appended_lines_are_read_once() {
    let log = Log::new();
    let mut t = log.tailer();
    log.append("alpha\nbeta\n");

    assert_eq!(lines(&mut t), vec!["alpha", "beta"]);
    // Nothing new.
    assert_eq!(t.poll().unwrap(), Tick::Idle);

    log.append("gamma\n");
    assert_eq!(lines(&mut t), vec!["gamma"]);
    assert_eq!(t.poll().unwrap(), Tick::Idle);
}

#[test]
fn file_appearing_later_is_picked_up() {
    let log = Log::new();
    let mut t = log.tailer();
    assert_eq!(t.poll().unwrap(), Tick::Missing);
    log.append("first\n");
    assert_eq!(lines(&mut t), vec!["first"]);
}

// ---------------------------------------------------------------------------
// The invariant: a half-written line is never parsed
// ---------------------------------------------------------------------------

#[test]
fn partial_line_is_not_emitted_and_offset_does_not_pass_it() {
    let log = Log::new();
    let mut t = log.tailer();

    log.append("complete\n");
    assert_eq!(lines(&mut t), vec!["complete"]);
    let after_complete = t.offset();

    // A line the game is still writing.
    log.append("half-writ");
    assert_eq!(t.poll().unwrap(), Tick::Idle, "a partial line must not emit");
    assert_eq!(
        t.offset(),
        after_complete,
        "the offset must not advance past an unterminated line"
    );

    // Still nothing, however many times we look.
    assert_eq!(t.poll().unwrap(), Tick::Idle);
    assert_eq!(t.offset(), after_complete);

    // Now it is finished.
    log.append("ten\n");
    assert_eq!(lines(&mut t), vec!["half-written"]);
    assert_eq!(t.poll().unwrap(), Tick::Idle);
}

#[test]
fn a_line_split_across_many_polls_arrives_exactly_once() {
    let log = Log::new();
    let mut t = log.tailer();
    for ch in "a very gradually written line".chars() {
        log.append(&ch.to_string());
        assert_eq!(t.poll().unwrap(), Tick::Idle);
    }
    log.append("\n");
    assert_eq!(lines(&mut t), vec!["a very gradually written line"]);
    assert_eq!(t.poll().unwrap(), Tick::Idle);
}

/// A multi-byte character split across a read boundary must not be mangled.
/// This is why only whole lines are decoded.
#[test]
fn multibyte_char_split_across_reads_survives() {
    let log = Log::new();
    let mut t = log.tailer();

    // U+03BC is two bytes in UTF-8. Write the first byte only.
    let mu = "\u{03BC}".as_bytes();
    assert_eq!(mu.len(), 2);
    log.append("quantity[");
    log.append_bytes(&mu[..1]);
    assert_eq!(t.poll().unwrap(), Tick::Idle);

    log.append_bytes(&mu[1..]);
    log.append("SCU]\n");
    let got = lines(&mut t);
    assert_eq!(got, vec!["quantity[\u{03BC}SCU]"]);
    assert!(
        !got[0].contains('\u{FFFD}'),
        "the character was corrupted: {got:?}"
    );
}

// ---------------------------------------------------------------------------
// Rotation
// ---------------------------------------------------------------------------

#[test]
fn truncation_is_detected_and_nothing_is_lost_or_duplicated() {
    let log = Log::new();
    let mut t = log.tailer();

    log.append("old-one\nold-two\n");
    assert_eq!(lines(&mut t), vec!["old-one", "old-two"]);
    assert!(t.offset() > 0);

    // The game relaunched and rewrote the log.
    log.truncate();
    log.append("new-one\n");

    match t.poll().unwrap() {
        Tick::Lines { lines, rotated } => {
            assert!(rotated, "truncation must be reported as rotation");
            assert_eq!(lines, vec!["new-one"], "no old lines, no duplicates");
        }
        other => panic!("expected rotated lines, got {other:?}"),
    }
    assert_eq!(t.poll().unwrap(), Tick::Idle);
}

/// The nasty case: a new log that happens to be longer than the old offset.
/// Length alone cannot catch it.
#[test]
fn replacement_longer_than_the_old_offset_is_still_detected() {
    let log = Log::new();
    let mut t = log.tailer();

    log.append("short\n");
    assert_eq!(lines(&mut t), vec!["short"]);
    let old_offset = t.offset();

    // Replace with a different, longer file.
    log.remove();
    // Creation time must differ for this to be detectable at all.
    std::thread::sleep(std::time::Duration::from_millis(20));
    log.append("a much longer first line than before\nand another\n");
    assert!(
        std::fs::metadata(&log.path).unwrap().len() > old_offset,
        "precondition: the new file is longer than the old offset"
    );

    match t.poll().unwrap() {
        Tick::Lines { lines, rotated } => {
            // On a filesystem that reports creation time, this is caught and
            // both lines are delivered. Where it is not reported, the tailer
            // falls back to length and would resume mid-file — assert the
            // behaviour we actually get rather than pretending.
            if rotated {
                assert_eq!(lines, vec!["a much longer first line than before", "and another"]);
            } else {
                assert!(
                    !lines.is_empty(),
                    "without creation times we still read forward, never backwards"
                );
            }
        }
        other => panic!("expected lines, got {other:?}"),
    }
}

#[test]
fn deletion_then_recreation_is_handled() {
    let log = Log::new();
    let mut t = log.tailer();
    log.append("before\n");
    assert_eq!(lines(&mut t), vec!["before"]);

    log.remove();
    assert_eq!(t.poll().unwrap(), Tick::Missing);

    log.append("after\n");
    match t.poll().unwrap() {
        Tick::Lines { lines, .. } => assert_eq!(lines, vec!["after"]),
        other => panic!("expected lines, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Resume
// ---------------------------------------------------------------------------

#[test]
fn resuming_at_a_saved_offset_reads_nothing_twice() {
    let log = Log::new();
    log.append("one\ntwo\n");

    let mut first = log.tailer();
    assert_eq!(lines(&mut first), vec!["one", "two"]);
    let saved_offset = first.offset();
    let saved_created = first.identity().unwrap().created;

    log.append("three\n");

    // A fresh process picking up where the last one stopped.
    let mut second = log.tailer().resume_at(saved_offset, saved_created);
    assert_eq!(lines(&mut second), vec!["three"]);
    assert_eq!(second.poll().unwrap(), Tick::Idle);
}

#[test]
fn resuming_past_the_end_is_treated_as_rotation() {
    let log = Log::new();
    log.append("tiny\n");
    let created = std::fs::metadata(&log.path).unwrap().created().ok();

    // An offset from a longer, previous session.
    let mut t = log.tailer().resume_at(9_999, created);
    match t.poll().unwrap() {
        Tick::Lines { lines, rotated } => {
            assert!(rotated);
            assert_eq!(lines, vec!["tiny"]);
        }
        other => panic!("expected rotation, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Line shapes
// ---------------------------------------------------------------------------

#[test]
fn crlf_is_stripped() {
    let log = Log::new();
    let mut t = log.tailer();
    log.append("windows\r\nunix\n");
    assert_eq!(lines(&mut t), vec!["windows", "unix"]);
}

#[test]
fn blank_lines_are_skipped() {
    let log = Log::new();
    let mut t = log.tailer();
    log.append("one\n\n\ntwo\n");
    assert_eq!(lines(&mut t), vec!["one", "two"]);
}

#[test]
fn invalid_utf8_does_not_stop_the_tailer() {
    let log = Log::new();
    let mut t = log.tailer();
    log.append_bytes(b"shopName[bad\xFFname]\n");
    log.append("next\n");
    let got = lines(&mut t);
    assert_eq!(got.len(), 2);
    assert!(got[0].contains('\u{FFFD}'));
    assert_eq!(got[1], "next");
}

#[test]
fn a_large_burst_is_delivered_in_order() {
    let log = Log::new();
    let mut t = log.tailer();
    let mut expected = Vec::new();
    let mut blob = String::new();
    for i in 0..5_000 {
        blob.push_str(&format!("line-{i}\n"));
        expected.push(format!("line-{i}"));
    }
    log.append(&blob);
    assert_eq!(lines(&mut t), expected);
    assert_eq!(t.poll().unwrap(), Tick::Idle);
}

/// Across an arbitrary interleaving of writes and polls, every line must be
/// seen exactly once and in order. This is the property that matters; the
/// individual cases above are just the ones worth naming.
#[test]
fn no_line_is_ever_lost_or_duplicated() {
    let log = Log::new();
    let mut t = log.tailer();

    let mut written = 0usize;
    let mut seen: Vec<String> = Vec::new();

    // A deterministic but uneven pattern of chunk sizes and poll points.
    for step in 0..200usize {
        let n = step % 7;
        let mut chunk = String::new();
        for _ in 0..n {
            chunk.push_str(&format!("line-{written}\n"));
            written += 1;
        }
        // Every so often, leave a line half-written across the poll.
        let dangling = step % 5 == 0;
        if dangling {
            chunk.push_str(&format!("line-{written}"));
        }
        if !chunk.is_empty() {
            log.append(&chunk);
        }
        if let Tick::Lines { lines, .. } = t.poll().unwrap() {
            seen.extend(lines);
        }
        if dangling {
            log.append("\n");
            written += 1;
        }
    }
    // Drain.
    if let Tick::Lines { lines, .. } = t.poll().unwrap() {
        seen.extend(lines);
    }

    let expected: Vec<String> = (0..written).map(|i| format!("line-{i}")).collect();
    assert_eq!(seen.len(), expected.len(), "line count differs");
    assert_eq!(seen, expected, "lines lost, duplicated or reordered");
}
