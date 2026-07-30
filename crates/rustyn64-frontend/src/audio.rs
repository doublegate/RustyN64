//! The audio ring + cpal output stream (native-only).
//!
//! A single-producer / single-consumer ring decouples the emulator thread
//! (producer: pushes the per-frame drained samples) from the cpal callback
//! (consumer: pulls into the device buffer). Dynamic rate control — the resampler
//! servo that nudges the produce rate to keep the ring near half-full — is a
//! frontend responsibility (the determinism contract keeps it OUT of the core);
//! the ring exposes the occupancy the servo needs and is otherwise a plain
//! SPSC buffer.
//!
//! **It is mutex-backed, not lock-free**, and this module used to say otherwise in
//! both its header and the type's own docs. The distinction is not cosmetic: the
//! cpal callback runs on a real-time thread, and a blocking acquire there is a
//! priority inversion — the audio thread waits on the emulator thread, misses its
//! deadline, and the device underruns. The consumer therefore **never blocks**
//! (see [`AudioRing::pull`]), and both sides do bulk transfers to keep the
//! critical section as short as the data allows. A genuinely wait-free ring would
//! need `unsafe` or a new dependency and is still worth doing; this removes the
//! deadline hazard without either.
//!
//! **What this does not fix — and what the ~1 s on / ~1 s off chopping actually
//! is.** The producer stages one *emulated frame* of audio per emulated frame
//! (`EmuCore::produce_audio`, private — hence a code span and not a link, which
//! `rustdoc -D warnings` rejects from public docs); the device consumes in wall-clock
//! time. So the supply ratio is exactly `fps / 60`, and at the ~10 FPS this core
//! currently sustains the ring receives under a fifth of what it must deliver.
//! That is **starvation by throughput**, not a ring defect and not a resampler
//! defect: no buffering strategy manufactures the missing 80%. It closes when the
//! core gets faster and not before — see `docs/performance.md`, which also records
//! that 60 FPS is out of reach for this execution model, so some form of explicit
//! slow-running audio policy will eventually be needed instead.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// A mutex-backed SPSC sample ring whose **consumer never blocks**.
///
/// The backing storage is a `Mutex<VecDeque<f32>>`; a true wait-free ring remains
/// a roadmap item, since it needs either `unsafe` or a new dependency. The atomic
/// occupancy counter lets the rate-control servo read the fill level without
/// taking the lock at all.
///
/// The asymmetry between the two sides is deliberate and is the point of the type:
/// the producer is an ordinary thread and may wait, while the consumer runs on the
/// audio device's real-time thread and must not. See [`AudioRing::pull`].
#[derive(Debug)]
pub struct AudioRing {
    buf: Mutex<std::collections::VecDeque<f32>>,
    occupancy: AtomicUsize,
    capacity: usize,
}

