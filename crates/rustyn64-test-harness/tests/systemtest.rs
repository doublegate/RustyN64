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
//! in `--release` (much longer unoptimised). That is far too slow for the default
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
const LATER_PHASES: [&str; 11] = [
    "RSP", "SP ", "RDP", "MI ", "cart", "spmem", "pifram", "VI", "AI", "PI ", "SI ",
];

/// Run the suite and return `(phase-1 failures, suite-wide failures, tests started, output)`.
fn run() -> (Vec<String>, usize, usize, String) {
    let image = std::fs::read(ROM).expect("the committed n64-systemtest ROM");
    let mut sys = System::new(0);

    // The payload is a linked ELF, so its segments carry their own load
    // addresses; a flat copy puts every address in the image at the wrong place.
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

    let deadline = sys.master_ticks() + BUDGET_TICKS;
    while sys.master_ticks() < deadline {
        sys.step_to_next_edge();
    }

    // The suite reports through the ISViewer debug channel, which it drives with
    // uncached CPU stores -- no DMA, so nothing has to be flushed to read it.
    let text = String::from_utf8_lossy(sys.bus.isviewer_output()).into_owned();

    let mut phase1 = Vec::new();
    let mut failures = 0usize;
    // `Running <name>...` is printed once per test as it STARTS; `Test '<name>'
    // failed:` only on failure. Counting the former is what witnesses execution
    // -- counting failures cannot, since zero of them is exactly what a run that
    // never started also produces.
    let started = text.lines().filter(|l| l.starts_with("Running ")).count();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("Test '") else {
            continue;
        };
        failures += 1;
        let name = rest.split('\'').next().unwrap_or(rest);
        if !LATER_PHASES.iter().any(|p| name.starts_with(p)) {
            phase1.push(line.to_string());
        }
    }
    (phase1, failures, started, text)
}

/// Phase 1's cut criterion: `Failed: 0` in the CPU/COP0/TLB/COP1 categories.
///
/// This is the gate `to-dos/VERSION-PLAN.md` §v0.2.0 names, and the evidence
/// behind the `MET` row in `docs/STATUS.md`.
#[test]
#[ignore = "~2 minutes in --release; run explicitly (see the module docs)"]
fn phase_1_categories_report_no_failures() {
    let (phase1, failures, started, text) = run();

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

    assert!(
        phase1.is_empty(),
        "Phase 1's categories must report no failures (VERSION-PLAN §v0.2.0).\n\
         {} failing, of {failures} suite-wide across {started} tests:\n{}",
        phase1.len(),
        phase1.join("\n")
    );

    eprintln!(
        "Phase 1 categories: 0 failing. {failures} failing suite-wide across \
         {started} tests started (the remainder are RSP/RCP -- Phase 2's criterion)."
    );
}
