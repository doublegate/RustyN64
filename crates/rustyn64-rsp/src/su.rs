//! The **scalar unit** — the RSP's MIPS-like integer core (T-21-004).
//!
//! Close enough to a 32-bit R4000 that standard MIPS documentation covers most
//! of it, so what this module records is the *differences* (N64brew *RSP CPU
//! Core* §Scalar unit):
//!
//! - **No multiply/divide unit.** `MULT`, `MULTU`, `DIV`, `DIVU`, `MFHI`,
//!   `MFLO`, `MTHI`, `MTLO` do not exist, and neither does `HI`/`LO`.
//! - **No 64-bit anything.** The registers are 32 bits, so every `D*` opcode is
//!   absent, as are `LD`/`SD`/`LDL`/`SDL`.
//! - **No misaligned-access opcodes**, because none are needed: `LW` and `SW`
//!   work at *any* address. `LWL`/`LWR`/`SWL`/`SWR` are absent.
//! - **No traps or exceptions at all** — no interrupts, no `SYSCALL`, no `TGE`
//!   family. `BREAK` exists and halts the core instead of raising anything.
//! - **No likely branches** (`BEQL`, `BLEZL`, …).
//!
//! # The two rules that catch a MIPS core reused wholesale
//!
//! **The PC is 12 bits and wraps.** Every high bit of a branch or jump target
//! is discarded, and running off the end of IMEM at `0xFFC` continues at
//! `0x000` rather than faulting. n64-systemtest's `RSP Wrap around` places two
//! `nop`s at `0xFF8` and a `BREAK` at `0x000`, runs from `0xFF8`, and expects
//! to stop at `0x4`.
//!
//! **Misaligned data accesses are correct, not faults.** A `LW` at `0x001`
//! returns the four bytes at `0x1..=0x4`; the address is masked to 12 bits and
//! each byte wraps inside DMEM independently, so a word read at `0xFFE` takes
//! two bytes from the end and two from the start. On the VR4300 the same access
//! is an `AddressError`, which makes this the single easiest place to get the
//! RSP wrong by reusing CPU code.

use crate::Rsp;
use crate::sp::{self, STATUS_INTBREAK};

/// What one scalar step asked the rest of the machine to do.
///
/// The RSP cannot reach RDRAM or the MI itself — it does not own them — so it
/// reports rather than acts, and `rustyn64-core::Bus` carries it out. This is
/// the same shape the PI engine uses, and it is what lets the RSP be stepped in
/// isolation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StepResult {
    /// A transfer an `MTC0` to a length register started.
    pub dma: Option<sp::Dma>,
    /// A change to the MI's SP interrupt line: `Some(true)` raises it,
    /// `Some(false)` acknowledges it, `None` leaves it alone.
    ///
    /// Three states rather than two, because the RSP acknowledges its **own**
    /// interrupt by writing `CLR_INTR` through `MTC0` — with a plain `bool`,
    /// "clear the line" and "this step said nothing about the line" are the
    /// same value, and the acknowledgement is silently dropped.
    pub interrupt_change: Option<bool>,
}

/// Decoded fields, named as the MIPS encoding names them.
struct Fields {
    op: u32,
    rs: usize,
    rt: usize,
    rd: usize,
    sa: u32,
    funct: u32,
    imm: u32,
    target: u32,
}

const fn decode(word: u32) -> Fields {
    Fields {
        op: word >> 26,
        rs: ((word >> 21) & 31) as usize,
        rt: ((word >> 16) & 31) as usize,
        rd: ((word >> 11) & 31) as usize,
        sa: (word >> 6) & 31,
        funct: word & 63,
        imm: word & 0xFFFF,
        target: word & 0x03FF_FFFF,
    }
}

/// Sign-extend a 16-bit immediate to 32 bits.
const fn sext(imm: u32) -> u32 {
    ((imm as u16).cast_signed() as i32).cast_unsigned()
}

impl Rsp {
    /// Read one register, with `r0` pinned to zero.
    const fn r(&self, i: usize) -> u32 {
        if i == 0 { 0 } else { self.su_regs[i] }
    }

    /// Write one scalar register; writes to `r0` are discarded.
    ///
    /// Public under this name so the VU's move instructions can reach it —
    /// `MFC2` and `CFC2` write a GPR, and they live in [`crate::vu`].
    pub const fn set_su(&mut self, i: usize, v: u32) {
        self.set_r(i, v);
    }

