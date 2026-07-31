//! The `fast-scheduler` differential gate (ADR 0011 §3, ADR 0012).
//!
//! Runs the same machine under both schedulers and requires them to land in
//! identical state. The accurate scheduler is the oracle; the fast path is the
//! thing on trial. Nothing here grades the accurate path — its own suites do that.
//!
//! **Why whole-state bytes rather than an enumerated field list.** ADR 0011 §3
//! names the state that must agree: GPRs/`HI`/`LO`/PC, COP0 and the TLB, the FP
//! register file and `FCSR`, RSP DMEM/IMEM and vector state, RDP and VI registers,
//! RDRAM, pending interrupts and exceptions. Comparing the serialized `System`
//! covers every one of those and, more importantly, **cannot go stale**: a field
//! added to any chip next year is compared automatically, whereas a hand-written
//! list silently stops covering new state and keeps reporting agreement. This
//! project has been bitten repeatedly by claims that decayed without failing, so
//! the gate is written to have nothing to decay.
//!
//! The trade is diagnostic quality — a byte offset is a worse error message than a
//! field name — so a mismatch reports the offset, the surrounding bytes, and the
//! tick, which is enough to bisect with. If the fast path ever stores state of its
//! own (ADR 0011 §4's mode marker), whole-state equality stops being the right
//! comparison and this must switch to an explicit projection; that is called out
//! here so the switch is a decision rather than a surprise.
//!
//! Runs only with `--features fast-scheduler`; the file is empty otherwise.

#![cfg(feature = "fast-scheduler")]

use rustyn64_core::fastpath::{BailOut, BailOutSet, FastRunReport};
use rustyn64_core::scheduler::{EDGE_PERIOD, System};

/// A seed with no special structure — the phase alignment it derives is what makes
/// the two runs a real comparison rather than two runs of the same schedule.
const SEED: u64 = 0x5265_616C_6974_7921;

/// Enough seeds to exercise **every** power-on phase alignment.
///
/// `Phases::from_seed` derives `cpu ∈ 0..CPU_DIVIDER` and `rcp ∈ 0..RCP_DIVIDER`,
/// so there are six distinct alignments and a single seed samples exactly one of
/// them. That is not a theoretical gap: an early version of the fast path's
/// replayed pattern was off by one, skipping the edge at `base + 1` — and whether
/// tick 1 *is* an edge is a function of the phases, so the one-seed gate passed a
/// fast path that was demonstrably wrong. ADR 0011 §6 asks for boundaries to be
/// forced rather than hoped for; this is that, at the cheapest boundary there is.
///
/// Sixteen consecutive seeds rather than six hand-picked ones: `SplitMix64` maps
/// them to alignments opaquely, so enumerating seeds is honest where enumerating
/// alignments would mean hardcoding the mapping and re-deriving it on every change.
///
/// **The limit of that, stated rather than glossed:** sixteen draws over six
/// alignment buckets makes full coverage very likely, not certain. Proving it would
/// mean reproducing `Phases::from_seed` here — a second copy of the thing under
/// test, which is worse. The alternative accepted is a probabilistic sweep with the
/// probability acknowledged.
///
/// **Cost:** with the tail sweep below this is 96 machine pairs, and a `System`
/// carries 8 MiB of RDRAM that is allocated, zeroed, and traversed for each. That
/// is ~60 s in a debug `cargo test` and a few seconds in `--release`. It is the
/// most expensive test in this crate by a wide margin, deliberately: it is the only
/// thing standing between a wrong fast path and a silently wrong emulator, and it
/// has already caught one.
const PHASE_SEEDS: core::ops::Range<u64> = 0..16;

/// Long enough that both domains step many thousands of times and the CPU retires
/// real work; short enough to stay in the default `cargo test` path.
const TICKS: u64 = 400_000;

/// Serialize a machine's full state for comparison.
fn state_of(sys: &System) -> Vec<u8> {
    bincode::serialize(sys).expect("a System is serializable; save-states rely on it")
}

/// A `std::io::Write` sink that hashes instead of storing.
struct HashSink(std::collections::hash_map::DefaultHasher);

