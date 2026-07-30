//! Video Interface (VI) register file.
//!
//! The VI reads the framebuffer at `VI_ORIGIN` and scans it out to the DAC,
//! raising the VI interrupt at the programmed scanline. This module owns the
//! **memory-mapped register block** at `0x0440_0000` — sixteen 32-bit registers
//! the CPU programs — plus the scan-position timing:
//!
//! - The register latches, with one side effect: writing `VI_V_CURRENT`
//!   acknowledges (clears) the VI interrupt.
//! - [`Vi::tick`] advances `VI_V_CURRENT` off `master_ticks` (the fractional VI
//!   domain — `docs/scheduler.md`) and reports a `VI_V_INTR` crossing, which the
//!   scheduler turns into `MI_INTR.vi`.
//!
//! Not here (elsewhere or deferred): the framebuffer→RGBA scan-*out* conversion
//! is `Bus::scanout` (the live 1:1 path) and `Bus::scanout_scaled` (the accurate
//! `VI_X_SCALE`/`VI_Y_SCALE`-resampling path being built up slice by slice against
//! the Angrylion `.vivec` oracle — ledger R-5); the field cadence is anchored to
//! nominal 60 Hz NTSC (ledger R-6, PAL later); and the per-register write masks are
//! not yet applied (ledger R-4). Reference: `n64brew_wiki/markdown/Video Interface.md`.

use serde::{Deserialize, Serialize};
/// `VI_CTRL` (`0x0440_0000`): pixel type, AA/serrate/dither config. `TYPE == 0`
/// turns the VI off (no interrupt is ever generated).
pub const VI_CTRL: u32 = 0;
/// `VI_ORIGIN` (0x04): RDRAM base of the framebuffer being scanned out.
pub const VI_ORIGIN: u32 = 1;
/// `VI_WIDTH` (0x08): framebuffer width in pixels.
pub const VI_WIDTH: u32 = 2;
/// `VI_V_INTR` (0x0C): the half-line at which the VI interrupt is raised.
pub const VI_V_INTR: u32 = 3;
/// `VI_V_CURRENT` (0x10): the half-line currently being scanned.
///
/// A **write acknowledges the VI interrupt**; the value itself is not
/// software-latched (it reflects the scan position, set by the scheduler once
/// that lands).
pub const VI_V_CURRENT: u32 = 4;
/// `VI_BURST` (0x14): color-burst timing.
pub const VI_BURST: u32 = 5;
/// `VI_V_TOTAL` (0x18): total half-lines per frame; bit 0 selects
/// interlaced/progressive `VI_V_INTR` behavior.
pub const VI_V_TOTAL: u32 = 6;
/// `VI_H_TOTAL` (0x1C): total pixels (quarter-precision) per line.
pub const VI_H_TOTAL: u32 = 7;
/// `VI_H_TOTAL_LEAP` (0x20): line-length modulation for exact frame timing.
pub const VI_H_TOTAL_LEAP: u32 = 8;
/// `VI_H_VIDEO` (0x24): active video horizontal start/end.
pub const VI_H_VIDEO: u32 = 9;
/// `VI_V_VIDEO` (0x28): active video vertical start/end.
pub const VI_V_VIDEO: u32 = 10;
/// `VI_V_BURST` (0x2C): vertical color-burst start/end.
pub const VI_V_BURST: u32 = 11;
/// `VI_X_SCALE` (0x30): horizontal scale factor (framebuffer → screen).
pub const VI_X_SCALE: u32 = 12;
/// `VI_Y_SCALE` (0x34): vertical scale factor.
pub const VI_Y_SCALE: u32 = 13;
/// `VI_TEST_ADDR` (0x38): RDRAM diagnostic access address.
pub const VI_TEST_ADDR: u32 = 14;
/// `VI_STAGED_DATA` (0x3C): RDRAM diagnostic staged data.
pub const VI_STAGED_DATA: u32 = 15;

/// Number of 32-bit registers in the VI block.
pub const VI_REG_COUNT: usize = 16;

