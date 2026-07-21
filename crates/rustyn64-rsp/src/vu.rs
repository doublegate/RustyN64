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

/// The reciprocal unit's staging latches.
///
/// `VRCP`/`VRSQ` take a 16-bit operand, but the two-instruction `VRCPH`+`VRCPL`
/// sequence feeds them a **32-bit** one: the `H` instruction latches the high
/// half into `input` and sets `pending`, and the following `L` sees it and
/// combines. `pending` is what distinguishes "a high half was just staged" from
/// "there is a stale value in the latch" — without it, an `L` instruction issued
/// on its own would silently consume whatever the last `H` left behind.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Divide {
    /// The high half staged by a `VRCPH`/`VRSQH`.
    pub input: u16,
    /// The high half of the last result, which `VRCPH`/`VRSQH` reads back.
    pub output: u16,
    /// Whether `input` was staged by the immediately preceding instruction.
    pub pending: bool,
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

    /// The extraction `VMADL` and `VMADN` use, which is **not** either clamp.
    ///
    /// When `acc >> 16` fits in a signed 16-bit value the result is the
    /// accumulator's *low* slice, untouched. When it does not, the result
    /// saturates to `0x0000` or `0xFFFF` by the sign. So the low bits are
    /// returned or discarded wholesale depending on a test applied to a
    /// different part of the accumulator -- which is why neither
    /// `clamp_signed` nor `clamp_unsigned` expresses it, and why reusing one of
    /// them looks right for small values and fails at the boundary.
    ///
    /// Derived from n64-systemtest's vectors: `VMADL` lane 4 leaves
    /// `acc = 0x0000_7FFF_C000` and yields `0xC000` (the low slice), while lane
    /// 7 leaves `0x0000_8000_C000` -- one step further -- and yields `0xFFFF`.
    const fn extract_low(acc: i64) -> u16 {
        let mid = acc >> 16;
        if mid > 32767 {
            0xFFFF
        } else if mid < -32768 {
            0
        } else {
            (acc.cast_unsigned() & 0xFFFF) as u16
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
                // The accumulating forms. Each adds the same product its
                // VMUL/VMUD counterpart *sets* -- with one difference that is
                // easy to miss: VMACF adds `2 * vs * vt` with **no rounding
                // constant**, where VMULF adds `+ 0x8000`. The oracle shows it
                // directly: lane 3 moves the accumulator from 0xC000 to
                // 0x1_0000, a delta of exactly 0x4000 = 2*8192.
                0x08 | 0x09 => {
                    let acc = self.acc_signed(lane) + ss * ts * 2;
                    self.set_acc(lane, acc);
                    let extracted = self.acc_signed(lane) >> 16;
                    if op == 0x08 {
                        Self::clamp_signed(extracted)
                    } else {
                        Self::clamp_unsigned(extracted)
                    }
                }
                // VMADL: accumulate VMUDL's product, extract the low slice.
                0x0C => {
                    let acc = self.acc_signed(lane) + ((su * tu) >> 16);
                    self.set_acc(lane, acc);
                    Self::extract_low(self.acc_signed(lane))
                }
                // VMADM: accumulate VMUDM's product. Unlike VMUDM this one
                // CLAMPS the extracted middle rather than truncating it.
                0x0D => {
                    let acc = self.acc_signed(lane) + ss * tu;
                    self.set_acc(lane, acc);
                    Self::clamp_signed(self.acc_signed(lane) >> 16)
                }
                // VMADN: accumulate VMUDN's product, extract the low slice.
                0x0E => {
                    let acc = self.acc_signed(lane) + su * ts;
                    self.set_acc(lane, acc);
                    Self::extract_low(self.acc_signed(lane))
                }
                // VMADH: accumulate VMUDH's product.
                0x0F => {
                    let acc = self.acc_signed(lane) + ((ss * ts) << 16);
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

    /// The oracle's own input pair, named as **it** names them.
    ///
    /// n64-systemtest loads these into `$v0` and `$v1` and then assembles e.g.
    /// `write_vmulf(V2, V0, V1)` — whose signature is **`(vd, vt, vs)`**, not
    /// the `(vd, vs, vt)` it reads like. So `$v1` is the instruction's `vs` and
    /// `$v0` is its `vt`.
    ///
    /// Getting that backwards is invisible for every *symmetric* multiply —
    /// `VMULF`, `VMACF`, `VMUDH`, `VMUDL` all commute — and shows up only on
    /// `VMUDM`/`VMADM` and `VMUDN`/`VMADN`, where one operand is read signed
    /// and the other unsigned. Naming the constants after the registers rather
    /// than after the operand roles is what keeps the distinction visible here.
    const V0: [u16; 8] = [
        0x0000, 0x0000, 0x0000, 0xE000, 0x8001, 0x8000, 0x7FFF, 0x8000,
    ];
    const V1: [u16; 8] = [
        0x0000, 0x0001, 0xFFFF, 0xFFFF, 0x8000, 0x7FFF, 0x7FFF, 0x8000,
    ];
    /// `vs` is `$v1` and `vt` is `$v0`; see [`V0`].
    const VS_REG: usize = 1;
    const VT_REG: usize = 0;

    fn seeded() -> Rsp {
        let mut rsp = Rsp::new();
        rsp.vu_regs[0] = V0;
        rsp.vu_regs[1] = V1;
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
        assert!(rsp.vu_compute(0x00, 0, VS_REG, VT_REG, 2));
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
        rsp.vu_compute(0x00, 0, VS_REG, VT_REG, 2);
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
        rsp.vu_regs[1] = V1;
        assert!(rsp.vu_compute(0x01, 0, VS_REG, VT_REG, 2));
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
        assert!(rsp.vu_compute(0x04, 0, VS_REG, VT_REG, 2));
        assert_eq!(
            rsp.vu_regs[2],
            [0, 0, 0, 0xdfff, 0x4000, 0x3fff, 0x3fff, 0x4000],
            "VMUDL result"
        );
        assert_eq!(acc_slice(&rsp, 32), [0; 8], "ACC_HI stays clear");
        assert_eq!(acc_slice(&rsp, 16), [0; 8], "ACC_MD too");
    }

    /// **The six accumulating forms, against n64-systemtest's vectors.**
    ///
    /// The suite primes the accumulator with a `VMULF` and *then* runs the
    /// accumulating instruction, so these reproduce that exactly — the whole
    /// point of the family is what it adds to an existing accumulator, and a
    /// test starting from zero would pass for an implementation that ignored
    /// the previous contents entirely.
    #[test]
    fn the_accumulating_forms_match_the_oracle_vectors() {
        /// `VMADN`'s test uses a **different** `$v0` from the other five —
        /// checked against each file rather than assumed, after taking the
        /// shared vector on faith produced a mismatch that looked like a bug in
        /// the instruction.
        const VMADN_V0: [u16; 8] = [
            0x0000, 0x8000, 0xFFFF, 0x8000, 0x8001, 0x8000, 0x7FFF, 0x8000,
        ];

        /// One accumulating case, transcribed from the matching `op_*.rs`.
        struct Case {
            op: u32,
            name: &'static str,
            v0: [u16; 8],
            result: [u16; 8],
            hi: [u16; 8],
            md: [u16; 8],
            lo: [u16; 8],
        }

        let cases = [
            Case {
                op: 0x08,
                name: "VMACF",
                v0: V0,
                result: [0, 0, 0, 0x1, 0x7fff, 0x8000, 0x7fff, 0x7fff],
                hi: [0, 0, 0, 0, 0, 0xffff, 0, 1],
                md: [0, 0, 0, 1, 0xfffe, 2, 0xfffc, 0],
                lo: [0x8000, 0x8000, 0x8000, 0, 0x8000, 0x8000, 0x8004, 0x8000],
            },
            Case {
                op: 0x09,
                name: "VMACU",
                v0: V0,
                result: [0, 0, 0, 1, 0xffff, 0, 0xffff, 0xffff],
                hi: [0, 0, 0, 0, 0, 0xffff, 0, 1],
                md: [0, 0, 0, 1, 0xfffe, 2, 0xfffc, 0],
                lo: [0x8000, 0x8000, 0x8000, 0, 0x8000, 0x8000, 0x8004, 0x8000],
            },
            Case {
                op: 0x0C,
                name: "VMADL",
                v0: V0,
                result: [
                    0x8000, 0x8000, 0x8000, 0x9fff, 0xc000, 0xbfff, 0xc001, 0xffff,
                ],
                hi: [0, 0, 0, 0, 0, 0xffff, 0, 0],
                md: [0, 0, 0, 1, 0x7fff, 0x8001, 0x7ffe, 0x8000],
                lo: [
                    0x8000, 0x8000, 0x8000, 0x9fff, 0xc000, 0xbfff, 0xc001, 0xc000,
                ],
            },
            Case {
                op: 0x0D,
                name: "VMADM",
                v0: V0,
                result: [0, 0, 0, 0xffff, 0x3fff, 0xc001, 0x7fff, 0x4000],
                hi: [0, 0, 0, 0xffff, 0, 0xffff, 0, 0],
                md: [0, 0, 0, 0xffff, 0x3fff, 0xc001, 0xbffd, 0x4000],
                lo: [0x8000, 0x8000, 0x8000, 0xe000, 0, 0, 0x8003, 0x8000],
            },
            Case {
                op: 0x0E,
                name: "VMADN",
                v0: VMADN_V0,
                result: [0x8000, 0, 0x8003, 0, 0, 0, 0xffff, 0x8000],
                hi: [0, 0xffff, 0xffff, 0xffff, 0, 0xffff, 0, 0],
                md: [0, 0xffff, 0xffff, 0x8002, 0x4000, 0x4002, 0xbffd, 0x4000],
                lo: [0x8000, 0, 0x8003, 0, 0, 0, 0x8003, 0x8000],
            },
            Case {
                op: 0x0F,
                name: "VMADH",
                v0: V0,
                result: [0, 0, 0, 0x2000, 0x7fff, 0x8000, 0x7fff, 0x7fff],
                hi: [0, 0, 0, 0, 0x3fff, 0xc000, 0x3fff, 0x4000],
                md: [0, 0, 0, 0x2000, 0xffff, 1, 0x7fff, 0x8000],
                lo: [
                    0x8000, 0x8000, 0x8000, 0xc000, 0x8000, 0x8000, 0x8002, 0x8000,
                ],
            },
        ];

        for Case {
            op,
            name,
            v0,
            result,
            hi,
            md,
            lo,
        } in cases
        {
            let mut rsp = seeded();
            rsp.vu_regs[0] = v0;
            // Prime the accumulator exactly as the suite does.
            assert!(
                rsp.vu_compute(0x00, 0, VS_REG, VT_REG, 2),
                "{name}: priming VMULF"
            );
            assert!(
                rsp.vu_compute(op, 0, VS_REG, VT_REG, 2),
                "{name} is implemented"
            );
            assert_eq!(rsp.vu_regs[2], result, "{name} result");
            assert_eq!(acc_slice(&rsp, 32), hi, "{name} ACC_HI");
            assert_eq!(acc_slice(&rsp, 16), md, "{name} ACC_MD");
            assert_eq!(acc_slice(&rsp, 0), lo, "{name} ACC_LO");
        }
    }

    /// **`VMACF` adds no rounding constant, where `VMULF` adds `0x8000`.**
    ///
    /// The single most confusable difference in the family, and the accumulator
    /// is the only place it shows: lane 3 moves from `0xC000` to `0x1_0000`, a
    /// delta of exactly `0x4000` = 2 x 8192. An implementation that reused
    /// `VMULF`'s expression would land on `0x1_8000`.
    #[test]
    fn vmacf_adds_no_rounding_constant() {
        let mut rsp = seeded();
        rsp.vu_compute(0x00, 0, VS_REG, VT_REG, 2);
        assert_eq!(rsp.vu_acc[3], 0xC000, "after the priming VMULF");
        rsp.vu_compute(0x08, 0, VS_REG, VT_REG, 2);
        assert_eq!(
            rsp.vu_acc[3], 0x1_0000,
            "delta is 2*vs*vt exactly, with no 0x8000 added"
        );
    }

    /// The broadcast modifier selects which `vt` lane each lane reads.
    #[test]
    fn the_broadcast_modifier_selects_lanes() {
        let rsp = seeded();
        // 0 and 1 are both "no broadcast".
        for e in [0, 1] {
            assert_eq!(
                core::array::from_fn::<u16, 8, _>(|i| rsp.vt_lane(1, e, i)),
                V1
            );
        }
        // e(0q): pairs take the even lane.
        assert_eq!(
            core::array::from_fn::<u16, 8, _>(|i| rsp.vt_lane(1, 2, i)),
            [V1[0], V1[0], V1[2], V1[2], V1[4], V1[4], V1[6], V1[6]]
        );
        // e(2h): each group of four takes lane 2 or 6.
        assert_eq!(
            core::array::from_fn::<u16, 8, _>(|i| rsp.vt_lane(1, 6, i)),
            [V1[2], V1[2], V1[2], V1[2], V1[6], V1[6], V1[6], V1[6]]
        );
        // e(5): lane 5 everywhere.
        assert_eq!(
            core::array::from_fn::<u16, 8, _>(|i| rsp.vt_lane(1, 13, i)),
            [V1[5]; 8]
        );
    }

    /// `VSAR` reads back the slice the element field names.
    #[test]
    fn vsar_reads_the_accumulator_slices() {
        let mut rsp = seeded();
        rsp.vu_compute(0x00, 0, VS_REG, VT_REG, 2);
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
        assert!(rsp.vu_compute(0x28, 0, VS_REG, VT_REG, 2)); // VAND
        assert_eq!(rsp.vu_regs[2][3], V1[3] & V0[3]);
        assert_eq!(acc_slice(&rsp, 0)[3], V1[3] & V0[3], "ACC_LO follows");

        assert!(rsp.vu_compute(0x29, 0, VS_REG, VT_REG, 3)); // VNAND
        assert_eq!(rsp.vu_regs[3][3], !(V1[3] & V0[3]));
    }

    /// An unimplemented opcode reports so, rather than writing a wrong result.
    ///
    /// `0x3F` is chosen because it is genuinely unassigned — an opcode from the
    /// not-yet-implemented list would silently turn this test into a no-op the
    /// day it lands, which is what happened when it named `VMACF`.
    #[test]
    fn an_unimplemented_opcode_is_reported_not_guessed() {
        let mut rsp = seeded();
        assert!(
            !rsp.vu_compute(0x3F, 0, VS_REG, VT_REG, 2),
            "an opcode with no implementation reports rather than guessing"
        );
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
                // **The register side does not wrap.** n64-systemtest states it
                // outright: "the element specifier specifies the starting
                // element. If there isn't enough room after e, there is no
                // wrap-around but the number of bytes loaded is reduced."
                //
                // Masking the byte index with 15 instead -- the obvious reading
                // of a 16-byte register -- silently wraps back to byte 0 and
                // corrupts the far end of the vector. The DMEM side *does* wrap,
                // and only for this group: "only three instructions can
                // overflow ...: LSV, LLV, LDV".
                let size = core::cmp::min(size, 16 - element);
                for i in 0..size {
                    let byte = element + i;
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
            // `LRV`/`SRV`: the **right-aligned** partner of `LQV`, and the
            // reason a misaligned 128-bit access needs two instructions.
            //
            // The transfer runs from the *previous* 16-byte boundary up to (and
            // excluding) the address, and it lands at the *far end* of the
            // register: with 8 bytes to move they go to `VPR[8..15]`, not
            // `VPR[0..7]`. The element field then shortens it from the front on
            // the DMEM side while moving the destination up — the wiki's own
            // worked example has `e(2)` read bytes `0x10..0x13` into
            // `VPR[12..15]`, which pins both halves of that at once.
            0x05 => {
                let end = rs.wrapping_add_signed(Self::sext7(offset) * 16);
                let addr = end & !15;
                let n = (end & 15) as usize;
                if element < n {
                    let count = n - element;
                    let dest_base = 16 - n + element;
                    for i in 0..count {
                        let byte = dest_base + i;
                        let at = addr.wrapping_add(i as u32);
                        if store {
                            let v = self.vu_byte(vt, byte);
                            self.dmem_write_pub(at, v);
                        } else {
                            let v = self.dmem_read_pub(at);
                            self.set_vu_byte(vt, byte, v);
                        }
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

    /// **`LRV` lands at the far end of the register, not the near one.**
    ///
    /// The wiki's worked example, reproduced exactly: with `a0` 16-byte aligned,
    /// `lrv $v0, 0x18(a0)` reads bytes `0x10..0x17` into `VPR[8..15]`. An
    /// implementation that writes from byte 0 — the natural mirror of `LQV` —
    /// puts the right-hand half of the vector in the left-hand slots, and the
    /// pair no longer reconstructs a misaligned 128-bit load.
    #[test]
    fn lrv_loads_into_the_far_end_of_the_register() {
        let bytes: [u8; 48] = core::array::from_fn(|i| i as u8 + 0x10);
        let mut rsp = with_dmem(&bytes);
        rsp.set_su(2, 0x18);
        assert!(rsp.vector_mem(false, 0x05, 2, 1, 0, 0));
        for i in 0..8 {
            assert_eq!(rsp.vu_byte(1, i), 0, "the left half stays untouched");
        }
        for i in 8..16 {
            assert_eq!(
                rsp.vu_byte(1, i),
                bytes[0x10 + (i - 8)],
                "byte {i} comes from the previous 16-byte boundary"
            );
        }
    }

    /// The element field shortens `LRV` from the front on the DMEM side while
    /// moving the destination **up** — the wiki's `e(2)` example reads
    /// `0x10..0x13` into `VPR[12..15]`.
    #[test]
    fn lrv_with_an_element_shortens_from_the_front() {
        let bytes: [u8; 48] = core::array::from_fn(|i| i as u8 + 0x10);
        let mut rsp = with_dmem(&bytes);
        rsp.set_su(2, 0x18);
        assert!(rsp.vector_mem(false, 0x05, 2, 1, 4, 0));
        for i in 0..12 {
            assert_eq!(rsp.vu_byte(1, i), 0, "byte {i} untouched");
        }
        for i in 12..16 {
            assert_eq!(
                rsp.vu_byte(1, i),
                bytes[0x10 + (i - 12)],
                "byte {i} still starts from the boundary, not from +4"
            );
        }
    }

    /// **`LQV` and `LRV` together reconstruct a misaligned 128-bit load**, which
    /// is the entire point of the pair. Neither alone can.
    #[test]
    fn lqv_and_lrv_together_load_a_misaligned_vector() {
        let bytes: [u8; 48] = core::array::from_fn(|i| i as u8 + 0x10);
        let mut rsp = with_dmem(&bytes);
        rsp.set_su(2, 0x08);
        rsp.vector_mem(false, 0x04, 2, 1, 0, 0); // lqv 0x08
        rsp.set_su(3, 0x18);
        rsp.vector_mem(false, 0x05, 3, 1, 0, 0); // lrv 0x18
        for i in 0..16 {
            assert_eq!(
                rsp.vu_byte(1, i),
                bytes[8 + i],
                "the pair must yield the 16 bytes at 0x08"
            );
        }
    }

    /// **A scalar load near the end of the register loads fewer bytes rather
    /// than wrapping.**
    ///
    /// The regression test for the bug this branch fixed: the register side
    /// masked its byte index with 15, so an `LDV` at element 12 wrapped its
    /// last four bytes back to `VPR[0..3]` instead of stopping. Nothing here
    /// caught it before — the existing scalar test uses `element = 0`, where
    /// wrapping and truncating agree.
    #[test]
    fn a_scalar_load_near_the_end_of_the_register_truncates() {
        let bytes: [u8; 16] = core::array::from_fn(|i| i as u8 + 0x10);
        let mut rsp = with_dmem(&bytes);
        // LDV wants 8 bytes; at element 12 only 4 fit.
        assert!(rsp.vector_mem(false, 0x03, 0, 1, 12, 0));
        for i in 12..16 {
            assert_eq!(rsp.vu_byte(1, i), bytes[i - 12], "byte {i} loaded");
        }
        for i in 0..12 {
            assert_eq!(
                rsp.vu_byte(1, i),
                0,
                "byte {i} must be untouched -- the tail must NOT wrap to the start"
            );
        }
    }

    /// An unimplemented opcode reports so rather than moving wrong bytes.
    #[test]
    fn an_unimplemented_vector_memory_op_is_reported() {
        let mut rsp = Rsp::new();
        assert!(
            !rsp.vector_mem(false, 0x0A, 0, 1, 0, 0),
            "an unassigned opcode"
        );
    }
}

/// The `VRCP` / `VRSQ` reciprocal ROMs.
///
/// 512 entries each, **generated by exact integer arithmetic** rather than
/// stored as literals — a deliberate departure from `docs/rsp.md`'s "data, not
/// a formula" rule, argued and bounded in accuracy ledger **C-31**. The short
/// version: the rule guards against *approximation*, and these constructions
/// have no rounding freedom, so they reproduce the ROM rather than estimating
/// it. The construction is ares's (ISC).
pub mod rom {
    /// The reciprocal ROM, built at **compile time**.
    ///
    /// A `static` rather than a function, which is what makes it a table in the
    /// binary: the generator runs during const evaluation and nothing computes
    /// a reciprocal at run time. An earlier revision computed each entry on
    /// demand, which made every `VRSQ` pay for a search — see the note on
    /// [`INVERSE_SQUARE_ROOT`].
    pub static RECIPROCAL: [u16; 512] = build_reciprocal();

    /// The inverse-square-root ROM, likewise built at compile time.
    pub static INVERSE_SQUARE_ROOT: [u16; 512] = build_inverse_square_root();

    /// `(1 << 34) / (index + 512)`, rounded.
    ///
    /// Entry 0 is `0xFFFF` and does **not** follow the formula — hardware pins
    /// it, and a generator that applies the formula uniformly gets the
    /// most-used entry wrong.
    const fn build_reciprocal() -> [u16; 512] {
        let mut table = [0u16; 512];
        table[0] = u16::MAX;
        let mut index = 1usize;
        while index < 512 {
            let a = (index as u64) + 512;
            let b = (1u64 << 34) / a;
            table[index] = ((b + 1) >> 8) as u16;
            index += 1;
        }
        table
    }

    /// The **smallest** `b ≥ 2¹⁷` with `a·(b+1)² ≥ 2⁴⁴`, where `a` is halved on
    /// odd indices.
    ///
    /// Note the predicate, which is *not* what ares's comment above the same
    /// loop says. That comment reads "find the largest b where b < 1.0 /
    /// sqrt(a)", but the loop is `while cond { b += 1 }` — it walks *through*
    /// the last satisfying value and stops one past it. The value the table
    /// actually holds is therefore one greater than the comment describes.
    ///
    /// This was found by pinning the bisection against twelve values captured
    /// from the original upward scan: implementing the comment's predicate gave
    /// `26964` where the scan gives `26965`. Without that test the off-by-one
    /// would have shipped, since every property the other tests check
    /// (monotonicity, the odd/even interleave, the 16-bit range) holds just as
    /// well one step to the left.
    ///
    /// **Binary search, not a linear scan.** The naive upward walk runs
    /// ~131,000 iterations of two 64-bit multiplications for the smallest `a`,
    /// per entry — tolerable once at build time, disastrous when an earlier
    /// revision called it per instruction, and emulated in software on
    /// `thumbv7em`, which this crate must build for.
    const fn build_inverse_square_root() -> [u16; 512] {
        let mut table = [0u16; 512];
        let mut index = 0usize;
        while index < 512 {
            let a = ((index as u64) + 512) >> ((index % 2 == 1) as u32);
            // `b + 1` stays under 2^19 for the smallest `a`, so `a*(b+1)^2`
            // fits in `u64`.
            let mut lo = 1u64 << 17;
            let mut hi = 1u64 << 19;
            while lo < hi {
                let mid = u64::midpoint(lo, hi);
                if a * (mid + 1) * (mid + 1) >= (1u64 << 44) {
                    hi = mid;
                } else {
                    lo = mid + 1;
                }
            }
            table[index] = (lo >> 1) as u16;
            index += 1;
        }
        table
    }

    /// The reciprocal ROM entry for `index`.
    #[must_use]
    pub fn reciprocal(index: usize) -> u16 {
        RECIPROCAL[index & 511]
    }

    /// The inverse-square-root ROM entry for `index`.
    #[must_use]
    pub fn inverse_square_root(index: usize) -> u16 {
        INVERSE_SQUARE_ROOT[index & 511]
    }
}

#[cfg(test)]
mod rom_tests {
    use super::rom;

    /// **Entry 0 is a special case in both tables' construction.**
    ///
    /// The reciprocal formula does not produce `0xFFFF` at index 0; hardware
    /// pins it, and a generator that applies the formula uniformly gets the
    /// most-used entry wrong.
    #[test]
    fn the_reciprocal_rom_pins_its_first_entry() {
        assert_eq!(rom::reciprocal(0), 0xFFFF);
        // And the formula does take over immediately afterwards.
        assert_eq!(rom::reciprocal(1), ((((1u64 << 34) / 513) + 1) >> 8) as u16);
    }

    /// The reciprocal ROM is **monotonically decreasing**: it approximates
    /// `1/x` over an increasing divisor, so any entry that rises above its
    /// predecessor is a construction error rather than a rounding artefact.
    ///
    /// This is a property test rather than a transcription check — it cannot
    /// prove the values are the hardware's, but it fails loudly for the class
    /// of error a generator actually makes (a wrong shift, a wrong width),
    /// which a handful of spot-checked entries would not.
    #[test]
    fn the_reciprocal_rom_decreases_monotonically() {
        for i in 2..512 {
            assert!(
                rom::reciprocal(i) <= rom::reciprocal(i - 1),
                "entry {i} rose above its predecessor"
            );
        }
    }

    /// **The binary search reproduces the linear scan exactly.**
    ///
    /// The build switched from an upward scan to a bisection for speed, and the
    /// two agreeing is the whole basis for calling that a refactor rather than
    /// a change. These twelve values were captured from the scan *before* the
    /// switch; if the bisection has an off-by-one at the boundary it lands
    /// here rather than in a wrong vertex months later.
    #[test]
    fn the_inverse_square_root_rom_matches_the_linear_scan() {
        assert_eq!(
            core::array::from_fn::<u16, 12, _>(rom::inverse_square_root),
            [
                27145, 65535, 26965, 65280, 26785, 65026, 26607, 64774, 26430, 64523, 26253, 64274
            ]
        );
    }

    /// Every entry fits the 16 bits the ROM is, at both ends of the range.
    #[test]
    fn both_roms_stay_within_sixteen_bits() {
        // Exercised for its panic-freedom and range; `u16` makes the bound
        // structural, so what this really pins is that the generators do not
        // overflow their intermediate `u64` arithmetic on the way.
        for i in 0..512 {
            let _ = rom::reciprocal(i);
            let _ = rom::inverse_square_root(i);
        }
    }

    /// **The inverse-square-root ROM is two interleaved decreasing sequences**,
    /// not one.
    ///
    /// Odd indices *halve* `a`, so they cover the neighbouring binade and come
    /// out **larger** than their even predecessors — the table alternates
    /// between roughly 92,000 and 131,000 rather than descending. Asserting a
    /// single monotonic run fails, and my first version of this test asserted
    /// the pairing backwards; the structure was checked against the generator's
    /// output rather than the assertion being flipped until it passed.
    ///
    /// That interleaving is what lets `VRSQ` cover a 2:1 input range with one
    /// table.
    #[test]
    fn the_inverse_square_root_rom_is_two_interleaved_sequences() {
        for i in (2..510).step_by(2) {
            assert!(
                rom::inverse_square_root(i) <= rom::inverse_square_root(i - 2),
                "the even subsequence rose at {i}"
            );
            assert!(
                rom::inverse_square_root(i + 1) <= rom::inverse_square_root(i - 1),
                "the odd subsequence rose at {i}"
            );
            assert!(
                rom::inverse_square_root(i + 1) > rom::inverse_square_root(i),
                "the odd entry at {i} must exceed its even neighbour"
            );
        }
    }
}

/// The single-lane reciprocal group: `VRCP`, `VRSQ`, their `L`/`H` partners,
/// `VMOV` and `VNOP`.
///
/// These do not operate lane-wise. They read **one** lane of `vt` (selected by
/// the element field), write **one** lane of `vd` (selected by the `de` field,
/// which occupies the `vs` position), and set the whole accumulator low slice to
/// the broadcast `vt`.
impl Rsp {
    /// The shared core of `VRCP`/`VRCPL` and `VRSQ`/`VRSQL`.
    ///
    /// `long` selects whether a staged high half is consumed; `sqrt` selects the
    /// inverse-square-root table and its halved shift.
    fn reciprocal_core(&mut self, element: u32, vt: usize, de: usize, long: bool, sqrt: bool) {
        let lane = self.vt_lane(vt, element, (element & 7) as usize);
        // A 32-bit operand only when an `H` instruction staged one immediately
        // before; otherwise the 16-bit lane, sign-extended.
        let input: i32 = if long && self.div.pending {
            ((i32::from(self.div.input)) << 16) | i32::from(lane)
        } else {
            i32::from(lane.cast_signed())
        };

        let mask = input >> 31;
        let mut data = input ^ mask;
        if input > -32768 {
            data -= mask;
        }

        let result: i32 = if data == 0 {
            // Division by zero saturates rather than faulting -- the RSP has no
            // exception mechanism to report it with.
            0x7FFF_FFFF
        } else if input == -32768 {
            // The one input whose negation is not representable.
            0xFFFF_0000u32.cast_signed()
        } else {
            let shift = data.cast_unsigned().leading_zeros();
            let index = ((u64::from(data.cast_unsigned()) << shift) & 0x7FC0_0000) >> 22;
            let entry = if sqrt {
                // The odd/even interleave: the low bit of the shift picks which
                // of the two sequences the entry comes from.
                rom::inverse_square_root(((index as usize) & 0x1FE) | (shift as usize & 1))
            } else {
                rom::reciprocal(index as usize)
            };
            let r = (0x10000 | i32::from(entry)) << 14;
            // The square root halves the renormalising shift, because it is
            // undoing a squaring.
            let back = if sqrt { (31 - shift) >> 1 } else { 31 - shift };
            (r >> back) ^ mask
        };

        self.div.pending = false;
        self.div.output = (result.cast_unsigned() >> 16) as u16;
        self.acc_low_from_broadcast(element, vt);
        self.set_vd_lane(de, result.cast_unsigned() as u16);
    }

    /// Set every accumulator low slice to the broadcast `vt`, which each of
    /// these instructions does regardless of what it computes.
    fn acc_low_from_broadcast(&mut self, element: u32, vt: usize) {
        for lane in 0..8 {
            let v = self.vt_lane(vt, element, lane);
            self.vu_acc[lane] = (self.vu_acc[lane] & 0xFFFF_FFFF_0000) | u64::from(v);
        }
    }

    /// Write the destination lane. Stored separately so `vd` can be resolved
    /// once by the caller.
    fn set_vd_lane(&mut self, de: usize, value: u16) {
        self.pending_vd_lane = Some((de, value));
    }

    /// The single-lane group's dispatch. Returns `false` for an opcode outside
    /// it, so the caller can fall through.
    pub fn vu_single_lane(
        &mut self,
        op: u32,
        element: u32,
        vt: usize,
        de: usize,
        vd: usize,
    ) -> bool {
        let de = de & 7;
        match op {
            0x30 | 0x31 | 0x34 | 0x35 => {
                let long = op == 0x31 || op == 0x35;
                let sqrt = op >= 0x34;
                self.reciprocal_core(element, vt, de, long, sqrt);
            }
            // The `H` partners stage the high half and hand back the high half
            // of the previous result. They compute nothing themselves.
            0x32 | 0x36 => {
                self.acc_low_from_broadcast(element, vt);
                self.div.input = self.vt_lane(vt, element, (element & 7) as usize);
                self.div.pending = true;
                self.pending_vd_lane = Some((de, self.div.output));
            }
            // VMOV copies one lane of the broadcast source.
            0x33 => {
                self.acc_low_from_broadcast(element, vt);
                let v = self.vt_lane(vt, element, de);
                self.pending_vd_lane = Some((de, v));
            }
            // VNOP and VNULL retire without effect.
            0x37 | 0x3F => return true,
            _ => return false,
        }
        if let Some((lane, value)) = self.pending_vd_lane.take() {
            self.vu_regs[vd & 31][lane] = value;
        }
        true
    }
}

#[cfg(test)]
mod reciprocal_tests {
    use super::*;

    /// **`VRCPH` stages a high half and hands back the previous result's.**
    ///
    /// Both halves of that matter: the write-back is the *old* `DIVOUT`, not
    /// anything derived from this instruction's operand, so an implementation
    /// that returns the staged value instead looks plausible and produces
    /// garbage on the second use.
    #[test]
    fn vrcph_stages_the_input_and_returns_the_previous_output() {
        let mut rsp = Rsp::new();
        rsp.vu_regs[1] = [0x1234; 8];
        rsp.div.output = 0xBEEF;

        assert!(rsp.vu_single_lane(0x32, 0, 1, 0, 2));
        assert_eq!(rsp.vu_regs[2][0], 0xBEEF, "the PREVIOUS output comes back");
        assert_eq!(rsp.div.input, 0x1234, "and this operand is staged");
        assert!(rsp.div.pending);
    }

    /// **`VRCPL` consumes a staged half only when one was just staged.**
    ///
    /// `pending` is what separates "a high half was staged by the preceding
    /// instruction" from "there is a stale value in the latch". Without it an
    /// `L` issued on its own silently consumes whatever the last `H` left, and
    /// the result depends on unrelated code that ran earlier.
    #[test]
    fn vrcpl_consumes_a_staged_half_only_once() {
        let mut rsp = Rsp::new();
        rsp.vu_regs[1] = [0x0002; 8];

        // Stage, then consume: `pending` must be cleared by the consumer.
        rsp.vu_single_lane(0x32, 0, 1, 0, 2);
        assert!(rsp.div.pending);
        rsp.vu_single_lane(0x31, 0, 1, 0, 3);
        assert!(!rsp.div.pending, "the L instruction clears the staging");

        // A second L with nothing staged must take the 16-bit path. Compare it
        // against a plain VRCP of the same operand, which is the same path.
        let mut plain = Rsp::new();
        plain.vu_regs[1] = [0x0002; 8];
        plain.vu_single_lane(0x30, 0, 1, 0, 4);

        rsp.vu_single_lane(0x31, 0, 1, 0, 5);
        assert_eq!(
            rsp.vu_regs[5][0], plain.vu_regs[4][0],
            "an unstaged L is a plain 16-bit reciprocal"
        );
    }

    /// Division by zero **saturates** rather than faulting — the RSP has no
    /// exception mechanism, so there is nothing for it to raise.
    #[test]
    fn reciprocal_of_zero_saturates() {
        let mut rsp = Rsp::new();
        rsp.vu_regs[1] = [0; 8];
        rsp.vu_single_lane(0x30, 0, 1, 0, 2);
        assert_eq!(rsp.div.output, 0x7FFF, "the high half of 0x7FFF_FFFF");
        assert_eq!(rsp.vu_regs[2][0], 0xFFFF, "and the low half");
    }

    /// Every one of these writes the accumulator's low slice from the broadcast
    /// source, whatever else it does — including `VRCPH`, which computes
    /// nothing.
    #[test]
    fn the_single_lane_group_always_writes_the_accumulator_low_slice() {
        for op in [0x30u32, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36] {
            let mut rsp = Rsp::new();
            rsp.vu_regs[1] = [0xABCD; 8];
            rsp.vu_single_lane(op, 0, 1, 0, 2);
            assert_eq!(
                (rsp.vu_acc[5] & 0xFFFF) as u16,
                0xABCD,
                "opcode {op:#04x} did not write ACC_LO"
            );
        }
    }

    /// `VNOP` retires without touching anything — including the accumulator,
    /// which separates it from the rest of the group.
    #[test]
    fn vnop_does_nothing_at_all() {
        let mut rsp = Rsp::new();
        rsp.vu_regs[1] = [0xABCD; 8];
        assert!(rsp.vu_single_lane(0x37, 0, 1, 0, 2));
        assert_eq!(rsp.vu_regs[2], [0; 8]);
        assert_eq!(rsp.vu_acc, [0; 8], "not even the accumulator");
    }
}
