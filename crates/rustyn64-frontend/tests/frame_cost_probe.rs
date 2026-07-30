//! Where does a frame's wall-clock time actually go?
//!
//! The P1 measurement, and it exists because the P0 present-handoff fix
//! (`docs/frontend.md` §Present handoff) made the GUI responsive without making it
//! fast: menu latency went from 15-45 s to ~1 s, while presentation only reached
//! ~1 frame/second against a predicted ~8. That means the per-frame cost is far
//! larger than the headless corpus census implied, and the first question is which
//! phase owns it — emulation (`System::run_until`) or presentation
//! (`Bus::scanout_scaled`, the accurate VI pipeline, which the census never ran).
//!
//! Measure before optimizing (module 30): this test answers that question and
//! asserts almost nothing, so it can never gate CI on a timing number.
//!
//! ```text
//! cargo test -p rustyn64-frontend --release --test frame_cost_probe -- --ignored --nocapture
//! ```
//!
//! `RUSTYN64_PROBE_ROM` overrides the ROM. Commercial images are never committed
//! (ADR 0008), so a local dump is passed by path rather than added to the repo.

use std::time::{Duration, Instant};

use rustyn64_frontend::emu::EmuCore;
use rustyn64_frontend::{FB_MAX_H, FB_MAX_W};

/// A committed CC0 homebrew ROM that actually drives the RDP, so the render path
/// is exercised rather than idling.
///
/// Resolved from `CARGO_MANIFEST_DIR` at compile time rather than as a
/// cwd-relative path: `cargo test` happens to run integration tests from the
/// package root, but nothing guarantees it, and a path that silently misses is
/// exactly how this probe would go back to reporting a pass over no measurement.
const DEFAULT_ROM: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/roms/homebrew/render_fill.z64"
);

/// Frames to advance looking for the VI to come up before giving up.
///
/// Super Mario 64 takes 36, so 300 is ample headroom. It is not larger because the
/// committed default ROM is bare-metal and never programs the VI at all — at ~58 ms
/// a frame, a 900-frame search cost 53 seconds to conclude nothing.
const MAX_WARM: usize = 300;

fn percentiles(label: &str, mut v: Vec<Duration>) -> Duration {
    assert!(!v.is_empty(), "{label}: no samples to summarize");
    v.sort_unstable();
    let mean = v.iter().sum::<Duration>() / u32::try_from(v.len()).unwrap_or(1);
    // No p99 column: at 30 samples `30 * 99 / 100` is index 29, so it would be
    // identical to max and imply resolution the sample size cannot carry.
    println!(
        "{label:>22}: mean {:>9.3?}  p50 {:>9.3?}  max {:>9.3?}",
        mean,
        v[v.len() / 2],
        v[v.len() - 1],
    );
    mean
}

#[test]
#[ignore = "a measurement, not a gate; machine-specific and seconds long"]
fn where_does_a_frame_go() {
    let path = std::env::var("RUSTYN64_PROBE_ROM").unwrap_or_else(|_| DEFAULT_ROM.to_string());
    // A missing or unbootable ROM is a FAILURE, not a skip. Returning `()` from a
    // #[test] marks it `ok`, so the earlier early-return meant this probe reported a
    // pass having measured nothing whatsoever -- the vacuous-pass hazard its own
    // module doc warns about, reproduced in the warning's own file. The default ROM
    // is committed, so it is always present; an override that names a missing file is
    // a caller error and must say so.
    let raw = std::fs::read(&path).unwrap_or_else(|e| panic!("probe ROM unreadable: {path}: {e}"));
    let mut core = EmuCore::new(0);
    core.load_rom(&raw)
        .unwrap_or_else(|e| panic!("probe ROM did not boot: {path}: {e:?}"));
    println!("ROM: {path} ({} bytes)", raw.len());

    let mut buf = vec![0u8; (FB_MAX_W * FB_MAX_H * 4) as usize];

    // Advance until the VI is actually programmed.
    //
    // This is load-bearing and the first version of this probe got it wrong: a
    // retail title spends its early frames in the OS boot before it touches the VI
    // registers, so `scanout_scaled` returns `(0, 0)` and **early-returns**. Measured
    // there it reported 26-239 ns and "0.0% of a frame", which attributes nothing —
    // it timed the guard, not the pipeline. A number produced before the subsystem
    // under test is running is the vacuous-pass hazard, so the window is found first
    // and reported, and a run that never gets there refuses to print the column.
    let mut warm = 0usize;
    let vi_dims = loop {
        core.run_frame();
        warm += 1;
        let (w, h) = core.system().bus.scanout_scaled(&mut buf);
        if w > 0 && h > 0 {
            break Some((w, h));
        }
        if warm >= MAX_WARM {
            break None;
        }
    };
    match vi_dims {
        Some((w, h)) => println!("VI came up after {warm} frames, scanning out {w}x{h}"),
        None => println!("VI NEVER came up in {warm} frames — scanout column omitted"),
    }

    let mut whole_frame = Vec::new();
    let mut scanout = Vec::new();

    for _ in 0..30 {
        // `run_frame` = run_until + produce_frame (which calls scanout_scaled) +
        // produce_audio.
        let t0 = Instant::now();
        core.run_frame();
        whole_frame.push(t0.elapsed());

        // The presentation phase in isolation. Timed separately (so it is counted
        // twice overall) purely to attribute cost; `scanout_scaled` takes `&self`
        // and mutates nothing, so calling it again is side-effect free.
        let t1 = Instant::now();
        let _ = core.system().bus.scanout_scaled(&mut buf);
        scanout.push(t1.elapsed());
    }

    println!("--- per-frame cost over {} frames ---", whole_frame.len());
    let frame_mean = percentiles("run_frame (total)", whole_frame);
    if vi_dims.is_some() {
        let scanout_mean = percentiles("scanout_scaled only", scanout);
        let pct = scanout_mean.as_secs_f64() / frame_mean.as_secs_f64() * 100.0;
        println!("scanout_scaled is {pct:.1}% of a frame (VI live)");
    } else {
        println!("     scanout_scaled: NOT MEASURED (VI never programmed)");
    }
    // Checked BEFORE the division below, so a zero mean cannot be formatted as
    // `inf FPS` -- a probe that measured nothing must not print a number at all.
    assert!(
        frame_mean > Duration::ZERO,
        "a measured frame must take nonzero time"
    );
    println!(
        "implied ceiling: {:.2} FPS (frame mean {:.1?}); 60 FPS needs {:.1?}",
        1.0 / frame_mean.as_secs_f64(),
        frame_mean,
        Duration::from_secs_f64(1.0 / 60.0),
    );
}
