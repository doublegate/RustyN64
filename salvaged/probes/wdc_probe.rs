//! SCRATCH probe (task #52): why does World Driver Championship issue 176,085
//! RDP commands under `hle_boot` and ZERO under `real_pif_boot`?
//!
//! Not for commit. Deleted before any commit.
#![cfg(not(target_arch = "wasm32"))]
#![allow(missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]

use std::collections::BTreeMap;
use std::path::Path;

use rustyn64_core::System;
use rustyn64_test_harness::rom;

const TPF: u64 = rustyn64_core::MASTER_HZ / 60;

struct Snap {
    label: &'static str,
    rdp: u64,
    retired: u64,
    rdram_nonzero: usize,
    dims: (u32, u32),
    /// retiring PC -> (count, first word seen)
    top: Vec<(u64, u64, u32)>,
    rsp_pcs: usize,
    nmi: bool,
    ai_status: u32,
    ai_dacrate: u32,
    ai_len: u32,
}

/// Run `frames`, then cycle-step `probe_cycles` sampling the DC/WB latch — the
/// retiring position, not the fetch PC (ADR 0007).
fn run(label: &'static str, sys: &mut System, frames: u64, probe_cycles: u64) -> Snap {
    let mut rsp_seen = std::collections::BTreeSet::new();
    // Per-frame AI_STATUS census. One instantaneous FULL=1 says nothing: the
    // question is whether the bit EVER clears, so count both states and the
    // transitions between them.
    let mut full_set = 0u64;
    let mut full_clear = 0u64;
    let mut full_transitions = 0u64;
    let mut prev_full: Option<bool> = None;
    for _ in 0..frames {
        let target = sys.master_ticks().saturating_add(TPF);
        sys.run_until(target);
        rsp_seen.insert(sys.bus.rsp.pc());
        let full = (sys.bus.audio.read_reg(3) >> 31) & 1 == 1;
        if full { full_set += 1 } else { full_clear += 1 }
        if prev_full.is_some_and(|p| p != full) { full_transitions += 1 }
        prev_full = Some(full);
    }
    eprintln!(
        "  [{label}] AI FULL over {frames} frames: set={full_set} clear={full_clear} transitions={full_transitions}"
    );

    // Fine-grained retirement census: 2 master ticks = 1 CPU cycle.
    let mut hist: BTreeMap<u64, (u64, u32)> = BTreeMap::new();
    for _ in 0..probe_cycles {
        let t = sys.master_ticks().saturating_add(2);
        sys.run_until(t);
        let l = &sys.cpu.pipeline.dc_wb;
        if l.occupied {
            let e = hist.entry(l.pc & 0xFFFF_FFFF).or_insert((0, l.word));
            e.0 += 1;
        }
    }
    let mut top: Vec<(u64, u64, u32)> = hist.iter().map(|(&p, &(c, w))| (c, p, w)).collect();
    top.sort_by(|a, b| b.0.cmp(&a.0));
    top.truncate(12);

    let mut frame = vec![0u8; 720 * 576 * 4];
    let dims = sys.bus.scanout_scaled(&mut frame);

    Snap {
        label,
        rdp: sys.bus.rdp.commands_processed,
        retired: sys.cpu.retired,
        rdram_nonzero: sys.bus.rdram.iter().filter(|&&b| b != 0).count(),
        dims,
        top,
        rsp_pcs: rsp_seen.len(),
        nmi: sys.bus.boot_nmi_halt(),
        // AI register indices: 0 DRAM_ADDR, 1 LEN, 2 CONTROL, 3 STATUS, 4 DACRATE
        ai_status: sys.bus.audio.read_reg(3),
        ai_dacrate: sys.bus.audio.read_reg(4),
        ai_len: sys.bus.audio.read_reg(1),
    }
}

fn report(s: &Snap) {
    eprintln!(
        "\n=== {} ===\n  rdp={} retired={} rdram_nonzero={} scanout={}x{} rsp_pcs={} nmi={}",
        s.label, s.rdp, s.retired, s.rdram_nonzero, s.dims.0, s.dims.1, s.rsp_pcs, s.nmi
    );
    eprintln!(
        "  AI_STATUS={:#010x}  FULL(b31)={} BUSY(b30)={} ENABLED(b25)={}  AI_LEN={:#x} AI_DACRATE={:#x}",
        s.ai_status,
        (s.ai_status >> 31) & 1,
        (s.ai_status >> 30) & 1,
        (s.ai_status >> 25) & 1,
        s.ai_len,
        s.ai_dacrate,
    );
    eprintln!("  top retiring PCs (of the fine-grained cycle census):");
    for (c, pc, w) in &s.top {
        eprintln!("      {pc:#010x}  word={w:#010x}  n={c}");
    }
}

#[test]
#[ignore = "scratch"]
fn wdc_boot_differential() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/roms/external/commercial");
    let rom_path = base.join("controller-pak/World Driver Championship.z64");
    let Ok(image) = std::fs::read(&rom_path) else {
        eprintln!("WDC not staged — skipped");
        return;
    };
    let pif = std::fs::read(base.join("bios/pifdata.bin")).ok();

    let frames: u64 = std::env::var("WDC_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(420);

    let mut a = System::new(0);
    rom::hle_boot(&mut a, &image).expect("hle boot");
    let sa = run("hle_boot", &mut a, frames, 4000);
    report(&sa);

    if let Some(pif) = pif {
        let mut b = System::new(0);
        rom::real_pif_boot(&mut b, &image, &pif).expect("real-pif boot");
        let sb = run("real_pif_boot", &mut b, frames, 4000);
        report(&sb);

        eprintln!(
            "\n  DELTA: rdp {} -> {} | rdram {} -> {} | scanout {}x{} -> {}x{}",
            sa.rdp, sb.rdp, sa.rdram_nonzero, sb.rdram_nonzero,
            sa.dims.0, sa.dims.1, sb.dims.0, sb.dims.1
        );
    } else {
        eprintln!("no PIF ROM staged — real-PIF half skipped");
    }
}
