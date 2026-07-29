//! `T-71-003` — a **retail title's own graphics microcode executes on the LLE
//! RSP**, and its output reaches the RDP (LOCAL only).
//!
//! This is the ADR 0002 payoff stated as a falsifiable claim. Because RustyN64
//! runs the real RSP instruction stream rather than pattern-matching display
//! lists, a commercial game's microcode — F3DEX and its many vendor variants —
//! executes for the same reason libdragon's `rdpq` does: there is no
//! microcode-specific code path anywhere in the tree to add.
//!
//! **What is asserted, and why each part is needed.** A weaker test would prove
//! nothing:
//!
//! - The game uploads microcode into IMEM. Necessary but far from sufficient — a
//!   DMA that lands bytes proves the *CPU* worked, not the RSP.
//! - The RSP leaves halt. Also insufficient: an RSP that unhalts and immediately
//!   stalls has executed nothing.
//! - **The RSP's PC visits many distinct IMEM addresses.** This is the load-bearing
//!   one. It cannot be satisfied by a stalled core, a halted core, or a core
//!   spinning on one instruction — only by actually executing the game's program.
//!
//! **Read the halt/PC state through `Rsp::halted()` / `Rsp::pc()`, never through
//! any same-named field.** The struct carries vestigial `halted`/`pc` fields that
//! are never written (kept only so the save-state layout is unchanged, and now
//! `#[deprecated]`). Sampling them reports "halted forever at PC 0" for a *running* RSP,
//! which is exactly the wrong conclusion this test exists to prevent.
//!
//! ROMs are **never committed** (copyright); this reads them at runtime and skips
//! when absent. `#[ignore]`d — a local capstone, not a CI gate. The licence-clean
//! CI-gated counterpart is `tests/microcode.rs`, which runs libdragon's public
//! domain `rdpq` microcode end to end.
#![allow(
    clippy::doc_markdown,
    reason = "narrative capstone docs name IMEM/RSP/RDP/F3DEX in prose"
)]

use std::collections::HashSet;
use std::path::Path;

use rustyn64_core::System;
use rustyn64_test_harness::rom;

/// One ~60 Hz frame of emulated master ticks.
const TICKS_PER_FRAME: u64 = rustyn64_core::MASTER_HZ / 60;

/// How finely the RSP is sampled, in master ticks. The RSP runs in bursts between
/// CPU work, so a per-frame sample would miss most of them; 24 ticks is 8 RCP
/// steps, fine enough to catch a burst without dominating the run time.
const SAMPLE_TICKS: u64 = 24;

/// What a run observed about the RSP.
struct RspActivity {
    /// Samples taken while `SP_STATUS.halt` was clear.
    running_samples: u64,
    /// Distinct IMEM addresses the RSP's PC visited — the evidence of execution.
    distinct_pcs: usize,
    /// Non-zero bytes in IMEM at the end (microcode was uploaded).
    imem_nonzero: usize,
    /// RDP commands retired through the DPC seam.
    dpc_commands: u64,
}

/// Boot `path` and watch the RSP for `frames` frames.
fn watch_rsp(path: &Path, frames: u64) -> Option<RspActivity> {
    let image = std::fs::read(path).ok()?;
    let mut sys = System::new(0);
    rom::hle_boot(&mut sys, &image).ok()?;

    let mut running_samples = 0u64;
    let mut pcs: HashSet<u32> = HashSet::new();
    for _ in 0..frames {
        let target = sys.master_ticks().saturating_add(TICKS_PER_FRAME);
        while sys.master_ticks() < target {
            sys.run_until(sys.master_ticks().saturating_add(SAMPLE_TICKS));
            // `Rsp::halted()` / `Rsp::pc()` — the accessors, which read
            // `SP_STATUS`. See the module docs on why the fields must not be used.
            if !sys.bus.rsp.halted() {
                running_samples += 1;
                pcs.insert(sys.bus.rsp.pc());
            }
        }
    }
    Some(RspActivity {
        running_samples,
        distinct_pcs: pcs.len(),
        imem_nonzero: sys.bus.rsp.imem.iter().filter(|b| **b != 0).count(),
        dpc_commands: sys.bus.rdp.commands_processed,
    })
}

