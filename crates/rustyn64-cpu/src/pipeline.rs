//! The VR4300's five-stage pipeline (ADR 0007).
//!
//! `IC` → `RF` → `EX` → `DC` → `WB` (VR4300 User's Manual §4.1, Figure 4-1):
//! Instruction Cache fetch, Register Fetch, Execution, Data Cache fetch, Write
//! Back. In-order, single-issue, with one architectural delay slot. At least 5
//! `PCycle`s are required to execute an instruction, and up to five are in flight
//! at once when the pipe flows.
//!
//! # Latches, not stages
//!
//! Five stages have four boundaries, and the state lives on the **boundaries**.
//! [`Latch`] is what travels with an instruction as it advances.
//!
//! `in_delay_slot` riding in the latch rather than in a global CPU flag is
//! load-bearing: a multi-cycle stall between a branch and its delay slot
//! desynchronises a global flag, and that is the classic bug in this area. With
//! the flag attached to the instruction, `Cause.BD` and `EPC` come out right for
//! free. `delay_slot_flag_survives_a_multi_cycle_stall` pins it.
//!
//! # Reverse step order is the latching
//!
//! [`Pipeline::advance`] runs **WB → DC → EX → RF → IC**. Each stage reads its
//! input latch and writes its output latch, so running downstream-first means a
//! stage's input still holds the *previous* cycle's value when it is read. No
//! value can therefore propagate two stages in one cycle, and **no double
//! buffering is needed** — the reverse order *is* the latching.
//!
//! This is a load-bearing invariant, not a style choice. Reversing it silently
//! makes the pipeline one-cycle-too-fast; `a_value_advances_exactly_one_stage_per_cycle`
//! is the guard.
//!
//! # Status
//!
//! **Structure only.** The stages move latches and account for time; they do not
//! decode or execute yet (T-11-002 onward). What is real here is the shape, the
//! stall/interlock mechanism, the delay-slot carriage, and the interrupt gate —
//! the parts that cannot be retrofitted later without rewriting every consumer.

use crate::Bus;
use crate::alu::HiLo;
use crate::cop0::Cop0;
use crate::cop1::Cop1Control;
use crate::decode::{Decoded, decode};
use crate::exception;
use crate::exec::{Cop0Access, Cop1Access, MemOp, TlbOp, WriteBack, execute};
use crate::fpr::Fpr;
use crate::mem;
use crate::regs::Regs;
use crate::softfloat;
use crate::tlb::Tlb;

/// The five pipeline stages, in hardware order (UM §4.1, Figure 4-1).
///
/// Note the names: **IC** and **DC**, not `IF`/`DF`. The manual's whole interlock
/// and exception taxonomy is stated stage-relative, so these spellings are what
/// make a citation resolvable.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Stage {
    /// Instruction Cache fetch.
    Ic,
    /// Register Fetch.
    Rf,
    /// Execution.
    Ex,
    /// Data Cache fetch — the bus access, and where interrupts are sampled.
    Dc,
    /// Write Back.
    Wb,
}

/// An aborting condition travelling down the pipe.
///
/// Deliberately **not** called `Fault`. UM §4.5 defines a *fault* as the union of
/// interlocks and exceptions (Figure 4-11: Faults = Interlocks ∪ Exceptions,
/// split into Stalls vs Abort), and CEN64 follows that wider usage. What rides in
/// a latch here is only the aborting subset, so it carries the narrower name.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Exception {
    /// An interrupt was accepted (`Cause.IP` unmasked, `IE` set, `EXL`/`ERL` clear).
    Interrupt,
    /// Address error on an instruction fetch or data access.
    ///
    /// Carries the direction because the architecture does: `AdEL` (4) and
    /// `AdES` (5) are **different** `ExcCode` values (UM Table 6-2, p. 172), and
    /// a handler distinguishes them. An instruction fetch is a load.
    AddressError {
        /// The faulting access was a store.
        store: bool,
    },
    /// Integer overflow (`ADD`, `ADDI`, `SUB`, `DADD`, …).
    Overflow,
    /// `SYSCALL`.
    Syscall,
    /// `BREAK`.
    Breakpoint,
    /// A conditional trap (`TGE`, `TEQ`, `TNEI`, …) whose condition held.
    Trap,
    /// A reserved / unimplemented opcode.
    ReservedInstruction,
    /// A reserved encoding **within a coprocessor's** instruction space.
    ///
    /// Reports the same `ExcCode` as [`Exception::ReservedInstruction`] but
    /// also sets `Cause.CE` to the coprocessor number. A plain
    /// `ReservedInstruction` leaves `CE` at zero, so the two are not
    /// interchangeable — n64-systemtest checks the whole `Cause` register, and
    /// `DCFC2` with `CU2` set expects `CE = 2`.
    CoprocessorReserved {
        /// Which coprocessor, for `Cause.CE`.
        unit: u8,
    },
    /// A coprocessor instruction with that unit disabled in `Status.CU`.
    CoprocessorUnusable {
        /// Which coprocessor, for `Cause.CE`.
        unit: u8,
    },
    /// A TLB refill — no entry matched. Takes the **refill** vector.
    TlbRefill {
        /// The faulting access was a store (`TLBS` rather than `TLBL`).
        store: bool,
    },
    /// A TLB entry matched but was invalid. Takes the **general** vector, with
    /// the same `ExcCode` as a refill — the vector is the only difference, which
    /// is why they are separate variants rather than one with a flag nobody
    /// reads.
    TlbInvalid {
        /// The faulting access was a store.
        store: bool,
    },
    /// A store to a valid but non-writable page.
    TlbModified,
    /// A floating-point operation raised a condition whose `FCSR.Enable` bit is
    /// set.
    ///
    /// Carries nothing: which condition fired is reported in `FCSR.Cause`, not
    /// in COP0 `Cause`, and the handler reads it from there. Adding a field
    /// here would duplicate — and could contradict — the architectural record.
    FloatingPoint,
}

/// What a COP1 operation writes when it does not trap.
///
/// Named rather than a `(u64, bool)` pair because the destinations are of
/// genuinely different kinds — two FPR widths and a single `FCSR` bit — and a
/// flag pair makes "write the condition to `fd`" representable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FpCommit {
    /// A 32-bit result into `fd`'s low half: `.S` values and `.W` integers.
    Single(u32),
    /// A 64-bit result into `fd` through the `FR` view: `.D` values and `.L`
    /// integers.
    Double(u64),
    /// `FCSR.C`. Only `C.cond.fmt` produces this, and it writes no FPR at all.
    Condition(bool),
}

/// The documented interlocks (UM Table 4-3).
///
/// Held as a named enum rather than a bare cycle count so a stall is always
/// attributable — "why did this stall" is answerable from a trace.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Interlock {
    /// Load interlock — 1 cycle (UM §4.6.5).
    ///
    /// Deliberately **imprecise**, matching hardware: it fires when the next
    /// instruction's `rs` *or* `rt` field equals the load's `rt`, whether or not
    /// that field is actually used as a source. See [`load_interlocks`].
    Ldi,
    /// Data cache busy — a cached store keeps the cache busy for its `DC` *and*
    /// `WB` stages, so a following cache access stalls 1 cycle (UM §4.6.7).
    Dcb,
    /// Data cache miss — the fill cost is `8..=9 + M` `PCycle`s (UM Table 11-1).
    Dcm,
    /// Instruction cache busy (UM §4.6.3).
    Icb,
    /// Instruction micro-TLB miss — 3 `PCycle`s (UM §4.6.2).
    Itm,
    /// Multi-cycle interlock: `MULT`/`DIV`/FPU stall the whole pipeline for the
    /// documented count (UM Tables 3-12, 7-14).
    Mci,
    /// Cache operation (UM Table 4-3).
    Cop,
    /// CP0 bypass interlock — **1 `PCycle`** (UM §4.6.9, p. 113).
    ///
    /// Fires when an instruction that caused an exception reaches `WB` while the
    /// next instruction in `DC` reads any CP0 register. The cost was recorded as
    /// undocumented in three files while sitting in the very paragraph they
    /// cited; see `docs/engineering-lessons.md` §3.3b.
    Cp0i,
    /// Taking an exception — **2 `PCycle`s** (UM §4.7, p. 114).
    ///
    /// Not strictly one of Table 4-3's eight interlocks: the pipeline stalls
    /// while the epilogue runs and the aborted instructions drain. Named here so
    /// a trace can attribute the cycles rather than showing an unexplained gap.
    Exception,
}

/// A stall request: how long, and what caused it.
///
/// ADR 0007 describes an interlock as `(cycles, resume_stage)`. The `resume`
/// half is **deliberately absent** until it can be load-bearing. Today
/// [`Pipeline::advance`] always runs the full cascade when not stalled, so a
/// stored `resume` would be read by nothing — and a field that looks like it
/// carries information while carrying none is the exact hazard
/// `docs/engineering-lessons.md` §3.2 is about. `Bus::poll_irq_at_phase` was
/// removed for the same reason rather than left in place looking wired.
///
/// It lands with T-11-002, when stages can stall independently and a partial
/// resume becomes meaningful.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Stall {
    /// `PCycle`s remaining.
    pub cycles: u32,
    /// Which documented interlock caused it.
    pub cause: Interlock,
}

/// State carried across one inter-stage boundary — what travels *with* an
/// instruction as it advances.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Latch {
    /// Is an instruction present at this boundary? A bubble is `false`.
    pub occupied: bool,
    /// The PC of the instruction in flight.
    pub pc: u64,
    /// The raw instruction word (decode is T-11-002).
    pub word: u32,
    /// Is this instruction in a branch delay slot?
    ///
    /// Travels with the instruction, never as global CPU state — that is the
    /// whole point (see the module docs).
    pub in_delay_slot: bool,
    /// An aborting condition stamped into this latch and every latch upstream.
    pub abort: Option<Exception>,
    /// The decoded instruction, filled at `IC`.
    pub decoded: Decoded,
    /// `rs` value, read at `RF`.
    pub rs_val: u64,
    /// `rt` value, read at `RF`.
    pub rt_val: u64,
    /// What `EX` computed, committed at `WB`.
    pub write_back: WriteBack,
    /// A memory access `EX` resolved for `DC` to perform.
    pub mem: Option<MemOp>,
    /// A COP0 access `EX` resolved: read performed in `DC`, write in `WB`.
    ///
    /// Split across those two stages because UM §4.6.9 defines the CP0 bypass
    /// interlock in terms of a write reaching `WB` while the next instruction
    /// reads in `DC` — a rule that cannot be expressed if both happen in `EX`.
    pub cop0: Option<Cop0Access>,
}

/// Does a load into `load_rt` interlock with the following instruction?
///
/// `rs` / `rt` are the *raw encoded fields* of the next instruction,
/// deliberately named for the encoding rather than for operands — the hardware
/// checks the fields whether or not they are used as sources, and naming them
/// `next_rs`/`next_operand` would imply a semantics the check does not have.
///
/// Reproduces the hardware's **imprecision**, which is the specification here —
/// emulating precise behaviour is the bug. From
/// `n64brew_wiki/markdown/VR4300.md` § Microarchitecture → Load Delay Interlock:
///
/// - Matches the load's `rt` against the next instruction's `rs` **or** `rt`
///   field, "whether or not they are actually used as a source". So a load
///   followed by `LUI` into the same register stalls, and two consecutive loads
///   into the same register stall.
/// - A load into `$zero` never interlocks.
/// - GPR loads interlock only with non-float instructions, and FPR loads only
///   with float instructions.
#[must_use]
pub const fn load_interlocks(load_rt: u8, rs: u8, rt: u8, same_reg_file: bool) -> bool {
    // The zero register is exempt: a load into $zero is discarded, so nothing
    // downstream can depend on it.
    if load_rt == 0 {
        return false;
    }
    if !same_reg_file {
        return false;
    }
    load_rt == rs || load_rt == rt
}

/// `Status.FR` — whether the FP register file presents 32 independent 64-bit
/// registers (set) or 16 built from FGR pairs (clear).
fn fr_of(cop0: &Cop0) -> bool {
    cop0.read(crate::cop0::reg::STATUS) & (1 << 26) != 0
}

/// An exception captured at its raising site, with the context the epilogue
/// needs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Pending {
    /// What happened.
    exc: Exception,
    /// The faulting instruction's address.
    pc: u64,
    /// Was the faulting instruction in a branch delay slot?
    in_delay_slot: bool,
    /// The offending address, for the exceptions that write `BadVAddr`.
    bad_vaddr: u64,
}

/// The four inter-stage latches plus the pipeline control state.
#[derive(Clone, Debug, Default)]
pub struct Pipeline {
    /// The **whole** of COP2: one 64-bit latch.
    ///
    /// COP2 is not populated on the VR4300, and what remains is a single
    /// value that every `MTC2`/`DMTC2` writes and every `MFC2`/`DMFC2` reads,
    /// with the register index ignored. See the `EX` stage and ledger C-20.
    pub cop2_latch: u64,
    /// `IC` → `RF`.
    pub ic_rf: Latch,
    /// `RF` → `EX`.
    pub rf_ex: Latch,
    /// `EX` → `DC`.
    pub ex_dc: Latch,
    /// `DC` → `WB`.
    pub dc_wb: Latch,
    /// Remaining stall cycles; while non-zero the pipeline does not advance.
    stall: Option<Stall>,
    /// Was the *previous* `PCycle` a run cycle (not a stall)?
    ///
    /// UM §4.7.1: *"NMI and interrupt exception requests are accepted only if the
    /// previous `PCycle` was a run cycle."* This is the gate, and it is the
    /// reason the flag exists at all.
    prev_was_run: bool,
    /// An abort was raised this cycle, so `IC` must fetch a bubble rather than a
    /// live instruction — the wrong-path fetch would otherwise escape the flush.
    ///
    /// Cleared at the end of each [`Pipeline::advance`].
    flush_pending: bool,
    /// Instructions retired at `WB` — a work tally, not a time position.
    pub retired: u64,
    /// An exception raised this cycle, awaiting dispatch at the end of it.
    ///
    /// Captured where it is *raised* rather than reconstructed afterwards,
    /// because the epilogue needs the faulting instruction's PC and delay-slot
    /// flag — and by the end of the cycle the reverse cascade has moved every
    /// latch, so the faulting instruction is no longer where it was.
    pending: Option<Pending>,
    /// The virtual address of the access that faulted this cycle.
    ///
    /// Recorded where the fault is *detected*, because `BadVAddr` needs the
    /// address and `Self::access` reports only which exception. Reconstructing
    /// it at dispatch time is impossible — the `MemOp` has been consumed.
    fault_vaddr: u64,
    /// COP1 **control** registers (T-12-006).
    pub cop1: Cop1Control,
    /// The floating-point register file (T-13-001).
    pub fpr: Fpr,
    /// The joint TLB and its instruction micro-TLB (T-12-004).
    pub tlb: Tlb,
    /// The 16 KiB primary instruction cache (T-11-003).
    pub icache: crate::cache::Icache,
    /// The 8 KiB primary write-back data cache (T-11-003).
    pub dcache: crate::cache::Dcache,
    /// The COP0 register file (T-12-001).
    ///
    /// Public because exception dispatch, the TLB and the interrupt path all
    /// read it, and they land in separate tickets.
    pub cop0: Cop0,
    /// The link bit, `LLbit`.
    ///
    /// *"set by the LL instruction, cleared by an ERET, and tested by the SC
    /// instruction"* (UM §3.1). Note what is **absent** from that list: `SC`
    /// itself does not clear it, and neither does an intervening load or store.
    /// Clearing it in `SC` is the natural-looking mistake, and it makes a
    /// retried `LL`/`SC` loop fail forever on the second iteration.
    ///
    /// `ERET` is the other half and lands with the exception model (Sprint 2);
    /// until then nothing clears this, which is correct-so-far rather than
    /// finished — recorded as such in `docs/cpu.md`.
    ll_bit: bool,
}

impl Pipeline {
    /// A fresh, empty pipeline.
    #[must_use]
    pub const fn new() -> Self {
        const EMPTY: Latch = Latch {
            occupied: false,
            pc: 0,
            word: 0,
            in_delay_slot: false,
            abort: None,
            cop0: None,
            decoded: Decoded {
                op: crate::decode::Op::Reserved,
                rs: 0,
                rt: 0,
                rd: 0,
                sa: 0,
                imm: 0,
                dest: 0,
                target: 0,
            },
            rs_val: 0,
            rt_val: 0,
            write_back: WriteBack::None,
            mem: None,
        };
        Self {
            // Power-on value: a documented zero (ADR 0004).
            cop2_latch: 0,
            ic_rf: EMPTY,
            rf_ex: EMPTY,
            ex_dc: EMPTY,
            dc_wb: EMPTY,
            stall: None,
            prev_was_run: false,
            flush_pending: false,
            retired: 0,
            pending: None,
            fault_vaddr: 0,
            cop1: Cop1Control::new(),
            fpr: Fpr::new(),
            tlb: Tlb::new(),
            icache: crate::cache::Icache::new(),
            dcache: crate::cache::Dcache::new(),
            cop0: Cop0::new(),
            ll_bit: false,
        }
    }

    /// The interlock currently stalling the pipeline, if any.
    #[must_use]
    pub const fn stalled_by(&self) -> Option<Interlock> {
        match self.stall {
            Some(s) => Some(s.cause),
            None => None,
        }
    }

    /// Was the previous `PCycle` a run cycle? Gates interrupt acceptance.
    #[must_use]
    pub const fn prev_cycle_was_run(&self) -> bool {
        self.prev_was_run
    }

    /// The link bit, as `SC` would test it.
    ///
    /// Exposed for the COP0 / `ERET` work in Sprint 2, which must clear it.
    #[must_use]
    pub const fn ll_bit(&self) -> bool {
        self.ll_bit
    }

    /// `LLAddr` (COP0 register 17): `PA(31:4)` of the most recent `LL`.
    ///
    /// Reads straight out of the COP0 file. `LL` writes it there and nowhere
    /// else — there is deliberately no second copy, because two stores of one
    /// architectural value drift, and `MFC0 $rt, $17` would then disagree with
    /// the CPU's own idea of the link address.
    #[must_use]
    pub const fn ll_addr(&self) -> u64 {
        self.cop0.read(crate::cop0::reg::LL_ADDR)
    }

    /// Request a stall of `cycles` `PCycle`s.
    ///
    /// A zero-cycle request is **not** a stall and is ignored. Recording it would
    /// still consume a cycle in [`Pipeline::advance`] and mark it as not-a-run
    /// cycle, which silently inserts a bubble *and* suppresses interrupt
    /// acceptance on the following cycle (UM §4.7.1) — a one-cycle timing error
    /// with no visible cause.
    pub const fn stall_for(&mut self, cycles: u32, cause: Interlock) {
        if cycles == 0 {
            return;
        }
        self.stall = Some(Stall { cycles, cause });
    }

    /// Stamp an abort into `at` **and every latch upstream of it** — the
    /// kill-younger-instructions step. Instructions older than `at` have already
    /// passed and are unaffected.
    ///
    /// # Ordering contract
    ///
    /// **A stage must call this BEFORE it moves its latch.** The instruction
    /// executing in stage S this cycle sits in S's *input* latch until the move,
    /// so stamping first is what makes the abort travel with the instruction that
    /// caused it. Calling it after the move stamps the abort onto the *younger*
    /// instruction instead, and the causing one escapes — a misalignment that no
    /// single-cycle assertion catches. `an_abort_survives_the_cascade` advances
    /// the pipeline to verify it, rather than checking latch state in place.
    ///
    /// The abort also raises an internal pending-flush flag, so the instruction
    /// fetched later in the same cycle is a bubble rather than a live
    /// wrong-path fetch.
    pub const fn abort_from(&mut self, at: Stage, exc: Exception) {
        self.abort_with(at, exc, 0);
    }

    /// [`Pipeline::abort_from`], additionally recording the offending address
    /// for the exceptions that write `BadVAddr`.
    pub const fn abort_with(&mut self, at: Stage, exc: Exception, bad_vaddr: u64) {
        // Capture the faulting instruction's context NOW. The latch holding it
        // is the one this stage is reading -- `ex_dc` for DC, `rf_ex` for EX --
        // and by the end of the cycle the cascade will have moved it on.
        //
        // Priority: an exception already pending this cycle came from a LATER
        // stage (the cascade runs WB first), and UM §4.7.2 gives a later stage
        // precedence over an earlier one. So the first capture wins.
        if self.pending.is_none() {
            let src = match at {
                Stage::Wb => &self.dc_wb,
                Stage::Dc => &self.ex_dc,
                Stage::Ex => &self.rf_ex,
                Stage::Rf | Stage::Ic => &self.ic_rf,
            };
            self.pending = Some(Pending {
                exc,
                pc: src.pc,
                in_delay_slot: src.in_delay_slot,
                bad_vaddr,
            });
        }
        self.flush_pending = true;
        match at {
            Stage::Wb => {
                self.dc_wb.abort = Some(exc);
                self.ex_dc.abort = Some(exc);
                self.rf_ex.abort = Some(exc);
                self.ic_rf.abort = Some(exc);
            }
            Stage::Dc => {
                self.ex_dc.abort = Some(exc);
                self.rf_ex.abort = Some(exc);
                self.ic_rf.abort = Some(exc);
            }
            Stage::Ex => {
                self.rf_ex.abort = Some(exc);
                self.ic_rf.abort = Some(exc);
            }
            Stage::Rf | Stage::Ic => self.ic_rf.abort = Some(exc),
        }
    }

    /// Advance the pipeline by exactly one `PCycle`.
    ///
    /// Stages run **WB → DC → EX → RF → IC**. Because each stage reads its input
    /// latch before any upstream stage writes it, no value moves two stages in
    /// one cycle and no double buffering is required. Do not reorder this.
    ///
    /// Hot path: allocation-free.
    pub fn advance<B: Bus>(&mut self, bus: &mut B, regs: &mut Regs, next_pc: &mut u64) {
        // The timeline is HELD, not advanced. `Count` runs at half PClock, so
        // bumping it once per PClock here would run the timer at double rate --
        // and inventing a parity bit to halve it would be a second incremented
        // counter, which is what ADR 0006 exists to forbid.
        //
        // Holding is the honest option: with no scheduler attached there is no
        // timeline, so `Count` does not move. Anything exercising `Count` or
        // `Compare` must call `advance_at` and supply the position.
        self.advance_at(bus, regs, next_pc, self.cop0.count_now());
    }

    /// [`Pipeline::advance`], with the scheduler's `Count` timeline supplied.
    ///
    /// `Count` is **derived** from the master clock (ADR 0006), so the position
    /// is passed in rather than incremented here. This is the path the scheduler
    /// uses ([`crate::Cpu::tick_at`]); [`Pipeline::advance`] is a convenience
    /// for callers with no scheduler, and **holds** the timeline rather than
    /// guessing at it.
    pub fn advance_at<B: Bus>(
        &mut self,
        bus: &mut B,
        regs: &mut Regs,
        next_pc: &mut u64,
        count_now: u64,
    ) {
        self.cop0.set_now(count_now);
        // A stall consumes the cycle. The pipeline holds its state, and the cycle
        // is recorded as NOT a run cycle so an interrupt cannot be accepted on the
        // cycle following it (UM §4.7.1).
        if let Some(mut s) = self.stall {
            s.cycles = s.cycles.saturating_sub(1);
            self.stall = if s.cycles == 0 { None } else { Some(s) };
            self.prev_was_run = false;
            return;
        }

        self.wb_stage(regs);
        self.dc_stage(bus);
        self.ex_stage(regs, next_pc);
        self.rf_stage(regs);
        self.ic_stage(bus, next_pc);

        self.prev_was_run = true;
        self.flush_pending = false;

        // Dispatch AFTER the cascade, so every stage has seen the abort and the
        // pipeline is drained, and exactly once per cycle regardless of how many
        // stages raised.
        if let Some(p) = self.pending.take() {
            let d = exception::dispatch(&mut self.cop0, p.exc, p.pc, p.in_delay_slot, p.bad_vaddr);
            *next_pc = d.vector;
            self.stall_for(d.stall_cycles, Interlock::Exception);
        }
    }

    /// `WB` — commit the result and retire the instruction.
    fn wb_stage(&mut self, regs: &mut Regs) {
        if self.dc_wb.occupied && self.dc_wb.abort.is_none() {
            // The COP0 WRITE lands here (UM §4.6.9). A `Read` in this latch was
            // already performed in DC and left its value in `write_back`.
            // The TLB instructions land in WB, with the COP0 write: they are
            // CP0 operations and `TLBR`/`TLBP` write COP0 registers, so doing
            // them earlier would let a following `MFC0` read the result a cycle
            // before hardware produces it.
            if let Some(Cop0Access::Cop1(Cop1Access::WriteFpr { dest, value, wide })) =
                self.dc_wb.cop0
            {
                // DMTC1 mirrors DMFC1: the FR view, not the physical register.
                if wide {
                    let fr = fr_of(&self.cop0);
                    self.fpr.write_d(dest, fr, value);
                } else {
                    self.fpr.write_s(dest, fr_of(&self.cop0), value as u32);
                }
            }
            if let Some(Cop0Access::Cop1(Cop1Access::WriteControl { dest, value })) =
                self.dc_wb.cop0
            {
                self.cop1.ctc1(dest, value);
                // **`CTC1` can raise on its own.** Writing `FCSR` with a Cause
                // bit whose Enable is also set meets the trap condition
                // immediately -- no arithmetic required. n64-systemtest writes
                // `enable_overflow | cause_overflow` in one go and expects the
                // FP exception to report the `CTC1` itself as `ExceptPC`.
                //
                // Bit 17 (Unimplemented) is unmaskable and traps regardless,
                // which is why it is tested outside the enable comparison.
                if self.fcsr_traps_now() {
                    self.abort_from(Stage::Wb, Exception::FloatingPoint);
                    self.dc_wb.occupied = false;
                    return;
                }
            }
            if let Some(Cop0Access::Cop1(Cop1Access::Arith {
                fmt,
                funct,
                ft,
                fs,
                fd,
            })) = self.dc_wb.cop0
                && self.fp_arith(fmt, funct, ft, fs, fd)
            {
                // Trapped. The instruction does **not** complete, so it must not
                // reach the retirement tail below: `Random` "decrements as each
                // instruction executes" (UM §5.4.2), and one that took an
                // exception did not execute.
                self.dc_wb.occupied = false;
                return;
            }
            if let Some(Cop0Access::Tlb(op)) = self.dc_wb.cop0 {
                match op {
                    TlbOp::Read => {
                        let i = (self.cop0.read(crate::cop0::reg::INDEX) & 0x3F) as usize;
                        self.tlb.read_entry(i, &mut self.cop0);
                    }
                    TlbOp::WriteIndexed => {
                        let i = (self.cop0.read(crate::cop0::reg::INDEX) & 0x3F) as usize;
                        // TLBWI CAN overwrite a wired entry; only TLBWR cannot
                        // (UM §5.4.4, p. 150). Guarding both is a natural-looking
                        // mistake that makes wired entries unwritable at all.
                        self.tlb.write_entry(i, &self.cop0);
                    }
                    TlbOp::WriteRandom => {
                        // `Random` never goes below `Wired`, so the wired entries
                        // are protected by the counter's range rather than by a
                        // check here -- which is how hardware does it.
                        let i = (self.cop0.read(crate::cop0::reg::RANDOM) & 0x3F) as usize;
                        self.tlb.write_entry(i, &self.cop0);
                    }
                    TlbOp::Probe => self.tlb.probe(&mut self.cop0),
                }
            }
            if let Some(Cop0Access::Write { dest, value, wide }) = self.dc_wb.cop0 {
                if wide {
                    self.cop0.dmtc0(dest, value);
                } else {
                    self.cop0.mtc0(dest, value);
                }
            }
            match self.dc_wb.write_back {
                WriteBack::None => {}
                // `Regs::write` discards `$zero`, so no guard is needed here --
                // and must not be added, or the rule lives in two places.
                WriteBack::Gpr { dest, value } => regs.write(dest, value),
                WriteBack::HiLo(hl) => {
                    regs.hi = hl.hi;
                    regs.lo = hl.lo;
                }
                WriteBack::Hi(v) => regs.hi = v,
                WriteBack::Lo(v) => regs.lo = v,
            }
            self.retired = self.retired.wrapping_add(1);
            // "Random decrements as each instruction executes" (UM §5.4.2,
            // p. 147) -- advanced HERE, at retirement, so it counts executed
            // instructions rather than cycles.
            //
            // This was implemented and then never called from the pipeline, so
            // `Random` sat at 31 forever and **every `TLBWR` overwrote the same
            // entry**. A refill handler that needs more than one mapping live at
            // once therefore destroys its previous entry on each miss and faults
            // again immediately -- an infinite refill loop, which is exactly what
            // n64-systemtest hit. A stuck counter is invisible to any test that
            // calls `tick_random` itself.
            self.cop0.tick_random();
        }
        self.dc_wb.occupied = false;
    }

    /// `DC` — the data-cache access, and the interrupt sampling point.
    ///
    /// The stage placement is documented, not inherited from a reference
    /// implementation: UM Figure 4-12 puts `INTR` in the `DC` column and §4.7.6
    /// "DC-Stage Interlock and Exception Priorities" lists the interrupt
    /// exception among them.
    fn dc_stage<B: Bus>(&mut self, bus: &mut B) {
        // Sample interrupts once per PCycle here. Accepted only if the previous
        // PCycle was a run cycle (UM §4.7.1). This is the ONLY interrupt
        // recognition predicate in the tree -- carrying two subtly different ones
        // is a known source of one-cycle discrepancies in other emulators.
        //
        // Two steps, and they are different things: the IP bits track what the
        // hardware is *asserting* regardless of masks, and recognition then
        // applies IE/EXL/ERL/IM. Folding them together would make a masked
        // interrupt invisible to `MFC0 Cause`, which software polls.
        //
        // IP2 is the RCP's aggregate line from the MI (libdragon `cop0.h`:
        // `C0_INTERRUPT_RCP = C0_INTERRUPT_2`; ledger U-4). IP3 is CART, IP4
        // PRENMI, IP7 the timer; the rest are unused on this board.
        self.cop0.set_ip(2, bus.poll_irq());
        // IP7 is LATCHED on the match and stays set until `Compare` is written
        // (UM §6.4.18, p. 200) -- note the one-way `if`, with no `else` clearing
        // it. Modelling it as a level tied to `Count == Compare` looks tidier
        // and silently DROPS any timer interrupt that fires while `EXL` is set,
        // because the equality holds for one tick and the handler never sees it.
        //
        // The trigger is the rising EDGE of the match, not the standing
        // equality: both `Count` and `Compare` reset to zero, so an equality
        // test latches `IP7` before a single instruction retires. See
        // `Cop0::timer_edge`.
        if self.cop0.timer_edge() {
            self.cop0.set_ip(7, true);
        }

        if self.prev_was_run && self.cop0.interrupt_pending() {
            self.abort_from(Stage::Dc, Exception::Interrupt);
        }
        // The memory access. This is the point the scheduler interleaves the RCP
        // around -- the whole reason the pipeline is modelled at all (ADR 0007).
        let mut out = self.ex_dc;
        if out.occupied
            && out.abort.is_none()
            && let Some(op) = out.mem
        {
            match self.access(bus, op) {
                Ok(wb) => out.write_back = wb,
                // Stamp before the latch move so the abort travels with the
                // instruction that caused it -- see `abort_from`.
                Err(exc) => {
                    self.abort_with(Stage::Dc, exc, self.fault_vaddr);
                    out = self.ex_dc;
                }
            }
        }
        // The COP0 READ happens here, in DC (UM §4.6.9). The write does not --
        // it happens in WB, and keeping them in different stages is what makes
        // the CP0 bypass interlock expressible at all.
        if out.occupied
            && out.abort.is_none()
            && let Some(Cop0Access::Cop1(Cop1Access::ReadFpr { src, dest, wide })) = out.cop0
        {
            out.write_back = WriteBack::Gpr {
                dest,
                // DMFC1 applies the FR view -- it does NOT move the physical
                // register. UM Ch. 17's pseudocode is explicit:
                //
                //   if FR = 1        then data <- FGR[fs]
                //   else if fs0 = 0  then data <- FGR[fs+1] || FGR[fs]
                //   else                  data <- undefined
                //
                // So with FR = 0 and an even `fs` it reads the PAIR, exactly like
                // LDC1. Only an odd `fs` with FR = 0 is undefined -- and it is
                // *undefined*, not a Reserved Instruction exception.
                //
                // MFC1 moves the low word, sign-extended, in both modes.
                value: if wide {
                    self.fpr.read_d(src, fr_of(&self.cop0))
                } else {
                    crate::alu::sext32(self.fpr.read_s(src, fr_of(&self.cop0)))
                },
            };
        }
        if out.occupied
            && out.abort.is_none()
            && let Some(Cop0Access::Cop1(Cop1Access::ReadControl { src, dest })) = out.cop0
        {
            out.write_back = WriteBack::Gpr {
                dest,
                // CFC1 is a 32-bit move, so the result is sign-extended into the
                // 64-bit GPR exactly as MFC0's is.
                value: crate::alu::sext32(self.cop1.cfc1(src)),
            };
        }
        if out.occupied
            && out.abort.is_none()
            && let Some(Cop0Access::Read { src, dest, wide }) = out.cop0
        {
            let value = if wide {
                self.cop0.dmfc0(src)
            } else {
                self.cop0.mfc0(src)
            };
            out.write_back = WriteBack::Gpr { dest, value };
        }
        self.dc_wb = out;
        self.ex_dc.occupied = false;
    }

