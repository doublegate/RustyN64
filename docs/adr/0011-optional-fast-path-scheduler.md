# 0011 — An optional, default-off fast-path scheduler, with the cycle-accurate one as the oracle

Status: **Proposed** — accepted on merge of the PR that introduces this file;
immutable thereafter.
Date: 2026-07-30
Deciders: repo owner
Supersedes: none · Superseded by: none
Amended by: [ADR 0012](0012-amend-0011-equivalence-and-gate-witness.md) — **this
document's** Decision items **1 and 4 are narrowed** there, and **3 and 6 gain
requirements** (the gate's completion witness, and the typed bail-out enumeration).
0012 numbers its own sections separately; read the two together.
Extended by: [ADR 0013](0013-fast-execution-mode.md) — which authorizes a *second*
mode whose timing model is relaxed. **Nothing in this document changes.** 0013 exists
because **this document's** Decision item **5** (*"a scheduling change, not a change
to what the CPU computes"*) excludes that relaxation in terms.
Amends: ADR 0006 (one canonical master clock), ADR 0007 (cycle-accurate VR4300
pipeline) — **without superseding either**; see *Relationship to 0006 and 0007*.

## Context

The frontend is now responsive but not fast. Two independent defects were separated
by measurement (`docs/frontend.md` §Present handoff):

- the present path took the emu mutex every UI frame — fixed in #206/#208, and
  confirmed by the user: menu latency went from 15-45 s to ~1 s;
- the core itself is far slower than real time, which this ADR is about.

### The measurements this decision rests on

All on the development machine, `--release`, Super Mario 64, `EmuCore::run_frame`
mean over 30 frames with the VI live, via
`crates/rustyn64-frontend/tests/frame_cost_probe.rs`:

| quantity | value |
| --- | --- |
| frame cost | **150.5 ms** → **6.6 FPS** |
| budget for 60 FPS | 16.7 ms |
| **required speedup** | **~9x** |
| `Bus::scanout_scaled` (VI presentation) | 35.5 ms — **23.6%** of a frame |
| latch copy/zero instructions (`perf annotate`) | **~16%** of total runtime |
| debug build, same ROM | 784.7 ms → 1.27 FPS (**7.7x** slower than release) |

`perf` (9,431 samples) attributes 63.9% to `System::step_due_here`, but that is
**inlining-confounded**: with `lto = "fat"` the whole CPU pipeline and the RSP/RDP
steps land inside it (13,317 annotated lines), so it reads as "all emulation work"
rather than as dispatch overhead.

### Why constant-factor work cannot reach 60 FPS

Three optimization attempts were made and **measured**; two were refuted by the
measurement, which is the whole reason this ADR exists rather than a pile of patches:

1. **Per-tick `u64` modulo in `next_edge_after`/`is_edge`.** Predicted to dominate
   (~6 divisions × ~2M edges/frame). `perf annotate` says divides are **under 2%**.
   Refuted before implementation.
2. **The double latch copy** (`let mut out = self.ex_dc; …; self.dc_wb = out;`).
   Not safely removable: the stages call `&mut self` methods mid-way, and
   `dc_stage`'s error branch **re-reads** `self.ex_dc` *after* `abort_with` has
   stamped the abort upstream. `Latch` is also already zero-padding-optimal — its
   fields sum to exactly its 120 bytes — so it cannot be shrunk without deleting
   information. Ceiling if perfectly eliminated: **1.19x**.
3. **Inlining the VI leaf readers** (`vi_read_cov` and friends, 21.4% combined).
   `#[inline]` was **declined** by LLVM (the symbol stayed at 13.6%), and forced
   `#[inline(always)]` made the scanout **36% worse** (35.5 → 48.4 ms). Call
   overhead is not the cost; the function bodies are. Refuted in both directions.

Even granting the two largest targets in full — latch copying (~16%) and the entire
VI scanout (23.6%) — Amdahl gives `1 / (1 - 0.40)` = **1.66x**, about 11 FPS. The
remaining ~60% is irreducible *per-cycle dispatch work*: **1.56 M CPU pipeline steps
and 1.04 M RCP steps every emulated frame**, each of which must run because the model
is defined per cycle.

**9x is therefore not reachable by optimizing within a strictly per-cycle-dispatched
model.** That is an arithmetic conclusion from measured shares, not a prediction.

### The requirement that forces the decision

The repo owner's stated target is the core running **well above 60 FPS** with the
display **locked at 60**. The display half is already satisfied — the present handoff
runs the UI at 60 Hz independently of core rate, re-presenting the newest frame. The
core half cannot be satisfied by the accurate model.

An earlier decision in this cycle was "optimize within the accurate model only; no
ADR 0006/0007 supersession". That decision and the 60 FPS requirement are **not
simultaneously satisfiable**, and this ADR resolves the conflict the only way that
keeps both the accuracy work and the performance goal: an *additional* execution
mode, off by default, with the accurate one retained as the reference.

## Decision

Add an **optional, default-off fast-path scheduler** behind the Cargo feature
**`fast-scheduler`** on `rustyn64-core`. The name is fixed here rather than deferred
to the implementing PR: this ADR is immutable once merged, so leaving it open would
make the document contradict its own status. The cycle-accurate scheduler remains the
default and becomes the **differential oracle** for the fast path.

1. **Default builds are byte-identical to today.** The feature is additive and
   off-by-default, per the additive-features rule; with it disabled the shipped
   binary must contain no behavior change whatsoever.
2. **The accurate path is the oracle, not legacy.** Every accuracy gate —
   n64-systemtest, the CPU golden-log 0-diff, the Angrylion `.rvec` conformance
   vectors, the VI vectors, the audio goldens — continues to run against the
   accurate scheduler, unchanged. The fast path never becomes the thing correctness
   is measured on.
3. **Equivalence is proven differentially, and scored from emulated STATE — not
   from pixels.** A new gate runs both schedulers over the same ROM + seed + input
   and requires equality of the **architectural state**: GPRs/`HI`/`LO`/PC, COP0 and
   the TLB, the FP register file and `FCSR`, RSP DMEM/IMEM and its vector state, RDP
   and VI register state, RDRAM contents, pending interrupts, and pending exception
   state. Framebuffer and audio equality are **supplemental** evidence, not the
   verdict: identical pixels can hide divergent state that only fails several frames
   later, which would make the gate report agreement precisely when it matters least.
   Where the two modes are permitted to differ, the difference must be enumerated in
   `docs/accuracy-ledger.md` before it ships, not discovered afterwards.
4. **Determinism is preserved per mode.** ADR 0004's contract (seed + ROM + input ⇒
   bit-identical AV) holds *within* each mode. The two modes are not required to
   agree on save-state layout; a state captured in one mode is not portable to the
   other, and the state header must record which mode produced it, so a mismatch is
   **rejected** rather than silently misinterpreted (ADR 0005).

   **Save-state compatibility, because adding that marker is a format change.** The
   header gains a scheduler-mode field behind a version bump. A state written *before*
   this change carries no marker and is therefore read as **accurate-mode** — which is
   what it is, since the fast path did not exist when it was written. Every existing
   save-state thus keeps loading in the default build, and no migration is asked of
   anyone. A state whose recorded mode does not match the running mode is refused with
   a diagnostic naming both; it is **never** loaded on the assumption that the layouts
   happen to agree. Refusing is a visible inconvenience, while loading a mismatched
   layout is silent corruption of the emulated machine.
