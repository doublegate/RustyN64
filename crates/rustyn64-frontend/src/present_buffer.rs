//! Decoupled triple-buffer framebuffer handoff — the present path must never
//! wait on emulation.
//!
//! **Ported from `RustySNES/crates/rustysnes-frontend/src/present_buffer.rs`**,
//! itself ported from `RustyNES` (same author throughout, so the port is
//! license-clean). The triple-buffer SPSC handoff has no console-specific
//! content; only the framebuffer sizing hint differs.
//!
//! ## The defect this fixes
//!
//! `RustyN64` shipped the `emu-thread` feature **without** this handoff, so it did
//! exactly the thing `RustySNES`'s copy of this module documents as wrong: the
//! winit present path called `App::snapshot()`, which took the **emu mutex**
//! every UI frame merely to clone the framebuffer — while the emu thread held
//! that same mutex across its entire `coordinator.step()`. The UI therefore
//! serialized against a whole emulated frame, which is precisely what the
//! `emu-thread` feature exists to prevent.
//!
//! And the copy it was waiting for was far bigger than it needed to be.
//! `Frame::blank` sizes `rgba` at `FB_MAX_W * FB_MAX_H * 4` = **1,228,800 bytes**
//! and `produce_frame` never resizes it, so `frame.rgba.clone()` copied the whole
//! backing store every UI frame whatever the active resolution. Publishing the
//! `w * h * 4` **prefix** instead is both correct and the point: a 625x237 NTSC
//! scan-out is 592,500 bytes, and 320x240 is 307,200 — a copy less than a quarter
//! the size, and now off the emu mutex entirely.
//!
//! It was worse than a stall, because the emu thread also never yielded when it
//! fell behind (and being ~6.5x slower than real time, it always did): it reset
//! its pacing phase and immediately re-locked, holding the mutex ~100% of the
//! time in a tight loop. Against an unfair mutex the UI thread starved for many
//! frames at a stretch — measured by the user as menu clicks taking 15-45
//! seconds and one presented frame every 30-60 seconds.
//!
//! This module moves the framebuffer copy OFF the emu mutex onto a dedicated
//! handoff that is **never held across emulation work** — only for the brief
//! RGBA8 memcpy of a publish or a take. The producer writes the slot neither
//! side is reading (`back`) and publishes it (swap `back`<->`ready`); the
//! consumer swaps the freshest slot into its own hand (`front`<->`ready`). The
//! present thread can block at most for one framebuffer copy.
//!
//! ## Determinism (ADR 0004)
//!
//! Purely a presentation-path change: it moves *where* already-produced,
//! deterministic bytes are copied. It touches no emulated state and no audio, so
//! seed + ROM + input still gives a bit-identical framebuffer and audio stream.
//!
//! ## SPSC contract
//!
//! Exactly ONE producer (the emu thread) and ONE consumer (the winit present
//! path). The packed index's three 2-bit fields (`front`/`ready`/`back`) are
//! always a permutation of `{0,1,2}`, so producer and consumer touch disjoint
//! slots. A small dedicated `Mutex` guards the index, the `has_new` flag, the
//! slot bytes and the slot dims *together*, so a publish and a take stay
//! consistent without `unsafe` — held only for the copy. A `Relaxed` `has_new`
//! pre-check lets the consumer early-out without taking the mutex.
//!
//! ## N64-specific adaptation
//!
//! The N64's scan-out is variable — `Bus::scanout_scaled` returns 625x237 for a
//! typical NTSC title and up to 720x576 for PAL, and a title can reprogram the
//! VI mid-run. [`PresentBuffer::fb_len`] is therefore only an informational
//! worst-case sizing hint; the slots resize per publish, so a smaller frame is
//! never zero-padded up. Each slot's `(width, height)` travels **with** its
//! bytes for the same reason `RustySNES` needs it: the consumer must never
//! re-query the dims separately from the bytes it is about to upload, or a VI
//! reprogram between the two reads yields a size that does not match the data.
//! That is not hypothetical here — ledger R-18 records a single-point sample of
//! Super Mario 64 catching `H_VIDEO = 0` mid-reprogram.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

