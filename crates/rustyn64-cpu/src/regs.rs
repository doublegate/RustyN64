//! The VR4300 register file.
//!
//! Split out so `$zero`'s hardwiring lives in exactly one place. Every read goes
//! through [`Regs::read`] and every write through [`Regs::write`], so no call
//! site can forget it — a scattered `if rd != 0` is how a write to `$zero`
//! eventually slips through and corrupts the architectural zero.

/// General-purpose registers plus the `HI`/`LO` multiply-divide pair.
#[derive(Clone, Debug, Eq, PartialEq)]
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
    #[must_use]
    pub const fn read(&self, i: u8) -> u64 {
        if i == 0 {
            0
        } else {
            self.gpr[(i & 31) as usize]
        }
    }

    /// Write a general register. A write to `$zero` is **discarded**, which is
    /// architectural, not a convenience: software relies on `$zero` staying zero
    /// after instructions that nominally target it.
    pub const fn write(&mut self, i: u8, v: u64) {
        if i != 0 {
            self.gpr[(i & 31) as usize] = v;
        }
    }
}
