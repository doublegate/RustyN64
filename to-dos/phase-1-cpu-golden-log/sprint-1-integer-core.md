# Sprint 1 — Register file, decode, and the integer core

**Phase:** Phase 1 — CPU golden log
**Sprint goal:** the scheduler counts one canonical 187.5 MHz master clock with every other
cycle position derived from it, and the VR4300 executes the MIPS III integer instruction set as
a five-stage pipeline advanced one PClock per step, with correct delay-slot and load-interlock
behaviour.
**Estimated duration:** 5 weeks (raised from 3: T-11-001 now carries the timebase rework and
the pipeline structure, both of which are prerequisites for every ticket after it.)

## Tickets

### T-11-001 — The canonical master clock and the pipeline skeleton

**Description:** rework the scheduler onto a single 187.5 MHz counter (ADR 0006) and build the
VR4300 as a five-stage pipeline of inter-stage latches advanced one PClock per step in reverse
stage order (ADR 0007). This is the structural foundation for the whole phase and **must land
before any instruction implementation** — both decisions are the kind that cannot be retrofitted
without rewriting the scheduler and every chip's step contract at once.

**Acceptance criteria:**

*The timebase (ADR 0006):*

- [x] `MASTER_HZ = 187_500_000`; `CPU_DIVIDER = 2`, `RCP_DIVIDER = 3`, `COUNT_DIVIDER = 4`.
- [x] `master_ticks: u64` is the **only** incremented counter in the core. `cpu_cycles()` and
      `rcp_cycles()` are derived accessors, not fields. The old `rcp_accum` is gone.
- [x] The scheduler advances **edge to edge**, never iterating the master tick.
- [x] **The residue invariant test**: affine offsets between `master_ticks` and every derived
      position are sampled at frame boundaries and asserted never to move after the first.
      In the default `cargo test` path, not behind a feature.
- [x] 3 CPU steps and 2 RCP steps per 6 master ticks, for every seed.
- [x] `reset()` re-derives the same phase; `master_ticks` starts at `phase`, not zero.

*The pipeline (ADR 0007):*

- [x] Four inter-stage latches (`ic_rf`, `rf_ex`, `ex_dc`, `dc_wb`), each carrying
      `{ pc, fault: Option<Fault>, in_delay_slot }`.
- [x] `tick` advances **one PClock**, running stages in **reverse order WB → DC → EX → RF → IC**.
      A comment states why: the reverse order *is* the latching, so no double-buffering is
      needed. A test asserts no value propagates two stages in one cycle.
- [x] Interlocks are expressed as `(stall_cycles, resume_stage)`; a stage that cannot complete
      back-pressures every upstream stage in the same cycle.
- [x] `in_delay_slot` travels with the instruction. **Pinned by a test where a multi-cycle stall
      separates a branch from its delay slot** and `Cause.BD`/`EPC` are still correct — the
      global-flag version of this passes the naive test and fails this one.
- [x] 64-bit GPRs with `$zero` hardwired; `HI`/`LO` present.
- [x] `Bus::poll_irq_at_phase` is **removed**. Interrupts are sampled once per PClock in DC,
      accepted only if the previous PCycle was a run cycle, via exactly **one** recognition
      predicate in the tree.
- [x] `docs/cpu.md` and `docs/scheduler.md` reflect the shipped code (already written ahead).

**Dependencies:** T-01-002, T-01-003
**Reference:** ADR 0006, ADR 0007; `docs/scheduler.md`; `docs/cpu.md`;
`n64brew_wiki/images/VR4300-Users-Manual.pdf` §4.1, §4.6–4.7
**Estimated complexity:** XL

---

### T-11-002 — Integer ALU, shifts, and multiply/divide

**Description:** implement the arithmetic, logical, shift, and multiply/divide families across
both 32- and 64-bit forms, with the correct sign-extension rules for the 32-bit operations.

**Acceptance criteria:**

- [x] `ADD`/`ADDU`/`SUB`/`SUBU`/`DADD`/`DADDU`/`DSUB`/`DSUBU` with overflow trapping where the
      instruction specifies it.
- [x] All logical and shift families including the `D*` 64-bit and `*32` variants.
- [x] `MULT`/`MULTU`/`DIV`/`DIVU` and the `D*` forms write `HI`/`LO` and **stall the entire
      pipeline** for the documented count — 5 / 37 / 8 / 69 PCycles (UM Table 3-12). These are
      not background operations.
