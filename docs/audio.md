# AI audio + the RSP audio microcode path — RustyN64

**References:** `ref-docs/research-report.md` §5 (AI), §3 (RSP); ADR 0004;
`crates/rustyn64-audio/src/lib.rs`; `docs/rsp.md`; `docs/frontend.md`.

This doc is the SPEC, not history — update it in the same PR as the code.

## Purpose

The N64 audio path splits cleanly: the **RSP audio microcode** does all the work
(decode ADPCM sample banks, apply envelopes/effects, mix, resample → 16-bit
signed stereo PCM into an RDRAM buffer), and the **Audio Interface (AI)** is a
dumb DAC fed by DMA — it "does absolutely no conversion on the samples"
(`ref-docs/research-report.md` §5). So under LLE, audio is **emulated for free**
by emulating the RSP (`docs/rsp.md`); this crate only models the AI side.

## Interfaces

```rust
pub trait AudioBus {
    fn ai_dma_read_u32(&self, addr: u32) -> u32; // a 32-bit word = two i16 L/R
    fn raise_ai_interrupt(&mut self);            // buffer drained → MI_INTR.ai
}

pub struct StereoSample { pub left: i16, pub right: i16 }

pub struct Audio {
    pub dram_addr: u32,    // AI_DRAM_ADDR — RDRAM base of the buffer DMA'd
    pub len: u32,          // AI_LEN — remaining transfer length, bytes
    pub sample_rate: u32,  // AI_DACRATE-derived output rate, Hz
    pub dma_enabled: bool, // AI_CONTROL bit 0
}
impl Audio {
    pub fn tick<B: AudioBus>(&mut self, bus: &mut B); // no-op while idle
}
```

## State

Beyond the skeleton (the rest is TODO): the **two-deep DMA address/length FIFO**
(the AI double-buffers — software queues the next buffer while the current one
plays), the **DACRATE / bitrate dividers**, and the `AI_STATUS` busy/full flags
the CPU polls.

## Behavior

### The DMA loop

The CPU points the AI at a finished PCM buffer (`AI_DRAM_ADDR` + `AI_LEN`) and
enables DMA. The AI streams the buffer to the DAC at the programmed sample rate,
raising the **AI interrupt** when the buffer drains so software can queue the next
one. With both FIFO slots filled, playback is gap-free
(`ref-docs/research-report.md` §5).

### Sample rate

Derived, not fixed (`ref-docs/research-report.md` §5):

```text
sample_rate = video_clock / (AI_DACRATE + 1)
```

e.g. `AI_DACRATE = 1103` → ~44136 Hz on NTSC (near CD quality). Games commonly
run 22–32 kHz to save RSP time. The **video clock differs by region** (NTSC vs
PAL), so the same `DACRATE` yields a different rate per region — carry the
video-clock constant in the region table (`docs/compatibility.md`).

### Where mixing lives

ADPCM decode, envelopes, mixing, and resampling are **RSP microcode**, not this
crate. RustyN64 does not implement an audio HLE; the audio microcode runs on the
LLE RSP core (`docs/rsp.md`, ADR 0002). The *host*-rate resample (N64 output rate
→ the host's `cpal` device rate) is a **frontend** stage, kept out of the
deterministic core (`docs/frontend.md`, ADR 0004) — exactly as RustyNES keeps DRC
/ run-ahead in the frontend.

## Edge cases and gotchas

- **The AI does no conversion.** Resist the temptation to add per-game audio HLE;
  correctness comes from the RSP (`ref-docs/research-report.md` §5).
- **Region changes the rate at the same DACRATE.** The video-clock divisor is
  region data; getting it wrong detunes every game's audio.
- **Double-buffer underrun.** If software does not queue the next buffer before
  drain, the DAC repeats/silences — model the FIFO depth (2) so busy-wait timing
  matches, and schedule the AI interrupt at the *drain* cycle, not instantly
  (`docs/scheduler.md` event model).
- **Host resample is non-deterministic by nature** — it MUST live in the
  frontend, never in the core, or the determinism contract breaks (ADR 0004).
- **Samples are big-endian i16 pairs.** `ai_dma_read_u32` returns a word holding
  L then R; unpack per the RDRAM byte order.

## Test plan

- **AI register/timing unit tests** — DACRATE→rate derivation per region; the
  FIFO double-buffer swap; the interrupt fires at drain, not immediately.
- **RSP audio-microcode integration** (in `docs/rsp.md`'s plan) — run real audio
  microcode and compare the mixed PCM buffer byte-for-byte against a reference.
- **PeterLemon audio tests** — the bare-metal audio corpus
  (`ref-docs/research-report.md` §7).
- **Determinism** — same seed + ROM + input ⇒ bit-identical AI output stream
  (before the frontend resample).

## Open questions

- Exact AI status-flag timing (busy/full) that busy-wait audio loops depend on —
  pin from PeterLemon + hardware logs.
- Per-region video-clock constants for the AI divisor (shared with the VI region
  table; `docs/compatibility.md`).
