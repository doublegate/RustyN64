# Changelog

All notable changes to RustyN64 are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/), and this project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

The next rung is `v0.2.0 "Interpreter"` — the VR4300 (see
[`to-dos/VERSION-PLAN.md`](to-dos/VERSION-PLAN.md)).

### Added — `LL` / `SC` / `LLD` / `SCD`, closing a Sprint 1 gap (T-11-003)

The synchronisation pair was listed in T-11-003's acceptance criteria and in the Phase 1 exit
criteria, and had **not** been implemented — the four opcodes decoded to `Reserved`. Found while
verifying Sprint 1's criteria against the code rather than against the ticket's own checkboxes.

- `LL`/`LLD` arm the link bit and record `PA(31:4)` in `LLAddr` (COP0 reg 17, diagnostic-only per
  UM §5.4.7). `SC`/`SCD` test it, store only if set, and write 1/0 to `rt` **either way**.
- A misaligned `SC` **raises** rather than reporting failure — *"If this instruction both fails
  and causes an exception, the exception takes precedence"* (UM §16 p. 487). A misaligned `LL`
  leaves the link disarmed.

### Fixed — `docs/cpu.md` had the link-bit clearing rule wrong

The spec said *"Any intervening store (or ERET) clears the link"*. The manual's list is
exhaustive and does not include stores: *"set by the LL instruction, cleared by an ERET, and
tested by the SC instruction"* (UM §3.1). `SC` is a **tester, not a clearer** — clearing it there
looks right, matches other architectures, and makes the second iteration of an `LL`/`SC` retry
loop fail forever. Now pinned by `sc_does_not_clear_the_link_bit`, and all five mutations of the
new behaviour were checked to fail the suite before the tests were kept.

`ERET` is the one thing that does clear it, and it arrives with the exception model in Sprint 2;
until then nothing clears the bit, which `docs/cpu.md` now states rather than leaving implied.

### Added — the `SysAD` transaction model (T-11-008, partial)

`sysad.rs` models the CPU↔RCP bus as a **packet protocol** — an address cycle carrying a
command, then a data cycle carrying the payload, with an unbounded wait between them where real
RDRAM latency lives. Neither reference emulator models this: both complete the access atomically
and charge a flat constant, and they disagree on the value.

- A `Transaction` is a **state machine** that is structurally incapable of completing in its
  address phase — pinned by `a_transaction_can_never_complete_in_its_address_phase`, since the
  whole point is that a device can observe the bus mid-transaction.
- The inter-phase wait is a **caller-supplied parameter**, not a constant, so it cannot be
  quietly tuned. The caller must justify what it passes.
- Block transfer orderings, including the **sub-block quirk**: a D-cache 128-bit read whose
  address bit 4 is set returns the addressed 64 bits and then the 64 bits *below* it — not the
  ones after. I-cache reads are always sequential regardless.

**`SYSCMD` bit-4 polarity resolved, and it was never a contradiction.** Ledger entry S-1 recorded
the manual and the wiki as disagreeing. Reading both carefully, they **agree on every bit**: a
request has bit 4 clear, a data beat has it set. They differ only in that the wiki calls the
*data-identifier* cycle "Command". No test ROM was needed. Kept as a ledger entry rather than
deleted, because the next reader will hit the same apparent conflict.

**Two criteria are deferred and say so.** The RCP is not yet stepped *between* the phases — the
model supports it, but driving it needs the scheduler to own the transaction rather than `DC`
completing inline, which is a `Bus`-trait change belonging with the cache model in Sprint 2. And
`M` is still unmeasured, exactly as this ticket's own note predicted: `basic.z64` is too short to
constrain it. `M` stays an explicit ledger entry with **no value** rather than a fitted-looking
number without provenance.

### Added — the determinism contract is exercised (T-11-007)

ADR 0004 said "same seed + ROM ⇒ bit-identical output" and nothing checked it. Four tests now do.

- **Bit-identical across runs** — the *whole* machine, not a summary: registers, `HI`/`LO`, PC,
  all three cycle positions, and a content hash of all of RDRAM. A partial hash can hide a
  divergence in a field that only later leaks into the hashed region. Repeated eleven times,
  because an entropy dependency surfaces intermittently rather than on the very next run.
- **Different seeds produce different machines**, added beyond the stated criteria — without it,
  a build that ignored the seed entirely would pass the first test.
- **Reset is reproducible** regardless of what ran before it.
- **A source-level guard** rejects `std::time`, `SystemTime`, `Instant::now`, `getrandom`,
  `thread::spawn`, `HashMap` and `HashSet` anywhere in the core crates. Deliberately *not*
  behavioural: such dependencies are intermittent, so a run-twice test can pass for months
  before the first divergence. This fails on the commit that introduces one, naming file and line.

The content hash is FNV-1a rather than `DefaultHasher`, whose output is randomised per process
— using it would have made the determinism test itself nondeterministic.

Mutation-tested: ignoring the seed fails the second test, and naming a banned construct in any
core crate fails the fourth.

### Added — the documented errata are recorded and pinned (T-11-005)

`docs/cpu.md` gains a **"reproduced, not corrected"** section stating each VR4300 erratum as
intended behaviour, with the manual-vs-hardware divergence spelled out and the pinning test
named. The manual documents none of these; the wiki is the only source.

