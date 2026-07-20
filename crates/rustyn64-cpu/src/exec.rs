//! `EX`-stage execution: [`Decoded`] plus operands in, result out (T-11-002).
//!
//! The bridge between [`mod@crate::decode`] and [`crate::alu`]. Kept separate from
//! the pipeline so it stays a pure function of `(op, rs_val, rt_val, hi, lo)` —
//! testable without a machine, and with no way to accidentally read register
//! state the decode did not name.

use crate::Exception;
use crate::alu::{self, HiLo, MulDiv};
use crate::decode::{Decoded, Op};
use crate::mem::{LoadKind, StoreKind};

/// What an executed instruction wants written back.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WriteBack {
    /// Nothing to commit.
    #[default]
    None,
    /// Write `value` to general register `dest`.
    Gpr {
        /// Destination register index.
        dest: u8,
        /// Value to commit.
        value: u64,
    },
    /// Write both `HI` and `LO` (every multiply and divide does).
    HiLo(HiLo),
    /// Write `HI` alone (`MTHI`).
    Hi(u64),
    /// Write `LO` alone (`MTLO`).
    Lo(u64),
}

/// A memory access `EX` computed and `DC` must perform.
///
/// `EX` resolves the effective address and hands the access to `DC`; it does not
/// touch the bus itself. That split is the point of the pipeline — `DC` is the
/// cycle the scheduler interleaves the RCP around (ADR 0007).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemOp {
    /// An aligned load into `dest`.
    Load {
        /// Width and signedness.
        kind: LoadKind,
        /// Effective address.
        addr: u64,
        /// Destination register.
        dest: u8,
    },
    /// An aligned store of `value`.
    Store {
        /// Width.
        kind: StoreKind,
        /// Effective address.
        addr: u64,
        /// Value from `rt`.
        value: u64,
    },
    /// A **load linked**: an aligned load that also arms the link bit and
    /// records the physical address in `LLAddr` (UM §16 p. 453).
    LinkedLoad {
        /// Width and signedness.
        kind: LoadKind,
        /// Effective address.
        addr: u64,
        /// Destination register.
        dest: u8,
    },
    /// A **store conditional**: stores `value` only if the link bit is set, and
    /// writes the outcome (1 = stored, 0 = not) to `dest` either way.
    ///
    /// Carrying `dest` is what makes this distinct from [`MemOp::Store`] — the
    /// flag is architecturally visible even when nothing is written to memory
    /// (UM §16 p. 487).
    ConditionalStore {
        /// Width.
        kind: StoreKind,
        /// Effective address.
        addr: u64,
        /// Value from `rt`.
        value: u64,
        /// Destination register for the success flag.
        dest: u8,
    },
    /// A `CACHE` maintenance operation.
    ///
    /// Carries the effective address so `DC` translates it — the instruction can
    /// raise a TLB fault — and the 5-bit operation selector so a trace can name
    /// what was requested. No data moves.
    Cache {
        /// Effective address.
        addr: u64,
        /// The `op` field (the instruction's `rt` slot): bits 1..=0 select the
        /// cache, bits 4..=2 the operation.
        op: u8,
    },
    /// An FP load or store (`LWC1`/`LDC1`/`SWC1`/`SDC1`).
    ///
    /// Kept separate from [`MemOp::Load`]/[`MemOp::Store`] because the value
    /// moves to or from the **FP** register file, and `DC` needs to know which —
    /// a shared variant would need a "which file" flag anyway.
    Fp {
        /// Which of the four forms.
        op: Op,
        /// Effective address.
        addr: u64,
        /// The FPR (`ft`).
        ft: u8,
    },
    /// One half of an unaligned access. `rt` is needed for both directions: a
    /// partial load merges into it, and a partial store merges out of it.
    Unaligned {
        /// Which of the eight forms.
        op: Op,
        /// Effective address (deliberately **not** aligned down here — `DC`
        /// needs the low bits to know which bytes are covered).
        addr: u64,
        /// Current `rt`.
        rt: u64,
        /// Destination register, or 0 for the store forms.
        dest: u8,
    },
}

/// A control-flow redirect `EX` resolved.
///
/// The delay slot has *already been fetched* by the time `EX` resolves a branch,
/// which is the whole point of the architectural delay slot. What `EX` decides is
/// where the fetch *after* the delay slot goes, and whether the delay slot runs
/// at all.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Redirect {
    /// Where to fetch next.
    pub target: u64,
    /// Nullify the already-fetched delay slot.
    ///
    /// True only for a **branch-likely** form that was *not* taken. An ordinary
    /// branch executes its delay slot whether or not it is taken; a likely branch
    /// squashes it when not taken. Confusing the two silently runs or skips one
    /// instruction per untaken branch.
    pub nullify_delay_slot: bool,
}