impl AudioRing {
    /// Construct a ring holding up to `capacity` interleaved samples.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            buf: Mutex::new(std::collections::VecDeque::with_capacity(capacity)),
            occupancy: AtomicUsize::new(0),
            capacity,
        }
    }

    /// Push produced samples (emu thread). Drops the oldest on overflow so a
    /// stalled consumer never blocks the producer.
    ///
    /// This side **may** block: it runs on the emulator thread, which has no
    /// deadline the audio device cares about.
    ///
    /// Overflow is trimmed once up-front rather than one element at a time inside
    /// the copy, so the lock is held for a bounded bulk `extend` instead of a
    /// per-sample test-and-pop. That shortening is what makes the consumer's
    /// non-blocking acquire (see [`AudioRing::pull`]) succeed in practice rather
    /// than merely in principle.
    ///
    /// A poisoned lock drops the batch rather than recovering it. That is not a
    /// silent swallow of a live failure: the release profile sets
    /// `panic = "abort"` (workspace `Cargo.toml`), so there is no unwinding and a
    /// `Mutex` **cannot** be poisoned in a shipped build. Under `test`/`debug` a
    /// poisoned lock means a panic already happened and was already reported by the
    /// panic handler; recovering the guard with `PoisonError::into_inner` would
    /// continue on state whose invariants that panic may have interrupted, to save
    /// audio that nobody is listening to in a failing test.
    pub fn push(&self, samples: &[f32]) {
        let Ok(mut q) = self.buf.lock() else {
            return;
        };
        // Anything beyond `capacity` can only displace what is already queued, so
        // keep the newest tail and skip copying the rest at all.
        let keep = samples.len().min(self.capacity);
        let incoming = &samples[samples.len() - keep..];
        let evict = (q.len() + keep).saturating_sub(self.capacity).min(q.len());
        drop(q.drain(..evict));
        q.extend(incoming);
        self.occupancy.store(q.len(), Ordering::Relaxed);
    }

    /// Pull into the device buffer (cpal callback). Underrun fills with silence.
    ///
    /// **Never blocks.** This runs on the device's real-time thread, where waiting
    /// on the emulator thread is a priority inversion: the callback would miss its
    /// deadline and the device would underrun anyway, so blocking buys nothing and
    /// costs a glitch of unbounded length. `try_lock` bounds the damage to one
    /// buffer of silence in the rare case the producer holds the lock.
    ///
    /// The trade is explicit: a contended callback emits silence where a blocking
    /// one *might* have got real samples. That is the better failure — bounded and
    /// bursty rather than unbounded — and `push` keeps the window narrow enough
    /// that it should be rare.
    ///
    /// `WouldBlock` and `Poisoned` are deliberately not distinguished: both mean
    /// "no samples available to this call", which is all a real-time callback can
    /// act on, and `Poisoned` is unreachable in a shipped build anyway because the
    /// release profile aborts rather than unwinds (see [`AudioRing::push`]).
    pub fn pull(&self, out: &mut [f32]) {
        let Ok(mut q) = self.buf.try_lock() else {
            out.fill(0.0);
            return;
        };
        let n = out.len().min(q.len());
        for (slot, s) in out.iter_mut().zip(q.drain(..n)) {
            *slot = s;
        }
        out[n..].fill(0.0);
        self.occupancy.store(q.len(), Ordering::Relaxed);
    }

    /// Current fill level (samples), for the rate-control servo.
    #[must_use]
    pub fn occupancy(&self) -> usize {
        self.occupancy.load(Ordering::Relaxed)
    }

    /// Ring capacity in samples.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }
}

/// An active cpal output stream feeding from an [`AudioRing`].
///
/// Holding the stream keeps it playing; dropping it stops playback.
pub struct AudioOutput {
    _stream: cpal::Stream,
    /// The shared ring the emulator pushes into.
    pub ring: Arc<AudioRing>,
    /// The negotiated device sample rate (Hz).
    pub sample_rate: u32,
}

impl AudioOutput {
    /// Open the default output device and start a stereo f32 stream.
    ///
    /// # Errors
    /// Returns a description string if no device / config is available or the
    /// stream fails to build or start.
    pub fn open() -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| "no default audio output device".to_string())?;
        let supported = device
            .default_output_config()
            .map_err(|e| format!("default output config: {e}"))?;
        // cpal 0.18: `sample_rate()` returns the `u32` sample rate directly.
        let sample_rate = supported.sample_rate();
        let channels = supported.channels() as usize;
        let config: cpal::StreamConfig = supported.into();

        // ~0.25 s of stereo headroom.
        let ring = Arc::new(AudioRing::new((sample_rate as usize) * channels / 4));
        let ring_cb = Arc::clone(&ring);

        let stream = device
            .build_output_stream(
                config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    ring_cb.pull(data);
                },
                |err| eprintln!("rustyn64: audio stream error: {err}"),
                None,
            )
            .map_err(|e| format!("build output stream: {e}"))?;
        stream.play().map_err(|e| format!("play stream: {e}"))?;

        Ok(Self {
            _stream: stream,
            ring,
            sample_rate,
        })
    }
}

