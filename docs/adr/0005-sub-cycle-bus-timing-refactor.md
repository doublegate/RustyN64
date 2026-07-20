# ADR 0005 — The deferred sub-cycle bus-timing refactor

## Status

Proposed, and deliberately deferred beyond v1.0.

Split out of ADR 0002, which originally carried it as a "Part B" under a different status from
its own Part A. That ADR is now [ADR 0002 — LLE coprocessors](0002-lle-coprocessors.md).

## Context

The scheduler advances on a canonical 187.5 MHz clock; the CPU steps every 2nd tick and the RCP
every 3rd (ADR 0006, superseding ADR 0001's fractional accumulator). ADR 0007 additionally models
the SysAD command/data split at SClock (62.5 MHz). Note that SClock is *coarser* than a PClock,
so none of that provides sub-PClock resolution — this ADR remains a separate, later question.

Some hardware behaviour is finer-grained than one VR4300 cycle. If a test ROM or a commercial
title turns out to observe it, whole-tick lockstep cannot represent it, and the scheduler needs a
finer timebase.

### What "sub-cycle" actually means on this machine

The original wording of this decision described the refactor as "a φ1/φ2 access split (the
Mesen2-style fractional master clock)". That framing was carried over from the sibling NES and
SNES projects and does not describe N64 hardware. φ1/φ2 is 6502 two-phase clock vocabulary, where
φ2 is the bus-access phase — it is the natural frame for a 6502 or 65816 bus model. It is not
used anywhere in the N64 hardware documentation, and Mesen2 is an NES/SNES emulator, so it is the
wrong reference implementation to name.

The N64's genuine sub-cycle concerns are these, and they are what this ADR is actually about:

1. **SysAD transaction structure.** The CPU-to-RCP interface is a handshake protocol, not a
   simple addressed read: `SYSCMD` carries the command, and `Pvalid` / `Evalid` / `EoK` sequence
   the transaction across multiple cycles while the buses go high-Z in between. A cached
   256-bit instruction read and a non-cached word write occupy the bus very differently
   (`n64brew_wiki/markdown/SysAD Interface.md`). Modelling a memory access as instantaneous at
   tick granularity elides that structure.
2. **RDRAM bank state, refresh, and latency.** The RI exposes `RI_LATENCY`, `RI_REFRESH`, and
   `RI_BANK_STATUS`, and the wiki documents explicit bank-status tracking. Access cost depends on
   whether the target bank is already open, and a refresh cycle can intrude
   (`n64brew_wiki/markdown/RDRAM Interface.md`).
3. **Arbitration between four bus masters.** The CPU, RSP, RDP, and the DMA engines all contend
   for one memory pool. How precisely that arbitration must be modelled is already an open
   question in `docs/architecture.md`, and it is the most likely source of a residual that
   whole-tick resolution cannot express.

The reference implementation to study for all three is **CEN64**, which models the machine at bus
level and is BSD-3-Clause, so it is readable *and* permissively licensed. That is the correct
analogue to what "Mesen2-style" was gesturing at for the other consoles.

## Decision

**Do not build this now.** Keep the whole-tick scheduler, and:

- Treat any residual that appears to need sub-cycle resolution as **documented and deferred**, in
  `docs/accuracy-ledger.md`, rather than point-fixed with a special case. A per-quirk patch that
  makes one ROM pass is precisely the failure mode ADR 0001's single-timeline design exists to
  avoid.
- Gate the refactor on **evidence**: a specific, reproducible test-ROM or commercial-title
  failure that is understood well enough to say which of the three mechanisms above is
  responsible. "Might need it eventually" is not a trigger.
- Before a residual may be attributed to sub-cycle resolution at all, classify the failing
  measurement as **absolute** or **differential**. A test that measures the interval between two
  events on the same clock is invariant under any uniform change to when a subsystem is serviced
  within the step — finer resolution cannot move it, and the real cause is a wrong duration, a
  wrong divisor, or a missing event. A sibling project implemented and rolled back five successive
  re-phasings of one timing track before recognising the whole family was immune. Differential
  residuals do not count as evidence for this refactor; recording that classification is the
  cheapest possible screen and costs one line in the ledger.
- When it does land, treat it as a **major version** (`v2.0.0`), because it is expected to break
  byte-identity and save-state compatibility. It is announced in advance, and it is the only
  currently-anticipated candidate for a MAJOR bump (`to-dos/VERSION-PLAN.md`).

Design *for* it in the meantime by keeping timing authority in the scheduler rather than inside
chips, so that increasing resolution is a change to one component and not to every chip.

## Consequences

### Positive

- v0.1 through v1.0 stay simple. The whole-tick model is easy to reason about, fast, and
  sufficient for everything the project can currently verify.
- The trigger is evidence-based, so the refactor cannot be justified by speculation, and the
  ledger records exactly what would justify it.
- Keeping timing authority in the scheduler means the eventual change is contained.

### Negative / costs

- Some residuals will stay open through v1.0 and be visible in the accuracy ledger. That is the
  intended trade: an honest open entry beats a per-quirk patch.
- Deferring means the eventual refactor lands against a much larger codebase, with more
  save-state surface to migrate.

### Risks

- **Three different things get called "finer timing", and conflating them would misrepresent the
  implementation as more precise than it is.** They are, coarse to fine: (1) the canonical
  187.5 MHz master clock, where a CPU cycle spans 2 ticks and an RCP cycle 3 (ADR 0006 — this
  exists); (2) the SysAD command/data split at SClock, 62.5 MHz, which is *coarser* than a
  PClock (ADR 0007 — this exists); and (3) resolution finer than one PClock, which is **this
  ADR and does not exist**. The sibling projects have a version-numbering history showing how
  easily these get merged in prose. All three senses are named explicitly here, in
  `docs/scheduler.md`, and in `docs/STATUS.md` §Version policy.
- **Scope creep into a rewrite.** Bus-accurate modelling can absorb unlimited effort. The gate is
  a named failing test, not an aspiration to be maximally accurate.
