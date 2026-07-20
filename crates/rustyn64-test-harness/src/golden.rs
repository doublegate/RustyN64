//! VR4300 / RSP golden-log differ (keyed to n64-systemtest).
//!
//! Per retired instruction we capture a [`TraceRecord`] — `(pc, gpr, cycle)` —
//! and diff the captured stream against a golden log. The golden source itself
//! is **STUBBED** ([`GoldenLogDiffer::load_golden_stub`]) until a reference
//! trace (e.g. a cen64/ares dump of n64-systemtest) is committed.
//!
//! See `docs/testing-strategy.md` §golden-log compare.

use rustyn64_core::cpu::Cpu;

/// One captured architectural-state record at instruction retire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceRecord {
    /// Program counter of the retired instruction.
    pub pc: u64,
    /// General-purpose register file snapshot after retire.
    pub gpr: [u64; 32],
    /// Cycle counter after retire.
    pub cycle: u64,
}

impl TraceRecord {
    /// Capture the current CPU state into a record.
    #[must_use]
    pub fn capture(cpu: &Cpu) -> Self {
        Self {
            pc: cpu.pc,
            gpr: cpu.regs.gpr,
            cycle: cpu.retired,
        }
    }
}

/// The result of diffing a captured stream against the golden log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoldenDiff {
    /// Captured stream matched the golden log for all compared records.
    Match,
    /// First divergence: the record index, and the expected vs. captured PC.
    Diverged {
        /// Index of the first mismatching record.
        index: usize,
        /// Golden-log PC at that index.
        expected_pc: u64,
        /// Captured PC at that index.
        actual_pc: u64,
    },
}

/// Captures a per-instruction trace and diffs it against a golden log.
#[derive(Debug, Default)]
pub struct GoldenLogDiffer {
    captured: Vec<TraceRecord>,
}

impl GoldenLogDiffer {
    /// New empty differ.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a captured record.
    pub fn push(&mut self, record: TraceRecord) {
        self.captured.push(record);
    }

    /// The captured records so far.
    #[must_use]
    pub fn records(&self) -> &[TraceRecord] {
        &self.captured
    }

    /// Load the reference golden log for a named suite.
    ///
    /// STUB: returns an empty log. Wire this to a committed reference trace
    /// (n64-systemtest captured on cen64/ares) when one lands; the diff then
    /// becomes a real regression gate.
    #[must_use]
    pub fn load_golden_stub(_suite: &str) -> Vec<TraceRecord> {
        // TODO(T-HARNESS-01): load `tests/golden/<suite>.log` once a reference
        // VR4300 trace is committed.
        Vec::new()
    }

    /// Diff the captured stream against a golden log by PC, record-for-record.
    #[must_use]
    pub fn diff(&self, golden: &[TraceRecord]) -> GoldenDiff {
        for (i, (g, a)) in golden.iter().zip(self.captured.iter()).enumerate() {
            if g.pc != a.pc {
                return GoldenDiff::Diverged {
                    index: i,
                    expected_pc: g.pc,
                    actual_pc: a.pc,
                };
            }
        }
        GoldenDiff::Match
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_streams_match() {
        let differ = GoldenLogDiffer::new();
        assert_eq!(differ.diff(&[]), GoldenDiff::Match);
    }

    #[test]
    fn detects_pc_divergence() {
        let mut differ = GoldenLogDiffer::new();
        differ.push(TraceRecord {
            pc: 0x100,
            gpr: [0; 32],
            cycle: 1,
        });
        let golden = [TraceRecord {
            pc: 0x200,
            gpr: [0; 32],
            cycle: 1,
        }];
        assert!(matches!(differ.diff(&golden), GoldenDiff::Diverged { .. }));
    }
}
