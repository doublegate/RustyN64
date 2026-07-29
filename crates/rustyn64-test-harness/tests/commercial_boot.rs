//! Phase 5 capstone — a *commercial* ROM boots and executes (LOCAL only).
//!
//! Boots a real cartridge from the gitignored corpus
//! (`tests/roms/external/commercial/`) via the retail HLE boot and runs it for a
//! few seconds of emulated time. The committable claim this asserts is
//! **"a commercial ROM boots and executes real code without panicking"**: the
//! HLE handoff lands, the CPU fetches the game's own instructions, and the PC
//! advances through millions of retired instructions across varied routines.
//!
//! It also *reports* the scanned-out lit-pixel count for information, but does
//! **not** assert on it: reaching a rendered title frame depends on the retail
//! OS-boot runtime (the VI vblank interrupt loop, the RI/RDRAM interface, and
//! F3DEX graphics microcode) that is out of scope for the Phase 5 cart boundary
//! and is tracked as a ledgered cross-subsystem gap (accuracy-ledger R-18).
//!
//! The ROMs are **never committed** (copyright), so this test reads them from
//! disk at runtime and **skips gracefully when absent** — it is a local
//! capstone, not a CI gate. `#[ignore]`d so a normal `cargo test` does not
//! attempt it.
#![allow(
    clippy::doc_markdown,
    reason = "narrative capstone docs name ROM/HLE/RDRAM in prose"
)]

use std::path::Path;

use rustyn64_core::System;
use rustyn64_test_harness::rom;

/// Boot `path` and run it for `frames` ~60 Hz frames, returning what the run
/// observed (see [`BootResult`]) — or `None` if the ROM could not be read or booted.
fn boot_and_run(path: &Path, frames: u64) -> Option<BootResult> {
    const TICKS_PER_FRAME: u64 = rustyn64_core::MASTER_HZ / 60;

    let image = std::fs::read(path).ok()?;
    let mut sys = System::new(0);
    rom::hle_boot(&mut sys, &image).ok()?;

    for _ in 0..frames {
        let target = sys.master_ticks().saturating_add(TICKS_PER_FRAME);
        sys.run_until(target);
    }

    let mut frame = vec![0u8; 640 * 480 * 4];
    let (w, h) = sys.bus.scanout(&mut frame);
    let lit = frame
        .chunks_exact(4)
        .take((w * h) as usize)
        .filter(|px| px[0] != 0 || px[1] != 0 || px[2] != 0)
        .count();
    // How much of RDRAM the boot chain actually populated, and where the CPU
    // ended up. Both are needed because a *huge* retired count proves nothing on
    // its own: a machine sledding through zeroed RDRAM retires hundreds of
    // millions of NOPs and looks identical to a booted game by that measure.
    let rdram_nonzero = sys.bus.rdram.iter().filter(|b| **b != 0).count();
    let pc = (sys.cpu.pc & 0xFFFF_FFFF) as u32;
    Some(BootResult {
        retired: sys.cpu.retired,
        lit,
        rdram_nonzero,
        pc,
    })
}

/// Is this ROM's bootcode CIC-6105? Read from the cartridge header the same way
/// the boot does, so the classification cannot drift from what actually runs.
fn is_cic_6105(path: &Path) -> bool {
    let Ok(image) = std::fs::read(path) else {
        return false;
    };
    rustyn64_core::cart::Cart::load(&image)
        .is_ok_and(|c| c.header().cic == rustyn64_core::cart::Cic::Cic6105)
}

/// What [`boot_and_run`] observed. Named fields rather than a tuple because the
/// assertions below distinguish "executed a lot" from "executed the *game*".
struct BootResult {
    retired: u64,
    lit: usize,
    rdram_nonzero: usize,
    pc: u32,
}

