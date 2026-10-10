# 0020 — Amend 0016: accept the RSP SIMD exception, for `multiply_lane` only

Status: **Accepted.** ADR 0016 wrote the exception down and recommended against
using it; this amends that recommendation to an acceptance, on a ceiling that is
**smaller** than the one 0016 declined and a frame that is **half** the size.
Date: 2026-08-02
Deciders: repo owner
Supersedes: none · Superseded by: none
Amends: **ADR 0016** — its four gates are carried forward unchanged and are the
operative requirements. Only its recommendation changes.

## Context

ADR 0016 defined a narrow `unsafe` exception — `core::arch` intrinsics in
`crates/rustyn64-rsp/src/vu.rs` only — and then declined to use it, because the
census put a perfect vectorization of `multiply_lane` at **5.3% of a frame /
1.056x**, below the 1.5x bar ADR 0017 used to decline a CPU recompiler at
1.26–1.40x.

**Two things have changed, and only one of them favors this.**

**Against it: the ceiling was re-derived by measurement and came out lower.**
0016's 5.3% was `62% x 8.5%` — an operation-count share multiplied by a time
share. Measured by doubling `multiply_lane` (`docs/performance.md`
§*`multiply_lane` measures 2.6–2.7% of a frame*), one pass is 1.33–1.39 ms:
**2.6–2.7%**, about half what 0016 declined. The multiply/accumulate family is
among the *cheapest* work the VU does, so 61.6% of the operations is well under
61.6% of the time.

**For it: the frame halved, so the same absolute cost is a larger share.** The
idle-loop skip, the fast commit and the VI coverage memo removed CPU and VI cost;
they removed no RSP cost. 1.36 ms of a **~31.6 ms** frame is **~4.3%**, a
**1.045x** ceiling.

**So the honest summary is that this is a worse technique than 0016 thought,
applied to a frame where it matters more.** 1.056x declined, 1.045x accepted.
The number did not improve; the context did.

## Decision

**Use the exception, for `multiply_lane` and nothing else.**

ADR 0016's four gates are carried forward **unchanged and unrelaxed**, and the
first of them is the one that matters:

1. **A scalar/vector equivalence test over the operand space**, not conformance
   to the ROM suite. The ROM suite passing is not evidence: it exercises what
   games use, and the whole risk of an intrinsic is the operand it handles
   differently.
2. The scalar implementation stays, compiled and tested, as the reference.
3. Runtime dispatch, with the scalar path taken when the feature is absent.
4. Measured A-B-A on a real workload, and reverted if it does not clear its
   ceiling by a margin worth the exception.

**Scope is `multiply_lane` and its callees only.** Nothing else in `vu.rs`, and
nothing outside it. A second site needs a new ADR, not an appeal to this one.

**`crates/rustyn64-rsp` drops from `forbid(unsafe_code)` to `deny`.** ADR 0016
spelled out that this is a real weakening and it remains one; `deny` still
requires an explicit per-site `#[allow]`, so every intrinsic block is visible in
review and carries a `// SAFETY:` comment naming the invariant.

## Consequences

**Gained:** at most ~4.3% of a frame — 1.045x — and realistically less, because
the dispatch, the register reads and the accumulator writeback do not vanish.
**If the measurement in gate (4) does not show most of that, this is reverted and
this ADR is superseded rather than argued with.**

**Given up:** `rustyn64-rsp`'s `forbid(unsafe_code)`, which was a property the
whole chip layer shared and now is not. That is the actual price, and it is paid
once for every future reader of that crate, not once for this change.

**What would make this a mistake, stated in advance:** if the equivalence test in
gate (1) turns out to be hard to write exhaustively over an 8-lane 16x16 product
space, that is not a reason to weaken it to sampling — it is a reason to stop.
The scalar path is correct and 4.3% is not worth an unproven one.
