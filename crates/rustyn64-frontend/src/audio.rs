//! The lock-free audio ring + cpal output stream (native-only).
//!
//! A single-producer / single-consumer ring decouples the emulator thread
//! (producer: pushes the per-frame drained samples) from the cpal callback
//! (consumer: pulls into the device buffer). Dynamic rate control — the resampler
//! servo that nudges the produce rate to keep the ring near half-full — is a
//! frontend responsibility (the determinism contract keeps it OUT of the core);
//! the v0.1 ring exposes the occupancy the servo needs and is otherwise a plain
//! SPSC buffer.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// A simple lock-free-ish SPSC sample ring.
///
/// The backing storage is a `Mutex<Vec<f32>>` for the v0.1 skeleton (a true
/// wait-free ring is a roadmap optimization); the atomic occupancy counter lets
/// the rate-control servo read the fill level without taking the lock.
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
    pub fn push(&self, samples: &[f32]) {
        if let Ok(mut q) = self.buf.lock() {
            for &s in samples {
                if q.len() >= self.capacity {
                    q.pop_front();
                }
                q.push_back(s);
            }
            self.occupancy.store(q.len(), Ordering::Relaxed);
        }
    }

    /// Pull into the device buffer (cpal callback). Underrun fills with silence.
    pub fn pull(&self, out: &mut [f32]) {
        if let Ok(mut q) = self.buf.lock() {
            for slot in out.iter_mut() {
                *slot = q.pop_front().unwrap_or(0.0);
            }
            self.occupancy.store(q.len(), Ordering::Relaxed);
        } else {
            out.fill(0.0);
        }
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