    /// The operand bypass network (UM §4.6).
    ///
    /// *"Bypassing ... allows data and conditions produced in the `EX`, `DC` and
    /// `WB` stages to be made available to the `EX` stage of the next cycle."*
    ///
    /// Without this, back-to-back dependent instructions read stale registers and
    /// essentially every real program computes wrong values — `LUI`+`ORI`, the
    /// standard way to build a 32-bit constant, breaks immediately. Its absence
    /// was invisible to every unit test in this crate and was caught only by
    /// `a_program_executes_through_the_whole_pipeline`.
    ///
    /// By the time `EX` runs, the reverse cascade has already committed one
    /// instruction (`WB` ran first, so the register file is current) and moved the
    /// next into `dc_wb`. Exactly **one** producer can therefore still be
    /// uncommitted, and `dc_wb` is it.
    ///
    /// Loads are the case this does *not* cover — a load's value is not ready in
    /// time, which is precisely why the hardware has a load-delay interlock. That
    /// lands with T-11-003 alongside the loads themselves.
    fn bypass(&self, reg: u8, regs: &Regs) -> u64 {
        if reg != 0
            && self.dc_wb.occupied
            && self.dc_wb.abort.is_none()
            && let WriteBack::Gpr { dest, value } = self.dc_wb.write_back
            && dest == reg
        {
            return value;
        }
        regs.read(reg)
    }

    /// `HI`/`LO` as `EX` should see them, bypassing an uncommitted producer.
    fn bypass_hi_lo(&self, regs: &Regs) -> HiLo {
        if self.dc_wb.occupied && self.dc_wb.abort.is_none() {
            match self.dc_wb.write_back {
                WriteBack::HiLo(hl) => return hl,
                WriteBack::Hi(v) => return HiLo { hi: v, lo: regs.lo },
                WriteBack::Lo(v) => return HiLo { hi: regs.hi, lo: v },
                _ => {}
            }
        }
        HiLo {
            hi: regs.hi,
            lo: regs.lo,
        }
    }

    /// Map a TLB fault to the exception it raises.
    ///
    /// The `store` flag selects `TLBL` vs `TLBS`; the *variant* selects the
    /// vector. Both matter and they are independent.
    const fn tlb_exception(f: crate::tlb::TlbFault, store: bool) -> Exception {
        match f {
            crate::tlb::TlbFault::Refill => Exception::TlbRefill { store },
            crate::tlb::TlbFault::Invalid => Exception::TlbInvalid { store },
            // Modified only ever arises on a store, so it carries no flag.
            crate::tlb::TlbFault::Modified => Exception::TlbModified,
        }
    }

    /// Translate a data address through the TLB.
    ///
    /// Returns the whole [`crate::addr::Physical`], **cacheability included**.
    /// It used to return the address alone, which is exactly the information a
    /// D-cache needs and cannot recover: once KSEG0 and KSEG1 have both become
    /// the same physical number, nothing downstream can tell them apart.
    fn translate_data(
        &mut self,
        vaddr: u64,
        store: bool,
    ) -> Result<crate::addr::Physical, Exception> {
        let asid = (self.cop0.read(crate::cop0::reg::ENTRY_HI) & 0xFF) as u8;
        let access = self.access_mode();
        match crate::addr::translate_via(&mut self.tlb, vaddr, asid, store, access) {
            Ok(p) => Ok(p),
            Err(e) => {
                self.fault_vaddr = vaddr;
                self.note_shutdown();
                Err(match e {
                    crate::addr::TranslateError::Address => Exception::AddressError { store },
                    crate::addr::TranslateError::Tlb(f) => Self::tlb_exception(f, store),
                })
            }
        }
    }

    /// The unaligned `LWL`/`LWR`/`LDL`/`LDR`/`SWL`/`SWR`/`SDL`/`SDR` family.
    ///
    /// Split out of [`Pipeline::access`] purely for size; the merging rules live
    /// in [`crate::mem`] and the alignment exemption is by construction — being
    /// usable at any byte offset is the entire reason these instructions exist.
    ///
    /// # Errors
    ///
    /// A TLB fault on the container address.
    fn access_unaligned<B: Bus>(
        &mut self,
        bus: &mut B,
        op: crate::decode::Op,
        addr: u64,
        rt: u64,
        dest: u8,
    ) -> Result<WriteBack, Exception> {
        use crate::decode::Op;
        // The unaligned family splits into loads and stores, and the
        // TLB check differs: a store must find the `D` bit set.
        let is_store = matches!(op, Op::Swl | Op::Swr | Op::Sdl | Op::Sdr);
        // On a fault, `BadVAddr` reports the address the INSTRUCTION named, not
        // the container address translated on its behalf. `translate_data`
        // records whatever it was handed, so the aligned-down value has to be
        // corrected back -- otherwise a fault on `SWL 0x12345001` reports
        // `0x12345000`, and n64-systemtest checks that exact case.
        let translate = |p: &mut Self, a: u64| match p.translate_data(a, is_store) {
            Ok(v) => Ok(v),
            Err(e) => {
                p.fault_vaddr = addr;
                Err(e)
            }
        };
        let word_addr = translate(self, addr & !3)?;
        let dword_addr = translate(self, addr & !7)?;
        let byte4 = addr & 3;
        let byte8 = addr & 7;
        Ok(match op {
            Op::Lwl | Op::Lwr => {
                let w = self.read_width(bus, word_addr, 4) as u32;
                let v = if matches!(op, Op::Lwl) {
                    mem::lwl(rt, w, byte4)
                } else {
                    mem::lwr(rt, w, byte4)
                };
                WriteBack::Gpr { dest, value: v }
            }
            Op::Ldl | Op::Ldr => {
                let d = self.read_width(bus, dword_addr, 8);
                let v = if matches!(op, Op::Ldl) {
                    mem::ldl(rt, d, byte8)
                } else {
                    mem::ldr(rt, d, byte8)
                };
                WriteBack::Gpr { dest, value: v }
            }
            Op::Swl | Op::Swr => {
                let w = self.read_width(bus, word_addr, 4) as u32;
                let merged = if matches!(op, Op::Swl) {
                    mem::swl(rt, w, byte4)
                } else {
                    mem::swr(rt, w, byte4)
                };
                self.write_width(bus, word_addr, 4, u64::from(merged));
                WriteBack::None
            }
            Op::Sdl | Op::Sdr => {
                let d = self.read_width(bus, dword_addr, 8);
                let merged = if matches!(op, Op::Sdl) {
                    mem::sdl(rt, d, byte8)
                } else {
                    mem::sdr(rt, d, byte8)
                };
                self.write_width(bus, dword_addr, 8, merged);
                WriteBack::None
            }
            // `MemOp::Unaligned` is only ever constructed for the eight
            // forms above.
            _ => WriteBack::None,
        })
    }

    /// Which coprocessor an instruction needs, if that unit is disabled.
    ///
    /// `Status.CU` (31:28) is one bit per unit. Two rules that are easy to miss:
    ///
    /// - **COP0 is usable from kernel mode regardless of `CU0`** — otherwise the
    ///   CPU could not run an exception handler before `Status` had been set up,
    ///   which is a chicken-and-egg the hardware does not have. Kernel mode is
    ///   `KSU == 0`, or `EXL`/`ERL` set.
    /// - A **valid but unimplemented** COP1 encoding still checks `CU1`. With
    ///   `CU1` set it must *not* raise here, so that Sprint 3's arithmetic is an
    ///   addition rather than a behaviour change.
    fn unusable_coprocessor(&self, d: Decoded) -> Option<u8> {
        use crate::decode::Op;
        let unit = match d.op {
            Op::Mfc0
            | Op::Dmfc0
            | Op::Mtc0
            | Op::Dmtc0
            | Op::Tlbr
            | Op::Tlbwi
            | Op::Tlbwr
            | Op::Tlbp
            | Op::Eret => 0,
            Op::Cfc1
            | Op::Ctc1
            | Op::Mfc1
            | Op::Dmfc1
            | Op::Mtc1
            | Op::Dmtc1
            | Op::Lwc1
            | Op::Ldc1
            | Op::Swc1
            | Op::Sdc1
            | Op::Cop1Unimplemented
            | Op::Cop1ReservedControl
            // FP arithmetic is a COP1 instruction like any other and must raise
            // Coprocessor Unusable with `CU1` clear. It was omitted when
            // `FpArith` was introduced, which left the arithmetic executing
            // unconditionally -- a program that had not enabled COP1 would get
            // results instead of an exception.
            | Op::FpArith => 1,
            Op::Cop2 | Op::Cop2ReservedControl | Op::Mfc2 | Op::Dmfc2 | Op::Mtc2 => 2,
            _ => return None,
        };
        let status = self.cop0.read(crate::cop0::reg::STATUS);
        if unit == 0 {
            /// `Status.KSU` (4:3) — 0 is kernel, 1 supervisor, 2 user.
            const KSU: u64 = 0b11 << 3;
            /// `Status.EXL` (1) or `Status.ERL` (2): either forces kernel mode
            /// regardless of `KSU`, which is what makes an exception handler's
            /// first instructions safe.
            const EXL_OR_ERL: u64 = 0b110;
            let kernel = status & KSU == 0 || status & EXL_OR_ERL != 0;
            if kernel {
                return None;
            }
        }
        if status & (1 << (28 + u64::from(unit))) == 0 {
            return Some(unit);
        }
        None
    }

    /// `Status.RE` — reverse endian, and **only in User mode** (UM §5.2).
    ///
    /// `EXL`/`ERL` force kernel mode regardless of `KSU`, so an exception
    /// handler reads memory the way it wrote it even with `RE` set. That is the
    /// same rule the coprocessor-usability check applies, for the same reason.
    fn reverse_endian(&self) -> bool {
        /// `Status.RE`, bit 25.
        const RE: u64 = 1 << 25;
        /// `Status.KSU` (4:3) — 2 is user.
        const KSU_USER: u64 = 0b10 << 3;
        /// `Status.KSU` (4:3).
        const KSU: u64 = 0b11 << 3;
        /// `Status.EXL` (1) or `Status.ERL` (2).
        const EXL_OR_ERL: u64 = 0b110;
        let status = self.cop0.read(crate::cop0::reg::STATUS);
        status & RE != 0 && status & KSU == KSU_USER && status & EXL_OR_ERL == 0
    }

    /// The reverse-endian byte-lane swap for an access of `width` bytes.
    ///
    /// Reversing endianness on a 64-bit datapath is a permutation of byte lanes
    /// within the doubleword, expressed as an XOR of the low address bits: a
    /// doubleword access covers the whole lane set and does not move, a word
    /// moves by 4, a halfword by 6 and a byte by 7.
    const fn re_swap(width: u64) -> u32 {
        match width {
            1 => 7,
            2 => 6,
            4 => 4,
            _ => 0,
        }
    }

    /// Translate a data address and apply the reverse-endian swap, if any.
    ///
    /// The swap is applied to the **physical** address, after translation. It
    /// touches only bits 2:0, which every translation maps identically, so this
    /// is exactly equivalent to swapping the virtual address first — and it
    /// keeps `BadVAddr` raw on a fault, which n64-systemtest asserts directly
    /// ("RE unmapped access keeps raw `BadVAddr`").
    ///
    /// # Errors
    ///
    /// A TLB fault on the untransformed address.
    fn translate_re(
        &mut self,
        vaddr: u64,
        store: bool,
        width: u64,
    ) -> Result<crate::addr::Physical, Exception> {
        let mut p = self.translate_data(vaddr, store)?;
        if self.reverse_endian() {
            p.addr ^= Self::re_swap(width);
        }
        Ok(p)
    }

    /// The privilege mode and addressing width the next access runs under.
    ///
    /// `EXL`/`ERL` force Kernel regardless of `KSU` — the same rule the
    /// coprocessor-usability check applies, and what makes an exception
    /// handler's first instructions safe.
    ///
    /// The addressing-width bit is picked **for the resolved mode**, not for
    /// `KSU`: a User-mode program that takes an exception runs the handler in
    /// Kernel mode, and the handler's addresses are governed by `KX`.
    fn access_mode(&self) -> crate::addr::Access {
        /// `Status.KSU`, bits 4:3.
        const KSU: u64 = 0b11 << 3;
        /// `Status.EXL` (1) or `Status.ERL` (2).
        const EXL_OR_ERL: u64 = 0b110;
        let status = self.cop0.read(crate::cop0::reg::STATUS);
        let mode = if status & EXL_OR_ERL != 0 {
            crate::addr::Mode::Kernel
        } else {
            match (status & KSU) >> 3 {
                1 => crate::addr::Mode::Supervisor,
                2 => crate::addr::Mode::User,
                // 0 is Kernel; 3 is not a defined encoding and the manual gives
                // it no behaviour, so it takes the most restrictive-to-reach
                // reading rather than an invented one.
                _ => crate::addr::Mode::Kernel,
            }
        };
        // `Status.UX` (5), `SX` (6), `KX` (7).
        let wide = match mode {
            crate::addr::Mode::User => status & (1 << 5) != 0,
            crate::addr::Mode::Supervisor => status & (1 << 6) != 0,
            crate::addr::Mode::Kernel => status & (1 << 7) != 0,
        };
        crate::addr::Access {
            mode,
            wide,
            erl: self.erl(),
        }
    }

    /// Are the MIPS III 64-bit operations reserved right now?
    ///
    /// Yes in User or Supervisor mode with that mode's `UX`/`SX` bit clear; never
    /// in Kernel mode, whatever `KX` says. Both halves are load-bearing: gating
    /// on the width bit alone would reserve them for a 32-bit kernel, and gating
    /// on the mode alone would reserve them for a 64-bit user program.
    fn sixty_four_bit_is_reserved(&self) -> bool {
        let a = self.access_mode();
        !matches!(a.mode, crate::addr::Mode::Kernel) && !a.wide
    }

    /// `Status.ERL` — the error level, which makes KUSEG unmapped (UM §5.2.2).
    fn erl(&self) -> bool {
        self.cop0.read(crate::cop0::reg::STATUS) & (1 << 2) != 0
    }

    /// Mirror a TLB shutdown into `Status.TS`.
    ///
    /// `TS` is read-only to software (UM Fig. 6-6, p. 167), so it goes through
    /// `set_hardware`. Without this the shutdown flag would be recorded inside
    /// the TLB and never observed, which is worse than not tracking it: software
    /// polls `Status.TS` precisely to discover that the TLB has died.
    fn note_shutdown(&mut self) {
        if self.tlb.is_shutdown() {
            let status = self.cop0.read(crate::cop0::reg::STATUS);
            self.cop0
                .set_hardware(crate::cop0::reg::STATUS, status | (1 << 21));
        }
    }

    /// Perform a memory access.
    ///
    /// # Errors
    ///
    /// [`Exception::AddressError`] when an *aligned* access is misaligned. The
    /// `LWL`/`LWR` family is exempt by construction — being usable at any byte
    /// offset is the entire reason it exists.
    fn access<B: Bus>(&mut self, bus: &mut B, op: MemOp) -> Result<WriteBack, Exception> {
        // TODO(T-11-003): charge the cache-miss cost (8..=9 + M PCycles for a
        // D-cache fill, UM Table 11-1) once `M` is measured -- accuracy-ledger C-1.
        match op {
            MemOp::Load { kind, addr, dest } => {
                if !kind.is_aligned(addr) {
                    self.fault_vaddr = addr;
                    return Err(Exception::AddressError { store: false });
                }
                let phys = self.translate_re(addr, false, kind.width())?;
                let raw = self.read_width(bus, phys, kind.width());
                Ok(WriteBack::Gpr {
                    dest,
                    value: kind.shape(raw),
                })
            }
            MemOp::Store { kind, addr, value } => {
                if !kind.is_aligned(addr) {
                    self.fault_vaddr = addr;
                    return Err(Exception::AddressError { store: true });
                }
                let phys = self.translate_re(addr, true, kind.width())?;
                self.write_width(bus, phys, kind.width(), value);
                Ok(WriteBack::None)
            }
            // Load linked: an ordinary aligned load that also arms the link.
            MemOp::LinkedLoad { kind, addr, dest } => {
                if !kind.is_aligned(addr) {
                    // "If either of the low-order two bits of the address are
                    // not zero, an address error exception takes place" (UM §16
                    // p. 453) -- and the link is NOT armed, because the
                    // instruction did not complete.
                    self.fault_vaddr = addr;
                    return Err(Exception::AddressError { store: false });
                }
                let phys = self.translate_re(addr, false, kind.width())?;
                let raw = self.read_width(bus, phys, kind.width());
                self.ll_bit = true;
                // "the value with the high-order four bits of the physical
                // address PA(31:4) ... zero-extended" (UM Figure 5-17). Written
                // via `set_hardware` because LLAddr is software-writable too:
                // this is the hardware side effect, not an MTC0.
                self.cop0
                    .set_hardware(crate::cop0::reg::LL_ADDR, u64::from(phys.addr >> 4));
                Ok(WriteBack::Gpr {
                    dest,
                    value: kind.shape(raw),
                })
            }
            // Store conditional: the store is conditional, the flag write is not.
            MemOp::ConditionalStore {
                kind,
                addr,
                value,
                dest,
            } => {
                if !kind.is_aligned(addr) {
                    // "If this instruction both fails and causes an exception,
                    // the exception takes precedence" (UM §16 p. 487) -- so the
                    // address check runs before the link bit is even consulted,
                    // and `dest` is left alone.
                    self.fault_vaddr = addr;
                    return Err(Exception::AddressError { store: true });
                }
                let stored = self.ll_bit;
                if stored {
                    let phys = self.translate_re(addr, true, kind.width())?;
                    self.write_width(bus, phys, kind.width(), value);
                }
                // Written whether or not the store happened. Note the link bit
                // is deliberately NOT cleared here -- see `Pipeline::ll_bit`.
                Ok(WriteBack::Gpr {
                    dest,
                    value: u64::from(stored),
                })
            }
            // FP loads and stores. Same alignment and translation rules as the
            // integer forms -- only the destination register file differs.
            MemOp::Fp { op, addr, ft } => {
                use crate::decode::Op;
                let double = matches!(op, Op::Ldc1 | Op::Sdc1);
                let store = matches!(op, Op::Swc1 | Op::Sdc1);
                let align = if double { 7 } else { 3 };
                if addr & align != 0 {
                    self.fault_vaddr = addr;
                    return Err(Exception::AddressError { store });
                }
                let phys = self.translate_re(addr, store, if double { 8 } else { 4 })?;
                // `Status.FR` selects the register-file view; a double under
                // FR = 0 occupies an FGR pair.
                let fr = fr_of(&self.cop0);
                match op {
                    Op::Lwc1 => {
                        let v = self.read_width(bus, phys, 4) as u32;
                        self.fpr.write_s(ft, fr_of(&self.cop0), v);
                    }
                    Op::Ldc1 => {
                        let v = self.read_width(bus, phys, 8);
                        self.fpr.write_d(ft, fr, v);
                    }
                    Op::Swc1 => {
                        let v = self.fpr.read_s(ft, fr_of(&self.cop0));
                        self.write_width(bus, phys, 4, u64::from(v));
                    }
                    _ => {
                        let v = self.fpr.read_d(ft, fr);
                        self.write_width(bus, phys, 8, v);
                    }
                }
                Ok(WriteBack::None)
            }
            MemOp::Cache { addr, op } => self.cache_op(bus, addr, op),
            // The unaligned family accesses the ALIGNED container holding `addr`
            // and merges, so it can never raise an address error -- but it CAN
            // still raise a TLB fault, which is why it is fallible.
            MemOp::Unaligned { op, addr, rt, dest } => {
                self.access_unaligned(bus, op, addr, rt, dest)
            }
        }
    }

    /// `CACHE`: dispatch to the index- or address-addressed half.
    ///
    /// Only the ADDRESS-addressed operations translate. `op4..2`
    /// (UM Ch. 16, p. 404):
    ///
    /// - `0..=2` `Index_Invalidate` / `Index_Load_Tag` / `Index_Store_Tag` —
    ///   address the cache "at the index specified", so they never consult the
    ///   TLB and cannot fault.
    /// - `3` `Create_Dirty_Exclusive` — "set the cache block tag to the
    ///   specified physical address", so it does.
    /// - `4..=6` `Hit_*` — "if the cache block contains the specified address",
    ///   so they do.
    ///
    /// Translating unconditionally raises spurious TLB refills on `Index_*` ops
    /// against unmapped addresses, which is exactly what cache-init code does at
    /// boot: walk every index with an arbitrary base. An earlier revision of
    /// this comment described the distinction while the code ignored it.
    ///
    /// # Errors
    ///
    /// A TLB fault, on the address-addressed operations only.
    fn cache_op<B: Bus>(&mut self, bus: &mut B, addr: u64, op: u8) -> Result<WriteBack, Exception> {
        if (op >> 2) >= 3 {
            let p = self.translate_data(addr, false)?;
            self.cache_hit_op(bus, op, p.addr);
        } else {
            // An `Index_*` op indexes on the raw VIRTUAL address. That is not a
            // shortcut around the untranslated case: the index lives in bits
            // 13:5 (I) / 12:4 (D), and every segment's translation leaves those
            // bits alone.
            self.cache_index_op(bus, op, addr as u32);
        }
        Ok(WriteBack::None)
    }

    /// The index-addressed half of `CACHE` (`op4..2` in `0..=2`).
    ///
    /// `addr` is the *virtual* address: these operations select a line by index
    /// and never translate. Bit 0 of `op` picks the cache.
    fn cache_index_op<B: Bus>(&mut self, bus: &mut B, op: u8, addr: u32) {
        let dcache = op & 1 == 1;
        match (op >> 2, dcache) {
            // Index_Invalidate (I) / Index_WriteBack_Invalidate (D).
            //
            // Both clear the valid bit and LEAVE THE TAG in place, which
            // `Index_Load_Tag` then reports. Clearing the tag as well would look
            // tidier and would be wrong: n64-systemtest asserts the PFN is
            // unchanged across an invalidate.
            (0, false) => self.icache.invalidate_index(addr),
            (0, true) => {
                if let Some(w) = self.dcache.flush_index(addr, true, false) {
                    Self::push_line(bus, w.addr, &w.data);
                }
            }
            // Index_Load_Tag -> COP0 TagLo. Written with `set_hardware` because
            // this is the hardware side effect of CACHE, not an MTC0, and the
            // register's software write mask must not apply to it.
            (1, false) => {
                let t = self.icache.load_tag(addr);
                self.cop0
                    .set_hardware(crate::cop0::reg::TAG_LO, u64::from(t));
            }
            (1, true) => {
                let t = self.dcache.load_tag(addr);
                self.cop0
                    .set_hardware(crate::cop0::reg::TAG_LO, u64::from(t));
            }
            // Index_Store_Tag <- COP0 TagLo.
            (2, false) => {
                let t = self.cop0.read(crate::cop0::reg::TAG_LO) as u32;
                self.icache.store_tag(addr, t);
            }
            (2, true) => {
                let t = self.cop0.read(crate::cop0::reg::TAG_LO) as u32;
                self.dcache.store_tag(addr, t);
            }
            _ => {}
        }
    }

    /// The address-addressed half of `CACHE` (`op4..2` in `3..=6`).
    ///
    /// `addr` is PHYSICAL — the caller has already translated it, which is what
    /// makes a TLB fault on these operations possible.
    fn cache_hit_op<B: Bus>(&mut self, bus: &mut B, op: u8, addr: u32) {
        let dcache = op & 1 == 1;
        match (op >> 2, dcache) {
            // Create_Dirty_Exclusive (D only): claim the line without a fill.
            (3, true) => {
                if let Some(w) = self.dcache.create_dirty_exclusive(addr) {
                    Self::push_line(bus, w.addr, &w.data);
                }
            }
            // Hit_Invalidate: only if the line is actually resident.
            (4, false) => self.icache.hit_invalidate(addr),
            (4, true) => {
                if self.dcache.hits(addr) {
                    self.dcache.flush_index(addr, true, false);
                }
            }
            // Fill (I) — an unconditional line fill, no hit test.
            (5, false) => self.icache_fill(bus, addr),
            // Hit_WriteBack_Invalidate (D).
            (5, true) => {
                if self.dcache.hits(addr)
                    && let Some(w) = self.dcache.flush_index(addr, true, false)
                {
                    Self::push_line(bus, w.addr, &w.data);
                }
            }
            // Hit_WriteBack: the line stays resident, and stops being dirty.
            (6, false) => {
                if let Some(w) = self.icache.flush_hit(addr) {
                    Self::push_line(bus, w.addr, &w.data);
                }
            }
            (6, true) => {
                if self.dcache.hits(addr)
                    && let Some(w) = self.dcache.flush_index(addr, false, true)
                {
                    Self::push_line(bus, w.addr, &w.data);
                }
            }
            _ => {}
        }
    }

    /// Make the I-cache line covering `addr` resident.
    fn icache_fill<B: Bus>(&mut self, bus: &mut B, addr: u32) {
        let base = addr & !(crate::cache::ICACHE_LINE - 1);
        let mut data = [0u8; 32];
        Self::pull_line(bus, base, &mut data);
        self.icache.install(base, data);
    }

    /// Make the D-cache line covering `addr` resident, writing back whatever it
    /// evicts.
    fn dcache_fill<B: Bus>(&mut self, bus: &mut B, addr: u32) {
        let Some(evicted) = self.dcache.miss_plan(addr) else {
            return;
        };
        if let Some(w) = evicted {
            Self::push_line(bus, w.addr, &w.data);
        }
        let base = addr & !(crate::cache::DCACHE_LINE - 1);
        let mut data = [0u8; 16];
        Self::pull_line(bus, base, &mut data);
        self.dcache.install(base, data);
    }

    /// Write a whole cache line out to the bus, a word at a time.
    ///
    /// Word-wide rather than byte-wide because [`Bus::read_u32`]/[`Bus::write_u32`]
    /// are the calls `rustyn64-core` gives a fast RDRAM path; a byte loop would
    /// issue four times the bus traffic on every fill and eviction, and a line
    /// is always naturally aligned so the split is exact.
    fn push_line<B: Bus>(bus: &mut B, addr: u32, data: &[u8]) {
        for (k, w) in data.chunks_exact(4).enumerate() {
            let word = u32::from_be_bytes([w[0], w[1], w[2], w[3]]);
            bus.write_u32(addr.wrapping_add(k as u32 * 4), word);
        }
    }

    /// Read a whole cache line from the bus into `data`.
    fn pull_line<B: Bus>(bus: &mut B, addr: u32, data: &mut [u8]) {
        for (k, w) in data.chunks_exact_mut(4).enumerate() {
            let word = bus.read_u32(addr.wrapping_add(k as u32 * 4));
            w.copy_from_slice(&word.to_be_bytes());
        }
    }

    /// Read `width` big-endian bytes, right-justified, through the D-cache when
    /// the access is cached.
    ///
    /// Dispatches on width so 4- and 8-byte *uncached* accesses go through
    /// [`Bus::read_u32`], which `rustyn64-core` overrides with a fast RDRAM path.
    /// A byte loop would issue 4-8x more bus calls on the *most common*
    /// operations, and memory access is the hot path for a core targeting full
    /// speed (`docs/performance.md`).
    ///
    /// Alignment is **not** rechecked here. `access` has already validated it
    /// against the specific [`crate::mem::LoadKind`]/[`crate::mem::StoreKind`],
    /// and the unaligned family passes an address it has aligned down itself.
    /// Duplicating the check would put the rule in two places, where it can drift.
    fn read_width<B: Bus>(&mut self, bus: &mut B, p: crate::addr::Physical, width: u64) -> u64 {
        let addr = p.addr;
        if p.cached == crate::addr::Cached::Yes {
            self.dcache_fill(bus, addr);
            return self.dcache.read(addr, width as usize);
        }
        match width {
            1 => u64::from(bus.read_u8(addr)),
            2 => (u64::from(bus.read_u8(addr)) << 8) | u64::from(bus.read_u8(addr.wrapping_add(1))),
            4 => u64::from(bus.read_u32(addr)),
            // Big-endian: the high word is at the lower address.
            8 => {
                (u64::from(bus.read_u32(addr)) << 32)
                    | u64::from(bus.read_u32(addr.wrapping_add(4)))
            }
            _ => 0,
        }
    }

    /// Write the low `width` big-endian bytes of `value`, through the D-cache
    /// when the access is cached.
    ///
    /// A cached store is a **write-allocate**: the line is filled first, then
    /// modified and left dirty. The VR4300's D-cache has no write-around path,
    /// so a partial store to an absent line must read the rest of it from memory
    /// or the eventual write-back would push out fifteen bytes of nothing.
    ///
    /// Width-dispatched for the same reason as [`Pipeline::read_width`].
    fn write_width<B: Bus>(
        &mut self,
        bus: &mut B,
        p: crate::addr::Physical,
        width: u64,
        value: u64,
    ) {
        let addr = p.addr;
        if p.cached == crate::addr::Cached::Yes {
            self.dcache_fill(bus, addr);
            self.dcache.write(addr, width as usize, value);
            return;
        }
        match width {
            1 => bus.write_u8(addr, value as u8),
            2 => {
                bus.write_u8(addr, (value >> 8) as u8);
                bus.write_u8(addr.wrapping_add(1), value as u8);
            }
            4 => bus.write_u32(addr, value as u32),
            8 => {
                bus.write_u32(addr, (value >> 32) as u32);
                bus.write_u32(addr.wrapping_add(4), value as u32);
            }
            _ => {}
        }
    }

