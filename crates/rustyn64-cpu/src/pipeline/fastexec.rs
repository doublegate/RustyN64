//! The **instruction-granular** execution path (ADR 0013), behind `fast-exec`.
//!
//! # What this is, and what it is not
//!
//! It executes the **same instruction stream** through the **same semantics** as
//! the accurate path — `decode`, `exec::execute`, `alu`, `cop0`, `tlb`, `addr`,
//! `softfloat`, the caches, the TLB, the exception model. Nothing here reimplements
//! what an instruction computes. What it gives up is the **five-stage timing
//! model**: instead of advancing four inter-stage latches once per `PClock` (ADR
//! 0007), it runs one instruction to completion and charges it a documented cost.
//!
//! That is a relaxation ADR 0011 §5 explicitly excludes (*"a scheduling change,
//! not a change to what the CPU computes"*), which is why ADR 0013 exists to
//! authorize it separately.
//!
//! # The cost model is the existing stall requests
//!
//! This is the part worth reading twice. The accurate path already computes every
//! documented cost — it just *spends* them as cycles:
//!
//! | cost | where it already comes from |
//! | --- | --- |
//! | `MULT` 5, `DIV` 37, `DMULT` 8, `DDIV` 69 | `Executed::stall_cycles`, from [`crate::alu::muldiv_stall_cycles`] (UM Table 3-12) |
//! | FPU add 3, mul 5/8, div/sqrt 29/58 | [`crate::fpu::stall_cycles`], raised by `wb_stage` (UM Table 7-14) |
//! | instruction micro-TLB miss 3 | [`crate::tlb::ITLB_MISS_PCYCLES`], raised by `fetch_word` (UM §4.6.2) |
//! | RCP register access 22 | `Pipeline::M_RCP_REGISTER`, **measured** (accuracy ledger) |
//! | exception epilogue 2 | [`crate::exception::EPILOGUE_STALL`] (UM §4.7, p. 114) |
//! | I-cache fill 46, D-cache fill 40 | `Pipeline::M_ICACHE_FILL` / `M_DCACHE_FILL`, **fitted, not measured** (ledger C-1) |
//!
//! The last row is the one to be careful about: those two are anchors, not
//! readings, and the ledger says so. Inheriting them is still the right choice —
//! the fast path must charge what the accurate path charges or the two are not
//! comparable — but a timing result from this mode is no more trustworthy than
//! `M(RDRAM)` is, and improving that improves both modes at once.
//!
//! Note also that `M_ICACHE_FILL` is charged under `#[cfg(not(test))]`, so this
//! crate's own unit tests see free fetches while its **integration** tests (this
//! module's differential gate among them) see the real cost. That is a deliberate
//! seam in the accurate path, documented at `icache_fill`, and the fast path
//! inherits it rather than acquiring a second opinion about it.
//!
//! So the fast path **does not carry a cost table of its own**. It drains
//! [`Pipeline::stall`] after each phase and adds the cycles to the instruction's
//! total. A number that is wrong here is wrong in the accurate path too, which is
//! the only arrangement under which the two can be compared at all — and it means
//! this file invents no constant, satisfying the project's never-invent rule by
//! construction rather than by discipline.
//!
//! Per ADR 0013 §2 the tabled values are **total** issue costs, not addends to the
//! one-cycle baseline; the baseline applies to instructions with *no* table entry,
//! which is why [`Pipeline::step_instruction`] charges `1` and then adds only what
//! the stall requests report.
//!
//! # Delay slots need no new state, and that is deliberate
//!
//! A branch and its delay slot are executed **in the same call**. The alternative —
//! a `pending_redirect` field on [`Pipeline`] — would be state the fast path owns,
//! which makes ADR 0011 §4's save-state mode marker fall due and breaks the
//! save-state layout. Executing the pair together costs nothing and owes nothing.
//!
//! A delay slot that is *itself* a branch continues the loop rather than
//! recursing. MIPS calls that case UNPREDICTABLE, and the accurate path resolves it
//! through the natural pipeline flow; here the loop reproduces the same sequence.
//! It terminates because every iteration retires an instruction.
//!
//! # What is NOT modeled, and why that is the whole point
//!
//! - **No bypass network.** Sequential execution makes every operand current by
//!   construction, so `EX`'s re-resolution through `Pipeline::bypass` has nothing
//!   to correct.
//! - **No load interlock.** `LDI` exists because a load's result is not ready in
//!   time to bypass; here it always is.
//! - **No `prev_was_run` interrupt gate.** That rule (UM §4.7.1) is about the
//!   cycle *after* a stall; an instruction boundary is always a legal point.
//! - **No `flush_pending` / abort cascade.** Nothing younger has been fetched, so
//!   there is nothing to flush.
//!
//! Each of those is a *timing* structure, which is exactly the layer ADR 0013
//! relaxes. Each is also a place the two paths can legitimately disagree, which is
//! why the differential predicate is architectural state at retirement boundaries
//! rather than whole-state equality (ADR 0013 §4).

