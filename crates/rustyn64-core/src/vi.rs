//! Video Interface (VI) register file.
//!
//! The VI reads the framebuffer at `VI_ORIGIN` and scans it out to the DAC,
//! raising the VI interrupt at the programmed scanline. This module is the
//! **memory-mapped register block** at `0x0440_0000` — sixteen 32-bit registers
//! the CPU programs. Two behaviours that need more than a latch are staged for
//! follow-up tickets and are called out where they belong:
//!
//! - **`VI_V_CURRENT` advancing with the scan position** and the VI interrupt
//!   *firing* at `VI_V_INTR` need the scheduler's fractional VI clock (VCLK is
//!   off a different crystal — `docs/scheduler.md`). Not here yet; `V_CURRENT`
//!   reads back 0 until then.
//! - **Scan-out** (framebuffer → a presentable RGBA buffer) is the next VI
//!   ticket.
//!
//! What *is* here: the register latches, and the one register with a side
//! effect — writing `VI_V_CURRENT` acknowledges (clears) the VI interrupt.
//!
//! Per-register write masks are **not yet applied**: the registers store the
//! full 32-bit value written. Which masks the hardware actually enforces is
//! recorded against n64-systemtest rather than guessed — see `docs/rdp.md`
//! (§VI) and the accuracy ledger. Reference:
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

/// The Video Interface register file (the `0x0440_0000` block).
#[derive(Debug, Clone)]
pub struct Vi {
    /// The sixteen 32-bit registers, indexed by word offset.
    pub regs: [u32; VI_REG_COUNT],
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
        }
    }

    /// Read a VI register by word offset within the block (mirrored to 16).
    #[must_use]
    pub const fn read(&self, offset: u32) -> u32 {
        self.regs[(offset & 0xF) as usize]
    }

    /// Write a VI register by word offset. Returns `true` iff this write should
    /// **acknowledge the VI interrupt** — a write to `VI_V_CURRENT`, which the
    /// caller turns into `MI_INTR.vi = false`.
    ///
    /// `VI_V_CURRENT` is not otherwise latched here: its value reflects the scan
    /// position, which the scheduler will drive; a software write only clears
    /// the interrupt.
    pub const fn write(&mut self, offset: u32, value: u32) -> bool {
        let idx = (offset & 0xF) as usize;
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
    }
}
