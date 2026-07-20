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

    // --- aligned loads
    /// `LB rt, off(base)` — signed byte.
    Lb,
    /// `LBU rt, off(base)`.
    Lbu,
    /// `LH rt, off(base)` — signed halfword.
    Lh,
    /// `LHU rt, off(base)`.
    Lhu,
    /// `LW rt, off(base)` — sign-extended into the 64-bit register.
    Lw,
    /// `LWU rt, off(base)` — zero-extended.
    Lwu,
    /// `LD rt, off(base)`.
    Ld,

    // --- aligned stores
    /// `SB rt, off(base)`.
    Sb,
    /// `SH rt, off(base)`.
    Sh,
    /// `SW rt, off(base)`.
    Sw,
    /// `SD rt, off(base)`.
    Sd,

    // --- the synchronisation pair (UM §16, pp. 453 and 487)
    //
    // The VR4300 is not a multiprocessor, but it implements these "in order to
    // maintain compatibility with VR4400 and VR4200" (UM §3.1), so they are real
    // instructions with observable behaviour, not reserved encodings.
    /// `LL rt, off(base)` — load word, sign-extend, set `LLbit` and `LLAddr`.
    Ll,
    /// `LLD rt, off(base)` — the doubleword form.
    Lld,
    /// `SC rt, off(base)` — store word iff `LLbit`; write the outcome to `rt`.
    Sc,
    /// `SCD rt, off(base)` — the doubleword form.
    Scd,

    // --- the unaligned family (used in pairs; see [`crate::mem`])
    /// `LWL rt, off(base)`.
    Lwl,
    /// `LWR rt, off(base)`.
    Lwr,
    /// `LDL rt, off(base)`.
    Ldl,
    /// `LDR rt, off(base)`.
    Ldr,
    /// `SWL rt, off(base)`.
    Swl,
    /// `SWR rt, off(base)`.
    Swr,
    /// `SDL rt, off(base)`.
    Sdl,
    /// `SDR rt, off(base)`.
    Sdr,

    // --- jumps
    /// `J target` — 26-bit region form.
    J,
    /// `JAL target` — links to `$31`.
    Jal,
    /// `JR rs` — register indirect.
    Jr,
    /// `JALR rd, rs` — register indirect, links to `rd`.
    Jalr,

    // --- branches. The `*L` forms are **branch-likely**: when NOT taken they
    // nullify the delay slot instead of executing it.
    /// `BEQ rs, rt, off`.
    Beq,
    /// `BNE rs, rt, off`.
    Bne,
    /// `BLEZ rs, off`.
    Blez,
    /// `BGTZ rs, off`.
    Bgtz,
    /// `BLTZ rs, off`.
    Bltz,
    /// `BGEZ rs, off`.
    Bgez,
    /// `BLTZAL rs, off` — links to `$31`.
    Bltzal,
    /// `BGEZAL rs, off` — links to `$31`.
    Bgezal,
    /// `BEQL` — branch-likely.
    Beql,
    /// `BNEL` — branch-likely.
    Bnel,
    /// `BLEZL` — branch-likely.
    Blezl,
    /// `BGTZL` — branch-likely.
    Bgtzl,
    /// `BLTZL` — branch-likely.
    Bltzl,
    /// `BGEZL` — branch-likely.
    Bgezl,
    /// `BLTZALL` — branch-likely, links.
    Bltzall,
    /// `BGEZALL` — branch-likely, links.
    Bgezall,

    // --- the trap family
    /// `TGE rs, rt`.
    Tge,
    /// `TGEU rs, rt`.
    Tgeu,
    /// `TLT rs, rt`.
    Tlt,
    /// `TLTU rs, rt`.
    Tltu,
    /// `TEQ rs, rt`.
    Teq,
    /// `TNE rs, rt`.
    Tne,
    /// `TGEI rs, imm`.
    Tgei,
    /// `TGEIU rs, imm`.
    Tgeiu,
    /// `TLTI rs, imm`.
    Tlti,
    /// `TLTIU rs, imm`.
    Tltiu,
    /// `TEQI rs, imm`.
    Teqi,
    /// `TNEI rs, imm`.
    Tnei,

    // --- COP0 access (T-12-001). The TLB and `ERET` encodings of this opcode
    // are NOT here: they are separate instructions landing in T-12-002/T-12-004,
    // and lumping them in would make `Op` claim support this crate lacks.
    /// `CACHE op, off(base)` — a cache maintenance operation.
    ///
    /// Decoded and **executed as an address-translating no-op**: this CPU does
    /// not model cache *contents*, so invalidate and write-back have nothing to
    /// act on. What matters is that it does **not raise** — IPL3 and libdragon
    /// both use it, so a `Reserved` decode blocks every real ROM. See
    /// `docs/cpu.md` and accuracy-ledger D-5.
    Cache,
    /// A COP0 **CO-class instruction in the `funct` 0x20-0x3F extension range**,
    /// executed as a no-op.
    ///
    /// # Why this is not `Reserved`
    ///
    /// n64-systemtest probes for the `emux` emulator by executing
    /// `COP0 CO funct 0x20` (its `XDETECT`) and reading the result out of a GPR.
    /// It does this from `init_allocator`, inside `entrypoint` -- **before**
    /// `main` installs any exception handler. If a real VR4300 raised Reserved
    /// Instruction there, the suite would derail on every N64 it has ever run
    /// on, before printing a single line. It does not, so hardware must retire
    /// these encodings harmlessly.
    ///
    /// The range is not a guess: the suite's own constant for the probe is named
    /// `XDETECT_CODE_EXTENSIONS_20_3F`, i.e. emux claims `funct` 0x20-0x3F as
    /// extension space precisely because the VR4300 leaves it inert.
    ///
    /// Decoding these to `Reserved` is what made the suite appear to hang: the
    /// RI dispatched to an uninstalled `0x8000_0180`, ran zeros as `NOP`s into
    /// `.text`, and faulted there instead.
    ///
    /// Recorded as an **inference** in the accuracy ledger (C-8), not a manual
    /// citation -- the writeback behaviour of the target GPR is untested.
    Cop0Extension,
    /// `CFC1 rt, fs` — read a COP1 **control** register.
    Cfc1,
    /// `CTC1 rt, fs` — write a COP1 **control** register.
    Ctc1,
    /// `MFC1 rt, fs` — move the low 32 bits of an FPR to a GPR, sign-extended.
    Mfc1,
    /// `DMFC1 rt, fs` — move a full 64-bit FGR to a GPR.
    Dmfc1,
    /// `MTC1 rt, fs` — move the low 32 bits of a GPR to an FPR.
    Mtc1,
    /// `DMTC1 rt, fs` — move a full 64-bit GPR to an FGR.
    Dmtc1,
    /// `LWC1 ft, off(base)` — load a word into an FPR.
    Lwc1,
    /// `LDC1 ft, off(base)` — load a doubleword into an FPR.
    Ldc1,
    /// `SWC1 ft, off(base)` — store an FPR word.
    Swc1,
    /// `SDC1 ft, off(base)` — store an FPR doubleword.
    Sdc1,
    /// A COP1 encoding this crate does not implement.
    ///
    /// Distinct from [`Op::Reserved`]: the encoding is *valid*, so it must raise
    /// **Coprocessor Unusable** when `Status.CU1` is clear rather than Reserved
    /// Instruction. Conflating the two sends the handler the wrong `ExcCode`.
    Cop1Unimplemented,
    /// Any **COP2** encoding.
    ///
    /// The VR4300 has a COP2 unit, so these are architecturally *valid*
    /// encodings. With `Status.CU2` clear they raise **Coprocessor Unusable**,
    /// not Reserved Instruction — the same distinction as
    /// [`Op::Cop1Unimplemented`], and for the same reason.
    ///
    /// Decoding them as `Reserved` is what produced n64-systemtest's
    /// "Exception storm detected. Aborting." during `MFC2/MTC2/DMFC2/DMTC2`:
    /// the suite expects `ExcCode 11` and got `10` five times running, which
    /// tripped its recovery limit and truncated the whole run.
    Cop2,
    /// A COP1 **arithmetic** operation, format and operation carried in the
    /// already-decoded fields: `rs` is the format, `funct` the operation, with
    /// `rt`=ft, `rd`=fs and `sa`=fd.
    ///
    /// One variant rather than ~60, because the pipeline dispatches into
    /// `crate::fpu` on `(fmt, funct)` anyway and a variant per opcode would just
    /// be a second copy of that table.
    FpArith,
    /// `TLBR` — read the TLB entry `Index` names into the COP0 registers.
    Tlbr,
    /// `TLBWI` — write the COP0 registers into the entry **`Index`** names.
    Tlbwi,
    /// `TLBWR` — write them into the entry **`Random`** names.
    Tlbwr,
    /// `TLBP` — probe for an entry matching `EntryHi`.
    Tlbp,
    /// `ERET` — return from exception (UM Ch. 16, p. 434).
    ///
    /// Has **no delay slot** and must not be placed in one, unlike every other
    /// control transfer in the instruction set.
    Eret,
    /// `MFC0 rt, rd` — 32-bit read of a COP0 register, sign-extended.
    Mfc0,
    /// `DMFC0 rt, rd` — 64-bit read of a COP0 register.
    Dmfc0,
    /// `MTC0 rt, rd` — 32-bit write to a COP0 register.
    Mtc0,
    /// `DMTC0 rt, rd` — 64-bit write to a COP0 register.
    Dmtc0,

    /// `SYNC` — *"handled as a NOP"* on this processor (UM §3.1).
    ///
    /// Not folded into [`Op::Sll`]-as-NOP: it is a distinct encoding that
    /// compilers emit, and decoding it to [`Op::Reserved`] would raise a
    /// reserved-instruction exception on code that runs fine on hardware.
    Sync,
    /// `SYSCALL`.
    Syscall,
    /// `BREAK`.
    Break,
}

