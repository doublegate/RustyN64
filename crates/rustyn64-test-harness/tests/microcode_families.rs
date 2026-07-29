//! **T-71-003 — custom graphics-microcode families render** (LOCAL only).
//!
//! This is the **one v0.8.0 cut criterion CI can never run**, and the reason is
//! structural rather than technical. The criterion is that commercial titles'
//! own graphics microcode — F3DEX, F3DEX2 and the per-studio variants — drives
//! our RDP to a picture. That microcode is proprietary and ships *inside*
//! commercial ROMs, which ADR 0008 forbids committing. There is no licence-clean
//! substitute that exercises the real thing: the rdpq path (`tests/microcode.rs`)
//! is a genuine LLE microcode test and is CI-gated, but rdpq is *not* F3DEX, so
//! it cannot stand in for this.
//!
//! The disposition, decided explicitly rather than defaulted: **this criterion
//! closes on a reproducible local census plus committed screenshots**, and
//! `to-dos/VERSION-PLAN.md` records it as the single non-CI gate. The runner is
//! committed so the evidence is reproducible by any maintainer holding a corpus;
//! the ROMs are not, and never are.
//!
//! What this asserts is deliberately weak and honest: that titles staged locally
//! **do** render through their own microcode, and that the run actually
//! exercised ROMs. It does **not** assert a pass rate — the corpus is whatever
//! happens to be staged, so a percentage computed from it would be a number
//! about one machine's ROM collection dressed up as an accuracy metric.
//!
//! ```text
//! cargo test -p rustyn64-test-harness --release --test microcode_families \
//!     -- --ignored --nocapture
//! ```
#![cfg(not(target_arch = "wasm32"))]
#![allow(
    clippy::doc_markdown,
    reason = "prose names RDP/RDRAM/F3DEX/CI constantly"
)]

use std::path::{Path, PathBuf};

use rustyn64_core::System;
use rustyn64_test_harness::rom;

/// Frames to run each title before judging it.
///
/// Retail titles spend the first seconds in their own boot, logo and thread
/// setup; Super Mario 64's title screen first appears around frame 360. 420
/// leaves margin past that without making a full-corpus sweep unbearable.
const FRAMES: u64 = 420;

/// Sample the frame every this many frames rather than every frame.
///
/// Also the reason a title is judged on its **best** sample rather than its
/// last: a single-point sample can land on a transient. Super Mario 64 was once
/// measured at `scanout = 0` and read as a geometry failure — it was one frame
/// with `H_VIDEO = 0` while the game reprogrammed the VI (ledger R-18).
const SAMPLE_EVERY: u64 = 60;

/// A title counts as rendering only above **both** thresholds.
///
/// Neither alone is evidence. Commands without lit pixels is the "rasterises to
/// black" state several titles sat in; lit pixels without commands is
/// uninitialised RDRAM scanned out, which scored 90%+ on titles producing pure
/// noise (ledger R-18). The pair is still only a *filter* — the committed
/// screenshots exist because the only real proof is looking.
const MIN_COMMANDS: u64 = 1_000;
/// Companion floor to [`MIN_COMMANDS`]; see its docs.
const MIN_LIT: usize = 1_000;

/// What one title did.
struct Census {
    name: String,
    commands: u64,
    dims: (u32, u32),
    lit: usize,
}

impl Census {
    /// Did this title render? See [`MIN_COMMANDS`] for why both terms are needed.
    const fn renders(&self) -> bool {
        self.commands >= MIN_COMMANDS && self.lit >= MIN_LIT
    }
}

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/roms/external/commercial")
}

/// Boot `path`, run it, and report the best frame it produced.
fn census(path: &Path) -> Option<Census> {
    let image = std::fs::read(path).ok()?;
    let mut sys = System::new(0);
    rom::hle_boot(&mut sys, &image).ok()?;

    let mut best_lit = 0usize;
    let mut dims = (0u32, 0u32);
    // Sized for the largest scan-out any supported mode produces, not for the
    // NTSC case in front of us: `scanout_scaled` returns `(0, 0)` on an
    // undersized destination, which would arrive here as "renders nothing".
    // PAL is 576 lines.
    let mut frame = vec![0u8; 720 * 576 * 4];
    for f in 1..=FRAMES {
        let target = sys
            .master_ticks()
            .saturating_add(rustyn64_core::MASTER_HZ / 60);
        sys.run_until(target);
        if !f.is_multiple_of(SAMPLE_EVERY) {
            continue;
        }
        let (w, h) = sys.bus.scanout_scaled(&mut frame);
        if w == 0 || h == 0 {
            continue;
        }
        let lit = frame
            .chunks_exact(4)
            .take((w * h) as usize)
            .filter(|px| px[0] != 0 || px[1] != 0 || px[2] != 0)
            .count();
        if lit > best_lit {
            best_lit = lit;
            dims = (w, h);
        }
    }
    Some(Census {
        name: path.file_stem()?.to_string_lossy().into_owned(),
        commands: sys.bus.rdp.commands_processed,
        dims,
        lit: best_lit,
    })
}

/// **Commercial titles render through their own graphics microcode** (T-71-003).
///
/// Skips loudly when no corpus is staged — a silent skip here would read as a
/// passing criterion, which is the failure this whole ledger entry is about.
#[test]
#[ignore = "local-only: reads the gitignored commercial corpus (ADR 0008)"]
fn commercial_titles_render_through_their_own_microcode() {
    let root = corpus_root();
    let Ok(folders) = std::fs::read_dir(&root) else {
        eprintln!(
            "SKIPPED (verified nothing): no corpus at {} -- stage ROMs there to \
             exercise T-71-003. This is NOT a pass.",
            root.display()
        );
        return;
    };

    let mut rows = Vec::new();
    for folder in folders.flatten() {
        let Ok(entries) = std::fs::read_dir(folder.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|x| x != "z64") {
                continue;
            }
            if let Some(row) = census(&path) {
                eprintln!(
                    "  {:<52} rdp={:<9} {}x{} lit={}{}",
                    row.name,
                    row.commands,
                    row.dims.0,
                    row.dims.1,
                    row.lit,
                    if row.renders() { "  RENDERS" } else { "" }
                );
                rows.push(row);
            }
        }
    }

    if rows.is_empty() {
        eprintln!("SKIPPED (verified nothing): no .z64 staged. This is NOT a pass.");
        return;
    }

    let rendering = rows.iter().filter(|r| r.renders()).count();
    eprintln!(
        "\n  {rendering} of {} staged titles render through their own microcode",
        rows.len()
    );
    let silent: Vec<&str> = rows
        .iter()
        .filter(|r| r.commands == 0)
        .map(|r| r.name.as_str())
        .collect();
    if !silent.is_empty() {
        eprintln!(
            "  {} issue ZERO RDP commands: {}",
            silent.len(),
            silent.join(", ")
        );
    }

    // The criterion: the microcode path works on real titles. Deliberately "at
    // least one", not a percentage -- the corpus is whatever a given machine has
    // staged, so a rate computed from it describes a ROM collection, not the
    // emulator. The per-title lines above and the committed screenshots are the
    // evidence a reader actually judges.
    assert!(
        rendering > 0,
        "no staged title rendered through its own graphics microcode -- \
         T-71-003 is not met (censused {} titles)",
        rows.len()
    );
}
