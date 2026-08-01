//! Present the frame through the **GPU** RDP (ADR 0014).
//!
//! # What this is, precisely
//!
//! A **display backend**, not a replacement rasterizer, and the distinction is
//! load-bearing rather than pedantic.
//!
//! `rustyn64-core` is `#![no_std]` and `#![forbid(unsafe_code)]`, so the `Bus`
//! cannot own a Vulkan device — the crate graph forbids it and that is the right
//! call, not an obstacle to work around. The software rasterizer therefore still
//! runs and still writes into the Bus's RDRAM, which is what keeps a game's
//! framebuffer read-backs working. This module renders the *same* command stream
//! a second time on the GPU and presents that result instead.
//!
//! So: two rasterizers run, the machine's state comes from the software one, and
//! the picture comes from parallel-rdp. That is more work per frame, not less
//! (see "Cost" below), and it is a stepping stone rather than a destination.
//!
//! # How the stream gets here
//!
//! `Bus::rdp_tap` (the `rdp-tap` feature) records every command word the Bus
//! feeds the RDP, captured by diffing the FIFO pointer so it cannot disagree
//! with the RDP about where a command ends. Re-reading the list from RDRAM would
//! not work: by the time the frontend looks, `DPC_CURRENT` has reached
//! `DPC_END` and the game has usually overwritten the buffer.
//!
//! # RDRAM
//!
//! parallel-rdp renders into **its own** RDRAM, which must therefore be seeded
//! with the Bus's each frame — textures live there, and so does the target
//! framebuffer. The transfer is a whole-RDRAM snapshot, which is:
//!
//! - **correct by construction.** There is no race, no dirty-region tracker to
//!   get subtly wrong, and therefore none of the "a wrong pixel occasionally, on
//!   some machines" hazard ADR 0014 §5 flags as the dangerous part.
//! - **expensive.** 8 MiB per frame, plus the word swap (see below). Measured
//!   and reported rather than estimated; dirty-region tracking is the named next
//!   step, and it is a separate piece of work with its own risk.
//!
//! # Byte order
//!
//! parallel-rdp stores RDRAM in native little-endian word order
//! (`vram8.data[index ^ 3]` throughout its shaders); the Bus stores big-endian
//! bytes. Every crossing reverses each aligned 4-byte group. Getting it wrong
//! does not fail — it renders the right shapes in the wrong colors.
//!
//! # Determinism
//!
//! ADR 0004 binds the **core**, and this is the frontend, so nothing here
//! changes what the deterministic core computes: the software rasterizer's
//! output is still what lands in RDRAM and still what a save-state captures.
//! What is presented is not covered by that contract, and this module makes no
//! determinism claim of its own.

use core::cell::RefCell;
use core::sync::atomic::{AtomicU64, Ordering};
use rustyn64_core::Bus;
use rustyn64_core::vi;
use rustyn64_rdp_gpu::{GpuRdp, ScanoutFrame, ViRegister};

/// 8 MiB, matching `Bus::rdram`. A multiple of 4096, which [`GpuRdp::new`]
/// requires.
const RDRAM_SIZE: usize = 8 * 1024 * 1024;

/// parallel-rdp's hidden RDRAM: one byte per RDRAM halfword.
const HIDDEN_RDRAM_SIZE: usize = RDRAM_SIZE / 2;

/// The VI registers to forward, as `(RustyN64 index, parallel-rdp register)`.
///
/// Written out rather than derived from the fact that the two enumerations
/// happen to agree on 0..=13. They agree **today**; a table that silently
/// depended on that would misprogram the video interface the moment either side
/// inserted a register, and the symptom would be a wrong picture rather than a
/// compile error.
const VI_FORWARD: &[(u32, ViRegister)] = &[
    (vi::VI_CTRL, ViRegister::Control),
    (vi::VI_ORIGIN, ViRegister::Origin),
    (vi::VI_WIDTH, ViRegister::Width),
    (vi::VI_V_INTR, ViRegister::Intr),
    (vi::VI_V_CURRENT, ViRegister::VCurrentLine),
    (vi::VI_BURST, ViRegister::Timing),
    (vi::VI_V_TOTAL, ViRegister::VSync),
    (vi::VI_H_TOTAL, ViRegister::HSync),
    (vi::VI_H_TOTAL_LEAP, ViRegister::Leap),
    (vi::VI_H_VIDEO, ViRegister::HStart),
    (vi::VI_V_VIDEO, ViRegister::VStart),
    (vi::VI_V_BURST, ViRegister::VBurst),
    (vi::VI_X_SCALE, ViRegister::XScale),
    (vi::VI_Y_SCALE, ViRegister::YScale),
];