- `SRA`/`SRAV` leak the upper 32 bits — **all consoles**, never known to be fixed.
- `MULT` is 64-bit × **35-bit**; `DIV` is 32-bit ÷ **35-bit** (divisor sign-extended on bit 34).
- Two new tests: `div_reproduces_the_35_bit_divisor_sign_extension_erratum` and
  `srav_shares_the_sra_erratum` — the variable shift form shares the erratum, and implementing
  one correctly and the other "properly" is an easy inconsistency.

All four errata guards are **mutation-tested**: "correcting" `sra` per the manual fires both the
`SRA` and `SRAV` guards, and reducing `div` to a plain 32-bit division fires its own.

**The FP multiplication bug is deferred to Sprint 3** and recorded rather than silently dropped.
It needs COP1, and it is the only erratum that is *not* universal — NUS-01/02/03 only — so it
also needs the console revision as a machine parameter. Its exact corrupted output is
undocumented and will have to be characterised against hardware.

### Added — the first ROM actually runs (T-11-006)

**`basic.z64` executes end to end and passes all five of its hardware-verified cases** — delay-slot
semantics, `J` in a delay slot, `BEQL` nullification, `BNEL` nullification, and `LWU`+`DADDI`.
59 instructions retired, 129 master ticks. `docs/STATUS.md` gains its first real accuracy number.

This is the first time the emulator has executed a ROM at all, and it validates the delay-slot
and branch-likely work against something other than our own expectations.

- `addr.rs` — virtual→physical translation, the prerequisite the plan had never written down.
  KSEG0/KSEG1 are unmapped and become a subtraction; the mapped segments are masked with a
  `TODO` until the TLB lands. `Cached` is returned now because the *segment* determines it and
  that information is unrecoverable once KSEG0 and KSEG1 have become the same number.
- `rom.rs` — direct loading, doing what IPL3 does: copy `0x10_0000` bytes from ROM offset
  `0x1000`, clamped to both the ROM's length and RDRAM's capacity. A commercial ROM is up to
  64 MiB against 8 MiB of RDRAM, so an unclamped copy panics on exactly the corpus it is for.
- `run_until_complete` implements Dillon's `r30` protocol for real. The pass sentinel is `-1` as
  a **64-bit** value; matching only the low 32 bits would also match `0xFFFF_FFFF`, which is a
  *failure* index.

**The test witnesses execution rather than trusting the sentinel.** `basic.z64` ends with
`BNE $30, $0, TestsFailed` / `ADDI $30, $0, -1`, so if `r30` is still 0 the branch falls through
and the pass sentinel is written **anyway** — "ran and passed" and "never ran" are identical at
the sentinel. The test therefore also asserts the PC entered the test subroutines and that a
plausible number of instructions retired. Mutation-tested: pointing the subroutine address
somewhere unreachable fails it with "this is a vacuous pass".

The test **skips rather than fails** when the ROM is absent — Dillon's suite has no licence, so
it is external-tier and CI has no copy. A gate that is red by default stops being read.

### Added — branches, jumps, and the trap family (T-11-004)

The full control-flow set: `J`/`JAL`/`JR`/`JALR`, every conditional branch including the eight
**branch-likely** forms, the twelve conditional traps, `SYSCALL` and `BREAK`.

**The delay slot now does real work**, and the reverse cascade turns out to make it fall out
cleanly. By the time `EX` resolves a branch, `IC` has already fetched the delay slot — that *is*
the architectural delay slot, not a modelling artefact. Because `EX` runs before `IC` in the same
cycle, writing the target to `next_pc` in `EX` makes the very next fetch land on it with exactly
one delay slot in between. No wrong-path fetch needs squashing.

**Branch-likely nullifies its delay slot when not taken**; an ordinary branch does not. Getting
that backwards silently runs or skips one instruction per untaken branch — invisible until a
loop's trip count is wrong. Mutation-tested.

Two bugs found by a fetch trace rather than by reasoning:

- The redirect was never applied at all — an earlier edit had failed to write, and everything
  still compiled and passed, because the test program's branch target coincided with the
  sequential path. The trace showed `next_pc` marching straight through.
- `in_delay_slot` read `ic_rf`, which `rf_stage` has already vacated by the time `IC` runs in the
  reverse cascade. The flag was silently always false. It now reads `rf_ex`.

**`in_delay_slot` is pinned by its own test**, because mutation-testing showed it was *not yet
load-bearing*: forcing it to `false` broke nothing, since its only consumer is `Cause.BD`/`EPC`
at exception time and COP0 arrives in Sprint 2. Rather than delete it — it is genuinely needed
and must ride in the latch — it is now verified from the moment it is written.

The reserved-instruction tests are repointed at encodings the VR4300 leaves **architecturally**
unassigned (primary opcodes `0o35`/`0o36`, SPECIAL funct `0o01`) rather than at instructions this
project merely hasn't implemented. They had to be moved three times — `LW`, then `BEQ` — because
they were tracking implementation progress instead of the architecture.

### Fixed — a stale test-ROM sentinel, and T-11-006 re-scoped

Investigating whether Sprint 1 could actually close turned up a documentation error that would
have cost real debugging time, and a dependency the plan had wrong.

