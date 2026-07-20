# Sprint 1 — The accuracy battery and corpus triage

**Phase:** Phase 7 — Accuracy breadth
**Sprint goal:** the accuracy battery stops being a stub and starts reporting a real pass rate,
and every commercial title in the corpus is run and classified — fixed, ledgered, or deferred.
**Estimated duration:** 4 weeks

## Tickets

### T-71-001 — Author the accuracy battery probes

**Description:** replace the stubbed `AccuracyScorer` with named probes covering the behaviours
the test ROMs and hardware documentation define, reporting a real `AccuracyReport`.

**Acceptance criteria:**

- [ ] Probes are named, individually reportable, and grouped by subsystem.
- [ ] Each probe derives from hardware documentation or a reference emulator, never from our own
      output — a probe written against our captures measures agreement with ourselves.
- [ ] The report gives a pass rate plus a per-probe breakdown.
- [ ] `docs/STATUS.md`'s battery row reports a real percentage.

**Dependencies:** T-61-004
**Reference:** `docs/testing-strategy.md` Layer 4
**Estimated complexity:** L

---

### T-71-002 — Run the commercial corpus and classify every failure

**Description:** boot all 66 staged commercial titles, capture boot/title/gameplay frames, and
classify each failure by subsystem and severity.

**Acceptance criteria:**

- [ ] Every title boots or its failure is recorded with the subsystem responsible.
- [ ] Screenshots and `.snap` baselines are committed; the ROMs remain absent.
- [ ] Failures are grouped by root cause rather than by title, so one fix closes many.
- [ ] No per-game special-casing is introduced to make a title pass.

**Dependencies:** T-71-001
**Reference:** `tests/roms/external/commercial/README.md`
**Estimated complexity:** L

---

### T-71-003 — The custom-microcode families

**Description:** drive the Factor 5 titles (Rogue Squadron, Battle for Naboo, Indiana Jones) and
the Boss Game Studios titles (World Driver Championship, Top Gear Rally 2) to correct rendering.

**Acceptance criteria:**

- [ ] All five render correctly through the LLE RSP and RDP, with no microcode-specific code
      path anywhere.
- [ ] The absence of a signature-matching HLE path is verified, not assumed.
- [ ] `docs/compatibility.md` records the result as the concrete payoff of ADR 0002.

**Dependencies:** T-71-002
**Reference:** `docs/adr/0002-fractional-timebase-refactor.md`; `docs/compatibility.md`
**Estimated complexity:** L

---

### T-71-004 — The accuracy ledger

**Description:** create `docs/accuracy-ledger.md`, mapping every known divergence to a
disposition — Remediated, No-stricter-oracle-available, Deferred, or Out-of-scope — as the "why"
companion to `docs/STATUS.md`'s pass counts.

**Acceptance criteria:**

- [ ] Every residual from T-71-002 has an entry and a disposition.
- [ ] Entries deferred to the ADR 0002 sub-cycle refactor say so explicitly.
- [ ] The ledger is linked from `docs/STATUS.md` and the README.
- [ ] Nothing is silently dropped: a residual with no entry is a process failure.

**Dependencies:** T-71-002
**Reference:** `docs/STATUS.md`; `docs/adr/0002-fractional-timebase-refactor.md`
**Estimated complexity:** M

---

### T-71-005 — Region timing as data

**Description:** move NTSC and PAL VI/AI divisors into a table, closing the open question about
exact PAL values, so region is configuration rather than branching.

**Acceptance criteria:**

- [ ] The divisors live in a table with a documented source for each value.
- [ ] No region-conditional branching remains in the timing path.
- [ ] Unit tests cover both regions, and the PAL coverage gap is stated honestly — the corpus is
      USA dumps, so PAL has no commercial regression coverage.

**Dependencies:** T-71-002
**Reference:** `docs/compatibility.md`; `n64brew_wiki/markdown/Video Interface.md`
**Estimated complexity:** M

---

## Sprint review checklist

- [ ] All tickets checked off or explicitly deferred (with reason).
- [ ] The battery reports a real pass rate and the ledger accounts for every gap.
- [ ] CHANGELOG.md updated.
- [ ] `docs/STATUS.md` and `docs/compatibility.md` updated in the same change.