impl Op {
    /// Does this instruction have a branch delay slot?
    ///
    /// Every jump and branch on MIPS does. The instruction *after* it executes
    /// before the target — which is why `in_delay_slot` has to travel with the
    /// instruction rather than live in a global flag.
    #[must_use]
    pub const fn has_delay_slot(self) -> bool {
        matches!(
            self,
            Self::J
                | Self::Jal
                | Self::Jr
                | Self::Jalr
                | Self::Beq
                | Self::Bne
                | Self::Blez
                | Self::Bgtz
                | Self::Bltz
                | Self::Bgez
                | Self::Bltzal
                | Self::Bgezal
                | Self::Beql
                | Self::Bnel
                | Self::Blezl
                | Self::Bgtzl
                | Self::Bltzl
                | Self::Bgezl
                | Self::Bltzall
                | Self::Bgezall
        )
    }

    /// Is this a **branch-likely** form?
    ///
    /// When a likely branch is *not* taken it **nullifies** its delay slot — the
    /// instruction is fetched and then squashed. An ordinary branch executes its
    /// delay slot either way. Getting this backwards silently executes or skips
    /// one instruction per untaken branch.
    #[must_use]
    pub const fn is_likely(self) -> bool {
        matches!(
            self,
            Self::Beql
                | Self::Bnel
                | Self::Blezl
                | Self::Bgtzl
                | Self::Bltzl
                | Self::Bgezl
                | Self::Bltzall
                | Self::Bgezall
        )
    }

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
    /// The J-type 26-bit target field, bits 25..0. Shifted left 2 and combined
    /// with the delay slot's region bits to form the address.
    pub target: u32,
}

