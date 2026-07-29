//! Phase 5 capstone — a *commercial* ROM boots and executes (LOCAL only).
//!
//! Boots a real cartridge from the gitignored corpus
//! (`tests/roms/external/commercial/`) via the retail HLE boot and runs it for a
//! few seconds of emulated time. The committable claim this asserts is
//! **"a commercial ROM boots and executes real code without panicking"**: the
//! HLE handoff lands, the CPU fetches the game's own instructions, and the PC
//! advances through millions of retired instructions across varied routines.
//!
//! It also *reports* the non-black pixel count for information, but does
//! **not** assert on it — and that count is NOT evidence of rendering, because
//! uninitialised RDRAM is non-black (ledger R-18). Reaching a rendered title
//! frame depends on the retail
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

    // Measured through `scanout_scaled`, which is what the frontend actually
    // presents (`rustyn64_frontend::emu::Emu::produce_frame`). It used to read the
    // 1:1 `Bus::scanout`, so the number reported here was not the number a user
    // would see: `scanout_scaled` applies the real `VI_X_SCALE` upscale, so a
    // 320-wide framebuffer scans out 625 wide and the counts differ by ~2x.
    //
    // Sized for the largest scan-out any supported mode can produce, not for the
    // NTSC case that happens to be in front of us. `scanout_scaled` returns
    // `(0, 0)` and writes nothing when the destination is too small — which would
    // arrive here as `non_black_pixels == 0`, i.e. a buffer-sizing mistake
    // wearing the exact costume of "this title renders nothing". PAL is 576
    // lines, so a 480-line buffer would have made every PAL title read as blank.
    // The dimensions are carried out in `BootResult` so a `(0, 0)` is visible as
    // itself rather than collapsing into the pixel count.
    let mut frame = vec![0u8; 720 * 576 * 4];
    let (w, h) = sys.bus.scanout_scaled(&mut frame);
    let non_black_pixels = frame
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
        non_black_pixels,
        scanout_dims: (w, h),
        rdram_nonzero,
        pc,
        rdram_len: u32::try_from(sys.bus.rdram.len()).unwrap_or(u32::MAX),
    })
}

/// Is this ROM's bootcode CIC-6105? Read from the cartridge header the same way
/// the boot does, so the classification cannot drift from what actually runs.
/// Only the 0x1000-byte boot header is read — the CIC is resolved from it.
fn is_cic_6105(path: &Path) -> bool {
    use std::io::Read as _;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut header = [0u8; 0x1000];
    if f.read_exact(&mut header).is_err() {
        return false;
    }
    rustyn64_core::cart::Cart::load(&header)
        .is_ok_and(|c| c.header().cic == rustyn64_core::cart::Cic::Cic6105)
}

