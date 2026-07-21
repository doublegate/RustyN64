//! The CPU golden-log 0-diff — Phase 1's second exit criterion (T-HARNESS-01).
//!
//! `to-dos/phase-1-cpu-golden-log/overview.md` requires that "the golden-log
//! differ finds no divergence across the captured trace, reporting the first
//! mismatched instruction rather than a bare failure". This is that gate.
//!
//! # What it proves, stated precisely
//!
//! *Given identical initial state, `RustyN64` retires the same instructions in
//! the same order as ares.* It is a **tandem-verification** claim in the sense
//! used by RISC-V co-simulation harnesses (RVVI/RVFI): the reference and the
//! device under test are aligned at a comparison boundary, and only deltas from
//! that boundary are the claim. It is not a claim about boot, about timing, or
//! about anything before the sync point.
//!
//! # Why the boundary is the ELF entry
//!
//! Everything earlier is PIF ROM and IPL3 — copyrighted Nintendo code plus
//! libdragon's bootloader — which must not enter the repository. It is also
//! where `RustyN64` begins executing, so the two streams are directly comparable
//! without modelling the cartridge subsystem (that is Phase 5).
//!
//! # Why it is `#[ignore]`d
//!
//! It runs the emulator for tens of thousands of instructions in release mode.
//! Fast, but not free, and the default `cargo test` path should stay quick.

use rustyn64_core::System;
use rustyn64_test_harness::golden::{GoldenDiff, GoldenLogDiffer, TraceRecord};
use rustyn64_test_harness::rom;

const ROM: &str = "../../tests/roms/n64-systemtest/n64-systemtest.z64";

#[test]
#[ignore = "runs the CPU for ~50k instructions; run explicitly"]
fn rustyn64_matches_the_reference_trace_instruction_for_instruction() {
    let golden = GoldenLogDiffer::load_golden("n64-systemtest")
        .expect("the committed reference trace must be present and parsable");
    assert!(
        golden.len() > 1000,
        "golden log has only {} records -- an empty or truncated log makes the \
         diff below pass against anything, which is the vacuous pass this gate \
         exists to prevent",
        golden.len()
    );

    let image = std::fs::read(ROM).expect("the committed n64-systemtest ROM");
    let mut sys = System::new(0);
    rom::load_elf(&mut sys, &image).expect("loadable ELF payload");
    let elf = rom::seed_ipl3_handoff(&mut sys, &image).expect("IPL3 handoff");
    let entry = u32::from_be_bytes([
        image[elf + 0x18],
        image[elf + 0x19],
        image[elf + 0x1A],
        image[elf + 0x1B],
    ]);
    sys.cpu
        .set_pc(i64::from(entry.cast_signed()).cast_unsigned());

    // The reference records RETIRED instructions, so this must too. `wb_stage`
    // runs first in the reverse cascade (ADR 0007), so the instruction retiring
    // during a step is the one in `dc_wb` *before* it -- sampling afterwards
    // captures speculative fetches that the pipeline goes on to squash.
    let mut differ = GoldenLogDiffer::new();
    let mut prev_retired = sys.cpu.retired;
    while differ.records().len() < golden.len() {
        let retiring = sys.cpu.pipeline.dc_wb.pc;
        let occupied = sys.cpu.pipeline.dc_wb.occupied;
        sys.step_to_next_edge();
        if sys.cpu.retired != prev_retired {
            prev_retired = sys.cpu.retired;
            if occupied {
                differ.push(TraceRecord {
                    pc: retiring,
                    gpr: sys.cpu.regs.gpr,
                    cycle: sys.cpu.retired,
                });
            }
        }
    }

    match differ.diff(&golden) {
        GoldenDiff::Match => {}
        GoldenDiff::Diverged {
            index,
            expected_pc,
            actual_pc,
        } => panic!(
            "golden-log divergence at retired instruction {index}: \
             reference (ares) executed {expected_pc:#018x}, RustyN64 executed \
             {actual_pc:#018x}.\n\
             Regenerate the log only if the REFERENCE is wrong -- see the \
             provenance header in tests/golden/n64-systemtest.log."
        ),
    }
}
