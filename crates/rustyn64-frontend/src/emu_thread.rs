//! The dedicated emulation thread (native, `emu-thread` feature).
//!
//! On native the emulator runs OFF the winit event-loop thread so UI / render
//! stalls never disturb emulation cadence. The thread owns the produce loop:
//! latch the lock-free [`SharedInput`], run a frame on the `Arc<Mutex<EmuCore>>`,
//! publish the frame and status into the [`PresentBuffer`], push the drained audio
//! into the ring, then pace to the region's target frame interval. **The winit
//! thread never takes the emu mutex** — it reads the handoff instead.
//!
//! Rate control, save-states, rewind, and run-ahead orchestration belong HERE
//! (frontend-side), never in the core — the determinism contract. The loop is a
//! wall-clock pacer driving a [`SaveStateCoordinator`] (rewind capture +
//! run-ahead + save/load), which is a plain `run_frame` when both are off.
//!
//! **The resampler servo is implemented** (`AudioServo`, private): it stretches each
//! emulated frame's audio over the wall-clock time that frame actually took, so
//! a core slower than real time produces a continuous stream instead of
//! `fps / 60` of one. Measured 14.2% -> 90.9% delivered, 100% in steady state
//! (`docs/audio.md`).
//!
//! # The pacer, and the bug it replaces
//!
//! The original loop was `next += interval; if next > now { sleep } else { next =
//! now }`. The else-branch has **no sleep and no yield**, and this core is ~6.5x
//! slower than real time, so it was taken every single iteration: the thread
//! re-locked the emu mutex immediately and held it ~100% of the time. The UI needed
//! that same mutex every frame just to read the framebuffer, so against an unfair
//! mutex it starved for many frames at a stretch — measured by the user as menu
//! clicks taking 15-45 seconds and roughly one presented frame per 30-60 seconds.
//!
//! The replacement is `RustyNES`'s pacer: a bounded catch-up burst
//! (`MAX_CATCHUP_FRAMES`) then a **snap forward**, followed by
//! `block_until_native` — capped naps (`SLEEP_CHUNK`) down to `SPIN_MARGIN`, then
//! a precise spin. Video frames coalesce (the handoff keeps only the newest);
//! **audio is never dropped**.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use web_time::{Duration, Instant};

use crate::audio::AudioRing;
use crate::config::Region;
use crate::emu::{EmuCore, MAX_AUDIO_STRETCH};
use crate::input::SharedInput;
use crate::present_buffer::PresentBuffer;
use crate::savestate::{RewindConfig, RunAhead, SaveStateControls, SaveStateCoordinator};

/// Sleep until this close to the target, then busy-spin to the exact instant.
/// Ported from `RustyNES`'s `emu_thread::SPIN_MARGIN`; an OS timer is not precise
/// enough to land a frame boundary on its own.
const SPIN_MARGIN: Duration = Duration::from_millis(2);

/// Cap on any single sleep inside [`block_until_native`], so one OS oversleep can
/// overshoot by at most this before the loop re-measures.
///
/// The cap is load-bearing, and is `RustyNES`'s fix for residual stutter: with one
/// `sleep(remaining - margin)`, a single oversleep blows past the target, the
/// precise spin never engages, and the frame lands late. Capping and re-measuring
/// keeps the wait converging.
///
/// Equal to [`SPIN_MARGIN`] **coincidentally**, not by construction — they answer
/// different questions ("how close before spinning" versus "how long may one nap
/// be") and the port defines them separately for that reason. Either can be tuned
/// without the other; do not collapse them into one constant.
const SLEEP_CHUNK: Duration = Duration::from_millis(2);

/// Frames the pacer will run back-to-back to catch up before giving up and
/// snapping forward (`RustyNES`'s `emu::MAX_CATCHUP_FRAMES`).
///
/// A bound rather than an unbounded catch-up loop: a core slower than real time
/// can *never* catch up, so replaying the window would starve the UI forever —
/// which is exactly the bug this pacer replaces.
const MAX_CATCHUP_FRAMES: u32 = 3;

/// The audio rate-control servo: how far to stretch one emulated frame's audio
/// so the device hears a continuous stream from a core slower than real time.
///
/// **The problem it solves** (`docs/audio.md`): the core stages exactly one
/// emulated frame of audio per emulated frame, and the device consumes in
/// wall-clock time, so supply is `fps / 60` — measured at **14.2%** on the
/// default build. No buffering manufactures the rest. Stretching the resample by
/// `60 / fps` fills the gap with real samples instead of silence, at the cost of
/// pitch: the slow-tape sound. That is the shipped policy, chosen deliberately
/// over chopping and over muting.
///
/// **Feed-forward plus a trim, not a pure integrator.** The dominant term is
/// measured directly — the wall-clock interval between produced frames — so the
/// servo is correct on its first frame instead of converging over seconds, which
/// a pure occupancy integrator with a 7x range to cover would not be. The
/// occupancy term only removes residual drift, because the feed-forward term
/// cannot know about samples already banked in the ring.
///
/// Pure and separately testable, like [`Schedule`] above and for the same
/// reason: the decisions are checkable with synthetic inputs, no thread, no real
/// clock, no timing flake.
#[derive(Debug)]
struct AudioServo {
    /// Smoothed wall-clock interval between produced frames.
    interval: Option<Duration>,
}

