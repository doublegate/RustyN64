# AI audio + the RSP audio microcode path — RustyN64

**References:** `ref-docs/research-report.md` §5 (AI), §3 (RSP); ADR 0004;
`n64brew_wiki/markdown/Audio Interface.md`; ares (ISC) `ref-proj/ares/ares/n64/ai/`;
`crates/rustyn64-audio/src/lib.rs`; `docs/rsp.md`; `docs/frontend.md`.

This doc is the SPEC, not history — update it in the same PR as the code.

## Purpose

The N64 audio path splits cleanly: the **RSP audio microcode** does all the work
(decode ADPCM sample banks, apply envelopes/effects, mix, resample → 16-bit
signed stereo PCM into an RDRAM buffer), and the **Audio Interface (AI)** is a
dumb DAC fed by DMA — it "does absolutely no conversion on the samples"
(`ref-docs/research-report.md` §5; N64brew *Audio Interface*). So under LLE, audio
is **emulated for free** by emulating the RSP (`docs/rsp.md`); this crate models
only the AI side, and it is **implemented** as of Phase 4 (Sprint 1).

## Interfaces

```rust
pub trait AudioBus {
    fn ai_dma_read_u32(&self, addr: u32) -> u32; // a 32-bit word = two i16 L/R
    fn raise_ai_interrupt(&mut self);            // a queued buffer became active
}

pub struct StereoSample { pub left: i16, pub right: i16 }

pub enum Region { Ntsc, Pal } // selects the AI video clock

pub struct Audio { /* two-deep DMA FIFO + DAC divider + derived-timing emitter */ }
impl Audio {
    pub fn read_reg(&self, index: u32) -> u32;        // AI_LENGTH mirror / AI_STATUS
    pub fn write_reg(&mut self, index: u32, val: u32) -> AiIrq; // Raise/Lower/None
    pub fn tick<B: AudioBus>(&mut self, now_master_ticks: u64, bus: &mut B);
    pub fn drain(&mut self) -> Vec<StereoSample>;     // per-frame, frontend resamples
}
```

The Bus decodes the register block at `0x0450_0000` (`is_ai_register`), applies
the `AiIrq` effect to `MI_INTR.ai`, and steps the DAC each RCP edge via
`Bus::audio_tick(master_ticks)`. The frontend drains `Bus::drain_audio_samples()`.

## Registers (`0x0450_0000`)

| Offset | Name | Access | Notes |
| --- | --- | --- | --- |
| +0x00 | `AI_DRAM_ADDR` | W (reads mirror `AI_LENGTH`) | 24-bit, `& ~7`; stages the next FIFO slot |
| +0x04 | `AI_LENGTH` | RW | 18-bit, `& ~7`; reads = **bytes remaining**; a write enqueues a buffer |
| +0x08 | `AI_CONTROL` | W (reads mirror `AI_LENGTH`) | bit 0 = `DMA_ENABLE` |
| +0x0C | `AI_STATUS` | R (write **acks** the AI interrupt) | `FULL`(31,0) `BUSY`(30) `ENABLED`(25); `COUNT`/`WC` best-effort |
| +0x10 | `AI_DACRATE` | W (reads mirror `AI_LENGTH`) | 14-bit sample-period divider |
| +0x14 | `AI_BITRATE` | W (reads mirror `AI_LENGTH`) | 4-bit half-bit-clock divider |

**Every write-only register reads back a mirror of `AI_LENGTH`** (the front
transfer's remaining bytes) — a documented hardware quirk (wiki §Registers; ares
`io.cpp`), reproduced.

## Behavior

### The double-buffered DMA loop

The AI holds a **two-deep FIFO** of `(address, length)` transfers. Software points
the AI at a finished PCM buffer (`AI_DRAM_ADDR` + `AI_LENGTH`) and enables DMA; the
AI streams the buffer to the DAC at the programmed sample rate. With both FIFO
slots filled, playback is gap-free (`ref-docs/research-report.md` §5).

**The AI interrupt fires when a transfer *starts*, not when it drains** (wiki
§DMA): enqueuing the first buffer into an idle queue starts it immediately and
raises `MI_INTR.ai` now; a second buffer queued behind a playing one raises
nothing until it is promoted to the front. This is what lets software refill
*during* playback — an end-of-buffer interrupt would be too late to avoid a gap.
A write to `AI_STATUS` acknowledges (lowers) the interrupt.

