//! MIPS III instruction decode (T-11-002).
//!
//! Word in, [`Decoded`] out. Pure and total: **every** 32-bit pattern decodes to
//! something, with anything unrecognised becoming [`Op::Reserved`] rather than a
//! panic or a silent no-op. A guest can execute arbitrary bytes, so decode must
//! not be able to fail.
//!
//! # Encoding
//!
//! Three formats, distinguished by the primary opcode in bits 31..26:
//!
//! ```text
//!  31    26 25  21 20  16 15  11 10   6 5    0
//! ┌────────┬──────┬──────┬──────┬──────┬──────┐
//! │ opcode │  rs  │  rt  │  rd  │  sa  │ funct│  R-type (opcode == 0, SPECIAL)
//! ├────────┼──────┼──────┼──────┴──────┴──────┤
//! │ opcode │  rs  │  rt  │       immediate    │  I-type
//! ├────────┼──────┴──────┴──────┴──────┴──────┤
//! │ opcode │              target              │  J-type
//! └────────┴──────────────────────────────────┘
//! ```
//!
//! This module covers the **integer subset** that [`crate::alu`] implements.
//! Loads, stores, branches, jumps, COP0, COP1 and the trap family decode to
//! [`Op::Reserved`] for now — they arrive with T-11-003 and T-11-004. That is
//! deliberate: an unimplemented opcode that decodes to `Reserved` raises a
//! reserved-instruction exception, which is visible, rather than executing as a
//! `NOP`, which silently produces wrong results.

/// The decoded operation. Only the integer subset so far; see the module docs.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum Op {
    /// Not (yet) a recognised encoding — raises a reserved-instruction
    /// exception rather than behaving as a `NOP`.
    #[default]
    Reserved,

    // --- arithmetic, register form
    /// `ADD rd, rs, rt` — traps on overflow.
    Add,
    /// `ADDU rd, rs, rt`.
    Addu,
    /// `SUB rd, rs, rt` — traps on overflow.
    Sub,
    /// `SUBU rd, rs, rt`.
    Subu,
    /// `DADD rd, rs, rt` — traps on overflow.
    Dadd,
    /// `DADDU rd, rs, rt`.
    Daddu,
    /// `DSUB rd, rs, rt` — traps on overflow.
    Dsub,
    /// `DSUBU rd, rs, rt`.
    Dsubu,
    /// `SLT rd, rs, rt`.
    Slt,
    /// `SLTU rd, rs, rt`.
    Sltu,

    // --- logical, register form
    /// `AND rd, rs, rt`.
    And,
    /// `OR rd, rs, rt`.
    Or,
    /// `XOR rd, rs, rt`.
    Xor,
    /// `NOR rd, rs, rt`.
    Nor,

    // --- arithmetic / logical, immediate form
    /// `ADDI rt, rs, imm` — traps on overflow.
    Addi,
    /// `ADDIU rt, rs, imm`.
    Addiu,
    /// `DADDI rt, rs, imm` — traps on overflow.
    Daddi,
    /// `DADDIU rt, rs, imm`.
    Daddiu,
    /// `SLTI rt, rs, imm`.
    Slti,
    /// `SLTIU rt, rs, imm`.
    Sltiu,
    /// `ANDI rt, rs, imm` — immediate is **zero**-extended.
    Andi,
    /// `ORI rt, rs, imm` — immediate is **zero**-extended.
    Ori,
    /// `XORI rt, rs, imm` — immediate is **zero**-extended.
    Xori,
    /// `LUI rt, imm`.
    Lui,

    // --- shifts, immediate amount
    /// `SLL rd, rt, sa`. `SLL $0, $0, 0` is the canonical `NOP`.
    Sll,
    /// `SRL rd, rt, sa`.
    Srl,
    /// `SRA rd, rt, sa` — reproduces the VR4300 erratum.
    Sra,
    /// `DSLL rd, rt, sa`.
    Dsll,
    /// `DSRL rd, rt, sa`.
    Dsrl,
    /// `DSRA rd, rt, sa`.
    Dsra,
    /// `DSLL32 rd, rt, sa` — `sa + 32`.
    Dsll32,
    /// `DSRL32 rd, rt, sa` — `sa + 32`.
    Dsrl32,
    /// `DSRA32 rd, rt, sa` — `sa + 32`.
    Dsra32,

    // --- shifts, register amount
    /// `SLLV rd, rt, rs`.
    Sllv,
    /// `SRLV rd, rt, rs`.
    Srlv,
    /// `SRAV rd, rt, rs` — reproduces the VR4300 erratum.
    Srav,
    /// `DSLLV rd, rt, rs`.
    Dsllv,
    /// `DSRLV rd, rt, rs`.
    Dsrlv,
    /// `DSRAV rd, rt, rs`.
    Dsrav,

    // --- multiply / divide and the HI/LO moves
    /// `MULT rs, rt`.
    Mult,
    /// `MULTU rs, rt`.
    Multu,
    /// `DIV rs, rt`.
    Div,
    /// `DIVU rs, rt`.
    Divu,
    /// `DMULT rs, rt`.
    Dmult,
    /// `DMULTU rs, rt`.
    Dmultu,
    /// `DDIV rs, rt`.
    Ddiv,
    /// `DDIVU rs, rt`.
    Ddivu,
    /// `MFHI rd`.
    Mfhi,
    /// `MTHI rs`.
    Mthi,
    /// `MFLO rd`.
    Mflo,
    /// `MTLO rs`.
    Mtlo,
}

