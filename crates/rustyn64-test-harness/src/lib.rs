//! `rustyn64-test-harness` — the accuracy gate (`RustyNES` `test-harness` shape).
//!
//! Reuses the `RustyNES` harness SHAPE (don't reinvent), retargeted to the N64:
//!
//! 1. [`golden`] — a VR4300/RSP golden-log differ keyed to **n64-systemtest**:
//!    capture `(pc, gpr, cycle)` per retired instruction and diff against a
//!    golden log (the golden source is STUBBED until a reference trace lands).
//! 2. [`runner`] — [`run_until_complete`], which steps a [`System`] until a
//!    completion sentinel (the n64-systemtest result protocol).
//! 3. [`accuracy`] — a first-party probe scorer over a battery of
//!    named probes (the battery itself is STUBBED).
//! 4. [`frame`] — a `.snap` / screenshot frame-hash comparator for the visual
//!    golden corpus (the hash is real; the corpus loader is a frontend job).
//!
//! N64 boards are **NOT tiered** (`boards_tiered = false`), so — unlike the
//! `RustyNES` `mapper_tier_honesty` test — there is no honesty-gate here.
//!
//! See `docs/testing-strategy.md`. Feature flags: `test-roms` (committed
//! CC0/public-domain suites + integration tests) and `commercial-roms` (the
//! local-dump oracle; ROMs are gitignored, only screenshots/`.snap` committed).

#![warn(missing_docs)]
// Harness builders/scorers are deliberately non-`const` (they grow real,
// allocation-driven bodies as suites land); accept the pedantic suggestion here.
#![allow(clippy::missing_const_for_fn)]
// The accuracy ratio is a presentation float; sub-mantissa precision loss on the
// probe count is irrelevant for a 0..1 score.
#![allow(clippy::cast_precision_loss)]

pub mod accuracy;
pub mod frame;
pub mod golden;
pub mod rom;
pub mod runner;

pub use accuracy::{AccuracyReport, AccuracyScorer, ProbeResult};
pub use frame::{FrameComparison, frame_hash};
pub use golden::{GoldenDiff, GoldenLogDiffer, TraceRecord};
pub use runner::{CompletionStatus, run_until_complete};

use rustyn64_core::System;
use rustyn64_core::cpu::Cpu;

/// Returns the crate version string.
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Convenience: build a fresh power-on [`System`] with a fixed determinism seed
/// (so harness runs are reproducible).
#[must_use]
pub fn system_for_tests(seed: u64) -> System {
    System::new(seed)
}

/// Convenience: a bare `Cpu` in an automation start state for the golden-log
/// differ (PC set, cycle counter primed). N64 analog of `RustyNES`'s
/// `cpu_for_nestest`.
#[must_use]
pub fn cpu_for_golden(entry_pc: u64) -> Cpu {
    let mut cpu = Cpu::new();
    cpu.set_pc(entry_pc);
    cpu
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_non_empty() {
        assert!(!version().is_empty());
    }

    #[test]
    fn builds_a_system() {
        let _sys = system_for_tests(0);
    }
}

#[cfg(test)]
mod feature_probe {
    /// Proves the `test-roms` feature actually reached this crate.
    ///
    /// This exists because a sibling project hit a Cargo feature-unification
    /// quirk where `cargo test --workspace --features test-roms` exited 0 while
    /// silently never enabling the feature for its harness crate — so the entire
    /// ROM-oracle gate ran zero tests and still reported success for an extended
    /// period.
    ///
    /// The failure mode is invisibility, not breakage: an oracle that runs
    /// nothing looks exactly like an oracle that passes. This test is named
    /// distinctively so CI can assert it actually executed
    /// (`.github/workflows/ci.yml`, the `test-roms` job), turning a silent skip
    /// into a hard failure.
    ///
    /// Verified on this workspace: `--workspace --features test-roms` DOES
    /// enable it here, and a bare `--workspace` does not.
    #[test]
    #[cfg(feature = "test-roms")]
    fn probe_test_roms_feature_is_enabled() {}
}