### Derived timing

The DAC period (master ticks per output sample) is `MASTER_HZ / sample_rate`, and
sample emission is **derived from `master_ticks`** (ADR 0006) — the number of
samples emitted between two ticks is a function of the clock, never an
independently incremented counter. Each output sample reads one 32-bit word
(two i16 L/R) from RDRAM, pushes it to the per-frame sink, and advances the
address; the frontend drains the sink and resamples to the host device rate.

**The period is not computed on the calls that have nothing to do.** `Audio::tick`
runs on every RCP step — roughly 1.04 M times a frame — while at a typical ~32 kHz
the period is ~5,859 master ticks, so about one call in 1,950 emits anything. The
division is therefore performed *after* the "is a sample due" test rather than
before it. Precisely: it is skipped when a schedule already exists and its next
sample is still ahead. It is still computed on the two paths that need it — when a
sample is due, and when `next_sample_tick` is zero and the first one has to be
anchored — so "only when due" would overstate it. That ordering is worth stating because the obvious version is the wrong
one: computing the period first and then discovering the sample is not due cost
**3.67% of a rendering frame**, the largest single source line outside the CPU
pipeline, for a quotient that was thrown away (`docs/performance.md`).

**Stepping the AI does not move it.** `Bus::audio_tick` used to `core::mem::take` the
whole `Audio` — 88 bytes out, a `Default` written into the vacated slot, and 88 back —
on **every RCP step**, purely so `tick` could borrow its owner. Since the DAC is idle on
~1,949 of every 1,950 of those steps, the AI is now asked first
(`Audio::tick_without_bus`) and only a `NeedsBus` token buys the move. The bus-free half
still runs every step and still mutates — it stamps `last_tick` and anchors the first
sample — so this skips the *move*, never the step. Measured **105.64 → 97.58 ms,
1.083x** (`docs/performance.md`).

The token carries the DAC period as well as the proof, so the divide above is not
repeated in the second half; its fields are private, and it derives neither `Copy` nor
`Clone`, so it cannot be forged or replayed against a schedule that has moved on.

It is an ordering change, not a cache: the quotient stays derived from
`sample_rate` on demand. Memoizing it in a field would add serialized state and so
change the save-state layout (ADR 0005) to buy what the reordering buys for
nothing. The boundary condition — a sample due at exactly `now` must be emitted on
that call, not the next — is pinned by
`a_sample_due_exactly_now_is_emitted_on_this_call`, because an off-by-one there is
audio that is correct but one RCP step late, which no other test in the suite could
see.

### Sample rate

Derived, not fixed (`ref-docs/research-report.md` §5; wiki §AI_DACRATE):

```text
sample_rate = video_clock / (AI_DACRATE + 1)
```

`AI_DACRATE = 1103` → ~44.1 kHz on NTSC (near CD quality). Games commonly run
22–32 kHz to save RSP time. The **video clock differs by region** —
`VIDEO_CLOCK_NTSC = 48_681_812 Hz`, `VIDEO_CLOCK_PAL = 49_656_530 Hz` (provenance:
project64/N64-Tests `DoubleShot`, which computes `(VI_NTSC_CLOCK / FREQ) − 1`, and
the N64brew wiki) — so the same `DACRATE` yields a different rate per region.
`Region` selects the clock (default NTSC; wired from the cart header at ROM load).

#### Before `AI_DACRATE` is written: the DAC still runs

An unprogrammed `AI_DACRATE` falls back to **`DEFAULT_DAC_HZ = 44_100`**, not to a
stopped DAC. **This is not cosmetic — it is a correctness requirement**, and the
reason is structural rather than about audio:

- a transfer is retired (`dma_count -= 1`) only inside the DAC's own sample step;
- so a stopped DAC cannot advance the two-deep FIFO at all;
- so `AI_STATUS.FULL` (`dma_count > 1`) latches permanently, and a title polling
  it for a free DMA slot spins forever.

World Driver Championship does exactly that — it queues two buffers and polls
`FULL` *before* programming the DAC. Under the old zero-rate model it issued zero
RDP commands under `real_pif_boot` where it issued 176,085 under `hle_boot`; the
flag transitioned 48 times under HLE against **exactly once** under real-PIF.