    /// Write one register; writes to `r0` are discarded.
    const fn set_r(&mut self, i: usize, v: u32) {
        if i != 0 {
            self.su_regs[i] = v;
        }
    }

    /// Read a byte of DMEM. The address is 12 bits — everything above is
    /// ignored, so a load never escapes the 4 KiB and never faults.
    const fn dmem_read(&self, addr: u32) -> u8 {
        self.dmem[(addr & 0xFFF) as usize]
    }

    /// Write a byte of DMEM, under the same 12-bit rule.
    const fn dmem_write(&mut self, addr: u32, val: u8) {
        self.dmem[(addr & 0xFFF) as usize] = val;
    }

    /// Read `n` bytes big-endian from DMEM, each byte wrapping independently.
    ///
    /// Byte-at-a-time rather than a word fetch because the access may be
    /// misaligned *and* may straddle the end of DMEM — `LWU` from `0xFFD` takes
    /// three bytes from the end and one from the start, which n64-systemtest
    /// checks directly.
    fn dmem_load(&self, addr: u32, n: u32) -> u32 {
        let mut v = 0u32;
        for i in 0..n {
            v = (v << 8) | u32::from(self.dmem_read(addr.wrapping_add(i)));
        }
        v
    }

    /// Store the low `n` bytes of `val` big-endian into DMEM.
    fn dmem_store(&mut self, addr: u32, n: u32, val: u32) {
        for i in 0..n {
            let shift = 8 * (n - 1 - i);
            self.dmem_write(addr.wrapping_add(i), (val >> shift) as u8);
        }
    }

    /// Fetch the instruction word at `pc` from IMEM.
    ///
    /// IMEM is a separate 4 KiB (the RSP is a Harvard machine), and the PC is
    /// masked to 12 bits so the fetch wraps rather than running off the end.
    fn imem_word(&self, pc: u32) -> u32 {
        let base = (pc & 0xFFF) as usize;
        u32::from_be_bytes([
            self.imem[base],
            self.imem[(base + 1) & 0xFFF],
            self.imem[(base + 2) & 0xFFF],
            self.imem[(base + 3) & 0xFFF],
        ])
    }