- **The n64-systemtest completion string in our docs was from 2021.** `docs/testing-strategy.md`
  and `tests/roms/README.md` both quoted `Done! Tests: 262. Failed: 0`. That string does **not**
  appear in the committed v2.1.0 ROM — verified with `strings`, zero occurrences. The real
  summary has the form `Finished in <T>s. Base: Failed <F> of <N> tests (<P>% success rate)`,
  whose counts vary per run — so the sentinel must match the stable pattern
  `Failed (\d+) of (\d+) tests` rather than any literal line. Anything written against the old
  string would have matched nothing, silently. Both documents corrected, and the three text
  sinks the ROM actually uses (emux COP0 hooks, ISViewer, SC64) are now documented — it writes
  to **no fixed RDRAM address**, and with no sink implemented `text_out` is a silent no-op.
- **T-11-006 re-scoped.** n64-systemtest cannot report anything in Sprint 1: it dies at
  `src/main.rs:68` on `CTC1 $31`, the third statement after entry, and needs COP1 control,
  COP0, MI, PI, VI, a heap and exception vectors first — a large fraction of its tests fault
  deliberately and would hang rather than fail. No flag avoids this; category selection is
  compile-time. Retargeted at **`basic.z64`**, which is uniquely suitable: it is the only
  Dillon ROM that does not PI-DMA itself at startup, its result protocol is a single GPR
  (`r30`: 0 running, `-1` pass, `1..=5` failing index), and it needs only the integer core plus
  the T-11-004 branch/jump family. The n64-systemtest goal moves to T-11-009 in Sprint 2.
- Two prerequisites nobody had written down are now acceptance criteria: **KSEG0/KSEG1 segment
  stripping** (nothing does it today, so no ROM can execute) and a **direct-load path**. Also
  recorded that Dillon's suite has no licence, so its test must **skip** rather than fail when
  the ROM is absent.
- T-11-008 notes that fitting `M` needs a ROM that runs long enough to measure — realistically
  n64-systemtest's default-off `timing` set, which is Sprint 2. The transaction model is Sprint
  1 work; the measurement may not be.

### Added — loads, stores, and the unaligned family (T-11-003)

- `mem.rs` — load/store data shaping as pure byte-level transforms. Width and signedness rules
  (`LW` sign-extends into the 64-bit register, `LWU` does not — confusing that pair silently
  breaks any address above 2 GiB), alignment requirements, and the `LWL`/`LWR`/`LDL`/`LDR` and
  `SWL`/`SWR`/`SDL`/`SDR` merges.
- The unaligned family is **big-endian-directional**, and getting a shift backwards produces
  plausible values that are wrong only at unaligned addresses. It is tested as a *pair* — the
  way it is actually used — plus a store-then-load round-trip across every byte offset, which is
  the strongest statement that the shift directions agree with each other.
- `EX` resolves the effective address (base + **sign**-extended offset) and `DC` performs the
  access. That split is the point of the pipeline: `DC` is the cycle the scheduler interleaves
  the RCP around.
- An unaligned *aligned-form* access raises `AddressError`; the `LWL`/`LWR` family is exempt by
  construction, since being usable at any byte offset is the reason it exists.

**The load-delay interlock is live** (UM §4.6.5) — it finally has something to interlock against.
It reproduces the hardware's documented imprecision, and it compares against the `ex_dc` latch
rather than `rf_ex`: in the reverse cascade `EX` runs before `RF`, so by the time `RF` executes,
the instruction that was in `EX` has already moved on. Checking the wrong latch made it silently
never fire, which is what the integration test caught. Mutation-tested.

### Added — decode and `EX`-stage wiring: the CPU executes (T-11-002, second part)

The pipeline stops moving empty latches and starts running programs.

- `decode.rs` — MIPS III decode for the integer subset. **Total**: every 32-bit pattern decodes
  to something, with anything unrecognised becoming `Op::Reserved` rather than panicking. A guest
  can execute arbitrary bytes. Unimplemented opcodes decode to `Reserved` (which raises a
  reserved-instruction exception) rather than to a `NOP`, so a missing opcode is loud instead of
  silently producing wrong results.
- `regs.rs` — the register file, split out so `$zero`'s hardwiring lives in exactly one place.
  A scattered `if rd != 0` is how a write to `$zero` eventually slips through.
- `exec.rs` — `EX` execution as a pure function of `(op, operands, HI/LO)`, bridging decode to
  the ALU. Carries the immediate sign/zero-extension asymmetry: arithmetic forms sign-extend,
  logical forms zero-extend, so `ORI $t0, $0, 0xFFFF` yields `0xFFFF` and not all-ones.
- The pipeline stages now do their jobs: `IC` fetches and decodes, `RF` reads, `EX` executes and
  raises the multiply/divide stall, `WB` commits.

**The operand bypass network (UM §4.6) — found by the end-to-end test, missed by every unit
test.** Without it, back-to-back dependent instructions read stale registers, and `LUI`+`ORI` —
the standard way to build a 32-bit constant — produced `0xFFFF` instead of `0x7FFFFFFF`. Every
one of the 46 unit tests passed while the CPU could not run a six-instruction program. The
bypass is mutation-tested: removing it fails the end-to-end test.

