# Engineering lessons carried into RustyN64

Two cycle-accurate emulators were built before this one — RustyNES and RustySNES — and both
accumulated hard-won knowledge about *how this kind of project goes wrong*. This document
distills that into practices adopted here, before the equivalent mistakes get a chance to happen.

**This is not a summary of those projects.** Their console-specific findings do not transfer: a
PPU sprite-evaluation quirk or a coprocessor tier matrix says nothing about the RSP. What
transfers is the **shape of the failure** — how a bug hid, why a test did not catch it, which
plausible theory wasted the most time. Each entry states the pattern generally, then what it means
for this machine, then the practice adopted here.

The ordering is by **when the lesson has to be acted on**, because that is the only property that
matters. Part 1 is structural: those decisions are cheap now and expensive-to-impossible later.
Part 2 concerns the oracle — what "green" is allowed to mean. Part 3 is debugging discipline,
relevant continuously.

Where a lesson has already changed something here, the change is cited. Where it is advice for a
future phase, it is also filed as a risk in that phase's overview, because a lessons document that
lives only in `docs/` is a document nobody reads at the moment it would have helped.

---

## Part 1 — Structural decisions, cheapest before any chip executes an instruction

### 1.1 One counter is incremented; every other position is derived from it

**The pattern.** The problem is not having several cycle counters — it is having
several counters that are each *incremented*. They then agree only because every call site
remembers to step them in matching amounts: an invariant held by construction rather than by
derivation. It is correct until one path forgets, and the resulting desync is invisible until
it manifests as a timing bug somewhere else entirely.

**How it went wrong before.** A sibling project shipped a scheduler with five independently
incremented counters (CPU, PPU, APU, bus, master). Its own ADR describes them as kept in sync
"by construction... not by derivation — a correct but fragile invariant". Collapsing them into
one canonical counter took a full scheduler rewrite, preceded by 17+ failed point-fixes and
dozens of audit documents, plus a save-state/movie format epoch break because the change
arrived after those formats had shipped.

Two results from that rewrite are worth carrying separately. First, the accuracy payoff was
real: 100% versus 94.24% on their oracle. Second, and more surprising, **the one-clock model
ended up ~9% faster than the design it replaced** once ordinary optimisation was applied — the
first cut was ~10% slower, and the whole deficit plus more came back from unrelated tuning. The
throughput objection to a canonical master clock does not survive contact with that data, which
matters because that objection is the usual reason projects avoid the design.

**What it means here.** ADR 0006 makes the 187.5 MHz tick the only incremented counter, with
`cpu_cycles()` and `rcp_cycles()` as derived accessors rather than fields. We got this for free:
the sibling paid a format break because their change came after shipping save states, and we
have none. This was the cheapest this decision will ever be, and it is now spent.

**Practice adopted.**

- No `+= 1` on any cycle position in the core except `master_ticks`. A new `cycles`/`ticks`
  field on a chip struct is a design smell requiring justification in review. The one legitimate
  use is a *retired-work* counter that nothing schedules against.
- Pin the derivation with a **residue invariant test**: sample the affine offsets between the
  canonical counter and every derived position at frame boundaries, and assert they never move
  after the first boundary. An independently-incremented counter fails this on the first frame
  where any path forgets to step it. Keep it in the default test path, never behind a feature.
- Prefer an integer divisor to an accumulator wherever the domain genuinely is a rational
  multiple of the master, and use an accumulator *only* where it genuinely is not (VI, AI). The
  split should carry information about the hardware, not about implementation convenience.

### 1.2 The bus-access split has to exist before the CPU is written

**The pattern.** A CPU written as one indivisible `tick` per instruction cannot later express "the
memory access happened partway through, and a device saw the bus in between". Retrofitting that
distinction means rewriting the scheduler and every chip's step contract at once — which is a
different and much larger change than it appears from the outside, and it lands after there is
already accuracy work standing on the old shape.

**How it went wrong before.** A sibling reached a residual class of timing failures that could only
be resolved by splitting each instruction into a begin/access/end structure, and by then the
change touched every consumer. The pre-work was the expensive part; the insight itself was cheap.

