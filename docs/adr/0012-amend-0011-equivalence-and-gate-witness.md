# 0012 — Amend 0011: byte-identity is about execution, and the differential gate needs a completion witness

Status: **Proposed** — accepted on merge of the PR that introduces this file;
immutable thereafter.
Date: 2026-07-30
Deciders: repo owner
Supersedes: none · Superseded by: none
Amends: ADR 0011 (optional fast-path scheduler) — corrections, not a change of
decision. 0011's decision stands in full.

## Context

ADR 0011 was merged as `a40852b` **while a review of it was still landing**. The
review's findings do not stop being right for having arrived late, and two of them are
substantive. Because 0011 declares itself immutable on merge, they cannot be edited in;
this ADR carries them instead.

That is the whole reason this document exists, and it is worth recording plainly: the
cost of merging before a review arrives is a second document for what would otherwise
have been two lines.

## Decision

### 1. "Byte-identical" is scoped to execution and AV output, not to the save-state container

ADR 0011 asserts both of the following, and they cannot both hold:

> **Default builds are byte-identical to today.** […] with it disabled the shipped
> binary must contain no behavior change whatsoever.

> […] the state header must record which mode produced it […] The header gains a
> scheduler-mode field behind a version bump.

A new header field changes the bytes a **default build writes**. The contradiction was
introduced in 0011's own revision history while adopting a reviewer suggestion about
save-state versioning, without re-checking it against the byte-identity claim a few
lines above.

**The claim is hereby narrowed.** With `fast-scheduler` disabled:

- **emulated execution** is unchanged — same instruction stream, same timing, same
  `master_ticks` progression;
- **audio and video output** are byte-identical, which is what ADR 0004's determinism
  contract is actually about;
- the **save-state container is not** byte-identical: it gains the mode field and a
  version bump.

Save-state *loading* compatibility is unaffected and remains as 0011 specifies: an
absent marker reads as accurate-mode, so every pre-existing state still loads in a
default build.

This is a correction to an over-broad claim, not a relaxation of a guarantee. The
guarantee anyone depends on — that turning the feature off gives them today's emulator
— is intact and now stated in terms that are true.

### 2. The differential gate must witness its own completion

ADR 0011 requires the gate to compare architectural state across both schedulers and to
force each bailout boundary, but says nothing about how the gate reports having
finished. Without that, **a gate that hangs mid-suite is indistinguishable from a gate
that passed**, because both produce no failure.

This project has paid for that exact shape twice in recent memory: an en-US spelling
gate that printed `passed: 0 files` and exited 0 with a completely broken file listing,
and a performance probe that reported `ok` having measured nothing at all. A
differential gate is a longer-running instance of the same hazard.

The gate must therefore:

- **emit an explicit end-of-suite marker** naming how many boundary fixtures and
  comparison points actually ran, and fail if that count is zero or below the expected
  number — a run that compared nothing must not read as agreement;
- **carry a timeout** per fixture and for the suite, so a hang fails rather than
  hanging;
- **define its failure conditions**, including "the fast path never engaged" and "no
  bailout boundary was reached", both of which would otherwise look like success.

The witness is asserted, not printed for a human to notice.

### 3. Editorial

0011's save-state-compatibility heading carries a comma before "because" that should not
be there. Recorded here rather than fixed in place, since 0011 is immutable — and noted
mainly so the discrepancy is not mistaken for a quotation error later.

## Consequences

**Good**

- The byte-identity guarantee is now checkable. As written in 0011 it was false, and a
  false guarantee is worse than a narrower true one — someone would eventually have
  diffed two save-states and concluded the feature was leaking into default builds.
- The differential gate cannot silently pass without doing work, which matters more for
  this gate than most: it is the *only* thing standing between a fast path and shipped
  divergence.

**Bad, and accepted**

- Two ADRs must now be read together to understand one decision. Mitigated only by
  cross-links in both directions and by keeping this document to corrections.

**Not changed**

Everything else in 0011: the default-off `fast-scheduler` feature, the accurate
scheduler as differential oracle, state-based rather than pixel-based equivalence, the
bailout invariant on `master_ticks` and latches, the block-based mechanism, and the
permission to bail out to the accurate path for anything unhandled.

## Process note

The generalizable lesson, since this is the second time a review has landed after a
merge in this repository: re-triggering a review and then polling once is not waiting
for it. A merge is safe only once the review has actually arrived and every thread is
adjudicated — for an **immutable** document, that is the difference between an edit and
an extra ADR.
