//! FPU arithmetic (T-13-002).
//!
//! Pure functions over IEEE-754 values, with the VR4300's `FCSR` semantics
//! layered on top. Kept separate from [`crate::fpr`] (the register file) and
//! [`crate::cop1`] (the control registers) so each can be tested without the
//! others.
//!
//! # Rounding — and where it does *not* yet apply
//!
//! `FCSR.RM` selects the mode (UM §7.2.4): 0 nearest-even, 1 toward zero,
//! 2 toward +∞, 3 toward −∞.
//!
//! **The conversions honour it; the arithmetic does not.** [`round_f64`] and the
//! integer conversions take a [`Rounding`] explicitly, but `add_*`, `sub_*`,
//! `mul_*` and `div_*` use Rust's `f32`/`f64` operators, which are nearest-even
//! only. So a program that sets `FCSR.RM` to toward-zero and then adds will get
//! a nearest-even result here and a toward-zero one on hardware.
//!
//! That gap is **stated rather than implied**, because an earlier version of
//! this comment claimed the modes were applied throughout while the arithmetic
//! ignored them — see `docs/engineering-lessons.md` §3.3c. Closing it needs
//! either soft-float arithmetic or per-operation re-rounding of an
//! exactly-computed result; it is accuracy-ledger **U-8**.
//!
//! # What is deliberately absent
//!
//! The **FP multiplication erratum** is not here. It is a property of specific
//! early console revisions (`n64brew_wiki/markdown/VR4300.md`) and belongs with
//! the revision model, not with the arithmetic; implementing it inline would
//! make every multiply on every console wrong.

// Two lints are allowed for this module, both because the thing they warn about
// is the thing being modelled:
//
//   * `cast_precision_loss` -- an FPU's conversion instructions exist precisely
//     to lose precision in a defined way. The loss is the behaviour, and it is
//     reported through the Inexact flag rather than avoided.
//   * `float_cmp` -- `C.EQ` is IEEE equality and the exactness checks are exact
//     by definition. An epsilon here would make the emulator report relations
//     and flags the hardware does not.
#![allow(clippy::cast_precision_loss, clippy::float_cmp)]

/// Which VR4300 stepping a console carries.
///
/// The only behaviour that currently depends on it is the **FP multiplication
/// erratum**, and that dependency is why this is modelled as console state
/// rather than folded into `mul`: an unconditional erratum would make every
/// multiply on *every* console wrong, and most consoles do not have it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Stepping {
    /// A later stepping, with the multiplication erratum **fixed**.
    ///
    /// The default, because it is the majority of hardware and because the
    /// erratum's output is undocumented — see [`Stepping::has_mul_erratum`].
    #[default]
    Fixed,
    /// An early stepping carrying the erratum: NUS-01 and NUS-02 (Japan only)
    /// and NUS-03 (the first US revision).
    Early,
}

impl Stepping {
    /// Does this stepping carry the FP multiplication erratum?
    ///
    /// # Why the erratum is not implemented
    ///
    /// The trigger is documented (`n64brew_wiki/markdown/VR4300.md`): a
    /// multiply whose *preceding* multiply had a NaN, zero or infinity operand
    /// *"may produce unexpected results"*. GCC's `-mfix4300` works around it by
    /// inserting two `nop`s after every `MUL.S`/`MUL.D`/`MULT`.
    ///
    /// **What the corrupted output actually is has never been characterised** —
    /// our own `ref-docs/2026-07-20-vr4300-timing-supplement.md` lists it under
    /// undocumented constants, with only trigger conditions known. So the
    /// erratum can be *detected* here but not *reproduced*, and inventing a
    /// plausible wrong value would be exactly the fitted-constant failure
    /// `docs/accuracy-ledger.md` forbids: every later result built on it would
    /// stop being evidence.
    ///
    /// Accuracy-ledger **U-7**. Selecting [`Stepping::Early`] therefore changes
    /// nothing yet — it exists so that when the output *is* characterised, the
    /// switch is already in the right place rather than threaded through
    /// afterwards.
    #[must_use]
    pub const fn has_mul_erratum(self) -> bool {
        matches!(self, Self::Early)
    }
}

