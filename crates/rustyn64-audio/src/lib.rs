//! `rustyn64-audio` — the Audio Interface (AI) DAC + sample-DMA path.
//!
//! The RSP's audio microcode mixes samples into an RDRAM buffer; the AI then
//! DMAs that buffer out to the DAC at a programmable sample rate (set via the
//! `AI_DACRATE` divider off the video clock) and raises an AI interrupt when a
//! buffer drains. This crate models the AI side; the actual mixing is RSP
//! microcode (in `rustyn64-rsp`). This is a **skeleton** — the DMA double-buffer
//! state machine and the resampler are roadmap phases left as marked TODOs.
//!
//! Part of the one-directional chip-crate graph (see `docs/architecture.md`):
//! this crate does NOT depend on any other chip crate; it reaches RDRAM and the
//! interrupt line through the [`AudioBus`] trait. `#![no_std]` + `alloc`.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::cast_possible_truncation, clippy::cast_lossless)]
// Skeleton `tick` is deliberately non-`const` (it will drive the DMA FIFO).
#![allow(clippy::missing_const_for_fn)]

extern crate alloc;

/// The narrow bus the AI sees (`RustyNES`'s `ApuBus` analog): fetch a DMA sample
/// word from RDRAM and raise the AI interrupt when a buffer drains.
pub trait AudioBus {
    /// Fetch a big-endian 32-bit sample word (two 16-bit L/R samples) from the
    /// AI DMA buffer in RDRAM at `addr`.
    fn ai_dma_read_u32(&self, addr: u32) -> u32;
    /// Raise the AI (audio-buffer-done) interrupt on the MI. Default no-op.
    fn raise_ai_interrupt(&mut self) {}
}

/// One stereo output frame (interleaved signed 16-bit L, R).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StereoSample {
    /// Left channel.
    pub left: i16,
    /// Right channel.
    pub right: i16,
}

/// Audio Interface state (skeleton).
#[derive(Debug, Default, Clone)]
pub struct Audio {
    /// `AI_DRAM_ADDR` — RDRAM base of the buffer being DMA'd.
    pub dram_addr: u32,
    /// `AI_LEN` — remaining transfer length in bytes.
    pub len: u32,
    /// `AI_DACRATE`-derived output sample rate in Hz.
    pub sample_rate: u32,
    /// DMA-enabled (`AI_CONTROL` bit 0).
    pub dma_enabled: bool,
    // TODO(T-AI-01): the two-deep DMA address/length FIFO, the bitrate divider,
    // and the AI status busy/full flags — see `docs/audio.md`.
}

impl Audio {
    /// Construct at power-on.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Advance the AI by one DMA step, raising the AI interrupt on drain.
    ///
    /// Hot path: keep allocation-free. No-op while DMA is disabled / idle.
    pub fn tick<B: AudioBus>(&mut self, bus: &mut B) {
        if !self.dma_enabled || self.len == 0 {
            return;
        }
        // TODO(T-AI-01): pull the next sample word via `bus.ai_dma_read_u32`,
        // advance `dram_addr` / decrement `len`, and on drain swap the FIFO
        // entry + `bus.raise_ai_interrupt()`.
        let _ = bus;
        self.len = self.len.saturating_sub(4);
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
    impl AudioBus for NullBus {
        fn ai_dma_read_u32(&self, _addr: u32) -> u32 {
            0
        }
    }

    #[test]
    fn idle_tick_is_noop() {
        let mut audio = Audio::new();
        let mut bus = NullBus;
        audio.tick(&mut bus);
        assert_eq!(audio.len, 0);
    }

    #[test]
    fn version_is_non_empty() {
        assert!(!version().is_empty());
    }
}
