//! `EmuCore` — the frontend's owner of the `rustyn64-core` [`System`].
//!
//! Wraps the deterministic core in the frontend-side state that must NOT live in
//! the core (the determinism contract): the produced video framebuffer staging,
//! the drained audio, and the per-frame controller latch. Run-ahead and rate
//! control are orchestrated above this, never inside the core's synthesis.
//!
//! # Video source
//!
//! The presented frame comes from the core's VI scan-out: `produce_frame` calls
//! [`rustyn64_core::Bus::scanout`], which converts the framebuffer at `VI_ORIGIN`
//! to RGBA8 (the LLE RDP/VI path). While the VI is off or unconfigured (cold
//! boot / no ROM), scan-out reports `(0, 0)` and a black frame at the default
//! resolution is shown. Audio is still a frontend-side placeholder until the
//! AI/RSP drain lands.

use rustyn64_core::System;

use crate::{FB_DEFAULT_H, FB_DEFAULT_W, FB_MAX_H, FB_MAX_W};

/// Canonical master ticks per emulated video frame at ~60 Hz NTSC.
///
/// `187_500_000 / 60` — the 187.5 MHz master tick of ADR 0006, NOT the VR4300
/// cycle. The pacer is wall-clock authoritative, so this only sets how much the
/// core advances per produced frame in the skeleton.
const MASTER_TICKS_PER_FRAME: u64 = rustyn64_core::MASTER_HZ / 60;

/// A produced video frame: an RGBA8 buffer plus its active dimensions.
///
/// The N64 VI resolution is variable; `w`/`h` give the active sub-rectangle the
/// blit uploads from the `FB_MAX_W * FB_MAX_H` backing store.
#[derive(Clone, Debug)]
pub struct Frame {
    /// RGBA8 pixels, row-major, `w * h * 4` bytes are valid.
    pub rgba: Vec<u8>,
    /// Active framebuffer width (320 or 640 in the common modes).
    pub w: u32,
    /// Active framebuffer height (240 or 480 in the common modes).
    pub h: u32,
}

impl Frame {
    /// A black frame at the default 320x240 resolution.
    #[must_use]
    pub fn blank() -> Self {
        let (w, h) = (FB_DEFAULT_W, FB_DEFAULT_H);
        Self {
            rgba: vec![0u8; (FB_MAX_W * FB_MAX_H * 4) as usize],
            w,
            h,
        }
    }
}

/// The frontend's emulator handle: the core `System` plus produced A/V staging.
#[derive(Debug)]
pub struct EmuCore {
    /// The deterministic core.
    system: System,
    /// The staged video framebuffer (placeholder until the RDP scanout lands).
    frame: Frame,
    /// Drained audio samples (interleaved stereo f32), consumed by the ring.
    audio: Vec<f32>,
    /// Produced-frame counter (drives the skeleton's test pattern).
    frames: u64,
    /// `true` while paused (the pacer keeps running, the core does not advance).
    paused: bool,
    /// A ROM has been loaded.
    loaded: bool,
}

