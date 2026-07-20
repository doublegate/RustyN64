//! The `run_until_complete` test-ROM runner.
//!
//! Steps a [`System`] until the suite signals completion.
//!
//! # The completion protocol
//!
//! Different suites signal differently, and the harness must not assume one.
//! What is implemented is **Dillon's `n64-tests` protocol**, which is the simplest
//! available and the reason `basic.z64` is the first target (T-11-006):
//!
//! > *"If at any point the value of r30 changes to a non-zero value, that means
//! > the tests have completed their run. If the value is -1, the tests passed!
//! > If the value is positive, that will tell you the test that failed."*
//! > — `ref-proj/n64-tests/README.md`
//!
//! **n64-systemtest is a different protocol entirely** and is not reachable yet:
//! it writes to no fixed address, emitting instead through emux COP0 hooks,
//! `ISViewer`, or SC64, and it cannot even reach its first test without COP0,
//! COP1 control and exception dispatch. That is T-11-009, deferred to Sprint 2.

use rustyn64_core::System;

/// The GPR Dillon's suite signals through.
const RESULT_REG: u8 = 30;

/// Outcome of a `run_until_complete` run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionStatus {
    /// The suite signalled success.
    Passed,
    /// The suite signalled failure, carrying the index of the failing test.
    Failed(u64),
    /// The step budget was exhausted before any completion signal.
    Timeout,
}

/// Step `system` until the completion signal fires or `max_ticks` elapse.
///
/// Polls once per CPU edge rather than once per master tick: the register can
/// only change when the CPU retires a write, so polling faster would be pure
/// overhead on the hot path.
#[must_use]
pub fn run_until_complete(system: &mut System, max_ticks: u64) -> CompletionStatus {
    let deadline = system.master_ticks().saturating_add(max_ticks);
    while system.master_ticks() < deadline {
        system.step_to_next_edge();
        match system.cpu.regs.read(RESULT_REG) {
            0 => {}
            // -1 as a 64-bit value. The suite writes it with ADDI, so it arrives
            // sign-extended; comparing against 0xFFFF_FFFF would miss it.
            u64::MAX => return CompletionStatus::Passed,
            n => return CompletionStatus::Failed(n),
        }
    }
    CompletionStatus::Timeout
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_idle_machine_times_out() {
        let mut sys = System::new(0);
        assert_eq!(run_until_complete(&mut sys, 64), CompletionStatus::Timeout);
    }

    /// The pass sentinel is `-1` as a full 64-bit value. Checking only the low 32
    /// bits would also match `0x0000_0000_FFFF_FFFF`, which is a *failure* index.
    #[test]
    fn the_pass_sentinel_is_sign_extended_minus_one() {
        let mut sys = System::new(0);
        sys.cpu.regs.write(RESULT_REG, u64::MAX);
        assert_eq!(run_until_complete(&mut sys, 64), CompletionStatus::Passed);

        let mut sys = System::new(0);
        sys.cpu.regs.write(RESULT_REG, 0xFFFF_FFFF);
        assert_eq!(
            run_until_complete(&mut sys, 64),
            CompletionStatus::Failed(0xFFFF_FFFF),
            "a 32-bit all-ones is a failure index, not the pass sentinel"
        );
    }

    #[test]
    fn a_positive_value_names_the_failing_test() {
        let mut sys = System::new(0);
        sys.cpu.regs.write(RESULT_REG, 3);
        assert_eq!(
            run_until_complete(&mut sys, 64),
            CompletionStatus::Failed(3)
        );
    }
}