    /// Execute one scalar instruction, if the core is running.
    ///
    /// Returns what the step asked the machine to do; see [`StepResult`].
    pub fn su_step(&mut self) -> StepResult {
        let mut out = StepResult::default();
        if self.sp.halted() {
            return out;
        }

        let pc = self.sp.pc();
        let word = self.imem_word(pc);
        // The branch target latched by the *previous* instruction. Taken now,
        // because the instruction being executed is that branch's delay slot.
        let after_delay = self.branch.take();
        let sequential = pc.wrapping_add(4) & 0xFFC;

        let d = decode(word);
        let mut halt_at = None;

        match d.op {
            // SPECIAL — the register-form ALU, shifts, jumps and BREAK.
            0 => halt_at = self.special(&d, sequential),
            // REGIMM: the four conditional branches on rs's sign.
            1 => {
                let v = self.r(d.rs).cast_signed();
                let take = match d.rt {
                    0o00 | 0o20 => v < 0,
                    0o01 | 0o21 => v >= 0,
                    _ => false,
                };
                // The AL forms link unconditionally, even when not taken.
                if d.rt & 0o20 != 0 {
                    self.set_r(31, sequential.wrapping_add(4) & 0xFFC);
                }
                if take {
                    self.branch = Some(branch_target(sequential, d.imm));
                }
            }
            0o02 => self.branch = Some((d.target << 2) & 0xFFC),
            0o03 => {
                self.set_r(31, sequential.wrapping_add(4) & 0xFFC);
                self.branch = Some((d.target << 2) & 0xFFC);
            }
            0o04 => {
                if self.r(d.rs) == self.r(d.rt) {
                    self.branch = Some(branch_target(sequential, d.imm));
                }
            }
            0o05 => {
                if self.r(d.rs) != self.r(d.rt) {
                    self.branch = Some(branch_target(sequential, d.imm));
                }
            }
            0o06 => {
                if self.r(d.rs).cast_signed() <= 0 {
                    self.branch = Some(branch_target(sequential, d.imm));
                }
            }
            0o07 => {
                if self.r(d.rs).cast_signed() > 0 {
                    self.branch = Some(branch_target(sequential, d.imm));
                }
            }
            // ADDI and ADDIU likewise coincide: no overflow trap exists.
            0o10 | 0o11 => self.set_r(d.rt, self.r(d.rs).wrapping_add(sext(d.imm))),
            0o12 => self.set_r(
                d.rt,
                u32::from(self.r(d.rs).cast_signed() < sext(d.imm).cast_signed()),
            ),
            0o13 => self.set_r(d.rt, u32::from(self.r(d.rs) < sext(d.imm))),
            0o14 => self.set_r(d.rt, self.r(d.rs) & d.imm),
            0o15 => self.set_r(d.rt, self.r(d.rs) | d.imm),
            0o16 => self.set_r(d.rt, self.r(d.rs) ^ d.imm),
            0o17 => self.set_r(d.rt, d.imm << 16),
            // COP0 — the SP and DP register files, reached by MFC0/MTC0.
            0o20 => match d.rs {
                0o00 => {
                    let v = self.cop0_read(d.rd as u32);
                    self.set_r(d.rt, v);
                }
                0o04 => out = self.cop0_write(d.rd as u32, self.r(d.rt)),
                _ => {}
            },
            // Loads. `LW` and `LWU` are the same operation on a 32-bit machine:
            // there is no upper half for the sign to extend into.
            0o40 => self.set_r(
                d.rt,
                ((self.load(&d, 1) as u8).cast_signed() as i32).cast_unsigned(),
            ),
            0o41 => self.set_r(
                d.rt,
                ((self.load(&d, 2) as u16).cast_signed() as i32).cast_unsigned(),
            ),
            0o43 | 0o47 => self.set_r(d.rt, self.load(&d, 4)),
            0o44 => self.set_r(d.rt, self.load(&d, 1)),
            0o45 => self.set_r(d.rt, self.load(&d, 2)),
            0o50 => self.store(&d, 1),
            0o51 => self.store(&d, 2),
            0o53 => self.store(&d, 4),
            // COP2. The moves are implemented; the computational instructions
            // (bit 25 set) are the rest of Sprint 2 and retire inertly, which is
            // what hardware does with anything it does not implement -- there is
            // no exception mechanism to report it with.
            0o22 => self.cop2(&d),
            // The vector load/store family is Sprint 3.
            _ => {}
        }

        // `r0` is pinned; a write may have slipped through a path above.
        self.su_regs[0] = 0;

        if let Some(next) = halt_at {
            self.sp.set_pc(next);
            self.sp.set_halted(true);
            self.sp.set_broke(true);
            if self.sp.status() & STATUS_INTBREAK != 0 {
                out.interrupt_change = Some(true);
            }
            return out;
        }

        self.sp.set_pc(after_delay.unwrap_or(sequential));
        out
    }

    /// The `SPECIAL` opcode group: register-form ALU, shifts, `JR`/`JALR` and
    /// `BREAK`. Split out so [`Rsp::su_step`] stays readable, not because the
    /// group is separable — it is one arm of the same decode.
    ///
    /// Returns the PC to halt at when the instruction was a `BREAK`.
    fn special(&mut self, d: &Fields, sequential: u32) -> Option<u32> {
        let mut halt_at = None;
        match d.funct {
            0o00 => self.set_r(d.rd, self.r(d.rt) << d.sa),
            0o02 => self.set_r(d.rd, self.r(d.rt) >> d.sa),
            0o03 => self.set_r(d.rd, (self.r(d.rt).cast_signed() >> d.sa).cast_unsigned()),
            0o04 => self.set_r(d.rd, self.r(d.rt) << (self.r(d.rs) & 31)),
            0o06 => self.set_r(d.rd, self.r(d.rt) >> (self.r(d.rs) & 31)),
            0o07 => self.set_r(
                d.rd,
                (self.r(d.rt).cast_signed() >> (self.r(d.rs) & 31)).cast_unsigned(),
            ),
            0o10 => self.branch = Some(self.r(d.rs) & 0xFFC),
            0o11 => {
                let target = self.r(d.rs) & 0xFFC;
                self.set_r(d.rd, sequential.wrapping_add(4) & 0xFFC);
                self.branch = Some(target);
            }
            // BREAK. Halts and latches BROKE; the interrupt is conditional.
            0o15 => halt_at = Some(sequential),
            // ADD and ADDU are the same instruction here: the RSP has no
            // exceptions, so there is no overflow trap to distinguish them.
            0o40 | 0o41 => self.set_r(d.rd, self.r(d.rs).wrapping_add(self.r(d.rt))),
            0o42 | 0o43 => self.set_r(d.rd, self.r(d.rs).wrapping_sub(self.r(d.rt))),
            0o44 => self.set_r(d.rd, self.r(d.rs) & self.r(d.rt)),
            0o45 => self.set_r(d.rd, self.r(d.rs) | self.r(d.rt)),
            0o46 => self.set_r(d.rd, self.r(d.rs) ^ self.r(d.rt)),
            0o47 => self.set_r(d.rd, !(self.r(d.rs) | self.r(d.rt))),
            0o52 => self.set_r(
                d.rd,
                u32::from(self.r(d.rs).cast_signed() < self.r(d.rt).cast_signed()),
            ),
            0o53 => self.set_r(d.rd, u32::from(self.r(d.rs) < self.r(d.rt))),
            // Everything else in SPECIAL is one of the absent opcodes
            // (multiply, divide, HI/LO, traps). They are not errors on this
            // core -- there is no exception mechanism to report them with --
            // so they retire doing nothing.
            _ => {}
        }
        halt_at
    }