impl Decoded {
    /// Does this instruction load into a general register?
    ///
    /// The load-delay interlock keys off this: only a *load* result is
    /// unavailable in time to bypass, which is why the interlock exists at all.
    #[must_use]
    pub const fn is_load(self) -> bool {
        matches!(
            self.op,
            Op::Lb
                | Op::Lbu
                | Op::Lh
                | Op::Lhu
                | Op::Lw
                | Op::Lwu
                | Op::Ld
                | Op::Lwl
                | Op::Lwr
                | Op::Ldl
                | Op::Ldr
                | Op::Ll
                | Op::Lld
        )
    }

    /// Does this instruction write `rt` with a value the `DC` stage produces
    /// *without* going to memory for it?
    ///
    /// `SC`/`SCD` are the only such forms: they write the success flag to `rt`
    /// whether or not the store happens (UM §16 p. 487, *"A successful SC
    /// instruction sets the contents of general purpose register rt to 1; an
    /// unsuccessful SC instruction sets it to 0"*). They are therefore stores
    /// that also have a register destination — a shape nothing else in the
    /// integer set has, and one that a `is_load`-vs-store dichotomy silently
    /// gets wrong in both directions.
    ///
    /// Deliberately **not** folded into [`Self::is_load`]: the load-delay
    /// interlock exists because a *memory* result is not ready in time, and the
    /// `SC` flag is not a memory result. Treating it as a load would stall a
    /// cycle the hardware does not.
    #[must_use]
    pub const fn is_store_conditional(self) -> bool {
        matches!(self.op, Op::Sc | Op::Scd)
    }

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
const OP_LDL: u32 = 0o32;
const OP_LDR: u32 = 0o33;
const OP_LB: u32 = 0o40;
const OP_LH: u32 = 0o41;
const OP_LWL: u32 = 0o42;
const OP_LW: u32 = 0o43;
const OP_LBU: u32 = 0o44;
const OP_LHU: u32 = 0o45;
const OP_LWR: u32 = 0o46;
const OP_LWU: u32 = 0o47;
const OP_SB: u32 = 0o50;
const OP_SH: u32 = 0o51;
const OP_SWL: u32 = 0o52;
const OP_SW: u32 = 0o53;
const OP_SDL: u32 = 0o54;
const OP_SDR: u32 = 0o55;
const OP_SWR: u32 = 0o56;
const OP_COP0: u32 = 0o20;
const OP_COP1: u32 = 0o21;
/// COP2.
const OP_COP2: u32 = 0o22;
const OP_LWC1: u32 = 0o61;
const OP_LDC1: u32 = 0o65;
const OP_SWC1: u32 = 0o71;
const OP_SDC1: u32 = 0o75;
const OP_CACHE: u32 = 0o57;
const OP_LL: u32 = 0o60;
const OP_LLD: u32 = 0o64;
const OP_SC: u32 = 0o70;
const OP_SCD: u32 = 0o74;
const OP_LD: u32 = 0o67;
const OP_SD: u32 = 0o77;
const OP_REGIMM: u32 = 0o01;
const OP_J: u32 = 0o02;
const OP_JAL: u32 = 0o03;
const OP_BEQ: u32 = 0o04;
const OP_BNE: u32 = 0o05;
const OP_BLEZ: u32 = 0o06;
const OP_BGTZ: u32 = 0o07;
const OP_BEQL: u32 = 0o24;
const OP_BNEL: u32 = 0o25;
const OP_BLEZL: u32 = 0o26;
const OP_BGTZL: u32 = 0o27;

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
        target: word & 0x03FF_FFFF,
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
            0o10 => Decoded { op: Op::Jr, ..base },
            0o11 => r!(Op::Jalr),
            0o14 => Decoded {
                op: Op::Syscall,
                ..base
            },
            0o15 => Decoded {
                op: Op::Break,
                ..base
            },
            0o17 => Decoded {
                op: Op::Sync,
                ..base
            },
            0o60 => Decoded {
                op: Op::Tge,
                ..base
            },
            0o61 => Decoded {
                op: Op::Tgeu,
                ..base
            },
            0o62 => Decoded {
                op: Op::Tlt,
                ..base
            },
            0o63 => Decoded {
                op: Op::Tltu,
                ..base
            },
            0o64 => Decoded {
                op: Op::Teq,
                ..base
            },
            0o66 => Decoded {
                op: Op::Tne,
                ..base
            },
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

