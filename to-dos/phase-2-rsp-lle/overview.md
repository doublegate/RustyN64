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
      **Made falsifiable (this was previously unmeasurable as written).** N64brew *RSP CPU
      Pipeline* states its scope up front: it *"describes the effects of the pipelines on code
      execution"* from a software point of view. Those effects are **entirely timing** — the
      4-cycle VU write latency, the 3-cycle DMEM-load latency, and the stalls they cause. None
      of them changes an architectural result: the same registers end up holding the same
      values, only later.
      The RSP exposes no cycle counter to software, and a `grep` for `dual`/`cycles`/`stall`
      across `ref-proj/n64-systemtest/src/tests/rsp/` returns **nothing** — the suite contains
      no RSP timing assertion of any kind. So *"the depth the test ROMs observe"* is currently
      **zero**, and this criterion is met by that measurement rather than by modelling.
      **Bounded, not dismissed.** It stops being zero the moment either of these is true, and
      whichever comes first should reopen this box: (a) a real microcode's *output* depends on
      when the RSP finishes relative to the CPU — an `SP_STATUS` polling loop makes RSP timing
      indirectly visible; (b) a cycle-counting harness exists to compare against. Recording the
      measurement is what makes the reopening trigger checkable instead of a matter of opinion.
- [ ] `n64-systemtest` reports `Failed: 0` for the RSP category.
- [ ] A real graphics microcode boots and emits a plausible RDP command list into RDRAM, even
      though nothing rasterises it yet.
      **Source resolved: libdragon's `src/rdpq/rsp_rdpq.S`.** libdragon is released into the
      **public domain** (`ref-proj/libdragon/LICENSE.md`, Unlicense) and is already on
      `ref-proj/README.md`'s vendorable list, so its RDP-queue microcode can be built and
      committed as a test fixture. That removes the licence obstacle that made this criterion
      look open-ended — F3DEX2 from a commercial ROM is *not* available to us and never was.
      **"Plausible" still needs defining, and should be defined before the work starts**, or it
      becomes a judgement call made by whoever happens to be looking at the output. The
      proposal: the emitted command list is compared byte-for-byte against the same microcode
      run under a reference emulator, exactly as the CPU golden log is (ledger C-26). That
      turns "plausible" into "identical to an oracle", which is the standard the rest of this
      project holds itself to. Anything weaker — eyeballing a command stream for
      reasonable-looking opcodes — would be the only unfalsifiable criterion in the phase.

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
  make SP events invisible to the next CPU step and silently violate the ADR 0006 lockstep contract. Mitigated by
  keeping the step granularity in the scheduler, never inside the chip.
- **Custom microcode is the whole point and the hardest target** — Factor 5 and Boss Game
  Studios titles are staged precisely because they break HLE. Mitigated by treating them as
  Phase 7 breadth work, not a Phase 2 gate.

## Reference docs

- [docs/rsp.md](../../docs/rsp.md) — the SU/VU spec.
- [docs/scheduler.md](../../docs/scheduler.md) — the lockstep contract.
- [docs/adr/0002-lle-coprocessors.md](../../docs/adr/0002-lle-coprocessors.md)
  — the LLE decision.
- `n64brew_wiki/markdown/Reality Signal Processor/CPU Core.md` — the SU and VU ISA.
- `n64brew_wiki/markdown/Reality Signal Processor/CPU Pipeline.md` — dual issue.
- `n64brew_wiki/markdown/Reality Signal Processor/Interface.md` — the SP registers.