**What it means here.** This was the single highest-leverage item in this document, and it was
caught with Phase 1 not yet started — which is the only reason it cost a design conversation
rather than a rewrite. The skeleton had `Bus::poll_irq_at_phase(BusPhase)` ignoring its argument
in both the trait default and the `rustyn64-core` implementation: correctly-shaped plumbing
carrying no signal (see §3.2). Resolving it properly meant going to the primary hardware
documentation rather than reasoning from the existing shape.

**Resolved by ADR 0007.** The CPU is a five-stage pipeline (IC/RF/EX/DC/WB) of inter-stage
latches advanced one PClock per step in reverse stage order, so the DC stage *is* the
interleavable point — the property is structural rather than bolted on. ADR 0005 covers the
*finer* sub-PClock question, which is genuinely deferrable; this one was not.

The inert `poll_irq_at_phase(BusPhase)` hook was not merely unwired — research against the
User's Manual showed it was shaped on the **wrong axis entirely**. Interrupt sampling has no
documented relationship to the SysAD command/data phase; the documented rule is per-PCycle,
gated on the previous PCycle having been a run cycle (UM §4.7.1). The hook was removed rather
than completed. See §3.2: had the parameter been wired up to *something* without checking, it
would have looked correct and encoded a fiction.

### 1.3 Re-phasing everything uniformly cannot fix a differential measurement

**The pattern.** When a timing test fails, the tempting fix is to shift when a subsystem is
serviced within the step. But a test that measures the *interval between two events on the same
clock* is invariant under a uniform shift — moving both endpoints moves nothing. Attempts of this
shape are guaranteed to fail, and they are indistinguishable from promising attempts until tried.

**How it went wrong before.** Five successive re-phasings of one timing track were implemented and
rolled back before the class of test was recognised as immune to that entire family of fix.

**What it means here.** N64 timing tests overwhelmingly measure *differences*: `Count`/`Compare`
deltas, DMA completion relative to an interrupt, VI line counts between frames. Before attempting
a phase adjustment, establish whether the failing measurement is absolute or differential. If it is
differential, a uniform re-phase cannot move it, and the cause is elsewhere — a wrong duration, a
wrong divisor, or a missing event — regardless of how much the numbers look like a phase problem.

**Practice adopted.** State the absolute-vs-differential classification in writing before the first
timing fix attempt, not after the third. This is the concrete form of the stop-condition discipline
in 3.3.

### 1.4 Write the determinism contract before there is anything to be non-deterministic

**The pattern.** Determinism is not a feature that can be added; it is a constraint that must never
have been violated. A single `HashMap` iteration, a wall-clock read, or an unseeded RNG anywhere in
the core silently voids replay, netplay rollback, and every bisect-driven debugging technique that
depends on reproducibility — and finding it later means auditing everything.

**What it means here.** ADR 0004 exists and the contract is written: seed + ROM + input produces
bit-identical AV, the frontend owns rate control, power-on phase comes from a seeded SplitMix64.
The lesson is not "write the ADR" — it is that the ADR's value is entirely in being enforced from
commit one, and that the enforcement is what erodes. `rustyn64-netplay` is an empty stub today; by
the time rollback needs determinism, a violation would be years old.

**Practice adopted.** Determinism is a review checklist item on every core PR, not a phase.
Specifically: no wall-clock, no OS RNG, no unordered-collection iteration order, and no
thread-scheduling dependence inside `rustyn64-core` or any chip crate. `to-dos/LOCKSTEP-CHECKLIST.md`
carries the operational form.

---

## Part 2 — The oracle, and what "green" is permitted to mean

### 2.1 A passing oracle proves "unchanged", not "correct"

**The pattern.** Framebuffer-hash and snapshot oracles compare against a stored baseline. When a
change alters output, the fastest way to make CI green is to re-bless the baseline — which converts
the oracle from a correctness check into a rubber stamp. The bug then ships *green*.