**Provenance, with the inference marked as one.** *Established:* ares (ISC,
`ref-proj/ares/ares/n64/ai/`) runs its DAC from power-on — `AI::power()` sets
`dac.frequency = 44100` and `AI::main()` calls `sample()` unconditionally.
*Inferred* from that plus a divider having no "off" encoding: the hardware counter
likewise has no stopped state. **Not established:** `AI_DACRATE`'s reset value,
which no source this project mirrors gives — hence a *modeling default*, not a
measured constant. Nothing observable should depend on the exact number, since
every title programs the register before it plays anything.

**Known simplification (ledger R-16):** the fallback branches on `dac_rate == 0`,
so it cannot tell "never programmed" from "software wrote `0`". ares keeps those
apart, honoring a literal zero as `max(1, video_clock / 1)` ≈ 48 MHz
(`ai/io.cpp`). Separating them needs a `dac_rate_programmed` flag, which changes
the save-state layout (ADR 0005) and so is not done in passing; no title requests
a ~48 MHz DAC, so it is unobservable in practice.

The rate must also **not** be derived as `video_clock / 1` for the unprogrammed
case: that is ~48 MHz and floods the sink. `DEFAULT_DAC_HZ` avoids both failures,
and `Audio::new()` initializes `sample_rate` to it directly — leaving it 0 there
would reintroduce the stopped DAC on any machine that never triggers a rate
recompute.

### The delayed-carry hardware bug

When the **last** sample of a transfer ends exactly on an `0x2000` (8 KiB) page
boundary, the AI adds `0x2000` to the **next** buffer's address (wiki §Delayed-carry
hardware bug; libdragon has a workaround). It is modeled — not corrected — as a
13-bit/11-bit address split with a one-sample-deferred carry (ares `ai.cpp`): the
low 13 bits advance per word, and the carry out is applied at the *top of the next
sample*, which lands on the next transfer when it wraps on the final word. A named
test (`delayed_carry_bug_bumps_the_next_buffer`) fails if the bug is "fixed".

### Underrun

If software does not queue the next buffer before drain, the DAC starves. Modeled
as **hold-and-decay toward silence** (deterministic integer decay of the last
sample; ares uses an exponential decay), with an observable `underruns()` counter
so a resampler cannot silently paper over a genuine AI-rate error.

### Where mixing lives

ADPCM decode, envelopes, mixing, and resampling are **RSP microcode**, not this
crate (`docs/rsp.md`, ADR 0002). The *host*-rate resample (N64 output rate → the
`cpal` device rate) is a **frontend** stage, kept out of the deterministic core
(`docs/frontend.md`, ADR 0004) — implemented as `EmuCore::produce_audio` +
`resample_stereo` (a carried-phase linear resampler), fed to the `cpal` ring.

## Edge cases and gotchas

- **The AI does no conversion.** Resist per-game audio HLE; correctness comes from
  the RSP (`ref-docs/research-report.md` §5).
- **The IRQ is on start, not end.** See above — this is the single most
  counter-intuitive AI fact.
- **`AI_LENGTH` granularity is 8 bytes** (`& ~7`), so the minimum transfer is two
  stereo sample-pairs.
- **Region changes the rate at the same DACRATE.** Getting the video clock wrong
  detunes every game's audio.
- **Samples are big-endian i16 pairs.** `ai_dma_read_u32` returns a word holding
  L then R.
- **Host resample is non-deterministic by nature** — it lives in the frontend,
  never in the core (ADR 0004).

## Ledgered residuals

- **R-16** — `AI_STATUS` `COUNT`/`WC`/`BC` readback and the bit-clock (`AI_BITRATE`)
  timing are a **best-effort** model with no public capture to pin their phase;
  striven for but ungated. `FULL`/`BUSY`/`ENABLED` (what software polls) are exact.
- **R-17** — the AI DMA has **no setup/arbitration latency or RDRAM bank-state
  cost**: the sample *rate* is exact from `DACRATE`, but the transfer begins and the
  start-interrupt fires at the derived sample boundary, not with the real DMA
  latency; the underrun decay is defined but unpinned.

## Test plan

- **AI unit tests** (`rustyn64-audio`) — DACRATE→rate per region; the FIFO
  double-buffer swap; IRQ-on-start; the `AI_LENGTH` mirror; the delayed-carry bug;
  underrun; determinism. **Done.**
- **Bus integration** (`rustyn64-core`) — the register block driven through the CPU
  memory-mapped path, end to end. **Done.**
