//! The n64-systemtest oracle runner — Phase 1's cut criterion, made reproducible.
//!
//! `to-dos/VERSION-PLAN.md` §v0.2.0 gates the release on `Failed: 0` in the
//! **CPU/COP0/TLB/COP1** categories of n64-systemtest. That is an oracle result,
//! not a self-assessment — but only if someone other than the author can re-run
//! it. Until this file existed the result was measured with a throwaway runner
//! rewritten from scratch each session, which made the number in `docs/STATUS.md`
//! unverifiable from the repository. A measured claim nobody can reproduce is a
//! self-assessment wearing a measurement's clothes.
//!
//! The ROM is committed (MIT, `tests/roms/n64-systemtest/`), so this needs no
//! external corpus.
//!
//! # Why `#[ignore]`
//!
//! A full run is ~600 million retired instructions and takes roughly two minutes
//! in `--release` (much longer unoptimized). That is far too slow for the default
//! `cargo test` path, which contributors run constantly. Run it explicitly:
//!
//! ```text
//! cargo test -p rustyn64-test-harness --release --test systemtest -- --ignored --nocapture
//! ```
//!
//! # Why it asserts a category count rather than a suite-wide one
//!
//! The RSP/RCP categories are **Phase 2's** criterion (§v0.3.0) and still fail in
//! the hundreds. Asserting suite-wide zero would fail for work this phase never
//! promised; asserting nothing at all would let the Phase 1 number regress
//! silently. So the gate is exactly the phase's own scope, and the suite-wide
//! total is reported alongside it for context.

use rustyn64_core::System;
use rustyn64_test_harness::rom;

/// The committed suite, relative to this crate.
const ROM: &str = "../../tests/roms/n64-systemtest/n64-systemtest.z64";

/// Master ticks to run before giving up.
///
/// Generous: a complete pass retires ~600M instructions. A budget rather than a
/// completion sentinel because the suite has no single "done" marker this
/// harness can key on, and a short budget would silently *reduce* the failure
/// count by ending the run early — which would look like progress.
const BUDGET_TICKS: u64 = 4_000_000_000;

/// Category prefixes that belong to a later phase.
///
/// Everything the suite prints that does **not** start with one of these is
/// CPU/COP0/TLB/COP1 — Phase 1's scope. Matching on what to *exclude* is
/// deliberate: a new CPU-side category added upstream then lands inside the gate
/// automatically, where an allowlist would silently ignore it.
/// The RSP's own category prefixes.
///
/// A **subset** of [`LATER_PHASES`], deliberately narrower: `"RSP"` is the vector
/// and scalar unit's tests and `"SP "` the DMA/status ones, which is what the VU
/// work in `docs/performance.md` needs a gate over. The RDP, MI, VI, AI, PI, SI
/// and cart entries in that list belong to other subsystems and are not asserted
/// here.
const RSP_CATEGORIES: [&str; 2] = ["RSP", "SP "];

const LATER_PHASES: [&str; 11] = [
    "RSP", "SP ", "RDP", "MI ", "cart", "spmem", "pifram", "VI", "AI", "PI ", "SI ",
];

