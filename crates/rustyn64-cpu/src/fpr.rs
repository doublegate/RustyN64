//! The floating-point register file (T-13-001).
//!
//! 32 physical 64-bit **FGRs**. How software sees them depends on
//! `Status.FR` (UM §6.3.5, and the FPR/FGR figures in Ch. 7):
//!
//! The view applies to **64-bit accesses only**:
//!
//! | `FR` | 64-bit (double / `DMxC1`) view |
//! | --- | --- |
//! | 1 | FPR *n* **is** FGR *n* — 32 independent 64-bit registers |
//! | 0 | 64-bit values use **even** indices; the value is the FGR **pair** `FGR[n+1]:FGR[n]` |
//!
//! **Single precision is unaffected**: all 32 indices are valid under both
//! settings, and a `.S` value is always the low 32 bits of FGR *n*. `FR = 0`
//! does not make half the register file disappear — it changes how a *double*
//! is laid out across it.
//!
//! # Why this indirection is not optional
//!
//! It is tempting to store 32 `u64`s and index them directly, which is exactly
//! right for `FR = 1` and silently wrong for `FR = 0` — where a double written
//! to FPR 2 must land in FGRs 2 **and** 3, and reading FPR 2 back through a
//! direct index returns only half of it. Both modes occur on real N64 software:
//! IPL3 leaves `FR` set, but plenty of games clear it.
//!
//! Single-precision is the same in both modes — the low 32 bits of FGR *n* —
//! which is what makes the bug survive casual testing: every `.S` operation
//! works, and only doubles break.

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

    /// Read a single-precision value — the low 32 bits of FGR *n*.
    ///
    /// Identical under both `FR` settings, which is precisely why an
    /// `FR`-ignoring implementation passes every single-precision test.
    #[must_use]
    pub const fn read_s(&self, n: u8) -> u32 {
        self.fgr[(n & 31) as usize] as u32
    }

    /// Write a single-precision value into the low 32 bits of FGR *n*.
    ///
    /// The upper half is **preserved**, not cleared: with `FR = 0` that half is
    /// the other word of a double held in the same pair, and clearing it would
    /// corrupt a value the program never touched.
    pub const fn write_s(&mut self, n: u8, v: u32) {
        let i = (n & 31) as usize;
        self.fgr[i] = (self.fgr[i] & 0xFFFF_FFFF_0000_0000) | v as u64;
    }

    /// Read a double-precision value, applying the `FR` view.
    ///
    /// With `FR = 0` the register number is forced even and the value is
    /// assembled from the pair — *"the odd register holds the high-order word"*.
    #[must_use]
    pub const fn read_d(&self, n: u8, fr: bool) -> u64 {
        let i = (n & 31) as usize;
        if fr {
            self.fgr[i]
        } else {
            // FR = 0: 64-bit accesses use even indices. An odd `n` here is
            // architecturally *undefined* (UM Ch. 17) -- forcing it even is a
            // documented choice, not a fact. Single-precision access to odd
            // registers remains perfectly valid; only this 64-bit path is
            // constrained.
            let even = i & !1;
            ((self.fgr[even | 1] & 0xFFFF_FFFF) << 32) | (self.fgr[even] & 0xFFFF_FFFF)
        }
    }

    /// Write a double-precision value, applying the `FR` view.
    pub const fn write_d(&mut self, n: u8, fr: bool, v: u64) {
        let i = (n & 31) as usize;
        if fr {
            self.fgr[i] = v;
        } else {
            let even = i & !1;
            self.fgr[even] = (self.fgr[even] & 0xFFFF_FFFF_0000_0000) | (v & 0xFFFF_FFFF);
            self.fgr[even | 1] =
                (self.fgr[even | 1] & 0xFFFF_FFFF_0000_0000) | ((v >> 32) & 0xFFFF_FFFF);
        }
    }

    /// Read a raw FGR — for `DMFC1`, which moves the physical register rather
    /// than a formatted value, and so ignores `FR`.
    #[must_use]
    pub const fn read_raw(&self, n: u8) -> u64 {
        self.fgr[(n & 31) as usize]
    }

    /// Write a raw FGR — for `DMTC1`.
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

    /// **With `FR = 0` a double occupies an FGR pair.** Writing FPR 2 must land
    /// in FGRs 2 *and* 3, with the odd register holding the high word.
    ///
    /// A direct-index implementation stores the whole 64 bits in FGR 2 and reads
    /// it back correctly — so this only fails once something *else* observes the
    /// pair, which is why the assertion is on the raw FGRs.
    #[test]
    fn fr_clear_splits_a_double_across_an_fgr_pair() {
        let mut f = Fpr::new();
        f.write_d(2, false, 0x1122_3344_5566_7788);
        assert_eq!(f.read_raw(2), 0x5566_7788, "even FGR holds the LOW word");
        assert_eq!(f.read_raw(3), 0x1122_3344, "odd FGR holds the HIGH word");
        assert_eq!(f.read_d(2, false), 0x1122_3344_5566_7788, "and reassembles");
    }

    /// With `FR = 0` the two views disagree, which is the whole point: reading
    /// the same register as a double under `FR = 1` sees only the low half.
    #[test]
    fn the_two_views_of_the_same_storage_differ() {
        let mut f = Fpr::new();
        f.write_d(2, false, 0x1122_3344_5566_7788);
        assert_ne!(
            f.read_d(2, true),
            f.read_d(2, false),
            "an FR-ignoring implementation would make these equal"
        );
        assert_eq!(f.read_d(2, true), 0x5566_7788, "FR=1 sees FGR 2 alone");
    }

    /// Single precision is **identical under both settings** — the low 32 bits
    /// of FGR *n*. This is why an `FR`-ignoring implementation passes every
    /// single-precision test and only breaks on doubles.
    #[test]
    fn single_precision_is_the_same_under_both_fr_settings() {
        let mut f = Fpr::new();
        for n in 0..32u8 {
            f.write_s(n, 0x4000_0000 | u32::from(n));
        }
        for n in 0..32u8 {
            assert_eq!(f.read_s(n), 0x4000_0000 | u32::from(n));
        }
    }

    /// A single-precision write **preserves** the upper half. With `FR = 0` that
    /// half is the other word of a double in the same pair — clearing it would
    /// corrupt a value the program never touched.
    #[test]
    fn a_single_write_preserves_the_upper_half_of_the_fgr() {
        let mut f = Fpr::new();
        f.write_raw(4, 0xAAAA_BBBB_CCCC_DDDD);
        f.write_s(4, 0x1234_5678);
        assert_eq!(
            f.read_raw(4),
            0xAAAA_BBBB_1234_5678,
            "the upper half must survive"
        );
    }

    /// For **64-bit** accesses, `FR = 0` forces the register number even, so a
    /// double at FPR 3 aliases FPR 2. That is a **documented choice** for an
    /// architecturally undefined case (UM Ch. 17), not a hardware fact — this
    /// test pins the choice so changing it is deliberate.
    ///
    /// Single-precision access to odd registers is unaffected and valid.
    #[test]
    fn fr_clear_forces_the_register_even_by_choice_not_by_evidence() {
        let mut f = Fpr::new();
        f.write_d(2, false, 0x1122_3344_5566_7788);
        assert_eq!(
            f.read_d(3, false),
            f.read_d(2, false),
            "odd FPR aliases its even partner -- see the doc comment"
        );
    }
}
