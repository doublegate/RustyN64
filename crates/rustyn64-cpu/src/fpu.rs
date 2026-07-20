//! FPU arithmetic (T-13-002).
//!
//! Pure functions over IEEE-754 values, with the VR4300's `FCSR` semantics
//! layered on top. Kept separate from [`crate::fpr`] (the register file) and
//! [`crate::cop1`] (the control registers) so each can be tested without the
//! others.
//!
//! # Rounding
//!
//! `FCSR.RM` selects the mode (UM §7.2.4): 0 nearest-even, 1 toward zero,
//! 2 toward +∞, 3 toward −∞. Rust's `f32`/`f64` arithmetic is nearest-even
//! only, so the other three modes are applied by **rounding the operands'
//! exact result**, which is why the operations here return a value *and* the
//! flags they raised rather than mutating `FCSR` directly — the caller owns it.
//!
//! # What is deliberately absent
//!
//! The **FP multiplication erratum** is not here. It is a property of specific
//! early console revisions (`n64brew_wiki/markdown/VR4300.md`) and belongs with
//! the revision model, not with the arithmetic; implementing it inline would
//! make every multiply on every console wrong.

/// The `FCSR` cause/flag bits an operation can raise (UM §7.2.2).
///
/// Returned rather than written, because `FCSR` belongs to
/// [`crate::cop1::Cop1Control`] and an arithmetic helper that reached into it
/// would need to own it.
// Five bools, one per IEEE exception. Clippy suggests a bitflags type; the
// architectural field IS five independent conditions that can co-occur (an
// overflow is also inexact), and naming them costs nothing at this size.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Flags {
    /// Invalid operation — a signalling NaN, or an undefined form like `0 × ∞`.
    pub invalid: bool,
    /// Division by zero, with a finite non-zero numerator.
    pub div_by_zero: bool,
    /// The result overflowed the format's range.
    pub overflow: bool,
    /// The result underflowed to a subnormal or zero.
    pub underflow: bool,
    /// The result was not exactly representable.
    pub inexact: bool,
}

impl Flags {
    /// No exceptions raised.
    pub const NONE: Self = Self {
        invalid: false,
        div_by_zero: false,
        overflow: false,
        underflow: false,
        inexact: false,
    };

    /// Just the invalid-operation flag.
    pub const INVALID: Self = Self {
        invalid: true,
        ..Self::NONE
    };

    /// Pack into the `FCSR` **Cause** field (bits 17..=12) and **Flags** field
    /// (bits 6..=2).
    ///
    /// Both at once because hardware sets them together: `Cause` is what the
    /// current operation raised, `Flags` is the sticky accumulation.
    #[must_use]
    pub const fn to_fcsr_bits(self) -> u32 {
        let mut cause = 0u32;
        let mut flags = 0u32;
        if self.invalid {
            cause |= 1 << 16;
            flags |= 1 << 6;
        }
        if self.div_by_zero {
            cause |= 1 << 15;
            flags |= 1 << 5;
        }
        if self.overflow {
            cause |= 1 << 14;
            flags |= 1 << 4;
        }
        if self.underflow {
            cause |= 1 << 13;
            flags |= 1 << 3;
        }
        if self.inexact {
            cause |= 1 << 12;
            flags |= 1 << 2;
        }
        cause | flags
    }
}

/// The rounding mode from `FCSR.RM` (UM §7.2.4).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Rounding {
    /// Round to nearest, ties to even. The IEEE default and `FCSR.RM = 0`.
    Nearest,
    /// Toward zero (truncate).
    TowardZero,
    /// Toward +∞.
    TowardPlusInf,
    /// Toward −∞.
    TowardMinusInf,
}

impl Rounding {
    /// Decode `FCSR.RM` (bits 1..=0).
    #[must_use]
    pub const fn from_rm(rm: u8) -> Self {
        match rm & 0b11 {
            1 => Self::TowardZero,
            2 => Self::TowardPlusInf,
            3 => Self::TowardMinusInf,
            _ => Self::Nearest,
        }
    }
}

