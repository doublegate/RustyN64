//! `rustyn64-audio` — the Audio Interface (AI) DAC + sample-DMA path.
//!
//! The RSP's audio microcode mixes samples into an RDRAM buffer; the AI then
//! DMAs that buffer out to the DAC at a programmable sample rate (set via the
//! `AI_DACRATE` divider off the video clock) and raises an AI interrupt when a
//! buffer **starts** playing (not when it drains — see [`Audio::write_reg`]).
//! This crate models the AI side; the actual mixing is RSP microcode (in
//! `rustyn64-rsp`), so under LLE the audio "falls out free" (ADR 0002).
//!
//! The model follows the hardware description in
//! `n64brew_wiki/markdown/Audio Interface.md` and the reference behavior in
//! ares (ISC) `ref-proj/ares/ares/n64/ai/`: a two-deep DMA FIFO, the
//! delayed-carry address bug reproduced as a 13-bit/11-bit split with a
//! one-sample-deferred carry, the `AI_LENGTH` mirror on every write-only
//! register, and a DAC that decays toward silence on underrun.
//!
//! Part of the one-directional chip-crate graph (see `docs/architecture.md`):
//! this crate does NOT depend on any other chip crate; it reaches RDRAM and the
//! interrupt line through the [`AudioBus`] trait. `#![no_std]` + `alloc`.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
// The DAC deliberately reinterprets the 32-bit RDRAM word's halves as signed
// 16-bit samples, so a wrapping u->i cast is the intended operation.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_lossless,
    clippy::cast_possible_wrap
)]

extern crate alloc;

use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

/// The canonical master clock (`MASTER_HZ`, ADR 0006), duplicated here because
/// the chip-crate graph forbids `rustyn64-audio` depending on `rustyn64-core`.
///
/// The DAC period (master ticks per output sample) is `MASTER_HZ / sample_rate`.
/// A cross-crate test in `rustyn64-core` asserts this equals the scheduler's
/// `MASTER_HZ`, so the two cannot drift.
pub const MASTER_HZ: u64 = 187_500_000;

/// Video clock feeding the AI DAC divider on **NTSC** consoles (Hz).
///
/// The sample rate is this divided by `AI_DACRATE + 1`, so `AI_DACRATE = 1103`
/// yields ~44.1 kHz. Provenance: project64/N64-Tests `DoubleShot` (which
/// computes `(VI_NTSC_CLOCK / FREQ) - 1`) and the N64brew wiki. Documented, not
/// tuned — accuracy-ledger entry for the region video clock.
pub const VIDEO_CLOCK_NTSC: u32 = 48_681_812;

/// Video clock feeding the AI DAC divider on **PAL** consoles (Hz).
///
/// Same derivation as [`VIDEO_CLOCK_NTSC`]; the differing clock is why the same
/// `AI_DACRATE` detunes between regions. PAL VI *cadence* is still residual R-6;
/// this constant is only the AI divisor.
pub const VIDEO_CLOCK_PAL: u32 = 49_656_530;

/// The emulated console region, selecting the AI video clock.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Region {
    /// NTSC (~60 Hz), the common region.
    #[default]
    Ntsc,
    /// PAL (~50 Hz).
    Pal,
}

impl Region {
    /// The video clock (Hz) this region feeds into the AI DAC divider.
    #[must_use]
    pub const fn video_clock(self) -> u32 {
        match self {
            Self::Ntsc => VIDEO_CLOCK_NTSC,
            Self::Pal => VIDEO_CLOCK_PAL,
        }
    }

    /// The region a cartridge's **destination code** (ROM header byte `0x3E`, the
    /// last character of the 4-byte game code) implies.
    ///
    /// The destination characters are the N64brew Wiki *ROM Header* §Standard
    /// header table (read from `n64brew_wiki/html/ROM Header.xhtml` — the markdown
    /// mirror renders that table as a bare `[TABLE]` placeholder):
    ///
    /// `A` All · `B` Brazil · `C` China · `D` Germany · `E` North America ·
    /// `F` France · `G` Gateway 64 (NTSC) · `H` Netherlands · `I` Italy ·
    /// `J` Japan · `K` Korea · `L` Gateway 64 (PAL) · `N` Canada · `P` Europe ·
    /// `S` Spain · `U` Australia · `W` Scandinavia · `X`/`Y`/`Z` Europe
    ///
    /// **Provenance, and its limit.** That table gives *destinations*, **not TV
    /// standards** — only `G`/`L` are labeled NTSC/PAL by the wiki itself. The
    /// 50/60 Hz classification of the remaining countries is the **broadcast
    /// standard for each territory**, not a hardware-documented N64 fact, and
    /// there is **no oracle** for it (no test ROM checks region detection). It is
    /// therefore modeled and ledgered as documented-but-ungated rather than
    /// presented as measured — see `docs/accuracy-ledger.md` (T-71-005).
    ///
    /// Three cases are decided explicitly rather than guessed:
    /// - **`B` Brazil → NTSC.** Brazil broadcast PAL-**M**, which is a *60 Hz*
    ///   standard; for the AI divisor it behaves as NTSC, not as 50 Hz PAL.
    /// - **`C` China → NTSC.** A PAL territory, but with no known retail N64; the
    ///   default is kept rather than inventing a 50 Hz cartridge that never shipped.
    /// - **`A` "All" → NTSC.** Names no single region, so it takes the default.
    ///
    /// Any unrecognized byte also returns [`Region::Ntsc`], so an unknown or
    /// homebrew code is behavior-preserving rather than silently retuning audio.
    #[must_use]
    pub const fn from_destination_code(code: u8) -> Self {
        match code {
            // Europe and the 50 Hz territories.
            b'D' | b'F' | b'H' | b'I' | b'L' | b'P' | b'S' | b'U' | b'W' | b'X' | b'Y' | b'Z' => {
                Self::Pal
            }
            // North America, Japan, Korea, Canada, Gateway-NTSC, Brazil (PAL-M is
            // 60 Hz), China (no retail N64), and "All" — plus every unknown byte.
            _ => Self::Ntsc,
        }
    }
}

