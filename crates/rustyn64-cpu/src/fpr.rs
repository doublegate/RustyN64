//! The floating-point register file (T-13-001).
//!
//! 32 physical 64-bit **FGRs**. How software sees them depends on
//! `Status.FR` (UM §6.3.5, and the FPR/FGR figures in Ch. 7):
//!
//! | `FR` | Register `n` addresses |
//! | --- | --- |
//! | 1 | FGR *n* — 32 independent 64-bit registers |
//! | 0 | FGR *n & !1* — **odd FGRs are not addressable at all** |
//!
//! With `FR = 0` there are 16 usable 64-bit registers, and a 32-bit access
//! picks a half of one of them: an **even** register number is the low half,
//! an **odd** register number is the **high** half of its even partner.
//!
//! # This is not the "FGR pair" model, and the difference is observable
//!
//! It is natural to read "`FR = 0` uses register pairs" as *the value is
//! `FGR[n+1]:FGR[n]`, assembled from two registers' low halves*. This module
//! had exactly that, and it is wrong: it makes `MTC1 $1` write FGR1, where
//! hardware writes the upper half of FGR0 and leaves FGR1 untouched.
//!
//! n64-systemtest pins it directly. In half mode, after `MTC1 $1`:
//!
//! ```text
//! DMFC1(0) == 0x01234567_89ABCDEF   <- the write landed in FGR0's HIGH half
//! DMFC1(1) == 0x44445555_66667777   <- unchanged
//! ```
//!
//! # Three write behaviours, not one
//!
//! - [`Fpr::write_s`] — `MTC1`/`LWC1`: deposit 32 bits, **preserve** the other
//!   half of the register.
//! - [`Fpr::write_s_arith`] — a single-precision arithmetic result: **clear**
//!   the other half. The suite's *"Upper bits of 32 bit operation"* reads the
//!   destination back with `DMFC1` after an `ADD.S` and expects zero there.
//! - [`Fpr::write_d`] — a 64-bit value, whole register.
//!
//! Collapsing the first two is invisible until something reads the register at
//! a different width, which is precisely what those tests do.

/// The 32 physical floating-point general registers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Fpr {
    /// Raw FGR storage. Prefer the accessors: they apply the `FR` view, which
    /// direct indexing silently gets wrong for `FR = 0` doubles.
    fgr: [u64; 32],
}

impl Default for Fpr {
    fn default() -> Self {
        Self::new()
    }
}

impl Fpr {
    /// Power-on state.
    ///
    /// The manual does not define one; ADR 0004 requires reproducibility, so it
    /// is a documented zero.
    #[must_use]
    pub const fn new() -> Self {
        Self { fgr: [0; 32] }
    }

    /// Read a 32-bit value from FPR `n` under the current `FR` view.
    ///
    /// | `FR` | Register `n` maps to |
    /// | --- | --- |
    /// | 1 | the **low** half of FGR *n* |
    /// | 0 | FGR *n & !1* — its **low** half for even `n`, its **HIGH** half for odd `n` |
    ///
    /// The `FR = 0` row is the whole subtlety, and it is not what "the pair
    /// `FGR[n+1]:FGR[n]`" suggests: an **odd** register in half mode is the
    /// upper 32 bits of its *even partner*, and odd FGRs are not addressable
    /// at all. n64-systemtest pins it directly — after `MTC1 $1` in half mode,
    /// `DMFC1(0)` shows the written value in its high half while `DMFC1(1)` is
    /// **unchanged**.
    #[must_use]
    pub const fn read_s(&self, n: u8, fr: bool) -> u32 {
        let i = (n & 31) as usize;
        if fr {
            self.fgr[i] as u32
        } else if i & 1 == 1 {
            (self.fgr[i & !1] >> 32) as u32
        } else {
            self.fgr[i] as u32
        }
    }