impl std::fmt::Debug for AudioOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioOutput")
            .field("sample_rate", &self.sample_rate)
            .field("ring_occupancy", &self.ring.occupancy())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    // The ring stores samples verbatim (no arithmetic), so exact f32 array
    // comparison in these assertions is correct, not an approximate-equality bug.
    #![allow(clippy::float_cmp)]

    use super::*;

    #[test]
    fn ring_push_pull() {
        let ring = AudioRing::new(8);
        ring.push(&[1.0, 2.0, 3.0]);
        assert_eq!(ring.occupancy(), 3);
        let mut out = [0.0f32; 4];
        ring.pull(&mut out);
        assert_eq!(out, [1.0, 2.0, 3.0, 0.0]);
        assert_eq!(ring.occupancy(), 0);
    }

    /// **A contended `pull` returns silence instead of waiting.**
    ///
    /// This is the property the whole type is shaped around: the cpal callback runs
    /// on the device's real-time thread, and blocking there is a priority inversion
    /// that turns a bounded glitch into an unbounded one. A blocking `lock()` passes
    /// every other test in this module, because none of them contend — so without
    /// this test the guarantee would be documentation only.
    ///
    /// Sequenced with channels rather than sleeps — the holder signals once it owns
    /// the lock — so there is no timing window to flake on. The `pull` itself runs
    /// on a third thread behind a `recv_timeout`, which is what makes a regression
    /// **fail** rather than **hang**: reverting to a blocking `lock()` here would
    /// otherwise wedge the whole test binary until CI's job timeout, burning a
    /// runner and reporting nothing useful. The holder is released on both paths so
    /// the blocked puller can finish and the process can still exit.
    #[test]
    fn a_contended_pull_yields_silence_and_does_not_block() {
        use std::sync::mpsc;
        use std::time::Duration;

        /// Generous enough that a loaded CI runner cannot trip it, short enough
        /// that a genuine block is reported in seconds rather than at job timeout.
        const PULL_DEADLINE: Duration = Duration::from_secs(10);

        let ring = Arc::new(AudioRing::new(8));
        ring.push(&[1.0, 2.0, 3.0, 4.0]);

        let (held_tx, held_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (done_tx, done_rx) = mpsc::channel::<[f32; 4]>();

        let holder_ring = Arc::clone(&ring);
        let holder = std::thread::spawn(move || {
            let _guard = holder_ring.buf.lock().expect("holder acquires the lock");
            held_tx.send(()).expect("signal that the lock is held");
            release_rx.recv().expect("wait for release");
        });

        held_rx.recv().expect("the lock is now held elsewhere");

        let puller_ring = Arc::clone(&ring);
        let puller = std::thread::spawn(move || {
            let mut out = [7.0f32; 4];
            puller_ring.pull(&mut out);
            let _ = done_tx.send(out);
        });

        let outcome = done_rx.recv_timeout(PULL_DEADLINE);
        // Release the holder before asserting, so a failure does not also strand
        // the puller thread and hang the harness on exit.
        release_tx.send(()).expect("release the holder");
        holder.join().expect("holder thread finishes");
        puller.join().expect("puller thread finishes");

        let out = outcome.expect("pull must return while the lock is held, not block on it");
        assert_eq!(
            out, [0.0; 4],
            "a pull that cannot take the lock must emit silence"
        );

        // And the queued samples were not consumed by the contended call.
        let mut after = [0.0f32; 4];
        ring.pull(&mut after);
        assert_eq!(
            after,
            [1.0, 2.0, 3.0, 4.0],
            "the contended pull must not have dropped what was queued"
        );
    }

    /// A push longer than the ring keeps the **newest** tail, matching the
    /// element-at-a-time eviction it replaced.
    #[test]
    fn a_push_larger_than_capacity_keeps_the_newest_tail() {
        let ring = AudioRing::new(3);
        ring.push(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(ring.occupancy(), 3);
        let mut out = [0.0f32; 3];
        ring.pull(&mut out);
        assert_eq!(out, [3.0, 4.0, 5.0]);
    }

    #[test]
    fn ring_overflow_drops_oldest() {
        let ring = AudioRing::new(4);
        ring.push(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        assert_eq!(ring.occupancy(), 4);
        let mut out = [0.0f32; 4];
        ring.pull(&mut out);
        assert_eq!(out, [3.0, 4.0, 5.0, 6.0]);
    }
}
