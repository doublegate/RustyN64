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

- [ ] Every register reads and writes correctly, including the write-only and clear-on-write
      bits.
- [ ] `AI_STATUS` reports full and busy accurately for the double-buffered queue.
- [ ] `AI_LENGTH` masks to its real granularity rather than accepting arbitrary values.

**Dependencies:** T-21-005
**Reference:** `n64brew_wiki/markdown/Audio Interface.md` §Registers
**Estimated complexity:** M

---

### T-41-002 — Double-buffered DMA and the drain interrupt

**Description:** implement the two-deep transfer queue that drains RDRAM to the DAC, raising the
AI interrupt through the MI when a buffer completes.

**Acceptance criteria:**

- [ ] Two transfers can be queued; the second begins when the first drains.
- [ ] The AI interrupt fires on drain and the CPU services it, so the game's audio loop advances.
- [ ] Underrun behaviour is defined and matches hardware rather than being "we stopped".
- [ ] A synthetic buffer test drives the whole path without requiring working microcode.

**Dependencies:** T-41-001
**Reference:** `n64brew_wiki/markdown/Audio Interface.md` §DMA
**Estimated complexity:** L

---

### T-41-003 — DAC rate derivation and the delayed-carry bug

**Description:** derive the sample rate as `video_clock / (DACRATE + 1)` so it follows region and
register state, and reproduce the AI's documented delayed-carry hardware bug.

**Acceptance criteria:**

- [ ] The rate is computed from the video clock and `AI_DACRATE`, never hardcoded.
- [ ] Changing region changes the rate correctly, because the video clock changes.
- [ ] The delayed-carry bug is reproduced, with a named test that fails if it is "fixed".
- [ ] `docs/audio.md` records the bug as intended behaviour.

**Dependencies:** T-41-001
**Reference:** `n64brew_wiki/markdown/Audio Interface.md` §Delayed-carry hardware bug
**Estimated complexity:** M

---

### T-41-004 — The host ring and the determinism boundary

**Description:** resample the emitted stream to the host device rate in the frontend, with a
lock-free ring and dynamic rate control — all of it outside the core, per ADR 0004.

**Acceptance criteria:**

- [ ] The core emits samples on the emulated timeline and never consults wall-clock time.
- [ ] The frontend resamples and paces, absorbing host jitter.
- [ ] Underrun is observable in the harness rather than silently concealed by the resampler.
- [ ] A determinism test proves the emitted sample stream is bit-identical across two runs from
      one seed.

**Dependencies:** T-41-002
**Reference:** `docs/frontend.md`; `docs/adr/0004-determinism-contract.md`
**Estimated complexity:** M

---

## Sprint review checklist

- [ ] All tickets checked off or explicitly deferred (with reason).
- [ ] A real ROM produces recognisable audio without underrun.
- [ ] CHANGELOG.md updated.
- [ ] `docs/audio.md` updated in the same change as the code it describes.