/// The narrow bus the AI sees (`RustyNES`'s `ApuBus` analog): fetch a DMA sample
/// word from RDRAM and raise the AI interrupt when a buffer starts.
pub trait AudioBus {
    /// Fetch a big-endian 32-bit sample word (two 16-bit L/R samples) from the
    /// AI DMA buffer in RDRAM at `addr`.
    fn ai_dma_read_u32(&self, addr: u32) -> u32;
    /// Raise the AI interrupt on the MI (a queued buffer became active). Default
    /// no-op so a non-interrupt bus can still drive the DAC.
    fn raise_ai_interrupt(&mut self) {}
}

/// One stereo output frame (interleaved signed 16-bit L, R).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StereoSample {
    /// Left channel.
    pub left: i16,
    /// Right channel.
    pub right: i16,
}

/// The result an AI register write hands back to the Bus, which owns the MI
/// interrupt lines the AI itself cannot name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AiIrq {
    /// The write had no interrupt effect.
    #[default]
    None,
    /// Raise `MI_INTR.ai` — the first buffer of an idle queue was enqueued and
    /// starts immediately.
    Raise,
    /// Lower `MI_INTR.ai` — a write to `AI_STATUS` acknowledges the interrupt.
    Lower,
}

/// Audio Interface state: the two-deep DMA FIFO, the DAC rate divider, and the
/// derived-timing sample emission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Audio {
    // --- The two-deep DMA FIFO (front = index 0). ---
    /// RDRAM base of each queued transfer (24-bit, 8-byte aligned). Slot 0 is
    /// the transfer currently draining.
    dma_addr: [u32; 2],
    /// Remaining bytes of each queued transfer (18-bit, 8-byte aligned).
    dma_len: [u32; 2],
    /// Number of queued transfers, 0..=2. `BUSY` = `> 0`, `FULL` = `> 1`.
    dma_count: u8,
    /// Deferred carry into `dma_addr[0]` bits 13..=23 — the delayed-carry bug.
    /// Set when the low 13 bits wrap past an `0x2000` page, applied one sample
    /// later (which is the *next transfer* when it wraps on the final sample).
    addr_carry: u32,
    /// `AI_CONTROL` bit 0.
    dma_enable: bool,

    // --- The DAC rate. ---
    /// `AI_DACRATE` (14-bit sample-period divider).
    dac_rate: u16,
    /// `AI_BITRATE` (4-bit half-bit-clock divider).
    bit_rate: u8,
    /// The region video clock (Hz) the DAC rate divides.
    video_clock: u32,
    /// Derived output sample rate in Hz, `video_clock / (dac_rate + 1)`, or
    /// [`Audio::DEFAULT_DAC_HZ`] before `AI_DACRATE` is programmed.
    ///
    /// Never 0 on a constructed machine: a zero rate stops the DAC, and a
    /// stopped DAC cannot retire a queued transfer (ledger R-16).
    sample_rate: u32,

    // --- Derived-timing emission (ADR 0006: everything off `master_ticks`). ---
    /// Master tick at which the next output sample is due; 0 = unanchored.
    next_sample_tick: u64,
    /// The most recent `master_ticks` the DAC was advanced to (drives the
    /// `AI_STATUS` `COUNT`/`WC` readback without threading the clock into reads).
    last_tick: u64,
    /// Last emitted sample, held and decayed on underrun so the DAC does not
    /// hard-stop (matches ares' decay-to-silence behavior).
    dac_hold: StereoSample,
    /// Count of buffer starvations (a transfer drained with none queued behind
    /// it) — observable so a resampler cannot silently paper over underrun.
    underruns: u64,

    /// The emitted stereo stream on the emulated timeline, drained per frame by
    /// the frontend (which resamples to the host rate — ADR 0004).
    sink: Vec<StereoSample>,
}

impl Default for Audio {
    fn default() -> Self {
        Self::new()
    }
}

impl Audio {
    /// The 8 KiB (`0x2000`) page whose crossing arms the delayed-carry bug.
    const PAGE: u32 = 0x2000;

    /// DAC rate used while `AI_DACRATE` is unprogrammed.
    ///
    /// **A modeling default, not a measured hardware value — see ledger R-16.**
    /// `AI_DACRATE`'s reset value is not documented anywhere this project
    /// mirrors, so no rate can be *derived* for the pre-programmed window. What
    /// matters, and what IS established, is structural: the DAC has no stopped
    /// state, so the transfer queue must be able to retire before software
    /// programs the rate. This value is taken from ares (ISC, vendorable), whose
    /// `AI::power()` sets `dac.frequency = 44100`.
    ///
    /// Nothing observable should depend on the exact number: every title
    /// programs `AI_DACRATE` before it plays anything, so this rate only governs
    /// how fast an unprogrammed DAC drains silence. If a future change makes an
    /// output depend on it, that dependency is the bug, not this constant.
    const DEFAULT_DAC_HZ: u32 = 44_100;

    /// Construct at power-on (NTSC, idle).
    #[must_use]
    pub const fn new() -> Self {
        let mut ai = Self {
            dma_addr: [0; 2],
            dma_len: [0; 2],
            dma_count: 0,
            addr_carry: 0,
            dma_enable: false,
            dac_rate: 0,
            bit_rate: 0,
            video_clock: VIDEO_CLOCK_NTSC,
            // Placeholder only — `recompute_rate` below derives the real value.
            sample_rate: 0,
            next_sample_tick: 0,
            last_tick: 0,
            dac_hold: StereoSample { left: 0, right: 0 },
            underruns: 0,
            sink: Vec::new(),
        };
        // Derive the power-on rate through the SAME function every later rate
        // change uses, rather than repeating its `dac_rate == 0` decision here.
        // The duplicate literal this replaces was only kept correct by a comment
        // saying "must match `recompute_rate`" — precisely the comment-enforced
        // invariant this project distrusts, and the drift would be silent: a
        // constructor left at 0 puts the machine back in the stopped-DAC state
        // ledger R-16's livelock needs, and no test of a *programmed* DAC would
        // notice. Raised in review on #205.
        ai.recompute_rate();
        ai
    }

    /// Select the console region (sets the video clock and re-derives the rate).
    /// Wired from the cart header at ROM load; defaults to NTSC.
    pub const fn set_region(&mut self, region: Region) {
        self.video_clock = region.video_clock();
        self.recompute_rate();
    }

