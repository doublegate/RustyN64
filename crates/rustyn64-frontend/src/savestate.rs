//! Frontend save-states: a rewind ring and run-ahead (ADR 0004).
//!
//! Save-states, rewind, and run-ahead are the frontend-side conveniences the
//! determinism contract explicitly assigns to the frontend: they snapshot and
//! restore the deterministic core, but consult wall-clock time / config / input
//! themselves. The core never learns what time it is. All of this is **off by
//! default** — with it off the emulator is byte-identical to one built without
//! it (the byte-identity contract).
//!
//! - [`EmuCore::snapshot`]/[`EmuCore::restore`] are the serialize/restore
//!   primitives (bincode over the whole `System`; the ROM is re-attached).
//! - [`RewindRing`] keeps a bounded ring of recent snapshots so the player can
//!   step backwards.
//! - [`RunAhead`] runs the emulation a few frames ahead of what is shown, using
//!   the latest input, to hide input latency — then rewinds the speculative work
//!   so the real timeline is never advanced by it.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::emu::EmuCore;

/// Cross-thread save-state requests: the UI thread sets them (from a hotkey /
/// menu), the emulation thread serves them on its next frame. A single manual
/// save slot lives here too.
#[derive(Debug, Default)]
pub struct SaveStateControls {
    rewind: AtomicBool,
    save: AtomicBool,
    load: AtomicBool,
    slot: Mutex<Option<Vec<u8>>>,
}

impl SaveStateControls {
    /// Request a one-step rewind on the next frame.
    pub fn request_rewind(&self) {
        self.rewind.store(true, Ordering::Relaxed);
    }
    /// Request a manual save-state (into the slot) on the next frame.
    pub fn request_save(&self) {
        self.save.store(true, Ordering::Relaxed);
    }
    /// Request restoring the manual save-state slot on the next frame.
    pub fn request_load(&self) {
        self.load.store(true, Ordering::Relaxed);
    }
    /// Is a manual save-state currently stored?
    #[must_use]
    pub fn has_slot(&self) -> bool {
        self.slot.lock().is_ok_and(|s| s.is_some())
    }

    fn take(flag: &AtomicBool) -> bool {
        flag.swap(false, Ordering::Relaxed)
    }
    fn store_slot(&self, blob: Vec<u8>) {
        if let Ok(mut s) = self.slot.lock() {
            *s = Some(blob);
        }
    }
    fn slot_clone(&self) -> Option<Vec<u8>> {
        self.slot.lock().ok().and_then(|s| s.clone())
    }
}

/// Drives save-states, rewind, and run-ahead for one machine, per produced
/// frame. Lives on the emulation thread; the UI thread pokes it via
/// [`SaveStateControls`].
#[derive(Debug)]
pub struct SaveStateCoordinator {
    ring: RewindRing,
    run_ahead: RunAhead,
    controls: std::sync::Arc<SaveStateControls>,
}

impl SaveStateCoordinator {
    /// Build a coordinator from config + the shared controls.
    #[must_use]
    pub const fn new(
        rewind: RewindConfig,
        run_ahead: RunAhead,
        controls: std::sync::Arc<SaveStateControls>,
    ) -> Self {
        Self {
            ring: RewindRing::new(rewind),
            run_ahead,
            controls,
        }
    }

    /// Serve pending controls, run one displayed frame (with run-ahead), and
    /// capture it into the rewind ring. Returns the audio to play.
    pub fn step(&mut self, core: &mut EmuCore) -> Vec<f32> {
        // Restore requests happen before the frame runs.
        if SaveStateControls::take(&self.controls.load)
            && let Some(blob) = self.controls.slot_clone()
        {
            let _ = core.restore(&blob);
        }
        if SaveStateControls::take(&self.controls.rewind) {
            self.ring.rewind(core);
        }

        let audio = self.run_ahead.step(core);
        self.ring.on_frame(core);

        // A save captures the just-produced committed state.
        if SaveStateControls::take(&self.controls.save) {
            self.controls.store_slot(core.snapshot());
        }
        audio
    }

    /// Drop retained rewind snapshots (e.g. on a new ROM load).
    pub fn clear_rewind(&mut self) {
        self.ring.clear();
    }
}

/// Rewind configuration. Off by default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RewindConfig {
    /// Master switch. When `false`, [`RewindRing`] captures nothing and costs
    /// nothing.
    pub enabled: bool,
    /// Capture a snapshot every this-many produced frames. Coarser = less memory
    /// and CPU, but chunkier rewind granularity.
    pub interval_frames: u32,
    /// Maximum snapshots retained — the ring bound (older ones are dropped). At
    /// ~8 MiB RDRAM per snapshot this caps the memory the ring can use.
    pub capacity: usize,
}

impl Default for RewindConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_frames: 6,
            capacity: 120,
        }
    }
}

