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
    // The histogram is cumulative from power-on like the counters above, so it
    // needs the same treatment: a reviewer caught it being reported RAW, which
    // folded ~36 warm-up frames into a table captioned "120 frames".
    let vu_before: [u64; 64] = *core.system().bus.rsp.vu_funct_histogram();

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

    // A zero window would make every `per ms` column `inf`, which prints as a
    // result. 120 frames of a real ROM cannot take zero time, so this is a
    // broken-measurement guard rather than a plausible case — and it says which.
    assert!(
        total_ms > 0.0,
        "the timed window measured {total_ms} ms; the clock is not usable here"
    );
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

    report_vu_histogram(&core, &vu_before);
    report_commit_census(&core);
    report_rcp_occupancy(&core);
}

/// **How often each RCP chip is stepped with nothing to do.**
///
/// The whole value of an event-driven scheduler is skipping idle chips, so its
/// ceiling *is* the idle fraction. A profile share cannot supply that: it says
/// what the RSP costs when it runs, not how often it is stepped while halted.
/// This is the number that decides whether the phase is worth building.
fn report_rcp_occupancy(core: &EmuCore) {
    use rustyn64_core::occupancy as occ;
    let c = core.system().rcp_occupancy;
    let steps = c[occ::STEPS];
    assert!(
        steps > 0,
        "no RCP steps were censused — the counter is not wired into the stepping \
         path, and a table of zeros reads as a result"
    );
    #[allow(
        clippy::cast_precision_loss,
        reason = "step counts over a bench run are far below 2^53"
    )]
    let pct = |n: u64| n as f64 / steps as f64 * 100.0;
    println!("\nRCP occupancy over {steps} steps:");
    println!(
        "  RSP halted   {:>12}  {:>6.2}%",
        c[occ::RSP_HALTED],
        pct(c[occ::RSP_HALTED])
    );
    println!(
        "  RDP idle     {:>12}  {:>6.2}%",
        c[occ::RDP_IDLE],
        pct(c[occ::RDP_IDLE])
    );
    println!(
        "\nAn event scheduler can skip only the idle steps, so those percentages \
         are its ceiling\nfor each chip — not the profile share the chip occupies \
         when it does run."
    );
}

/// **How much of the instruction stream a direct commit path could actually
/// serve.**
///
/// `fast-exec` builds a 120-byte `Latch` for every instruction and runs it
/// through the accurate path's `wb_stage` — deliberately, because that is what
/// keeps COP0, COP1, TLB and retirement semantics identical in both modes
/// without a second implementation. A direct commit would skip that, but only
/// safely for instructions that touch none of it.
///
/// This is the fraction that decides whether such a path is worth writing.
/// Sizing it on an assumed "most instructions are simple ALU ops" is exactly the
/// move that produced four reverted changes in the previous program.
fn report_commit_census(core: &EmuCore) {
    use rustyn64_core::cpu::commit_class as cc;
    let census = core.system().cpu.pipeline.commit_census();
    let total: u64 = census.iter().sum();
    assert!(
        total > 0,
        "no instructions were classified — the census is not wired into the \
         executing path, and a table of zeros reads as a result"
    );
    #[allow(
        clippy::cast_precision_loss,
        reason = "counts over a bench run are far below 2^53"
    )]
    let pct = |n: u64| n as f64 / total as f64 * 100.0;
    let names = [
        (cc::SIMPLE, "SIMPLE  (GPR or nothing)"),
        (cc::MEM, "MEM     (has a DC access)"),
        (cc::COP, "COP     (COP0/COP1)"),
        (cc::HILO, "HILO    (mul/div)"),
        (cc::OTHER, "OTHER"),
    ];
    println!("\ncommit classes over {total} retired instructions:");
    for (idx, label) in names {
        println!(
            "  {label:<28} {:>12}  {:>6.2}%",
            census[idx],
            pct(census[idx])
        );
    }
    println!(
        "\nA direct commit path could serve the SIMPLE class only: {:.2}%.\n\
         That share bounds what skipping the Latch can win — the rest must keep \
         going through wb_stage.",
        pct(census[cc::SIMPLE])
    );
}

/// Print which COP2 computational ops a real workload actually runs.
///
/// Split from `main` because the two are separate questions — how much
/// work each subsystem does, and which VU operations that work consists of
/// — and together they exceed the line-count gate.
fn report_vu_histogram(core: &EmuCore, before: &[u64; 64]) {
    // WHICH VU operations a real workload runs. `vu.rs` is 143 functions;
    // vectorizing it means `unsafe` intrinsics, so hand-writing all of them to
    // recover the cost of the few that matter would be the expensive way round.
    // The DELTA over the timed window, matching every other figure this
    // harness prints. Reporting the raw cumulative counters would describe boot
    // plus the window, under a caption naming only the window.
    let hist: [u64; 64] =
        core::array::from_fn(|i| core.system().bus.rsp.vu_funct_histogram()[i] - before[i]);
    let total: u64 = hist.iter().sum();
    assert!(
        total > 0,
        "no COP2 computational instructions executed in the timed window; the \
histogram would be a table of zeros, which reads as a result"
    );
    #[allow(
        clippy::cast_precision_loss,
        reason = "counts are far below 2^53 over 120 frames"
    )]
    let total_f = total as f64;
    let mut ranked: Vec<(usize, u64)> = hist
        .iter()
        .enumerate()
        .filter_map(|(i, &n)| (n > 0).then_some((i, n)))
        .collect();
    ranked.sort_unstable_by_key(|&(_, n)| core::cmp::Reverse(n));

    println!(
        "\nCOP2 computational ops actually executed ({total} total, {} distinct):",
        ranked.len()
    );
    println!(
        "{:<10} {:>14} {:>9} {:>9}",
        "funct", "count", "share", "cumul"
    );
    let mut cumulative = 0.0;
    for (funct, n) in ranked.iter().take(12) {
        #[allow(
            clippy::cast_precision_loss,
            reason = "counts are far below 2^53 over 120 frames"
        )]
        let share = *n as f64 / total_f * 100.0;
        cumulative += share;
        println!("{funct:#04x}       {n:>14} {share:>8.2}% {cumulative:>8.2}%");
    }

    // The rollup that decides where vectorization goes. `funct` 0x00..=0x0F is
    // the whole multiply/multiply-accumulate family, and it is dispatched by ONE
    // function (`multiply_lane`) — so its share is the share a single
    // vectorized function would cover.
    let mad: u64 = hist[0x00..=0x0F].iter().sum();
    #[allow(
        clippy::cast_precision_loss,
        reason = "counts are far below 2^53 over 120 frames"
    )]
    let mad_share = mad as f64 / total_f * 100.0;
    println!(
        "\nfunct 0x00..=0x0F (the multiply/accumulate family, one dispatch \
         function): {mad} = {mad_share:.2}%"
    );
}