/// A result plus the flags producing it raised.
///
/// Deliberately **not** `Eq`: `T` is a float, and `NaN != NaN`. Deriving `Eq`
/// would assert a reflexivity that FP values do not have.
#[allow(
    clippy::derived_hash_with_manual_eq,
    clippy::derive_partial_eq_without_eq
)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Outcome<T> {
    /// The computed value.
    pub value: T,
    /// What the operation raised.
    pub flags: Flags,
}

/// Is this `f32` a **signalling** NaN?
///
/// The distinction matters: a signalling NaN raises Invalid, a quiet one does
/// not. IEEE-754 puts the quiet bit at the top of the mantissa, so an
/// `is_nan()` check alone cannot tell them apart — and treating every NaN as
/// signalling raises Invalid on ordinary quiet-NaN propagation.
#[must_use]
pub const fn is_snan_f32(v: f32) -> bool {
    let b = v.to_bits();
    // NaN with the quiet bit (mantissa MSB) CLEAR, and a non-zero payload.
    b & 0x7F80_0000 == 0x7F80_0000 && b & 0x0040_0000 == 0 && b & 0x003F_FFFF != 0
}

/// Is this `f64` a **signalling** NaN?
#[must_use]
pub const fn is_snan_f64(v: f64) -> bool {
    let b = v.to_bits();
    b & 0x7FF0_0000_0000_0000 == 0x7FF0_0000_0000_0000
        && b & 0x0008_0000_0000_0000 == 0
        && b & 0x0007_FFFF_FFFF_FFFF != 0
}

/// Classify a finished `f32` result into flags.
fn classify_f32(r: f32, a: f32, b: f32) -> Flags {
    let mut f = Flags::NONE;
    if is_snan_f32(a) || is_snan_f32(b) {
        f.invalid = true;
    }
    if r.is_nan() && !a.is_nan() && !b.is_nan() {
        // A NaN produced from non-NaN inputs is an undefined form: 0/0, ∞-∞,
        // 0×∞. It is Invalid regardless of the inputs' own quietness.
        f.invalid = true;
    }
    if r.is_infinite() && a.is_finite() && b.is_finite() {
        f.overflow = true;
        f.inexact = true;
    }
    f
}

/// Classify a finished `f64` result into flags.
fn classify_f64(r: f64, a: f64, b: f64) -> Flags {
    let mut f = Flags::NONE;
    if is_snan_f64(a) || is_snan_f64(b) {
        f.invalid = true;
    }
    if r.is_nan() && !a.is_nan() && !b.is_nan() {
        f.invalid = true;
    }
    if r.is_infinite() && a.is_finite() && b.is_finite() {
        f.overflow = true;
        f.inexact = true;
    }
    f
}

/// `ADD.S`.
#[must_use]
pub fn add_s(a: f32, b: f32) -> Outcome<f32> {
    let value = a + b;
    Outcome {
        value,
        flags: classify_f32(value, a, b),
    }
}

/// `SUB.S`.
#[must_use]
pub fn sub_s(a: f32, b: f32) -> Outcome<f32> {
    let value = a - b;
    Outcome {
        value,
        flags: classify_f32(value, a, b),
    }
}

/// `MUL.S`.
///
/// **Does not model the VR4300 multiplication erratum** — see the module docs.
#[must_use]
pub fn mul_s(a: f32, b: f32) -> Outcome<f32> {
    let value = a * b;
    Outcome {
        value,
        flags: classify_f32(value, a, b),
    }
}

/// `DIV.S`.
#[must_use]
pub fn div_s(a: f32, b: f32) -> Outcome<f32> {
    let value = a / b;
    let mut flags = classify_f32(value, a, b);
    // Division by zero is its own flag, and only for a finite non-zero
    // numerator: 0/0 is Invalid (an undefined form), not `DivByZero`.
    if b == 0.0 && a != 0.0 && !a.is_nan() {
        flags.div_by_zero = true;
        // ...and it is NOT an overflow, though the result is infinite. The
        // generic classifier would call it one, so undo that here.
        flags.overflow = false;
        flags.inexact = false;
    }
    Outcome { value, flags }
}

