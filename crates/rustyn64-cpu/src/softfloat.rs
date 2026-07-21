//! Soft-float arithmetic with exact IEEE-754 exception flags (T-13-003).
//!
//! # Why this exists
//!
//! Rust's `f32`/`f64` operators give a correctly-rounded result and **discard
//! everything else**: there is no way to ask whether the operation was inexact,
//! underflowed, or what it would have produced under a directed rounding mode.
//! The VR4300 reports all of that through `FCSR`, so an emulator built on the
//! native operators can be bit-exact on values and still wrong on every flag —
//! which is precisely where accuracy ledger **C-11** left the FPU.
//!
//! # Why the cheap version does not work
//!
//! The tempting shortcut is to compute in `f64` and compare: if the `f64`
//! result differs from the widened `f32` result, call it inexact. That is
//! **right in the normal range and wrong where it matters**:
//!
//! - For `MUL.S` it happens to hold — the exact product of two 24-bit
//!   significands needs at most 48 bits, and `f64` carries 53.
//! - For `ADD.S` it does not. The exact sum of `2^127` and `2^-149` spans ~277
//!   significand bits, so the `f64` sum is *itself* rounded and the comparison
//!   silently becomes a guess.
//! - For any `.D` operation there is no wider type to compute in at all.
//!
//! An earlier attempt along those lines was implemented and reverted (C-10). A
//! flag that is right in the common case and wrong in the range the oracle
//! deliberately probes is worse than no flag, because it makes every later
//! result stop being evidence.
//!
//! # How this works instead
//!
//! One code path for both formats, parameterised by [`Format`]. Values are
//! unpacked to `(sign, significand, exponent)` with the significand as a plain
//! integer — value = `sig × 2^exp` — computed at a widened scale in `u128`, and
//! rounded **once** at the end by a single internal rounding step, which is the
//! only place any flag is
//! produced. Bits that fall off the bottom are never simply dropped: they are
//! folded into a sticky bit, which is what makes `inexact` exact rather than
//! approximate.
//!
//! There is no `unsafe`, no allocation and no `std`; the widest type used is
//! `u128`, which `core` provides everywhere this crate builds.
//!
//! # What is deliberately NOT modelled here
//!
//! The VR4300 does not produce subnormal results: it raises the unmaskable
//! **unimplemented-operation** cause for subnormal operands and results (unless
//! `FCSR.FS` is set, which flushes instead). This module implements the *IEEE*
//! behaviour and produces the subnormal, because that separation is what lets
//! it be checked against an independent oracle — every `f32`/`f64` operation in
//! Rust. Layering the VR4300's refusal on top is a separate change; doing both
//! at once would leave the arithmetic with nothing to be tested against.

// Four lints are allowed module-wide, each because the thing it warns about is
// the thing being modelled or tested:
//
//   * `float_cmp` -- this module exists to be bit-exact. Every comparison here
//     is against an exactly-representable value or a bit pattern, and an
//     epsilon would defeat the purpose of the differential test.
//   * `unreadable_literal` -- the rounding vectors are transcribed verbatim
//     from n64-systemtest. Reformatting them breaks the correspondence with the
//     oracle they were copied from, which is what makes them checkable.
//   * `cast_precision_loss` -- the test corpora deliberately build floats from
//     integers; the loss is how the sample is drawn.
//   * `many_single_char_names` -- `f` is the format and `a`/`b` the operands
//     throughout, matching the IEEE-754 text this implements.
#![allow(
    clippy::float_cmp,
    clippy::unreadable_literal,
    clippy::cast_precision_loss,
    clippy::many_single_char_names
)]

use crate::fpu::{Flags, Rounding};

/// The parameters of an IEEE-754 binary interchange format.
///
/// Held as data rather than as a type parameter so that one implementation
/// serves both precisions. A second copy of this logic specialised per format
/// is exactly how the two diverge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Format {
    /// Significand bits **including** the implicit leading one (24 / 53).
    pub p: u32,
    /// Total width in bits (32 / 64).
    pub width: u32,
    /// Exponent bias (127 / 1023).
    pub bias: i32,
}

/// Single precision.
pub const F32: Format = Format {
    p: 24,
    width: 32,
    bias: 127,
};

/// Double precision.
pub const F64: Format = Format {
    p: 53,
    width: 64,
    bias: 1023,
};

impl Format {
    /// Stored mantissa bits — one fewer than [`Format::p`], the implicit bit
    /// not being stored.
    #[must_use]
    pub const fn man_bits(self) -> u32 {
        self.p - 1
    }

    /// The all-ones exponent field, which encodes infinity and NaN.
    #[must_use]
    pub const fn max_biased(self) -> u32 {
        (1u32 << (self.width - self.p)) - 1
    }

    /// The exponent of the **least significant bit** of the smallest subnormal
    /// — `-149` for `f32`, `-1074` for `f64`.
    ///
    /// This is the floor the rounding step clamps to, and it is what makes a
    /// result subnormal rather than merely small.
    #[must_use]
    pub const fn min_lsb_exp(self) -> i32 {
        1 - self.bias - self.man_bits() as i32
    }

    /// The largest finite value's encoding, magnitude only.
    #[must_use]
    pub const fn max_finite(self) -> u64 {
        ((self.max_biased() as u64 - 1) << self.man_bits()) | ((1u64 << self.man_bits()) - 1)
    }

    /// Positive infinity, magnitude only.
    #[must_use]
    pub const fn infinity(self) -> u64 {
        (self.max_biased() as u64) << self.man_bits()
    }

    /// The NaN the VR4300 delivers as the result of an invalid operation.
    ///
    /// `0x7FBF_FFFF` / `0x7FF7_FFFF_FFFF_FFFF` — the significand's MSB is
    /// **clear**, which by IEEE-754:2008 would make the result of every invalid
    /// operation a *signalling* NaN, absurdly re-trapping on first use.
    ///
    /// It is not absurd, because the VR4300 uses the **legacy MIPS
    /// convention**, where MSB set means signalling. Under its own rules this
    /// is an ordinary quiet NaN. See `fpu::is_snan_f32` and accuracy ledger
    /// C-12; this value is the corroboration that the convention really is
    /// inverted rather than the tests being odd.
    #[must_use]
    pub const fn default_nan(self) -> u64 {
        let man = (1u64 << self.man_bits()) - 1;
        let quiet = 1u64 << (self.man_bits() - 1);
        self.infinity() | (man & !quiet)
    }
}

/// What an unpacked value is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Class {
    /// ±0.
    Zero,
    /// A non-zero finite value, normal or subnormal.
    Finite,
    /// ±∞.
    Inf,
    /// Not a number.
    Nan,
}

