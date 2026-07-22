//! `rustyn64-rdp` — RDP (Reality Display Processor), the RCP rasterizer.
//!
//! The RDP consumes a command stream (from the RSP or the CPU via the DP FIFO)
//! and rasterizes triangles/rectangles into a framebuffer in RDRAM, running the
//! color-combiner + blender + Z/coverage pipeline. The Video Interface (VI)
//! then scans that framebuffer out. The accuracy bar is **LLE** — a faithful
//! per-pixel pipeline (the ParaLLEl-RDP / angrylion reference), not a
//! triangle-list HLE.
//!
//! This is a **skeleton**: the actual rasterizer (edge walking, the texture
//! engine with TMEM, the combiner/blender, dithering, coverage AA) is the major
//! roadmap phase and is left behind a no-op step with a TODO marker.
//!
//! Part of the one-directional chip-crate graph (see `docs/architecture.md`):
//! this crate depends on **exactly one** chip crate, `rustyn64-cart`, purely for
//! its [`RdramBus`] memory-bus trait — the RDP reads texture and framebuffer
//! reaches its tile storage through `rustynes-mappers`. `#![no_std]` + `alloc`.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::cast_possible_truncation, clippy::cast_lossless)]
// Skeleton `tick` is deliberately non-`const` (it will drain the DP FIFO).
#![allow(clippy::missing_const_for_fn)]

extern crate alloc;

pub use rustyn64_cart::RdramBus;

/// The narrow bus the RDP sees.
///
/// RDRAM access (for the framebuffer + texture fetches) plus the
/// DP-interrupt-raise hook. Extends [`RdramBus`] (`RustyNES`'s `PpuBus` analog)
/// with the IRQ notify the rasterizer needs on `SYNC_FULL` / DP-done.
pub trait VideoBus: RdramBus {
    /// Raise the DP (RDP-done) interrupt on the MI. Default no-op for ad-hoc
    /// test buses; `rustyn64-core` sets the live `MI_INTR.dp` line.
    fn raise_dp_interrupt(&mut self) {}
}

/// One RGBA8888 output pixel (post-VI-filter); the framebuffer the frontend
/// presents is a slice of these.
pub type Pixel = u32;

/// `DPC_STATUS.XBUS` — the DP reads commands from DMEM rather than RDRAM.
pub const DP_STATUS_XBUS: u32 = 0x1;
/// `DPC_STATUS.FREEZE` — the DP is halted; registers can be read/written freely
/// without the command FIFO advancing.
pub const DP_STATUS_FREEZE: u32 = 0x2;
/// `DPC_STATUS.END_VALID` — an end address is latched and pending.
pub const DP_STATUS_END_VALID: u32 = 0x200;
/// `DPC_STATUS.START_VALID` — a start address is latched and pending; further
/// writes to `DPC_START` are ignored until it is consumed by a `DPC_END` write.
pub const DP_STATUS_START_VALID: u32 = 0x400;

/// The `DPC_START`/`DPC_END` register mask: a 24-bit, 8-byte-aligned RDRAM
/// address (n64-systemtest's `RDP START & END REG (masking)`).
const DPC_ADDR_MASK: u32 = 0x00FF_FFF8;

/// RDP state (skeleton).
///
/// Holds the command-FIFO pointers, the current render mode (other-modes),
/// scissor rectangle, and the color-image / Z-image RDRAM addresses. TMEM and
/// the per-tile descriptors come with the texture engine in a later phase.
#[derive(Debug, Default, Clone)]
pub struct Rdp {
    /// DP command FIFO start (`DPC_START`).
    pub cmd_start: u32,
    /// DP command FIFO end (`DPC_END`).
    pub cmd_end: u32,
    /// DP command FIFO current (`DPC_CURRENT`).
    pub cmd_current: u32,
    /// DP command FIFO status (`DPC_STATUS`): FREEZE, START/END-valid, XBUS,
    /// and (later) the busy/counter bits.
    pub status: u32,
    /// Color-image (framebuffer) base in RDRAM (`SET_COLOR_IMAGE`).
    pub color_image: u32,
    /// Z-image base in RDRAM (`SET_Z_IMAGE`).
    pub z_image: u32,
    // TODO(T-RDP-01): TMEM (4 KiB), the 8 tile descriptors, other-modes bits,
    // the combiner + blend-mode latches, the scissor rect — see `docs/rdp.md`.
}