        // Loads write `rt`; stores read it and write memory, so they have no
        // register destination.
        OP_LB => i!(Op::Lb),
        OP_LBU => i!(Op::Lbu),
        OP_LH => i!(Op::Lh),
        OP_LHU => i!(Op::Lhu),
        OP_LW => i!(Op::Lw),
        OP_LWU => i!(Op::Lwu),
        OP_LD => i!(Op::Ld),
        OP_LWL => i!(Op::Lwl),
        OP_LWR => i!(Op::Lwr),
        OP_LDL => i!(Op::Ldl),
        OP_LDR => i!(Op::Ldr),
        // LL/SC write `rt`, so they take the `i!` (destination = rt) shape even
        // though SC also stores. A store form with a destination is unusual
        // enough that giving SC the store shape here is the natural mistake:
        // the success flag would then never reach the register file.
        // COP0. The form is in `rs`; `rd` names the COP0 register, and for the
        // move-from forms `rt` is the GPR destination.
        OP_COP0 => {
            let rs = ((word >> 21) & 31) as u8;
            match rs {
                0o00 => Decoded {
                    op: Op::Mfc0,
                    dest: ((word >> 16) & 31) as u8,
                    ..base
                },
                0o01 => Decoded {
                    op: Op::Dmfc0,
                    dest: ((word >> 16) & 31) as u8,
                    ..base
                },
                // The move-TO forms write COP0, not a GPR, so `dest` stays 0 --
                // giving them a GPR destination would corrupt the register the
                // instruction reads its value from.
                0o04 => Decoded {
                    op: Op::Mtc0,
                    ..base
                },
                0o05 => Decoded {
                    op: Op::Dmtc0,
                    ..base
                },
                // rs bit 4 set: the CP0 "CO" forms -- TLBR/TLBWI/TLBWR/TLBP
                // and ERET, distinguished by the `funct` field. Only ERET is
                // implemented; the TLB forms arrive with T-12-004 and stay
                // `Reserved` until then rather than decoding to a no-op.
                rs if rs & 0o20 != 0 => match word & 0o77 {
                    0o01 => Decoded {
                        op: Op::Tlbr,
                        ..base
                    },
                    0o02 => Decoded {
                        op: Op::Tlbwi,
                        ..base
                    },
                    0o06 => Decoded {
                        op: Op::Tlbwr,
                        ..base
                    },
                    0o10 => Decoded {
                        op: Op::Tlbp,
                        ..base
                    },
                    0o30 => Decoded {
                        op: Op::Eret,
                        ..base
                    },
                    // funct 0x20-0x3F: the emux extension space, inert on
                    // hardware. See `Op::Cop0Extension`.
                    0o40..=0o77 => Decoded {
                        op: Op::Cop0Extension,
                        ..base
                    },
                    _ => base,
                },
                _ => base,
            }
        }
        // COP2. Every encoding is valid on the VR4300, so the usability check
        // in EX decides between executing and Coprocessor Unusable. No COP2
        // operation is implemented, which is why one arm covers the opcode.
        OP_COP2 => Decoded {
            op: Op::Cop2,
            ..base
        },
        // COP1. The CONTROL moves (T-12-006), the DATA moves (T-13-001) and the
        // S/D arithmetic below are implemented; the remaining formats and the
        // conversions are not, and the FP load/store forms have their own
        // primary opcodes. Everything unhandled decodes to `Cop1Unimplemented`
        // rather than `Reserved`, because the encodings are valid and must raise
        // Coprocessor Unusable, not Reserved Instruction.
        OP_COP1 => {
            let rs = ((word >> 21) & 31) as u8;
            match rs {
                // The move-FROM forms write a GPR; the move-TO forms write an
                // FPR, so `dest` (a GPR index) stays 0 for those or they would
                // clobber the register they read their value from.
                0o00 => Decoded {
                    op: Op::Mfc1,
                    dest: ((word >> 16) & 31) as u8,
                    ..base
                },
                0o01 => Decoded {
                    op: Op::Dmfc1,
                    dest: ((word >> 16) & 31) as u8,
                    ..base
                },
                0o02 => Decoded {
                    op: Op::Cfc1,
                    dest: ((word >> 16) & 31) as u8,
                    ..base
                },
                // Format 16 = single, 17 = double. `funct` 0..=3 are
                // ADD/SUB/MUL/DIV and 5..=7 are ABS/MOV/NEG; `funct` 4 is
                // `SQRT`, still unwired, and everything above 7 (the
                // conversions and `C.cond.fmt`) stays `Cop1Unimplemented`.
                //
                // **`MOV` matters far more than its size suggests.** It is
                // funct 6, so admitting only `<= 3` made every `MOV.fmt` a
                // silent no-op — and the compiler emits one for each FP
                // argument and each FP return value. A single n64-systemtest
                // FP thunk contains three, so its operands were stale and its
                // result never left the callee. That accounted for the whole
                // `Result after <op>` failure block, which had been read as an
                // FPU arithmetic fault for nine rounds (ledger C-10).
                // `funct` 4 is `SQRT`, which has no implementation yet and so
                // stays `Cop1Unimplemented` rather than becoming a wrong
                // result. Everything else in the S/D formats is wired:
                //
                // | `funct` | Operation |
                // | --- | --- |
                // | `0..=3` | `ADD` / `SUB` / `MUL` / `DIV` |
                // | `5..=7` | `ABS` / `MOV` / `NEG` |
                // | `0o10..=0o17` | `ROUND`/`TRUNC`/`CEIL`/`FLOOR` to `.L` then `.W` |
                // | `0o40`/`0o41`/`0o44`/`0o45` | `CVT.S` / `CVT.D` / `CVT.W` / `CVT.L` |
                // | `0o60..=0o77` | `C.cond.fmt`, the low 4 bits being the condition |
                0o20 | 0o21
                    if matches!(
                        word & 0o77,
                        0..=3 | 5..=7 | 0o10..=0o17 | 0o40 | 0o41 | 0o44 | 0o45 | 0o60..=0o77
                    ) =>
                {
                    Decoded {
                        op: Op::FpArith,
                        ..base
                    }
                }
                // The **integer** source formats, `.W` (20) and `.L` (21).
                //
                // Easy to miss: `CVT.S.W` carries its source format in the same
                // `fmt` field, so a decoder that only admits 16/17 leaves every
                // integer-to-float conversion a silent no-op — the same shape of
                // gap that made `MOV.fmt` cost nine rounds. Only `CVT.S` and
                // `CVT.D` are defined from these formats; converting an integer
                // to an integer is not an instruction.
                0o24 | 0o25 if matches!(word & 0o77, 0o40 | 0o41) => Decoded {
                    op: Op::FpArith,
                    ..base
                },
                0o04 => Decoded {
                    op: Op::Mtc1,
                    ..base
                },
                0o05 => Decoded {
                    op: Op::Dmtc1,
                    ..base
                },
                0o06 => Decoded {
                    op: Op::Ctc1,
                    ..base
                },
                _ => Decoded {
                    op: Op::Cop1Unimplemented,
                    ..base
                },
            }
        }
        // CACHE writes no register: `rt` is the operation selector, not a
        // destination. Giving it the `i!` shape would clobber a GPR chosen by
        // the cache-op encoding, which is a spectacularly confusing bug.
        OP_CACHE => Decoded {
            op: Op::Cache,
            ..base
        },
        // The FP load/store forms. `rt` names an FPR, not a GPR, so `dest`
        // stays 0 -- giving them a GPR destination corrupts an integer register.
        OP_LWC1 => Decoded {
            op: Op::Lwc1,
            ..base
        },
        OP_LDC1 => Decoded {
            op: Op::Ldc1,
            ..base
        },
        OP_SWC1 => Decoded {
            op: Op::Swc1,
            ..base
        },
        OP_SDC1 => Decoded {
            op: Op::Sdc1,
            ..base
        },
        OP_LL => i!(Op::Ll),
        OP_LLD => i!(Op::Lld),
        OP_SC => i!(Op::Sc),
        OP_SCD => i!(Op::Scd),
        OP_SB => Decoded { op: Op::Sb, ..base },
        OP_SH => Decoded { op: Op::Sh, ..base },
        OP_SW => Decoded { op: Op::Sw, ..base },
        OP_SD => Decoded { op: Op::Sd, ..base },
        OP_SWL => Decoded {
            op: Op::Swl,
            ..base
        },
        OP_SWR => Decoded {
            op: Op::Swr,
            ..base
        },
        OP_SDL => Decoded {
            op: Op::Sdl,
            ..base
        },
        OP_SDR => Decoded {
            op: Op::Sdr,
            ..base
        },

