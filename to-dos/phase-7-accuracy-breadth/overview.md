# Phase 7 — Accuracy breadth

## Goal

Drive the accuracy battery across the commercial corpus rather than the test-ROM corpus: the
custom-microcode families, every save backend, and NTSC/PAL region timing expressed as data.
This is where the 66-ROM corpus staged in Phase 0 is finally used for what it was selected for,
and where residuals are either closed or documented — never point-fixed with a per-game hack.

## Exit criteria

- [ ] The accuracy battery is authored: named probes with a reported pass rate, replacing the
      stubbed `AccuracyScorer`.
- [ ] The battery reaches its target pass rate, with every failure either fixed or entered in the
      accuracy ledger with a disposition.
- [ ] The custom-microcode titles render correctly — the Factor 5 set (Rogue Squadron, Battle for
      Naboo, Indiana Jones) and the Boss Game Studios set (World Driver Championship, Top Gear
      Rally 2). These are the payoff for LLE and the proof it was the right call.
- [ ] Every save backend is validated against real titles from its folder, not synthetic writes.
- [ ] Region timing is data, not branches: the NTSC and PAL VI/AI divisors live in a table, and
      the open question about exact PAL values is closed.
- [ ] The Expansion Pak titles run with the 8 MB memory map (Donkey Kong 64, Perfect Dark,
      Majora's Mask, Turok 2).
- [ ] The exotic peripherals work: the VRU (Hey You, Pikachu!) and the Transfer Pak (both Pokemon
      Stadium titles).
- [ ] A visual golden corpus is committed — screenshots and `.snap` baselines from the commercial
      ROMs, with the ROMs themselves still absent.
- [ ] An accuracy ledger exists (`docs/accuracy-ledger.md`) mapping every known divergence to
      Remediated, No-stricter-oracle-available, Deferred, or Out-of-scope.
- [ ] The wgpu-compute RDP accelerator, if it lands here, is validated against the software
      reference and does not become the oracle.

## Scope

In-scope:

- Authoring the accuracy battery and its probes.
- Driving the commercial corpus and triaging what fails.
- Region timing tables and the memory-configuration matrix.
- The exotic peripherals.
- The accuracy ledger and the visual golden corpus.
- Optionally, the wgpu-compute RDP backend, validated against the software reference.

Out-of-scope:

- Per-game hacks. If a title needs one, that is a modelling failure recorded in the ledger, not
  a fix.
- The sub-cycle bus-timing refactor. Residuals that genuinely require it are deferred to
  ADR 0005, beyond v1.0.
- Netplay, achievements, and tooling (Phase 8).

## Sprints

- [Sprint 1 — The accuracy battery and corpus triage](sprint-1-battery-triage.md) —
  author the probes, run the corpus, and classify every failure.
- Sprint 2 — Custom microcode, Expansion Pak, and the exotic peripherals.
  **Status:** stub — refine when Sprint 1 is close to complete.
- Sprint 3 — Region timing as data, and the accuracy ledger.
  **Status:** stub — refine when Sprint 2 is close to complete.

## Dependencies

Phases 1-6 complete. This phase cannot begin meaningfully until a commercial ROM boots, renders,
plays audio, and accepts input, because that is the minimum bar for judging a game's accuracy at
all.

## Risks

- **Per-game hacks are the path of least resistance** — one title misbehaving is far quicker to
  special-case than to model. Mitigated by ADR 0003's position that the N64 has one cart model,
  and by requiring a ledger entry instead of a branch.
- **The battery can be gamed** — probes authored after seeing our own output measure agreement
  with ourselves. Mitigated by deriving probes from hardware behaviour and reference emulators,
  not from our captures.
- **A GPU backend can quietly become the oracle** — once the accelerator is faster and looks
  right, it is tempting to grade against it. Mitigated by ADR 0002: the software reference stays
  the oracle, and the accelerator is graded against it.
- **PAL is under-tested** — the corpus is USA dumps, so PAL timing has no regression coverage.
  Mitigated by treating the region table as data with its own unit tests, and by recording the
  gap honestly rather than implying coverage.

## Reference docs

- [docs/compatibility.md](../../docs/compatibility.md) — regions, memory configuration, per-game
  save and CIC.
- [docs/testing-strategy.md](../../docs/testing-strategy.md) — Layers 4 and 5.
- [docs/performance.md](../../docs/performance.md) — when acceleration is warranted.
- [docs/adr/0003-no-board-tiering-honesty-gate.md](../../docs/adr/0003-no-board-tiering-honesty-gate.md)
- [tests/roms/external/commercial/README.md](../../tests/roms/external/commercial/README.md) —
  the corpus and why each title is in it.
