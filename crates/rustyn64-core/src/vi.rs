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
//! is `Bus::scanout`; the scan-out scaling/filters are ledger R-5; the field
//! cadence is anchored to nominal 60 Hz NTSC (ledger R-6, PAL later); and the
//! per-register write masks are not yet applied (ledger R-4). Reference:
//! `n64brew_wiki/markdown/Video Interface.md`.

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
/// `VI_BURST` (0x14): colour-burst timing.
pub const VI_BURST: u32 = 5;
/// `VI_V_TOTAL` (0x18): total half-lines per frame; bit 0 selects
/// interlaced/progressive `VI_V_INTR` behaviour.
pub const VI_V_TOTAL: u32 = 6;
/// `VI_H_TOTAL` (0x1C): total pixels (quarter-precision) per line.
pub const VI_H_TOTAL: u32 = 7;
/// `VI_H_TOTAL_LEAP` (0x20): line-length modulation for exact frame timing.
pub const VI_H_TOTAL_LEAP: u32 = 8;
/// `VI_H_VIDEO` (0x24): active video horizontal start/end.
pub const VI_H_VIDEO: u32 = 9;
/// `VI_V_VIDEO` (0x28): active video vertical start/end.
pub const VI_V_VIDEO: u32 = 10;
/// `VI_V_BURST` (0x2C): vertical colour-burst start/end.
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
/// field cadence is anchored to the standard **60 Hz** and the per-half-line
/// period derived from the software-programmed `VI_V_TOTAL`. Documented as open
/// residual R-6; PAL (50 Hz) is a later refinement.
pub const VI_FIELD_HZ: u64 = 60;

/// The Video Interface register file (the `0x0440_0000` block).
#[derive(Debug, Clone)]
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

    /// Total scan half-lines per field (`VI_V_TOTAL + 1`), 1..=1024.
    const fn total_halflines(&self) -> u32 {
        (self.regs[VI_V_TOTAL as usize] & 0x3FF) + 1
    }

    /// Master ticks per scan half-line, from the nominal field rate and the
    /// programmed `VI_V_TOTAL`. One division (not two) to avoid compounding the
    /// truncation. Zero-guarded by `total_halflines() >= 1`.
    fn ticks_per_halfline(&self) -> u64 {
        crate::MASTER_HZ / (VI_FIELD_HZ * u64::from(self.total_halflines()))
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
        let per_hl = self.ticks_per_halfline();
        // No timing until `VI_V_TOTAL` is programmed; prev_ticks is still advanced
        // above so the pre-setup ticks are not accumulated retroactively.
        if per_hl == 0 {
            return false;
        }
        self.acc += delta;
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
        assert!(!vi.tick(per_hl * 100)); // advance ~100 half-lines
        let mid = vi.read(VI_V_CURRENT);
        // Shrink the field; VI_V_INTR (600) is unreachable in both, so no fire.
        vi.regs[VI_V_TOTAL as usize] = 262; // now 263 half-lines
        let per_hl2 = crate::MASTER_HZ / (VI_FIELD_HZ * 263);
        assert!(
            !vi.tick(per_hl * 100 + per_hl2 * 10),
            "no spurious fire on rebase"
        );
        assert!(
            vi.read(VI_V_CURRENT) < 263,
            "position stays within the new field ({} advanced from {mid})",
            vi.read(VI_V_CURRENT)
        );
    }
}