    /// Write a 32-bit value to FPR `n` under the current `FR` view.
    ///
    /// The other half of the 64-bit register is **preserved** — this models
    /// `MTC1`/`LWC1`, which deposit 32 bits and leave the rest alone. An
    /// arithmetic `.S` result does not behave this way; see
    /// [`Fpr::write_s_arith`].
    ///
    /// See [`Fpr::read_s`] for the `FR = 0` mapping.
    pub const fn write_s(&mut self, n: u8, fr: bool, v: u32) {
        let i = (n & 31) as usize;
        if !fr && i & 1 == 1 {
            let e = i & !1;
            self.fgr[e] = (self.fgr[e] & 0xFFFF_FFFF) | ((v as u64) << 32);
        } else {
            let e = if fr { i } else { i & !1 };
            self.fgr[e] = (self.fgr[e] & 0xFFFF_FFFF_0000_0000) | v as u64;
        }
    }

    /// Write a single-precision **arithmetic result**, which clears the other
    /// half of the destination rather than preserving it.
    ///
    /// This is what separates an arithmetic write-back from `MTC1`.
    /// n64-systemtest's *"Upper bits of 32 bit operation"* reads the
    /// destination back with `DMFC1` after an `ADD.S` and expects the upper
    /// 32 bits to be **zero**, not the register's previous contents.
    ///
    /// The destination index is used **as-is in both `FR` modes** — unlike
    /// [`Fpr::write_s`], which under `FR = 0` reaches an even partner. `ADD.S $1`
    /// in half mode leaves its result in FGR1, upper half cleared, which is what
    /// the suite reads back.
    pub const fn write_s_arith(&mut self, n: u8, _fr: bool, v: u32) {
        self.fgr[(n & 31) as usize] = v as u64;
    }

    /// Read the **`fs`** operand of a floating-point *arithmetic* instruction.
    ///
    /// Under `FR = 0` an odd index reads its **even partner's** value — the low
    /// bit of the field is simply dropped. This is *not* the same mapping as
    /// [`Fpr::read_s`], which models `MTC1`/`LWC1` and reaches the partner's
    /// **high** half. Two different mappings for two different instruction
    /// classes is surprising, and it is what the hardware does.
    ///
    /// # Why this is measured rather than looked up
    ///
    /// The manual declines to specify it: *"If the FR bit is 0, an odd-numbered
    /// register cannot be specified"*, and for the arithmetic instructions
    /// *"If an odd number is specified, the operation is undefined"* (UM §7.5.3,
    /// §16). Undefined in the manual is still deterministic in silicon, and
    /// n64-systemtest measures it — so the ROM's table is the oracle here, and
    /// the accuracy ledger records it as such rather than as documentation.
    #[must_use]
    pub const fn read_s_fs(&self, n: u8, fr: bool) -> u32 {
        let i = (n & 31) as usize;
        self.fgr[if fr { i } else { i & !1 }] as u32
    }

    /// Read the **`ft`** operand of a floating-point arithmetic instruction.
    ///
    /// Unlike [`Fpr::read_s_fs`], the index is used **as-is** in both `FR`
    /// modes: an odd `ft` reads the odd FGR's own low half.
    ///
    /// The asymmetry is the whole point of having two accessors. It is pinned by
    /// a pair of rows that disagree under any single rule: with `FR = 0`,
    /// `SQRT.S $13, $31` yields `sqrt(16)` — so `fs = 31` read FGR30 — while
    /// `ADD.S $2, $28, $31` yields `-10 + -16` — so `ft = 31` read FGR31. One
    /// shared mapping cannot satisfy both.
    #[must_use]
    pub const fn read_s_ft(&self, n: u8, _fr: bool) -> u32 {
        self.fgr[(n & 31) as usize] as u32
    }

    /// Read the `fs` operand of a **double**-precision arithmetic instruction.
    #[must_use]
    pub const fn read_d_fs(&self, n: u8, fr: bool) -> u64 {
        let i = (n & 31) as usize;
        self.fgr[if fr { i } else { i & !1 }]
    }

    /// Read the `ft` operand of a double-precision arithmetic instruction.
    #[must_use]
    pub const fn read_d_ft(&self, n: u8, _fr: bool) -> u64 {
        self.fgr[(n & 31) as usize]
    }

