# Phase 4 — AI audio

> **STATUS: COMPLETE — released as v0.5.0 "Resonance" (2026-07-24).** The AI is implemented
> (Sprint 1); the real libdragon mixer microcode runs on the LLE RSP and produces verified PCM,
> and a bare-metal ROM plays PCM through the AI (Sprint 2); the frontend drains + resamples the
> stream to the host device (Sprint 3). Exit criteria marked below; the one that needs a real
> *game* through the full path (`recognisable audio through the frontend ring`) is honestly
> carried to Phase 5/6, which provide cart boot and the playable shell.

## Goal

The Audio Interface in `rustyn64-audio` DMAs the PCM buffer that the RSP audio microcode
produced in RDRAM out to the DAC, raising the AI interrupt on drain, and the frontend resamples
that stream to the host device. There is no per-game audio HLE and no separate audio DSP: the
audio microcode already runs on the LLE RSP built in Phase 2, so this phase is the interface and
the timing, not the synthesis (ADR 0002).

## Exit criteria

- [x] The AI registers behave: `AI_DRAM_ADDR`, `AI_LENGTH`, `AI_CONTROL`, `AI_STATUS`,
      `AI_DACRATE`, and `AI_BITRATE`. *(Sprint 1.)*
- [x] The double-buffered DMA is correct — the AI holds two pending transfers, and `AI_STATUS`
      reports full/busy accordingly.
- [x] The sample rate derives from the video clock as `video_clock / (DACRATE + 1)`, so the rate
      is a consequence of region and register state rather than a constant.
- [x] The **delayed-carry hardware bug** in the AI is reproduced, not corrected — it is
      observable and documented upstream. *(Named test fails if "fixed".)*
- [x] The AI interrupt fires when a transfer **starts** (corrected from "on drain") and drives the
      MI line, so the game's audio loop advances.
- [x] Underrun behaviour is **defined** (hold-and-decay + observable count); hardware equivalence
      of the decay curve is unverified (ledger R-17).
- [~] A real ROM produces recognisable audio through the frontend ring without underrun. **Split:**
      a bare-metal ROM plays PCM through the AI end to end (`audio_play_rom.rs`), and the frontend
      resampler + drain are wired and unit-tested — but a real *game* through the full ring needs
      cart boot (Phase 5) + the playable shell (Phase 6). Carried there.
- [x] The audio path is deterministic: same seed + ROM + input ⇒ bit-identical sample stream, all
      rate control in the frontend (ADR 0004). *(AI stream and mixer PCM both pinned deterministic.)*

## Scope

In-scope:

- The AI register set, the double-buffered DMA, and the interrupt.
- The DAC rate derivation and the region dependency.
- The documented AI errata.
- The frontend-side resampler and ring buffer, plus the pacing that keeps them fed.

Out-of-scope:

- Audio *synthesis* — that is the RSP audio microcode, already covered by Phase 2. If audio is
  wrong and the AI is right, the bug is in the RSP.
- Dynamic rate control and run-ahead interaction (Phase 6), which live in the frontend.
- Expansion audio and any per-game audio hack: neither exists on this platform, by design.

## Sprints

- [Sprint 1 — The AI register set, DMA, and the host ring](sprint-1-ai-dma.md) —
  from an RDRAM buffer to audible output, with the timing derived rather than assumed.

## Dependencies

Phase 2 complete: the audio microcode must actually run for there to be a PCM buffer to DMA.
Phase 1 for the CPU that programs the AI registers. The Bus already exposes the narrow
`AudioBus` trait.

## Risks

- **Audio bugs are attributed to the wrong chip** — a wrong sample stream usually means the RSP
  microcode is wrong, not the AI. Mitigated by gating Phase 4 behind a green RSP category and by
  testing the AI with a synthetic buffer before trusting microcode output.
- **The delayed-carry bug looks like a defect** — as with the VR4300 errata, it invites being
  "fixed". Mitigated by a named test that fails if the bug is removed.
- **Determinism is easy to lose here** — audio is the natural place to reach for wall-clock
  pacing. Mitigated by ADR 0004's hard split: the core emits samples on the emulated timeline,
  and only the frontend knows what time it is.
- **Underrun masks timing errors** — a resampler that papers over gaps hides an AI that is
  draining at the wrong rate. Mitigated by making underrun observable in the harness rather than
  silently concealed.

## Reference docs

- [docs/audio.md](../../docs/audio.md) — the AI spec and the mixer.
- [docs/adr/0004-determinism-contract.md](../../docs/adr/0004-determinism-contract.md)
- [docs/frontend.md](../../docs/frontend.md) — the host ring and pacing.
- `n64brew_wiki/markdown/Audio Interface.md` — the register set, the DMA, and the
  delayed-carry hardware bug.