- **Real bare-metal PCM ROM** — our own `audio_play.z64` (CPU-fed PCM, programs the
  AI directly, no RSP microcode); the emitted stream matches the buffer byte-for-byte
  (`audio_play_rom.rs`). **Done (Sprint 2).**
- **RSP audio-microcode integration** — the real libdragon **mixer** microcode
  (`rsp_mixer.S`) runs on our LLE RSP, driven by a hand-built channel table + sample
  bank through the rspq overlay path, and produces a mixed 16-bit stereo PCM buffer
  pinned as a golden (`mixer_microcode.rs`). **Done (Sprint 2).**
- **Determinism** — same seed + ROM + input ⇒ bit-identical stream: for the AI
  (`audio_play_rom.rs`) and for the mixer's PCM output (`mixer_microcode.rs`).
  **Done.**

## The chopping, measured at both boundaries

**User report:** audio plays ~1 s, then ~1 s of silence, repeating.
**Status:** root cause identified and measured; the *period* in the report does
not reproduce. Recorded here because a symptom description that survives into
three documents becomes the thing people design against.

Two probes, both against Super Mario 64, both committed so the numbers are
re-runnable rather than remembered:

| probe | boundary | how to run |
| --- | --- | --- |
| `crates/rustyn64-frontend/examples/audio_probe.rs` | the emulated AI, before the ring | `RUSTYN64_PROBE_ROM=… cargo run --release --example audio_probe --features fast-exec,fast-scheduler` |
| `emu_thread::tests::measure_audio_gaps_at_the_device_boundary` | the device callback, after the ring | `RUSTYN64_PROBE_ROM=… cargo test -p rustyn64-frontend --release --lib measure_audio_gaps -- --ignored --nocapture` |

### What the emulated machine produces — it is not the AI

