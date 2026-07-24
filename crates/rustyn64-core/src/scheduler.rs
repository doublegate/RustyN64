//! The canonical master-clock scheduler — the heart of the emulator.
//!
//! # Timebase model (ADR 0006)
//!
//! The tick unit is **187.5 MHz** ([`MASTER_HZ`]) — the LCM of the VR4300's
//! 93.75 MHz `PClock` and the RCP's 62.5 MHz `MClock`. That makes **every** emulated
//! clock domain an integer divisor of one counter:
//!
//! | Component | Rate | Divider |
//! |-----------|------------|---------|
//! | VR4300 `PClock` | 93.75 MHz | 2 |
//! | RCP (`MClock` / `SClock`) | 62.5 MHz | 3 |
//! | COP0 `Count` (half `PClock`) | 46.875 MHz | 4 |
//! | Serial Interface | 15.625 MHz | 12 |
//! | Cartridge / PIF | 1.953125 MHz | 96 |
//!
//! No accumulator, no remainder, no drift — drift is *unrepresentable* rather
//! than merely avoided. Note the hardware derives the CPU **from** `MClock` (a 3/2
//! PLL at `DivMode = 0b01`), not the reverse; ADR 0001 had the CPU as master,
//! which inverted that and is why the RCP needed a fractional remainder.
//!
//! Only the VI (VCLK, ~48.68 MHz, off a *different* crystal) and the AI genuinely
//! are not rational multiples of this tick; those keep a fractional accumulator.
//!
//! # The one rule
//!
//! **`master_ticks` is the only counter in the core that is ever incremented.**
//! Every other cycle position is *derived* — see [`System::cpu_cycles`] and
//! [`System::rcp_cycles`], which are accessors rather than fields. A second
//! incremented counter agrees with the first only because every call site
//! remembers to step it: an invariant held by construction rather than by
//! derivation, correct until one path forgets. The `residue_invariants_never_move`
//! test exists to catch exactly that.
//!
//! A *retired-work* tally is a different thing and is legitimate
//! (`Cpu::retired`); it counts work done and nothing schedules against it.
//!
//! # Lockstep, edge to edge
//!
//! This is **LOCKSTEP**, not catch-up: an RCP event (a DP-done IRQ, an SP halt) is
//! visible to the very next CPU step. But nothing iterates 187.5M times a second —
//! the CPU lands on every 2nd tick and the RCP on every 3rd, so the pattern repeats
//! every 6 ticks and [`System::step_to_next_edge`] jumps straight to the next tick
//! where something is due. The unit is a time base, not a loop counter.
//!
//! There are **never** OS threads in the core; one timeline is the whole reason the
//! determinism contract holds (ADR 0004). Rollback / run-ahead live in the frontend.
//!
//! See `docs/scheduler.md` and `docs/adr/0006-one-canonical-master-clock.md`.

// The PRNG step and the skeleton `reset` are flagged const-able, but they will
// gain non-const bodies (reset warms the Bus subsystems); accept at module level.
#![allow(clippy::missing_const_for_fn)]

use crate::bus::Bus;
use rustyn64_cpu::Cpu;

/// The canonical master tick rate: **187.5 MHz**, the LCM of the CPU and RCP clocks.
///
/// Beware the name: the *hardware* documentation uses `MasterClock` for the
/// 62.5 MHz [`RCP_HZ`], and superseded ADR 0001 used it for [`CPU_HZ`]. Always
/// state the rate — see `docs/glossary.md`.
pub const MASTER_HZ: u64 = 187_500_000;

/// The VR4300 pipeline clock (`PClock`), 93.75 MHz. `MASTER_HZ / CPU_DIVIDER`.
pub const CPU_HZ: u64 = 93_750_000;

/// The RCP clock (`MClock`, and the CPU's `SClock` bus rate), 62.5 MHz.
pub const RCP_HZ: u64 = 62_500_000;

/// Master ticks per VR4300 `PClock`.
pub const CPU_DIVIDER: u64 = 2;

/// Master ticks per RCP (`MClock`) cycle.
pub const RCP_DIVIDER: u64 = 3;

/// Master ticks per COP0 `Count` increment — `Count` runs at **half** `PClock`
/// (46.875 MHz), per VR4300 User's Manual §6.3.3 Figure 6-3. Forgetting the
/// halving is a documented source of 2x timing bugs.
pub const COUNT_DIVIDER: u64 = 4;

/// Master ticks per Serial Interface cycle (15.625 MHz).
pub const SI_DIVIDER: u64 = 12;