/// **A commercial ROM boots and executes** (local capstone). Runs the first
/// available ROM from each save-type folder for a few seconds of emulated time,
/// asserts it retires a substantial number of real game instructions without
/// panicking, and reports how far it got. A folder with no ROMs is skipped, and
/// the whole test skips gracefully when no ROMs are staged at all.
#[test]
#[ignore = "local-only: reads the gitignored commercial corpus"]
fn a_commercial_rom_boots_and_executes() {
    /// A booted retail ROM retires far more than this within a few frames; a
    /// stalled or mis-booted machine retires near zero.
    const MIN_RETIRED: u64 = 1_000_000;
    /// A booted title has ~0.8-1.2 MiB of game code and data in RDRAM (IPL3 copies
    /// 1 MiB); a machine that faulted out of IPL3 leaves it entirely zero.
    const MIN_RDRAM_NONZERO: usize = 256 * 1024;

    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/roms/external/commercial");
    let mut any = false;
    for folder in [
        "eeprom-4k",
        "eeprom-16k",
        "sram",
        "flashram",
        "controller-pak",
    ] {
        let dir = base.join(folder);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let Some(rom_path) = entries
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|x| x == "z64"))
        else {
            continue;
        };
        any = true;
        let name = rom_path.file_name().unwrap().to_string_lossy().into_owned();
        // **CIC-6105 is a documented HLE scope limit, not a silent skip.** Its
        // IPL3 is a different program: it opens with a self-descrambling XOR loop
        // (`LW t0, -0xFF0(t1)` / `LW t2, 0x44(t3)` / `XOR` / `SW`) over registers
        // that only the real IPL2 leaves set. `hle_boot` seeds `sp` and `s3`-`s7`
        // but not `t1`/`t3`, so the first load faults and the machine sleds
        // (ledger R-23). The real-PIF capstone below boots these titles correctly,
        // which is why this is scoped rather than fixed here.
        if is_cic_6105(&rom_path) {
            eprintln!(
                "[{folder}] {name}: SKIPPED — CIC-6105 IPL3 is not HLE-bootable \
                 (ledger R-23); the real-PIF capstone covers it"
            );
            continue;
        }
        match boot_and_run(&rom_path, 120) {
            Some(r) => {
                eprintln!(
                    "[{folder}] {name}: retired={}, pc={:#010x}, rdram_nonzero={}, lit pixels={}",
                    r.retired, r.pc, r.rdram_nonzero, r.lit
                );
                assert!(
                    r.retired >= MIN_RETIRED,
                    "[{folder}] {name} retired only {} instructions \
                     (< {MIN_RETIRED}) — it did not boot and execute",
                    r.retired,
                );
                // **IPL3 loaded the game.** Retired count alone cannot show this:
                // for a long time every title took a TLB-refill exception out of
                // IPL3's prologue and executed a NOP sled through *empty* RDRAM,
                // retiring ~180 million instructions while loading nothing at all
                // (ledger R-18). A populated RDRAM is what separates the two.
                assert!(
                    r.rdram_nonzero >= MIN_RDRAM_NONZERO,
                    "[{folder}] {name} left only {} non-zero RDRAM bytes \
                     (< {MIN_RDRAM_NONZERO}) — IPL3 never copied the game in",
                    r.rdram_nonzero,
                );
                // **And the CPU is running that game code**, in cached RDRAM
                // (KSEG0 below the installed 8 MiB) — not still in IPL3's DMEM, and
                // not sledding past the end of memory.
                assert!(
                    (0x8000_0000..0x8080_0000).contains(&r.pc),
                    "[{folder}] {name} ended at pc={:#010x}, outside cached RDRAM \
                     — it is not executing the game",
                    r.pc,
                );
            }
            None => panic!("[{folder}] could not read/boot {}", rom_path.display()),
        }
    }
    if !any {
        eprintln!("no commercial ROMs staged — capstone skipped (local-only)");
    }
}