// The presenter lives in THREAD-LOCAL storage, and that is a correctness
// decision rather than a convenience.
//
// `GpuRdp` is `!Send`: `Vulkan::Device` runs a worker thread and
// `CommandProcessor` holds a command ring, and nothing upstream documents that
// the handle may cross threads. The alternative — an `unsafe impl Send`
// justified by a guess — would trade a compile error for a data race, which is
// the worst trade available. `EmuCore` is moved onto the emulation thread under
// the default-on `emu-thread` feature, so a plain field would have made it
// `!Send` and broken that move.
//
// Keeping the device in TLS means it is created, used, and destroyed on exactly
// one thread: whichever one calls `present`. No `unsafe`, no claim to defend.
thread_local! {
    static PRESENTER: RefCell<Option<GpuPresenter>> = const { RefCell::new(None) };
    /// Distinguishes "not tried yet" from "tried and there is no device", so a
    /// machine without Vulkan pays for one failed device probe rather than one
    /// per frame.
    static PROBED: RefCell<bool> = const { RefCell::new(false) };
}

/// Frames the GPU backend has produced, across every thread.
///
/// A global rather than a field on the presenter because the presenter is
/// thread-local and the UI thread has to be able to read this — the point of the
/// counter is telling "enabled but never produced a frame" apart from
/// "disabled", and those look identical from outside.
static PRODUCED: AtomicU64 = AtomicU64::new(0);

/// Frames the GPU backend has produced.
#[must_use]
pub fn frames_produced() -> u64 {
    PRODUCED.load(Ordering::Relaxed)
}

/// Whether the device probe ran on this thread and found nothing usable.
///
/// Exists so a test can tell "the GPU path is silently not running" from "there
/// is no GPU here", which are the same observation from outside and mean
/// opposite things.
#[must_use]
pub fn probe_failed() -> bool {
    PROBED.with(|p| *p.borrow()) && PRESENTER.with(|p| p.borrow().is_none())
}

/// Render one frame through the GPU backend, creating it on first use.
///
/// Returns `None` — leaving `out` untouched — when there is no usable device or
/// the GPU path declined to produce a picture, so the caller falls back to the
/// software scan-out.
///
/// The device is created on the **calling** thread and never leaves it.
pub fn present(bus: &mut Bus, out: &mut [u8]) -> Option<(u32, u32)> {
    let already_probed = PROBED.with(|p| core::mem::replace(&mut *p.borrow_mut(), true));
    if !already_probed {
        PRESENTER.with(|p| *p.borrow_mut() = GpuPresenter::new());
    }
    PRESENTER.with(|p| {
        let mut slot = p.borrow_mut();
        let presenter = slot.as_mut()?;
        let geometry = presenter.present(bus, out)?;
        PRODUCED.fetch_add(1, Ordering::Relaxed);
        Some(geometry)
    })
}

/// The GPU display backend, and the scratch it reuses across frames.
struct GpuPresenter {
    gpu: GpuRdp,
    /// Scan-out pixels as `u32` RGBA8, reused across frames so the per-frame
    /// path allocates nothing.
    pixels: Vec<u32>,
}

impl GpuPresenter {
    /// Create the backend, or `None` when there is no usable Vulkan device.
    ///
    /// `None` is an ordinary outcome (no GPU, a headless session, a driver that
    /// cannot run parallel-rdp) and the caller is expected to fall back to the
    /// software scan-out rather than fail.
    fn new() -> Option<Self> {
        let gpu = GpuRdp::new(RDRAM_SIZE, HIDDEN_RDRAM_SIZE)?;
        if !gpu.device_is_supported() {
            return None;
        }
        Some(Self {
            gpu,
            pixels: Vec::new(),
        })
    }

    /// Render one frame from `bus` and write it into `out` as RGBA8.
    ///
    /// Returns the produced geometry, or `None` when the VI produced no picture
    /// (it is off, or unconfigured), when the frame does not fit `out`, or when
    /// any step of the GPU path failed. Every `None` leaves `out` untouched, so
    /// a caller that falls back to the software scan-out cannot present a
    /// half-written buffer.
    ///
    /// Drains `bus`'s command tap whether or not a picture results — leaving it
    /// undrained would replay this frame's commands on top of the next one's.
    fn present(&mut self, bus: &mut Bus, out: &mut [u8]) -> Option<(u32, u32)> {
        let commands = bus.take_rdp_commands();

        // Seed RDRAM from the Bus. Whole-buffer and unconditional: the alternative
        // is knowing which bytes the CPU touched since the last frame, which is
        // the dirty-region tracker ADR 0014 §5 calls the dangerous part.
        //
        // The swap writes STRAIGHT INTO the mapped buffer. Staging it first and
        // then copying cost a second full pass over 8 MiB — measured at ~2.1 ms
        // for the swap and ~2.6 ms for the copy, together ~90% of the frame's
        // GPU cost. Fusing them halves the traffic and removes an 8 MiB
        // allocation; `docs/performance.md` carries the before/after.
        self.gpu
            .with_rdram_mut(|rdram| swap_words_into(rdram, &bus.rdram))?;

        for cmd in split_commands(&commands) {
            if !self.gpu.enqueue_command(cmd) {
                return None;
            }
        }

        for &(idx, reg) in VI_FORWARD {
            if !self.gpu.set_vi_register(reg, bus.vi.read(idx)) {
                return None;
            }
        }

        // Size the read-back to what the caller can hold, in pixels. `scanout_sync`
        // refuses rather than truncates when a frame is larger, which is the
        // behavior wanted here: a truncated frame is a plausible-looking wrong
        // picture and the caller cannot tell it from a real one.
        let capacity = out.len() / 4;
        if self.pixels.len() < capacity {
            self.pixels.resize(capacity, 0);
        }
        let ScanoutFrame {
            width,
            height,
            pixels,
        } = self.gpu.scanout_sync(&mut self.pixels[..capacity])?;

        // parallel-rdp hands back RGBA8 already in R,G,B,A byte order (verified
        // in `rustyn64-rdp-gpu`'s smoke test, where an opaque-red fill reads back
        // as 0xFF0000FF little-endian). So this is a plain reinterpretation, not
        // a conversion.
        for (dst, &px) in out.chunks_exact_mut(4).zip(&self.pixels[..pixels]) {
            dst.copy_from_slice(&px.to_le_bytes());
        }

        Some((width, height))
    }
}