impl AudioServo {
    /// Weight of a new sample in the interval EMA. Low enough that one slow
    /// frame does not swing the pitch audibly, high enough to track a real
    /// change in load within a few frames.
    const SMOOTHING: u32 = 8;

    /// How hard the ring's fill level trims the feed-forward term.
    ///
    /// **Measured, and deliberately the smallest of the three tried.** 0.2, 0.4
    /// and 0.6 all reach **100.0% steady-state delivery** and all reach their
    /// first fully-fed callback at the same place (#15, ~320 ms); the only
    /// difference is in the whole-window figure (90.6 / 91.3 / 91.9%), which is
    /// dominated by the startup ramp from an empty ring and is not a
    /// steady-state result. Picking 0.6 for that 1.3-point edge would be tuning
    /// against a transient, so the lowest gain wins on the tiebreak that matters
    /// here: a larger gain makes the pitch hunt audibly, and nothing was bought
    /// with it. Re-run `measure_audio_gaps_at_the_device_boundary` before
    /// changing it.
    const TRIM_GAIN: f64 = 0.2;

    const fn new() -> Self {
        Self { interval: None }
    }

    /// Fold one measured frame interval into the estimate.
    fn observe(&mut self, frame: Duration) {
        self.interval = Some(self.interval.map_or(frame, |prev| {
            (prev * (Self::SMOOTHING - 1) + frame) / Self::SMOOTHING
        }));
    }

    /// The stretch factor for the next frame.
    ///
    /// `fill` is the ring's occupancy as a fraction of capacity. Returns `1.0`
    /// (real time, i.e. the previous behavior) until an interval has been
    /// observed, and never *compresses* below real time: a core running faster
    /// than 60 FPS is already supplying more than the device wants, and the
    /// ring's drop-oldest is the right answer there.
    fn stretch(&self, period: Duration, fill: f64) -> f64 {
        let Some(interval) = self.interval else {
            return 1.0;
        };
        let period = period.as_secs_f64();
        if period <= 0.0 {
            return 1.0;
        }
        // Feed-forward: one emulated frame must cover one wall-clock interval.
        let base = interval.as_secs_f64() / period;
        // Trim toward a half-full ring. Under half, stretch a little more;
        // over half, a little less.
        let trim = Self::TRIM_GAIN.mul_add(0.5 - fill.clamp(0.0, 1.0), 1.0);
        (base * trim).clamp(1.0, MAX_AUDIO_STRETCH)
    }
}

/// Wall-clock pacing diagnostics.
///
/// Published for the UI and for the later perf work. Counters only — nothing
/// schedules against them (ADR 0006: `master_ticks` is the only clock position
/// that is ever incremented).
#[derive(Debug, Default)]
pub struct PacerStats {
    /// Iterations that ran more than one frame back-to-back.
    pub catchup_bursts: AtomicU64,
    /// Iterations that gave up catching up and re-based the schedule on `now`.
    pub snap_forwards: AtomicU64,
    /// Frames the pacer has produced.
    pub produced: AtomicU64,
    /// The most recent audio stretch factor, as `f64::to_bits`, or **zero when
    /// the servo has never run** — which is the case whenever no ring is
    /// attached.
    ///
    /// Zero is a usable sentinel because `AudioServo::stretch` is clamped to
    /// `[1.0, MAX_AUDIO_STRETCH]` and so can never produce it. That distinction
    /// is the point: it lets a test tell "the servo ran and asked for real time"
    /// apart from "the servo was never consulted", which are otherwise identical
    /// from outside — the failure mode where rate control is computed correctly
    /// and then never applied.
    ///
    /// A diagnostic. Nothing schedules against it (ADR 0006).
    pub audio_stretch_bits: AtomicU64,
}

impl PacerStats {
    /// The last stretch the servo asked for, or `None` if it never ran.
    #[must_use]
    pub fn audio_stretch(&self) -> Option<f64> {
        match self.audio_stretch_bits.load(Ordering::Relaxed) {
            0 => None,
            bits => Some(f64::from_bits(bits)),
        }
    }
}

/// Everything [`EmuThread::spawn`] needs.
///
/// A struct rather than a parameter list because the eighth argument (the present
/// handoff) trips `clippy::too_many_arguments` — and named fields read better than
/// eight positional values at the one construction site anyway.
pub struct EmuThreadParams {
    /// The shared core.
    pub emu: Arc<Mutex<EmuCore>>,
    /// The lock-free controller handoff the UI thread writes.
    pub input: Arc<SharedInput>,
    /// The framebuffer + status handoff the present path reads instead of taking
    /// the emu mutex.
    pub present: Arc<PresentBuffer>,
    /// Receives the drained audio (`None` when no device opened).
    pub ring: Option<Arc<AudioRing>>,
    /// Sets the wall-clock target frame interval.
    pub region: Region,
    /// Rewind capture configuration.
    pub rewind: RewindConfig,
    /// Run-ahead configuration.
    pub run_ahead: RunAhead,
    /// Save-state / rewind requests this thread serves.
    pub controls: Arc<SaveStateControls>,
}

