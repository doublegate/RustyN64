//! PeterLemon **CPU / CP1 instruction-timing** oracle (a curated, self-judging
//! timing ROM — the practical alternative to n64-systemtest's monolithic,
//! non-terminating `--features timing` build).
//!
//! `CPUTIMINGNTSC` / `CP1TIMINGNTSC` (krom / Peter Lemon, Unlicense) each time a
//! fixed loop of one instruction with the COP0 `Count` register, compare the
//! measured delta against a **hardware-expected value baked into the ROM**
//! (`ADDCOUNT: dw $DB1F`, …), and draw the label in **green (pass)** or **red
//! (fail)**. So the frame itself is the verdict: an all-green frame means our
//! cycle timing matches hardware for every instruction it covers; any red text
//! means a mismatch — exactly the C-1 (`M`) / C-29 (FPU rates) signal.
//!
//! This runner boots the ROM, runs until it has drawn its result frame, and
//! reports how many red (fail) vs green (pass) glyph pixels the scan-out
//! contains. It is a **measurement**, `#[ignore]`d — it does not yet assert
//! all-green, because the timing residuals are open (Stage D). Its job today is
//! to prove the ROM runs to a verdict in our emulator (unlike the hanging
//! n64-systemtest timing suite) and to quantify the gap.
//!
//! ```text
//! cargo test -p rustyn64-test-harness --release --test peterlemon_timing -- --ignored --nocapture
//! ```

// `r`/`g`/`b` pixel channels and `w`/`h` scan-out dims read best as their
// conventional single letters here; "PeterLemon" is a name, not a code item.
#![allow(clippy::many_single_char_names, clippy::doc_markdown)]

use rustyn64_core::System;
use rustyn64_test_harness::rom;

// Anchored at the crate manifest dir so resolution is independent of the CWD
// `cargo test` / `cargo test --workspace` runs from.
const CPU_ROM: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/roms/peterlemon-timing/CPUTIMINGNTSC.z64"
);
const CP1_ROM: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/roms/peterlemon-timing/CP1TIMINGNTSC.z64"
);

/// Master ticks to run before giving up. These ROMs time a few dozen loops then
/// draw once and spin; generous so the whole battery + draw completes.
const BUDGET_TICKS: u64 = 2_000_000_000;

/// A lit glyph pixel is classified by its dominant channel: the fonts are
/// saturated red / green on black.
const CHANNEL_HI: u8 = 160;
/// The other two channels must be below this for a clean red/green classification.
const CHANNEL_LO: u8 = 96;
/// Enough drawn glyph pixels to count as "reached a verdict grid" (CPU variant).
const MIN_VERDICT_PIXELS: usize = 100;
/// Enough retired instructions to count as "executed the battery" (FPU variant).
const MIN_BATTERY_RETIRED: u64 = 100_000_000;

/// Boot a PeterLemon timing ROM, run it to its result frame, and return
/// `(red_pixels, green_pixels, retired, vi_programmed)`.
fn run(path: &str) -> (usize, usize, u64, bool) {
    let image = std::fs::read(path).unwrap_or_else(|_| panic!("committed timing ROM {path}"));
    let entry = rom::entry_point(&image).expect("readable N64 header");

    let mut sys = System::new(0);
    // Bare-metal flat load (the ROM carries its own setup; the same path
    // `render_fill` boots through), sign-extended entry.
    rom::load_direct(&mut sys, &image, entry).expect("loadable");

    // The ROM programs the VI at startup (before running its timing battery),
    // then draws each instruction's green/red verdict *as it goes* and spins at
    // the end. So run the whole budget and scan the FINAL frame — scanning right
    // after VI_ORIGIN is first set would catch a half-drawn (or empty) frame,
    // which is why the FPU variant (slower ops) needs the full run. Witness that
    // the VI actually got programmed so an all-black "did not run" can't pass.
    let deadline = sys.master_ticks() + BUDGET_TICKS;
    let mut vi_programmed = false;
    while sys.master_ticks() < deadline {
        sys.step_to_next_edge();
        if !vi_programmed && sys.bus.vi.read(rustyn64_core::vi::VI_ORIGIN) != 0 {
            vi_programmed = true;
        }
    }

    let mut rgba = vec![0u8; 640 * 480 * 4];
    let (w, h) = sys.bus.scanout(&mut rgba);
    let (mut red, mut green) = (0usize, 0usize);
    if w > 0 && h > 0 {
        let n = (w as usize * h as usize * 4).min(rgba.len());
        for px in rgba[..n].chunks_exact(4) {
            let (r, g, b) = (px[0], px[1], px[2]);
            if r > CHANNEL_HI && g < CHANNEL_LO && b < CHANNEL_LO {
                red += 1;
            } else if g > CHANNEL_HI && r < CHANNEL_LO && b < CHANNEL_LO {
                green += 1;
            }
        }
    }
    (red, green, sys.cpu.retired, vi_programmed)
}

/// The CPU instruction-timing oracle. Asserts the ROM **drew a verdict grid**
/// (a substantial green/red glyph area — a real execution witness, not a vacuous
/// "it started"; the ROM spins after drawing, so the full-budget run captures the
/// finished frame), then reports the pass/fail pixel counts.
///
/// This is an **aggregate** verdict, not a per-instruction measurement: an
/// all-red frame says our `Count` deltas do not match the ROM's baked-in expected
/// values for the covered instructions, but it does not by itself isolate C-1
/// (`M`) or prove each instruction is individually wrong (see ledger §C-1). The
/// falsifiable target is all-green; deriving `M` needs the differential
/// measured-vs-expected deltas, a Stage-D follow-up.
#[test]
#[ignore = "curated timing oracle; an aggregate measurement, run explicitly"]
fn cpu_timing_rom_runs_to_a_verdict() {
    let (red, green, retired, vi) = run(CPU_ROM);
    assert!(
        vi && (red + green) > MIN_VERDICT_PIXELS,
        "CPUTIMINGNTSC did not draw a verdict grid (red={red}, green={green}, \
         retired={retired}, vi={vi}) — no measurement to report"
    );
    println!(
        "CPUTIMINGNTSC: {green} green (pass) vs {red} red (fail) glyph pixels, \
         {retired} retired. Aggregate verdict — all-green ⇒ every covered \
         instruction's timing matches the ROM's expected `Count` delta; red ⇒ a \
         mismatch (not yet an isolated `M` measurement, ledger §C-1). Stage D \
         drives it to all-green."
    );
}

/// The FPU (COP1) timing variant — the direct C-29 (FPU op stall-rate) oracle.
///
/// This one asserts only that it **executes without hanging** (which is itself
/// the point: n64-systemtest's `--features timing` build does not terminate in
/// the emulator, this ROM runs ~10⁹ instructions cleanly). Its FPU battery is
/// slow (DIV/SQRT are 29–58 cycles each), so it does not always draw its full
/// verdict grid inside the shared budget yet; the pixel counts are reported for
/// whatever it drew. Wiring it to a full verdict is Stage-D follow-up.
#[test]
#[ignore = "curated timing oracle (FPU); runs without hanging, run explicitly"]
fn cp1_timing_rom_executes_without_hanging() {
    let (red, green, retired, vi) = run(CP1_ROM);
    assert!(
        vi && retired > MIN_BATTERY_RETIRED,
        "CP1TIMINGNTSC did not execute (retired={retired}, vi={vi})"
    );
    println!(
        "CP1TIMINGNTSC: {green} green (pass) vs {red} red (fail) glyph pixels, \
         {retired} retired. Executes without hanging (the point vs n64-systemtest); \
         a full FPU-timing verdict is Stage-D follow-up."
    );
}