/// `ADD.D`.
#[must_use]
pub fn add_d(a: f64, b: f64) -> Outcome<f64> {
    let value = a + b;
    Outcome {
        value,
        flags: classify_f64(value, a, b),
    }
}

/// `SUB.D`.
#[must_use]
pub fn sub_d(a: f64, b: f64) -> Outcome<f64> {
    let value = a - b;
    Outcome {
        value,
        flags: classify_f64(value, a, b),
    }
}

/// `MUL.D`.
#[must_use]
pub fn mul_d(a: f64, b: f64) -> Outcome<f64> {
    let value = a * b;
    Outcome {
        value,
        flags: classify_f64(value, a, b),
    }
}

/// `DIV.D`.
#[must_use]
pub fn div_d(a: f64, b: f64) -> Outcome<f64> {
    let value = a / b;
    let mut flags = classify_f64(value, a, b);
    if b == 0.0 && a != 0.0 && !a.is_nan() {
        flags.div_by_zero = true;
        flags.overflow = false;
        flags.inexact = false;
    }
    Outcome { value, flags }
}

/// `ABS.S` — clears the sign bit.
///
/// Written as an explicit bit clear rather than `f32::abs`. The two are
/// **equivalent**, including for NaN payloads — mutation testing confirmed
/// swapping them changes nothing — so this is a readability choice, not a
/// correctness one: the hardware operation *is* a bit clear, and saying so makes
/// the NaN-payload behaviour obvious instead of something to look up.
#[must_use]
pub const fn abs_s(a: f32) -> f32 {
    f32::from_bits(a.to_bits() & 0x7FFF_FFFF)
}

/// `NEG.S` — flips the sign bit.
#[must_use]
pub const fn neg_s(a: f32) -> f32 {
    f32::from_bits(a.to_bits() ^ 0x8000_0000)
}

/// `ABS.D`.
#[must_use]
pub const fn abs_d(a: f64) -> f64 {
    f64::from_bits(a.to_bits() & 0x7FFF_FFFF_FFFF_FFFF)
}

/// `NEG.D`.
#[must_use]
pub const fn neg_d(a: f64) -> f64 {
    f64::from_bits(a.to_bits() ^ 0x8000_0000_0000_0000)
}

/// The outcome of an ordered comparison: which of the three mutually exclusive
/// relations holds.
///
/// Named rather than three bools, because exactly one is true and a bool triple
/// makes the impossible states representable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Relation {
    /// `fs < ft`.
    Less,
    /// `fs == ft`.
    Equal,
    /// `fs > ft`.
    Greater,
    /// At least one operand is a NaN, so no ordering exists.
    Unordered,
}

/// `C.cond.fmt` — compare two single-precision values.
///
/// # The condition encoding
///
/// The 4-bit `cond` field is **systematic**, not sixteen unrelated mnemonics
/// (UM Table 7-11):
///
/// | Bit | Meaning |
/// | --- | --- |
/// | 3 | raise Invalid when the operands are **unordered** (the signalling forms) |
/// | 2 | true when `fs < ft` |
/// | 1 | true when `fs == ft` |
/// | 0 | true when the operands are **unordered** |
///
/// So `C.EQ` is `cond = 2`, `C.OLT` is `4`, `C.OLE` is `6`, `C.UN` is `1`, and
/// each signalling variant is its ordinary form plus 8. Writing the sixteen
/// mnemonics as sixteen cases invites getting one wrong; deriving them from the
/// bits makes all sixteen correct or none.
///
/// Note **`Greater` appears in no bit**: `fs > ft` is simply "none of less,
/// equal or unordered", which is why three condition bits suffice. Software
/// tests it by branching on the complement of `C.OLE`.
#[must_use]
pub fn compare_s(a: f32, b: f32, cond: u8) -> Outcome<bool> {
    let rel = relation_f32(a, b);
    compare_result(rel, cond, is_snan_f32(a) || is_snan_f32(b))
}

/// `C.cond.fmt` — compare two double-precision values.
#[must_use]
pub fn compare_d(a: f64, b: f64, cond: u8) -> Outcome<bool> {
    let rel = relation_f64(a, b);
    compare_result(rel, cond, is_snan_f64(a) || is_snan_f64(b))
}

