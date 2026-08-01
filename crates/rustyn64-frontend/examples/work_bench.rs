//! **Work units**, not time: how much the CPU, RSP and Bus actually *do* per
//! frame.
//!
//! `docs/performance.md` records that the RSP and Bus buckets appear to grow by
//! +12.1% and +13.5% under `fast-exec`, and says outright that **those two rows
//! must not be used to argue that anything got slower** until a work-unit count
//! exists. This is that count.
//!
//! The reason a profile cannot settle it: a sampled share cannot separate
//!
//! 1. *this code got slower*, from
//! 2. *this code ran more often*, from
//! 3. *the compiler charged it differently*.
//!
//! A per-frame work count separates (2) from the other two outright. Run it in
//! both modes and compare:
//!
//! ```text
//! # accurate
//! RUSTYN64_PROBE_ROM=/path/rom.z64 \
//!   cargo run --release --example work_bench --features work-counters
//!
//! # fast-exec
//! RUSTYN64_PROBE_ROM=/path/rom.z64 \
//!   cargo run --release --example work_bench --features work-counters,fast-exec,fast-scheduler
//! ```
//!
//! **If the counts match and the profile share moved, the share moved for a
//! reason other than work** — which would confirm the attribution-shift
//! hypothesis the document currently records as untested. If the counts differ,
//! the share moved because the machine is doing more, and by exactly how much.
//!
//! Times are printed too, but only so the two can be divided: **work per
//! millisecond** is the quantity that says whether a subsystem got faster,
//! and neither number alone does.

use std::time::Instant;

use rustyn64_frontend::emu::EmuCore;
use rustyn64_frontend::{FB_MAX_H, FB_MAX_W};

/// Frames to search for the VI coming up. Super Mario 64 takes 36.
const MAX_WARM: usize = 300;

/// Timed frames, matching `frame_bench` so the windows are comparable.
const FRAMES: u32 = 120;

/// Which execution mode this binary was built for.
///
/// Printed rather than inferred by the reader: the whole comparison is between
/// two builds, and a run whose mode is guessed from the command line that
/// *should* have produced it is how two `fast-exec` runs get compared against
/// each other.
const MODE: &str = match (
    cfg!(feature = "fast-exec"),
    cfg!(feature = "fast-scheduler"),
) {
    (true, true) => "fast-exec + fast-scheduler",
    (true, false) => "fast-exec",
    (false, true) => "fast-scheduler",
    (false, false) => "accurate",
};

fn main() {
    let path = std::env::var("RUSTYN64_PROBE_ROM").unwrap_or_else(|_| {
        panic!(
            "set RUSTYN64_PROBE_ROM: the committed homebrew ROM never brings the \
             VI up, so the machine would not reach steady state and the counts \
             would describe boot"
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
        "the VI never came up in {warm} frames — this would count the boot path, not a frame"
    );

    // Deltas across the timed window only. The counters are cumulative from
    // power-on, and boot is not steady state — a whole-run total would fold ~36
    // frames of a different workload into every figure.
    let cpu_before = core.system().cpu.retired;
    let rsp_before = core.system().bus.rsp.retired();
    let bus_before = core.system().bus.accesses();

    let t0 = Instant::now();
    for _ in 0..FRAMES {
        core.run_frame();
    }
    let total_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let cpu = core.system().cpu.retired - cpu_before;
    let rsp = core.system().bus.rsp.retired() - rsp_before;
    let bus = core.system().bus.accesses() - bus_before;

    // Each unit witnesses its own subsystem. A zero here is not a small number,
    // it is a broken measurement — and a table of zeros reads as a result.
    assert!(cpu > 0, "no CPU instructions retired in the timed window");
    assert!(
        rsp > 0,
        "no RSP instructions executed in the timed window; either the microcode \
         never ran or the counter is not wired up"
    );
    assert!(bus > 0, "no bus accesses serviced in the timed window");

    let frames = f64::from(FRAMES);
    #[allow(
        clippy::cast_precision_loss,
        reason = "work counts are far below 2^53 over 120 frames"
    )]
    let (cpu_f, rsp_f, bus_f) = (cpu as f64, rsp as f64, bus as f64);

    println!("rom={path}");
    println!("mode={MODE} frames={FRAMES} warm={warm}");
    println!("frame mean = {:.3} ms\n", total_ms / frames);
    println!("{:<24} {:>16} {:>14}", "work unit", "per frame", "per ms");
    println!(
        "{:<24} {:>16.0} {:>14.0}",
        "CPU instructions",
        cpu_f / frames,
        cpu_f / total_ms
    );
    println!(
        "{:<24} {:>16.0} {:>14.0}",
        "RSP instructions",
        rsp_f / frames,
        rsp_f / total_ms
    );
    println!(
        "{:<24} {:>16.0} {:>14.0}",
        "Bus accesses",
        bus_f / frames,
        bus_f / total_ms
    );
    println!(
        "\nCompare `per frame` across modes to answer whether a bucket does more \
         work.\n`per ms` is throughput and moves with the whole frame, so it \
         cannot answer that alone."
    );
}