        // Jumps and branches write no general register except the linking forms,
        // which target $31 (or `rd` for JALR).
        OP_J => Decoded { op: Op::J, ..base },
        OP_JAL => Decoded {
            op: Op::Jal,
            dest: 31,
            ..base
        },
        OP_BEQ => Decoded {
            op: Op::Beq,
            ..base
        },
        OP_BNE => Decoded {
            op: Op::Bne,
            ..base
        },
        OP_BLEZ => Decoded {
            op: Op::Blez,
            ..base
        },
        OP_BGTZ => Decoded {
            op: Op::Bgtz,
            ..base
        },
        OP_BEQL => Decoded {
            op: Op::Beql,
            ..base
        },
        OP_BNEL => Decoded {
            op: Op::Bnel,
            ..base
        },
        OP_BLEZL => Decoded {
            op: Op::Blezl,
            ..base
        },
        OP_BGTZL => Decoded {
            op: Op::Bgtzl,
            ..base
        },

        // REGIMM: the `rt` field selects the operation, not a register.
        OP_REGIMM => {
            let linking = matches!(rt, 0o20..=0o23);
            let op = match rt {
                0o00 => Op::Bltz,
                0o01 => Op::Bgez,
                0o02 => Op::Bltzl,
                0o03 => Op::Bgezl,
                0o10 => Op::Tgei,
                0o11 => Op::Tgeiu,
                0o12 => Op::Tlti,
                0o13 => Op::Tltiu,
                0o14 => Op::Teqi,
                0o16 => Op::Tnei,
                0o20 => Op::Bltzal,
                0o21 => Op::Bgezal,
                0o22 => Op::Bltzall,
                0o23 => Op::Bgezall,
                _ => Op::Reserved,
            };
            Decoded {
                op,
                dest: if linking { 31 } else { 0 },
                ..base
            }
        }
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
        // These must be encodings the VR4300 genuinely leaves UNASSIGNED, not
        // merely ones this project has not implemented yet. Primary opcodes
        // 0o34..0o37 are reserved on MIPS III, and SPECIAL funct 0o01 is unused.
        // Earlier revisions of this test used LW and then BEQ, and had to be
        // repointed each time that opcode landed -- which made the test track
        // implementation progress instead of the architecture.
        assert_eq!(decode(r(0o01, 1, 2, 3, 0)).op, Op::Reserved);
        assert_eq!(decode(i(0o35, 1, 2, 0)).op, Op::Reserved);
        assert_eq!(decode(i(0o36, 1, 2, 0)).op, Op::Reserved);
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