**How it went wrong before.** A rendering-pipeline change re-baselined both the snapshot corpus and
the screenshot corpus to accept its own new output. The resulting visual defect shipped green
through four consecutive releases. The separate accuracy suite did not catch it either, because
that suite was genuinely neutral to the affected pixels.

**What it means here.** Layer 5 is exactly this: `frame_hash`, `FrameComparison`, committed goldens
under `tests/golden/`, and `.snap` baselines from the commercial corpus. From the moment the RDP
produces pixels (Phase 3), every RDP change will alter goldens, and the pressure to re-bless will
be constant.

**Practice adopted.**

- Never re-bless a golden to make CI green. Re-blessing requires proving the *new* output correct
  against an external reference first — ParaLLEl-RDP's fuzz suite, Angrylion reference output, or
  hardware documentation — never against our own previous output.
- Record *why* a baseline moved in the commit message. A baseline change with no stated external
  justification fails review.
- Keep at least one oracle that is not a stored baseline. n64-systemtest is self-judging: it
  decides pass/fail internally, so it cannot be silently re-blessed. That property is why it is the
  committed-tier corpus and should stay the primary CPU/RSP gate.

### 2.2 An oracle that runs nothing looks exactly like an oracle that passes

**The pattern.** A test gate can silently execute zero tests and still exit 0. Exit codes cannot
distinguish "everything passed" from "nothing ran".

**How it went wrong before.** A Cargo feature-unification quirk meant `cargo test --workspace
--features test-roms` never enabled the feature for the harness crate. The ROM-oracle binaries
never appeared in the output, the command exited 0, and the gate reported green while testing
nothing.

**What it means here.** Our CI runs that identical command. It was tested: on this workspace the
feature *does* reach `rustyn64-test-harness` (verified — a probe test compiled in under
`--workspace --features test-roms` and was absent under a bare `--workspace`). So the bug does not
reproduce today. But nothing would have told us if it did.

**Practice adopted.** `feature_probe::probe_test_roms_feature_is_enabled` is a permanent test that
exists only when the feature is on, and the CI `test-roms` job greps for it having *run*, failing
the build if it is absent. The invisibility, not the quirk, was the real hazard.

### 2.3 Score from state, not from pixels, wherever the test permits

**The pattern.** Reading a test ROM's verdict off the rendered framebuffer couples the accuracy
gate to the entire video pipeline. A correct CPU then fails its own test because the font renders
badly, and a scoring change is indistinguishable from an accuracy change.

**What it means here.** n64-systemtest writes structured results and is designed to be read
programmatically rather than looked at. Wherever a suite offers a state-readable verdict — a RAM
location, a serial/`isviewer` log, an exit code — read that. Reserve pixel comparison for suites
that are genuinely *about* pixels (RDP conformance, the video-timing suites).

**Practice adopted.** The harness's default verdict source is emulated state. Pixel scoring is
opt-in per suite and must be justified in `docs/testing-strategy.md`, not the default.

### 2.4 Verify the reference passes the test before trusting a disagreement

**The pattern.** When our output disagrees with a reference emulator, the reference is assumed
correct. Often it simply fails that test too — or fails it differently — and the "gap" being chased
does not exist.

**Corollary that is easy to miss.** Multiple references that share an ancestor are one data point,
not three. Agreement between forks of the same lineage is evidence about the lineage, not about
hardware.

**What it means here.** ares, CEN64, Gopher64, ParaLLEl-RDP, and mupen64plus do not all descend from
one source, which is genuinely useful — but the RDP implementations in particular have real shared
history. Treat "three emulators agree" as one vote unless their independence is established.

**Practice adopted.** Before filing a discrepancy, run the same test ROM against the reference and
record its actual result. Where a disagreement is real, name which references were consulted *and*
whether they are independent, in the accuracy ledger.

### 2.5 A test you wrote is not an oracle until something else confirms it

**The pattern.** A hand-written test ROM or unit test encodes the author's *belief* about the
hardware. If that belief is wrong, the test locks the bug in and actively resists the correct fix.

