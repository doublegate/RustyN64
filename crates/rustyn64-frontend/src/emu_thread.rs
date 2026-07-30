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
//! run-ahead + save/load), which is a plain `run_frame` when both are off. The
//! resampler servo is still a roadmap refinement.
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
use crate::emu::EmuCore;
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
                        let audio = emu.lock().map_or_else(
                            |_| Vec::new(),
                            |mut core| {
                                core.set_controllers(ports);
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