impl std::io::Write for HashSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        std::hash::Hasher::write(&mut self.0, buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The same whole-state comparison as [`state_of`], reduced to 64 bits.
///
/// Covers exactly the same bytes — it is the identical `bincode` traversal — but
/// streams them through a hasher instead of allocating. That matters at scale: the
/// serialized machine includes **8 MiB of RDRAM**, and the phase x tail
/// cross-product below compares 96 pairs, so materializing both images each time
/// took the test past 100 seconds and out of the default `cargo test` budget.
///
/// A 64-bit digest can collide in principle. It is used only where the alternative
/// was running fewer cases, which is a strictly worse trade: a birthday collision
/// against one specific other image is ~2^-64, while a case not run is a hole with
/// probability 1. The single-seed test still compares full bytes, so the exact
/// comparison is exercised on every run of this file.
fn state_digest(sys: &System) -> u64 {
    let mut sink = HashSink(std::collections::hash_map::DefaultHasher::new());
    bincode::serialize_into(&mut sink, sys).expect("a System is serializable");
    std::hash::Hasher::finish(&sink.0)
}

/// Report where two state images diverge, with enough context to bisect.
fn describe_divergence(a: &[u8], b: &[u8]) -> String {
    if a.len() != b.len() {
        return format!(
            "state images differ in LENGTH: accurate {} bytes, fast {} bytes — the \
             two modes no longer serialize the same shape, so this gate's \
             whole-state comparison is the wrong tool (see the module docs)",
            a.len(),
            b.len()
        );
    }
    let at = a
        .iter()
        .zip(b)
        .position(|(x, y)| x != y)
        .expect("callers only call this when the images differ");
    let lo = at.saturating_sub(8);
    let hi = (at + 8).min(a.len());
    format!(
        "state diverges at byte {at} of {}\n  accurate[{lo}..{hi}] = {:02x?}\n  fast    [{lo}..{hi}] = {:02x?}",
        a.len(),
        &a[lo..hi],
        &b[lo..hi]
    )
}

/// **The fast path must land on exactly the state the accurate path would.**
///
/// This was trivially true in #224, when `run_until_fast` bailed out on every step,
/// and that was the point: the gate had to exist and pass *before* block execution
/// was written, or it would grade nothing at the moment it was most needed.
///
/// It is no longer trivial — the fast path now replays a precomputed edge pattern —
/// but note that this single-seed test is still **not** the one doing the work.
/// See `the_fast_path_agrees_at_every_phase_alignment`: a pattern off by one offset
/// passed *this* test and failed that one.
///
/// **`master_ticks` is asserted separately and first** even though the byte
/// comparison would also catch it. Landing on the right state at the wrong tick is
/// the *correct-but-late* failure ADR 0011 §6 singles out, and it deserves an error
/// message that names it rather than a byte offset into a serialized struct.
#[test]
fn the_fast_path_lands_where_the_accurate_path_would() {
    let mut accurate = System::new(SEED);
    let mut fast = System::new(SEED);

    assert_eq!(
        state_of(&accurate),
        state_of(&fast),
        "the two machines must start identical, or nothing after this means anything"
    );

    accurate.run_until(TICKS);
    let report = fast.run_until_fast(TICKS);

    // Engagement (ADR 0012 §2). Before #225 this function deferred *everything* to
    // the accurate scheduler, and every assertion below passed — correctly, and
    // uselessly. The report is what tells the two apart.
    assert!(
        report.engaged(),
        "the fast path executed no block; it deferred the whole run and this \
         comparison grades nothing"
    );

    // Completion witness (ADR 0012): a run that did nothing would agree just as
    // convincingly as one that did everything. Assert the machines actually
    // advanced before trusting the comparison — an empty run is the vacuous pass
    // this project has shipped before.
    assert_eq!(
        accurate.master_ticks(),
        TICKS,
        "the accurate run reached the target"
    );
    assert!(
        accurate.cpu.retired > 0,
        "the accurate run retired no instructions — the comparison would be vacuous"
    );

    assert_eq!(
        fast.master_ticks(),
        accurate.master_ticks(),
        "correct-but-late: the fast path finished at a different master tick, which \
         no AV comparison would ever reveal (ADR 0011 §6)"
    );
    assert_eq!(
        fast.cpu.retired, accurate.cpu.retired,
        "the two paths retired different instruction counts"
    );

    // A failure walks the images twice — once for this comparison, once inside
    // `describe_divergence` to find the offset. That is the failure path only, so
    // it costs nothing in practice. Review suggested the more explicit
    // `if a != f { panic!(..) }`; clippy's `manual_assert` rejects that form under
    // this workspace's pedantic set, so `assert!` is not merely the tidier choice
    // here, it is the only one that compiles.
    let (a, f) = (state_of(&accurate), state_of(&fast));
    assert!(a == f, "{}", describe_divergence(&a, &f));
}

/// **The same comparison, across every power-on phase alignment.**
///
/// The single-seed test above samples one of six alignments. That is not enough:
/// which ticks are edges depends on the phases, so a fast path that mishandles one
/// specific offset can be invisible at one seed and obvious at another. This ran
/// green on a fast path whose replayed pattern was off by one — until it was given
/// more than one seed.
///
/// A shorter run than the single-seed test, since the point here is breadth of
/// alignment rather than depth of execution, and running the full cross-product at
/// full length would put this outside the default `cargo test` budget.
#[test]
fn the_fast_path_agrees_at_every_phase_alignment() {
    // The full cross-product of alignment x tail length, not one tail per seed.
    //
    // `BASE` is an exact multiple of the edge period, so a target of `BASE + tail`
    // leaves exactly `tail` ticks for the partial-period fallback. Sweeping `tail`
    // over `0..EDGE_PERIOD` inside the seed loop is what makes the claim true: a
    // tail bug that only bites at one alignment needs *that* alignment and *that*
    // remainder together, and an earlier version of this test picked a single tail
    // per seed while its comment claimed the cross-product. Review caught that; it
    // is the same over-claim this file exists to prevent, so it is spelled out
    // rather than quietly corrected.
    const BASE: u64 = 24_000;
    const { assert!(BASE.is_multiple_of(EDGE_PERIOD)) };

    let mut blocks = 0u64;
    for seed in PHASE_SEEDS {
        for tail in 0..EDGE_PERIOD {
            let target = BASE + tail;
            let mut accurate = System::new(seed);
            let mut fast = System::new(seed);
            accurate.run_until(target);
            blocks += fast.run_until_fast(target).blocks;

            assert!(
                accurate.cpu.retired > 0,
                "seed {seed} tail {tail}: the accurate run retired nothing, so the comparison is vacuous"
            );
            assert_eq!(
                fast.master_ticks(),
                accurate.master_ticks(),
                "seed {seed} tail {tail}: correct-but-late — the fast path finished at a different tick"
            );
            assert_eq!(
                fast.cpu.retired, accurate.cpu.retired,
                "seed {seed} tail {tail}: the two paths retired different instruction counts"
            );

            // Digest first; only materialize both images to describe a failure.
            if state_digest(&accurate) != state_digest(&fast) {
                let (a, f) = (state_of(&accurate), state_of(&fast));
                panic!("seed {seed} tail {tail}: {}", describe_divergence(&a, &f));
            }
        }
    }

    // Engagement across the whole sweep (ADR 0012 §2). Asserted once at the end
    // rather than per case: `BASE` is thousands of periods, so every case engages,
    // and a per-case assertion would only add noise to a loop that already reports
    // seed and tail on failure.
    assert!(
        blocks > 0,
        "no case in the sweep executed a single block — the fast path deferred \
         everything and none of the {} comparisons above graded it",
        (PHASE_SEEDS.end - PHASE_SEEDS.start) * EDGE_PERIOD
    );
}

// **The top of the tick range is guarded by construction, not by a test — and this
// records why there is no test.**
//
// Review asked for boundary cases at `u64::MAX - 1` and `u64::MAX`. They were
// written, and they hang: `master_ticks` only reaches that region by *running*
// there, so `run_until_fast(u64::MAX)` means emulating some 1.5e18 periods. It does
// not wrap — it simply never returns, exactly as the accurate `run_until` would
// not. At 187.5 MHz, `u64::MAX` is roughly three thousand years of emulated time.
//
// So the arithmetic this fast path introduces is made safe where it is written
// rather than pinned by a test that cannot run: the pattern probe uses
// `saturating_add` (a wrapped probe would report an edge that is not there) and the
// loop bound uses `checked_add` (overflow ends the loop, which is correct — there
// is no whole period left below `target`). The pre-existing `+ 1` in
// `next_edge_after` and the phase additions inside `is_edge` are untouched and
// remain out of scope for this change.
//
// The reachable half of that concern — a target at or before the current tick — is
// tested immediately below.

/// A target at or before the current tick must do nothing at all.
#[test]
fn a_target_already_reached_is_a_no_op() {
    let mut sys = System::new(SEED);
    let _ = sys.run_until_fast(10_000);
    let (ticks, retired) = (sys.master_ticks(), sys.cpu.retired);
    for report in [sys.run_until_fast(ticks), sys.run_until_fast(ticks - 1)] {
        // A no-op is neither engagement nor a hand-off, and the report must say so
        // (ADR 0012 §2). Counting a reached target as a bail-out would let a suite
        // of nothing but redundant calls satisfy the coverage witness.
        assert_eq!(
            report,
            FastRunReport::default(),
            "a reached target executed no block and handed nothing back"
        );
    }
    assert_eq!(
        sys.master_ticks(),
        ticks,
        "a reached target must not move the clock"
    );
    assert_eq!(
        sys.cpu.retired, retired,
        "a reached target must not retire work"
    );
}

/// The gate must be able to **fail**, which a comparison of two identical runs
/// cannot demonstrate on its own.
///
/// Perturbing one machine by a single extra tick has to be caught. Without this,
/// a `state_of` that returned a constant — or an `assert` on the wrong pair —
/// would leave the gate above green forever while grading nothing, which is
/// exactly how a vacuous gate survives review.
#[test]
fn the_gate_detects_a_one_tick_divergence() {
    let mut a = System::new(SEED);
    let mut b = System::new(SEED);
    a.run_until(TICKS);
    b.run_until(TICKS + 2); // one extra CPU edge (CPU_DIVIDER == 2)

    assert_ne!(
        a.master_ticks(),
        b.master_ticks(),
        "the perturbation must move the clock, or it is not a perturbation"
    );
    assert!(
        state_of(&a) != state_of(&b),
        "the gate compared two demonstrably different machines as equal — it cannot \
         detect divergence and therefore grades nothing"
    );
}

/// Timing probe for the fast path, `#[ignore]`d like the project's other probes.
///
/// Not a gate — it asserts nothing about speed, because a wall-clock threshold in
/// CI is a flake generator. It exists so the PR that changes the block executor has
/// a number to quote, measured through the scheduler alone rather than through a
/// frame, which is the only way to see this change without the CPU and RDP burying
/// it.
///
/// ```text
/// cargo test -p rustyn64-core --release --features fast-scheduler \
///   --test fast_scheduler_differential -- --ignored --nocapture
/// ```
#[test]
#[ignore = "timing probe, not a gate"]
fn probe_scheduler_dispatch_cost() {
    use std::time::Instant;

    const TICKS: u64 = 60_000_000;
    const REPS: u32 = 3;

    let mut acc_best = f64::MAX;
    let mut fast_best = f64::MAX;
    for _ in 0..REPS {
        let mut a = System::new(SEED);
        let t = Instant::now();
        a.run_until(TICKS);
        acc_best = acc_best.min(t.elapsed().as_secs_f64());

        let mut f = System::new(SEED);
        let t = Instant::now();
        let _ = f.run_until_fast(TICKS);
        fast_best = fast_best.min(t.elapsed().as_secs_f64());
    }
    println!(
        "accurate {acc_best:.4}s  fast {fast_best:.4}s  ratio {:.4}x  ({TICKS} ticks, best of {REPS})",
        acc_best / fast_best
    );
}

/// **The same equivalence, driving a real commercial title.**
///
/// Everything above runs the reset-vector workload: no cart, no IPL3, no game code.
/// That exercises the scheduler but not the instruction mix, the memory traffic, or
/// the RSP/RDP activity a real title produces — and ADR 0011's promotion criteria
/// are about the fast path being trustworthy on *real* workloads, not synthetic
/// ones. This is that evidence.
///
/// Compared in **chunks** rather than once at the end, so a divergence is localized
/// to the interval that produced it instead of being reported after millions of
/// ticks of downstream consequences.
///
/// `#[ignore]`d and env-gated because commercial ROMs are never committed (ADR
/// 0008). It **fails** rather than skips when `RUSTYN64_PROBE_ROM` is set but
/// unusable — a silent skip on a misconfigured path is the vacuous pass this repo
/// has shipped before.
///
/// ```text
/// RUSTYN64_PROBE_ROM=/path/rom.z64 cargo test -p rustyn64-core --release \
///   --features fast-scheduler --test fast_scheduler_differential -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs a local commercial ROM (never committed, ADR 0008)"]
fn the_fast_path_agrees_while_running_a_real_rom() {
    /// Ticks per compared interval, and how many. 20 x 2M ticks is ~0.2 s of
    /// emulated time — well past the boot and into game code.
    const CHUNK: u64 = 2_000_000;
    const CHUNKS: u64 = 20;

    let Ok(path) = std::env::var("RUSTYN64_PROBE_ROM") else {
        println!("RUSTYN64_PROBE_ROM unset — set it to a local ROM to run this gate");
        return;
    };
    let raw = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("RUSTYN64_PROBE_ROM is set but unreadable: {path}: {e}"));

    let mut accurate = System::new(SEED);
    let mut fast = System::new(SEED);
    for sys in [&mut accurate, &mut fast] {
        rustyn64_core::boot::hle_boot(sys, &raw)
            .unwrap_or_else(|e| panic!("probe ROM did not boot: {path}: {e:?}"));
    }
    assert_eq!(
        state_digest(&accurate),
        state_digest(&fast),
        "the two machines must be identical after boot"
    );

    // Anchored to the post-boot tick, not to zero. `hle_boot` does not currently
    // advance `master_ticks`, but if it ever did, targets computed from zero would
    // land in the past and the early chunks would silently compare two machines
    // that executed nothing — a vacuous pass wearing the shape of a real one.
    // Raised in review; the fix costs one variable and removes the dependency on a
    // property of `hle_boot` that this test has no business relying on.
    let start = accurate.master_ticks();
    assert_eq!(
        start,
        fast.master_ticks(),
        "both machines start at the same tick"
    );

    for chunk in 1..=CHUNKS {
        let target = start + chunk * CHUNK;
        accurate.run_until(target);
        let report = fast.run_until_fast(target);

        assert!(
            report.engaged(),
            "chunk {chunk}: the fast path executed no block, so this chunk compared \
             the accurate scheduler against itself"
        );
        assert_eq!(
            fast.master_ticks(),
            accurate.master_ticks(),
            "chunk {chunk}: correct-but-late — the fast path finished at a different tick"
        );
        assert_eq!(
            fast.cpu.retired, accurate.cpu.retired,
            "chunk {chunk}: the two paths retired different instruction counts"
        );
        if state_digest(&accurate) != state_digest(&fast) {
            let (a, f) = (state_of(&accurate), state_of(&fast));
            panic!(
                "chunk {chunk} (tick {target}): {}",
                describe_divergence(&a, &f)
            );
        }
        // Progress, since this runs silently for 40M ticks otherwise and the only
        // way to run it is interactively with `--nocapture`. It also makes the
        // retired count visible per chunk, which is what distinguishes a run that
        // reached game code from one that stalled early.
        println!(
            "chunk {chunk}/{CHUNKS} tick {target}: identical, {} retired",
            accurate.cpu.retired
        );
    }

    // Completion witness (ADR 0012): a run that executed nothing would agree just
    // as convincingly. A real title retires millions of instructions here.
    assert!(
        accurate.cpu.retired > 1_000_000,
        "only {} instructions retired over {} ticks — this did not reach game code, \
         so the comparison says nothing about a real workload",
        accurate.cpu.retired,
        CHUNKS * CHUNK
    );
    println!(
        "real-ROM differential: {} chunks x {CHUNK} ticks, {} instructions retired, states identical",
        CHUNKS, accurate.cpu.retired
    );
}

