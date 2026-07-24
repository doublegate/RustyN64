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
//! resolution is shown.
//!
//! # Audio source
//!
//! `produce_audio` drains the AI's emitted stereo stream ([`rustyn64_core::Bus::drain_audio_samples`])
//! and resamples it from the N64 output rate to the host device rate
//! (`resample_stereo`), staging interleaved f32 for the `cpal` ring. The
//! resample is the frontend's non-deterministic host-timing stage (ADR 0004);
//! the core only ever emits on the emulated timeline.

use rustyn64_core::System;
use rustyn64_core::audio::StereoSample;

use crate::{FB_DEFAULT_H, FB_DEFAULT_W, FB_MAX_H, FB_MAX_W};

/// Canonical master ticks per emulated video frame at ~60 Hz NTSC.
///
/// `187_500_000 / 60` — the 187.5 MHz master tick of ADR 0006, NOT the VR4300
/// cycle. The pacer is wall-clock authoritative, so this only sets how much the
/// core advances per produced frame in the skeleton.
const MASTER_TICKS_PER_FRAME: u64 = rustyn64_core::MASTER_HZ / 60;

/// The default host output rate (Hz) before a `cpal` device reports its own.
const DEFAULT_OUTPUT_RATE: u32 = 48_000;

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
    /// The host device output rate (Hz) the emitted N64 stream is resampled to.
    /// Set from the opened `cpal` device; defaults to 48 kHz.
    output_rate: u32,
    /// The linear resampler's carried fractional input position, so the N64→host
    /// rate conversion stays continuous (click-free) across frames. Frontend-only
    /// state — the deterministic core never sees it (ADR 0004).
    resample_pos: f64,
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
            output_rate: DEFAULT_OUTPUT_RATE,
            resample_pos: 0.0,
            frames: 0,
            paused: false,
            loaded: false,
        }
    }

    /// Set the host device output sample rate (Hz) — the target the emitted N64
    /// stream is resampled to. Called when the `cpal` device opens; a zero or
    /// unchanged rate is ignored.
    pub const fn set_output_rate(&mut self, rate: u32) {
        if rate != 0 {
            self.output_rate = rate;
        }
    }

    /// Observed AI buffer underruns (starvations) — surfaced so the frontend /
    /// harness can see them rather than the resampler silently concealing them.
    #[must_use]
    pub const fn audio_underruns(&self) -> u64 {
        self.system.bus.audio.underruns()
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
    /// `VI_ORIGIN` to RGBA8 (the LLE RDP/VI path). It **self-guards against a
    /// buffer overrun** from untrusted VI registers: it returns `(0, 0)` and
    /// writes nothing when its output cannot hold `w * h * 4` bytes, so a ROM
    /// cannot make it overrun `frame.rgba`. It also returns `(0, 0)` while the VI
    /// is off or unconfigured (cold boot / no ROM).
    ///
    /// A returned `(0, 0)` — or a valid-but-oversized geometry that fits the
    /// backing store yet exceeds the blit's `FB_MAX` texture — is presented as a
    /// black frame at the default resolution; the whole buffer is cleared so no
    /// stale pixels from a previous, larger frame survive.
    fn produce_frame(&mut self) {
        let (w, h) = self.system.bus.scanout(&mut self.frame.rgba);
        if let Some((w, h)) = presentable_geometry(w, h) {
            self.frame.w = w;
            self.frame.h = h;
        } else {
            self.frame.w = FB_DEFAULT_W;
            self.frame.h = FB_DEFAULT_H;
            self.frame.rgba.fill(0);
        }
    }

    /// Drain the AI's emitted stereo stream and resample it from the N64 output
    /// rate to the host device rate, staging interleaved f32 for the ring.
    ///
    /// The resample (a non-deterministic host-timing stage) lives here in the
    /// frontend, never in the core (ADR 0004). When the AI is idle (no rate
    /// programmed, or nothing emitted this frame) a frame of silence keeps the
    /// ring fed without pretending audio played.
    fn produce_audio(&mut self) {
        self.audio.clear();
        let in_rate = self.system.bus.audio.sample_rate();
        let samples = self.system.bus.drain_audio_samples();
        if in_rate == 0 || samples.is_empty() {
            // Idle: one frame of silence at the host rate (keeps the ring fed).
            let pairs = (self.output_rate / 60) as usize;
            self.audio.resize(pairs * 2, 0.0);
            return;
        }
        resample_stereo(
            &samples,
            in_rate,
            self.output_rate,
            &mut self.resample_pos,
            &mut self.audio,
        );
    }
}

/// The geometry to present from a scan-out `(w, h)`: `Some((w, h))` when it is a
/// non-empty frame that fits the blit's `FB_MAX` texture, or `None` (present a
/// black default frame) for the blank `(0, 0)` case or a geometry that would
/// exceed the texture. Pure so the boundary is unit-testable without the VI.
const fn presentable_geometry(w: u32, h: u32) -> Option<(u32, u32)> {
    if w == 0 || h == 0 || w > FB_MAX_W || h > FB_MAX_H {
        None
    } else {
        Some((w, h))
    }
}