/// Copy `src` into `dst`, reversing each aligned 4-byte group.
///
/// Converts between the Bus's big-endian RDRAM bytes and parallel-rdp's native
/// word order. Its own inverse, which is why one function serves both
/// directions.
///
/// # Panics
///
/// Panics if the lengths differ or are not a multiple of 4 — a partial trailing
/// group has no defined mapping, and leaving it unswapped would corrupt exactly
/// the last pixels of a framebuffer, where nobody looks.
fn swap_words_into(dst: &mut [u8], src: &[u8]) {
    assert_eq!(dst.len(), src.len(), "word-swap length mismatch");
    assert_eq!(src.len() % 4, 0, "word-swap needs whole 32-bit words");
    for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
        d.copy_from_slice(
            &u32::from_ne_bytes([s[0], s[1], s[2], s[3]])
                .swap_bytes()
                .to_ne_bytes(),
        );
    }
}

/// Split a flat command-word stream into individual commands.
///
/// parallel-rdp's `enqueue_command` takes **one** command per call and reads
/// however many words the opcode implies, so the stream cannot be handed over
/// whole. The lengths come from `rustyn64_rdp::command`, the same table the
/// software RDP consumes the FIFO with, so the two cannot disagree about where a
/// command ends.
fn split_commands(words: &[u32]) -> impl Iterator<Item = &[u32]> {
    let mut i = 0usize;
    core::iter::from_fn(move || {
        let opcode = rustyn64_rdp::command::opcode_of(*words.get(i)?);
        let len = rustyn64_rdp::command::command_len_words(opcode) as usize * 2;
        // A zero length would not advance the cursor and a short tail cannot be
        // decoded; both end the stream rather than looping or reading past it.
        if len == 0 || i + len > words.len() {
            return None;
        }
        let out = &words[i..i + len];
        i += len;
        Some(out)
    })
}

#[cfg(test)]
mod tests {
    use super::{split_commands, swap_words_into};

    /// The swap is an involution, and it is the 4-byte one.
    ///
    /// Both halves matter: an 8-byte reversal would also round-trip, so the
    /// involution alone does not pin the permutation.
    #[test]
    fn word_swap_reverses_each_aligned_four_bytes() {
        let mut out = [0u8; 8];
        swap_words_into(&mut out, &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(out, [4, 3, 2, 1, 8, 7, 6, 5]);
        let mut back = [0u8; 8];
        swap_words_into(&mut back, &out);
        assert_eq!(back, [1, 2, 3, 4, 5, 6, 7, 8]);
    }

    /// Commands are split by the RDP's own length table.
    ///
    /// A Sync Full (0x29, 1 u64 word = 2 u32 words) then a Set Fill Color
    /// (0x37, likewise), then a Fill Triangle (0x08, 4 u64 words = 8 u32 words).
    #[test]
    fn commands_split_on_their_declared_lengths() {
        let mut stream = vec![0x2900_0000, 0, 0x3700_0000, 0xFF00_00FF];
        stream.push(0x0800_0000);
        stream.extend(core::iter::repeat_n(0, 7));
        let lens: Vec<usize> = split_commands(&stream).map(<[u32]>::len).collect();
        assert_eq!(lens, vec![2, 2, 8]);
    }

    /// A trailing partial command is dropped, not decoded against words that
    /// are not there.
    #[test]
    fn a_short_tail_is_dropped() {
        // A Fill Triangle needs 8 u32 words; give it 3.
        let stream = vec![0x0800_0000, 0, 0];
        assert_eq!(split_commands(&stream).count(), 0);
    }
}