impl Rdp {
    /// Construct at power-on.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Read a DP command register by word offset within the `0x0410_0000`
    /// block: 0 `DPC_START`, 1 `DPC_END`, 2 `DPC_CURRENT`, 3 `DPC_STATUS`. The
    /// clock/busy/counter registers (4..=7) are not modelled and read zero.
    #[must_use]
    pub const fn dpc_read(&self, offset: u32) -> u32 {
        match offset & 7 {
            0 => self.cmd_start,
            1 => self.cmd_end,
            2 => self.cmd_current,
            3 => self.status,
            _ => 0,
        }
    }

    /// Write a DP command register (word offsets as in [`Rdp::dpc_read`]).
    ///
    /// The FIFO uses a double-latch that n64-systemtest's `RSP STATUS:
    /// start-valid` pins exactly:
    ///
    /// - Writing `DPC_START` latches the (masked) address and sets `START_VALID`
    ///   **only if it was clear** — a second write while valid is ignored.
    /// - Writing `DPC_END` latches the end address, copies the pending start
    ///   into `DPC_CURRENT`, and clears `START_VALID`. On unfrozen hardware this
    ///   also starts the RDP; while frozen it only latches, and `END_VALID`
    ///   stays clear because a frozen FIFO never advances to consume it.
    pub const fn dpc_write(&mut self, offset: u32, value: u32) {
        match offset & 7 {
            0 => {
                if self.status & DP_STATUS_START_VALID == 0 {
                    self.cmd_start = value & DPC_ADDR_MASK;
                    self.status |= DP_STATUS_START_VALID;
                }
            }
            1 => {
                self.cmd_end = value & DPC_ADDR_MASK;
                self.cmd_current = self.cmd_start;
                self.status &= !DP_STATUS_START_VALID;
            }
            3 => self.dpc_write_status(value),
            _ => {}
        }
    }

    /// Apply a `DPC_STATUS` write, whose bits are set/clear *commands* rather
    /// than the status layout read back. Only XBUS and FREEZE are modelled; the
    /// FLUSH/TMEM/PIPE/CMD/CLOCK-counter commands come with the FIFO drain.
    const fn dpc_write_status(&mut self, value: u32) {
        const CLEAR_XBUS: u32 = 0x1;
        const SET_XBUS: u32 = 0x2;
        const CLEAR_FREEZE: u32 = 0x4;
        const SET_FREEZE: u32 = 0x8;
        if value & CLEAR_XBUS != 0 {
            self.status &= !DP_STATUS_XBUS;
        }
        if value & SET_XBUS != 0 {
            self.status |= DP_STATUS_XBUS;
        }
        if value & CLEAR_FREEZE != 0 {
            self.status &= !DP_STATUS_FREEZE;
        }
        if value & SET_FREEZE != 0 {
            self.status |= DP_STATUS_FREEZE;
        }
    }

    /// Advance the RDP by one rasterization step (drain part of the DP FIFO).
    ///
    /// Hot path: keep allocation-free. No-op while the FIFO is empty or the DP
    /// is frozen (`DPC_STATUS.FREEZE`).
    pub fn tick<B: VideoBus>(&mut self, bus: &mut B) {
        if self.status & DP_STATUS_FREEZE != 0 || self.cmd_current >= self.cmd_end {
            return;
        }
        // TODO(v0.x): LLE RDP rasterizer — parse the next DP command word at
        // `cmd_current` via `bus.rdram_read_u32`, dispatch (triangle / rectangle
        // / sync / set-mode), edge-walk the primitive, run the texture+combiner+
        // blender per-pixel pipeline into the `color_image`, and raise the DP
        // interrupt on `SYNC_FULL` via `bus.raise_dp_interrupt()`.
        let _ = bus;
        self.cmd_current = self.cmd_current.wrapping_add(8);
    }
}

