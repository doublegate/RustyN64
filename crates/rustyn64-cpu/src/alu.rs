//! The VR4300 integer ALU as pure functions (T-11-002).
//!
//! Kept free of pipeline and register-file state on purpose: every rule here is
//! a property of the *arithmetic*, so it can be tested exhaustively without
//! constructing a machine. The pipeline calls these from `EX`.
//!
//! # The two rules that dominate MIPS III
//!
//! **32-bit results are sign-extended into the 64-bit register.** Every `*W`-class
//! operation produces a 32-bit value that is then sign-extended to 64 bits before
//! it reaches the register file. Skipping this is the single most common source
//! of MIPS III emulator bugs, because it is invisible until a program compares or
//! branches on the upper half.
//!
//! **The documented errata are reproduced, not corrected.** `SRA`/`SRAV` and the
//! `MULT`/`DIV` sign-extension bugs are real hardware behaviour that software can
//! observe and depend on. Implementing them "correctly" per the manual is the bug
//! — see [`sra`] and [`mult`]. Each is pinned by a test that fails if it is
//! "fixed", so the intent survives a well-meaning future reader.

use crate::Exception;

/// Sign-extend a 32-bit result into the 64-bit register file.
///
/// The MIPS III rule for every `*W` operation. Named rather than inlined so that
/// call sites read as an explicit statement of intent.
#[must_use]
pub const fn sext32(v: u32) -> u64 {
    v as i32 as i64 as u64
}

// ---------------------------------------------------------------- arithmetic

/// `ADD` — 32-bit signed add, **traps on overflow**.
///
/// # Errors
/// [`Exception::Overflow`] when the signed 32-bit addition overflows. The
/// register is left unmodified in that case (the trap precedes the write-back).
pub const fn add(a: u64, b: u64) -> Result<u64, Exception> {
    let (r, ovf) = (a as i32).overflowing_add(b as i32);
    if ovf {
        Err(Exception::Overflow)
    } else {
        Ok(sext32(r as u32))
    }
}

/// `ADDU` — 32-bit add, no trap. The `U` means "unchecked", not "unsigned":
/// the result is still sign-extended as a signed 32-bit value.
#[must_use]
pub const fn addu(a: u64, b: u64) -> u64 {
    sext32((a as u32).wrapping_add(b as u32))
}

/// `SUB` — 32-bit signed subtract, **traps on overflow**.
///
/// # Errors
/// [`Exception::Overflow`] on signed 32-bit overflow.
pub const fn sub(a: u64, b: u64) -> Result<u64, Exception> {
    let (r, ovf) = (a as i32).overflowing_sub(b as i32);
    if ovf {
        Err(Exception::Overflow)
    } else {
        Ok(sext32(r as u32))
    }
}

/// `SUBU` — 32-bit subtract, no trap.
#[must_use]
pub const fn subu(a: u64, b: u64) -> u64 {
    sext32((a as u32).wrapping_sub(b as u32))
}

/// `DADD` — 64-bit signed add, **traps on overflow**.
///
/// # Errors
/// [`Exception::Overflow`] on signed 64-bit overflow.
pub const fn dadd(a: u64, b: u64) -> Result<u64, Exception> {
    let (r, ovf) = (a as i64).overflowing_add(b as i64);
    if ovf {
        Err(Exception::Overflow)
    } else {
        Ok(r as u64)
    }
}

/// `DADDU` — 64-bit add, no trap.
#[must_use]
pub const fn daddu(a: u64, b: u64) -> u64 {
    a.wrapping_add(b)
}

/// `DSUB` — 64-bit signed subtract, **traps on overflow**.
///
/// # Errors
/// [`Exception::Overflow`] on signed 64-bit overflow.
pub const fn dsub(a: u64, b: u64) -> Result<u64, Exception> {
    let (r, ovf) = (a as i64).overflowing_sub(b as i64);
    if ovf {
        Err(Exception::Overflow)
    } else {
        Ok(r as u64)
    }
}

/// `DSUBU` — 64-bit subtract, no trap.
#[must_use]
pub const fn dsubu(a: u64, b: u64) -> u64 {
    a.wrapping_sub(b)
}

