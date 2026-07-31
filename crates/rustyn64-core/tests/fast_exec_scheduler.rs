//! Scheduler-level tests for `fast-exec` (ADR 0013).
//!
//! The CPU crate's `fast_exec_differential` grades what an instruction *computes*.
//! Nothing there can reach `System::run_until_exec`, which is where the mode's
//! **scheduling** lives: the RCP catching up over an elapsed span, the overshoot
//! past `target`, and the halted-CPU branch. This file is that half — added after
//! review pointed out that CI linted the wiring without executing it.
//!
//! Runs only with `--features fast-exec`; the file is empty otherwise.

#![cfg(feature = "fast-exec")]

use rustyn64_core::scheduler::{CPU_DIVIDER, RCP_DIVIDER, System};
use rustyn64_core::vi::{VI_H_TOTAL, VI_V_CURRENT, VI_V_TOTAL};

/// A seed with no special structure; the phase alignment it derives is what makes
/// the RCP-edge arithmetic below a real test rather than one aligned case.
const SEED: u64 = 0x5265_616C_6974_7921;

/// The whole point of the mode: it advances the machine without walking edges.
#[test]
fn it_engages_and_reaches_the_target() {
    const TARGET: u64 = 200_000;

    let mut sys = System::new(SEED);
    let report = sys.run_until_exec(TARGET);

    assert!(
        report.engaged(),
        "the fast path executed no instruction, so it deferred the whole run"
    );
    assert!(
        sys.master_ticks() >= TARGET,
        "the run finished at {} but was asked for {TARGET}",
        sys.master_ticks()
    );
    assert!(
        sys.cpu.retired > 0,
        "no instruction retired — the run advanced the clock without executing"
    );
}

/// **The overshoot is bounded**, which is the claim `run_until_exec`'s doc comment
/// makes and the reason ADR 0013 §4 asks for a measured divergence rather than
/// none.
///
/// A cost is only known after the instruction has run, so the last instruction of a
/// call can carry `master_ticks` past `target`. What must not happen is an
/// *unbounded* overshoot: that would mean the loop ran on after the target.
#[test]
fn the_overshoot_is_at_most_one_instructions_cost() {
    /// Comfortably above the most expensive single instruction — `DDIV` 69 plus a
    /// cache fill and the exception epilogue is under 200 `PCycles`. Expressed in
    /// master ticks, hence the divider.
    const MAX_INSTRUCTION_TICKS: u64 = 200 * CPU_DIVIDER;

    // Sweep targets across a whole RCP period so no single alignment can pass by
    // luck; an off-by-one in the RCP catch-up shows at one offset and not another.
    for offset in 0..RCP_DIVIDER * 4 {
        let target = 50_000 + offset;
        let mut sys = System::new(SEED);
        let _ = sys.run_until_exec(target);
        let landed = sys.master_ticks();
        assert!(
            landed >= target,
            "offset {offset}: landed short at {landed}"
        );
        assert!(
            landed - target < MAX_INSTRUCTION_TICKS,
            "offset {offset}: overshot by {} ticks, more than one instruction can \
             cost — the loop kept running past the target",
            landed - target
        );
    }
}

/// A target at or before the current tick must do nothing at all.
///
/// The mirror of `fast_scheduler_differential`'s no-op test, and it matters more
/// here: this loop's exit condition is `master_ticks < target` rather than an edge
/// search, so a sign error would execute one whole instruction.
#[test]
fn a_reached_target_is_a_no_op() {
    let mut sys = System::new(SEED);
    let _ = sys.run_until_exec(10_000);
    let (ticks, retired) = (sys.master_ticks(), sys.cpu.retired);

    for target in [ticks, ticks - 1, 0] {
        let report = sys.run_until_exec(target);
        assert!(
            !report.engaged(),
            "a reached target ({target}) executed an instruction"
        );
    }
    assert_eq!(
        sys.master_ticks(),
        ticks,
        "the clock moved for a reached target"
    );
    assert_eq!(
        sys.cpu.retired, retired,
        "work retired for a reached target"
    );
}

