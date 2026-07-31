# 0013 — A fast execution mode: instruction-granular timing, authorized separately from 0011

Status: **Proposed** — accepted on merge of the PR that introduces this file;
immutable thereafter.
Date: 2026-07-30
Deciders: repo owner
Supersedes: none · Superseded by: none
Extends: [ADR 0011](0011-optional-fast-path-scheduler.md) (optional fast-path
scheduler) and [ADR 0012](0012-amend-0011-equivalence-and-gate-witness.md) — both
stand in full. This ADR authorizes a relaxation that 0011's Decision item **5**
excludes in terms; it does not reopen anything 0011 decided.
Amends: [ADR 0006](0006-one-canonical-master-clock.md) (one canonical master clock)
and [ADR 0007](0007-cycle-accurate-vr4300-pipeline.md) (cycle-accurate VR4300
pipeline) — **for the fast mode only, without superseding either**; see
*Relationship to 0006, 0007 and 0011*.

## Context

### The optimization work 0011 anticipated is finished, and it fell short

ADR 0011 was written on the premise that a block-based *scheduling* change could
reach the target. That premise has now been tested to exhaustion. Eleven merged
PRs (#219–#229) took a rendering frame from **155.13 ms to 93.06 ms — 1.67x, 6.45
to 10.75 FPS** — and closed every remaining avenue that is not architectural:

| axis | result | recorded in |
| --- | --- | --- |
| structural waste | **exhausted** — every profile bucket at or above 3% examined line by line | `docs/performance.md` §*The search for structural waste is exhausted* |
| `-C target-cpu=native` | **neutral** (A-B-A; the return leg came in below both B legs) | §*Ruled out* 5b |
| profile-guided optimization | **4.96% slower**, legs non-overlapping | §*Ruled out* 5c |
| the `fast-scheduler` periodic-edge block | **+5.4%**, merged, opt-in, tick-identical | ADR 0011, `docs/scheduler.md` |

Against a 16.67 ms budget, 93.06 ms still needs **5.58x**. `docs/performance.md`
§*The 60 FPS target is out of reach for this execution model* computes the ceiling
from measured shares: the CPU pipeline and the scheduler together are **53.5%** of a
frame, so setting **both to zero** caps the accurate model at **2.15x**. That bound
was computed on the then-current 103.3 ms frame; the frame has since shrunk and the
required speedup with it, and the conclusion does not move — 5.58x against a 2.15x
ceiling is the same verdict as 6.19x against it. (Recording the drift rather than
quietly restating it follows ADR 0012's *"the decision does not move"* note; the
reverse would have invalidated this ADR before it was written.)

Optimizing *when* work happens is therefore finished. What is left changes **what is
computed**.

### ADR 0011 does not authorize that, and is not silent about it

0011's Decision item 5 says, in full:

> **The mechanism is block-based execution, not reduced accuracy.** […] This is a
> *scheduling* change, not a change to what the CPU computes.

Charging an instruction its documented issue cost instead of walking five pipeline
stages **is** a change to the timing model. It is not covered by 0011, and 0011 is
immutable, so it cannot be edited to cover it. Its rejected-alternatives section
draws the same line from the other side: a dynamic recompiler was *"not rejected on
merit, but out of scope here"*, with block-based interpretation to be measured first
**since it may suffice**. It has now been measured, and it does not suffice.

This ADR exists so that the relaxation is authorized explicitly rather than inferred
from a boundary that 0011 states rather than omits.

### What is not being relaxed

Stating this first, because the phrase "fast mode" invites the wrong reading:

- **A recompiler is not HLE.** Recompiling or reordering the *execution* of the
  VR4300's own instruction stream still executes that instruction stream. LLE is
  retained everywhere. There is **no per-game HLE microcode** anywhere in this
  decision, and `CLAUDE.md`'s *LLE, never HLE, in the core* is untouched.
- **What is given up is the VR4300 pipeline *timing model*** — the per-cycle latch
  cascade — and nothing else about what the CPU computes.
- The relaxation is **opt-in and default-off**, and the cycle-accurate path is kept,
  maintained, and remains the oracle.

## Decision

### 1. A second execution mode, behind one default-off feature named `fast-exec`

`rustyn64-core` gains the Cargo feature **`fast-exec`**. The name is fixed here for
the same reason 0011 fixed `fast-scheduler`: this document is immutable on merge, so
deferring the name would make it contradict its own status.

`fast-exec` and `fast-scheduler` are **independent features**; neither implies the
other. They are different kinds of change and must stay separable:

- `fast-scheduler` is **tick-identical** — a different enumeration of the same edges,
  gradeable by whole-state equality;
- `fast-exec` is **not** tick-identical, and is graded by the split predicate in
  item 4 below.

Where both are enabled, **`fast-exec`'s scheduler is the one that runs.** That
precedence is settled here so it is not re-decided in each implementing PR, and so
that "both features on" is a defined configuration rather than an accident. `--all-features`
remains forbidden in this workspace; each feature carries its own CI entries.

### 2. The sanctioned relaxation is exactly one thing, and it is cited, not invented

In `fast-exec`, the CPU may charge **instruction-granular issue costs** — one cycle
per instruction plus a documented per-class cost, plus cache-miss penalties — instead
of advancing the five-stage cascade per cycle.

The cost table is **already in this repository and sourced**, in
[`ref-docs/2026-07-20-vr4300-timing-supplement.md`](../../ref-docs/2026-07-20-vr4300-timing-supplement.md)
§3: `MULT` 5, `DIV` 37, `DMULT` 8, `DDIV` 69, FPU add 3, FPU mul 5/8, FPU div and
sqrt 29/58, LDI 1, DCB 1, ITM 3, D-cache miss 8–9 + M, I-cache miss 14–15 + M — each
row carrying its NEC User's Manual table reference. This matters because the
project's rule is *never invent a value the documentation does not give*, and a fast
timing model is precisely where fitted constants would otherwise arrive.

Two things carry forward from §3 rather than being laundered by restatement:

- §3's own **caveat on the 1-cycle baseline** — it is a *latency* claim (UM §7.5.6)
  used here as an *issue* cost, defensible against §4.1's throughput model but an
  inference rather than a direct citation. It stays labeled as one.
- **`M` is measured, never tuned** (`docs/accuracy-ledger.md`). `M(RCP-reg) = 22` is
  measured; the RDRAM regimes are a documented two-regime model. `fast-exec` may not
  adjust any of them to make a frame rate or a ROM come out right. A constant tuned
  to a result makes every later result built on it stop being evidence.

**Nothing else is relaxed.** The instruction stream, the architectural results of
every instruction, exception and interrupt semantics, the memory model, the softfloat
core, LLE for the RSP and RDP, and ADR 0004's determinism contract *within* the mode
all hold unchanged.

### 3. The accurate path remains the oracle, and that is not weakened by this ADR

0011's Decision item 2 is carried forward in force: every accuracy gate —
n64-systemtest, the CPU golden-log 0-diff, the Angrylion `.rvec` vectors, the VI
vectors, the audio goldens — runs against the **accurate** path, unchanged. The fast
mode never becomes what correctness is measured on. A gate result obtained on the
fast path is not an accuracy result and must not be reported as one.

### 4. The comparison predicate splits, and the timing-derived state is carved out

0011 item 3 requires equality of architectural state, and 0012 item 2 requires the
gate to witness its own completion. Both hold. What changes is that whole-state
equality **cannot be the fast mode's verdict**, because the fast mode is deliberately
not `master_ticks`-identical. So:

- **Architectural equality is required at instruction-retirement boundaries** — 0011
  item 3's list: GPRs/`HI`/`LO`/PC, COP0 and the TLB, the FP register file and
  `FCSR`, RSP DMEM/IMEM and vector state, RDP and VI register state, RDRAM, pending
  interrupts, pending exception state.
- **Timing divergence is measured, bounded, and reported** — not required to be zero.
  The bound is recorded in `docs/accuracy-ledger.md` **before it ships**, per 0011
  item 3's enumeration requirement. A divergence that grows without bound is a
  failure even when every architectural comparison passes.
- **The timing-derived architectural registers are excluded from the equality
  predicate and included in the divergence measurement instead.** COP0 `Count` is the
  concrete case: it is architectural state a program can read with `MFC0`, and if
  timing differs then `Count` differs, so demanding equality on it would demand the
  very thing this mode gives up.

That carve-out has a consequence worth naming here rather than discovering in a
failing suite: **a program that branches on a timing-derived value can take a
different path in the two modes, so the instruction streams themselves diverge.**
That is a legitimate outcome of this decision, not a defect. The gate must *detect
and report* it and **end the comparison for that fixture**, naming the point of
divergence. What it must never do is absorb it silently, which would turn every
subsequent "agreement" into a comparison of two unrelated runs.

`fast-scheduler`'s existing whole-state tests keep their stricter predicate. They are
grading a tick-identical path and would be weakened for nothing by relaxing them.

### 5. `unsafe` stays out of the chip crates and out of `-core`

If this work later needs host code generation or explicit SIMD, both of which require
`unsafe`, it lives in a **new crate that permits it** — `rustyn64-jit` — and nothing
else moves. Every chip crate and `rustyn64-core` keep `#![forbid(unsafe_code)]`
exactly as they carry it today. No `unsafe` enters `fast-exec`'s scope by any other
route, and none is authorized by this ADR outside that crate.

### 6. ADR 0012's gate machinery is a prerequisite of this mode, not an extension of it

0012's apparatus — one typed bail-out enum returned through one signature, a
variant-coverage witness **derived from the enum rather than written as a literal**,
per-fixture and suite timeouts, abnormal termination as a gate failure rather than an
uncounted exit, and suite-wide *"the fast path never engaged"* / *"no boundary was
reached"* failures — **does not exist yet**. It was never needed, because the current
fast path bails out nowhere.

It must land **before** any backend that can genuinely bail. Building the gate after
the thing it grades produces a gate that has never been observed to fail, which this
project has already paid for twice (0012's *Context*).

## Relationship to 0006, 0007 and 0011

- **ADR 0007** remains the definition of the CPU's behavior and is what the accurate
  mode executes. 0011 already permitted the fast path to skip the cascade *"when it
  can show no observer could distinguish the result"*. **This ADR goes further**: in
  `fast-exec` the cascade is skipped **without that showing**, in exchange for a
  measured and bounded divergence. That single sentence is the whole difference
  between 0011 and 0013, and the reason 0011 could not have covered it.
- **ADR 0006** stays in force **unchanged in the accurate mode**, and the residue
  invariant test stays in the default `cargo test` path.

  In `fast-exec` it does not, and that must be said plainly rather than argued
  around. A per-domain deficit counter — the mechanism this mode is expected to
  adopt — **is** an independently advanced position, which 0006's *"`master_ticks` is
  the only counter that is ever incremented"* forbids outright, and which
  `CLAUDE.md` restates as a hard rule. This is the **second** sanctioned relaxation
  in this ADR, and it is named here precisely because it would otherwise arrive as a
  reviewer's objection to an already-written PR.

  Two things bound it. `master_ticks` remains the mode's **single reported
  timebase** — what save-states record, what the divergence measurement is expressed
  in, and what anything user-visible reads. And each domain's deficit is
  **reconciled to it at every synchronization point**, so the counters are budgets
  drawn against the canonical clock rather than rival clocks. A deficit that is
  never reconciled is a rival clock, and would be a defect under this ADR, not a
  permitted consequence of it.
- **ADR 0011 and 0012** are unchanged in every respect. This ADR adds a mode
  alongside the one they describe; it does not alter the fast-path scheduler, its
  gate, or its promotion criteria.

Neither 0006 nor 0007 is retired, because neither is wrong. Both remain exactly true
of the mode the project ships by default and validates against.

## Consequences

### Good

- The target becomes reachable at all. Every non-architectural lever is now measured
  and spent; without this decision the honest statement is that the project stops at
  ~10.75 FPS.
- The accuracy work keeps its meaning in full. The oracles still measure the accurate
  path, so no accuracy result is invalidated by anything authorized here.
- The relaxation is **enumerated**. A single named change to the timing model, cited
  to a table already in the repository, is auditable in a way that "make it faster"
  never is.
- Naming the 0006 conflict up front converts the most likely late objection into a
  decided question.

### Bad, and accepted

- **A third execution configuration.** Default, `fast-scheduler`, and `fast-exec`,
  each needing CI entries, and `--all-features` unavailable to cover them cheaply.
- **The fast mode's results are not comparable to the accurate mode's timing.** Any
  measurement taken in it must say so, or it will eventually be quoted as an accuracy
  figure.
- **The divergence bound is a maintenance obligation**, not a one-time measurement:
  it must be re-established whenever the cost model changes.
- **Instruction streams may legitimately diverge**, which makes some fixtures
  uncomparable past a point. The gate reports that rather than hiding it, but it does
  mean coverage is not uniform across a long run.
- The pipeline is where this project has been bitten most — changes that compile,
  pass every test, and do nothing. A second CPU timing path is more of that surface.

### Explicitly rejected alternatives

- *Do this under ADR 0011.* Rejected: 0011 item 5 excludes it in terms. Proceeding
  anyway would mean citing an ADR for permission it explicitly withholds, which
  corrupts the one mechanism this project uses to make decisions checkable later.
- *Supersede ADR 0007.* Rejected: 0007 is not wrong, and the accurate mode executes
  it. Superseding it would retire the oracle to authorize an opt-in mode.
- *Make instruction-granular timing the default.* Rejected for 0011 item 2's
  reasoning, unchanged: the default build is what users run and what bugs are
  reported against, and it should be the mode the oracles validate.
- *Lower accuracy globally, or adopt per-game HLE microcode.* Rejected outright —
  the project's reason for existing.
- *Tune the cost table until frame rates or ROMs come out right.* Rejected: it would
  make every downstream timing result unfalsifiable. Constants are cited or measured.

## Follow-up work this ADR does not decide

- Whether a dynamic recompiler is built at all. It is re-scoped, not scheduled: the
  measurement that would justify it comes **after** instruction-granular timing and
  the scheduler change, not before.
- The RSP's execution strategy and where its SIMD, if any, is allowed to live.
- A GPU-backed RDP, which is orthogonal to everything above and would carry its own
  licensing and sync decisions.
- **Promotion.** Shipping `fast-exec` in a release artifact, or defaulting it, is a
  separate decision requiring evidence that does not exist yet, exactly as 0011
  requires for `fast-scheduler`.