/// A decoded float: for [`Class::Finite`], the value is `sig × 2^exp`.
///
/// The significand is a plain integer with **no** normalisation requirement,
/// which is what lets subnormals and normals take the same code path.
#[derive(Clone, Copy, Debug)]
struct Unpacked {
    sign: bool,
    class: Class,
    /// Significand for a finite value; the raw trailing payload for a NaN.
    sig: u128,
    exp: i32,
    /// A NaN with the quiet bit clear.
    snan: bool,
}

/// Decode an encoding into [`Unpacked`].
fn unpack(bits: u64, f: Format) -> Unpacked {
    let man_bits = f.man_bits();
    let sign = (bits >> (f.width - 1)) & 1 != 0;
    let man = u128::from(bits & ((1u64 << man_bits) - 1));
    let biased = ((bits >> man_bits) & u64::from(f.max_biased())) as u32;

    if biased == 0 {
        return Unpacked {
            sign,
            class: if man == 0 { Class::Zero } else { Class::Finite },
            sig: man,
            // A subnormal's exponent is the same as the smallest normal's, and
            // the leading bit is absent rather than implicit.
            exp: f.min_lsb_exp(),
            snan: false,
        };
    }
    if biased == f.max_biased() {
        let signal_bit = 1u128 << (man_bits - 1);
        return Unpacked {
            sign,
            class: if man == 0 { Class::Inf } else { Class::Nan },
            sig: man,
            exp: 0,
            // The VR4300 uses the LEGACY MIPS convention: significand MSB
            // **set** means signalling, the opposite of IEEE-754:2008. See
            // `fpu::is_snan_f32` and accuracy ledger C-12. Naming the constant
            // `quiet_bit` and then testing it for *signalling* would be a trap
            // for the next reader, so it is named for the position it occupies.
            snan: man & signal_bit != 0,
        };
    }
    Unpacked {
        sign,
        class: Class::Finite,
        sig: man | (1u128 << man_bits),
        exp: biased as i32 - f.bias - man_bits as i32,
        snan: false,
    }
}

/// A computed result: the encoding plus what producing it raised.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rounded {
    /// The result's encoding, in the low [`Format::width`] bits.
    pub bits: u64,
    /// The IEEE exceptions the operation raised.
    pub flags: Flags,
}

/// ±0 with no flags.
const fn zero(sign: bool, f: Format) -> Rounded {
    Rounded {
        bits: (sign as u64) << (f.width - 1),
        flags: Flags::NONE,
    }
}

/// ±∞ with no flags.
const fn inf(sign: bool, f: Format) -> Rounded {
    Rounded {
        bits: ((sign as u64) << (f.width - 1)) | f.infinity(),
        flags: Flags::NONE,
    }
}

/// The default NaN, flagged Invalid.
const fn invalid(f: Format) -> Rounded {
    Rounded {
        bits: f.default_nan(),
        flags: Flags::INVALID,
    }
}

/// Round `sign × (sig + ε) × 2^exp` into `f`, where `ε ∈ (0, 1)` exactly when
/// `sticky` — and report every flag the rounding raised.
///
/// This is the only place a result is rounded and the only place `inexact`,
/// `overflow` and `underflow` are decided, so those three cannot disagree with
/// each other by construction.
///
/// # Panics
///
/// Never in release. `sig == 0` together with `sticky` would describe a value
/// smaller than any the callers can produce — each of them keeps at least 64
/// guard bits, so a discarded bit implies a significand far from zero — and the
/// debug assertion pins that argument rather than leaving it in a comment.
fn round_pack(sign: bool, sig: u128, exp: i32, sticky: bool, f: Format, mode: Rounding) -> Rounded {
    debug_assert!(
        !(sig == 0 && sticky),
        "a discarded bit implies a large significand; see the callers' guard argument"
    );
    if sig == 0 {
        return zero(sign, f);
    }

    let used = 128 - sig.leading_zeros() as i32;
    // Drop down to `p` bits...
    let to_p = used - f.p as i32;
    // ...but never below the smallest subnormal's LSB, which is what turns an
    // out-of-range result into a subnormal instead of a wrong normal.
    let to_min = f.min_lsb_exp() - exp;
    let shift = if to_p > to_min { to_p } else { to_min };

    let (mut kept, mut exp_f, inexact) = if shift <= 0 {
        // Room to spare: shifting LEFT is exact.
        #[allow(clippy::cast_sign_loss)] // guarded by `shift <= 0`
        let left = (-shift) as u32;
        (sig << left, exp + shift, sticky)
    } else {
        #[allow(clippy::cast_sign_loss)] // guarded by the `else`
        let sh = shift as u32;
        // A shift of 128 or more is well-defined here only because everything
        // is then discarded; `u128 >> 128` is UB-adjacent in other languages
        // and a panic in Rust, so it is branched rather than relied upon.
        let (kept, discarded_nonzero) = if sh >= 128 {
            (0u128, sig != 0)
        } else {
            (sig >> sh, sig & ((1u128 << sh) - 1) != 0)
        };
        let round_bit = if sh > 128 {
            false
        } else {
            sig >> (sh - 1) & 1 != 0
        };
        let lower = if sh <= 1 {
            false
        } else if sh > 128 {
            sig != 0
        } else {
            sig & ((1u128 << (sh - 1)) - 1) != 0
        };
        let sticky_all = lower || sticky;
        let inexact = discarded_nonzero || sticky;

        let increment = match mode {
            // Ties to even: step up on a tie only when it would leave an odd
            // last bit behind.
            Rounding::Nearest => round_bit && (sticky_all || kept & 1 != 0),
            Rounding::TowardZero => false,
            Rounding::TowardPlusInf => inexact && !sign,
            Rounding::TowardMinusInf => inexact && sign,
        };
        (kept + u128::from(increment), exp + shift, inexact)
    };

    // A round-up can carry out of the significand: 0x1FF…F + 1 = 0x200…0.
    if kept >> f.p != 0 {
        kept >>= 1;
        exp_f += 1;
    }

    let mut flags = Flags::NONE;
    flags.inexact = inexact;

    if kept == 0 {
        // Rounded all the way to zero: tiny and inexact.
        flags.underflow = inexact;
        return Rounded {
            bits: (sign as u64) << (f.width - 1),
            flags,
        };
    }

    let sign_bit = (sign as u64) << (f.width - 1);
    if kept >> (f.p - 1) != 0 {
        // Normal — the implicit bit is present.
        let biased = exp_f + f.p as i32 - 1 + f.bias;
        if biased >= f.max_biased() as i32 {
            return overflowed(sign, f, mode);
        }
        #[allow(clippy::cast_sign_loss)] // `biased >= 1` on this branch
        let biased = biased as u64;
        let man = (kept as u64) & ((1u64 << f.man_bits()) - 1);
        return Rounded {
            bits: sign_bit | (biased << f.man_bits()) | man,
            flags,
        };
    }

    // Subnormal. `to_min` won the shift, so `exp_f` is the minimum LSB exponent
    // and `kept` is the stored mantissa verbatim.
    debug_assert_eq!(exp_f, f.min_lsb_exp());
    flags.underflow = inexact;
    Rounded {
        bits: sign_bit | (kept as u64),
        flags,
    }
}