/// The wall-clock frame schedule: how many frames are due, and how to advance.
///
/// Extracted from the produce loop so the pacing **decisions** are testable with
/// synthetic instants — no thread, no real clock, and therefore no timing flake.
/// The loop below only supplies `Instant::now()` and runs the frames.
struct Schedule {
    /// When the next frame is due.
    target: Instant,
    /// The region's frame interval.
    period: Duration,
}

impl Schedule {
    const fn new(now: Instant, period: Duration) -> Self {
        Self {
            target: now,
            period,
        }
    }

    /// Frames due at `now`, **capped** at [`MAX_CATCHUP_FRAMES`].
    ///
    /// The cap is the load-bearing part: a core slower than real time is behind on
    /// every iteration, so an uncapped count would run frames back-to-back forever
    /// and never reach the wait that lets the UI take the emu mutex.
    fn frames_due(&self, now: Instant) -> u32 {
        if self.target > now {
            return 0;
        }
        // `now >= target` here, so the subtraction cannot underflow.
        let behind = now - self.target;
        let period_ns = self.period.as_nanos().max(1);
        // `+ 1` because the frame due exactly AT the target is itself due.
        let due = behind.as_nanos() / period_ns + 1;
        u32::try_from(due)
            .unwrap_or(u32::MAX)
            .min(MAX_CATCHUP_FRAMES)
    }

    /// Charge one produced frame to the schedule.
    fn advance(&mut self) {
        self.target += self.period;
    }

    /// After a burst: if still behind, re-base the schedule on `now` and report it.
    ///
    /// Re-basing rather than replaying the missed window is what makes the
    /// subsequent wait a real wait. The old code did `next = now`, which left the
    /// target already reached and so never waited at all.
    fn snap_if_behind(&mut self, now: Instant) -> bool {
        if self.target <= now {
            self.target = now + self.period;
            true
        } else {
            false
        }
    }

    const fn target(&self) -> Instant {
        self.target
    }
}

/// Sleep-then-spin wait to a precise `target`.
///
/// Ported from `RustyNES/crates/rustynes-frontend/src/emu_thread.rs`. Each nap is
/// capped at [`SLEEP_CHUNK`] and the remaining time is re-measured every pass, so
/// an OS oversleep cannot skip the precise spin.
fn block_until_native(target: Instant) {
    loop {
        let now = Instant::now();
        if now >= target {
            return;
        }
        let remaining = target - now;
        if remaining > SPIN_MARGIN {
            std::thread::sleep(remaining.saturating_sub(SPIN_MARGIN).min(SLEEP_CHUNK));
        } else {
            std::hint::spin_loop();
        }
    }
}

/// A handle to the running emulation thread. Dropping it signals the thread to
/// stop and joins it.
pub struct EmuThread {
    handle: Option<JoinHandle<()>>,
    running: Arc<AtomicBool>,
    stats: Arc<PacerStats>,
}

impl EmuThread {
    /// Spawn the emulation thread. See [`EmuThreadParams`] for the inputs.
    ///
    /// # Panics
    /// Panics if the OS refuses to spawn the `emu-thread` (an unrecoverable
    /// platform failure at startup).
    #[must_use]
    pub fn spawn(params: EmuThreadParams) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let run_flag = Arc::clone(&running);
        let stats = Arc::new(PacerStats::default());
        let thread_stats = Arc::clone(&stats);