// ---------------------------------------------------------------------------
// The completion witness (ADR 0012 §2)
// ---------------------------------------------------------------------------
//
// Everything above compares two schedulers. None of it can tell you whether the
// gate *ran* — and ADR 0012 §2 is explicit that "a gate that hangs mid-suite is
// indistinguishable from a gate that passed, because both produce no failure".
// This section is the part that reports having finished.
//
// **Why boundary fixtures are separate from the equivalence sweeps above.** The
// sweeps are long by design (96 machine pairs, ~60 s debug); re-running them here
// to collect coverage would double the suite for information they already have but
// do not aggregate. ADR 0011 §6 asks for something different anyway: "fixtures —
// crafted machine states and instruction sequences that reach each boundary —
// driven through the ordinary public entry points". Crafted and cheap is the
// point. Each fixture below still asserts equivalence for its own state, so
// coverage is never bought with a run that grades nothing.

/// One crafted machine state that reaches a specific boundary.
///
/// A plain `fn` pointer rather than a closure so the driver can move it to a
/// worker thread without a `Box<dyn ...>` dance.
struct Fixture {
    /// Reported in every failure message, and the value `RUSTYN64_GATE_FIXTURE`
    /// matches against.
    name: &'static str,
    /// Runs the crafted case, asserts its own equivalence, and returns what the
    /// fast path reported doing.
    run: fn() -> FastRunReport,
}