/// The result of a magnitude too large for the format.
///
/// **Which value comes back depends on the rounding mode**, and it is not
/// always infinity: a directed mode that rounds *toward* the finite range
/// delivers the largest finite value instead. Reaching for infinity
/// unconditionally is correct only for round-to-nearest.
fn overflowed(sign: bool, f: Format, mode: Rounding) -> Rounded {
    let sign_bit = (sign as u64) << (f.width - 1);
    let to_inf = match mode {
        Rounding::Nearest => true,
        Rounding::TowardZero => false,
        Rounding::TowardPlusInf => !sign,
        Rounding::TowardMinusInf => sign,
    };
    let mut flags = Flags::NONE;
    flags.overflow = true;
    flags.inexact = true;
    Rounded {
        bits: sign_bit | if to_inf { f.infinity() } else { f.max_finite() },
        flags,
    }
}

/// Propagate a NaN operand, or produce the default NaN.
///
/// A **signalling** operand raises Invalid; a quiet one does not. Either way
/// the VR4300 delivers its own default NaN rather than the operand's payload,
/// which is why nothing is copied through.
fn nan_result(a: Unpacked, b: Unpacked, f: Format) -> Rounded {
    let mut flags = Flags::NONE;
    flags.invalid = a.snan || b.snan;
    Rounded {
        bits: f.default_nan(),
        flags,
    }
}

/// Guard bits kept below the larger operand's LSB during an addition.
///
/// 64 is far more than correct rounding needs (two guard bits and a sticky
/// suffice). The margin buys the argument that makes `round_pack`'s debug
/// assertion hold: bits are discarded only when the exponents differ by more
/// than this, and at that separation the operands cannot cancel — so a
/// discarded bit never coexists with a zero significand.
const GUARD: i32 = 64;

/// `a + b`.
#[must_use]
pub fn add(a_bits: u64, b_bits: u64, f: Format, mode: Rounding) -> Rounded {
    add_unpacked(unpack(a_bits, f), unpack(b_bits, f), f, mode)
}

/// `a - b` — addition with the subtrahend's sign flipped, which is exact and
/// is how the hardware does it too.
#[must_use]
pub fn sub(a_bits: u64, b_bits: u64, f: Format, mode: Rounding) -> Rounded {
    let mut b = unpack(b_bits, f);
    // Flipping the sign of a NaN must not change that it is a NaN, and the
    // sign of a NaN is not otherwise consulted.
    b.sign = !b.sign;
    add_unpacked(unpack(a_bits, f), b, f, mode)
}

fn add_unpacked(a: Unpacked, b: Unpacked, f: Format, mode: Rounding) -> Rounded {
    if a.class == Class::Nan || b.class == Class::Nan {
        return nan_result(a, b, f);
    }
    if a.class == Class::Inf || b.class == Class::Inf {
        return match (a.class, b.class) {
            // ∞ + (−∞) is the undefined form; same-signed infinities are not.
            (Class::Inf, Class::Inf) if a.sign != b.sign => invalid(f),
            (Class::Inf, _) => inf(a.sign, f),
            _ => inf(b.sign, f),
        };
    }
    if a.class == Class::Zero && b.class == Class::Zero {
        // (+0) + (−0) is +0 in every mode except toward −∞, where it is −0.
        // Getting this wrong is invisible until something reads the sign bit.
        let sign = if a.sign == b.sign {
            a.sign
        } else {
            matches!(mode, Rounding::TowardMinusInf)
        };
        return zero(sign, f);
    }
    if a.class == Class::Zero {
        return round_pack(b.sign, b.sig, b.exp, false, f, mode);
    }
    if b.class == Class::Zero {
        return round_pack(a.sign, a.sig, a.exp, false, f, mode);
    }

    let (hi, lo) = if a.exp >= b.exp { (a, b) } else { (b, a) };
    let diff = hi.exp - lo.exp;
    let target_exp = hi.exp - GUARD;

    let hi_scaled = hi.sig << GUARD;
    #[allow(clippy::cast_sign_loss)] // both branches are guarded on `diff`
    let (lo_scaled, sticky) = if diff <= GUARD {
        (lo.sig << (GUARD - diff) as u32, false)
    } else {
        let sh = (diff - GUARD) as u32;
        if sh >= 128 {
            (0u128, true)
        } else {
            (lo.sig >> sh, lo.sig & ((1u128 << sh) - 1) != 0)
        }
    };

    if hi.sign == lo.sign {
        // Same sign: magnitudes add, and any discarded bits stay below.
        return round_pack(hi.sign, hi_scaled + lo_scaled, target_exp, sticky, f, mode);
    }

    // Opposite signs: magnitudes subtract.
    if sticky {
        // The true subtrahend is `lo_scaled + ε`, so the difference is
        // `(hi - lo - 1) + (1 - ε)`: one less, with a fresh sticky remainder.
        // Simply ignoring ε here rounds the wrong way on a tie.
        return round_pack(
            hi.sign,
            hi_scaled - lo_scaled - 1,
            target_exp,
            true,
            f,
            mode,
        );
    }
    if hi_scaled == lo_scaled {
        // Exact cancellation. IEEE 754 §6.3: the sum is +0 in every mode
        // except toward −∞.
        return zero(matches!(mode, Rounding::TowardMinusInf), f);
    }
    let (sign, mag) = if hi_scaled > lo_scaled {
        (hi.sign, hi_scaled - lo_scaled)
    } else {
        (lo.sign, lo_scaled - hi_scaled)
    };
    round_pack(sign, mag, target_exp, false, f, mode)
}

/// `a × b`.
///
/// The product of two significands is **exact** in `u128`: at most 53 × 53 =
/// 106 bits. So there is no sticky bit to carry here, and every flag comes from
/// the single rounding.
#[must_use]
pub fn mul(a_bits: u64, b_bits: u64, f: Format, mode: Rounding) -> Rounded {
    let a = unpack(a_bits, f);
    let b = unpack(b_bits, f);
    if a.class == Class::Nan || b.class == Class::Nan {
        return nan_result(a, b, f);
    }
    let sign = a.sign ^ b.sign;
    if a.class == Class::Inf || b.class == Class::Inf {
        // 0 × ∞ is the undefined form.
        if a.class == Class::Zero || b.class == Class::Zero {
            return invalid(f);
        }
        return inf(sign, f);
    }
    if a.class == Class::Zero || b.class == Class::Zero {
        return zero(sign, f);
    }
    round_pack(sign, a.sig * b.sig, a.exp + b.exp, false, f, mode)
}

