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
//! `n64brew_wiki/markdown/Audio Interface.md` and the reference behaviour in
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StereoSample {
    /// Left channel.
    pub left: i16,
    /// Right channel.
    pub right: i16,
}

/// The result an AI register write hands back to the Bus, which owns the MI
/// interrupt lines the AI itself cannot name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
#[derive(Debug, Clone)]
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
    /// Derived output sample rate in Hz, `video_clock / (dac_rate + 1)`, or 0
    /// before `AI_DACRATE` is programmed.
    sample_rate: u32,

    // --- Derived-timing emission (ADR 0006: everything off `master_ticks`). ---
    /// Master tick at which the next output sample is due; 0 = unanchored.
    next_sample_tick: u64,
    /// The most recent `master_ticks` the DAC was advanced to (drives the
    /// `AI_STATUS` `COUNT`/`WC` readback without threading the clock into reads).
    last_tick: u64,
    /// Last emitted sample, held and decayed on underrun so the DAC does not
    /// hard-stop (matches ares' decay-to-silence behaviour).
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

    /// Construct at power-on (NTSC, idle).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            dma_addr: [0; 2],
            dma_len: [0; 2],
            dma_count: 0,
            addr_carry: 0,
            dma_enable: false,
            dac_rate: 0,
            bit_rate: 0,
            video_clock: VIDEO_CLOCK_NTSC,
            sample_rate: 0,
            next_sample_tick: 0,
            last_tick: 0,
            dac_hold: StereoSample { left: 0, right: 0 },
            underruns: 0,
            sink: Vec::new(),
        }
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
        if self.bit_rate != 0 && self.dac_rate != 0 {
            let half = u64::from(self.dac_rate) / 2;
            if half != 0 {
                let vi_ticks =
                    self.last_tick.saturating_mul(u64::from(self.video_clock)) / MASTER_HZ;
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
        self.sample_rate = if self.dac_rate == 0 && self.video_clock == 0 {
            0
        } else {
            self.video_clock / (self.dac_rate as u32 + 1)
        };
    }

    /// Master ticks between output samples, `MASTER_HZ / sample_rate`, or 0 when
    /// the rate is unprogrammed.
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
        let period = self.period_ticks();
        if period == 0 {
            return; // DAC not yet programmed — emit nothing.
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

    #[test]
    fn idle_tick_emits_nothing_before_dacrate() {
        let mut ai = Audio::new();
        let mut bus = TestBus::new(0x1000);
        ai.tick(1_000_000, &mut bus);
        assert!(ai.drain().is_empty(), "no rate programmed → no samples");
    }

    #[test]
    fn dacrate_derives_rate_per_region() {
        let mut ai = Audio::new();
        ai.write_reg(4, 1103);
        assert_eq!(ai.sample_rate(), VIDEO_CLOCK_NTSC / 1104);
        ai.set_region(Region::Pal);
        assert_eq!(ai.sample_rate(), VIDEO_CLOCK_PAL / 1104);
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
