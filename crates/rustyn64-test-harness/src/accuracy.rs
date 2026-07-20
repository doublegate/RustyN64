//! The accuracy scorer over a first-party probe battery.
//!
//! The N64 has no single all-in-one oracle ROM the way the NES has AccuracyCoin,
//! so this battery is assembled from named probes rather than wrapping one suite.
//!
//! Runs a battery of named accuracy probes and tallies a pass score. The
//! battery itself is **STUBBED** ([`AccuracyScorer::default_battery_stub`]) —
//! it grows as suites under `tests/roms/` come online. Unlike `RustyNES`, N64
//! boards are NOT tiered, so there is no honesty-gate concept here.
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

    /// Pass ratio in `0.0..=1.0` (1.0 for an empty battery — vacuously green).
    #[must_use]
    pub fn ratio(&self) -> f64 {
        if self.probes.is_empty() {
            1.0
        } else {
            self.passed() as f64 / self.total() as f64
        }
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

    /// The default probe battery.
    ///
    /// STUB: empty until suites land. The wiring (a list of probe ROMs + their
    /// expected result words run through [`super::run_until_complete`]) is a
    /// roadmap TODO.
    #[must_use]
    pub fn default_battery_stub() -> Self {
        // TODO(T-HARNESS-03): populate from the committed accuracy suites.
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_battery_is_vacuously_green() {
        let report = AccuracyScorer::default_battery_stub().finish();
        assert_eq!(report.total(), 0);
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
