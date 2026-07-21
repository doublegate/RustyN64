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

/// The 48-bit-per-lane accumulator, and the computational instructions.
///
/// # The accumulator is one 48-bit register per lane, not three 16-bit ones
///
/// `VSAR` slices it into `ACC_HI` (bits 47..32), `ACC_MD` (31..16) and `ACC_LO`
/// (15..0), which invites modelling it as three separate halfwords. It is not:
/// the multiply instructions write and accumulate across the full 48 bits, and
/// the extraction that produces `vd` reads a 32-bit window *spanning* two of
/// those slices. Splitting the storage makes carries between them disappear.
impl Rsp {
    /// Broadcast-modified read of a `vt` lane (N64brew *RSP CPU Core*
    /// §Broadcast modifier).
    ///
    /// `element` 0 and 1 both mean "no broadcast" — the table lists them
    /// separately and gives them identical lane sets, so this is not a
    /// simplification.
    #[must_use]
    pub const fn vt_lane(&self, vt: usize, element: u32, lane: usize) -> u16 {
        let src = match element {
            0 | 1 => lane,
            // Quarter broadcasts: pairs share the even (2) or odd (3) lane.
            2 => lane & !1,
            3 => (lane & !1) | 1,
            // Half broadcasts: each group of four takes one lane.
            4..=7 => (lane & !3) | (element as usize - 4),
            // Single-lane broadcast across all eight.
            _ => element as usize - 8,
        };
        self.vu_regs[vt & 31][src & 7]
    }

    /// Signed clamp to `[-32768, 32767]`.
    const fn clamp_signed(v: i64) -> u16 {
        if v < -32768 {
            0x8000
        } else if v > 32767 {
            0x7FFF
        } else {
            (v as i16).cast_unsigned()
        }
    }

    /// Unsigned clamp: negatives become 0, and the saturating threshold is
    /// **15-bit** while the saturated value is 16-bit.
    ///
    /// That asymmetry is the documented rule, not a typo — anything above
    /// `0x7FFF` saturates to `0xFFFF` rather than passing through. A naive
    /// `> 65535` test lets values in the range `0x8000..=0xFFFF` through
    /// unchanged and fails the VU tests.
    const fn clamp_unsigned(v: i64) -> u16 {
        if v < 0 {
            0
        } else if v > 32767 {
            0xFFFF
        } else {
            // In range, so the truncation is exact and the sign is already
            // known non-negative.
            v.cast_unsigned() as u16
        }
    }

    /// Sign-extend the 48-bit accumulator lane to a signed 64-bit value.
    const fn acc_signed(&self, lane: usize) -> i64 {
        let v = self.vu_acc[lane] & 0xFFFF_FFFF_FFFF;
        // Bit 47 is the sign; shifting up and back propagates it.
        (v << 16).cast_signed() >> 16
    }

    /// Store a signed value back into a 48-bit accumulator lane.
    const fn set_acc(&mut self, lane: usize, v: i64) {
        self.vu_acc[lane] = v.cast_unsigned() & 0xFFFF_FFFF_FFFF;
    }

