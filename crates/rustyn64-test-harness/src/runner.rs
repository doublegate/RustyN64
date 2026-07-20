//! The `run_until_complete` test-ROM runner.
//!
//! Steps a [`System`] until the suite signals completion (the n64-systemtest
//! result protocol writes a status word to a known RDRAM/IO location). The
//! completion decode is **STUBBED** — wire it to the real sentinel when the
//! first suite ROM is committed under `tests/roms/`.
//!
//! See `docs/testing-strategy.md` §test-ROM corpus.

use rustyn64_core::System;

/// Outcome of a `run_until_complete` run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionStatus {
    /// The suite signalled success.
    Passed,
    /// The suite signalled failure (carries the suite's result code).
    Failed(u32),
    /// The step budget was exhausted before any completion sentinel.
    Timeout,
}

/// Step `system` until the completion sentinel fires or `max_ticks` is reached.
///
/// STUB: the sentinel decode always reports [`CompletionStatus::Timeout`] right
/// now — there is no committed suite ROM to read a result word from yet. The
/// loop + budget are real so this is a drop-in gate once the decode lands.
#[must_use]
pub fn run_until_complete(system: &mut System, max_ticks: u64) -> CompletionStatus {
    for _ in 0..max_ticks {
        system.tick_one_unit();
        // TODO(T-HARNESS-02): poll the n64-systemtest result word (a known
        // RDRAM/IO address) and return Passed / Failed(code) when it is set.
    }
    CompletionStatus::Timeout
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_out_on_stub() {
        let mut sys = System::new(0);
        assert_eq!(run_until_complete(&mut sys, 16), CompletionStatus::Timeout);
    }
}