    /// `SC` is a store that nonetheless writes `rt`, so it must decode with a
    /// destination. The unit tests in `pipeline` construct `MemOp` directly and
    /// therefore cannot catch a decode that drops it — this one can.
    #[test]
    fn the_synchronisation_pair_decodes_with_rt_as_the_destination() {
        // opcode, rs=1 (base), rt=9, imm=0x20
        let enc = |opcode: u32| (opcode << 26) | (1 << 21) | (9 << 16) | 0x20;

        for (opcode, op) in [
            (0o60u32, Op::Ll),
            (0o64, Op::Lld),
            (0o70, Op::Sc),
            (0o74, Op::Scd),
        ] {
            let d = decode(enc(opcode));
            assert_eq!(d.op, op, "opcode {opcode:#o}");
            assert_eq!(d.rs, 1, "{op:?} base");
            assert_eq!(d.imm, 0x20, "{op:?} offset");
            assert_eq!(
                d.dest, 9,
                "{op:?} must write rt -- SC reports success there even when it stores nothing"
            );
        }
    }

    /// The interlock must treat `LL`/`LLD` as loads (their value comes from
    /// memory) but `SC`/`SCD` as not loads (the flag does not).
    #[test]
    fn only_the_linked_loads_count_as_loads_for_the_interlock() {
        let enc = |opcode: u32| (opcode << 26) | (1 << 21) | (9 << 16);
        assert!(decode(enc(0o60)).is_load(), "LL");
        assert!(decode(enc(0o64)).is_load(), "LLD");
        assert!(!decode(enc(0o70)).is_load(), "SC is not a load");
        assert!(!decode(enc(0o74)).is_load(), "SCD is not a load");
        assert!(decode(enc(0o70)).is_store_conditional(), "SC");
        assert!(decode(enc(0o74)).is_store_conditional(), "SCD");
        assert!(!decode(enc(0o53)).is_store_conditional(), "SW is not");
    }