- [x] The `MFHI`/`MFLO` two-instruction hazard is modelled as a *non-interlocked* hazard
      producing hardware's wrong result, not as a stall.
- [x] 32-bit results are sign-extended into the 64-bit register as hardware does.
- [x] Unit tests per family in both widths.

**Dependencies:** T-11-001
**Reference:** `docs/cpu.md`; `n64brew_wiki/markdown/MIPS III instructions.md`
**Estimated complexity:** L

---

### T-11-003 — Loads, stores, and the unaligned family

**Description:** implement the load/store set including the unaligned `LWL`/`LWR`/`LDL`/`LDR`
family and the atomic `LL`/`SC`/`LLD`/`SCD`, honouring the endianness and the alignment
exception rules.

**Acceptance criteria:**

- [x] All widths implemented, signed and unsigned, with correct sign extension.
- [x] The unaligned family merges partial words exactly, at every byte offset.
- [x] `LL`/`SC` set and test the link bit, and `SC` reports success correctly.
- [x] Unaligned access on an instruction that requires alignment raises the address exception
      rather than silently succeeding.
- [ ] **Blocked, not skipped** — n64-systemtest's RAM/ROM/SPMEM/PIF access categories cannot
      report anything yet: the suite dies on `CTC1 $31` three statements after entry, so it
      needs COP1 control and COP0/exception dispatch first. Re-scoped to T-11-009 (Sprint 2).

**Dependencies:** T-11-002
**Reference:** `docs/cpu.md` §memory; `n64brew_wiki/markdown/SysAD Interface.md`
**Estimated complexity:** L

---

### T-11-004 — Branches, jumps, and the trap family

**Description:** implement the branch, branch-likely, jump, jump-and-link, and `TRAP` families,
plus `BREAK` and `SYSCALL`, each raising the correct exception.

**Acceptance criteria:**

- [x] Every branch and jump form computes the right target, including the register-indirect and
      the 26-bit region forms.
- [x] Branch-likely nullifies the delay slot when not taken; ordinary branches do not.
- [x] `TRAP` conditions, `BREAK`, and `SYSCALL` raise their exceptions with the right cause.
- [ ] **Blocked, not skipped** — same cause as T-11-003's n64-systemtest criterion; the suite
      cannot start. `TRAP`/`BREAK`/`SYSCALL` are implemented and unit-tested; what is missing is
      the *oracle*, not the behaviour. Re-scoped to T-11-009 (Sprint 2).

**Dependencies:** T-11-003
**Reference:** `docs/cpu.md` §control flow
**Estimated complexity:** M

---

### T-11-005 — The documented VR4300 errata

**Description:** reproduce the CPU's known hardware bugs rather than correcting them: the
multiplication bug, the 32-bit shift-right-arithmetic bug, and the sign-extension bugs.

**Acceptance criteria:**

- [x] Each documented erratum reproduced, with a named test that fails if it is "fixed" —
      `sra_reproduces_the_vr4300_erratum`, `srav_shares_the_sra_erratum`,
      `mult_reproduces_the_35_bit_sign_extension_erratum`,
      `div_reproduces_the_35_bit_divisor_sign_extension_erratum`.
- [x] Each test cites `n64brew_wiki/markdown/VR4300.md` so the intent is obvious to the next
      reader.
- [x] `docs/cpu.md` records each erratum as intended behaviour.
- [x] **The FP multiplication bug is deferred to Sprint 3**, where COP1 lands. It is also the
      only erratum that is *not* universal — NUS-01/02/03 only — so it needs the console
      revision as a machine parameter, and its exact corrupted output is undocumented and will
      have to be characterised. Recorded here rather than silently dropped.

**Dependencies:** T-11-002
**Reference:** `n64brew_wiki/markdown/VR4300.md` §Known Bugs
**Estimated complexity:** M

---

### T-11-006 — First real pass/fail out of the harness (`basic.z64`)

**Description:** replace the harness's stubbed completion sentinel and get the first genuine
pass/fail out of a ROM, targeting **`basic.z64`** from Dillon's suite.