    /// `EX` — execute.
    fn ex_stage(&mut self, regs: &Regs, next_pc: &mut u64) {
        let mut out = self.rf_ex;
        if out.occupied && out.abort.is_none() {
            // Resolve operands through the bypass network rather than trusting
            // the values latched at RF, which may be one cycle stale.
            out.rs_val = self.bypass(out.decoded.rs, regs);
            out.rt_val = self.bypass(out.decoded.rt, regs);
            let hilo = self.bypass_hi_lo(regs);
            // Coprocessor usability is checked BEFORE execution, in EX (UM
            // §4.7.5 lists CPU among the EX-stage exceptions). COP0 is exempt in
            // kernel mode regardless of `CU0`, which is why the CPU can run
            // exception handlers before any `Status` setup has happened.
            if let Some(unit) = self.unusable_coprocessor(out.decoded) {
                self.abort_from(Stage::Ex, Exception::CoprocessorUnusable { unit });
                out = self.rf_ex;
                self.rf_ex.occupied = false;
                self.ex_dc = out;
                return;
            }
            // A 64-bit operation is RESERVED in 32-bit User or Supervisor mode.
            // Kernel may use them at any width, which is why this cannot be a
            // property of `Status.KX` alone.
            //
            // Checked after coprocessor usability, not before: an unusable
            // coprocessor is reported as such even when the encoding is also a
            // 64-bit one, and the suite distinguishes the two causes.
            if out.decoded.op.is_64_bit() && self.sixty_four_bit_is_reserved() {
                self.abort_from(Stage::Ex, Exception::ReservedInstruction);
                out = self.rf_ex;
                self.rf_ex.occupied = false;
                self.ex_dc = out;
                return;
            }
            // `DCFC1`/`DCTC1` are usable-but-unimplemented: the `CU1` check
            // above has already passed, so this is the *other* outcome. The
            // whole `Cause` field is replaced, leaving only bit 17 -- the suite
            // pre-loads unrelated cause bits specifically to check they clear.
            // COP2's equivalent declines differently: Reserved Instruction,
            // and `FCSR` is not involved at all.
            if out.decoded.op == crate::decode::Op::Cop2ReservedControl {
                self.abort_from(Stage::Ex, Exception::CoprocessorReserved { unit: 2 });
                out = self.rf_ex;
                self.rf_ex.occupied = false;
                self.ex_dc = out;
                return;
            }
            if out.decoded.op == crate::decode::Op::Cop1ReservedControl {
                /// `FCSR.Cause`, bits 17:12.
                const CAUSE_MASK: u32 = 0x3F << 12;
                let fcsr = self.cop1.fcsr();
                self.cop1
                    .ctc1(31, (fcsr & !CAUSE_MASK) | crate::fpu::CAUSE_UNIMPLEMENTED);
                self.abort_from(Stage::Ex, Exception::FloatingPoint);
                out = self.rf_ex;
                self.rf_ex.occupied = false;
                self.ex_dc = out;
                return;
            }
            match execute(out.decoded, out.rs_val, out.rt_val, hilo, out.pc) {
                Ok(e) => {
                    out.write_back = e.write_back;
                    out.mem = e.mem;
                    out.cop0 = e.cop0;
                    // Control flow. The delay slot has ALREADY been fetched -- it
                    // is in `ic_rf` right now, because IC ran a cycle ahead. That
                    // is the architectural delay slot, not a modelling artefact.
                    //
                    // Because the cascade runs backwards, `ic_stage` executes
                    // AFTER this in the same cycle, so writing `next_pc` here
                    // makes the very next fetch land on the target with exactly
                    // one delay slot in between. No wrong-path fetch needs
                    // squashing -- that falls out of the reverse order rather
                    // than being arranged.
                    // **The link is `next_pc`, read BEFORE this instruction's
                    // own redirect is applied.**
                    //
                    // At `EX` time `next_pc` already holds the address of the
                    // instruction that will run after this one's delay slot:
                    // `pc + 8` for an ordinary jump, and the OUTER target `+ 4`
                    // when this jump is itself in a delay slot, because the
                    // outer jump redirected a cycle earlier and `IC` has since
                    // advanced past it.
                    //
                    // Computing `pc + 8` in `execute` was right for the
                    // ordinary case and silently wrong for the nested one; the
                    // live `next_pc` is that address by construction rather
                    // than by a second formula that can disagree.
                    // **COP2 is one 64-bit latch, not a register file.** The
                    // register index is ignored entirely: n64-systemtest writes with
                    // one index and reads back with several others, including 30 and
                    // 31, and gets the same value every time. `MTC2` writes all 64
                    // bits despite being nominally a 32-bit move; `MFC2` returns the
                    // low half sign-extended and `DMFC2` the whole thing.
                    //
                    // The same shape as the reserved COP0 registers (ledger C-15) --
                    // this processor's answer to "a coprocessor that is not really
                    // there" is a single latch, twice over.
                    match out.decoded.op {
                        crate::decode::Op::Mtc2 => self.cop2_latch = out.rt_val,
                        crate::decode::Op::Mfc2 => {
                            out.write_back = WriteBack::Gpr {
                                dest: out.decoded.dest,
                                value: crate::alu::sext32(self.cop2_latch as u32),
                            };
                        }
                        crate::decode::Op::Dmfc2 => {
                            out.write_back = WriteBack::Gpr {
                                dest: out.decoded.dest,
                                value: self.cop2_latch,
                            };
                        }
                        _ => {}
                    }
                    if let Some(dest) = e.link {
                        out.write_back = WriteBack::Gpr {
                            dest,
                            value: *next_pc,
                        };
                    }
                    if let Some(r) = e.redirect {
                        *next_pc = r.target;
                        if r.nullify_delay_slot {
                            // A branch-LIKELY that was not taken squashes its
                            // already-fetched delay slot. An ordinary branch
                            // never does.
                            self.ic_rf = Latch::default();
                        }
                    }
                    // Multiply and divide stall the ENTIRE pipeline for the
                    // documented count (UM Table 3-12), so the request is raised
                    // here and honoured from the next cycle onward.
                    if e.stall_cycles > 0 {
                        self.stall_for(e.stall_cycles, Interlock::Mci);
                    }
                    // ERET. Resolved here rather than in `execute` because its
                    // target comes out of COP0, not out of the instruction.
                    //
                    // It has NO delay slot (UM Ch. 16, p. 434) -- alone among
                    // the control transfers -- so the instruction IC already
                    // fetched must be squashed. Every branch reaches this point
                    // with its delay slot legitimately in flight, which is why
                    // the squash is spelled out here instead of falling out of
                    // the reverse cascade as a branch's does.
                    if matches!(e.cop0, Some(Cop0Access::Eret)) {
                        *next_pc = exception::eret(&mut self.cop0);
                        // "cleared by an ERET" (UM §3.1) -- the other half of
                        // the LL/SC contract, which had nothing clearing it
                        // until now.
                        self.ll_bit = false;
                        self.ic_rf = Latch::default();
                    }
                }
                // Stamp BEFORE the latch move, so the abort travels with the
                // instruction that caused it -- see `abort_from`.
                Err(exc) => {
                    self.abort_from(Stage::Ex, exc);
                    out = self.rf_ex;
                }
            }
        }
        self.ex_dc = out;
        self.rf_ex.occupied = false;
    }

    /// `RF` — register fetch, and where the load interlock is detected.
    fn rf_stage(&mut self, regs: &Regs) {
        let mut out = self.ic_rf;
        if out.occupied {
            out.rs_val = regs.read(out.decoded.rs);
            out.rt_val = regs.read(out.decoded.rt);
        }
        // These reads are a first approximation: EX re-resolves them through the
        // bypass network, since a producer one instruction ahead has not
        // committed yet. RF still performs the read because that is where the
        // load interlock is detected (T-11-003).
        //
        // TODO(T-11-002): the MFHI/MFLO hazard window -- a MFHI followed within
        // two instructions by a HI write reads hardware's WRONG value, and that
        // is non-interlocked (alu::MFHI_MFLO_HAZARD_INSTRUCTIONS).
        // The load-delay interlock (UM §4.6.5). A load's result is not ready in
        // time to bypass, so if the NEXT instruction names the loaded register
        // the pipeline stalls one cycle. The detection is deliberately imprecise,
        // matching hardware -- see `load_interlocks`.
        //
        // Compare against `ex_dc`, not `rf_ex`. In the reverse cascade `EX` runs
        // before `RF`, so by now the instruction that was in `EX` this cycle has
        // already moved into `ex_dc` and `rf_ex` has been vacated. Checking
        // `rf_ex` here silently never fires -- which is exactly what it did
        // before `a_load_followed_by_its_use_interlocks...` caught it.
        if out.occupied
            && self.ex_dc.occupied
            && self.ex_dc.decoded.is_load()
            && load_interlocks(
                self.ex_dc.decoded.dest,
                out.decoded.rs,
                out.decoded.rt,
                self.ex_dc.decoded.targets_fpr() == out.decoded.targets_fpr(),
            )
        {
            self.stall_for(1, Interlock::Ldi);
        }
        self.rf_ex = out;
        self.ic_rf.occupied = false;
    }

    /// `IC` — instruction-cache fetch, and where the delay-slot flag is set.
    fn ic_stage<B: Bus>(&mut self, bus: &mut B, next_pc: &mut u64) {
        // An abort raised earlier this cycle flushes younger instructions. The
        // fetch happening now is younger than all of them, so it must not become
        // a live instruction -- otherwise it escapes the flush entirely and
        // executes down the wrong path.
        //
        // TODO(T-11-002): redirect `next_pc` to the exception vector instead of
        // bubbling. Until the vector exists, a bubble is the honest behaviour:
        // it declines to execute rather than executing the wrong thing.
        if self.flush_pending {
            self.ic_rf = Latch::default();
            return;
        }
        let pc = *next_pc;

        // Computed BEFORE the alignment check, so a faulting fetch carries the
        // right delay-slot flag into its latch and therefore into `Cause.BD`.
        // Depends only on `rf_ex`, never on the fetch, so hoisting it is safe.
        //
        // Check `rf_ex`, not `ic_rf`: `rf_stage` runs immediately before this in
        // the reverse cascade and has already moved the previous instruction out
        // of `ic_rf`, so a branch fetched last cycle is in `rf_ex` by now.
        // Reading `ic_rf` here makes the flag silently always false.
        let in_delay_slot = self.rf_ex.occupied && self.rf_ex.decoded.op.has_delay_slot();

        // An instruction fetch must be word-aligned. An unaligned PC raises an
        // address error (AdEL) rather than fetching -- so the bus is NOT touched,
        // because the access itself is what is invalid.
        //
        // Not reachable from straight-line execution, which advances by 4 from an
        // aligned reset vector. It becomes reachable with the jump and branch
        // family (T-11-004), where a computed target can be unaligned, and it is
        // already reachable through the public `Cpu::set_pc` the golden-log
        // harness uses.
        if !pc.is_multiple_of(4) {
            // Populate the latch BEFORE raising. `abort_with` captures the
            // faulting instruction's context out of the latch its stage reads,
            // which for `Stage::Ic` is `ic_rf` -- so raising first would capture
            // the PREVIOUS fetch's `pc` and delay-slot flag and write a wrong
            // `EPC`. Stamp before you move, and populate before you stamp.
            self.ic_rf = Latch {
                cop0: None,
                occupied: true,
                pc,
                in_delay_slot,
                abort: Some(Exception::AddressError { store: false }),
                ..Latch::default()
            };
            self.abort_with(Stage::Ic, Exception::AddressError { store: false }, pc);
            // `next_pc` is deliberately NOT realigned. Rounding it down would
            // silently "fix" the faulting address and let execution continue on a
            // path hardware never takes -- turning a raised exception into a
            // wrong answer. The redirect to the exception vector happens in
            // `advance`, after the cascade (T-12-002).
            return;
        }

        // The fetch itself now goes through the I-cache (see below); what is
        // still outstanding is the COST.
        // TODO(T-11-003): charge the I-cache miss cost (14..=15 + M PCycles, UM
        // Table 11-2) once `M` is measured -- accuracy-ledger C-1.
        // Every address handed to the Bus is PHYSICAL (`docs/cpu.md`); the
        // segment map is applied here, in the CPU, not by the Bus.
        // Instruction fetch goes through the micro-ITLB in front of the JTLB
        // (UM §1.5.1). A micro-TLB miss is a STALL of 3 PCycles (UM §4.6.2); a
        // JTLB miss is an exception. Only the mapped segments involve either --
        // KSEG0/KSEG1 fetches, which is all of early boot, bypass both.
        let asid = (self.cop0.read(crate::cop0::reg::ENTRY_HI) & 0xFF) as u8;
        let (phys, cached) = match crate::addr::segment(pc, self.access_mode()) {
            crate::addr::Segment::Direct { addr, cached } => (addr, cached),
            // Not a valid address in this mode: an address error, raised without
            // consulting the TLB at all.
            crate::addr::Segment::Invalid => {
                let exc = Exception::AddressError { store: false };
                self.ic_rf = Latch {
                    cop0: None,
                    occupied: true,
                    pc,
                    in_delay_slot,
                    abort: Some(exc),
                    ..Latch::default()
                };
                self.abort_with(Stage::Ic, exc, pc);
                return;
            }
            crate::addr::Segment::Mapped => {
                // The 3-PCycle penalty is "incurred when the micro-TLB is
                // updated from the JTLB" (UM §4.6.2) -- so it is charged only
                // when a reload can actually happen. A fetch that misses BOTH
                // levels goes straight to its exception without paying for a
                // reload that never occurred.
                if !self.tlb.itlb_probe(pc, asid) && self.tlb.jtlb_has_match(pc, asid) {
                    self.tlb.itlb_fill(pc, asid);
                    self.stall_for(crate::tlb::ITLB_MISS_PCYCLES, Interlock::Itm);
                }
                match self.tlb.lookup(pc, asid, false) {
                    Ok(t) => (
                        t.addr,
                        if t.uncached {
                            crate::addr::Cached::No
                        } else {
                            crate::addr::Cached::Yes
                        },
                    ),
                    Err(f) => {
                        // An instruction fetch is a load, so TLBL never TLBS.
                        let exc = Self::tlb_exception(f, false);
                        self.ic_rf = Latch {
                            cop0: None,
                            occupied: true,
                            pc,
                            in_delay_slot,
                            abort: Some(exc),
                            ..Latch::default()
                        };
                        self.abort_with(Stage::Ic, exc, pc);
                        return;
                    }
                }
            }
        };
        // Instruction fetch is a 4-byte access, so `Status.RE` swaps it within
        // its doubleword exactly as it swaps a `LW` -- which is why the test ROM
        // has to emit its reverse-endian programs with each instruction PAIR
        // exchanged for them to execute in order.
        let phys = if self.reverse_endian() {
            phys ^ Self::re_swap(4)
        } else {
            phys
        };
        // Fetch through the I-cache when the segment is cached. This is what
        // makes an uncached patch to already-fetched code invisible until a
        // CACHE invalidate -- the behaviour n64-systemtest's ICACHE group
        // asserts, and the reason the cache is modelled at all.
        let word = if cached == crate::addr::Cached::Yes {
            if !self.icache.hits(phys) {
                self.icache_fill(bus, phys);
            }
            self.icache.read_word(phys)
        } else {
            bus.read_u32(phys)
        };
        // Decode here rather than at RF: a branch must be decoded before the
        // NEXT fetch, so that fetch can be marked as its delay slot.
        //
        // A branch decoded last cycle is in `rf_ex` by now, so the instruction
        // being fetched here is its delay slot -- see `in_delay_slot` above,
        // computed once at the top of the stage and used by both paths.
        self.ic_rf = Latch {
            occupied: true,
            pc,
            word,
            in_delay_slot,
            abort: None,
            decoded: decode(word),
            rs_val: 0,
            rt_val: 0,
            write_back: WriteBack::None,
            mem: None,
            cop0: None,
        };
        *next_pc = pc.wrapping_add(4);
    }

    /// Perform a COP1 arithmetic operation against the FPR file.
    ///
    /// Lives here rather than in `exec::execute` because it reads two FPRs and
    /// writes a third, and `execute` has no access to the register file — the
    /// same reason the COP1 moves are split this way.
    ///
    /// # Register access goes through the `FR` view
    ///
    /// Operands are read with [`Fpr::read_s`]/[`Fpr::read_d`], **not**
    /// `read_raw`. Using the raw register was ledger U-7's bug: with `FR = 0` a
    /// double lives across an FGR *pair*, and a raw read returns half of it.
    ///
    /// # `FCSR`
    ///
    /// `Cause` is bits **17:12** and reports what *this* operation raised; it
    /// is replaced wholesale each time. `Flags` (6:2) is the sticky
    /// accumulation and is OR-ed in. `Flags::to_fcsr_bits` produces both, so
    /// clearing only `Cause` before OR-ing preserves the sticky half.
    ///
    /// **The field is 17:12, not 16:12.** Bit 17 is `Cause.E`, Unimplemented
    /// Operation — part of `Cause` despite having no `Enable` bit and no sticky
    /// `Flags` twin, which means the mask is the *only* thing that ever clears
    /// it. This comment said 16:12 while `CAUSE_MASK` covered 16:12 too, and
    /// the result was a bit that could never be cleared once raised. Only the
    /// five *maskable* conditions live in 16:12; that narrower range is what
    /// the enable comparison below uses, and it is a different statement.
    ///
    /// # Enabled traps
    ///
    /// A condition whose `FCSR.Enable` bit is set raises
    /// [`Exception::FloatingPoint`] instead of completing, and **three** things
    /// then differ from the untrapped path. All three are architectural, and
    /// each is separately observable by n64-systemtest:
    ///
    /// 1. **`fd` is not written.** The trap is precise, so the destination keeps
    ///    its old value — which is what the suite checks with its
    ///    `Result after operation (with exception)` assertion.
    /// 2. **The sticky `Flags` field is not updated.** Only `Cause` is. This is
    ///    easy to get wrong because the untrapped path sets both from the same
    ///    helper, and a trapped operation that also OR-ed into `Flags` looks
    ///    right in every test that does not read `FCSR` back.
    /// 3. **The instruction does not retire**, so it must not tick `Random`.
    ///
    /// Returns `true` when it trapped, so `wb_stage` can skip its retirement
    /// tail.
    ///
    /// # Still not handled
    ///
    /// The **unimplemented-operation** cause (bit 17) is unmaskable and is not
    /// produced by the arithmetic here — the VR4300 raises it for subnormal
    /// operands and results, which this FPU computes normally instead. That is
    /// a separate body of work from the maskable enables, and the suite's
    /// `expected_unimplemented` cases still fail.
    fn fp_arith(&mut self, fmt: u8, funct: u8, ft: u8, fs: u8, fd: u8) -> bool {
        use crate::fpu;
        /// `FCSR` Cause field, bits **17:12** — replaced wholesale per
        /// operation.
        ///
        /// The range includes bit 17, `Unimplemented Operation`, which is part
        /// of `Cause` even though it is not an IEEE exception and has no
        /// corresponding `Enable` or sticky `Flags` bit. Masking only 16:12 —
        /// which this did — leaves a *stale* bit 17 set forever, because no
        /// later operation can clear a bit the mask does not cover. Software
        /// reading `FCSR` after a successful conversion would then still see
        /// the previous unimplemented operation.
        const CAUSE_MASK: u32 = 0x3F << 12;
        /// `FCSR.C`, the compare condition — bit 23, above the Cause field and
        /// written only by `C.cond.fmt`.
        const FCSR_C: u32 = 1 << 23;

        let fr = fr_of(&self.cop0);
        // `fmt` is 16 (single) or 17 (double) -- decode admits no other value
        // into `FpArith`, so this is a two-way split, not a table.
        // `funct` 5/6/7 read only `fs`, so they are handled ahead of the
        // arithmetic split and `ft` is never read for an instruction whose
        // `ft` field is architecturally zero.
        //
        // **`MOV` (funct 6) alone is the pure bit move.** `ABS` (5) and `NEG`
        // (7) look like sign flips and are not: they classify their operand,
        // raising Invalid on a signalling NaN and unimplemented-operation on a
        // subnormal or an MSB-clear NaN, and they REPLACE the `Cause` field.
        // An earlier version of this comment described all three as raising
        // nothing, which was true of `MOV` and never of its neighbours.
        //
        // n64-systemtest settles which is which by construction rather than by
        // description: `MOV.S` is driven through
        // `test_floating_point_f32_which_preserves_cause_bits`, while `ABS.S`
        // and `NEG.S` go through the ordinary `test_floating_point_f32`, which
        // asserts `Cause` was cleared. Treating all three alike was worth 52
        // assertions.
        if matches!(funct, 5 | 7) {
            return self.fp_sign_op(fmt, funct, fs, fd, fr);
        }
        if funct == 6 {
            // **`MOV.S` moves all 64 bits, not just the formatted half.**
            // n64-systemtest's "Upper bits of 32 bit operation" reads the
            // destination back with `DMFC1` after a `MOV.S` and expects the
            // SOURCE's upper half there, not the destination's previous
            // contents -- so this is a whole-register transfer that happens to
            // be spelled `.S`.
            let v = self.fpr.read_d_fs(fs, fr);
            self.fpr.write_d_arith(fd, fr, v);
            // **`FCSR` is left completely alone**, `Cause` included.
            //
            // Clearing `Cause` here was written first, on no evidence, and was
            // measurably wrong: the compiler emits `MOV.fmt` to move an FP
            // return value, so a `MOV` sitting between an arithmetic operation
            // and the `CFC1` that reads its result wiped the very `Cause` bits
            // the program was about to inspect. n64-systemtest saw
            // `flags: inexact` with `causes: ""` — the sticky half surviving
            // and the per-operation half erased — which is the signature of a
            // later instruction overwriting it, not of a flag never set.
            //
            // The architectural rule is that `Cause` is written by operations
            // that *can* raise. These cannot, so they write nothing.
            return false;
        }

        // `FCSR.RM` is read **here**, per operation, rather than being captured
        // anywhere earlier: software changes it between instructions, and
        // n64-systemtest sweeps all four modes over the same operand pair.
        let mode = fpu::Rounding::from_rm(self.cop1.rounding_mode());

        // Computed but **not committed**. Whether the write happens depends on
        // the enables, and they cannot be consulted until the flags are known.
        // Writing inside a branch and undoing it afterwards would be wrong
        // under `FR = 0`, where a `.S` write can disturb a neighbouring
        // register's half.
        let (commit, flags, unimplemented) = match funct {
            0o00..=0o03 => self.fp_binary(fmt, funct, ft, fs, fr, mode),
            0o04 => self.fp_sqrt(fmt, fs, fr, mode),
            0o10..=0o17 => self.fp_to_integer(fmt, funct, fs, fr),
            0o40 | 0o41 | 0o44 | 0o45 => self.fp_convert(fmt, funct, fs, fr, mode),
            // 0o60..=0o77 -- `C.cond.fmt`. The low four bits ARE the condition
            // (UM Table 7-11), so the sixteen mnemonics need no table.
            _ => self.fp_compare(fmt, funct & 0xF, ft, fs, fr),
        };

        let raised = flags.to_fcsr_bits()
            | if unimplemented {
                fpu::CAUSE_UNIMPLEMENTED
            } else {
                0
            };
        let fcsr = self.cop1.fcsr();

        // `Cause` bits 16:12 and the `Enable` field bits 11:7 hold the five
        // conditions in the SAME order, so shifting `Cause` down by 12 lines it
        // up with what `Cop1Control::enables` returns. Comparing them in
        // different orders is a silent mis-map that only shows up on whichever
        // condition happens to be tested first.
        //
        // **Unimplemented Operation (bit 17) is unmaskable** and sits above
        // that field, so it is checked separately rather than being folded into
        // the enable comparison — where it would have been silently ignored,
        // since no enable bit corresponds to it.
        if unimplemented || (raised >> 12) & self.cop1.enables() != 0 {
            // Cause only. The sticky `Flags` field is deliberately left
            // untouched — see the doc comment.
            self.cop1
                .ctc1(31, (fcsr & !CAUSE_MASK) | (raised & CAUSE_MASK));
            self.abort_from(Stage::Wb, Exception::FloatingPoint);
            return true;
        }

        match commit {
            // Preserves the upper half, as `MTC1` does. Writing the full
            // register (`write_raw`, zeroing the upper half) was tried and
            // REVERTED: it moved the oracle by nothing and it bypasses the `FR`
            // view, which is exactly the mistake ledger U-7 records (C-10).
            FpCommit::Single(v) => self.fpr.write_s_arith(fd, fr, v),
            FpCommit::Double(v) => self.fpr.write_d_arith(fd, fr, v),
            // `FCSR.C` is bit 23, and it is NOT part of the `Cause`/`Flags`
            // bookkeeping — a compare writes it and no other operation touches
            // it. Confirmed against n64-systemtest's own `FCSR` bitfield rather
            // than inferred.
            FpCommit::Condition(c) => {
                let base = (fcsr & !CAUSE_MASK & !FCSR_C) | raised;
                self.cop1.ctc1(31, base | if c { FCSR_C } else { 0 });
                return false;
            }
        }
        self.cop1.ctc1(31, (fcsr & !CAUSE_MASK) | raised);
        false
    }

    /// `ABS` and `NEG` — sign manipulation, but **not** a pure bit flip.
    ///
    /// The VR4300 classifies the operand first: a subnormal or an MSB-clear NaN
    /// raises unimplemented-operation, and an MSB-set (signalling, ledger C-12)
    /// NaN raises Invalid and yields the default NaN rather than the operand
    /// with its sign changed. Only when the operand is ordinary does the sign
    /// bit move.
    ///
    /// Unlike `MOV`, these REPLACE the `Cause` field — clearing it on success.
    fn fp_sign_op(&mut self, fmt: u8, funct: u8, fs: u8, fd: u8, fr: bool) -> bool {
        use crate::fpu;
        /// `FCSR` Cause field, bits 17:12.
        const CAUSE_MASK: u32 = 0x3F << 12;

        let fcsr = self.cop1.fcsr();
        let (commit, flags, unimplemented) = if fmt == 0o20 {
            let a = f32::from_bits(self.fpr.read_s_fs(fs, fr));
            if fpu::is_subnormal_f32(a) || fpu::is_unimplemented_nan_f32(a) {
                (0u64, fpu::Flags::NONE, true)
            } else if fpu::is_snan_f32(a) {
                (
                    u64::from(crate::softfloat::F32.default_nan() as u32),
                    fpu::Flags::INVALID,
                    false,
                )
            } else {
                let v = if funct == 5 {
                    fpu::abs_s(a)
                } else {
                    fpu::neg_s(a)
                };
                (u64::from(v.to_bits()), fpu::Flags::NONE, false)
            }
        } else {
            let a = f64::from_bits(self.fpr.read_d_fs(fs, fr));
            if fpu::is_subnormal_f64(a) || fpu::is_unimplemented_nan_f64(a) {
                (0u64, fpu::Flags::NONE, true)
            } else if fpu::is_snan_f64(a) {
                (
                    crate::softfloat::F64.default_nan(),
                    fpu::Flags::INVALID,
                    false,
                )
            } else {
                let v = if funct == 5 {
                    fpu::abs_d(a)
                } else {
                    fpu::neg_d(a)
                };
                (v.to_bits(), fpu::Flags::NONE, false)
            }
        };

        let raised = flags.to_fcsr_bits()
            | if unimplemented {
                fpu::CAUSE_UNIMPLEMENTED
            } else {
                0
            };
        if unimplemented {
            self.cop1
                .ctc1(31, (fcsr & !CAUSE_MASK) | (raised & CAUSE_MASK));
            self.abort_from(Stage::Wb, Exception::FloatingPoint);
            return true;
        }
        if flags.invalid && self.cop1.enables() & (1 << 4) != 0 {
            self.cop1
                .ctc1(31, (fcsr & !CAUSE_MASK) | (raised & CAUSE_MASK));
            self.abort_from(Stage::Wb, Exception::FloatingPoint);
            return true;
        }
        if fmt == 0o20 {
            self.fpr.write_s_arith(fd, fr, commit as u32);
        } else {
            self.fpr.write_d_arith(fd, fr, commit);
        }
        self.cop1.ctc1(31, (fcsr & !CAUSE_MASK) | raised);
        false
    }

    /// `SQRT.fmt`.
    ///
    /// Unary, so it takes the operand policy but not the *result* policy: the
    /// square root of the smallest normal is about `1e-19`, nowhere near the
    /// subnormal range, so a normal operand cannot produce a subnormal result.
    ///
    /// A negative operand is Invalid rather than unimplemented — except `-0`,
    /// whose root is `-0` and raises nothing. [`softfloat::sqrt`] draws that
    /// distinction.
    fn fp_sqrt(
        &self,
        fmt: u8,
        fs: u8,
        fr: bool,
        mode: crate::fpu::Rounding,
    ) -> (FpCommit, crate::fpu::Flags, bool) {
        use crate::fpu;
        if fmt == 0o20 {
            let bits = u64::from(self.fpr.read_s_fs(fs, fr));
            let a = f32::from_bits(bits as u32);
            if fpu::is_subnormal_f32(a) || fpu::is_unimplemented_nan_f32(a) {
                return (FpCommit::Single(0), fpu::Flags::NONE, true);
            }
            let r = softfloat::sqrt(bits, softfloat::F32, mode);
            (FpCommit::Single(r.bits as u32), r.flags, false)
        } else {
            let bits = self.fpr.read_d_fs(fs, fr);
            let a = f64::from_bits(bits);
            if fpu::is_subnormal_f64(a) || fpu::is_unimplemented_nan_f64(a) {
                return (FpCommit::Double(0), fpu::Flags::NONE, true);
            }
            let r = softfloat::sqrt(bits, softfloat::F64, mode);
            (FpCommit::Double(r.bits), r.flags, false)
        }
    }

    /// The VR4300's policy for a **subnormal result**, applied wherever one can
    /// be produced — arithmetic and the narrowing `CVT.S.D`.
    ///
    /// Three outcomes, in order:
    ///
    /// 1. `FCSR.FS` clear — the processor cannot represent the result at all,
    ///    so *unimplemented operation*.
    /// 2. `FS` set but underflow or inexact **enabled** — it cannot deliver a
    ///    trapped underflow's defined result either, so unimplemented again.
    ///    n64-systemtest's own comment on this case reads "(wow)".
    /// 3. `FS` set and both disabled — flush per
    ///    [`fpu::flush_subnormal_f32`], reporting underflow and inexact.
    fn subnormal_policy_s(
        &self,
        out: crate::fpu::Outcome<f32>,
        mode: crate::fpu::Rounding,
    ) -> (FpCommit, crate::fpu::Flags, bool) {
        use crate::fpu;
        // **Either** condition triggers, and both are needed.
        //
        // `is_subnormal` alone misses a result that underflows past the
        // subnormal grid all the way to zero (`f64::MIN_POSITIVE` narrowed to
        // `f32`; `MIN_POSITIVE` squared) — the VR4300 refuses those exactly as
        // it refuses a subnormal.
        //
        // `flags.underflow` alone misses an *exact* subnormal, because IEEE
        // signals underflow only when tiny **and inexact**. Replacing the
        // first test with the second was tried and regressed the oracle from
        // 89 to 131.
        if !fpu::is_subnormal_f32(out.value) && !out.flags.underflow {
            return (FpCommit::Single(out.value.to_bits()), out.flags, false);
        }
        if !self.cop1.flush_denorm_to_zero() || self.underflow_traps() {
            return (FpCommit::Single(0), fpu::Flags::NONE, true);
        }
        let mut flags = out.flags;
        flags.underflow = true;
        flags.inexact = true;
        let v = fpu::flush_subnormal_f32(out.value, mode);
        (FpCommit::Single(v.to_bits()), flags, false)
    }