Three integration tests that exercise the whole machine rather than a piece of it: a real
program computing through `ADDIU`/`MULT`/`MFLO`/`ADDU`/`SLL` into the register file; `$zero`
surviving an instruction that targets it; and an overflowing `ADD` aborting without committing.

### Added — the integer ALU (T-11-002, first part)

`crates/rustyn64-cpu/src/alu.rs`: the arithmetic, logical, shift and multiply/divide families as
**pure functions**, deliberately free of pipeline and register-file state so every rule can be
tested without constructing a machine. Decode and the `EX`-stage wiring are the remainder of
T-11-002 and follow separately.

- 32-bit arithmetic (`ADD`/`ADDU`/`SUB`/`SUBU`) trapping on signed overflow where specified, and
  the 64-bit `D*` forms trapping at 64-bit boundaries.
- The logical family (`AND`/`OR`/`XOR`/`NOR`/`LUI`) and all shifts including the `D*` forms.
- `MULT`/`MULTU`/`DIV`/`DIVU` and the `D*` forms writing `HI`/`LO`, with the documented
  full-pipeline stalls (5 / 37 / 8 / 69 `PCycle`s, UM Table 3-12) — these are not background
  operations.
- **Every 32-bit result is sign-extended into the 64-bit register**, the rule that dominates
  MIPS III and the most common source of emulator bugs in it, since it stays invisible until
  software inspects the upper half.
- The `MFHI`/`MFLO` hazard is recorded as a **non-interlocked** two-instruction window producing
  hardware's wrong result — modelling it as a stall would add timing hardware does not have *and*
  hide the value software can observe.

**Two errata reproduced rather than corrected**, each pinned by a test that fails if it is
"fixed":

- `SRA`/`SRAV` leak the upper 32 bits instead of sign-extending bit 31. Present on every console
  and never known to be fixed, so software can depend on it.
- `MULT` acts as a 64-bit by **35-bit** signed multiply (second operand sign-extended on bit 34).
  Invisible for well-formed inputs, which is why ordinary compiler output never trips it.

Two behaviours are **guesses and are recorded as such** in `docs/accuracy-ledger.md` (C-5, C-6)
rather than left looking authoritative: the `DIV` quotient when divisor bits 63 and 31 differ
(N64brew calls this "currently unclear"), and the architecturally-undefined divide-by-zero
values. What is tested and non-negotiable is that neither panics — a guest can do both at will.

### Fixed — rustdoc gets its own CI job

`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` was the **last step of the
`test` job**, after the slow test run. Two consequences, both observed rather than theorised:

- It was the most likely thing to be lost to `cancel-in-progress`. A commit with a broken
  intra-doc link (public docs linking to a private item) went through with its run marked
  **`cancelled`, not failed** — rustdoc never executed. The gate did not fail; it did not run.
- A rustdoc failure reported as `test (ubuntu-latest) failed`, pointing at the wrong subsystem.

Now a dedicated `rustdoc (-D warnings)` job with no `needs:`, so it starts immediately, runs in
parallel with the tests, and finishes in well under a minute — fast enough to be useful feedback
and small enough to rarely be mid-flight when a supersede happens. Verified by reintroducing the
exact defect that slipped through: the job's command rejects it, and `cargo test` is blind to it.

### Added — the ADR 0007 five-stage pipeline (T-11-001, second half)

`crates/rustyn64-cpu/src/pipeline.rs`. **Structure, not instructions** — the stages move latches
and account for time; decode and execute are T-11-002 onward. What is real here is the shape,
which is the part that cannot be retrofitted without rewriting every consumer.

- Four inter-stage `Latch`es (`ic_rf`, `rf_ex`, `ex_dc`, `dc_wb`), each carrying `pc`, `word`,
  `occupied`, `in_delay_slot`, and `abort`. Five stages have four boundaries; the state lives on
  the boundaries.
- `Pipeline::advance` runs **WB → DC → EX → RF → IC**. Each stage reads its input latch before
  any upstream stage writes it, so no value moves two stages in one cycle and no double buffering
  is needed — the reverse order *is* the latching.
- `Stall { cycles, cause }` with `Interlock` naming all eight documented interlocks (LDI, DCB,
  DCM, ICB, ITM, MCI, **COp**, CP0I — UM Table 4-3) so a stall is always attributable in a trace.
  ADR 0007's `resume_stage` is deliberately **absent** until it can be load-bearing: `advance`
  always runs the full cascade today, so a stored `resume` would be read by nothing — the same
  hazard `poll_irq_at_phase` was removed for (`engineering-lessons.md` §3.2).
- An abort raises a pending flush, so the instruction fetched later in the *same* cycle is a
  bubble rather than a live wrong-path fetch that would escape the flush entirely.
- `stall_for(0)` is ignored rather than recorded — a zero-cycle stall would still consume a cycle
  and mark it not-a-run-cycle, silently inserting a bubble *and* suppressing interrupt acceptance
  on the following cycle.
- `Exception` is deliberately **not** named `Fault`: UM §4.5 defines a fault as interlocks ∪
  exceptions, and only the aborting subset rides in a latch.
- `abort_from` stamps an exception into its own latch and every latch **upstream** — the
  kill-younger-instructions step. Older instructions are untouched.