**What it means here.** The libdragon/Tiny3D build tooling makes writing our own N64 test ROMs
easy, which makes this hazard likely rather than hypothetical. A homegrown ROM has exactly the
authority of the reasoning behind it — which is less than a hardware trace and less than
n64-systemtest.

**Practice adopted.** A homegrown test ROM must state its source of truth (hardware doc section,
n64brew wiki page, hardware capture) in a comment at the top. One whose expected value came from
"what our emulator currently does" is a regression pin, and must be labelled as such rather than
presented as an accuracy test.

### 2.6 Pin ROM identity by hash, and re-verify the input before debugging the emulator

**The pattern.** Time gets spent hunting an emulation bug when the actual problem is the test
input — a bad dump, a ROM hack, or a fixture that does not contain what its name implies.

**How it went wrong before.** A coprocessor was believed to have a boot bug for an extended period.
The local dump turned out to be a fan-translation hack rather than an original cartridge. The
emulator had been right the whole time; the "gap" was ROM sourcing.

**What it means here.** The commercial corpus is 66 personal dumps, every one MD5-matched against a
catalogue when staged. That is the mitigation — but it only works if the check is *repeated* when
a title misbehaves rather than assumed to hold forever. A file can also be replaced in place.

**Practice adopted.** When a single title fails while others in its class pass, re-verify its hash
against the recorded expectation before opening any emulator source. `tests/roms/external/commercial/`
records the expected MD5 for every staged title precisely so this costs seconds.

### 2.7 Prefer an invariant that cannot drift to a value that can

