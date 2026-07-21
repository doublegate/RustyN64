//! The **vector unit** — the RSP's 8-lane SIMD coprocessor (Sprint 2).
//!
//! 32 registers of 128 bits, each eight lanes of 16 bits, exposed through COP2.
//! This module currently implements the **register file and the SU/VU moves**;
//! the computational instructions, the 48-bit accumulator and the `VRCP`/`VRSQ`
//! tables are the rest of the sprint.
//!
//! # Lanes are a view over bytes, not the storage
//!
//! Every move here addresses the register by **byte offset**, not by lane, and
//! the two disagree in ways that matter: `MTC2` with an odd offset straddles two
//! lanes, and an offset of 15 wraps. Modelling the register as eight `u16`s and
//! converting at the edges is what keeps that expressible — the alternative,
//! treating a lane as the unit, silently rounds every odd offset.

use crate::Rsp;

/// The VU's three control registers (N64brew *RSP CPU Core* §Control registers).
///
/// `VCO` and `VCC` are 16 bits, `VCE` is 8. They are flag registers rather than
/// data: each instruction defines what it reads and writes, so there is no
/// useful general description of their contents.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Control {
    /// Carry / overflow, 16 bits — two flags per lane.
    pub vco: u16,
    /// Compare results, 16 bits.
    pub vcc: u16,
    /// Clip-equality, 8 bits.
    pub vce: u8,
}

/// Which control register a `CFC2`/`CTC2` names.
///
/// Only `0`, `1` and `2` are defined. The RSP has no exception mechanism, so a
/// wider index cannot fault; it is masked, which is what the encoding's two
/// usable bits already imply.
const fn control_index(vs: u32) -> u32 {
    vs & 3
}

impl Rsp {
    /// Read a byte of a vector register, big-endian within the 128 bits.
    ///
    /// Byte 0 is the most significant half of lane 0, matching the wiki's
    /// convention that byte indices count *"from the higher part of the register
    /// (in big-endian order)"*.
    #[must_use]
    pub const fn vu_byte(&self, reg: usize, byte: usize) -> u8 {
        let lane = self.vu_regs[reg & 31][(byte & 15) >> 1];
        if byte & 1 == 0 {
            (lane >> 8) as u8
        } else {
            lane as u8
        }
    }

    /// Write a byte of a vector register.
    pub const fn set_vu_byte(&mut self, reg: usize, byte: usize, val: u8) {
        let lane = &mut self.vu_regs[reg & 31][(byte & 15) >> 1];
        if byte & 1 == 0 {
            *lane = (*lane & 0x00FF) | ((val as u16) << 8);
        } else {
            *lane = (*lane & 0xFF00) | val as u16;
        }
    }

    /// `MFC2` — copy two bytes of a vector register into a GPR, sign-extended.
    ///
    /// The pair is taken at a **byte** offset, so an odd offset straddles two
    /// lanes. At offset 15 the second byte **wraps to byte 0** of the same
    /// register rather than reading past the end — a rule that is invisible
    /// until something actually addresses the last byte.
    pub fn mfc2(&mut self, rt: usize, vs: usize, elem: usize) -> u32 {
        let hi = self.vu_byte(vs, elem);
        let lo = self.vu_byte(vs, (elem + 1) & 15);
        let v = (u16::from(hi) << 8) | u16::from(lo);
        let sext = i32::from(v.cast_signed()).cast_unsigned();
        self.set_su(rt, sext);
        sext
    }

    /// `MTC2` — copy the low 16 bits of a GPR into a vector register at a byte
    /// offset.
    ///
    /// At offset 15 **only one byte is written**, taken from `rt[15..8]`: there
    /// is no byte 16 to receive the other half, and unlike `MFC2` it does not
    /// wrap around to byte 0. The asymmetry between the two is deliberate on
    /// hardware and is exactly what a lane-oriented implementation loses.
    pub const fn mtc2(&mut self, value: u32, vs: usize, elem: usize) {
        let hi = (value >> 8) as u8;
        let lo = value as u8;
        self.set_vu_byte(vs, elem, hi);
        if elem != 15 {
            self.set_vu_byte(vs, elem + 1, lo);
        }
    }

    /// `CFC2` — copy a VU control register into a GPR, sign-extended from 16
    /// bits. The element field is ignored.
    pub fn cfc2(&mut self, rt: usize, vs: u32) -> u32 {
        let v = match control_index(vs) {
            0 => self.vu_ctrl.vco,
            1 => self.vu_ctrl.vcc,
            // `VCE` is only 8 bits wide, and the read is still described as a
            // 16-bit value sign-extended to 32 -- so the byte is zero-extended
            // into the halfword first, and the halfword's sign bit is therefore
            // always clear.
            _ => u16::from(self.vu_ctrl.vce),
        };
        let sext = i32::from(v.cast_signed()).cast_unsigned();
        self.set_su(rt, sext);
        sext
    }