/// Would the erratum's trigger condition fire for this multiply?
///
/// True when an operand of the **previous** multiply was a NaN, zero or
/// infinity. Exposed so a future characterisation has a tested trigger to hang
/// the corrupted output on, and so a trace can flag affected instructions today.
#[must_use]
pub fn mul_erratum_triggers(prev_a: f64, prev_b: f64) -> bool {
    let suspicious = |v: f64| v.is_nan() || v == 0.0 || v.is_infinite();
    suspicious(prev_a) || suspicious(prev_b)
}

/// The `FCSR` cause/flag bits an operation can raise (UM §7.2.2).
///
/// # Completeness
///
/// **Not all five are fully modelled yet**, and a caller must not assume they
/// are:
///
/// | Flag | State |
/// | --- | --- |
/// | `invalid` | complete — signalling NaNs and undefined forms |
/// | `div_by_zero` | complete |
/// | `overflow` | complete for arithmetic and narrowing conversions |
/// | `inexact` | **partial** — set for overflow and conversions, not for ordinary rounding |
/// | `underflow` | **partial** — set for conversions, not for gradual arithmetic underflow |
///
/// Detecting ordinary inexactness and gradual underflow needs the exact result
/// before rounding, which the hardware float operators do not expose. Recorded
/// as accuracy-ledger **U-8** alongside the rounding-mode gap rather than left
/// for a caller to discover by trusting a bit that never sets.
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