/// Per-fixture wall-clock limit (ADR 0012 §2: "carry a timeout per fixture and for
/// the suite, so a hang fails rather than hanging").
///
/// Deliberately generous — two orders of magnitude above what these cost even in a
/// debug build on a loaded machine. The timeout exists to convert a **hang** into a
/// failure, not to police performance: a tight wall-clock bound in CI is a flake
/// generator, and a flaky gate gets disabled, which is strictly worse than a slow
/// one. `probe_scheduler_dispatch_cost` is where speed is measured.
const FIXTURE_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(2);

/// Whole-suite limit, so a pathology spread thinly across fixtures still fails.
const SUITE_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(10);

/// Environment variable that restricts the run to fixtures whose name contains it.
///
/// A filtered run **cannot satisfy a suite-wide expectation and must not pretend
/// to** (ADR 0012 §2): it reports that the witness did not apply, and it is the
/// unfiltered run in CI that gates.
const FILTER_ENV: &str = "RUSTYN64_GATE_FIXTURE";

/// **Boundary: a partial period remains**, so the accurate scheduler runs the tail.
///
/// Reaches [`BailOut::PartialPeriodTail`].
fn fixture_partial_period_tail() -> FastRunReport {
    const BASE: u64 = 6_000;
    const { assert!(BASE.is_multiple_of(EDGE_PERIOD)) };

    let mut acc = FastRunReport::default();
    // Every non-zero remainder, not just one: the tail length is the thing that
    // decides how much the accurate scheduler is handed, and a hand-off correct at
    // one remainder is not thereby correct at another.
    for tail in 1..EDGE_PERIOD {
        let target = BASE + tail;
        let mut accurate = System::new(SEED);
        let mut fast = System::new(SEED);
        accurate.run_until(target);
        let report = fast.run_until_fast(target);

        assert!(
            report.bailed.contains(BailOut::PartialPeriodTail),
            "tail {tail}: a partial period remained and was not reported as a hand-off"
        );
        assert_eq!(
            fast.master_ticks(),
            accurate.master_ticks(),
            "tail {tail}: correct-but-late across the hand-off boundary (ADR 0011 §6)"
        );
        assert_eq!(
            state_digest(&accurate),
            state_digest(&fast),
            "tail {tail}: state diverged across the hand-off boundary"
        );
        acc = acc.merge(report);
    }
    acc
}