    /// The computational COP2 instructions.
    ///
    /// Returns `false` for an opcode this does not implement yet, so the caller
    /// can leave the instruction inert rather than writing a wrong result.
    pub fn vu_compute(&mut self, op: u32, element: u32, vs: usize, vt: usize, vd: usize) -> bool {
        for lane in 0..8 {
            let s = self.vu_regs[vs & 31][lane];
            let t = self.vt_lane(vt, element, lane);
            let ss = i64::from(s.cast_signed());
            let ts = i64::from(t.cast_signed());
            let su = i64::from(s);
            let tu = i64::from(t);

            let out = match op {
                // Multiply, S1.15 * S1.15, doubled and rounded. The +0x8000 is
                // the rounding constant, and it lands in the accumulator, not
                // just in the result -- the oracle reads ACC_LO back as 0x8000
                // for a zero product, which is how the constant is visible.
                0x00 | 0x01 => {
                    let acc = ss * ts * 2 + 0x8000;
                    self.set_acc(lane, acc);
                    let extracted = self.acc_signed(lane) >> 16;
                    if op == 0x00 {
                        Self::clamp_signed(extracted)
                    } else {
                        Self::clamp_unsigned(extracted)
                    }
                }
                // VMUDL: U0.16 * U0.16, keeping the high half. The product is
                // unsigned, so nothing sign-extends into the upper accumulator.
                0x04 => {
                    let acc = (su * tu) >> 16;
                    self.set_acc(lane, acc);
                    acc.cast_unsigned() as u16
                }
                // VMUDM: S0.16 * U0.16.
                0x05 => {
                    let acc = ss * tu;
                    self.set_acc(lane, acc);
                    (acc >> 16).cast_unsigned() as u16
                }
                // VMUDN: U0.16 * S0.16 -- the mirror of VMUDM, and it extracts
                // the LOW half where VMUDM takes the high one.
                0x06 => {
                    let acc = su * ts;
                    self.set_acc(lane, acc);
                    acc.cast_unsigned() as u16
                }
                // VMUDH: S0.16 * S0.16, shifted into the upper accumulator.
                0x07 => {
                    let acc = (ss * ts) << 16;
                    self.set_acc(lane, acc);
                    Self::clamp_signed(self.acc_signed(lane) >> 16)
                }
                // VSAR: read a 16-bit slice of the accumulator. The slice is
                // chosen by the *element* field, not by an operand.
                0x1D => {
                    let acc = self.vu_acc[lane];
                    match element {
                        8 => (acc >> 32) as u16,
                        9 => (acc >> 16) as u16,
                        10 => acc as u16,
                        // Any other element reads zero; the RSP has no
                        // exception to raise for an undefined selector.
                        _ => 0,
                    }
                }
                // The bitwise group. Each also writes its result into ACC_LO.
                0x28 => s & t,
                0x29 => !(s & t),
                0x2A => s | t,
                0x2B => !(s | t),
                0x2C => s ^ t,
                0x2D => !(s ^ t),
                _ => return false,
            };

            // The logical operations leave ACC_LO holding their result.
            if (0x28..=0x2D).contains(&op) {
                self.vu_acc[lane] = (self.vu_acc[lane] & 0xFFFF_FFFF_0000) | u64::from(out);
            }
            self.vu_regs[vd & 31][lane] = out;
        }
        true
    }
}

#[cfg(test)]
mod compute_tests {
    use super::*;

    /// The oracle's own input pair, from `op_vmulf.rs`.
    const VS: [u16; 8] = [
        0x0000, 0x0000, 0x0000, 0xE000, 0x8001, 0x8000, 0x7FFF, 0x8000,
    ];
    const VT: [u16; 8] = [
        0x0000, 0x0001, 0xFFFF, 0xFFFF, 0x8000, 0x7FFF, 0x7FFF, 0x8000,
    ];

    fn seeded() -> Rsp {
        let mut rsp = Rsp::new();
        rsp.vu_regs[0] = VS;
        rsp.vu_regs[1] = VT;
        rsp
    }

    fn acc_slice(rsp: &Rsp, shift: u32) -> [u16; 8] {
        core::array::from_fn(|i| (rsp.vu_acc[i] >> shift) as u16)
    }

    /// **`VMULF` against n64-systemtest's own expected vectors.**
    ///
    /// Result *and* all three accumulator slices, because the result alone
    /// cannot distinguish the rounding constant landing in the accumulator from
    /// it being applied only to the extracted value — `ACC_LO` reading back
    /// `0x8000` for a zero product is the only place that shows.
    #[test]
    fn vmulf_matches_the_oracle_vectors() {
        let mut rsp = seeded();
        assert!(rsp.vu_compute(0x00, 0, 0, 1, 2));
        assert_eq!(
            rsp.vu_regs[2],
            [0, 0, 0, 0, 0x7fff, 0x8001, 0x7ffe, 0x7fff],
            "VMULF result"
        );
        assert_eq!(acc_slice(&rsp, 32), [0, 0, 0, 0, 0, 0xffff, 0, 0], "ACC_HI");
        assert_eq!(
            acc_slice(&rsp, 16),
            [0, 0, 0, 0, 0x7fff, 0x8001, 0x7ffe, 0x8000],
            "ACC_MD"
        );
        assert_eq!(
            acc_slice(&rsp, 0),
            [
                0x8000, 0x8000, 0x8000, 0xc000, 0x8000, 0x8000, 0x8002, 0x8000
            ],
            "ACC_LO"
        );
    }

    /// The last lane is the one that pins the **clamp**: the 48-bit accumulator
    /// is positive there, so `acc >> 16` is `0x8000` = 32768, one past the
    /// signed maximum, and the result saturates to `0x7FFF`.
    #[test]
    fn vmulf_saturates_where_the_accumulator_overflows_the_result() {
        let mut rsp = seeded();
        rsp.vu_compute(0x00, 0, 0, 1, 2);
        assert_eq!(rsp.vu_acc[7] >> 16, 0x8000, "the accumulator holds 32768");
        assert_eq!(rsp.vu_regs[2][7], 0x7FFF, "and the result saturates");
    }

