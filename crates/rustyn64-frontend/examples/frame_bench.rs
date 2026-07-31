//! Headless frame-cost benchmark — the same measurement as `frame_cost_probe`,
//! shaped as a **binary** so it can be profiled and rebuilt with PGO.
//!
//! It exists because that probe is a `#[test]`, and a test cannot be built with
//! `-Cprofile-use` in this workspace: tests require `panic = "unwind"` while the
//! release profile is `panic = "abort"`, and `RUSTFLAGS` applies the PGO flag to
//! every crate in the graph, so the two collide:
//!
//! ```text
//! error: the crate `rustyn64_frontend` requires panic strategy `abort`
//!        which is incompatible with this crate's strategy of `unwind`
//! ```
//!
//! That is not stale artifacts — it survives `cargo clean --release`. The binary
//! path has no such requirement and builds cleanly under `-Cprofile-use`, which is
//! what makes this file the difference between "PGO is untested" and a number.
//!
//! An **example**, not a `[[bin]]`, so a plain `cargo build` does not carry a
//! benchmark into every developer's build while `cargo build --example` still
//! reaches it.
//!
//! ```text
//! RUSTYN64_PROBE_ROM=/path/rom.z64 \
//!   cargo run --release --example frame_bench --features fast-scheduler
//! ```
//!
//! Prints the mean over [`FRAMES`] frames after the VI comes up. Warm-up frames are
//! excluded on purpose: they are boot, not steady state, and averaging them in was
//! how an earlier probe attributed boot cost to a render-phase number.

use std::time::Instant;

use rustyn64_frontend::emu::EmuCore;
use rustyn64_frontend::{FB_MAX_H, FB_MAX_W};

/// Same committed CC0 homebrew default as `frame_cost_probe`, resolved at compile
/// time so a cwd change cannot silently miss it.
const DEFAULT_ROM: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/roms/homebrew/render_fill.z64"
);

/// Frames to search for the VI coming up. Super Mario 64 takes 36.
const MAX_WARM: usize = 300;

/// Timed frames. More than `frame_cost_probe`'s 30, because this is the harness a
/// PGO comparison is decided on and the extra samples cost seconds.
///
/// `u32` rather than `usize` so the division below is an exact widening to `f64`
/// rather than a `usize as f64` that clippy correctly flags as lossy above 2^52.
const FRAMES: u32 = 120;

fn main() {
    let path = std::env::var("RUSTYN64_PROBE_ROM").unwrap_or_else(|_| DEFAULT_ROM.to_string());
    // A missing or unbootable ROM aborts rather than printing a number over no
    // measurement — the vacuous-pass hazard this repo has shipped before.
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

    // `run_frame` already includes the scan-out: it is `run_until` + `produce_frame`
    // (which calls `scanout_scaled`) + `produce_audio`. The extra `scanout_scaled`
    // above is only the VI-liveness probe for the warm-up loop and is deliberately
    // outside the timed region — counting it there would bill the presentation
    // pipeline twice. Raised in review, which read the warm-up call as the only one.
    let retired_before = core.system().cpu.retired;
    let t0 = Instant::now();
    for _ in 0..FRAMES {
        core.run_frame();
    }
    let mean_ms = t0.elapsed().as_secs_f64() * 1000.0 / f64::from(FRAMES);

    // Witness that the TIMED window executed, not merely that the process did: the
    // cumulative counter includes the warm-up, so a timed loop that stalled entirely
    // would still show millions retired and report a very fast "frame".
    let retired = core.system().cpu.retired - retired_before;
    assert!(
        retired > 0,
        "no instructions retired during the timed window — the measurement is vacuous"
    );

    println!("frames={FRAMES} warm={warm} retired={retired} mean={mean_ms:.3}ms");
}