    /// See [`Pipeline::subnormal_policy_s`].
    fn subnormal_policy_d(
        &self,
        out: crate::fpu::Outcome<f64>,
        mode: crate::fpu::Rounding,
    ) -> (FpCommit, crate::fpu::Flags, bool) {
        use crate::fpu;
        // **Either** condition triggers, and both are needed.
        //
        // `is_subnormal` alone misses a result that underflows past the
        // subnormal grid all the way to zero (`f64::MIN_POSITIVE` narrowed to
        // `f32`; `MIN_POSITIVE` squared) — the VR4300 refuses those exactly as
        // it refuses a subnormal.
        //
        // `flags.underflow` alone misses an *exact* subnormal, because IEEE
        // signals underflow only when tiny **and inexact**. Replacing the
        // first test with the second was tried and regressed the oracle from
        // 89 to 131.
        if !fpu::is_subnormal_f64(out.value) && !out.flags.underflow {
            return (FpCommit::Double(out.value.to_bits()), out.flags, false);
        }
        if !self.cop1.flush_denorm_to_zero() || self.underflow_traps() {
            return (FpCommit::Double(0), fpu::Flags::NONE, true);
        }
        let mut flags = out.flags;
        flags.underflow = true;
        flags.inexact = true;
        let v = fpu::flush_subnormal_f64(out.value, mode);
        (FpCommit::Double(v.to_bits()), flags, false)
    }

    /// Does the CURRENT `FCSR` already meet a trap condition?
    ///
    /// Used after a `CTC1` writes it: the register can be put into a trapping
    /// state directly, without any arithmetic having run.
    fn fcsr_traps_now(&self) -> bool {
        /// `FCSR.Cause` maskable bits 16:12, shifted to line up with `Enable`.
        const CAUSE_SHIFT: u32 = 12;
        let fcsr = self.cop1.fcsr();
        if fcsr & crate::fpu::CAUSE_UNIMPLEMENTED != 0 {
            return true;
        }
        (fcsr >> CAUSE_SHIFT) & self.cop1.enables() != 0
    }

    /// Is underflow or inexact enabled? Either turns a flushed subnormal into
    /// an unimplemented operation.
    fn underflow_traps(&self) -> bool {
        /// `FCSR.Enable` underflow (bit 8) and inexact (bit 7), as
        /// `Cop1Control::enables` returns them — shifted down by 7.
        const ENABLE_UNDERFLOW_OR_INEXACT: u32 = 0b11;
        self.cop1.enables() & ENABLE_UNDERFLOW_OR_INEXACT != 0
    }

    /// `ADD`/`SUB`/`MUL`/`DIV` in either format.
    ///
    /// # The VR4300 cannot compute with subnormals
    ///
    /// It raises the unmaskable *unimplemented operation* cause instead, and
    /// there are three distinct occasions (UM §7.5; pinned by n64-systemtest):
    ///
    /// 1. **A subnormal operand** — checked before the operation is attempted,
    ///    and it outranks everything, including a NaN that would otherwise
    ///    raise Invalid.
    /// 2. **A subnormal result with `FCSR.FS` clear.**
    /// 3. **A subnormal result with `FS` set but underflow or inexact
    ///    *enabled*.** The processor cannot deliver a trapped underflow's
    ///    defined result, so it declines instead — the suite's own comment on
    ///    this case reads "(wow)".
    ///
    /// With `FS` set and those enables clear it flushes, per
    /// [`fpu::flush_subnormal_f32`], and reports underflow + inexact.
    ///
    /// The returned flags are deliberately [`fpu::Flags::NONE`] on every
    /// unimplemented path: `FCSR` must end up with bit 17 and *nothing else*,
    /// which is what the suite asserts.
    fn fp_binary(
        &self,
        fmt: u8,
        funct: u8,
        ft: u8,
        fs: u8,
        fr: bool,
        mode: crate::fpu::Rounding,
    ) -> (FpCommit, crate::fpu::Flags, bool) {
        use crate::fpu;
        if fmt == 0o20 {
            let a = f32::from_bits(self.fpr.read_s_fs(fs, fr));
            let b = f32::from_bits(self.fpr.read_s_ft(ft, fr));
            if fpu::arith_unimplemented_s(a, b) {
                return (FpCommit::Single(0), fpu::Flags::NONE, true);
            }
            let out = match funct {
                0 => fpu::add_s(a, b, mode),
                1 => fpu::sub_s(a, b, mode),
                2 => fpu::mul_s(a, b, mode),
                _ => fpu::div_s(a, b, mode),
            };
            self.subnormal_policy_s(out, mode)
        } else {
            let a = f64::from_bits(self.fpr.read_d_fs(fs, fr));
            let b = f64::from_bits(self.fpr.read_d_ft(ft, fr));
            if fpu::arith_unimplemented_d(a, b) {
                return (FpCommit::Double(0), fpu::Flags::NONE, true);
            }
            let out = match funct {
                0 => fpu::add_d(a, b, mode),
                1 => fpu::sub_d(a, b, mode),
                2 => fpu::mul_d(a, b, mode),
                _ => fpu::div_d(a, b, mode),
            };
            self.subnormal_policy_d(out, mode)
        }
    }

    /// `ROUND`/`TRUNC`/`CEIL`/`FLOOR` to `.W` or `.L` (funct 8..=15).
    ///
    /// These carry their rounding mode **in the opcode** and ignore `FCSR.RM`
    /// entirely — that is the whole reason they exist alongside `CVT.W`/`CVT.L`,
    /// which do consult it. Passing the live `RM` here would make all four
    /// behave identically whenever `RM` happened to match, and the difference
    /// would only show up under a non-default mode.
    fn fp_to_integer(
        &self,
        fmt: u8,
        funct: u8,
        fs: u8,
        fr: bool,
    ) -> (FpCommit, crate::fpu::Flags, bool) {
        use crate::fpu::{self, Rounding};
        let mode = match funct & 0o3 {
            0 => Rounding::Nearest,
            1 => Rounding::TowardZero,
            2 => Rounding::TowardPlusInf,
            _ => Rounding::TowardMinusInf,
        };
        // The source is widened to `f64` first, which is EXACT for an `f32`, so
        // no rounding happens before the one the instruction asks for.
        let v = self.fp_source_as_f64(fmt, fs, fr);
        // funct 8..=11 target `.L`, 12..=15 target `.W`.
        let wide = funct < 0o14;
        if self.integer_conversion_unimplemented(fmt, fs, fr) {
            return (FpCommit::Single(0), fpu::Flags::NONE, true);
        }
        if wide {
            let out = fpu::to_i64(v, mode);
            // `to_i64` reports NaN and out-of-range as Invalid, which is the
            // IEEE answer and NOT this processor's: the VR4300 declines with
            // *unimplemented operation* instead. n64-systemtest expects `Err`
            // for infinities, NaNs and anything past the target's range.
            if out.flags.invalid {
                return (FpCommit::Double(0), fpu::Flags::NONE, true);
            }
            #[allow(clippy::cast_sign_loss)] // a bit pattern, not a magnitude
            (FpCommit::Double(out.value as u64), out.flags, false)
        } else {
            let out = fpu::to_i32(v, mode);
            if out.flags.invalid {
                return (FpCommit::Single(0), fpu::Flags::NONE, true);
            }
            #[allow(clippy::cast_sign_loss)] // a bit pattern, not a magnitude
            (FpCommit::Single(out.value as u32), out.flags, false)
        }
    }

    /// Is the source of a float-to-integer conversion one the VR4300 refuses?
    ///
    /// Subnormality, plus a magnitude of `2^53` or more. NaN and infinity are
    /// detected from the conversion's own result, because "out of range"
    /// depends on the target width.
    ///
    /// The limit applies to `.W` targets as well, where it is **unobservable**:
    /// `2^53` is far outside `i32`, so such a value is refused either way. It
    /// was originally guarded on the target width, and the guard was removed
    /// because no test could distinguish the two — an undistinguishable branch
    /// is one that rots.
    ///
    /// # The `2^53` limit is narrower than `i64`
    ///
    /// `9007198717870080` converts; `9007199254740992` (`2^53`) raises
    /// unimplemented — both far inside `i64`'s range. n64-systemtest brackets
    /// the threshold with adjacent values on either side, which is what
    /// identifies it as `2^53` rather than some larger bound. `2^53` is the
    /// last integer a `double` represents exactly, so the natural reading is
    /// that the conversion runs through double precision internally and
    /// declines whatever it cannot hold exactly.
    fn integer_conversion_unimplemented(&self, fmt: u8, fs: u8, fr: bool) -> bool {
        use crate::fpu;
        /// The last integer a `double` represents exactly.
        const TWO_POW_53: f64 = 9_007_199_254_740_992.0;

        let v = if fmt == 0o20 {
            let a = f32::from_bits(self.fpr.read_s_fs(fs, fr));
            if fpu::is_subnormal_f32(a) {
                return true;
            }
            f64::from(a)
        } else {
            let a = f64::from_bits(self.fpr.read_d_fs(fs, fr));
            if fpu::is_subnormal_f64(a) {
                return true;
            }
            a
        };
        v.abs() >= TWO_POW_53
    }

    /// `CVT.S`/`CVT.D`/`CVT.W`/`CVT.L`, from any source format.
    fn fp_convert(
        &self,
        fmt: u8,
        funct: u8,
        fs: u8,
        fr: bool,
        mode: crate::fpu::Rounding,
    ) -> (FpCommit, crate::fpu::Flags, bool) {
        use crate::fpu;
        match funct {
            // To single.
            0o40 => match fmt {
                0o21 => {
                    let bits = self.fpr.read_d_fs(fs, fr);
                    let a = f64::from_bits(bits);
                    if fpu::is_subnormal_f64(a) || fpu::is_unimplemented_nan_f64(a) {
                        return (FpCommit::Single(0), fpu::Flags::NONE, true);
                    }
                    // Narrowing is ARITHMETIC: it rounds 53 significand bits
                    // into 24, so it can be inexact, can overflow, and can land
                    // in the subnormal range -- each depending on `FCSR.RM`.
                    // `v as f32` reports none of that and rounds to nearest
                    // only, which is where ledger C-11 found this operation
                    // after the arithmetic had already been fixed.
                    let r = softfloat::convert(bits, softfloat::F64, softfloat::F32, mode);
                    let out = fpu::Outcome {
                        value: f32::from_bits(r.bits as u32),
                        flags: r.flags,
                    };
                    // Narrowing can produce a subnormal from a perfectly normal
                    // double, so the result policy applies exactly as it does
                    // to the arithmetic.
                    self.subnormal_policy_s(out, mode)
                }
                #[allow(clippy::cast_possible_wrap)] // reinterpreting a word as signed
                0o24 => {
                    // Through `softfloat`, not a Rust `as` cast: an i32 can need
                    // more than 24 significand bits, so this rounds -- and must
                    // round the way `FCSR.RM` says.
                    let v = i64::from(self.fpr.read_s_fs(fs, fr) as i32);
                    let r = softfloat::from_int(v, softfloat::F32, mode);
                    (FpCommit::Single(r.bits as u32), r.flags, false)
                }
                // From `.L`, which the VR4300 restricts: bits 63:55 must be all
                // zeroes or all ones (UM §7.5.2). Outside that it raises
                // Unimplemented rather than converting, and there is no defined
                // result -- so the commit value is a placeholder the trap path
                // discards.
                #[allow(clippy::cast_possible_wrap)]
                _ => {
                    let v = self.fpr.read_d_fs(fs, fr) as i64;
                    if fpu::long_convertible(v) {
                        let r = softfloat::from_int(v, softfloat::F32, mode);
                        (FpCommit::Single(r.bits as u32), r.flags, false)
                    } else {
                        // No defined result when the restriction is violated, so
                        // the value is a placeholder the trap path discards.
                        (FpCommit::Single(0), fpu::Flags::NONE, true)
                    }
                }
            },
            // To double.
            0o41 => match fmt {
                0o20 => {
                    let bits = u64::from(self.fpr.read_s_fs(fs, fr));
                    let a = f32::from_bits(bits as u32);
                    if fpu::is_subnormal_f32(a) || fpu::is_unimplemented_nan_f32(a) {
                        return (FpCommit::Double(0), fpu::Flags::NONE, true);
                    }
                    // Widening cannot lose anything, so no result policy is
                    // needed — but it goes through the same path so there is
                    // one conversion rather than two that can disagree.
                    let r = softfloat::convert(bits, softfloat::F32, softfloat::F64, mode);
                    (FpCommit::Double(r.bits), r.flags, false)
                }
                #[allow(clippy::cast_possible_wrap)]
                0o24 => {
                    // Exact for every i32 -- 53 bits hold 32 -- but routed the
                    // same way as the others so there is one conversion rather
                    // than two that can disagree.
                    let v = i64::from(self.fpr.read_s_fs(fs, fr) as i32);
                    let r = softfloat::from_int(v, softfloat::F64, mode);
                    (FpCommit::Double(r.bits), r.flags, false)
                }
                #[allow(clippy::cast_possible_wrap)]
                _ => {
                    let v = self.fpr.read_d_fs(fs, fr) as i64;
                    if fpu::long_convertible(v) {
                        let r = softfloat::from_int(v, softfloat::F64, mode);
                        (FpCommit::Double(r.bits), r.flags, false)
                    } else {
                        // No defined result when the restriction is violated, so
                        // the value is a placeholder the trap path discards.
                        (FpCommit::Double(0), fpu::Flags::NONE, true)
                    }
                }
            },
            // To word / to long, both honouring `FCSR.RM` -- which is what
            // separates them from the fixed-mode family above.
            0o44 => {
                if self.integer_conversion_unimplemented(fmt, fs, fr) {
                    return (FpCommit::Single(0), fpu::Flags::NONE, true);
                }
                let out = fpu::to_i32(self.fp_source_as_f64(fmt, fs, fr), mode);
                if out.flags.invalid {
                    return (FpCommit::Single(0), fpu::Flags::NONE, true);
                }
                #[allow(clippy::cast_sign_loss)]
                (FpCommit::Single(out.value as u32), out.flags, false)
            }
            _ => {
                if self.integer_conversion_unimplemented(fmt, fs, fr) {
                    return (FpCommit::Double(0), fpu::Flags::NONE, true);
                }
                let out = fpu::to_i64(self.fp_source_as_f64(fmt, fs, fr), mode);
                if out.flags.invalid {
                    return (FpCommit::Double(0), fpu::Flags::NONE, true);
                }
                #[allow(clippy::cast_sign_loss)]
                (FpCommit::Double(out.value as u64), out.flags, false)
            }
        }
    }

    /// `C.cond.fmt` — writes `FCSR.C`, never an FPR.
    fn fp_compare(
        &self,
        fmt: u8,
        cond: u8,
        ft: u8,
        fs: u8,
        fr: bool,
    ) -> (FpCommit, crate::fpu::Flags, bool) {
        use crate::fpu;
        let out = if fmt == 0o20 {
            fpu::compare_s(
                f32::from_bits(self.fpr.read_s_fs(fs, fr)),
                f32::from_bits(self.fpr.read_s_ft(ft, fr)),
                cond,
            )
        } else {
            fpu::compare_d(
                f64::from_bits(self.fpr.read_d_fs(fs, fr)),
                f64::from_bits(self.fpr.read_d_ft(ft, fr)),
                cond,
            )
        };
        (FpCommit::Condition(out.value), out.flags, false)
    }

    /// Read `fs` in `fmt` and widen to `f64`.
    ///
    /// `f32` to `f64` is exact, so a `.S` source loses nothing on the way in and
    /// the only rounding is the one the instruction performs.
    fn fp_source_as_f64(&self, fmt: u8, fs: u8, fr: bool) -> f64 {
        if fmt == 0o20 {
            f64::from(f32::from_bits(self.fpr.read_s_fs(fs, fr)))
        } else {
            f64::from_bits(self.fpr.read_d_fs(fs, fr))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NullBus {
        irq: bool,
    }
    impl Bus for NullBus {
        fn read_u8(&mut self, _addr: u32) -> u8 {
            0
        }
        fn write_u8(&mut self, _addr: u32, _val: u8) {}
        fn poll_irq(&mut self) -> bool {
            self.irq
        }
    }
    fn quiet() -> NullBus {
        NullBus { irq: false }
    }

    /// **The reverse-order invariant.** A value must advance exactly one stage
    /// per cycle. If the cascade is ever reordered to run forwards, a value falls
    /// through several stages in one cycle and the pipeline is silently too fast.
    #[test]
    fn a_value_advances_exactly_one_stage_per_cycle() {
        let mut p = Pipeline::new();
        let mut pc = 0xFFFF_FFFF_8000_0000;
        let mut regs = Regs::new();
        let mut bus = quiet();

        // Cycle 1 fetches a sentinel into ic_rf.
        p.advance(&mut bus, &mut regs, &mut pc);
        let sentinel = p.ic_rf.pc;
        assert!(p.ic_rf.occupied);
        assert_eq!(sentinel, 0xFFFF_FFFF_8000_0000);

        // Each subsequent cycle moves it exactly one boundary along.
        p.advance(&mut bus, &mut regs, &mut pc);
        assert_eq!(p.rf_ex.pc, sentinel, "after 1 cycle it should be at RF->EX");
        assert_ne!(p.ic_rf.pc, sentinel, "and no longer at IC->RF");

        p.advance(&mut bus, &mut regs, &mut pc);
        assert_eq!(p.ex_dc.pc, sentinel, "after 2 cycles, EX->DC");

        p.advance(&mut bus, &mut regs, &mut pc);
        assert_eq!(p.dc_wb.pc, sentinel, "after 3 cycles, DC->WB");

        // 5 stages => at least 5 PCycles per instruction (UM §4.1).
        assert_eq!(p.retired, 0, "nothing may retire before WB has run on it");
        p.advance(&mut bus, &mut regs, &mut pc);
        assert_eq!(p.retired, 1, "retires at WB on the 5th cycle, not sooner");
    }

    /// **The delay-slot invariant** — the Phase 1 exit criterion.
    ///
    /// A multi-cycle stall between a branch and its delay slot must not
    /// desynchronise the flag. A global `in_delay_slot` bool passes the naive
    /// test and fails this one, which is why the flag rides in the latch.
    #[test]
    fn delay_slot_flag_survives_a_multi_cycle_stall() {
        let mut p = Pipeline::new();
        let mut pc = 0xFFFF_FFFF_8000_0000;
        let mut regs = Regs::new();
        let mut bus = quiet();

        // Two instructions in flight; mark the younger as the delay slot.
        p.advance(&mut bus, &mut regs, &mut pc);
        p.ic_rf.in_delay_slot = true;
        let slot_pc = p.ic_rf.pc;

        // A long interlock lands between them (e.g. DDIV = 69 PCycles).
        p.stall_for(69, Interlock::Mci);
        for _ in 0..69 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert!(p.stalled_by().is_none(), "the stall should have expired");

        // The flag must still be attached to the SAME instruction.
        assert!(
            p.ic_rf.in_delay_slot && p.ic_rf.pc == slot_pc,
            "the delay-slot flag detached from its instruction across the stall"
        );

        // And it must travel with it, not stay behind at a fixed boundary.
        p.advance(&mut bus, &mut regs, &mut pc);
        assert!(
            p.rf_ex.in_delay_slot && p.rf_ex.pc == slot_pc,
            "the flag failed to travel with the instruction"
        );
        assert!(
            !p.ic_rf.in_delay_slot,
            "the flag was left behind on the boundary instead of moving"
        );
    }

    /// A stall holds every latch in place — nothing advances, nothing retires.
    #[test]
    fn a_stall_freezes_the_pipeline() {
        let mut p = Pipeline::new();
        let mut pc = 0xFFFF_FFFF_8000_0000;
        let mut regs = Regs::new();
        let mut bus = quiet();
        for _ in 0..4 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        let before = (p.ic_rf, p.rf_ex, p.ex_dc, p.dc_wb, p.retired);

        p.stall_for(3, Interlock::Dcm);
        for _ in 0..3 {
            p.advance(&mut bus, &mut regs, &mut pc);
            assert_eq!(
                (p.ic_rf, p.rf_ex, p.ex_dc, p.dc_wb, p.retired),
                before,
                "a stalled cycle must not advance any latch"
            );
        }
        assert!(p.stalled_by().is_none());
        p.advance(&mut bus, &mut regs, &mut pc);
        assert_ne!(
            p.dc_wb, before.3,
            "the pipeline must resume after the stall"
        );
    }

    /// **The interrupt gate** (UM §4.7.1): an interrupt is accepted only if the
    /// *previous* `PCycle` was a run cycle. The cycle right after a stall is not.
    #[test]
    fn interrupt_is_not_accepted_on_the_cycle_after_a_stall() {
        /// `IE` set, `IM2` (the RCP line) unmasked, `EXL`/`ERL` clear.
        ///
        /// Needed since T-12-003: an asserted line is no longer sufficient on
        /// its own, and cold reset leaves `ERL` **set**, which alone blocks
        /// every interrupt.
        const IRQ_READY: u64 = 1 | (1 << 10);

        let mut p = Pipeline::new();
        p.cop0.set_hardware(crate::cop0::reg::STATUS, IRQ_READY);
        let mut pc = 0xFFFF_FFFF_8000_0000;
        let mut regs = Regs::new();
        let mut bus = NullBus { irq: true };

        // Warm up so prev_was_run is true, then confirm an IRQ IS taken.
        p.advance(&mut bus, &mut regs, &mut pc);
        assert!(p.prev_cycle_was_run());
        p.advance(&mut bus, &mut regs, &mut pc);
        assert_eq!(
            p.ex_dc.abort,
            Some(Exception::Interrupt),
            "an interrupt should be accepted after a run cycle"
        );

        // Now stall, and check the very next cycle refuses it.
        let mut p = Pipeline::new();
        p.cop0.set_hardware(crate::cop0::reg::STATUS, IRQ_READY);
        let mut pc = 0xFFFF_FFFF_8000_0000;
        let mut regs = Regs::new();
        p.advance(&mut bus, &mut regs, &mut pc);
        p.stall_for(1, Interlock::Dcb);
        p.advance(&mut bus, &mut regs, &mut pc); // the stalled cycle
        assert!(
            !p.prev_cycle_was_run(),
            "a stalled cycle must not count as a run cycle"
        );
        let ex_dc_before = p.ex_dc.abort;
        p.advance(&mut bus, &mut regs, &mut pc); // the cycle immediately after
        assert_eq!(
            p.ex_dc.abort, ex_dc_before,
            "an interrupt must NOT be accepted when the previous PCycle stalled"
        );
    }

    /// **An abort must survive the cascade**, not merely be present the instant
    /// it is stamped.
    ///
    /// The shallow version of this test asserted latch state immediately after
    /// `abort_from` and never advanced — so it could not tell a real flush from
    /// one that gets overwritten by the reverse cascade in the same cycle. This
    /// version steps the pipeline and follows the consequences.
    #[test]
    fn an_abort_survives_the_cascade() {
        let mut p = Pipeline::new();
        let mut pc = 0xFFFF_FFFF_8000_0000;
        let mut regs = Regs::new();
        let mut bus = quiet();
        for _ in 0..4 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }

        // Abort at DC: the instruction in DC plus everything younger.
        p.abort_from(Stage::Dc, Exception::AddressError { store: false });
        let aborted_pc = p.ex_dc.pc;
        p.advance(&mut bus, &mut regs, &mut pc);

        // The causing instruction carried its abort forward into WB's latch...
        assert_eq!(
            (p.dc_wb.abort, p.dc_wb.pc),
            (Some(Exception::AddressError { store: false }), aborted_pc),
            "the aborting instruction lost its flag while advancing"
        );
        // ...and the younger ones kept theirs rather than having them
        // overwritten by the latch moves.
        assert_eq!(
            p.ex_dc.abort,
            Some(Exception::AddressError { store: false }),
            "a younger instruction's abort was overwritten by the cascade"
        );

        // The younger instruction that was already in flight kept its abort as it
        // advanced, rather than having it overwritten by the latch move.
        assert_eq!(
            p.rf_ex.abort,
            Some(Exception::AddressError { store: false }),
            "an in-flight younger instruction lost its abort while advancing"
        );

        // And the fetch issued during the aborting cycle is a bubble, not a live
        // wrong-path instruction that would escape the flush entirely.
        assert!(
            !p.ic_rf.occupied,
            "the instruction fetched during the abort escaped the flush"
        );
    }

    /// A zero-cycle stall request is ignored — recording it would burn a cycle
    /// and suppress interrupt acceptance on the next one, with no visible cause.
    #[test]
    fn a_zero_cycle_stall_is_not_a_stall() {
        let mut p = Pipeline::new();
        let mut pc = 0xFFFF_FFFF_8000_0000;
        let mut regs = Regs::new();
        let mut bus = quiet();
        p.advance(&mut bus, &mut regs, &mut pc);

        p.stall_for(0, Interlock::Ldi);
        assert!(p.stalled_by().is_none(), "a 0-cycle request is not a stall");

        let before = p.ic_rf.pc;
        p.advance(&mut bus, &mut regs, &mut pc);
        assert_ne!(before, p.ic_rf.pc, "the cycle was silently consumed");
        assert!(
            p.prev_cycle_was_run(),
            "a non-stall must still count as a run cycle, or the interrupt gate \
             is wrongly suppressed on the following cycle"
        );
    }

    /// An abort kills its own stage and everything younger, never anything older.
    #[test]
    fn abort_kills_younger_instructions_only() {
        let mut p = Pipeline::new();
        let mut pc = 0xFFFF_FFFF_8000_0000;
        let mut regs = Regs::new();
        let mut bus = quiet();
        for _ in 0..4 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        p.abort_from(Stage::Ex, Exception::Overflow);
        assert_eq!(p.rf_ex.abort, Some(Exception::Overflow), "EX's own latch");
        assert_eq!(p.ic_rf.abort, Some(Exception::Overflow), "younger: killed");
        assert_eq!(p.ex_dc.abort, None, "older instruction must survive");
        assert_eq!(p.dc_wb.abort, None, "older instruction must survive");
    }

    /// An aborted instruction must not retire.
    #[test]
    fn an_aborted_instruction_does_not_retire() {
        let mut p = Pipeline::new();
        let mut pc = 0xFFFF_FFFF_8000_0000;
        let mut regs = Regs::new();
        let mut bus = quiet();
        for _ in 0..4 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        p.dc_wb.abort = Some(Exception::AddressError { store: false });
        let retired = p.retired;
        p.advance(&mut bus, &mut regs, &mut pc);
        assert_eq!(p.retired, retired, "an aborted instruction retired anyway");
    }

    /// **End to end**: a real program, fetched from a bus, decoded, executed and
    /// committed to the register file through all five stages.
    ///
    /// Until this passed, every other test in the crate exercised a piece in
    /// isolation. This is the one that says the CPU *runs*.
    #[test]
    fn a_program_executes_through_the_whole_pipeline() {
        /// A bus holding a program at 0, returning `NOP` past its end.
        struct Rom(alloc::vec::Vec<u32>);
        impl Bus for Rom {
            fn read_u8(&mut self, _addr: u32) -> u8 {
                0
            }
            fn write_u8(&mut self, _addr: u32, _val: u8) {}
            fn read_u32(&mut self, addr: u32) -> u32 {
                self.0.get((addr / 4) as usize).copied().unwrap_or(0)
            }
        }
        const fn r(funct: u32, rs: u32, rt: u32, rd: u32, sa: u32) -> u32 {
            (rs << 21) | (rt << 16) | (rd << 11) | (sa << 6) | funct
        }
        const fn i(opcode: u32, rs: u32, rt: u32, imm: u16) -> u32 {
            (opcode << 26) | (rs << 21) | (rt << 16) | imm as u32
        }

        //   ADDIU $1, $0, 6      ; $1 = 6
        //   ADDIU $2, $0, 7      ; $2 = 7
        //   MULT  $1, $2         ; HI:LO = 42   (stalls 5 PCycles)
        //   MFLO  $3             ; $3 = 42
        //   ADDU  $4, $1, $2     ; $4 = 13
        //   SLL   $5, $2, 2      ; $5 = 28
        let program = alloc::vec![
            i(0o11, 0, 1, 6),
            i(0o11, 0, 2, 7),
            r(0o30, 1, 2, 0, 0),
            r(0o22, 0, 0, 3, 0),
            r(0o41, 1, 2, 4, 0),
            r(0o00, 0, 2, 5, 2),
        ];
        let mut bus = Rom(program);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        let mut pc = 0xFFFF_FFFF_8000_0000u64;

        // Generous budget: 6 instructions, 5 stages deep, plus the MULT stall.
        for _ in 0..64 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }

        assert_eq!(regs.read(1), 6, "ADDIU $1");
        assert_eq!(regs.read(2), 7, "ADDIU $2");
        assert_eq!(regs.lo, 42, "MULT wrote LO");
        assert_eq!(regs.hi, 0, "MULT wrote HI");
        assert_eq!(regs.read(3), 42, "MFLO read LO into $3");
        assert_eq!(regs.read(4), 13, "ADDU $1 + $2");
        assert_eq!(regs.read(5), 28, "SLL $2 << 2");
        assert_eq!(regs.read(0), 0, "$zero stayed zero");
        assert!(p.retired >= 6, "all six instructions retired");
    }

    /// `$zero` must survive an instruction that nominally targets it — software
    /// depends on that, and `ADDIU $0, $0, 5` is a legal encoding.
    #[test]
    fn writes_to_zero_are_discarded_by_the_pipeline() {
        struct Ones;
        impl Bus for Ones {
            fn read_u8(&mut self, _addr: u32) -> u8 {
                0
            }
            fn write_u8(&mut self, _addr: u32, _val: u8) {}
            fn read_u32(&mut self, _addr: u32) -> u32 {
                // ADDIU $0, $0, 5 -- targets $zero, forever.
                (0o11 << 26) | 5
            }
        }
        let mut bus = Ones;
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        let mut pc = 0xFFFF_FFFF_8000_0000u64;
        for _ in 0..32 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(regs.read(0), 0, "$zero was written");
        assert_eq!(regs.gpr[0], 0, "$zero was written through the raw array");
    }