/// A **coprocessor** access that `EX` resolved but must not itself perform.
///
/// Despite the name this now covers COP0, the TLB **and** COP1 control (`Cop1`).
/// They share a variant because they share the stage split below, not because
/// they are the same unit — the name is kept for churn reasons and this note
/// exists so it does not mislead as COP1 grows in Sprint 3.
///
/// The stage split is the manual's, not a convenience: UM §4.6.9 describes the
/// CP0 bypass interlock as firing when *"an instruction which caused an
/// exception reaches the WB stage and the subsequent instruction in the DC stage
/// requests a read of any CP0 register"* — so a coprocessor **read happens in
/// DC** and a **write happens in WB**. Performing both in `EX` would make that
/// interlock unexpressible, which is the same mistake ADR 0007 exists to prevent
/// one level up.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Cop0Access {
    /// Read COP0 register `src` into GPR `dest`, in `DC`.
    Read {
        /// COP0 register number.
        src: u8,
        /// GPR destination.
        dest: u8,
        /// 64-bit (`DMFC0`) rather than 32-bit sign-extended (`MFC0`).
        wide: bool,
    },
    /// A COP1 **control** access, performed where the state lives.
    Cop1(Cop1Access),
    /// A TLB instruction. Performed where the TLB lives, not in `EX`.
    Tlb(TlbOp),
    /// `ERET` — restore `PC` from `EPC`/`ErrorEPC`, clear `EXL`/`ERL`, clear
    /// the link bit.
    ///
    /// Cannot be resolved in `EX` like an ordinary redirect, because the target
    /// comes out of COP0 rather than out of the instruction.
    Eret,
    /// Write `value` to COP0 register `dest`, in `WB`.
    Write {
        /// COP0 register number.
        dest: u8,
        /// Value from `rt`.
        value: u64,
        /// 64-bit (`DMTC0`) rather than 32-bit (`MTC0`).
        wide: bool,
    },
}

/// The COP1 control moves (T-12-006). Arithmetic is Sprint 3.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Cop1Access {
    /// `MFC1`/`DMFC1` — move an FPR to a GPR.
    ReadFpr {
        /// FPR number (`fs`).
        src: u8,
        /// GPR destination.
        dest: u8,
        /// 64-bit (`DMFC1`) rather than 32-bit sign-extended (`MFC1`).
        wide: bool,
    },
    /// `MTC1`/`DMTC1` — move a GPR to an FPR.
    WriteFpr {
        /// FPR number (`fs`).
        dest: u8,
        /// Value from `rt`.
        value: u64,
        /// 64-bit (`DMTC1`) rather than 32-bit (`MTC1`).
        wide: bool,
    },
    /// `CFC1` — read control register `src` into GPR `dest`.
    ReadControl {
        /// COP1 control register number.
        src: u8,
        /// GPR destination.
        dest: u8,
    },
    /// `CTC1` — write `value` to control register `dest`.
    WriteControl {
        /// COP1 control register number.
        dest: u8,
        /// Value from `rt`.
        value: u32,
    },
}

/// The four TLB instructions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TlbOp {
    /// `TLBR` — entry → COP0 registers.
    Read,
    /// `TLBWI` — COP0 registers → entry `Index`.
    WriteIndexed,
    /// `TLBWR` — COP0 registers → entry `Random`.
    WriteRandom,
    /// `TLBP` — probe, reporting through `Index`.
    Probe,
}

/// The outcome of executing one instruction in `EX`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Executed {
    /// What to commit at `WB`.
    pub write_back: WriteBack,
    /// Extra `PCycle`s the whole pipeline stalls for (multiply/divide only).
    pub stall_cycles: u32,
    /// A memory access for `DC` to perform, if any.
    pub mem: Option<MemOp>,
    /// A control-flow redirect, if this was a taken branch, a jump, or an
    /// untaken branch-likely.
    pub redirect: Option<Redirect>,
    /// A COP0 access for `DC` (read) or `WB` (write) to perform.
    pub cop0: Option<Cop0Access>,
}

/// An `Executed` that does nothing: no write-back, no stall, no access, no
/// redirect. Base for the arms that only set one field.
const NOTHING: Executed = Executed {
    write_back: WriteBack::None,
    stall_cycles: 0,
    mem: None,
    redirect: None,
    cop0: None,
};

/// Sign-extend a 16-bit immediate — the arithmetic and `SLT` immediate forms.
const fn sext_imm(imm: u16) -> u64 {
    imm as i16 as i64 as u64
}

/// A jump or taken branch: redirect, and link if the form links.
const fn control(d: Decoded, target: u64, nullify: bool, pc: u64) -> Executed {
    Executed {
        // The linking forms save the address *after* the delay slot, so a
        // returning `JR $31` resumes past it rather than re-executing it.
        write_back: if d.dest == 0 {
            WriteBack::None
        } else {
            WriteBack::Gpr {
                dest: d.dest,
                value: pc.wrapping_add(8),
            }
        },
        stall_cycles: 0,
        mem: None,
        redirect: Some(Redirect {
            target,
            nullify_delay_slot: nullify,
        }),
        cop0: None,
    }
}

