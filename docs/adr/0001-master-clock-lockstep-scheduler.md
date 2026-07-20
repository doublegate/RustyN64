# ADR 0001 — Fractional master-clock lockstep scheduler

## Status

**Superseded by [ADR 0006](0006-one-canonical-master-clock.md) (2026-07-20).**

The goals recorded below are unchanged and still correct: one timeline, lockstep rather than
catch-up, a seeded power-on phase, and no OS threads in the core. ADR 0006 changes two things.
The master tick unit becomes **187.5 MHz** rather than the VR4300 cycle, which makes every
emulated clock domain an integer divisor and removes the fractional accumulator described here.
And it adds the rule this ADR lacked: exactly one counter may be incremented, and every other
cycle position is assigned from it. Read 0006 for the current model; this ADR is retained
unmodified as the record of what was decided first and why.

## Context

RustyN64 targets cycle accuracy at the ares / CEN64 / Gopher64 bar without
per-quirk patches. The machine has **three** asynchronous compute engines — the
VR4300 CPU (93.75 MHz), the RSP, and the RDP (both 62.5 MHz) — plus several timed
DMA channels (PI/SI/SP/AI) and region-dependent VI/AI clock divisors, all sharing
one RDRAM bus. Per `ref-docs/research-report.md` §Executive summary and
§Background, accurate emulation is fundamentally a **co-scheduling** problem: games
synchronize CPU code against RSP/RDP progress (SP_STATUS polls, DP/SP/PI/SI
interrupts, framebuffer read-back), so running an engine "to completion" at a
frame boundary breaks mid-frame effects.

The VR4300:RCP ratio is a clean **3:2** (the R4300i `DivMode` 1.5:1 pin setting,
§1). But the surrounding clocks — the AI rate (`video_clock / (DACRATE+1)`), the
VI counters, PAL field timing, byte-count-driven DMA durations — are not integer
multiples of either core clock.

## Decision

A single **lockstep** scheduler in `rustyn64-core` (`scheduler.rs`) advances the
**VR4300 cycle as the master tick unit** (`MASTER_HZ = 93_750_000`); the RCP
advances on a **fractional 3:2 accumulator** (2 RCP ticks per 3 master ticks,
`RCP_NUM=2`/`RCP_DEN=3`). `System::tick_one_unit()` steps the CPU, then drains the
RCP accumulator (RSP → RDP → AI in order) on the same `&mut Bus`. Timed
completions (DMA, VI scanline) are scheduled as future events, not applied
instantaneously.

Lockstep — not catch-up — means a mid-instruction RCP event (a DP-done IRQ, an SP
halt, an AI drain) is visible to the very next CPU step. Power-on CPU/RCP phase
alignment comes from a **seeded** PRNG (ADR 0004), so the schedule is
reproducible. There are never OS threads in the core; the dedicated emulation
thread lives in the frontend (ADR 0004).

We choose the **finer** clock (VR4300) as the master so the RCP runs on an
integer-fraction accumulator. We make the timebase **fractional** (not integer
lockstep) so the AI/VI divisors, PAL timing, and DMA durations slot in without
per-quirk fudges. See `docs/scheduler.md` for the full derivation.

## Consequences

- **+** No per-quirk hacks for mid-frame coprocessor effects; the
  framebuffer-read-back and mid-display-list synchronization games rely on "just
  work" because the three engines share one timeline.
- **+** Determinism falls out: one timeline + seeded phase ⇒ bit-identical A/V
  (ADR 0004), enabling save-states, TAS, and netplay rollback.
- **−** One global run loop; the divisor table and the RCP step order must be
  exact. The 3:2 accumulator and reset-preserves-phase are pinned by unit tests.
- **−** The single-threaded RSP+CPU on one timeline is the performance bottleneck
  under LLE (`ref-docs/research-report.md` §challenge 7) — addressed by a later
  dynarec layer that keeps the interpreter as the deterministic oracle
  (`docs/performance.md`).
- The future sub-cycle bus-timing refinement (if a hard-tier ROM needs it) is a
  separate milestone (ADR 0005), not this scheduler.
