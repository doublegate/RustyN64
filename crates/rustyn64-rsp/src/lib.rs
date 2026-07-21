//! `rustyn64-rsp` — RSP (Reality Signal Processor), the RCP's vector coprocessor.
//!
//! The RSP is a MIPS-derived scalar unit (SU) plus a 32-lane × 8 × 16-bit SIMD
//! vector unit (VU), running game-supplied microcode out of its 4 KiB IMEM with
//! 4 KiB of DMEM scratch. It drives geometry transform (display lists → RDP
//! commands) and audio mixing. The accuracy bar is **LLE** (low-level emulation
//! — interpret the microcode instruction-by-instruction, the cen64 / ares model)
//! rather than HLE microcode recognition.
//!
//! This is a **skeleton**: the SU instruction interpreter and the VU (the
//! 48-element accumulator, the reciprocal/`VRCP`/`VRSQ` tables, clamping) are
//! the major roadmap phases and are left behind no-op step methods with TODO
//! markers. The RSP talks to the rest of the machine through the [`RspBus`]
//! trait, so it stays independent of the other chip crates.
//!
//! Part of the one-directional chip-crate graph (see `docs/architecture.md`):
//! this crate does NOT depend on any other chip crate. `#![no_std]` + `alloc`;
//! only the frontend carries `std` + `unsafe`.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::cast_possible_truncation, clippy::cast_lossless)]
// Skeleton step methods are deliberately non-`const` and will use `&mut self`
// once the SU/VU execution lands; accept the pedantic suggestions at module
// level rather than salt every stub.
#![allow(
    clippy::missing_const_for_fn,
    clippy::unused_self,
    clippy::needless_pass_by_ref_mut
)]

extern crate alloc;

/// Size of RSP DMEM / IMEM (each 4 KiB).
pub const SP_MEM_SIZE: usize = 4 * 1024;

/// The bus the RSP sees.
///
/// DMA between DMEM/IMEM and RDRAM, plus the SP/DP status-register handshake.
/// `rustyn64-core` implements this; keeping it a trait lets the RSP be stepped
/// in isolation by the golden-log differ.
pub trait RspBus {
    /// Read a byte from RDRAM (the source/target of an SP DMA).
    fn rdram_read(&self, addr: u32) -> u8;
    /// Write a byte to RDRAM.
    fn rdram_write(&mut self, addr: u32, val: u8);
    /// Raise the SP interrupt (microcode signalled completion via `$MI`).
    fn raise_sp_interrupt(&mut self) {}
}

/// RSP architectural state (skeleton).
///
/// Holds the SU register file, the VU vector register file + accumulator, the
/// program counter into IMEM, the halted flag, and the DMEM/IMEM scratch. The
/// execution engine itself is a roadmap TODO.
#[derive(Debug, Clone)]
pub struct Rsp {
    /// Scalar unit: 32 × 32-bit general registers.
    pub su_regs: [u32; 32],
    /// Vector unit: 32 registers × 8 lanes of 16-bit.
    pub vu_regs: [[u16; 8]; 32],
    /// 48-bit-per-lane VU accumulator (modeled as `[u64; 8]`, low 48 used).
    pub vu_acc: [u64; 8],
    /// Program counter into IMEM (12-bit).
    pub pc: u16,
    /// Halted (`SP_STATUS.halt`) — the RSP idles until the CPU clears it.
    pub halted: bool,
    /// 4 KiB data memory.
    pub dmem: alloc::boxed::Box<[u8; SP_MEM_SIZE]>,
    /// 4 KiB instruction memory.
    pub imem: alloc::boxed::Box<[u8; SP_MEM_SIZE]>,
    // TODO(T-RSP-01): VCO/VCC/VCE flag registers, the divide-in/out latches for
    // VRCP/VRSQ, the DMA length/skip latches — see `docs/rsp.md`.
}

impl Default for Rsp {
    fn default() -> Self {
        Self::new()
    }
}