/// Run the suite and return `(phase-1 failures, suite-wide failures, tests
/// started, guest reached EXIT, output)`.
fn run() -> Report {
    let image = std::fs::read(ROM).expect("the committed n64-systemtest ROM");
    let mut sys = System::new(0);

    // The payload is a linked ELF, so its segments carry their own load
    // addresses; a flat copy puts every address in the image at the wrong place.
    // Opt into EMUX: the `xlog` console needs no PI/SI/ISViewer emulation and
    // runs ~9x faster, and `xioctl(EXIT)` gives a definite end-of-run. This is a
    // deliberate departure from hardware, which is why it is opt-in and why the
    // golden-log gate does NOT enable it.
    sys.bus.enable_emux();
    rom::load_elf(&mut sys, &image).expect("loadable ELF payload");
    let elf = rom::seed_ipl3_handoff(&mut sys, &image).expect("IPL3 handoff");

    // The ELF entry, SIGN-EXTENDED. Under 32-bit addressing every valid address
    // is the sign extension of its low word, so `0x0000_0000_800A_15E8` is an
    // address error rather than a shorthand -- the very first fetch would fault.
    let entry = u32::from_be_bytes([
        image[elf + 0x18],
        image[elf + 0x19],
        image[elf + 0x1A],
        image[elf + 0x1B],
    ]);
    sys.cpu
        .set_pc(i64::from(entry.cast_signed()).cast_unsigned());

    // Stop on the guest's own EXIT rather than only on the budget: a definite
    // end-of-run beats "we stopped watching". The budget remains as a backstop.
    let deadline = sys.master_ticks() + BUDGET_TICKS;
    while sys.master_ticks() < deadline && !sys.bus.emux_exited() {
        sys.step_to_next_edge();
    }
    // Did the guest reach its own `xioctl(EXIT)`, or did we stop on the budget?
    // A mid-suite hang leaves this false while still producing a full Phase-1
    // `Failed: 0` (the results before the hang), which is exactly how R-19 hid
    // for so long -- so completion is now witnessed, not assumed.
    let exited = sys.bus.emux_exited();

    // The suite picks its console at runtime. It prefers EMUX `xlog` when the
    // emulator advertises it (which we now do) and falls back to ISViewer
    // otherwise, so both are read and whichever produced output wins. EMUX is
    // ~9x faster here because it skips ISViewer's PI-status polling entirely.
    let emux = String::from_utf8_lossy(sys.bus.emux_output()).into_owned();
    let text = if emux.is_empty() {
        String::from_utf8_lossy(sys.bus.isviewer_output()).into_owned()
    } else {
        emux
    };

    let mut phase1 = Vec::new();
    let mut rsp = Vec::new();
    let mut rsp_started = 0usize;
    let mut failures = 0usize;
    // `Running <name>...` is printed once per test as it STARTS; `Test '<name>'
    // failed:` only on failure. Counting the former is what witnesses execution
    // -- counting failures cannot, since zero of them is exactly what a run that
    // never started also produces.
    let started = text.lines().filter(|l| l.starts_with("Running ")).count();
    // The RSP's own started-count, for the same reason `started` exists: zero
    // failures in a category that never ran is not a pass.
    rsp_started += text
        .lines()
        .filter_map(|l| l.strip_prefix("Running "))
        .filter(|n| RSP_CATEGORIES.iter().any(|p| n.starts_with(p)))
        .count();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("Test '") else {
            continue;
        };
        failures += 1;
        let name = rest.split('\'').next().unwrap_or(rest);
        if !LATER_PHASES.iter().any(|p| name.starts_with(p)) {
            phase1.push(line.to_string());
        }
        if RSP_CATEGORIES.iter().any(|p| name.starts_with(p)) {
            rsp.push(line.to_string());
        }
    }
    Report {
        phase1,
        rsp,
        rsp_started,
        failures,
        started,
        exited,
        text,
    }
}

/// What one suite run reported.
///
/// A struct rather than the tuple this used to return: the tuple reached five
/// fields and a sixth was being added, at which point every call site is a
/// positional puzzle and a swapped pair compiles.
struct Report {
    /// Failures outside [`LATER_PHASES`] — Phase 1's scope.
    phase1: Vec<String>,
    /// Failures in the RSP's own categories.
    rsp: Vec<String>,
    /// RSP tests that STARTED, which is what witnesses the category ran.
    rsp_started: usize,
    /// Failures suite-wide.
    failures: usize,
    /// Tests started suite-wide.
    started: usize,
    /// Did the guest reach its own `xioctl(EXIT)`?
    exited: bool,
    /// The captured console text.
    text: String,
}