    /// `SYNC` is a real encoding the VR4300 retires as a NOP (UM §3.1).
    /// Decoding it to `Reserved` raises a reserved-instruction exception on
    /// code that runs fine on hardware — compilers emit it.
    #[test]
    fn sync_decodes_to_a_nop_not_a_reserved_instruction() {
        let d = decode(0o17);
        assert_eq!(d.op, Op::Sync);
        assert_ne!(d.op, Op::Reserved, "SYNC must not raise");
        assert_eq!(d.dest, 0, "SYNC writes no register");
    }

    /// COP0 CO `funct` 0x20-0x3F is the **emux extension range** and must retire
    /// as a no-op, not raise Reserved Instruction.
    ///
    /// n64-systemtest probes it from `init_allocator`, inside `entrypoint`,
    /// **before** `main` installs an exception handler -- so an RI here derails
    /// the suite before it prints a line. Decoding it to `Reserved` is exactly
    /// what made the suite appear to hang: the RI dispatched to an uninstalled
    /// `0x8000_0180`, ran zeros as `NOP`s into `.text`, and faulted there.
    #[test]
    fn cop0_co_extension_functs_are_inert_not_reserved() {
        // The exact word n64-systemtest executes: COP0, rs = CO, funct = 0x20.
        assert_eq!(decode(0x4280_0060).op, Op::Cop0Extension, "emux XDETECT");
        // The rest of the documented extension space.
        for funct in 0x20u32..=0x3F {
            let word = (0x10 << 26) | (0x10 << 21) | funct;
            assert_eq!(
                decode(word).op,
                Op::Cop0Extension,
                "COP0 CO funct {funct:#04X} is extension space"
            );
        }
        // Below 0x20 the real CO instructions and the genuinely reserved
        // encodings are unaffected.
        assert_eq!(decode((0x10 << 26) | (0x10 << 21) | 0x18).op, Op::Eret);
        assert_eq!(decode((0x10 << 26) | (0x10 << 21) | 0x02).op, Op::Tlbwi);
        assert_eq!(
            decode((0x10 << 26) | (0x10 << 21) | 0x1F).op,
            Op::Reserved,
            "funct 0x1F is still reserved -- the range starts at 0x20"
        );
    }

    /// **COP2 encodings are valid**, so they must not decode to `Reserved`.
    ///
    /// With `Status.CU2` clear they raise Coprocessor Unusable (`ExcCode 11`);
    /// `Reserved` would raise `10`. n64-systemtest's `MFC2/MTC2/DMFC2/DMTC2`
    /// test saw `10` five times running, tripped its recovery limit, and
    /// aborted the entire run with "Exception storm detected".
    #[test]
    fn cop2_encodings_are_valid_not_reserved() {
        // MFC2, DMFC2, CFC2, MTC2, DMTC2, CTC2 -- the `rs` sub-opcodes.
        for rs in [0o00u32, 0o01, 0o02, 0o04, 0o05, 0o06] {
            let word = (0o22 << 26) | (rs << 21);
            assert_eq!(
                decode(word).op,
                Op::Cop2,
                "COP2 rs={rs:#o} is a valid encoding"
            );
        }
    }

    /// COP1 S/D arithmetic decodes to [`Op::FpArith`], not `Cop1Unimplemented`.
    ///
    /// The FPU has been implemented in `fpu.rs` since Sprint 3, but nothing
    /// decoded to it, so the whole unit was unreachable from an instruction
    /// stream — which is why COP1 accounted for 85% of n64-systemtest's
    /// failures.
    #[test]
    fn cop1_single_and_double_arithmetic_decode_to_fp_arith() {
        // COP1, fmt, ft, fs, fd, funct
        let enc = |fmt: u32, funct: u32| {
            (0o21 << 26) | (fmt << 21) | (2 << 16) | (3 << 11) | (4 << 6) | funct
        };
        for fmt in [0o20u32, 0o21] {
            for funct in 0..=3u32 {
                let d = decode(enc(fmt, funct));
                assert_eq!(d.op, Op::FpArith, "fmt {fmt:#o} funct {funct}");
                assert_eq!(d.rs, fmt as u8, "rs carries the format");
                assert_eq!(d.rt, 2, "rt = ft");
                assert_eq!(d.rd, 3, "rd = fs");
                assert_eq!(d.sa, 4, "sa = fd");
            }
        }
        // funct 4 (SQRT) is not wired yet and must stay unimplemented, NOT
        // become a wrong ADD.
        assert_eq!(decode(enc(0o20, 4)).op, Op::Cop1Unimplemented);
    }