use crate::Bus;
use crate::decode::decode;
use crate::exception;
use crate::exec::{Cop0Access, WriteBack, execute};
use crate::regs::Regs;

use super::{Exception, FCSR_C, Latch, Pipeline};

/// What executing one instruction did to control flow.
enum Flow {
    /// Fall through to the next sequential address.
    Next,
    /// A taken branch or jump. The delay slot runs, then control goes to the
    /// target.
    Branch {
        /// Where control lands after the delay slot.
        target: u64,
        /// A branch-**likely** that was not taken annuls its delay slot.
        annul: bool,
    },
    /// Control has already been redirected — an exception vector, or `ERET`.
    /// Neither has a delay slot to run.
    Redirected(u64),
}

impl Pipeline {
    /// Execute one instruction (plus its delay slot, if it branches) and return
    /// the `PCycle`s it cost.
    ///
    /// `next_pc` is read for the address to execute and written with the address
    /// to execute next. `count_now` is the scheduler's derived COP0 `Count`
    /// position, passed in rather than incremented for the same reason
    /// [`Pipeline::advance_at`] takes it: ADR 0006 allows exactly one incremented
    /// counter and this is not it.
    ///
    /// The return value is a **cost**, never a position. Nothing here advances a
    /// clock; the caller decides what to do with the number.
    pub fn step_instruction<B: Bus>(
        &mut self,
        bus: &mut B,
        regs: &mut Regs,
        next_pc: &mut u64,
        count_now: u64,
    ) -> u32 {
        self.cop0.set_now(count_now);
        self.sample_interrupt_lines(bus);

        // Interrupt recognition. An instruction boundary is always a legal point,
        // so the accurate path's `prev_was_run` gate (UM §4.7.1) — which exists to
        // stop an interrupt landing on the cycle after a stall — has nothing to
        // say here. NMI is checked first and unmasked (UM §6.4.6).
        if self.nmi_pending {
            self.nmi_pending = false;
            return self.vector_to(Exception::Nmi, *next_pc, false, 0, next_pc);
        }
        if self.cop0.interrupt_pending() {
            return self.vector_to(Exception::Interrupt, *next_pc, false, 0, next_pc);
        }

        let mut cost = 0u32;
        let mut pc = *next_pc;
        // `fall` is where control goes if this instruction does not branch;
        // `link` is what `EX` would see in `next_pc` — the address **after this
        // instruction's own delay slot**, which is what a jump-and-link stores
        // (accuracy ledger C-19). For an ordinary instruction that is `pc + 8`;
        // for a delay slot whose branch targeted `T` it is `T + 4`, because the
        // delay slot's own delay slot is the instruction at `T`.
        let mut fall = pc.wrapping_add(4);
        let mut link = pc.wrapping_add(8);
        let mut in_delay_slot = false;

        loop {
            let (c, flow) = self.execute_one(bus, regs, pc, in_delay_slot, link, next_pc);
            cost = cost.saturating_add(c);
            match flow {
                Flow::Redirected(vector) => {
                    *next_pc = vector;
                    return cost;
                }
                Flow::Next => {
                    *next_pc = fall;
                    return cost;
                }
                Flow::Branch { target, annul } => {
                    if annul {
                        // A branch-likely that was not taken annuls its delay
                        // slot: the instruction is not executed at all, and
                        // control resumes past it.
                        *next_pc = target;
                        return cost;
                    }
                    // Run the delay slot, then land on the target. A delay slot
                    // that branches continues this loop, reproducing the accurate
                    // path's sequence for the UNPREDICTABLE nested case.
                    pc = fall;
                    in_delay_slot = true;
                    fall = target;
                    link = target.wrapping_add(4);
                }
            }
        }
    }