impl Op {
    /// Does this operation write `HI`/`LO` rather than a general register?
    ///
    /// Used for the `MFHI`/`MFLO` hazard window: a `MFHI` followed within two
    /// instructions by anything that writes `HI` produces hardware's wrong
    /// result, and that is a *non-interlocked* hazard (see
    /// [`crate::alu::MFHI_MFLO_HAZARD_INSTRUCTIONS`]).
    #[must_use]
    pub const fn writes_hi_lo(self) -> bool {
        matches!(
            self,
            Self::Mult
                | Self::Multu
                | Self::Div
                | Self::Divu
                | Self::Dmult
                | Self::Dmultu
                | Self::Ddiv
                | Self::Ddivu
                | Self::Mthi
                | Self::Mtlo
        )
    }
}

/// A decoded instruction: the operation plus its raw encoded fields.
///
/// The fields are kept as encoded (`rs`, `rt`, `rd`, `sa`, `imm`) rather than
/// resolved into "operands", because the load-delay interlock matches on the
/// **fields** whether or not they are used as sources
/// (see [`crate::pipeline::load_interlocks`]). Resolving them away would make
/// that check impossible to state correctly.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Decoded {
    /// The operation.
    pub op: Op,
    /// `rs` field, bits 25..21.
    pub rs: u8,
    /// `rt` field, bits 20..16.
    pub rt: u8,
    /// `rd` field, bits 15..11.
    pub rd: u8,
    /// Shift amount, bits 10..6 — already adjusted by +32 for the `*32` forms,
    /// so it is the **effective** amount the shift helpers expect.
    pub sa: u32,
    /// Immediate, bits 15..0, unextended.
    pub imm: u16,
    /// The general register this writes, or 0 for none. `$zero` is never
    /// actually written, so 0 doubles as "no destination".
    pub dest: u8,
}

impl Decoded {
    /// Does this instruction target a floating-point register?
    ///
    /// Always `false` for the integer subset. Present because the load-delay
    /// interlock does **not** cross the GPR/FPR boundary, so the check needs to
    /// know which file a destination belongs to.
    #[must_use]
    pub const fn targets_fpr(self) -> bool {
        false
    }
}

// Primary opcodes (bits 31..26).
const OP_SPECIAL: u32 = 0o00;
const OP_ADDI: u32 = 0o10;
const OP_ADDIU: u32 = 0o11;
const OP_SLTI: u32 = 0o12;
const OP_SLTIU: u32 = 0o13;
const OP_ANDI: u32 = 0o14;
const OP_ORI: u32 = 0o15;
const OP_XORI: u32 = 0o16;
const OP_LUI: u32 = 0o17;
const OP_DADDI: u32 = 0o30;
const OP_DADDIU: u32 = 0o31;

