//! Cached-miss microbench — the first-party instrument for **`M(RDRAM)`**, the
//! D-cache-fill memory latency (accuracy ledger C-1).
//!
//! Unlike the uncached RCP-register `M` (measured from the PeterLemon
//! CPUTIMINGNTSC mult/div differential), the cached D-cache-fill cost
//! `8..=9 + M(RDRAM)` (UM Table 11-1) has **no hardware timing oracle** in the
//! committed reference set: PeterLemon's timing ROMs time only ALU ops (no
//! loads), n64-systemtest's `Level::Timing` set is two COP0 `Random` tests, and
//! the reference emulators' fill costs (ares 40, CEN64 44) are self-admittedly
//! *fitted* ("Currently using fixed values") — not spec- or hardware-derived. So
//! this measures **our** per-miss cost; it does not, by itself, yield the
//! hardware value.
//!
//! What it does give: (1) proof the D-cache model exercises correctly (a strided
//! sweep over a region larger than the 8 KiB cache misses every line; a fixed
//! address hits after the first fill), and (2) our charged per-miss cost, wired
//! to the emulator so the moment a hardware cached-miss oracle appears (a
//! first-party ROM run on hardware, or a community capture) the number drops
//! straight in and this test flips to assert it. Today that cost is **0** —
//! `dcache_fill` charges no `M` — which is the honest current state, not a
//! measurement of hardware.

// Prose names (ares, CEN64, ALU) are not code items; the tick counts are small
// enough that the u64->f64 casts lose nothing.
#![allow(clippy::doc_markdown, clippy::cast_precision_loss)]

use rustyn64_core::System;

// A KSEG0 loop: load, advance the pointer by a stride, decrement a counter,
// branch. `$t1` (r9) = load pointer, `$t3` (r11) = stride, `$t4` (r12) = count.
//   L: lw   $zero, 0($t1)   8D20_0000
//      addu $t1, $t1, $t3   012B_4821
//      addiu $t4, $t4, -1   218C_FFFF
//      bne  $t4, $zero, L   1580_FFFC   (-4 words back to L)
//      nop                  0000_0000
const PROG: [u32; 5] = [
    0x8D20_0000,
    0x012B_4821,
    0x218C_FFFF,
    0x1580_FFFC,
    0x0000_0000,
];
const CODE_PHYS: u32 = 0x1000;
const CODE_VADDR: u64 = 0xFFFF_FFFF_8000_1000;
/// Data region base, cached KSEG0 (phys 0x10000) — clear of the code.
const DATA_VADDR: u64 = 0xFFFF_FFFF_8001_0000;
/// The emulator's charged D-cache-fill cost, in PClocks (`Pipeline::M_DCACHE_FILL`).
/// **FITTED, not measured** (ledger C-1): adopted from ares (40), since no
/// hardware cached-miss oracle exists. Update this and the ledger together if the
/// charge changes — the microbench asserts the two agree.
const CHARGED_M_RDRAM_PCLOCKS: f64 = 40.0;

/// Run the loop `n` times with pointer stride `stride`; return master ticks spent.
fn run_loop(n: u32, stride: u32) -> u64 {
    let mut sys = System::new(0);
    for (i, w) in PROG.iter().enumerate() {
        let off = CODE_PHYS as usize + i * 4;
        sys.bus.rdram[off..off + 4].copy_from_slice(&w.to_be_bytes());
    }
    sys.cpu.regs.gpr[9] = DATA_VADDR;
    sys.cpu.regs.gpr[11] = u64::from(stride);
    sys.cpu.regs.gpr[12] = u64::from(n);
    sys.cpu.set_pc(CODE_VADDR);

    let start = sys.master_ticks();
    let mut guard = 0u64;
    // Stop on the counter reaching 0 -- a retirement signal, immune to the
    // speculative-fetch PC that runs ahead of the branch.
    while sys.cpu.regs.gpr[12] != 0 && guard < 50_000_000 {
        sys.step_to_next_edge();
        guard += 1;
    }
    sys.master_ticks() - start
}

/// The per-miss extra cost our emulator charges equals its D-cache-fill `M`.
///
/// Stride 16 (one D-cache line) over `n = 4096` lines sweeps a 64 KiB region --
/// far larger than the 8 KiB D-cache -- so every load misses; stride 0 hits the
/// one line after its first fill. The difference isolates the fill cost.
#[test]
fn the_dcache_fill_cost_matches_the_charged_m_rdram() {
    let n = 4096u32;
    let miss = run_loop(n, 16);
    let hit = run_loop(n, 0);

    // Both loops must actually run (guard against a mis-encoded loop that exits
    // early): ~5 PClocks/iteration = ~10 master ticks each.
    assert!(
        miss > u64::from(n) * 8 && hit > u64::from(n) * 8,
        "loops did not run: miss={miss} hit={hit} for n={n}"
    );

    // CPU steps every 2 master ticks, so PClocks = ticks / 2.
    let per_miss_pclocks = (miss as f64 - hit as f64) / f64::from(n) / 2.0;
    eprintln!(
        "cached-miss microbench: miss={miss}t hit={hit}t n={n} -> per-miss M = {per_miss_pclocks:.3} PClocks"
    );
    assert!(
        (per_miss_pclocks - CHARGED_M_RDRAM_PCLOCKS).abs() < 0.5,
        "per-miss cost {per_miss_pclocks:.3} disagrees with the charged M(RDRAM) \
         {CHARGED_M_RDRAM_PCLOCKS}; update both this constant and ledger C-1 together"
    );
}
