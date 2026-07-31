//! The `fast-exec` differential gate at the **CPU** level (ADR 0013 §4).
//!
//! Runs one program through both execution paths — the five-stage cascade and the
//! instruction-granular path — and requires them to agree on **architectural
//! state at instruction-retirement boundaries**.
//!
//! # Why the predicate is a projection here, not whole-state bytes
//!
//! The `fast-scheduler` gate in `rustyn64-core` compares the entire serialized
//! machine, and that is right for it: that path is tick-identical, so *everything*
//! must match. This one cannot use the same test, because the two paths are
//! deliberately not tick-identical and their **microarchitectural** state
//! therefore differs by construction:
//!
//! - the four inter-stage latches hold whatever the cascade last had in flight;
//!   the sequential path leaves only the instruction it just committed;
//! - `prev_was_run`, `flush_pending`, and the stall state exist to sequence
//!   cycles, and the sequential path has no cycles to sequence.
//!
//! None of that is in ADR 0011 §3's list of architectural state, so comparing it
//! would fail the gate for the mode working exactly as authorized. The projection
//! below is that list.
//!
//! # What is carved out, and why the carve-out is not a loophole
//!
//! COP0 `Count` is excluded (ADR 0013 §4): it is architectural state a program can
//! read with `MFC0`, and if timing differs then `Count` differs — demanding
//! equality on it would demand the very thing this mode gives up. Everything else
//! COP0 holds is compared.
//!
//! The carve-out is bounded by the programs used: none of them reads `Count`, so
//! none can branch on it, so the two runs execute the **same instruction stream**
//! and a divergence is a real disagreement rather than the legitimate
//! stream-divergence outcome ADR 0013 §4 describes. A program that *did* read
//! `Count` would need that third verdict, and is deliberately not here yet.
//!
//! Runs only with `--features fast-exec`; the file is empty otherwise.

#![cfg(feature = "fast-exec")]

use rustyn64_cpu::{Bus, Cpu};

/// Where the test programs are loaded, as a `KSEG0` virtual address.
///
/// `KSEG0` is unmapped and **cached**, so this exercises the I-cache fill path in
/// both modes rather than bypassing it — the fill is a shared side effect of
/// `fetch_word` and a place the two paths could silently disagree.
const PROG_VA: u64 = 0xFFFF_FFFF_8000_0000;

/// Backing store for both machines: flat RAM, no devices.
struct Ram {
    mem: Vec<u8>,
}

impl Ram {
    fn with_program(words: &[u32]) -> Self {
        let mut mem = vec![0u8; 0x4000];
        for (i, w) in words.iter().enumerate() {
            mem[i * 4..i * 4 + 4].copy_from_slice(&w.to_be_bytes());
        }
        Self { mem }
    }
}

impl Bus for Ram {
    fn read_u8(&mut self, addr: u32) -> u8 {
        self.mem.get(addr as usize).copied().unwrap_or(0)
    }
    fn write_u8(&mut self, addr: u32, val: u8) {
        if let Some(b) = self.mem.get_mut(addr as usize) {
            *b = val;
        }
    }
}

// ---------------------------------------------------------------------------
// Instruction encoders. Deliberately spelled out rather than pulled from a
// helper crate: a program the gate cannot read is a program nobody can debug
// when the gate fails.
// ---------------------------------------------------------------------------

const fn i_type(op: u32, rs: u32, rt: u32, imm: u16) -> u32 {
    (op << 26) | (rs << 21) | (rt << 16) | imm as u32
}
const fn r_type(rs: u32, rt: u32, rd: u32, sa: u32, funct: u32) -> u32 {
    (rs << 21) | (rt << 16) | (rd << 11) | (sa << 6) | funct
}

