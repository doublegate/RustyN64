# 0009 — HLE default boot, with a real-PIF path behind an off-by-default flag

Status: **Proposed** — accepted on merge of the PR that introduces this file;
immutable thereafter (design; the HLE path ships in this PR, the real-PIF path is staged)
Date: 2026-07-24
Deciders: repo owner
Supersedes: none · Superseded by: none

## Context

`to-dos/VERSION-PLAN.md` §v0.6.0 "Cartridge" (Phase 5) makes a *commercial* cartridge
actually boot. On real hardware a cold boot is not a jump to the cartridge — it is a
sequence the emulator must reproduce the *result* of:

1. The PIF's **IPL1/IPL2** (the on-board PIF ROM at `0x1FC0_0000`) runs first, performs
   the **CIC lockout challenge** (a seed/checksum handshake with the cartridge's CIC-NUS
   chip), and, on success, DMAs the cartridge's first `0x1000` bytes — its **IPL3** — into
   RSP DMEM and jumps to it.
2. **IPL3** (which lives on the cartridge, and differs per CIC revision) copies the game to
   RDRAM and jumps to the header entry point.
3. The game runs.

Two facts constrain how we can reproduce step 1:

- **The PIF ROM is copyrighted.** It cannot be committed (the three no-ROM guards —
  `.gitignore`, `check_no_roms.sh`, the `no-commercial-roms` CI job — exist to enforce
  exactly this). A boot path that *requires* the PIF ROM cannot run in CI and cannot be the
  default, or the project has no reproducible boot at all.
- **IPL3 is the cartridge's own code and is copyright-clean to *run*** (we do not ship it;
  the ROM the user supplies contains it). Running the cart's real IPL3 is LLE where it
  matters — the game's own bootcode executes — while the copyrighted, un-shippable part
  (IPL1/IPL2 + the CIC MCU) is exactly the part we must stand in for.

The determinism contract (ADR 0004) and the no-per-game-DB rule (ADR 0003) also bind: the
core must not consult a game database to decide how to boot, and the same seed+ROM+input
must produce bit-identical AV.

## Decision

Provide **two** boot paths:

- **HLE boot — the default, CI-able path.** Seed the observable *result* of IPL1/IPL2 + the
  CIC challenge, then run the cartridge's own IPL3:
  - copy the cart's real IPL3 (`ROM 0x40..0x1000`) into RSP DMEM and jump to it at
    `0xA400_0040`;
  - inject the per-CIC **seed word** into PIF RAM `0x24..0x28`;
  - seed the post-IPL3 machine state IPL3 hands the game: COP0 `Status`/`Config`, the
    `s3..s7` boot-argument GPRs, and the PI DOM1 bus-timing registers decoded from the ROM
    header (as IPL2 programs them).

  Every seeded value is a **cited constant** (N64brew *CIC-NUS* / *PIF-NUS*, cen64
  `si/cic.c`), documented with provenance in `docs/accuracy-ledger.md` **C-32**, and pinned
  by `hle_boot_seeds_retail_state` — none is fitted to make a ROM pass. This is
  `rustyn64-test-harness::rom::hle_boot`, and it ships in this PR.

- **Real-PIF boot — an off-by-default, local-only path (staged, Sprint 2).** Execute the
  user-supplied PIF ROM dump at `0x1FC0_0000` (IPL1/IPL2) with the CIC modelled
  (seed/checksum; a decapped CIC-MCU ROM only if one is available, else the seed-based
  response). Because it needs the copyrighted PIF ROM, it is **behind an off-by-default
  flag, validated locally, and never CI-gated.** It is not implemented in this PR; it is
  the Sprint-2 deliverable, recorded here so the design is fixed before the code lands.

The seed injection is the HLE path's stand-in for the CIC challenge, not a reimplementation
of it; the real-PIF path is the faithful reproduction for those who supply the ROM.

### Why not the alternatives

- **Real-PIF only.** Faithful, but the copyrighted PIF ROM cannot ship, so there would be
  **no reproducible boot in CI** and no boot at all for a user without the dump — a
  non-starter for the default.
- **HLE only.** Copyright-clean and CI-able, but it can never be *bit-for-bit* the real
  boot: the seeds are the documented *result*, not proof they are the *only* state a real
  IPL3 leaves. Offering the real-PIF path (opt-in) is what lets an owner with the ROM check
  the HLE state against the genuine one.
- **A per-game boot database.** Rejected by ADR 0003/0004 — the core must not consult a DB.
  Boot is parameterised by save type + CIC + region (resolved outside the core), never by a
  game-keyed table.

## Consequences

- `rom::hle_boot` is the default retail boot and the one CI exercises; the n64-systemtest
  ELF-payload path (`seed_ipl3_handoff`) is unchanged for the accuracy suite.
- The HLE seeds are a small set of **cited constants** (C-32); changing any of them fails
  `hle_boot_seeds_retail_state`. The un-modelled remainder is bounded by the real-PIF path
  (opt-in) and by whatever n64-systemtest boot-state coverage runs.
- The real-PIF path introduces a **local-only, copyrighted input** (the PIF ROM), which the
  no-ROM guards keep out of the repository; it is off by default and never in a CI gate.
- Boot is **additive and default-clean**: a machine with no cartridge behaves exactly as
  before, so determinism and byte-identity (ADR 0004/0005) are unaffected.
- **Booting is all-or-nothing and exercises the whole emulator.** A commercial ROM that
  boots may still not reach video because of *downstream* subsystems (the VI vblank loop,
  the RI/RDRAM interface, F3DEX graphics microcode) that are outside the cart boundary. That
  gap is characterised honestly in `docs/accuracy-ledger.md` **R-18**, not hidden behind a
  faked pass — the commercial-boot capstone asserts "boots and executes real code", and
  *reports* (does not assert) whether a frame was produced.

## Staged implementation plan

- **This PR — the HLE boot.** `rom::hle_boot`, the cited seeds (C-32), the boot-state test,
  and the commercial-boot capstone (local, `#[ignore]`d; R-18).
- **Sprint 2 — the real-PIF path.** Execute the local PIF ROM with the CIC modelled, behind
  an off-by-default flag; validate locally against the HLE state on ROMs the owner supplies.
  Never CI-gated.