/// Decode one instruction word. Total — never fails, never panics.
#[must_use]
#[allow(clippy::too_many_lines)] // a flat opcode table reads better than nested helpers
pub const fn decode(word: u32) -> Decoded {
    let opcode = word >> 26;
    let rs = ((word >> 21) & 0x1F) as u8;
    let rt = ((word >> 16) & 0x1F) as u8;
    let rd = ((word >> 11) & 0x1F) as u8;
    let sa = (word >> 6) & 0x1F;
    let imm = (word & 0xFFFF) as u16;
    let funct = word & 0x3F;

    let base = Decoded {
        op: Op::Reserved,
        rs,
        rt,
        rd,
        sa,
        imm,
        dest: 0,
    };

    // R-type: the operation is in `funct`, and the destination is `rd`.
    macro_rules! r {
        ($op:expr) => {
            Decoded {
                op: $op,
                dest: rd,
                ..base
            }
        };
    }
    // R-type shift by 32: the encoded 5-bit `sa` means `sa + 32`.
    macro_rules! r32 {
        ($op:expr) => {
            Decoded {
                op: $op,
                dest: rd,
                sa: sa + 32,
                ..base
            }
        };
    }
    // I-type: the destination is `rt`.
    macro_rules! i {
        ($op:expr) => {
            Decoded {
                op: $op,
                dest: rt,
                ..base
            }
        };
    }
    // Writes HI/LO, so no general-register destination.
    macro_rules! hilo {
        ($op:expr) => {
            Decoded { op: $op, ..base }
        };
    }

    match opcode {
        OP_SPECIAL => match funct {
            0o00 => r!(Op::Sll),
            0o02 => r!(Op::Srl),
            0o03 => r!(Op::Sra),
            0o04 => r!(Op::Sllv),
            0o06 => r!(Op::Srlv),
            0o07 => r!(Op::Srav),
            0o20 => r!(Op::Mfhi),
            0o21 => hilo!(Op::Mthi),
            0o22 => r!(Op::Mflo),
            0o23 => hilo!(Op::Mtlo),
            0o24 => r!(Op::Dsllv),
            0o26 => r!(Op::Dsrlv),
            0o27 => r!(Op::Dsrav),
            0o30 => hilo!(Op::Mult),
            0o31 => hilo!(Op::Multu),
            0o32 => hilo!(Op::Div),
            0o33 => hilo!(Op::Divu),
            0o34 => hilo!(Op::Dmult),
            0o35 => hilo!(Op::Dmultu),
            0o36 => hilo!(Op::Ddiv),
            0o37 => hilo!(Op::Ddivu),
            0o40 => r!(Op::Add),
            0o41 => r!(Op::Addu),
            0o42 => r!(Op::Sub),
            0o43 => r!(Op::Subu),
            0o44 => r!(Op::And),
            0o45 => r!(Op::Or),
            0o46 => r!(Op::Xor),
            0o47 => r!(Op::Nor),
            0o52 => r!(Op::Slt),
            0o53 => r!(Op::Sltu),
            0o54 => r!(Op::Dadd),
            0o55 => r!(Op::Daddu),
            0o56 => r!(Op::Dsub),
            0o57 => r!(Op::Dsubu),
            0o70 => r!(Op::Dsll),
            0o72 => r!(Op::Dsrl),
            0o73 => r!(Op::Dsra),
            0o74 => r32!(Op::Dsll32),
            0o76 => r32!(Op::Dsrl32),
            0o77 => r32!(Op::Dsra32),
            _ => base,
        },
        OP_ADDI => i!(Op::Addi),
        OP_ADDIU => i!(Op::Addiu),
        OP_SLTI => i!(Op::Slti),
        OP_SLTIU => i!(Op::Sltiu),
        OP_ANDI => i!(Op::Andi),
        OP_ORI => i!(Op::Ori),
        OP_XORI => i!(Op::Xori),
        OP_LUI => i!(Op::Lui),
        OP_DADDI => i!(Op::Daddi),
        OP_DADDIU => i!(Op::Daddiu),
        _ => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assemble an R-type word, so the tests read as assembly rather than hex.
    const fn r(funct: u32, rs: u32, rt: u32, rd: u32, sa: u32) -> u32 {
        (rs << 21) | (rt << 16) | (rd << 11) | (sa << 6) | funct
    }
    /// Assemble an I-type word.
    const fn i(opcode: u32, rs: u32, rt: u32, imm: u16) -> u32 {
        (opcode << 26) | (rs << 21) | (rt << 16) | imm as u32
    }

    #[test]
    fn fields_land_in_the_right_places() {
        // ADD $t0($8), $s0($16), $s1($17)
        let d = decode(r(0o40, 16, 17, 8, 0));
        assert_eq!(d.op, Op::Add);
        assert_eq!((d.rs, d.rt, d.rd, d.dest), (16, 17, 8, 8));
        // ADDI $t0, $s0, -1 -- rt is the DESTINATION for I-type, not a source
        let d = decode(i(0o10, 16, 8, 0xFFFF));
        assert_eq!(d.op, Op::Addi);
        assert_eq!((d.rs, d.rt, d.dest, d.imm), (16, 8, 8, 0xFFFF));
    }

    /// `SLL $0, $0, 0` — the all-zero word — is the canonical `NOP`, and it must
    /// decode as a real instruction rather than as `Reserved`. Getting this wrong
    /// makes every padding byte raise an exception.
    #[test]
    fn the_all_zero_word_is_nop_not_reserved() {
        let d = decode(0);
        assert_eq!(d.op, Op::Sll);
        assert_eq!(d.dest, 0, "writes $zero, so it commits nothing");
    }

    /// Decode is **total**: no 32-bit pattern may panic, and anything
    /// unrecognised becomes `Reserved` rather than silently acting as a `NOP`.
    /// A guest can execute arbitrary bytes.
    #[test]
    fn decode_is_total_over_every_opcode_and_funct() {
        // Every primary opcode with every SPECIAL funct, plus a sweep of the
        // rest of the encoding space.
        for opcode in 0..64u32 {
            for low in 0..64u32 {
                let word = (opcode << 26) | low;
                let _ = decode(word);
            }
        }
        for bit in 0..32 {
            let _ = decode(1u32 << bit);
        }
        assert_eq!(decode(0xFFFF_FFFF).op, Op::Reserved);
        // Not yet implemented => Reserved, which is loud. `LW` is opcode 0o43.
        assert_eq!(decode(i(0o43, 1, 2, 0)).op, Op::Reserved);
    }

    /// The `*32` shift variants add 32 to the encoded 5-bit field, so `sa` is the
    /// effective amount the helpers expect.
    #[test]
    fn the_32_shift_variants_add_32_to_the_encoded_field() {
        assert_eq!(decode(r(0o70, 0, 1, 2, 5)).sa, 5, "DSLL keeps sa");
        assert_eq!(decode(r(0o74, 0, 1, 2, 5)).sa, 37, "DSLL32 adds 32");
        assert_eq!(decode(r(0o76, 0, 1, 2, 0)).sa, 32, "DSRL32 of 0 is 32");
        assert_eq!(decode(r(0o77, 0, 1, 2, 31)).sa, 63, "DSRA32 tops out at 63");
    }

    /// Multiply/divide and `MTHI`/`MTLO` write `HI`/`LO`, so they have no general
    /// destination — and `dest = 0` must mean "nothing", not "$zero".
    #[test]
    fn hi_lo_writers_have_no_general_destination() {
        for funct in [0o30, 0o31, 0o32, 0o33, 0o34, 0o35, 0o36, 0o37, 0o21, 0o23] {
            let d = decode(r(funct, 1, 2, 3, 0));
            assert_eq!(d.dest, 0, "funct {funct:o} must not target rd");
            assert!(d.op.writes_hi_lo(), "funct {funct:o} should write HI/LO");
        }
        // MFHI/MFLO read them and DO have a destination.
        assert_eq!(decode(r(0o20, 0, 0, 9, 0)).dest, 9);
        assert!(!decode(r(0o20, 0, 0, 9, 0)).op.writes_hi_lo());
    }
}