/// Nominal NTSC field rate anchoring the VI scan cadence.
///
/// The VI dot clock is off a separate crystal (~48.68 MHz) that the N64brew wiki
/// gives only *roughly*, so rather than fit an imprecise dot-clock frequency, the
/// field cadence is anchored to the standard NTSC **60 Hz** and the per-half-line
/// period derived from the software-programmed `VI_V_TOTAL`. PAL uses
/// [`VI_FIELD_HZ_PAL`], selected by `Vi::field_hz` from the field length.
pub const VI_FIELD_HZ: u64 = 60;

/// Nominal PAL field rate (R-6): PAL's standard **50 Hz** field cadence.
///
/// The counterpart to NTSC's [`VI_FIELD_HZ`] — a documented broadcast standard
/// (N64brew *Video Interface*: NTSC ~60 Hz / PAL 50 Hz), not a fitted value, so it
/// is anchored the same way as the NTSC rate. Selected when the programmed field is
/// PAL-length (`VI_V_TOTAL > 550`, ~625 half-lines vs NTSC's ~525), matching the
/// scan-out geometry's `ispal` test. The interlace/serrate half-field quirk and the
/// exact `H_TOTAL` sub-field timing remain deferred under R-6.
pub const VI_FIELD_HZ_PAL: u64 = 50;

/// The number of half-lines the widest `VI_V_TOTAL` can encode.
///
/// `Vi::total_halflines` is `(VI_V_TOTAL & 0x3FF) + 1`, so the field is 1..=1024
/// half-lines long and can never be zero — which is why `ticks_per_halfline` cannot
/// divide by zero, and also why it can never *return* zero.
const VI_MAX_HALFLINES: u64 = 1024;

/// The smallest value `Vi::ticks_per_halfline` can take over the **entire**
/// programmable space: the fastest field rate against the longest field.
///
/// `Vi::tick` runs on every RCP step and a half-line elapses on roughly one call in
/// 1,980, so the division that produces the real period is almost always wasted. It
/// is skipped when the accumulator has not reached this bound, which is exact rather
/// than approximate: below it the `while` loop cannot execute for *any* legal
/// register programming, so nothing that depends on the true period is being guessed.
///
/// Both operands are extremal on purpose. `VI_FIELD_HZ` (60) is the larger of the two
/// field rates and [`VI_MAX_HALFLINES`] the longest field, so their product is the
/// largest divisor and this quotient the smallest period. A `const` assertion below
/// pins that, so a new field rate or a wider `VI_V_TOTAL` mask breaks the build
/// rather than silently making the bound too large — which would skip a half-line.
const VI_MIN_TICKS_PER_HALFLINE: u64 = crate::MASTER_HZ / (VI_FIELD_HZ * VI_MAX_HALFLINES);

const _: () = {
    assert!(
        VI_FIELD_HZ >= VI_FIELD_HZ_PAL,
        "VI_MIN_TICKS_PER_HALFLINE assumes the NTSC rate is the faster one; if PAL \
         ever exceeds it, the minimum period is computed from the wrong field rate"
    );
    // The mask in `total_halflines` is what actually bounds the field length. If it
    // widens, this constant must widen with it or `Vi::tick` will skip a half-line.
    assert!(
        VI_MAX_HALFLINES == (0x3FF + 1),
        "VI_MAX_HALFLINES must match the 0x3FF mask in Vi::total_halflines"
    );
    assert!(
        VI_MIN_TICKS_PER_HALFLINE > 0,
        "a zero bound would disable the fast path rather than merely loosen it"
    );
};

/// The **encoded** `VI_V_TOTAL` register value above which a field is treated as
/// **PAL** rather than NTSC.
///
/// The comparison is against the raw register value, which encodes `half-lines − 1`
/// (`total_halflines() == VI_V_TOTAL + 1`): NTSC programs ~524 (525 half-lines), PAL
/// ~624 (625 half-lines), so the encoded `> 550` splits them with wide margin.
/// Shared by `Vi::field_hz` (the field-rate select) and `bus::scanout_scaled` (the
/// geometry `ispal` select) so cadence and geometry agree on the region. Named after
/// N64brew *Video Interface* §Clocks (region field lengths); the register at this
/// offset is `VI_V_SYNC` in the wiki's naming.
pub const VI_PAL_V_TOTAL_THRESHOLD: u32 = 550;