    /// An overflowing `ADD` must abort rather than commit, and the destination
    /// register must be left untouched.
    #[test]
    fn an_overflow_trap_prevents_the_write_back() {
        struct Overflowing;
        impl Bus for Overflowing {
            fn read_u8(&mut self, _addr: u32) -> u8 {
                0
            }
            fn write_u8(&mut self, _addr: u32, _val: u8) {}
            fn read_u32(&mut self, addr: u32) -> u32 {
                match addr / 4 {
                    // LUI $1, 0x7FFF ; ORI $1, $1, 0xFFFF  => $1 = i32::MAX
                    0 => (0o17 << 26) | (1 << 16) | 0x7FFF,
                    1 => (0o15 << 26) | (1 << 21) | (1 << 16) | 0xFFFF,
                    // ADDIU $2, $0, 1
                    2 => (0o11 << 26) | (2 << 16) | 1,
                    // ADD $3, $1, $2  -> overflows
                    3 => (1 << 21) | (2 << 16) | (3 << 11) | 0o40,
                    _ => 0,
                }
            }
        }
        let mut bus = Overflowing;
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        let mut pc = 0xFFFF_FFFF_8000_0000u64;
        for _ in 0..48 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(regs.read(1), 0x7FFF_FFFF, "LUI+ORI built i32::MAX");
        assert_eq!(regs.read(3), 0, "the overflowing ADD must not commit");
    }

    /// An unaligned instruction fetch raises an address error instead of
    /// fetching, and must not silently realign the PC — that would convert a
    /// raised exception into a wrong answer on a path hardware never takes.
    #[test]
    fn an_unaligned_fetch_raises_address_error_without_realigning() {
        struct Watch {
            fetched: bool,
        }
        impl Bus for Watch {
            fn read_u8(&mut self, _addr: u32) -> u8 {
                0
            }
            fn write_u8(&mut self, _addr: u32, _val: u8) {}
            fn read_u32(&mut self, _addr: u32) -> u32 {
                self.fetched = true;
                0
            }
        }
        let mut bus = Watch { fetched: false };
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        let mut pc = 0xFFFF_FFFF_8000_0002; // deliberately unaligned

        p.advance(&mut bus, &mut regs, &mut pc);

        assert_eq!(
            p.ic_rf.abort,
            Some(Exception::AddressError { store: false }),
            "an unaligned fetch must raise AdEL"
        );
        assert!(
            !bus.fetched,
            "the bus must not be accessed for a bad address"
        );
        // The PC is not silently realigned -- it is *vectored*, which is the
        // architectural response. Before T-12-002 nothing dispatched and this
        // asserted the PC stayed at the bad address; that was the absence of
        // dispatch, not a rule.
        assert_eq!(
            pc, 0xFFFF_FFFF_BFC0_0380,
            "the BEV=1 general vector -- cold reset leaves BEV set (UM §6.4.4), \
             so a fresh CPU vectors into the boot ROM, not into RDRAM"
        );
        assert_eq!(
            p.cop0.read(crate::cop0::reg::BAD_VADDR),
            0xFFFF_FFFF_8000_0002,
            "and BadVAddr holds the unaligned address, un-realigned"
        );
        assert_eq!(
            (p.cop0.read(crate::cop0::reg::CAUSE) >> 2) & 0x1F,
            crate::exception::exc_code::ADEL,
            "an instruction fetch is a load: AdEL, not AdES"
        );
        // EPC must name the FAULTING fetch. `abort_with` captures its context
        // out of `ic_rf`, so raising before populating that latch would silently
        // record the previous fetch's PC here -- which is exactly what this code
        // did until review caught it.
        assert_eq!(
            p.cop0.read(crate::cop0::reg::EPC),
            0xFFFF_FFFF_8000_0002,
            "EPC names the faulting fetch, not the previous one"
        );

        // And the faulting instruction must never retire. Bounded to the
        // epilogue stall on purpose: past that the CPU is running the *handler*,
        // whose instructions retire legitimately, so a longer window would be
        // asserting that exception handling does not work.
        assert_eq!(p.retired, 0, "the faulting fetch retired");
        assert_eq!(
            p.stalled_by(),
            Some(Interlock::Exception),
            "the 2-PCycle epilogue stall is in progress (UM §4.7 p.114)"
        );
        for _ in 0..crate::exception::EPILOGUE_STALL {
            p.advance(&mut bus, &mut regs, &mut pc);
            assert_eq!(p.retired, 0, "nothing retires while the pipe drains");
        }
        assert_eq!(p.stalled_by(), None, "the stall was exactly 2 PCycles");
    }

    /// A RAM-backed bus, so loads and stores can be exercised end to end.
    struct Ram {
        prog: alloc::vec::Vec<u32>,
        data: alloc::vec::Vec<u8>,
    }
    impl Ram {
        fn new(prog: alloc::vec::Vec<u32>) -> Self {
            Self {
                prog,
                data: alloc::vec![0; 0x1000],
            }
        }
    }
    impl Bus for Ram {
        fn read_u8(&mut self, addr: u32) -> u8 {
            self.data.get(addr as usize).copied().unwrap_or(0)
        }
        fn write_u8(&mut self, addr: u32, val: u8) {
            if let Some(b) = self.data.get_mut(addr as usize) {
                *b = val;
            }
        }
        fn read_u32(&mut self, addr: u32) -> u32 {
            // Instructions live above 0x800; data below it.
            if addr >= 0x800 {
                self.prog
                    .get(((addr - 0x800) / 4) as usize)
                    .copied()
                    .unwrap_or(0)
            } else {
                u32::from_be_bytes([
                    self.read_u8(addr),
                    self.read_u8(addr + 1),
                    self.read_u8(addr + 2),
                    self.read_u8(addr + 3),
                ])
            }
        }
    }
    const fn ld_st(opcode: u32, base: u32, rt: u32, off: u16) -> u32 {
        (opcode << 26) | (base << 21) | (rt << 16) | off as u32
    }

    /// A store followed by a load round-trips through real memory, at every
    /// width, with the sign/zero-extension rules applied.
    #[test]
    fn stores_and_loads_round_trip_through_memory() {
        //   LUI   $1, 0x8000        ; KSEG0 base
        //   ADDIU $2, $0, -2        ; value = 0xFFFF_FFFF_FFFF_FFFE
        //   SW    $2, 0x100($1)
        //   LW    $3, 0x100($1)     ; sign-extended  -> 0xFFFF_FFFF_FFFF_FFFE
        //   LWU   $4, 0x100($1)     ; zero-extended  -> 0x0000_0000_FFFF_FFFE
        //   LBU   $5, 0x100($1)     ; big-endian MSB -> 0xFF
        let prog = alloc::vec![
            lui_kseg0(1),
            (0o11 << 26) | (2 << 16) | 0xFFFE,
            ld_st(0o53, 1, 2, 0x100),
            ld_st(0o43, 1, 3, 0x100),
            ld_st(0o47, 1, 4, 0x100),
            ld_st(0o44, 1, 5, 0x100),
        ];
        let mut bus = Ram::new(prog);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        let mut pc = KSEG0_PROG;
        for _ in 0..80 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(regs.read(3), 0xFFFF_FFFF_FFFF_FFFE, "LW sign-extends");
        assert_eq!(regs.read(4), 0x0000_0000_FFFF_FFFE, "LWU zero-extends");
        assert_eq!(regs.read(5), 0xFF, "LBU reads the big-endian MSB");
    }

    /// An unaligned `LW` raises an address error and does not commit — the whole
    /// reason the `LWL`/`LWR` family exists.
    #[test]
    fn an_unaligned_load_raises_address_error() {
        //   ADDIU $1, $0, 0x101   ; deliberately unaligned
        //   LW    $2, 0($1)
        let prog = alloc::vec![(0o11 << 26) | (1 << 16) | 0x101, ld_st(0o43, 1, 2, 0)];
        let mut bus = Ram::new(prog);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        let mut pc = KSEG0_PROG;
        for _ in 0..40 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(regs.read(2), 0, "an unaligned LW must not commit");
    }

    /// **The load-delay interlock** (UM §4.6.5) — it finally has something to
    /// interlock against. A load followed by an instruction naming the loaded
    /// register stalls one cycle, and the dependent instruction must still see
    /// the loaded value.
    #[test]
    fn a_load_followed_by_its_use_interlocks_and_still_reads_the_value() {
        //   LUI   $1, 0x8000     ; KSEG0 base -- unmapped, so no TLB entry needed
        //   ADDIU $2, $0, 0x55
        //   SW    $2, 0x100($1)
        //   LW    $3, 0x100($1)  ; load
        //   ADDIU $4, $3, 1      ; uses $3 immediately -> LDI stall
        let prog = alloc::vec![
            lui_kseg0(1),
            (0o11 << 26) | (2 << 16) | 0x55,
            ld_st(0o53, 1, 2, 0x100),
            ld_st(0o43, 1, 3, 0x100),
            (0o11 << 26) | (3 << 21) | (4 << 16) | 1,
        ];
        let mut bus = Ram::new(prog);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        let mut pc = KSEG0_PROG;
        let mut saw_ldi = false;
        for _ in 0..80 {
            p.advance(&mut bus, &mut regs, &mut pc);
            if p.stalled_by() == Some(Interlock::Ldi) {
                saw_ldi = true;
            }
        }
        assert!(saw_ldi, "the load-delay interlock never fired");
        assert_eq!(regs.read(3), 0x55, "LW loaded the stored value");
        assert_eq!(regs.read(4), 0x56, "the dependent instruction saw it");
    }

    /// The `LWL`/`LWR` pair assembles an unaligned word from memory — the
    /// end-to-end version of the unit tests in `mem`.
    #[test]
    fn the_unaligned_pair_assembles_a_word_from_real_memory() {
        //   ADDIU $1, $0, 0x100
        //   LWL   $3, 1($1)
        //   LWR   $3, 4($1)
        let prog = alloc::vec![
            lui_kseg0(1),
            ld_st(0o42, 1, 3, 0x101),
            ld_st(0o46, 1, 3, 0x104),
        ];
        let mut bus = Ram::new(prog);
        // Memory at 0x100: 00 11 22 33 44 55 66 77
        for (k, b) in [0x00u8, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77]
            .into_iter()
            .enumerate()
        {
            bus.write_u8(0x100 + k as u32, b);
        }
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        let mut pc = KSEG0_PROG;
        for _ in 0..60 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(
            regs.read(3),
            crate::alu::sext32(0x1122_3344),
            "LWL+LWR must assemble the unaligned word at 0x101"
        );
    }

    /// **The branch delay slot executes before the target.** This is the single
    /// most load-bearing property of MIPS control flow: the instruction *after* a
    /// branch runs whether or not the branch is taken.
    #[test]
    fn the_delay_slot_executes_before_the_branch_target() {
        //   0x800: ADDIU $1, $0, 1
        //   0x804: BEQ   $0, $0, +2   ; taken, to 0x810
        //   0x808: ADDIU $2, $0, 2    ; DELAY SLOT -- must execute
        //   0x80C: ADDIU $3, $0, 3    ; skipped
        //   0x810: ADDIU $4, $0, 4    ; target
        let prog = alloc::vec![
            (0o11 << 26) | (1 << 16) | 1,
            (0o04 << 26) | 2,
            (0o11 << 26) | (2 << 16) | 2,
            (0o11 << 26) | (3 << 16) | 3,
            (0o11 << 26) | (4 << 16) | 4,
        ];
        let mut bus = Ram::new(prog);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        let mut pc = KSEG0_PROG;
        for _ in 0..60 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(regs.read(1), 1, "before the branch");
        assert_eq!(regs.read(2), 2, "the DELAY SLOT must execute");
        assert_eq!(regs.read(3), 0, "the instruction after the slot is skipped");
        assert_eq!(regs.read(4), 4, "the target must execute");
    }

    /// **Branch-likely nullifies its delay slot when NOT taken**; an ordinary
    /// branch does not. Confusing the two silently runs or skips one instruction
    /// per untaken branch, which is invisible until a loop's trip count is wrong.
    #[test]
    fn branch_likely_nullifies_its_delay_slot_but_an_ordinary_branch_does_not() {
        // BNEL $0, $0, +1   ; NOT taken (0 == 0), so the slot is nullified
        // ADDIU $2, $0, 2   ; DELAY SLOT -- must be squashed
        // ADDIU $3, $0, 3
        let likely = alloc::vec![
            (0o25 << 26) | 1,
            (0o11 << 26) | (2 << 16) | 2,
            (0o11 << 26) | (3 << 16) | 3,
        ];
        let mut bus = Ram::new(likely);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        let mut pc = KSEG0_PROG;
        for _ in 0..50 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(
            regs.read(2),
            0,
            "BNEL not taken must NULLIFY its delay slot"
        );
        assert_eq!(regs.read(3), 3, "execution continues after the slot");

        // The same shape with the ordinary BNE: the slot DOES execute.
        let ordinary = alloc::vec![
            (0o05 << 26) | 1,
            (0o11 << 26) | (2 << 16) | 2,
            (0o11 << 26) | (3 << 16) | 3,
        ];
        let mut bus = Ram::new(ordinary);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        let mut pc = KSEG0_PROG;
        for _ in 0..50 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(
            regs.read(2),
            2,
            "BNE not taken must still RUN its delay slot"
        );
    }

    /// `JAL` links the address *after* the delay slot, so a returning `JR $31`
    /// resumes past it rather than re-executing it.
    #[test]
    fn jal_links_past_the_delay_slot_and_jr_returns_there() {
        //   0x800: JAL 0x204        ; -> 0x810, links $31 = 0x808
        //   0x804: ADDIU $1, $0, 1  ; DELAY SLOT
        //   0x808: ADDIU $2, $0, 2  ; where JR $31 must return to
        //   0x80C: ADDIU $9, $0, 9  ; must NOT run before the return
        //   0x810: JR $31
        //   0x814: ADDIU $3, $0, 3  ; the callee's DELAY SLOT
        let prog = alloc::vec![
            (0o03 << 26) | 0x204,
            (0o11 << 26) | (1 << 16) | 1,
            (0o11 << 26) | (2 << 16) | 2,
            (0o11 << 26) | (9 << 16) | 9,
            (31 << 21) | 0o10,
            (0o11 << 26) | (3 << 16) | 3,
        ];
        let mut bus = Ram::new(prog);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        let mut pc = KSEG0_PROG;
        for _ in 0..80 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(
            regs.read(31),
            KSEG0_PROG + 8,
            "JAL links PC+8, past the delay slot"
        );
        assert_eq!(regs.read(1), 1, "JAL's delay slot ran");
        assert_eq!(regs.read(3), 3, "JR's delay slot ran");
        assert_eq!(regs.read(2), 2, "JR $31 returned to the linked address");
    }

    /// A trap whose condition holds raises an exception and does not commit.
    #[test]
    fn a_taken_trap_raises_and_an_untaken_one_does_not() {
        //   ADDIU $1, $0, 5
        //   TEQ   $1, $1      ; equal -> traps
        //   ADDIU $2, $0, 2   ; must not commit
        let prog = alloc::vec![
            (0o11 << 26) | (1 << 16) | 5,
            (1 << 21) | (1 << 16) | 0o64,
            (0o11 << 26) | (2 << 16) | 2,
        ];
        let mut bus = Ram::new(prog);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        let mut pc = KSEG0_PROG;
        let mut trapped = false;
        for _ in 0..50 {
            p.advance(&mut bus, &mut regs, &mut pc);
            if p.ex_dc.abort == Some(Exception::Trap) || p.dc_wb.abort == Some(Exception::Trap) {
                trapped = true;
            }
        }
        assert!(trapped, "TEQ with equal operands must trap");

        // TNE with equal operands does NOT trap.
        let prog = alloc::vec![
            (0o11 << 26) | (1 << 16) | 5,
            (1 << 21) | (1 << 16) | 0o66,
            (0o11 << 26) | (2 << 16) | 2,
        ];
        let mut bus = Ram::new(prog);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        let mut pc = KSEG0_PROG;
        for _ in 0..50 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(
            regs.read(2),
            2,
            "an untaken trap must not disturb execution"
        );
    }

    /// `SYSCALL` and `BREAK` raise their own exceptions.
    #[test]
    fn syscall_and_break_raise_their_exceptions() {
        for (funct, want) in [(0o14u32, Exception::Syscall), (0o15, Exception::Breakpoint)] {
            let mut bus = Ram::new(alloc::vec![funct]);
            let mut regs = Regs::new();
            let mut p = Pipeline::new();
            let mut pc = KSEG0_PROG;
            let mut seen = false;
            for _ in 0..40 {
                p.advance(&mut bus, &mut regs, &mut pc);
                if p.ex_dc.abort == Some(want) || p.dc_wb.abort == Some(want) {
                    seen = true;
                }
            }
            assert!(seen, "funct {funct:o} should raise {want:?}");
        }
    }

    /// **`in_delay_slot` must actually be set**, and only on the instruction
    /// after a branch or jump.
    ///
    /// This test exists because mutation-testing found the flag was **not yet
    /// load-bearing**: forcing it to `false` broke nothing, since its only
    /// consumer is `Cause.BD`/`EPC` at exception time and COP0 arrives in
    /// Sprint 2. A field that is written and never read is the exact pattern
    /// this crate has twice deleted (`poll_irq_at_phase`, `Stall.resume`).
    ///
    /// Rather than delete it — it is genuinely needed, and it must ride in the
    /// latch rather than be recomputed later — it is pinned here so it is
    /// verified from the moment it is written.
    #[test]
    fn in_delay_slot_is_set_on_exactly_the_instruction_after_a_branch() {
        //   0x800: ADDIU $1, $0, 1   ; not a delay slot
        //   0x804: BEQ   $0, $0, +2  ; a branch, not itself a delay slot
        //   0x808: ADDIU $2, $0, 2   ; IS the delay slot
        //   0x810: ADDIU $4, $0, 4   ; target, not a delay slot
        let prog = alloc::vec![
            (0o11 << 26) | (1 << 16) | 1,
            (0o04 << 26) | 2,
            (0o11 << 26) | (2 << 16) | 2,
            (0o11 << 26) | (3 << 16) | 3,
            (0o11 << 26) | (4 << 16) | 4,
        ];
        let mut bus = Ram::new(prog);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        let mut pc = KSEG0_PROG;

        let mut flagged = alloc::vec::Vec::new();
        for _ in 0..8 {
            p.advance(&mut bus, &mut regs, &mut pc);
            if p.ic_rf.occupied && p.ic_rf.in_delay_slot {
                flagged.push(p.ic_rf.pc);
            }
        }
        assert_eq!(
            flagged,
            alloc::vec![KSEG0_PROG + 8],
            "exactly one instruction -- the one after the branch -- must be \
             flagged as a delay slot"
        );
    }

    /// The load interlock reproduces the hardware's documented imprecision.
    /// Emulating *precise* behaviour here is the bug.
    #[test]
    fn load_interlock_is_imprecise_exactly_as_hardware_is() {
        // Matches on rt: the ordinary true positive.
        assert!(load_interlocks(8, 0, 8, true));
        // Matches on rs.
        assert!(load_interlocks(8, 8, 0, true));
        // False positive hardware really has: LUI's unused rs field matching.
        assert!(
            load_interlocks(8, 8, 9, true),
            "hardware stalls even when the field is not used as a source"
        );
        // Two consecutive loads into the same register also stall.
        assert!(load_interlocks(8, 0, 8, true));
        // $zero is exempt -- a load into it can never be depended on.
        assert!(!load_interlocks(0, 0, 0, true));
        // GPR loads do not interlock with float instructions, or vice versa.
        assert!(!load_interlocks(8, 8, 8, false));
        // No overlap at all.
        assert!(!load_interlocks(8, 9, 10, true));
    }

    // --- LL / SC (UM §16 pp. 453, 487; §3.1; §5.4.7) ------------------------

    use crate::mem::{LoadKind, StoreKind};

    /// Where [`Ram`] keeps test programs, addressed through **KSEG0**.
    ///
    /// KSEG0 is unmapped, so it reaches physical `0x800` without a TLB entry —
    /// which is how real code runs and, since T-12-004, the only way a test can
    /// fetch at all without installing a mapping. Fetching from a bare `0x800`
    /// is a KUSEG address and now correctly raises a TLB refill.
    const KSEG0_PROG: u64 = 0xFFFF_FFFF_8000_0800;

    /// A bus that fetches `NOP`s and holds its interrupt line asserted.
    ///
    /// Reads return 0, which decodes to `SLL $0, $0, 0` — the canonical `NOP` —
    /// so the pipeline runs without any instruction interfering with what these
    /// tests observe.
    struct AlwaysIrq;
    impl Bus for AlwaysIrq {
        fn read_u8(&mut self, _a: u32) -> u8 {
            0
        }
        fn write_u8(&mut self, _a: u32, _v: u8) {}
        fn read_u32(&mut self, _a: u32) -> u32 {
            0
        }
        fn poll_irq(&mut self) -> bool {
            true
        }
    }

    /// The synchronisation tests need only the data half of [`Ram`].
    fn ram() -> Ram {
        Ram::new(alloc::vec![])
    }

    /// Push the dirty D-cache line covering `vaddr` out to the bus.
    ///
    /// A store to a cached segment now lands in the write-back D-cache and RAM
    /// does not see it until something forces it out. That is the whole point of
    /// a write-back cache, and it is what real software does before handing a
    /// buffer to DMA -- so a test that asserts on RAM without this would be
    /// asserting the cache does not exist.
    fn writeback<B: Bus>(p: &mut Pipeline, bus: &mut B, vaddr: u64) {
        p.access(
            bus,
            MemOp::Cache {
                addr: vaddr,
                op: 25,
            },
        )
        .expect("Hit_WriteBack on a mapped address cannot fault");
    }

    /// Without a preceding `LL` the store must not happen, and `rt` must still
    /// be written — with 0. A `Store`-shaped implementation writes memory
    /// unconditionally; a `Load`-shaped one never writes `rt` on failure.
    #[test]
    fn sc_without_ll_stores_nothing_and_reports_failure() {
        let mut p = Pipeline::new();
        let mut bus = ram();
        let wb = p
            .access(
                &mut bus,
                MemOp::ConditionalStore {
                    kind: StoreKind::Word,
                    addr: 0xFFFF_FFFF_8000_0000 | 0x40,
                    value: 0xDEAD_BEEF,
                    dest: 9,
                },
            )
            .expect("aligned");
        assert_eq!(
            wb,
            WriteBack::Gpr { dest: 9, value: 0 },
            "failure is reported in rt as 0"
        );
        assert_eq!(bus.read_u32(0x40), 0, "memory untouched");
    }

    /// The ordinary success path, and the `LLAddr` side effect.
    #[test]
    fn ll_arms_the_link_and_records_the_physical_address() {
        let mut p = Pipeline::new();
        let mut bus = ram();
        bus.data[0x40..0x44].copy_from_slice(&0x1234_5678u32.to_be_bytes());

        // KSEG0, so `translate` has to strip the segment for LLAddr to be right.
        let wb = p
            .access(
                &mut bus,
                MemOp::LinkedLoad {
                    kind: LoadKind::SignedWord,
                    addr: 0xFFFF_FFFF_8000_0040,
                    dest: 8,
                },
            )
            .expect("aligned");
        assert_eq!(
            wb,
            WriteBack::Gpr {
                dest: 8,
                value: 0x1234_5678
            }
        );
        assert!(p.ll_bit(), "LL arms the link bit");
        assert_eq!(
            p.ll_addr(),
            0x40 >> 4,
            "LLAddr holds PA(31:4) of the PHYSICAL address, not the virtual one"
        );

        let wb = p
            .access(
                &mut bus,
                MemOp::ConditionalStore {
                    kind: StoreKind::Word,
                    addr: 0xFFFF_FFFF_8000_0040,
                    value: 0xA5A5_A5A5,
                    dest: 9,
                },
            )
            .expect("aligned");
        assert_eq!(wb, WriteBack::Gpr { dest: 9, value: 1 });
        writeback(&mut p, &mut bus, 0xFFFF_FFFF_8000_0040);
        assert_eq!(bus.read_u32(0x40), 0xA5A5_A5A5, "the store happened");
    }

    /// The manual lists exactly what clears `LLbit`: *"set by the LL
    /// instruction, cleared by an ERET, and tested by the SC instruction"*
    /// (UM §3.1). `SC` is a *tester*, not a clearer.
    ///
    /// This is the assertion that fails if someone "tidies up" by clearing the
    /// link in `SC` — which looks right, matches several other architectures,
    /// and would make a second `SC` spuriously fail.
    #[test]
    fn sc_does_not_clear_the_link_bit() {
        let mut p = Pipeline::new();
        let mut bus = ram();
        p.access(
            &mut bus,
            MemOp::LinkedLoad {
                kind: LoadKind::SignedWord,
                addr: 0xFFFF_FFFF_8000_0000 | 0x40,
                dest: 8,
            },
        )
        .expect("aligned");

        for round in 0..3 {
            let wb = p
                .access(
                    &mut bus,
                    MemOp::ConditionalStore {
                        kind: StoreKind::Word,
                        addr: 0xFFFF_FFFF_8000_0000 | 0x40,
                        value: 1,
                        dest: 9,
                    },
                )
                .expect("aligned");
            assert_eq!(
                wb,
                WriteBack::Gpr { dest: 9, value: 1 },
                "SC #{round} must still succeed -- nothing has cleared LLbit"
            );
            assert!(p.ll_bit(), "and the bit is still armed after SC #{round}");
        }
    }

    /// *"If this instruction both fails and causes an exception, the exception
    /// takes precedence"* (UM §16 p. 487) — so a misaligned `SC` must raise,
    /// not quietly report failure in `rt`.
    #[test]
    fn misaligned_sc_raises_rather_than_reporting_failure() {
        let mut p = Pipeline::new();
        let mut bus = ram();
        let err = p
            .access(
                &mut bus,
                MemOp::ConditionalStore {
                    kind: StoreKind::Word,
                    addr: 0xFFFF_FFFF_8000_0000 | 0x42,
                    value: 1,
                    dest: 9,
                },
            )
            .expect_err("misaligned");
        assert_eq!(
            err,
            Exception::AddressError { store: true },
            "SC is a store, so AdES not AdEL"
        );
    }

    /// A misaligned `LL` must not arm the link — the instruction did not
    /// complete, so a following `SC` has nothing to succeed against.
    #[test]
    fn misaligned_ll_does_not_arm_the_link() {
        let mut p = Pipeline::new();
        let mut bus = ram();
        let err = p
            .access(
                &mut bus,
                MemOp::LinkedLoad {
                    kind: LoadKind::SignedWord,
                    addr: 0xFFFF_FFFF_8000_0000 | 0x42,
                    dest: 8,
                },
            )
            .expect_err("misaligned");
        assert_eq!(
            err,
            Exception::AddressError { store: false },
            "LL is a load, so AdEL not AdES"
        );
        assert!(!p.ll_bit(), "a faulted LL leaves the link disarmed");
    }

    /// The doubleword forms share the path, but the width must actually differ.
    #[test]
    fn lld_and_scd_operate_on_eight_bytes() {
        let mut p = Pipeline::new();
        let mut bus = ram();
        bus.data[0x40..0x48].copy_from_slice(&0x0123_4567_89AB_CDEFu64.to_be_bytes());

        let wb = p
            .access(
                &mut bus,
                MemOp::LinkedLoad {
                    kind: LoadKind::Double,
                    addr: 0xFFFF_FFFF_8000_0000 | 0x40,
                    dest: 8,
                },
            )
            .expect("aligned");
        assert_eq!(
            wb,
            WriteBack::Gpr {
                dest: 8,
                value: 0x0123_4567_89AB_CDEF
            }
        );

        p.access(
            &mut bus,
            MemOp::ConditionalStore {
                kind: StoreKind::Double,
                addr: 0xFFFF_FFFF_8000_0000 | 0x40,
                value: u64::MAX,
                dest: 9,
            },
        )
        .expect("aligned");
        writeback(&mut p, &mut bus, 0xFFFF_FFFF_8000_0040);
        assert_eq!(bus.read_u32(0x40), u32::MAX);
        assert_eq!(bus.read_u32(0x44), u32::MAX, "all eight bytes, not four");
    }

    // --- COP0 access through the pipeline (T-12-001) -----------------------

    /// Build a `COP0` instruction word: opcode 0o20, `rs` = form, `rt` = GPR,
    /// `rd` = COP0 register.
    const fn cop0_word(rs: u32, rt: u32, rd: u32) -> u32 {
        (0o20 << 26) | (rs << 21) | (rt << 16) | (rd << 11)
    }

    /// `LUI rt, 0x8000` — put a **KSEG0** base in `rt`.
    ///
    /// Data addresses need this for the same reason instruction fetches need
    /// [`KSEG0_PROG`]: a bare low address is KUSEG, which is TLB-mapped and now
    /// correctly raises a refill rather than being silently masked.
    const fn lui_kseg0(rt: u32) -> u32 {
        (0o17 << 26) | (rt << 16) | 0x8000
    }

    /// `ADDIU rt, $0, imm` — the constant-loading prologue these tests share.
    /// Written as a helper rather than inline so the `rs = $0` term does not
    /// have to be spelled as a no-op shift that clippy objects to.
    const fn addiu_zero(rt: u32, imm: u16) -> u32 {
        (0o11 << 26) | (rt << 16) | imm as u32
    }

    /// `MTC0` then `MFC0` must round-trip through the real register file,
    /// exercising the WB-write / DC-read split rather than a direct call.
    #[test]
    fn mtc0_then_mfc0_round_trips_through_the_pipeline() {
        //   ADDIU $1, $0, 0x18     ; a value to write
        //   MTC0  $1, Compare      ; COP0 write happens in WB
        //   MFC0  $2, Compare      ; COP0 read happens in DC
        let program = alloc::vec![
            addiu_zero(1, 0x18),
            cop0_word(0o04, 1, u32::from(crate::cop0::reg::COMPARE)),
            cop0_word(0o00, 2, u32::from(crate::cop0::reg::COMPARE)),
        ];
        let mut bus = Ram::new(program);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        let mut pc = KSEG0_PROG;

        for _ in 0..32 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }

