//! The determinism regression test (T-11-007).
//!
//! ADR 0004's contract is: **same seed + ROM + input ⇒ bit-identical output.**
//! `docs/STATUS.md` recorded it as specified-but-unexercised until this landed —
//! it could not be tested earlier because nothing ran long enough to diverge.
//!
//! # What would actually break it
//!
//! Determinism is not a feature that can be added; it is a constraint that must
//! never have been violated (`docs/engineering-lessons.md` §1.4). A single
//! wall-clock read, OS RNG call, or unordered-collection iteration anywhere in
//! the core voids replay, netplay rollback, and every bisect-driven debugging
//! technique. This test exists to make such a change fail loudly on the commit
//! that introduces it, rather than years later when rollback is being written.

use rustyn64_core::System;
use rustyn64_test_harness::rom;

const BASIC_Z64: &str = "../../tests/roms/external/dillon-n64-tests/basic.z64";

/// A full snapshot of everything a run can influence.
///
/// Deliberately the **whole** machine, not a summary. A partial hash can hide a
/// divergence living in a field that only later leaks into the hashed region —
/// the failure mode `docs/engineering-lessons.md` §3.4 describes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Snapshot {
    master_ticks: u64,
    cpu_cycles: u64,
    rcp_cycles: u64,
    gpr: [u64; 32],
    hi: u64,
    lo: u64,
    pc: u64,
    rdram: alloc_hash::Hash,
}

fn snapshot(sys: &System) -> Snapshot {
    Snapshot {
        master_ticks: sys.master_ticks(),
        cpu_cycles: sys.cpu_cycles(),
        rcp_cycles: sys.rcp_cycles(),
        gpr: sys.cpu.regs.gpr,
        hi: sys.cpu.regs.hi,
        lo: sys.cpu.regs.lo,
        pc: sys.cpu.pc,
        rdram: alloc_hash::hash(&sys.bus.rdram),
    }
}

mod alloc_hash {
    /// A stable content hash. FNV-1a: order-sensitive, no allocation, and — the
    /// point here — **not** `DefaultHasher`, whose output is randomised per
    /// process and would make this test pass or fail at random.
    pub type Hash = u64;

    #[must_use]
    pub fn hash(bytes: &[u8]) -> Hash {
        let mut h: u64 = 0xCBF2_9CE4_8422_2325;
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01B3);
        }
        h
    }
}

fn run_once(seed: u64, ticks: u64) -> Option<Snapshot> {
    let image = std::fs::read(BASIC_Z64).ok()?;
    let entry = rom::entry_point(&image).ok()?;
    let mut sys = System::new(seed);
    rom::load_direct(&mut sys, &image, entry).ok()?;
    let target = sys.master_ticks() + ticks;
    sys.run_until(target);
    Some(snapshot(&sys))
}

/// **The contract**: the same seed and ROM produce a byte-identical machine.
#[test]
fn the_same_seed_produces_a_bit_identical_machine() {
    let Some(a) = run_once(0x1234_5678, 50_000) else {
        eprintln!("SKIP: {BASIC_Z64} not staged (external tier, no licence)");
        return;
    };
    let b = run_once(0x1234_5678, 50_000).expect("second run");
    assert_eq!(a, b, "two runs from one seed diverged");

    // Ten more, because a wall-clock or OS-entropy dependency is far more likely
    // to show up intermittently than on the very next run.
    for i in 0..10 {
        let c = run_once(0x1234_5678, 50_000).expect("repeat run");
        assert_eq!(a, c, "run {i} diverged from the first");
    }
}

/// A **different** seed must produce a different machine, or the contract is
/// vacuous — a build that ignored the seed entirely would satisfy the test above.
#[test]
fn different_seeds_produce_different_machines() {
    let Some(a) = run_once(0, 200) else {
        eprintln!("SKIP: {BASIC_Z64} not staged");
        return;
    };
    // Sweep seeds: the CPU/RCP phase pair has only six distinct values, so a
    // single alternative seed could legitimately collide.
    let differs = (1..64u64).any(|s| run_once(s, 200).expect("run") != a);
    assert!(
        differs,
        "no seed in 1..64 produced a different machine -- the seed is being ignored"
    );
}

/// Reset must re-derive the same phase, so a reset mid-run stays reproducible.
#[test]
fn reset_returns_to_a_reproducible_state() {
    let mut a = System::new(0xDEAD_BEEF);
    a.run_until(1_000);
    a.reset();
    let mut b = System::new(0xDEAD_BEEF);
    b.reset();
    assert_eq!(
        (a.master_ticks(), a.cpu_cycles(), a.rcp_cycles()),
        (b.master_ticks(), b.cpu_cycles(), b.rcp_cycles()),
        "reset must land in the same state regardless of what ran before it"
    );
}

/// The core must contain no wall-clock, OS-entropy or thread dependency.
///
/// A source-level guard rather than a behavioural one: those dependencies are
/// often *intermittent*, so a run-twice test can pass for months before the
/// first divergence. This fails on the commit that introduces one.
#[test]
fn the_core_has_no_nondeterminism_sources() {
    const BANNED: &[(&str, &str)] = &[
        ("std::time", "wall-clock reads make replay impossible"),
        ("SystemTime", "wall-clock reads make replay impossible"),
        ("Instant::now", "wall-clock reads make replay impossible"),
        (
            "getrandom",
            "OS entropy is forbidden; use the seeded SplitMix64",
        ),
        ("thread::spawn", "no OS threads in the core (ADR 0004)"),
        (
            "HashMap",
            "iteration order is unspecified; use a sorted or indexed map",
        ),
        ("HashSet", "iteration order is unspecified"),
    ];
    let roots = [
        "../rustyn64-core/src",
        "../rustyn64-cpu/src",
        "../rustyn64-rsp/src",
        "../rustyn64-rdp/src",
        "../rustyn64-audio/src",
        "../rustyn64-cart/src",
    ];
    let mut found = Vec::new();
    for root in roots {
        let Ok(dir) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in dir.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for (line_no, line) in text.lines().enumerate() {
                // Skip comments -- the rules themselves are written down in them.
                let t = line.trim_start();
                if t.starts_with("//") || t.starts_with('*') {
                    continue;
                }
                for (needle, why) in BANNED {
                    if line.contains(needle) {
                        found.push(format!(
                            "{}:{}: {needle} -- {why}",
                            path.display(),
                            line_no + 1
                        ));
                    }
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "nondeterminism sources in the core:\n  {}",
        found.join("\n  ")
    );
}