    /// Fetch, gate, execute, access memory, and commit exactly one instruction.
    ///
    /// Returns its cost and what it did to control flow. Every failure path
    /// vectors through [`Self::vector_to`] and reports [`Flow::Redirected`], so a
    /// caller cannot forget to handle an exception — there is no way to return a
    /// cost without also saying where control went.
    fn execute_one<B: Bus>(
        &mut self,
        bus: &mut B,
        regs: &mut Regs,
        pc: u64,
        in_delay_slot: bool,
        link: u64,
        next_pc: &mut u64,
    ) -> (u32, Flow) {
        // One PCycle to issue. Everything above that arrives as a stall request
        // from the shared code, which is why there is no table in this file.
        let mut cost = 1u32;

        let word = match self.fetch_word(bus, pc) {
            Ok(word) => word,
            Err(exc) => {
                cost = cost.saturating_add(self.take_stall_cost());
                let c = self.vector_to(exc, pc, in_delay_slot, pc, next_pc);
                return (cost.saturating_add(c), Flow::Redirected(*next_pc));
            }
        };
        // A micro-ITLB reload charged by `fetch_word` (UM §4.6.2).
        cost = cost.saturating_add(self.take_stall_cost());

        let decoded = decode(word);
        if let Err(exc) = self.ex_gate(decoded) {
            let c = self.vector_to(exc, pc, in_delay_slot, 0, next_pc);
            return (cost.saturating_add(c), Flow::Redirected(*next_pc));
        }

        // Operands come straight from the register file. The accurate path
        // re-resolves them through the bypass network because a producer one
        // instruction ahead has not committed yet; here it always has.
        // `source` / `target` rather than `rs_val` / `rt_val`: clippy's
        // `similar_names` rejects the pair the rest of the crate uses, and the
        // longer names are no worse here since they are used twice each.
        let source = regs.read(decoded.rs);
        let target = regs.read(decoded.rt);
        let hilo = crate::alu::HiLo {
            hi: regs.hi,
            lo: regs.lo,
        };
        // Likewise `FCSR.C`: the accurate path bypasses a compare still in flight,
        // and here the compare has already written the register.
        let fp_condition = self.cop1.fcsr() & FCSR_C != 0;

        let e = match execute(decoded, source, target, hilo, pc, fp_condition) {
            Ok(e) => e,
            Err(exc) => {
                let c = self.vector_to(exc, pc, in_delay_slot, 0, next_pc);
                return (cost.saturating_add(c), Flow::Redirected(*next_pc));
            }
        };
        // Multiply and divide (UM Table 3-12), raised by `execute` itself.
        cost = cost.saturating_add(e.stall_cycles);

        // Stage the instruction into the latch the commit path reads. This is
        // reuse, not a shortcut: `wb_stage` and `apply_cop0_read` are the accurate
        // path's own code, and giving them the same input is what makes the two
        // paths agree on COP0 writes, the TLB instructions, FP arithmetic, and
        // retirement without a second implementation to keep in step.
        let mut latch = Latch {
            occupied: true,
            pc,
            word,
            in_delay_slot,
            abort: None,
            decoded,
            rs_val: source,
            rt_val: target,
            write_back: e.write_back,
            mem: e.mem,
            cop0: e.cop0,
        };

        // COP2 is one 64-bit latch rather than a register file; the index is
        // ignored (ledger C-15's shape, twice over). Handled here for the same
        // reason `ex_stage` handles it: it needs the `rt` value and the latch, neither of
        // which `execute` can reach.
        match decoded.op {
            crate::decode::Op::Mtc2 => self.cop2_latch = target,
            crate::decode::Op::Mfc2 => {
                latch.write_back = WriteBack::Gpr {
                    dest: decoded.dest,
                    value: crate::alu::sext32(self.cop2_latch as u32),
                };
            }
            crate::decode::Op::Dmfc2 => {
                latch.write_back = WriteBack::Gpr {
                    dest: decoded.dest,
                    value: self.cop2_latch,
                };
            }
            _ => {}
        }

        if let Some(op) = latch.mem {
            match self.access(bus, op) {
                Ok(wb) => latch.write_back = wb,
                Err(exc) => {
                    cost = cost.saturating_add(self.take_stall_cost());
                    let c = self.vector_to(exc, pc, in_delay_slot, self.fault_vaddr, next_pc);
                    return (cost.saturating_add(c), Flow::Redirected(*next_pc));
                }
            }
            // An RCP register access, or any other cost `access` charged.
            cost = cost.saturating_add(self.take_stall_cost());
        }

        // The link value is the EX-time `next_pc` (ledger C-19). The accurate
        // path's delay-slot-fault special case does not arise: a delay slot that
        // faults has not been fetched yet here, because nothing runs ahead.
        if let Some(dest) = e.link {
            latch.write_back = WriteBack::Gpr { dest, value: link };
        }

        self.apply_cop0_read(&mut latch);
        self.dc_wb = latch;
        self.wb_stage(regs);
        // `wb_stage` raises FP traps (`CTC1` meeting its enable, and enabled
        // arithmetic exceptions) through `abort_from(Stage::Wb, ..)`, which leaves
        // them in `self.pending` rather than dispatching.
        if let Some(p) = self.pending.take() {
            let c = self.vector_to(p.exc, p.pc, p.in_delay_slot, p.bad_vaddr, next_pc);
            return (cost.saturating_add(c), Flow::Redirected(*next_pc));
        }
        // FP arithmetic rate (UM Table 7-14), raised by `wb_stage`.
        cost = cost.saturating_add(self.take_stall_cost());

        // `ERET` has no delay slot (UM Ch. 16, p. 434) — alone among the control
        // transfers — so it reports `Redirected` rather than `Branch`.
        if matches!(e.cop0, Some(Cop0Access::Eret)) {
            *next_pc = exception::eret(&mut self.cop0);
            self.ll_bit = false;
            return (cost, Flow::Redirected(*next_pc));
        }

        let flow = e.redirect.map_or(Flow::Next, |r| Flow::Branch {
            target: r.target,
            annul: r.nullify_delay_slot,
        });
        (cost, flow)
    }