**Re-scoped 2026-07-20.** This ticket originally targeted n64-systemtest. Investigation
showed that is **not reachable in Sprint 1**: n64-systemtest dies at `src/main.rs:68` on
`CTC1 $31` — the third statement after entry — and before reporting anything it needs COP1
control, COP0 (`Status`/`Count`/`CACHE`), MI, PI status, VI init, a working heap, and exception
vectors, because a large fraction of its tests fault deliberately and would otherwise hang
rather than fail. There is no flag around it: category selection is compile-time `cfg!()` and
COP0 sits on the pre-test path regardless. That work is Sprint 2 (COP0, the TLB and the
exception model); the remaining harness-side piece is captured as T-11-009 below. No Sprint 2
ticket ID is cited because that sprint's tickets are not minted yet — inventing one would give
a dangling reference that looks tracked and is not.

`basic.z64` is the right first target and needs almost nothing beyond this sprint:

- Entry `0x8000_1000`, size exactly `0x10_1000`.
- **The only Dillon ROM that does not PI-DMA itself at startup**, so it needs no Phase 5 work.
  (`sll.z64`/`addiu.z64` and the rest do, and are the natural *second* step once PI lands.)
- Result protocol is one GPR: **`r30`** is 0 while running, `u64::MAX` (`-1`) on pass, and
  `1..=5` for the index of the failing test.
- Instruction set: the integer core plus `J`/`JAL`/`JR`/`JALR`/`BEQ`/`BNE`/`BEQL`/`BNEL`,
  `LWU` and `DADDI` — i.e. exactly T-11-004 and what is already done.
- The only MMIO before the sentinel is one `SW` to PIF RAM `0xBFC0_07FC`; a write-accepting
  stub suffices. VI writes happen *after* `r30` is set.

**Acceptance criteria:**

- [x] KSEG0/KSEG1 segment stripping in the CPU, so a virtual address becomes physical before
      it reaches the Bus. Nothing does this today, and no ROM can execute without it.
