# RustyN64 agent reference: conventions and engineering lessons

Moved verbatim from the AGENTS.md project block. The incident narratives behind each rule; cross-references point at `docs/engineering-lessons.md` and `docs/accuracy-ledger.md`.

## Conventions

- A chip change touches the chip code AND its `docs/<chip>.md` in the same commit.
- **A comment stating a rule is not an implementation of it.** Four times in Sprints 2–3 (`CACHE`
  Index/Hit, `DMFC1` "ignores FR", the PI byte-write RMW, `DIV` on `inf/0`) the comment was right
  and the code disagreed — and no test failed, because a wrong comment breaks nothing. If a rule is
  worth a comment, write the test that fails without it (`docs/engineering-lessons.md` §3.3c).
- **"Undocumented" is a claim about a document, and it decays.** Three files asserted the exception
  epilogue cost was undocumented while it sits in the opening sentence of the section they cited.
  It has now happened **three times**; the most recent was `PRId.Rev`, recorded as undocumented on
  the strength of the User's Manual while the N64brew wiki this project *mirrors* names the values
  outright. Nothing fails when such a claim goes stale, so it survives review and is then cited as
  if it described the hardware. Cite the pages actually read, and name the source — "the UM does not
  say" is checkable, "undocumented" is not (§3.3b).
- **Never invent a value the documentation does not give — and that includes *behavior*.** The FP
  multiplication erratum's trigger is documented and its *output* is not, so `Stepping::Early`
  changes no arithmetic (ledger U-7). A fitted constant makes every later result built on it stop
  being evidence. Invented **side effects** are worse, because a constant at least lands in the
  ledger where it can be argued with: `MOV`/`ABS`/`NEG` were written to clear `FCSR.Cause` on no
  authority, which erased the previous operation's result before software could read it — 112
  n64-systemtest assertions, and it made a separately-correct feature measure as zero improvement.
  Before writing any incidental state change (clearing a field, zeroing a half-register), ask what
  documents it; if nothing does, do nothing and say so. Doing nothing is falsifiable.
- **An instruction that decodes to a silent no-op is invisible to every "does not raise" test.**
  `MOV.fmt` (COP1 funct 6) fell outside the decode arm and no-op'd; compilers emit it for every FP
  argument and return value, so operands were stale and results never left their callee. It cost
  ~100 oracle assertions and nine rounds aimed at the wrong subsystem, while the existing test for
  that path asserted only "does not raise when `CU1` is set" — which a no-op satisfies. Assert the
  **effect**: seed the destination so it differs from the expected result in every byte. When
  adding a decode arm, enumerate the neighboring funct/opcode space rather than only the encoding
  that prompted the change.
- **Capture the instruction stream, not the state, and correlate the capture.** When a value looks
  wrong or stale, dump `(pc, word)` around the site before theorizing about the unit that produced
  it — four register-watching probes failed where one code dump succeeded. Arm the capture on the
  suite's own `Running <test>...` marker so the instruction is provably the failing case's;
  uncorrelated captures produced two confident conclusions here that had to be retracted. Three
  signatures worth knowing: a value **identical across every case regardless of input** means the
  instruction never wrote it; a value that is some **earlier test's fill pattern** is stale
  leftover, not this test's sentinel; a **sticky flag present while its per-operation twin is
  clear** means a later instruction overwrote it. And read the oracle's own source in `ref-proj/`
  first — it is cheaper than any probe.
- **Never increment a cycle counter except `master_ticks`.** Every other cycle position is a
  derived accessor (ADR 0006). A new `cycles`/`ticks` field on a chip struct needs justification
  in review; the one legitimate use is a *retired-work* tally that nothing schedules against. The
  residue invariant test exists to catch this and must stay in the default `cargo test` path.
- **In the reverse cascade, check which latch actually holds what you mean.** Stages run
  WB → DC → EX → RF → IC, so by the time a stage executes, every *downstream* stage has already
  moved its latch on. Reading the "obvious" latch has now silently produced a no-op twice — the
  load interlock read `rf_ex` when the load was in `ex_dc`, and the delay-slot flag read `ic_rf`
  after `rf_stage` had vacated it. Both compiled, both passed every existing test, and both did
  nothing. When a pipeline check mysteriously never fires, suspect this first.
- **Trace, don't reason, about pipeline timing.** Dumping the actual fetch/PC sequence found two
  such bugs in one run after several minutes of reasoning had produced two wrong answers.
- **A test whose success and failure paths converge proves nothing.** A control-flow test whose
  branch target equalled the sequential path passed with the redirect code entirely absent.
  Choose targets that are unreachable if the feature is broken.
- **Mutation-check every guard before keeping it**: revert the fix, confirm the test goes red,
  restore. A test asserting "a trapped FP operation does not retire" compared *total* retired
  counts between a trapping and non-trapping run and **passed with the fix removed**, because the
  trap flushes the pipeline and the totals differ over a fixed cycle budget either way. Put the
  reason in the test's doc comment or the confounded version returns as a "simplification".
- **Agreeing flags are not evidence the value is right — and vice versa.** Three bugs this project
  shipped had exactly one half correct: the `as`-cast integer conversions reported `inexact`
  perfectly while ignoring `FCSR.RM`, so the flag gave no signal at all; the underflow-after-rounding
  bug produced the *correct value* with a missing flag. When an oracle disagrees, check both halves
  independently — a matching flag is the most convincing way to look at the wrong one.