/// A bounded ring of recent machine snapshots for rewind.
#[derive(Debug)]
pub struct RewindRing {
    cfg: RewindConfig,
    snaps: std::collections::VecDeque<Vec<u8>>,
    since_capture: u32,
}

impl RewindRing {
    /// A ring with the given config (empty until frames are captured).
    #[must_use]
    pub const fn new(cfg: RewindConfig) -> Self {
        Self {
            cfg,
            snaps: std::collections::VecDeque::new(),
            since_capture: 0,
        }
    }

    /// Whether rewind capture is enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.cfg.enabled
    }

    /// How many snapshots are currently retained.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.snaps.len()
    }

    /// Call once per produced frame. When enabled, captures a snapshot every
    /// `interval_frames`, dropping the oldest past `capacity`.
    pub fn on_frame(&mut self, core: &EmuCore) {
        if !self.cfg.enabled {
            return;
        }
        self.since_capture += 1;
        if self.since_capture >= self.cfg.interval_frames {
            self.since_capture = 0;
            if self.snaps.len() >= self.cfg.capacity {
                self.snaps.pop_front();
            }
            self.snaps.push_back(core.snapshot());
        }
    }

    /// Rewind one step: restore the most recent snapshot into `core`. Returns
    /// `false` if the ring is empty (nothing to rewind to).
    pub fn rewind(&mut self, core: &mut EmuCore) -> bool {
        match self.snaps.pop_back() {
            Some(blob) => {
                // Our own blobs are always valid; ignore the bool.
                let _ = core.restore(&blob);
                self.since_capture = 0;
                true
            }
            None => false,
        }
    }

    /// Drop all retained snapshots (e.g. on a new ROM load or a config change).
    pub fn clear(&mut self) {
        self.snaps.clear();
        self.since_capture = 0;
    }
}

/// Run-ahead: hide `frames` of input latency by presenting a speculatively
/// advanced frame, without advancing the real timeline by it. `frames == 0`
/// disables it (a plain `run_frame`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RunAhead {
    /// Frames of latency to hide. 0 = off (the default).
    pub frames: u32,
}

impl RunAhead {
    /// Whether run-ahead is active.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.frames > 0
    }

    /// Advance one **displayed** frame and return the audio to play.
    ///
    /// With run-ahead off this is a plain `run_frame` and its audio. With
    /// `frames = N`: run the real (committed) frame and keep its audio, snapshot,
    /// run `N` speculative frames on the same latched input, then restore to the
    /// committed snapshot — leaving the *system* back at the committed point but
    /// the staged *video frame* at the speculative-ahead position (which the
    /// caller then presents). Speculative audio is discarded so it is never heard
    /// twice.
    pub fn step(&self, core: &mut EmuCore) -> Vec<f32> {
        if self.frames == 0 {
            core.run_frame();
            return core.drain_audio();
        }
        core.run_frame();
        let committed_audio = core.drain_audio();
        let snapshot = core.snapshot();
        for _ in 0..self.frames {
            core.run_frame();
        }
        let _ = core.drain_audio(); // discard speculative audio
        let _ = core.restore(&snapshot); // system back to committed; frame stays ahead
        committed_audio
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_disabled_ring_captures_nothing() {
        let mut ring = RewindRing::new(RewindConfig::default());
        let core = EmuCore::new(0);
        for _ in 0..100 {
            ring.on_frame(&core);
        }
        assert_eq!(ring.depth(), 0, "off by default: no capture, no cost");
        assert!(!ring.is_enabled());
    }

    #[test]
    fn the_ring_captures_on_interval_and_stays_bounded() {
        let cfg = RewindConfig {
            enabled: true,
            interval_frames: 2,
            capacity: 3,
        };
        let mut ring = RewindRing::new(cfg);
        let core = EmuCore::new(0);
        // 20 frames / interval 2 = 10 captures, but capacity caps at 3.
        for _ in 0..20 {
            ring.on_frame(&core);
        }
        assert_eq!(ring.depth(), 3, "the ring is bounded by capacity");
    }

    #[test]
    fn rewind_restores_and_reports_empty() {
        let cfg = RewindConfig {
            enabled: true,
            interval_frames: 1,
            capacity: 4,
        };
        let mut ring = RewindRing::new(cfg);
        let mut core = EmuCore::new(0);
        ring.on_frame(&core); // one snapshot captured
        assert_eq!(ring.depth(), 1);
        assert!(ring.rewind(&mut core), "a captured snapshot rewinds");
        assert_eq!(ring.depth(), 0);
        assert!(!ring.rewind(&mut core), "an empty ring cannot rewind");
    }

    #[test]
    fn run_ahead_off_is_a_plain_frame() {
        let ra = RunAhead::default();
        assert!(!ra.is_active());
        let mut core = EmuCore::new(0);
        let _audio = ra.step(&mut core);
        // With no ROM the machine still advances one frame's worth of ticks.
        assert!(core.master_ticks() > 0, "one frame advanced");
    }
}