/// Returns the crate version string.
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NullBus;
    impl RdramBus for NullBus {
        fn rdram_read(&self, _addr: u32) -> u8 {
            0
        }
        fn rdram_write(&mut self, _addr: u32, _val: u8) {}
    }
    impl VideoBus for NullBus {}

    #[test]
    fn empty_fifo_tick_is_noop() {
        let mut rdp = Rdp::new();
        let mut bus = NullBus;
        rdp.tick(&mut bus);
        assert_eq!(rdp.cmd_current, 0);
    }

    #[test]
    fn version_is_non_empty() {
        assert!(!version().is_empty());
    }

    /// **`DPC_STATUS` writes are set/clear commands.** `SET_FREEZE` (0x8) raises
    /// FREEZE; `CLEAR_FREEZE` (0x4) drops it. n64-systemtest's `RDP START & END
    /// REG` freezes the DP precisely so it can poke the registers.
    #[test]
    fn status_write_sets_and_clears_freeze() {
        let mut rdp = Rdp::new();
        rdp.dpc_write(3, 0x8); // SET_FREEZE
        assert_ne!(rdp.dpc_read(3) & DP_STATUS_FREEZE, 0, "freeze set");
        rdp.dpc_write(3, 0x4); // CLEAR_FREEZE
        assert_eq!(rdp.dpc_read(3) & DP_STATUS_FREEZE, 0, "freeze cleared");
    }

    /// **`DPC_START`/`END` mask to a 24-bit, 8-aligned address**, and writing
    /// `END` copies the latched start into `CURRENT`.
    #[test]
    fn start_end_mask_and_current_follows_start() {
        let mut rdp = Rdp::new();
        rdp.dpc_write(3, 0x8); // freeze
        rdp.dpc_write(0, 0x12FF_FFFF); // START
        rdp.dpc_write(1, 0x12FF_FFFF); // END
        assert_eq!(rdp.dpc_read(0), 0x00FF_FFF8, "START masked");
        assert_eq!(rdp.dpc_read(1), 0x00FF_FFF8, "END masked");
        assert_eq!(rdp.dpc_read(2), 0x00FF_FFF8, "CURRENT = START after END");
    }

    /// **The `START_VALID` double-latch.** Writing START sets `START_VALID`; a
    /// second write while valid is *ignored*; writing END consumes it (clears
    /// `START_VALID`, leaves `END_VALID` clear while frozen). This is the exact
    /// sequence `RSP STATUS: start-valid` walks.
    #[test]
    fn start_valid_latch_ignores_a_second_start_write() {
        let mut rdp = Rdp::new();
        rdp.dpc_write(3, 0x8); // freeze
        assert_eq!(rdp.dpc_read(3) & DP_STATUS_START_VALID, 0, "clear at entry");

        rdp.dpc_write(0, 0x1238); // START
        assert_ne!(
            rdp.dpc_read(3) & DP_STATUS_START_VALID,
            0,
            "set after write"
        );
        assert_eq!(rdp.dpc_read(0), 0x1238);

        rdp.dpc_write(0, 0x12_3450); // ignored while valid
        assert_eq!(rdp.dpc_read(0), 0x1238, "second START write ignored");

        rdp.dpc_write(1, 0x1238); // END consumes the latch
        assert_eq!(rdp.dpc_read(3) & DP_STATUS_START_VALID, 0, "cleared by END");
        assert_eq!(
            rdp.dpc_read(3) & DP_STATUS_END_VALID,
            0,
            "END_VALID clear while frozen"
        );
        assert_eq!(rdp.dpc_read(2), 0x1238, "CURRENT = START");
    }

    /// **A frozen DP does not advance the FIFO**, so registers stay put even
    /// with `cmd_current < cmd_end`.
    #[test]
    fn a_frozen_dp_does_not_tick() {
        let mut rdp = Rdp::new();
        rdp.status = DP_STATUS_FREEZE;
        rdp.cmd_current = 0x10;
        rdp.cmd_end = 0x40;
        let mut bus = NullBus;
        rdp.tick(&mut bus);
        assert_eq!(rdp.cmd_current, 0x10, "frozen: CURRENT unchanged");
    }
}
