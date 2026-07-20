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

/// Which half of a `SysAD` bus transaction the lockstep scheduler is currently in.
///
/// The VR4300 talks to the RCP over the `SysAD` bus, which multiplexes the command
/// and the data onto the same lines. `SYSCMD` bit 4 is literally documented as
/// "Command or Data", and this enum mirrors that split: the transaction issues a
/// command, then transfers data.
///
/// The scheduler needs the distinction because an RCP or interface event landing
/// between the two halves must be visible to the correct one — that is the
/// not-catch-up property in ADR 0001. The [`Bus::poll_irq_at_phase`] hook is
/// parameterized over it so `rustyn64-core` and the test harness can import it
/// through `rustyn64_core::scheduler`.
///
/// Note this is *not* a sub-cycle bus-timing model: the scheduler still advances
/// at whole-VR4300-cycle resolution. Finer resolution is the deferred ADR 0005
/// refactor.
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

    /// Sample the pending-interrupt level at a given `SysAD` transaction half.
    ///
    /// Default: no interrupt pending. `rustyn64-core` overrides to OR the MI
    /// interrupt mask against the live RCP interrupt lines.
    fn poll_irq_at_phase(&mut self, _phase: BusPhase) -> bool {
        false
    }
}

/// NEC VR4300 architectural state.
///
/// This is the **skeleton** register file only; the decode/execute pipeline,
/// the CP0 TLB, and the CP1 FPU are roadmap phases left as marked TODOs.
#[derive(Debug, Clone)]
pub struct Cpu {
    /// 32 general-purpose 64-bit registers (`gpr[0]` is hard-wired zero).
    pub gpr: [u64; 32],
    /// The HI multiply/divide result register.
    pub hi: u64,
    /// The LO multiply/divide result register.
    pub lo: u64,
    /// Program counter (virtual address).
    pub pc: u64,
    /// Retired-instruction / cycle counter (used by the golden-log differ).
    pub cycles: u64,
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
            gpr: [0; 32],
            hi: 0,
            lo: 0,
            pc: 0xBFC0_0000,
            cycles: 0,
        }
    }

    /// Set the program counter (used by golden-log automation harnesses).
    pub const fn set_pc(&mut self, pc: u64) {
        self.pc = pc;
    }

    /// Advance the CPU by one issued instruction.
    ///
    /// Hot path: keep allocation-free (no `Vec`/`Box` in `tick`). The `bus`
    /// argument is the `&mut Bus` the scheduler hands down each step.
    pub fn tick<B: Bus>(&mut self, bus: &mut B) {
        // TODO(T-CPU-01): fetch at `pc` via `bus.read_u32`, decode the MIPS III
        // opcode, execute (incl. the branch-delay slot), advance CP0 Count, and
        // raise exceptions. Skeleton: just retire a cycle and keep $zero pinned.
        let _ = bus;
        self.gpr[0] = 0;
        self.cycles = self.cycles.wrapping_add(1);
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
        assert_eq!(cpu.gpr[0], 0);
        assert_eq!(cpu.pc, 0xBFC0_0000);
    }

    #[test]
    fn tick_retires_a_cycle() {
        let mut cpu = Cpu::new();
        let mut bus = NullBus;
        cpu.tick(&mut bus);
        assert_eq!(cpu.cycles, 1);
    }

    #[test]
    fn version_is_non_empty() {
        assert!(!version().is_empty());
    }
}
