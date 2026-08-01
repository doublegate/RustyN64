//! Where the GPU present path's time actually goes, phase by phase.
//!
//! **This exists to size the remaining GPU work before any of it is built.** The
//! plan for the GPU backend has three further steps that each remove a
//! *different* phase of [`gpu_rdp::present`]:
//!
//! | planned work | phase it removes |
//! | --- | --- |
//! | share one Vulkan device with wgpu | `scanout` read-back **and** `copy` |
//! | asynchronous RDP (no fence wait) | the wait inside `scanout` |
//! | GPU VI scan-out | shifts work into `scanout`, out of the core |
//!
//! Argued against one aggregate "GPU present costs N ms" number, all three look
//! equally attractive and at most one of them can be right. This prints the
//! split, and prints it as a share of a real frame so the answer is in the same
//! units as the decision: *is this worth building at all?*
//!
//! The phases do **not** sum to the whole of `present` — the geometry checks,
//! the tap drain and the dirty-map read sit between them. The residual is
//! reported rather than hidden, because a large one would mean the phases are
//! mismeasuring what they name.
//!
//! ```text
//! RUSTYN64_PROBE_ROM=/path/rom.z64 \
//!   cargo run --release --example gpu_phase_bench --features gpu-rdp
//! ```
//!
//! A commercial ROM is required in practice: the committed `render_fill.z64`
//! never brings the VI up (see `docs/performance.md`), and without a picture the
//! GPU path produces nothing to measure. The harness says so rather than
//! printing zeros.

use std::time::Instant;

use rustyn64_frontend::emu::EmuCore;
use rustyn64_frontend::gpu_rdp::{Phase, phase_totals, reset_phase_totals};
use rustyn64_frontend::{FB_MAX_H, FB_MAX_W};

/// Frames to search for the VI coming up. Super Mario 64 takes 36.
const MAX_WARM: usize = 300;

/// Timed frames, matching `frame_bench` so the two are directly comparable.
const FRAMES: u32 = 120;

fn main() {
    let path = std::env::var("RUSTYN64_PROBE_ROM").unwrap_or_else(|_| {
        panic!(
            "set RUSTYN64_PROBE_ROM: the committed homebrew ROM never brings the \
             VI up, so the GPU path would produce no frames and every phase would \
             read zero — which looks like a result"
        )
    });
    let raw = std::fs::read(&path).unwrap_or_else(|e| panic!("bench ROM unreadable: {path}: {e}"));
    let mut core = EmuCore::new(0);
    core.load_rom(&raw)
        .unwrap_or_else(|e| panic!("bench ROM did not boot: {path}: {e:?}"));

    let mut buf = vec![0u8; (FB_MAX_W * FB_MAX_H * 4) as usize];
    let mut warm = 0usize;
    let live = loop {
        core.run_frame();
        warm += 1;
        let (w, h) = core.system().bus.scanout_scaled(&mut buf);
        if w > 0 && h > 0 {
            break true;
        }
        if warm >= MAX_WARM {
            break false;
        }
    };
    assert!(
        live,
        "the VI never came up in {warm} frames — this would time the boot path, not a frame"
    );

    // After warm-up, so the first frame's whole-of-RDRAM stage and the Vulkan
    // device build are excluded. Including them overstates `stage` by orders of
    // magnitude, and a benchmark that flatters the phase a previous change
    // already optimized is worse than no benchmark.
    reset_phase_totals();
    let gpu_before = core.gpu_frames();
    let t0 = Instant::now();
    for _ in 0..FRAMES {
        core.run_frame();
    }
    let total_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let mean_ms = total_ms / f64::from(FRAMES);

    // Witness that the GPU actually produced the frames. Without this the phases
    // would all read zero on a machine with no Vulkan device and the run would
    // print a tidy table of nothing — the vacuous-pass shape this repo has
    // shipped before.
    let gpu_frames = core.gpu_frames() - gpu_before;
    assert!(
        gpu_frames > 0,
        "the GPU backend produced no frames in the timed window, so every phase \
         below would be zero. Either there is no usable Vulkan device or the \
         backend fell back to software; this is not a measurement"
    );

    let (_, counted) = phase_totals(Phase::Stage);
    assert!(counted > 0, "no frames reached the end of present");

    println!("rom={path}");
    println!("frames={FRAMES} warm={warm} gpu_frames={gpu_frames} counted={counted}");
    println!("frame mean = {mean_ms:.3} ms\n");
    println!("{:<10} {:>10} {:>10}", "phase", "ms/frame", "% of frame");

    let mut summed_ms = 0.0;
    for phase in Phase::ALL {
        let (ns, frames) = phase_totals(phase);
        #[allow(
            clippy::cast_precision_loss,
            reason = "ns and frame counts are far below 2^53"
        )]
        let ms = ns as f64 / 1.0e6 / frames as f64;
        summed_ms += ms;
        println!(
            "{:<10} {:>10.4} {:>9.3}%",
            phase.name(),
            ms,
            ms / mean_ms * 100.0
        );
    }
    println!(
        "{:<10} {:>10.4} {:>9.3}%   <- all of present that is measured",
        "TOTAL",
        summed_ms,
        summed_ms / mean_ms * 100.0
    );

    println!(
        "\nCeiling if the whole present path became free: {:.3}x",
        mean_ms / (mean_ms - summed_ms)
    );
}