/// `SLT` — set on less than, signed 64-bit comparison.
#[must_use]
pub const fn slt(a: u64, b: u64) -> u64 {
    ((a as i64) < (b as i64)) as u64
}

/// `SLTU` — set on less than, unsigned 64-bit comparison.
#[must_use]
pub const fn sltu(a: u64, b: u64) -> u64 {
    (a < b) as u64
}

// ------------------------------------------------------------------ logical

// The logical family operates on the full 64 bits and needs no sign extension:
// the operands are already 64-bit register values and the result is too. This is
// the one family where the 32-bit/64-bit distinction does not arise.

/// `AND` — bitwise and, full 64-bit.
#[must_use]
pub const fn and(a: u64, b: u64) -> u64 {
    a & b
}

/// `OR` — bitwise or, full 64-bit.
#[must_use]
pub const fn or(a: u64, b: u64) -> u64 {
    a | b
}

/// `XOR` — bitwise exclusive or, full 64-bit.
#[must_use]
pub const fn xor(a: u64, b: u64) -> u64 {
    a ^ b
}

/// `NOR` — bitwise nor, full 64-bit. MIPS has no `NOT`; `NOR rd, rs, $0` is it.
#[must_use]
pub const fn nor(a: u64, b: u64) -> u64 {
    !(a | b)
}

/// `LUI` — load upper immediate.
///
/// The 16-bit immediate is placed in bits 31..16 and the **32-bit** result is
/// then sign-extended, so a `LUI` of `0x8000` produces `0xFFFF_FFFF_8000_0000`
/// rather than `0x0000_0000_8000_0000`.
#[must_use]
pub const fn lui(imm: u16) -> u64 {
    sext32((imm as u32) << 16)
}

// ------------------------------------------------------------------- shifts

/// `SLL` — 32-bit shift left logical, result sign-extended.
///
/// Note `SLL $0, $0, 0` is the canonical `NOP`.
#[must_use]
pub const fn sll(v: u64, sa: u32) -> u64 {
    sext32((v as u32) << (sa & 31))
}

/// `SRL` — 32-bit shift right logical, result sign-extended.
#[must_use]
pub const fn srl(v: u64, sa: u32) -> u64 {
    sext32((v as u32) >> (sa & 31))
}

/// `SRA` — 32-bit shift right arithmetic. **Reproduces the VR4300 erratum.**
///
/// The processor manual says the low 32 bits are filled with copies of bit 31 and
/// bit 31 is then sign-extended into the upper half. **Hardware does not do
/// that.** In practice the most significant bits are filled from the *upper 32
/// bits of the register* first, and the new bit 31 is then sign-extended — which
/// leaks 64-bit state that should be inaccessible, in both 32- and 64-bit mode.
///
/// ```text
/// manual:   rd = (uint64_t)(int32_t)((int32_t)rt >> sa)
/// hardware: rd = (uint64_t)(int32_t)((int64_t)rt >> sa)
/// ```
///
/// With `rt = 0x0123456789ABCDEF`, `sa = 16`, the manual predicts
/// `0xFFFFFFFFFFFF89AB`; hardware gives `0x00000000456789AB`.
///
/// This is **not** a bug to fix. It is present on more consoles than the
/// multiplication erratum and is not known to have ever been corrected, so
/// software can depend on it. `sra_reproduces_the_vr4300_erratum` fails if it is
/// "corrected". Source: `n64brew_wiki/markdown/VR4300.md` § Known Bugs.
#[must_use]
pub const fn sra(v: u64, sa: u32) -> u64 {
    // The 64-bit shift, then truncate-and-sign-extend, is the erratum.
    sext32(((v as i64) >> (sa & 31)) as u32)
}

/// `DSLL` — 64-bit shift left logical.
///
/// `sa` is the **effective** shift amount, `0..64`, masked to 6 bits. The
/// encoding splits that range across two opcodes — `DSLL` carries a 5-bit field
/// for `0..32` and `DSLL32` adds 32 to it for `32..64` — but that split belongs
/// to decode, not here. Pass the effective amount; do not pre-mask to 5 bits, or
/// every `*32` variant silently becomes its non-`32` counterpart.
#[must_use]
pub const fn dsll(v: u64, sa: u32) -> u64 {
    v << (sa & 63)
}

