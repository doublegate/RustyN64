//! The fractional master-clock lockstep scheduler — the heart of the emulator.
//!
//! # Timebase model
//!
//! The N64 runs the VR4300 at **93.75 MHz** and the RCP (RSP + RDP) at
//! **62.5 MHz** — exactly a **3:2** ratio. Rather than two free-running clocks
//! (which drift and force per-quirk resync patches, the trap `RustyNES`'s
//! postmortem warns against), we co-schedule everything on ONE fractional
//! master timeline. The master tick unit is the **VR4300 cycle**
//! ([`MASTER_HZ`] = `93_750_000` Hz); the RCP advances on a **2/3** fractional
//! divisor accumulator: every 3 master ticks the RCP gets 2 ticks.
//!
//! | Component        | Rate        | Divisor vs. master (VR4300 cycles) |
//! |------------------|-------------|------------------------------------|
//! | VR4300 (master)  | 93.75 MHz   | 1   (the tick unit)                |
//! | RSP + RDP (RCP)  | 62.5  MHz   | 3:2 — 2 RCP ticks per 3 master     |
//! | AI / interfaces  | derived     | sub-divided off the RCP / VI clock |
//!
//! This is **LOCKSTEP**, not catch-up: a mid-instruction RCP event (a DP-done
//! IRQ, an SP halt) is visible to the very next CPU step. It also means there
//! are **NEVER** OS threads in the core — one timeline is the whole reason the
//! determinism contract (same seed + ROM + input ⇒ bit-identical A/V) holds.
//! Netplay rollback / run-ahead orchestration lives in the frontend, never here.
//!
//! See `docs/scheduler.md` for the full divisor derivation (the docs sub-agent
//! expands it).

// The PRNG step and the skeleton `reset` are flagged const-able, but they will
// gain non-const bodies (reset warms the Bus subsystems); accept at module level.
#![allow(clippy::missing_const_for_fn)]

use crate::bus::Bus;
use rustyn64_cpu::Cpu;

/// The fractional master clock rate: the VR4300 cycle rate, 93.75 MHz.
pub const MASTER_HZ: u64 = 93_750_000;

/// The RCP (RSP + RDP) clock rate, 62.5 MHz.
pub const RCP_HZ: u64 = 62_500_000;

/// RCP-ticks numerator of the master:RCP fractional divisor (2 RCP per 3 master).
pub const RCP_NUM: u32 = 2;
/// Master-ticks denominator of the master:RCP fractional divisor.
pub const RCP_DEN: u32 = 3;

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

/// Derive the seeded power-on phase in `0..RCP_DEN`. Split out so the `as u32`
/// never appears in the hot path and the value is provably in range.
fn phase_from_seed(seed: u64) -> u32 {
    let r = SplitMix64::new(seed).next_u64() % u64::from(RCP_DEN);
    // `r < RCP_DEN` (a small constant), so this always succeeds.
    u32::try_from(r).unwrap_or(0)
}

/// Owns the run loop and ties the CPU to the Bus on one timeline.
///
/// Determinism contract: same seed + ROM + input ⇒ bit-identical A/V.
#[derive(Debug)]
pub struct System {
    /// The CPU (the timing master).
    pub cpu: Cpu,
    /// The Bus — owns everything else mutable (RDRAM / RSP / RDP / AI / cart /
    /// controllers / RCP registers).
    pub bus: Bus,
    /// Per-power-on phase alignment (0..`RCP_DEN`), from the SEEDED PRNG.
    phase: u32,
    /// The fractional RCP-divisor accumulator (counts master ticks mod `RCP_DEN`).
    rcp_accum: u32,
    /// Total elapsed master (VR4300) ticks since power-on.
    master_ticks: u64,
    /// The determinism seed, retained so `reset` can preserve phase alignment.
    seed: u64,
}

impl System {
    /// Power on with a determinism seed (drives the fractional phase alignment).
    #[must_use]
    pub fn new(seed: u64) -> Self {
        let phase = phase_from_seed(seed);
        Self {
            cpu: Cpu::new(),
            bus: Bus::default(),
            phase,
            rcp_accum: phase,
            master_ticks: 0,
            seed,
        }
    }

    /// Reset (warm). Re-derives the SAME phase alignment from the retained seed
    /// so a reset mid-run stays deterministic (`RustyNES` contract: reset
    /// preserves alignment).
    pub fn reset(&mut self) {
        self.phase = phase_from_seed(self.seed);
        self.rcp_accum = self.phase;
        self.cpu = Cpu::new();
        // TODO(T-CORE-03): warm-reset the Bus subsystems (RSP halt, clear DMA)
        // without zeroing RDRAM — see `docs/scheduler.md`.
    }

    /// Master ticks elapsed since power-on.
    #[must_use]
    pub const fn master_ticks(&self) -> u64 {
        self.master_ticks
    }

    /// Advance the machine by one master (VR4300) tick, then the RCP on its
    /// fractional 2/3 divisor. LOCKSTEP — each chip sees the others' just-made
    /// state. Hot path: allocation-free.
    pub fn tick_one_unit(&mut self) {
        // The VR4300 advances every master tick (it IS the tick unit).
        self.cpu.tick(&mut self.bus);

        // The RCP (RSP + RDP) advances on the 2/3 fractional divisor: add the
        // numerator each master tick and fire one RCP tick per denominator wrap.
        self.rcp_accum += RCP_NUM;
        while self.rcp_accum >= RCP_DEN {
            self.rcp_accum -= RCP_DEN;
            self.step_rcp();
        }

        self.master_ticks = self.master_ticks.wrapping_add(1);
    }

    /// One RCP step: the RSP microcode unit, then the RDP rasterizer, then the
    /// AI/interface DMA progress — all on the SAME `&mut self.bus`.
    fn step_rcp(&mut self) {
        // The chips each see only their narrow trait of `self.bus`.
        // TODO(v0.x): LLE RSP scalar+vector execution — `Rsp::tick` is a stub.
        self.bus.rsp_tick();
        // TODO(v0.x): LLE RDP rasterizer — `Rdp::tick` is a stub.
        self.bus.rdp_tick();
        // AI / interface sub-clock advance.
        self.bus.audio_tick();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fractional_divisor_holds_3_to_2() {
        let mut sys = System::new(0);
        sys.phase = 0;
        sys.rcp_accum = 0;
        // Over 3 master ticks the RCP should advance exactly 2 times.
        let before = sys.bus.rcp_steps_for_test();
        for _ in 0..3 {
            sys.tick_one_unit();
        }
        assert_eq!(sys.bus.rcp_steps_for_test() - before, 2);
    }

    #[test]
    fn reset_preserves_phase() {
        let mut sys = System::new(0xDEAD_BEEF);
        let phase = sys.phase;
        sys.tick_one_unit();
        sys.reset();
        assert_eq!(sys.phase, phase);
    }
}