5. **The mechanism is block-based execution, not reduced accuracy.** The fast path
   earns its speed by *not dispatching per cycle when nothing observable depends on
   the per-cycle position* — executing a run of instructions whose timing effects
   can be resolved in aggregate, and falling back to per-cycle stepping the moment
   an observable interaction is possible (an RCP event, a memory-mapped access, an
   interrupt window, a pending exception). This is how ares and CEN64 obtain their
   throughput. It is a *scheduling* change, not a change to what the CPU computes.
6. **The fast path may be incomplete, but the hand-off is exact.** It is allowed to
   bail out to the accurate scheduler for any situation it does not handle — a
   correct-but-slow fallback is always acceptable; a fast-but-wrong path is not.
   **The bailout invariant:** at every bailout boundary the accurate scheduler must
   resume with exactly the state it would have held had it executed that stretch
   itself — the same `master_ticks` (and therefore the same derived edge positions,
   ADR 0006), the same pipeline latches, the same pending events and exception state,
   and the same memory effects already applied. A fallback that lands on the right
   state at the wrong `master_ticks` is *correct-but-late*, and every timing result
   downstream of it is then wrong in a way no AV comparison would reveal. The
   differential gate must therefore **force each bailout boundary explicitly** rather
   than hoping a ROM happens to hit them. That forcing comes from *inputs*, not from
   instrumentation: fixtures — crafted machine states and instruction sequences that
   reach each boundary — driven through the ordinary public entry points. Where a
   boundary genuinely cannot be reached that way, the seam is **test-only**, behind
   `#[cfg(test)]` or a test feature, and never a hook compiled into a release build. A
   production path carrying branches that exist only for its tests is a path whose
   released behavior nobody measured.