- Interrupts are sampled once per `PClock` in **DC** (UM Figure 4-12, §4.7.6) and accepted only
  if the previous `PCycle` was a run cycle (§4.7.1). Exactly one recognition predicate exists.
- `load_interlocks` reproduces the hardware's documented **imprecision** — matching on the `rs`
  *or* `rt` encoded field whether or not it is used as a source, exempting `$zero`, and not
  crossing the GPR/FPR boundary. Emulating precise behaviour here would be the bug.

Seven pipeline tests, two of which are the structural guards and both **mutation-tested**:

- `a_value_advances_exactly_one_stage_per_cycle` — reversing the cascade to run forwards fails it.
- `delay_slot_flag_survives_a_multi_cycle_stall` — the Phase 1 exit criterion. Dropping the flag
  in transit fails it. A global `in_delay_slot` bool passes a naive test and fails this one.
- `an_abort_survives_the_cascade` — removing the flush fails it.
- Plus stall-freezes-the-pipeline, the interrupt run-cycle gate, abort-kills-younger-only,
  aborted-instructions-do-not-retire, zero-cycle-stall-is-not-a-stall, and the load-interlock
  imprecision cases.

Two existing tests had premises invalidated by this change and were corrected rather than
patched around: `Cpu::tick` no longer retires an instruction per call (it takes 5 `PCycle`s to
fill the pipeline), and the scheduler's step count now derives from `cpu_cycles()` rather than
`Cpu::retired`, since retirement lags stepping by the pipeline depth and the two are no longer
interchangeable. The residue invariant's third term likewise moved to an inter-domain
(CPU vs RCP) comparison, keeping it a property of the clock rather than of the CPU.

### Changed — the ADR 0006 scheduler rework (T-11-001, first half)

The canonical master clock is now **implemented**, not just decided.

- `MASTER_HZ` is **187,500,000**. `master_ticks: u64` is the only counter in the core that is
  ever incremented; `cpu_cycles()`, `rcp_cycles()` and `count_ticks()` are derived accessors,
  not fields. The 3:2 fractional accumulator (`rcp_accum`, `RCP_NUM`, `RCP_DEN`) is deleted.
- Every domain is an integer divisor, exported and asserted exact: `CPU_DIVIDER` 2,
  `RCP_DIVIDER` 3, `COUNT_DIVIDER` 4 (half PClock), `SI_DIVIDER` 12, `PIF_DIVIDER` 96.
- **Per-domain seeded phase offsets.** `Phases { cpu, rcp }` are constants derived from the
  SplitMix64 seed — not counters — so the ownership rule holds while the seeded power-on phase
  stays meaningful. A single shared offset would have made every seed produce an identical
  interleaving from tick 6 onward.
- `tick_one_unit()` is replaced by `step_to_next_edge()` and `run_until(target)`, which advance
  **edge to edge**. The master tick is a time base and is never iterated; `run_until` steps every
  edge in `(now, target]` and deliberately does not overshoot by one.
- `Cpu::cycles` is renamed **`Cpu::retired`** — a retired-work tally, which is the one kind of
  counter still allowed to be incremented because nothing schedules against it. `Cpu::tick` is
  documented as advancing **one PClock, not one instruction**.
- `Bus::poll_irq_at_phase(BusPhase)` is replaced by `Bus::poll_irq()`. `BusPhase` survives to
  describe the bus protocol, with its doc comment stating plainly that it carries no interrupt
  semantics and why.

Seven new tests, two of which are the guards that make the rest trustworthy:

- **`residue_invariants_never_move`** — samples the affine offsets between `master_ticks` and
  every derived position across 64 periods and asserts they never move.
- **`seeds_produce_distinct_interleavings`** — fails if the per-domain phases ever collapse.
- Plus `every_divider_is_exact`, `three_cpu_and_two_rcp_steps_per_six_ticks` (all seeds),
  `same_seed_same_timeline`, `edges_are_never_skipped`, `reset_preserves_phase`.

Both guards were **mutation-tested**: introducing an independently-incremented counter fails the
residue test, and collapsing the phases fails the interleaving test. A guard that cannot fail is
not a guard.

### Added — reference corpus and the accuracy ledger

- **[`docs/accuracy-ledger.md`](docs/accuracy-ledger.md)** — referenced from five documents and
  never created until now. Records measured constants with their provenance, open residuals,
  ruled-out approaches, and contradictions between primary sources. Seeded with the four timing
  constants the hardware documentation does not supply (`M`, the exception epilogue, CP0I, RDRAM
  bank state) and the four source contradictions found during the ADR review. The governing rule:
  a constant is measured, never tuned until a ROM passes — a tuned constant makes every later
  timing result unfalsifiable.
- **[`ref-docs/2026-07-20-vr4300-timing-supplement.md`](ref-docs/2026-07-20-vr4300-timing-supplement.md)**
  — `ref-docs/` is an immutable corpus, so corrections to `research-report.md` land as a dated
  supplement rather than an in-place edit. Records the IC/RF/EX/DC/WB stage-name correction, the
  MClock-is-primary clock derivation, the documented cycle-cost tables, the errata with exact
  behaviour, the SysAD block-ordering rules, and an explicit list of what is *not* documented.
