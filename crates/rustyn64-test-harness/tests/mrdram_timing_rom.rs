//! Runner for the authored **M(RDRAM) cached-load timing ROM**
//! (`tools/mrdram-timing-rom/`).
//!
//! That ROM measures the VR4300 D-cache line-fill cost on real hardware via a
//! COP0-`Count` differential (a loop of cached loads that MISS vs. one that
//! HITS). Run on an N64, the numbers it emits are the *real* fill cost — the
//! measurement that would replace the value currently FITTED from ares/cen64
//! (accuracy ledger C-1).
//!
//! In the emulator it necessarily reads back **our** charged D-cache fill
//! (`M_DCACHE_FILL` = 40), so this test doubles as (a) proof the ROM's
//! measurement logic is correct end-to-end through a real machine boot, and
//! (b) a regression guard tying the ROM to the charged constant. On hardware the
//! same ROM yields the true number.

use rustyn64_core::System;
use rustyn64_test_harness::rom;

const ROM: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tools/mrdram-timing-rom/mrdram_timing.z64"
);

/// Result words the ROM writes to uncached RDRAM (phys `0x2000`).
fn word(sys: &System, phys: usize) -> u32 {
    let r = &sys.bus.rdram;
    u32::from_be_bytes([r[phys], r[phys + 1], r[phys + 2], r[phys + 3]])
}

#[test]
fn the_timing_rom_measures_the_charged_dcache_fill() {
    let image = std::fs::read(ROM).expect("assembled timing ROM (see tools/mrdram-timing-rom)");
    let entry = rom::entry_point(&image).expect("ROM header entry point");
    let mut sys = System::new(0);
    rom::load_direct(&mut sys, &image, entry).expect("load the ROM");

    // Run until the ROM writes its sentinel (the N word), rather than a bare
    // magic step count: the two N=1024 loops finish, then it stores the results
    // and spins. The cap is a generous backstop against a ROM that never writes.
    let mut steps = 0u64;
    while word(&sys, 0x200C) == 0 && steps < 20_000_000 {
        sys.step_to_next_edge();
        steps += 1;
    }

    let delta_miss = word(&sys, 0x2000);
    let delta_hit = word(&sys, 0x2004);
    let diff = word(&sys, 0x2008);
    let n = word(&sys, 0x200C);
    assert_eq!(n, 1024, "the ROM ran its loops and wrote its sentinel N");
    assert!(
        delta_miss > delta_hit,
        "the miss loop must be slower than the hit loop (miss={delta_miss} hit={delta_hit})"
    );

    // COP0 `Count` ticks once per 2 PClocks, so *2 gives PClocks.
    let per_miss_pclocks = f64::from(diff) / f64::from(n) * 2.0;
    println!(
        "timing ROM: delta_miss={delta_miss} delta_hit={delta_hit} diff={diff} N={n} \
         -> D-cache fill = {per_miss_pclocks:.2} PClocks (our charged M_DCACHE_FILL)"
    );
    assert!(
        (per_miss_pclocks - 40.0).abs() < 1.0,
        "the ROM measured {per_miss_pclocks:.2} PClocks; in-emulator it must read the charged \
         D-cache fill (40). If M_DCACHE_FILL changed, update this and ledger C-1."
    );
}