/// `a ÷ b`.
///
/// A quotient is generally non-terminating in binary, so unlike the other three
/// operations this one *must* carry a sticky bit: the division's remainder is
/// exactly the information the native operator throws away.
#[must_use]
pub fn div(a_bits: u64, b_bits: u64, f: Format, mode: Rounding) -> Rounded {
    let a = unpack(a_bits, f);
    let b = unpack(b_bits, f);
    if a.class == Class::Nan || b.class == Class::Nan {
        return nan_result(a, b, f);
    }
    let sign = a.sign ^ b.sign;
    match (a.class, b.class) {
        // ∞/∞ and 0/0 are undefined forms; they are NOT division by zero.
        (Class::Inf, Class::Inf) | (Class::Zero, Class::Zero) => return invalid(f),
        (Class::Inf, _) => return inf(sign, f),
        (_, Class::Inf) | (Class::Zero, _) => return zero(sign, f),
        (_, Class::Zero) => {
            // A finite non-zero numerator over zero. This is the ONLY case that
            // raises DivideByZero: the flag marks an infinity created out of
            // finite operands, which is why `∞/0` above does not qualify.
            let mut flags = Flags::NONE;
            flags.div_by_zero = true;
            return Rounded {
                bits: inf(sign, f).bits,
                flags,
            };
        }
        _ => {}
    }

    // Normalise both significands to bit 63 so the quotient always carries at
    // least 64 significant bits — enough for `p + 2` in either format. Without
    // this a subnormal numerator over a large divisor yields a quotient of only
    // a dozen bits and the rounding below has nothing to round.
    let (na, nae) = norm_to_63(a.sig, a.exp);
    let (nb, nbe) = norm_to_63(b.sig, b.exp);

    let num = na << 64;
    let q = num / nb;
    let r = num % nb;
    round_pack(sign, q, nae - nbe - 64, r != 0, f, mode)
}

/// `SQRT.fmt` — correctly rounded, with exact flags.
///
/// # How
///
/// `value = m x 2^e`. Force `e` even (shifting `m` left compensates), then
/// scale `m` up by `2^62` so the integer square root has ~64 significant bits
/// — comfortably more than the `p + 2` correct rounding needs:
///
/// ```text
/// sqrt(m x 2^e) = sqrt(m x 2^62) x 2^(e/2 - 31)
/// ```
///
/// `u128::isqrt` gives the floor of that root, and the root is exact precisely
/// when `q * q == n` — so that comparison **is** the sticky bit, with no
/// tolerance and no second rounding. `q < 2^64`, so the square cannot overflow
/// `u128`.
///
/// # Signs
///
/// `sqrt(-0)` is `-0`, not a NaN: the sign is preserved and nothing is raised.
/// Any *other* negative operand is Invalid. Collapsing the two is a common
/// error and IEEE is explicit about the exception.
#[must_use]
pub fn sqrt(bits: u64, f: Format, mode: Rounding) -> Rounded {
    let a = unpack(bits, f);
    match a.class {
        Class::Nan => {
            let mut flags = Flags::NONE;
            flags.invalid = a.snan;
            Rounded {
                bits: f.default_nan(),
                flags,
            }
        }
        // Both zeros come back unchanged, sign included.
        Class::Zero => zero(a.sign, f),
        // Every negative operand except `-0` is Invalid, infinity included;
        // `-0` was already returned above with its sign intact.
        Class::Inf | Class::Finite if a.sign => invalid(f),
        Class::Inf => inf(false, f),
        Class::Finite => {
            let (mut m, mut e) = norm_to_63(a.sig, a.exp);
            if e & 1 != 0 {
                // `e` must be even to halve it exactly. Shifting `m` left one
                // and decrementing `e` is the same value.
                m <<= 1;
                e -= 1;
            }
            // `m < 2^65`, so `m << 62 < 2^127` and the shift cannot overflow.
            let n = m << 62;
            let q = n.isqrt();
            round_pack(false, q, e / 2 - 31, q * q != n, f, mode)
        }
    }
}

/// Convert between formats — `CVT.S.D` narrowing, `CVT.D.S` widening.
///
/// # Why a conversion belongs here and not in `fpu`
///
/// **Narrowing is an arithmetic operation.** `CVT.S.D` has to round a 53-bit
/// significand into 24, so it can be inexact, can overflow to infinity, and can
/// underflow into the subnormal range — and each of those depends on
/// `FCSR.RM`. A `v as f32` cast reports none of it and rounds to nearest only,
/// which is where accuracy ledger C-11 found this operation still sitting after
/// the arithmetic had been fixed.
///
/// Widening cannot lose anything, but goes through the same path so there is
/// one conversion rather than two that can disagree.
///
/// # NaN handling
///
/// A NaN operand yields the **target format's** default NaN, flagged Invalid
/// when the operand signals. The VR4300's *other* NaN class — significand MSB
/// clear, quiet by its convention (ledger C-12) — raises unimplemented
/// operation instead and is rejected by the caller before reaching here, so it
/// is deliberately not a case below.
#[must_use]
pub fn convert(bits: u64, from: Format, to: Format, mode: Rounding) -> Rounded {
    let a = unpack(bits, from);
    match a.class {
        Class::Nan => {
            let mut flags = Flags::NONE;
            flags.invalid = a.snan;
            Rounded {
                bits: to.default_nan(),
                flags,
            }
        }
        Class::Inf => inf(a.sign, to),
        Class::Zero => zero(a.sign, to),
        // The significand and exponent are format-independent here: `unpack`
        // has already turned them into a plain `sig * 2^exp`, so the only work
        // is rounding that into the target's precision and range.
        Class::Finite => round_pack(a.sign, a.sig, a.exp, false, to, mode),
    }
}

