//! `rustyn64-rsp` — RSP (Reality Signal Processor), the RCP's vector coprocessor.
//!
//! The RSP is a MIPS-derived scalar unit (SU) plus a 32-lane × 8 × 16-bit SIMD
//! vector unit (VU), running game-supplied microcode out of its 4 KiB IMEM with
//! 4 KiB of DMEM scratch. It drives geometry transform (display lists → RDP
//! commands) and audio mixing. The accuracy bar is **LLE** (low-level emulation
//! — interpret the microcode instruction-by-instruction, the cen64 / ares model)
//! rather than HLE microcode recognition.
//!
//! The **scalar unit runs** ([`su`]) and the SP interface registers are
//! modelled ([`sp`]). The **vector unit does not yet exist** — the 48-bit
//! accumulator, the `VRCP`/`VRSQ` tables and the clamping rules are Sprint 2,
//! so a COP2 instruction currently retires inertly.
//!
//! The RSP never borrows the rest of the machine. [`Rsp::tick`] *returns* what
//! it wants done — a DMA to perform, an interrupt to raise — and
//! `rustyn64-core::Bus` carries it out, because the RSP owns neither RDRAM nor
//! the MI. That keeps this crate independent of the other chip crates and lets
//! the RSP be stepped in isolation.
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

pub mod sp;
pub mod su;
pub mod vu;

/// Size of RSP DMEM / IMEM (each 4 KiB).
pub const SP_MEM_SIZE: usize = 4 * 1024;

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
    /// The SP interface registers, shared by the CPU's memory-mapped window and
    /// the RSP's own COP0 -- one set of physical registers, so one field.
    pub sp: sp::SpRegs,
    /// The VU's three control registers (`VCO`, `VCC`, `VCE`).
    pub vu_ctrl: vu::Control,
    /// The reciprocal unit's staging latches (`DIVIN`/`DIVOUT`/`DIVDP`).
    pub div: vu::Divide,
    /// The destination lane a single-lane VU instruction computed, applied
    /// after the accumulator write so the two cannot alias.
    pending_vd_lane: Option<(usize, u16)>,
    /// A branch target latched by the previous instruction, taken after the
    /// delay slot retires. `None` means the next PC is sequential.
    branch: Option<u32>,
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
            sp: sp::SpRegs::new(),
            vu_ctrl: vu::Control {
                vco: 0,
                vcc: 0,
                vce: 0,
            },
            div: vu::Divide {
                input: 0,
                output: 0,
                pending: false,
            },
            pending_vd_lane: None,
            branch: None,
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
    /// reject inside the window. Provenance is recorded in accuracy ledger
    /// **C-30** — the wiki documents only the first 8 KiB, so the mirroring
    /// rests on the oracle.
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

    /// Advance the RSP by one instruction when running.
    ///
    /// Returns what the step asked of the rest of the machine — see
    /// [`su::StepResult`]. It **reports** rather than acting because the RSP
    /// owns neither RDRAM nor the MI, and a chip reaching back into its owner is
    /// the dependency cycle `docs/architecture.md` exists to prevent.
    ///
    /// This is also why it no longer borrows a bus: the caller needed to move
    /// the whole chip out of the Bus to satisfy the borrow checker, and moving
    /// it out meant `Default`-constructing a replacement — **two 4 KiB
    /// allocations on every RCP step**, behind a comment that claimed there were
    /// none.
    pub fn tick(&mut self) -> su::StepResult {
        // The vector unit is Sprint 2; the scalar unit runs today, which is
        // enough to execute a microcode that sets up, DMAs and breaks.
        self.su_step()
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

    #[test]
    fn constructs_halted() {
        let rsp = Rsp::new();
        assert!(rsp.halted);
    }

    /// A halted RSP fetches nothing and asks nothing of the machine.
    #[test]
    fn halted_tick_is_noop() {
        let mut rsp = Rsp::new();
        let out = rsp.tick();
        assert_eq!(out, su::StepResult::default());
        assert_eq!(rsp.sp.pc(), 0);
    }

    #[test]
    fn version_is_non_empty() {
        assert!(!version().is_empty());
    }
}