/// `DSRL` — 64-bit shift right logical. `sa` is the effective `0..64` amount;
/// see [`dsll`] on the `DSRL`/`DSRL32` encoding split.
#[must_use]
pub const fn dsrl(v: u64, sa: u32) -> u64 {
    v >> (sa & 63)
}

/// `DSRA` — 64-bit shift right arithmetic. `sa` is the effective `0..64` amount;
/// see [`dsll`] on the `DSRA`/`DSRA32` encoding split.
///
/// Not affected by the `SRA` erratum:
/// it is already a 64-bit shift, so there is no truncation for the bug to
/// manifest through.
#[must_use]
pub const fn dsra(v: u64, sa: u32) -> u64 {
    ((v as i64) >> (sa & 63)) as u64
}

// --------------------------------------------------------- multiply / divide

/// The `HI`/`LO` register pair, written by every multiply and divide.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HiLo {
    /// `HI` — the high half, or the division remainder.
    pub hi: u64,
    /// `LO` — the low half, or the division quotient.
    pub lo: u64,
}

/// `MULT` — 32-bit signed multiply. **Reproduces the VR4300 sign-extension erratum.**
///
/// When the inputs are not properly sign-extended 32-bit values, `MULT` behaves as
/// a **64-bit by 35-bit** signed multiply: the second operand is sign-extended on
/// **bit 34** before a 64-bit multiplication, and the first is taken as a full
/// 64-bit value.
///
/// For well-formed inputs (both properly sign-extended 32-bit values) this reduces
/// to the expected 32×32 signed multiply, which is why the erratum is invisible to
/// ordinary compiler output and only surfaces with hand-written or miscompiled
/// code. Results are for processor revision 2.2
/// (`n64brew_wiki/markdown/VR4300.md` § Known Bugs).
#[must_use]
pub const fn mult(a: u64, b: u64) -> HiLo {
    // Sign-extend the second operand on bit 34 (a 35-bit signed value).
    let b35 = ((b << 29) as i64) >> 29;
    let product = (a as i64).wrapping_mul(b35);
    HiLo {
        hi: sext32((product >> 32) as u32),
        lo: sext32(product as u32),
    }
}

/// `MULTU` — 32-bit unsigned multiply. Both operands are taken as their low 32
/// bits, zero-extended; no erratum applies.
#[must_use]
pub const fn multu(a: u64, b: u64) -> HiLo {
    let product = (a as u32 as u64).wrapping_mul(b as u32 as u64);
    HiLo {
        hi: sext32((product >> 32) as u32),
        lo: sext32(product as u32),
    }
}

/// `DMULT` — 64-bit signed multiply, full 128-bit result.
#[must_use]
pub const fn dmult(a: u64, b: u64) -> HiLo {
    let product = (a as i64 as i128).wrapping_mul(b as i64 as i128);
    HiLo {
        hi: (product >> 64) as u64,
        lo: product as u64,
    }
}

/// `DMULTU` — 64-bit unsigned multiply, full 128-bit result.
#[must_use]
pub const fn dmultu(a: u64, b: u64) -> HiLo {
    let product = (a as u128).wrapping_mul(b as u128);
    HiLo {
        hi: (product >> 64) as u64,
        lo: product as u64,
    }
}

