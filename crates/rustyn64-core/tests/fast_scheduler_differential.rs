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

use rustyn64_core::scheduler::System;

/// A seed with no special structure — the phase alignment it derives is what makes
/// the two runs a real comparison rather than two runs of the same schedule.
const SEED: u64 = 0x5265_616C_6974_7921;

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
/// While `run_until_fast` bails out on every step this is trivially true, and that
/// is the point: the gate has to exist and pass *before* block execution is
/// written, or it grades nothing at the moment it is most needed. Once blocks
/// start executing, this is the test that fails.
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

    let (a, f) = (state_of(&accurate), state_of(&fast));
    assert!(a == f, "{}", describe_divergence(&a, &f));
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
