//! Reading and understanding Game.log.
//!
//! The pipeline is deliberately split so the hard parts stay pure:
//!   `parse`    — text in, structure out. No I/O, no clock.
//!   `scan`     — a whole-file pass, for backfill and diagnostics.
//!   `discover` — finding the install.
//! Phase 2 adds `tail` (the polling loop) and `correlate` (the live grace
//! window); `scan` is their offline equivalent.

pub mod discover;
pub mod parse;
pub mod scan;
