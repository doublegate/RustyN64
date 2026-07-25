//! Runner for the authored **I-cache-fill timing ROM**
//! (`tools/mrdram-timing-rom/icache_timing.asm`).
//!
//! The companion to `mrdram_timing_rom.rs` (which measures the D-cache fill).
//! That ROM measures the VR4300 **I-cache** line-fill cost on real hardware: it
//! runs a straight-line block of N one-PClock instructions LARGER than the
//! 16 KiB instruction cache, so every 32-byte fetch line misses, and times it
//! with COP0 `Count`. Run on an N64, the number it emits is the *real* fill cost
//! — the measurement that would replace the value currently FITTED from
//! ares/cen64 (accuracy ledger C-1).
//!
//! In the emulator it necessarily reads back **our** charged I-cache fill
//! (`M_ICACHE_FILL` = 46), so this test doubles as (a) proof the ROM's
//! measurement logic is correct end-to-end through a real machine boot, and
//! (b) a regression guard tying the ROM to the charged constant. On hardware the
//! same ROM yields the true number.

use rustyn64_core::System;
use rustyn64_test_harness::rom;

const ROM: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tools/mrdram-timing-rom/icache_timing.z64"
);

/// Result words the ROM writes to uncached RDRAM (phys `0x10000`).
fn word(sys: &System, phys: usize) -> u32 {
    let r = &sys.bus.rdram;
    u32::from_be_bytes([r[phys], r[phys + 1], r[phys + 2], r[phys + 3]])
}

#[test]
fn the_timing_rom_measures_the_charged_icache_fill() {
    let image = std::fs::read(ROM).expect("assembled timing ROM (see tools/mrdram-timing-rom)");
    let entry = rom::entry_point(&image).expect("ROM header entry point");
    let mut sys = System::new(0);
    rom::load_direct(&mut sys, &image, entry).expect("load the ROM");

    // Run until the ROM writes its sentinel (the N word at phys 0x10004): the
    // straight-line block runs, then it stores delta + N and spins. The cap is a
    // generous backstop against a ROM that never writes.
    let mut steps = 0u64;
    while word(&sys, 0x10004) == 0 && steps < 20_000_000 {
        sys.step_to_next_edge();
        steps += 1;
    }

    let delta = word(&sys, 0x10000);
    let n = word(&sys, 0x10004);
    assert_eq!(n, 8192, "the ROM ran its block and wrote its sentinel N");

    // The block is N one-PClock `addiu`s; every 8-instruction (32 B) fetch line
    // misses. Subtract the 1-PClock-per-instruction base, divide by the number
    // of line fills (N/8). COP0 `Count` ticks once per 2 PClocks, so *2 gives
    // PClocks: fill = (delta * 2 - N) / (N / 8).
    let fill = (f64::from(delta) * 2.0 - f64::from(n)) / (f64::from(n) / 8.0);
    println!(
        "timing ROM: delta={delta} N={n} \
         -> I-cache fill = {fill:.2} PClocks (our charged M_ICACHE_FILL)"
    );
    assert!(
        (fill - 46.0).abs() < 1.0,
        "the ROM measured {fill:.2} PClocks; in-emulator it must read the charged \
         I-cache fill (46). If M_ICACHE_FILL changed, update this and ledger C-1."
    );
}