impl Rsp {
    /// Construct at power-on (halted, zeroed scratch).
    #[must_use]
    pub fn new() -> Self {
        Self {
            su_regs: [0; 32],
            vu_regs: [[0; 8]; 32],
            vu_acc: [0; 8],
            pc: 0,
            halted: true,
            dmem: alloc::boxed::Box::new([0; SP_MEM_SIZE]),
            imem: alloc::boxed::Box::new([0; SP_MEM_SIZE]),
        }
    }

    /// Byte offset within the CPU-visible SP memory window, folded into the one
    /// 8 KiB image that DMEM and IMEM form.
    ///
    /// The window at `0x0400_0000` is 8 KiB of real storage repeated all the way
    /// to `0x0404_0000` — n64-systemtest writes `0x3E000` and reads the result
    /// back at offset 0 (`sp_memory::SW (out of bounds)`), which is the same
    /// 8 KiB seen for the 31st time. Masking is therefore the behaviour, not a
    /// bounds-check standing in for one: there is no out-of-range access to
    /// reject inside the window.
    const fn fold(off: u32) -> usize {
        (off & 0x1FFF) as usize
    }

    /// Read a byte of DMEM/IMEM as the CPU sees it.
    ///
    /// Bit 12 of the folded offset selects IMEM over DMEM, and each bank wraps
    /// within its own 4 KiB — a transfer or access never spills from one into
    /// the other (N64brew *RSP Interface* §DMEM and IMEM).
    #[must_use]
    pub fn mem_read(&self, off: u32) -> u8 {
        let off = Self::fold(off);
        let bank = if off & 0x1000 == 0 {
            &self.dmem
        } else {
            &self.imem
        };
        bank[off & 0xFFF]
    }

    /// Write a byte of DMEM/IMEM as the CPU sees it.
    pub const fn mem_write(&mut self, off: u32, val: u8) {
        let off = Self::fold(off);
        let bank = if off & 0x1000 == 0 {
            &mut self.dmem
        } else {
            &mut self.imem
        };
        bank[off & 0xFFF] = val;
    }

    /// Advance the RSP by one microcode instruction when running.
    ///
    /// Hot path: keep allocation-free. No-op while halted.
    pub fn tick<B: RspBus>(&mut self, bus: &mut B) {
        if self.halted {
            return;
        }
        // TODO(v0.x): LLE RSP scalar+vector execution — fetch the IMEM word at
        // `pc`, decode the SU op (LWC2/SWC2 vector loads, branches, the COP2
        // escape) and the VU op, run `step_su`/`step_vu`, advance `pc`.
        let _ = bus;
        self.step_su();
        self.step_vu();
    }

    /// LLE scalar-unit step (skeleton).
    fn step_su(&mut self) {
        // TODO(v0.x): LLE RSP scalar execution — MIPS-subset interpreter over
        // DMEM/IMEM with the vector load/store ops (LQV/SQV/LPV/...).
        self.su_regs[0] = 0; // $zero stays pinned.
    }

    /// LLE vector-unit step (skeleton).
    fn step_vu(&mut self) {
        // TODO(v0.x): LLE RSP vector execution — the 8-lane SIMD ALU, the
        // 48-bit accumulator, signed/unsigned clamping, and the
        // VRCP/VRSQ/VRCPH reciprocal table lookups.
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
    impl RspBus for NullBus {
        fn rdram_read(&self, _addr: u32) -> u8 {
            0
        }
        fn rdram_write(&mut self, _addr: u32, _val: u8) {}
    }

    #[test]
    fn constructs_halted() {
        let rsp = Rsp::new();
        assert!(rsp.halted);
    }

    #[test]
    fn halted_tick_is_noop() {
        let mut rsp = Rsp::new();
        let mut bus = NullBus;
        rsp.tick(&mut bus);
        assert_eq!(rsp.pc, 0);
    }

    #[test]
    fn version_is_non_empty() {
        assert!(!version().is_empty());
    }
}