| | |
| --- | --- |
| frames carrying audible samples | **108 / 120** (the 12 are contiguous, at the start, before the game's audio engine begins) |
| `Audio::underruns` over the window | **3** |
| samples staged per frame | **1600**, exactly `output_rate / 60 * 2` |
| supply ratio | **26.1%**, against `fps / 60 = 26.1%` |

**So the emulated AI does not gap**, and the "supply is `fps / 60`" claim in
`audio.rs` is now measured rather than asserted. The competing hypothesis — that
the game's audio DMA is starving inside the machine, which would be an emulation
defect no host buffering could fix — is **refuted**: the stream is continuous
once it starts, and the underrun counter barely moves.

### What the device receives

At the device boundary, with the shipped 0.25 s ring and a 1024-frame callback:

| | |
| --- | --- |
| callbacks fully fed | **0 / 469** |
| callbacks fully empty | **384 / 469** |
| samples delivered | **14.2%** (default build, ~8.5 FPS — `fps / 60 = 14.2%`) |
| silent runs | mean **95 ms**, max **128 ms** |

**The reported ~1 s period is off by an order of magnitude.** The measured chop
is ~10 Hz, and its period is one emulated frame plus the pacer's yield — *not*
the ring's 0.25 s capacity, which an earlier note had guessed. Both halves of
that guess were wrong, and neither was checked before it was written down.

### Why no buffering fixes it

Supply is `fps / 60` by construction: `EmuCore::produce_audio` stages exactly one
emulated frame of audio per emulated frame, and the device consumes in wall-clock
time. A ring cannot emit samples that were never produced. The only levers are:

1. **make the core faster** — `docs/performance.md` records that 60 FPS is out of
   reach for this execution model, and that the entire declined optimization
   backlog is worth ~1.12x;
2. **an explicit slow-running audio policy** — resample to the speed actually
   achieved (continuous, heavily pitch-shifted), or mute below a speed threshold;
3. **accept the chopping**, which is what ships today.

That is a product decision, not a defect, and it is recorded here rather than
silently resolved.

### The rate-control servo does not exist

`audio.rs` describes a servo that "nudges the produce rate to keep the ring near
half-full", and `AudioRing::occupancy` is documented as existing to feed it.
**`occupancy` has no callers outside its own module.** The accessor was written
for a consumer that was never built, and three doc comments describe it as though
it runs. Whichever policy above is chosen, it is the servo's slot that would hold
it.

### One tempting fix, refuted by measurement

The pacer sleeps a full frame period after **every** frame while already ~85 ms
behind (`emu_thread::Schedule::snap_if_behind` sets `target = now + period`).
That is 17.7 ms of idle on a 100 ms frame — confirmed two ways: `frame_bench`
measures **99.955 ms** unpaced, the pacer delivers **117.6 ms**.

Shrinking that yield to 2 ms was built and measured A-B-A:

| | frames / 10 s | samples delivered | UI emu-lock p50 |
| --- | --- | --- | --- |
| A — full-period yield (shipped) | 85 | 14.2% | **664 ns** |
| B — 2 ms yield | 96–97 | 16.0–16.2% | **76.5 ms** |
| A — repeat | 85 | 14.2% | 614 ns |

**1.14x throughput, and it is not worth taking.** The UI's median wait for the
emu mutex goes from sub-microsecond to 76 ms, and the stand-in UI thread
completed 98 iterations instead of 173 in the same window. The yield is
*load-bearing*: it is the only interval in which anything else can take the emu
mutex, because the pacer holds it for the whole frame. The hypothesis that it was
vestigial is refuted.

The real unlock is architectural: `emu_thread`'s own header says "**the winit
thread never takes the emu mutex**", and `app.rs` takes it in six places
(ROM load, save-state, pause, reset). Route those through the existing
`SaveStateControls`-style request queue and the yield stops being a UI window, at
which point the 1.14x is available for free. That needs its own change and is not
attempted here.

## The fix that shipped: stretch, don't chop

**Policy chosen:** continuous audio at the wrong pitch, over chopped audio at the
right pitch. Of the three levers above, (1) is unavailable and (3) was what
shipped; this is (2).

`emu_thread::AudioServo` stretches each emulated frame's audio over the
wall-clock time that frame actually took. The samples are the same samples — the
servo cannot manufacture the missing 86%, and nothing can — they are simply spent
over the whole frame instead of over `1/60 s` followed by silence. A core at 14%
speed therefore sounds like a tape running at 14%: continuous, two and a half
octaves down.

### How it decides

| term | source | why |
| --- | --- | --- |
| feed-forward | EMA of the measured wall-clock interval between produced frames | correct on the *first* frame; an occupancy integrator covering a 7x range would take seconds to converge |
| trim | ring occupancy against half-full | removes residual drift, which the feed-forward term cannot see because it does not know what is already banked |

Clamped to `[1.0, MAX_AUDIO_STRETCH]`. It **never compresses below real time**: a
core running faster than 60 FPS is already oversupplying, and the ring's
drop-oldest is the right answer there. `MAX_AUDIO_STRETCH = 12.0` bounds it at
5 FPS — below that the audio chops again rather than becoming an unrecognizable
drone that also pins the ring.

### Measured, same harness as the defect

| | before | after |
| --- | --- | --- |
| callbacks fully fed | **0 / 469** | **412 / 469** |
| callbacks fully empty | 384 / 469 | **34 / 469** |
| samples delivered | **14.2%** | **90.9%** |
| ...second half only (steady state) | 14.2% | **100.0%** |
| silent runs | 86, mean 95 ms | 14, mean 52 ms |
| first fully-fed callback | never | **#15 (320 ms in)** |
| UI emu-lock p50 | 664 ns | 590 ns (unchanged) |
| pacer frames / 10 s | 85 | 85 (unchanged) |

**Steady state is 100%.** The whole-window 90.9% is the ramp from an empty ring
at startup, which is why the harness reports the second half separately — a
single figure charges a one-off transient to continuous quality.

### The trim gain is measured, not tuned

0.2, 0.4 and 0.6 were each run. **All three reach 100.0% steady state and all
three reach their first fully-fed callback at #15.** The only difference is the
whole-window figure (90.6 / 91.3 / 91.9%), which is dominated by the ramp.
Picking 0.6 for that 1.3-point edge would be tuning against a transient, so the
**lowest** gain ships: a larger one makes the pitch hunt audibly and nothing was
bought with it.

### What it does not change

- **Determinism.** The stretch is applied in the frontend resampler, which
  ADR 0004 already designates as the non-deterministic host-timing stage. The
  core's emitted stream (`Bus::drain_audio_samples`) is untouched, so
  `audio_play_rom` and `mixer_microcode` — which pin the *core* PCM — are
  unaffected, and the save-state trace compare does not read the resampled f32.
- **Anything without a ring.** With `ring: None` the servo is never consulted and
  the core keeps its default `1.0`, so headless builds and every test are
  byte-identical to before.
- **The pacer.** Frames produced per second is unchanged (85 both ways); this
  trades no throughput.
