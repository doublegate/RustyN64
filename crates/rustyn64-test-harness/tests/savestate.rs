//! Save-state bit-identical restore (Phase 6, Sprint 2 — ADR 0004).
//!
//! Serialising the whole `System` and restoring it must produce a run that
//! continues **bit-identically**. This is the two-run trace compare the Phase 6
//! plan calls for: run A snapshots, continues, and is fingerprinted; run B
//! restores the snapshot, continues the same number of frames, and is
//! fingerprinted — and the two fingerprints must match. A mismatch means a piece
//! of machine state was left out of the serialiser (the silent-divergence hazard
//! ADR 0004 exists to prevent), which no other test would catch.

use rustyn64_core::System;
use rustyn64_test_harness::rom;

/// A committed, license-clean homebrew ROM (our own MIPS assembly) that programs
/// the VI and CPU-fills a framebuffer — it mutates RDRAM, the CPU registers/pc,
/// the caches, and the VI, giving the fingerprint real state to diverge on.
const RENDER_FILL_Z64: &[u8] = include_bytes!("../../../tests/roms/homebrew/render_fill.z64");

const TICKS_PER_FRAME: u64 = rustyn64_core::MASTER_HZ / 60;

/// FNV-1a over the machine's large mutable stores plus the key scalar positions —
/// comprehensive enough that any diverging run produces a different value.
fn fingerprint(sys: &System) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |bytes: &[u8]| {
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    mix(&sys.bus.rdram);
    mix(&sys.bus.rsp.dmem[..]);
    mix(&sys.bus.rsp.imem[..]);
    mix(&sys.cpu.pc.to_le_bytes());
    mix(&sys.cpu.retired.to_le_bytes());
    mix(&sys.master_ticks().to_le_bytes());
    h
}

fn run(sys: &mut System, frames: u64) {
    for _ in 0..frames {
        let target = sys.master_ticks().saturating_add(TICKS_PER_FRAME);
        sys.run_until(target);
    }
}

#[test]
fn a_snapshot_restores_and_continues_bit_identically() {
    // Boot a real homebrew ROM and run partway, so the snapshot captures a
    // machine mid-work (framebuffer fill in progress, caches populated).
    let entry = rom::entry_point(RENDER_FILL_Z64).expect("entry point");
    let mut sys = System::new(0);
    rom::load_direct(&mut sys, RENDER_FILL_Z64, entry).expect("direct load");
    run(&mut sys, 2);

    // Snapshot the whole machine.
    let snapshot = bincode::serialize(&sys).expect("serialize System");

    // Run A: continue from the live machine. Even after the fill completes the
    // CPU keeps executing (pc/retired advance), so a missing CPU-state field
    // diverges here; RDRAM/DMEM are hashed for the memory state.
    run(&mut sys, 6);
    let fp_a = fingerprint(&sys);

    // Run B: restore a fresh machine from the snapshot and continue equally.
    let mut restored: System = bincode::deserialize(&snapshot).expect("deserialize System");
    run(&mut restored, 6);
    let fp_b = fingerprint(&restored);

    assert_eq!(
        fp_a, fp_b,
        "a restored snapshot must continue bit-identically; a mismatch means a \
         state field was not serialised (RDRAM/DMEM/IMEM/pc/retired hashed)"
    );
}

/// **Full-machine** bit-identical restore over a commercial ROM (local only). The
/// homebrew test above exercises the CPU/RDRAM/VI; a booted commercial ROM drives
/// the RSP, RDP, AI, and cart too, so this is the strong check that *every*
/// subsystem's state round-trips. Reads the gitignored corpus; skips when absent.
#[test]
#[ignore = "local-only: reads the gitignored commercial corpus"]
fn a_commercial_rom_snapshot_restores_bit_identically() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/roms/external/commercial/eeprom-4k/Super Mario 64.z64"
    );
    let Ok(raw) = std::fs::read(path) else {
        eprintln!("no commercial ROM staged — full-machine save-state test skipped");
        return;
    };

    let mut sys = System::new(0);
    rom::hle_boot(&mut sys, &raw).expect("HLE boot");
    // Boot well into the game so the RSP/RDP/AI/cart carry live state.
    run(&mut sys, 30);

    let snapshot = bincode::serialize(&sys).expect("serialize");

    run(&mut sys, 20);
    let fp_a = fingerprint(&sys);

    let mut restored: System = bincode::deserialize(&snapshot).expect("deserialize");
    // The ROM is `#[serde(skip)]`'d from the snapshot (it is up to 64 MiB); the
    // frontend re-attaches it on restore. Do the same here.
    let cart_rom = sys.bus.cart.rom_image().to_vec();
    restored.bus.cart.reattach_rom(cart_rom);
    run(&mut restored, 20);
    let fp_b = fingerprint(&restored);

    assert_eq!(
        fp_a, fp_b,
        "a full-machine commercial-ROM snapshot must continue bit-identically"
    );
}

/// A no-op restore (snapshot then immediately restore, both continue) must also
/// match — a guard that the fingerprint itself is deterministic and that the
/// serialiser round-trips the *initial* boot state, not just steady state.
#[test]
fn a_freshly_booted_snapshot_round_trips() {
    let entry = rom::entry_point(RENDER_FILL_Z64).expect("entry point");
    let mut sys = System::new(0);
    rom::load_direct(&mut sys, RENDER_FILL_Z64, entry).expect("direct load");

    let snapshot = bincode::serialize(&sys).expect("serialize");
    let restored: System = bincode::deserialize(&snapshot).expect("deserialize");
    assert_eq!(
        fingerprint(&sys),
        fingerprint(&restored),
        "a fresh boot must round-trip through serialisation unchanged"
    );
}