- `docs/glossary.md` gains a **Clocks** section, because "master clock" is overloaded three ways
  and the primary sources own one of them (MasterClock = 62.5 MHz).
- `docs/performance.md` states the project goal plainly — sustained fully cycle-accurate
  emulation at full speed — with the budget (~156M component-steps/s, ~32 host cycles each) and
  an honest account of why it is unproven rather than impossible.

### Changed — the timebase and CPU microarchitecture are settled (Phase 1 design)

Two ADRs land ahead of any Phase 1 code, because both decisions are of the kind that cannot be
retrofitted without rewriting the scheduler and every chip's step contract at once.

- **[ADR 0006](docs/adr/0006-one-canonical-master-clock.md) — one canonical 187.5 MHz master
  clock. Supersedes [ADR 0001](docs/adr/0001-master-clock-lockstep-scheduler.md)**, which is
  retained unmodified as the record of the first design. 187.5 MHz is the LCM of the 93.75 MHz
  CPU and 62.5 MHz RCP clocks, which makes *every* emulated domain an integer divisor: CPU
  every 2 ticks, RCP every 3, COP0 `Count` every 4 (half PClock), SI every 12, PIF every 96.
  The 3:2 fractional accumulator is gone; a fractional path survives only for VI and AI, which
  genuinely run off a different crystal. The load-bearing rule is not the unit but the
  ownership: **`master_ticks` is the only counter ever incremented**, every other cycle position
  is a derived accessor, and a residue invariant test fails if any position becomes independent.
- **[ADR 0007](docs/adr/0007-cycle-accurate-vr4300-pipeline.md) — the VR4300 is a cycle-accurate
  five-stage pipeline** (IC/RF/EX/DC/WB) of four inter-stage latches advanced one PClock per
  step in **reverse stage order** (WB → DC → EX → RF → IC), which is what makes the latching
  implicit and removes any need for double-buffered state. `in_delay_slot` travels with the
  instruction in the latch chain rather than as global CPU state, so `Cause.BD`/`EPC` survive a
  multi-cycle stall between a branch and its delay slot. Interlocks are `(cycles, resume_stage)`.

Three factual corrections came out of the hardware research, all in `docs/cpu.md`:

- The pipeline stages are **IC/RF/EX/DC/WB**, not the "IF/RF/EX/DF/WB" previously documented.
  The manual's entire interlock and exception taxonomy is named stage-relative, so this matters.
- "The CPU advances one issued instruction per master tick" was not a timing model. `DDIV`
  stalls the whole pipeline for 69 PCycles, and at minimum 5 PCycles are needed per instruction.
  The documented cycle-cost tables are now transcribed into `docs/cpu.md` §Cycle costs.
- **`Bus::poll_irq_at_phase(BusPhase)` is removed, not completed.** It was shaped on the
  assumption that interrupt sampling is tied to the SysAD command/data phase; no such coupling
  is documented anywhere. The real rule is per-PCycle, gated on the previous PCycle having been
  a run cycle. Wiring the parameter up to *something* would have encoded a fiction that looked
  correct. `BusPhase` is retained for the bus protocol, with no interrupt semantics attached.

The full 655-page NEC VR4300 User's Manual turned out to be in the local wiki mirror already
(`n64brew_wiki/images/VR4300-Users-Manual.pdf`) with an intact text layer — it is now cited as
the primary timing oracle. Extract with `mutool draw -F txt`; `pdftotext` fails on it.

`docs/scheduler.md`, `docs/cpu.md`, `docs/architecture.md`, `docs/engineering-lessons.md`,
`README.md`, `AGENTS.md`, and the Phase 1 plan are updated to match. Sprint 1 gains T-11-008
(the SysAD transaction model and the measurement of `M`, the undocumented memory access time)
and its estimate rises from 3 to 5 weeks.

### Changed — dependency and toolchain refresh

Everything moved to the newest mutually-compatible versions available.

- Rust toolchain 1.96 → **1.97.1** (`rust-toolchain.toml`, workspace `rust-version`, and the
  `dtolnay/rust-toolchain` pin in all three workflows).
- egui / egui-wgpu / egui-winit 0.34 → **0.35** (MSRV 1.92, satisfied). `Panel::show_inside` is
  deprecated in favour of `Panel::show`; both call sites in `ui_shell` updated.
- `directories` 5 → **6**, `pollster` 0.4 → **1.0**, plus patch/minor moves across `winit`,
  `cpal`, `gilrs`, `rfd`, `bytemuck`, `thiserror`, `bitflags`, and `insta` via `cargo update`.
- GitHub Actions: `checkout` v4 → **v7**, `upload-artifact` v4 → **v7**, `download-artifact`
  v4 → **v8**, `configure-pages` v5 → **v6**, `upload-pages-artifact` v3 → **v5**,
  `deploy-pages` v4 → **v5**. `Swatinem/rust-cache` stays on the floating `v2` (currently 2.9.1).
- `markdownlint-cli` v0.39.0 → **v0.49.1**, which adds MD060 (table-column-style). Delimiter
  rows are normalised to padded form across 15 documents, and MD060 is pinned to
  `style: "padded"` rather than the default per-table inference — inference classified two
  single-row tables with 300-character cells as "aligned" and demanded header padding that
  cannot be met.