/// Resolve a conditional branch. Cannot fail — a branch raises no exception.
const fn branch(d: Decoded, taken: bool, pc: u64) -> Executed {
    if taken {
        // Target is relative to the DELAY SLOT's address, not this one.
        let target = pc
            .wrapping_add(4)
            .wrapping_add((sext_imm(d.imm) as i64 as u64) << 2);
        return control(d, target, false, pc);
    }
    // Not taken. A branch-likely nullifies its delay slot; an ordinary branch
    // lets it run. Either way the linking forms STILL link -- BLTZAL writes $31
    // even when the branch is not taken, which is easy to miss.
    Executed {
        write_back: if d.dest == 0 {
            WriteBack::None
        } else {
            WriteBack::Gpr {
                dest: d.dest,
                value: pc.wrapping_add(8),
            }
        },
        stall_cycles: 0,
        mem: None,
        cop0: None,
        redirect: if d.op.is_likely() {
            Some(Redirect {
                target: pc.wrapping_add(8),
                nullify_delay_slot: true,
            })
        } else {
            None
        },
    }
}

/// A conditional trap.
///
/// # Errors
/// [`Exception::Trap`] when the condition holds.
const fn trap_if(cond: bool) -> Result<Executed, Exception> {
    if cond {
        Err(Exception::Trap)
    } else {
        Ok(Executed {
            write_back: WriteBack::None,
            stall_cycles: 0,
            mem: None,
            redirect: None,
            cop0: None,
        })
    }
}

/// Zero-extend a 16-bit immediate — the *logical* immediate forms only.
///
/// This asymmetry is real and easy to get wrong: `ADDI` sign-extends while
/// `ANDI`/`ORI`/`XORI` zero-extend, so `ORI $t0, $0, 0xFFFF` yields `0x0000FFFF`
/// and not `0xFFFFFFFFFFFFFFFF`.
const fn zext_imm(imm: u16) -> u64 {
    imm as u64
}