/// Worst-case N64 scan-out in bytes (720x576 PAL x RGBA8) — an informational
/// sizing hint only; the slots resize to whatever `publish` is given.
const FB_LEN: usize = 720 * 576 * 4;

/// The status-bar readings the producer publishes beside each frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PresentStatus {
    /// Frames the core has run.
    pub frames: u64,
    /// `master_ticks` (ADR 0006) — the canonical clock position.
    pub master_ticks: u64,
    /// Whether a ROM is loaded.
    pub rom_loaded: bool,
    /// Whether the core is paused.
    pub paused: bool,
}

/// The three reusable byte buffers behind the handoff.
struct Slots {
    bufs: [Vec<u8>; 3],
    /// The `(width, height)` each slot's bytes decode as. Travels with the bytes
    /// through the same lock so the pair is always consistent — see the module
    /// doc on VI reprogramming.
    dims: [(u32, u32); 3],
    /// `front | (ready << 2) | (back << 4)`. Mutated only under the slots lock,
    /// by `publish` and `take_into`, always keeping the fields a permutation of
    /// `{0,1,2}`.
    index: u8,
}

/// Triple buffer for the N64 framebuffer handoff.
///
/// Producer = emu thread, consumer = winit present path, guarded by a small
/// dedicated mutex held only for the brief publish/take copy — never across
/// emulation work.
pub struct PresentBuffer {
    /// Set by `publish`, cleared by `take_into`: whether `ready` is fresh.
    has_new: AtomicBool,
    /// Published-frame count (diagnostic — proves the producer is live).
    generation: AtomicUsize,
    slots: Mutex<Slots>,

    // --- Status-bar readings, published beside the frame. ---
    //
    // These live here rather than in a second primitive for one reason: they are
    // the OTHER thing the UI used to take the emu mutex for. `App::snapshot`
    // read the frame *and* the frame count, master ticks, loaded and paused flags
    // in one locked closure, so decoupling only the bytes would have left the
    // starvation in place for the sake of a status line. Plain atomics, written
    // once per frame by the producer and read freely by the UI: a status reading
    // that is one frame stale is invisible, and no reader can ever block a frame
    // of emulation to get it.
    //
    // They are four INDEPENDENT atomics, so `status()` may also tear *across*
    // fields — a frame count from frame N beside a tick count from N-1. That is
    // accepted for the same reason staleness is: it is a status line, the two
    // counters are never compared against each other, and the alternative is a
    // seqlock or a 128-bit atomic for a cosmetic readout. If anything ever needs a
    // consistent (frames, ticks) pair, add a seqlock generation here rather than
    // tightening the orderings, which cannot fix cross-field tearing.
    /// Frames the core has run (the status bar's frame counter).
    frames: AtomicU64,
    /// `master_ticks` (ADR 0006) — the status bar's tick counter.
    master_ticks: AtomicU64,
    /// Whether a ROM is loaded.
    rom_loaded: AtomicBool,
    /// Whether the core is paused.
    paused: AtomicBool,
}

impl PresentBuffer {
    /// Initial packed index: front = 0, ready = 1, back = 2.
    const INIT_INDEX: u8 = Self::pack(0, 1, 2);