**`wgpu` is deliberately held at 29.** wgpu 30 is released, but no published `egui-wgpu`
accepts it — 0.35.0 requires `wgpu = "^29.0"`, and egui's unreleased `main` still pins 29.0.
Bumping wgpu alone would not fail resolution; cargo would link both wgpu 29 (for egui-wgpu) and
wgpu 30 (for the frontend), making the two `wgpu::Device` types distinct and breaking the build
with "expected `wgpu::Device`, found `wgpu::Device`". The rationale is recorded at the
dependency itself, with `cargo tree -p wgpu -d` given as the check.

### Added

- [`docs/engineering-lessons.md`](docs/engineering-lessons.md) — failure patterns carried over
  from two prior cycle-accurate emulators, generalised to this machine rather than copied.
  Ordered by when each lesson must be acted on: structural decisions that are cheap now and
  expensive-to-impossible later, then what "green" is permitted to mean, then debugging
  discipline. Phase-specific entries are also filed as risks in the relevant phase overview.
- Explicit `timeout-minutes` on every job in all three workflows (CI 30, Pages 20, release 45).
  Jobs previously inherited GitHub's 6-hour default, where a hang holds a concurrency slot and
  presents as "CI is slow" rather than as a hang.
- Phase 1 exit criterion requiring an instruction's memory access to be a point the scheduler can
  interleave around, with `Bus::poll_irq_at_phase` reaching a genuinely per-phase branch pinned by
  a test. Retrofitting that structure later means rewriting the scheduler and every chip's step
  contract at once.
- `docs/testing-strategy.md`: re-blessing a visual golden now requires justification against an
  external reference (ParaLLEl-RDP, Angrylion, hardware docs), never against our own previous
  output, with the reference named in the commit message.

### Changed

- ADR 0005 now requires a residual to be classified as an absolute or differential measurement
  before it can be attributed to sub-cycle bus timing. Differential measurements are invariant
  under uniform re-phasing and cannot be evidence for the refactor.
- Phase 5 records the per-game database as a bounded-authority data file: it may describe
  cartridge identity only, the core never consults it, and a frontend-only reproduction is
  treated as a load-path problem until proven otherwise.

### Fixed

- `Bus::poll_irq_at_phase` documented as not yet phase-sensitive. It ignores its `BusPhase`
  argument in both the trait default and the `rustyn64-core` implementation, so the signature
  implied per-phase interrupt sampling that does not exist yet.

## [0.1.0] "Foundation" — 2026-07-20

The architectural skeleton. The workspace compiles, CI is green across three platforms, the
reference corpus is acquired and licence-classified — and **no chip executes an instruction yet**.
This tag exists so the foundation is a fixed, citable point rather than an ever-growing
`[Unreleased]` section.

### Added

- Initial workspace scaffold (cycle-accurate emulator architecture, ported from RustyNES).
- Always-on egui shell (menu bar + status bar + stub debugger window) wired to the
  `winit + wgpu + cpal + egui` frontend, presenting a cleared/test-pattern frame at the
  N64 VI dimensions with the N64 controller input map (digital + analog stick).
- Release automation: a `v*` tag now publishes a GitHub Release with per-target archives
  (Linux x86_64, macOS aarch64, Windows x86_64) containing the `rustyn64` binary, both
  licences, `NOTICE`, `README`, and `CHANGELOG`, plus a `SHA256SUMS` manifest. The tag is
  checked against the workspace version before anything is published.
- Documentation site: pushes to `main` publish the rustdoc API reference to GitHub Pages
  under `/api/`, with `/` reserved for the wasm demo that lands in Phase 6.
- `scripts/mirror_n64brew_wiki.py` builds a gitignored offline mirror of the N64brew Wiki
  at `n64brew_wiki/` (see its `README.md`). 324 pages and 96 media files, with parallel
  HTML / Markdown / wikitext trees and `--refresh` for revision-based incremental updates.
- Test-ROM corpora under `tests/roms/`, split into a committed tier (permissively licensed,
  each ROM shipping its upstream `LICENSE`) and a gitignored external tier:
  - **committed** — `n64-systemtest` (MIT), the self-judging CPU/COP0/TLB/RSP gate, built
    from source since upstream publishes no prebuilt ROM;
  - **external** — PeterLemon/`krom` (196 ROMs), Dillon's n64-tests (26), the 240p Test
    Suite (built from source in a container), and a commercial regression corpus organised
    by save type.
- `scripts/check_no_roms.sh` plus a `no-commercial-roms` CI job: commercial ROMs are blocked
  by three independent guards (`.gitignore`, a pre-commit hook over the staged file list,
  and a server-side CI scan). The hook also enforces that any allowlisted ROM ships a
  `LICENSE` beside it.
- Full phase and sprint planning in `to-dos/`: the ROADMAP gains a status section, a phase spine
  with per-phase goal/exit criteria, cross-phase dependencies, and the open questions that gate
  deeper planning. All nine phase overviews adopt the seven-section skeleton (Goal, Exit criteria,
  Scope, Sprints, Dependencies, Risks, Reference docs), and ten sprint files mint **49 tickets**
  (`T-PS-NNN`, P = phase, S = sprint) with acceptance criteria, dependencies, spec references, and
  complexity. Phase 0's tickets are checked off against what actually shipped; the rest are
  forward plans grounded in the register-level detail from the wiki mirror.