const fn lui(rt: u32, imm: u16) -> u32 {
    i_type(0o17, 0, rt, imm)
}
const fn ori(rt: u32, rs: u32, imm: u16) -> u32 {
    i_type(0o15, rs, rt, imm)
}
const fn addiu(rt: u32, rs: u32, imm: u16) -> u32 {
    i_type(0o11, rs, rt, imm)
}
const fn sw(rt: u32, base: u32, off: u16) -> u32 {
    i_type(0o53, base, rt, off)
}
const fn lw(rt: u32, base: u32, off: u16) -> u32 {
    i_type(0o43, base, rt, off)
}
const fn beq(rs: u32, rt: u32, off: u16) -> u32 {
    i_type(0o4, rs, rt, off)
}
const fn bne(rs: u32, rt: u32, off: u16) -> u32 {
    i_type(0o5, rs, rt, off)
}
const fn sll(rd: u32, rt: u32, sa: u32) -> u32 {
    r_type(0, rt, rd, sa, 0o0)
}
const fn addu(rd: u32, rs: u32, rt: u32) -> u32 {
    r_type(rs, rt, rd, 0, 0o41)
}
const fn subu(rd: u32, rs: u32, rt: u32) -> u32 {
    r_type(rs, rt, rd, 0, 0o43)
}
const fn and_(rd: u32, rs: u32, rt: u32) -> u32 {
    r_type(rs, rt, rd, 0, 0o44)
}
const fn xor(rd: u32, rs: u32, rt: u32) -> u32 {
    r_type(rs, rt, rd, 0, 0o46)
}
const fn slt(rd: u32, rs: u32, rt: u32) -> u32 {
    r_type(rs, rt, rd, 0, 0o52)
}
const fn mult(rs: u32, rt: u32) -> u32 {
    r_type(rs, rt, 0, 0, 0o30)
}
const fn div(rs: u32, rt: u32) -> u32 {
    r_type(rs, rt, 0, 0, 0o32)
}
const fn mfhi(rd: u32) -> u32 {
    r_type(0, 0, rd, 0, 0o20)
}
const fn mflo(rd: u32) -> u32 {
    r_type(0, 0, rd, 0, 0o22)
}
const fn jal(target_word_addr: u32) -> u32 {
    // `JAL`'s 26-bit field is the word address within the current 256 MiB region.
    // Taken as a `u32` rather than cast from the virtual address: the truncation
    // is real (a `u64` VA does not fit) and hiding it behind an `as` would put the
    // burden of remembering the region on every caller.
    (0o3 << 26) | ((target_word_addr >> 2) & 0x03FF_FFFF)
}
const fn jr(rs: u32) -> u32 {
    r_type(rs, 0, 0, 0, 0o10)
}
const fn nop() -> u32 {
    0
}

/// The architectural state ADR 0011 §3 names, minus ADR 0013 §4's timing carve-out.
///
/// A tuple of plain values rather than a struct: it is compared as a whole and
/// printed on failure, and naming thirty fields would add nothing a `{:#x?}` of the
/// difference does not already say.
#[derive(Debug, PartialEq, Eq)]
struct Architectural {
    gprs: [u64; 32],
    hi: u64,
    lo: u64,
    // **No PC at all, and that is a finding rather than a gap.**
    //
    // Neither of the two PC-like quantities is comparable across the modes:
    //
    // - `Cpu::pc` is the **`IC` fetch pointer**. In the cascade it sits up to four
    //   instructions *ahead* of the one that retired, because `IC` runs that far in
    //   front; the sequential path has nothing running ahead, so its `pc` is simply
    //   the next instruction. Different quantities.
    // - `dc_wb.pc` is not the retired instruction either. The cascade runs
    //   **backwards** (`WB` → `DC` → …), so by the end of the tick that retired
    //   instruction *k*, `dc_stage` has already moved *k + 1* into `dc_wb`. The
    //   sequential path leaves *k* there. Off by exactly one, every time.
    //
    // Both were tried, and each reported a divergence on the very first boundary of
    // a run that agrees on everything else — which is what "the carve-out is
    // transitive" (ADR 0013 §4) looks like when you meet it in code. The
    // architectural PC of a retiring instruction is simply not observable from
    // outside the pipeline in the accurate mode; `EPC` is the only place it
    // surfaces, and only on an exception, where it *is* compared below as part of
    // COP0.
    //
    // What carries the weight instead: every program here writes a **distinct
    // value to a distinct register per instruction**, so an instruction skipped,
    // repeated, or executed out of order shows up in the GPR comparison within one
    // boundary. A gate that cannot see the PC can still see that the wrong
    // instructions ran.
    retired: u64,
    /// Every COP0 register **except** `Count` (9), which ADR 0013 §4 carves out.
    cop0: Vec<(u8, u64)>,
    fgrs: [u64; 32],
    fcsr: u32,
    ram: Vec<u8>,
}

/// COP0 `Count` — register 9. The one exclusion, named rather than skipped
/// silently so a reader can see the carve-out is exactly one register wide.
const COUNT: u8 = 9;

fn project(cpu: &Cpu, ram: &Ram) -> Architectural {
    let mut gprs = [0u64; 32];
    for (i, g) in gprs.iter_mut().enumerate() {
        *g = cpu
            .regs
            .read(u8::try_from(i).expect("32 registers fit in a u8"));
    }
    let mut fgrs = [0u64; 32];
    for (i, f) in fgrs.iter_mut().enumerate() {
        // `fr = true` reads the physical register rather than the `FR = 0` pair
        // view, which is what a state comparison wants: the view is a property of
        // how a read is performed, not of what the machine holds.
        *f = cpu
            .pipeline
            .fpr
            .read_d(u8::try_from(i).expect("32 registers fit in a u8"), true);
    }
    Architectural {
        gprs,
        hi: cpu.regs.hi,
        lo: cpu.regs.lo,
        retired: cpu.retired,
        cop0: (0..32u8)
            .filter(|n| *n != COUNT)
            .map(|n| (n, cpu.pipeline.cop0.read(n)))
            .collect(),
        fgrs,
        fcsr: cpu.pipeline.cop1.fcsr(),
        ram: ram.mem.clone(),
    }
}