    /// **`VMULU` against the oracle's vectors** — the same accumulator as
    /// `VMULF`, differing *only* in the clamp.
    ///
    /// That shared path is exactly why this needs its own instruction-level
    /// case: `clamp_unsigned` being right as a helper says nothing about the
    /// `op == 0x01` arm selecting it, and a mis-selection would hide behind the
    /// `VMULF` coverage. Lane 5 is the discriminator — `VMULF` gives `0x8001`
    /// there and `VMULU` gives `0`, because the accumulator is negative and
    /// unsigned clamping floors it. Lane 7 is the other half: positive and over
    /// the 15-bit threshold, so it saturates to `0xFFFF` where `VMULF` gives
    /// `0x7FFF`.
    ///
    /// Note this test's `vs` differs from `VMULF`'s in lane 2 (`0x0010`), which
    /// is the oracle's own input — kept rather than normalised, so the expected
    /// vectors can be compared against the suite verbatim.
    #[test]
    fn vmulu_matches_the_oracle_vectors() {
        let mut rsp = Rsp::new();
        rsp.vu_regs[0] = [
            0x0000, 0x0000, 0x0010, 0xE000, 0x8001, 0x8000, 0x7FFF, 0x8000,
        ];
        rsp.vu_regs[1] = VT;
        assert!(rsp.vu_compute(0x01, 0, 0, 1, 2));
        assert_eq!(
            rsp.vu_regs[2],
            [0, 0, 0, 0, 0x7fff, 0, 0x7ffe, 0xffff],
            "VMULU result"
        );
        assert_eq!(acc_slice(&rsp, 32), [0, 0, 0, 0, 0, 0xffff, 0, 0], "ACC_HI");
        assert_eq!(
            acc_slice(&rsp, 16),
            [0, 0, 0, 0, 0x7fff, 0x8001, 0x7ffe, 0x8000],
            "ACC_MD"
        );
        assert_eq!(
            acc_slice(&rsp, 0),
            [
                0x8000, 0x8000, 0x7fe0, 0xc000, 0x8000, 0x8000, 0x8002, 0x8000
            ],
            "ACC_LO -- identical to VMULF's, since only the clamp differs"
        );
    }

    /// **`VMUDL` against the oracle's vectors** — an unsigned product keeping
    /// the high half, so nothing sign-extends into the upper accumulator.
    #[test]
    fn vmudl_matches_the_oracle_vectors() {
        let mut rsp = seeded();
        assert!(rsp.vu_compute(0x04, 0, 0, 1, 2));
        assert_eq!(
            rsp.vu_regs[2],
            [0, 0, 0, 0xdfff, 0x4000, 0x3fff, 0x3fff, 0x4000],
            "VMUDL result"
        );
        assert_eq!(acc_slice(&rsp, 32), [0; 8], "ACC_HI stays clear");
        assert_eq!(acc_slice(&rsp, 16), [0; 8], "ACC_MD too");
    }

    /// The broadcast modifier selects which `vt` lane each lane reads.
    #[test]
    fn the_broadcast_modifier_selects_lanes() {
        let rsp = seeded();
        // 0 and 1 are both "no broadcast".
        for e in [0, 1] {
            assert_eq!(
                core::array::from_fn::<u16, 8, _>(|i| rsp.vt_lane(1, e, i)),
                VT
            );
        }
        // e(0q): pairs take the even lane.
        assert_eq!(
            core::array::from_fn::<u16, 8, _>(|i| rsp.vt_lane(1, 2, i)),
            [VT[0], VT[0], VT[2], VT[2], VT[4], VT[4], VT[6], VT[6]]
        );
        // e(2h): each group of four takes lane 2 or 6.
        assert_eq!(
            core::array::from_fn::<u16, 8, _>(|i| rsp.vt_lane(1, 6, i)),
            [VT[2], VT[2], VT[2], VT[2], VT[6], VT[6], VT[6], VT[6]]
        );
        // e(5): lane 5 everywhere.
        assert_eq!(
            core::array::from_fn::<u16, 8, _>(|i| rsp.vt_lane(1, 13, i)),
            [VT[5]; 8]
        );
    }

    /// `VSAR` reads back the slice the element field names.
    #[test]
    fn vsar_reads_the_accumulator_slices() {
        let mut rsp = seeded();
        rsp.vu_compute(0x00, 0, 0, 1, 2);
        let hi = acc_slice(&rsp, 32);
        let md = acc_slice(&rsp, 16);
        let lo = acc_slice(&rsp, 0);

        rsp.vu_compute(0x1D, 8, 0, 0, 3);
        assert_eq!(rsp.vu_regs[3], hi, "element 8 = ACC_HI");
        rsp.vu_compute(0x1D, 9, 0, 0, 4);
        assert_eq!(rsp.vu_regs[4], md, "element 9 = ACC_MD");
        rsp.vu_compute(0x1D, 10, 0, 0, 5);
        assert_eq!(rsp.vu_regs[5], lo, "element 10 = ACC_LO");
    }