/// Master ticks per cartridge / PIF cycle (1.953125 MHz).
pub const PIF_DIVIDER: u64 = 96;

/// The CPU:RCP interleaving period: `lcm(CPU_DIVIDER, RCP_DIVIDER)` master ticks.
///
/// The CPU lands on ticks 0, 2, 4 and the RCP on 0, 3, so they coincide only
/// every 6. Seeded power-on phases are offsets within this period.
pub const PHASE_PERIOD: u64 = 6;

/// A tiny deterministic `SplitMix64` PRNG.
///
/// Used ONLY for the seeded power-on phase alignment — the determinism contract
/// forbids the OS RNG (and system time / thread scheduling) anywhere in the core.
#[derive(Debug, Clone)]
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// The seeded power-on phase offsets, one per clock domain.
///
/// These are **constants, not counters** — nothing increments them, so the
/// one-incremented-counter rule is intact. They must be per-domain: if every
/// domain keyed off the same absolute tick, the CPU/RCP interleaving would be
/// byte-identical for every seed from tick 6 onward, and the seeded phase ADR 0004
/// requires would be decorative — `reset_preserves_phase` would still pass while
/// testing nothing.
///
/// The hardware basis is UM Table 11-1's "**1 to 2** `PCycles`: synchronize with
/// `SClock`" line, an indeterminacy the vendor documents at exactly this boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Phases {
    cpu: u64,
    rcp: u64,
}

impl Phases {
    /// Derive both offsets from one seed. Split out so the modulo reduction is
    /// provably in range and never appears in the hot path.
    fn from_seed(seed: u64) -> Self {
        let mut rng = SplitMix64::new(seed);
        Self {
            cpu: rng.next_u64() % CPU_DIVIDER,
            rcp: rng.next_u64() % RCP_DIVIDER,
        }
    }
}

/// Owns the run loop and ties the CPU to the Bus on one timeline.
///
/// Determinism contract: same seed + ROM + input ⇒ bit-identical A/V (ADR 0004).
#[derive(Debug)]
pub struct System {
    /// The CPU.
    pub cpu: Cpu,
    /// The Bus — owns everything else mutable (RDRAM / RSP / RDP / AI / cart /
    /// controllers / RCP registers).
    pub bus: Bus,
    /// **The** counter. Nothing else in the core is incremented.
    master_ticks: u64,
    /// Seeded per-domain power-on phase offsets (constants, not counters).
    phases: Phases,
    /// The determinism seed, retained so `reset` re-derives the same phases.
    seed: u64,
}