/// **A retail game's own microcode executes on the LLE RSP** (`T-71-003`).
///
/// Runs every staged title and requires that *at least one* demonstrates the full
/// chain — microcode in IMEM, the RSP running, and many distinct RSP PCs. It is
/// deliberately not "every title": some are still blocked upstream by R-18's
/// remaining boot work and by R-23 (CIC-6105), and asserting on all of them would
/// conflate "this game boots" with "the RSP executes microcode", which are
/// different claims with different causes.
///
/// Per-title results are always printed, so a regression that reduces the set is
/// visible even while the assertion still passes.
#[test]
#[ignore = "local-only: reads the gitignored commercial corpus"]
fn a_retail_titles_own_microcode_executes_on_the_rsp() {
    /// An RSP that merely unhalts and stalls visits one or two PCs. A real
    /// microcode task — even a short one — runs through dozens of instructions.
    /// Set well above a stall and well below what the observed titles reach
    /// (148-331), so it distinguishes execution from non-execution rather than
    /// pinning a particular microcode's length.
    const MIN_DISTINCT_PCS: usize = 32;

    /// ROMs sampled per save-type folder. A cap, not a filter: each title costs
    /// ~7 s of emulated boot, so the full 66-title corpus would take ~8 minutes.
    /// Three per folder is 15 titles across every save type, which is ample for a
    /// claim that is about the RSP rather than about breadth of coverage — the
    /// corpus-wide sweep is `T-71-002`, a separate ticket.
    const ROMS_PER_FOLDER: usize = 3;

    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/roms/external/commercial");
    let mut staged = 0usize;
    let mut witnesses: Vec<String> = Vec::new();
    // Staged ROMs that failed to boot at all. Tracked and asserted on, because a
    // title regressing from "boots" to "does not boot" would otherwise vanish
    // silently as long as some *other* title still witnessed the claim.
    let mut boot_failures: Vec<String> = Vec::new();

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
        let mut roms: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "z64"))
            .collect();
        // Sorted so the reported set is stable across runs and filesystems.
        roms.sort();
        for rom_path in roms.into_iter().take(ROMS_PER_FOLDER) {
            staged += 1;
            let name = rom_path.file_name().unwrap().to_string_lossy().into_owned();
            let Some(a) = watch_rsp(&rom_path, 90) else {
                eprintln!("[{folder}] {name}: FAILED TO BOOT");
                boot_failures.push(format!("[{folder}] {name}"));
                continue;
            };
            eprintln!(
                "[{folder}] {name}: rsp_running={}, distinct_rsp_pcs={}, imem_nonzero={}, dpc_cmds={}",
                a.running_samples, a.distinct_pcs, a.imem_nonzero, a.dpc_commands
            );
            if a.imem_nonzero > 0 && a.running_samples > 0 && a.distinct_pcs >= MIN_DISTINCT_PCS {
                witnesses.push(format!(
                    "{name} ({} distinct RSP PCs, {} RDP commands)",
                    a.distinct_pcs, a.dpc_commands
                ));
            }
        }
    }

    if staged == 0 {
        eprintln!("no commercial ROMs staged — T-71-003 witness skipped (local-only)");
        return;
    }
    // A staged ROM that cannot boot is a regression, not a skip. R-23 (CIC-6105)
    // titles still *boot* under HLE far enough to be measured here — they fail
    // later — so this does not need a carve-out for them.
    assert!(
        boot_failures.is_empty(),
        "staged ROMs failed to boot: {}",
        boot_failures.join(", ")
    );
    assert!(
        !witnesses.is_empty(),
        "no staged title executed its own microcode on the RSP: none reached \
         {MIN_DISTINCT_PCS} distinct RSP PCs with microcode in IMEM. \
         ADR 0002's claim is that this needs no per-microcode support, so a \
         regression here is in the RSP or the SP task-start path, not in \
         microcode coverage",
    );
    eprintln!("T-71-003 witnessed by: {}", witnesses.join("; "));
}
