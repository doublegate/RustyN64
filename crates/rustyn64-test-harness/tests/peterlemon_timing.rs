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

/// Run the CPU ROM and read back the **measured** `COUNTWORD` (how many loop
/// iterations of the *last* timed instruction our emulator fit in the ROM's
/// VI-scanline window) together with that instruction's **expected** value.
///
/// Two subtleties make this correct where a naive RDRAM read is not:
/// 1. The expected-value table is a contiguous run of big-endian words all in
///    `[0xDB00, 0xDBFF]` (every covered instruction's hardware count is
///    `~0xDB1D`); it is loaded from ROM and never written, so RDRAM holds it.
///    `COUNTWORD` is the word immediately after the table's last entry.
/// 2. `COUNTWORD` lives at a **KSEG0 (cached)** address and is written by the ROM
///    through the **write-back** D-cache. Raw RDRAM there is *stale* (the dirty
///    line has not been evicted), so we must read it through the D-cache —
///    `Dcache::hits` + `read` — exactly as the CPU does. This is the lesson
///    `docs/engineering-lessons.md` records: verify cached addresses through the
///    cache, not through RDRAM.
///
/// Returns `(measured, expected)`.
fn measured_vs_expected(path: &str) -> Option<(u32, u32)> {
    let image = std::fs::read(path).ok()?;
    let entry = rom::entry_point(&image).ok()?;
    let mut sys = System::new(0);
    rom::load_direct(&mut sys, &image, entry).ok()?;
    let deadline = sys.master_ticks() + BUDGET_TICKS;
    while sys.master_ticks() < deadline {
        sys.step_to_next_edge();
    }

    let word = |i: usize| {
        let r = &sys.bus.rdram;
        u32::from_be_bytes([r[i], r[i + 1], r[i + 2], r[i + 3]])
    };
    let is_expected = |w: u32| (0xDB00..=0xDBFF).contains(&w);

    // Largest gap-tolerant cluster of expected words (consecutive within 32 bytes).
    let (mut best_first, mut best_last, mut best_n) = (0usize, 0usize, 0usize);
    let (mut cur_first, mut cur_last, mut cur_n): (Option<usize>, usize, usize) = (None, 0, 0);
    let mut i = 0x1000; // the code + data load at physical RDRAM 0x1000.
    while i + 4 <= sys.bus.rdram.len() {
        if is_expected(word(i)) {
            match cur_first {
                None => {
                    cur_first = Some(i);
                    cur_n = 0;
                }
                // The table has a genuine internal gap (one covered instruction's
                // count is not `0xDB1x`), so the cluster must tolerate a small gap
                // to span it and reach the true last entry; a strictly-adjacent
                // scan would split the table there and mislocate `COUNTWORD`. The
                // trailing word (`COUNTWORD`, then font data) is not `0xDB1x`, so
                // the cluster still ends at the real last entry.
                Some(first) if i - cur_last > 32 => {
                    if cur_n > best_n {
                        (best_first, best_last, best_n) = (first, cur_last, cur_n);
                    }
                    cur_first = Some(i);
                    cur_n = 0;
                }
                Some(_) => {}
            }
            cur_last = i;
            cur_n += 1;
        }
        i += 4;
    }
    if let Some(first) = cur_first
        && cur_n > best_n
    {
        (best_first, best_last, best_n) = (first, cur_last, cur_n);
    }
    if best_n < 15 || best_last + 8 > sys.bus.rdram.len() {
        return None;
    }

    let countword_phys = u32::try_from(best_last + 4).ok()?; // physical RDRAM offset
    let expected = word(best_first);
    // Read COUNTWORD the way the CPU sees it: from the write-back D-cache if the
    // line is resident (it is — the ROM just stored it), else RDRAM. Both paths are
    // big-endian: `Dcache::read` returns a 4-byte value MSB-first in the low 32
    // bits (matching how the CPU stored it), the same order as `from_be_bytes`, so
    // a resident hit and the RDRAM fallback agree.
    let dc = &sys.cpu.pipeline.dcache;
    let measured = if dc.hits(countword_phys) {
        u32::try_from(dc.read(countword_phys, 4)).unwrap_or(0)
    } else {
        word(best_last + 4)
    };
    Some((measured, expected))
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

/// The **differential** the aggregate green/red verdict cannot give (ledger §C-1):
/// our emulator's measured `COUNTWORD` for the last timed instruction vs the
/// hardware-expected value, read *through the D-cache* (COUNTWORD is a KSEG0
/// write-back store — raw RDRAM there is stale).
///
/// The ROM counts how many loop iterations of the test instruction fit in a fixed
/// VI-scanline window (wait for `VI_V_CURRENT == 0`, then count until `== 512`), so
/// this is a *joint* function of the CPU rate and the VI scanline advance, not a
/// clean per-instruction cycle count. It is the first real timing *number* off
/// this oracle; interpreting it into `M` is the ongoing Stage-D work.
#[test]
#[ignore = "timing differential diagnostic; run explicitly"]
fn cpu_timing_differential() {
    let (measured, expected) =
        measured_vs_expected(CPU_ROM).expect("located the expected-value table + COUNTWORD");
    assert!(
        expected != 0,
        "expected value must be non-zero (the table was located)"
    );
    let ratio = f64::from(measured) / f64::from(expected);
    println!(
        "CPU timing differential: measured COUNTWORD = {measured} (0x{measured:08X}), \
         expected ≈ {expected} (0x{expected:08X}), ratio measured/expected = {ratio:.4}. \
         The ROM counts test-instruction iterations per fixed VI window, so ratio > 1 ⇒ our \
         CPU runs too fast relative to the VI scanline advance, < 1 ⇒ too slow. A joint CPU+VI \
         number (ledger §C-1); the VI tick rate is verified correct, so the gap is CPU-side."
    );
}

/// Derive `M` (uncached RCP-register read latency) from the CPUTIMINGNTSC
/// mult/div differential — the reproducible, falsifiable measurement behind the
/// `M_RCP_REGISTER = 22` constant (ledger C-1, T-11-003).
///
/// The ROM times each instruction in an identical loop and bakes in the
/// hardware iteration count. Instructions of *documented* stall cost `c_i`
/// (UM Table 3-12) give `expected_i = W / (B + c_i)`: a linear regression of
/// `1/expected_i` on `c_i` recovers the window `W` and the base-loop cost `B`
/// (which contains `M`) WITHOUT tuning to any single absolute count. Our own
/// base loop is exactly 4 PClocks (measured, `cpu_timing_differential`), so
/// `M ~= B - 4`. This runs no ROM — it is the arithmetic over documented
/// constants, kept in-tree so the derivation cannot silently rot.
#[test]
// The regression's `a*b + c` accumulations read clearer than `mul_add` here.
#[allow(clippy::suboptimal_flops)]
fn m_is_derived_from_the_multdiv_differential_not_fitted() {
    // (documented instruction stall c_i, hardware-baked expected count) — the
    // c_i are UM Table 3-12; the counts are CPUTIMINGNTSC's `*COUNT` table
    // (`ref-proj/PeterLemon-N64/.../CPUTIMINGNTSC.asm`).
    // Counts are the baked `*COUNT` values in decimal (0xDB1C = 56_092, etc.).
    let pts: [(f64, f64); 9] = [
        (1.0, 56_092.0),  // add   (1-cycle ALU baseline, 0xDB1C)
        (5.0, 49_988.0),  // mult   (0xC344)
        (5.0, 49_986.0),  // multu  (0xC342)
        (8.0, 45_999.0),  // dmult  (0xB3B7)
        (8.0, 46_010.0),  // dmultu (0xB3BA)
        (37.0, 24_177.0), // div    (0x5E71)
        (37.0, 24_177.0), // divu   (0x5E71)
        (69.0, 16_108.0), // ddiv   (0x3EEC)
        (69.0, 16_108.0), // ddivu  (0x3EEC)
    ];
    // Regress y = 1/expected against x = c:  slope = 1/W, intercept = B/W.
    let n = 9.0_f64;
    let (mut sx, mut sy) = (0.0, 0.0);
    for &(c, e) in &pts {
        sx += c;
        sy += 1.0 / e;
    }
    let (mx, my) = (sx / n, sy / n);
    let (mut cov, mut var) = (0.0, 0.0);
    for &(c, e) in &pts {
        cov += (c - mx) * (1.0 / e - my);
        var += (c - mx) * (c - mx);
    }
    let slope = cov / var;
    let w = 1.0 / slope; // window, PClocks
    let b = (my - slope * mx) * w; // base-loop cost, PClocks (contains M)
    let m = b - 4.0; // our base loop is exactly 4 PClocks (four 1-cycle instrs)

    // Fit quality: the model must predict every baked count within a few percent.
    for &(c, e) in &pts {
        let model = w / (b + c);
        let err = (model - e).abs() / e;
        assert!(
            err < 0.03,
            "fit err {err:.3} at c={c}: model {model:.0} vs {e:.0}"
        );
    }
    println!("derived: W = {w:.0} PClocks, B = {b:.2} PClocks, M = B - 4 = {m:.2} PClocks");
    // The measured M rounds to the charged constant. This is a *measurement*
    // band, not a fit target: the constant was independently confirmed by the
    // absolute count landing at 56_330 vs 56_092 (0.4%) once charged.
    assert!(
        (20.0..=23.0).contains(&m),
        "M derived from the differential is {m:.2}; the charged 22 must sit in the band"
    );
}
