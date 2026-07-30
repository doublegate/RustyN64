# 0012 — Amend 0011: byte-identity is about execution, and the differential gate needs a completion witness

Status: **Proposed** — accepted on merge of the PR that introduces this file;
immutable thereafter.
Date: 2026-07-30
Deciders: repo owner
Supersedes: none · Superseded by: none
Amends: [ADR 0011](0011-optional-fast-path-scheduler.md) (optional fast-path
scheduler) — corrections, not a change of decision. 0011's decision stands in full.

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

ADR 0011 asserts both of the following, and they cannot both hold. First, in its
Decision:

> **Default builds are byte-identical to today.** […] with it disabled the shipped
> binary must contain no behavior change whatsoever.

Then, nineteen lines later, in the same Decision:

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
  number — a run that compared nothing must not read as agreement. The expected number is
  **not a hand-maintained constant**: it is the length of the enumerated bail-out set the
  fast path itself declares, so adding a bail-out reason without a fixture that reaches it
  fails the gate, and no one has to remember to bump a total. A gate whose expected count
  is a literal drifts the moment the suite grows, which converts the witness into
  decoration.

  For that enumeration to mean anything, **bailing out must not be expressible any other
  way**: the fast path's only exit to the accurate scheduler is a typed reason — one enum,
  returned through one signature — so an ad-hoc early return in execution logic is a
  compile error rather than an uncounted path. The gate then matches that enum
  exhaustively, which makes "a new reason arrived with no fixture" a build failure rather
  than a silent coverage hole. An enumeration that code can bypass is a list of the cases
  someone remembered;
- **carry a timeout** per fixture and for the suite, so a hang fails rather than
  hanging;
- **define its failure conditions**, including "the fast path never engaged" and "no
  bailout boundary was reached", both of which would otherwise look like success.

The witness is asserted, not printed for a human to notice.

### 3. Editorial

0011's save-state-compatibility heading carries a comma before "because" that should not
be there. Recorded here rather than fixed in place, since 0011 is immutable — and noted
mainly so the discrepancy is not mistaken for a quotation error later.

### 4. The pointer line is the one part of an immutable ADR that still changes

0011 gains an `Amended by: ADR 0012` marker in its header, and nothing else. That is not a
breach of its immutability: the header's `Superseded by:` field cannot be filled in by the
document that carries it, so in this repository the pointer line has always been written
after the fact — [ADR 0001](0001-master-clock-lockstep-scheduler.md)'s Status section names
ADR 0006, which did not exist when 0001 was accepted. Immutability protects the
**reasoning**, which is what later readers cite; a reader who arrives at 0011 and is not
told 0012 exists has been misled by the omission, not protected by it.

### 5. 0011's measured table is superseded by the paired re-measurement

Every figure in 0011's *"The measurements this decision rests on"* was taken as a single
run, and several were later found not to pair: its **7.7x** debug ratio divided a debug
frame cost by a release figure from a **different window** (before the VI is programmed
versus after). `docs/performance.md` §Measured now carries two-run paired figures for all
of them, and it is authoritative where the two disagree:

"Pre-fix" and "post-fix" below refer to commit `646a3e0`, which skips the zero-weight
bilinear taps in the VI scan-out and is the only change between the two trees measured.

| 0011 says | paired measurement says |
| --- | --- |
| frame cost 150.5 ms → 6.6 FPS | 155.1 ms → 6.44 FPS (pre-fix), 139.3 ms → 7.18 FPS (post) |
| scan-out 35.5 ms, 23.6% of a frame | 35.5 ms, 22.9% (pre-fix); 21.6 ms, 15.5% (post) |
| debug is 7.7x slower | **8.7x**, paired on one tree and one window |
| required speedup ~9x | ~8.3x from 139.3 ms |

**The decision does not move.** That is the point of recording the drift rather than
quietly correcting it: 0011's argument is that ~9x is unreachable inside a per-cycle model
whose in-model ceiling is ~1.66x, and 8.3x against 1.66x is the same conclusion. A
measurement that changes the numbers without changing the decision is worth writing down
precisely because the reverse would have invalidated the ADR.

## Consequences

### Good

- The byte-identity guarantee is now checkable. As written in 0011 it was false, and a
  false guarantee is worse than a narrower true one — someone would eventually have
  diffed two save-states and concluded the feature was leaking into default builds.
- The differential gate cannot silently pass without doing work, which matters more for
  this gate than most: it is the *only* thing standing between a fast path and shipped
  divergence.

### Bad, and accepted

- Two ADRs must now be read together to understand one decision. Mitigated only by
  cross-links in both directions and by keeping this document to corrections.

### Not changed

Everything else in 0011: the default-off `fast-scheduler` feature, the accurate
scheduler as differential oracle, state-based rather than pixel-based equivalence, the
bailout invariant on `master_ticks` and latches, the block-based mechanism, and the
permission to bail out to the accurate path for anything unhandled.

## Process note

The generalizable lesson is recorded where this repository keeps them, as
`docs/engineering-lessons.md` §4.4 — re-requesting a review and then checking once is not
waiting for it. It is referenced rather than argued here so that this document stays a
specification.