    /// Write a **double**-precision arithmetic result.
    ///
    /// Like [`Fpr::write_s_arith`], the destination index is used as-is in both
    /// modes. `ADD.D $1` under `FR = 0` leaves the result in FGR1, which
    /// n64-systemtest checks by observing that FGR1 does *not* keep its
    /// preloaded value.
    pub const fn write_d_arith(&mut self, n: u8, _fr: bool, v: u64) {
        self.fgr[(n & 31) as usize] = v;
    }

    /// Read a 64-bit value from FPR `n` under the current `FR` view.
    ///
    /// With `FR = 0` the register number is forced even and the **whole**
    /// 64-bit FGR is the value — not an assembly of two FGRs' low halves,
    /// which is the shape this originally had and which disagreed with
    /// hardware on every odd index.
    #[must_use]
    pub const fn read_d(&self, n: u8, fr: bool) -> u64 {
        let i = (n & 31) as usize;
        self.fgr[if fr { i } else { i & !1 }]
    }

    /// Write a 64-bit value to FPR `n` under the current `FR` view.
    pub const fn write_d(&mut self, n: u8, fr: bool, v: u64) {
        let i = (n & 31) as usize;
        self.fgr[if fr { i } else { i & !1 }] = v;
    }

    /// Read a raw FGR, ignoring `FR`.
    ///
    /// **Not for any instruction.** `DMFC1` looked like a user of this and is
    /// not: it is a *formatted* 64-bit access and goes through [`Fpr::read_d`]
    /// (accuracy ledger U-7). This exists for tests and for save-state
    /// serialisation, which want the physical file.
    #[must_use]
    pub const fn read_raw(&self, n: u8) -> u64 {
        self.fgr[(n & 31) as usize]
    }