- [x] A direct-load path that does what IPL3 would: copy **`0x10_0000` bytes** from ROM
      offset `0x1000` to RDRAM `0x1000`, clamped to the ROM's actual length, and set
      `PC = 0x8000_1000`. No CIC handshake, no PI DMA. The byte count is the documented boot
      behaviour (`ref-proj/n64-tests/README.md`: *"copy 0x100000 bytes from 0x10001000 to
      0x00001000"*), not `basic.z64`'s size — the two coincide here, and hard-coding either an
      end offset or "the whole ROM" breaks on the next target. Clamping matters because RDRAM
      is 8 MiB and a commercial ROM is up to 64 MiB.
- [x] `run_until_complete` polls `r30` and returns `Passed` / `Failed(index)` / `Timeout`
      instead of always timing out.
- [x] `basic.z64` reports a genuine result, and a failure names *which* of the five tests failed.
- [x] The test **skips, not fails**, when the ROM is absent — Dillon's suite has **no licence**
      and is external-tier, so it cannot be committed and CI must stay green without it.
- [x] `docs/STATUS.md`'s accuracy table gains its first real number.

**Dependencies:** T-11-004 (the branch/jump family, incl. branch-likely)
**Reference:** `ref-proj/n64-tests/README.md` §"If your emulator is very young";
`crates/rustyn64-test-harness/src/runner.rs`; `tests/roms/README.md`
**Estimated complexity:** M

---

### T-11-009 — Deferred: n64-systemtest reports a genuine count

**Status: deferred to Sprint 2**, recorded here so the dependency is visible rather than
rediscovered.

n64-systemtest cannot report anything until COP0, COP1 control and exception dispatch exist
(see T-11-006's re-scope note). When they do, the remaining work is small:

- Decode the **emux** COP0 hooks — `xdetect` (funct `0x20`), `xlog` (`0x25`), `xioctl`
  (`0x2C`). `xioctl exit` is an exact completion edge needing no polling, and `xlog` gives the
  full text stream. Roughly 60 lines in the decoder plus a host-side log buffer.
  *Alternatively* map `0x13FF_0000..0x13FF_0220` as ISViewer scratch RAM in the PI decode,
  which needs no CPU changes at all.
- Match `Failed (\d+) of (\d+) tests` — **not** the `Done! Tests: N. Failed: M` string that
  earlier revisions of our docs quoted, which does not exist in the committed v2.1.0 ROM.

**Dependencies:** Sprint 2 (COP0, the exception model)

---

### T-11-008 — The SysAD transaction model and the `M` measurement

**Description:** model the CPU↔RCP bus as a real transaction at SClock (62.5 MHz — 3 master
ticks against the CPU's 2) with a command cycle and a data cycle, rather than an access that
completes atomically. Then *measure* `M`, the memory access time, instead of guessing it.

This is the one place the project can exceed the reference emulators rather than match them:
neither CEN64 nor ares models the phase split at all — both complete the access in zero emulated
time and charge a flat constant, and they disagree on what that constant is.

**Acceptance criteria:**

- [x] A transaction is a **state machine** with distinct address and data phases on the SClock
      grid, and it is structurally incapable of completing in its address phase — pinned by
      `a_transaction_can_never_complete_in_its_address_phase`.
- [x] The inter-phase wait is **unbounded** and supplied by the caller rather than being a
      constant, so it cannot be quietly tuned.
- [x] Block transfer orderings, including the sub-block quirk when address bit 4 is set and the
      rule that I-cache reads are always sequential regardless.
- [x] `SYSCMD` bit-4 polarity resolved — and it turned out **not to be a contradiction**: both
      sources put a request at bit 4 clear and a data beat at bit 4 set, and differ only in
      which cycle they call "command". Recorded in `docs/accuracy-ledger.md` S-1.
- [ ] **Deferred — the RCP is not yet stepped between the phases.** The transaction model
      supports it (that is why it is a state machine), but driving it requires the scheduler to
      own the transaction rather than the `DC` stage completing the access inline. That is a
      change to the `Bus` trait and the scheduler contract, and it belongs with the cache model
      in Sprint 2, where `DC` gains a reason to stall for multiple cycles anyway.
- [ ] **Deferred — `M` is not measured.** As this ticket's own note predicted, `basic.z64` is
      too short to constrain it and the realistic source is n64-systemtest's default-off
      `timing` set, which needs Sprint 2. `M` remains an explicit ledger entry (C-1) with no
      value rather than a fitted-looking number without provenance.

**Note on the `M` measurement.** Fitting `M` needs a ROM that runs long enough to measure, and
`basic.z64` (T-11-006) is too short and too simple to constrain it. The realistic source is
n64-systemtest's `timing` feature set, which is **default-off upstream** and depends on
Sprint 2. So the *transaction model* is Sprint 1 work and the *measurement* is not; if the
measurement slips, land the model with `M` as a single documented placeholder in the accuracy
ledger and do not let a fitted-looking number ship without provenance.

**Dependencies:** T-11-003
**Reference:** `n64brew_wiki/markdown/SysAD Interface.md`; UM §12, Tables 11-1/11-2, 12-2
**Estimated complexity:** L

---

### T-11-007 — The determinism regression test

**Description:** close the ADR 0004 gap that `docs/STATUS.md` records as unexercised: two runs
from the same seed and input must produce byte-identical traces.

**Acceptance criteria:**

- [x] A test runs the same ROM twice from one seed and compares the full machine byte for byte
      — registers, `HI`/`LO`, PC, all three cycle positions, and a content hash of all of RDRAM.
      Repeated eleven times, since an entropy dependency surfaces intermittently.
- [x] The test fails if any wall-clock, OS entropy, or iteration-order dependency is introduced
      — a **source-level** guard, because such dependencies are intermittent and a run-twice
      test can pass for months before the first divergence.
- [x] A **different seed produces a different machine**, so the contract is not vacuous. Added
      beyond the stated criteria: without it, a build that ignored the seed entirely would pass.
- [x] `docs/adr/0004-determinism-contract.md` and `docs/STATUS.md` are updated to say the
      contract is exercised rather than merely specified. (Note `STATUS.md` did not in fact
      contain the "unexercised" claim this ticket referred to; it now records the gate.)

**Dependencies:** T-11-006
**Reference:** `docs/adr/0004-determinism-contract.md`
**Estimated complexity:** M

---

## Sprint review checklist

- [x] All tickets checked off or explicitly deferred (with reason).
- [x] The residue invariant test passes and is in the default test path.
- [x] No `+= 1` on any cycle position in the core except `master_ticks`.
- [ ] **Not met, and re-scoped rather than quietly dropped** — n64-systemtest reports no
      number for the integer categories because it cannot reach them (`CTC1 $31` at entry).
      Sprint 1's real pass/fail came from `basic.z64` instead, 5/5. → T-11-009 (Sprint 2).
- [x] CHANGELOG.md updated.
- [x] `docs/cpu.md` updated in the same change as the code it describes.