/// Run both paths over `program` until each has retired `target` instructions,
/// comparing the architectural projection at every retirement boundary.
///
/// Returns the total cost the fast path reported, so a caller can assert it is
/// plausible — a path that charged nothing would agree perfectly and mean nothing.
fn compare(name: &str, program: &[u32], target: u64) -> u64 {
    let mut acc_ram = Ram::with_program(program);
    let mut fast_ram = Ram::with_program(program);
    let mut acc = Cpu::new();
    let mut fast = Cpu::new();
    acc.pc = PROG_VA;
    fast.pc = PROG_VA;

    assert_eq!(
        project(&acc, &acc_ram),
        project(&fast, &fast_ram),
        "{name}: the two machines must start identical, or nothing after this means anything"
    );

    let mut cost = 0u64;
    // A budget, not a schedule. Its only job is to fail rather than hang if a
    // program never retires, so it is deliberately far above the worst real case.
    //
    // Where 400 comes from, since a bare multiplier ages badly: the accurate path
    // spends one `tick_at` per `PCycle`, and the most expensive single instruction
    // here costs roughly `DDIV` 69 + an I-cache fill 46 + a D-cache fill 40 + the
    // 5-stage fill and epilogue — call it under 200. Doubling that leaves room for
    // an instruction that acquires a new stall without anyone remembering this
    // line. Raised in review, which asked for the derivation rather than the number.
    let mut budget = target * 400;

    // **The fast path sets the pace, and the accurate path follows.** One call can
    // retire *two* instructions — a taken branch and its delay slot execute
    // together, which is what lets the sequential path carry no delay-slot state
    // (ADR 0013, `fastexec`'s module docs). So the boundaries are wherever the fast
    // path lands, and the oracle is advanced to meet them; the reverse would
    // require the fast path to stop somewhere it structurally cannot.
    while fast.retired < target {
        cost += u64::from(fast.step_instruction_at(&mut fast_ram, 0));
        while acc.retired < fast.retired {
            acc.tick_at(&mut acc_ram, 0);
            budget -= 1;
            assert!(
                budget > 0,
                "{name}: the accurate path never reached retirement {}",
                fast.retired
            );
        }
        assert_eq!(
            acc.retired, fast.retired,
            "{name}: the accurate path overshot the boundary, so the two are being \
             compared at different points in the program"
        );
        let step = fast.retired;
        let (a, f) = (project(&acc, &acc_ram), project(&fast, &fast_ram));
        assert!(
            a == f,
            "{name}: architectural state diverged after {step} retired instructions\n\
             first differing GPR: {:?}\n\
             hi/lo accurate {:#x}/{:#x} fast {:#x}/{:#x}\n\
             first differing COP0 register: {:?}",
            (0..32).find(|i| a.gprs[*i] != f.gprs[*i]).map(|i| (
                i,
                format!("{:#018x}", a.gprs[i]),
                format!("{:#018x}", f.gprs[i])
            )),
            a.hi,
            a.lo,
            f.hi,
            f.lo,
            a.cop0
                .iter()
                .zip(&f.cop0)
                .find(|(x, y)| x != y)
                .map(|(x, y)| format!("r{} accurate {:#x} fast {:#x}", x.0, x.1, y.1)),
        );
    }
    cost
}

/// Straight-line integer work: constant building, arithmetic, shifts, compares.
#[test]
fn straight_line_arithmetic_agrees() {
    let prog = [
        lui(1, 0x8000),
        ori(2, 0, 0x1234),
        addiu(3, 2, 0x1000),
        sll(4, 2, 4),
        addu(5, 2, 3),
        subu(6, 3, 2),
        and_(7, 2, 3),
        xor(8, 2, 3),
        slt(9, 2, 3),
        slt(10, 3, 2),
        nop(),
        nop(),
    ];
    let cost = compare("straight-line", &prog, 10);
    assert!(
        cost >= 10,
        "ten instructions cannot cost fewer than ten PCycles"
    );
}

/// Loads and stores, which route through `Pipeline::access` in both paths and
/// therefore through the same translation, cache, and endian handling.
#[test]
fn loads_and_stores_agree() {
    let prog = [
        lui(1, 0x8000),
        ori(2, 0, 0x5678),
        sw(2, 1, 0x200),
        lw(3, 1, 0x200),
        addiu(4, 3, 1),
        sw(4, 1, 0x204),
        lw(5, 1, 0x204),
        nop(),
        nop(),
    ];
    compare("load-store", &prog, 7);
}

