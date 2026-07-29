//! The accuracy scorer over a first-party probe battery.
//!
//! The N64 has no single all-in-one oracle ROM the way the NES has `AccuracyCoin`,
//! so this battery is assembled from named probes rather than wrapping one suite.
//!
//! Runs a battery of named accuracy probes and tallies a pass score. Unlike
//! `RustyNES`, N64 boards are NOT tiered, so there is no honesty-gate concept
//! here.
//!
//! **Every probe's expected value comes from an external oracle, never from
//! `RustyN64`'s own output.** The default battery
//! ([`AccuracyScorer::default_battery`]) scores each committed Angrylion RDP
//! conformance vector ([`crate::conformance::RDP_VECTORS`]): the expected
//! framebuffer in each `.rvec` was rendered by Angrylion, so a probe passes only
//! when `RustyN64` reproduces the oracle byte-for-byte. The battery grows as further
//! oracle-backed suites come online.
//!
//! See `docs/testing-strategy.md` §accuracy battery.

/// The pass/fail result of one named accuracy probe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeResult {
    /// Probe name (stable identifier, e.g. `n64-systemtest/COP1/CVT_W_S`).
    pub name: String,
    /// Whether the probe passed.
    pub passed: bool,
}

/// A tally over a probe battery.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccuracyReport {
    /// Per-probe results.
    pub probes: Vec<ProbeResult>,
}

impl AccuracyReport {
    /// Number of probes that passed.
    #[must_use]
    pub fn passed(&self) -> usize {
        self.probes.iter().filter(|p| p.passed).count()
    }

    /// Total probe count.
    #[must_use]
    pub fn total(&self) -> usize {
        self.probes.len()
    }

    /// Pass ratio in `0.0..=1.0`.
    ///
    /// An **empty** battery scores `0.0`, not `1.0`: a battery that ran nothing has
    /// demonstrated nothing, and reporting it as fully green is precisely the
    /// vacuous pass this project treats as a defect (`docs/engineering-lessons.md`
    /// — a sentinel set by code that never ran the tests). Callers that want to
    /// distinguish "empty" from "all failed" should check [`Self::total`].
    #[must_use]
    pub fn ratio(&self) -> f64 {
        if self.probes.is_empty() {
            0.0
        } else {
            self.passed() as f64 / self.total() as f64
        }
    }

    /// Names of the probes that failed, in battery order.
    #[must_use]
    pub fn failures(&self) -> Vec<&str> {
        self.probes
            .iter()
            .filter(|p| !p.passed)
            .map(|p| p.name.as_str())
            .collect()
    }
}

/// Runs the accuracy battery and produces an [`AccuracyReport`].
#[derive(Debug, Default)]
pub struct AccuracyScorer {
    probes: Vec<ProbeResult>,
}

impl AccuracyScorer {
    /// New empty scorer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a probe outcome.
    pub fn record(&mut self, name: impl Into<String>, passed: bool) {
        self.probes.push(ProbeResult {
            name: name.into(),
            passed,
        });
    }

    /// Finalize into a report.
    #[must_use]
    pub fn finish(self) -> AccuracyReport {
        AccuracyReport {
            probes: self.probes,
        }
    }

    /// The default probe battery: every committed Angrylion RDP conformance
    /// vector, replayed through `RustyN64`'s RDP and scored against the oracle's
    /// golden framebuffer.
    ///
    /// Each probe is named `rdp-conformance/<vector>`. A probe passes only on a
    /// byte-for-byte match with Angrylion's output, so the score is an
    /// externally-defined measurement rather than a self-assessment.
    ///
    /// This is the battery `docs/STATUS.md` reports. It grows as further
    /// oracle-backed suites (the VI `.vivec` set, the n64-systemtest categories)
    /// are wired in.
    #[must_use]
    pub fn default_battery() -> Self {
        let mut scorer = Self::new();
        for (name, bytes) in crate::conformance::RDP_VECTORS {
            let passed = crate::conformance::first_mismatch(bytes).is_none();
            scorer.record(format!("rdp-conformance/{name}"), passed);
        }
        scorer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **An empty battery is NOT green.** A battery that ran nothing has proven
    /// nothing; scoring it 1.0 would be a vacuous pass.
    #[test]
    fn empty_battery_scores_zero_not_one() {
        let report = AccuracyScorer::new().finish();
        assert_eq!(report.total(), 0);
        assert!(
            report.ratio().abs() < f64::EPSILON,
            "an empty battery must not report a passing ratio"
        );
    }

    /// **The default battery actually runs the committed oracle vectors.** It must
    /// be non-empty (an empty battery would silently report nothing) and every
    /// probe must be named for the vector it scored.
    #[test]
    fn default_battery_scores_every_committed_vector() {
        let report = AccuracyScorer::default_battery().finish();
        assert_eq!(
            report.total(),
            crate::conformance::RDP_VECTORS.len(),
            "one probe per committed vector"
        );
        assert!(report.total() >= 35, "battery unexpectedly small");
        assert!(
            report
                .probes
                .iter()
                .all(|p| p.name.starts_with("rdp-conformance/")),
            "probes are named for their oracle suite"
        );
    }

    /// **The battery is green against the Angrylion oracle.** This is the accuracy
    /// number `docs/STATUS.md` reports; a regression in any RDP path fails here
    /// with the offending vector named.
    #[test]
    fn default_battery_matches_the_oracle() {
        let report = AccuracyScorer::default_battery().finish();
        assert!(
            report.failures().is_empty(),
            "vectors diverged from the Angrylion oracle: {:?}",
            report.failures()
        );
        assert!((report.ratio() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn tallies_mixed_results() {
        let mut scorer = AccuracyScorer::new();
        scorer.record("a", true);
        scorer.record("b", false);
        let report = scorer.finish();
        assert_eq!(report.passed(), 1);
        assert_eq!(report.total(), 2);
    }
}