/// **A commercial ROM boots through the real PIF ROM** (local capstone, faithful
/// path). Unlike the HLE capstone above, this installs the console's real
/// IPL1/IPL2 and boots from the reset vector `0xBFC0_0000`, so the CPU runs the
/// genuine boot chain: IPL1 (from the PIF ROM) → IPL2 (from IMEM, which locks the
/// PIF ROM and has the PIF verify the IPL2 checksum against the CIC's) → the
/// cartridge's own IPL3 → the game in RDRAM.
///
/// Asserts the boot actually completed the real chain — the CPU is executing in
/// RDRAM (KSEG0, `0x8xxx_xxxx`), the PIF ROM has been locked off the bus, and the
/// checksum verify did **not** NMI-freeze the CPU (i.e. the CPU reproduced the
/// documented CIC checksum over the real IPL3). Needs both the gitignored PIF ROM
/// dump and a commercial ROM; skips gracefully when either is absent.
#[test]
#[ignore = "local-only: reads the gitignored PIF ROM + commercial corpus"]
fn a_commercial_rom_boots_through_the_real_pif() {
    const TICKS_PER_FRAME: u64 = rustyn64_core::MASTER_HZ / 60;

    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/roms/external/commercial");
    let Ok(pif_rom) = std::fs::read(base.join("bios/pifdata.bin")) else {
        eprintln!("no PIF ROM dump staged — real-PIF capstone skipped (local-only)");
        return;
    };

    let mut any = false;
    for folder in [
        "eeprom-4k",
        "eeprom-16k",
        "sram",
        "flashram",
        "controller-pak",
    ] {
        let Ok(entries) = std::fs::read_dir(base.join(folder)) else {
            continue;
        };
        let Some(rom_path) = entries
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|x| x == "z64"))
        else {
            continue;
        };
        let Ok(image) = std::fs::read(&rom_path) else {
            // A positively-found .z64 that fails to read is a real error, not an
            // absence — say so, so it can't be mistaken for "nothing staged".
            eprintln!("[{folder}] failed to read {}, skipping", rom_path.display());
            continue;
        };
        any = true;
        let name = rom_path.file_name().unwrap().to_string_lossy().into_owned();

        let mut sys = System::new(0);
        rom::real_pif_boot(&mut sys, &image, &pif_rom).expect("real-PIF boot setup");
        assert_eq!(
            sys.cpu.pc, 0xFFFF_FFFF_BFC0_0000,
            "[{folder}] {name}: the CPU must start at the PIF reset vector"
        );

        // Run a couple of seconds of emulated time — long enough for the whole
        // IPL1/IPL2/IPL3 chain to hand off to the game.
        for _ in 0..120 {
            let target = sys.master_ticks().saturating_add(TICKS_PER_FRAME);
            sys.run_until(target);
        }

        let pc = sys.cpu.pc & 0xFFFF_FFFF;
        eprintln!(
            "[{folder}] {name}: pc={pc:#010x} retired={} nmi={} pif_rom_byte0={:#x}",
            sys.cpu.retired,
            sys.bus.boot_nmi_halt(),
            sys.bus.cart.pif_boot_rom_read(0),
        );
        assert!(
            !sys.bus.boot_nmi_halt(),
            "[{folder}] {name}: the IPL2 checksum verify NMI-froze the CPU — the \
             CPU did not reproduce the CIC checksum over the real IPL3"
        );
        assert_eq!(
            sys.bus.cart.pif_boot_rom_read(0),
            0,
            "[{folder}] {name}: IPL2 must have locked the PIF ROM off the bus"
        );
        assert_eq!(
            pc & 0xE000_0000,
            0x8000_0000,
            "[{folder}] {name}: the CPU should be executing the game in RDRAM \
             (KSEG0) after the real boot chain, but pc={pc:#010x}"
        );
    }
    if !any {
        eprintln!("no commercial ROMs staged — real-PIF capstone skipped (local-only)");
    }
}