- **An address in 32-bit mode must be the SIGN EXTENSION of its low word.** `0x0000_0000_8000_1000`
  is not shorthand for `KSEG0`; it is an address error, and n64-systemtest asserts it. Truncating to
  `u32` is the natural shortcut and accepts it silently — it had reached the reset vector, the ROM
  entry point, and every kernel address in the test suite, all of which were passing on addresses
  the hardware rejects.
- **The segment map is a function of `(address, mode, width)`, never the address alone.** `KSEG0`
  does not exist in User mode, and it is the *address-space check* — not the TLB — that stops a user
  program reaching it. An out-of-range address raises AdEL **before** the TLB is consulted; folding
  it into a refill sends the program to the refill handler, where a well-behaved kernel maps the
  page and grants the access it was never allowed to make.
- **Read the oracle's table; do not compute against it.** Three separate rules this session
  (`FR = 0`'s `fs`/`ft` asymmetry, `PageMask` pairs, the `RE` byte swap) looked self-contradictory
  under arithmetic and fell out immediately once the expected values were read as a table — twice
  the suite states the rule outright in its own assertion messages.
- **A stall cannot substitute for a bypass when the stall freezes both sides.** `stall_for` halts
  every stage, so holding a consumer also delays the producer's commit and the gap never closes. The
  load interlock is not a counter-example: it works because its consumer reads through the *bypass
  network*, and the one-cycle stall merely buys `DC` time. Before adding an interlock, check whether
  a forwarding path exists for the value at all (ledger C-25).
- **Delete a superseded function; do not leave it unused.** Four `cvt_*` converters became dead when
  the pipeline moved to `softfloat::from_int`, and each still rounded to nearest-even. An unused
  function that quietly gets an operation wrong is the inert-API hazard `docs/engineering-lessons.md`
  §3.2 describes — `addr.rs` deleted a stale `translate` for the same reason, and that precedent is
  the argument to cite.
- **A timing constant the hardware docs do not supply is MEASURED, never tuned.** It goes in
  `docs/accuracy-ledger.md` with its provenance. Adjusting one until a ROM passes makes every
  later timing result unfalsifiable. Currently unmeasured: `M` (memory access time), the
  exception-epilogue cost, CP0I, RDRAM bank-state costs.
- **NaN classification on the VR4300 is INVERTED from IEEE-754:2008**: significand MSB **set**
  means *signaling*, so `f32::NAN` (`0x7FC0_0000`) raises Invalid here. This looks like a bug on
  every reading and is not — it is the legacy MIPS convention, corroborated by the processor's own
  default NaN result (`0x7FBF_FFFF`, MSB clear) being quiet only under it. Never "correct" it back
  to IEEE; see ledger **C-12** and the test that asserts `is_snan_f32(f32::NAN)` on purpose.
- **The VR4300 has no subnormal datapath.** A subnormal operand *or result* raises the unmaskable
  unimplemented-operation cause (bit 17), not a number — `FCSR.FS` flushing is the only exception,
  and even that only when underflow and inexact are both disabled. Ledger **C-13**. Two traps
  here: `MOV` is a pure bit move while `ABS`/`NEG` classify their operand and replace `Cause`; and
  **compares are exempt** — `C.cond.fmt` treats a subnormal as an ordinary number, so applying the
  rule there regresses all sixteen compare tests.
- Say "master clock" only with its rate. The sources use **MasterClock = 62.5 MHz**; this
  project's master tick is **187.5 MHz**; ADR 0001 used it for 93.75 MHz. See `docs/glossary.md`.
- `unsafe` is allowed in the frontend and FFI, and **nowhere else without an ADR**. Every chip
  crate and `-core` carry `#![forbid(unsafe_code)]`. Two qualifications, both of which the old
  wording ("there is zero `unsafe` in the tree today") had outrun:
  - **`rustyn64-rdp-gpu` already contains `unsafe`** — 12 blocks, the parallel-rdp FFI shim,
    quarantined there on purpose (ADR 0014). The tree has not had zero `unsafe` since #241.
  - **`rustyn64-rsp` has a conditional exception** (ADR 0016): `core::arch` intrinsics in
    `vu.rs` only, behind four gates including a scalar/vector equivalence test. **Unused today**
    — the crate is still `forbid` — and using it drops the crate to `deny`, which is a real
    weakening the ADR spells out. Read it before reaching for an intrinsic; it records the trade
    as authorized, not recommended.
- Stubs are `TODO(T-XXX-NN)` comments in no-op bodies that still compile, NOT `todo!()`. So a
  green `cargo test` does not mean a subsystem works — check `docs/STATUS.md`.
- Ticket IDs are `T-PS-NNN`, where **P is the phase digit and S the sprint digit** — so
  phase 1 sprint 1 mints `T-11-001`, phase 3 sprint 2 mints `T-32-004`. CONTRIBUTING/ROADMAP
  state the template; the phase overviews instantiate it. Code TODOs use a separate
  subsystem-scoped form (`T-CPU-01`, `T-HARNESS-02`) for pre-ticket scaffolding.
- Never commit commercial ROMs.
- Versioning starts clean at v0.1.0 (now at v0.3.0) — RustyNES "v2.0 / engine-lineage" anchors are NOT this
  project's releases.