        assert_eq!(
            p.cop0.read(crate::cop0::reg::COMPARE),
            0x18,
            "MTC0 reached the COP0 register file"
        );
        assert_eq!(regs.read(2), 0x18, "MFC0 brought it back into $2");
    }

    /// `MTC0` must not be given a GPR destination by decode: it reads `rt` and
    /// writes COP0. If `dest` were set to `rt`, the instruction would clobber
    /// the very register it sourced its value from.
    #[test]
    fn mtc0_does_not_write_a_general_register() {
        let program = alloc::vec![
            addiu_zero(1, 0x55),
            cop0_word(0o04, 1, u32::from(crate::cop0::reg::COMPARE)),
        ];
        let mut bus = Ram::new(program);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        let mut pc = KSEG0_PROG;
        for _ in 0..24 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(regs.read(1), 0x55, "$1 still holds the source value");
        assert_eq!(p.cop0.read(crate::cop0::reg::COMPARE), 0x55);
    }

    /// The write-mask rules must survive the pipeline path, not just direct
    /// calls: `MTC0` to `Cause` may only touch IP1:IP0.
    #[test]
    fn a_pipelined_mtc0_still_respects_the_write_mask() {
        let program = alloc::vec![
            // ADDIU $1, $0, -1  => $1 = 0xFFFF_FFFF_FFFF_FFFF
            addiu_zero(1, 0xFFFF),
            cop0_word(0o04, 1, u32::from(crate::cop0::reg::CAUSE)),
        ];
        let mut bus = Ram::new(program);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        // Move `Compare` off `Count`'s reset value first. Both reset undefined
        // (UM §6.4.4) and we choose a deterministic zero for each, so they match
        // at power-on and latch IP7 -- see accuracy-ledger D-3. Harmless, but it
        // would show up in `Cause` here and obscure what this test is about.
        p.cop0.mtc0(crate::cop0::reg::COMPARE, 0xFFFF);
        let mut pc = KSEG0_PROG;
        for _ in 0..24 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(
            p.cop0.read(crate::cop0::reg::CAUSE),
            0x0000_0300,
            "only the software interrupt bits took"
        );
    }

    /// `LL` records `LLAddr`, which *is* COP0 register 17 — so a `MFC0` of it
    /// must see what `LL` wrote. Until COP0 existed, `LLAddr` lived on
    /// `Pipeline` as a second copy of the same architectural value; this test
    /// pins the fact that there is now only one.
    #[test]
    fn ll_writes_the_real_cop0_lladdr_register() {
        let mut p = Pipeline::new();
        let mut bus = Ram::new(alloc::vec![]);
        p.access(
            &mut bus,
            MemOp::LinkedLoad {
                kind: LoadKind::SignedWord,
                addr: 0xFFFF_FFFF_8000_0000 | 0x40,
                dest: 8,
            },
        )
        .expect("aligned");
        assert_eq!(p.ll_addr(), 0x04, "PA(31:4) of 0x40");
        assert_eq!(
            p.cop0.read(crate::cop0::reg::LL_ADDR),
            0x04,
            "the accessor and COP0 reg 17 are the same storage, not two copies"
        );
        assert_eq!(
            p.cop0.mfc0(crate::cop0::reg::LL_ADDR),
            0x04,
            "and software can read it back with MFC0"
        );
    }

    // --- exception dispatch and ERET through the pipeline (T-12-002) --------

    /// The stale-capture regression, with a *plausible* wrong answer available.
    ///
    /// The pipeline runs valid instructions first, so `ic_rf` holds a real PC
    /// when the unaligned fetch arrives. A capture taken before the latch is
    /// populated therefore reports that earlier PC — a value that looks entirely
    /// reasonable in `EPC`, which is what makes the bug survive inspection.
    #[test]
    fn an_unaligned_fetch_after_valid_ones_still_reports_its_own_address() {
        let program = alloc::vec![addiu_zero(1, 1), addiu_zero(2, 2), addiu_zero(3, 3)];
        let mut bus = Ram::new(program);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        p.cop0.set_hardware(crate::cop0::reg::STATUS, 0);
        let mut pc = KSEG0_PROG;

        // Let the pipeline fill with real instructions.
        for _ in 0..3 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert!(
            p.ic_rf.pc >= 0x800,
            "a real PC is latched to be stale about"
        );

        // Now fetch an unaligned address.
        pc = 0xFFFF_FFFF_8000_0006;
        p.advance(&mut bus, &mut regs, &mut pc);

        assert_eq!(
            p.cop0.read(crate::cop0::reg::EPC),
            0xFFFF_FFFF_8000_0006,
            "EPC must be the unaligned fetch, not the last good one"
        );
        assert_eq!(
            p.cop0.read(crate::cop0::reg::BAD_VADDR),
            0xFFFF_FFFF_8000_0006,
            "and BadVAddr likewise"
        );
    }

    /// `ERET` clears `LLbit`, completing the `LL`/`SC` contract that Sprint 1
    /// left open: until now **nothing** cleared the link, so a `LL`; `ERET`;
    /// `SC` sequence wrongly succeeded.
    #[test]
    fn eret_clears_the_link_bit_and_makes_a_following_sc_fail() {
        let mut p = Pipeline::new();
        let mut bus = ram();
        p.access(
            &mut bus,
            MemOp::LinkedLoad {
                kind: LoadKind::SignedWord,
                addr: 0xFFFF_FFFF_8000_0000 | 0x40,
                dest: 8,
            },
        )
        .expect("aligned");
        assert!(p.ll_bit(), "LL armed the link");

        // ERET: opcode 0o20, rs = 0o20 (CO), funct = 0o30.
        let word = (0o20 << 26) | (0o20 << 21) | 0o30;
        assert_eq!(decode(word).op, crate::decode::Op::Eret);

        p.cop0
            .set_hardware(crate::cop0::reg::STATUS, 1 << 1 /* EXL */);
        p.cop0
            .set_hardware(crate::cop0::reg::EPC, 0xFFFF_FFFF_8000_5000);

        let mut regs = Regs::new();
        let mut prog = Ram::new(alloc::vec![word]);
        let mut pc = KSEG0_PROG;
        for _ in 0..16 {
            p.advance(&mut prog, &mut regs, &mut pc);
        }

        assert!(!p.ll_bit(), "ERET cleared the link (UM §3.1)");
        let wb = p
            .access(
                &mut bus,
                MemOp::ConditionalStore {
                    kind: StoreKind::Word,
                    addr: 0xFFFF_FFFF_8000_0000 | 0x40,
                    value: 0xFFFF,
                    dest: 9,
                },
            )
            .expect("aligned");
        assert_eq!(
            wb,
            WriteBack::Gpr { dest: 9, value: 0 },
            "SC after ERET must fail"
        );
        assert_eq!(bus.data[0x40..0x44], [0, 0, 0, 0], "and store nothing");
    }

    /// `ERET` resumes at `EPC` and clears `EXL`, and it has **no delay slot** —
    /// the instruction after it must not execute.
    #[test]
    fn eret_resumes_at_epc_and_has_no_delay_slot() {
        let eret = (0o20 << 26) | (0o20 << 21) | 0o30;
        // ERET, then an instruction that would be a delay slot for any branch.
        // If it runs, $5 becomes 0x1234 -- which is the whole assertion.
        let program = alloc::vec![eret, addiu_zero(5, 0x1234)];
        let mut bus = Ram::new(program);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        p.cop0.set_hardware(crate::cop0::reg::STATUS, 1 << 1);
        p.cop0
            .set_hardware(crate::cop0::reg::EPC, 0xFFFF_FFFF_8000_5000);
        let mut pc = KSEG0_PROG;

        for _ in 0..12 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }

        assert_eq!(
            regs.read(5),
            0,
            "ERET has no delay slot -- the following instruction must be squashed"
        );
        assert_eq!(
            p.cop0.read(crate::cop0::reg::STATUS) & (1 << 1),
            0,
            "EXL cleared"
        );
    }

    /// A `SYSCALL` executed through the pipeline must vector, record `EPC`, and
    /// set the right `ExcCode` — the whole epilogue, end to end.
    #[test]
    fn a_syscall_vectors_and_records_its_cause() {
        // SYSCALL is SPECIAL funct 0o14.
        let program = alloc::vec![0o14];
        let mut bus = Ram::new(program);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        // BEV=0 so the vector is the RDRAM one, which is what a running game
        // uses; cold reset would otherwise send us to the boot ROM.
        p.cop0.set_hardware(crate::cop0::reg::STATUS, 0);
        let mut pc = KSEG0_PROG;

        // Stop AT the dispatch cycle. Running on would be fine architecturally
        // -- the handler starts fetching -- but then `pc` has moved past the
        // vector and asserting on it would be asserting that nothing executes.
        let mut cycles = 0;
        while p.stalled_by() != Some(Interlock::Exception) {
            p.advance(&mut bus, &mut regs, &mut pc);
            cycles += 1;
            assert!(cycles < 8, "SYSCALL never dispatched");
        }

        assert_eq!(
            (p.cop0.read(crate::cop0::reg::CAUSE) >> 2) & 0x1F,
            crate::exception::exc_code::SYS
        );
        assert_eq!(p.cop0.read(crate::cop0::reg::EPC), KSEG0_PROG);
        assert_eq!(pc, 0xFFFF_FFFF_8000_0180);
        assert_ne!(
            p.cop0.read(crate::cop0::reg::STATUS) & (1 << 1),
            0,
            "EXL set, so the handler runs in kernel mode with interrupts off"
        );
    }

    /// The epilogue must not overwrite `EPC` when `EXL` is already set — tested
    /// **through the pipeline**, not just against `dispatch` directly, because
    /// this is the failure that only shows up when handlers nest.
    #[test]
    fn a_second_exception_in_a_handler_preserves_the_first_epc() {
        let program = alloc::vec![0o14, 0o14];
        let mut bus = Ram::new(program);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        p.cop0.set_hardware(crate::cop0::reg::STATUS, 0);
        let mut pc = KSEG0_PROG;

        for _ in 0..8 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        let first_epc = p.cop0.read(crate::cop0::reg::EPC);
        assert_eq!(first_epc, KSEG0_PROG);
        assert_ne!(p.cop0.read(crate::cop0::reg::STATUS) & (1 << 1), 0);

        // Now run a second SYSCALL while EXL is still set. Point the fetch back
        // at the program so it hits the second word.
        pc = KSEG0_PROG + 4;
        for _ in 0..8 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(
            p.cop0.read(crate::cop0::reg::EPC),
            first_epc,
            "the first handler's return address must survive (UM §6.3.7)"
        );
    }

    // --- interrupts, Count/Compare (T-12-003) -------------------------------

    /// Every term of the recognition predicate is load-bearing. Dropping the
    /// `EXL`/`ERL` terms is the classic bug: it works until an interrupt arrives
    /// inside a handler, and then re-enters it forever.
    #[test]
    fn every_term_of_the_interrupt_predicate_is_required() {
        use crate::cop0::reg;
        let ready = 1u64 | (1 << 10); // IE | IM2

        let mut p = Pipeline::new();
        p.cop0.set_hardware(reg::STATUS, ready);
        p.cop0.set_ip(2, true);
        assert!(p.cop0.interrupt_pending(), "all four conditions met");

        for (name, status) in [
            ("IE clear", ready & !1),
            ("EXL set", ready | (1 << 1)),
            ("ERL set", ready | (1 << 2)),
            ("IM2 masked", ready & !(1 << 10)),
        ] {
            let mut p = Pipeline::new();
            p.cop0.set_hardware(reg::STATUS, status);
            p.cop0.set_ip(2, true);
            assert!(
                !p.cop0.interrupt_pending(),
                "{name}: the interrupt must NOT be recognised"
            );
        }
    }

    /// A masked interrupt must still be *visible* in `Cause.IP`, because
    /// software polls it. Folding assertion and recognition into one step makes
    /// a masked line invisible to `MFC0 Cause`.
    #[test]
    fn a_masked_interrupt_is_still_visible_in_cause() {
        use crate::cop0::reg;
        let mut p = Pipeline::new();
        let mut regs = Regs::new();
        let mut pc = KSEG0_PROG;
        let mut bus = AlwaysIrq;
        // IE set but IM2 MASKED, so nothing is recognised.
        p.cop0.set_hardware(reg::STATUS, 1);

        p.advance(&mut bus, &mut regs, &mut pc);
        assert_ne!(
            p.cop0.read(reg::CAUSE) & (1 << 10),
            0,
            "IP2 is asserted even though it is masked"
        );
        assert!(!p.cop0.interrupt_pending(), "but not recognised");
    }

    /// `Count` reaching `Compare` raises `IP7`, and writing `Compare` clears it
    /// as a side effect (UM §6.3.4, p. 165).
    #[test]
    fn the_timer_interrupt_sets_ip7_and_a_compare_write_clears_it() {
        use crate::cop0::reg;
        let mut p = Pipeline::new();
        let mut regs = Regs::new();
        let mut pc = KSEG0_PROG;
        let mut bus = Ram::new(alloc::vec![addiu_zero(1, 1)]);
        p.cop0.set_hardware(reg::STATUS, 0);
        p.cop0.mtc0(reg::COMPARE, 3);

        // Walk the timeline to the match.
        for now in 0..=3 {
            p.advance_at(&mut bus, &mut regs, &mut pc, now);
        }
        assert_ne!(
            p.cop0.read(reg::CAUSE) & (1 << 15),
            0,
            "IP7 set at Count==Compare"
        );

        // It must PERSIST as Count runs past Compare. A level implementation
        // clears here, and with it drops any timer interrupt raised while the
        // CPU could not accept one.
        for now in 4..20 {
            p.advance_at(&mut bus, &mut regs, &mut pc, now);
            assert_ne!(
                p.cop0.read(reg::CAUSE) & (1 << 15),
                0,
                "IP7 must stay latched past the match (UM §6.4.18)"
            );
        }

        // Only writing Compare clears it.
        p.cop0.mtc0(reg::COMPARE, 999);
        assert_eq!(
            p.cop0.read(reg::CAUSE) & (1 << 15),
            0,
            "writing Compare clears the timer interrupt"
        );
        p.advance_at(&mut bus, &mut regs, &mut pc, 20);
        assert_eq!(p.cop0.read(reg::CAUSE) & (1 << 15), 0, "and it stays clear");
    }

    /// The convenience [`Pipeline::advance`] **holds** the `Count` timeline
    /// rather than guessing a rate for it.
    ///
    /// `Count` runs at half `PClock`, so stepping it once per `advance` would run
    /// the timer at double rate — and halving it with a parity bit would be a
    /// second incremented counter, exactly what ADR 0006 forbids. Anything that
    /// exercises `Count` must use `advance_at`.
    #[test]
    fn the_convenience_advance_holds_the_count_timeline() {
        let mut p = Pipeline::new();
        let mut regs = Regs::new();
        let mut pc = KSEG0_PROG;
        let mut bus = Ram::new(alloc::vec![]);

        p.advance_at(&mut bus, &mut regs, &mut pc, 7);
        assert_eq!(p.cop0.read(crate::cop0::reg::COUNT), 7);
        for _ in 0..10 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(
            p.cop0.read(crate::cop0::reg::COUNT),
            7,
            "held, not advanced at PClock rate"
        );
    }

    /// **Why `IP7` must latch.** A timer interrupt that fires while the CPU
    /// cannot accept one — `EXL` set, i.e. inside a handler — must still be
    /// waiting when the handler returns.
    ///
    /// With `IP7` modelled as a level tied to `Count == Compare`, the equality
    /// holds for a single tick, so the interrupt is silently **lost**. That is
    /// the failure the latch prevents, and it is invisible to any test that only
    /// checks the match cycle itself.
    #[test]
    fn a_timer_interrupt_raised_while_exl_is_set_survives_until_eret() {
        use crate::cop0::reg;
        let mut p = Pipeline::new();
        let mut regs = Regs::new();
        let mut pc = KSEG0_PROG;
        let mut bus = Ram::new(alloc::vec![]);
        // IE and IM7 set, but EXL set too: a handler is running.
        p.cop0.set_hardware(reg::STATUS, 1 | (1 << 15) | (1 << 1));
        p.cop0.mtc0(reg::COMPARE, 3);

        for now in 0..=10 {
            p.advance_at(&mut bus, &mut regs, &mut pc, now);
        }
        assert!(
            !p.cop0.interrupt_pending(),
            "EXL blocks it, correctly, for now"
        );
        assert_ne!(
            p.cop0.read(reg::CAUSE) & (1 << 15),
            0,
            "but IP7 is still asserted, waiting"
        );

        // The handler returns.
        let status = p.cop0.read(reg::STATUS);
        p.cop0.set_hardware(reg::STATUS, status & !(1 << 1));
        assert!(
            p.cop0.interrupt_pending(),
            "and the timer interrupt is taken now, not dropped"
        );
    }

    /// An interrupt taken through the pipeline runs the whole epilogue: `EXL`
    /// set, `ExcCode` = 0, vectored.
    #[test]
    fn an_accepted_interrupt_vectors_with_exccode_zero() {
        use crate::cop0::reg;
        let mut p = Pipeline::new();
        let mut regs = Regs::new();
        let mut pc = KSEG0_PROG;
        let mut bus = AlwaysIrq;
        p.cop0.set_hardware(reg::STATUS, 1 | (1 << 10));

        let mut cycles = 0;
        while p.stalled_by() != Some(Interlock::Exception) {
            p.advance(&mut bus, &mut regs, &mut pc);
            cycles += 1;
            assert!(cycles < 12, "the interrupt was never taken");
        }
        assert_eq!(
            (p.cop0.read(reg::CAUSE) >> 2) & 0x1F,
            crate::exception::exc_code::INT
        );
        assert_ne!(p.cop0.read(reg::STATUS) & (1 << 1), 0, "EXL set");
        assert_eq!(pc, 0xFFFF_FFFF_8000_0180);
        // And now that EXL is set, the still-asserted line must NOT re-enter.
        assert!(
            !p.cop0.interrupt_pending(),
            "EXL blocks re-entry while the handler runs"
        );
    }

    // --- the TLB through the pipeline (T-12-004) ---------------------------

    /// Install a 4 KiB global mapping `vaddr` -> `pfn`, valid and writable.
    fn map(p: &mut Pipeline, index: u64, vaddr: u64, pfn: u64) {
        use crate::cop0::reg;
        p.cop0.set_hardware(reg::PAGE_MASK, 0);
        p.cop0.set_hardware(reg::ENTRY_HI, vaddr & 0xFFFF_E000);
        // V | D | C=3 | G, in both halves so the entry is global.
        p.cop0
            .set_hardware(reg::ENTRY_LO0, (pfn << 6) | (3 << 3) | 0b111);
        p.cop0
            .set_hardware(reg::ENTRY_LO1, ((pfn + 1) << 6) | (3 << 3) | 0b111);
        p.cop0.set_hardware(reg::INDEX, index);
        p.tlb.write_entry(index as usize, &p.cop0);
    }

    /// A KUSEG access with no mapping raises a **refill**, which takes the
    /// refill vector — not the general one.
    #[test]
    fn an_unmapped_kuseg_access_takes_the_refill_vector() {
        use crate::cop0::reg;
        let mut p = Pipeline::new();
        let mut regs = Regs::new();
        let mut bus = Ram::new(alloc::vec![lui_kseg0(1), ld_st(0o43, 0, 3, 0x100)]);
        // BEV=0, EXL=0 so the refill vector is the RDRAM one.
        p.cop0.set_hardware(reg::STATUS, 0);
        let mut pc = KSEG0_PROG;

        let mut cycles = 0;
        while p.stalled_by() != Some(Interlock::Exception) {
            p.advance(&mut bus, &mut regs, &mut pc);
            cycles += 1;
            assert!(cycles < 16, "no exception raised");
        }
        assert_eq!(
            (p.cop0.read(reg::CAUSE) >> 2) & 0x1F,
            crate::exception::exc_code::TLBL,
            "a load miss is TLBL"
        );
        assert_eq!(
            pc, 0xFFFF_FFFF_8000_0000,
            "the REFILL vector (0x000), not the general one (0x180)"
        );
        assert_eq!(p.cop0.read(reg::BAD_VADDR), 0x100);
    }

    /// A mapped, valid page translates end to end through a real load.
    #[test]
    fn a_mapped_page_translates_a_real_load() {
        let mut p = Pipeline::new();
        let mut regs = Regs::new();
        // Map KUSEG page-pair 0 with pfn 0 (even) / 1 (odd), so the even page is
        // an identity mapping onto the small test RAM. Note 0x1000 and 0x0000
        // are the SAME pair -- VPN2 tags at 8 KiB granularity, not 4 KiB.
        let mut bus = Ram::new(alloc::vec![ld_st(0o43, 0, 3, 0x100)]);
        bus.write_u8(0x100, 0xAB);
        bus.write_u8(0x101, 0xCD);
        bus.write_u8(0x102, 0xEF);
        bus.write_u8(0x103, 0x01);
        map(&mut p, 0, 0x1000, 0);
        p.cop0.set_hardware(crate::cop0::reg::STATUS, 0);
        let mut pc = KSEG0_PROG;

        for _ in 0..40 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(
            regs.read(3),
            crate::alu::sext32(0xABCD_EF01),
            "the even page of pair 0 maps to physical 0x100 via the TLB"
        );
    }

    /// A store to a page whose `D` bit is clear raises **Modified**, which takes
    /// the general vector — an entry was found, so there is nothing to refill.
    #[test]
    fn a_store_to_a_clean_page_raises_modified_at_the_general_vector() {
        use crate::cop0::reg;
        let mut p = Pipeline::new();
        let mut regs = Regs::new();
        let mut bus = Ram::new(alloc::vec![ld_st(0o53, 0, 0, 0x1100)]);
        // V | C=3 | G but NOT D -- readable, not writable.
        p.cop0.set_hardware(reg::PAGE_MASK, 0);
        p.cop0.set_hardware(reg::ENTRY_HI, 0x1000);
        p.cop0.set_hardware(reg::ENTRY_LO0, (3 << 3) | 0b011);
        p.cop0
            .set_hardware(reg::ENTRY_LO1, (1 << 6) | (3 << 3) | 0b011);
        p.tlb.write_entry(0, &p.cop0);
        p.cop0.set_hardware(reg::STATUS, 0);
        let mut pc = KSEG0_PROG;

        let mut cycles = 0;
        while p.stalled_by() != Some(Interlock::Exception) {
            p.advance(&mut bus, &mut regs, &mut pc);
            cycles += 1;
            assert!(cycles < 16, "no exception raised");
        }
        assert_eq!(
            (p.cop0.read(reg::CAUSE) >> 2) & 0x1F,
            crate::exception::exc_code::MOD
        );
        assert_eq!(
            pc, 0xFFFF_FFFF_8000_0180,
            "Modified takes the GENERAL vector"
        );
    }

    /// A TLB exception fills `EntryHi`, `Context` and `XContext` — the refill
    /// handler reads `Context` as a ready-made page-table pointer, which is why
    /// hardware assembles it rather than leaving it to software.
    #[test]
    fn a_tlb_exception_assembles_entryhi_and_context() {
        use crate::cop0::reg;
        let mut p = Pipeline::new();
        let mut regs = Regs::new();
        let mut bus = Ram::new(alloc::vec![ld_st(0o43, 0, 3, 0x4000)]);
        p.cop0.set_hardware(reg::STATUS, 0);
        // A page-table base the handler would have set up.
        p.cop0.set_hardware(reg::CONTEXT, 0xFFFF_FFFF_8080_0000);
        let mut pc = KSEG0_PROG;

        let mut cycles = 0;
        while p.stalled_by() != Some(Interlock::Exception) {
            p.advance(&mut bus, &mut regs, &mut pc);
            cycles += 1;
            assert!(cycles < 16, "no exception raised");
        }
        assert_eq!(
            p.cop0.read(reg::ENTRY_HI) & 0xFFFF_E000,
            0x4000,
            "EntryHi holds the faulting VPN2"
        );
        assert_eq!(
            p.cop0.read(reg::CONTEXT) & 0xFFFF_FFFF_FF80_0000,
            0xFFFF_FFFF_8080_0000,
            "PTEBase is preserved"
        );
        assert_eq!(
            (p.cop0.read(reg::CONTEXT) >> 4) & 0x7_FFFF,
            0x4000 >> 13,
            "BadVPN2 is filled in"
        );
    }

    /// `TLBWI` writes the entry `Index` names; `TLBWR` writes the one `Random`
    /// names. Using the wrong register is a silent, hard-to-see swap.
    #[test]
    fn tlbwi_uses_index_and_tlbwr_uses_random() {
        use crate::cop0::reg;
        const TLBWI: u32 = (0o20 << 26) | (0o20 << 21) | 0o02;
        const TLBWR: u32 = (0o20 << 26) | (0o20 << 21) | 0o06;

        let mut p = Pipeline::new();
        let mut regs = Regs::new();
        let mut bus = Ram::new(alloc::vec![TLBWI]);
        p.cop0.set_hardware(reg::STATUS, 0);
        p.cop0.set_hardware(reg::ENTRY_HI, 0x2000);
        p.cop0
            .set_hardware(reg::ENTRY_LO0, (7 << 6) | (3 << 3) | 0b111);
        p.cop0
            .set_hardware(reg::ENTRY_LO1, (8 << 6) | (3 << 3) | 0b111);
        p.cop0.set_hardware(reg::INDEX, 5);
        let mut pc = KSEG0_PROG;
        for _ in 0..16 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(p.tlb.entry(5).lo0.pfn, 7, "TLBWI wrote entry Index = 5");

        // TLBWR with Random forced to a different index.
        let mut p = Pipeline::new();
        let mut regs = Regs::new();
        let mut bus = Ram::new(alloc::vec![TLBWR]);
        p.cop0.set_hardware(reg::STATUS, 0);
        p.cop0.set_hardware(reg::ENTRY_HI, 0x2000);
        p.cop0
            .set_hardware(reg::ENTRY_LO0, (9 << 6) | (3 << 3) | 0b111);
        p.cop0
            .set_hardware(reg::ENTRY_LO1, (10 << 6) | (3 << 3) | 0b111);
        p.cop0.set_hardware(reg::INDEX, 5);
        p.cop0.set_hardware(reg::RANDOM, 20);
        let mut pc = KSEG0_PROG;
        for _ in 0..16 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(p.tlb.entry(20).lo0.pfn, 9, "TLBWR wrote entry Random = 20");
        assert_ne!(p.tlb.entry(5).lo0.pfn, 9, "and NOT entry Index");
    }

    /// `TLBWR` cannot reach a wired entry, because `Random` never goes below
    /// `Wired` — but **`TLBWI` can** (UM §5.4.4, p. 150). Guarding both is a
    /// natural-looking mistake that makes wired entries unwritable at all.
    #[test]
    fn tlbwi_can_overwrite_a_wired_entry_even_though_tlbwr_cannot() {
        use crate::cop0::reg;
        const TLBWI: u32 = (0o20 << 26) | (0o20 << 21) | 0o02;
        let mut p = Pipeline::new();
        let mut regs = Regs::new();
        let mut bus = Ram::new(alloc::vec![TLBWI]);
        p.cop0.set_hardware(reg::STATUS, 0);
        p.cop0.mtc0(reg::WIRED, 8);
        p.cop0.set_hardware(reg::ENTRY_HI, 0x2000);
        p.cop0
            .set_hardware(reg::ENTRY_LO0, (11 << 6) | (3 << 3) | 0b111);
        p.cop0
            .set_hardware(reg::ENTRY_LO1, (12 << 6) | (3 << 3) | 0b111);
        p.cop0.set_hardware(reg::INDEX, 3); // inside the wired range
        let mut pc = KSEG0_PROG;
        for _ in 0..16 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(
            p.tlb.entry(3).lo0.pfn,
            11,
            "TLBWI must be able to write a wired entry"
        );

        // And Random's range protects those entries from TLBWR structurally,
        // rather than by a check.
        for _ in 0..200 {
            p.cop0.tick_random();
            assert!(
                p.cop0.read(reg::RANDOM) >= 8,
                "Random must never select a wired entry"
            );
        }
    }

    /// A TLB fault on a sign-extended kernel address must record the **`R`**
    /// region in `EntryHi`, not just `VPN2`.
    ///
    /// Leaving `R` zero puts every such fault in region 0, so the handler's
    /// `TLBWR` installs an entry that can never match the address that faulted —
    /// an infinite refill loop, not a visibly wrong value.
    #[test]
    fn a_fault_on_a_sign_extended_address_records_its_region_in_entryhi() {
        use crate::cop0::reg;
        let mut p = Pipeline::new();
        let mut regs = Regs::new();
        // LW $3, 0($1) with $1 = 0xFFFF_FFFF_E000_0000 (KSEG3, mapped).
        let mut bus = Ram::new(alloc::vec![
            (0o17 << 26) | (1 << 16) | 0xE000, // LUI $1, 0xE000
            ld_st(0o43, 1, 3, 0),
        ]);
        p.cop0.set_hardware(reg::STATUS, 0);
        let mut pc = KSEG0_PROG;

        let mut cycles = 0;
        while p.stalled_by() != Some(Interlock::Exception) {
            p.advance(&mut bus, &mut regs, &mut pc);
            cycles += 1;
            assert!(cycles < 20, "no TLB exception raised");
        }
        assert_eq!(
            p.cop0.read(reg::BAD_VADDR),
            0xFFFF_FFFF_E000_0000,
            "the full sign-extended address faulted"
        );
        assert_eq!(
            (p.cop0.read(reg::ENTRY_HI) >> 62) & 0b11,
            0b11,
            "EntryHi.R must carry the faulting region, not 0"
        );
        assert_eq!(
            p.cop0.read(reg::ENTRY_HI) & crate::tlb::VPN2_MASK,
            0xFFFF_FFFF_E000_0000 & crate::tlb::VPN2_MASK,
            "and VPN2 alongside it"
        );
    }

    /// **`Random` advances as instructions retire** (UM §5.4.2, p. 147).
    ///
    /// It was implemented and never called from the pipeline, so it sat at 31
    /// forever and every `TLBWR` overwrote the same entry. A stuck counter is
    /// invisible to any test that calls `tick_random` itself — which is what the
    /// COP0 unit tests do — so this asserts it through `advance`.
    #[test]
    fn random_advances_as_instructions_retire() {
        use crate::cop0::reg;
        let mut p = Pipeline::new();
        let mut regs = Regs::new();
        let mut bus = Ram::new(alloc::vec![
            addiu_zero(1, 1),
            addiu_zero(2, 2),
            addiu_zero(3, 3),
            addiu_zero(4, 4),
        ]);
        p.cop0.set_hardware(reg::STATUS, 0);
        p.cop0.mtc0(reg::WIRED, 0);
        let start = p.cop0.read(reg::RANDOM);
        let mut pc = KSEG0_PROG;
        for _ in 0..24 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert!(p.retired >= 4, "instructions retired");
        assert_ne!(
            p.cop0.read(reg::RANDOM),
            start,
            "Random must move as instructions retire -- a stuck Random makes \
             every TLBWR overwrite the same entry"
        );
    }

    /// TLB shutdown must reach **`Status.TS`**, not just an internal flag —
    /// software polls `TS` precisely to discover that the TLB has died.
    #[test]
    fn tlb_shutdown_sets_status_ts() {
        use crate::cop0::reg;
        let mut p = Pipeline::new();
        let mut regs = Regs::new();
        let mut bus = Ram::new(alloc::vec![ld_st(0o43, 0, 3, 0x100)]);
        p.cop0.set_hardware(reg::STATUS, 0);
        // Two coinciding entries.
        map(&mut p, 0, 0x0000, 0);
        map(&mut p, 7, 0x0000, 4);
        assert_eq!(p.cop0.read(reg::STATUS) & (1 << 21), 0, "TS clear so far");

        let mut pc = KSEG0_PROG;
        for _ in 0..24 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert!(p.tlb.is_shutdown(), "the duplicate was noticed");
        assert_ne!(
            p.cop0.read(reg::STATUS) & (1 << 21),
            0,
            "Status.TS must be set (UM Fig. 6-6) -- an internal flag nobody can \
             read is worse than not tracking it"
        );
    }

    /// The 3-PCycle micro-ITLB reload is charged **only when a reload happens**
    /// (UM §4.6.2). A fetch that misses both levels goes straight to its
    /// exception rather than paying for a reload that never occurred.
    ///
    /// **This test cannot currently observe the charge itself.** `stall_for`
    /// replaces any pending stall, so the exception's 2-PCycle stall supersedes
    /// a wrongly-charged 3-PCycle reload in the same cycle — mutating the guard
    /// away produces no behavioural difference today, which mutation testing
    /// duly reported. The guard is kept because it is what the manual says and
    /// because it becomes observable the moment stalls compose rather than
    /// replace; what is asserted here is the *decision*
    /// ([`Tlb::jtlb_has_match`]) and the absence of a reload, not the timing.
    #[test]
    fn a_fetch_missing_both_tlb_levels_is_not_charged_for_a_reload() {
        use crate::cop0::reg;
        let mut p = Pipeline::new();
        let mut regs = Regs::new();
        let mut bus = Ram::new(alloc::vec![]);
        p.cop0.set_hardware(reg::STATUS, 0);
        // Fetch from an unmapped KUSEG address: misses ITLB and JTLB alike.
        let mut pc = 0x0000_4000u64;

        assert!(
            !p.tlb.jtlb_has_match(pc, 0),
            "the JTLB has nothing to reload the micro-TLB from"
        );
        p.advance(&mut bus, &mut regs, &mut pc);
        assert_eq!(
            (p.cop0.read(reg::CAUSE) >> 2) & 0x1F,
            crate::exception::exc_code::TLBL,
            "it went straight to the refill exception"
        );
    }

    // --- COP1 control and coprocessor usability (T-12-006) -----------------

    /// The exact instruction n64-systemtest dies on: `CTC1 $rt, $31`, its fourth
    /// statement. If this does not work the suite reports nothing at all, and
    /// every COP0/TLB test in Sprint 2 is unreachable behind it.
    #[test]
    fn ctc1_to_fcr31_works_which_is_what_unblocks_the_oracle() {
        use crate::cop0::reg;
        // CTC1: opcode 0o21, rs = 0o06, rt = GPR, rd = fs.
        const fn ctc1(rt: u32, fs: u32) -> u32 {
            (0o21 << 26) | (0o06 << 21) | (rt << 16) | (fs << 11)
        }
        const fn cfc1(rt: u32, fs: u32) -> u32 {
            (0o21 << 26) | (0o02 << 21) | (rt << 16) | (fs << 11)
        }
        //   LUI   $1, 0x0100    ; bit 24 -- flush_denorm_to_zero
        //   ORI   $1, $1, 0x800 ; bit 11 -- enable_invalid_operation
        //   CTC1  $1, $31
        //   CFC1  $2, $31
        let prog = alloc::vec![
            (0o17 << 26) | (1 << 16) | 0x0100,
            (0o15 << 26) | (1 << 21) | (1 << 16) | 0x0800,
            ctc1(1, 31),
            cfc1(2, 31),
        ];
        let mut bus = Ram::new(prog);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        // CU1 enabled, as IPL3 leaves it (Status = 0x3400_0000).
        p.cop0.set_hardware(reg::STATUS, 0x3400_0000);
        let mut pc = KSEG0_PROG;
        for _ in 0..40 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }

        let want = (1u64 << 24) | (1 << 11);
        assert_eq!(u64::from(p.cop1.fcsr()), want, "CTC1 reached FCR31");
        assert_eq!(regs.read(2), want, "and CFC1 read it back");
        assert!(p.cop1.flush_denorm_to_zero());
    }

    /// With `CU1` clear, a COP1 instruction raises **Coprocessor Unusable** with
    /// `Cause.CE = 1` — not Reserved Instruction, which is the natural mistake
    /// for an unimplemented encoding.
    #[test]
    fn a_cop1_instruction_with_cu1_clear_raises_coprocessor_unusable() {
        use crate::cop0::reg;
        const CTC1: u32 = (0o21 << 26) | (0o06 << 21) | (1 << 16) | (31 << 11);
        let mut bus = Ram::new(alloc::vec![CTC1]);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        // Kernel mode, but CU1 CLEAR.
        p.cop0.set_hardware(reg::STATUS, 0);
        let mut pc = KSEG0_PROG;

        let mut cycles = 0;
        while p.stalled_by() != Some(Interlock::Exception) {
            p.advance(&mut bus, &mut regs, &mut pc);
            cycles += 1;
            assert!(cycles < 16, "no exception raised");
        }
        assert_eq!(
            (p.cop0.read(reg::CAUSE) >> 2) & 0x1F,
            crate::exception::exc_code::CPU,
            "Coprocessor Unusable, not Reserved Instruction"
        );
        assert_eq!(
            (p.cop0.read(reg::CAUSE) >> 28) & 0b11,
            1,
            "Cause.CE names the offending unit"
        );
        assert_eq!(p.cop1.fcsr(), 0, "and the write did not take effect");
    }

    /// **COP0 is usable from kernel mode regardless of `CU0`.** Otherwise the CPU
    /// could not run an exception handler before `Status` had been set up — a
    /// chicken-and-egg the hardware does not have.
    #[test]
    fn cop0_is_usable_in_kernel_mode_even_with_cu0_clear() {
        use crate::cop0::reg;
        const MTC0: u32 = (0o20 << 26) | (0o04 << 21) | (1 << 16) | ((reg::COMPARE as u32) << 11);
        let mut bus = Ram::new(alloc::vec![addiu_zero(1, 0x77), MTC0]);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        // KSU = 0 (kernel), CU0 clear, EXL/ERL clear.
        p.cop0.set_hardware(reg::STATUS, 0);
        let mut pc = KSEG0_PROG;
        for _ in 0..24 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(
            p.cop0.read(reg::COMPARE),
            0x77,
            "MTC0 must work in kernel mode without CU0"
        );
        assert_eq!(
            (p.cop0.read(reg::CAUSE) >> 2) & 0x1F,
            0,
            "and must not have raised"
        );
    }

    /// In **user** mode with `CU0` clear, COP0 *is* unusable — otherwise the
    /// kernel-mode exemption would be a blanket bypass rather than a rule.
    #[test]
    fn cop0_is_unusable_in_user_mode_without_cu0() {
        use crate::cop0::reg;
        const MTC0: u32 = (0o20 << 26) | (0o04 << 21) | (1 << 16) | ((reg::COMPARE as u32) << 11);
        let mut bus = Ram::new(alloc::vec![MTC0]);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        // KSU = 2 (user), CU0 clear, EXL/ERL clear.
        p.cop0.set_hardware(reg::STATUS, 0b10 << 3);
        // The program cannot live in KSEG0 here: that segment does not exist in
        // User mode, so the FETCH would raise AdEL and the test would pass on
        // the wrong exception. Map page-pair 0 identically instead and run from
        // USEG, which is the only place a user program can be.
        map(&mut p, 0, 0, 0);
        let mut pc = KSEG0_PROG & 0x1FFF_FFFF;

        let mut cycles = 0;
        while p.stalled_by() != Some(Interlock::Exception) {
            p.advance(&mut bus, &mut regs, &mut pc);
            cycles += 1;
            assert!(cycles < 16, "no exception raised");
        }
        assert_eq!(
            (p.cop0.read(reg::CAUSE) >> 2) & 0x1F,
            crate::exception::exc_code::CPU
        );
        assert_eq!((p.cop0.read(reg::CAUSE) >> 28) & 0b11, 0, "unit 0");
    }

    /// A 64-bit operation is Reserved in 32-bit User or Supervisor mode, and
    /// usable everywhere else.
    ///
    /// All four rows are needed. Gating on the width bit alone reserves them for
    /// a 32-bit *kernel* — which is the mode every N64 boots into, so that
    /// mistake breaks everything and would be caught. Gating on the mode alone
    /// reserves them for a 64-bit *user* program, which nothing common does, so
    /// that mistake would sit unnoticed behind the rows that do pass.
    #[test]
    fn a_64_bit_operation_is_reserved_only_in_32_bit_non_kernel_mode() {
        use crate::cop0::reg;
        /// `DADD $2, $1, $1` — SPECIAL funct 0o54.
        const DADD: u32 = (1 << 21) | (1 << 16) | (2 << 11) | 0o54;
        /// `Status.KSU` = user, and `Status.UX`.
        const USER: u64 = 0b10 << 3;
        const UX: u64 = 1 << 5;

        for (status, want_reserved, why) in [
            (0, false, "32-bit kernel: usable"),
            (1 << 7, false, "64-bit kernel: usable"),
            (USER, true, "32-bit user: reserved"),
            (USER | UX, false, "64-bit user: usable"),
        ] {
            let mut bus = Ram::new(alloc::vec![DADD]);
            let mut regs = Regs::new();
            let mut p = Pipeline::new();
            p.cop0.set_hardware(reg::STATUS, status);
            // User mode cannot fetch from KSEG0, so the program runs from USEG
            // through an identity mapping in both cases -- keeping the only
            // difference between the rows the one under test.
            map(&mut p, 0, 0, 0);
            let mut pc = KSEG0_PROG & 0x1FFF_FFFF;

            let mut raised = false;
            for _ in 0..16 {
                p.advance(&mut bus, &mut regs, &mut pc);
                if p.stalled_by() == Some(Interlock::Exception) {
                    raised = true;
                    break;
                }
            }
            assert_eq!(raised, want_reserved, "{why}");
            if raised {
                assert_eq!(
                    (p.cop0.read(reg::CAUSE) >> 2) & 0x1F,
                    crate::exception::exc_code::RI,
                    "{why}: and it is Reserved Instruction, not something else"
                );
            }
        }
    }

    /// An unimplemented COP1 encoding with `CU1` **set** must not raise. Sprint 3
    /// then *adds* behaviour rather than changing it — and an emulator that
    /// raised here would look correct until the FPU landed.
    #[test]
    fn an_unimplemented_cop1_encoding_does_not_raise_when_cu1_is_set() {
        use crate::cop0::reg;
        // `BC1F` -- a real, valid COP1 branch that is genuinely not wired yet.
        //
        // This test has now outlived two subjects: it was `ADD.S` until the S/D
        // arithmetic landed, then `SQRT.S` until T-13-005. Each move is the
        // test doing its job -- the point is the *unimplemented* path, so the
        // subject follows whatever is still on it. When `BC1` lands, move it
        // again rather than deleting it.
        const BC1F: u32 = (0o21 << 26) | (0o10 << 21);
        assert_eq!(
            decode(BC1F).op,
            crate::decode::Op::Cop1Unimplemented,
            "valid encoding, not Reserved"
        );
        let mut bus = Ram::new(alloc::vec![BC1F]);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        p.cop0.set_hardware(reg::STATUS, 0x3400_0000); // CU1 set
        let mut pc = KSEG0_PROG;
        for _ in 0..16 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(
            (p.cop0.read(reg::CAUSE) >> 2) & 0x1F,
            0,
            "no exception with CU1 set"
        );
    }

    /// **`MOV.S` must actually move.** Decoding it is not enough — the failure
    /// this pins is a *silent no-op*, which is invisible to every test that
    /// only checks for an absent exception.
    ///
    /// The encoding is the one the correlated capture found in the delay slot
    /// of the failing n64-systemtest thunk's `jr $ra` (ledger C-10): with the
    /// move doing nothing, the callee's result never reached its caller and
    /// ~250 FP results were reported against a register the instruction under
    /// test never wrote.
    ///
    /// The destination is seeded with a value that differs from the source in
    /// **both halves**, so neither a no-op nor a half-width copy passes.
    #[test]
    fn mov_s_copies_the_low_word_and_a_no_op_would_fail_here() {
        use crate::cop0::reg;
        /// `MOV.S $f0, $f4` — fmt 16, fs 4, fd 0, funct 6.
        const MOV_S: u32 = 0x4600_2006;

        let mut bus = Ram::new(alloc::vec![MOV_S]);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        p.cop0.set_hardware(reg::STATUS, 0x3400_0000); // CU1 | FR
        p.fpr.write_raw(4, 0x0011_0011_4000_0000); // source: 2.0f
        p.fpr.write_raw(0, 0xDEAD_BEEF_1122_3344); // destination: junk

        let mut pc = KSEG0_PROG;
        for _ in 0..16 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }

        assert_eq!(
            p.fpr.read_s(0, true),
            0x4000_0000,
            "MOV.S must copy fs's low word into fd"
        );
        assert_eq!(
            (p.cop0.read(reg::CAUSE) >> 2) & 0x1F,
            0,
            "MOV.S raises nothing"
        );
    }

    /// `NEG.S` and `ABS.S` share the arm `MOV.S` was missing from, and are just
    /// as silent when absent: both leave a plausible-looking value behind.
    #[test]
    fn neg_s_and_abs_s_execute_rather_than_no_op() {
        use crate::cop0::reg;
        /// fmt 16, fs 4, fd 0.
        const fn fp(funct: u32) -> u32 {
            (0o21 << 26) | (0o20 << 21) | (4 << 11) | funct
        }

        for (funct, input, want) in [
            (7u32, 0x4000_0000u32, 0xC000_0000u32), // NEG.S: 2.0 -> -2.0
            (5, 0xC000_0000, 0x4000_0000),          // ABS.S: -2.0 -> 2.0
        ] {
            let mut bus = Ram::new(alloc::vec![fp(funct)]);
            let mut regs = Regs::new();
            let mut p = Pipeline::new();
            p.cop0.set_hardware(reg::STATUS, 0x3400_0000);
            p.fpr.write_s(4, true, input);
            p.fpr.write_s(0, true, 0x1122_3344);
            let mut pc = KSEG0_PROG;
            for _ in 0..16 {
                p.advance(&mut bus, &mut regs, &mut pc);
            }
            assert_eq!(p.fpr.read_s(0, true), want, "funct {funct} did not execute");
        }
    }

    // --- Enabled FP traps (T-13-002) ----------------------------------------

    /// `ADD.S $f4, $f0, $f2` — the encoding the FP-trap tests drive.
    const ADD_S_F4_F0_F2: u32 = 0x4602_0100;
    /// `FCSR.Enable` for Invalid Operation (bit 11).
    const ENABLE_INVALID: u32 = 1 << 11;
    /// `FCSR.Cause` for Invalid Operation (bit 16).
    const CAUSE_INVALID: u32 = 1 << 16;
    /// `FCSR.Flags` (sticky) for Invalid Operation (bit 6).
    const FLAG_INVALID: u32 = 1 << 6;

    /// Run `ADD.S $f4, $f0, $f2` on `inf + (-inf)` — an Invalid Operation —
    /// with `FCSR` preloaded, and report what the machine ended up in.
    fn run_invalid_add_s(fcsr: u32) -> (Pipeline, u64) {
        use crate::cop0::reg;
        let mut bus = Ram::new(alloc::vec![ADD_S_F4_F0_F2]);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        p.cop0.set_hardware(reg::STATUS, 0x3400_0000); // CU1 | FR
        p.cop1.ctc1(31, fcsr);
        p.fpr.write_s(0, true, 0x7F80_0000); // +inf
        p.fpr.write_s(2, true, 0xFF80_0000); // -inf
        p.fpr.write_raw(4, 0x1122_3344_5566_7788); // untouched-if-trapped marker
        let mut pc = KSEG0_PROG;
        for _ in 0..24 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        let code = (p.cop0.read(reg::CAUSE) >> 2) & 0x1F;
        (p, code)
    }

    /// With the Invalid enable **clear**, the operation completes: `fd` is
    /// written, both `Cause` and the sticky `Flags` record it, and nothing
    /// raises. This is the control for the trap test below — without it, a
    /// pipeline that raised on *every* invalid operation would pass that test.
    #[test]
    fn a_masked_fp_condition_completes_and_sets_both_cause_and_flags() {
        let (p, code) = run_invalid_add_s(0);
        assert_eq!(code, 0, "masked: no exception");
        assert_ne!(
            p.fpr.read_s(4, true),
            0x5566_7788,
            "fd must be written when no trap is taken"
        );
        let fcsr = p.cop1.fcsr();
        assert_ne!(fcsr & CAUSE_INVALID, 0, "Cause.V set");
        assert_ne!(fcsr & FLAG_INVALID, 0, "sticky Flags.V set");
    }

    /// With the enable **set**, the same operation traps: `ExcCode` is 15, `fd`
    /// keeps its old value, and `Cause` records the condition.
    #[test]
    fn an_enabled_fp_condition_raises_and_leaves_the_destination_alone() {
        use crate::cop0::reg;
        let (p, code) = run_invalid_add_s(ENABLE_INVALID);
        assert_eq!(code, crate::exception::exc_code::FPE, "ExcCode 15 (FPE)");
        assert_eq!(
            p.fpr.read_raw(4),
            0x1122_3344_5566_7788,
            "a trapped operation must not write fd"
        );
        assert_ne!(p.cop1.fcsr() & CAUSE_INVALID, 0, "Cause.V set");
        assert_eq!(
            (p.cop0.read(reg::CAUSE) >> 28) & 0b11,
            0,
            "Cause.CE is 0 for an FP exception, not the coprocessor number"
        );
    }

    /// **The sticky `Flags` field is NOT updated on a trap** — only `Cause` is.
    ///
    /// Split from the test above deliberately. Both come from the same
    /// `Flags::to_fcsr_bits` value, so writing the whole thing on the trap path
    /// is the natural implementation and is wrong; it passes every assertion
    /// about the exception itself and only shows up when `FCSR` is read back,
    /// which is exactly what n64-systemtest does.
    #[test]
    fn a_trapped_operation_does_not_accumulate_into_the_sticky_flags() {
        let (p, _) = run_invalid_add_s(ENABLE_INVALID);
        assert_eq!(
            p.cop1.fcsr() & FLAG_INVALID,
            0,
            "Flags must be left alone when the trap is taken"
        );
    }

    /// A trapped FP operation does not retire, so it must not tick `Random`.
    ///
    /// `Random` is decremented in the retirement tail of `WB`, which is also
    /// where the FP write-back happens — so an implementation that raises the
    /// exception but falls through keeps counting an instruction that never
    /// completed.
    ///
    /// **Asserted on the trap cycle specifically.** Comparing total `retired`
    /// between a trapping and a non-trapping run was tried first and proved
    /// nothing: the trap flushes the pipe and redirects `PC`, so the totals
    /// differ over a fixed cycle budget whether or not the trapping instruction
    /// itself retired. That version passed with the fix removed.
    #[test]
    fn a_trapped_fp_operation_does_not_retire() {
        use crate::cop0::reg;
        let mut bus = Ram::new(alloc::vec![ADD_S_F4_F0_F2]);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        p.cop0.set_hardware(reg::STATUS, 0x3400_0000);
        p.cop1.ctc1(31, ENABLE_INVALID);
        p.fpr.write_s(0, true, 0x7F80_0000);
        p.fpr.write_s(2, true, 0xFF80_0000);

        let mut pc = KSEG0_PROG;
        let mut saw_trap = false;
        for _ in 0..24 {
            let retired_before = p.retired;
            let random_before = p.cop0.read(reg::RANDOM);
            p.advance(&mut bus, &mut regs, &mut pc);
            let code = (p.cop0.read(reg::CAUSE) >> 2) & 0x1F;
            if !saw_trap && code == crate::exception::exc_code::FPE {
                saw_trap = true;
                assert_eq!(
                    p.retired, retired_before,
                    "the trapping instruction must not retire on the trap cycle"
                );
                assert_eq!(
                    p.cop0.read(reg::RANDOM),
                    random_before,
                    "and must not tick Random"
                );
            }
        }
        assert!(saw_trap, "no FP trap was taken -- the test proved nothing");
    }

    /// **A `MOV.S` must not disturb `FCSR.Cause`.**
    ///
    /// This is a regression test for a real defect. `MOV`/`ABS`/`NEG` were
    /// first written to clear the `Cause` field, on no evidence — and because
    /// the compiler emits `MOV.fmt` to move an FP return value, a `MOV` almost
    /// always sits between an arithmetic operation and the `CFC1` that reads
    /// its result. It therefore erased the very bits the program was about to
    /// inspect, costing 112 n64-systemtest assertions.
    ///
    /// The signature was distinctive and worth recording: the suite reported
    /// `flags: inexact` with `causes: ""` — the sticky half surviving and the
    /// per-operation half gone. That shape means *a later instruction
    /// overwrote it*, not that the flag was never raised.
    ///
    /// The sequence below is exactly the shape a compiled FP call has:
    /// arithmetic, then a move of the result.
    #[test]
    fn a_following_mov_s_leaves_the_cause_field_of_a_previous_operation_intact() {
        use crate::cop0::reg;
        /// `ADD.S $f4, $f0, $f2` then `MOV.S $f6, $f4`.
        const MOV_S_F6_F4: u32 = (0o21 << 26) | (0o20 << 21) | (4 << 11) | (6 << 6) | 6;
        /// `FCSR.Cause` for Inexact (bit 12).
        const CAUSE_INEXACT: u32 = 1 << 12;

        let mut bus = Ram::new(alloc::vec![ADD_S_F4_F0_F2, MOV_S_F6_F4]);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        p.cop0.set_hardware(reg::STATUS, 0x3400_0000);
        // f32::MAX + 1.0 -- the value is unchanged and the operation is
        // inexact, which is the n64-systemtest case from ledger C-11.
        p.fpr.write_s(0, true, f32::MAX.to_bits());
        p.fpr.write_s(2, true, 1.0f32.to_bits());

        let mut pc = KSEG0_PROG;
        for _ in 0..8 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_ne!(
            p.cop1.fcsr() & CAUSE_INEXACT,
            0,
            "ADD.S must raise Cause.Inexact in the first place"
        );
        for _ in 0..16 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(
            p.fpr.read_s(6, true),
            p.fpr.read_s(4, true),
            "the MOV.S did run"
        );
        assert_ne!(
            p.cop1.fcsr() & CAUSE_INEXACT,
            0,
            "and must not have cleared the ADD.S's Cause"
        );
    }

    /// **A later COP1 operation clears a stale `Cause.E` (bit 17).**
    ///
    /// `Cause` is bits **17:12** and is replaced wholesale by each operation.
    /// The mask here originally covered only 16:12, so the unimplemented-
    /// operation bit — which has no `Enable` and no sticky `Flags` twin, and so
    /// is only ever cleared by that mask — stayed set forever once raised.
    /// Software reading `FCSR` after a perfectly good conversion would still
    /// see the previous failure.
    ///
    /// Found by a review bot, not by this suite, which had no case that raised
    /// bit 17 and then ran another COP1 instruction.
    #[test]
    fn a_later_operation_clears_a_stale_unimplemented_cause() {
        use crate::cop0::reg;
        /// `ADD.S $f4, $f0, $f2` — an ordinary, entirely successful operation.
        const ADD_S: u32 = 0x4602_0100;
        /// `FCSR.Cause.E`, bit 17.
        const CAUSE_E: u32 = 1 << 17;

        let mut bus = Ram::new(alloc::vec![ADD_S]);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        p.cop0.set_hardware(reg::STATUS, 0x3400_0000);
        // Pre-set the bit, as a previous unimplemented operation would have.
        p.cop1.ctc1(31, CAUSE_E);
        p.fpr.write_s(0, true, 1.0f32.to_bits());
        p.fpr.write_s(2, true, 2.0f32.to_bits());

        let mut pc = KSEG0_PROG;
        for _ in 0..16 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(p.fpr.read_s(4, true), 3.0f32.to_bits(), "the ADD.S ran");
        assert_eq!(
            p.cop1.fcsr() & CAUSE_E,
            0,
            "a successful operation must clear the whole Cause field"
        );
    }

    /// `C.cond.fmt` writes `FCSR.C` and **no FPR at all**.
    ///
    /// Both halves matter. A compare that also wrote `fd` would corrupt a
    /// register the program never named, and one that computed the right
    /// condition without storing it leaves every dependent branch wrong.
    #[test]
    fn a_compare_writes_the_fcsr_condition_and_leaves_the_registers_alone() {
        use crate::cop0::reg;
        /// `FCSR.C`, bit 23.
        const FCSR_C: u32 = 1 << 23;
        /// `C.EQ.S $f0, $f2` — fmt 16, funct 0o62 (cond 2 = EQ).
        const C_EQ_S: u32 = (0o21 << 26) | (0o20 << 21) | (2 << 16) | 0o62;

        for (a, b, want) in [(1.0f32, 1.0f32, true), (1.0, 2.0, false)] {
            let mut bus = Ram::new(alloc::vec![C_EQ_S]);
            let mut regs = Regs::new();
            let mut p = Pipeline::new();
            p.cop0.set_hardware(reg::STATUS, 0x3400_0000);
            // Start with the condition at the OPPOSITE of the expected result,
            // so "wrote the right value" is distinguishable from "left it".
            p.cop1.ctc1(31, if want { 0 } else { FCSR_C });
            p.fpr.write_s(0, true, a.to_bits());
            p.fpr.write_s(2, true, b.to_bits());
            p.fpr.write_raw(4, 0xDEAD_BEEF_1122_3344);

            let mut pc = KSEG0_PROG;
            for _ in 0..16 {
                p.advance(&mut bus, &mut regs, &mut pc);
            }
            assert_eq!(p.cop1.fcsr() & FCSR_C != 0, want, "{a} == {b}");
            assert_eq!(
                p.fpr.read_raw(4),
                0xDEAD_BEEF_1122_3344,
                "a compare must not write an FPR"
            );
        }
    }

    /// **`TRUNC.W.S` takes its rounding mode from the OPCODE, not `FCSR.RM`.**
    ///
    /// This is the entire difference between the `ROUND`/`TRUNC`/`CEIL`/`FLOOR`
    /// family and `CVT.W`/`CVT.L`, and it is invisible whenever `RM` happens to
    /// agree with the opcode. So `FCSR.RM` is set to round-to-nearest and the
    /// input chosen where nearest and truncate disagree: `-1.5` truncates to
    /// `-1` and rounds to `-2`.
    ///
    /// `CVT.W.S` on the same input under the same `FCSR` must give `-2`,
    /// proving the two families really are wired differently rather than both
    /// happening to truncate.
    #[test]
    fn the_fixed_mode_conversions_ignore_fcsr_rm_and_cvt_w_honours_it() {
        use crate::cop0::reg;
        /// `TRUNC.W.S $f4, $f0` — fmt 16, `fs` 0 (the zero shift is elided),
        /// `fd` 4, funct 0o15.
        const TRUNC_W_S: u32 = (0o21 << 26) | (0o20 << 21) | (4 << 6) | 0o15;
        /// `CVT.W.S $f4, $f0` — fmt 16, `fs` 0, `fd` 4, funct 0o44.
        const CVT_W_S: u32 = (0o21 << 26) | (0o20 << 21) | (4 << 6) | 0o44;

        for (word, want) in [(TRUNC_W_S, -1i32), (CVT_W_S, -2)] {
            let mut bus = Ram::new(alloc::vec![word]);
            let mut regs = Regs::new();
            let mut p = Pipeline::new();
            p.cop0.set_hardware(reg::STATUS, 0x3400_0000);
            p.cop1.ctc1(31, 0); // RM = 0, round to nearest even
            p.fpr.write_s(0, true, (-1.5f32).to_bits());

            let mut pc = KSEG0_PROG;
            for _ in 0..16 {
                p.advance(&mut bus, &mut regs, &mut pc);
            }
            #[allow(clippy::cast_possible_wrap)] // reading the word back as signed
            let got = p.fpr.read_s(4, true) as i32;
            assert_eq!(got, want, "instruction {word:#010X} on -1.5");
        }
    }

    /// `CVT.S.W` reads its source as a **32-bit integer**, which is a different
    /// format carried in the same `fmt` field.
    ///
    /// A decoder that admits only formats 16/17 leaves every integer-to-float
    /// conversion a silent no-op, and `fd` keeps whatever it had — which looks
    /// exactly like a plausible float.
    #[test]
    fn cvt_s_w_converts_an_integer_source() {
        use crate::cop0::reg;
        /// `CVT.S.W $f4, $f0` — fmt 20 (`.W`), `fs` 0, `fd` 4, funct 0o40.
        const CVT_S_W: u32 = (0o21 << 26) | (0o24 << 21) | (4 << 6) | 0o40;

        let mut bus = Ram::new(alloc::vec![CVT_S_W]);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        p.cop0.set_hardware(reg::STATUS, 0x3400_0000);
        p.fpr.write_s(0, true, 12345u32);
        p.fpr.write_s(4, true, 0x1122_3344);

        let mut pc = KSEG0_PROG;
        for _ in 0..16 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        // Compared as BITS, not as a float: 12345.0 is exactly representable,
        // so this is the stricter check and it also catches a wrong-signed
        // zero or a NaN payload that float equality would accept.
        assert_eq!(
            p.fpr.read_s(4, true),
            12345.0f32.to_bits(),
            "the integer source must be converted, not reinterpreted"
        );
    }

    /// **`CTC1` can raise an FP exception by itself.** Writing `FCSR` with a
    /// Cause bit whose Enable is also set meets the trap condition
    /// immediately — no arithmetic has to run.
    ///
    /// The instruction must also not retire, exactly as a trapping arithmetic
    /// operation does not.
    #[test]
    fn ctc1_raises_when_it_writes_a_cause_bit_that_is_enabled() {
        use crate::cop0::reg;
        /// `CTC1 $1, $f31` — rs 0o06, rt 1, fs 31.
        const CTC1_F31: u32 = (0o21 << 26) | (0o06 << 21) | (1 << 16) | (31 << 11);
        /// `Cause.overflow` (bit 14) with `Enable.overflow` (bit 9).
        const OVERFLOW_ARMED: u64 = (1 << 14) | (1 << 9);

        let mut bus = Ram::new(alloc::vec![CTC1_F31]);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        p.cop0.set_hardware(reg::STATUS, 0x3400_0000);
        regs.write(1, OVERFLOW_ARMED);
        let mut pc = KSEG0_PROG;
        for _ in 0..24 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(
            (p.cop0.read(reg::CAUSE) >> 2) & 0x1F,
            crate::exception::exc_code::FPE,
            "the CTC1 itself must raise"
        );
        assert_eq!(
            u64::from(p.cop1.fcsr()),
            OVERFLOW_ARMED,
            "and the written value stands"
        );
    }

    /// The *enable* half is load-bearing: the same Cause bit with its Enable
    /// clear must NOT raise. Without this, an implementation that trapped on
    /// any non-zero Cause would pass the test above.
    #[test]
    fn ctc1_does_not_raise_when_the_matching_enable_is_clear() {
        use crate::cop0::reg;
        const CTC1_F31: u32 = (0o21 << 26) | (0o06 << 21) | (1 << 16) | (31 << 11);

        let mut bus = Ram::new(alloc::vec![CTC1_F31]);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        p.cop0.set_hardware(reg::STATUS, 0x3400_0000);
        // Cause.overflow set, Enable.overflow clear; an unrelated enable set.
        regs.write(1, (1 << 14) | (1 << 11));
        let mut pc = KSEG0_PROG;
        for _ in 0..24 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(
            (p.cop0.read(reg::CAUSE) >> 2) & 0x1F,
            0,
            "a Cause bit whose Enable is clear must not trap"
        );
    }

    /// **`DCFC1`/`DCTC1` and `DCFC2`/`DCTC2` decline in DIFFERENT ways.**
    ///
    /// The encodings are structurally identical — the doubleword control moves
    /// of their respective coprocessors — and it is tempting to give them one
    /// behaviour. COP1's raise a *floating-point* exception with `FCSR.Cause`
    /// set to unimplemented-operation; COP2's raise *Reserved Instruction* with
    /// `Cause.CE = 2` and do not touch `FCSR` at all.
    ///
    /// Both are tested here together precisely because treating them uniformly
    /// is the natural mistake.
    #[test]
    fn the_doubleword_control_moves_decline_differently_per_coprocessor() {
        use crate::cop0::reg;
        /// `DCFC1 $1, $f0` — COP1, rs 0o03.
        const DCFC1: u32 = (0o21 << 26) | (0o03 << 21) | (1 << 16);
        /// `DCFC2 $1, $0` — COP2, rs 0o03.
        const DCFC2: u32 = (0o22 << 26) | (0o03 << 21) | (1 << 16);

        // COP1: floating-point exception, FCSR.Cause = unimplemented ONLY.
        let mut bus = Ram::new(alloc::vec![DCFC1]);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        p.cop0.set_hardware(reg::STATUS, 0x3400_0000); // CU1 set
        // Pre-load unrelated cause bits: they must be cleared, not merged.
        p.cop1.ctc1(31, 0x0001_F000);
        let mut pc = KSEG0_PROG;
        for _ in 0..24 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(
            (p.cop0.read(reg::CAUSE) >> 2) & 0x1F,
            crate::exception::exc_code::FPE,
            "DCFC1 raises FPE"
        );
        assert_eq!(
            p.cop1.fcsr() & (0x3F << 12),
            crate::fpu::CAUSE_UNIMPLEMENTED,
            "and Cause is ONLY the unimplemented bit"
        );

        // COP2: Reserved Instruction, and Cause.CE names the coprocessor.
        let mut bus = Ram::new(alloc::vec![DCFC2]);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        // CU2 set (bit 30) as well as CU1, so this is not an unusable fault.
        p.cop0.set_hardware(reg::STATUS, 0x7400_0000);
        let mut pc = KSEG0_PROG;
        for _ in 0..24 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(
            (p.cop0.read(reg::CAUSE) >> 2) & 0x1F,
            crate::exception::exc_code::RI,
            "DCFC2 raises Reserved Instruction, not FPE"
        );
        assert_eq!(
            (p.cop0.read(reg::CAUSE) >> 28) & 0b11,
            2,
            "and Cause.CE names COP2 -- a plain RI would leave it zero"
        );
    }

    /// **A `JAL` inside another jump's delay slot links to the OUTER target
    /// + 4**, not to its own `pc + 8`.
    ///
    /// Its own delay slot never runs — the outer jump already redirected — so
    /// the instruction after it is the outer target. n64-systemtest states the
    /// rule in its assertion text: *"JAL in delay slot writes target address+4
    /// of original jump into delay slot"*.
    ///
    /// The ordinary case is asserted alongside it, because the two share one
    /// implementation and a test of only the nested case would pass with the
    /// link hard-wired to `next_pc` in a way that broke normal jumps.
    #[test]
    fn a_jal_in_a_delay_slot_links_past_the_outer_target() {
        /// `JAL <target>` — the target is a word index within the 256 MB region.
        const fn jal(target_word: u32) -> u32 {
            (0o03 << 26) | target_word
        }
        /// `J <target>`.
        const fn j(target_word: u32) -> u32 {
            (0o02 << 26) | target_word
        }

        // Ordinary: JAL at KSEG0_PROG, delay slot after it, link = pc + 8.
        {
            let outer = u32::try_from(KSEG0_PROG & 0x0FFF_FFFF).unwrap() >> 2;
            let mut bus = Ram::new(alloc::vec![jal(outer + 8), 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            let mut regs = Regs::new();
            let mut p = Pipeline::new();
            let mut pc = KSEG0_PROG;
            for _ in 0..24 {
                p.advance(&mut bus, &mut regs, &mut pc);
            }
            assert_eq!(regs.read(31), KSEG0_PROG + 8, "an ordinary JAL links pc+8");
        }

        // Nested: J to T, with a JAL in its delay slot. The JAL must link to
        // T + 4, NOT to its own address + 8.
        {
            let base = u32::try_from(KSEG0_PROG & 0x0FFF_FFFF).unwrap() >> 2;
            // T is eight words along; the JAL sits in the J's delay slot.
            let t_word = base + 8;
            let prog = alloc::vec![j(t_word), jal(base + 4), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
            let mut bus = Ram::new(prog);
            let mut regs = Regs::new();
            let mut p = Pipeline::new();
            let mut pc = KSEG0_PROG;
            for _ in 0..24 {
                p.advance(&mut bus, &mut regs, &mut pc);
            }
            let outer_target = KSEG0_PROG + 8 * 4;
            assert_eq!(
                regs.read(31),
                outer_target + 4,
                "the nested JAL links to the OUTER target + 4"
            );
            assert_ne!(
                regs.read(31),
                KSEG0_PROG + 4 + 8,
                "and specifically NOT to its own pc + 8"
            );
        }
    }

    /// **COP2 is one 64-bit latch, not a register file.**
    ///
    /// The register index is ignored entirely: n64-systemtest writes with one
    /// index and reads back with several others — including 30 and 31 — and
    /// gets the same value every time. `MTC2` writes all 64 bits despite being
    /// nominally a 32-bit move; `MFC2` returns the low half sign-extended and
    /// `DMFC2` the whole thing.
    ///
    /// The index-independence is the assertion that matters: a real 32-entry
    /// register file passes a write-then-read-same-index test perfectly.
    #[test]
    fn cop2_is_a_single_latch_whose_register_index_is_ignored() {
        use crate::cop0::reg;
        /// `MTC2 $1, $5` — COP2 rs 0o04, rt 1, rd 5.
        const MTC2_R1_TO_5: u32 = (0o22 << 26) | (0o04 << 21) | (1 << 16) | (5 << 11);
        /// `DMFC2 $2, $30` — rs 0o01, rt 2, rd 30. A DIFFERENT index.
        const DMFC2_30_TO_R2: u32 = (0o22 << 26) | (0o01 << 21) | (2 << 16) | (30 << 11);
        /// `MFC2 $3, $31` — rs 0o00 (so no `rs` term), rt 3, rd 31. Another
        /// different index again.
        const MFC2_31_TO_R3: u32 = (0o22 << 26) | (3 << 16) | (31 << 11);

        let mut bus = Ram::new(alloc::vec![MTC2_R1_TO_5, DMFC2_30_TO_R2, MFC2_31_TO_R3]);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        // CU2 usable, or these fault before reaching the latch.
        p.cop0.set_hardware(reg::STATUS, 0x5000_0000);
        regs.write(1, 0x0123_4567_89AB_CDEF);

        let mut pc = KSEG0_PROG;
        for _ in 0..32 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(
            regs.read(2),
            0x0123_4567_89AB_CDEF,
            "DMFC2 from a DIFFERENT index reads all 64 bits MTC2 wrote"
        );
        assert_eq!(
            regs.read(3),
            0xFFFF_FFFF_89AB_CDEF,
            "MFC2 from another index reads the low half, sign-extended"
        );
    }

    // --- Unimplemented operation on subnormals (T-13-004) -------------------

    /// `ADD.S $f4, $f0, $f2`.
    const ADD_S_SUB: u32 = 0x4602_0100;
    /// `FCSR.Cause.E`, bit 17 — unmaskable unimplemented operation.
    const CAUSE_E: u32 = 1 << 17;
    /// `FCSR.FS`, bit 24 — flush denormals to zero.
    const FCSR_FS: u32 = 1 << 24;

    /// Run `ADD.S $f4, $f0, $f2` with the given operands and `FCSR`.
    fn run_add_s(fcsr: u32, a: u32, b: u32) -> Pipeline {
        use crate::cop0::reg;
        let mut bus = Ram::new(alloc::vec![ADD_S_SUB]);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        p.cop0.set_hardware(reg::STATUS, 0x3400_0000);
        p.cop1.ctc1(31, fcsr);
        p.fpr.write_s(0, true, a);
        p.fpr.write_s(2, true, b);
        p.fpr.write_raw(4, 0x1122_3344_5566_7788);
        let mut pc = KSEG0_PROG;
        for _ in 0..24 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        p
    }

    /// A **subnormal operand** raises unimplemented operation before the
    /// arithmetic is attempted — even with `FS` set, which does not rescue it.
    ///
    /// `FCSR` must end up with bit 17 and *nothing else*: no Invalid, no
    /// Inexact, and the sticky `Flags` untouched.
    #[test]
    fn a_subnormal_operand_raises_unimplemented_operation() {
        use crate::cop0::reg;
        let subnormal = 1u32; // the smallest positive subnormal
        for fcsr in [0, FCSR_FS] {
            let p = run_add_s(fcsr, subnormal, 2.0f32.to_bits());
            assert_eq!(
                (p.cop0.read(reg::CAUSE) >> 2) & 0x1F,
                crate::exception::exc_code::FPE,
                "fcsr={fcsr:#X}"
            );
            assert_ne!(p.cop1.fcsr() & CAUSE_E, 0, "Cause.E set");
            assert_eq!(
                p.cop1.fcsr() & !(CAUSE_E | FCSR_FS),
                0,
                "bit 17 and nothing else -- no flags, no other causes"
            );
            assert_eq!(
                p.fpr.read_raw(4),
                0x1122_3344_5566_7788,
                "fd must be untouched"
            );
        }
    }

    /// A **subnormal result** with `FS` clear is equally refused. The operands
    /// here are both normal, so this is the result path and not the operand
    /// one — the two are separate checks and a test using a subnormal input
    /// would pass with the result check deleted.
    #[test]
    fn a_subnormal_result_raises_unimplemented_when_fs_is_clear() {
        use crate::cop0::reg;
        let a = 1.528_510_4e-37f32;
        let b = -1.539_154_3e-37f32;
        assert!(!crate::fpu::is_subnormal_f32(a), "operands are normal");
        assert!(!crate::fpu::is_subnormal_f32(b));

        let p = run_add_s(0, a.to_bits(), b.to_bits());
        assert_eq!(
            (p.cop0.read(reg::CAUSE) >> 2) & 0x1F,
            crate::exception::exc_code::FPE
        );
        assert_ne!(p.cop1.fcsr() & CAUSE_E, 0);
    }

    /// **An underflow that rounds past the subnormal grid to ZERO is refused
    /// too**, not just one that lands on a subnormal.
    ///
    /// `MIN_POSITIVE * MIN_POSITIVE` is about `1.4e-76`, far below `f32`'s
    /// smallest subnormal, so the rounded result is plain zero and
    /// `is_subnormal` is false for it. Testing only that condition let every
    /// such case through silently — worth 22 oracle assertions.
    ///
    /// The converse matters as much and is covered by
    /// `a_subnormal_result_raises_unimplemented_when_fs_is_clear`: IEEE signals
    /// underflow only when tiny **and inexact**, so an *exact* subnormal has
    /// `underflow` clear. Neither condition implies the other; the
    /// implementation needs both.
    #[test]
    fn an_underflow_that_reaches_zero_is_refused_as_well() {
        use crate::cop0::reg;
        /// `MUL.S $f4, $f0, $f2` — fmt 16, funct 2.
        const MUL_S: u32 = (0o21 << 26) | (0o20 << 21) | (2 << 16) | (4 << 6) | 2;

        let mut bus = Ram::new(alloc::vec![MUL_S]);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        p.cop0.set_hardware(reg::STATUS, 0x3400_0000);
        p.fpr.write_s(0, true, f32::MIN_POSITIVE.to_bits());
        p.fpr.write_s(2, true, f32::MIN_POSITIVE.to_bits());

        let mut pc = KSEG0_PROG;
        for _ in 0..24 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_ne!(
            p.cop1.fcsr() & CAUSE_E,
            0,
            "an underflow to zero must raise unimplemented with FS clear"
        );
    }

    /// With `FS` set, the same operation **flushes** — and where it flushes to
    /// depends on the rounding mode. These are n64-systemtest's own vectors.
    ///
    /// Round-to-nearest and toward-zero give a signed zero; a mode that rounds
    /// *away* from zero must give the smallest **normal** instead, because zero
    /// is on the wrong side of the true result. Getting that wrong yields `-0`
    /// in all four cases, which looks entirely reasonable.
    #[test]
    fn with_fs_set_a_subnormal_result_flushes_per_rounding_mode() {
        let a = 1.528_510_4e-37f32.to_bits();
        let b = (-1.539_154_3e-37f32).to_bits();
        // (RM, expected) -- the true result is a tiny NEGATIVE subnormal.
        for (rm, want) in [
            (0u32, (-0.0f32).to_bits()),         // nearest
            (1, (-0.0f32).to_bits()),            // toward zero
            (2, (-0.0f32).to_bits()),            // toward +inf
            (3, (-f32::MIN_POSITIVE).to_bits()), // toward -inf: away from zero
        ] {
            let p = run_add_s(FCSR_FS | rm, a, b);
            assert_eq!(p.fpr.read_s(4, true), want, "RM={rm}");
            let fcsr = p.cop1.fcsr();
            assert_ne!(fcsr & (1 << 13), 0, "Cause.underflow, RM={rm}");
            assert_ne!(fcsr & (1 << 12), 0, "Cause.inexact, RM={rm}");
            assert_eq!(fcsr & CAUSE_E, 0, "not unimplemented, RM={rm}");
        }
        // ...and the mirrored operands flush the other way.
        let p = run_add_s(
            FCSR_FS | 2,
            (-1.528_510_4e-37f32).to_bits(),
            1.539_154_3e-37f32.to_bits(),
        );
        assert_eq!(
            p.fpr.read_s(4, true),
            f32::MIN_POSITIVE.to_bits(),
            "a positive tiny result under toward-+inf"
        );
    }

    /// **`FS` set but underflow enabled is unimplemented, not a trap.** The
    /// processor cannot deliver a trapped underflow's defined result either.
    ///
    /// Easy to miss because it is the interaction of two features that each
    /// work: flushing works, and enabled traps work, but together they do not.
    #[test]
    fn fs_plus_an_enabled_underflow_is_unimplemented_rather_than_a_trap() {
        let a = 1.528_510_4e-37f32.to_bits();
        let b = (-1.539_154_3e-37f32).to_bits();
        // bit 8 = enable underflow, bit 7 = enable inexact.
        for enable in [1u32 << 8, 1 << 7] {
            let p = run_add_s(FCSR_FS | enable, a, b);
            assert_ne!(
                p.cop1.fcsr() & CAUSE_E,
                0,
                "enable={enable:#X} must give unimplemented"
            );
        }
    }

    /// **The two NaN classes trap differently.** MSB clear (quiet by the
    /// VR4300's convention) is unimplemented; MSB set (signalling) is Invalid.
    ///
    /// Swapping them is invisible until `FCSR` is read back, and both are
    /// "the operation trapped", so a test asserting only that would pass either
    /// way. See ledger C-12.
    #[test]
    fn the_two_nan_classes_raise_different_causes() {
        let msb_clear = 0x7F80_0001u32; // unimplemented here
        let msb_set = 0x7FC0_0001u32; // signalling here -> Invalid

        let p = run_add_s(0, msb_clear, 2.0f32.to_bits());
        assert_ne!(p.cop1.fcsr() & CAUSE_E, 0, "MSB clear -> unimplemented");
        assert_eq!(p.cop1.fcsr() & (1 << 16), 0, "and NOT invalid");

        let p = run_add_s(0, msb_set, 2.0f32.to_bits());
        assert_ne!(p.cop1.fcsr() & (1 << 16), 0, "MSB set -> Cause.invalid");
        assert_eq!(p.cop1.fcsr() & CAUSE_E, 0, "and NOT unimplemented");
    }

    /// **`ABS`/`NEG` classify their operand; `MOV` does not.** All three look
    /// like sign/bit manipulation, and only `MOV` actually is.
    ///
    /// n64-systemtest settles it by construction rather than description:
    /// `MOV.S` is driven through the cause-*preserving* harness while `ABS.S`
    /// and `NEG.S` go through the ordinary one. Treating all three alike was
    /// worth 52 assertions.
    #[test]
    fn abs_and_neg_refuse_a_subnormal_but_mov_moves_it() {
        use crate::cop0::reg;
        /// fmt 16, `fs` 0, `fd` 4.
        const fn unary(funct: u32) -> u32 {
            (0o21 << 26) | (0o20 << 21) | (4 << 6) | funct
        }
        let subnormal = 1u32;

        for funct in [5u32, 7] {
            let mut bus = Ram::new(alloc::vec![unary(funct)]);
            let mut regs = Regs::new();
            let mut p = Pipeline::new();
            p.cop0.set_hardware(reg::STATUS, 0x3400_0000);
            p.fpr.write_s(0, true, subnormal);
            let mut pc = KSEG0_PROG;
            for _ in 0..24 {
                p.advance(&mut bus, &mut regs, &mut pc);
            }
            assert_ne!(
                p.cop1.fcsr() & CAUSE_E,
                0,
                "funct {funct} (ABS/NEG) must refuse a subnormal"
            );
        }

        // MOV.S is a pure move: it transports the subnormal untouched and
        // raises nothing at all.
        let mut bus = Ram::new(alloc::vec![unary(6)]);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        p.cop0.set_hardware(reg::STATUS, 0x3400_0000);
        p.fpr.write_s(0, true, subnormal);
        let mut pc = KSEG0_PROG;
        for _ in 0..24 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(
            p.fpr.read_s(4, true),
            subnormal,
            "MOV.S moves the subnormal"
        );
        assert_eq!(p.cop1.fcsr(), 0, "and raises nothing");
    }

    /// **A `.L` conversion refuses a magnitude of `2^53` or more**, which is
    /// far narrower than `i64`.
    ///
    /// The threshold is bracketed rather than assumed: `9007198717870080`
    /// converts and `9007199254740992` (`2^53`) does not, and both are
    /// comfortably inside `i64`. A test using only a huge value would pass with
    /// the limit set anywhere between `2^53` and `i64::MAX`.
    ///
    /// The same limit applies to `.W`, where it is **unobservable**: `2^53` is
    /// far outside `i32`, so both paths end in unimplemented. Guarding it on
    /// the target width was tried and removed — a branch no test can
    /// distinguish is a branch that will rot.
    #[test]
    fn a_long_conversion_refuses_two_to_the_fifty_three_but_not_just_below() {
        use crate::cop0::reg;
        /// `CVT.L.D $f4, $f0` — fmt 17 (`.D`), `fs` 0, `fd` 4, funct 0o45.
        const CVT_L_D: u32 = (0o21 << 26) | (0o21 << 21) | (4 << 6) | 0o45;

        for (src, want_unimplemented) in [
            (9_007_198_717_870_080.0f64, false),
            (9_007_199_254_740_992.0f64, true), // 2^53
            (-9_007_199_254_740_992.0f64, true),
            (-9_007_198_717_870_080.0f64, false),
        ] {
            let mut bus = Ram::new(alloc::vec![CVT_L_D]);
            let mut regs = Regs::new();
            let mut p = Pipeline::new();
            p.cop0.set_hardware(reg::STATUS, 0x3400_0000);
            p.fpr.write_d(0, true, src.to_bits());
            let mut pc = KSEG0_PROG;
            for _ in 0..24 {
                p.advance(&mut bus, &mut regs, &mut pc);
            }
            let raised = p.cop1.fcsr() & CAUSE_E != 0;
            assert_eq!(raised, want_unimplemented, "{src:e}");
            if !want_unimplemented {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let want = src as i64 as u64;
                assert_eq!(p.fpr.read_d(4, true), want, "{src:e} must convert");
            }
        }
    }

    /// An out-of-range float-to-integer conversion is **unimplemented**, not
    /// Invalid. IEEE says Invalid; this processor declines instead, and
    /// `fpu::to_i32` reports the IEEE answer that must be translated.
    #[test]
    fn an_out_of_range_integer_conversion_is_unimplemented_not_invalid() {
        use crate::cop0::reg;
        /// `CVT.W.S $f4, $f0` — fmt 16, `fs` 0, `fd` 4, funct 0o44.
        const CVT_W_S: u32 = (0o21 << 26) | (0o20 << 21) | (4 << 6) | 0o44;

        for src in [1e30f32, f32::INFINITY, f32::from_bits(0x7FC0_0000)] {
            let mut bus = Ram::new(alloc::vec![CVT_W_S]);
            let mut regs = Regs::new();
            let mut p = Pipeline::new();
            p.cop0.set_hardware(reg::STATUS, 0x3400_0000);
            p.fpr.write_s(0, true, src.to_bits());
            let mut pc = KSEG0_PROG;
            for _ in 0..24 {
                p.advance(&mut bus, &mut regs, &mut pc);
            }
            assert_ne!(p.cop1.fcsr() & CAUSE_E, 0, "{src:e} -> unimplemented");
            assert_eq!(p.cop1.fcsr() & (1 << 16), 0, "{src:e} -> NOT invalid");
            assert_eq!(
                (p.cop0.read(reg::CAUSE) >> 2) & 0x1F,
                crate::exception::exc_code::FPE
            );
        }
    }

    // --- CACHE (T-12-005) ---------------------------------------------------

    /// `CACHE` must **not** raise. IPL3 and libdragon both issue it, so a
    /// reserved-instruction exception here blocks every real ROM — which is why
    /// this was called out as a hard blocker before it was implemented.
    #[test]
    fn cache_executes_instead_of_raising() {
        use crate::cop0::reg;
        // CACHE op=0, 0($1) with $1 = KSEG0 base.
        let prog = alloc::vec![lui_kseg0(1), ld_st(0o57, 1, 0, 0x100)];
        assert_eq!(decode(prog[1]).op, crate::decode::Op::Cache);
        let mut bus = Ram::new(prog);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        p.cop0.set_hardware(reg::STATUS, 0);
        let mut pc = KSEG0_PROG;
        for _ in 0..24 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(
            (p.cop0.read(reg::CAUSE) >> 2) & 0x1F,
            0,
            "CACHE must not raise"
        );
        assert_eq!(p.stalled_by(), None, "and must not be stuck in an epilogue");
    }

    /// `CACHE`'s `rt` slot is the **operation selector**, not a destination.
    /// Decoding it as a load would clobber whichever GPR the cache-op encoding
    /// happens to name — a spectacularly confusing bug, since the register
    /// destroyed depends on which cache operation was requested.
    #[test]
    fn cache_writes_no_general_register() {
        // op = 0b10101 = 21, which as a destination would be $21.
        let word = ld_st(0o57, 1, 21, 0);
        let d = decode(word);
        assert_eq!(d.op, crate::decode::Op::Cache);
        assert_eq!(d.dest, 0, "rt is the cache operation, not a destination");

        let prog = alloc::vec![lui_kseg0(1), addiu_zero(21, 0x33), word];
        let mut bus = Ram::new(prog);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        p.cop0.set_hardware(crate::cop0::reg::STATUS, 0);
        let mut pc = KSEG0_PROG;
        for _ in 0..32 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(regs.read(21), 0x33, "$21 must survive CACHE op 21");
    }

    /// An **`Index_*`** `CACHE` op addresses the cache by index and **must not
    /// translate**, so it cannot fault however unmapped the address is.
    ///
    /// This matters at boot: cache-initialisation code walks every index with an
    /// arbitrary base address, and translating would raise a TLB refill on the
    /// first one — before any mapping exists to satisfy it.
    #[test]
    fn an_index_cache_op_never_faults_however_unmapped_the_address() {
        use crate::cop0::reg;
        for op in [0u16, 4, 8] {
            // op4..2 = 0, 1, 2 -> Index_Invalidate / Load_Tag / Store_Tag.
            assert!((op >> 2) < 3, "op {op} must be an Index form");
            let mut bus = Ram::new(alloc::vec![ld_st(0o57, 0, u32::from(op), 0x4000)]);
            let mut regs = Regs::new();
            let mut p = Pipeline::new();
            p.cop0.set_hardware(reg::STATUS, 0);
            let mut pc = KSEG0_PROG;
            for _ in 0..24 {
                p.advance(&mut bus, &mut regs, &mut pc);
            }
            assert_eq!(
                (p.cop0.read(reg::CAUSE) >> 2) & 0x1F,
                0,
                "Index op {op} must not raise -- it never consults the TLB"
            );
        }
    }

    /// A **`Hit_*`** `CACHE` op translates, so it raises a TLB fault on an
    /// unmapped address — it is defined in terms of *"the specified address"*.
    #[test]
    fn a_hit_cache_op_on_an_unmapped_address_faults() {
        use crate::cop0::reg;
        // op = 16 -> op4..2 = 4 = Hit_Invalidate.
        let mut bus = Ram::new(alloc::vec![ld_st(0o57, 0, 16, 0x4000)]);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        p.cop0.set_hardware(reg::STATUS, 0);
        let mut pc = KSEG0_PROG;

        let mut cycles = 0;
        while p.stalled_by() != Some(Interlock::Exception) {
            p.advance(&mut bus, &mut regs, &mut pc);
            cycles += 1;
            assert!(cycles < 16, "CACHE did not translate");
        }
        assert_eq!(
            (p.cop0.read(reg::CAUSE) >> 2) & 0x1F,
            crate::exception::exc_code::TLBL
        );
    }

    // --- FP register file, moves and loads/stores (T-13-001) ---------------

    /// `MTC1` then `MFC1` round-trips through the real FP register file, and
    /// `MTC1` must **not** write a general register — `rt` is its source.
    #[test]
    fn mtc1_then_mfc1_round_trips_and_writes_no_gpr() {
        use crate::cop0::reg;
        const fn cop1(rs: u32, rt: u32, fs: u32) -> u32 {
            (0o21 << 26) | (rs << 21) | (rt << 16) | (fs << 11)
        }
        //   ADDIU $1, $0, 0x55
        //   MTC1  $1, $f4
        //   MFC1  $2, $f4
        let prog = alloc::vec![addiu_zero(1, 0x55), cop1(0o04, 1, 4), cop1(0o00, 2, 4)];
        let mut bus = Ram::new(prog);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        p.cop0.set_hardware(reg::STATUS, 0x3400_0000); // CU1 | FR
        let mut pc = KSEG0_PROG;
        for _ in 0..40 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(
            p.fpr.read_s(4, true),
            0x55,
            "MTC1 reached the FP register file"
        );
        assert_eq!(regs.read(2), 0x55, "and MFC1 read it back");
        assert_eq!(regs.read(1), 0x55, "$1 -- MTC1's source -- must survive");
    }

    /// **`DMFC1`/`DMTC1` apply the `FR` view**, they do not move the physical
    /// register. UM Ch. 17's pseudocode: with `FR = 0` and an even `fs`,
    /// In half mode a 64-bit access addresses **FGR `fs & !1` in its
    /// entirety**, and the odd FGR is not part of it.
    ///
    /// This test previously asserted the *pair* model — low word in the even
    /// FGR, high word in the odd one — which round-trips through
    /// `DMTC1`/`DMFC1` perfectly and is still wrong: hardware never touches
    /// the odd register. See `fpr.rs` for the n64-systemtest vector that
    /// settles it.
    #[test]
    fn dmtc1_and_dmfc1_apply_the_fr_view_rather_than_moving_the_raw_fgr() {
        use crate::cop0::reg;
        const fn cop1(rs: u32, rt: u32, fs: u32) -> u32 {
            (0o21 << 26) | (rs << 21) | (rt << 16) | (fs << 11)
        }
        let mut p = Pipeline::new();
        // CU1 set, FR CLEAR -- the paired view.
        p.cop0.set_hardware(reg::STATUS, 1 << 29);
        // DMTC1 $1, $f2 with $1 = 0x1122_3344_5566_7788.
        let prog = alloc::vec![cop1(0o05, 1, 2), cop1(0o01, 2, 2)];
        let mut bus = Ram::new(prog);
        let mut regs = Regs::new();
        regs.write(1, 0x1122_3344_5566_7788);
        let mut pc = KSEG0_PROG;
        for _ in 0..32 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }

        // All 64 bits in FGR 2; FGR 3 is not involved.
        assert_eq!(
            p.fpr.read_raw(2),
            0x1122_3344_5566_7788,
            "the whole value lives in the even FGR"
        );
        assert_eq!(p.fpr.read_raw(3), 0, "the odd FGR is untouched");
        assert_eq!(regs.read(2), 0x1122_3344_5566_7788, "DMFC1 reassembles it");
        // And it agrees with the LDC1/SDC1 view of the same register.
        assert_eq!(p.fpr.read_d(2, false), 0x1122_3344_5566_7788);
    }

    /// `SDC1` then `LDC1` round-trips a double through memory, and with
    /// `FR = 0` the value lives in an **FGR pair** — so this exercises the view
    /// that a direct-index register file gets wrong.
    #[test]
    fn ldc1_and_sdc1_round_trip_a_double_with_fr_clear() {
        use crate::cop0::reg;
        //   LUI  $1, 0x8000
        //   SDC1 $f2, 0x100($1)
        //   LDC1 $f4, 0x100($1)
        let prog = alloc::vec![
            lui_kseg0(1),
            ld_st(0o75, 1, 2, 0x100),
            ld_st(0o65, 1, 4, 0x100),
        ];
        let mut bus = Ram::new(prog);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        // CU1 set, FR CLEAR -- the paired view.
        p.cop0.set_hardware(reg::STATUS, 1 << 29);
        p.fpr.write_d(2, false, 0x0123_4567_89AB_CDEF);
        let mut pc = KSEG0_PROG;
        for _ in 0..48 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(
            p.fpr.read_d(4, false),
            0x0123_4567_89AB_CDEF,
            "the double survived memory in the FR = 0 paired view"
        );
        writeback(&mut p, &mut bus, 0xFFFF_FFFF_8000_0100);
        assert_eq!(
            bus.read_u32(0x100),
            0x0123_4567,
            "big-endian high word first"
        );
    }

    /// FP loads and stores obey the same alignment rules as the integer forms —
    /// `LDC1` needs 8-byte alignment, and a misaligned one raises `AdEL`.
    #[test]
    fn a_misaligned_ldc1_raises_an_address_error() {
        use crate::cop0::reg;
        let prog = alloc::vec![lui_kseg0(1), ld_st(0o65, 1, 4, 0x104)];
        let mut bus = Ram::new(prog);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        p.cop0.set_hardware(reg::STATUS, 1 << 29);
        let mut pc = KSEG0_PROG;

        let mut cycles = 0;
        while p.stalled_by() != Some(Interlock::Exception) {
            p.advance(&mut bus, &mut regs, &mut pc);
            cycles += 1;
            assert!(cycles < 24, "no address error raised");
        }
        assert_eq!(
            (p.cop0.read(reg::CAUSE) >> 2) & 0x1F,
            crate::exception::exc_code::ADEL,
            "a misaligned LDC1 is a load address error"
        );
    }

    /// The FP moves and loads need `CU1` like every other COP1 instruction.
    #[test]
    fn fp_loads_need_cu1() {
        use crate::cop0::reg;
        let prog = alloc::vec![lui_kseg0(1), ld_st(0o61, 1, 4, 0x100)];
        let mut bus = Ram::new(prog);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        p.cop0.set_hardware(reg::STATUS, 0); // CU1 CLEAR
        let mut pc = KSEG0_PROG;

        let mut cycles = 0;
        while p.stalled_by() != Some(Interlock::Exception) {
            p.advance(&mut bus, &mut regs, &mut pc);
            cycles += 1;
            assert!(cycles < 24, "no exception raised");
        }
        assert_eq!(
            (p.cop0.read(reg::CAUSE) >> 2) & 0x1F,
            crate::exception::exc_code::CPU
        );
        assert_eq!((p.cop0.read(reg::CAUSE) >> 28) & 0b11, 1, "unit 1");
    }
}
