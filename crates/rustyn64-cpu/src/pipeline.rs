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
use crate::decode::{Decoded, decode};
use crate::exec::{MemOp, WriteBack, execute};
use crate::mem;
use crate::regs::Regs;

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
    AddressError,
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
    /// CP0 bypass interlock (UM §4.6.9). **Cycle cost undocumented** — see
    /// `docs/accuracy-ledger.md` C-3.
    Cp0i,
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

/// The four inter-stage latches plus the pipeline control state.
#[derive(Clone, Debug, Default)]
pub struct Pipeline {
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
            ic_rf: EMPTY,
            rf_ex: EMPTY,
            ex_dc: EMPTY,
            dc_wb: EMPTY,
            stall: None,
            prev_was_run: false,
            flush_pending: false,
            retired: 0,
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
    }

    /// `WB` — commit the result and retire the instruction.
    fn wb_stage(&mut self, regs: &mut Regs) {
        if self.dc_wb.occupied && self.dc_wb.abort.is_none() {
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
        if self.prev_was_run && bus.poll_irq() {
            self.abort_from(Stage::Dc, Exception::Interrupt);
        }
        // The memory access. This is the point the scheduler interleaves the RCP
        // around -- the whole reason the pipeline is modelled at all (ADR 0007).
        let mut out = self.ex_dc;
        if out.occupied
            && out.abort.is_none()
            && let Some(op) = out.mem
        {
            match Self::access(bus, op) {
                Ok(wb) => out.write_back = wb,
                // Stamp before the latch move so the abort travels with the
                // instruction that caused it -- see `abort_from`.
                Err(exc) => {
                    self.abort_from(Stage::Dc, exc);
                    out = self.ex_dc;
                }
            }
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

    /// Perform a memory access.
    ///
    /// # Errors
    ///
    /// [`Exception::AddressError`] when an *aligned* access is misaligned. The
    /// `LWL`/`LWR` family is exempt by construction — being usable at any byte
    /// offset is the entire reason it exists.
    fn access<B: Bus>(bus: &mut B, op: MemOp) -> Result<WriteBack, Exception> {
        // TODO(T-11-003): charge the cache-miss cost (8..=9 + M PCycles for a
        // D-cache fill, UM Table 11-1) once `M` is measured -- accuracy-ledger C-1.
        match op {
            MemOp::Load { kind, addr, dest } => {
                if !kind.is_aligned(addr) {
                    return Err(Exception::AddressError);
                }
                let raw = Self::read_width(bus, addr, kind.width());
                Ok(WriteBack::Gpr {
                    dest,
                    value: kind.shape(raw),
                })
            }
            MemOp::Store { kind, addr, value } => {
                if !kind.is_aligned(addr) {
                    return Err(Exception::AddressError);
                }
                Self::write_width(bus, addr, kind.width(), value);
                Ok(WriteBack::None)
            }
            // The unaligned family accesses the ALIGNED container holding `addr`
            // and merges, so it can never raise an address error.
            MemOp::Unaligned { op, addr, rt, dest } => {
                use crate::decode::Op;
                let word_addr = addr & !3;
                let dword_addr = addr & !7;
                let byte4 = addr & 3;
                let byte8 = addr & 7;
                Ok(match op {
                    Op::Lwl | Op::Lwr => {
                        let w = bus.read_u32(word_addr as u32);
                        let v = if matches!(op, Op::Lwl) {
                            mem::lwl(rt, w, byte4)
                        } else {
                            mem::lwr(rt, w, byte4)
                        };
                        WriteBack::Gpr { dest, value: v }
                    }
                    Op::Ldl | Op::Ldr => {
                        let d = Self::read_width(bus, dword_addr, 8);
                        let v = if matches!(op, Op::Ldl) {
                            mem::ldl(rt, d, byte8)
                        } else {
                            mem::ldr(rt, d, byte8)
                        };
                        WriteBack::Gpr { dest, value: v }
                    }
                    Op::Swl | Op::Swr => {
                        let w = bus.read_u32(word_addr as u32);
                        let merged = if matches!(op, Op::Swl) {
                            mem::swl(rt, w, byte4)
                        } else {
                            mem::swr(rt, w, byte4)
                        };
                        bus.write_u32(word_addr as u32, merged);
                        WriteBack::None
                    }
                    Op::Sdl | Op::Sdr => {
                        let d = Self::read_width(bus, dword_addr, 8);
                        let merged = if matches!(op, Op::Sdl) {
                            mem::sdl(rt, d, byte8)
                        } else {
                            mem::sdr(rt, d, byte8)
                        };
                        Self::write_width(bus, dword_addr, 8, merged);
                        WriteBack::None
                    }
                    // `MemOp::Unaligned` is only ever constructed for the eight
                    // forms above.
                    _ => WriteBack::None,
                })
            }
        }
    }

    /// Read `width` big-endian bytes, right-justified.
    ///
    /// Dispatches on width so 4- and 8-byte accesses go through [`Bus::read_u32`],
    /// which `rustyn64-core` overrides with a fast RDRAM path. A byte loop would
    /// issue 4-8x more bus calls on the *most common* operations, and memory
    /// access is the hot path for a core targeting full speed
    /// (`docs/performance.md`).
    ///
    /// Alignment is **not** rechecked here. `access` has already validated it
    /// against the specific [`crate::mem::LoadKind`]/[`crate::mem::StoreKind`],
    /// and the unaligned family passes an address it has aligned down itself.
    /// Duplicating the check would put the rule in two places, where it can drift.
    fn read_width<B: Bus>(bus: &mut B, addr: u64, width: u64) -> u64 {
        match width {
            1 => u64::from(bus.read_u8(addr as u32)),
            2 => {
                (u64::from(bus.read_u8(addr as u32)) << 8)
                    | u64::from(bus.read_u8((addr + 1) as u32))
            }
            4 => u64::from(bus.read_u32(addr as u32)),
            // Big-endian: the high word is at the lower address.
            8 => {
                (u64::from(bus.read_u32(addr as u32)) << 32)
                    | u64::from(bus.read_u32((addr + 4) as u32))
            }
            _ => 0,
        }
    }

    /// Write the low `width` big-endian bytes of `value`.
    ///
    /// Width-dispatched for the same reason as [`Pipeline::read_width`].
    fn write_width<B: Bus>(bus: &mut B, addr: u64, width: u64, value: u64) {
        match width {
            1 => bus.write_u8(addr as u32, value as u8),
            2 => {
                bus.write_u8(addr as u32, (value >> 8) as u8);
                bus.write_u8((addr + 1) as u32, value as u8);
            }
            4 => bus.write_u32(addr as u32, value as u32),
            8 => {
                bus.write_u32(addr as u32, (value >> 32) as u32);
                bus.write_u32((addr + 4) as u32, value as u32);
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
            match execute(out.decoded, out.rs_val, out.rt_val, hilo, out.pc) {
                Ok(e) => {
                    out.write_back = e.write_back;
                    out.mem = e.mem;
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
            self.abort_from(Stage::Ic, Exception::AddressError);
            self.ic_rf = Latch {
                occupied: true,
                pc,
                abort: Some(Exception::AddressError),
                ..Latch::default()
            };
            // `next_pc` is deliberately NOT realigned. Rounding it down would
            // silently "fix" the faulting address and let execution continue on a
            // path hardware never takes -- turning a raised exception into a
            // wrong answer. TODO(T-11-004): redirect to the exception vector,
            // which is where hardware actually goes.
            return;
        }

        // TODO(T-11-003): fetch through the I-cache rather than straight off the
        // bus, and charge the miss cost (14..=15 + M PCycles, UM Table 11-2).
        let word = bus.read_u32(pc as u32);
        // Decode here rather than at RF: a branch must be decoded before the
        // NEXT fetch, so that fetch can be marked as its delay slot.
        //
        // A branch decoded last cycle is sitting in `ic_rf` right now, so the
        // instruction being fetched now is its delay slot. The flag then travels
        // with the latch and is never global state -- a multi-cycle stall between
        // a branch and its slot would desynchronise a global flag, which is the
        // classic bug here.
        // Check `rf_ex`, not `ic_rf`. `rf_stage` runs immediately before this in
        // the reverse cascade and has already moved the previous instruction out
        // of `ic_rf` (clearing `occupied`), so a branch fetched last cycle is in
        // `rf_ex` by now. Reading `ic_rf` here makes the flag silently always
        // false -- which is exactly what it did until the fetch trace showed it.
        let in_delay_slot = self.rf_ex.occupied && self.rf_ex.decoded.op.has_delay_slot();
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
        };
        *next_pc = pc.wrapping_add(4);
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
        let mut pc = 0x8000_0000;
        let mut regs = Regs::new();
        let mut bus = quiet();

        // Cycle 1 fetches a sentinel into ic_rf.
        p.advance(&mut bus, &mut regs, &mut pc);
        let sentinel = p.ic_rf.pc;
        assert!(p.ic_rf.occupied);
        assert_eq!(sentinel, 0x8000_0000);

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
        let mut pc = 0x8000_0000;
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
        let mut pc = 0x8000_0000;
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
        let mut p = Pipeline::new();
        let mut pc = 0x8000_0000;
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
        let mut pc = 0x8000_0000;
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
        let mut pc = 0x8000_0000;
        let mut regs = Regs::new();
        let mut bus = quiet();
        for _ in 0..4 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }

        // Abort at DC: the instruction in DC plus everything younger.
        p.abort_from(Stage::Dc, Exception::AddressError);
        let aborted_pc = p.ex_dc.pc;
        p.advance(&mut bus, &mut regs, &mut pc);

        // The causing instruction carried its abort forward into WB's latch...
        assert_eq!(
            (p.dc_wb.abort, p.dc_wb.pc),
            (Some(Exception::AddressError), aborted_pc),
            "the aborting instruction lost its flag while advancing"
        );
        // ...and the younger ones kept theirs rather than having them
        // overwritten by the latch moves.
        assert_eq!(
            p.ex_dc.abort,
            Some(Exception::AddressError),
            "a younger instruction's abort was overwritten by the cascade"
        );

        // The younger instruction that was already in flight kept its abort as it
        // advanced, rather than having it overwritten by the latch move.
        assert_eq!(
            p.rf_ex.abort,
            Some(Exception::AddressError),
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
        let mut pc = 0x8000_0000;
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
        let mut pc = 0x8000_0000;
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
        let mut pc = 0x8000_0000;
        let mut regs = Regs::new();
        let mut bus = quiet();
        for _ in 0..4 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        p.dc_wb.abort = Some(Exception::AddressError);
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
        let mut pc = 0u64;

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
        let mut pc = 0u64;
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
        let mut pc = 0u64;
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
        let mut pc = 0x8000_0002; // deliberately unaligned

        p.advance(&mut bus, &mut regs, &mut pc);

        assert_eq!(
            p.ic_rf.abort,
            Some(Exception::AddressError),
            "an unaligned fetch must raise AdEL"
        );
        assert!(
            !bus.fetched,
            "the bus must not be accessed for a bad address"
        );
        assert_eq!(pc, 0x8000_0002, "the PC must NOT be silently realigned");

        // And the faulting instruction must never retire.
        let retired = p.retired;
        for _ in 0..8 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(p.retired, retired, "a faulting fetch retired anyway");
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
        //   ADDIU $1, $0, 0x100     ; base
        //   ADDIU $2, $0, -2        ; value = 0xFFFF_FFFF_FFFF_FFFE
        //   SW    $2, 0($1)
        //   LW    $3, 0($1)         ; sign-extended  -> 0xFFFF_FFFF_FFFF_FFFE
        //   LWU   $4, 0($1)         ; zero-extended  -> 0x0000_0000_FFFF_FFFE
        //   LBU   $5, 0($1)         ; big-endian MSB -> 0xFF
        let prog = alloc::vec![
            (0o11 << 26) | (1 << 16) | 0x100,
            (0o11 << 26) | (2 << 16) | 0xFFFE,
            ld_st(0o53, 1, 2, 0),
            ld_st(0o43, 1, 3, 0),
            ld_st(0o47, 1, 4, 0),
            ld_st(0o44, 1, 5, 0),
        ];
        let mut bus = Ram::new(prog);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        let mut pc = 0x800u64;
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
        let mut pc = 0x800u64;
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
        //   ADDIU $1, $0, 0x100
        //   ADDIU $2, $0, 0x55
        //   SW    $2, 0($1)
        //   LW    $3, 0($1)      ; load
        //   ADDIU $4, $3, 1      ; uses $3 immediately -> LDI stall
        let prog = alloc::vec![
            (0o11 << 26) | (1 << 16) | 0x100,
            (0o11 << 26) | (2 << 16) | 0x55,
            ld_st(0o53, 1, 2, 0),
            ld_st(0o43, 1, 3, 0),
            (0o11 << 26) | (3 << 21) | (4 << 16) | 1,
        ];
        let mut bus = Ram::new(prog);
        let mut regs = Regs::new();
        let mut p = Pipeline::new();
        let mut pc = 0x800u64;
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
            (0o11 << 26) | (1 << 16) | 0x100,
            ld_st(0o42, 1, 3, 1),
            ld_st(0o46, 1, 3, 4),
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
        let mut pc = 0x800u64;
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
        let mut pc = 0x800u64;
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
        let mut pc = 0x800u64;
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
        let mut pc = 0x800u64;
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
        let mut pc = 0x800u64;
        for _ in 0..80 {
            p.advance(&mut bus, &mut regs, &mut pc);
        }
        assert_eq!(regs.read(31), 0x808, "JAL links PC+8, past the delay slot");
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
        let mut pc = 0x800u64;
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
        let mut pc = 0x800u64;
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
            let mut pc = 0x800u64;
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
        let mut pc = 0x800u64;

        let mut flagged = alloc::vec::Vec::new();
        for _ in 0..8 {
            p.advance(&mut bus, &mut regs, &mut pc);
            if p.ic_rf.occupied && p.ic_rf.in_delay_slot {
                flagged.push(p.ic_rf.pc);
            }
        }
        assert_eq!(
            flagged,
            alloc::vec![0x808],
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
}