impl System {
    /// Power on with a determinism seed (drives the per-domain phase alignment).
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            cpu: Cpu::new(),
            bus: Bus::default(),
            master_ticks: 0,
            phases: Phases::from_seed(seed),
            seed,
        }
    }

    /// Reset (warm). Re-derives the SAME phase alignment from the retained seed
    /// so a reset mid-run stays deterministic.
    pub fn reset(&mut self) {
        self.phases = Phases::from_seed(self.seed);
        self.master_ticks = 0;
        self.cpu = Cpu::new();
        // The VI scan timeline is keyed off `master_ticks`, so it must rebase to
        // the new zero — otherwise its delta baseline stays in the old timeline
        // and the VI interrupt is suppressed until the run catches up.
        self.bus.vi.reset_scan();
        // TODO(T-CORE-03): warm-reset the remaining Bus subsystems (RSP halt,
        // clear DMA) without zeroing RDRAM — see `docs/scheduler.md`.
    }

    /// Master ticks elapsed since power-on. The canonical time position.
    #[must_use]
    pub const fn master_ticks(&self) -> u64 {
        self.master_ticks
    }

    /// The CPU's position on the timeline, in `PClock`s. **Derived, not stored.**
    ///
    /// This is a *position*, not a count of work done — with a nonzero seeded
    /// phase it is nonzero before the CPU has stepped, which is correct and is
    /// what the residue invariant pins. For work retired, use [`Cpu::retired`].
    #[must_use]
    pub const fn cpu_cycles(&self) -> u64 {
        (self.master_ticks + self.phases.cpu) / CPU_DIVIDER
    }

    /// The RCP's position on the timeline, in `MClock`s. **Derived, not stored.**
    #[must_use]
    pub const fn rcp_cycles(&self) -> u64 {
        (self.master_ticks + self.phases.rcp) / RCP_DIVIDER
    }

    /// The COP0 `Count` *offset* since power-on, at half `PClock`.
    ///
    /// Not the architectural register: `Count` is guest-writable via `MTC0`, so
    /// the real value is affine — `epoch_value + (master_ticks - epoch_tick) /
    /// COUNT_DIVIDER`, re-based on every write. The affine half lives in the CPU
    /// (`rustyn64_cpu::cop0`, T-12-003) and is fed from here via `tick_at`; this
    /// is the timeline half.
    #[must_use]
    pub const fn count_ticks(&self) -> u64 {
        (self.master_ticks + self.phases.cpu) / COUNT_DIVIDER
    }

    /// Is `tick` an edge for a domain with this divider and phase?
    const fn is_edge(tick: u64, phase: u64, divider: u64) -> bool {
        (tick + phase).is_multiple_of(divider)
    }

    /// The next tick strictly after `tick` at which this domain steps.
    const fn next_edge_after(tick: u64, phase: u64, divider: u64) -> u64 {
        let next = tick + 1;
        let rem = (next + phase) % divider;
        if rem == 0 {
            next
        } else {
            next + (divider - rem)
        }
    }

    /// The next tick strictly after the current one at which any domain is due.
    const fn next_edge(&self) -> u64 {
        let next_cpu = Self::next_edge_after(self.master_ticks, self.phases.cpu, CPU_DIVIDER);
        let next_rcp = Self::next_edge_after(self.master_ticks, self.phases.rcp, RCP_DIVIDER);
        if next_cpu < next_rcp {
            next_cpu
        } else {
            next_rcp
        }
    }

    /// Step every domain due at the *current* tick.
    ///
    /// CPU first, then the RCP, so an RCP event lands where the next CPU step
    /// sees it. Reversing this changes which engine observes whose write first
    /// and is a determinism-visible change.
    fn step_due_here(&mut self) {
        if Self::is_edge(self.master_ticks, self.phases.cpu, CPU_DIVIDER) {
            // `count_ticks` is derived from `master_ticks`, never incremented,
            // and the CPU turns it into the guest-writable `Count` (ADR 0006).
            let count_now = self.count_ticks();
            self.cpu.tick_at(&mut self.bus, count_now);
        }
        if Self::is_edge(self.master_ticks, self.phases.rcp, RCP_DIVIDER) {
            self.step_rcp();
        }
    }

    /// Advance to the next tick at which **any** domain is due, and step every
    /// domain due at that tick. Returns the tick landed on.
    ///
    /// Edge-to-edge: the master tick is never iterated. Hot path — allocation-free.
    ///
    /// Note the tick the machine currently sits on is never re-stepped, so tick 0
    /// is a position rather than an executed edge. Only the *intervals* between
    /// ticks carry work, which is what keeps the residue invariant constant.
    pub fn step_to_next_edge(&mut self) -> u64 {
        self.master_ticks = self.next_edge();
        self.step_due_here();
        self.master_ticks
    }

    /// Run until `master_ticks() == target`, stepping every domain on every edge
    /// in `(now, target]` — and **not** one past it.
    ///
    /// Overshooting would make "how many CPU steps in N ticks" depend on where
    /// the edges happened to fall, which is exactly the kind of off-by-a-phase
    /// error the residue invariant is meant to make impossible.
    pub fn run_until(&mut self, target: u64) {
        while self.next_edge() <= target {
            self.master_ticks = self.next_edge();
            self.step_due_here();
        }
        if self.master_ticks < target {
            self.master_ticks = target;
        }
    }

    /// One RCP step: the RSP microcode unit, then the RDP rasterizer, then the
    /// AI/interface DMA progress — all on the SAME `&mut self.bus`.
    fn step_rcp(&mut self) {
        // The chips each see only their narrow trait of `self.bus`.
        // TODO(v0.x): LLE RSP scalar+vector execution — `Rsp::tick` is a stub.
        self.bus.rsp_tick();
        // TODO(v0.x): LLE RDP rasterizer — `Rdp::tick` is a stub.
        self.bus.rdp_tick();
        // AI / interface sub-clock advance — derives sample emission off the
        // canonical `master_ticks` (ADR 0006), like the VI scan below.
        self.bus.audio_tick(self.master_ticks);
        // The PI's asynchronous direct-I/O write finalises on this clock.
        self.bus.pi_tick();
        // The VI scan position advances off `master_ticks` (the one fractional
        // domain, ADR 0006 / `docs/scheduler.md`); a `VI_V_INTR` crossing raises
        // the VI line into the MI.
        if self.bus.vi.tick(self.master_ticks) {
            self.bus.rcp.mi_intr.vi = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// `rustyn64-audio` duplicates `MASTER_HZ` (the chip-crate graph forbids it
    /// depending on `-core`); this pins the two copies together so the AI's DAC
    /// period cannot silently drift from the canonical clock.
    #[test]
    fn audio_crate_master_hz_matches() {
        assert_eq!(rustyn64_audio::MASTER_HZ, MASTER_HZ);
    }

    /// The divisors must be exact. A wrong one is silent and poisons everything.
    #[test]
    fn every_divider_is_exact() {
        assert_eq!(MASTER_HZ / CPU_DIVIDER, CPU_HZ);
        assert_eq!(MASTER_HZ % CPU_DIVIDER, 0);
        assert_eq!(MASTER_HZ / RCP_DIVIDER, RCP_HZ);
        assert_eq!(MASTER_HZ % RCP_DIVIDER, 0);
        // COP0 Count is half `PClock` (UM §6.3.3).
        assert_eq!(MASTER_HZ / COUNT_DIVIDER, CPU_HZ / 2);
        assert_eq!(MASTER_HZ / SI_DIVIDER, 15_625_000);
        assert_eq!(MASTER_HZ / PIF_DIVIDER, 1_953_125);
        // 187.5 MHz really is the LCM of the two core clocks.
        assert_eq!(CPU_HZ * CPU_DIVIDER, MASTER_HZ);
        assert_eq!(RCP_HZ * RCP_DIVIDER, MASTER_HZ);
    }

    #[test]
    fn three_cpu_and_two_rcp_steps_per_six_ticks() {
        for seed in [0, 1, 0xDEAD_BEEF, u64::MAX] {
            let mut sys = System::new(seed);
            let rcp_before = sys.bus.rcp_steps_for_test();
            // Count CPU *steps* from the derived position, not from `Cpu::retired`
            // -- since ADR 0007 the CPU is a 5-stage pipeline, so retirement lags
            // stepping by the pipeline depth and the two are not interchangeable.
            let cpu_before = sys.cpu_cycles();
            sys.run_until(PHASE_PERIOD);
            assert_eq!(
                sys.cpu_cycles() - cpu_before,
                3,
                "3 CPU steps per 6 master ticks (seed {seed})"
            );
            assert_eq!(
                sys.bus.rcp_steps_for_test() - rcp_before,
                2,
                "2 RCP steps per 6 master ticks (seed {seed})"
            );
        }
    }

    #[test]
    fn reset_preserves_phase() {
        let mut sys = System::new(0xDEAD_BEEF);
        let phases = sys.phases;
        sys.step_to_next_edge();
        sys.reset();
        assert_eq!(sys.phases, phases);
        assert_eq!(sys.master_ticks(), 0);
    }

    /// Different seeds must produce genuinely different CPU↔RCP interleavings,
    /// not merely a different starting point in an identical pattern.
    ///
    /// This is the test that fails if the per-domain phase offsets are ever
    /// collapsed into one offset on `master_ticks` — the defect the ADR review
    /// caught in the design before it was written.
    #[test]
    fn seeds_produce_distinct_interleavings() {
        let fingerprint = |seed: u64| -> Vec<(bool, bool)> {
            let sys = System::new(seed);
            (0..PHASE_PERIOD)
                .map(|t| {
                    (
                        System::is_edge(t, sys.phases.cpu, CPU_DIVIDER),
                        System::is_edge(t, sys.phases.rcp, RCP_DIVIDER),
                    )
                })
                .collect()
        };
        let mut seen: Vec<Vec<(bool, bool)>> = Vec::new();
        for seed in 0..64u64 {
            let f = fingerprint(seed);
            if !seen.contains(&f) {
                seen.push(f);
            }
        }
        assert!(
            seen.len() > 1,
            "all seeds produced one interleaving -- the per-domain phase offsets \
             have collapsed and the seeded power-on phase is decorative"
        );
    }

    /// **The residue invariant.** Every derived position must stay in a fixed
    /// affine relationship with `master_ticks`. A position that has become
    /// independently incremented drifts out of it on the first path that forgets
    /// to step it — the failure mode ADR 0006 exists to prevent.
    #[test]
    fn residue_invariants_never_move() {
        fn sample(s: &System) -> (i64, i64, i64) {
            let master = i64::try_from(s.master_ticks()).unwrap();
            let cpu_pos = i64::try_from(s.cpu_cycles()).unwrap();
            let rcp_pos = i64::try_from(s.rcp_cycles()).unwrap();
            (
                master - i64::try_from(CPU_DIVIDER).unwrap() * cpu_pos,
                master - i64::try_from(RCP_DIVIDER).unwrap() * rcp_pos,
                // the two domains against each other -- catches inter-domain
                // drift even if each stayed affine to master individually.
                // NOT `cpu.retired`: since ADR 0007 the CPU is a 5-stage
                // pipeline, so retirement lags stepping and is a CPU property
                // rather than a clock one.
                i64::try_from(CPU_DIVIDER).unwrap() * cpu_pos
                    - i64::try_from(RCP_DIVIDER).unwrap() * rcp_pos,
            )
        }

        let mut sys = System::new(0x1234_5678_9ABC_DEF0);
        // Sample at period boundaries so the comparison point is consistent; the
        // residues must then be constant forever.
        sys.run_until(PHASE_PERIOD);
        let first = sample(&sys);
        for period in 1..64u64 {
            sys.run_until(PHASE_PERIOD * (period + 1));
            assert_eq!(
                sample(&sys),
                first,
                "residues moved at period {period} -- a cycle position is being \
                 incremented independently instead of derived from master_ticks"
            );
        }
    }

    /// Same seed ⇒ identical timeline. The determinism contract's floor.
    #[test]
    fn same_seed_same_timeline() {
        let mut a = System::new(42);
        let mut b = System::new(42);
        for _ in 0..256 {
            assert_eq!(a.step_to_next_edge(), b.step_to_next_edge());
            assert_eq!(a.cpu_cycles(), b.cpu_cycles());
            assert_eq!(a.rcp_cycles(), b.rcp_cycles());
        }
    }

    #[test]
    fn edges_are_never_skipped() {
        let mut sys = System::new(7);
        let mut prev = sys.master_ticks();
        for _ in 0..512 {
            let now = sys.step_to_next_edge();
            assert!(now > prev, "the scheduler must advance");
            // The gap can never exceed the coarsest divider we step on.
            assert!(now - prev <= RCP_DIVIDER, "an edge was skipped");
            prev = now;
        }
    }

    /// **A running system raises the VI interrupt as the scan crosses
    /// `VI_V_INTR`.** With a standard NTSC field (525 half-lines) and the VI on,
    /// stepping past `per-half-line × V_INTR` master ticks drives `MI_INTR.vi`
    /// through the scheduler's per-step `Vi::tick` call.
    #[test]
    fn a_running_system_raises_the_vi_interrupt_at_v_intr() {
        use crate::vi::{VI_CTRL, VI_V_INTR, VI_V_TOTAL};
        let mut sys = System::new(1);
        sys.bus.vi.regs[VI_V_TOTAL as usize] = 524; // 525 half-lines
        sys.bus.vi.regs[VI_V_INTR as usize] = 2;
        sys.bus.vi.regs[VI_CTRL as usize] = 2; // VI on
        assert!(!sys.bus.rcp.mi_intr.vi, "clear before running");
        // per-half-line ≈ 5952 ticks; 15_000 is well past half-line 2.
        while sys.master_ticks() < 15_000 {
            sys.step_to_next_edge();
        }
        assert!(
            sys.bus.rcp.mi_intr.vi,
            "the VI interrupt fired during the run"
        );
    }

    /// **The VI keeps firing after a reset.** A reset zeroes `master_ticks`, so
    /// the VI scan timeline must rebase (`Vi::reset_scan`) — otherwise its delta
    /// baseline stays in the old timeline and the interrupt is suppressed until
    /// the new run catches up. Acknowledge → reset → the next field fires again.
    #[test]
    fn a_reset_rebases_the_vi_scan_so_it_fires_again() {
        use crate::vi::{VI_CTRL, VI_V_INTR, VI_V_TOTAL};
        let mut sys = System::new(2);
        sys.bus.vi.regs[VI_V_TOTAL as usize] = 524;
        sys.bus.vi.regs[VI_V_INTR as usize] = 2;
        sys.bus.vi.regs[VI_CTRL as usize] = 2;
        while sys.master_ticks() < 15_000 {
            sys.step_to_next_edge();
        }
        assert!(sys.bus.rcp.mi_intr.vi, "fires before reset");
        sys.bus.rcp.mi_intr.vi = false; // the CPU would ack via VI_V_CURRENT
        sys.reset();
        assert!(!sys.bus.rcp.mi_intr.vi, "clear immediately after reset");
        while sys.master_ticks() < 15_000 {
            sys.step_to_next_edge();
        }
        assert!(
            sys.bus.rcp.mi_intr.vi,
            "fires again after the reset rebases"
        );
    }
}