    /// Observed underrun count (buffer starvations) — for the harness.
    #[must_use]
    pub const fn underruns(&self) -> u64 {
        self.underruns
    }

    /// The derived output sample rate in Hz (0 until `AI_DACRATE` is set).
    #[must_use]
    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Drain the emitted stereo stream produced since the last drain.
    pub fn drain(&mut self) -> Vec<StereoSample> {
        core::mem::take(&mut self.sink)
    }

    /// Read an AI register (`index` = `(addr >> 2) & 7`).
    ///
    /// Every register except `AI_STATUS` (index 3) is write-only and reads back
    /// a mirror of `AI_LENGTH` (the front transfer's remaining bytes), per the
    /// wiki and ares. `AI_STATUS` reports the FULL/BUSY/ENABLED flags plus the
    /// best-effort `COUNT`/`WC` readback (ledgered — no oracle pins its phase).
    #[must_use]
    pub fn read_reg(&self, index: u32) -> u32 {
        if index == 3 {
            self.status()
        } else {
            // AI_LENGTH mirror: remaining bytes of the active transfer.
            self.dma_len[0] & 0x0003_FFFF
        }
    }

    /// Assemble `AI_STATUS`.
    ///
    /// FULL (bit 31 and bit 0), BUSY (bit 30), and ENABLED (bit 25) are the
    /// flags software polls and are exact. Bits 20 and 24 read as 1 on hardware
    /// (ares). `COUNT` (bits 14..=1) and `WC` (bit 19) are a best-effort model
    /// of the DAC's internal down-counter — striven, not gated (ledgered),
    /// because no public capture pins their exact phase.
    fn status(&self) -> u32 {
        let mut s = 0u32;
        if self.dma_count > 1 {
            s |= 1 << 31; // FULL
            s |= 1 << 0; // FULL (mirror copy)
        }
        if self.dma_count > 0 {
            s |= 1 << 30; // BUSY
        }
        s |= 1 << 24; // always 1 (ares)
        if self.dma_enable {
            s |= 1 << 25; // ENABLED
        }
        s |= 1 << 20; // always 1 (ares)
        // COUNT ticks down at the VI clock, from DACRATE/2 to 0, reloading; WC
        // (LRCK) toggles at DACRATE/2, out of phase by half a period. Both are
        // gated by BITRATE != 0. Derived from `last_tick`; see ledger.
        // Gated on `AI_BITRATE` ALONE: the wiki says the counter "always ticks
        // unless `AI_BITRATE` is 0" and that a `BITRATE` of 0 "stops the clock" —
        // neither is said to depend on `AI_DACRATE`. `COUNT`/`WC` additionally need
        // a DAC rate to have a range at all, which their own guard below supplies.
        if self.bit_rate != 0 {
            // All three readbacks are phases of the same video-clock tick count.
            let vi_ticks = self.last_tick.saturating_mul(u64::from(self.video_clock)) / MASTER_HZ;
            let half = u64::from(self.dac_rate) / 2;
            if half != 0 {
                let phase = vi_ticks % (half * 2);
                let count = if phase < half {
                    half - phase
                } else {
                    half * 2 - phase
                };
                s |= ((count as u32) & 0x3FFF) << 1;
                if phase >= half {
                    s |= 1 << 19; // WC high on the second half-period
                }
            }
            // BC (bit 16) — the BCLK line to the BU9480 DAC. `AI_BITRATE` is
            // "Half of bit clock period" and "the bit clock rate is the Video
            // clock, divided by two, divided by one more than this number"
            // (wiki §AI_BITRATE), so one half-period is `BITRATE + 1` video
            // clocks and the line toggles once per half-period. A `BITRATE` of 0
            // stops the clock, which the enclosing gate excludes — and note that
            // gate is BITRATE-only, so BCLK keeps running with no DAC rate set.
            //
            // The wiki itself hedges this ("believed", "probably") because the
            // CPU "cannot reliably sample it rapidly enough even when BITRATE is
            // set to 15" — so it is un-observable by software in practice and
            // stays ungated (ledger R-16), like `COUNT`/`WC`.
            let half_periods = vi_ticks / (u64::from(self.bit_rate) + 1);
            if half_periods & 1 != 0 {
                s |= 1 << 16; // BC
            }
        }
        s
    }

    /// Write an AI register (`index` = `(addr >> 2) & 7`), returning the MI
    /// interrupt effect for the Bus to apply.
    ///
    /// The **interrupt fires when a transfer starts, not when it ends**: writing
    /// `AI_LENGTH` into an idle queue (`dma_count == 0`) starts that buffer
    /// immediately and raises the interrupt now; a second buffer queued behind a
    /// playing one raises nothing until it is promoted in [`Audio::tick`]. This
    /// is what lets software refill during playback (wiki §DMA).
    pub fn write_reg(&mut self, index: u32, val: u32) -> AiIrq {
        match index {
            0 => {
                // AI_DRAM_ADDR: stage the next free slot's base (24-bit, & ~7).
                if self.dma_count < 2 {
                    self.dma_addr[self.dma_count as usize] = val & 0x00FF_FFF8;
                }
                AiIrq::None
            }
            1 => {
                // AI_LENGTH: stage the next free slot's length (18-bit, & ~7)
                // and enqueue it. Enqueueing into an idle queue starts playback.
                let length = val & 0x0003_FFF8;
                if self.dma_count < 2 {
                    let starting = self.dma_count == 0;
                    self.dma_len[self.dma_count as usize] = length;
                    self.dma_count += 1;
                    if starting {
                        // The buffer starts now; anchor the first sample one
                        // period out from the DAC's current position and fire
                        // the IRQ (it fires on *start*, not drain).
                        self.addr_carry = 0;
                        self.next_sample_tick = self.last_tick.saturating_add(self.period_ticks());
                        return AiIrq::Raise;
                    }
                }
                AiIrq::None
            }
            2 => {
                // AI_CONTROL: DMA enable (bit 0).
                self.dma_enable = val & 1 != 0;
                AiIrq::None
            }
            3 => AiIrq::Lower, // AI_STATUS write acknowledges the interrupt.
            4 => {
                // AI_DACRATE (14-bit): re-derive the sample rate.
                self.dac_rate = (val & 0x3FFF) as u16;
                self.recompute_rate();
                AiIrq::None
            }
            5 => {
                // AI_BITRATE (4-bit).
                self.bit_rate = (val & 0xF) as u8;
                AiIrq::None
            }
            _ => AiIrq::None, // indices 6/7 are unmapped.
        }
    }