    /// New, empty handoff (slots are zero-length until the first publish).
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            has_new: AtomicBool::new(false),
            generation: AtomicUsize::new(0),
            slots: Mutex::new(Slots {
                bufs: [Vec::new(), Vec::new(), Vec::new()],
                dims: [(0, 0), (0, 0), (0, 0)],
                index: Self::INIT_INDEX,
            }),
            frames: AtomicU64::new(0),
            master_ticks: AtomicU64::new(0),
            rom_loaded: AtomicBool::new(false),
            paused: AtomicBool::new(false),
        })
    }

    const fn front(idx: u8) -> usize {
        (idx & 0b11) as usize
    }
    const fn ready(idx: u8) -> usize {
        ((idx >> 2) & 0b11) as usize
    }
    const fn back(idx: u8) -> usize {
        ((idx >> 4) & 0b11) as usize
    }
    #[allow(clippy::cast_possible_truncation, reason = "each id is 0..=2")]
    const fn pack(front: usize, ready: usize, back: usize) -> u8 {
        (front as u8) | ((ready as u8) << 2) | ((back as u8) << 4)
    }

    /// Producer: copy `frame` and its `dims` into the back slot, then publish
    /// (swap back<->ready). The consumer's `front` is never touched, so a
    /// concurrent take contends only for the brief copy window.
    ///
    /// # Panics
    /// Only if the internal slots mutex is poisoned — not a condition this
    /// module's own code can create.
    pub fn publish(&self, frame: &[u8], dims: (u32, u32)) {
        let mut slots = self.slots.lock().expect("present buffer slots");
        let idx = slots.index;
        let back = Self::back(idx);
        {
            let buf = &mut slots.bufs[back];
            buf.clear();
            buf.extend_from_slice(frame);
        }
        slots.dims[back] = dims;
        // The index move, the slot write and the fresh flag all happen under the
        // same lock, so the consumer (which also locks) can never observe a
        // half-published frame.
        let front = Self::front(idx);
        let ready = Self::ready(idx);
        slots.index = Self::pack(front, back, ready);
        self.has_new.store(true, Ordering::Relaxed);
        drop(slots);
        // Bumped OUTSIDE the lock, deliberately: this is the present hot path and
        // the counter is a diagnostic, not part of the frame contract. The cost is
        // a bounded, cosmetic race with `reset` — an increment landing just after
        // `reset` zeroes the counter leaves `has_published()` true over an empty
        // handoff, so the present path re-shows its previous frame for one UI frame
        // instead of black. No slice is ever taken from that state: `has_new` and
        // the dims ARE under the lock, so `take_into` still returns `None`.
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    /// Consumer: if a frame was published since the last call, swap it into the
    /// front slot and copy it into `out`, returning its exact `(width, height)`.
    /// `None` means nothing new — the caller keeps presenting the previous `out`.
    ///
    /// The returned dims are always the pair `publish` was called with for these
    /// bytes, never a separately-queried reading that could disagree.
    ///
    /// # Panics
    /// Only if the internal slots mutex is poisoned.
    pub fn take_into(&self, out: &mut Vec<u8>) -> Option<(u32, u32)> {
        // Cheap pre-check off the lock; the authoritative check is under it.
        if !self.has_new.load(Ordering::Relaxed) {
            return None;
        }
        let mut slots = self.slots.lock().expect("present buffer slots");
        if !self.has_new.swap(false, Ordering::Relaxed) {
            return None;
        }
        let idx = slots.index;
        let front = Self::front(idx);
        let ready = Self::ready(idx);
        let back = Self::back(idx);
        // Swap front <-> ready: the producer's next publish reuses our old front
        // as its back, and we now hold the freshest frame.
        slots.index = Self::pack(ready, front, back);
        out.clear();
        out.extend_from_slice(&slots.bufs[ready]);
        Some(slots.dims[ready])
    }

    /// Producer: publish the status-bar readings. Called once per frame, right
    /// beside [`Self::publish`], and deliberately lock-free — see the field docs
    /// for why these live here instead of behind the emu mutex.
    pub fn publish_status(&self, frames: u64, master_ticks: u64, rom_loaded: bool, paused: bool) {
        self.frames.store(frames, Ordering::Relaxed);
        self.master_ticks.store(master_ticks, Ordering::Relaxed);
        self.rom_loaded.store(rom_loaded, Ordering::Relaxed);
        self.paused.store(paused, Ordering::Relaxed);
    }

    /// Consumer: the most recently published status readings.
    #[must_use]
    pub fn status(&self) -> PresentStatus {
        PresentStatus {
            frames: self.frames.load(Ordering::Relaxed),
            master_ticks: self.master_ticks.load(Ordering::Relaxed),
            rom_loaded: self.rom_loaded.load(Ordering::Relaxed),
            paused: self.paused.load(Ordering::Relaxed),
        }
    }

    /// True once at least one frame has been published, so the present path can
    /// tell "no ROM / nothing produced yet" from "have a frame".
    #[must_use]
    pub fn has_published(&self) -> bool {
        self.generation.load(Ordering::Relaxed) > 0
    }

    /// Frames published so far — the status bar's liveness reading, and the way
    /// to tell a starved producer from a stalled consumer.
    #[must_use]
    pub fn generation(&self) -> usize {
        self.generation.load(Ordering::Relaxed)
    }

    /// Reset to the empty state on a ROM load / power cycle, so the next present
    /// shows black until a new frame arrives.
    ///
    /// # Panics
    /// Only if the internal slots mutex is poisoned.
    /// The ROM-loaded and paused flags are deliberately left alone: they are the
    /// caller's state, not the handoff's, and the producer republishes them with
    /// the next frame.
    pub fn reset(&self) {
        self.frames.store(0, Ordering::Relaxed);
        self.master_ticks.store(0, Ordering::Relaxed);
        // Every part of the frame handoff is cleared under the ONE lock that
        // `publish` and `take_into` also take, so a concurrent publish either
        // completes entirely before this reset or entirely after it.
        //
        // `has_new` used to be cleared BEFORE acquiring the lock, which broke this
        // module's own contract (see the SPSC section of the module doc: the mutex
        // guards the flag, the index, the bytes and the dims *together*). A
        // publish landing in that window set `has_new = true` and then had its
        // bytes wiped, so the next `take_into` returned `Some(dims)` for a
        // zero-length buffer — and the present path slices `out[..w * h * 4]` from
        // exactly that pair, which panics. `reset` runs on the UI thread while the
        // emu thread publishes, so the window was real, not theoretical.
        //
        // Clearing the dims is the second, independent half: it makes the pair
        // "nonzero dims, no bytes" *unrepresentable* rather than merely
        // unreachable, so the invariant does not rest on the locking alone.
        //
        // `generation` is the one field that stays racy on purpose — see `publish`
        // for why, and for the bound on what that race can do.
        let mut slots = self.slots.lock().expect("present buffer slots");
        self.has_new.store(false, Ordering::Relaxed);
        self.generation.store(0, Ordering::Relaxed);
        slots.index = Self::INIT_INDEX;
        for b in &mut slots.bufs {
            b.clear();
        }
        slots.dims = [(0, 0); 3];
    }

    /// Worst-case N64 scan-out byte length (for the no-ROM black frame) — a
    /// sizing hint only; see the module doc for why it is not load-bearing.
    #[must_use]
    pub const fn fb_len() -> usize {
        FB_LEN
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_then_take_roundtrips() {
        let pb = PresentBuffer::new();
        assert!(!pb.has_published());
        let frame = vec![0xABu8; 16];
        pb.publish(&frame, (2, 2));
        assert!(pb.has_published());
        let mut out = Vec::new();
        assert_eq!(pb.take_into(&mut out), Some((2, 2)));
        assert_eq!(out, frame);
    }

    /// A take with nothing new returns `None` and leaves `out` untouched, so the
    /// caller re-presents the previous frame rather than a black one.
    #[test]
    fn a_take_with_nothing_new_leaves_the_previous_frame() {
        let pb = PresentBuffer::new();
        pb.publish(&[1, 2, 3, 4], (1, 1));
        let mut out = Vec::new();
        assert_eq!(pb.take_into(&mut out), Some((1, 1)));
        assert_eq!(pb.take_into(&mut out), None, "nothing new");
        assert_eq!(out, vec![1, 2, 3, 4], "out must be preserved");
    }

    /// **The freshest frame wins, and no slot aliases another.** Publishing twice
    /// between takes must hand over the SECOND frame, not the first — a
    /// mis-packed index would return stale bytes or alias the producer's slot.
    #[test]
    fn publishing_twice_between_takes_yields_the_newest() {
        let pb = PresentBuffer::new();
        pb.publish(&[0xAA; 8], (2, 1));
        pb.publish(&[0xBB; 8], (4, 1));
        let mut out = Vec::new();
        assert_eq!(pb.take_into(&mut out), Some((4, 1)));
        assert_eq!(out, vec![0xBB; 8], "the newest frame, not the first");
    }

    /// Dims travel WITH the bytes: a publish whose size changes must never let a
    /// consumer pair new dims with old bytes (the VI-reprogram hazard).
    #[test]
    fn dims_travel_with_their_bytes_across_a_size_change() {
        let pb = PresentBuffer::new();
        let mut out = Vec::new();
        pb.publish(&[1; 4], (1, 1));
        assert_eq!(pb.take_into(&mut out), Some((1, 1)));
        assert_eq!(out.len(), 4);
        pb.publish(&[2; 16], (2, 2));
        assert_eq!(pb.take_into(&mut out), Some((2, 2)));
        assert_eq!(out.len(), 16, "the bytes resized with their dims");
    }

    /// The index stays a permutation of {0,1,2} over many cycles — the invariant
    /// that keeps producer and consumer on disjoint slots.
    #[test]
    fn the_slot_index_stays_a_permutation() {
        let pb = PresentBuffer::new();
        let mut out = Vec::new();
        for i in 0..32u8 {
            pb.publish(&[i; 4], (1, 1));
            let _ = pb.take_into(&mut out);
            let index = {
                let slots = pb.slots.lock().unwrap();
                slots.index
            };
            let mut seen = [
                PresentBuffer::front(index),
                PresentBuffer::ready(index),
                PresentBuffer::back(index),
            ];
            seen.sort_unstable();
            assert_eq!(seen, [0, 1, 2], "iteration {i}: slots must stay disjoint");
        }
    }

    #[test]
    fn reset_clears_the_published_state() {
        let pb = PresentBuffer::new();
        pb.publish(&[9; 4], (1, 1));
        pb.reset();
        assert!(!pb.has_published());
        let mut out = Vec::new();
        assert_eq!(pb.take_into(&mut out), None);
    }

    /// **`reset` must leave no dims behind.** "Nonzero dims, zero bytes" is the
    /// pair a torn reset could hand a consumer, and the present path slices
    /// `out[..w * h * 4]` straight from it. Clearing the dims makes that pair
    /// unrepresentable independently of the locking that makes it unreachable, so
    /// this is the deterministic half of the fix — restore the old `reset`, which
    /// cleared the bytes but not the dims, and this goes red.
    #[test]
    fn reset_clears_the_dims_so_none_can_be_paired_with_empty_bytes() {
        let pb = PresentBuffer::new();
        pb.publish(&[7; 16], (2, 2));
        let mut out = Vec::new();
        assert_eq!(pb.take_into(&mut out), Some((2, 2)));
        pb.reset();
        let slots = pb.slots.lock().unwrap();
        assert_eq!(slots.dims, [(0, 0); 3], "every slot's dims must be cleared");
        for (i, b) in slots.bufs.iter().enumerate() {
            assert!(b.is_empty(), "slot {i}: bytes must be cleared");
        }
    }

    /// The status readings round-trip, and `reset` zeroes the two counters while
    /// leaving the ROM/pause flags to the caller that owns them — a closed ROM
    /// must therefore republish `rom_loaded = false` rather than expect `reset` to
    /// infer it.
    #[test]
    fn status_round_trips_and_reset_zeroes_only_the_counters() {
        let pb = PresentBuffer::new();
        assert_eq!(pb.status(), PresentStatus::default());
        pb.publish_status(42, 1_234_567, true, true);
        assert_eq!(
            pb.status(),
            PresentStatus {
                frames: 42,
                master_ticks: 1_234_567,
                rom_loaded: true,
                paused: true,
            }
        );
        pb.reset();
        let s = pb.status();
        assert_eq!((s.frames, s.master_ticks), (0, 0), "counters restart");
        assert!(
            s.rom_loaded,
            "reset does not decide whether a ROM is loaded"
        );
        assert!(s.paused, "nor whether the core is paused");
    }
}
