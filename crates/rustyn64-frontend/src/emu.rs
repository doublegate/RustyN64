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
//! [`rustyn64_core::Bus::scanout_scaled`], which converts the framebuffer at
//! `VI_ORIGIN` to RGBA8 through the accurate VI pipeline (scale-resample + filters,
//! ledger R-5). While the VI is off or unconfigured (cold
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
    /// The staged video framebuffer, filled each frame from the core's real VI
    /// scan-out (`Bus::scanout_scaled`) — the LLE RDP/VI path, not a test pattern.
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
    /// Produced-frame counter, surfaced via `frame_count` for the status bar.
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

    /// Load and **HLE-boot** a ROM: parse the cart, seed the retail boot state,
    /// and jump into the cart's real IPL3 (`rustyn64_core::boot::hle_boot`) so the
    /// game actually runs. Without the boot the CPU would sit at the PIF reset
    /// vector fetching zeros.
    ///
    /// # Errors
    /// Returns [`rustyn64_core::boot::BootError`] on a too-small image or a cart
    /// the loader cannot parse (unrecognized byte order / truncated header).
    pub fn load_rom(&mut self, raw: &[u8]) -> Result<(), rustyn64_core::boot::BootError> {
        // A warm reset first so a re-load starts from a clean power-on timeline;
        // `hle_boot` then installs the cart and seeds the boot state + entry PC.
        self.system.reset();
        rustyn64_core::boot::hle_boot(&mut self.system, raw)?;
        self.loaded = true;
        self.frames = 0;
        Ok(())
    }

    /// Serialize the whole machine to a save-state blob.
    ///
    /// The cartridge ROM is excluded (it is immutable and up to 64 MiB); it is
    /// re-attached on [`EmuCore::restore`] from the loaded image. Save-states,
    /// rewind, and run-ahead are frontend-owned — the core has no notion of
    /// wall-clock time or snapshots (ADR 0004).
    ///
    /// # Panics
    /// Never in practice: `System` derives `Serialize` for every field, so
    /// bincode encoding of an in-memory value cannot fail.
    #[must_use]
    pub fn snapshot(&self) -> Vec<u8> {
        bincode::serialize(&self.system).expect("System is serde-serializable")
    }

    /// Restore a save-state blob over the current machine, re-attaching the
    /// currently-loaded ROM (the blob excludes it). Returns `false` on a
    /// malformed or mismatched blob, leaving the machine untouched.
    #[must_use]
    pub fn restore(&mut self, blob: &[u8]) -> bool {
        let Ok(mut restored) = bincode::deserialize::<System>(blob) else {
            return false;
        };
        // Re-attach the ROM the snapshot skipped, from the live cart.
        let rom = self.system.bus.cart.rom_image().to_vec();
        restored.bus.cart.reattach_rom(rom);
        self.system = restored;
        // The staged frame/audio are regenerated on the next `run_frame`.
        true
    }

    /// Load a **bare-metal homebrew** image for the wasm demo (no cartridge /
    /// IPL3): copy the payload past the 4 KiB header into RDRAM and jump to the
    /// header's entry point. A demo facility for our committed homebrew ROMs, not
    /// the retail boot ([`EmuCore::load_rom`]) — those go through the real IPL3.
    ///
    /// Every read is length-checked, so a short or empty image degrades
    /// gracefully rather than panicking: a `< 12`-byte image yields entry `0`, and
    /// a `< 0x1000`-byte image copies an empty payload. This is deliberate — the
    /// only caller passes a committed, compile-time [`include_bytes!`] ROM, so a
    /// fallible signature would add error handling at a call site that cannot fail.
    pub fn load_bare_metal_demo(&mut self, rom: &[u8]) {
        self.system.reset();
        // Entry point: header offset 0x08, big-endian, sign-extended to 64 bits.
        let entry = rom.get(8..12).map_or(0, |b| {
            let raw = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
            i64::from(raw.cast_signed()).cast_unsigned()
        });
        let payload = rom.get(0x1000..).unwrap_or(&[]);
        // Real IPL3 copies ROM[0x1000..] into RDRAM at the header entry's physical
        // offset (KSEG0/KSEG1 → `entry & 0x1FFF_FFFF`) and jumps there. Deriving
        // the load destination from the entry keeps the copy and the jump
        // consistent for any bare-metal image, not just ones that happen to link
        // at 0x1000 (our demo ROMs do: entry 0x8000_1000 → offset 0x1000). The
        // checked `get_mut` also makes an (impossibly) small RDRAM a no-op copy
        // rather than a panic on the range slice.
        let dst_off = (entry & 0x1FFF_FFFF) as usize;
        if let Some(dst) = self.system.bus.rdram.get_mut(dst_off..) {
            let n = payload.len().min(dst.len());
            dst[..n].copy_from_slice(&payload[..n]);
        }
        self.system.cpu.set_pc(entry);
        self.loaded = true;
        self.frames = 0;
    }

    /// The most-recently produced frame's RGBA8 pixels and active dimensions,
    /// for a caller that blits directly (e.g. the wasm canvas demo).
    #[must_use]
    pub fn frame_rgba(&self) -> (&[u8], u32, u32) {
        (&self.frame.rgba, self.frame.w, self.frame.h)
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
        // The ADR 0011 fast path when it is compiled in, the accurate scheduler
        // otherwise. Additive and off by default: with `fast-scheduler` disabled
        // this is `run_until` and the binary is unchanged, which is ADR 0011 §1.
        //
        // The two are not "approximately equal" — the block replays the same edge
        // schedule rather than a different one, and the differential gate in
        // `rustyn64-core` requires byte-identical machine state across every phase
        // alignment, every partial-period tail, and a real commercial title.
        // The run report is ADR 0012's gate witness (which blocks ran, which
        // bail-out reasons were reached). The frontend has nothing to do with it, so
        // it is discarded explicitly rather than by the `#[must_use]` being absent —
        // an emulator that changed behavior on a diagnostic would be the ADR 0011 §6
        // defect of a released path shaped by its tests.
        //
        // `fast-exec` takes precedence where both are enabled (ADR 0013 §1), which
        // is settled here rather than left to whichever `cfg` happens to be written
        // first. The three arms are mutually exclusive by construction, so exactly
        // one entry point is compiled in any configuration.
        #[cfg(feature = "fast-exec")]
        let _ = self.system.run_until_exec(target);
        #[cfg(all(feature = "fast-scheduler", not(feature = "fast-exec")))]
        let _ = self.system.run_until_fast(target);
        #[cfg(not(any(feature = "fast-scheduler", feature = "fast-exec")))]
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

    /// Copy this core's active frame and its status-bar readings into a present
    /// handoff.
    ///
    /// One implementation on purpose: both producers — the dedicated emu thread
    /// and the synchronous `emu-thread`-off path — publish through here, so they
    /// cannot drift into disagreeing about what a published frame is.
    ///
    /// Only the `w * h * 4` **prefix** of `frame.rgba` is published, not the whole
    /// `FB_MAX`-sized backing store, which `produce_frame` never resizes. A frame
    /// whose dims are not covered by its bytes is skipped rather than truncated: a
    /// partial frame is not a frame, and
    /// [`PresentBuffer::publish`](crate::present_buffer::PresentBuffer::publish)
    /// requires the pair to agree. `presentable_geometry` clamps the dims to the blit
    /// texture, so the guard is for a future geometry change rather than a
    /// currently-reachable case.
    pub fn publish_into(&self, present: &crate::present_buffer::PresentBuffer) {
        let (rgba, w, h) = self.frame_rgba();
        let need = (w as usize).saturating_mul(h as usize).saturating_mul(4);
        // Loud in debug, tolerant in release. `presentable_geometry` makes this
        // unreachable today, so if it ever trips it is a core-side bug that deserves
        // a panic in development rather than a silently frozen picture.
        debug_assert!(
            need > 0 && need <= rgba.len(),
            "produce_frame yielded {w}x{h}, needing {need} bytes of {}",
            rgba.len()
        );
        if need > 0 && need <= rgba.len() {
            present.publish(&rgba[..need], (w, h));
        }
        present.publish_status(
            self.frame_count(),
            self.master_ticks(),
            self.is_loaded(),
            self.is_paused(),
        );
    }

    /// Scan the core's framebuffer out into the presented frame.
    ///
    /// `Bus::scanout_scaled` reads the VI registers and converts the framebuffer at
    /// `VI_ORIGIN` to RGBA8 through the **accurate** VI pipeline (the real
    /// `VI_X_SCALE`/`VI_Y_SCALE` resampling, active-span/overscan geometry, and the
    /// coverage/gamma post-filters — ledger R-5), replacing the earlier 1:1
    /// `Bus::scanout`. It **self-guards against a buffer overrun** from untrusted VI
    /// registers: it returns `(0, 0)` and writes nothing when its output cannot hold
    /// `w * h * 4` bytes, so a ROM cannot make it overrun `frame.rgba` (sized
    /// `FB_MAX_W * FB_MAX_H`). It also returns `(0, 0)` while the VI is off or
    /// unconfigured (cold boot / no ROM).
    ///
    /// Two guards keep an over-large geometry off the screen. `scanout_scaled` bounds
    /// its width by the DAC prescale (≤ `FB_MAX_W`) but its height can reach the
    /// 625-line prescale: a field whose `w * h * 4` overflows `frame.rgba` trips
    /// `scanout_scaled`'s own `(0, 0)` self-guard, and any other dimension past
    /// `FB_MAX` (in practice a tall height) is rejected by `presentable_geometry`.
    /// Either way — and while the VI is off — the frame is presented black at the
    /// default resolution, and the whole buffer is cleared so no stale pixels from a
    /// previous, larger frame survive.
    fn produce_frame(&mut self) {
        let (w, h) = self.system.bus.scanout_scaled(&mut self.frame.rgba);
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

    #[test]
    fn bare_metal_demo_boots_and_frame_rgba_is_exact_size() {
        // The committed license-clean homebrew the wasm demo blits: it CPU-fills a
        // framebuffer and programs the VI, so a frame renders through the LLE path.
        // The VI becoming programmed (w,h > 0) transitively proves the loader set
        // the entry PC and copied the payload — the code only runs if both are right.
        const DEMO: &[u8] = include_bytes!("../../../tests/roms/homebrew/render_fill.z64");
        let mut emu = EmuCore::new(0);
        emu.load_bare_metal_demo(DEMO);
        assert!(emu.is_loaded());
        // Drive frames until the VI is configured and scans out a real picture.
        for _ in 0..8 {
            emu.run_frame();
        }
        let (rgba, w, h) = emu.frame_rgba();
        assert!(w > 0 && h > 0, "demo ROM should program the VI");
        // The invariant the wasm blit relies on: the backing buffer always holds
        // at least the active w*h*4 bytes, so an exact-size ImageData never fails.
        assert!(rgba.len() >= (w * h * 4) as usize);
    }

    #[test]
    fn bare_metal_demo_survives_a_truncated_image() {
        // A short image must degrade gracefully (no panic / no corruption): the
        // loader length-checks every read, so this is a no-op-ish load, and a
        // subsequent frame must still run without panicking.
        let mut emu = EmuCore::new(0);
        emu.load_bare_metal_demo(&[0u8; 4]);
        assert!(emu.is_loaded());
        emu.run_frame();
        assert_eq!(emu.frame_count(), 1);
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

    /// With no ROM the VI is off, so `Bus::scanout_scaled` returns `(0, 0)` and
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

    /// **`produce_frame` presents the accurate scaled scan-out, not the 1:1 copy.**
    /// Programs a 16-bit VI whose active-span/overscan geometry makes the accurate
    /// `Bus::scanout_scaled` dims differ from the 1:1 `Bus::scanout` dims, asserts the
    /// two genuinely disagree (so the test can tell them apart), then confirms the
    /// frame `produce_frame` presents carries the `scanout_scaled` dims — pinning that
    /// the frontend routes through the accurate path.
    #[test]
    fn produce_frame_presents_the_scaled_scan_out() {
        use rustyn64_core::cpu::Bus as CpuBus;
        const VI: u32 = 0x0440_0000;
        let mut emu = EmuCore::new(0);
        // 16-bit REPLICATE, 80-px source at 0x1000, NTSC field, the same active-span
        // geometry the VI conformance vectors use (output crops to 25x16).
        CpuBus::write_u32(&mut emu.system.bus, VI, 0x0000_0302); // VI_CTRL
        CpuBus::write_u32(&mut emu.system.bus, VI + 0x04, 0x1000); // VI_ORIGIN
        CpuBus::write_u32(&mut emu.system.bus, VI + 0x08, 80); // VI_WIDTH
        CpuBus::write_u32(&mut emu.system.bus, VI + 0x18, 525); // VI_V_TOTAL (NTSC)
        CpuBus::write_u32(&mut emu.system.bus, VI + 0x24, 0x006C_0094); // VI_H_VIDEO
        CpuBus::write_u32(&mut emu.system.bus, VI + 0x28, 0x0022_0042); // VI_V_VIDEO
        CpuBus::write_u32(&mut emu.system.bus, VI + 0x30, 0x0000_0400); // VI_X_SCALE
        CpuBus::write_u32(&mut emu.system.bus, VI + 0x34, 0x0000_0400); // VI_Y_SCALE

        let mut scaled = vec![0u8; (FB_MAX_W * FB_MAX_H * 4) as usize];
        let scaled_dims = emu.system.bus.scanout_scaled(&mut scaled);
        let mut one_to_one = vec![0u8; (FB_MAX_W * FB_MAX_H * 4) as usize];
        let one_to_one_dims = emu.system.bus.scanout(&mut one_to_one);
        assert_ne!(
            scaled_dims, one_to_one_dims,
            "test setup must distinguish the accurate and 1:1 scan-out geometries"
        );
        assert_eq!(
            scaled_dims,
            (25, 16),
            "accurate scan-out crops to the active span"
        );

        emu.produce_frame();
        assert_eq!(
            (emu.frame().w, emu.frame().h),
            scaled_dims,
            "produce_frame presents the scanout_scaled geometry"
        );
    }

    /// **The frontend boots a real game** through `EmuCore::load_rom` → the core's
    /// HLE boot → the cart's own IPL3, so the CPU ends up executing game code in
    /// RDRAM (`0x8xxx_xxxx`) rather than spinning at the PIF reset vector. Local
    /// only: reads the gitignored commercial corpus, skips gracefully when absent.
    ///
    /// This is the Sprint-1 proof that the shell can *run* a cartridge — before
    /// the boot-to-core move, `load_rom` left the CPU fetching zeros at
    /// `0xBFC0_0000`. (Reaching a rendered frame is a separate, R-18 gap: a
    /// commercial ROM boots and executes but does not program the VI yet.)
    #[test]
    #[ignore = "local-only: reads the gitignored commercial corpus"]
    fn the_frontend_boots_a_commercial_rom_into_rdram() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/roms/external/commercial/eeprom-4k/Super Mario 64.z64"
        );
        let Ok(raw) = std::fs::read(path) else {
            eprintln!("no commercial ROM staged — frontend boot test skipped");
            return;
        };
        let mut emu = EmuCore::new(0);
        emu.load_rom(&raw).expect("HLE boot");
        for _ in 0..60 {
            emu.run_frame();
        }
        let pc = emu.system.cpu.pc & 0xFFFF_FFFF;
        assert_eq!(
            pc & 0xE000_0000,
            0x8000_0000,
            "the frontend must boot the game into RDRAM, not spin at the reset vector (pc={pc:#010x})"
        );
        assert!(
            emu.system.cpu.retired > 1_000_000,
            "the game must actually execute (retired={})",
            emu.system.cpu.retired
        );
    }
}
