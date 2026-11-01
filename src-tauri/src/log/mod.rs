//! Reading and understanding Game.log.
//!
//! The pipeline is deliberately split so the hard parts stay pure:
//!   `parse`    — text in, structure out. No I/O, no clock.
//!   `scan`     — a whole-file pass, for backfill and diagnostics.
//!   `discover` — finding the install.
//!   `tail`     — the polling loop, and the offset invariant.
//! Phase 2 adds `correlate` (the live grace window); `scan` is its offline
//! equivalent.

pub mod correlate;
pub mod discover;
pub mod parse;
pub mod scan;
pub mod tail;
pub mod watcher;