**The pattern.** Asserting an exact number pins behavior but says nothing about *why*, and it must
be re-blessed on every legitimate change — putting it on the same slope as 2.1. Asserting a
structural property ("the accumulator returns to zero every 3 master ticks", "no bus access occurs
between these two events") survives implementation change and states the intent.

**What it means here.** The 3:2 accumulator, DMA completion ordering, and interrupt-line
edge/level behavior are all better expressed as invariants than as expected cycle counts. Exact
counts still belong where hardware genuinely specifies an exact count.

**Practice adopted.** When writing a timing assertion, ask whether the property or the number is
the thing being claimed, and assert that. Property-based tests are the natural home for the former.

---

## Part 3 — Debugging discipline, relevant continuously

### 3.1 End-of-step equivalence harnesses miss mid-step divergence

**The pattern.** A harness that compares two implementations at the end of a step can report
bit-identity while they diverge *within* the step. If any other component reads the shared state
mid-step, the divergence is real and invisible.

**How it went wrong before.** An FSM rewrite cleared shared per-scanline state early in a step. A
later fixup in the same step restored the end-of-step value, so a 1,013-case equivalence harness
reported bit-identity. But the rendering path read that state mid-step, so most of the screen lost
its sprites. Multiple commercial titles broke at once, and the bug reached the main branch.

**What it means here.** The N64 has more of this, not less. The Bus steps each chip with a
`core::mem::take` split-borrow — the chip is moved out, ticked against `&mut Bus`, moved back — so
every chip `tick` writes shared state other chips may read within the same master tick. The whole
point of ADR 0001's not-catch-up property is that an RCP event is visible to the very next CPU
step, which makes mid-step visibility a *designed-in* property, not an edge case. Phases 2 and 3
are where this bites.

**Practice adopted.**

- Before changing any `tick`/step function, grep every write site against every read site for the
  same field within the same step. Recorded as a standing risk in the Phase 2 and Phase 3 overviews.
- Treat an equivalence harness as necessary but never sufficient; pair it with a real-ROM
  regression check.
- When the symptom is "the game broke" rather than "this test failed", reach for `git bisect run`
  with a ROM-driven harness rather than reasoning forward from the diff.

### 3.2 Prove the instrumentation carries information before reasoning from it

**The pattern.** A debug hook, log line, or trace field that is always the same value looks like
evidence and is not. Worse, plumbing that is *shaped* correctly but never actually varies invites
long chains of reasoning built on a constant.

**What it means here.** This is live in the tree right now, which is why it earns its own entry
rather than a footnote: `Bus::poll_irq_at_phase` takes a `BusPhase` and ignores it. That is
appropriate for a skeleton, and it is exactly the shape that becomes dangerous the moment someone
concludes "interrupts are sampled per phase" from the signature. See 1.2 for the binding
requirement attached to it.

**Practice adopted.** Before drawing a conclusion from instrumentation, confirm it takes at least
two distinct values across the cases being compared. When adding a diagnostic, add the negative
case too — the input that should make it read differently.

### 3.3 Bound the attempts, and record what was ruled out

**The pattern.** In a system with many plausible interacting subsystems, the first theory tends to
be the most *interesting* rather than the most *likely*, and interesting theories are expensive to
disprove. Without a stop condition, variants of one failed approach get retried indefinitely,
because each variant feels different from the inside.

**How it went wrong before.** Threading, run-ahead, audio pacing, and power-cycle behavior were
each investigated at length for a bug whose cause was a data row in a load-time override. Separately,
five successive attempts at one timing track were rolled back before the *approach* was
reconsidered — see 1.3 for why that family could not have worked.

**What it means here.** The N64 offers an unusually rich set of attractive wrong theories: bus
arbitration, RDRAM latency, cache coherency, sub-cycle timing. Each is real, each is genuinely
uncertain (all four are open questions in `docs/architecture.md`), and each is expensive.

**Practice adopted.**

- Prefer the cheap discriminating test to the compelling theory. "Does it reproduce headless?"
  costs minutes and eliminates an entire class.
- After two rollbacks of the same approach, stop and write down what is actually known before a
  third variant. That record is what `docs/accuracy-ledger.md` is for when it lands in Phase 7, and
  a ruled-out approach belongs in it explicitly — an unrecorded dead end gets rediscovered.
- ADR 0005 exists partly to keep sub-cycle bus timing from becoming the default explanation for
  every residual. It requires a named failing test before the refactor may begin.

### 3.4 A frontend-only reproduction is a load-path smell, and needs a full-state diff

**The pattern.** A per-game correction database applied at load time can silently corrupt state the
emulated hardware is supposed to own at runtime. One bad row then perfectly imitates a deep engine
bug — and it will be investigated as one, because it presents as one.

**How it went wrong before.** A vendored per-game database force-applied a mirroring correction to
boards that control mirroring themselves. A single row froze a commercial title, and the
investigation chased threading, run-ahead, audio, pacing, and achievements at length before the
load path was suspected at all. The core had been correct the entire time.

**What it means here.** We already depend on this class of file. `docs/cart.md` states that save
type is DB-resolved rather than header-read, and the commercial corpus was organised by save types
resolved via MD5 against the mupen64plus catalogue; region and CIC variant resolve similarly. On
N64 the DB is genuinely authoritative for save type — the header has no reliable field — so the
answer is not "stop using a DB", it is "bound what the DB is allowed to decide".

**Practice adopted, for Phase 5.**

- The DB describes cartridge **identity** (save type, CIC variant, region). It must never override
  state the hardware determines at runtime.
- The **core must never consult the DB.** Test suites construct the machine directly, which keeps
  the determinism contract and every accuracy gate DB-immune by construction.
- Therefore a bug that reproduces through the frontend but not headless is a load-path problem
  until proven otherwise. Check the DB, the overrides, and save-type resolution *before*
  theorising about timing.
- Diff the **full** machine state between the failing frontend run and a headless replay, per
  frame. A partial hash (RAM only, framebuffer only) can hide a divergence living in a field that
  only later leaks into the hashed region.

### 3.5 Paired pipelines must change together

**The pattern.** Hardware often has two or more lanes advancing on the same clock. Rewriting one
lane's model and leaving its sibling on the old model puts them out of phase, presenting as a
subtle systematic offset rather than an obvious break.

**What it means here.** The N64 equivalents are concrete: the RSP's SU and VU dual-issue lanes; the
RDP's two-cycle pipeline mode where both cycles feed one combiner configuration; the scheduler's
CPU tick and RCP accumulator, which must advance together or the 3:2 ratio drifts.

**Practice adopted.** When changing one lane of a paired structure, change every lane in the same
commit, and say in the message which lanes were audited. Recorded as a risk in the Phase 2 and
Phase 3 overviews.

### 3.6 A failed attempt should leave its infrastructure behind

**The pattern.** Rolling back a failed approach usually reverts the harness, logging, and
comparison tooling built to evaluate it — so the next attempt rebuilds them, and the cost is paid
per attempt rather than once.

**What it means here.** Anything built to *measure* — a trace differ, a cycle-count comparator, a
state dumper — is separable from the change being evaluated and outlives it. This is most of the
real work in a timing investigation.

**Practice adopted.** Land measurement tooling as its own commit, before the change it exists to
evaluate. When the change is reverted, the tooling stays.

### 3.7 Correctness before acceleration, and never let the accelerator become the oracle

**The pattern.** Once a faster backend exists and looks right, it quietly becomes the thing new
output is compared against — at which point its errors are undetectable.

**What it means here.** This is already ADR 0002 policy: the software reference RDP is the oracle
and the wgpu-compute backend is validated against it, never the reverse. It is recorded here
because it is a *discipline* that erodes under performance pressure, not a decision that stays made
on its own. The same applies to optimisation generally — profile before optimising, and never
accept a speedup that has not been shown to preserve output.

---

## Part 4 — Process

### 4.1 Do not write a version number before it exists

**The pattern.** Labelling in-flight work with the next version number, before checking what has
actually shipped, produces a large mislabelling to unwind at release time.

**How it went wrong before.** A body of work was labelled throughout the changelog, docs, and dozens
of code comments with a version one ahead of reality, because an earlier version had never actually
been tagged. Every occurrence had to be relabelled across dozens of files.

**What it means here.** This project had the same latent condition until very recently: `v0.1.0`
existed in the manifest and the README badge while no tag existed at all.

**Practice adopted.** Check `git tag --list` / `gh release list` before writing any version label
into a document. `to-dos/VERSION-PLAN.md` records the ladder, but the ladder is a plan — what has
shipped is whatever is tagged.

### 4.2 Automation and manual process must not both own the same step

**The pattern.** When a workflow automates a step a human also performs by habit, the two race, and
the loser's output is confusing rather than merely redundant.

**How it went wrong before.** A release-automation workflow auto-created a tag and release from the
changelog while a hand-authored tag was being prepared for the same version. The manual push was
rejected; the automated result won, and it contradicted the hand-written intent.

**What it means here.** Our release workflow triggers on a tag *push*, so the human owns tag
creation and the workflow owns everything after. That boundary is clean — but only while it stays
written down. `docs/release-notes/README.md` records the precedence order for release-note sources
for the same reason.

### 4.3 Give every unattended job a time budget

**The pattern.** A CI or release job with no `timeout-minutes` inherits GitHub's 6-hour default. A
job that hangs — waiting on a prompt, a lock, a network stall — holds a concurrency slot for hours
and blocks everything queued behind it, and the failure presents as "CI is slow" rather than as a
hang.

**How it went wrong before.** A release job squatted the queue for roughly a day before anyone
attributed the stall to it.

**Practice adopted.** Every job in every workflow carries an explicit `timeout-minutes` sized to a
few times its expected runtime (30 for CI legs, 20 for Pages, 45 for release builds). A job that
hits its budget is reporting a hang, which is information; a job with no budget reports nothing.

---

## Applying this document

These are defaults, not laws. The failure mode this document is itself most at risk of is being
written once and never consulted — so the lessons that map to a specific phase are also recorded as
**risks in that phase's overview**, where they will be read at the time they matter. Part 1 in
particular is only useful before Phase 1 starts.

When a new lesson is learned *in this project*, add it here with the same shape: the pattern, how it
manifested, and the practice adopted. A lesson without a practice attached is just an anecdote.