## Relationship to 0006 and 0007

This ADR **amends** both without superseding them, and the distinction matters:

- **ADR 0006** (`master_ticks` is the only incremented counter; every other cycle
  position is derived) stays in force **unchanged in both modes**. The fast path
  still derives every position from `master_ticks`; it changes *how often the
  scheduler stops to look*, not what the clock is. The residue invariant test stays
  in the default `cargo test` path.
- **ADR 0007** (the five-stage pipeline, latches advanced in reverse order) stays the
  definition of the CPU's behavior and remains what the accurate mode executes. The
  fast path is permitted to *skip* the per-cycle latch cascade for a block when it
  can show no observer could distinguish the result — and where it cannot show that,
  it must run the cascade.

Neither ADR is retired because neither is wrong. What was wrong is the implicit
assumption that the accurate model is the *only* execution mode the project would
ever ship.

## Consequences

### Good

- 60 FPS becomes reachable at all, which it currently is not.
- The accuracy work retains its meaning: the accurate scheduler is still the thing
  every oracle measures, so no accuracy result is invalidated by this change.
- Differential testing gets stronger, not weaker — two schedulers agreeing on a
  golden output is better evidence than one implementation matching itself. Note the
  two are *not* independent implementations: the fast path reuses the same CPU/RSP/RDP
  execution code and differs only in dispatch, so their agreement rules out dispatch
  bugs and nothing more. Claiming more than that would overstate the gate.
- The fallback design means partial work ships safely.

### Bad, and accepted

- **Two execution paths to maintain**, and the classic hazard that the fast path
  silently drifts. Mitigated by the differential gate being a merge requirement, not
  an optional check.
- **Save-states are not portable between modes.** A real user-visible limitation.
- **A second scheduler is a large surface for subtle bugs**, precisely in the area
  this project has been bitten most (pipeline changes that compile, pass every test,
  and do nothing).
- CI cost rises: the differential gate runs the same work twice.

### Explicitly rejected alternatives

- *Keep optimizing the accurate model only.* Measured ceiling ~1.66x. Does not meet
  the requirement, and would consume a lot of effort producing 10% increments against
  an unreachable target.
- *Make the fast path the default and keep the accurate one for tests.* Rejected: the
  default build is what users run and what bugs get reported against, and it should be
  the mode the oracles actually validate.
- *Lower accuracy globally* (drop the pipeline, approximate timing). Rejected
  outright — it is the project's entire reason for existing (`CLAUDE.md`: LLE, never
  HLE, in the core).
- *A dynamic recompiler (JIT).* Not rejected on merit, but out of scope here: it is a
  much larger change with `unsafe`/codegen implications, and block-based interpretation
  should be measured first since it may suffice.

## Follow-up work this ADR does not decide

- The feature name, the block-boundary conditions, and the bail-out set.
- Whether the RSP and RDP get the same treatment, or only the VR4300 (the CPU is
  1.56 M of the ~2.6 M steps per frame, so it is where the leverage is).
- The VI scanout's 23.6%, which is **orthogonal** — it is presentation, not
  dispatch, and remains worth attacking in either mode. The known lead is that the
  filters re-fetch neighboring source pixels per output pixel, so a sliding-window
  reuse should cut fetch count; inlining has already been measured not to be the
  answer.
- Making a release build *convenient*, since a debug build is **7.7x slower** and that
  trap cost real diagnostic time here (a reported "~1 FPS" proved to be a debug
  binary). The fix is explicit **aliases** — `cargo full-build` / `full-run`, adopted
  from `RustyNES` — and deliberately **not** redefining the default profile in
  `.cargo/config.toml`, which would silently break ordinary debugging.