        let handle = std::thread::Builder::new()
            .name("emu-thread".to_string())
            .spawn(move || {
                let EmuThreadParams {
                    emu,
                    input,
                    present,
                    ring,
                    region,
                    rewind,
                    run_ahead,
                    controls,
                } = params;
                let period = Duration::from_secs_f64(1.0 / region.target_fps());
                let mut schedule = Schedule::new(Instant::now(), period);
                // Save-states, rewind, and run-ahead are driven here (frontend-side,
                // ADR 0004); with rewind off and run-ahead 0 the coordinator is a
                // plain `run_frame` + drain, so output stays byte-identical.
                let mut coordinator = SaveStateCoordinator::new(rewind, run_ahead, controls);
                // Audio rate control. Only meaningful with a ring attached; with
                // `ring: None` the servo is never consulted and the core keeps
                // its default 1.0 stretch, so a headless build is unchanged.
                let mut servo = AudioServo::new();
                let mut last_frame = Instant::now();
                while run_flag.load(Ordering::Relaxed) {
                    let due = schedule.frames_due(Instant::now());
                    let mut produced = 0u32;
                    // Bounded catch-up burst. The emu lock is taken and released
                    // PER FRAME rather than across the whole burst — a deliberate
                    // deviation from the RustyNES port, because an N64 frame costs
                    // this core ~100 ms, so holding across three would be a ~300 ms
                    // UI stall. Per-frame gives the UI a window between each.
                    while produced < due && run_flag.load(Ordering::Relaxed) {
                        let ports = input.load_all();
                        // Measured BEFORE the frame runs, from the last frame's
                        // completion: the servo needs the interval the device
                        // actually experienced, and the frame about to run has
                        // not happened yet.
                        let stretch = ring.as_ref().map(|r| {
                            let now = Instant::now();
                            servo.observe(now - last_frame);
                            last_frame = now;
                            #[allow(
                                clippy::cast_precision_loss,
                                reason = "ring occupancy and capacity are far below 2^53"
                            )]
                            let fill = r.occupancy() as f64 / r.capacity().max(1) as f64;
                            servo.stretch(period, fill)
                        });
                        if let Some(stretch) = stretch {
                            thread_stats
                                .audio_stretch_bits
                                .store(stretch.to_bits(), Ordering::Relaxed);
                        }
                        let audio = emu.lock().map_or_else(
                            |_| Vec::new(),
                            |mut core| {
                                core.set_controllers(ports);
                                if let Some(stretch) = stretch {
                                    core.set_audio_stretch(stretch);
                                }
                                let audio = coordinator.step(&mut core);
                                // Published under the lock we already hold (as
                                // RustyNES's emu thread does): one memcpy instead of
                                // staging and copying twice. The UI still never takes
                                // this mutex — it waits only on the handoff's own,
                                // and only for one copy.
                                core.publish_into(&present);
                                audio
                            },
                        );
                        if let Some(ring) = ring.as_ref() {
                            ring.push(&audio);
                        }
                        schedule.advance();
                        produced += 1;
                    }
                    thread_stats
                        .produced
                        .fetch_add(u64::from(produced), Ordering::Relaxed);
                    if produced >= 2 {
                        thread_stats.catchup_bursts.fetch_add(1, Ordering::Relaxed);
                    }
                    // Measured AFTER the work, because the work is what put us
                    // behind. Still behind after the capped burst means re-base on
                    // now: the old code did `next = now` and so never waited, holding
                    // the emu mutex ~100% of the time and starving the UI for many
                    // frames at a stretch — the 15-45 s menu latency.
                    if schedule.snap_if_behind(Instant::now()) {
                        thread_stats.snap_forwards.fetch_add(1, Ordering::Relaxed);
                    }
                    // The yield. Audio is never dropped (every produced frame's
                    // samples were pushed above); only video coalesces, because the
                    // handoff keeps just the newest frame.
                    block_until_native(schedule.target());
                }
            })
            .expect("spawn emu-thread");

        Self {
            handle: Some(handle),
            running,
            stats,
        }
    }

    /// Signal the thread to stop (does not block).
    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }

    /// The pacing diagnostics (catch-up bursts, snap-forwards, frames produced).
    #[must_use]
    pub const fn stats(&self) -> &Arc<PacerStats> {
        &self.stats
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

#[cfg(test)]
mod tests {
    use super::*;

    const PERIOD: Duration = Duration::from_millis(16);

    /// **The measurement behind the whole change.** Runs a real emu thread and
    /// times, from a competing "UI" thread, the two ways the present path could get
    /// a frame: through the handoff, and through the emu mutex the way
    /// `App::snapshot` used to. Prints both distributions.
    ///
    /// `#[ignore]`d because it is a stopwatch, not a gate — it takes seconds and its
    /// numbers are machine-specific. Run it with:
    ///
    /// ```text
    /// cargo test -p rustyn64-frontend --release --lib \
    ///     measure_ui_read_latency -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "a measurement, not a gate; seconds long and machine-specific"]
    fn measure_ui_read_latency_through_the_handoff_versus_the_emu_mutex() {
        let present = PresentBuffer::new();
        let emu = Arc::new(Mutex::new(EmuCore::new(0)));
        let thread = EmuThread::spawn(EmuThreadParams {
            emu: Arc::clone(&emu),
            input: Arc::new(SharedInput::new()),
            present: Arc::clone(&present),
            ring: None,
            region: Region::default(),
            rewind: RewindConfig::default(),
            run_ahead: RunAhead::default(),
            controls: Arc::new(SaveStateControls::default()),
        });

        let mut via_handoff = Vec::new();
        let mut via_emu_lock = Vec::new();
        let mut staging = Vec::new();
        // ~2 s of a 60 Hz UI loop.
        for _ in 0..120 {
            let t0 = Instant::now();
            let _ = present.take_into(&mut staging);
            let _ = present.status();
            via_handoff.push(t0.elapsed());

            let t1 = Instant::now();
            drop(emu.lock().map(|core| core.frame_count()));
            via_emu_lock.push(t1.elapsed());

            std::thread::sleep(Duration::from_millis(16));
        }
        let stats = thread.stats();
        let (produced, bursts, snaps) = (
            stats.produced.load(Ordering::Relaxed),
            stats.catchup_bursts.load(Ordering::Relaxed),
            stats.snap_forwards.load(Ordering::Relaxed),
        );
        drop(thread);

        let report = |label: &str, mut v: Vec<Duration>| {
            v.sort_unstable();
            let max = v[v.len() - 1];
            let p50 = v[v.len() / 2];
            let p99 = v[v.len() * 99 / 100];
            println!("{label:>14}: p50 {p50:>10.3?}  p99 {p99:>10.3?}  max {max:>10.3?}");
            max
        };
        println!(
            "--- UI-side read latency over {} samples ---",
            via_handoff.len()
        );
        let handoff_max = report("handoff", via_handoff);
        let lock_max = report("emu mutex", via_emu_lock);
        println!(
            "pacer: produced {produced} frames, {bursts} catch-up bursts, {snaps} snap-forwards"
        );

        // A loose sanity assertion only -- the point is the printed numbers. If the
        // handoff were somehow SLOWER than the mutex the change would be pointless.
        assert!(
            handoff_max <= lock_max || lock_max < Duration::from_millis(1),
            "the handoff must not be slower than the emu mutex it replaces"
        );
    }

    /// Before any frame has been timed the servo asks for real time, so a build
    /// that never observes an interval is byte-identical to the pre-servo one.
    #[test]
    fn the_servo_asks_for_real_time_until_it_has_measured_something() {
        let servo = AudioServo::new();
        let s = servo.stretch(Duration::from_secs_f64(1.0 / 60.0), 0.0);
        assert!(
            (s - 1.0).abs() < f64::EPSILON,
            "an unmeasured servo must not change the rate; got {s}"
        );
    }

    /// The feed-forward term IS the ratio of wall-clock interval to frame
    /// period: a core running at a quarter speed must stretch four-fold, or the
    /// device gets three-quarters silence.
    #[test]
    fn the_servo_stretches_by_exactly_how_far_behind_real_time_the_core_is() {
        let period = Duration::from_secs_f64(1.0 / 60.0);
        let mut servo = AudioServo::new();
        // A quarter speed: each emulated frame takes four frame periods.
        servo.observe(period * 4);
        // Half-full ring, so the trim term is exactly 1 and only the
        // feed-forward term is under test.
        let s = servo.stretch(period, 0.5);
        assert!(
            (s - 4.0).abs() < 1e-9,
            "a quarter-speed core needs a 4x stretch; got {s}"
        );
    }

    /// The trim pushes toward a half-full ring in both directions, and it is a
    /// *trim*: it must not swamp the feed-forward term.
    #[test]
    fn the_trim_pushes_toward_half_full_without_swamping_the_feed_forward() {
        let period = Duration::from_secs_f64(1.0 / 60.0);
        let mut servo = AudioServo::new();
        servo.observe(period * 4);
        let empty = servo.stretch(period, 0.0);
        let half = servo.stretch(period, 0.5);
        let full = servo.stretch(period, 1.0);
        assert!(
            empty > half && half > full,
            "an emptier ring must ask for more stretch: {empty} / {half} / {full}"
        );
        // Within 10% of the feed-forward term at either extreme (TRIM_GAIN/2).
        assert!(
            (empty - 4.0).abs() < 0.5 && (full - 4.0).abs() < 0.5,
            "the trim must not dominate: {empty} / {full}"
        );
    }

    /// A core running FASTER than real time must not compress: it is already
    /// supplying more than the device consumes, and the ring's drop-oldest is
    /// the right answer. Compressing would raise the pitch of a fast core.
    #[test]
    fn a_core_faster_than_real_time_is_never_compressed() {
        let period = Duration::from_secs_f64(1.0 / 60.0);
        let mut servo = AudioServo::new();
        servo.observe(period / 4);
        assert!(
            servo.stretch(period, 0.0) >= 1.0 && servo.stretch(period, 1.0) >= 1.0,
            "stretch must never drop below real time"
        );
    }

    /// The bound holds even for a core that has effectively stopped, so one
    /// pathological stall cannot ask for a minutes-long stretch.
    #[test]
    fn a_stalled_core_is_clamped_to_the_documented_maximum() {
        let period = Duration::from_secs_f64(1.0 / 60.0);
        let mut servo = AudioServo::new();
        servo.observe(Duration::from_secs(30));
        let s = servo.stretch(period, 0.0);
        assert!(
            (s - MAX_AUDIO_STRETCH).abs() < f64::EPSILON,
            "expected the clamp at {MAX_AUDIO_STRETCH}; got {s}"
        );
    }

    /// **The servo is actually applied to the core, not merely computed.**
    ///
    /// Rate control that is calculated correctly and then dropped on the floor
    /// passes every unit test above — this repo's decoded-but-no-op failure
    /// mode, which has cost it real time before.
    ///
    /// Asserting the applied *value* is not enough on its own: a ROM-less core
    /// runs far faster than real time, so the servo legitimately asks for `1.0`,
    /// which is also the default. **So the destination is seeded with a value
    /// the servo can never produce** (`SENTINEL`, outside the clamp range) and
    /// the test asserts it was overwritten — the "seed the destination so it
    /// differs from the expected result" rule, applied to a field whose
    /// legitimate value is indistinguishable from its default.
    ///
    /// Mutation checks, both verified:
    /// - delete `core.set_audio_stretch(stretch)` → the sentinel survives → red;
    /// - delete the `ring.as_ref().map(...)` block → `audio_stretch()` stays
    ///   `None` → red.
    #[test]
    fn the_servo_is_applied_to_the_core_when_a_ring_is_attached() {
        /// Outside `[1.0, MAX_AUDIO_STRETCH]`, so no servo output can equal it.
        const SENTINEL: f64 = 99.0;

        let run = |ring: Option<Arc<AudioRing>>| {
            let mut core = EmuCore::new(0);
            core.set_audio_stretch(SENTINEL);
            let emu = Arc::new(Mutex::new(core));
            let thread = EmuThread::spawn(EmuThreadParams {
                emu: Arc::clone(&emu),
                input: Arc::new(SharedInput::new()),
                present: PresentBuffer::new(),
                ring,
                region: Region::default(),
                rewind: RewindConfig::default(),
                run_ahead: RunAhead::default(),
                controls: Arc::new(SaveStateControls::default()),
            });
            // Long enough for several frame periods at 60 Hz.
            std::thread::sleep(Duration::from_millis(200));
            let reported = thread.stats().audio_stretch();
            drop(thread);
            let applied = emu.lock().expect("emu mutex").audio_stretch();
            (reported, applied)
        };

        let (reported, applied) = run(Some(Arc::new(AudioRing::new(4096))));
        let reported = reported.expect("the servo must run when a ring is attached");
        assert!(
            (1.0..=MAX_AUDIO_STRETCH).contains(&reported),
            "the servo's output must be in range; got {reported}"
        );
        assert!(
            (applied - SENTINEL).abs() > f64::EPSILON,
            "the servo ran but its value never reached the core: the sentinel survived"
        );
        assert!(
            (applied - reported).abs() < f64::EPSILON,
            "the core must hold what the servo last asked for: {applied} vs {reported}"
        );

        let (reported, applied) = run(None);
        assert_eq!(
            reported, None,
            "with no ring there is nothing to rate-control, so the servo must not run"
        );
        assert!(
            (applied - SENTINEL).abs() < f64::EPSILON,
            "with no ring the core's stretch must be left exactly as the caller set it"
        );
    }

    /// **What the audio device actually experiences** — the host half of the
    /// ~1 s on / ~1 s off report, measured rather than reasoned about.
    ///
    /// `examples/audio_probe.rs` measures the *core* half and settles it: the
    /// emulated AI produces a continuous stream, and supply is exactly
    /// `fps / 60`. Neither of those is what a listener hears. This spawns the
    /// real [`EmuThread`] against a real [`AudioRing`] and drains it from a
    /// wall-clock-paced consumer standing in for the `cpal` callback, so the
    /// **period** of the gaps is observed at the point the symptom is reported.
    ///
    /// No audio device is opened. That is deliberate: a real device would add
    /// its own scheduling to the thing being measured, and the quantity of
    /// interest — how much of each buffer is real samples — is entirely
    /// determined by [`AudioRing::pull`].
    ///
    /// Needs a ROM with a running audio engine; skips loudly without one.
    ///
    /// ```text
    /// RUSTYN64_PROBE_ROM=/path/rom.z64 cargo test -p rustyn64-frontend --release \
    ///     --lib measure_audio_gaps -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "a measurement, not a gate; ~10 s and needs a commercial ROM"]
    fn measure_audio_gaps_at_the_device_boundary() {
        /// Stereo samples per simulated device callback (1024 frames — cpal's
        /// usual default), so the callback cadence is ~21 ms like a real one.
        const BUF: usize = 2048;
        /// Host rate the ring and consumer agree on.
        const RATE: u32 = 48_000;
        /// Wall-clock seconds to observe. Must span several of the reported
        /// ~1 s cycles, or a run could straddle one and show nothing.
        const OBSERVE: Duration = Duration::from_secs(10);

        let Ok(path) = std::env::var("RUSTYN64_PROBE_ROM") else {
            println!("SKIP: set RUSTYN64_PROBE_ROM to a ROM that runs an audio engine");
            return;
        };
        let raw = std::fs::read(&path).expect("probe ROM readable");
        let mut core = EmuCore::new(0);
        core.set_output_rate(RATE);
        core.load_rom(&raw).expect("probe ROM boots");
        // Warm past boot: the first frames are silent while the game starts its
        // audio engine, and counting those would report a gap that is not the
        // reported one.
        for _ in 0..48 {
            core.run_frame();
            drop(core.drain_audio());
        }

        // Same capacity the shipped `AudioOutput::open` uses: 0.25 s of stereo.
        let ring = Arc::new(AudioRing::new(RATE as usize * 2 / 4));
        let emu = Arc::new(Mutex::new(core));

        // A stand-in for the UI thread's OCCASIONAL emu-mutex takers (load ROM,
        // save state, pause — `app.rs`; the per-frame present path uses the
        // handoff and never takes this lock). The pacer's post-snap yield exists
        // to keep these from starving, so any change to it has to be judged
        // against this latency and not against throughput alone.
        let ui_emu = Arc::clone(&emu);
        let ui_stop = Arc::new(AtomicBool::new(false));
        let ui_flag = Arc::clone(&ui_stop);
        let ui = std::thread::spawn(move || {
            let mut waits = Vec::new();
            while !ui_flag.load(Ordering::Relaxed) {
                let t = Instant::now();
                drop(ui_emu.lock().map(|c| c.frame_count()));
                waits.push(t.elapsed());
                std::thread::sleep(Duration::from_millis(16));
            }
            waits
        });

        let thread = EmuThread::spawn(EmuThreadParams {
            emu: Arc::clone(&emu),
            input: Arc::new(SharedInput::new()),
            present: PresentBuffer::new(),
            ring: Some(Arc::clone(&ring)),
            region: Region::default(),
            rewind: RewindConfig::default(),
            run_ahead: RunAhead::default(),
            controls: Arc::new(SaveStateControls::default()),
        });

        let period = Duration::from_secs_f64(BUF as f64 / 2.0 / f64::from(RATE));
        let mut buf = vec![0.0f32; BUF];
        let mut fed = Vec::new();
        let start = Instant::now();
        let mut next = start;
        while start.elapsed() < OBSERVE {
            next += period;
            while Instant::now() < next {
                std::thread::sleep(Duration::from_micros(200));
            }
            ring.pull(&mut buf);
            // How much of this buffer was real audio. `pull` zero-fills the
            // tail on underrun, so trailing silence IS the underrun — but a
            // genuinely quiet passage also reads as zero, which is why the core
            // probe establishes separately that the stream is not silent.
            let real = buf.iter().rposition(|s| *s != 0.0).map_or(0, |i| i + 1);
            fed.push(real);
        }
        let stats = thread.stats();
        let (produced, bursts, snaps) = (
            stats.produced.load(Ordering::Relaxed),
            stats.catchup_bursts.load(Ordering::Relaxed),
            stats.snap_forwards.load(Ordering::Relaxed),
        );
        drop(thread);
        ui_stop.store(true, Ordering::Relaxed);
        let mut waits = ui.join().expect("UI stand-in thread finishes");
        waits.sort_unstable();

        let callbacks = fed.len();
        report_audio_gaps(
            &fed,
            BUF,
            period,
            &waits,
            (produced, bursts, snaps),
            OBSERVE,
        );

        assert!(
            callbacks > 100,
            "the consumer must actually have run; got {callbacks} callbacks"
        );
    }

    /// Print what the device saw. Split from the measurement so the two are
    /// separable — and because together they exceed the line gate.
    #[allow(
        clippy::cast_precision_loss,
        reason = "callback counts over a 10 s window are far below 2^53"
    )]
    fn report_audio_gaps(
        fed: &[usize],
        buf: usize,
        period: Duration,
        waits: &[Duration],
        pacer: (u64, u64, u64),
        observe: Duration,
    ) {
        let callbacks = fed.len();
        let starved = fed.iter().filter(|n| **n < buf).count();
        let empty = fed.iter().filter(|n| **n == 0).count();
        let delivered: usize = fed.iter().sum();
        let wanted = callbacks * buf;
        // Run lengths of fully-empty callbacks: the "off" half of the report.
        let mut gaps = Vec::new();
        let mut run = 0usize;
        for n in fed {
            if *n == 0 {
                run += 1;
            } else if run > 0 {
                gaps.push(run);
                run = 0;
            }
        }
        if run > 0 {
            gaps.push(run);
        }
        let ms = |n: usize| n as f64 * period.as_secs_f64() * 1000.0;
        let (produced, bursts, snaps) = pacer;

        println!("--- {callbacks} device callbacks of {buf} samples over {observe:?} ---");
        println!(
            "  callbacks fully fed   : {}/{callbacks}",
            callbacks - starved
        );
        println!("  callbacks fully empty : {empty}/{callbacks}");
        println!(
            "  samples delivered     : {delivered}/{wanted} ({:.1}%)",
            delivered as f64 / wanted as f64 * 100.0
        );
        // The second half separately: with a servo attached the first seconds
        // are its ramp (an empty ring being banked), and a whole-window figure
        // charges that startup transient to steady-state quality.
        let half = callbacks / 2;
        let tail: usize = fed[half..].iter().sum();
        println!(
            "  ...second half only   : {:.1}%  (steady state; the first half includes any servo ramp)",
            tail as f64 / ((callbacks - half) * buf) as f64 * 100.0
        );
        println!(
            "  first fully-fed call  : {}",
            fed.iter().position(|n| *n == buf).map_or_else(
                || "never".to_string(),
                |i| format!("#{i} ({:.0} ms in)", ms(i))
            )
        );
        println!(
            "  silent runs           : {}, mean {:.1} ms, max {:.1} ms",
            gaps.len(),
            ms(gaps.iter().sum::<usize>()) / gaps.len().max(1) as f64,
            ms(gaps.iter().copied().max().unwrap_or(0)),
        );
        println!("  pacer: {produced} frames, {bursts} bursts, {snaps} snap-forwards");
        println!(
            "  UI emu-lock wait      : p50 {:.3?}  p99 {:.3?}  max {:.3?}  (n={})",
            waits[waits.len() / 2],
            waits[waits.len() * 99 / 100],
            waits[waits.len() - 1],
            waits.len()
        );
        print!("  timeline: ");
        for n in fed {
            print!(
                "{}",
                match *n {
                    0 => '.',
                    x if x == buf => '#',
                    _ => '+',
                }
            );
        }
        println!();
    }

    /// **End-to-end: the producer actually publishes into the handoff.** The
    /// `emu-thread` feature shipped without this wiring, so the UI had nothing to
    /// read and took the emu mutex instead — the defect this whole change fixes.
    /// Remove the `core.publish_into(&present)` call and this goes red.
    ///
    /// It also pins the bytes/dims contract at the real producer: the published
    /// length must be exactly `w * h * 4`, not the `FB_MAX`-sized backing store.
    #[test]
    fn the_emu_thread_publishes_a_consistent_frame() {
        let present = PresentBuffer::new();
        let thread = EmuThread::spawn(EmuThreadParams {
            emu: Arc::new(Mutex::new(EmuCore::new(0))),
            input: Arc::new(SharedInput::new()),
            present: Arc::clone(&present),
            ring: None,
            region: Region::default(),
            rewind: RewindConfig::default(),
            run_ahead: RunAhead::default(),
            controls: Arc::new(SaveStateControls::default()),
        });

        // Bounded wait for the first publish. Generous so a slow CI box cannot
        // flake it; the assertion is that it happens at all, not how fast.
        let deadline = Instant::now() + Duration::from_secs(20);
        while !present.has_published() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(thread);

        assert!(
            present.has_published(),
            "the emu thread must publish frames into the handoff"
        );
        let mut out = Vec::new();
        let (w, h) = present
            .take_into(&mut out)
            .expect("a published frame is available to take");
        assert_eq!(
            out.len(),
            w as usize * h as usize * 4,
            "the published bytes must cover exactly the published dims"
        );
        assert!(w > 0 && h > 0, "published dims must be a real frame");
    }

    /// The frame due exactly AT the target counts; one before it does not.
    #[test]
    fn frames_due_starts_at_the_target() {
        let t0 = Instant::now();
        let s = Schedule::new(t0, PERIOD);
        assert_eq!(
            s.frames_due(t0.checked_sub(Duration::from_millis(1)).unwrap()),
            0,
            "nothing is due before the target"
        );
        assert_eq!(s.frames_due(t0), 1, "the frame at the target is due");
        assert_eq!(s.frames_due(t0 + PERIOD), 2, "one period late: two due");
    }

    /// **The cap is what keeps a slow core from starving the UI.** `RustyN64`'s core
    /// runs ~6.5x slower than real time, so it is behind on every iteration; an
    /// uncapped count would run frames back-to-back forever and never reach the
    /// wait that lets the UI take the emu mutex. Mutation-checked: removing the
    /// `.min(...)` makes this report 225,001 frames due.
    #[test]
    fn frames_due_is_capped_however_far_behind_the_core_falls() {
        let t0 = Instant::now();
        let s = Schedule::new(t0, PERIOD);
        assert_eq!(
            s.frames_due(t0 + Duration::from_hours(1)),
            MAX_CATCHUP_FRAMES,
            "an hour behind must still ask for at most the cap"
        );
    }

    /// A core that cannot keep up re-bases on `now` rather than replaying the
    /// window — and the new target is in the FUTURE, which is what makes the
    /// following wait a real wait. The old `next = now` left the target already
    /// reached, so the loop never waited and never released the emu mutex.
    #[test]
    fn a_core_that_cannot_keep_up_snaps_forward() {
        let t0 = Instant::now();
        let mut s = Schedule::new(t0, PERIOD);
        // One iteration of a core far slower than real time: it produced the capped
        // number of frames while ten periods of wall time went by.
        for _ in 0..MAX_CATCHUP_FRAMES {
            s.advance();
        }
        let now = t0 + PERIOD * 10;
        assert!(s.snap_if_behind(now), "still behind: must snap");
        assert_eq!(s.target(), now + PERIOD, "schedule re-based on now");
        assert!(
            s.target() > now,
            "the target must be in the future, or the wait is not a wait"
        );
    }

    /// **A core keeping up must NOT be snapped.** Snapping unconditionally would
    /// re-base the schedule every iteration and destroy the cadence, so the
    /// behind-check is load-bearing in both directions.
    #[test]
    fn a_core_keeping_up_is_left_on_cadence() {
        let t0 = Instant::now();
        let mut s = Schedule::new(t0, PERIOD);
        s.advance();
        let now = t0 + Duration::from_millis(1);
        assert!(!s.snap_if_behind(now), "ahead of schedule: no snap");
        assert_eq!(s.target(), t0 + PERIOD, "cadence preserved exactly");
    }

    /// The wait does not return early. Bounds are generous so CI scheduling noise
    /// cannot flake it; the assertion that matters is the lower one.
    #[test]
    fn block_until_native_does_not_return_before_the_target() {
        let target = Instant::now() + Duration::from_millis(20);
        block_until_native(target);
        assert!(Instant::now() >= target, "must not return early");
    }

    /// A target already in the past returns at once rather than waiting a period —
    /// the path taken on every iteration of a core that is behind.
    #[test]
    fn block_until_native_returns_at_once_for_a_past_target() {
        let start = Instant::now();
        block_until_native(start.checked_sub(Duration::from_millis(50)).unwrap());
        assert!(
            start.elapsed() < Duration::from_millis(50),
            "a past target must not sleep"
        );
    }
}
