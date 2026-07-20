# Phase 2 — RSP LLE

## Goal

The RSP in `rustyn64-rsp` executes real, game-supplied microcode under master-clock lockstep:
the Scalar Unit (a stripped-down MIPS 32-bit core) and the Vector Unit (a COP2 SIMD unit over
32 registers of 8 × 16-bit lanes), addressing the 4 KiB DMEM and 4 KiB IMEM, driven by the CPU
through the SP interface. This is the phase that makes the LLE-over-HLE decision (ADR 0002)
pay off: custom microcode runs because the instruction stream runs, not because it was
recognised.

## Exit criteria

- [ ] The SU implements its MIPS subset: no 64-bit operations, no TLB, addressing DMEM/IMEM
      only, with the RSP's own branch and delay-slot behaviour.
- [ ] The VU implements the full vector ISA over 8 lanes of 16 bits, including the 48-bit
      per-lane accumulator and its high/mid/low readback.
- [ ] The reciprocal and reciprocal-square-root ROM tables produce bit-exact results — these are
      table lookups, not computed approximations.
- [ ] The vector load/store family (`LQV`/`SQV`/`LRV`/`LPV`/`LUV`/`LHV`/`LFV`/`LTV` and their
      stores) handles element offsets and unaligned wrapping exactly.
- [ ] The SP interface registers behave: `SP_DMA_SPADDR`, `SP_DMA_RAMADDR`, `SP_DMA_RDLEN`,
      `SP_DMA_WRLEN`, `SP_STATUS`, `SP_DMA_FULL`, `SP_DMA_BUSY`, `SP_SEMAPHORE`, `SP_PC`,
      including DMA double-buffering.
- [ ] DMEM/IMEM DMA transfers to and from RDRAM are correct, including the skip/count stride
      form and the alignment rules.
- [ ] `SP_STATUS` halt, broke, and interrupt semantics drive the MI line so the CPU's polling
      loops terminate.
- [ ] The SU/VU dual-issue pipeline is modelled to the depth the test ROMs observe.
- [ ] `n64-systemtest` reports `Failed: 0` for the RSP category.
- [ ] A real graphics microcode boots and emits a plausible RDP command list into RDRAM, even
      though nothing rasterises it yet.

## Scope

In-scope:

- The SU and VU instruction sets, the register files, and the accumulator.
- DMEM/IMEM and their DMA paths.
- The SP interface registers and the halt/break/interrupt handshake.
- The RSP's step function under the scheduler, preserving the not-catch-up property: an SP event
  must be visible to the very next CPU step.

Out-of-scope:

- The RDP (Phase 3). This phase produces a command list; it does not consume one.
- Audio microcode output as *audio* (Phase 4) — the same core runs it, but the AI is not wired.
- Any HLE fast path. If a microcode is slow, it is slow; a graphics HLE may only ever exist
  later behind a default-off flag (ADR 0002).

## Sprints

- [Sprint 1 — The scalar unit, DMEM/IMEM, and the SP interface](sprint-1-scalar-sp.md) —
  the RSP boots, DMAs, halts, and interrupts correctly.
- Sprint 2 — The vector unit: ISA, accumulator, and the reciprocal tables.
  **Status:** stub — refine when Sprint 1 is close to complete.
- Sprint 3 — Vector load/store element addressing and the dual-issue pipeline.
  **Status:** stub — refine when Sprint 2 is close to complete.

## Dependencies

Phase 1 complete: the CPU must be able to write `SP_DMA_*` and clear `SP_STATUS.halt`, which is
how microcode gets uploaded and started in the first place. The Bus already exposes the narrow
`RspBus` trait.

## Risks

- **The VU is where accuracy is won or lost** — the accumulator width, the clamping rules, and
  the element-select semantics have many near-miss implementations that pass casual tests and
  fail real microcode. Mitigated by driving n64-systemtest's RSP category to zero before
  attempting any real microcode.
- **The reciprocal tables invite computation** — implementing `VRCP`/`VRSQ` arithmetically gets
  close and is wrong. Mitigated by treating them as ROM data and pinning them with a table test.
- **Lockstep can be quietly broken here** — stepping the RSP in large batches for speed would
  make SP events invisible to the next CPU step and silently violate ADR 0001. Mitigated by
  keeping the step granularity in the scheduler, never inside the chip.
- **Custom microcode is the whole point and the hardest target** — Factor 5 and Boss Game
  Studios titles are staged precisely because they break HLE. Mitigated by treating them as
  Phase 7 breadth work, not a Phase 2 gate.

## Reference docs

- [docs/rsp.md](../../docs/rsp.md) — the SU/VU spec.
- [docs/scheduler.md](../../docs/scheduler.md) — the lockstep contract.
- [docs/adr/0002-fractional-timebase-refactor.md](../../docs/adr/0002-fractional-timebase-refactor.md)
  — the LLE decision.
- `n64brew_wiki/markdown/Reality Signal Processor/CPU Core.md` — the SU and VU ISA.
- `n64brew_wiki/markdown/Reality Signal Processor/CPU Pipeline.md` — dual issue.
- `n64brew_wiki/markdown/Reality Signal Processor/Interface.md` — the SP registers.
