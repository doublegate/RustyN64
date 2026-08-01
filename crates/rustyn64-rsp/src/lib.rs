//! `rustyn64-rsp` — RSP (Reality Signal Processor), the RCP's vector coprocessor.
//!
//! The RSP is a MIPS-derived scalar unit (SU) plus a 32-lane × 8 × 16-bit SIMD
//! vector unit (VU), running game-supplied microcode out of its 4 KiB IMEM with
//! 4 KiB of DMEM scratch. It drives geometry transform (display lists → RDP
//! commands) and audio mixing. The accuracy bar is **LLE** (low-level emulation
//! — interpret the microcode instruction-by-instruction, the cen64 / ares model)
//! rather than HLE microcode recognition.
//!
//! Both the **scalar unit** ([`su`]) and the full **8-lane vector unit** ([`vu`])
//! run: the 48-bit accumulator, the `VRCP`/`VRSQ` reciprocal ROM tables, the
//! clamping rules, and the whole vector load/store family are implemented, and
//! the SP interface registers are modeled ([`sp`]). The RSP category of
//! n64-systemtest passes `Failed: 0` (Phase 2).
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
// Several step methods take `&mut self` and are not all `const`; accept the
// pedantic suggestions at module level rather than annotate each one.
#![allow(
    clippy::missing_const_for_fn,
    clippy::unused_self,
    clippy::needless_pass_by_ref_mut
)]

extern crate alloc;

use serde::{Deserialize, Serialize};

pub mod sp;
pub mod su;
pub mod vu;

/// Size of RSP DMEM / IMEM (each 4 KiB).
pub const SP_MEM_SIZE: usize = 4 * 1024;

/// A zeroed COP2 `funct` histogram, for `#[serde(skip)]`.
///
/// **`default` is not optional here.** `[u64; 64]` does not implement `Default`
/// — the standard impls stop at 32 — so `#[serde(skip)]` alone does not compile.
/// That is the compiler catching, at the type level, the same class of mistake
/// #245 shipped at runtime: a skipped field that deserializes into something
/// unusable.
#[cfg(feature = "work-counters")]
fn zeroed_funct_histogram() -> alloc::boxed::Box<[u64; 64]> {
    alloc::boxed::Box::new([0; 64])
}