    /// Re-derive [`Audio::sample_rate`] from the current video clock and
    /// `AI_DACRATE`.
    const fn recompute_rate(&mut self) {
        // An unprogrammed `AI_DACRATE` falls back to [`Self::DEFAULT_DAC_HZ`]
        // rather than to zero. Zero used to mean "emit nothing", which is wrong
        // in a way that reaches far past audio: with the DAC stopped, `tick()`
        // returns before `emit_sample`, and `emit_sample` is the ONLY place a
        // drained transfer is retired — so `AI_STATUS.FULL` could latch and never
        // clear, and a game polling it for a free DMA slot spun forever. World
        // Driver Championship did exactly that (ledger R-16).
        //
        // What is ESTABLISHED and what is INFERRED, kept apart deliberately —
        // the reset semantics are admitted undocumented below, and an inference
        // dressed as a hardware fact is what this ledger exists to prevent.
        //
        // ESTABLISHED (readable reference, ISC): ares runs its DAC from power-on.
        // `AI::power()` sets `dac.frequency = 44100` and `AI::main()` calls
        // `sample()` unconditionally, so its equivalent retirement block runs
        // before any `AI_DACRATE` write.
        // INFERRED (from that, plus a divider having no "off" encoding): the
        // hardware DAC counter likewise has no stopped state.
        // NOT ESTABLISHED: what `AI_DACRATE` holds at reset. No source this
        // project mirrors says, which is exactly why the rate is labeled a
        // modeling default and not a measured value.
        //
        // The naive alternative — letting `dac_rate == 0` compute
        // `video_clock / 1` ≈ 48 MHz — is what the old zero-gate was avoiding,
        // and it would flood the sink. A default rate avoids both failures.
        //
        // TODO(T-AUDIO-01): separate an explicit `AI_DACRATE = 0` write from the
        // unprogrammed reset state. Needs a `dac_rate_programmed` field, hence a
        // save-state layout bump (ADR 0005) — deferred, see below and ledger R-16.
        //
        // KNOWN SIMPLIFICATION, ledgered (R-16): this conflates "never
        // programmed" with "software explicitly wrote 0". ares keeps them apart —
        // `power()` sets 44100, while its `AI_DACRATE` write honors a literal
        // zero (`dac.frequency = max(1, videoFrequency / (dacRate + 1))`,
        // `ai/io.cpp`) — so full fidelity needs a `dac_rate_programmed` flag to
        // tell the two apart. That adds a field to a serialized struct and so
        // changes the save-state layout (ADR 0005), which is not a change to make
        // in passing. Unobservable in practice: a DACRATE of 0 asks for a ~48 MHz
        // DAC and no title does it. Recorded rather than silently accepted.
        self.sample_rate = if self.video_clock == 0 {
            0
        } else if self.dac_rate == 0 {
            Self::DEFAULT_DAC_HZ
        } else {
            self.video_clock / (self.dac_rate as u32 + 1)
        };
    }

    /// Master ticks between output samples, `MASTER_HZ / sample_rate`.
    ///
    /// Zero only when the video clock is unset, which cannot happen on a
    /// constructed machine. It is **no longer** zero for an unprogrammed
    /// `AI_DACRATE` — that falls back to [`Self::DEFAULT_DAC_HZ`], because a
    /// stopped DAC cannot retire a transfer and latched `AI_STATUS.FULL`
    /// (ledger R-16).
    fn period_ticks(&self) -> u64 {
        if self.sample_rate == 0 {
            0
        } else {
            (MASTER_HZ / u64::from(self.sample_rate)).max(1)
        }
    }

    /// Advance the AI to `now` master ticks, emitting every output sample whose
    /// scheduled tick has arrived.
    ///
    /// Derived timing (ADR 0006): the number of samples emitted is a function of
    /// `now` and the DAC period, never of an independently incremented counter.
    /// Hot path — allocation into the sink is the only cost while playing.
    pub fn tick<B: AudioBus>(&mut self, now: u64, bus: &mut B) {
        self.last_tick = now;
        // A schedule exists and its next sample is still ahead: nothing to do, and
        // in particular no need for `period_ticks`, whose 64-bit divide dominated
        // this function (`docs/audio.md` §Derived timing for the cost and the
        // reason it is an ordering change rather than a cached field).
        //
        // The old code returned from the same states without touching a field, so
        // this only moves the decision earlier.
        if self.next_sample_tick != 0 && now < self.next_sample_tick {
            // The R-16 guard still has to be checkable on the hot path, or it
            // would only ever be evaluated on the ~0.05% of calls that emit.
            // `debug_assert` compiles out of release, so this costs nothing.
            debug_assert!(
                self.sample_rate != 0,
                "a constructed AI must never have a stopped DAC"
            );
            return;
        }
        let period = self.period_ticks();
        if period == 0 {
            // Unreachable on a constructed machine: `video_clock` is always
            // set and an unprogrammed `AI_DACRATE` now yields
            // `DEFAULT_DAC_HZ`, not zero (ledger R-16). Kept as a guard against
            // a divide-by-zero rather than as a modeled DAC state.
            //
            // The assert makes that claim CHECKABLE instead of merely stated: a
            // future change that reintroduces a zero rate is exactly the R-16
            // defect, and a silent `return` is how it hid the first time.
            debug_assert!(period > 0, "a constructed AI must never have a stopped DAC");
            return;
        }
        if self.next_sample_tick == 0 {
            // Anchor the first sample one period out so a large `now` does not
            // dump a backlog of silence at power-on / rate change.
            self.next_sample_tick = now.saturating_add(period);
            return;
        }
        while self.next_sample_tick <= now {
            self.emit_sample(bus);
            self.next_sample_tick = self.next_sample_tick.saturating_add(period);
        }
    }

