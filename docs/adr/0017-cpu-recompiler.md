# 0017 — A CPU recompiler in `rustyn64-jit`, and what it must prove before it counts

Status: **Proposed, and FAILING ITS OWN STAGE-2 GATE.** A design put up for
review whose stage-2 evidence — gathered while this file was open — says **do
not build it**. Kept and merged as the record of that, not as an approval. See
*Stage 2's verdict* below.
Recommendation: **do not write `rustyn64-jit` on the current evidence.**
Date: 2026-08-01
Deciders: repo owner
Supersedes: none · Superseded by: none
Builds on: [ADR 0011](0011-optional-fast-path-scheduler.md) / [0012](0012-amend-0011-equivalence-and-gate-witness.md) (`fast-scheduler`),
[ADR 0013](0013-fast-execution-mode.md) (`fast-exec`) — whose equivalence and
divergence machinery this reuses rather than reinventing.

## Context

The CPU is **32.29% of a frame** under `fast-exec` (`docs/performance.md`), the
largest bucket by a wide margin, and every alternative has now been sized:

| item | share | status |
| --- | --- | --- |
| **CPU bucket** (not a recompiler result) | **32.29%** | measured |
| RSP vector unit (ADR 0016, **open in #252**) | 5.3% | inferred ceiling |
| whole GPU present path | 4.0–4.4% | measured |
| async RDP (A3) | 1.7% | inferred from a measured wait |
| dirty-page RDRAM staging | 2.54% | measured, shipped |
| `read_u32` fast path | 1.32% | measured, shipped |

**Everything except the CPU, added together and assumed perfect, is about
15%** — the rows total 14.86–15.26% depending on which end of each range is
taken, so "under 15%" (an earlier draft) was wrong at the upper end. `docs/performance.md` records that 60 FPS needs 6.19x and that a perfect
fast-scheduler caps at ~2.15x; a recompiler is the only remaining change that
can move the frame rate materially rather than incrementally.

The interpreter is not slow by accident — it is a cycle-accurate five-stage
pipeline (ADR 0007), and that is the product. `fast-exec` (ADR 0013) already
established the pattern for trading that model for throughput behind a
default-off flag, and measured **1.53x**. This proposes the same trade taken
further.

## Decision

**Add `crates/rustyn64-jit`: a block-based recompiler for the VR4300 integer and
COP0/COP1 subset, behind a default-off feature, with the interpreter retained as
the oracle and the fallback.**

### It is still LLE

The recompiler executes **the same instruction stream** the interpreter does. It
gives up the *pipeline timing model*, not the instruction semantics — precisely
the trade ADR 0013 already authorized and bounded for `fast-exec`. Nothing here
is HLE, and nothing here is per-game.

### Shape

- **Block-based**, keyed on `(physical address, state key)`. Two ares mechanisms
  are worth copying and are named here so they are decided rather than
  rediscovered: **state-key block specialization** (a block compiled for
  32-bit-addressing mode is not reused in 64-bit mode) and **deferred cycle
  accounting** (charge the block's cycles once at its exit, not per instruction).

  **Deferred accounting is not free at this seam**, and a reviewer was right to
  press on it. The interface is instruction-granular — `Cpu::step_instruction_at`
  returns one instruction's cost and `Cpu::tick_at` takes the scheduler's `Count`
  position — so charging a whole block at exit delays `Count`/`Compare`,
  interrupts, exceptions and every Bus-visible effect to the block boundary. An
  implementation must define **commit points** for each externally visible event
  and terminate blocks there, gated by ADR 0013's equivalence machinery. A design
  constraint, not a detail.
- **`dynasm-rs`**, targeting **x86-64 and aarch64**. Direct assemblers are what
  working recompilers use — sljit in ares, Lightning in parallel-rsp — and a
  general codegen backend would import a compile-time cost per block that is the
  whole thing being optimized away.
- **Invalidation on the existing seam.** Self-modifying code and DMA into code
  pages invalidate blocks; the Bus already has a per-page dirty map (#245) and
  it is the natural hook. This is the part most likely to be got wrong quietly,
  which is why gate 4 below exists.
- **The interpreter stays.** It is the equivalence oracle, the fallback for any
  block the compiler declines, and the only path on targets without a backend —
  including `thumbv7em-none-eabihf`, which the chip stack must keep building for.

### `unsafe`

Executing generated code requires it. Scoped to `rustyn64-jit` alone; every
other crate's policy is untouched, including the narrow `vu.rs` exception proposed in ADR 0016 (open in #252; the
link resolves once that merges). Same `// SAFETY:`
requirement on every block or operation.

## How this ADR is accepted — and why that differs from the others

**Merging this file accepts the *design*, not the work.** Every other ADR here
records a decision already taken; this one is a plan for something large enough
that committing to it on a document would be the mistake.

It is accepted in three stages, each of which can stop:

1. **Design review** (this file). Does the shape hold?
2. **A spike, measured and thrown away.** One block shape, one benchmark, one
   number, against `frame_bench` by the standing A-B-A rules. **If it does not
   clear 1.5x on top of `fast-exec`, this ADR is superseded and the crate is not
   written.** A recompiler that wins less than the interpreter tweak already
   shipped is not worth its maintenance surface.
3. **Incremental landing** behind the flag, each slice gated as below.

## The gates

Non-negotiable, and all of them, per slice:

1. **`systemtest`: `0 failing`** on the Phase 1 categories and RSP, 90
   suite-wide — the same numbers the interpreter reports today. A recompiler
   that regresses accuracy has failed regardless of its speed.
2. **The CPU golden-log 0-diff against ares**, which is the Phase 1 exit
   criterion and the strongest oracle this project has.
3. **Equivalence with the interpreter**, reusing ADR 0013's machinery verbatim:
   architectural state compared at boundaries, stream divergence reported and
   not counted as a pass, disagreement a failure. **Do not build a second
   comparison harness.**
4. **An invalidation test that fails without invalidation.** Write to a code
   page, execute it, assert the new instruction ran. Mutation-checked by
   removing the invalidation hook. This is stated as its own gate because a
   missing invalidation produces a *correct-looking* emulator that runs stale
   code only in the games that self-modify — the exact shape of bug this
   project's engineering lessons keep describing.
5. **A-B-A on a real workload**, conservative pairing, outliers named,
   neutral-or-worse reverted.
6. **Determinism.** ADR 0004 binds the core. Either the recompiler is
   bit-identical to the interpreter, or it is a distinct mode whose output
   identity includes the mode — the position ADR 0011/0013 already take, and
   `EmuCore` already reports which mode produced a frame.

## Consequences

### Positive

- The only remaining change that can approach playable frame rates on the
  measured profile.
- The equivalence and divergence machinery already exists (ADR 0013), so the
  hardest correctness question arrives with an answer rather than a blank page.
- A recompiler is the natural home for the RSP later, and the same gates
  transfer.

### Negative / costs

- **The largest single piece of work in the project**, and the least reversible:
  a code cache, an invalidation protocol, two architecture backends and a
  register allocator are not something to half-land.
- **A second `unsafe` crate**, after `rustyn64-rdp-gpu`'s FFI and ADR 0016's
  conditional exception. The tree's `unsafe` surface would then be three
  crates — worth stating plainly, because each was individually justified and
  the total is what a reader should be shown.
- **Two implementations of the CPU that must agree forever.** The interpreter
  cannot be retired; it is the oracle. Every future CPU fix lands twice.
- **`thumbv7em-none-eabihf` gets nothing**, and must keep building. The
  interpreter is not a fallback there, it is the implementation.

### What would make this the wrong call

Stated up front so it is checkable rather than rationalized later:

- The spike measures under 1.5x. Then the maintenance surface buys less than
  `fast-exec` already did.
- Invalidation proves intractable on the existing Bus seam. A recompiler that
  needs the Bus restructured has a cost far above this estimate.
- The 32.29% turns out to be dominated by something a recompiler does not remove
  — memory access through the Bus, say, which is a separate 18.06% bucket and
  which the census in #248 showed the CPU drives. **Partially decomposed since
  this ADR was drafted; the rest is still owed** — see below.

### Update: decode is 8.1–9.4% of a frame, and a decode cache captures it

Measured after this ADR was written (`docs/performance.md`): **the CPU's
`decode` is 8.1–9.4% of a frame, roughly a quarter of its bucket.** The same
measurement on the RSP gave 0.29%, so the CPU's is ~28x more expensive.

That changes the case here in two ways, and both are recorded rather than
argued away:

**A decode cache was the obvious inference. It was built, and it is 1.0%
SLOWER** — reverted under the standing rule (`docs/performance.md`). The 8.1-9.4%
measured a decode forced through `black_box`.

**The regression is measured; the explanation is INFERRED.** That the real
`decode` is inlined into the dispatch `match`, and that a cache hit defeats that
inlining, is the reading most consistent with all three results — but **no
generated assembly was inspected**, and this ADR must not be cited as if it had
been. What is measured: the probe said 8.1-9.4%, the profiler says 4.36%, and
the cache is 1.0% slower.

So, corrected:

- **A decode cache is refuted, not deferred.** Do not rebuild it.
- **The recompiler's margin is the full 32.29%**, and its advantage is *not*
  skipping decode — the interpreter barely pays for decode as emitted. It is
  skipping the **dispatch**, the per-instruction bookkeeping and the interpreter
  loop. Stage 2's 1.5x spike is measured against 32.29%.
- **The general warning stands and is now demonstrated inside this ADR's own
  amendment**: a sizing that has not survived being built is a hypothesis. This
  one did not survive by a day.

### Stage 2's decomposition is done (`docs/performance.md`)

Profiled rather than probed, deliberately — the probe technique had just failed
on `decode` and reusing it here would have repeated the error. Source-line
attribution, because **everything inlines into `run_until_exec`** (61% self
time) and a symbol profile cannot see inside it.

**The buckets are EXCLUSIVE**, and saying so resolves a confusion an earlier
draft carried: it asked "how much of the 32.29% is Bus work" while also treating
Bus as a separate bucket. Both cannot hold. Source-line attribution assigns each
sample to the file it came from, so a sample in `bus.rs` is Bus and never CPU —
even when inlined into `run_until_exec`. Nothing is double-counted, and the CPU
bucket contains **no** Bus work.

| share | subsystem |
| --- | --- |
| 42.88% | CPU (`rustyn64-cpu` files only) |
| 23.60% | Bus + scheduler (`bus.rs`, `scheduler.rs`) |
| 10.95% | RSP |

and inside the CPU: `fastexec.rs` **16.10%**, `pipeline.rs` 8.24%, `decode.rs`
**4.36%**, `addr.rs` 3.71%, then `cop0`/`cache`/`exec`/`cop1`/`regs` at 1.6-2.5%
each.

**What a recompiler removes** is the interpreter driver — roughly `fastexec.rs`
plus `decode.rs` plus part of `pipeline.rs`, **20-25%**. What it does **not**
remove is
the Bus's 23.60% (memory the emulated program really performs), `addr.rs`
(translation still happens per access), or `cop0`/`cop1`/`cache` (real emulated
work).

**So stage 2's 1.5x spike is measured against ~20-25%.** That is the honest
target, and it is still the largest single item in the project.

`decode.rs` measuring **4.36%** here, against the probe's 8.1-9.4%, is a second
independent confirmation that the probe overstated it — the first being that the
cache was built and came out slower.

### Stage 2's verdict: the gate is not reachable

The decomposition answers stage 3 arithmetically, so the spike need not be
built. A recompiler's speedup is bounded by `1 / (1 - share removed)`:

| share removed | ceiling | assumes |
| --- | --- | --- |
| 20.46% | **1.257x** | the driver (`fastexec` + `decode`) removed entirely |
| 28.70% | **1.403x** | + all of `pipeline.rs` |
| 34.03% | 1.516x | + `addr.rs` and `regs.rs` — perfect register allocation *and* no per-access translation, at zero codegen/lookup/invalidation cost |

**1.5x requires removing 33.3% of a frame.** The realistic band is
**1.26-1.40x**, and this ADR's gate sits at the edge of a *perfect* recompiler.

So: **it does not qualify**, by the bar this ADR set for itself and for the
reason it set it — `fast-exec` already measured 1.53x, and a recompiler buying
less than that is not worth a new crate, a third `unsafe` surface, two CPU
implementations that must agree forever, an invalidation protocol, two
architecture backends, and nothing for `thumbv7em-none-eabihf`.

**This is stage 2 working.** The ADR was written so this answer could arrive
before the crate existed, and it did — for the cost of a profile.

**The gate is a judgment and it is the maintainer's to move.** 1.5x meant "beat
what `fast-exec` delivered". If the bar is instead "the largest single available
win", 1.26-1.40x is still it and nothing else is close. If the bar is 60 FPS,
this settles it the other way: the frame needs 6.19x and a perfect recompiler
contributes at most ~1.4x, which reinforces this project's standing position
that 60 FPS is unreachable.