/// **The RCP runs over the span the CPU consumed**, which is the half of this mode
/// that is easiest to omit and hardest to notice: a fast path that advanced
/// `master_ticks` and never stepped the RCP would still retire instructions, still
/// reach the target, and still pass every other test in this file.
///
/// # The witness has to be RETAINED state, not a derived position
///
/// The obvious assertion — that `System::rcp_cycles` advanced — is **vacuous**, and
/// mutation-checking is what showed it: deleting the RCP catch-up loop entirely
/// left that version green. `rcp_cycles` is a *derived accessor* off `master_ticks`
/// (ADR 0006's whole point), so it advances whether or not a single RCP step ran.
///
/// `VI_V_CURRENT` is the opposite: the VI's half-line counter is **incremented** by
/// `Vi::tick`, which only `step_rcp` calls. If the RCP never steps it stays where
/// it started, and no amount of clock advancing moves it.
///
/// The general lesson is worth the paragraph: in a codebase that derives every
/// position from one counter, *most* of the obvious progress indicators cannot
/// witness that work happened. Ask what is stored, not what is reported.
#[test]
fn the_rcp_actually_steps_over_the_elapsed_span() {
    const TARGET: u64 = 400_000;

    let mut sys = System::new(SEED);
    // The VI needs a programmed frame geometry before its scan advances; a bare
    // power-on VI has `VI_V_TOTAL == 0` and holds. These are the NTSC defaults the
    // scan-out tests use.
    sys.bus.vi.write(VI_V_TOTAL, 524);
    sys.bus.vi.write(VI_H_TOTAL, 0x0000_0C15);
    let before = sys.bus.vi.read(VI_V_CURRENT);

    let _ = sys.run_until_exec(TARGET);

    assert_ne!(
        sys.bus.vi.read(VI_V_CURRENT),
        before,
        "the VI half-line never moved over {} ticks, so `step_rcp` was never \
         called: the CPU ran alone and every other assertion here is about a \
         machine with no RCP in it",
        sys.master_ticks()
    );
}

/// The accurate and fast paths must reach the **same target** from the same seed,
/// even though they take different amounts of emulated work to get there.
///
/// Not an equality gate on state — ADR 0013 §4 explicitly does not ask for that
/// here — but it does pin that both modes *terminate on the same contract*, which
/// is what every caller of `run_until` relies on.
#[test]
fn both_modes_honor_the_same_target_contract() {
    const TARGET: u64 = 120_000;

    let mut accurate = System::new(SEED);
    accurate.run_until(TARGET);
    assert_eq!(
        accurate.master_ticks(),
        TARGET,
        "the accurate path lands exactly on the target"
    );

    let mut fast = System::new(SEED);
    let _ = fast.run_until_exec(TARGET);
    assert!(
        fast.master_ticks() >= TARGET,
        "the fast path reaches the target (and may pass it, by design)"
    );
}

// **The halted-CPU branch is NOT tested here, and this records why rather than
// leaving a hole to be discovered.**
//
// `Bus::boot_nmi_halt` is latched only by `pif_boot_command_if_cmd`, on a real-PIF
// boot whose IPL2 checksum verify fails. Reaching it needs a PIF ROM and a
// deliberately corrupted image; `crates/rustyn64-test-harness/tests/commercial_boot.rs`
// is where that is exercised, and commercial/PIF images are never committed.
//
// A `#[cfg(test)]` setter would not help — this is an integration test, so it
// compiles against the crate with `cfg(test)` false — and a test-only *feature* on
// `Bus` would put a seam in the shipped path for one branch. ADR 0011 §6 permits
// that only where a boundary genuinely cannot be reached any other way, and the
// branch is three lines whose failure mode (a halted CPU stops the clock) is loud
// rather than silent. The honest state is: covered by inspection, not by a test.
