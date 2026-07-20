# Phase 1 — CPU golden log

## Goal

The NEC VR4300 core in `rustyn64-cpu` executes the MIPS III instruction set correctly —
including the 64-bit operations, the TLB and COP0 system control, the FPU (COP1), and the
documented hardware errata — and 0-diffs against a golden instruction trace captured from a
reference emulator. The N64 has no Nintendulator-style textual CPU log, so the oracle is
twofold: a captured per-instruction trace for the differ, and **n64-systemtest**, which is
self-judging and reports `Failed: 0` when the CPU categories pass.

## Exit criteria

- [ ] Every MIPS III instruction implemented, including the 64-bit `D*` forms, `LL`/`SC`/
      `LLD`/`SCD`, and the unaligned `LWL`/`LWR`/`LDL`/`LDR` family.
- [ ] COP0 implemented: the register file with correct 64-bit widths on `EntryHi`/`BadVAddr`,
      `Count`/`Compare` timer interrupts, and the `Status`/`Cause` exception path.
- [ ] The TLB implemented: 32 dual entries, variable page sizes, ASID matching, and the
      TLB refill / invalid / modified exception vectors.
- [ ] COP1 (FPU) implemented for single and double precision, including the rounding modes and
      the FCSR cause/enable/flag bits.
- [ ] The exception model is exact: overflow (`ADD`/`DADD`), unaligned access, `TRAP`, `BREAK`,
      `SYSCALL`, and interrupt dispatch through IP2 from the MI.
- [ ] The documented VR4300 errata are reproduced: the multiplication bug, the 32-bit
      shift-right-arithmetic bug, and the sign-extension bugs (`n64brew_wiki/markdown/VR4300.md`).
- [ ] The load-delay interlock is modelled, since it is observable through the pipeline.
- [ ] `n64-systemtest` reports `Failed: 0` for the CPU, COP0, and TLB categories.
- [ ] The golden-log differ finds no divergence across the captured trace, reporting the first
      mismatched instruction rather than a bare failure.
- [ ] A determinism regression test exists: two runs from the same seed produce byte-identical
      traces (closes the ADR 0004 gap that `docs/STATUS.md` currently records as unexercised).
- [ ] An instruction's memory access is a point the scheduler can interleave around, not an
      opaque interior step, and `Bus::poll_irq_at_phase` reaches at least one branch that
      behaves differently per `BusPhase` — pinned by a test that fails if `Command` and `Data`
      are made to behave identically. See the risk below for why this is an exit criterion
      rather than later work.

## Scope

In-scope:

- The VR4300 register file, the 64-bit datapath, and the delay-slot semantics.
- Instruction decode and execute, COP0, the TLB, and COP1.
- The exception and interrupt model, driven by the MI interrupt lines the Bus already owns.
- The cycle cost per instruction, fed back into the master-clock scheduler.
- The I/D caches only to the depth the test ROMs observe (see the open question below).

Out-of-scope:

- The RSP and the RDP (Phases 2 and 3); the CPU merely writes their registers through the Bus.
- PI/SI DMA and the CIC boot handshake (Phase 5). Phase 1 test ROMs are loaded directly, with
  the boot sequence stubbed.
- Any rendering: the VI is not scanned out yet, so pass/fail comes from the result protocol and
  the trace, never from an image.

## Sprints

- [Sprint 1 — Register file, decode, and the integer core](sprint-1-integer-core.md) —
  the datapath to first-pass completeness against the simplest test ROMs.
- Sprint 2 — COP0, the TLB, and the exception model.
  **Status:** stub — refine when Sprint 1 is close to complete.
- Sprint 3 — COP1 (FPU), the errata, and the golden-log 0-diff.
  **Status:** stub — refine when Sprint 2 is close to complete.

## Dependencies

Phase 0 complete. Specifically: the Bus and its `CpuBus` trait, the scheduler's `tick_one_unit`,
and the harness's golden-log differ, all of which exist. The golden trace itself is the deferred
Phase 0 criterion (T-02-005) and must land before the 0-diff gate can be met.

## Risks

- **The golden trace does not exist yet** — the differ is written against a file contract whose
  producer is not. Mitigated by n64-systemtest being self-judging: the CPU categories can be
  driven to `Failed: 0` without any trace, and the trace then becomes a regression net rather
  than the primary gate.
- **Cache-coherency depth is an open question** — modelling the I/D caches exactly is expensive
  and may not be observable. Mitigated by implementing to the depth the test ROMs actually
  detect and recording the decision, rather than guessing at a level up front.
- **Errata are easy to "fix"** — the multiplication and shift-right-arithmetic bugs look like
  implementation mistakes and invite correction. Mitigated by pinning each with a named test
  citing the wiki, so removing the bug fails the suite.
- **Delay slots and the load interlock leak into every instruction** — retrofitting them after
  the fact is a rewrite. Mitigated by building them into the step function in Sprint 1, before
  the instruction count grows.
- **The step's internal structure is decided here, permanently** — this is the highest-leverage
  and least-reversible decision in the phase. A CPU written as one indivisible `tick` per
  instruction cannot later express "the memory access happened partway through, and a device saw
  the bus in between"; adding that distinction afterwards means rewriting the scheduler and every
  chip's step contract simultaneously, with accuracy work already standing on the old shape. A
  sibling project reached exactly that point and paid for it. Note this is *not* the ADR 0005
  sub-cycle refactor, which is genuinely deferrable — this is the coarser question of whether the
  access is an addressable point at all, and it is not deferrable. `BusPhase` already models the
  right hardware fact (`SYSCMD` multiplexes command and data on the same `SysAD` lines) but is
  inert plumbing today: `poll_irq_at_phase` ignores its argument in both the trait default and
  the `rustyn64-core` impl. Mitigated by the exit criterion above, and by
  `docs/engineering-lessons.md` §1.2 / §3.2.

## Reference docs

- [docs/cpu.md](../../docs/cpu.md) — the register, mode, and timing spec.
- [docs/scheduler.md](../../docs/scheduler.md) — how CPU cycles drive the master clock.
- [docs/testing-strategy.md](../../docs/testing-strategy.md) — Layers 2 and 3, the differ and
  the ROM corpus.
- [docs/adr/0004-determinism-contract.md](../../docs/adr/0004-determinism-contract.md)
- `n64brew_wiki/markdown/VR4300.md` — microarchitecture, the load-delay interlock, the errata.
- `n64brew_wiki/markdown/MIPS R4300.md`, `SysAD Interface.md` — the bus interface.