/// What [`boot_and_run`] observed. Named fields rather than a tuple because the
/// assertions below distinguish "executed a lot" from "executed the *game*".
struct BootResult {
    retired: u64,
    /// Non-black pixels on the final scanned-out frame.
    ///
    /// **Named for what it measures, not what it was mistaken for.** It is NOT
    /// evidence of rendering — uninitialised RDRAM is non-black, and two titles
    /// scored 90%+ here while producing pure noise (ledger R-18). A diagnostic;
    /// never assert on it.
    ///
    /// Re-confirmed 2026-07-29 rather than merely restated: Ocarina of Time
    /// scored 62,963 of 75,840 (measured 1:1, before this caller migrated) on a
    /// 300-frame run and, rendered to PNG and looked at, is pure noise; the same
    /// run's Paper Mario frame is a real picture (`screenshots/`). The two are
    /// indistinguishable by this count.
    ///
    /// The denominator is `scanout_dims.0 * scanout_dims.1` — the **scaled**
    /// dimensions this is now measured at — not the 1:1 framebuffer size.
    non_black_pixels: usize,
    /// The `(width, height)` [`rustyn64_core::Bus::scanout_scaled`] returned.
    ///
    /// Reported alongside the count so `(0, 0)` — the VI blanked, or a
    /// destination buffer too small — is distinguishable from "scanned a real
    /// frame and none of it was lit". Both produce `non_black_pixels == 0`.
    scanout_dims: (u32, u32),
    rdram_nonzero: usize,
    pc: u32,
    /// Installed RDRAM length, so the PC bound below is the real mapped size
    /// rather than a hard-coded 8 MiB that an Expansion Pak build would outgrow.
    rdram_len: u32,
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
    /// Floor for "IPL3 actually copied the game in", **derived** rather than
    /// picked: IPL3 copies a documented `IPL3_COPY_BYTES` (1 MiB) from ROM into
    /// RDRAM (N64brew *IPL2* §IPL3; `rom::IPL3_COPY_BYTES`). A quarter of that is
    /// far below any real title — the staged corpus measures 787 KiB to 1.23 MiB
    /// non-zero, recorded in ledger R-18 — and far above the **zero** a machine
    /// that faulted out of IPL3 leaves. It separates those two states; it is not a
    /// claim about how much any particular game loads.
    const MIN_RDRAM_NONZERO: usize = rustyn64_test_harness::rom::IPL3_COPY_BYTES / 4;

    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/roms/external/commercial");
    // `staged` counts ROMs present; `exercised` counts ROMs actually put through
    // `boot_and_run`. They are separate on purpose. Counting only "a ROM was
    // found" is what made this test able to report success having asserted
    // **nothing**: it took the *first* ROM per folder, and if that one was
    // CIC-6105 it was skipped — which is the real situation for three of the five
    // save-type folders. The test then passed with two folders' worth of evidence
    // while looking like five.
    let mut staged = 0usize;
    let mut exercised = 0usize;
    // Whether any exercised title was CIC-6105. R-23 is specifically the claim
    // that those boot, so a run that never exercised one has not tested it —
    // reported rather than silently assumed.
    let mut cic_6105_seen = false;
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
        let mut roms: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "z64"))
            .collect();
        // Sorted for a stable, filesystem-independent choice.
        roms.sort();
        if roms.is_empty() {
            continue;
        }
        staged += 1;
        // No eligibility filter: R-23 closed the CIC-6105 gap, so the first ROM
        // is exercised whatever its CIC.
        //
        // That is NOT a claim that every CIC variant boots — R-18 still has open
        // cases across 6102/6103/6106 (titles that fill IMEM but never start the
        // RSP, or never load microcode). It is the narrower claim this capstone
        // actually asserts: whichever title is selected reaches its own code in
        // RDRAM. `cic_6105_seen` below adds the R-23-specific proof.
        let rom_path = roms.remove(0);
        exercised += 1;
        let name = rom_path.file_name().unwrap().to_string_lossy().into_owned();
        let is_6105 = is_cic_6105(&rom_path);
        cic_6105_seen |= is_6105;
        match boot_and_run(&rom_path, 120) {
            Some(r) => {
                eprintln!(
                    // Diagnostic only, never a pass condition — uninitialised
                    // RDRAM is non-black too (ledger R-18).
                    "[{folder}] {name}: retired={}, pc={:#010x}, rdram_nonzero={}, \
                     scanout={}x{}, non-black pixels (NOT proof of rendering)={}/{}",
                    r.retired,
                    r.pc,
                    r.rdram_nonzero,
                    r.scanout_dims.0,
                    r.scanout_dims.1,
                    r.non_black_pixels,
                    r.scanout_dims.0 * r.scanout_dims.1
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
                // KSEG0 spans 512 MiB, but only the installed RDRAM is backed;
                // a sled through an unmapped KSEG0 address would satisfy a bare
                // `0x8xxx_xxxx` check, so bound it by the real length.
                let kseg0_end = 0x8000_0000u32.saturating_add(r.rdram_len);
                assert!(
                    (0x8000_0000..kseg0_end).contains(&r.pc),
                    "[{folder}] {name} ended at pc={:#010x}, outside the mapped \
                     cached-RDRAM window 0x8000_0000..{kseg0_end:#010x} — it is \
                     not executing the game",
                    r.pc,
                );
            }
            None => panic!("[{folder}] could not read/boot {}", rom_path.display()),
        }
    }
    if staged == 0 {
        eprintln!("no commercial ROMs staged — capstone skipped (local-only)");
        return;
    }
    // Staged input that exercised nothing is a FAILURE, not a pass. An oracle
    // must not report success when its corpus was skipped.
    assert!(
        exercised > 0,
        "{staged} save-type folder(s) are staged but none was exercised, so this \
         capstone asserted nothing"
    );
    eprintln!("HLE capstone exercised {exercised} of {staged} staged folder(s)");
    // R-23's own evidence. Not an assertion, because a corpus may legitimately
    // contain no 6105 title — but said out loud, so "R-23 still holds" is never
    // inferred from a run that never exercised one.
    if cic_6105_seen {
        eprintln!("  ... including a CIC-6105 title, so R-23 is exercised");
    } else {
        eprintln!(
            "  ... but NO CIC-6105 title was exercised, so R-23 was not tested by \
             this run (the microcode witness covers Ocarina/Majora/Conker)"
        );
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
