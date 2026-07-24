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

/// Boot `path` and run it for `frames` ~60 Hz frames, returning the retired
/// instruction count and the number of non-black scanned-out pixels on the final
/// frame (0 = never produced video).
fn boot_and_run(path: &Path, frames: u64) -> Option<(u64, usize)> {
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
    Some((sys.cpu.retired, lit))
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
        match boot_and_run(&rom_path, 120) {
            Some((retired, lit)) => {
                eprintln!("[{folder}] {name}: retired={retired}, lit pixels={lit}");
                assert!(
                    retired >= MIN_RETIRED,
                    "[{folder}] {name} retired only {retired} instructions \
                     (< {MIN_RETIRED}) — it did not boot and execute",
                );
            }
            None => panic!("[{folder}] could not read/boot {}", rom_path.display()),
        }
    }
    if !any {
        eprintln!("no commercial ROMs staged — capstone skipped (local-only)");
    }
}
