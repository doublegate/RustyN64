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

/// `beq $0, $0, -1` — a branch whose target is the branch itself.
const SELF_BRANCH: u32 = 0x1000_FFFF;
/// The delay slot an idle loop must have for the skip to be state-preserving.
const SLOT_NOP: u32 = 0x0000_0000;
/// What the skipped pair costs: one `PCycle` to issue each (`execute_one`'s base
/// charge). Neither can request a stall — a `beq` reads no memory and a `nop`
/// does nothing — so there is no variable part to model.
const IDLE_LOOP_PCYCLES: u32 = 2;
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
    ///
    /// Deliberately **carries no target**. Everything that produces this variant
    /// has already written `next_pc` (that is what redirecting *is*), so a payload
    /// would be a second copy of the same fact and a place for the two to
    /// disagree. Raised in review, which spotted the redundant write.
    Redirected,
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
        /// How many consecutive branch-in-delay-slot links to expect.
        ///
        /// The loop **terminates by construction** — every iteration retires an
        /// instruction — so this is not a safety net against an infinite loop. It
        /// is a bound on how long one call can run without returning to the
        /// scheduler, which is a *scheduling* concern rather than a correctness
        /// one: a chain of jumps each in the previous one's delay slot starves the
        /// RCP for its duration.
        ///
        /// A release build **continues past it** rather than truncating, because
        /// stopping mid-chain would execute the wrong instructions — a wrong answer
        /// is worse than a long call. The real cap belongs in the wiring slice,
        /// where the scheduler is the thing that cares. Raised in review.
        const EXPECTED_CHAIN: u32 = 64;

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

        // **The idle-loop skip** (see [`Pipeline::idle_pc`]). Reached only after the
        // NMI and interrupt checks above, so the loop is left on exactly the cycle
        // an interrupt would have left it.
        //
        // This is not an approximation of running the two instructions: `beq $0,
        // $0, -1` puts the PC back at itself and its `nop` slot does nothing, so
        // the only state either one touches is what is reproduced here. `Count` is
        // derived from the scheduler's tick and needs nothing.
        if self.idle_pc == Some(*next_pc) {
            self.retired = self.retired.wrapping_add(2);
            self.cop0.tick_random();
            self.cop0.tick_random();
            return IDLE_LOOP_PCYCLES;
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

        let mut links = 0u32;

        loop {
            let (c, flow) = self.execute_one(bus, regs, pc, in_delay_slot, link, next_pc);
            cost = cost.saturating_add(c);
            match flow {
                Flow::Redirected => return cost,
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
                    links += 1;
                    debug_assert!(
                        links < EXPECTED_CHAIN,
                        "{links} consecutive branches in delay slots: correct, but \
                         this call has not returned to the scheduler in a long time"
                    );
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
                return (cost.saturating_add(c), Flow::Redirected);
            }
        };
        // A micro-ITLB reload charged by `fetch_word` (UM §4.6.2).
        cost = cost.saturating_add(self.take_stall_cost());

        self.recognize_idle_loop(bus, word, pc, in_delay_slot);

        let decoded = decode(word);
        if let Err(exc) = self.ex_gate(decoded) {
            let c = self.vector_to(exc, pc, in_delay_slot, 0, next_pc);
            return (cost.saturating_add(c), Flow::Redirected);
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
                return (cost.saturating_add(c), Flow::Redirected);
            }
        };
        // Multiply and divide (UM Table 3-12), raised by `execute` itself.
        cost = cost.saturating_add(e.stall_cycles);

        // Census BEFORE the latch is built: the question this answers is how
        // often building it is avoidable at all.
        #[cfg(feature = "work-counters")]
        self.count_commit(e.cop0, e.mem, e.write_back);

        let write_back = self.apply_cop2(decoded, target, e.write_back);

        // ---- The commit fast path ----
        //
        // **96.81% of retired instructions commit a GPR or nothing** and touch no
        // COP0/COP1 access and no memory operation (`work_bench`'s commit census
        // on Super Mario 64, 221 M instructions). For those, everything the slow
        // path below does reduces to one register write plus the retirement tail,
        // and the 120-byte `Latch` exists only to carry it there.
        //
        // **What this deliberately does NOT duplicate.** Every subtle case —
        // COP0 writes, the TLB instructions, COP1 control and arithmetic with
        // their traps, `HI`/`LO`, and anything with a `DC` access — falls through
        // to `wb_stage` exactly as before. The logic reproduced here is
        // `regs.write(dest, value)` and the two retirement counters, which is
        // small enough that it cannot silently disagree with `wb_stage` about
        // semantics it never implements.
        //
        // The retirement tail is the load-bearing part and is easy to miss:
        // `retired` feeds the golden-log comparison, and `tick_random` advances
        // COP0 `Random` — which "decrements as each instruction executes"
        // (UM §5.4.2). A fast path that skipped it would leave `Random` stuck,
        // and a stuck `Random` makes every `TLBWR` overwrite the same entry. That
        // exact bug has already been shipped once here, from the other direction:
        // `tick_random` was implemented and never called.
        //
        // `self.dc_wb` is left alone. `wb_stage` clears `occupied` on every exit,
        // so it is already `false` on entry here, and the remaining fields are
        // inert while it is — nothing reads them, and a save-state restores the
        // same inert values.
        if e.cop0.is_none() && e.mem.is_none() {
            // The link value is the EX-time `next_pc` (ledger C-19), applied in
            // the same position as the slow path applies it.
            let wb = e
                .link
                .map_or(write_back, |dest| WriteBack::Gpr { dest, value: link });
            if let WriteBack::None | WriteBack::Gpr { .. } = wb {
                if let WriteBack::Gpr { dest, value } = wb {
                    // `Regs::write` discards `$zero`, so no guard here — and one
                    // must not be added, or that rule lives in two places.
                    regs.write(dest, value);
                }
                self.retired = self.retired.wrapping_add(1);
                self.cop0.tick_random();
                // No `pending` check and no FP stall cost: both are raised by
                // `wb_stage`, which cannot run for an instruction with no COP0
                // access. `ERET` is a `Cop0Access`, so it cannot arrive here
                // either.
                let flow = e.redirect.map_or(Flow::Next, |r| Flow::Branch {
                    target: r.target,
                    annul: r.nullify_delay_slot,
                });
                return (cost, flow);
            }
        }

        // ---- The slow path: the accurate path's own commit ----
        //
        // Reuse, not a shortcut: `wb_stage` and `apply_cop0_read` are the accurate
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
            write_back,
            mem: e.mem,
            cop0: e.cop0,
        };

        if let Some(op) = latch.mem {
            match self.access(bus, op) {
                Ok(wb) => latch.write_back = wb,
                Err(exc) => {
                    cost = cost.saturating_add(self.take_stall_cost());
                    let c = self.vector_to(exc, pc, in_delay_slot, self.fault_vaddr, next_pc);
                    return (cost.saturating_add(c), Flow::Redirected);
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
            return (cost.saturating_add(c), Flow::Redirected);
        }
        // FP arithmetic rate (UM Table 7-14), raised by `wb_stage`.
        cost = cost.saturating_add(self.take_stall_cost());

        // `ERET` has no delay slot (UM Ch. 16, p. 434) — alone among the control
        // transfers — so it reports `Redirected` rather than `Branch`.
        if matches!(e.cop0, Some(Cop0Access::Eret)) {
            *next_pc = exception::eret(&mut self.cop0);
            self.ll_bit = false;
            return (cost, Flow::Redirected);
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
    /// Fold the COP2 move family into a write-back.
    ///
    /// COP2 is one 64-bit latch rather than a register file, and the register
    /// index is ignored (ledger C-15's shape, twice over). Handled outside
    /// `execute` for the same reason `ex_stage` handles it: it needs the `rt`
    /// value, which `execute` cannot reach.
    fn apply_cop2(
        &mut self,
        decoded: crate::decode::Decoded,
        target: u64,
        write_back: WriteBack,
    ) -> WriteBack {
        match decoded.op {
            crate::decode::Op::Mtc2 => {
                self.cop2_latch = target;
                write_back
            }
            crate::decode::Op::Mfc2 => WriteBack::Gpr {
                dest: decoded.dest,
                value: crate::alu::sext32(self.cop2_latch as u32),
            },
            crate::decode::Op::Dmfc2 => WriteBack::Gpr {
                dest: decoded.dest,
                value: self.cop2_latch,
            },
            _ => write_back,
        }
    }

    /// Recognize a self-branch idle loop at `pc` on the way past, for
    /// [`Pipeline::idle_pc`] to skip on later iterations.
    ///
    /// The delay-slot fetch is the price of admission and is paid **once per
    /// recognition**, not per iteration. `in_delay_slot` excludes the slot
    /// itself: a `beq $0, $0, -1` sitting in some other branch's delay slot is
    /// not a loop head, and treating it as one would skip an instruction that
    /// really does execute.
    fn recognize_idle_loop<B: Bus>(
        &mut self,
        bus: &mut B,
        word: u32,
        pc: u64,
        in_delay_slot: bool,
    ) {
        if word != SELF_BRANCH || in_delay_slot || self.idle_pc.is_some() {
            return;
        }
        if self.fetch_word(bus, pc.wrapping_add(4)) == Ok(SLOT_NOP) {
            self.idle_pc = Some(pc);
        }
        // Whatever that fetch charged is dropped. The slot's real fetch happens on
        // the iteration that actually runs it, and charging both would make
        // recognition cost a PCycle the hardware does not spend.
        let _ = self.take_stall_cost();
    }

    fn vector_to(
        &mut self,
        exc: Exception,
        pc: u64,
        in_delay_slot: bool,
        bad_vaddr: u64,
        next_pc: &mut u64,
    ) -> u32 {
        // Control is leaving wherever it was, and the handler may well be what
        // rewrites the code the recognition was made against. Dropping it here
        // costs one re-recognition per idle period and removes a whole class of
        // question about how long the cached PC may live.
        self.idle_pc = None;
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

#[cfg(test)]
mod idle_loop_tests {
    use super::Bus;
    use super::{IDLE_LOOP_PCYCLES, SELF_BRANCH, SLOT_NOP};
    use crate::Cpu;

    /// A bus holding a program at address 0 and `NOP` past its end.
    struct Rom {
        words: alloc::vec::Vec<u32>,
    }
    impl Bus for Rom {
        fn read_u8(&mut self, _addr: u32) -> u8 {
            0
        }
        fn write_u8(&mut self, _addr: u32, _val: u8) {}
        fn read_u32(&mut self, addr: u32) -> u32 {
            self.words.get((addr / 4) as usize).copied().unwrap_or(0)
        }
    }

    /// A CPU parked at the idle loop, which sits at physical 0x100 (`KSEG0`).
    fn parked() -> (Cpu, Rom) {
        let mut words = alloc::vec![0u32; 0x80];
        words[0x40] = SELF_BRANCH; // 0x100
        words[0x41] = SLOT_NOP; // 0x104
        let mut cpu = Cpu::new();
        cpu.set_pc(0xFFFF_FFFF_8000_0100);
        (cpu, Rom { words })
    }

    /// **The property the whole optimization rests on:** skipping the loop must
    /// leave the machine in the state executing it would have.
    ///
    /// It compares the FULL architectural state — every GPR, the PC, `HI`/`LO`,
    /// and the COP0 file — not a summary. A test that compared only the PC would
    /// pass with `Random` frozen, which is the one field the skip has to
    /// reproduce by hand and therefore the one most likely to be wrong.
    #[test]
    fn skipping_the_idle_loop_matches_executing_it() {
        // Leg A: the recognition is defeated by clearing `idle_pc` after every
        // step, so every iteration runs the real fetch/decode/execute path.
        let (mut a, mut a_bus) = parked();
        for i in 0..64 {
            a.pipeline.idle_pc = None;
            let _ = a.step_instruction_at(&mut a_bus, i);
        }

        // Leg B: the skip is allowed to work.
        let (mut b, mut b_bus) = parked();
        for i in 0..64 {
            let _ = b.step_instruction_at(&mut b_bus, i);
        }

        assert_eq!(b.pc, a.pc, "the skip left the PC somewhere else");
        assert_eq!(b.retired, a.retired, "the skip retired a different count");
        for r in 0..32 {
            assert_eq!(b.regs.read(r), a.regs.read(r), "GPR {r} diverged");
        }
        for r in 0..32 {
            assert_eq!(
                b.pipeline.cop0.read(r),
                a.pipeline.cop0.read(r),
                "COP0 register {r} diverged — `Random` is the likely one"
            );
        }
    }

    /// The recognition is only sound because a `CACHE` operation retires it. With
    /// that hook gone this test fails, which is what makes it a guard rather than
    /// a description: mutation-checked by deleting `self.idle_pc = None` from
    /// `Pipeline::cache_op`.
    #[test]
    fn a_cache_operation_retires_the_recognition() {
        let (mut cpu, mut bus) = parked();
        let _ = cpu.step_instruction_at(&mut bus, 0);
        assert_eq!(
            cpu.pipeline.idle_pc,
            Some(0xFFFF_FFFF_8000_0100),
            "the loop was never recognized, so this test cannot see the hook"
        );
        // `CACHE 0, 0($0)` — index-invalidate on the I-cache.
        let _ = cpu.pipeline.cache_op(&mut bus, 0xFFFF_FFFF_8000_0000, 0);
        assert_eq!(
            cpu.pipeline.idle_pc, None,
            "a cache operation must retire the idle-loop recognition; without \
             this the skip could outlive the code it was made against"
        );
    }

    /// A self-branch sitting in someone else's delay slot is not a loop head, and
    /// recognizing it would skip an instruction that really does execute.
    #[test]
    fn a_self_branch_in_a_delay_slot_is_not_a_loop_head() {
        // `BEQ $0, $0, +1` at 0x100 jumps over the word after its delay slot; the
        // delay slot at 0x104 is itself a self-branch.
        let mut words = alloc::vec![0u32; 0x80];
        words[0x40] = 0x1000_0001;
        words[0x41] = SELF_BRANCH;
        let mut cpu = Cpu::new();
        cpu.set_pc(0xFFFF_FFFF_8000_0100);
        let mut bus = Rom { words };
        let _ = cpu.step_instruction_at(&mut bus, 0);
        assert_eq!(
            cpu.pipeline.idle_pc, None,
            "a self-branch in a delay slot was mistaken for an idle loop"
        );
    }

    /// `retire_idle_pairs(n)` must equal `n` trips through the per-boundary skip.
    ///
    /// The scheduler's idle batch calls it instead of stepping, so any drift here
    /// is drift in `Random` and the retired tally that no ROM test would localize.
    /// `Wired` is swept because `Random` wraps to 31 *at* `Wired` rather than at
    /// zero — a batch that crosses that wrap is exactly where a closed-form
    /// shortcut would go wrong, and the loop this asserts against is why one is
    /// not used.
    #[test]
    fn a_batch_of_idle_pairs_matches_the_same_number_of_boundaries() {
        for wired in [0u64, 1, 7, 30, 31] {
            for pairs in [1u64, 2, 31, 32, 33, 97] {
                let (mut one, mut one_bus) = parked();
                one.pipeline
                    .cop0
                    .write(super::super::super::cop0::reg::WIRED, wired);
                let (mut many, mut many_bus) = parked();
                many.pipeline
                    .cop0
                    .write(super::super::super::cop0::reg::WIRED, wired);

                // Recognize the loop on both, so the comparison starts aligned.
                let _ = one.step_instruction_at(&mut one_bus, 0);
                let _ = many.step_instruction_at(&mut many_bus, 0);

                for i in 0..pairs {
                    let _ = one.step_instruction_at(&mut one_bus, i + 1);
                }
                many.retire_idle_pairs(pairs);

                assert_eq!(
                    many.retired, one.retired,
                    "wired {wired}, {pairs} pairs: retired tally diverged"
                );
                assert_eq!(
                    many.pipeline
                        .cop0
                        .read(super::super::super::cop0::reg::RANDOM),
                    one.pipeline
                        .cop0
                        .read(super::super::super::cop0::reg::RANDOM),
                    "wired {wired}, {pairs} pairs: `Random` diverged"
                );
            }
        }
    }

    /// The skip charges what the two instructions charge. `execute_one` bills one
    /// `PCycle` to issue and neither a `beq` nor a `nop` can request a stall, so
    /// the pair is exactly two — and a wrong constant here would silently change
    /// how fast emulated time runs.
    #[test]
    fn the_skip_charges_what_the_pair_charges() {
        let (mut real, mut real_bus) = parked();
        real.pipeline.idle_pc = None;
        let first = real.step_instruction_at(&mut real_bus, 0);

        let (mut skipped, mut skipped_bus) = parked();
        let _ = skipped.step_instruction_at(&mut skipped_bus, 0); // recognizes
        let cost = skipped.step_instruction_at(&mut skipped_bus, 1); // skips
        assert_eq!(cost, IDLE_LOOP_PCYCLES);
        assert_eq!(
            cost, first,
            "the skip and the executed pair must cost the same, or emulated time \
             runs at a different rate inside an idle loop than outside it"
        );
    }
}