impl EmuCore {
    /// Power on with a determinism seed.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            system: System::new(seed),
            frame: Frame::blank(),
            audio: Vec::new(),
            frames: 0,
            paused: false,
            loaded: false,
        }
    }

    /// Load a normalized ROM image into the cart.
    ///
    /// # Errors
    /// Returns the [`rustyn64_core::cart::CartError`] from the loader on an
    /// unrecognized byte order or a truncated header.
    pub fn load_rom(&mut self, raw: &[u8]) -> Result<(), rustyn64_core::cart::CartError> {
        let cart = rustyn64_core::cart::Cart::load(raw)?;
        self.system.bus.cart = cart;
        self.system.reset();
        self.loaded = true;
        self.frames = 0;
        Ok(())
    }

    /// `true` once a ROM has been loaded.
    #[must_use]
    pub const fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// Pause / resume (the core stops advancing; the pacer keeps running).
    pub const fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    /// Whether the core is paused.
    #[must_use]
    pub const fn is_paused(&self) -> bool {
        self.paused
    }

    /// Latch the per-port controller words for the SI joybus.
    pub const fn set_controllers(&mut self, ports: [u32; 4]) {
        self.system.bus.controllers = ports;
    }

    /// Reset (warm) — re-runs the seeded phase alignment, preserving determinism.
    pub fn reset(&mut self) {
        self.system.reset();
    }

    /// Run one emulated frame's worth of master ticks, then stage the produced
    /// frame. The pacer decides *when* this is called; rate control lives above.
    pub fn run_frame(&mut self) {
        if self.paused {
            return;
        }
        // Edge-to-edge: the master tick is a time base, never an iteration count.
        let target = self
            .system
            .master_ticks()
            .saturating_add(MASTER_TICKS_PER_FRAME);
        self.system.run_until(target);
        self.frames = self.frames.wrapping_add(1);
        self.produce_frame();
        self.produce_audio();
    }

    /// The most-recently produced video frame.
    #[must_use]
    pub const fn frame(&self) -> &Frame {
        &self.frame
    }

    /// Drain the produced audio samples (interleaved stereo f32). The caller
    /// pushes them into the lock-free ring.
    pub fn drain_audio(&mut self) -> Vec<f32> {
        core::mem::take(&mut self.audio)
    }

    /// Elapsed master (VR4300) ticks since power-on (a status-bar diagnostic).
    #[must_use]
    pub const fn master_ticks(&self) -> u64 {
        self.system.master_ticks()
    }

    /// Produced frame count.
    #[must_use]
    pub const fn frame_count(&self) -> u64 {
        self.frames
    }

    /// Borrow the core read-only (the debugger panel reads VR4300 state here).
    #[must_use]
    pub const fn system(&self) -> &System {
        &self.system
    }

    /// Scan the core's framebuffer out into the presented frame.
    ///
    /// `Bus::scanout` reads the VI registers and converts the framebuffer at
    /// `VI_ORIGIN` to RGBA8 (the LLE RDP/VI path). It returns `(0, 0)` while the
    /// VI is off or unconfigured — the cold-boot / no-ROM state — in which case a
    /// black frame at the default resolution is presented. A reported geometry
    /// larger than the backing store also falls back rather than overrun it.
    fn produce_frame(&mut self) {
        let (w, h) = self.system.bus.scanout(&mut self.frame.rgba);
        if w == 0 || h == 0 || w > FB_MAX_W || h > FB_MAX_H {
            self.frame.w = FB_DEFAULT_W;
            self.frame.h = FB_DEFAULT_H;
            let n = (FB_DEFAULT_W * FB_DEFAULT_H * 4) as usize;
            self.frame.rgba[..n].fill(0);
        } else {
            self.frame.w = w;
            self.frame.h = h;
        }
    }

    /// SKELETON: produce silence. Replaced by the AI/RSP audio drain when the
    /// audio pipeline lands.
    fn produce_audio(&mut self) {
        // ~800 stereo sample-pairs per 60 Hz frame at 48 kHz.
        self.audio.resize(800 * 2, 0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advances_master_ticks_on_run_frame() {
        let mut emu = EmuCore::new(0);
        emu.loaded = true;
        let before = emu.master_ticks();
        emu.run_frame();
        assert!(emu.master_ticks() > before);
        assert_eq!(emu.frame_count(), 1);
    }

    #[test]
    fn paused_does_not_advance() {
        let mut emu = EmuCore::new(0);
        emu.set_paused(true);
        let before = emu.master_ticks();
        emu.run_frame();
        assert_eq!(emu.master_ticks(), before);
    }

    #[test]
    fn frame_has_default_dims() {
        let emu = EmuCore::new(0);
        assert_eq!(emu.frame().w, FB_DEFAULT_W);
        assert_eq!(emu.frame().h, FB_DEFAULT_H);
    }

    /// With no ROM the VI is off, so `Bus::scanout` returns `(0, 0)` and
    /// `produce_frame` presents a black frame at the default resolution — the
    /// wiring falls back rather than blitting stale/garbage memory.
    #[test]
    fn produce_frame_falls_back_to_black_when_the_vi_is_off() {
        let mut emu = EmuCore::new(0);
        emu.run_frame();
        assert_eq!(emu.frame().w, FB_DEFAULT_W);
        assert_eq!(emu.frame().h, FB_DEFAULT_H);
        let n = (FB_DEFAULT_W * FB_DEFAULT_H * 4) as usize;
        assert!(
            emu.frame().rgba[..n].iter().all(|&b| b == 0),
            "black frame while the VI is off"
        );
    }
}
