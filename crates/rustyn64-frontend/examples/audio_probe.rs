//! **Where does the audio actually stop?** — evidence for the ~1 s on / ~1 s off
//! report, gathered at each boundary rather than reasoned about.
//!
//! `crates/rustyn64-frontend/src/audio.rs` currently *asserts* the cause in its
//! module header: the producer stages one emulated frame of audio per emulated
//! frame while the device consumes in wall-clock time, so supply is `fps / 60`
//! and everything else is downstream of throughput. That is a plausible
//! mechanism and it has never been measured. This probe measures it.
//!
//! It also tests a **competing hypothesis the header does not consider**: that
//! the *emulated* AI is itself gapping — the game's audio DMA starving inside
//! the machine — which would produce silence no host-side buffering could fix
//! and would be a genuine emulation defect rather than a speed consequence.
//! Those two have different signatures and this separates them:
//!
//! | | host starvation | emulated-AI gap |
//! | --- | --- | --- |
//! | frames carrying audible samples | **all of them** | only some |
//! | `Audio::underruns` | flat | climbing |
//! | silence period | set by the pacer (~`MAX_CATCHUP_FRAMES` / fps) | set by the game |
//!
//! The reported ~1 s period is the thing to explain, and neither hypothesis
//! predicts it on its face — 3 catch-up frames at ~15 FPS is a ~200 ms cycle,
//! not a 2 s one. Printing the actual run lengths is the point.
//!
//! ```text
//! RUSTYN64_PROBE_ROM=/path/rom.z64 \
//!   cargo run --release --example audio_probe --features fast-exec,fast-scheduler
//! ```
//!
//! No audio device is opened and none is needed: every quantity here is on the
//! core side of `AudioRing::push`, which is exactly the half the header's claim
//! is about.

use std::time::Instant;

use rustyn64_frontend::emu::EmuCore;
use rustyn64_frontend::{FB_MAX_H, FB_MAX_W};

/// Frames to search for the VI coming up. Super Mario 64 takes 36.
const MAX_WARM: usize = 300;

/// Timed frames. At ~15 FPS this is ~8 s of wall clock and ~5 s of emulated
/// audio — several periods of the reported ~1 s cycle, which a shorter window
/// could straddle without ever showing one.
const FRAMES: usize = 120;

/// The host rate the resampler targets. Fixed rather than device-negotiated so
/// the run is reproducible without a sound card; 48 kHz is `EmuCore`'s default.
const OUTPUT_RATE: u32 = 48_000;

/// Below this peak amplitude a frame is treated as silence. Not zero: the AI
/// decays the held sample toward zero on underrun rather than snapping to it,
/// so a strict `== 0.0` test would call a decaying tail "audible" and hide the
/// very gaps this probe exists to find.
const SILENCE_FLOOR: f32 = 1.0e-4;

/// One emulated frame's audio, reduced to what distinguishes the hypotheses.
struct FrameAudio {
    /// Interleaved stereo samples staged for the ring.
    samples: usize,
    /// Peak absolute amplitude in the frame.
    peak: f32,
    /// `Audio::underruns` after this frame.
    underruns: u64,
}

fn main() {
    let path = std::env::var("RUSTYN64_PROBE_ROM").unwrap_or_else(|_| {
        panic!(
            "set RUSTYN64_PROBE_ROM: the committed homebrew ROMs do not run a \
             game audio engine, so a run against one would measure silence and \
             prove nothing about the reported hiccup"
        )
    });
    let raw = std::fs::read(&path).unwrap_or_else(|e| panic!("probe ROM unreadable: {path}: {e}"));
    let mut core = EmuCore::new(0);
    core.set_output_rate(OUTPUT_RATE);
    core.load_rom(&raw)
        .unwrap_or_else(|e| panic!("probe ROM did not boot: {path}: {e:?}"));

    // Warm to a live VI, matching every other harness here: boot is not steady
    // state, and audio during boot is not what was reported.
    let mut buf = vec![0u8; (FB_MAX_W * FB_MAX_H * 4) as usize];
    let mut warm = 0usize;
    loop {
        core.run_frame();
        drop(core.drain_audio());
        warm += 1;
        let (w, h) = core.system().bus.scanout_scaled(&mut buf);
        if (w > 0 && h > 0) || warm >= MAX_WARM {
            break;
        }
    }

    let t0 = Instant::now();
    let mut log = Vec::with_capacity(FRAMES);
    for _ in 0..FRAMES {
        core.run_frame();
        let samples = core.drain_audio();
        let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        log.push(FrameAudio {
            samples: samples.len(),
            peak,
            underruns: core.audio_underruns(),
        });
    }
    let wall = t0.elapsed().as_secs_f64();

    report(&path, &log, wall, core.system().bus.audio.sample_rate());
}