/// **Boundary: the target lands exactly on a period edge**, so nothing is handed
/// back beyond the landing.
///
/// Reaches no reason at all, which is the point. If `PartialPeriodTail` were ever
/// reported unconditionally — the natural mistake, since `run_until` is called on
/// every path — the coverage witness would still be satisfied and would have stopped
/// meaning anything. This fixture is the mutation guard for that: it fails on the
/// buggy version and passes on the correct one.
fn fixture_whole_periods_only() -> FastRunReport {
    const TARGET: u64 = 6_000;
    const { assert!(TARGET.is_multiple_of(EDGE_PERIOD)) };

    let mut accurate = System::new(SEED);
    let mut fast = System::new(SEED);
    accurate.run_until(TARGET);
    let report = fast.run_until_fast(TARGET);

    assert!(
        report.bailed.is_empty(),
        "a target on a period boundary handed nothing back, yet the report claims \
         {:?} — a reason reported on every call is a reason that distinguishes \
         nothing",
        report.bailed
    );
    assert!(
        report.engaged(),
        "the fixture that proves the no-bail-out case must itself run blocks, or it \
         proves only that nothing happened"
    );
    assert_eq!(
        state_digest(&accurate),
        state_digest(&fast),
        "state diverged on a whole-period run"
    );
    report
}

