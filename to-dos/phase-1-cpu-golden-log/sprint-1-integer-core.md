# Sprint 1 — Register file, decode, and the integer core

**Phase:** Phase 1 — CPU golden log
**Sprint goal:** the VR4300 datapath executes the MIPS III integer instruction set with correct
delay-slot and load-interlock behaviour, stepping under the existing scheduler and reporting
cycle cost back to it.
**Estimated duration:** 3 weeks

## Tickets

### T-11-001 — Register file, the 64-bit datapath, and the step function

**Description:** implement the 32 general-purpose registers at 64 bits, `HI`/`LO`, the program
counter with its delay-slot shadow, and a `step` that fetches through the existing `CpuBus` and
returns the cycle cost to the scheduler.

**Acceptance criteria:**

- [ ] 64-bit GPRs with `$zero` hardwired; `HI`/`LO` present.
- [ ] The step function fetches through `CpuBus` and advances the master clock by the real cost.
- [ ] Branch delay slots execute before the branch target, including the branch-likely variants.
- [ ] The load-delay interlock is modelled, since it is observable.
- [ ] Unit tests cover delay-slot ordering and interlock stalls.

**Dependencies:** T-01-002, T-01-003
**Reference:** `docs/cpu.md` §registers; `n64brew_wiki/markdown/VR4300.md` §Load Delay Interlock
**Estimated complexity:** L

---

### T-11-002 — Integer ALU, shifts, and multiply/divide

**Description:** implement the arithmetic, logical, shift, and multiply/divide families across
both 32- and 64-bit forms, with the correct sign-extension rules for the 32-bit operations.

**Acceptance criteria:**

- [ ] `ADD`/`ADDU`/`SUB`/`SUBU`/`DADD`/`DADDU`/`DSUB`/`DSUBU` with overflow trapping where the
      instruction specifies it.
- [ ] All logical and shift families including the `D*` 64-bit and `*32` variants.
- [ ] `MULT`/`MULTU`/`DIV`/`DIVU` and the `D*` forms writing `HI`/`LO` with the right latency.
- [ ] 32-bit results are sign-extended into the 64-bit register as hardware does.
- [ ] Unit tests per family in both widths.

**Dependencies:** T-11-001
**Reference:** `docs/cpu.md`; `n64brew_wiki/markdown/MIPS III instructions.md`
**Estimated complexity:** L

---

### T-11-003 — Loads, stores, and the unaligned family

**Description:** implement the load/store set including the unaligned `LWL`/`LWR`/`LDL`/`LDR`
family and the atomic `LL`/`SC`/`LLD`/`SCD`, honouring the endianness and the alignment
exception rules.

**Acceptance criteria:**

- [ ] All widths implemented, signed and unsigned, with correct sign extension.
- [ ] The unaligned family merges partial words exactly, at every byte offset.
- [ ] `LL`/`SC` set and test the link bit, and `SC` reports success correctly.
- [ ] Unaligned access on an instruction that requires alignment raises the address exception
      rather than silently succeeding.
- [ ] n64-systemtest's RAM/ROM/SPMEM/PIF access categories pass at 8, 16, 32, and 64 bits.

**Dependencies:** T-11-002
**Reference:** `docs/cpu.md` §memory; `n64brew_wiki/markdown/SysAD Interface.md`
**Estimated complexity:** L

---

### T-11-004 — Branches, jumps, and the trap family

**Description:** implement the branch, branch-likely, jump, jump-and-link, and `TRAP` families,
plus `BREAK` and `SYSCALL`, each raising the correct exception.

**Acceptance criteria:**

- [ ] Every branch and jump form computes the right target, including the register-indirect and
      the 26-bit region forms.
- [ ] Branch-likely nullifies the delay slot when not taken; ordinary branches do not.
- [ ] `TRAP` conditions, `BREAK`, and `SYSCALL` raise their exceptions with the right cause.
- [ ] n64-systemtest's `TRAP`/`BREAK`/`SYSCALL` categories pass.

**Dependencies:** T-11-003
**Reference:** `docs/cpu.md` §control flow
**Estimated complexity:** M

---

### T-11-005 — The documented VR4300 errata

**Description:** reproduce the CPU's known hardware bugs rather than correcting them: the
multiplication bug, the 32-bit shift-right-arithmetic bug, and the sign-extension bugs.

**Acceptance criteria:**

- [ ] Each documented erratum reproduced, with a named test that fails if it is "fixed".
- [ ] Each test cites `n64brew_wiki/markdown/VR4300.md` so the intent is obvious to the next
      reader.
- [ ] `docs/cpu.md` records each erratum as intended behaviour.

**Dependencies:** T-11-002
**Reference:** `n64brew_wiki/markdown/VR4300.md` §Known Bugs
**Estimated complexity:** M

---

### T-11-006 — First test-ROM run through the harness

**Description:** replace the harness's stubbed completion sentinel so `run_until_complete`
detects the n64-systemtest result protocol, and get the first real pass/fail out of a ROM.

**Acceptance criteria:**

- [ ] `run_until_complete` decodes the real sentinel instead of always returning `Timeout`.
- [ ] The committed `n64-systemtest.z64` runs and reports a genuine pass/fail count.
- [ ] Failures name the failing test, not just a count.
- [ ] `docs/STATUS.md`'s accuracy table gains its first real number.

**Dependencies:** T-11-004
**Reference:** `crates/rustyn64-test-harness/src/runner.rs`; `tests/roms/README.md`
**Estimated complexity:** M

---

### T-11-007 — The determinism regression test

**Description:** close the ADR 0004 gap that `docs/STATUS.md` records as unexercised: two runs
from the same seed and input must produce byte-identical traces.

**Acceptance criteria:**

- [ ] A test runs the same ROM twice from one seed and compares the full trace byte for byte.
- [ ] The test fails if any wall-clock, OS entropy, or iteration-order dependency is introduced.
- [ ] `docs/adr/0004-determinism-contract.md` and `docs/STATUS.md` are updated to say the
      contract is exercised rather than merely specified.

**Dependencies:** T-11-006
**Reference:** `docs/adr/0004-determinism-contract.md`
**Estimated complexity:** M

---

## Sprint review checklist

- [ ] All tickets checked off or explicitly deferred (with reason).
- [ ] n64-systemtest reports a real number for the integer categories.
- [ ] CHANGELOG.md updated.
- [ ] `docs/cpu.md` updated in the same change as the code it describes.
