//! The first real ROM run (T-11-006).
//!
//! Loads `basic.z64` from Dillon's n64-tests directly into RDRAM, runs it, and
//! reads its pass/fail out of `r30`.
//!
//! # Why this SKIPS rather than fails when the ROM is absent
//!
//! Dillon's n64-tests carries **no licence** (`tests/roms/README.md`), so it
//! cannot be committed and lives in the gitignored external tier. CI has no copy.
//! A missing ROM is therefore a normal condition, not a failure — treating it as
//! one would make the suite red for every contributor who has not staged the
//! external corpus, and a gate that is red by default stops being read.

use rustyn64_core::System;
use rustyn64_test_harness::rom;
use rustyn64_test_harness::runner::CompletionStatus;

/// Where the external corpus is staged, relative to the workspace root.
const BASIC_Z64: &str = "../../tests/roms/external/dillon-n64-tests/basic.z64";

/// Generous: the suite is five short subroutines, but the pipeline is five stages
/// deep and every instruction is a full `PClock`.
const BUDGET_TICKS: u64 = 20_000_000;

fn load() -> Option<Vec<u8>> {
    std::fs::read(BASIC_Z64).ok()
}

#[test]
fn basic_z64_runs_and_reports_a_real_result() {
    /// The address range `basic.z64`'s five test subroutines occupy.
    ///
    /// **Bounded at both ends deliberately.** A lower bound alone is not enough:
    /// an exception vector lives at `0xBFC0_0000` in KSEG1, which is numerically
    /// *above* the subroutines, so a crashed CPU jumping there would satisfy a
    /// `pc >= SUBROUTINES` test and hand back the vacuous pass this guard exists
    /// to catch.
    const SUBROUTINES: core::ops::Range<u64> = 0xFFFF_FFFF_8000_12E8..0xFFFF_FFFF_8000_2000;

    let Some(image) = load() else {
        eprintln!("SKIP: {BASIC_Z64} not staged (external tier, no licence)");
        return;
    };

    let entry = rom::entry_point(&image).expect("readable header");
    assert_eq!(
        entry, 0xFFFF_FFFF_8000_1000,
        "basic.z64's documented entry point, sign-extended as a 64-bit address"
    );

    let mut sys = System::new(0);
    let copied = rom::load_direct(&mut sys, &image, entry).expect("loadable");
    assert_eq!(copied, rom::IPL3_COPY_BYTES, "IPL3 copies exactly 1 MiB");

    // Step manually so the run can be *witnessed*, not just scored.
    //
    // A pass alone does not prove the tests ran. `basic.z64` ends with
    // `BNE $30, $0, TestsFailed` / `ADDI $30, $0, -1`: if `r30` is still 0 the
    // branch falls through and the pass sentinel is written anyway. "Ran and
    // passed" and "never ran" are indistinguishable at the sentinel — the same
    // hazard as a test gate that executes zero tests and exits 0
    // (`docs/engineering-lessons.md` §2.2).
    //
    // So this also asserts execution actually entered the test subroutines at
    // 0x8000_12E8, which no vacuous pass can satisfy.
    let mut entered_subroutines = false;
    let mut status = CompletionStatus::Timeout;
    let deadline = sys.master_ticks() + BUDGET_TICKS;
    while sys.master_ticks() < deadline {
        sys.step_to_next_edge();
        if SUBROUTINES.contains(&sys.cpu.pc) {
            entered_subroutines = true;
        }
        match sys.cpu.regs.read(30) {
            0 => {}
            u64::MAX => {
                status = CompletionStatus::Passed;
                break;
            }
            n => {
                status = CompletionStatus::Failed(n);
                break;
            }
        }
    }

    assert!(
        entered_subroutines,
        "r30 signalled completion but execution never reached the test \
         subroutines at {SUBROUTINES:#X?} -- this is a vacuous pass"
    );
    assert!(
        sys.cpu.retired > 20,
        "only {} instructions retired; the suite cannot have run",
        sys.cpu.retired
    );

    match status {
        CompletionStatus::Passed => {}
        CompletionStatus::Failed(n) => panic!("basic.z64 failed its test {n} (r30 = {n})"),
        CompletionStatus::Timeout => panic!(
            "basic.z64 never signalled completion within {BUDGET_TICKS} master ticks \
             (pc = {:#X}, retired = {})",
            sys.cpu.pc, sys.cpu.retired
        ),
    }
}

/// The loader must not panic on a ROM larger than RDRAM — the commercial corpus
/// is made of exactly those.
#[test]
fn loading_clamps_to_rdram_rather_than_panicking() {
    let mut sys = System::new(0);
    let huge = vec![0xAAu8; 16 * 1024 * 1024];
    let n = rom::load_direct(&mut sys, &huge, 0xFFFF_FFFF_8000_1000).expect("loadable");
    assert_eq!(n, rom::IPL3_COPY_BYTES, "clamped to what IPL3 copies");
    assert_eq!(sys.bus.rdram[0x1000], 0xAA);
}

#[test]
fn a_truncated_image_is_an_error_not_a_panic() {
    let mut sys = System::new(0);
    assert_eq!(
        rom::load_direct(&mut sys, &[0u8; 4], 0),
        Err(rom::LoadError::TooSmall)
    );
    assert_eq!(rom::entry_point(&[0u8; 4]), Err(rom::LoadError::TooSmall));
}