    /// `CTC2` — copy the low 16 bits of a GPR into a VU control register.
    pub const fn ctc2(&mut self, value: u32, vs: u32) {
        let v = value as u16;
        match control_index(vs) {
            0 => self.vu_ctrl.vco = v,
            1 => self.vu_ctrl.vcc = v,
            _ => self.vu_ctrl.vce = v as u8,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bytes are big-endian across the 128 bits: byte 0 is the top half of
    /// lane 0, byte 1 the bottom half, and so on.
    #[test]
    fn bytes_address_the_register_big_endian() {
        let mut rsp = Rsp::new();
        rsp.vu_regs[4][0] = 0xAABB;
        rsp.vu_regs[4][7] = 0x1122;
        assert_eq!(rsp.vu_byte(4, 0), 0xAA);
        assert_eq!(rsp.vu_byte(4, 1), 0xBB);
        assert_eq!(rsp.vu_byte(4, 14), 0x11);
        assert_eq!(rsp.vu_byte(4, 15), 0x22);

        rsp.set_vu_byte(4, 1, 0xCC);
        assert_eq!(rsp.vu_regs[4][0], 0xAACC, "only the low byte moved");
    }

    /// **`MTC2` takes a byte offset, so an odd one straddles two lanes.**
    ///
    /// A lane-oriented implementation rounds this to a lane and writes the
    /// wrong 16 bits — and the two agree for every *even* offset, so a test
    /// that only uses aligned offsets cannot tell them apart.
    #[test]
    fn mtc2_at_an_odd_offset_straddles_two_lanes() {
        let mut rsp = Rsp::new();
        rsp.mtc2(0x1234, 2, 1);
        assert_eq!(
            rsp.vu_regs[2][0], 0x0012,
            "high byte into lane 0's low half"
        );
        assert_eq!(
            rsp.vu_regs[2][1], 0x3400,
            "low byte into lane 1's high half"
        );
    }

    /// At byte 15 `MTC2` writes **one** byte and does not wrap.
    #[test]
    fn mtc2_at_the_last_byte_writes_only_one() {
        let mut rsp = Rsp::new();
        rsp.mtc2(0x1234, 3, 15);
        assert_eq!(rsp.vu_regs[3][7], 0x0012, "rt[15..8] into the last byte");
        assert_eq!(rsp.vu_regs[3][0], 0, "and nothing wrapped to the start");
    }

    /// **`MFC2` at byte 15 wraps its second byte to byte 0** — the asymmetry
    /// with `MTC2`, which does not.
    #[test]
    fn mfc2_at_the_last_byte_wraps_to_the_first() {
        let mut rsp = Rsp::new();
        rsp.vu_regs[5][7] = 0x00AB; // byte 15 = 0xAB
        rsp.vu_regs[5][0] = 0xCD00; // byte 0  = 0xCD
        let v = rsp.mfc2(1, 5, 15);
        assert_eq!(v, 0xFFFF_ABCD, "0xABCD, sign-extended");
        assert_eq!(rsp.su_regs[1], 0xFFFF_ABCD);
    }

    /// `MFC2` sign-extends, so a value with bit 15 set fills the upper half.
    #[test]
    fn mfc2_sign_extends_to_thirty_two_bits() {
        let mut rsp = Rsp::new();
        rsp.vu_regs[6][2] = 0x8001;
        assert_eq!(rsp.mfc2(2, 6, 4), 0xFFFF_8001);
        rsp.vu_regs[6][2] = 0x7FFF;
        assert_eq!(rsp.mfc2(2, 6, 4), 0x0000_7FFF);
    }

    /// The three control registers round-trip, and `VCE` keeps only 8 bits.
    #[test]
    fn the_control_registers_round_trip() {
        let mut rsp = Rsp::new();
        rsp.ctc2(0xFFFF_1234, 0);
        rsp.ctc2(0xFFFF_5678, 1);
        rsp.ctc2(0xFFFF_00AB, 2);
        assert_eq!(rsp.vu_ctrl.vco, 0x1234);
        assert_eq!(rsp.vu_ctrl.vcc, 0x5678);
        assert_eq!(rsp.vu_ctrl.vce, 0xAB, "VCE is 8 bits wide");

        assert_eq!(rsp.cfc2(1, 0), 0x0000_1234);
        assert_eq!(rsp.cfc2(1, 1), 0x0000_5678);
        // 0xAB zero-extends into the halfword, so the sign bit is clear and the
        // 32-bit result has no upper ones.
        assert_eq!(rsp.cfc2(1, 2), 0x0000_00AB);
    }

    /// `CFC2` sign-extends from 16 bits, which `VCO` and `VCC` can reach.
    #[test]
    fn cfc2_sign_extends_the_sixteen_bit_registers() {
        let mut rsp = Rsp::new();
        rsp.ctc2(0x8000, 0);
        assert_eq!(rsp.cfc2(1, 0), 0xFFFF_8000);
    }
}