    /// **Unsigned clamping saturates at a 15-bit threshold to a 16-bit value.**
    ///
    /// A naive `> 65535` test lets `0x8000..=0xFFFF` through unchanged; the rule
    /// is that anything above `0x7FFF` becomes `0xFFFF`.
    #[test]
    fn unsigned_clamping_uses_a_fifteen_bit_threshold() {
        assert_eq!(Rsp::clamp_unsigned(-1), 0);
        assert_eq!(Rsp::clamp_unsigned(0x7FFF), 0x7FFF);
        assert_eq!(Rsp::clamp_unsigned(0x8000), 0xFFFF, "not 0x8000");
        assert_eq!(Rsp::clamp_unsigned(0xFFFF), 0xFFFF);
    }

    /// The bitwise group computes and leaves its result in `ACC_LO`.
    #[test]
    fn the_bitwise_group_writes_the_accumulator_low_slice() {
        let mut rsp = seeded();
        assert!(rsp.vu_compute(0x28, 0, 0, 1, 2)); // VAND
        assert_eq!(rsp.vu_regs[2][3], VS[3] & VT[3]);
        assert_eq!(acc_slice(&rsp, 0)[3], VS[3] & VT[3], "ACC_LO follows");

        assert!(rsp.vu_compute(0x29, 0, 0, 1, 3)); // VNAND
        assert_eq!(rsp.vu_regs[3][3], !(VS[3] & VT[3]));
    }

    /// An unimplemented opcode reports so, rather than writing a wrong result.
    #[test]
    fn an_unimplemented_opcode_is_reported_not_guessed() {
        let mut rsp = seeded();
        assert!(!rsp.vu_compute(0x08, 0, 0, 1, 2), "VMACF is not in yet");
        assert_eq!(rsp.vu_regs[2], [0; 8], "and it wrote nothing");
    }
}

/// The vector load/store family (Sprint 3, brought forward).
///
/// Encoding: `LWC2`/`SWC2` | `base` (25..21) | `vt` (20..16) | `opcode`
/// (15..11) | `element` (10..7) | `offset` (6..0, **signed 7-bit**).
///
/// The offset is scaled by the access size, and `element` is a **byte** index
/// into the vector register naming the first byte the operation touches — so a
/// non-zero element means *fewer* bytes move, not a shifted window.
impl Rsp {
    /// Sign-extend the 7-bit offset field.
    const fn sext7(offset: u32) -> i32 {
        (offset & 0x7F).cast_signed() << 25 >> 25
    }

