# 0010 — The retail boot lives in the core; only the ELF direct-load stays in the harness

Status: **Proposed** — accepted on merge of the PR that introduces this file;
immutable thereafter.
Date: 2026-07-24
Deciders: repo owner
Supersedes: none · Superseded by: none

## Context

Phase 5 landed the retail cartridge boot — the HLE boot (`hle_boot`) and the faithful
real-PIF boot (`real_pif_boot`) — in `rustyn64-test-harness::rom`, alongside the ELF
direct-load (`load_direct` / `seed_ipl3_handoff` / `load_elf`) that the harness had used
since Phase 1. That module's doc stated the rule the code followed at the time:

> This is a **harness** facility, deliberately not a core one. The core must never depend
> on it, or the determinism contract would acquire a load-path dependency.

Phase 6 (the frontend shell) then hit a wall: the frontend must **boot a game** to present
anything, but `rustyn64-frontend` depends on `rustyn64-core`, not on the test harness — and
the only boot lived in the harness. Verified concretely: `EmuCore::load_rom` did
`Cart::load` + `System::reset`, which leaves the CPU at the PIF reset vector `0xBFC0_0000`
with no PIF ROM installed, so it fetched zeros (NOPs) and never reached game code. The shell
could not run a cartridge at all.

The "boot is a harness facility" rule conflated two different things:

1. **The retail boot** — what a real N64 actually does at power-on (seed the IPL3 state or
   run IPL1/IPL2, then jump into the cart's IPL3). This is emulation behaviour, and it is
   deterministic (fixed, cited seeds; ledger C-32/C-33). Every consumer needs it.
2. **The ELF direct-load** — a genuine *test shortcut*: n64-systemtest ships an ELF payload
   with no IPL3, so the harness places its program segments directly and seeds the handoff.
   This is not something hardware does, and the core must not depend on it (a real load-path
   dependency would let a test convenience influence the deterministic core).

The original rule is right about (2) and wrong to bind (1) to the harness.

## Decision

Split the seam:

- **Move the retail boot into the core** as `rustyn64_core::boot` — `hle_boot`,
  `real_pif_boot`, `cic_seed`, and a `BootError`. Both the frontend and the harness consume
  it. `EmuCore::load_rom` calls `rustyn64_core::boot::hle_boot`, so the shell boots a game.
- **Keep the ELF direct-load in the harness** — `load_direct`, `seed_ipl3_handoff`,
  `load_elf`, `entry_point`, and `LoadError` stay in `rustyn64-test-harness::rom`. The core
  never gains an ELF/test load path.
- The harness keeps a thin `rom::hle_boot` **wrapper** that routes an ELF-payload ROM to
  `seed_ipl3_handoff` and every other ROM to `rustyn64_core::boot::hle_boot`, and re-exports
  `real_pif_boot` / `cic_seed`, so all existing harness and test callers are unchanged.

The core stays `#![no_std]` — the retail boot operates on `&[u8]` and seeds `System` state,
needing no `std` and no host time / RNG, so the determinism contract is intact.

## Consequences

- The frontend can boot games. This is what makes Phase 6 ("the shell presents the machine")
  possible; without it the shell is a viewer of a machine that never runs a cartridge.
- One retail-boot implementation, shared by the frontend and the harness — no drift between a
  "frontend boot" and a "harness boot".
- The `docs/engineering-lessons.md` §3.4 rule ("the core must not acquire a test load-path
  dependency") is **preserved and sharpened**: it applies to the ELF direct-load (a test
  shortcut), not to the retail boot (real console behaviour). The earlier blanket "boot is a
  harness facility" phrasing is superseded by this ADR.
- `rustyn64_core::boot::BootError` carries the cart parse error (`BootError::Cart`) so the
  frontend can report *why* a ROM failed to load, not just "too small".
- No behaviour change to any existing test: the harness `hle_boot` wrapper reproduces the old
  dispatch exactly, and n64-systemtest (the ELF path) is unaffected (still `Failed: 0` on the
  Phase-1 categories; suite-wide count unchanged).
