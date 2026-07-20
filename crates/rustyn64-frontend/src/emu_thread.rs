//! The dedicated emulation thread (native, `emu-thread` feature).
//!
//! On native the emulator runs OFF the winit event-loop thread so UI / render
//! stalls never disturb emulation cadence. The thread owns the produce loop:
//! latch the lock-free [`SharedInput`], run a frame on
//! the `Arc<Mutex<EmuCore>>`, push the drained audio into the ring, then sleep to
//! the region's target frame interval. The winit thread only reads the staged
//! framebuffer (under a brief lock) and presents.
//!
//! Rate control + run-ahead orchestration belong HERE (frontend-side), never in
//! the core — the determinism contract. The v0.1 loop is a simple wall-clock
//! pacer; the resampler servo + run-ahead snapshots are roadmap.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use web_time::{Duration, Instant};

use crate::audio::AudioRing;
use crate::config::Region;
use crate::emu::EmuCore;
use crate::input::SharedInput;

/// A handle to the running emulation thread. Dropping it signals the thread to
/// stop and joins it.
pub struct EmuThread {
    handle: Option<JoinHandle<()>>,
    running: Arc<AtomicBool>,
}

impl EmuThread {
    /// Spawn the emulation thread.
    ///
    /// - `emu` is the shared core the winit thread also reads (briefly) to blit.
    /// - `input` is the lock-free controller handoff the UI thread writes.
    /// - `ring` receives the drained audio (`None` when no device opened).
    /// - `region` sets the wall-clock target frame interval.
    ///
    /// # Panics
    /// Panics if the OS refuses to spawn the `emu-thread` (an unrecoverable
    /// platform failure at startup).
    #[must_use]
    pub fn spawn(
        emu: Arc<Mutex<EmuCore>>,
        input: Arc<SharedInput>,
        ring: Option<Arc<AudioRing>>,
        region: Region,
    ) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let run_flag = Arc::clone(&running);

        let handle = std::thread::Builder::new()
            .name("emu-thread".to_string())
            .spawn(move || {
                let frame_interval = Duration::from_secs_f64(1.0 / region.target_fps());
                let mut next = Instant::now();
                while run_flag.load(Ordering::Relaxed) {
                    // Latch input, run one frame, drain audio — under a brief lock.
                    let ports = input.load_all();
                    let audio = emu.lock().map_or_else(
                        |_| Vec::new(),
                        |mut core| {
                            core.set_controllers(ports);
                            core.run_frame();
                            core.drain_audio()
                        },
                    );
                    if let Some(ring) = ring.as_ref() {
                        ring.push(&audio);
                    }

                    // Wall-clock pace to the target frame interval.
                    next += frame_interval;
                    let now = Instant::now();
                    if next > now {
                        std::thread::sleep(next - now);
                    } else {
                        // Fell behind — reset the phase so we don't spin to catch up.
                        next = now;
                    }
                }
            })
            .expect("spawn emu-thread");

        Self {
            handle: Some(handle),
            running,
        }
    }

    /// Signal the thread to stop (does not block).
    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

impl Drop for EmuThread {
    fn drop(&mut self) {
        self.stop();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl std::fmt::Debug for EmuThread {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmuThread")
            .field("running", &self.running.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}
