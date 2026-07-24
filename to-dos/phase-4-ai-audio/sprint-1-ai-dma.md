# Sprint 1 — The AI register set, DMA, and the host ring

**Phase:** Phase 4 — AI audio
**Sprint goal:** a PCM buffer in RDRAM reaches the host audio device at the right rate, with the
DAC timing derived from register state rather than assumed, and the delayed-carry hardware bug
reproduced.
**Estimated duration:** 2 weeks

## Tickets

### T-41-001 — The AI register set

**Description:** implement `AI_DRAM_ADDR`, `AI_LENGTH`, `AI_CONTROL`, `AI_STATUS`, `AI_DACRATE`,
and `AI_BITRATE` with hardware read/write semantics.

**Acceptance criteria:**

- [x] Every register reads and writes correctly, including the write-only and clear-on-write
      bits. *(Write-only regs mirror `AI_LENGTH`; an `AI_STATUS` write acks the interrupt.)*
- [x] `AI_STATUS` reports full and busy accurately for the double-buffered queue.
- [x] `AI_LENGTH` masks to its real granularity rather than accepting arbitrary values. *(18-bit,
      `& ~7`.)*

**Dependencies:** T-21-005
**Reference:** `n64brew_wiki/markdown/Audio Interface.md` §Registers
**Estimated complexity:** M
**Status:** DONE (Sprint 1) — `rustyn64-audio::Audio::{read_reg,write_reg}`,
`Bus::is_ai_register`/`ai_write`; unit + Bus integration tests.

---

### T-41-002 — Double-buffered DMA and the drain interrupt

**Description:** implement the two-deep transfer queue that drains RDRAM to the DAC, raising the
AI interrupt through the MI when a buffer **starts** — both enqueue-into-idle and promotion of the
queued buffer (not when a buffer completes; wiki §DMA).

**Acceptance criteria:**

- [x] Two transfers can be queued; the second begins when the first drains.
- [x] The AI interrupt fires **on start, not drain** — the acceptance wording is corrected: the
      interrupt fires when a buffer *starts* (enqueue-into-idle, and promotion of the queued
      buffer), which is what lets the CPU refill during playback (wiki §DMA, ares). The CPU
      services it via the MI.
- [x] Underrun behaviour is **defined as hold-and-decay** rather than being "we stopped";
      hardware equivalence (the analogue decay curve) remains **unverified** (ledger R-17).
      *(Deterministic integer decay + an observable `underruns()` counter.)*
- [x] A synthetic buffer test drives the whole path without requiring working microcode.

**Dependencies:** T-41-001
**Reference:** `n64brew_wiki/markdown/Audio Interface.md` §DMA
**Estimated complexity:** L
**Status:** DONE (Sprint 1) — derived-timing emission off `master_ticks` (ADR 0006);
`plays_a_buffer_and_drains_it`, `second_buffer_promotes_and_raises_irq_on_start`,
`underrun_is_observable_and_decays`.

---

### T-41-003 — DAC rate derivation and the delayed-carry bug

**Description:** derive the sample rate as `video_clock / (DACRATE + 1)` so it follows region and
register state, and reproduce the AI's documented delayed-carry hardware bug.

**Acceptance criteria:**

- [x] The rate is computed from the video clock and `AI_DACRATE`, never hardcoded.
- [x] Changing region changes the rate correctly, because the video clock changes.
      *(`dacrate_derives_rate_per_region`.)*
- [x] The delayed-carry bug is reproduced, with a named test that fails if it is "fixed".
      *(`delayed_carry_bug_bumps_the_next_buffer`.)*
- [x] `docs/audio.md` records the bug as intended behaviour.

**Dependencies:** T-41-001
**Reference:** `n64brew_wiki/markdown/Audio Interface.md` §Delayed-carry hardware bug
**Estimated complexity:** M
**Status:** DONE (Sprint 1) — region video-clock constants (documented provenance);
`AI_STATUS` `COUNT`/`WC` are a best-effort readback (ledger R-16).

---

### T-41-004 — The host ring and the determinism boundary

**Description:** resample the emitted stream to the host device rate in the frontend, with a
lock-free ring and dynamic rate control — all of it outside the core, per ADR 0004.

**Acceptance criteria:**

- [x] The core emits samples on the emulated timeline and never consults wall-clock time.
      *(`Audio::tick` derives emission from `master_ticks`; a source-level guard already forbids
      wall-clock/entropy in the core.)*
- [~] The frontend resamples and paces, absorbing host jitter. **Deferred to Sprint 3** — the core
      sink (`Bus::drain_audio_samples`) and the `cpal`+`AudioRing` host side already exist; wiring
      `produce_audio` to the real drain with an N64→device resampler is Sprint 3, alongside the
      end-to-end real-ROM audio gate.
- [x] Underrun is observable in the harness rather than silently concealed by the resampler.
      *(`Audio::underruns()`; ledger R-17.)*
- [x] A determinism test proves the emitted sample stream is bit-identical across two runs from
      one seed. *(`emission_is_deterministic`; ROM-level determinism is Sprint 3.)*

**Dependencies:** T-41-002
**Reference:** `docs/frontend.md`; `docs/adr/0004-determinism-contract.md`
**Estimated complexity:** M
**Status:** core sample sink + determinism DONE (Sprint 1); frontend resampler DEFERRED to
Sprint 3.

---

## Sprint review checklist

- [x] All tickets checked off or explicitly deferred (with reason). *(T-41-004's frontend
      resampler is the one explicit deferral, to Sprint 3.)*
- [~] A real ROM produces recognisable audio without underrun. **Deferred to Sprint 2/3** — needs
      the ELF/PI-DMA boot harness for the `DoubleShot` PCM ROM (Sprint 2) and the RSP-microcode
      ROM (Sprint 2), then the frontend drain (Sprint 3).
- [x] CHANGELOG.md updated.
- [x] `docs/audio.md` updated in the same change as the code it describes.

## Sprint status — CLOSED (Sprint 1 scope)

The AI **interface** — the register block, the two-deep DMA FIFO, the derived DAC rate, the
interrupt-on-start, the delayed-carry bug, and the deterministic sample sink — landed with unit
tests (`rustyn64-audio`) and a Bus integration test (`rustyn64-core`). All gates green
(fmt/clippy/test/rustdoc/no_std). Two items are deliberately carried forward, not dropped:

- **The real-ROM audio rungs** (project64 `DoubleShot` bare-metal PCM, then the libdragon-mixer
  RSP-microcode ROM) both need the ELF/PI-DMA boot harness and move to **Sprint 2**, where the
  RSP audio microcode is brought up.
- **The frontend resampler** (N64 rate → `cpal` device rate) moves to **Sprint 3** with the
  end-to-end wiring and the phase-close release.
