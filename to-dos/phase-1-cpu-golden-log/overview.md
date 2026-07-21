# Phase 1 — CPU golden log

## Goal

The NEC VR4300 core in `rustyn64-cpu` executes the MIPS III instruction set correctly —
including the 64-bit operations, the TLB and COP0 system control, the FPU (COP1), and the
documented hardware errata — and 0-diffs against a golden instruction trace captured from a
reference emulator. The N64 has no Nintendulator-style textual CPU log, so the oracle is
twofold: a captured per-instruction trace for the differ, and **n64-systemtest**, which is
self-judging and reports `Failed: 0` when the CPU categories pass.

## Exit criteria

- [x] Every MIPS III instruction implemented, including the 64-bit `D*` forms, `LL`/`SC`/
      `LLD`/`SCD`, and the unaligned `LWL`/`LWR`/`LDL`/`LDR` family.
- [x] COP0 implemented: the register file with correct 64-bit widths on `EntryHi`/`BadVAddr`,
      `Count`/`Compare` timer interrupts, and the `Status`/`Cause` exception path.
- [x] The TLB implemented: 32 dual entries, variable page sizes, ASID matching, and the
      TLB refill / invalid / modified exception vectors.
- [x] COP1 (FPU) implemented for single and double precision, including the rounding modes and
      the FCSR cause/enable/flag bits.
- [x] The exception model is exact: overflow (`ADD`/`DADD`), unaligned access, `TRAP`, `BREAK`,
      `SYSCALL`, and interrupt dispatch through IP2 from the MI.
- [x] The documented VR4300 errata are reproduced: the multiplication bug, the 32-bit
      shift-right-arithmetic bug, and the sign-extension bugs (`n64brew_wiki/markdown/VR4300.md`).
- [x] The load-delay interlock is modelled, since it is observable through the pipeline.
- [x] `n64-systemtest` reports `Failed: 0` for the **CPU, COP0, TLB and COP1** categories.
      COP1 is named explicitly because `to-dos/VERSION-PLAN.md` §v0.2.0 — which is authoritative
      for the cut — includes it, and an earlier wording here listed only the first three. That is
      the same conflation `docs/STATUS.md` once carried; the scope is the four categories, and
      the suite reports 0 across all of them.
      **Confirmed by reading the suite's source, not assumed**: `entrypoint()` calls
      `set_fcsr` — i.e. `ctc1::<31>` — as its fourth statement, so COP1 *control* access is
      required before any COP0 test runs; `main()` then immediately installs handlers at all
      three of `0x8000_0000`/`0x080`/`0x180`. Sprint 1's first real pass/fail came from
      `basic.z64` instead (5/5). Owned by T-12-007.
- [x] The golden-log differ finds no divergence across the captured trace, reporting the first
      mismatched instruction rather than a bare failure.
- [x] A determinism regression test exists: two runs from the same seed produce byte-identical
      traces (closes the ADR 0004 gap that `docs/STATUS.md` currently records as unexercised).
- [x] The scheduler counts **187.5 MHz master ticks** as the only incremented counter, with
      every other cycle position derived from it (ADR 0006), pinned by a residue invariant
      test that fails if any position becomes independently incremented.
- [x] The CPU is a **five-stage pipeline** (IC/RF/EX/DC/WB) of inter-stage latches advanced one
      PClock per step in **reverse stage order** (WB → DC → EX → RF → IC), so the DC stage is
      the point the scheduler interleaves the RCP around (ADR 0007).
- [x] `in_delay_slot` travels *with the instruction* in the latch chain, not as global CPU
      state — pinned by a test where a multi-cycle stall separates a branch from its delay slot
      and `Cause.BD` / `EPC` still come out right.
- [x] COP0 `Count` advances at **half PClock** (every 4th master tick), pinned by a test.
- [x] The documented cycle costs are encoded (`docs/cpu.md` §Cycle costs): mul/div pipeline
      stalls, FPU rates with the latency = rate + 1 rule, LDI/DCB/ITM/exception costs.
- [x] The load-delay interlock reproduces the hardware's **imprecision** — it stalls on an
      `rs`/`rt` field match whether or not the field is used as a source, exempts `$zero`, and
      does not cross the GPR/FPR boundary.

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
- [Sprint 2 — COP0, the TLB, and the exception model](sprint-2-cop0-tlb-exceptions.md) —
  gets n64-systemtest to report a genuine number, which is the first oracle this project did not
  write itself. **Status:** COMPLETE.
- Sprint 3 — COP1 (FPU), the errata, and the golden-log 0-diff.
  **Status:** COMPLETE. The FPU runs on a soft-float core (ledger C-11), the errata are
  reproduced (deviation D-2), and the golden log 0-diffs against ares over 50,027 retired
  records (C-26). Sprint 3 never got its own plan file: it was executed against the ledger and
  the two oracles rather than a ticket list, which is why this line is the record of it.

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
- **The step's internal structure is decided here, permanently** — settled by ADR 0007 before
  any interpreter code was written, which is the only reason it cost a design conversation
  rather than a rewrite. A CPU written as one indivisible `tick` per instruction cannot later
  express "a device saw the bus partway through this instruction". The residual risk is
  *erosion*: a later shortcut that completes a bus access atomically inside a stage, or adds an
  independently-incremented counter, silently reverts the decision. Mitigated by the residue
  invariant test and by the exit criteria above, not by anyone remembering.
- **`M`, the memory access time in PCycles, is undocumented** and both cache-miss formulas
  depend on it. The two most accurate N64 emulators disagree on the equivalent constant (CEN64
  charges 44 PClocks for a D-cache fill, ares 40) and neither derived it from a spec. Mitigated
  by fitting it against test ROMs and recording it in the accuracy ledger as a *measured*
  constant with its measurement cited — never tuned until a ROM passes, which would make every
  later timing result unfalsifiable.
- **Performance is unproven at this accuracy level** — no public N64 core sustains full speed
  fully cycle-accurate, and this project's goal is exactly that. Mitigated by treating the
  budget (~32 host cycles per emulated component step) as a design input from the first line:
  latches cache-resident, no allocation in `tick`, no per-cycle branching on cold conditions.
  Bounding evidence: a sibling's equivalent timebase rewrite cost **6–8%** in end-to-end frame
  time (its isolated CPU loop got ~35% faster — the cost is bus-side), so the clock model itself
  is a single-digit-percent tax rather than an order-of-magnitude one. CEN64's discouraging
  numbers come from a stalled C project on 2013-era hardware whose bus was not cycle-accurate
  anyway. Neither refutes the goal; neither establishes it.
- **n64-systemtest's `cycle` and `cop0hazard` sets are default-off upstream**, because the
  authors state the rules are not yet fully derived. Mitigated by not making them a v1.0 gate;
  the CPU/COP0/TLB categories are the bar.

## Reference docs

- [docs/cpu.md](../../docs/cpu.md) — the register, mode, and timing spec.
- [docs/scheduler.md](../../docs/scheduler.md) — how CPU cycles drive the master clock.
- [docs/testing-strategy.md](../../docs/testing-strategy.md) — Layers 2 and 3, the differ and
  the ROM corpus.
- [docs/adr/0004-determinism-contract.md](../../docs/adr/0004-determinism-contract.md)
- `n64brew_wiki/markdown/VR4300.md` — microarchitecture, the load-delay interlock, the errata.
- `n64brew_wiki/markdown/MIPS R4300.md`, `SysAD Interface.md` — the bus interface.