/// Every boundary fixture. The driver walks this; nothing else may.
const FIXTURES: &[Fixture] = &[
    Fixture {
        name: "partial_period_tail",
        run: fixture_partial_period_tail,
    },
    Fixture {
        name: "whole_periods_only",
        run: fixture_whole_periods_only,
    },
];

/// Run one fixture under a timeout, converting **any** abnormal end into a verdict.
///
/// ADR 0012 §2: "An abnormal termination is a gate failure, never an uncounted
/// exit. A panic, an abort, or a fixture that hits its timeout is not a bail-out
/// reason and must not be absorbed as one." The two failure modes are told apart by
/// the channel: a panic drops the sender (`Disconnected`), a hang does not
/// (`Timeout`). Both fail; they get different messages because they need different
/// investigations.
///
/// The worker thread is deliberately **not** joined on timeout — it cannot be
/// killed, and blocking on it would reproduce the hang this is here to convert.
fn run_fixture(fixture: &Fixture) -> FastRunReport {
    let (tx, rx) = std::sync::mpsc::channel();
    let run = fixture.run;
    let name = fixture.name;
    std::thread::Builder::new()
        .name(format!("gate:{name}"))
        .spawn(move || {
            let _ = tx.send(run());
        })
        .unwrap_or_else(|e| panic!("fixture {name}: could not spawn a worker: {e}"));

    match rx.recv_timeout(FIXTURE_TIMEOUT) {
        Ok(report) => report,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!(
            "fixture {name}: HUNG — no result after {FIXTURE_TIMEOUT:?}. A gate that \
             hangs is indistinguishable from one that passed, so this is a failure \
             (ADR 0012 §2)"
        ),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => panic!(
            "fixture {name}: terminated abnormally (its panic is printed above). An \
             abnormal termination is a gate failure, never an uncounted exit \
             (ADR 0012 §2)"
        ),
    }
}

