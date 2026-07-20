# ADR 0003 — No board tiering / honesty gate (one cart model)

## Status

Accepted.

## Context

RustyNES carries a **mapper tiering** system (Core / Curated / BestEffort) plus a
CI **honesty gate** (`mapper_tier_honesty.rs`) whose job is to prevent a
low-confidence "BestEffort" mapper from silently backing the accuracy oracle —
because the NES has *hundreds* of mapper families of wildly varying support
quality, and the project must not claim accuracy it hasn't earned.

The N64 is structurally different. Per `ref-docs/research-report.md` §6 and §Scope,
there is essentially **one cartridge model**: a ROM image read over the PI bus,
parameterized by a small, closed set of axes —

- **save type** (EEPROM 4k/16k, SRAM, FlashRAM, Controller Pak),
- **CIC variant** (6101/6102/6103/6105/6106 + PAL 71xx),
- **region** (NTSC/PAL),
- **Expansion Pak** presence.

These are *data* resolved from a per-game DB by serial/CRC (`docs/cart.md`,
`docs/compatibility.md`), not hundreds of distinct board implementations of varying
fidelity. There is no equivalent of the NES "this mapper is a rough guess" problem
to gate against. The harness already encodes this:
`rustyn64-test-harness` sets `boards_tiered = false` and ships **no**
honesty-gate test.

## Decision

RustyN64 has **no board tiering and no honesty-gate test.** The accuracy oracle is
simply:

- **n64-systemtest** pass/fail (CPU / COP0 / TLB / RSP — self-judging,
  hardware-verified), and
- the **ParaLLEl-RDP** conformance fuzz suite (RDP bit-exactness),

plus the PeterLemon/Dillonb regression ROMs and commercial screenshots
(`docs/testing-strategy.md`). Cartridge correctness is validated as
save-round-trip + SaveTest-N64 + the n64-systemtest PIF/SI categories, not as a
tier matrix.

If a future need arises (e.g. exotic flashcart hardware, the 64DD), this ADR is
revisited — but the default N64 posture is one un-tiered cart model.

## Consequences

- **+** No tiering bookkeeping or honesty-gate CI test to maintain; the
  `Cartridge` trait stays a thin, default-no-op parameterization.
- **+** "Accurate" has a single crisp definition for the cart layer:
  n64-systemtest + the save oracle pass.
- **−** Save-type / CIC correctness leans entirely on the per-game DB; a wrong DB
  entry mis-detects a game's save backend or boot offset (mitigated by the save
  round-trip oracle catching mismatches).
- The `docs/STATUS.md` board matrix records `boards_tiered = NO` so no future
  contributor reintroduces NES-style tiering by reflex.
