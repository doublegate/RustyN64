//! `rustyn64-cpu` — NEC VR4300 (MIPS R4300i) main CPU.
//!
//! 64-bit MIPS III core: 32 general-purpose registers, HI/LO, the CP0
//! system-control coprocessor (TLB + exceptions), the CP1 FPU, and the `SysAD`
//! bus interface. This is a **skeleton** — the real interpreter/JIT, the TLB,
//! and the FPU are major roadmap phases. Behavior is pinned against
//! `n64-systemtest` FIRST (test-ROM-is-spec), then implemented until it passes.
//!
//! Part of the one-directional chip-crate graph (see `docs/architecture.md`):
//! this crate does NOT depend on any other chip crate. It talks to the rest of
//! the machine through the [`Bus`] trait, which `rustyn64-core` implements.
//! `#![no_std]` + `alloc` so it cross-compiles to a bare-metal target; only the
//! frontend carries `std` + `unsafe`.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
// Truncating / sign casts are the canonical encoding for MIPS register
// arithmetic; we annotate once at module level rather than per line.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_lossless,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]
// Skeleton `tick`/step methods are deliberately non-`const`: they will gain
// real (non-const) bus-driven bodies as the chip is implemented. Accept the
// pedantic const-fn suggestion at module level rather than salt every stub.
#![allow(clippy::missing_const_for_fn)]

extern crate alloc;

pub mod addr;
pub mod alu;
pub mod cache;
pub mod cop0;
pub mod cop1;
pub mod decode;
pub mod exception;
pub mod exec;
pub mod fpr;
pub mod fpu;
pub mod mem;
pub mod pipeline;
pub mod regs;
pub mod softfloat;
pub mod sysad;
pub mod tlb;

pub use addr::{Cached, Physical, Segment, segment, translate_via};
pub use alu::{HiLo, MulDiv};
pub use decode::{Decoded, Op, decode};
pub use exec::{Executed, WriteBack, execute};
pub use mem::{LoadKind, StoreKind};
pub use pipeline::{Exception, Interlock, Latch, Pipeline, Stage};
pub use regs::Regs;
pub use sysad::{BlockOrder, Phase, Transaction, Width, block_order};

/// Which half of a `SysAD` bus transaction is on the wire.
///
/// The VR4300 talks to the RCP over the `SysAD` bus, which multiplexes the command
/// and the data onto the same lines; `SYSCMD` bit 4 is documented as "Command or
/// Data" and this enum mirrors that split.
///
/// **This describes the bus protocol and carries no interrupt semantics.** An
/// earlier revision paired it with a `poll_irq_at_phase(BusPhase)` hook, on the
/// assumption that interrupts are sampled at a particular half of a transaction.
/// No such coupling is documented anywhere — not in the User's Manual, not on the
/// wiki. The documented rule (UM §4.7.1) is per-`PCycle` and gated on stall state:
/// *"NMI and interrupt exception requests are accepted only if the previous
/// `PCycle` was a run cycle."* That hook was shaped on the wrong axis and was
/// removed rather than completed (ADR 0007).
///
/// `SysAD` runs at `SClock` = `MClock` = 62.5 MHz, so one bus cycle is 1.5 `PCycles` — 3
/// master ticks against the CPU's 2 (ADR 0006). This is *not* the deferred ADR
/// 0005 refactor, which concerns resolution finer than one `PClock`.
///
/// TODO(T-11-008): the transaction model that actually uses this.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum BusPhase {
    /// The command half — the CPU drives `SYSCMD` and the address.
    Command,
    /// The data half — the transfer itself, and where the result commits.
    Data,
}

/// The system-memory bus the VR4300 borrows during [`Cpu::tick`].
///
/// Implemented by `rustyn64-core::Bus`. Kept as a trait so the CPU can be
/// fuzzed and benchmarked against a tiny in-crate bus without pulling in the
/// rest of the machine.
pub trait Bus {
    /// Read a byte at a 32-bit physical address (post-TLB).
    fn read_u8(&mut self, addr: u32) -> u8;
    /// Write a byte at a 32-bit physical address (post-TLB).
    fn write_u8(&mut self, addr: u32, val: u8);

    /// Read an aligned big-endian 32-bit word. Default composes four byte
    /// reads; `rustyn64-core` overrides with a fast RDRAM path.
    fn read_u32(&mut self, addr: u32) -> u32 {
        let b = [
            self.read_u8(addr),
            self.read_u8(addr.wrapping_add(1)),
            self.read_u8(addr.wrapping_add(2)),
            self.read_u8(addr.wrapping_add(3)),
        ];
        u32::from_be_bytes(b)
    }

    /// Write an aligned big-endian 32-bit word.
    fn write_u32(&mut self, addr: u32, val: u32) {
        let b = val.to_be_bytes();
        self.write_u8(addr, b[0]);
        self.write_u8(addr.wrapping_add(1), b[1]);
        self.write_u8(addr.wrapping_add(2), b[2]);
        self.write_u8(addr.wrapping_add(3), b[3]);
    }

    /// Sample the pending-interrupt level: the MI lines masked by `MI_MASK`.
    ///
    /// Default: no interrupt pending. `rustyn64-core` overrides it.
    ///
    /// Sampling happens **once per `PClock` in the DC stage** (UM Figure 4-12 and
    /// §4.7.6, which lists the interrupt exception among the DC-stage priorities)
    /// and is accepted only if the previous `PCycle` was a run cycle (§4.7.1). The
    /// run-cycle gate lives in the pipeline, not here — this hook only reports the
    /// level. Exactly one recognition predicate exists in the tree.
    fn poll_irq(&mut self) -> bool {
        false
    }
}