/// **The gate reports having finished, and what it covered** (ADR 0012 §2).
///
/// Three suite-wide conditions, each of which would otherwise look exactly like
/// success:
///
/// 1. **Every bail-out reason was reached.** The expected set is
///    [`BailOut::ALL`], generated from the same list as the enum, so adding a
///    reason without a fixture that reaches it fails here and **nobody has to
///    remember to bump a total**. A literal expected count drifts the moment the
///    suite grows, which converts the witness into decoration.
/// 2. **The fast path engaged.** A run that deferred everything agrees with the
///    accurate scheduler perfectly — #224 shipped exactly that, on purpose.
/// 3. **A boundary was reached at all.** Distinct from (1) only when the reason
///    set is empty, which is why the set being empty is itself a failure: a
///    coverage assertion over nothing passes.
///
/// Engagement and coverage are **suite-wide, not per-fixture**, per ADR 0012: "a
/// fixture that runs start to finish on the fast path without ever handing back is
/// a legitimate and valuable case", which `whole_periods_only` is.
#[test]
fn the_gate_witnesses_its_own_completion() {
    let filter = std::env::var(FILTER_ENV).ok();
    let selected: Vec<&Fixture> = FIXTURES
        .iter()
        .filter(|f| filter.as_ref().is_none_or(|p| f.name.contains(p.as_str())))
        .collect();

    assert!(
        !selected.is_empty(),
        "{FILTER_ENV}={:?} matched no fixture; the run would report success having \
         checked nothing",
        filter.as_deref().unwrap_or("")
    );

    let started = std::time::Instant::now();
    // One accumulator through `FastRunReport::merge`, not a count beside a set:
    // adding the blocks and forgetting the union compiles, still reports
    // engagement, and silently under-reports coverage — disabling the very check
    // this test is.
    let mut total = FastRunReport::default();
    for fixture in &selected {
        let report = run_fixture(fixture);
        total = total.merge(report);
        println!(
            "fixture {}: {} blocks, reasons {:?}",
            fixture.name, report.blocks, report.bailed
        );
        assert!(
            started.elapsed() < SUITE_TIMEOUT,
            "the suite exceeded {SUITE_TIMEOUT:?} after fixture {} — failing rather \
             than continuing, since a gate that never finishes reports nothing",
            fixture.name
        );
    }

    if filter.is_some() {
        // A filtered run cannot satisfy a suite-wide expectation and must not
        // pretend to (ADR 0012 §2). It says so out loud; CI runs unfiltered.
        println!(
            "{FILTER_ENV} is set — {} of {} fixtures ran, so the suite-wide witness \
             DID NOT APPLY and was not asserted. The unfiltered run is what gates.",
            selected.len(),
            FIXTURES.len()
        );
        return;
    }

    assert!(
        !BailOut::ALL.is_empty(),
        "the bail-out enumeration is empty, so the coverage assertion below would \
         pass over nothing — the fast path must declare its exits"
    );
    assert!(
        total.engaged(),
        "THE FAST PATH NEVER ENGAGED across {} fixtures: it deferred every stretch \
         to the accurate scheduler, so the whole suite compared that scheduler with \
         itself",
        selected.len()
    );
    assert!(
        !total.bailed.is_empty(),
        "NO BAIL-OUT BOUNDARY WAS REACHED across {} fixtures, so the hand-off to the \
         accurate scheduler — where ADR 0011 §6's bailout invariant lives — is \
         entirely unexercised",
        selected.len()
    );

    let missing: Vec<&'static str> = BailOut::ALL
        .iter()
        .filter(|r| !total.bailed.contains(**r))
        .map(|r| r.describe())
        .collect();
    assert!(
        missing.is_empty(),
        "{} of {} bail-out reasons were never reached by any fixture:\n  - {}\n\
         Add a fixture that reaches each, or the gate reports agreement about a \
         hand-off nobody exercised (ADR 0012 §2).",
        missing.len(),
        BailOut::ALL.len(),
        missing.join("\n  - ")
    );

    println!(
        "GATE COMPLETE: {} fixtures, {} blocks executed, {}/{} bail-out \
         reasons covered",
        selected.len(),
        total.blocks,
        BailOut::ALL.len(),
        BailOut::ALL.len()
    );
}