    /// Execute a vector load or store. Returns `false` for an opcode not
    /// implemented yet, leaving the instruction inert.
    pub fn vector_mem(
        &mut self,
        store: bool,
        op: u32,
        base: usize,
        vt: usize,
        element: usize,
        offset: u32,
    ) -> bool {
        let rs = self.r(base);
        match op {
            // Scalar group: 1, 2, 4 or 8 bytes, the size doubling with the
            // opcode. The offset scales by that same size.
            0x00..=0x03 => {
                let size = 1usize << op;
                let addr = rs.wrapping_add_signed(Self::sext7(offset) * size.cast_signed() as i32);
                for i in 0..size {
                    let byte = (element + i) & 15;
                    let at = addr.wrapping_add(i as u32);
                    if store {
                        let v = self.vu_byte(vt, byte);
                        self.dmem_write_pub(at, v);
                    } else {
                        let v = self.dmem_read_pub(at);
                        self.set_vu_byte(vt, byte, v);
                    }
                }
                true
            }
            // `LQV`/`SQV`: up to 16 bytes, **left-aligned** — the transfer runs
            // from the address up to (and including) the last byte before the
            // next 16-byte boundary, so a misaligned address moves fewer bytes
            // rather than crossing the boundary.
            0x04 => {
                let addr = rs.wrapping_add_signed(Self::sext7(offset) * 16);
                let end = addr | 15;
                let size = core::cmp::min(end - addr, 15 - element as u32);
                for i in 0..=size {
                    let byte = (element + i as usize) & 15;
                    let at = addr.wrapping_add(i);
                    if store {
                        let v = self.vu_byte(vt, byte);
                        self.dmem_write_pub(at, v);
                    } else {
                        let v = self.dmem_read_pub(at);
                        self.set_vu_byte(vt, byte, v);
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// DMEM byte read, for the vector memory paths.
    pub(crate) const fn dmem_read_pub(&self, addr: u32) -> u8 {
        self.dmem[(addr & 0xFFF) as usize]
    }

    /// DMEM byte write, for the vector memory paths.
    pub(crate) const fn dmem_write_pub(&mut self, addr: u32, val: u8) {
        self.dmem[(addr & 0xFFF) as usize] = val;
    }
}

#[cfg(test)]
mod mem_tests {
    use super::*;

    fn with_dmem(pattern: &[u8]) -> Rsp {
        let mut rsp = Rsp::new();
        for (i, b) in pattern.iter().enumerate() {
            rsp.dmem[i] = *b;
        }
        rsp
    }

    /// **An aligned `LQV` fills the whole register.**
    #[test]
    fn an_aligned_lqv_loads_sixteen_bytes() {
        let bytes: [u8; 16] = core::array::from_fn(|i| i as u8 + 0x10);
        let mut rsp = with_dmem(&bytes);
        assert!(rsp.vector_mem(false, 0x04, 0, 1, 0, 0));
        for (i, want) in bytes.iter().enumerate() {
            assert_eq!(rsp.vu_byte(1, i), *want, "byte {i}");
        }
    }

    /// **A misaligned `LQV` stops at the 16-byte boundary rather than crossing
    /// it.** This is the whole reason `LRV` exists, and an implementation that
    /// simply reads 16 bytes from the address passes an aligned test and fails
    /// here.
    #[test]
    fn a_misaligned_lqv_stops_at_the_boundary() {
        let bytes: [u8; 32] = core::array::from_fn(|i| i as u8 + 0x10);
        let mut rsp = with_dmem(&bytes);
        // Address 0x08: eight bytes to the boundary at 0x10.
        rsp.set_su(2, 8);
        assert!(rsp.vector_mem(false, 0x04, 2, 1, 0, 0));
        for i in 0..8 {
            assert_eq!(rsp.vu_byte(1, i), bytes[8 + i], "loaded byte {i}");
        }
        for i in 8..16 {
            assert_eq!(rsp.vu_byte(1, i), 0, "byte {i} must be untouched");
        }
    }

    /// A non-zero `element` moves **fewer** bytes: the window is
    /// `VPR[element..15]`, not a shifted 16.
    #[test]
    fn a_non_zero_element_shortens_the_transfer() {
        let bytes: [u8; 16] = core::array::from_fn(|i| i as u8 + 0x10);
        let mut rsp = with_dmem(&bytes);
        assert!(rsp.vector_mem(false, 0x04, 0, 1, 12, 0));
        for i in 0..12 {
            assert_eq!(rsp.vu_byte(1, i), 0, "below the element, untouched");
        }
        for i in 12..16 {
            assert_eq!(rsp.vu_byte(1, i), bytes[i - 12], "from the start of DMEM");
        }
    }

    /// `SQV` is the mirror: the register's bytes land in DMEM.
    #[test]
    fn sqv_round_trips_with_lqv() {
        let bytes: [u8; 16] = core::array::from_fn(|i| i as u8 + 0xA0);
        let mut rsp = with_dmem(&bytes);
        rsp.vector_mem(false, 0x04, 0, 1, 0, 0);
        rsp.set_su(2, 0x100);
        assert!(rsp.vector_mem(true, 0x04, 2, 1, 0, 0));
        for (i, want) in bytes.iter().enumerate() {
            assert_eq!(rsp.dmem[0x100 + i], *want, "stored byte {i}");
        }
    }

    /// The scalar group's offset scales by the access size, which is what makes
    /// `LDV`'s reach eight times `LBV`'s for the same encoded offset.
    #[test]
    fn the_scalar_offset_scales_with_the_access_size() {
        let bytes: [u8; 64] = core::array::from_fn(|i| i as u8);
        let mut rsp = with_dmem(&bytes);
        // LBV, offset 2 -> address 2.
        assert!(rsp.vector_mem(false, 0x00, 0, 1, 0, 2));
        assert_eq!(rsp.vu_byte(1, 0), 2);
        // LDV, offset 2 -> address 16.
        assert!(rsp.vector_mem(false, 0x03, 0, 2, 0, 2));
        assert_eq!(rsp.vu_byte(2, 0), 16);
    }

    /// An unimplemented opcode reports so rather than moving wrong bytes.
    #[test]
    fn an_unimplemented_vector_memory_op_is_reported() {
        let mut rsp = Rsp::new();
        assert!(!rsp.vector_mem(false, 0x05, 0, 1, 0, 0), "LRV not in yet");
    }
}
