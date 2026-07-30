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
const PHASE_SEEDS: core::ops::Range<u64> = 0..16;

/// Long enough that both domains step many thousands of times and the CPU retires
/// real work; short enough to stay in the default `cargo test` path.
const TICKS: u64 = 400_000;

/// Serialize a machine's full state for comparison.
fn state_of(sys: &System) -> Vec<u8> {
    bincode::serialize(sys).expect("a System is serializable; save-states rely on it")
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
    fast.run_until_fast(TICKS);

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
/// alignment rather than depth of execution, and sixteen full-length runs would put
/// this outside the default `cargo test` budget.
#[test]
fn the_fast_path_agrees_at_every_phase_alignment() {
    // Deliberately NOT a multiple of the edge period. 60_000 is, and with a target
    // that lands exactly on a period boundary the partial-period tail — the part
    // that falls back to the accurate loop — never runs at all. The offsets below
    // sweep every possible remainder, so each alignment is tested with each tail
    // length rather than only the one that happens to be zero.
    const BASE: u64 = 60_000;

    for seed in PHASE_SEEDS {
        let target = BASE + seed % (EDGE_PERIOD + 1);
        let mut accurate = System::new(seed);
        let mut fast = System::new(seed);
        accurate.run_until(target);
        fast.run_until_fast(target);

        assert!(
            accurate.cpu.retired > 0,
            "seed {seed} (target {target}): the accurate run retired nothing, so the comparison is vacuous"
        );
        assert_eq!(
            fast.master_ticks(),
            accurate.master_ticks(),
            "seed {seed} (target {target}): correct-but-late — the fast path finished at a different tick"
        );
        assert_eq!(
            fast.cpu.retired, accurate.cpu.retired,
            "seed {seed} (target {target}): the two paths retired different instruction counts"
        );

        let (a, f) = (state_of(&accurate), state_of(&fast));
        assert!(
            a == f,
            "seed {seed} (target {target}): {}",
            describe_divergence(&a, &f)
        );
    }
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
    sys.run_until_fast(10_000);
    let (ticks, retired) = (sys.master_ticks(), sys.cpu.retired);
    sys.run_until_fast(ticks);
    sys.run_until_fast(ticks - 1);
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
        f.run_until_fast(TICKS);
        fast_best = fast_best.min(t.elapsed().as_secs_f64());
    }
    println!(
        "accurate {acc_best:.4}s  fast {fast_best:.4}s  ratio {:.4}x  ({TICKS} ticks, best of {REPS})",
        acc_best / fast_best
    );
}