/// Execute one decoded instruction.
///
/// `rs_val` / `rt_val` are the register values resolved through the bypass
/// network at `EX`; `hilo` is the current multiply-divide pair; `pc` is the
/// address of *this* instruction, needed by the control-flow forms.
///
/// # Errors
///
/// [`Exception::Overflow`] from the trapping arithmetic forms, and
/// [`Exception::ReservedInstruction`] for anything not yet decoded. Returning
/// `Reserved` rather than treating an unknown encoding as a `NOP` is deliberate:
/// a missing opcode should be loud, not silently produce wrong results.
#[allow(clippy::too_many_lines)]
// a flat match over the opcode table
// `rs_val`/`rt_val` trip `similar_names`, but they mirror the MIPS operand
// naming used throughout the manual and this crate. Any pair dissimilar enough
// to satisfy the lint would be less clear, so the names stay and the lint goes.
#[allow(clippy::similar_names)]
pub const fn execute(
    d: Decoded,
    rs_val: u64,
    rt_val: u64,
    hilo: HiLo,
    pc: u64,
) -> Result<Executed, Exception> {
    // Most instructions write one general register with no stall.
    macro_rules! gpr {
        ($v:expr) => {
            Ok(Executed {
                write_back: WriteBack::Gpr {
                    dest: d.dest,
                    value: $v,
                },
                stall_cycles: 0,
                mem: None,
                redirect: None,
                cop0: None,
            })
        };
    }
    // A load: EX resolves the address, DC performs the access and produces the
    // write-back. Nothing is committed here.
    macro_rules! mem_load {
        ($kind:expr) => {
            Ok(Executed {
                write_back: WriteBack::None,
                stall_cycles: 0,
                mem: Some(MemOp::Load {
                    kind: $kind,
                    addr: rs_val.wrapping_add(sext_imm(d.imm)),
                    dest: d.dest,
                }),
                redirect: None,
                cop0: None,
            })
        };
    }
    macro_rules! mem_store {
        ($kind:expr) => {
            Ok(Executed {
                write_back: WriteBack::None,
                stall_cycles: 0,
                mem: Some(MemOp::Store {
                    kind: $kind,
                    addr: rs_val.wrapping_add(sext_imm(d.imm)),
                    value: rt_val,
                }),
                redirect: None,
                cop0: None,
            })
        };
    }
    // Multiply/divide write HI/LO and stall the ENTIRE pipeline for the
    // documented count (UM Table 3-12) -- they are not background operations.
    macro_rules! muldiv {
        ($res:expr, $kind:expr) => {
            Ok(Executed {
                write_back: WriteBack::HiLo($res),
                stall_cycles: alu::muldiv_stall_cycles($kind),
                mem: None,
                redirect: None,
                cop0: None,
            })
        };
    }

    match d.op {
        Op::Reserved => Err(Exception::ReservedInstruction),

        // --- arithmetic, register form
        Op::Add => match alu::add(rs_val, rt_val) {
            Ok(v) => gpr!(v),
            Err(e) => Err(e),
        },
        Op::Addu => gpr!(alu::addu(rs_val, rt_val)),
        Op::Sub => match alu::sub(rs_val, rt_val) {
            Ok(v) => gpr!(v),
            Err(e) => Err(e),
        },
        Op::Subu => gpr!(alu::subu(rs_val, rt_val)),
        Op::Dadd => match alu::dadd(rs_val, rt_val) {
            Ok(v) => gpr!(v),
            Err(e) => Err(e),
        },
        Op::Daddu => gpr!(alu::daddu(rs_val, rt_val)),
        Op::Dsub => match alu::dsub(rs_val, rt_val) {
            Ok(v) => gpr!(v),
            Err(e) => Err(e),
        },
        Op::Dsubu => gpr!(alu::dsubu(rs_val, rt_val)),
        Op::Slt => gpr!(alu::slt(rs_val, rt_val)),
        Op::Sltu => gpr!(alu::sltu(rs_val, rt_val)),

        // --- logical, register form
        Op::And => gpr!(alu::and(rs_val, rt_val)),
        Op::Or => gpr!(alu::or(rs_val, rt_val)),
        Op::Xor => gpr!(alu::xor(rs_val, rt_val)),
        Op::Nor => gpr!(alu::nor(rs_val, rt_val)),

        // --- immediate forms. Note the sign/zero-extension asymmetry.
        Op::Addi => match alu::add(rs_val, sext_imm(d.imm)) {
            Ok(v) => gpr!(v),
            Err(e) => Err(e),
        },
        Op::Addiu => gpr!(alu::addu(rs_val, sext_imm(d.imm))),
        Op::Daddi => match alu::dadd(rs_val, sext_imm(d.imm)) {
            Ok(v) => gpr!(v),
            Err(e) => Err(e),
        },
        Op::Daddiu => gpr!(alu::daddu(rs_val, sext_imm(d.imm))),
        Op::Slti => gpr!(alu::slt(rs_val, sext_imm(d.imm))),
        Op::Sltiu => gpr!(alu::sltu(rs_val, sext_imm(d.imm))),
        Op::Andi => gpr!(alu::and(rs_val, zext_imm(d.imm))),
        Op::Ori => gpr!(alu::or(rs_val, zext_imm(d.imm))),
        Op::Xori => gpr!(alu::xor(rs_val, zext_imm(d.imm))),
        Op::Lui => gpr!(alu::lui(d.imm)),

        // --- shifts. The immediate forms shift `rt`; the variable forms take
        // the amount from `rs` (masked by the helper).
        Op::Sll => gpr!(alu::sll(rt_val, d.sa)),
        Op::Srl => gpr!(alu::srl(rt_val, d.sa)),
        Op::Sra => gpr!(alu::sra(rt_val, d.sa)),
        Op::Dsll | Op::Dsll32 => gpr!(alu::dsll(rt_val, d.sa)),
        Op::Dsrl | Op::Dsrl32 => gpr!(alu::dsrl(rt_val, d.sa)),
        Op::Dsra | Op::Dsra32 => gpr!(alu::dsra(rt_val, d.sa)),
        Op::Sllv => gpr!(alu::sll(rt_val, rs_val as u32)),
        Op::Srlv => gpr!(alu::srl(rt_val, rs_val as u32)),
        Op::Srav => gpr!(alu::sra(rt_val, rs_val as u32)),
        Op::Dsllv => gpr!(alu::dsll(rt_val, rs_val as u32)),
        Op::Dsrlv => gpr!(alu::dsrl(rt_val, rs_val as u32)),
        Op::Dsrav => gpr!(alu::dsra(rt_val, rs_val as u32)),

        // --- multiply / divide
        Op::Mult => muldiv!(alu::mult(rs_val, rt_val), MulDiv::Mult),
        Op::Multu => muldiv!(alu::multu(rs_val, rt_val), MulDiv::Multu),
        Op::Div => muldiv!(alu::div(rs_val, rt_val), MulDiv::Div),
        Op::Divu => muldiv!(alu::divu(rs_val, rt_val), MulDiv::Divu),
        Op::Dmult => muldiv!(alu::dmult(rs_val, rt_val), MulDiv::Dmult),
        Op::Dmultu => muldiv!(alu::dmultu(rs_val, rt_val), MulDiv::Dmultu),
        Op::Ddiv => muldiv!(alu::ddiv(rs_val, rt_val), MulDiv::Ddiv),
        Op::Ddivu => muldiv!(alu::ddivu(rs_val, rt_val), MulDiv::Ddivu),

        // --- HI/LO moves
        Op::Mfhi => gpr!(hilo.hi),
        Op::Mflo => gpr!(hilo.lo),
        Op::Mthi => Ok(Executed {
            write_back: WriteBack::Hi(rs_val),
            stall_cycles: 0,
            mem: None,
            redirect: None,
            cop0: None,
        }),
        Op::Mtlo => Ok(Executed {
            write_back: WriteBack::Lo(rs_val),
            stall_cycles: 0,
            mem: None,
            redirect: None,
            cop0: None,
        }),

        // --- memory. EX resolves the effective address only; DC performs the
        // access. The address is base + SIGN-extended offset, always.
        Op::Lb => mem_load!(LoadKind::SignedByte),
        Op::Lbu => mem_load!(LoadKind::UnsignedByte),
        Op::Lh => mem_load!(LoadKind::SignedHalf),
        Op::Lhu => mem_load!(LoadKind::UnsignedHalf),
        Op::Lw => mem_load!(LoadKind::SignedWord),
        Op::Lwu => mem_load!(LoadKind::UnsignedWord),
        Op::Ld => mem_load!(LoadKind::Double),
        Op::Sb => mem_store!(StoreKind::Byte),
        Op::Sh => mem_store!(StoreKind::Half),
        Op::Sw => mem_store!(StoreKind::Word),
        Op::Sd => mem_store!(StoreKind::Double),
        // The synchronisation pair. `EX` treats them exactly like their ordinary
        // counterparts; all the link-bit behaviour is in `DC`, because that is
        // where the bus access it is conditional on happens.
        Op::Ll => Ok(Executed {
            write_back: WriteBack::None,
            stall_cycles: 0,
            mem: Some(MemOp::LinkedLoad {
                kind: LoadKind::SignedWord,
                addr: rs_val.wrapping_add(sext_imm(d.imm)),
                dest: d.dest,
            }),
            redirect: None,
            cop0: None,
        }),
        Op::Lld => Ok(Executed {
            write_back: WriteBack::None,
            stall_cycles: 0,
            mem: Some(MemOp::LinkedLoad {
                kind: LoadKind::Double,
                addr: rs_val.wrapping_add(sext_imm(d.imm)),
                dest: d.dest,
            }),
            redirect: None,
            cop0: None,
        }),
        Op::Sc => Ok(Executed {
            write_back: WriteBack::None,
            stall_cycles: 0,
            mem: Some(MemOp::ConditionalStore {
                kind: StoreKind::Word,
                addr: rs_val.wrapping_add(sext_imm(d.imm)),
                value: rt_val,
                dest: d.dest,
            }),
            redirect: None,
            cop0: None,
        }),
        Op::Scd => Ok(Executed {
            write_back: WriteBack::None,
            stall_cycles: 0,
            mem: Some(MemOp::ConditionalStore {
                kind: StoreKind::Double,
                addr: rs_val.wrapping_add(sext_imm(d.imm)),
                value: rt_val,
                dest: d.dest,
            }),
            redirect: None,
            cop0: None,
        }),
        // --- control flow. `pc` is this instruction's address; the delay slot
        // is at `pc + 4` and the instruction after it at `pc + 8`, which is what
        // the linking forms save.
        Op::J | Op::Jal => {
            // The 26-bit region form keeps the top 4 bits of the DELAY SLOT's
            // address, not this instruction's -- they differ across a 256 MiB
            // boundary, which is exactly where a naive implementation breaks.
            let region = pc.wrapping_add(4) & 0xFFFF_FFFF_F000_0000;
            let target = region | ((d.target as u64) << 2);
            Ok(control(d, target, false, pc))
        }
        // JR and JALR differ only in whether decode gave them a destination:
        // `control` links iff `d.dest != 0`, so one arm serves both.
        Op::Jr | Op::Jalr => Ok(control(d, rs_val, false, pc)),

        Op::Beq | Op::Beql => Ok(branch(d, rs_val == rt_val, pc)),
        Op::Bne | Op::Bnel => Ok(branch(d, rs_val != rt_val, pc)),
        Op::Blez | Op::Blezl => Ok(branch(d, (rs_val as i64) <= 0, pc)),
        Op::Bgtz | Op::Bgtzl => Ok(branch(d, (rs_val as i64) > 0, pc)),
        Op::Bltz | Op::Bltzl | Op::Bltzal | Op::Bltzall => Ok(branch(d, (rs_val as i64) < 0, pc)),
        Op::Bgez | Op::Bgezl | Op::Bgezal | Op::Bgezall => Ok(branch(d, (rs_val as i64) >= 0, pc)),

        // --- traps. The comparison is 64-bit; the `*U` forms are unsigned.
        Op::Tge => trap_if((rs_val as i64) >= (rt_val as i64)),
        Op::Tgeu => trap_if(rs_val >= rt_val),
        Op::Tlt => trap_if((rs_val as i64) < (rt_val as i64)),
        Op::Tltu => trap_if(rs_val < rt_val),
        Op::Teq => trap_if(rs_val == rt_val),
        Op::Tne => trap_if(rs_val != rt_val),
        // The immediate trap forms SIGN-extend, including the unsigned
        // comparisons -- the `U` refers to the comparison, not the extension.
        Op::Tgei => trap_if((rs_val as i64) >= (sext_imm(d.imm) as i64)),
        Op::Tgeiu => trap_if(rs_val >= sext_imm(d.imm)),
        Op::Tlti => trap_if((rs_val as i64) < (sext_imm(d.imm) as i64)),
        Op::Tltiu => trap_if(rs_val < sext_imm(d.imm)),
        Op::Teqi => trap_if(rs_val == sext_imm(d.imm)),
        Op::Tnei => trap_if(rs_val != sext_imm(d.imm)),

        // "all load/store instructions in this processor are executed in program
        // order since the SYNC instruction is handled as a NOP" (UM §3.1). It
        // retires normally -- what it must NOT do is raise reserved-instruction.
        // COP0 access. `rd` is the COP0 register; `EX` only resolves which.
        Op::Mfc0 | Op::Dmfc0 => Ok(Executed {
            cop0: Some(Cop0Access::Read {
                src: d.rd,
                dest: d.dest,
                wide: matches!(d.op, Op::Dmfc0),
            }),
            ..NOTHING
        }),
        // `fs` is the COP1 control register, encoded in the `rd` field.
        Op::Mfc1 | Op::Dmfc1 => Ok(Executed {
            cop0: Some(Cop0Access::Cop1(Cop1Access::ReadFpr {
                src: d.rd,
                dest: d.dest,
                wide: matches!(d.op, Op::Dmfc1),
            })),
            ..NOTHING
        }),
        Op::Mtc1 | Op::Dmtc1 => Ok(Executed {
            cop0: Some(Cop0Access::Cop1(Cop1Access::WriteFpr {
                dest: d.rd,
                value: rt_val,
                wide: matches!(d.op, Op::Dmtc1),
            })),
            ..NOTHING
        }),
        // FP loads and stores resolve their address exactly like the integer
        // forms; only the register file they land in differs.
        Op::Lwc1 | Op::Ldc1 | Op::Swc1 | Op::Sdc1 => Ok(Executed {
            mem: Some(MemOp::Fp {
                op: d.op,
                addr: rs_val.wrapping_add(sext_imm(d.imm)),
                ft: d.rt,
            }),
            ..NOTHING
        }),
        Op::Cfc1 => Ok(Executed {
            cop0: Some(Cop0Access::Cop1(Cop1Access::ReadControl {
                src: d.rd,
                dest: d.dest,
            })),
            ..NOTHING
        }),
        Op::Ctc1 => Ok(Executed {
            cop0: Some(Cop0Access::Cop1(Cop1Access::WriteControl {
                dest: d.rd,
                value: rt_val as u32,
            })),
            ..NOTHING
        }),
        // Two distinct cases that share one behaviour -- retire with no
        // architectural effect -- and are merged only because clippy rejects
        // identical arms:
        //
        // - `Cop1Unimplemented`: a valid COP1 encoding we do not implement.
        //   Raising here would be wrong -- with `Status.CU1` SET hardware would
        //   execute it, so pretending otherwise would make Sprint 3's arrival a
        //   behaviour change rather than an addition. The coprocessor-usable
        //   check happens in the pipeline.
        // - `Cop0Extension`: a COP0 CO instruction in the emux `funct`
        //   0x20-0x3F extension range, inert on hardware (ledger C-8). Notably
        //   the target GPR is **not** written, so a probe reads back whatever
        //   was already there and concludes emux is absent.
        Op::Cop1Unimplemented | Op::Cop0Extension => Ok(NOTHING),
        Op::Tlbr => Ok(Executed {
            cop0: Some(Cop0Access::Tlb(TlbOp::Read)),
            ..NOTHING
        }),
        Op::Tlbwi => Ok(Executed {
            cop0: Some(Cop0Access::Tlb(TlbOp::WriteIndexed)),
            ..NOTHING
        }),
        Op::Tlbwr => Ok(Executed {
            cop0: Some(Cop0Access::Tlb(TlbOp::WriteRandom)),
            ..NOTHING
        }),
        Op::Tlbp => Ok(Executed {
            cop0: Some(Cop0Access::Tlb(TlbOp::Probe)),
            ..NOTHING
        }),
        Op::Eret => Ok(Executed {
            cop0: Some(Cop0Access::Eret),
            ..NOTHING
        }),
        Op::Mtc0 | Op::Dmtc0 => Ok(Executed {
            cop0: Some(Cop0Access::Write {
                dest: d.rd,
                value: rt_val,
                wide: matches!(d.op, Op::Dmtc0),
            }),
            ..NOTHING
        }),
        // CACHE resolves its effective address like any load/store -- so it can
        // raise a TLB fault, and DC must perform the translation -- but performs
        // no data transfer. Modelled as a zero-width probe: see `MemOp::Cache`.
        Op::Cache => Ok(Executed {
            mem: Some(MemOp::Cache {
                addr: rs_val.wrapping_add(sext_imm(d.imm)),
                op: d.rt,
            }),
            ..NOTHING
        }),
        Op::Sync => Ok(Executed {
            write_back: WriteBack::None,
            stall_cycles: 0,
            mem: None,
            redirect: None,
            cop0: None,
        }),
        Op::Syscall => Err(Exception::Syscall),
        Op::Break => Err(Exception::Breakpoint),

        Op::Lwl | Op::Lwr | Op::Ldl | Op::Ldr | Op::Swl | Op::Swr | Op::Sdl | Op::Sdr => {
            Ok(Executed {
                write_back: WriteBack::None,
                stall_cycles: 0,
                mem: Some(MemOp::Unaligned {
                    op: d.op,
                    addr: rs_val.wrapping_add(sext_imm(d.imm)),
                    rt: rt_val,
                    dest: d.dest,
                }),
                redirect: None,
                cop0: None,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::decode;

    #[allow(clippy::similar_names)] // mirrors the MIPS operand naming
    fn run(word: u32, rs_val: u64, rt_val: u64) -> Result<Executed, Exception> {
        execute(decode(word), rs_val, rt_val, HiLo { hi: 0, lo: 0 }, 0)
    }
    const fn r(funct: u32, rs: u32, rt: u32, rd: u32, sa: u32) -> u32 {
        (rs << 21) | (rt << 16) | (rd << 11) | (sa << 6) | funct
    }
    const fn i(opcode: u32, rs: u32, rt: u32, imm: u16) -> u32 {
        (opcode << 26) | (rs << 21) | (rt << 16) | imm as u32
    }

    #[test]
    fn register_form_arithmetic_reaches_the_alu() {
        // ADDU $3, $1, $2  with $1 = 2, $2 = 3
        let e = run(r(0o41, 1, 2, 3, 0), 2, 3).unwrap();
        assert_eq!(e.write_back, WriteBack::Gpr { dest: 3, value: 5 });
        assert_eq!(e.stall_cycles, 0);
    }

    /// The immediate extension asymmetry: arithmetic sign-extends, logical
    /// zero-extends. Getting this backwards is a classic MIPS bug — `ORI` with
    /// 0xFFFF would produce all-ones instead of 0xFFFF.
    #[test]
    fn immediates_sign_extend_for_arithmetic_and_zero_extend_for_logical() {
        // ADDIU $2, $0, -1  =>  sign-extended to 0xFFFF_FFFF_FFFF_FFFF
        let e = run(i(0o11, 0, 2, 0xFFFF), 0, 0).unwrap();
        assert_eq!(
            e.write_back,
            WriteBack::Gpr {
                dest: 2,
                value: u64::MAX
            }
        );
        // ORI $2, $0, 0xFFFF  =>  ZERO-extended to 0x0000_0000_0000_FFFF
        let e = run(i(0o15, 0, 2, 0xFFFF), 0, 0).unwrap();
        assert_eq!(
            e.write_back,
            WriteBack::Gpr {
                dest: 2,
                value: 0xFFFF
            }
        );
        // ANDI and XORI likewise.
        let e = run(i(0o14, 1, 2, 0xFFFF), u64::MAX, 0).unwrap();
        assert_eq!(
            e.write_back,
            WriteBack::Gpr {
                dest: 2,
                value: 0xFFFF
            }
        );
    }

    #[test]
    fn trapping_arithmetic_returns_overflow_instead_of_a_value() {
        // ADD $3, $1, $2 with 0x7FFF_FFFF + 1
        assert_eq!(
            run(r(0o40, 1, 2, 3, 0), 0x7FFF_FFFF, 1),
            Err(Exception::Overflow)
        );
        // ADDU with the same inputs must NOT trap.
        assert!(run(r(0o41, 1, 2, 3, 0), 0x7FFF_FFFF, 1).is_ok());
    }

    #[test]
    fn multiply_and_divide_write_hi_lo_and_stall_the_documented_count() {
        // MULT $1, $2 with 6 * 7
        let e = run(r(0o30, 1, 2, 0, 0), 6, 7).unwrap();
        match e.write_back {
            WriteBack::HiLo(hl) => assert_eq!((hl.hi, hl.lo), (0, 42)),
            other => panic!("MULT should write HI/LO, got {other:?}"),
        }
        assert_eq!(e.stall_cycles, 5, "UM Table 3-12");
        // DDIV is the expensive one.
        assert_eq!(run(r(0o36, 1, 2, 0, 0), 100, 7).unwrap().stall_cycles, 69);
        // ...and an ordinary ALU op stalls not at all.
        assert_eq!(run(r(0o41, 1, 2, 3, 0), 1, 1).unwrap().stall_cycles, 0);
    }

    #[test]
    fn hi_lo_moves_read_and_write_the_pair() {
        // MFHI $5 reads HI
        let e = execute(
            decode(r(0o20, 0, 0, 5, 0)),
            0,
            0,
            HiLo {
                hi: 0xDEAD,
                lo: 0xBEEF,
            },
            0,
        )
        .unwrap();
        assert_eq!(
            e.write_back,
            WriteBack::Gpr {
                dest: 5,
                value: 0xDEAD
            }
        );
        // MFLO $5 reads LO
        let e = execute(
            decode(r(0o22, 0, 0, 5, 0)),
            0,
            0,
            HiLo {
                hi: 0xDEAD,
                lo: 0xBEEF,
            },
            0,
        )
        .unwrap();
        assert_eq!(
            e.write_back,
            WriteBack::Gpr {
                dest: 5,
                value: 0xBEEF
            }
        );
        // MTHI $1 writes HI from rs
        let e = execute(
            decode(r(0o21, 1, 0, 0, 0)),
            0x1234,
            0,
            HiLo { hi: 0, lo: 0 },
            0,
        )
        .unwrap();
        assert_eq!(e.write_back, WriteBack::Hi(0x1234));
    }

    /// An unimplemented opcode must be **loud**. Treating it as a `NOP` would let
    /// a program run past instructions that did nothing, producing wrong results
    /// with no indication of why.
    /// **The `SRAV` instruction path shares the `SRA` erratum.**
    ///
    /// Tested through `execute` rather than by calling `alu::sra`, because the
    /// risk being guarded against is precisely that `Op::Srav` stops routing
    /// through the shared helper — gaining its own "corrected" implementation
    /// while `SRA` stays right. A test that calls the helper directly cannot see
    /// that happen, which is what an earlier version of this test did.
    ///
    /// Source: `n64brew_wiki/markdown/VR4300.md` § Known Bugs.
    #[test]
    fn the_srav_instruction_path_shares_the_sra_erratum() {
        let rt = 0x0123_4567_89AB_CDEF;
        // SRAV $3, $2, $1  -- amount from rs, value from rt.
        let word = r(0o07, 1, 2, 3, 0);
        for (amount, want) in [
            (1u64, 0xFFFF_FFFF_C4D5_E6F7u64),
            (8, 0x0000_0000_6789_ABCD),
            (16, 0x0000_0000_4567_89AB),
            (31, 0x0000_0000_0246_8ACF),
        ] {
            let e = run(word, amount, rt).unwrap();
            assert_eq!(
                e.write_back,
                WriteBack::Gpr {
                    dest: 3,
                    value: want
                },
                "SRAV by {amount} must leak the upper half, as SRA does"
            );
        }
        // And the immediate form agrees with it, through its own path.
        let e = run(r(0o03, 0, 2, 3, 16), 0, rt).unwrap();
        assert_eq!(
            e.write_back,
            WriteBack::Gpr {
                dest: 3,
                value: 0x0000_0000_4567_89AB
            },
            "SRA and SRAV must not diverge"
        );
    }

    #[test]
    fn unimplemented_opcodes_raise_reserved_instruction() {
        // These must be encodings the VR4300 genuinely leaves UNASSIGNED, not
        // merely ones this project has not implemented yet. Primary opcodes
        // 0o34..0o37 are reserved on MIPS III, and SPECIAL funct 0o01 is unused.
        // Earlier revisions of this test used LW and then BEQ, and had to be
        // repointed each time that opcode landed -- which made the test track
        // implementation progress instead of the architecture.
        assert_eq!(
            run(i(0o35, 1, 2, 0), 0, 0),
            Err(Exception::ReservedInstruction)
        );
        assert_eq!(
            run(r(0o01, 1, 2, 3, 0), 0, 0),
            Err(Exception::ReservedInstruction)
        );
    }

    /// `SLL $0, $0, 0` is `NOP`: it executes successfully and its write-back
    /// targets `$zero`, which `Regs::write` discards.
    #[test]
    fn nop_executes_and_commits_nothing() {
        let e = run(0, 0, 0).unwrap();
        assert_eq!(e.write_back, WriteBack::Gpr { dest: 0, value: 0 });
        assert_eq!(e.stall_cycles, 0);
    }
}