    /// Write a raw FGR, ignoring `FR`. See [`Fpr::read_raw`].
    pub const fn write_raw(&mut self, n: u8, v: u64) {
        self.fgr[(n & 31) as usize] = v;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With `FR = 1`, an FPR **is** an FGR: 32 independent 64-bit registers.
    #[test]
    fn fr_set_gives_thirty_two_independent_registers() {
        let mut f = Fpr::new();
        for n in 0..32u8 {
            f.write_d(n, true, 0xDEAD_0000_0000_0000 | u64::from(n));
        }
        for n in 0..32u8 {
            assert_eq!(
                f.read_d(n, true),
                0xDEAD_0000_0000_0000 | u64::from(n),
                "FPR {n} must be independent"
            );
        }
    }

    /// **The n64-systemtest `MTC1` half-mode sequence, verbatim.**
    ///
    /// This is the vector that showed the old "FGR pair" model was wrong, so it
    /// is transcribed rather than paraphrased: writing an **odd** register in
    /// half mode lands in the **high half of its even partner**, and the odd
    /// FGR is left completely alone.
    #[test]
    fn a_half_mode_odd_index_writes_the_high_half_of_its_even_partner() {
        let mut f = Fpr::new();
        f.write_raw(0, 0x0000_1111_2222_3333);
        f.write_raw(1, 0x4444_5555_6666_7777);
        f.write_raw(2, 0x8888_9999_AAAA_BBBB);

        // MTC1 $0 in half mode -> low half of FGR0, high half preserved.
        f.write_s(0, false, 0x89AB_CDEF);
        assert_eq!(f.read_d(0, true), 0x0000_1111_89AB_CDEF);

        // MTC1 $1 in half mode -> HIGH half of FGR0. FGR1 is untouched.
        f.write_s(1, false, 0x0123_4567);
        assert_eq!(
            f.read_d(0, true),
            0x0123_4567_89AB_CDEF,
            "landed in FGR0's high half"
        );
        assert_eq!(
            f.read_d(1, true),
            0x4444_5555_6666_7777,
            "FGR1 must be untouched -- the old pair model wrote here"
        );

        // MTC1 $2 -> low half of FGR2; MTC1 $3 -> high half of FGR2.
        f.write_s(2, false, 0x1234_5678);
        assert_eq!(f.read_d(2, true), 0x8888_9999_1234_5678);
        f.write_s(3, false, 0x9ABC_DEF0);
        assert_eq!(f.read_d(3, false), 0x9ABC_DEF0_1234_5678);
    }

    /// Reading mirrors writing: an odd index in half mode reads the high half.
    #[test]
    fn a_half_mode_odd_index_reads_the_high_half() {
        let mut f = Fpr::new();
        f.write_raw(0, 0x0123_4567_89AB_CDEF);
        f.write_raw(1, 0xFFFF_FFFF_FFFF_FFFF);
        assert_eq!(f.read_s(0, false), 0x89AB_CDEF, "even -> low half");
        assert_eq!(f.read_s(1, false), 0x0123_4567, "odd -> HIGH half of FGR0");
        // ...and under FR = 1 the same index is a different register entirely.
        assert_eq!(f.read_s(1, true), 0xFFFF_FFFF, "FR=1 -> FGR1's low half");
    }

    /// A 64-bit access in half mode is the **whole** even register, not two
    /// registers' low halves assembled.
    #[test]
    fn a_half_mode_64_bit_access_is_one_whole_register() {
        let mut f = Fpr::new();
        f.write_d(2, false, 0x1122_3344_5566_7788);
        assert_eq!(f.read_raw(2), 0x1122_3344_5566_7788, "all 64 bits in FGR2");
        assert_eq!(f.read_raw(3), 0, "FGR3 is not part of it");
        assert_eq!(
            f.read_d(3, false),
            0x1122_3344_5566_7788,
            "odd aliases its partner"
        );
    }

    /// **`MTC1` preserves the other half; an arithmetic result clears it.**
    ///
    /// Both write 32 bits to the same place, so a single `write_s` for both is
    /// the natural implementation — and it is wrong. n64-systemtest reads the
    /// destination back with `DMFC1` after an `ADD.S` and expects zero above.
    #[test]
    fn an_arithmetic_write_clears_the_other_half_but_mtc1_preserves_it() {
        let mut f = Fpr::new();
        f.write_raw(4, 0xAAAA_BBBB_CCCC_DDDD);
        f.write_s(4, true, 0x1234_5678);
        assert_eq!(f.read_raw(4), 0xAAAA_BBBB_1234_5678, "MTC1 preserves");

        f.write_raw(4, 0xAAAA_BBBB_CCCC_DDDD);
        f.write_s_arith(4, true, 0x1234_5678);
        assert_eq!(f.read_raw(4), 0x0000_0000_1234_5678, "arithmetic clears");

        // Half mode, odd destination: the arithmetic result goes to the ODD
        // FGR itself, upper half cleared, and the even partner is untouched.
        //
        // This is where an arithmetic write parts company with `MTC1`, which
        // *would* reach FGR4's high half. `ADD.S $1` in half mode leaves its
        // result in FGR1 -- n64-systemtest sees FGR1 lose its preloaded value,
        // which cannot happen if the write is folded into the even partner.
        f.write_raw(4, 0xAAAA_BBBB_CCCC_DDDD);
        f.write_raw(5, 0x1111_2222_3333_4444);
        f.write_s_arith(5, false, 0x1234_5678);
        assert_eq!(f.read_raw(5), 0x0000_0000_1234_5678, "odd destination");
        assert_eq!(f.read_raw(4), 0xAAAA_BBBB_CCCC_DDDD, "partner untouched");
    }

    /// `FR = 0` addresses only even FGRs, so the odd ones are unreachable
    /// through every accessor. Pinned because the old model used them as
    /// storage.
    #[test]
    fn half_mode_never_touches_an_odd_fgr() {
        let mut f = Fpr::new();
        for n in 0..32u8 {
            f.write_raw(n, 0x5A5A_5A5A_5A5A_5A5A);
        }
        for n in 0..32u8 {
            f.write_s(n, false, 0x1111_2222);
            f.write_d(n, false, 0x3333_4444_5555_6666);
        }
        for n in (1..32u8).step_by(2) {
            assert_eq!(
                f.read_raw(n),
                0x5A5A_5A5A_5A5A_5A5A,
                "FGR {n} is odd and must be untouched in half mode"
            );
        }
    }
}