/// Phase 1's cut criterion: `Failed: 0` in the CPU/COP0/TLB/COP1 categories.
///
/// This is the gate `to-dos/VERSION-PLAN.md` §v0.2.0 names, and the evidence
/// behind the `MET` row in `docs/STATUS.md`.
#[test]
#[ignore = "~2 minutes in --release; run explicitly (see the module docs)"]
fn phase_1_categories_report_no_failures() {
    let Report {
        phase1,
        failures,
        started,
        exited,
        text,
        ..
    } = run();

    // Witness that the suite actually RAN before trusting a zero. An empty
    // output produces zero failures just as convincingly as a passing run, which
    // is the vacuous-pass hazard `docs/engineering-lessons.md` §2.2 describes --
    // and here it is the more likely of the two, since any harness regression
    // that stops execution shows up as silence.
    assert!(
        text.contains("Running "),
        "the suite produced no output at all -- it did not run, so a zero \
         failure count means nothing (captured {} bytes)",
        text.len()
    );
    // A complete pass starts ~900 tests. The bound is deliberately loose -- it
    // exists to catch a run that died early, not to pin the suite's size.
    assert!(
        started > 800,
        "only {started} tests started; the run was truncated, so the Phase 1 \
         count below is measured against a partial pass"
    );
    // Witness COMPLETION, not just that it started. R-19 was a mid-suite hang
    // that this gate could not see: it left a full Phase-1 `Failed: 0` (captured
    // before the hang) while never reaching `xioctl(EXIT)`, so a whole later
    // category (the `tlb64` 64-bit-addressing group) never ran. Requiring the
    // guest's own EXIT closes that blind spot for good.
    assert!(
        exited,
        "the suite did not reach xioctl(EXIT) ({started} tests started, \
         {failures} suite-wide failures so far) -- it hung mid-run, so the \
         Phase-1 count below is measured against a partial pass (see ledger R-19)"
    );

    assert!(
        phase1.is_empty(),
        "Phase 1's categories must report no failures (VERSION-PLAN §v0.2.0).\n\
         {} failing, of {failures} suite-wide across {started} tests:\n{}",
        phase1.len(),
        phase1.join("\n")
    );

    eprintln!(
        "Phase 1 categories: 0 failing (suite ran to xioctl(EXIT)). {failures} \
         failing suite-wide across {started} tests started (the remainder are \
         RSP/RCP -- Phase 2's criterion)."
    );
}

/// **Phase 2's cut criterion: `Failed: 0` in the RSP's own categories.**
///
/// This gate did not exist until now, and its absence was found the hard way: a
/// performance note cited "n64-systemtest's RSP category" as what would guard a
/// vector-unit refactor, and a reviewer pointed out that
/// [`phase_1_categories_report_no_failures`] **excludes** `"RSP"` and `"SP "`
/// through [`LATER_PHASES`]. The RSP did reach `Failed: 0` at v0.3.0 — that is
/// `to-dos/VERSION-PLAN.md` §v0.3.0's criterion and the `MET` row in
/// `docs/STATUS.md` — but nothing committed asserted it, so the vector unit could
/// have regressed arbitrarily with every gate green.
///
/// Same shape as the Phase 1 assertion, for the same reasons: witness that output
/// exists, that the suite ran to `xioctl(EXIT)`, and **that the RSP category
/// itself started**. That last one is not redundant with the suite-wide count —
/// a change that made every RSP test fail to *start* would leave zero RSP
/// failures, which is the vacuous pass this project keeps re-encountering.
#[test]
#[ignore = "~2 minutes in --release; run explicitly (see the module docs)"]
fn rsp_categories_report_no_failures() {
    let Report {
        rsp,
        rsp_started,
        failures,
        started,
        exited,
        text,
        ..
    } = run();

    assert!(
        text.contains("Running "),
        "the suite produced no output at all -- it did not run, so a zero RSP \
         failure count means nothing (captured {} bytes)",
        text.len()
    );
    assert!(
        exited,
        "the suite did not reach xioctl(EXIT) ({started} tests started, \
         {failures} suite-wide failures) -- it hung mid-run, so the RSP count \
         below is measured against a partial pass (see ledger R-19)"
    );
    // The RSP's OWN witness. A regression that stopped these tests starting would
    // otherwise read as a perfect pass.
    assert!(
        rsp_started > 20,
        "only {rsp_started} RSP-category tests started; the category did not run, \
         so its zero failure count says nothing about the vector unit"
    );

    assert!(
        rsp.is_empty(),
        "the RSP categories must report no failures (VERSION-PLAN §v0.3.0).\n\
         {} failing, of {failures} suite-wide across {started} tests \
         ({rsp_started} RSP):\n{}",
        rsp.len(),
        rsp.join("\n")
    );

    eprintln!(
        "RSP categories: 0 failing across {rsp_started} RSP tests started \
         (suite ran to xioctl(EXIT); {failures} failing suite-wide across \
         {started} started)."
    );
}
