# Phase 2 — RSP LLE

## Goal

The RSP in `rustyn64-rsp` executes real, game-supplied microcode under master-clock lockstep:
the Scalar Unit (a stripped-down MIPS 32-bit core) and the Vector Unit (a COP2 SIMD unit over
32 registers of 8 × 16-bit lanes), addressing the 4 KiB DMEM and 4 KiB IMEM, driven by the CPU
through the SP interface. This is the phase that makes the LLE-over-HLE decision (ADR 0002)
pay off: custom microcode runs because the instruction stream runs, not because it was
recognised.

**Status: COMPLETE (v0.3.0 "Microcode", 2026-07-22).** Both exit criteria are met by oracle: the
RSP category reports `n64-systemtest Failed: 0`, and libdragon's real `rdpq` microcode boots on
the RSP and emits an RDP command list witnessed byte-for-byte through the DPC seam. The one box
left unchecked below — SU/VU dual-issue pipeline timing — is an explicit, ledgered accuracy
deferral (unobserved by any test ROM, not part of the cut), not an oversight.

## Exit criteria

- [x] The SU implements its MIPS subset: no 64-bit operations, no TLB, addressing DMEM/IMEM
      only, with the RSP's own branch and delay-slot behaviour.
- [x] The VU implements the full vector ISA over 8 lanes of 16 bits, including the 48-bit
      per-lane accumulator and its high/mid/low readback.
- [x] The reciprocal and reciprocal-square-root ROM tables produce bit-exact results — these are
      table lookups, not computed approximations.
- [x] The vector load/store family (`LQV`/`SQV`/`LRV`/`LPV`/`LUV`/`LHV`/`LFV`/`LTV` and their
      stores) handles element offsets and unaligned wrapping exactly.
- [x] The SP interface registers behave: `SP_DMA_SPADDR`, `SP_DMA_RAMADDR`, `SP_DMA_RDLEN`,
      `SP_DMA_WRLEN`, `SP_STATUS`, `SP_DMA_FULL`, `SP_DMA_BUSY`, `SP_SEMAPHORE`, `SP_PC`,
      including DMA double-buffering.
- [x] DMEM/IMEM DMA transfers to and from RDRAM are correct, including the skip/count stride
      form and the alignment rules.
- [x] `SP_STATUS` halt, broke, and interrupt semantics drive the MI line so the CPU's polling
      loops terminate.
- [ ] The SU/VU dual-issue pipeline is modelled to the depth the test ROMs observe.
      **Made falsifiable (this was previously unmeasurable as written).** N64brew *RSP CPU
      Pipeline* states its scope up front: it *"describes the effects of the pipelines on code
      execution"* from a software point of view. Those effects are **entirely timing** — the
      4-cycle VU write latency, the 3-cycle DMEM-load latency, and the stalls they cause. None
      of them changes an architectural result: the same registers end up holding the same
      values, only later.
      **The distinction that keeps this honest:** the pipeline timing itself is *unmeasured and
      unmodelled* — this box does not claim otherwise, and stays unchecked. What **is** measured
      is a separate, weaker fact about the instrument: the RSP exposes no cycle counter to
      software, and a `grep` for `dual`/`cycles`/`stall` across
      `ref-proj/n64-systemtest/src/tests/rsp/` returns **nothing**, so the *test ROMs observe a
      depth of zero*. That is a claim about the suite, not about the hardware, and the two must
      not be conflated: "nothing observes it" is not "its value is 0".
      So the criterion as phrased (*"to the depth the test ROMs observe"*) has no work to do
      today — there is no observed behaviour to model against — but it is **not validated** and
      the box stays open. It should be checked only when a real measurement or an explicit
      timing model exists. Two triggers create that, whichever comes first: (a) a real
      microcode's *output* depends on when the RSP finishes relative to the CPU — an `SP_STATUS`
      polling loop makes RSP timing indirectly visible; (b) a cycle-counting harness exists to
      compare against. Recording the zero-observation measurement is what makes those triggers
      checkable rather than a matter of opinion.
- [x] `n64-systemtest` reports `Failed: 0` for the RSP category.
- [x] A real graphics microcode boots and emits a plausible RDP command list into RDRAM, even
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
      **A byte-for-byte comparison is only meaningful if both harnesses execute the same thing**,
      so the oracle must be fully specified before it is run — otherwise "identical" compares
      two different executions and proves nothing. The fixture must pin: the initial DMEM, IMEM,
      RDRAM and SP-register state (a fixed power-on image plus the microcode DMA'd to a stated
      IMEM address); the microcode **entry point** written to `SP_PC`; the RDRAM **base** the
      command list is emitted to and the **exact byte length** compared (from the microcode's
      own output pointer, not a guess); and the **completion condition** — the `BREAK` that halts
      the RSP, at which point the byte range is frozen. Determinism is already the project
      contract (ADR 0004), so given identical inputs the two harnesses either agree completely
      or the difference is a real bug; an under-specified fixture forfeits that guarantee.

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
  the RSP boots, DMAs, halts, and interrupts correctly. **Status:** done.
- Sprint 2 — The vector unit: ISA, accumulator, and the reciprocal tables.
  **Status:** done — the full VU (multiplies, accumulating forms, compares, the
  clip compares, `VRND`/`VMULQ`/`VMACQ`, the reciprocals, the reserved opcodes)
  landed in PRs #41–#42. No separate sprint doc; the work was tracked by PR.
- Sprint 3 — Vector load/store element addressing and the dual-issue pipeline.
  **Status:** load/store **done** (#41). Dual-issue timing is **not part of the
  v0.3.0 cut criterion** — the criterion is "observed depth zero" (#40: there is
  no cycle counter, and n64-systemtest asserts no dual-issue timing), so it does
  not block the release — but it remains an **open accuracy item**, deferred to a
  later phase and consistent with the pipeline-timing limitations noted above. It
  is not done; it is out of scope for the cut.
- [Sprint 4 — Booting a real graphics microcode](sprint-4-microcode-boot.md) —
  Phase 2's **second** exit criterion (the first, RSP category `Failed: 0`, is
  met). Boots libdragon's real `rdpq` on the RSP and byte-compares the emitted
  RDP command list against a hardware-doc-derived golden. Design: ADR 0008.
  **Status:** COMPLETE — the real `rdpq` microcode boots and emits an RDP command list, witnessed
  byte-for-byte (T-24-001…004). The `mips64-elf` toolchain blocker was resolved.

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