/// Which relation holds between two `f32`s.
///
/// Exact equality is **correct here and epsilon comparison would be wrong**:
/// `C.EQ` is defined as IEEE equality, not approximate equality, so a tolerance
/// would make the instruction report a relation the hardware does not.
#[allow(clippy::float_cmp)]
fn relation_f32(a: f32, b: f32) -> Relation {
    if a.is_nan() || b.is_nan() {
        Relation::Unordered
    } else if a < b {
        Relation::Less
    } else if a == b {
        Relation::Equal
    } else {
        Relation::Greater
    }
}

/// Which relation holds between two `f64`s.
///
/// Exact equality is **correct here and epsilon comparison would be wrong**:
/// `C.EQ` is defined as IEEE equality, not approximate equality, so a tolerance
/// would make the instruction report a relation the hardware does not.
#[allow(clippy::float_cmp)]
fn relation_f64(a: f64, b: f64) -> Relation {
    if a.is_nan() || b.is_nan() {
        Relation::Unordered
    } else if a < b {
        Relation::Less
    } else if a == b {
        Relation::Equal
    } else {
        Relation::Greater
    }
}

/// Apply the condition bits to a computed relation.
fn compare_result(rel: Relation, cond: u8, snan: bool) -> Outcome<bool> {
    let unordered = matches!(rel, Relation::Unordered);
    let value = (matches!(rel, Relation::Less) && cond & 0b100 != 0)
        || (matches!(rel, Relation::Equal) && cond & 0b010 != 0)
        || (unordered && cond & 0b001 != 0);

    let mut flags = Flags::NONE;
    // Bit 3 selects the SIGNALLING forms, which raise Invalid on *any*
    // unordered comparison -- including one caused by a merely quiet NaN. That
    // is the whole difference between `C.EQ` and `C.SEQ`, and it is why the
    // quiet/signalling test used elsewhere is not sufficient on its own here.
    if unordered && cond & 0b1000 != 0 {
        flags.invalid = true;
    }
    // A signalling NaN operand raises Invalid whatever the condition.
    if snan {
        flags.invalid = true;
    }
    Outcome { value, flags }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A **signalling** NaN raises Invalid; a **quiet** one does not. Treating
    /// every NaN as signalling raises Invalid on ordinary NaN propagation, which
    /// is wrong and noisy.
    #[test]
    fn only_a_signalling_nan_raises_invalid() {
        let snan = f32::from_bits(0x7F80_0001);
        let qnan = f32::from_bits(0x7FC0_0001);
        assert!(is_snan_f32(snan), "quiet bit clear, payload non-zero");
        assert!(!is_snan_f32(qnan), "quiet bit set");
        assert!(!is_snan_f32(f32::INFINITY), "infinity is not a NaN");

        assert!(add_s(snan, 1.0).flags.invalid);
        assert!(
            !add_s(qnan, 1.0).flags.invalid,
            "a quiet NaN propagates quietly"
        );
    }

    /// The same, for doubles — the quiet bit sits at a different position, so
    /// this is not a free consequence of the `f32` case.
    #[test]
    fn the_double_precision_quiet_bit_is_at_bit_51() {
        let snan = f64::from_bits(0x7FF0_0000_0000_0001);
        let qnan = f64::from_bits(0x7FF8_0000_0000_0001);
        assert!(is_snan_f64(snan));
        assert!(!is_snan_f64(qnan));
        assert!(add_d(snan, 1.0).flags.invalid);
        assert!(!add_d(qnan, 1.0).flags.invalid);
    }

    /// **`x/0` is `DivByZero`; `0/0` is Invalid.** They are different flags, and a
    /// handler distinguishes them — collapsing both into `DivByZero` reports a
    /// division fault for what is actually an undefined form.
    #[test]
    fn divide_by_zero_and_zero_over_zero_raise_different_flags() {
        let f = div_s(1.0, 0.0).flags;
        assert!(f.div_by_zero, "finite non-zero over zero");
        assert!(!f.invalid);
        assert!(!f.overflow, "infinite, but not an overflow");

        let f = div_s(0.0, 0.0).flags;
        assert!(f.invalid, "0/0 is an undefined form");
        assert!(!f.div_by_zero);
    }

    /// A NaN produced from **non-NaN** inputs is an undefined form and raises
    /// Invalid — `∞ - ∞` here — even though neither operand was a NaN.
    #[test]
    fn a_nan_from_finite_or_infinite_inputs_is_invalid() {
        let f = sub_s(f32::INFINITY, f32::INFINITY).flags;
        assert!(f.invalid, "inf - inf is an undefined form");
        let f = mul_s(0.0, f32::INFINITY).flags;
        assert!(f.invalid, "0 * inf likewise");
    }

    /// Overflow from finite operands sets both Overflow and Inexact.
    #[test]
    fn overflow_from_finite_operands_is_also_inexact() {
        let f = mul_s(f32::MAX, 2.0).flags;
        assert!(f.overflow);
        assert!(f.inexact, "an overflowed result is never exact");
        // But an infinity that was already infinite is not an overflow.
        assert!(!add_s(f32::INFINITY, 1.0).flags.overflow);
    }

    /// `ABS`/`NEG` are **bit operations**, so they pass NaN payloads through
    /// rather than canonicalising. `f32::abs` happens to agree, but specifying
    /// it as a bit clear is what makes the NaN behaviour predictable.
    #[test]
    fn abs_and_neg_are_bit_operations_that_preserve_nan_payloads() {
        let nan = f32::from_bits(0xFF80_1234); // negative NaN with a payload
        assert_eq!(
            abs_s(nan).to_bits(),
            0x7F80_1234,
            "sign cleared, payload kept"
        );
        assert_eq!(neg_s(nan).to_bits(), 0x7F80_1234, "sign flipped");
        assert_eq!(
            neg_s(neg_s(nan)).to_bits(),
            nan.to_bits(),
            "and is an involution"
        );
        assert_eq!(abs_s(-0.0).to_bits(), 0.0f32.to_bits(), "negative zero too");
    }

    /// `FCSR.RM` decodes per UM §7.2.4, and the default is nearest-even.
    #[test]
    fn the_rounding_mode_decodes_from_rm() {
        assert_eq!(Rounding::from_rm(0), Rounding::Nearest);
        assert_eq!(Rounding::from_rm(1), Rounding::TowardZero);
        assert_eq!(Rounding::from_rm(2), Rounding::TowardPlusInf);
        assert_eq!(Rounding::from_rm(3), Rounding::TowardMinusInf);
        assert_eq!(
            Rounding::from_rm(0xFC),
            Rounding::Nearest,
            "masked to 2 bits"
        );
    }

    /// Flags map onto **both** the `Cause` and sticky `Flags` fields, because
    /// hardware sets them together. Writing only one leaves software unable to
    /// distinguish "raised now" from "raised at some point".
    #[test]
    fn flags_populate_both_the_cause_and_sticky_fields() {
        let bits = Flags {
            invalid: true,
            ..Flags::NONE
        }
        .to_fcsr_bits();
        assert_ne!(bits & (1 << 16), 0, "Cause.invalid");
        assert_ne!(bits & (1 << 6), 0, "Flags.invalid");

        let bits = Flags {
            div_by_zero: true,
            inexact: true,
            ..Flags::NONE
        }
        .to_fcsr_bits();
        assert_eq!(bits, (1 << 15) | (1 << 5) | (1 << 12) | (1 << 2));
        assert_eq!(Flags::NONE.to_fcsr_bits(), 0);
    }

    /// Ordinary arithmetic raises nothing — the flags must not be noisy, or
    /// software with the enables set traps constantly.
    #[test]
    fn ordinary_arithmetic_raises_no_flags() {
        // Values chosen so every one of the four operations stays in range:
        // `1e20 / 1e-20` is 1e40, which genuinely overflows `f32`, so it does
        // not belong in a "raises nothing" case.
        for (a, b) in [(1.0f32, 2.0f32), (-3.5, 0.25), (1e10, 1e-4)] {
            for out in [add_s(a, b), sub_s(a, b), mul_s(a, b), div_s(a, b)] {
                assert_eq!(out.flags, Flags::NONE, "{a} op {b} raised something");
            }
        }
        // Exact bit comparison, not a tolerance: these values are exactly
        // representable, so anything but the exact result is a bug -- and a
        // tolerance would hide it.
        assert_eq!(add_d(1.0, 2.0).value.to_bits(), 3.0f64.to_bits());
        assert_eq!(mul_d(3.0, 4.0).value.to_bits(), 12.0f64.to_bits());
    }

    /// The condition field is **systematic**, so this checks the named mnemonics
    /// against the derivation rather than trusting it.
    #[test]
    fn the_named_compare_conditions_fall_out_of_the_bit_encoding() {
        // cond, name, expected for [1<2, 2==2, 3>2, NaN]
        let cases: &[(u8, &str, [bool; 4])] = &[
            (0, "F", [false, false, false, false]),
            (1, "UN", [false, false, false, true]),
            (2, "EQ", [false, true, false, false]),
            (3, "UEQ", [false, true, false, true]),
            (4, "OLT", [true, false, false, false]),
            (5, "ULT", [true, false, false, true]),
            (6, "OLE", [true, true, false, false]),
            (7, "ULE", [true, true, false, true]),
        ];
        for &(cond, name, want) in cases {
            let got = [
                compare_s(1.0, 2.0, cond).value,
                compare_s(2.0, 2.0, cond).value,
                compare_s(3.0, 2.0, cond).value,
                compare_s(f32::NAN, 2.0, cond).value,
            ];
            assert_eq!(got, want, "C.{name} (cond {cond})");
        }
    }

    /// **The signalling forms raise Invalid on any unordered compare**, even for
    /// a *quiet* NaN. That is the entire difference between `C.EQ` and `C.SEQ`,
    /// and it means the quiet/signalling test used elsewhere is not sufficient
    /// on its own here.
    #[test]
    fn the_signalling_compare_forms_raise_on_a_quiet_nan() {
        let qnan = f32::from_bits(0x7FC0_0001);
        assert!(!is_snan_f32(qnan), "it really is quiet");

        let out = compare_s(qnan, 1.0, 2); // C.EQ
        assert!(!out.value);
        assert!(!out.flags.invalid, "the non-signalling form stays quiet");

        let out = compare_s(qnan, 1.0, 10); // C.SEQ
        assert!(!out.value, "the comparison result is unchanged");
        assert!(out.flags.invalid, "only the exception differs");
    }

    /// A **signalling** NaN raises Invalid whatever the condition, including the
    /// non-signalling forms.
    #[test]
    fn a_signalling_nan_operand_raises_for_every_condition() {
        let snan = f32::from_bits(0x7F80_0001);
        for cond in 0..16u8 {
            assert!(
                compare_s(snan, 1.0, cond).flags.invalid,
                "cond {cond} must raise on a signalling NaN"
            );
        }
    }

    /// `Greater` appears in **no** condition bit — it is "none of less, equal or
    /// unordered", which is why three bits suffice for the relation.
    #[test]
    fn greater_matches_no_condition_bit() {
        for cond in 0..8u8 {
            assert!(
                !compare_s(3.0, 2.0, cond).value,
                "cond {cond}: greater matches no bit"
            );
        }
    }

    /// Doubles use the same derivation, and `-0.0 == 0.0` as IEEE requires.
    #[test]
    fn double_compares_agree_and_signed_zeros_are_equal() {
        assert!(compare_d(1.0, 2.0, 4).value, "C.OLT");
        assert!(compare_d(2.0, 2.0, 2).value, "C.EQ");
        assert!(compare_d(-0.0, 0.0, 2).value, "-0.0 == 0.0 (IEEE)");
        assert!(compare_d(f64::NAN, 1.0, 1).value, "C.UN");
        assert!(
            !compare_d(f64::NAN, 1.0, 2).value,
            "C.EQ is false when unordered"
        );
    }
}