/// Linearly resample an interleaved stereo `i16` stream from `in_rate` to
/// `out_rate`, appending interleaved `f32` (`[-1, 1)`) to `out`.
///
/// `pos` is the fractional input position carried across calls so the rate
/// conversion is continuous — the remainder past the last consumed input sample
/// is preserved for the next frame, which is what keeps successive frames
/// click-free. This is the frontend's non-deterministic host-rate stage
/// (ADR 0004); a windowed / servo-controlled resampler is a later refinement,
/// so at a frame boundary the final output sample interpolates against the last
/// input sample rather than the (not-yet-known) next frame's first sample.
#[allow(
    clippy::while_float,
    reason = "walking a fractional input cursor is the natural resampler loop"
)]
fn resample_stereo(
    input: &[StereoSample],
    in_rate: u32,
    out_rate: u32,
    pos: &mut f64,
    out: &mut Vec<f32>,
) {
    debug_assert!(in_rate > 0 && out_rate > 0, "rates must be non-zero");
    // Input samples consumed per output sample.
    let step = f64::from(in_rate) / f64::from(out_rate);
    let len = input.len() as f64;
    let to_f32 = |v: i16| f32::from(v) / 32_768.0;
    let mut cursor = *pos;
    while cursor < len {
        let idx = cursor as usize;
        let frac = (cursor - cursor.floor()) as f32;
        let cur = input[idx];
        let nxt = *input.get(idx + 1).unwrap_or(&cur);
        out.push((to_f32(nxt.left) - to_f32(cur.left)).mul_add(frac, to_f32(cur.left)));
        out.push((to_f32(nxt.right) - to_f32(cur.right)).mul_add(frac, to_f32(cur.right)));
        cursor += step;
    }
    // Carry the fractional remainder into the next frame's input space.
    *pos = cursor - len;
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

    fn sample(l: i16, r: i16) -> StereoSample {
        StereoSample { left: l, right: r }
    }

    #[test]
    fn resample_identity_passes_samples_through() {
        // Equal rates: each input sample maps to one output pair, i16→f32.
        let input = [sample(16384, -16384), sample(-32768, 32767)];
        let mut pos = 0.0;
        let mut out = Vec::new();
        resample_stereo(&input, 48_000, 48_000, &mut pos, &mut out);
        assert_eq!(out.len(), 4, "two pairs in, two pairs out");
        assert!((out[0] - 0.5).abs() < 1e-6);
        assert!((out[1] + 0.5).abs() < 1e-6);
        assert!((out[2] + 1.0).abs() < 1e-6);
    }

    #[test]
    fn resample_downsamples_and_upsamples_by_rate() {
        let input: Vec<_> = (0..100).map(|i| sample(i, i)).collect();
        // Halving the rate yields ~half as many output pairs.
        let mut pos = 0.0;
        let mut down = Vec::new();
        resample_stereo(&input, 48_000, 24_000, &mut pos, &mut down);
        assert!(
            (45..=55).contains(&(down.len() / 2)),
            "downsample ~50 pairs, got {}",
            down.len() / 2
        );
        // Doubling the rate yields ~twice as many.
        let mut pos = 0.0;
        let mut up = Vec::new();
        resample_stereo(&input, 24_000, 48_000, &mut pos, &mut up);
        assert!(
            (195..=205).contains(&(up.len() / 2)),
            "upsample ~200 pairs, got {}",
            up.len() / 2
        );
    }

    #[test]
    fn resample_carries_position_across_frames() {
        // Ten 30-sample frames must resample to essentially the same total as one
        // 300-sample frame: the carried `pos` bounds the rounding error to O(1)
        // overall, whereas a naive per-frame reset would round up every frame and
        // over-produce by ~10 pairs. Non-integer ratio (32k→48k) so the fraction
        // actually carries.
        let mut pos = 0.0;
        let mut split = Vec::new();
        for f in 0..10 {
            let frame: Vec<_> = (0..30).map(|i| sample(f * 30 + i, f * 30 + i)).collect();
            resample_stereo(&frame, 32_000, 48_000, &mut pos, &mut split);
        }
        let whole: Vec<_> = (0..300).map(|i| sample(i, i)).collect();
        let (mut pos1, mut one) = (0.0, Vec::new());
        resample_stereo(&whole, 32_000, 48_000, &mut pos1, &mut one);

        let diff = (split.len() as i64 - one.len() as i64).abs();
        assert!(
            diff <= 2,
            "carried position keeps the rate stable across frames (diff={diff})"
        );
    }

    #[test]
    fn idle_produce_audio_is_silence_at_the_output_rate() {
        // No ROM → the AI never programs a rate → a frame of host-rate silence.
        let mut emu = EmuCore::new(0);
        emu.set_output_rate(48_000);
        emu.loaded = true;
        emu.run_frame();
        let a = emu.drain_audio();
        assert_eq!(a.len(), (48_000 / 60) * 2, "one frame of stereo silence");
        assert!(a.iter().all(|&s| s == 0.0), "idle output is silent");
        assert_eq!(emu.audio_underruns(), 0, "no starvation while idle");
    }

    /// **The presented-geometry clamp accepts fits and rejects zero/oversized.**
    /// Zero dimensions and any geometry past the `FB_MAX` blit texture fall back;
    /// a fit (including exactly `FB_MAX`) is presented as-is.
    #[test]
    fn presentable_geometry_clamps_zero_and_oversized() {
        assert_eq!(presentable_geometry(320, 240), Some((320, 240)));
        assert_eq!(
            presentable_geometry(FB_MAX_W, FB_MAX_H),
            Some((FB_MAX_W, FB_MAX_H)),
            "exactly FB_MAX fits"
        );
        assert_eq!(presentable_geometry(0, 0), None, "blank");
        assert_eq!(presentable_geometry(0, 240), None, "zero width");
        assert_eq!(presentable_geometry(FB_MAX_W + 1, 1), None, "too wide");
        assert_eq!(presentable_geometry(1, FB_MAX_H + 1), None, "too tall");
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