/// `DIV` — 32-bit signed divide. **Reproduces the VR4300 sign-extension erratum.**
///
/// Acts as a **32-bit by 35-bit** signed division: the dividend is sign-extended
/// on bit 31, the divisor on **bit 34**, before a 64-bit division.
///
/// # The unknown case
///
/// When bits 63 and 31 of the divisor **differ**, the quotient written to `LO` is
/// documented as incorrect and *"it is currently unclear how the outputs of this
/// last case are arrived at"* — unknown even to N64brew. `HI` is at least
/// well-defined: `remainder = (int32_t)(dividend - quotient * divisor)`, computed
/// in 64-bit.
///
/// This implementation performs the 32×35 division in that case too, which is a
/// **guess**, and it is recorded as such in `docs/accuracy-ledger.md`. It must be
/// characterised against hardware rather than left to look authoritative.
///
/// Divide-by-zero is architecturally *undefined* on MIPS; the values below follow
/// the conventional interpretation and also need hardware confirmation.
#[must_use]
pub const fn div(dividend: u64, divisor: u64) -> HiLo {
    let n = dividend as i32 as i64;
    // Sign-extend the divisor on bit 34 (a 35-bit signed value) -- the erratum.
    let d = ((divisor << 29) as i64) >> 29;
    if d == 0 {
        // Undefined per the architecture; conventional emulator behaviour.
        return HiLo {
            lo: if n < 0 { 1 } else { u64::MAX },
            hi: sext32(n as u32),
        };
    }
    // i64::MIN / -1 overflows; MIPS defines the result as the dividend.
    if n == i32::MIN as i64 && d == -1 {
        return HiLo {
            lo: sext32(n as u32),
            hi: 0,
        };
    }
    HiLo {
        lo: sext32(n.wrapping_div(d) as u32),
        hi: sext32(n.wrapping_rem(d) as u32),
    }
}

/// `DIVU` — 32-bit unsigned divide. No erratum applies.
#[must_use]
pub const fn divu(dividend: u64, divisor: u64) -> HiLo {
    let n = dividend as u32;
    let d = divisor as u32;
    if d == 0 {
        return HiLo {
            lo: sext32(u32::MAX),
            hi: sext32(n),
        };
    }
    HiLo {
        lo: sext32(n / d),
        hi: sext32(n % d),
    }
}

/// `DDIV` — 64-bit signed divide.
#[must_use]
pub const fn ddiv(dividend: u64, divisor: u64) -> HiLo {
    let n = dividend as i64;
    let d = divisor as i64;
    if d == 0 {
        return HiLo {
            lo: if n < 0 { 1 } else { u64::MAX },
            hi: n as u64,
        };
    }
    if n == i64::MIN && d == -1 {
        return HiLo {
            lo: n as u64,
            hi: 0,
        };
    }
    HiLo {
        lo: n.wrapping_div(d) as u64,
        hi: n.wrapping_rem(d) as u64,
    }
}

/// `DDIVU` — 64-bit unsigned divide.
#[must_use]
pub const fn ddivu(dividend: u64, divisor: u64) -> HiLo {
    if divisor == 0 {
        return HiLo {
            lo: u64::MAX,
            hi: dividend,
        };
    }
    HiLo {
        lo: dividend / divisor,
        hi: dividend % divisor,
    }
}

/// Pipeline stall in `PCycle`s for a multiply or divide (UM Table 3-12).
///
/// These **stall the entire pipeline** — they are not background operations that
/// complete while other instructions issue. `MULT` 5, `DIV` 37, `DMULT` 8,
/// `DDIV` 69, with the unsigned forms costing the same as the signed.
#[must_use]
pub const fn muldiv_stall_cycles(op: MulDiv) -> u32 {
    match op {
        MulDiv::Mult | MulDiv::Multu => 5,
        MulDiv::Div | MulDiv::Divu => 37,
        MulDiv::Dmult | MulDiv::Dmultu => 8,
        MulDiv::Ddiv | MulDiv::Ddivu => 69,
    }
}

/// The multiply/divide family, for cost lookup.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum MulDiv {
    /// `MULT` — 32-bit signed multiply.
    Mult,
    /// `MULTU` — 32-bit unsigned multiply.
    Multu,
    /// `DIV` — 32-bit signed divide.
    Div,
    /// `DIVU` — 32-bit unsigned divide.
    Divu,
    /// `DMULT` — 64-bit signed multiply.
    Dmult,
    /// `DMULTU` — 64-bit unsigned multiply.
    Dmultu,
    /// `DDIV` — 64-bit signed divide.
    Ddiv,
    /// `DDIVU` — 64-bit unsigned divide.
    Ddivu,
}