/// RSP architectural state.
///
/// Holds the SU register file, the VU vector register file + accumulator, the
/// program counter into IMEM, the halted flag, and the DMEM/IMEM scratch. The
/// execution engine (SU + VU) runs the microcode instruction stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rsp {
    /// Scalar unit: 32 × 32-bit general registers.
    pub su_regs: [u32; 32],
    /// Vector unit: 32 registers × 8 lanes of 16-bit.
    pub vu_regs: [[u16; 8]; 32],
    /// 48-bit-per-lane VU accumulator (modeled as `[u64; 8]`, low 48 used).
    pub vu_acc: [u64; 8],
    /// **Vestigial.** The authoritative PC lives in [`Self::sp`]
    /// (`SpRegs::pc()`), which is what `su_step` fetches from; this
    /// field is never written. Kept only so the save-state layout is unchanged —
    /// removing it is a format break (ADR 0005), which module 70 reserves for an
    /// announced major release — and **deprecated** so any read is a loud warning
    /// (the workspace treats warnings as errors). Use [`Rsp::pc`] instead.
    ///
    /// This is not hypothetical tidiness: while it was `pub` it was sampled twice
    /// in one debugging session and produced two confident, wrong conclusions —
    /// "the RSP never starts" and "its PC never advances" — when in fact retail
    /// microcode was executing hundreds of distinct instructions. That is the
    /// inert-API hazard in `docs/engineering-lessons.md` §3.2 exactly.
    #[deprecated(
        since = "0.8.0",
        note = "never written; read `Rsp::pc()` instead, which returns SP_STATUS's PC"
    )]
    pub pc: u16,
    /// **Vestigial** — see [`Self::pc`]. The authoritative halt state is
    /// `sp.halted()` (`SP_STATUS.halt`), which is what gates execution. Use
    /// [`Rsp::halted`] instead.
    #[deprecated(
        since = "0.8.0",
        note = "never written; read `Rsp::halted()` instead, which returns SP_STATUS.halt"
    )]
    pub halted: bool,
    /// 4 KiB data memory.
    #[serde(with = "rustyn64_snapshot::boxed_bytes")]
    pub dmem: alloc::boxed::Box<[u8; SP_MEM_SIZE]>,
    /// 4 KiB instruction memory.
    #[serde(with = "rustyn64_snapshot::boxed_bytes")]
    pub imem: alloc::boxed::Box<[u8; SP_MEM_SIZE]>,
    /// The SP interface registers, shared by the CPU's memory-mapped window and
    /// the RSP's own COP0 -- one set of physical registers, so one field.
    pub sp: sp::SpRegs,
    /// Shadow of the eight DP command registers (COP0 `c8`–`c15` =
    /// `DP_START`/`END`/`CURRENT`/`STATUS`/…), so an `MFC0` reads back what a
    /// prior `MTC0` in the same run wrote. The authoritative copy lives in
    /// `rustyn64-rdp`; the Bus forwards each write there (`StepResult::dp_write`)
    /// and this shadow is the RSP-local view — which is why reads of DP state the
    /// RDP mutated on its own are not yet reflected here (Phase 3).
    pub dp: [u32; 8],
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
    /// Instructions this RSP has **executed** (`work-counters`).
    ///
    /// Not a cycle counter and nothing schedules against it — the project rule
    /// is that `master_ticks` is the only counter that is ever incremented, and
    /// its stated exception is exactly this: a retired-work tally. Reading it
    /// cannot affect timing, and nothing in the emulator does read it.
    ///
    /// It counts **executed**, not stepped, and the increment sits *after*
    /// `su_step`'s halt check for that reason. A halted RSP is stepped every RCP
    /// cycle and does nothing; counting those would make the tally track the
    /// scheduler instead of the microcode — which is the opposite of what it is
    /// for, and would look perfectly plausible. `a_halted_rsp_retires_nothing`
    /// pins it.
    ///
    /// `#[serde(skip)]`, so ADR 0005's save-state layout is untouched.
    #[cfg(feature = "work-counters")]
    #[serde(skip)]
    retired: u64,
    /// How many times each COP2 computational `funct` has executed
    /// (`work-counters`).
    ///
    /// `vu.rs` is 143 functions and ~8.5% of a frame. Vectorizing it means
    /// writing `unsafe` intrinsics, so it matters enormously WHICH operations a
    /// real workload actually runs — the alternative is 143 hand-vectorized
    /// functions to recover the cost of the five that matter.
    ///
    /// 64 slots because `funct` is six bits. `#[serde(skip)]`, and a
    /// retired-work tally like [`Self::retired`].
    #[cfg(feature = "work-counters")]
    #[serde(skip, default = "zeroed_funct_histogram")]
    vu_funct: alloc::boxed::Box<[u64; 64]>,
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
    #[expect(
        deprecated,
        reason = "the vestigial `pc`/`halted` fields must still be initialized so                   the save-state layout is unchanged; they are never read"
    )]
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
            dp: [0; 8],
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
            #[cfg(feature = "work-counters")]
            retired: 0,
            #[cfg(feature = "work-counters")]
            vu_funct: alloc::boxed::Box::new([0; 64]),
        }
    }

    /// Byte offset within the CPU-visible SP memory window, folded into the one
    /// 8 KiB image that DMEM and IMEM form.
    ///
    /// The window at `0x0400_0000` is 8 KiB of real storage repeated all the way
    /// to `0x0404_0000` — n64-systemtest writes `0x3E000` and reads the result
    /// back at offset 0 (`sp_memory::SW (out of bounds)`), which is the same
    /// 8 KiB seen for the 31st time. Masking is therefore the behavior, not a
    /// bounds-check standing in for one: there is no out-of-range access to
    /// reject inside the window. Provenance is recorded in accuracy ledger
    /// **C-30** — the wiki documents only the first 8 KiB, so the mirroring
    /// rests on the oracle.
    const fn fold(off: u32) -> usize {
        (off & 0x1FFF) as usize
    }

    /// The RSP's program counter into IMEM — the value execution actually uses.
    ///
    /// Delegates to `SP_STATUS`'s register file rather than the struct field of
    /// the same name, which is vestigial.
    ///
    /// Returns `u32`, not the `u16` the vestigial field used, because that is
    /// `SpRegs::pc()`'s type and widening here would be a lossless re-narrowing
    /// for every caller. The value is a 12-bit IMEM offset either way — `su_step`
    /// masks it with `0xFFC` — so the wider type costs nothing and avoids a cast
    /// at every call site.
    #[must_use]
    pub const fn pc(&self) -> u32 {
        self.sp.pc()
    }

    /// Is the RSP halted? This is `SP_STATUS.halt`, the flag that actually gates
    /// [`Rsp::tick`] — not the vestigial `halted` field.
    #[must_use]
    pub const fn halted(&self) -> bool {
        self.sp.halted()
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

    /// Instructions this RSP has executed since power-on (`work-counters`).
    ///
    /// See the field for why this is a legitimate counter under the
    /// derive-don't-increment rule.
    #[cfg(feature = "work-counters")]
    #[must_use]
    pub const fn retired(&self) -> u64 {
        self.retired
    }

    /// Per-`funct` COP2 computational execution counts (`work-counters`).
    #[cfg(feature = "work-counters")]
    #[must_use]
    pub const fn vu_funct_histogram(&self) -> &[u64; 64] {
        &self.vu_funct
    }

    /// Count one COP2 computational dispatch of `funct`.
    #[cfg_attr(
        not(feature = "work-counters"),
        allow(
            clippy::unused_self,
            reason = "the body is empty without the feature; the call site stays uniform"
        )
    )]
    #[inline(always)]
    #[allow(
        clippy::inline_always,
        reason = "called once per VU instruction; a call would cost more than the count"
    )]
    pub(crate) fn count_vu_funct(&mut self, funct: u32) {
        #[cfg(feature = "work-counters")]
        {
            self.vu_funct[(funct & 0x3F) as usize] += 1;
        }
        #[cfg(not(feature = "work-counters"))]
        {
            let _ = funct;
        }
    }

    /// Count one **executed** instruction.
    ///
    /// A method rather than a `cfg` block at the call site so that site stays
    /// one line: `su_step` was already within two lines of the `too_many_lines`
    /// gate, and an inline block tripped it. Compiles to nothing without the
    /// feature.
    #[cfg_attr(
        not(feature = "work-counters"),
        allow(
            clippy::unused_self,
            clippy::missing_const_for_fn,
            reason = "the body is empty without the feature; the call site stays uniform"
        )
    )]
    #[inline(always)]
    #[allow(
        clippy::inline_always,
        reason = "called once per executed RSP instruction; a call would cost more than the count"
    )]
    pub(crate) const fn count_retired(&mut self) {
        #[cfg(feature = "work-counters")]
        {
            self.retired = self.retired.wrapping_add(1);
        }
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
        // `su_step` fetches and decodes one instruction and dispatches it — the
        // scalar ops directly, and the COP2 / vector load-store family through to
        // the VU (`cop2`, `vector_memory`). So a single `tick` runs the full
        // scalar+vector engine, which is why the RSP category is `Failed: 0`.
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

    /// **The RSP powers up halted** — asserted through [`Rsp::halted`], the
    /// accessor that reads `SP_STATUS`.
    ///
    /// It previously asserted on the `halted` *field*, which is never written and
    /// is therefore unconditionally `true`. The test passed for a reason that had
    /// nothing to do with the RSP's actual state, and would have kept passing if
    /// `SP_STATUS` powered up running.
    /// A halted RSP is stepped every RCP cycle and must retire nothing.
    ///
    /// This is the whole reason the increment sits *after* the halt check.
    /// Counting ticks instead of executed instructions would make the tally
    /// track the scheduler rather than the microcode — and it would look
    /// perfectly plausible, because a halted RSP is stepped constantly.
    #[cfg(feature = "work-counters")]
    #[test]
    fn a_halted_rsp_retires_nothing() {
        let mut rsp = Rsp::new();
        assert!(rsp.halted(), "the premise: it powers up halted");
        for _ in 0..64 {
            rsp.tick();
        }
        assert_eq!(
            rsp.retired(),
            0,
            "a halted RSP retired instructions; the counter is counting ticks"
        );
    }

    /// A running RSP retires exactly one instruction per tick.
    ///
    /// The non-vacuous half of the pair above: without it, a counter that never
    /// incremented at all would satisfy `a_halted_rsp_retires_nothing`
    /// perfectly.
    #[cfg(feature = "work-counters")]
    #[test]
    fn a_running_rsp_retires_one_per_tick() {
        let mut rsp = Rsp::new();
        // NOPs, so nothing branches, halts or faults — the count is then
        // unambiguously one per tick rather than one per something else.
        for i in 0..16u32 {
            let a = (i * 4) as usize;
            rsp.imem[a..a + 4].copy_from_slice(&0u32.to_be_bytes());
        }
        rsp.sp.set_halted(false);
        assert!(!rsp.halted(), "the premise: it is running");

        for n in 1..=8u64 {
            rsp.tick();
            assert_eq!(
                rsp.retired(),
                n,
                "after {n} ticks of a running RSP the tally should be {n}"
            );
        }
    }

    /// `retired` is `#[serde(skip)]`, so a restored RSP starts at zero and keeps
    /// counting — the mirror of `rustyn64-core`'s
    /// `a_deserialized_bus_starts_at_zero_and_still_counts`.
    ///
    /// This PR added two counters and originally tested the save-state
    /// semantics of only one. An asymmetry like that is how the untested half
    /// later turns out to behave differently: #245 shipped a `#[serde(skip)]`
    /// field that deserialized into something unusable and panicked on the next
    /// write, and the test that should have caught it passed vacuously.
    #[cfg(feature = "work-counters")]
    #[test]
    fn a_deserialized_rsp_starts_at_zero_and_still_counts() {
        let mut rsp = Rsp::new();
        rsp.imem[0..4].copy_from_slice(&0u32.to_be_bytes());
        rsp.sp.set_halted(false);
        rsp.tick();
        assert!(rsp.retired() > 0, "the premise: it counted something");

        let bytes = bincode::serialize(&rsp).expect("serialize");
        let mut restored: Rsp = bincode::deserialize(&bytes).expect("deserialize");
        assert_eq!(
            restored.retired(),
            0,
            "the tally survived a save-state; it is a measurement, not machine state"
        );
        // And it is still live afterwards, which zero alone cannot show.
        restored.sp.set_halted(false);
        restored.tick();
        assert_eq!(restored.retired(), 1, "a restored RSP stopped counting");
    }

    #[test]
    fn constructs_halted() {
        let mut rsp = Rsp::new();
        assert!(rsp.halted(), "SP_STATUS.halt must be set at power-on");
        assert_eq!(rsp.pc(), 0, "and the PC starts at 0");

        // Power-on values alone cannot distinguish the accessors from the
        // vestigial fields: BOTH representations start `halted = true, pc = 0`,
        // so the assertions above would still pass if either accessor regressed
        // to reading the dead field. Mutate the SP registers and require the
        // accessors to follow — which the fields, never being written, cannot do.
        rsp.sp.write(sp::reg::STATUS, 1); // CLR_HALT
        rsp.sp.set_pc(0x123);
        assert!(
            !rsp.halted(),
            "halted() must track SP_STATUS, not the vestigial field"
        );
        assert_eq!(
            rsp.pc(),
            0x120,
            "pc() must track SP_PC (word-aligned), not the vestigial field"
        );
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