    /// **The compares and conversions must decode.** They are implemented in
    /// `fpu.rs` and were unreachable for the same reason `MOV` was: the decode
    /// arm admitted only `funct 0..=3` and `5..=7`.
    ///
    /// Enumerated rather than spot-checked. The failure mode here is a *gap* in
    /// a range, and a gap is exactly what a single representative encoding does
    /// not find.
    #[test]
    fn the_compares_and_conversions_decode_rather_than_no_op() {
        let enc = |fmt: u32, funct: u32| {
            (0o21 << 26) | (fmt << 21) | (2 << 16) | (3 << 11) | (4 << 6) | funct
        };
        for fmt in [0o20u32, 0o21] {
            // ROUND/TRUNC/CEIL/FLOOR to .L (8..=11) then to .W (12..=15).
            for funct in 0o10..=0o17u32 {
                assert_eq!(
                    decode(enc(fmt, funct)).op,
                    Op::FpArith,
                    "fmt {fmt:#o} funct {funct:#o}"
                );
            }
            // CVT.S / CVT.D / CVT.W / CVT.L.
            for funct in [0o40u32, 0o41, 0o44, 0o45] {
                assert_eq!(
                    decode(enc(fmt, funct)).op,
                    Op::FpArith,
                    "CVT funct {funct:#o}"
                );
            }
            // All sixteen C.cond.fmt forms.
            for funct in 0o60..=0o77u32 {
                assert_eq!(
                    decode(enc(fmt, funct)).op,
                    Op::FpArith,
                    "C.cond funct {funct:#o}"
                );
            }
        }
        // The INTEGER source formats. Easy to miss, because `CVT.S.W` carries
        // its source format in the same field as `.S`/`.D` — a decoder that
        // admits only 16/17 leaves every integer-to-float conversion a no-op.
        for fmt in [0o24u32, 0o25] {
            for funct in [0o40u32, 0o41] {
                assert_eq!(
                    decode(enc(fmt, funct)).op,
                    Op::FpArith,
                    "fmt {fmt:#o} funct {funct:#o}"
                );
            }
        }
        // `SQRT` has no implementation, so it must stay unimplemented rather
        // than being swept in by a too-wide range.
        assert_eq!(
            decode(enc(0o20, 4)).op,
            Op::Cop1Unimplemented,
            "SQRT is still unwired"
        );
    }

    /// **`MOV.fmt` (funct 6) must decode.** With the arm admitting only
    /// `funct <= 3` it did not, and executed as a silent no-op.
    ///
    /// That is not a cosmetic gap. The compiler emits `MOV.fmt` for every FP
    /// argument and every FP return value, so a no-op left callees reading
    /// stale operands and callers reading a register the callee never wrote.
    /// It cost the whole `Result after <op>` block in n64-systemtest and nine
    /// rounds of investigation aimed at the FPU (ledger C-10) — the arithmetic
    /// was correct the entire time.
    ///
    /// ABS (5) and NEG (7) share the arm and are covered here for the same
    /// reason: nothing fails when a move quietly does nothing.
    #[test]
    fn abs_mov_and_neg_decode_rather_than_silently_doing_nothing() {
        let enc = |fmt: u32, funct: u32| (0o21 << 26) | (fmt << 21) | (3 << 11) | (4 << 6) | funct;
        for fmt in [0o20u32, 0o21] {
            for funct in 5..=7u32 {
                let d = decode(enc(fmt, funct));
                assert_eq!(
                    d.op,
                    Op::FpArith,
                    "fmt {fmt:#o} funct {funct} must not be a no-op"
                );
                assert_eq!(d.rd, 3, "rd = fs");
                assert_eq!(d.sa, 4, "sa = fd");
            }
        }
        // The exact encoding the correlated capture found in the delay slot of
        // the failing test's `jr $ra`: `MOV.S $f0, $f4`.
        let d = decode(0x4600_2006);
        assert_eq!(d.op, Op::FpArith, "MOV.S $f0, $f4 must decode");
        assert_eq!((d.rs, d.rd, d.sa), (0o20, 4, 0), "fmt=S, fs=4, fd=0");
    }
}