    /// The COP2 escape: SU/VU moves today, computational instructions to come.
    ///
    /// Bit 25 separates the two groups. When it is set the instruction is a
    /// computational one, whose `element` field is a *broadcast modifier*; when
    /// clear it is a move, whose element field is a **byte offset**. Conflating
    /// them is the first thing to get wrong here.
    fn cop2(&mut self, d: &Fields) {
        // Bit 25 of the word is the top bit of the `rs` field, which is how the
        // two groups share one opcode. The four moves are `rs` 0/2/4/6, all
        // with it clear.
        if d.rs & 0x10 != 0 {
            return;
        }
        // `vs_elem` is bits 10..=7 of the word, a byte offset into the register.
        let elem = ((d.sa >> 1) & 0xF) as usize;
        match d.rs {
            0o00 => {
                self.mfc2(d.rt, d.rd, elem);
            }
            0o02 => {
                self.cfc2(d.rt, d.rd as u32);
            }
            0o04 => self.mtc2(self.r(d.rt), d.rd, elem),
            0o06 => self.ctc2(self.r(d.rt), d.rd as u32),
            _ => {}
        }
    }

    /// Address for a load or store: `base + sign-extended offset`, 12 bits.
    fn addr(&self, d: &Fields) -> u32 {
        self.r(d.rs).wrapping_add(sext(d.imm)) & 0xFFF
    }

    fn load(&self, d: &Fields, n: u32) -> u32 {
        self.dmem_load(self.addr(d), n)
    }

    fn store(&mut self, d: &Fields, n: u32) {
        let addr = self.addr(d);
        self.dmem_store(addr, n, self.r(d.rt));
    }

    /// `MFC0` — read an SP register, or a DP register.
    ///
    /// `c0`–`c7` are the SP interface registers, the *same* physical registers
    /// the CPU sees at `0x0404_0000`. `c8`–`c15` are the RDP's command
    /// registers, which do not exist yet and read zero (Phase 3).
    fn cop0_read(&mut self, index: u32) -> u32 {
        if index < 8 { self.sp.read(index) } else { 0 }
    }

    /// `MTC0` — write an SP register, possibly starting a DMA.
    fn cop0_write(&mut self, index: u32, value: u32) -> StepResult {
        let mut out = StepResult::default();
        if index >= 8 {
            return out;
        }
        if index == sp::reg::STATUS {
            // Both directions propagate. The RSP acknowledging its own
            // interrupt is a `CLR_INTR` write, and it must reach the MI.
            out.interrupt_change = sp::SpRegs::interrupt_change(value);
        }
        out.dma = self.sp.write(index, value);
        out
    }
}