impl<T> Outcome<T> {
    /// Did the operation underflow?
    #[must_use]
    pub const fn underflowed(&self) -> bool {
        self.flags.underflow
    }
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
    // `a.is_finite()` is load-bearing and was missing: `inf / 0` is NOT a
    // division-by-zero. IEEE reserves the flag for a **finite** non-zero
    // numerator, because only there does a zero divisor create an infinity out
    // of nothing -- `inf / 0` was already infinite. Without the check the
    // condition disagreed with the comment directly above it.
    if b == 0.0 && a != 0.0 && a.is_finite() {
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
    // See `div_s`: a finite non-zero numerator, not merely a non-NaN one.
    if b == 0.0 && a != 0.0 && a.is_finite() {
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

/// The `Unimplemented Operation` cause bit (`FCSR` bit 17).
///
/// Distinct from Invalid: it means *"this processor cannot do this in
/// hardware — trap to software"*, not *"the operation is mathematically
/// undefined"*. The VR4300 uses it for the long-integer conversion restriction
/// below, which is a **hardware limitation**, not a numerical error.
pub const CAUSE_UNIMPLEMENTED: u32 = 1 << 17;

/// Does a 64-bit integer satisfy `CVT.[S,D].L`'s range restriction?
///
/// > *"When converting a long integer to a single- or double-precision
/// > floating-point number (`CVT.[S,D].L`), bits 63:55 of the 64-bit integer
/// > must be all zeroes or ones, otherwise the VR4300 processor raises a
/// > floating-point instruction exception."* — UM §7.5.2
///
/// This is a **VR4300-specific hardware limitation**, not IEEE behaviour: the
/// value is perfectly representable, the processor simply declines. An emulator
/// that converts it anyway produces a *correct* number where hardware traps, so
/// software's fixup path never runs and the difference surfaces far downstream.
#[must_use]
pub const fn long_convertible(v: i64) -> bool {
    let top = (v >> 55) & 0x1FF;
    top == 0 || top == 0x1FF
}

/// `CVT.S.W` / `CVT.D.W` — 32-bit integer to float. Always exact for `f64`.
#[must_use]
pub fn cvt_s_w(v: i32) -> Outcome<f32> {
    let value = v as f32;
    let mut flags = Flags::NONE;
    // 24 bits of mantissa cannot hold every i32, so this one CAN be inexact --
    // unlike the f64 case, which is exact for every i32.
    //
    // Compared through `f64`, NOT by casting back to `i32`: Rust's float-to-int
    // cast **saturates**, so `i32::MAX as f32 as i32` is `i32::MAX` again and
    // the round-trip check silently never fires for exactly the value most
    // likely to be inexact.
    if f64::from(value) != f64::from(v) {
        flags.inexact = true;
    }
    Outcome { value, flags }
}

/// `CVT.D.W` — 32-bit integer to double. Exact for every input.
#[must_use]
pub fn cvt_d_w(v: i32) -> Outcome<f64> {
    Outcome {
        value: f64::from(v),
        flags: Flags::NONE,
    }
}

/// `CVT.S.L` — 64-bit integer to single, honouring the VR4300 restriction.
///
/// # Errors
///
/// [`CAUSE_UNIMPLEMENTED`] when bits 63:55 are neither all-zero nor all-one.
/// There is no defined result to return in that case, which is why this is a
/// `Result` rather than a value plus a flag.
pub fn cvt_s_l(v: i64) -> Result<Outcome<f32>, u32> {
    if !long_convertible(v) {
        return Err(CAUSE_UNIMPLEMENTED);
    }
    let value = v as f32;
    let mut flags = Flags::NONE;
    // Via `f64` for the same saturation reason as `cvt_s_w`. `f32`→`f64` is
    // exact, and the restriction bounds `|v| < 2^55`, so the `f64`→`i64` step is
    // exact too for any value an `f32` can hold.
    if (f64::from(value)) as i64 != v {
        flags.inexact = true;
    }
    Ok(Outcome { value, flags })
}

/// `CVT.D.L` — 64-bit integer to double, honouring the VR4300 restriction.
///
/// # Errors
///
/// [`CAUSE_UNIMPLEMENTED`] when bits 63:55 are neither all-zero nor all-one.
pub fn cvt_d_l(v: i64) -> Result<Outcome<f64>, u32> {
    if !long_convertible(v) {
        return Err(CAUSE_UNIMPLEMENTED);
    }
    let value = v as f64;
    let mut flags = Flags::NONE;
    if value as i64 != v {
        flags.inexact = true;
    }
    Ok(Outcome { value, flags })
}

/// `CVT.D.S` — single to double. Always exact: every `f32` is an `f64`.
#[must_use]
pub fn cvt_d_s(v: f32) -> Outcome<f64> {
    Outcome {
        value: f64::from(v),
        flags: if is_snan_f32(v) {
            Flags::INVALID
        } else {
            Flags::NONE
        },
    }
}

/// `CVT.S.D` — double to single. Can overflow, underflow or lose precision.
#[must_use]
pub fn cvt_s_d(v: f64) -> Outcome<f32> {
    let value = v as f32;
    let mut flags = Flags::NONE;
    if is_snan_f64(v) {
        flags.invalid = true;
    }
    if v.is_finite() {
        if value.is_infinite() {
            flags.overflow = true;
            flags.inexact = true;
        } else if value == 0.0 && v != 0.0 {
            // A non-zero double that narrows to zero has underflowed -- the
            // one underflow case this module can detect without the exact
            // pre-rounding result.
            flags.underflow = true;
            flags.inexact = true;
        } else if f64::from(value) != v {
            flags.inexact = true;
        }
    }
    Outcome { value, flags }
}

/// `2^63`, the bound an `i64` conversion must stay strictly below.
const TWO_POW_63: f64 = 9_223_372_036_854_775_808.0;

/// `2^52` — above this magnitude every `f64` is already an integer, so the
/// rounding helpers can return the value untouched.
const F64_INTEGRAL_THRESHOLD: f64 = 4_503_599_627_370_496.0;

/// `|v|`, by clearing the sign bit.
///
/// `f64::abs` and friends live in `std`; this crate is `#![no_std]`, so the
/// handful of float operations the FPU needs are implemented here rather than
/// pulling in `libm` for four functions.
const fn fabs(v: f64) -> f64 {
    f64::from_bits(v.to_bits() & 0x7FFF_FFFF_FFFF_FFFF)
}

/// Truncate toward zero.
fn trunc(v: f64) -> f64 {
    if !v.is_finite() || fabs(v) >= F64_INTEGRAL_THRESHOLD {
        // Already integral (or not a number), so there is nothing to remove.
        return v;
    }
    (v as i64) as f64
}

/// Round toward −∞.
fn floor(v: f64) -> f64 {
    let t = trunc(v);
    if v < 0.0 && t != v { t - 1.0 } else { t }
}

/// Round toward +∞.
fn ceil(v: f64) -> f64 {
    let t = trunc(v);
    if v > 0.0 && t != v { t + 1.0 } else { t }
}

/// Round to nearest, **ties to even**.
///
/// Not "round half away from zero", which is what most `round` functions do and
/// which no MIPS rounding mode selects.
fn round_ties_even(v: f64) -> f64 {
    if !v.is_finite() || fabs(v) >= F64_INTEGRAL_THRESHOLD {
        return v;
    }
    let f = floor(v);
    let diff = v - f;
    if diff > 0.5 {
        f + 1.0
    } else if diff < 0.5 {
        f
    } else if (f as i64) % 2 == 0 {
        // Exactly halfway: pick the even neighbour.
        f
    } else {
        f + 1.0
    }
}

/// Round a float to an integer under an explicit [`Rounding`] mode.
///
/// Split out because `CVT.W`, `ROUND.W`, `TRUNC.W`, `CEIL.W` and `FLOOR.W`
/// differ **only** in this: `CVT` uses `FCSR.RM`, and the other four hard-code
/// one mode each. Sharing the body means a rounding bug cannot exist in one and
/// not the others.
#[must_use]
pub fn round_f64(v: f64, mode: Rounding) -> f64 {
    match mode {
        // `round_ties_even` rather than `round`, which rounds half AWAY from
        // zero -- a different mode that no MIPS setting selects.
        Rounding::Nearest => round_ties_even(v),
        Rounding::TowardZero => trunc(v),
        Rounding::TowardPlusInf => ceil(v),
        Rounding::TowardMinusInf => floor(v),
    }
}

/// Convert a float to a 32-bit integer under `mode`.
///
/// An out-of-range or NaN input raises **Invalid**. The value returned in that
/// case is `i32::MAX`, which is the conventional MIPS result — and a *choice*
/// here, since the architecture leaves it undefined when the exception is
/// masked.
#[must_use]
pub fn to_i32(v: f64, mode: Rounding) -> Outcome<i32> {
    let r = round_f64(v, mode);
    if v.is_nan() || r < f64::from(i32::MIN) || r > f64::from(i32::MAX) {
        return Outcome {
            value: i32::MAX,
            flags: Flags::INVALID,
        };
    }
    let value = r as i32;
    let mut flags = Flags::NONE;
    if f64::from(value) != v {
        flags.inexact = true;
    }
    Outcome { value, flags }
}

/// Convert a float to a 64-bit integer under `mode`.
#[must_use]
pub fn to_i64(v: f64, mode: Rounding) -> Outcome<i64> {
    let r = round_f64(v, mode);
    // The bounds are compared as f64 deliberately: `i64::MAX` is not exactly
    // representable, so `r > i64::MAX as f64` is the correct test and
    // `r as i64 == i64::MAX` is not.
    if v.is_nan() || !(-TWO_POW_63..TWO_POW_63).contains(&r) {
        return Outcome {
            value: i64::MAX,
            flags: Flags::INVALID,
        };
    }
    let value = r as i64;
    let mut flags = Flags::NONE;
    if value as f64 != v {
        flags.inexact = true;
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

    /// **`inf / 0` is not a division by zero.** IEEE reserves the flag for a
    /// *finite* non-zero numerator, because only there does a zero divisor
    /// create an infinity out of nothing — `inf / 0` was already infinite.
    ///
    /// The condition originally tested `!a.is_nan()`, which let infinities
    /// through and disagreed with the comment directly above it.
    #[test]
    fn an_infinite_numerator_over_zero_is_not_a_division_by_zero() {
        for a in [f32::INFINITY, f32::NEG_INFINITY] {
            let f = div_s(a, 0.0).flags;
            assert!(!f.div_by_zero, "{a} / 0 is not DivByZero");
            assert!(!f.invalid, "nor is it an undefined form");
        }
        // The finite case still is.
        assert!(div_s(1.0, 0.0).flags.div_by_zero);
        assert!(div_d(-2.5, 0.0).flags.div_by_zero);
        for a in [f64::INFINITY, f64::NEG_INFINITY] {
            assert!(!div_d(a, 0.0).flags.div_by_zero);
        }
    }

    /// A double that narrows to zero has **underflowed** — the one underflow
    /// case detectable without the exact pre-rounding result.
    #[test]
    fn a_double_narrowing_to_zero_underflows() {
        let out = cvt_s_d(1e-300);
        assert!(out.underflowed(), "1e-300 has no f32 representation");
        assert!(out.flags.inexact);
        assert_eq!(out.value, 0.0);
        // A representable small value does not.
        assert!(!cvt_s_d(1e-30).underflowed());
        // ...and a genuine zero is not an underflow.
        assert!(!cvt_s_d(0.0).underflowed());
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

    /// **The VR4300 long-conversion restriction** (UM §7.5.2): bits 63:55 must
    /// be all-zero or all-one. This is a *hardware limitation*, not IEEE
    /// behaviour — the value is representable, the processor declines.
    ///
    /// Converting it anyway produces a correct number where hardware traps, so
    /// software's fixup path never runs and the divergence surfaces far
    /// downstream from its cause.
    #[test]
    fn cvt_from_long_rejects_values_outside_the_vr4300_range() {
        // Small positives and negatives: bits 63:55 uniform.
        for v in [0i64, 1, -1, 1 << 40, -(1 << 40), (1 << 55) - 1] {
            assert!(long_convertible(v), "{v} must be convertible");
            assert!(cvt_d_l(v).is_ok());
        }
        // Bits 63:55 mixed -- declines.
        for v in [1i64 << 55, 1 << 60, i64::MAX, i64::MIN + 1] {
            assert!(!long_convertible(v), "{v} must be rejected");
            assert_eq!(cvt_d_l(v), Err(CAUSE_UNIMPLEMENTED));
            assert_eq!(cvt_s_l(v), Err(CAUSE_UNIMPLEMENTED));
        }
        // i64::MIN is 0x8000_0000_0000_0000, so bits 63:55 are 0b1_0000_0000 --
        // neither all-zero nor all-one, so it is NOT convertible. Easy to
        // assume otherwise from "the sign bit is set".
        assert!(!long_convertible(i64::MIN));
        // The largest magnitudes that ARE convertible sit at the 2^55 boundary.
        assert!(long_convertible((1i64 << 55) - 1));
        assert!(long_convertible(-(1i64 << 55)));
    }

    /// `Unimplemented` is **not** `Invalid`: it means "this processor cannot do
    /// this", not "the operation is undefined". Conflating them sends the
    /// handler down the numerical-error path for a hardware limitation.
    #[test]
    fn unimplemented_is_a_different_cause_bit_from_invalid() {
        assert_eq!(CAUSE_UNIMPLEMENTED, 1 << 17);
        assert_ne!(
            CAUSE_UNIMPLEMENTED,
            Flags::INVALID.to_fcsr_bits() & 0x0003_F000,
            "distinct from the Invalid cause bit"
        );
    }

    /// `CVT.D.W` is exact for every `i32`; `CVT.S.W` is not, because 24 mantissa
    /// bits cannot hold every 32-bit integer.
    #[test]
    fn int_to_double_is_exact_but_int_to_single_can_be_inexact() {
        for v in [0i32, 1, -1, i32::MAX, i32::MIN] {
            assert_eq!(cvt_d_w(v).flags, Flags::NONE, "{v} to double is exact");
        }
        assert_eq!(cvt_s_w(1).flags, Flags::NONE);
        assert!(
            cvt_s_w(i32::MAX).flags.inexact,
            "0x7FFFFFFF does not fit in 24 mantissa bits"
        );
    }

    /// `CVT.S.D` can overflow; `CVT.D.S` never can, since every `f32` is an
    /// `f64`.
    #[test]
    fn narrowing_can_overflow_but_widening_cannot() {
        let out = cvt_s_d(1e300);
        assert!(out.flags.overflow, "1e300 has no f32 representation");
        assert!(out.flags.inexact);
        assert!(!cvt_s_d(1.0).flags.overflow);
        assert_eq!(cvt_d_s(1.5).value.to_bits(), 1.5f64.to_bits());
        assert_eq!(cvt_d_s(f32::MAX).flags, Flags::NONE, "widening is exact");
    }

    /// The four rounding modes differ, and `Nearest` is **ties-to-even** — not
    /// `f64::round`, which rounds half away from zero and matches no MIPS mode.
    #[test]
    fn the_rounding_modes_differ_and_nearest_is_ties_to_even() {
        assert_eq!(round_f64(2.5, Rounding::Nearest), 2.0, "ties to EVEN");
        assert_eq!(round_f64(3.5, Rounding::Nearest), 4.0);
        assert_eq!(round_f64(2.5, Rounding::TowardZero), 2.0);
        assert_eq!(round_f64(2.5, Rounding::TowardPlusInf), 3.0);
        assert_eq!(round_f64(2.5, Rounding::TowardMinusInf), 2.0);
        assert_eq!(round_f64(-2.5, Rounding::TowardZero), -2.0);
        assert_eq!(round_f64(-2.5, Rounding::TowardMinusInf), -3.0);
    }

    /// Out-of-range and NaN conversions raise Invalid rather than wrapping.
    #[test]
    fn an_out_of_range_conversion_raises_invalid() {
        for v in [1e30f64, -1e30, f64::NAN, f64::INFINITY] {
            let out = to_i32(v, Rounding::Nearest);
            assert!(out.flags.invalid, "{v} does not fit in an i32");
            assert_eq!(out.value, i32::MAX, "the conventional MIPS result");
        }
        assert_eq!(to_i32(42.0, Rounding::Nearest).value, 42);
        assert_eq!(to_i32(42.0, Rounding::Nearest).flags, Flags::NONE);
        assert!(to_i32(42.5, Rounding::TowardZero).flags.inexact);
    }

    /// The 64-bit bound is compared as an `f64` because `i64::MAX` is **not
    /// exactly representable** — `r > i64::MAX as f64` is the correct test, and
    /// casting first is not.
    #[test]
    fn the_64_bit_conversion_bound_accounts_for_representability() {
        // 2^63 exactly: out of range, since i64::MAX is 2^63 - 1.
        let out = to_i64(9_223_372_036_854_775_808.0, Rounding::Nearest);
        assert!(out.flags.invalid, "2^63 does not fit in an i64");
        // Just inside.
        let out = to_i64(9_223_372_036_854_774_784.0, Rounding::Nearest);
        assert!(!out.flags.invalid);
        assert_eq!(to_i64(-1.0, Rounding::Nearest).value, -1);
    }

    /// The FP multiplication erratum is **detectable but not reproducible**:
    /// the trigger is documented, the corrupted output never was.
    ///
    /// Selecting the affected stepping therefore changes no arithmetic. That is
    /// deliberate — inventing a plausible wrong value would be the
    /// fitted-constant failure the accuracy ledger forbids, and every later
    /// result built on it would stop being evidence.
    #[test]
    fn the_multiplication_erratum_is_modelled_but_not_invented() {
        assert!(!Stepping::default().has_mul_erratum(), "fixed by default");
        assert!(Stepping::Early.has_mul_erratum());

        // Selecting the affected stepping changes nothing about a multiply,
        // because there is nothing documented to change it to.
        let a = mul_s(3.0, 4.0);
        assert_eq!(a.value, 12.0, "arithmetic is stepping-independent today");
        assert_eq!(a.flags, Flags::NONE);
    }

    /// The trigger is a property of the **previous** multiply's operands: a NaN,
    /// zero or infinity. It is tested now so a future characterisation has
    /// somewhere correct to attach the output.
    #[test]
    fn the_erratum_trigger_keys_off_the_previous_multiplys_operands() {
        for (a, b) in [
            (f64::NAN, 1.0),
            (0.0, 1.0),
            (1.0, -0.0),
            (f64::INFINITY, 1.0),
            (1.0, f64::NEG_INFINITY),
        ] {
            assert!(mul_erratum_triggers(a, b), "{a} * {b} arms the erratum");
        }
        assert!(!mul_erratum_triggers(2.0, 3.0), "ordinary operands do not");
        assert!(!mul_erratum_triggers(-1.5, 1e30));
    }
}