    /// Take the exception epilogue and point `next_pc` at the vector.
    ///
    /// Calls [`crate::exception::dispatch`] directly with an explicit context
    /// rather than going through `abort_from`, which captures its context out of
    /// the latch belonging to a [`Stage`](super::Stage) — a selection that has no
    /// meaning without a pipeline.
    fn vector_to(
        &mut self,
        exc: Exception,
        pc: u64,
        in_delay_slot: bool,
        bad_vaddr: u64,
        next_pc: &mut u64,
    ) -> u32 {
        let d = exception::dispatch(&mut self.cop0, exc, pc, in_delay_slot, bad_vaddr);
        *next_pc = d.vector;
        // Discard any stall the raising code requested: the exception abandons the
        // work it was for, and the epilogue is the cost that applies (the same
        // reasoning `wb_stage` gives for a trapped FP operation).
        self.stall = None;
        d.stall_cycles
    }

    /// Convert a pending stall request into cost, clearing it.
    ///
    /// The shared code raises stalls by calling `stall_for`, because that is what
    /// the accurate path does with them. Here they are *charges*, not cycles to
    /// spend, so they must be drained — a request left in `self.stall` would be
    /// counted again by the next instruction that drains, or, worse, would make
    /// `advance_at` stall if the two modes were ever interleaved.
    fn take_stall_cost(&mut self) -> u32 {
        self.stall.take().map_or(0, |s| s.cycles)
    }
}
