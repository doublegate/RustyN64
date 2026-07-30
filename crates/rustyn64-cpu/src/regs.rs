//! The VR4300 register file.
//!
//! Split out so `$zero`'s hardwiring lives in exactly one place. Every read goes
//! through [`Regs::read`] and every write through [`Regs::write`], so no call
//! site can forget it — a scattered `if rd != 0` is how a write to `$zero`
//! eventually slips through and corrupts the architectural zero.

use serde::{Deserialize, Serialize};

/// MIPS general-purpose register indices, by their ABI names.
///
/// Lives here rather than beside any one caller so the numbering is stated once:
/// a second private copy elsewhere is how `$t3` and `$t4` eventually swap in one
/// of them. Only the names currently needed are defined — this is a shared
/// vocabulary, not an exhaustive table for its own sake.
pub mod gpr {
    /// `$at` — assembler temporary.
    pub const AT: u8 = 1;
    /// `$a2` — argument 2.
    pub const A2: u8 = 6;
    /// `$a3` — argument 3.
    pub const A3: u8 = 7;
    /// `$t0` — temporary 0.
    pub const T0: u8 = 8;
    /// `$t2` — temporary 2.
    pub const T2: u8 = 10;
    /// `$t3` — temporary 3.
    pub const T3: u8 = 11;
    /// `$s3` — saved 3.
    pub const S3: u8 = 19;
    /// `$s4` — saved 4.
    pub const S4: u8 = 20;
    /// `$s5` — saved 5.
    pub const S5: u8 = 21;
    /// `$s6` — saved 6.
    pub const S6: u8 = 22;
    /// `$s7` — saved 7.
    pub const S7: u8 = 23;
    /// `$sp` — stack pointer.
    pub const SP: u8 = 29;
    /// `$ra` — return address.
    pub const RA: u8 = 31;
}

/// General-purpose registers plus the `HI`/`LO` multiply-divide pair.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Regs {
    /// 32 general-purpose 64-bit registers. Index 0 is architecturally zero;
    /// prefer [`Regs::read`] / [`Regs::write`] over touching this directly.
    pub gpr: [u64; 32],
    /// `HI` — multiply high half, or division remainder.
    pub hi: u64,
    /// `LO` — multiply low half, or division quotient.
    pub lo: u64,
}

impl Default for Regs {
    fn default() -> Self {
        Self::new()
    }
}

impl Regs {
    /// Power-on state: everything zero.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            gpr: [0; 32],
            hi: 0,
            lo: 0,
        }
    }

    /// Read a general register. `$zero` always reads as 0.
    ///
    /// The index is masked to 5 bits **first**, then the `$zero` rule is applied
    /// to the *masked* value. Checking before masking would let `read(32)` fall
    /// through to `gpr[32 & 31]` — `gpr[0]` — and leak it if it were ever
    /// corrupted. This is public API and cannot assume its caller pre-masked.
    #[must_use]
    pub const fn read(&self, i: u8) -> u64 {
        let i = i & 31;
        if i == 0 { 0 } else { self.gpr[i as usize] }
    }

    /// Write a general register. A write to `$zero` is **discarded**, which is
    /// architectural, not a convenience: software relies on `$zero` staying zero
    /// after instructions that nominally target it.
    ///
    /// Masked to 5 bits **before** the `$zero` check, for the same reason as
    /// [`Regs::read`] but with worse consequences: checking first would make
    /// `write(32, v)` land in `gpr[0]` and corrupt the architectural zero — in
    /// the one function whose entire purpose is preventing exactly that.
    pub const fn write(&mut self, i: u8, v: u64) {
        let i = i & 31;
        if i != 0 {
            self.gpr[i as usize] = v;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_reads_as_zero_and_cannot_be_written() {
        let mut r = Regs::new();
        r.write(0, 0xDEAD_BEEF);
        assert_eq!(r.read(0), 0);
        assert_eq!(r.gpr[0], 0, "the raw array must be untouched too");
    }

    /// An out-of-range index must not alias `$zero`. `write(32, v)` naively
    /// masks to `gpr[0]` and corrupts the architectural zero — in the very
    /// function that exists to prevent that.
    #[test]
    fn out_of_range_indices_do_not_alias_zero() {
        let mut r = Regs::new();
        for i in [32u8, 64, 96, 128, 160, 192, 224] {
            r.write(i, 0xDEAD_BEEF);
            assert_eq!(r.gpr[0], 0, "write({i}) corrupted $zero");
            assert_eq!(r.read(i), 0, "read({i}) did not honor the $zero rule");
        }
        // A corrupted gpr[0] must still never be observable through `read`.
        r.gpr[0] = 0xBAD;
        assert_eq!(r.read(0), 0);
        assert_eq!(r.read(32), 0);
    }

    #[test]
    fn ordinary_registers_round_trip() {
        let mut r = Regs::new();
        for i in 1..32u8 {
            r.write(i, u64::from(i) * 0x1111_1111);
        }
        for i in 1..32u8 {
            assert_eq!(r.read(i), u64::from(i) * 0x1111_1111);
        }
    }
}