    /// Emit exactly one output sample: play from RDRAM if a transfer is active,
    /// otherwise decay the held DAC value toward silence.
    fn emit_sample<B: AudioBus>(&mut self, bus: &mut B) {
        let active = self.dma_count > 0 && self.dma_len[0] > 0 && self.dma_enable;
        if active {
            // Apply the deferred carry into the high address bits first — this
            // is the one-cycle-late carry that produces the +0x2000 bug when the
            // previous sample crossed a page on the final word of a transfer.
            let high =
                (self.dma_addr[0] & !(Self::PAGE - 1)).wrapping_add(self.addr_carry * Self::PAGE);
            self.dma_addr[0] = high | (self.dma_addr[0] & (Self::PAGE - 1));

            let word = bus.ai_dma_read_u32(self.dma_addr[0] & 0x00FF_FFFF);
            let sample = StereoSample {
                left: (word >> 16) as i16,
                right: word as i16,
            };
            self.dac_hold = sample;
            self.sink.push(sample);

            // Advance the low 13 bits by one word; the carry out is remembered,
            // not applied, until the next sample.
            let low = (self.dma_addr[0] & (Self::PAGE - 1)).wrapping_add(4);
            self.addr_carry = u32::from(low >= Self::PAGE);
            self.dma_addr[0] = (self.dma_addr[0] & !(Self::PAGE - 1)) | (low & (Self::PAGE - 1));
            self.dma_len[0] -= 4;
        } else {
            // Underrun / idle: hold-and-decay toward zero (deterministic, no
            // float). A DAC that keeps its rate but has nothing to play settles
            // to silence rather than clicking off.
            self.dac_hold.left = (i32::from(self.dac_hold.left) * 63 / 64) as i16;
            self.dac_hold.right = (i32::from(self.dac_hold.right) * 63 / 64) as i16;
            self.sink.push(self.dac_hold);
        }

        // A drained front transfer promotes the queued one (if any) and raises
        // the interrupt as that buffer *starts*.
        if self.dma_count > 0 && self.dma_len[0] == 0 {
            self.dma_count -= 1;
            if self.dma_count > 0 {
                self.dma_addr[0] = self.dma_addr[1];
                self.dma_len[0] = self.dma_len[1];
                bus.raise_ai_interrupt();
            } else if self.dma_enable {
                // Ran dry with nothing queued — an observable starvation.
                self.underruns += 1;
            }
        }
    }
}

