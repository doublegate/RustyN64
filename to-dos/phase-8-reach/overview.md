# Phase 8 — Reach

## Goal

Extend the emulator beyond faithful playback: rollback netplay, RetroAchievements, TAS tooling,
Lua scripting, and a shader pipeline. Every item here is **additive and off by default**, so the
shipped build stays byte-identical with the flags off — the same posture RustyNES and RustySNES
hold, and the reason their accuracy numbers survive feature growth.

## Exit criteria

- [ ] Rollback netplay works for 2-4 players: predict, advance, roll back, and re-simulate on the
      deterministic core, with bit-identical resimulation under latency, jitter, and loss.
- [ ] RetroAchievements integrates through the `rcheevos` FFI, behind an opt-in feature, with
      hardcore mode disabling save-state load and rewind.
- [ ] TAS tooling records and replays input deterministically in a versioned movie format, with
      save-state branching.
- [ ] Lua scripting runs sandboxed, with memory and state access plus per-frame callbacks, and
      cannot perturb core determinism when enabled.
- [ ] A shader and filter pipeline runs post-framebuffer, never touching the core.
- [ ] The `rustyn64-netplay` and `rustyn64-cheevos` crates stop being one-line placeholders.
- [ ] With every flag off, the default build is byte-identical to the pre-Phase-8 build — proven
      by the golden corpus, not asserted.
- [ ] Every feature is documented with its determinism posture stated explicitly.

## Scope

In-scope:

- Rollback netplay on the existing determinism contract.
- RetroAchievements, TAS movies, Lua scripting, and the shader pipeline.
- Filling in the two placeholder crates.

Out-of-scope:

- Anything that changes core behaviour when disabled. If a feature cannot be additive, it does
  not land in this phase.
- Mobile platforms and a libretro core: both are plausible later, neither is in the v1.0 line.
- HLE microcode fast paths. Still barred by ADR 0002, and this phase is not a loophole.

## Sprints

- [Sprint 1 — Rollback netplay on the determinism contract](sprint-1-netplay.md) —
  the feature that most directly exercises ADR 0004, and the best proof it holds.
- Sprint 2 — RetroAchievements, TAS movies, and Lua scripting.
  **Status:** stub — refine when Sprint 1 is close to complete.
- Sprint 3 — The shader and filter pipeline.
  **Status:** stub — refine when Sprint 2 is close to complete.

## Dependencies

Phase 6 for the frontend surfaces these features attach to, and Phase 7 for an emulator accurate
enough that the features are worth having. Netplay in particular depends on the determinism
contract being *proven*, not merely specified — the ADR 0004 regression test from Phase 1.

## Risks

- **Netplay is the determinism contract's audit** — any nondeterminism anywhere shows up as a
  desync, usually far from the cause. Mitigated by rollback being built on the same snapshot and
  restore path that save-states already exercise, so bugs surface earlier.
- **Off-by-default drifts to on-by-default** — a feature that becomes convenient tends to get
  enabled, and byte-identity silently ends. Mitigated by proving byte-identity against the golden
  corpus in CI rather than trusting the flag.
- **Scripting is an escape hatch into the core** — a Lua API that can write arbitrary memory can
  break determinism and every guarantee built on it. Mitigated by sandboxing, by scoping the
  write surface, and by documenting the posture rather than leaving it implied.
- **Feature growth outpaces accuracy** — the reach features are more visible than the residuals
  in the ledger, and easier to work on. Mitigated by phase ordering: this is Phase 8 for a
  reason.

## Reference docs

- [docs/frontend.md](../../docs/frontend.md) — where these features attach.
- [docs/adr/0004-determinism-contract.md](../../docs/adr/0004-determinism-contract.md) —
  what netplay depends on.
- [docs/adr/0002-fractional-timebase-refactor.md](../../docs/adr/0002-fractional-timebase-refactor.md)
  — why HLE is still barred.
- [docs/STATUS.md](../../docs/STATUS.md) — the byte-identity claim this phase must not break.