/// The Video Interface register file (the `0x0440_0000` block).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vi {
    /// The sixteen 32-bit registers, indexed by word offset. `pub(crate)` so
    /// every external access goes through [`Vi::read`]/[`Vi::write`] — which is
    /// where the `VI_V_CURRENT` side effect (and future write masks) live; the
    /// scan-out and tests, being in this crate, read them directly.
    pub(crate) regs: [u32; VI_REG_COUNT],
    /// The current scan half-line (`VI_V_CURRENT`'s read-back value). Advanced by
    /// [`Vi::tick`] one half-line at a time — this is the fractional-domain state
    /// the scheduler drives (`docs/scheduler.md`). Kept **relative** (incremented
    /// and wrapped at `VI_V_TOTAL + 1`) rather than derived from absolute
    /// `master_ticks`, so a mid-run `VI_V_TOTAL` change re-bases cleanly.
    v_current: u32,
    /// Master ticks accumulated toward the next half-line advance (the fractional
    /// remainder the scheduler doc calls for).
    acc: u64,
    /// The `master_ticks` at the last [`Vi::tick`], to compute the elapsed delta.
    prev_ticks: u64,
}

impl Default for Vi {
    fn default() -> Self {
        Self::new()
    }
}

impl Vi {
    /// Construct at power-on: every register zero, so `VI_CTRL.TYPE == 0` (the
    /// VI is off) — the correct cold-boot state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            regs: [0; VI_REG_COUNT],
            v_current: 0,
            acc: 0,
            prev_ticks: 0,
        }
    }

    /// Rebase the scan timeline to a fresh `master_ticks == 0` (a warm reset),
    /// clearing the accumulated position and the delta baseline **without**
    /// touching the programmed registers. Without this, `prev_ticks` would keep
    /// the pre-reset value and every post-reset delta would saturate to 0 until
    /// the new run caught up — suppressing the VI interrupt across a reset.
    pub const fn reset_scan(&mut self) {
        self.v_current = 0;
        self.acc = 0;
        self.prev_ticks = 0;
    }

    /// Total scan half-lines per field (`VI_V_TOTAL + 1`), 1..=1024.
    const fn total_halflines(&self) -> u32 {
        (self.regs[VI_V_TOTAL as usize] & 0x3FF) + 1
    }

    /// The nominal field rate for the programmed field length: PAL 50 Hz when the
    /// field is PAL-length (`VI_V_TOTAL > 550`), else NTSC 60 Hz (R-6). Matches the
    /// scan-out geometry's `ispal` test (`bus::scanout_scaled`) so cadence and
    /// geometry agree on the region.
    const fn field_hz(&self) -> u64 {
        if (self.regs[VI_V_TOTAL as usize] & 0x3FF) > VI_PAL_V_TOTAL_THRESHOLD {
            VI_FIELD_HZ_PAL
        } else {
            VI_FIELD_HZ
        }
    }

    /// Master ticks per scan half-line, from the field rate ([`Vi::field_hz`]) and the
    /// programmed `VI_V_TOTAL`. One division (not two) to avoid compounding the
    /// truncation. Zero-guarded by `total_halflines() >= 1`.
    fn ticks_per_halfline(&self) -> u64 {
        crate::MASTER_HZ / (self.field_hz() * u64::from(self.total_halflines()))
    }

    /// Advance the scan position by the master ticks elapsed since the last call
    /// and report whether the VI interrupt should fire.
    ///
    /// `VI_V_CURRENT` advances one half-line every `ticks_per_halfline` master
    /// ticks (accumulating the fractional remainder), wrapping at `VI_V_TOTAL +
    /// 1`. The interrupt fires when the position **lands on** `VI_V_INTR` — the
    /// per-half-line step means no crossing is skipped even when a call spans
    /// many half-lines — and only while the VI is on (`VI_CTRL.TYPE != 0`;
    /// N64brew *Video Interface* §`VI_V_INTR`). A `VI_V_INTR` beyond the field
    /// (`>= VI_V_TOTAL + 1`) is unreachable, so it never fires. The position is
    /// kept relative, so a mid-run `VI_V_TOTAL` change re-bases without a scale
    /// jump. The scheduler calls this each RCP step and raises `MI_INTR.vi` on a
    /// `true` return.
    pub fn tick(&mut self, master_ticks: u64) -> bool {
        let delta = master_ticks.saturating_sub(self.prev_ticks);
        self.prev_ticks = master_ticks;
        // Below the smallest period any legal programming can produce, the `while`
        // cannot execute whatever the registers hold, so the division that would
        // produce the exact period is skipped. Exact, not approximate — and it is
        // most of the work here, since a half-line elapses on about one call in
        // 1,980 (`docs/performance.md`).
        let acc = self.acc + delta;
        if acc < VI_MIN_TICKS_PER_HALFLINE {
            self.acc = acc;
            return false;
        }
        let per_hl = self.ticks_per_halfline();
        // Unreachable: `total_halflines()` is `(VI_V_TOTAL & 0x3FF) + 1`, so it is at
        // least 1, and `field_hz()` is 50 or 60 — the divisor is therefore at most
        // 61,440 against a 187.5 MHz numerator and the quotient cannot be zero. Kept
        // as a divide-by-zero guard rather than as modeled VI state, with an assert
        // so the claim is checkable instead of merely asserted.
        //
        // The comment this replaces said "no timing until `VI_V_TOTAL` is
        // programmed", which misdescribed it: an unprogrammed `VI_V_TOTAL` is one
        // half-line, giving a period of 3,750,000 ticks — very long, never zero.
        debug_assert!(per_hl > 0, "the VI period is bounded below by construction");
        if per_hl == 0 {
            return false;
        }
        self.acc = acc;
        let halflines = self.total_halflines();
        let v_intr = self.regs[VI_V_INTR as usize] & 0x3FF;
        let on = self.regs[VI_CTRL as usize] & 0x3 != 0;
        let mut fired = false;
        while self.acc >= per_hl {
            self.acc -= per_hl;
            self.v_current = (self.v_current + 1) % halflines;
            if on && self.v_current == v_intr {
                fired = true;
            }
        }
        fired
    }

    /// Read a VI register by word offset within the block (mirrored to 16).
    /// `VI_V_CURRENT` reads back the scan position advanced by [`Vi::tick`].
    #[must_use]
    pub const fn read(&self, word_offset: u32) -> u32 {
        let idx = (word_offset & 0xF) as usize;
        if idx == VI_V_CURRENT as usize {
            return self.v_current;
        }
        self.regs[idx]
    }

    /// Write a VI register by word offset. Returns `true` iff this write should
    /// **acknowledge the VI interrupt** — a write to `VI_V_CURRENT`, which the
    /// caller turns into `MI_INTR.vi = false`.
    ///
    /// `VI_V_CURRENT` is not otherwise latched here: its value reflects the scan
    /// position, which the scheduler will drive; a software write only clears
    /// the interrupt.
    pub const fn write(&mut self, word_offset: u32, value: u32) -> bool {
        let idx = (word_offset & 0xF) as usize;
        if idx == VI_V_CURRENT as usize {
            return true;
        }
        self.regs[idx] = value;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The skip bound is a true lower bound across the whole programmable space.**
    ///
    /// `Vi::tick` returns before dividing when the accumulator is below
    /// [`VI_MIN_TICKS_PER_HALFLINE`]. That is only sound if no legal
    /// `(VI_V_TOTAL, field_hz)` pair can produce a *shorter* period — otherwise the
    /// fast path swallows a half-line, `VI_V_CURRENT` runs slow, and the VI
    /// interrupt arrives late, with nothing to indicate why.
    ///
    /// So this checks the bound against every encodable `VI_V_TOTAL` rather than
    /// against the two the retail regions happen to use, walking all 1,024 values.
    /// Mutation-checked: halving `VI_MAX_HALFLINES` (which doubles the bound) turns
    /// this red.
    #[test]
    fn no_legal_programming_produces_a_shorter_halfline_than_the_skip_bound() {
        let mut vi = Vi::new();
        for encoded in 0..=0x3FFu32 {
            vi.regs[VI_V_TOTAL as usize] = encoded;
            let per_hl = vi.ticks_per_halfline();
            assert!(
                per_hl >= VI_MIN_TICKS_PER_HALFLINE,
                "VI_V_TOTAL={encoded} gives a {per_hl}-tick half-line, below the \
                 {VI_MIN_TICKS_PER_HALFLINE}-tick skip bound: tick() would skip it"
            );
            assert!(per_hl > 0, "VI_V_TOTAL={encoded} divides to a zero period");
        }
    }

    /// **Ticking one master tick at a time lands on exactly the same half-line as
    /// one big jump.** The skip must not lose an accumulated remainder.
    ///
    /// The per-tick cadence is what the scheduler actually does, and it is the path
    /// that takes the early-out thousands of times per half-line; the single jump
    /// takes it never. Both are driven to the same `master_ticks`, so any tick the
    /// fast path drops shows up as a different `VI_V_CURRENT`.
    #[test]
    fn stepping_one_tick_at_a_time_matches_one_jump() {
        let mut stepped = Vi::new();
        let mut jumped = Vi::new();
        for vi in [&mut stepped, &mut jumped] {
            vi.regs[VI_V_TOTAL as usize] = 524; // NTSC, 525 half-lines
            vi.regs[VI_CTRL as usize] = 0x3; // VI on
            vi.regs[VI_V_INTR as usize] = 2;
        }
        // Three half-lines' worth, so the early-out runs thousands of times.
        let target = stepped.ticks_per_halfline() * 3 + 17;

        let mut stepped_fired = false;
        for now in 1..=target {
            stepped_fired |= stepped.tick(now);
        }
        let jumped_fired = jumped.tick(target);

        assert_eq!(
            stepped.v_current, jumped.v_current,
            "the per-tick cadence must land on the same half-line as one jump"
        );
        assert_eq!(stepped.acc, jumped.acc, "and carry the same remainder");
        assert!(
            stepped_fired,
            "V_INTR == 2 is crossed within three half-lines"
        );
        assert_eq!(
            stepped_fired, jumped_fired,
            "and both paths must agree that it fired"
        );
    }

    #[test]
    fn power_on_is_all_zero_so_the_vi_is_off() {
        let vi = Vi::new();
        assert_eq!(vi.read(VI_CTRL) & 0x3, 0, "TYPE == 0: VI off at cold boot");
        assert!(vi.regs.iter().all(|&r| r == 0));
    }

    #[test]
    fn ordinary_registers_round_trip() {
        let mut vi = Vi::new();
        for (off, val) in [
            (VI_ORIGIN, 0x0010_0000),
            (VI_WIDTH, 320),
            (VI_V_INTR, 2),
            (VI_X_SCALE, 0x0000_0200),
        ] {
            assert!(!vi.write(off, val), "ordinary write does not ack the IRQ");
            assert_eq!(vi.read(off), val);
        }
    }

    #[test]
    fn writing_v_current_signals_an_interrupt_ack_and_does_not_latch() {
        let mut vi = Vi::new();
        assert!(
            vi.write(VI_V_CURRENT, 0x1234),
            "a VI_V_CURRENT write acknowledges the interrupt"
        );
        assert_eq!(
            vi.read(VI_V_CURRENT),
            0,
            "the written value is not latched into V_CURRENT"
        );
        assert_eq!(
            vi.regs[VI_V_CURRENT as usize], 0,
            "and nothing reached the backing storage either"
        );
    }

    /// **`VI_V_CURRENT` advances with `master_ticks` and wraps at the field.**
    /// With `V_TOTAL + 1 = 525` half-lines, one half-line is
    /// `MASTER_HZ / 60 / 525` master ticks; the read-back tracks it and wraps to
    /// 0 at the field boundary.
    #[test]
    fn v_current_advances_with_master_ticks_and_wraps() {
        let mut vi = Vi::new();
        vi.regs[VI_V_TOTAL as usize] = 524; // 525 half-lines
        let per_hl = crate::MASTER_HZ / (VI_FIELD_HZ * 525);
        vi.tick(0);
        assert_eq!(vi.read(VI_V_CURRENT), 0);
        vi.tick(per_hl);
        assert_eq!(vi.read(VI_V_CURRENT), 1, "one half-line later");
        vi.tick(per_hl * 524);
        assert_eq!(vi.read(VI_V_CURRENT), 524, "last half-line of the field");
        vi.tick(per_hl * 525);
        assert_eq!(vi.read(VI_V_CURRENT), 0, "wraps to 0 at the field boundary");
    }

    /// **The VI interrupt fires once per field as `VI_V_INTR` is crossed.**
    /// It does not re-fire within the same field, and re-fires the next field.
    #[test]
    fn the_vi_interrupt_fires_once_per_field_at_v_intr() {
        let mut vi = Vi::new();
        vi.regs[VI_V_TOTAL as usize] = 524;
        vi.regs[VI_V_INTR as usize] = 2;
        vi.regs[VI_CTRL as usize] = 2; // 16-bit type, VI on
        let per_hl = crate::MASTER_HZ / (VI_FIELD_HZ * 525);
        assert!(!vi.tick(0), "before V_INTR: no interrupt");
        assert!(vi.tick(per_hl * 2), "crossing half-line 2 fires");
        assert!(!vi.tick(per_hl * 3), "already fired this field");
        assert!(vi.tick(per_hl * (525 + 2)), "the next field fires again");
    }

    /// **A disabled VI (`TYPE == 0`) never interrupts**, even past `VI_V_INTR`.
    #[test]
    fn a_disabled_vi_never_interrupts() {
        let mut vi = Vi::new();
        vi.regs[VI_V_TOTAL as usize] = 524;
        vi.regs[VI_V_INTR as usize] = 2;
        vi.regs[VI_CTRL as usize] = 0; // VI off
        let per_hl = crate::MASTER_HZ / (VI_FIELD_HZ * 525);
        assert!(!vi.tick(0));
        assert!(!vi.tick(per_hl * 3), "off: no interrupt even past V_INTR");
    }

    /// **`VI_V_INTR` beyond the field never fires.** `VI_V_CURRENT` wraps at
    /// `VI_V_TOTAL + 1`, so an interrupt line the scan can never reach is inert —
    /// no spurious `v_intr % halflines` phantom.
    #[test]
    fn a_v_intr_past_the_field_never_fires() {
        let mut vi = Vi::new();
        vi.regs[VI_V_TOTAL as usize] = 262; // 263 half-lines
        vi.regs[VI_V_INTR as usize] = 300; // > 263: unreachable
        vi.regs[VI_CTRL as usize] = 2;
        let per_hl = crate::MASTER_HZ / (VI_FIELD_HZ * 263);
        // Run several full fields; the interrupt must never fire.
        for k in 1..=(263 * 3) {
            assert!(!vi.tick(per_hl * k), "unreachable V_INTR never fires");
        }
    }

    /// **A mid-run `VI_V_TOTAL` change re-bases cleanly** — because the position
    /// is relative, changing the field length does not scale-jump the counter or
    /// spuriously fire; the scan just continues and wraps at the new length.
    #[test]
    fn a_mid_run_v_total_change_rebases_without_a_spurious_interrupt() {
        let mut vi = Vi::new();
        vi.regs[VI_V_TOTAL as usize] = 524; // 525 half-lines
        vi.regs[VI_V_INTR as usize] = 600; // unreachable in either config
        vi.regs[VI_CTRL as usize] = 2;
        let per_hl = crate::MASTER_HZ / (VI_FIELD_HZ * 525);
        assert!(!vi.tick(per_hl * 100)); // advance exactly 100 half-lines
        let mid = vi.read(VI_V_CURRENT);
        assert_eq!(mid, 100, "100 half-lines into the 525-line field");
        // Shrink the field; VI_V_INTR (600) is unreachable in both, so no fire.
        vi.regs[VI_V_TOTAL as usize] = 262; // now 263 half-lines
        let per_hl2 = crate::MASTER_HZ / (VI_FIELD_HZ * 263);
        assert!(
            !vi.tick(per_hl * 100 + per_hl2 * 10),
            "no spurious fire on rebase"
        );
        // Relative: 10 more half-lines from 100, wrapped at the *new* 263 — an
        // absolute-time implementation would scale-jump to 60 here instead.
        assert_eq!(
            vi.read(VI_V_CURRENT),
            (mid + 10) % 263,
            "position continues relative across the field-length change"
        );
    }

    /// **A PAL-length field scans at 50 Hz, not 60 (R-6).** With `V_TOTAL + 1 = 625`
    /// half-lines (PAL, `> 550`), one half-line is `MASTER_HZ / 50 / 625 = 6000`
    /// master ticks — distinct from the `MASTER_HZ / 60 / 625 = 5000` an NTSC-rate
    /// field would give, so this is non-vacuous: at `pal_per_hl` ticks the counter
    /// has advanced exactly one half-line, while at the 60 Hz period it would already
    /// be past it. Also confirms an NTSC-length field is unaffected.
    #[test]
    fn a_pal_length_field_scans_at_50hz() {
        let mut vi = Vi::new();
        vi.regs[VI_V_TOTAL as usize] = 624; // 625 half-lines → PAL (> 550)
        let pal_per_hl = crate::MASTER_HZ / (VI_FIELD_HZ_PAL * 625);
        let ntsc_per_hl = crate::MASTER_HZ / (VI_FIELD_HZ * 625);
        assert_eq!(pal_per_hl, 6000);
        assert_eq!(ntsc_per_hl, 5000);
        vi.tick(0);
        assert_eq!(vi.read(VI_V_CURRENT), 0);
        // One PAL half-line period advances exactly one half-line.
        vi.tick(pal_per_hl);
        assert_eq!(
            vi.read(VI_V_CURRENT),
            1,
            "advances at the 50 Hz PAL cadence"
        );
        // A fresh VI clocked for the same wall-ticks at the (wrong) 60 Hz period would
        // already be on half-line 1 well before `pal_per_hl`; assert the PAL VI is
        // still on 0 at the 60 Hz period, proving it is genuinely slower.
        let mut vi2 = Vi::new();
        vi2.regs[VI_V_TOTAL as usize] = 624;
        vi2.tick(0);
        vi2.tick(ntsc_per_hl);
        assert_eq!(
            vi2.read(VI_V_CURRENT),
            0,
            "at the 60 Hz period the PAL field has not yet crossed a half-line"
        );

        // An NTSC-length field is unaffected — still 60 Hz.
        let mut ntsc = Vi::new();
        ntsc.regs[VI_V_TOTAL as usize] = 524; // 525 half-lines → NTSC
        let ntsc525 = crate::MASTER_HZ / (VI_FIELD_HZ * 525);
        ntsc.tick(0);
        ntsc.tick(ntsc525);
        assert_eq!(ntsc.read(VI_V_CURRENT), 1, "NTSC field still 60 Hz");
    }

    /// **The PAL/NTSC split is exactly `VI_V_TOTAL > 550`.** Pins the boundary so an
    /// off-by-one (`>=` vs `>`, or a shifted threshold) is caught: a field with
    /// `VI_V_TOTAL == 550` is still NTSC (60 Hz), and `== 551` is already PAL (50 Hz).
    #[test]
    fn the_pal_threshold_is_exactly_550() {
        let mut vi = Vi::new();
        vi.regs[VI_V_TOTAL as usize] = VI_PAL_V_TOTAL_THRESHOLD; // 550: still NTSC
        assert_eq!(vi.field_hz(), VI_FIELD_HZ, "V_TOTAL == 550 is NTSC");
        vi.regs[VI_V_TOTAL as usize] = VI_PAL_V_TOTAL_THRESHOLD + 1; // 551: PAL
        assert_eq!(vi.field_hz(), VI_FIELD_HZ_PAL, "V_TOTAL == 551 is PAL");
    }
}