/// Returns the crate version string.
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bus backed by a flat RDRAM image, recording interrupt raises.
    struct TestBus {
        ram: Vec<u8>,
        irqs: u32,
    }
    impl TestBus {
        fn new(size: usize) -> Self {
            Self {
                ram: alloc::vec![0u8; size],
                irqs: 0,
            }
        }
        fn write_word(&mut self, addr: u32, val: u32) {
            let a = addr as usize;
            self.ram[a..a + 4].copy_from_slice(&val.to_be_bytes());
        }
    }
    impl AudioBus for TestBus {
        fn ai_dma_read_u32(&self, addr: u32) -> u32 {
            let a = addr as usize;
            u32::from_be_bytes([
                self.ram[a],
                self.ram[a + 1],
                self.ram[a + 2],
                self.ram[a + 3],
            ])
        }
        fn raise_ai_interrupt(&mut self) {
            self.irqs += 1;
        }
    }

    /// Program a standard NTSC ~44 kHz DAC and enable DMA.
    fn programmed() -> Audio {
        let mut ai = Audio::new();
        ai.write_reg(4, 1103); // AI_DACRATE → ~44_136 Hz
        ai.write_reg(2, 1); // AI_CONTROL: DMA enable
        ai
    }

    /// **The FIRST `tick` only anchors the sample clock; it never emits.**
    ///
    /// This test used to be called `idle_tick_emits_nothing_before_dacrate` and
    /// asserted "no rate programmed → no samples". It was **vacuous, and vacuous
    /// before the DAC default landed**: a single `tick` returns at the
    /// `next_sample_tick == 0` anchor branch regardless of the rate, so it passed
    /// identically before and after a change to the very behavior it claimed to
    /// pin — the "success and failure paths converge" trap. Renamed and re-aimed
    /// at the rule it actually exercises, which is worth pinning on its own: the
    /// anchor exists so a large first `now` cannot dump a backlog of silence.
    #[test]
    fn the_first_tick_only_anchors_the_sample_clock() {
        let mut ai = Audio::new();
        let mut bus = TestBus::new(0x1000);
        ai.tick(1_000_000, &mut bus);
        assert!(
            ai.drain().is_empty(),
            "the anchoring tick must not emit a backlog"
        );
        // And the second tick DOES emit — without this half the test would pass
        // just as well against a DAC that never emits anything at all.
        ai.tick(1_000_000 + MASTER_HZ / 60, &mut bus);
        assert!(
            !ai.drain().is_empty(),
            "the tick after the anchor must emit at the default rate"
        );
    }

    #[test]
    fn dacrate_derives_rate_per_region() {
        let mut ai = Audio::new();
        ai.write_reg(4, 1103);
        assert_eq!(ai.sample_rate(), VIDEO_CLOCK_NTSC / 1104);
        ai.set_region(Region::Pal);
        assert_eq!(ai.sample_rate(), VIDEO_CLOCK_PAL / 1104);
    }

    /// **The destination code selects the region (T-71-005).** The characters are
    /// the N64brew ROM-header table; the three judgment calls are pinned here so
    /// a later reader sees them as decisions rather than accidents.
    #[test]
    fn destination_code_selects_the_region() {
        use Region::{Ntsc, Pal};
        // The 50 Hz territories.
        for c in [
            b'D', b'F', b'H', b'I', b'L', b'P', b'S', b'U', b'W', b'X', b'Y', b'Z',
        ] {
            assert_eq!(
                Region::from_destination_code(c),
                Pal,
                "{} should be PAL",
                c as char
            );
        }
        // The 60 Hz territories.
        for c in [b'E', b'J', b'K', b'N', b'G'] {
            assert_eq!(
                Region::from_destination_code(c),
                Ntsc,
                "{} should be NTSC",
                c as char
            );
        }
        // The three explicit decisions (see `from_destination_code`): Brazil is
        // PAL-M, a 60 Hz standard; China has no known retail N64; "All" names no
        // single region.
        assert_eq!(
            Region::from_destination_code(b'B'),
            Ntsc,
            "Brazil is PAL-M/60Hz"
        );
        assert_eq!(
            Region::from_destination_code(b'C'),
            Ntsc,
            "China: no retail N64"
        );
        assert_eq!(
            Region::from_destination_code(b'A'),
            Ntsc,
            "\"All\" takes the default"
        );
        // An unknown/homebrew byte must be behavior-preserving, not retune audio.
        assert_eq!(
            Region::from_destination_code(0),
            Ntsc,
            "unknown code keeps NTSC"
        );
        assert_eq!(
            Region::from_destination_code(b'?'),
            Ntsc,
            "unknown code keeps NTSC"
        );
    }

    /// **The selected region actually changes the sample rate.** Guards against a
    /// mapping that is correct but never reaches the DAC divisor.
    #[test]
    fn a_pal_destination_code_retunes_the_dac() {
        let mut ai = Audio::new();
        ai.write_reg(4, 1103); // AI_DACRATE: rate = clock / (dacrate + 1)
        let ntsc = ai.sample_rate();
        ai.set_region(Region::from_destination_code(b'P')); // Europe
        assert_ne!(
            ai.sample_rate(),
            ntsc,
            "a PAL cartridge must retune the DAC"
        );
        assert_eq!(ai.sample_rate(), VIDEO_CLOCK_PAL / 1104);
    }

    #[test]
    fn set_region_before_dacrate_does_not_fabricate_the_video_clock_rate() {
        // What this has always been protecting: selecting a region before
        // AI_DACRATE is programmed must NOT compute `video_clock / 1` (~48 MHz)
        // and flood the sink.
        //
        // It previously asserted the rate was exactly **0** and that the DAC
        // "emits nothing". That over-specified the protection into a bug: a
        // zero rate makes `tick` return before `emit_sample`, and `emit_sample`
        // is the only place a drained transfer is retired — so `AI_STATUS.FULL`
        // could latch forever (see
        // `full_clears_even_when_dacrate_was_never_programmed`, and ledger R-16).
        // The unprogrammed DAC now runs at `DEFAULT_DAC_HZ`. The assertion is
        // therefore an ORDER-OF-MAGNITUDE bound, which is what the comment
        // always described, rather than an exact value the hardware does not
        // document.
        let mut ai = Audio::new();
        ai.set_region(Region::Pal);
        // Two assertions, each catching something the other cannot. The equality
        // pins the WIRING — that `recompute_rate` reads the named constant rather
        // than an inlined literal that could drift from it. The bound pins the
        // PROPERTY, and it is the one that catches the failure class this test was
        // written for: any future rate derived from the video clock rather than an
        // audio rate.
        assert_eq!(
            ai.sample_rate(),
            Audio::DEFAULT_DAC_HZ,
            "an unprogrammed DAC must run at the documented default"
        );
        assert!(
            ai.sample_rate() > 0 && ai.sample_rate() < 100_000,
            "an unprogrammed DAC runs at an audio rate, not the video clock: {}",
            ai.sample_rate()
        );
        let mut bus = TestBus::new(0x1000);
        // Prime the sample clock first. Without this the single `tick` below
        // returns at the `next_sample_tick == 0` anchor branch, `emitted` is
        // always 0, and the upper bound passes even with the emission path
        // completely broken — the vacuity `the_first_tick_only_anchors_the_sample_clock`
        // documents. Raised in review on #205.
        ai.tick(0, &mut bus);
        ai.tick(MASTER_HZ / 60, &mut bus); // one frame
        let emitted = ai.drain().len();
        // A TWO-SIDED bound, because only the pair is evidence: the upper bound
        // rejects the ~800k samples/frame a video-clock rate would produce, and
        // the lower bound rejects a DAC that emits nothing — which is the state
        // that caused the R-16 livelock and which an upper bound alone accepts.
        // 44100/60 = 735.
        assert!(
            (500..5_000).contains(&emitted),
            "one frame of an unprogrammed DAC must emit ~735 samples, not a flood \
             and not silence: {emitted}"
        );
    }

    /// **`AI_STATUS.FULL` must be able to clear even if `AI_DACRATE` was never
    /// programmed** — the World Driver Championship livelock (ledger R-16).
    ///
    /// A game may queue two buffers and poll `FULL` for a free slot before it
    /// programs the DAC. Retirement lives in `emit_sample`, which only runs when
    /// the DAC has a period, so a stopped DAC latched `FULL` permanently and the
    /// poll never exited. Mutation guard: restore the `dac_rate == 0 → 0` rate
    /// and this test hangs on `FULL` forever (it fails on the assertion below).
    #[test]
    fn full_clears_even_when_dacrate_was_never_programmed() {
        let mut ai = Audio::new();
        let mut bus = TestBus::new(0x4000);
        // Two queued transfers, no AI_DACRATE write, DMA enabled.
        ai.write_reg(2, 1); // AI_CONTROL: DMA enable
        ai.write_reg(0, 0x0000_1000);
        ai.write_reg(1, 0x40);
        ai.write_reg(0, 0x0000_2000);
        ai.write_reg(1, 0x40);
        assert_ne!(ai.status() & (1 << 31), 0, "two queued → FULL");

        // Advance a second of emulated time. With a running DAC the two 64-byte
        // transfers drain almost immediately; with a stopped one, never.
        ai.tick(MASTER_HZ, &mut bus);
        assert_eq!(
            ai.status() & (1 << 31),
            0,
            "FULL must clear once a transfer retires, even with AI_DACRATE unset"
        );
    }

    #[test]
    fn write_only_registers_mirror_ai_length() {
        let mut ai = programmed();
        ai.write_reg(0, 0x0000_1000); // AI_DRAM_ADDR
        ai.write_reg(1, 0x40); // AI_LENGTH = 64 bytes
        // Indices 0,1,2,4,5 all read back the AI_LENGTH mirror (remaining bytes).
        assert_eq!(ai.read_reg(0), 0x40);
        assert_eq!(ai.read_reg(1), 0x40);
        assert_eq!(ai.read_reg(2), 0x40);
        assert_eq!(ai.read_reg(4), 0x40);
        assert_eq!(ai.read_reg(5), 0x40);
    }

    #[test]
    fn ai_length_masks_to_eight_byte_granularity() {
        let mut ai = programmed();
        ai.write_reg(1, 0x47); // low 3 bits dropped → 0x40
        assert_eq!(ai.read_reg(1), 0x40);
    }

    #[test]
    fn first_buffer_raises_irq_on_enqueue() {
        let mut ai = programmed();
        assert_eq!(ai.write_reg(0, 0x100), AiIrq::None);
        assert_eq!(
            ai.write_reg(1, 0x40),
            AiIrq::Raise,
            "the first buffer starts immediately and raises the AI interrupt"
        );
    }

    #[test]
    fn ai_status_write_acknowledges_the_interrupt() {
        let mut ai = programmed();
        assert_eq!(ai.write_reg(3, 0), AiIrq::Lower);
    }

    /// **`BC` toggles at the documented bit-clock half-period (R-16).**
    ///
    /// The wiki gives `AI_BITRATE` as "half of bit clock period" and the bit clock
    /// as "the Video clock, divided by two, divided by one more than this number",
    /// so one half-period is `BITRATE + 1` video clocks and BCLK toggles once per
    /// half-period. This counts the transitions over a fixed span and compares
    /// against that relation derived independently from the two documented
    /// quantities — so a wrong divisor (`BITRATE` instead of `BITRATE + 1`, or the
    /// DAC rate) changes the count and fails.
    #[test]
    fn bc_toggles_at_the_documented_bit_clock_half_period() {
        const BITRATE: u32 = 15;
        let span_master_ticks = 20_000u64;

        let mut ai = programmed();
        ai.write_reg(5, BITRATE); // AI_BITRATE
        let mut bus = TestBus::new(0x1000);

        let mut transitions = 0usize;
        let mut prev = ai.status() & (1 << 16) != 0;
        for now in 1..=span_master_ticks {
            ai.tick(now, &mut bus);
            let bc = ai.status() & (1 << 16) != 0;
            if bc != prev {
                transitions += 1;
                prev = bc;
            }
        }

        // Expected from the documented relation alone: video clocks elapsed over
        // the span, divided by the half-period of `BITRATE + 1` video clocks.
        let vi_ticks = span_master_ticks * u64::from(VIDEO_CLOCK_NTSC) / MASTER_HZ;
        let expected = vi_ticks / u64::from(BITRATE + 1);
        assert_eq!(
            transitions as u64,
            expected,
            "BC must toggle once per {} video clocks",
            BITRATE + 1
        );
        assert!(
            transitions > 100,
            "the span must actually exercise the clock"
        );
    }

    /// **The bit clock does not depend on the DAC rate.** The wiki gates the
    /// counter on `AI_BITRATE` alone ("It always ticks unless `AI_BITRATE` is 0")
    /// and never on `AI_DACRATE`, so BCLK must keep toggling with no DAC rate
    /// programmed — even though `COUNT`/`WC`, which need a range, report nothing.
    #[test]
    fn the_bit_clock_runs_without_a_dac_rate() {
        let mut ai = Audio::new();
        ai.write_reg(5, 15); // AI_BITRATE only — no AI_DACRATE
        let mut bus = TestBus::new(0x1000);
        let mut seen_high = false;
        let mut seen_low = false;
        for now in 1..=5_000u64 {
            ai.tick(now, &mut bus);
            let st = ai.status();
            if st & (1 << 16) != 0 {
                seen_high = true;
            } else {
                seen_low = true;
            }
            // COUNT has no range without a DAC rate, so it must stay clear.
            assert_eq!(st & (0x3FFF << 1), 0, "COUNT needs a DAC rate");
        }
        assert!(
            seen_high && seen_low,
            "BCLK must toggle with no DAC rate programmed"
        );
    }

    /// **A `BITRATE` of 0 stops the bit clock.** The wiki: "A written value of 0
    /// instead stops the clock", so `BC` must not toggle.
    #[test]
    fn a_zero_bitrate_stops_the_bit_clock() {
        let mut ai = programmed();
        ai.write_reg(5, 0); // AI_BITRATE = 0 → clock stopped
        let mut bus = TestBus::new(0x1000);
        for now in 1..=5_000u64 {
            ai.tick(now, &mut bus);
            assert_eq!(ai.status() & (1 << 16), 0, "BC must stay low at BITRATE 0");
        }
    }

    #[test]
    fn status_reports_busy_and_full() {
        let mut ai = programmed();
        ai.write_reg(0, 0x100);
        ai.write_reg(1, 0x40); // one queued → BUSY
        assert_ne!(ai.status() & (1 << 30), 0, "BUSY");
        assert_eq!(ai.status() & (1 << 31), 0, "not FULL with one buffer");
        ai.write_reg(0, 0x200);
        ai.write_reg(1, 0x40); // two queued → FULL
        assert_ne!(ai.status() & (1 << 31), 0, "FULL");
        assert_ne!(ai.status() & 1, 0, "FULL mirror bit 0");
        assert_ne!(ai.status() & (1 << 25), 0, "ENABLED");
    }

    /// **A sample due exactly at `now` is emitted on that call, not the next one.**
    ///
    /// [`Audio::tick`] returns early — before computing the DAC period — when no
    /// sample is due, which is what keeps a 64-bit divide off the hot path. The
    /// boundary of that early-out is `now < next_sample_tick`, and an off-by-one
    /// there defers every sample by one RCP step: audio that is *correct but late*,
    /// with no wrong sample anywhere.
    ///
    /// Nothing else in this suite can see that. Mutating the test to `<=` leaves the
    /// whole workspace green, because the other AI tests advance `now` in strides of
    /// a full period or longer and so never land on the boundary. The gross failure
    /// (never emitting at all) is caught seven times over; this one-tick case was
    /// caught zero times, which is why it is pinned here rather than assumed.
    ///
    /// `next_sample_tick` is read directly instead of recomputing the anchor from
    /// `period_ticks`, so the test asserts against where the DAC actually is rather
    /// than against a second copy of the formula it is meant to check. The schedule
    /// comes from `write_reg` — enqueuing a transfer sets `next_sample_tick` — so no
    /// priming tick is needed and none is issued; an earlier version called `tick`
    /// first and claimed it anchored the schedule, which it did not.
    #[test]
    fn a_sample_due_exactly_now_is_emitted_on_this_call() {
        let mut ai = programmed();
        let mut bus = TestBus::new(0x1_0000);
        bus.write_word(0x100, 0x0001_0002);
        bus.write_word(0x104, 0x0003_0004);
        ai.write_reg(0, 0x100);
        ai.write_reg(1, 8); // 8 bytes = 2 sample-pairs

        let due = ai.next_sample_tick;
        assert!(
            due > 1,
            "enqueuing a transfer must schedule the next sample"
        );

        ai.tick(due - 1, &mut bus);
        assert!(
            ai.drain().is_empty(),
            "no sample is due one tick before the boundary"
        );

        ai.tick(due, &mut bus);
        assert_eq!(
            ai.drain(),
            [StereoSample { left: 1, right: 2 }],
            "the sample due exactly at `now` must be emitted on this call"
        );
    }

    #[test]
    fn plays_a_buffer_and_drains_it() {
        let mut ai = programmed();
        let mut bus = TestBus::new(0x1_0000);
        // 4 stereo words at 0x100.
        for i in 0..4u32 {
            bus.write_word(0x100 + i * 4, 0x0001_0002u32.wrapping_add(i));
        }
        ai.write_reg(0, 0x100);
        ai.write_reg(1, 16); // 16 bytes = 4 sample-pairs
        // Advance exactly 4 sample periods (the anchor puts sample 1 at +period,
        // so ticks period..=4*period emit 4 samples before the DAC would decay).
        let period = ai.period_ticks();
        ai.tick(period * 4, &mut bus);
        let out = ai.drain();
        assert_eq!(out.len(), 4, "exactly the 4 buffered samples played");
        assert_eq!(out[0], StereoSample { left: 1, right: 2 });
        assert_eq!(out[3], StereoSample { left: 1, right: 5 });
    }

    #[test]
    fn second_buffer_promotes_and_raises_irq_on_start() {
        let mut ai = programmed();
        let mut bus = TestBus::new(0x1_0000);
        // 8-byte (two-pair) buffers — AI_LENGTH granularity is 8 bytes (& ~7).
        bus.write_word(0x100, 0x1111_2222);
        bus.write_word(0x104, 0x1111_3333);
        bus.write_word(0x200, 0x3333_4444);
        bus.write_word(0x204, 0x3333_5555);
        ai.write_reg(0, 0x100);
        ai.write_reg(1, 8); // buffer 1
        ai.write_reg(0, 0x200);
        ai.write_reg(1, 8); // buffer 2 (queued, no IRQ yet)
        let period = ai.period_ticks();
        ai.tick(period * 6, &mut bus);
        let out = ai.drain();
        assert_eq!(
            out[0],
            StereoSample {
                left: 0x1111,
                right: 0x2222
            }
        );
        assert_eq!(
            out[2],
            StereoSample {
                left: 0x3333,
                right: 0x4444
            }
        );
        assert_eq!(bus.irqs, 1, "promotion of buffer 2 raised exactly one IRQ");
    }

    /// **The delayed-carry hardware bug.** A transfer whose final sample ends
    /// exactly on an `0x2000` page boundary makes the AI add `0x2000` to the
    /// *next* buffer's address. This test fails if the bug is "corrected".
    #[test]
    fn delayed_carry_bug_bumps_the_next_buffer() {
        let mut ai = programmed();
        let mut bus = TestBus::new(0x1_0000);
        // Buffer 1: two pairs (8 bytes) ending exactly on the 0x2000 boundary —
        // the final read at 0x1FFC advances the low 13 bits 0x1FFC → 0x2000,
        // wrapping to 0 and arming the deferred carry.
        bus.write_word(0x1FF8, 0xAAAA_BBBB);
        bus.write_word(0x1FFC, 0xAAAA_CCCC);
        ai.write_reg(0, 0x1FF8);
        ai.write_reg(1, 8);
        // Buffer 2 is programmed at 0x0100, but the delayed carry adds 0x2000,
        // so playback reads from 0x2100 instead.
        bus.write_word(0x0100, 0x0000_0000); // what a correct AI would read
        bus.write_word(0x2100, 0xCCCC_DDDD); // what the buggy AI actually reads
        ai.write_reg(0, 0x0100);
        ai.write_reg(1, 8);
        let period = ai.period_ticks();
        ai.tick(period * 8, &mut bus);
        let out = ai.drain();
        assert_eq!(
            out[1],
            StereoSample {
                left: 0xAAAAu16 as i16,
                right: 0xCCCCu16 as i16
            }
        );
        assert_eq!(
            out[2],
            StereoSample {
                left: 0xCCCCu16 as i16,
                right: 0xDDDDu16 as i16
            },
            "the delayed carry bumped buffer 2's address by 0x2000"
        );
    }

    #[test]
    fn underrun_is_observable_and_decays() {
        let mut ai = programmed();
        let mut bus = TestBus::new(0x1_0000);
        bus.write_word(0x100, 0x4000_4000);
        bus.write_word(0x104, 0x4000_4000);
        ai.write_reg(0, 0x100);
        ai.write_reg(1, 8); // two pairs, then starvation
        let period = ai.period_ticks();
        ai.tick(period * 6, &mut bus);
        assert_eq!(ai.underruns(), 1, "the starvation is counted");
        let out = ai.drain();
        assert_eq!(
            out[0],
            StereoSample {
                left: 0x4000,
                right: 0x4000
            }
        );
        // Once starved, samples decay toward zero from the last held value.
        assert!(out.len() > 2 && out[2].left < 0x4000 && out[2].left > 0);
    }

    #[test]
    fn emission_is_deterministic() {
        let run = || {
            let mut ai = programmed();
            let mut bus = TestBus::new(0x1_0000);
            for i in 0..8u32 {
                bus.write_word(0x100 + i * 4, 0xDEAD_0000u32.wrapping_add(i));
            }
            ai.write_reg(0, 0x100);
            ai.write_reg(1, 32);
            ai.tick(ai.period_ticks() * 16, &mut bus);
            ai.drain()
        };
        assert_eq!(run(), run(), "same input → byte-identical sample stream");
    }

    #[test]
    fn version_is_non_empty() {
        assert!(!version().is_empty());
    }
}