- `docs/DOCUMENTATION_INDEX.md`, the docs map both sibling projects carry — subsystem specs,
  cross-cutting references, subdirectories, and the material outside `docs/`.
- Reference emulators and test suites cloned for study under the gitignored `ref-proj/`
  (ares, cen64, gopher64, simple64, parallel-rdp, parallel-rsp, angrylion-rdp-plus,
  n64-systemtest, n64-tests, libdragon, PeterLemon/N64). Per-repo licences are verified and
  recorded in `ref-proj/README.md`, which classifies each as vendor-ok or study-only.

### Changed

- Modernized the frontend against egui 0.34 / wgpu 29 / cpal 0.18 (`Panel::*::show_inside`,
  `MenuBar::new().ui`, `ui.close`, `Context::run_ui`, `CurrentSurfaceTexture`, the
  `experimental_features` / `multiview_mask` / `immediate_size` descriptor fields).
- Aligned the `to-dos/` phase overviews + ROADMAP to the N64 phase set (RSP LLE, RDP LLE+VI,
  AI audio, cart boot + saves, frontend integration, accuracy breadth).
- Filled out `CONTRIBUTING.md` (Rust-only) and fixed the markdownlint pre-commit hook to
  pass `--config .markdownlint.json`.
- `docs/STATUS.md` gained a project-infrastructure table and a test-ROM corpus table, and
  its accuracy table gained an "oracle available?" column. Several gates now have their ROM
  staged locally while remaining not-started, and collapsing those two states into one
  would misrepresent progress.
- `docs/testing-strategy.md` documents the corpus tiers, the commercial-ROM guards, and the
  per-corpus licensing that decides which tier a corpus lands in.
- `README.md` rebuilt to the structure RustyNES and RustySNES share — centred title block with a
  three-row badge set, Overview, Why RustyN64, Highlights, Features, Quick Start, Default
  Controls, Architecture with crate and layout tables, Compatibility and Accuracy, Performance,
  Platform Support, Documentation, Current Release, Roadmap, Contributing, License,
  Acknowledgments, Citation, and the shared footer. Adapted honestly for a pre-alpha: the
  accuracy badges read "not started" rather than borrowing the siblings' 0-diff claims, the
  Highlights table separates working from stubbed, and the Performance section states that no
  measurements exist rather than inventing any.
- `.gitignore` covers output our own workflows produce when run locally (`_site/` from pages.yml,
  `dist/` and the release archives from release.yml) plus `site/` for a future docs handbook.
- `docs/adr/0004-determinism-contract.md` gained the `Consequences` section the Nygard format and
  the project's own docs rules require — it was the only ADR without one. Records the costs the
  contract imposes and the fact that it is currently specified but unexercised.
- Root `Cargo.toml` excludes `ref-proj/` and `n64brew_wiki/` from the workspace. Cargo's
  upward workspace discovery otherwise makes any nested project there — `n64-systemtest`,
  `gopher64` — resolve *this* workspace as its root and fail to parse our members.

### Fixed

- CI, release, and Pages jobs now install the Linux system headers the frontend needs
  (`libasound2-dev`, `libudev-dev`, `libxkbcommon-dev`, `libwayland-dev`). `cpal` pulls
  `alsa-sys` and `gilrs` pulls `libudev-sys`, whose build scripts call `pkg-config`; those
  headers are absent from the GitHub runner images, so every Linux job that compiled the
  workspace would have failed in a build script.
- `release.yml` pins the toolchain to 1.96 instead of `@stable`. `rust-toolchain.toml`
  takes precedence over the action's default, so the previous config installed one
  toolchain and built with another.
- The nine phase overviews cited "the skill's references/roadmap_template.md" for their exit
  criteria — a dangling reference to the generator skill, which is not part of this repository, so
  no phase had a stated exit bar. Every phase now carries real, checkable criteria.
- `AGENTS.md` claimed the repository used three incompatible ticket-ID schemes. It does not:
  `T-PS-NNN` is a template where P is the phase digit and S the sprint digit, and the overviews
  instantiate it correctly as `T-01-NNN` through `T-81-NNN`. Only the pre-ticket code TODOs use a
  separate subsystem-scoped form.
- `README.md` cited the "Mesen2 / ares / higan" accuracy bar, which belongs to the NES/SNES
  projects. The N64 reference set is ares / CEN64 / Gopher64 / ParaLLEl, per
  `docs/architecture.md`. It also listed only 8 of the 10 workspace crates, and its
  quick-start implied the binary plays games — it currently opens a shell and presents a
  test pattern.
- `crates/rustyn64-frontend/web/Trunk.toml` pinned the wasm-bindgen CLI to 0.2.100 while
  `Cargo.lock` resolved the library to 0.2.126. Trunk requires these to be equal, so the
  wasm build would have failed at bindgen time. A new `wasm-bindgen-pin` CI job now
  compares the two and fails on drift, which is otherwise invisible until someone runs
  `trunk build`.

[Unreleased]: https://github.com/doublegate/RustyN64/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/doublegate/RustyN64/releases/tag/v0.1.0