/// Print the evidence. Split from `main` so the measurement and its
/// presentation are separable, and because together they exceed the line gate.
#[allow(
    clippy::cast_precision_loss,
    reason = "sample counts over 120 frames are far below 2^53"
)]
fn report(path: &str, log: &[FrameAudio], wall: f64, in_rate: u32) {
    let total_samples: usize = log.iter().map(|f| f.samples).sum();
    let audible = log.iter().filter(|f| f.peak > SILENCE_FLOOR).count();
    let underruns = log.last().map_or(0, |f| f.underruns) - log.first().map_or(0, |f| f.underruns);

    // Emulated audio produced, against wall-clock elapsed. THIS is the header's
    // claim, stated as a ratio it can be checked against.
    let produced_secs = (total_samples / 2) as f64 / f64::from(OUTPUT_RATE);
    let supply = produced_secs / wall;
    let fps = log.len() as f64 / wall;

    println!("rom={path}");
    println!(
        "frames={} wall={wall:.3}s fps={fps:.2} AI rate={in_rate} Hz host rate={OUTPUT_RATE} Hz",
        log.len()
    );
    println!();
    println!("--- boundary 1: does the emulated AI produce continuous audio? ---");
    println!(
        "  frames carrying audible samples : {audible}/{} ({:.1}%)",
        log.len(),
        audible as f64 / log.len() as f64 * 100.0
    );
    println!("  AI underruns over the window    : {underruns}");
    println!(
        "  samples staged per frame        : {:.0} (expected {:.0} = rate/60)",
        total_samples as f64 / log.len() as f64,
        f64::from(OUTPUT_RATE) / 60.0 * 2.0
    );
    println!();
    println!("--- boundary 2: is supply enough for a real-time device? ---");
    println!("  emulated audio produced         : {produced_secs:.3} s");
    println!("  wall clock elapsed              : {wall:.3} s");
    println!(
        "  supply ratio                    : {:.1}%  (fps/60 = {:.1}%)",
        supply * 100.0,
        fps / 60.0 * 100.0
    );
    println!();
    print_runs(log);
}

/// Print the run-length timeline: the reported symptom is a *period*, and only
/// run lengths can confirm or refute a ~1 s one.
#[allow(
    clippy::cast_precision_loss,
    reason = "run lengths over 120 frames are far below 2^53"
)]
fn print_runs(log: &[FrameAudio]) {
    println!("--- boundary 3: what is the actual period of the gaps? ---");
    let mut runs: Vec<(bool, usize)> = Vec::new();
    for f in log {
        let loud = f.peak > SILENCE_FLOOR;
        match runs.last_mut() {
            Some((kind, n)) if *kind == loud => *n += 1,
            _ => runs.push((loud, 1)),
        }
    }
    if runs.len() == 1 {
        println!(
            "  no alternation at all: {} frames, all {}",
            log.len(),
            if runs[0].0 { "audible" } else { "silent" }
        );
        println!("  => the emulated stream does NOT gap; any chopping is host-side.");
        return;
    }
    let silent: Vec<usize> = runs.iter().filter(|r| !r.0).map(|r| r.1).collect();
    let loud: Vec<usize> = runs.iter().filter(|r| r.0).map(|r| r.1).collect();
    let mean = |v: &[usize]| {
        if v.is_empty() {
            0.0
        } else {
            v.iter().sum::<usize>() as f64 / v.len() as f64
        }
    };
    println!(
        "  runs: {} audible (mean {:.1} frames = {:.0} ms emulated), \
         {} silent (mean {:.1} frames = {:.0} ms emulated)",
        loud.len(),
        mean(&loud),
        mean(&loud) * 1000.0 / 60.0,
        silent.len(),
        mean(&silent),
        mean(&silent) * 1000.0 / 60.0,
    );
    print!("  timeline: ");
    for f in log {
        print!("{}", if f.peak > SILENCE_FLOOR { '#' } else { '.' });
    }
    println!();
}