/// Shift `sig` left until its most significant bit sits at bit 63, adjusting
/// the exponent to match. Exact — it is a change of scale, not of value.
fn norm_to_63(sig: u128, exp: i32) -> (u128, i32) {
    debug_assert!(sig != 0);
    // Significands are at most 53 bits, so this shift is always to the left.
    let sh = sig.leading_zeros() as i32 - 64;
    debug_assert!(sh > 0);
    (sig << sh, exp - sh)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn as32(r: Rounded) -> f32 {
        f32::from_bits(r.bits as u32)
    }
    fn as64(r: Rounded) -> f64 {
        f64::from_bits(r.bits)
    }
    fn b32(v: f32) -> u64 {
        u64::from(v.to_bits())
    }

    /// A deterministic PRNG — ADR 0004 forbids entropy, and a differential test
    /// that cannot be replayed is not evidence.
    struct SplitMix(u64);
    impl SplitMix {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
    }

    // --- Differential against the native operators ---------------------------

    /// **The load-bearing test.** In round-to-nearest-even the soft-float
    /// result must be *bit-identical* to the hardware operator for every one of
    /// a large pseudo-random corpus, in both formats and all four operations.
    ///
    /// This is what makes the flags trustworthy: the flags come from the same
    /// rounding step as the value, so a value that matches an independent
    /// oracle bit-for-bit is strong evidence that the guard/sticky bookkeeping
    /// the flags are read from is right. Testing the flags alone would be
    /// self-referential — there is no other implementation here to disagree
    /// with.
    ///
    /// NaN results are compared as "both are NaN": the VR4300's default NaN is
    /// deliberately not the one Rust produces, and that difference is the
    /// subject of its own test below.
    #[test]
    fn every_operation_matches_the_native_operator_bit_for_bit_in_round_to_nearest() {
        let mut rng = SplitMix(0x5EED);
        for _ in 0..40_000 {
            let ab = rng.next() as u32;
            let bb = rng.next() as u32;
            let (x, y) = (f32::from_bits(ab), f32::from_bits(bb));
            for (op, want, got) in [
                (
                    "add",
                    x + y,
                    as32(add(u64::from(ab), u64::from(bb), F32, Rounding::Nearest)),
                ),
                (
                    "sub",
                    x - y,
                    as32(sub(u64::from(ab), u64::from(bb), F32, Rounding::Nearest)),
                ),
                (
                    "mul",
                    x * y,
                    as32(mul(u64::from(ab), u64::from(bb), F32, Rounding::Nearest)),
                ),
                (
                    "div",
                    x / y,
                    as32(div(u64::from(ab), u64::from(bb), F32, Rounding::Nearest)),
                ),
            ] {
                if want.is_nan() {
                    assert!(got.is_nan(), "{op}: {x:e} {y:e} -> want NaN, got {got:e}");
                } else {
                    assert_eq!(
                        got.to_bits(),
                        want.to_bits(),
                        "f32 {op}: {x:e} ({ab:#010X}) op {y:e} ({bb:#010X})"
                    );
                }
            }

            let (ab, bb) = (rng.next(), rng.next());
            let (x, y) = (f64::from_bits(ab), f64::from_bits(bb));
            for (op, want, got) in [
                ("add", x + y, as64(add(ab, bb, F64, Rounding::Nearest))),
                ("sub", x - y, as64(sub(ab, bb, F64, Rounding::Nearest))),
                ("mul", x * y, as64(mul(ab, bb, F64, Rounding::Nearest))),
                ("div", x / y, as64(div(ab, bb, F64, Rounding::Nearest))),
            ] {
                if want.is_nan() {
                    assert!(got.is_nan(), "{op}: {x:e} {y:e}");
                } else {
                    assert_eq!(
                        got.to_bits(),
                        want.to_bits(),
                        "f64 {op}: {x:e} ({ab:#018X}) op {y:e} ({bb:#018X})"
                    );
                }
            }
        }
    }

    /// The random corpus above is dominated by huge and tiny magnitudes, which
    /// is good for exponent handling and bad for coverage of the ordinary
    /// range. This one draws small integers and simple fractions, where
    /// cancellation and exactness actually occur.
    #[test]
    fn the_ordinary_numeric_range_matches_the_native_operator_too() {
        let mut rng = SplitMix(0xC0FFEE);
        for _ in 0..40_000 {
            let x = (rng.next() % 2001) as f32 / 8.0 - 125.0;
            let y = (rng.next() % 2001) as f32 / 8.0 - 125.0;
            let (xb, yb) = (b32(x), b32(y));
            assert_eq!(
                as32(add(xb, yb, F32, Rounding::Nearest)).to_bits(),
                (x + y).to_bits(),
                "{x} + {y}"
            );
            assert_eq!(
                as32(sub(xb, yb, F32, Rounding::Nearest)).to_bits(),
                (x - y).to_bits(),
                "{x} - {y}"
            );
            assert_eq!(
                as32(mul(xb, yb, F32, Rounding::Nearest)).to_bits(),
                (x * y).to_bits(),
                "{x} * {y}"
            );
            let q = div(xb, yb, F32, Rounding::Nearest);
            if !(x / y).is_nan() {
                assert_eq!(as32(q).to_bits(), (x / y).to_bits(), "{x} / {y}");
            }
        }
    }

    /// Subnormal results are where the `f64`-comparison shortcut breaks, so
    /// they get their own sweep rather than being left to chance.
    #[test]
    fn the_subnormal_range_matches_the_native_operator() {
        let mut rng = SplitMix(0xD3ADB33F);
        for _ in 0..20_000 {
            // Magnitudes just above and below f32's smallest normal.
            let a = f32::from_bits((rng.next() as u32 & 0x007F_FFFF) | 0x0080_0000);
            let b = f32::from_bits(rng.next() as u32 & 0x00FF_FFFF);
            let (ab, bb) = (b32(a), b32(b));
            assert_eq!(
                as32(add(ab, bb, F32, Rounding::Nearest)).to_bits(),
                (a + b).to_bits()
            );
            assert_eq!(
                as32(sub(ab, bb, F32, Rounding::Nearest)).to_bits(),
                (a - b).to_bits()
            );
            assert_eq!(
                as32(mul(ab, bb, F32, Rounding::Nearest)).to_bits(),
                (a * b).to_bits()
            );
        }
    }

    // --- Flags ---------------------------------------------------------------

    /// An exactly representable result raises **nothing**. This is the control
    /// for every inexact assertion below: an implementation that flagged
    /// everything inexact would satisfy them all.
    #[test]
    fn an_exact_operation_raises_no_flags() {
        let r = add(b32(1.0), b32(2.0), F32, Rounding::Nearest);
        assert_eq!(as32(r), 3.0);
        assert_eq!(r.flags, Flags::NONE, "1 + 2 is exact");
        let r = mul(b32(3.0), b32(0.5), F32, Rounding::Nearest);
        assert_eq!(r.flags, Flags::NONE, "3 * 0.5 is exact");
        let r = div(b32(1.0), b32(4.0), F32, Rounding::Nearest);
        assert_eq!(r.flags, Flags::NONE, "1 / 4 terminates");
    }

    /// **The case ledger C-11 is about**, verbatim from n64-systemtest:
    /// `f32::MIN + (-1.0)` returns `f32::MIN` and must raise Inexact.
    ///
    /// The result is *unchanged*, which is exactly why the flag is the only
    /// observable: an implementation that returns the right value and no flag
    /// looks correct until `FCSR` is read back.
    #[test]
    fn the_c11_case_raises_inexact_though_the_value_is_unchanged() {
        for (a, b) in [
            (f32::MIN, -1.0f32),
            (f32::MAX, -1.0f32),
            (f32::MAX, 1.0f32),
            (f32::MAX, f32::MIN_POSITIVE),
        ] {
            let r = add(b32(a), b32(b), F32, Rounding::Nearest);
            assert_eq!(as32(r), a + b, "value");
            assert!(r.flags.inexact, "{a:e} + {b:e} must raise Inexact");
            assert!(!r.flags.overflow, "and must not overflow");
            assert!(!r.flags.underflow, "nor underflow");
        }
    }

    /// `1/3` does not terminate in binary, so the sticky bit must survive the
    /// division. This is the flag the native operator cannot report at all.
    #[test]
    fn a_non_terminating_quotient_is_inexact() {
        let r = div(b32(1.0), b32(3.0), F32, Rounding::Nearest);
        assert!(r.flags.inexact, "1/3 is inexact");
        assert_eq!(as32(r), 1.0f32 / 3.0);
        // ...and one that does terminate is not, so the flag is not simply
        // always set on division.
        assert!(
            !div(b32(1.0), b32(2.0), F32, Rounding::Nearest)
                .flags
                .inexact
        );
    }

    /// Overflow implies Inexact — they are not independent, and reporting
    /// overflow alone leaves `FCSR` in a state hardware never produces.
    #[test]
    fn overflow_is_also_inexact_and_saturates_per_rounding_mode() {
        let big = b32(3e38);
        let r = add(big, b32(8e37), F32, Rounding::Nearest);
        assert!(r.flags.overflow && r.flags.inexact);
        assert_eq!(as32(r), f32::INFINITY);

        // Toward zero cannot reach infinity: it saturates at MAX.
        let r = add(big, b32(8e37), F32, Rounding::TowardZero);
        assert!(r.flags.overflow && r.flags.inexact);
        assert_eq!(as32(r), f32::MAX, "toward zero saturates, it does not inf");

        // Toward −∞ on a positive overflow likewise stops at MAX.
        let r = add(big, b32(8e37), F32, Rounding::TowardMinusInf);
        assert_eq!(as32(r), f32::MAX);
        // ...but a negative overflow in the same mode does reach −∞.
        let r = add(b32(-3e38), b32(-8e37), F32, Rounding::TowardMinusInf);
        assert_eq!(as32(r), f32::NEG_INFINITY);
    }

    /// Underflow is signalled when the result is tiny **and** inexact — an
    /// exact subnormal result raises neither.
    #[test]
    fn underflow_needs_both_tininess_and_inexactness() {
        // Two subnormals whose sum is exactly representable: tiny, exact.
        let a = f32::from_bits(3);
        let b = f32::from_bits(4);
        let r = add(b32(a), b32(b), F32, Rounding::Nearest);
        assert_eq!(as32(r), f32::from_bits(7));
        assert!(!r.flags.underflow, "an exact subnormal does not underflow");
        assert!(!r.flags.inexact);

        // A product that lands below the subnormal grid: tiny and inexact.
        let r = mul(b32(f32::from_bits(3)), b32(0.5), F32, Rounding::Nearest);
        assert!(
            r.flags.underflow && r.flags.inexact,
            "flags = {:?}",
            r.flags
        );
    }

    /// The four IEEE special forms, kept apart. `∞/0` in particular is **not**
    /// `DivideByZero`: that flag marks an infinity conjured from finite operands.
    #[test]
    fn the_invalid_and_divide_by_zero_forms_are_distinguished() {
        let nan = |r: Rounded| as32(r).is_nan();

        assert!(nan(add(
            b32(f32::INFINITY),
            b32(f32::NEG_INFINITY),
            F32,
            Rounding::Nearest
        )));
        assert!(
            add(
                b32(f32::INFINITY),
                b32(f32::NEG_INFINITY),
                F32,
                Rounding::Nearest
            )
            .flags
            .invalid
        );
        assert!(
            mul(b32(0.0), b32(f32::INFINITY), F32, Rounding::Nearest)
                .flags
                .invalid
        );
        assert!(
            div(b32(0.0), b32(0.0), F32, Rounding::Nearest)
                .flags
                .invalid
        );
        assert!(
            div(
                b32(f32::INFINITY),
                b32(f32::INFINITY),
                F32,
                Rounding::Nearest
            )
            .flags
            .invalid
        );

        let dz = div(b32(1.0), b32(0.0), F32, Rounding::Nearest);
        assert!(dz.flags.div_by_zero, "finite / 0 is DivideByZero");
        assert!(!dz.flags.invalid, "and not Invalid");
        assert_eq!(as32(dz), f32::INFINITY);

        let iz = div(b32(f32::INFINITY), b32(0.0), F32, Rounding::Nearest);
        assert!(!iz.flags.div_by_zero, "inf / 0 was already infinite");
        assert_eq!(as32(iz), f32::INFINITY);
    }

    /// An invalid operation delivers the **VR4300's** default NaN, which has the
    /// quiet bit clear and is therefore not the NaN Rust would produce.
    #[test]
    fn an_invalid_operation_delivers_the_vr4300_default_nan() {
        let r = add(
            b32(f32::INFINITY),
            b32(f32::NEG_INFINITY),
            F32,
            Rounding::Nearest,
        );
        assert_eq!(r.bits, 0x7FBF_FFFF, "the value n64-systemtest expects");
        let r = add(
            f64::INFINITY.to_bits(),
            f64::NEG_INFINITY.to_bits(),
            F64,
            Rounding::Nearest,
        );
        assert_eq!(r.bits, 0x7FF7_FFFF_FFFF_FFFF);
    }

    /// A **signalling** NaN operand raises Invalid; a quiet one propagates
    /// silently. Treating every NaN as signalling raises Invalid on ordinary
    /// NaN propagation, which is a common and invisible error.
    #[test]
    fn only_a_signalling_nan_operand_raises_invalid() {
        // **Inverted from IEEE**: on the VR4300 the significand MSB set means
        // *signalling*. See `fpu::is_snan_f32` and ledger C-12.
        let snan = 0x7FC0_0000u64; // MSB set   -> signalling here
        let qnan = 0x7FA0_0000u64; // MSB clear -> quiet here
        assert!(add(snan, b32(1.0), F32, Rounding::Nearest).flags.invalid);
        assert!(!add(qnan, b32(1.0), F32, Rounding::Nearest).flags.invalid);
        assert!(mul(b32(1.0), snan, F32, Rounding::Nearest).flags.invalid);
    }

    // --- Rounding modes ------------------------------------------------------

    /// The four modes bracket an inexact result: toward −∞ ≤ nearest ≤
    /// toward +∞, and toward zero equals one of the outer two by sign.
    #[test]
    fn the_directed_modes_bracket_the_nearest_result() {
        // 1e15 + 5e-20 is inexact in f32 and n64-systemtest tests it directly.
        let (a, b) = (b32(1e15), b32(5e-20));
        let down = as32(add(a, b, F32, Rounding::TowardMinusInf));
        let near = as32(add(a, b, F32, Rounding::Nearest));
        let up = as32(add(a, b, F32, Rounding::TowardPlusInf));
        let zero = as32(add(a, b, F32, Rounding::TowardZero));
        assert!(down <= near && near <= up, "{down:e} {near:e} {up:e}");
        assert!(down < up, "an inexact result must differ between the modes");
        assert_eq!(zero, down, "toward zero == toward −∞ for a positive value");
    }

    /// The exact n64-systemtest expectations for the `ADD.S` rounding-mode
    /// cases. These are golden vectors from an independent oracle, not values
    /// this implementation produced — which is the only kind that can falsify
    /// it (module 20, *Golden vectors*).
    #[test]
    fn the_n64_systemtest_rounding_vectors_hold() {
        let cases: &[(f32, f32, Rounding, f32)] = &[
            (1e15, 5e-20, Rounding::Nearest, 1e15),
            (1e15, 5e-20, Rounding::TowardZero, 1e15),
            (1e15, 5e-20, Rounding::TowardPlusInf, 1000000050000000f32),
            (1e15, 5e-20, Rounding::TowardMinusInf, 1e15),
            (-1e15, -5e-20, Rounding::Nearest, -1e15),
            (-1e15, -5e-20, Rounding::TowardZero, -1e15),
            (-1e15, -5e-20, Rounding::TowardPlusInf, -1e15),
            (
                -1e15,
                -5e-20,
                Rounding::TowardMinusInf,
                -1000000050000000f32,
            ),
            (1e15, 33500000f32, Rounding::Nearest, 1e15),
            (1e15, 33600000f32, Rounding::Nearest, 1000000050000000f32),
            (1e15, 33500000f32, Rounding::TowardZero, 1e15),
            (1e15, 33600000f32, Rounding::TowardZero, 1e15),
        ];
        for &(a, b, mode, want) in cases {
            let r = add(b32(a), b32(b), F32, mode);
            assert_eq!(
                as32(r).to_bits(),
                want.to_bits(),
                "{a:e} + {b:e} under {mode:?}"
            );
            assert!(r.flags.inexact, "{a:e} + {b:e} under {mode:?} is inexact");
        }
    }

    /// Ties-to-even resolves *to even*, not away from zero. `f32` has 24
    /// significand bits, so `2^24 + 1` is exactly a tie.
    #[test]
    fn a_tie_rounds_to_even_not_away_from_zero() {
        let two24 = b32(16_777_216.0); // 2^24
        // 2^24 + 1 is a tie between 2^24 and 2^24+2; even wins.
        let r = add(two24, b32(1.0), F32, Rounding::Nearest);
        assert_eq!(as32(r), 16_777_216.0, "ties down to the even value");
        assert!(r.flags.inexact);
        // 2^24 + 3 ties between 2^24+2 and 2^24+4; even is +4.
        let r = add(two24, b32(3.0), F32, Rounding::Nearest);
        assert_eq!(as32(r), 16_777_220.0, "ties up to the even value");
    }

    // --- Signed zero ---------------------------------------------------------

    /// The sign of a zero sum is mode-dependent, and it is the one place a
    /// rounding mode changes a result that is otherwise exact.
    #[test]
    fn the_sign_of_a_cancelled_zero_follows_the_rounding_mode() {
        let r = add(b32(1.0), b32(-1.0), F32, Rounding::Nearest);
        assert_eq!(r.bits, 0, "+0 in round-to-nearest");
        let r = add(b32(1.0), b32(-1.0), F32, Rounding::TowardMinusInf);
        assert_eq!(r.bits, 0x8000_0000, "−0 toward −∞");
        let r = add(b32(-0.0), b32(-0.0), F32, Rounding::Nearest);
        assert_eq!(r.bits, 0x8000_0000, "(−0) + (−0) is −0 in every mode");
    }

    /// Narrowing must match the native `as f32` in round-to-nearest, across the
    /// same three corpora the arithmetic uses — including the subnormal
    /// boundary, where a naive implementation double-rounds.
    #[test]
    fn narrowing_matches_the_native_cast_in_round_to_nearest() {
        let mut rng = SplitMix(0xC0DE_1234);
        for _ in 0..40_000 {
            let bits = rng.next();
            let v = f64::from_bits(bits);
            let got = convert(bits, F64, F32, Rounding::Nearest);
            let want = v as f32;
            if want.is_nan() {
                assert!(f32::from_bits(got.bits as u32).is_nan(), "{v:e}");
            } else {
                assert_eq!(got.bits as u32, want.to_bits(), "narrowing {v:e}");
            }
        }
        // Widening is exact for every f32, so it must round-trip.
        for _ in 0..20_000 {
            let b = rng.next() as u32;
            let v = f32::from_bits(b);
            let got = convert(u64::from(b), F32, F64, Rounding::Nearest);
            if v.is_nan() {
                assert!(f64::from_bits(got.bits).is_nan());
            } else {
                assert_eq!(got.bits, f64::from(v).to_bits(), "widening {v:e}");
                assert_eq!(got.flags, Flags::NONE, "widening is always exact");
            }
        }
    }

    /// The n64-systemtest `CVT.S.D` vectors: a value that needs rounding must
    /// differ between the modes, and one past `f32::MAX` must overflow the way
    /// the mode directs.
    #[test]
    fn narrowing_honours_the_rounding_mode_and_overflows_per_mode() {
        let v = 4.123_456_789_123_456_f64.to_bits();
        let down = convert(v, F64, F32, Rounding::TowardMinusInf);
        let near = convert(v, F64, F32, Rounding::Nearest);
        let up = convert(v, F64, F32, Rounding::TowardPlusInf);
        assert!(down.flags.inexact && near.flags.inexact && up.flags.inexact);
        assert_ne!(
            down.bits, up.bits,
            "the modes must disagree on an inexact value"
        );
        assert_eq!(
            convert(v, F64, F32, Rounding::TowardZero).bits,
            down.bits,
            "toward zero == toward -inf for a positive value"
        );

        // Just past f32::MAX.
        let big = 3.402_823_48e38_f64.to_bits();
        let r = convert(big, F64, F32, Rounding::TowardPlusInf);
        assert!(r.flags.overflow && r.flags.inexact);
        assert_eq!(f32::from_bits(r.bits as u32), f32::INFINITY);
        let r = convert(big, F64, F32, Rounding::TowardZero);
        assert_eq!(
            f32::from_bits(r.bits as u32),
            f32::MAX,
            "toward zero saturates"
        );
    }

    /// A double inside `f32`'s **subnormal** range must narrow to an actual
    /// subnormal and report underflow — the case the VR4300 then refuses
    /// outright (ledger C-13), which it can only do if this reports it.
    ///
    /// The first draft used `f64::MIN_POSITIVE`, which is ~2.2e-308 and narrows
    /// to plain **zero** — far below `f32`'s entire range, so it never produced
    /// a subnormal and did not test what its name claimed. `1e-40` sits between
    /// `f32`'s smallest subnormal (~1.4e-45) and its smallest normal
    /// (~1.18e-38), which is the band that matters.
    #[test]
    fn narrowing_into_the_subnormal_range_reports_underflow() {
        let r = convert(1e-40_f64.to_bits(), F64, F32, Rounding::Nearest);
        let got = f32::from_bits(r.bits as u32);
        assert!(got != 0.0, "must not flush to zero: {got:e}");
        assert!(
            got.to_bits() & 0x7F80_0000 == 0,
            "and must be an actual subnormal: {got:e}"
        );
        assert!(
            r.flags.underflow && r.flags.inexact,
            "flags = {:?}",
            r.flags
        );

        // Below the whole range it does reach zero, still underflowing.
        let r = convert(f64::MIN_POSITIVE.to_bits(), F64, F32, Rounding::Nearest);
        assert_eq!(f32::from_bits(r.bits as u32), 0.0, "far below f32's range");
        assert!(r.flags.underflow && r.flags.inexact);
    }

    /// `SQRT` must be bit-identical to the native square root across the same
    /// corpora as the arithmetic, in both formats.
    ///
    /// `f32::sqrt` lives in `std` and this crate is `#![no_std]`, so the
    /// reference is pulled in for the test only.
    #[test]
    fn sqrt_matches_the_native_square_root_in_round_to_nearest() {
        extern crate std;
        let mut rng = SplitMix(0x5017_5017);
        for _ in 0..40_000 {
            let ab = rng.next() as u32;
            let x = f32::from_bits(ab);
            let got = as32(sqrt(u64::from(ab), F32, Rounding::Nearest));
            let want = std::primitive::f32::sqrt(x);
            if want.is_nan() {
                assert!(got.is_nan(), "sqrt({x:e}) want NaN, got {got:e}");
            } else {
                assert_eq!(got.to_bits(), want.to_bits(), "sqrt({x:e})");
            }

            let bb = rng.next();
            let y = f64::from_bits(bb);
            let got = as64(sqrt(bb, F64, Rounding::Nearest));
            let want = std::primitive::f64::sqrt(y);
            if want.is_nan() {
                assert!(got.is_nan(), "sqrt({y:e})");
            } else {
                assert_eq!(got.to_bits(), want.to_bits(), "sqrt({y:e})");
            }
        }
        // Small positive integers, where exact results and ties actually occur.
        for i in 0..20_000u32 {
            let x = f32::from(i as u16) + 0.25;
            let got = as32(sqrt(b32(x), F32, Rounding::Nearest));
            assert_eq!(
                got.to_bits(),
                std::primitive::f32::sqrt(x).to_bits(),
                "sqrt({x})"
            );
        }
    }

    /// The n64-systemtest `SQRT.S` vectors, including the signs IEEE is
    /// explicit about and the directed-rounding split on `f32::MAX`.
    #[test]
    fn the_n64_systemtest_sqrt_vectors_hold() {
        // Exact results raise nothing.
        let r = sqrt(b32(4.0), F32, Rounding::Nearest);
        assert_eq!(as32(r), 2.0);
        assert_eq!(r.flags, Flags::NONE, "sqrt(4) is exact");

        // sqrt(2) is inexact.
        assert!(sqrt(b32(2.0), F32, Rounding::Nearest).flags.inexact);

        // **sqrt(-0) is -0**, not a NaN, and raises nothing.
        let r = sqrt(b32(-0.0), F32, Rounding::Nearest);
        assert_eq!(r.bits, 0x8000_0000, "sqrt(-0) = -0");
        assert_eq!(r.flags, Flags::NONE);
        // Every other negative is Invalid.
        for v in [-4.0f32, f32::MIN, -f32::MIN_POSITIVE, f32::NEG_INFINITY] {
            let r = sqrt(b32(v), F32, Rounding::Nearest);
            assert!(r.flags.invalid, "sqrt({v:e}) must be Invalid");
            assert_eq!(r.bits, 0x7FBF_FFFF, "and give the VR4300 default NaN");
        }

        assert_eq!(
            as32(sqrt(b32(f32::INFINITY), F32, Rounding::Nearest)),
            f32::INFINITY
        );

        // The rounding mode splits sqrt(f32::MAX) between two neighbours.
        let near = as32(sqrt(b32(f32::MAX), F32, Rounding::Nearest));
        let up = as32(sqrt(b32(f32::MAX), F32, Rounding::TowardPlusInf));
        let down = as32(sqrt(b32(f32::MAX), F32, Rounding::TowardMinusInf));
        assert_eq!(near.to_bits(), 1.8446743e19f32.to_bits());
        assert_eq!(up.to_bits(), 1.8446744e19f32.to_bits());
        assert_eq!(down.to_bits(), 1.8446743e19f32.to_bits());

        // A normal operand whose root is exact raises nothing.
        let r = sqrt(b32(f32::MIN_POSITIVE), F32, Rounding::Nearest);
        assert_eq!(as32(r).to_bits(), 1.0842022e-19f32.to_bits());
        assert_eq!(r.flags, Flags::NONE);
    }

    /// Format constants, asserted rather than assumed — every derived quantity
    /// in this module is computed from them.
    #[test]
    fn the_format_parameters_are_right() {
        assert_eq!(F32.max_biased(), 255);
        assert_eq!(F64.max_biased(), 2047);
        assert_eq!(
            F32.min_lsb_exp(),
            -149,
            "f32's smallest subnormal is 2^-149"
        );
        assert_eq!(F64.min_lsb_exp(), -1074);
        assert_eq!(F32.infinity(), 0x7F80_0000);
        assert_eq!(F32.max_finite(), 0x7F7F_FFFF);
        assert_eq!(F64.max_finite(), 0x7FEF_FFFF_FFFF_FFFF);
        assert_eq!(F32.default_nan(), 0x7FBF_FFFF);
        assert_eq!(F64.default_nan(), 0x7FF7_FFFF_FFFF_FFFF);
    }
}