/// A branch target: PC-relative, from the delay slot, masked to 12 bits.
const fn branch_target(sequential: u32, imm: u32) -> u32 {
    sequential.wrapping_add(sext(imm) << 2) & 0xFFC
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assemble into IMEM and run until halted, returning the RSP.
    fn run(program: &[u32], start: u32) -> Rsp {
        let mut rsp = Rsp::new();
        for (i, w) in program.iter().enumerate() {
            let at = (start as usize + i * 4) & 0xFFF;
            for (b, byte) in w.to_be_bytes().iter().enumerate() {
                rsp.imem[(at + b) & 0xFFF] = *byte;
            }
        }
        rsp.sp.set_pc(start);
        rsp.sp.set_halted(false);
        for _ in 0..10_000 {
            rsp.su_step();
            if rsp.sp.halted() {
                break;
            }
        }
        rsp
    }

    const NOP: u32 = 0;
    const BREAK: u32 = 0o15;

    const fn ori(rt: u32, rs: u32, imm: u32) -> u32 {
        (0o15 << 26) | (rs << 21) | (rt << 16) | imm
    }
    const fn sw(rt: u32, base: u32, off: u32) -> u32 {
        (0o53 << 26) | (base << 21) | (rt << 16) | off
    }
    const fn addu(rd: u32, rs: u32, rt: u32) -> u32 {
        (rs << 21) | (rt << 16) | (rd << 11) | 0o41
    }

    /// **`BREAK` halts and latches `BROKE`, and the PC stops after it.**
    ///
    /// `SP_STATUS` reads `0x3` — both bits — which is what n64-systemtest's
    /// `RSP Wrap around` expects.
    #[test]
    fn break_halts_and_sets_broke() {
        let rsp = run(&[NOP, BREAK], 0);
        assert!(rsp.sp.halted());
        assert_eq!(rsp.sp.status(), 0x3, "HALTED | BROKE");
        assert_eq!(rsp.sp.pc(), 0x8, "the PC sits past the BREAK");
    }

    /// **The 12-bit PC wraps at the end of IMEM instead of running off it.**
    ///
    /// n64-systemtest's own case: two `nop`s at `0xFF8`, a `BREAK` at `0x000`,
    /// started at `0xFF8`, expecting to stop at `0x4` with status `0x3`. A core
    /// that lets the PC grow past `0xFFF` fetches garbage and never halts.
    #[test]
    fn the_pc_wraps_from_the_end_of_imem_to_the_start() {
        let mut rsp = Rsp::new();
        for (at, w) in [(0xFF8u32, NOP), (0xFFC, NOP), (0x000, BREAK)] {
            for (b, byte) in w.to_be_bytes().iter().enumerate() {
                rsp.imem[(at as usize + b) & 0xFFF] = *byte;
            }
        }
        rsp.sp.set_pc(0xFF8);
        rsp.sp.set_halted(false);
        for _ in 0..16 {
            rsp.su_step();
            if rsp.sp.halted() {
                break;
            }
        }
        assert_eq!(rsp.sp.pc(), 0x4, "wrapped to 0x000 and stopped after BREAK");
        assert_eq!(rsp.sp.status(), 0x3);
    }

    /// **A misaligned word load is correct, not a fault.**
    ///
    /// The VR4300 raises `AddressError` for exactly this access, which is what
    /// makes it the easiest thing to get wrong by reusing CPU code. Values from
    /// n64-systemtest's `RSP LWU`, which seeds `BADDECAF01234567` at DMEM 0.
    #[test]
    fn a_misaligned_word_load_reads_across_the_boundary() {
        let mut rsp = Rsp::new();
        for (i, b) in 0xBADD_ECAF_0123_4567u64.to_be_bytes().iter().enumerate() {
            rsp.dmem[i] = *b;
        }
        assert_eq!(rsp.dmem_load(0x000, 4), 0xBADD_ECAF);
        assert_eq!(rsp.dmem_load(0x001, 4), 0xDDEC_AF01, "misaligned by one");
        assert_eq!(rsp.dmem_load(0x003, 4), 0xAF01_2345);
    }

    /// A load that runs off the end of DMEM **wraps to the start**, because
    /// every byte address is masked independently.
    #[test]
    fn a_load_at_the_end_of_dmem_wraps_to_the_beginning() {
        let mut rsp = Rsp::new();
        rsp.dmem[0xFFE] = 0xBC;
        rsp.dmem[0xFFF] = 0xAD;
        rsp.dmem[0x000] = 0x7E;
        rsp.dmem[0x001] = 0x8F;
        assert_eq!(rsp.dmem_load(0xFFE, 4), 0xBCAD_7E8F);
    }

    /// The integer core computes and stores: `ori` builds a value, `addu` sums
    /// it, `sw` lands it in DMEM. Chosen so a no-op decode arm cannot pass —
    /// the stored word depends on all three instructions.
    #[test]
    fn the_integer_core_computes_and_stores() {
        let rsp = run(
            &[
                ori(1, 0, 0x1234),
                ori(2, 0, 0x1111),
                addu(3, 1, 2),
                sw(3, 0, 0x20),
                BREAK,
            ],
            0,
        );
        assert_eq!(rsp.dmem_load(0x20, 4), 0x2345, "0x1234 + 0x1111");
    }

    /// `r0` stays zero however hard an instruction tries to write it.
    #[test]
    fn register_zero_is_pinned() {
        let rsp = run(&[ori(0, 0, 0xFFFF), sw(0, 0, 0x30), BREAK], 0);
        assert_eq!(rsp.r(0), 0);
        assert_eq!(rsp.dmem_load(0x30, 4), 0, "and it stores as zero");
    }

    /// A taken branch executes its **delay slot** before redirecting. The slot
    /// here writes a value nothing else writes, so a core that skips it fails.
    #[test]
    fn a_branch_executes_its_delay_slot() {
        // beq r0, r0, +2  /  ori r1, 0x55 (delay slot)  /  ori r1, 0x99 (skipped)
        // ... target: sw r1, 0x40 / break
        let beq = (0o04 << 26) | 2;
        let rsp = run(
            &[beq, ori(1, 0, 0x55), ori(1, 0, 0x99), sw(1, 0, 0x40), BREAK],
            0,
        );
        assert_eq!(
            rsp.dmem_load(0x40, 4),
            0x55,
            "the delay slot ran and the skipped instruction did not"
        );
    }

    /// **The RSP can acknowledge its own interrupt.**
    ///
    /// `MTC0 SP_STATUS` with `CLR_INTR` must reach the MI as a *clear*. With a
    /// plain `bool` on [`StepResult`], "clear the line" and "this step said
    /// nothing about the line" are the same value, so the acknowledgement is
    /// dropped and the CPU's `IP2` stays asserted for ever. The three cases are
    /// asserted separately because that is exactly what a two-state flag cannot
    /// express.
    #[test]
    fn the_rsp_can_raise_and_clear_its_own_interrupt() {
        const SET_INTR: u32 = 1 << 4;
        const CLR_INTR: u32 = 1 << 3;
        let mut rsp = Rsp::new();

        let out = rsp.cop0_write(sp::reg::STATUS, SET_INTR);
        assert_eq!(out.interrupt_change, Some(true), "raise");

        let out = rsp.cop0_write(sp::reg::STATUS, CLR_INTR);
        assert_eq!(out.interrupt_change, Some(false), "acknowledge");

        let out = rsp.cop0_write(sp::reg::STATUS, SET_INTR | CLR_INTR);
        assert_eq!(out.interrupt_change, None, "both together: no change");

        // A write that mentions neither must not disturb the line.
        let out = rsp.cop0_write(sp::reg::STATUS, 1 << 10);
        assert_eq!(out.interrupt_change, None, "unrelated flag write");
    }

    /// `BREAK` raises the line **only** when `INTBREAK` is set.
    ///
    /// Both configurations execute the same single `BREAK` and differ *only* in
    /// that flag, so the differing `interrupt_change` is attributable to nothing
    /// else. The disabled case asserts `None` rather than merely "not raised":
    /// `BREAK` must leave a previously-raised line alone, and `Some(false)`
    /// would clear it.
    #[test]
    fn break_raises_the_interrupt_only_when_enabled() {
        /// Run one `BREAK` at IMEM 0 and return what the step reported.
        fn break_once(intbreak: bool) -> (StepResult, u32) {
            let mut rsp = Rsp::new();
            for (b, byte) in BREAK.to_be_bytes().iter().enumerate() {
                rsp.imem[b] = *byte;
            }
            if intbreak {
                rsp.sp.write(sp::reg::STATUS, 1 << 8); // SET_INTBREAK
            }
            rsp.sp.set_pc(0);
            rsp.sp.set_halted(false);
            let out = rsp.su_step();
            (out, rsp.sp.status())
        }

        let (out, status) = break_once(false);
        assert_eq!(
            out.interrupt_change, None,
            "with INTBREAK clear, BREAK must not touch the line at all -- \
             Some(false) would acknowledge an interrupt it never raised"
        );
        assert_eq!(status & 0x3, 0x3, "still HALTED | BROKE");

        let (out, status) = break_once(true);
        assert_eq!(out.interrupt_change, Some(true), "INTBREAK set");
        assert_eq!(status & 0x3, 0x3, "and it halts either way");
    }

    /// `MFC0` reads the SP registers the CPU shares, and `MTC0` writes them.
    #[test]
    fn cop0_reaches_the_sp_registers() {
        let mut rsp = Rsp::new();
        rsp.sp.write(sp::reg::STATUS, 1 << 10); // SET_SIG0
        let v = rsp.cop0_read(sp::reg::STATUS);
        assert_ne!(v & sp::STATUS_SIG0, 0, "MFC0 sees the signal bit");

        // c8 and above are the RDP's, which do not exist yet.
        assert_eq!(rsp.cop0_read(9), 0);
    }
}
