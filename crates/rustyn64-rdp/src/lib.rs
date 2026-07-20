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

    /// Advance the RDP by one rasterization step (drain part of the DP FIFO).
    ///
    /// Hot path: keep allocation-free. No-op while the FIFO is empty.
    pub fn tick<B: VideoBus>(&mut self, bus: &mut B) {
        if self.cmd_current >= self.cmd_end {
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
}
