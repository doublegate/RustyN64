# Accuracy ledger — RustyN64

**References:** ADR 0005 (what defers here), ADR 0006, ADR 0007;
`ref-docs/2026-07-20-vr4300-timing-supplement.md` (the undocumented-constants list);
`docs/testing-strategy.md`; `docs/engineering-lessons.md` §3.3.

## What this file is for

Three things, and nothing else:

1. **Measured constants** — numbers the hardware documentation does not supply, which we fitted
   from test ROMs. Each records *how* it was measured, so it is falsifiable.
2. **Open residuals** — known-wrong behaviour we have chosen to document rather than point-fix.
3. **Ruled-out approaches** — attempts that failed, with the reason, so nobody rediscovers them.

The rule that gives this file its value: **an entry here is honest, a per-quirk patch is not.**
When a ROM fails and the fix would be a special case, the entry goes here instead (ADR 0005).

Equally: **a measured constant is never adjusted to make a specific ROM pass.** The moment a
number is tuned rather than measured, every later timing result built on it becomes
unfalsifiable — the whole suite silently stops being evidence. If a constant looks wrong, measure
it again and say so; do not nudge it.

## Status

**Empty of results.** No chip executes instructions yet (`docs/STATUS.md`), so nothing has been
measured and no residual has been observed. The entries below are *placeholders with known
provenance* — constants we already know we will have to measure, recorded now so the first
implementation does not silently invent a value and move on.

---

## 1. Measured constants

| # | Constant | Value | How measured | Status |
| --- | --- | --- | --- | --- |
| C-1 | `M` — memory access time (PCycles) | — | — | **not yet measured** |
| C-2 | Exception epilogue cost (PCycles) | — | — | **not yet measured** |
| C-3 | CP0I (CP0 bypass interlock) cost | — | — | **not yet measured** |
| C-4 | RDRAM row-hit / row-miss / dirty-miss | — | — | **not yet measured** |

### C-1 — `M`, memory access time in PCycles

The single most load-bearing unknown. Both documented cache-miss formulas are parameterised on
it: D-cache fill = **8–9 + M**, I-cache fill = **14–15 + M** (UM Tables 11-1/11-2). No source
gives a value.

Informal hints, all explicitly hedged by their authors and none usable as a number: RDRAM "about
10-20+ clock wait time"; RCP registers "5-6 PClock cycles"; MI registers "about 2"; RSP
DMEM/IMEM "4-5".

For scale, the reference emulators collapse the whole access into one constant and **disagree**:
CEN64 charges 38 PClocks for an uncached word, 44 for a D-cache fill, 48 for an I-cache fill
(under the source comment `// Currently using fixed values....`); ares charges 40 for a D-cache
fill. Neither derived theirs from a spec. Note CEN64's 44 = 8 + 38, which is consistent with the
Table 11-1 sum plus its own word delay — weak corroboration that the formula reading is right.

`M` is almost certainly **not a single number** — it should vary with target region (RDRAM vs
RCP register vs SP memory vs cart) and with RDRAM bank state (C-4). Expect a small table, not a
scalar. Recording it as a scalar first is acceptable; recording it as a scalar *permanently* is
how a fitted constant becomes a fudge factor.

**Owner:** T-11-008.

### C-2 — exception epilogue cost

Widely quoted as 2 PCycles. **Not documented**: no figure appears in UM §4.7 or chapter 6. The
number originates in CEN64, whose own source says: *"TODO: Is the cycle count just the killing of
IC/RF, or do we actually delay an additional two cycles?"* Treat 2 as a starting hypothesis to
be confirmed, not as a fact inherited from the manual.

### C-3 — CP0I

Named in UM Table 4-3 and §4.6.9 as one of the eight interlocks; no cycle count located in the
manual text. Low priority until COP0 lands (Phase 1 Sprint 2), and note that n64-systemtest's
`cop0hazard` set is default-off upstream because the rules are not fully derived by anyone.

### C-4 — RDRAM bank state

`RDRAM Interface.md` documents row-open/Ack, row-miss/NAck-close-and-reload, and "takes even
longer if the current row is dirty" — qualitatively, with no cycle counts. The programmable
timing registers (`RasInterval`: `RowPrecharge`/`RowSense`/`RowImpRestore`/`RowExpRestore`;
`Delay`: `AckWinDelay`/`ReadDelay`/`AckDelay`/`WriteDelay`) are documented bitwise but the values
IPL3 programs are not translated into cycles. Interacts with C-1.

---

## 2. Open residuals

| # | Symptom | Suspected mechanism | Classification | Status |
| --- | --- | --- | --- | --- |
| — | none yet | — | — | — |

Every entry must carry a **classification** of the failing measurement as **absolute** or
**differential** before any mechanism is proposed (ADR 0005, `engineering-lessons.md` §1.3). A
differential measurement — the interval between two events on the same clock — is invariant
under uniform re-phasing, so an entire family of plausible fixes is ruled out for free. A sibling
project implemented and rolled back five successive re-phasings before recognising this. One line
here saves that.

---

## 3. Ruled-out approaches

| # | Approach | Applied to | Why it cannot work | Date |
| --- | --- | --- | --- | --- |
| — | none yet | — | — | — |

Record an approach here after **two** rollbacks, not after five (`engineering-lessons.md` §3.3).
An unrecorded dead end gets rediscovered by the next person, or by the same person in six months.

---

## 4. Contradictions in the sources

Not our bugs, but they will look like ours if undocumented.

| # | Contradiction | Sources | Resolution |
| --- | --- | --- | --- |
| S-1 | `SYSCMD` bit 4 polarity: command = 0 or 1? | UM §12.11.1 says command = `SysCmd4` **0**; `SysAD Interface.md` cheat sheet says **1** | **unresolved** — pin with a test ROM before encoding either |
| S-2 | Pipeline stage names | `ref-docs/research-report.md` §1 says IF/RF/EX/DF/WB; UM §4.1 Fig 4-1 says IC/RF/EX/DC/WB | **resolved** — manual wins; see `ref-docs/2026-07-20-vr4300-timing-supplement.md` §1 |
| S-3 | Exception vector for an exception with `EXL` already set | MIPS docs say `0x80`; CEN64 routes to `0x180` with a source comment that `0x80` "doesn't make any sense" | **unresolved** — pin with n64-systemtest |
| S-4 | D-cache fill cost | CEN64 charges 44 PClocks; ares charges 40 | **unresolved** — neither is spec-derived; supersede both with C-1 |

---

## 5. Deliberate deviations from hardware

Behaviour we model differently *on purpose*, so it is never mistaken for a bug.

| # | Deviation | Why | Bounded by |
| --- | --- | --- | --- |
| D-1 | Power-on CPU/RCP phase comes from a seeded PRNG, not from real indeterminacy | The determinism contract requires reproducibility; the hardware's own indeterminacy is documented (UM Table 11-1's "1 to 2 PCycles: synchronize with SClock") and is modelled as a *parameter* rather than eliminated | ADR 0004, ADR 0006 |
| D-2 | The VR4300 errata are **reproduced**, not fixed | They are observable behaviour that software depends on; `sra`/`srav` in particular affects every console | ADR 0007; pinned by named tests that fail if "corrected" |
