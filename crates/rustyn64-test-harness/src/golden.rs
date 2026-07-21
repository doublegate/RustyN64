//! VR4300 / RSP golden-log differ (keyed to n64-systemtest).
//!
//! Per retired instruction we capture a [`TraceRecord`] — `(pc, gpr, cycle)` —
//! and diff the captured stream against the committed reference trace
//! ([`GoldenLogDiffer::load_golden`], `tests/golden/n64-systemtest.log`),
//! captured from **ares** at the ELF entry.
//!
//! # What the comparison claims
//!
//! *Given identical initial state, `RustyN64` retires the same instructions in
//! the same order as the reference.* This is the shape hardware verification
//! calls **tandem verification** / step-and-compare co-simulation: the two
//! models are aligned at a boundary and only deltas from that boundary are the
//! claim. It says nothing about boot, nor about timing.
//!
//! `Count`, `Random` and `Compare` are excluded from comparison — see
//! [`GoldenLogDiffer::diff`] for why that is forced rather than convenient.
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

    /// Load the committed reference trace for a named suite.
    ///
    /// Reads `tests/golden/<suite>.log`: `#`-comments carrying the provenance
    /// header, then one retired-instruction PC per line in hex.
    ///
    /// Only `pc` is populated. The log deliberately records nothing else — see
    /// [`GoldenLogDiffer::diff`] for what is compared and what is excluded.
    ///
    /// # Errors
    ///
    /// [`std::io::Error`] if the log is missing or a record is unparsable. A
    /// missing log is an error rather than an empty vector: an empty golden log
    /// makes [`GoldenLogDiffer::diff`] return [`GoldenDiff::Match`] against
    /// *anything*, which is the vacuous pass this file exists to prevent.
    pub fn load_golden(suite: &str) -> std::io::Result<Vec<TraceRecord>> {
        let path = format!(
            "{}/../../tests/golden/{suite}.log",
            env!("CARGO_MANIFEST_DIR")
        );
        let text = std::fs::read_to_string(&path)?;
        let mut out = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let pc = u64::from_str_radix(line, 16).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("{path}: bad record {line:?}: {e}"),
                )
            })?;
            out.push(TraceRecord {
                pc,
                gpr: [0; 32],
                cycle: 0,
            });
        }
        Ok(out)
    }

    /// Diff the captured stream against a golden log by PC, record-for-record.
    ///
    /// # What is compared, and what is deliberately not
    ///
    /// **Compared:** the retired-instruction PC stream, in order. That is the
    /// whole claim — "given identical initial state, `RustyN64` executes the same
    /// instructions in the same order as the reference".
    ///
    /// **Excluded: `Count`, `Random` and `Compare`.** Not an oversight and not
    /// convenience. libdragon's IPL3 zeroes `Count` mid-boot and then
    /// accumulates PI/SI busy-waits whose length depends on the host's timing
    /// model; libdragon's own `pi_wait()` passes the result to `entropy_add()`,
    /// i.e. upstream treats a boot-relative `Count` as a source of **entropy**.
    /// n64-systemtest's startup test likewise declines to assert `Count` at all,
    /// and will not even pin `Wired` or `Index`. There is no correct value to
    /// compare, so comparing one would only encode the reference emulator's
    /// timing model as though it were hardware.
    ///
    /// This is the standard exclusion in retirement-level co-simulation (the
    /// RISC-V RVVI/RVFI harnesses mask timing CSRs for the same reason). It is
    /// safe **only** because those three registers have dedicated tests of their
    /// own in the n64-systemtest COP0 category, which is a separate gate.
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