/// The witness must be able to **fail**, which a passing run cannot demonstrate.
///
/// Two mutations, checked directly rather than by hand-editing the gate: a
/// coverage set missing a reason must be detected, and an empty one must be too.
/// Without this, a `contains` that returned `true` unconditionally — or a
/// `BailOut::ALL` that was somehow empty — would leave the witness green forever
/// while asserting nothing, which is the exact shape ADR 0012 §2 was written about.
#[test]
fn the_completion_witness_detects_a_coverage_hole() {
    let mut full = BailOutSet::new();
    for &reason in BailOut::ALL {
        full.insert(reason);
    }
    assert!(
        BailOut::ALL.iter().all(|r| full.contains(*r)),
        "a set built from every reason must cover every reason"
    );

    // Drop exactly one reason and require the check that the witness performs to
    // notice. Done per reason, so this stays true as the enumeration grows.
    for &dropped in BailOut::ALL {
        let mut partial = BailOutSet::new();
        for &reason in BailOut::ALL {
            if reason != dropped {
                partial.insert(reason);
            }
        }
        assert!(
            !partial.contains(dropped),
            "the coverage set claims {dropped:?} was reached when it was omitted"
        );
        let missing = BailOut::ALL
            .iter()
            .filter(|r| !partial.contains(**r))
            .count();
        assert_eq!(
            missing, 1,
            "omitting {dropped:?} must leave exactly one reason uncovered"
        );
    }

    assert!(
        BailOutSet::new().is_empty(),
        "an empty coverage set must report itself empty, or the \"no boundary was \
         reached\" failure can never fire"
    );
}
