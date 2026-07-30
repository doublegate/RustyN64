//! Reconnaissance: does a retail title reach a *rendering* phase, and what does a
//! frame cost there?
//!
//! `frame_cost_probe` measures frames 37-66 of Super Mario 64 — early boot. A
//! source-line profile of that window put the **RDP at 0.00%**, which cannot be true
//! while anything is actually rasterizing, so the priorities derived from it are
//! suspect. This probe advances much further, mashing START/A the way a player would
//! to get past the title, and reports whether the presented frame is *changing* —
//! the cheap proxy for "something is drawing".
//!
//! Deliberately not a gate: it asserts nothing about progress, because whether a
//! given title reaches gameplay is exactly the open question (ledger R-18).
//!
//! ```text
//! RUSTYN64_PROBE_ROM=/path/rom.z64 RUSTYN64_PROBE_SKIP=900 \
//!   cargo test -p rustyn64-frontend --release --test gameplay_phase_probe \
//!   -- --ignored --nocapture
//! ```

use std::time::Instant;

use rustyn64_frontend::emu::EmuCore;
use rustyn64_frontend::input::{N64Buttons, bit};

/// Frames between samples of the presented frame's fingerprint.
const SAMPLE_EVERY: usize = 100;

/// Cheap order-sensitive fingerprint of the active frame, so two different images
/// are very unlikely to collide. Not a hash for correctness — only for "did this
/// change".
fn fingerprint(rgba: &[u8], w: u32, h: u32) -> u64 {
    let n = (w as usize).saturating_mul(h as usize).saturating_mul(4);
    let mut acc = 0xcbf2_9ce4_8422_2325u64;
    // Stride so a 600 KB frame costs microseconds, not milliseconds.
    for (i, b) in rgba[..n.min(rgba.len())].iter().enumerate().step_by(97) {
        acc ^= u64::from(*b) ^ (i as u64);
        acc = acc.wrapping_mul(0x100_0000_01b3);
    }
    acc
}

#[test]
#[ignore = "reconnaissance, minutes long, needs a local commercial ROM"]
fn does_a_retail_title_reach_a_rendering_phase() {
    let Ok(path) = std::env::var("RUSTYN64_PROBE_ROM") else {
        println!("set RUSTYN64_PROBE_ROM to a local ROM (never committed — ADR 0008)");
        return;
    };
    let skip: usize = std::env::var("RUSTYN64_PROBE_SKIP")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(900);

    let raw = std::fs::read(&path).unwrap_or_else(|e| panic!("probe ROM unreadable: {path}: {e}"));
    let mut core = EmuCore::new(0);
    core.load_rom(&raw)
        .unwrap_or_else(|e| panic!("probe ROM did not boot: {path}: {e:?}"));
    println!("ROM: {path}\nadvancing {skip} frames, mashing START/A\n");

    let mut prev_fp = 0u64;
    let mut distinct_frames = 0usize;
    let t_start = Instant::now();

    for f in 0..skip {
        // Mash: the title screen advances on a fresh press, so alternate held and
        // released blocks rather than toggling every frame.
        let phase = (f / 8) % 4;
        let mut b = N64Buttons::default();
        match phase {
            0 => b.set(bit::START, true),
            2 => b.set(bit::A, true),
            _ => {}
        }
        core.set_controllers([b.pack(), 0, 0, 0]);
        core.run_frame();

        if f % SAMPLE_EVERY == 0 || f + 1 == skip {
            let (rgba, w, h) = core.frame_rgba();
            let fp = fingerprint(rgba, w, h);
            let n = (w as usize).saturating_mul(h as usize).saturating_mul(4);
            let lit = rgba[..n.min(rgba.len())]
                .chunks_exact(4)
                .filter(|p| p[0] | p[1] | p[2] != 0)
                .count();
            let is_new = fp != prev_fp;
            if is_new {
                distinct_frames += 1;
            }
            println!(
                "frame {f:>5}  {w:>3}x{h:<3}  lit {lit:>7}  fp {fp:016x}  {}",
                if is_new { "CHANGED" } else { "static" }
            );
            prev_fp = fp;
        }
    }

    let el = t_start.elapsed();
    let per_frame = el.as_secs_f64() * 1000.0 / f64::from(u32::try_from(skip).unwrap_or(u32::MAX));
    println!(
        "\n{skip} frames in {el:.1?} ({per_frame:.1} ms/frame); content changed at \
         {distinct_frames} sampled points"
    );
}