/// How many instructions after `MFHI`/`MFLO` must not write `HI`/`LO`.
///
/// A **non-interlocked** hazard: the hardware does not stall, it produces a wrong
/// result. From `n64brew_wiki/markdown/MIPS III instructions.md` § Hazards:
/// *"The `mfhi` and `mflo` instructions will produce incorrect results if any of
/// the two following instructions modify the `HI` and `LO` registers."*
///
/// Modelling this as a stall would be wrong in both directions: it would add
/// timing that hardware does not have, and it would hide the incorrect result
/// that software can actually observe.
pub const MFHI_MFLO_HAZARD_INSTRUCTIONS: u32 = 2;

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------- sign extension

    /// The rule that dominates MIPS III: every 32-bit result reaches the register
    /// file sign-extended. Missing it is invisible until software inspects the
    /// upper half, which is what makes it such a common bug.
    #[test]
    fn every_32_bit_result_is_sign_extended() {
        // A result with bit 31 set must fill the whole upper half with ones.
        assert_eq!(addu(0x8000_0000, 0), 0xFFFF_FFFF_8000_0000);
        assert_eq!(subu(0, 1), 0xFFFF_FFFF_FFFF_FFFF);
        assert_eq!(sll(1, 31), 0xFFFF_FFFF_8000_0000);
        assert_eq!(srl(0xFFFF_FFFF, 0), 0xFFFF_FFFF_FFFF_FFFF);
        // ...and a positive one must leave it clear.
        assert_eq!(addu(0x7FFF_FFFF, 0), 0x0000_0000_7FFF_FFFF);
        // 32-bit ops ignore the upper half of their inputs entirely.
        assert_eq!(addu(0xDEAD_BEEF_0000_0001, 1), 2);
    }

    // ------------------------------------------------------------ arithmetic

    #[test]
    fn add_traps_only_on_signed_32_bit_overflow() {
        assert_eq!(add(1, 2), Ok(3));
        assert_eq!(add(0x7FFF_FFFF, 1), Err(Exception::Overflow));
        // The unchecked form wraps instead of trapping.
        assert_eq!(addu(0x7FFF_FFFF, 1), 0xFFFF_FFFF_8000_0000);
        // Negative overflow traps too.
        assert_eq!(
            add(sext32(0x8000_0000), sext32(0xFFFF_FFFF)),
            Err(Exception::Overflow)
        );
        // Carry out of bit 31 without signed overflow does NOT trap.
        assert_eq!(add(sext32(0xFFFF_FFFF), 1), Ok(0));
    }

    #[test]
    fn sub_traps_only_on_signed_32_bit_overflow() {
        assert_eq!(sub(3, 1), Ok(2));
        assert_eq!(sub(sext32(0x8000_0000), 1), Err(Exception::Overflow));
        assert_eq!(subu(sext32(0x8000_0000), 1), 0x0000_0000_7FFF_FFFF);
        assert_eq!(sub(0, 1), Ok(0xFFFF_FFFF_FFFF_FFFF));
    }

    #[test]
    fn the_64_bit_forms_trap_at_64_bit_boundaries() {
        assert_eq!(dadd(1, 2), Ok(3));
        assert_eq!(dadd(i64::MAX as u64, 1), Err(Exception::Overflow));
        assert_eq!(daddu(i64::MAX as u64, 1), 0x8000_0000_0000_0000);
        assert_eq!(dsub(i64::MIN as u64, 1), Err(Exception::Overflow));
        assert_eq!(dsubu(i64::MIN as u64, 1), 0x7FFF_FFFF_FFFF_FFFF);
        // A value that overflows 32-bit ADD is fine for DADD.
        assert_eq!(dadd(0x7FFF_FFFF, 1), Ok(0x8000_0000));
    }

    #[test]
    fn set_on_less_than_compares_at_64_bits() {
        assert_eq!(slt(u64::MAX, 0), 1, "-1 < 0 signed");
        assert_eq!(sltu(u64::MAX, 0), 0, "max > 0 unsigned");
        assert_eq!(slt(0, 1), 1);
        assert_eq!(sltu(0, 1), 1);
    }

    #[test]
    fn the_logical_family_is_full_width_and_needs_no_sign_extension() {
        assert_eq!(
            and(0xFFFF_0000_FFFF_0000, 0x0F0F_0F0F_0F0F_0F0F),
            0x0F0F_0000_0F0F_0000
        );
        assert_eq!(
            or(0xFFFF_0000_0000_0000, 0x0000_0000_0000_FFFF),
            0xFFFF_0000_0000_FFFF
        );
        assert_eq!(xor(u64::MAX, u64::MAX), 0);
        // NOR with $zero is how MIPS spells NOT.
        assert_eq!(nor(0x0000_0000_0000_00FF, 0), 0xFFFF_FFFF_FFFF_FF00);
        // The upper half participates -- these are not 32-bit operations.
        assert_eq!(and(u64::MAX, 0xFFFF_FFFF_FFFF_FFFF), u64::MAX);
    }

    /// `LUI` is a 32-bit operation, so its result IS sign-extended -- a `LUI` of
    /// 0x8000 fills the upper half with ones. Missing this is a classic bug,
    /// because `LUI`+`ORI` address construction then silently breaks above 2 GiB.
    #[test]
    fn lui_sign_extends_its_32_bit_result() {
        assert_eq!(lui(0x8000), 0xFFFF_FFFF_8000_0000);
        assert_eq!(lui(0x7FFF), 0x0000_0000_7FFF_0000);
        assert_eq!(lui(0), 0);
    }

    // ---------------------------------------------------------------- shifts

    /// **The SRA erratum.** This test fails if someone "corrects" `sra` to match
    /// the processor manual. That correction is the bug: hardware leaks the upper
    /// 32 bits, on every console, and software can depend on it.
    ///
    /// Source: `n64brew_wiki/markdown/VR4300.md` § Known Bugs.
    #[test]
    fn sra_reproduces_the_vr4300_erratum() {
        // The worked example from the wiki.
        let rt = 0x0123_4567_89AB_CDEF;
        assert_eq!(
            sra(rt, 16),
            0x0000_0000_4567_89AB,
            "SRA must leak the upper 32 bits (the erratum), not sign-extend bit 31"
        );
        // What the manual claims, and what must NOT happen:
        let manual = sext32(((rt as u32) as i32 >> 16) as u32);
        assert_eq!(manual, 0xFFFF_FFFF_FFFF_89AB);
        assert_ne!(
            sra(rt, 16),
            manual,
            "the manual's behaviour is not hardware's"
        );

        // With a properly sign-extended input the erratum is invisible, which is
        // why ordinary compiler output never trips over it.
        let clean = sext32(0x89AB_CDEF);
        assert_eq!(sra(clean, 16), sext32(0xFFFF_89AB));
    }

    #[test]
    fn shift_amounts_are_masked_to_the_operand_width() {
        // 32-bit shifts mask to 5 bits, 64-bit to 6.
        assert_eq!(sll(1, 32), sll(1, 0));
        assert_eq!(dsll(1, 64), dsll(1, 0));
        assert_eq!(srl(0x8000_0000, 33), srl(0x8000_0000, 1));
    }

    /// The `*32` opcode variants are decode's business: they add 32 to the
    /// encoded 5-bit field and call the same helper. This pins that mapping, so
    /// the doc claim is enforced rather than merely asserted — pre-masking to 5
    /// bits here would silently turn every `*32` form into its counterpart.
    #[test]
    fn the_32_variants_are_the_same_helper_with_32_added() {
        let v = 0x0123_4567_89AB_CDEF;
        for encoded in 0..32u32 {
            // DSLL32 sa=n  ==  a 64-bit shift of (n + 32)
            assert_eq!(dsll(v, encoded + 32), v << (encoded + 32));
            assert_eq!(dsrl(v, encoded + 32), v >> (encoded + 32));
            assert_eq!(dsra(v, encoded + 32), ((v as i64) >> (encoded + 32)) as u64);
        }
        // And the two halves are genuinely different operations.
        assert_ne!(dsll(v, 1), dsll(v, 33));
    }

    #[test]
    fn the_64_bit_shifts_do_not_truncate() {
        assert_eq!(dsll(1, 63), 0x8000_0000_0000_0000);
        assert_eq!(dsrl(0x8000_0000_0000_0000, 63), 1);
        assert_eq!(dsra(0x8000_0000_0000_0000, 63), u64::MAX);
        // DSRA is a true 64-bit shift, so the SRA erratum cannot manifest.
        assert_eq!(dsra(0x0123_4567_89AB_CDEF, 16), 0x0000_0123_4567_89AB);
    }

    // ----------------------------------------------------- multiply / divide

    #[test]
    fn multiply_writes_hi_lo_with_sign_extended_halves() {
        let r = mult(sext32(0x0001_0000), sext32(0x0001_0000));
        assert_eq!((r.hi, r.lo), (1, 0), "0x10000^2 = 0x1_0000_0000");
        // Negative operands.
        let r = mult(sext32(0xFFFF_FFFF), sext32(2));
        assert_eq!(r.lo, sext32(0xFFFF_FFFE), "-1 * 2 = -2");
        assert_eq!(r.hi, u64::MAX, "the high half sign-extends too");
        // Unsigned does not sign-extend the operands.
        let r = multu(0xFFFF_FFFF, 2);
        assert_eq!((r.hi, r.lo), (1, sext32(0xFFFF_FFFE)));
    }

    /// **The MULT erratum**: with inputs that are not properly sign-extended
    /// 32-bit values, `MULT` acts as a 64-bit by *35-bit* signed multiply.
    #[test]
    fn mult_reproduces_the_35_bit_sign_extension_erratum() {
        // Bit 34 set in the second operand: hardware sign-extends from there, so
        // the value is treated as negative even though bit 31 is clear.
        let b = 0x0000_0004_0000_0000; // bit 34 set
        let got = mult(1, b);
        let naive = mult(1, 0); // what a 32-bit-only reading would give
        assert_ne!(
            got, naive,
            "the erratum must make bit 34 of the second operand significant"
        );
        // For well-formed 32-bit inputs the erratum is invisible.
        let clean = mult(sext32(7), sext32(6));
        assert_eq!(clean.lo, 42);
        assert_eq!(clean.hi, 0);
    }

    /// **The DIV erratum**: like `MULT`, `DIV` sign-extends its *divisor* on bit
    /// **34**, so it behaves as a 32-bit by 35-bit signed division rather than
    /// 32x32. Fails if someone "corrects" it to a plain 32-bit division.
    ///
    /// Source: `n64brew_wiki/markdown/VR4300.md` § Known Bugs, revision 2.2.
    #[test]
    fn div_reproduces_the_35_bit_divisor_sign_extension_erratum() {
        // Bit 34 set in the divisor: hardware sign-extends from there, so the
        // divisor is NEGATIVE despite bit 31 being clear. A 32-bit-only reading
        // would treat the low 32 bits (zero) as the divisor and divide by zero.
        let divisor = 0x0000_0004_0000_0000u64; // bit 34
        let got = div(sext32(100), divisor);
        let naive = div(sext32(100), 0); // what a 32-bit reading gives
        assert_ne!(
            got, naive,
            "bit 34 of the divisor must be significant -- that IS the erratum"
        );

        // For well-formed sign-extended 32-bit inputs the erratum is invisible,
        // which is why ordinary compiler output never trips over it.
        let clean = div(sext32(100), sext32(7));
        assert_eq!((clean.lo, clean.hi), (14, 2));
        let negative = div(sext32(100), sext32(0xFFFF_FFF9)); // 100 / -7
        assert_eq!(negative.lo, sext32(0xFFFF_FFF2), "-14");
    }

    /// `SRA` against **pre-computed** hardware values.
    ///
    /// Deliberately not `assert_eq!(sra(v, n), <the expression sra uses>)` — that
    /// restates the implementation and can only catch a *different* one, never a
    /// wrong one. These constants were derived independently from the erratum's
    /// definition: 64-bit arithmetic shift, truncate to 32, sign-extend.
    ///
    /// The `SRAV` *instruction path* is pinned separately in `exec`, because a
    /// helper-level test cannot see `Op::Srav` stop routing through this helper.
    #[test]
    fn sra_matches_precomputed_hardware_values() {
        let rt = 0x0123_4567_89AB_CDEF;
        assert_eq!(sra(rt, 1), 0xFFFF_FFFF_C4D5_E6F7, "sa=1");
        assert_eq!(sra(rt, 8), 0x0000_0000_6789_ABCD, "sa=8");
        assert_eq!(
            sra(rt, 16),
            0x0000_0000_4567_89AB,
            "sa=16, the wiki's example"
        );
        assert_eq!(sra(rt, 31), 0x0000_0000_0246_8ACF, "sa=31");
        // The amount masks to 5 bits, so 48 is 16.
        assert_eq!(sra(rt, 48), sra(rt, 16), "sa masks to 5 bits");
    }

    #[test]
    fn divide_writes_quotient_to_lo_and_remainder_to_hi() {
        let r = div(sext32(17), sext32(5));
        assert_eq!((r.lo, r.hi), (3, 2));
        // MIPS truncates toward zero, so a negative dividend gives a negative
        // remainder -- not the Euclidean result.
        let r = div(sext32(0xFFFF_FFEF), sext32(5)); // -17 / 5
        assert_eq!((r.lo, r.hi), (sext32(0xFFFF_FFFD), sext32(0xFFFF_FFFE)));
        let r = divu(17, 5);
        assert_eq!((r.lo, r.hi), (3, 2));
    }

    #[test]
    fn divide_by_zero_does_not_panic() {
        // Architecturally undefined; the values are conventional and flagged in
        // docs/accuracy-ledger.md as needing hardware confirmation. What is NOT
        // negotiable is that it must not panic -- a guest program can do this.
        let r = div(sext32(5), 0);
        assert_eq!(r.hi, sext32(5), "HI carries the dividend");
        let _ = divu(5, 0);
        let _ = ddiv(5, 0);
        let _ = ddivu(5, 0);
    }

    #[test]
    fn the_signed_divide_overflow_case_does_not_panic() {
        // i32::MIN / -1 has no representable result. MIPS defines it as the
        // dividend; in Rust the raw division would panic.
        let r = div(sext32(0x8000_0000), sext32(0xFFFF_FFFF));
        assert_eq!((r.lo, r.hi), (sext32(0x8000_0000), 0));
        let r = ddiv(i64::MIN as u64, u64::MAX);
        assert_eq!((r.lo, r.hi), (i64::MIN as u64, 0));
    }

    #[test]
    fn the_64_bit_multiplies_keep_the_full_128_bit_product() {
        let r = dmultu(u64::MAX, u64::MAX);
        assert_eq!((r.hi, r.lo), (0xFFFF_FFFF_FFFF_FFFE, 1));
        let r = dmult(u64::MAX, u64::MAX); // -1 * -1
        assert_eq!((r.hi, r.lo), (0, 1));
        let r = ddivu(u64::MAX, 2);
        assert_eq!(r.lo, 0x7FFF_FFFF_FFFF_FFFF);
    }

    /// The documented pipeline stalls (UM Table 3-12). These are full-pipeline
    /// stalls, not background operations.
    #[test]
    fn muldiv_stalls_match_the_manual() {
        assert_eq!(muldiv_stall_cycles(MulDiv::Mult), 5);
        assert_eq!(muldiv_stall_cycles(MulDiv::Multu), 5);
        assert_eq!(muldiv_stall_cycles(MulDiv::Div), 37);
        assert_eq!(muldiv_stall_cycles(MulDiv::Divu), 37);
        assert_eq!(muldiv_stall_cycles(MulDiv::Dmult), 8);
        assert_eq!(muldiv_stall_cycles(MulDiv::Dmultu), 8);
        assert_eq!(muldiv_stall_cycles(MulDiv::Ddiv), 69);
        assert_eq!(muldiv_stall_cycles(MulDiv::Ddivu), 69);
    }

    #[test]
    fn the_mfhi_mflo_hazard_is_two_instructions_and_not_a_stall() {
        // Documented as a non-interlocked hazard producing a wrong result, so the
        // only thing to assert here is the window. Modelling it as a stall would
        // add timing hardware does not have AND hide the observable wrong value.
        assert_eq!(MFHI_MFLO_HAZARD_INSTRUCTIONS, 2);
    }
}