/// The multiply/divide unit: the results agree, **and the documented costs are
/// actually charged**.
///
/// The cost half is measured as an **A/B difference**, not as a threshold on one
/// run. A first version asserted `cost >= 8 + 5 + 37` on the multiply program
/// alone, and it **passed with the charge deleted** — other per-instruction costs
/// (an I-cache fill on each of eight cold fetches) were enough to clear the bar on
/// their own. That is the converging-test hazard: the success and failure paths met
/// at the same number.
///
/// Two programs of the **same length** differing only in `MULT`/`DIV` versus `NOP`
/// cancel every shared cost, so what is left is exactly what the multiply and
/// divide were charged. Mutation-checked: deleting `e.stall_cycles` from the fast
/// path fails this, and the earlier threshold version did not.
#[test]
fn multiply_and_divide_agree_and_are_charged() {
    /// The two programs, identical except for the two instructions under test.
    /// `MULT`/`DIV` are replaced by `NOP`, so both retire the same count and fetch
    /// the same number of words.
    const fn program(with_muldiv: bool) -> [u32; 10] {
        [
            ori(2, 0, 0x0123),
            ori(3, 0, 0x0045),
            if with_muldiv { mult(2, 3) } else { nop() },
            mflo(4),
            mfhi(5),
            if with_muldiv { div(2, 3) } else { nop() },
            mflo(6),
            mfhi(7),
            nop(),
            nop(),
        ]
    }

    let with = compare("muldiv", &program(true), 8);
    let without = compare("muldiv-baseline", &program(false), 8);

    // UM Table 3-12: `MULT` 5, `DIV` 37. Asserted as `==` rather than `>=` because
    // the two programs are otherwise identical, so anything else in the difference
    // would be a cost that depends on the *operand* rather than the operation —
    // which this model does not have and should be told about if it acquires one.
    assert_eq!(
        with - without,
        5 + 37,
        "the MULT (5) and DIV (37) costs are not what the fast path charged: \
         {with} with them, {without} without"
    );
}

/// Branches and their delay slots — the case that needed no new pipeline state,
/// and the one most likely to diverge if the sequential ordering is wrong.
///
/// The program is chosen so the **not-taken path is unreachable**: the skipped
/// instruction writes a sentinel no other instruction produces, so a fast path that
/// mishandled the delay slot would show it in `$9` rather than converging on the
/// same answer. A control-flow test whose branch target equals the sequential path
/// proves nothing.
#[test]
fn branches_and_delay_slots_agree() {
    let prog = [
        ori(2, 0, 7),
        ori(3, 0, 7),
        beq(2, 3, 2),      // taken: skips the sentinel below
        ori(8, 0, 0xAA),   // delay slot — MUST execute
        ori(9, 0, 0xDEAD), // skipped — a sentinel nothing else writes
        ori(10, 0, 0xCC),
        bne(2, 3, 2),     // NOT taken
        ori(11, 0, 0xBB), // delay slot — executes either way
        ori(12, 0, 0xEE), // reached, because the branch was not taken
        nop(),
        nop(),
    ];
    compare("branch", &prog, 9);
}

/// `JAL` and `JR`, so the link value is compared as architectural state.
///
/// The link is the address after the delay slot (accuracy ledger C-19), and the
/// sequential path computes it from a different quantity than the cascade does —
/// the cascade reads the live `next_pc`, this reads a value it carried forward. If
/// those disagree the projection catches it in `$31`.
#[test]
fn jump_and_link_agrees() {
    // The callee sits eight words in; `JAL` takes an absolute word address within
    // the region, and the program is loaded at physical 0.
    let callee = 8u32 * 4;
    let prog = [
        ori(2, 0, 1),
        jal(callee),
        ori(3, 0, 0xAA), // delay slot
        ori(4, 0, 0xBB), // returned to here
        nop(),
        nop(),
        nop(),
        nop(),
        ori(5, 0, 0xCC), // callee entry
        jr(31),
        ori(6, 0, 0xDD), // callee delay slot
        nop(),
    ];
    compare("jal", &prog, 8);
}

/// A loop, so the comparison covers repeated execution rather than a single pass —
/// and so the I-cache is hit rather than only filled.
#[test]
fn a_loop_agrees_across_iterations() {
    let prog = [
        ori(2, 0, 5), // counter
        ori(3, 0, 0), // accumulator
        // loop:
        addiu(3, 3, 3),      // 2
        addiu(2, 2, 0xFFFF), // -1
        bne(2, 0, 0xFFFD),   // back to `loop`
        nop(),               // delay slot
        ori(4, 0, 0x99),
        nop(),
    ];
    compare("loop", &prog, 20);
}