/// NEC VR4300 architectural state.
///
/// This is the **skeleton** register file only; the decode/execute pipeline,
/// the CP0 TLB, and the CP1 FPU are roadmap phases left as marked TODOs.
#[derive(Debug, Clone)]
pub struct Cpu {
    /// The register file: 32 GPRs plus `HI`/`LO`. `$zero`'s hardwiring lives in
    /// [`Regs::read`]/[`Regs::write`] so no call site can forget it.
    pub regs: Regs,
    /// Program counter (virtual address).
    pub pc: u64,
    /// The five-stage pipeline: four inter-stage latches plus control state
    /// (ADR 0007). Advanced one `PClock` per [`Cpu::tick`].
    pub pipeline: Pipeline,
    /// Retired-work tally: instructions retired since power-on, for the
    /// golden-log differ.
    ///
    /// This is **not** a time position and nothing schedules against it — the
    /// scheduler derives every cycle position from its one `master_ticks` counter
    /// (ADR 0006). A work tally is the one kind of counter that is still allowed
    /// to be incremented; see `System::cpu_cycles()` for the CPU's *position*.
    pub retired: u64,
    // TODO(T-CPU-01): branch-delay-slot latch, CP0 registers + TLB entries,
    // CP1 (FPU) register file + control/status, LL/SC link bit — see `docs/cpu.md`.
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    /// Construct at power-on / cold reset.
    ///
    /// `gpr[0]` is the architectural zero register. Any phase alignment, where
    /// applicable, comes from the *seeded* scheduler PRNG (the determinism
    /// contract — see `docs/adr/0004`), never the OS RNG.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            regs: Regs::new(),
            // The reset vector, as a SIGN-EXTENDED 64-bit address. In 32-bit
            // addressing mode every valid address is one, and `0x0000_0000_BFC0_0000`
            // is not the same address -- it is an address error. n64-systemtest
            // asserts exactly that distinction ("LW with address not sign
            // extended"), so the truncated form is not a harmless shorthand.
            pc: 0xFFFF_FFFF_BFC0_0000,
            pipeline: Pipeline::new(),
            retired: 0,
        }
    }

    /// Set the program counter (used by golden-log automation harnesses).
    pub const fn set_pc(&mut self, pc: u64) {
        self.pc = pc;
    }

    /// Advance the CPU by **one `PClock`** — not one instruction.
    ///
    /// The scheduler calls this on every CPU edge (every 2nd master tick, ADR
    /// 0006). At least 5 `PCycles` are required to execute an instruction (UM
    /// §4.1), and `DDIV` stalls the whole pipeline for 69 (UM Table 3-12), so a
    /// tick is emphatically not an instruction.
    ///
    /// Hot path: keep allocation-free (no `Vec`/`Box` in `tick`). The `bus`
    /// argument is the `&mut Bus` the scheduler hands down each step.
    /// **Prefer [`Cpu::tick_at`] when a scheduler is present.** This path holds
    /// the COP0 `Count` timeline still, because `Count` is derived from the
    /// master clock (ADR 0006) and this function has no access to it — so
    /// `Count`/`Compare` and the timer interrupt do not advance. That is
    /// deliberate: guessing a rate here would be wrong, and `Count` runs at half
    /// `PClock`, not one step per call.
    pub fn tick<B: Bus>(&mut self, bus: &mut B) {
        self.pipeline.advance(bus, &mut self.regs, &mut self.pc);
        self.retired = self.pipeline.retired;
    }

    /// Step one `PCycle` with the scheduler's `Count` timeline supplied.
    ///
    /// The scheduler owns `master_ticks` and derives `count_ticks` from it
    /// (ADR 0006); the CPU turns that into the architectural, guest-writable
    /// `Count`. Passing it in rather than incrementing locally is what keeps
    /// `master_ticks` the only incremented counter in the core.
    pub fn tick_at<B: Bus>(&mut self, bus: &mut B, count_now: u64) {
        self.pipeline
            .advance_at(bus, &mut self.regs, &mut self.pc, count_now);
        self.retired = self.pipeline.retired;
    }
}

/// Returns the crate version string.
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NullBus;
    impl Bus for NullBus {
        fn read_u8(&mut self, _addr: u32) -> u8 {
            0
        }
        fn write_u8(&mut self, _addr: u32, _val: u8) {}
    }

    #[test]
    fn constructs_with_zero_register() {
        let cpu = Cpu::new();
        assert_eq!(cpu.regs.read(0), 0);
        assert_eq!(cpu.pc, 0xFFFF_FFFF_BFC0_0000);
    }

    /// A tick is one `PClock`, not one instruction. The pipeline is 5 stages
    /// deep, so nothing retires until it has filled (UM §4.1: "at least 5
    /// `PCycle`s are required to execute an instruction").
    #[test]
    fn a_tick_is_a_pclock_not_an_instruction() {
        let mut cpu = Cpu::new();
        let mut bus = NullBus;
        for cycle in 1..=4 {
            cpu.tick(&mut bus);
            assert_eq!(cpu.retired, 0, "retired on cycle {cycle}, before WB ran");
        }
        cpu.tick(&mut bus);
        assert_eq!(cpu.retired, 1, "the first instruction retires on cycle 5");
    }

    #[test]
    fn version_is_non_empty() {
        assert!(!version().is_empty());
    }
}
