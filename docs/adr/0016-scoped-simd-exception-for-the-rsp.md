# 0016 — A scoped `unsafe` exception for the RSP vector unit, and the evidence required to use it

Status: **Proposed** — accepted on merge of the PR that introduces this file;
immutable thereafter.
Date: 2026-08-01
Deciders: repo owner
Supersedes: none · Superseded by: none
Amends: the `unsafe` policy stated in `AGENTS.md`
("`unsafe` is allowed only in the frontend and FFI"), narrowly and
conditionally. **`AGENTS.md` is updated in the same change**, so the tree does
not carry the old blanket rule alongside this exception. `docs/architecture.md`
is *not* amended — it states no `unsafe` policy; an earlier draft of this line
claimed it did, which a reviewer caught.

## Context

`crates/rustyn64-rsp` carries `#![forbid(unsafe_code)]`, as every chip crate
does. That is not incidental: `docs/architecture.md` makes zero `unsafe` in the
chip crates a property of the design, and the tree has never had any.

The vector unit is the largest single hot spot outside the CPU. Two measurements
bound what is at stake, and both are already merged:

- **The VU is ~8.5% of a frame** (`docs/performance.md`, the `fast-exec`
  profile).
- **62% of the VU's computational work is one function.** The COP2 census ([#250](https://github.com/doublegate/RustyN64/pull/250))
  found that `funct 0x00..=0x0F` — the multiply / multiply-accumulate family — is
  **61.62%** of 14.6 million executed operations, dispatched by `multiply_lane`.
  Only 32 of 64 `funct` values ever appear, and four operations are half the work.

**So the ceiling on vectorizing the VU is about 5.3% of a frame** (62% of 8.5%),
and the real figure is lower, because the dispatch, the register reads and the
accumulator writeback do not vanish.

Two cheaper things were tried first and are recorded as negative results:

- **Hoisting the family's dispatch out of the lane loop**: built, verified
  accurate, and **measured neutral** — `docs/performance.md`, *"The VU family
  hoist: built, accurate, and measured NEUTRAL"*. Reverted under the standing
  rule. Status: **measured**.
- **A decode cache**: **0.29%** for a *perfect* one — same document, the decode
  sizing above that section. Status: **an inferred upper bound**, not a
  measurement of an implementation, because none was built.

`core::simd` is still unstable on the pinned toolchain (exact 1.96.0, held for
libretro build reproducibility), so a safe portable-SIMD path does not exist.
That leaves `core::arch` intrinsics, which are `unsafe`.

## Decision

**Permit `unsafe` in `crates/rustyn64-rsp`, for SIMD intrinsics only, and only
once the conditions below are met.**

This ADR **authorizes a technique; it does not schedule the work**, and it does
not lower the bar for it. Nothing in the crate changes until the conditions are
satisfied.

### The scope of the exception

1. **`core::arch` intrinsics only.** Not raw pointers, not `transmute`, not
   `get_unchecked`, not FFI. If a change wants `unsafe` for anything other than
   a vendor intrinsic, this ADR does not cover it.

   **This one is not compiler-enforceable and must not be written as if it
   were.** `unsafe_code` is a binary lint: it cannot permit an intrinsic call
   and reject a `transmute` in the same module. Restriction 1 is therefore
   enforced by **review**, and the narrow module scope in (2) is what makes that
   review tractable — a reviewer reads one file, not a crate. A CI check that
   greps `vu.rs` for `unsafe` blocks whose body is not a `core::arch` call would
   strengthen it and is worth adding if the exception is ever used.

2. **`vu.rs` only**, and precisely:

   - the crate-level attribute changes from `#![forbid(unsafe_code)]` to
     `#![deny(unsafe_code)]` — necessary because `forbid` cannot be overridden
     by an inner `allow`, which is the whole point of `forbid`;
   - `vu.rs` alone carries `#![allow(unsafe_code)]` with this ADR cited;
   - **every other module is then `deny`, not `forbid`**, which is a real and
     deliberate weakening: `deny` can be locally overridden and `forbid` cannot.
     That is the price of the exception and it is why (1) is enforced by review
     — the compiler stops being the thing that guarantees it.
3. **Every `unsafe` block *or operation* carries a `// SAFETY:` comment** naming
   the invariant and who guarantees it — the existing rule quoted in full, not a
   new one. "Block" alone would leave an `unsafe` operation inside an `unsafe
   fn` unexplained, which is the case most likely to arise here: intrinsics are
   often called from a `#[target_feature]` function, and those are `unsafe fn`.
4. **A safe scalar path remains, compiled and tested.** The intrinsics are an
   alternative implementation, never the only one. Targets without the required
   feature use the scalar path, and it stays exercised in CI rather than
   bit-rotting behind a `cfg`.

### The conditions for using it

**These are gates, not aspirations. A PR that adds `unsafe` here without all
four is incomplete.**

1. **Equivalence, not just conformance.** A test asserting that the vectorized
   path and the scalar path produce **identical** output — every lane, the
   accumulator, and every flag — over the operand space, including the edge cases
   the VU is notorious for (saturation, the `VMUDL`/`VMUDN` unsigned/signed
   asymmetry, accumulator overflow wrap). Conformance to the ROM suite is
   necessary and **not sufficient**: the suite exercises what microcode happens
   to use, and a divergence outside that set is exactly the bug this ADR's
   technique invites.
2. **`rsp_categories_report_no_failures` stays at `0 failing`** across 224 RSP
   tests ([#238](https://github.com/doublegate/RustyN64/pull/238)). Non-negotiable.
3. **Measured against the 5.3% ceiling, A-B-A, on a real workload**, by
   `docs/performance.md`'s standing rules — interleaved legs, conservative
   pairing, outliers named. **A neutral or worse result is reverted**, exactly as
   the family hoist and PGO were.
4. **A named portability matrix, all of it built and tested** — "a tested
   fallback" was too vague to hold anyone to:

   | target | what must pass |
   | --- | --- |
   | `x86_64`, SSE2 only | the vectorized path, since SSE2 is the baseline |
   | `x86_64`, SSSE3/SSE4.1 present | whichever wider path is added |
   | `aarch64` | NEON path, or the scalar fallback |
   | `thumbv7em-none-eabihf`, `--no-default-features` | **the scalar path**, and this is the hard one |

   The embedded target is the constraint that decides the design: the chip
   stack must keep building `no_std + alloc` there, and it has no SIMD at all.
   So the scalar path is not a courtesy fallback — it is a first-class
   implementation that a supported target depends on, which is why gate 1 is
   equivalence between the two rather than conformance of the fast one.

   Note the structural cost, because it shapes the code rather than decorating
   it: runtime dispatch means `#[target_feature]` functions, which are
   themselves `unsafe` to call, do not inline across the boundary, and force the
   dispatch decision out of the hot loop and up to a level where it is amortized.
   A design that checks a feature flag per instruction has spent the win before
   it starts.

### What this does not do

- It does **not** extend to any other chip crate. `rustyn64-cpu`,
  `rustyn64-rdp`, `rustyn64-cart`, `rustyn64-audio`, `rustyn64-core` and
  `rustyn64-snapshot` keep `forbid(unsafe_code)`.
- It does **not** authorize an RSP dynarec. That is a different technique with a
  different risk profile and needs its own ADR.
- It does **not** commit anyone to doing the work. If a policy-compliant
  restructure reaches the same place, that is strictly preferable and this
  exception goes unused.

## Consequences

### Positive

- The largest remaining non-CPU hot spot becomes addressable, with a ceiling
  that is known (5.3%) rather than assumed (8.5%).
- The scope is narrow enough to audit: one module, one class of intrinsic, a
  grep for `unsafe` in `crates/rustyn64-rsp` returns either nothing or `vu.rs`.
  That auditability is what has to carry the weight, since restriction (1) is
  not compiler-enforced.
- The equivalence gate is reusable for any future alternate implementation of
  the same operations, including a dynarec.

### Negative / costs

- **The tree stops having zero `unsafe` in the chip crates**, which was a
  property worth something on its own — it made "is this crate memory-safe?" a
  question with a one-word answer.
- **`rustyn64-rsp` drops from `forbid` to `deny`**, so the guarantee for its
  other modules becomes one a future edit can locally override rather than one
  the compiler refuses. That is a strictly larger weakening than "`vu.rs` may
  use intrinsics", and it is the part most likely to be forgotten later.
- Vendor intrinsics are per-architecture, so the VU acquires a portability
  surface it did not have. The scalar fallback is the mitigation and is also a
  maintenance burden: two implementations that must agree, forever, which is why
  the equivalence test is condition 1 rather than a nicety.
- A 5.3% ceiling against that cost is a genuinely marginal trade, and this ADR
  does not pretend otherwise. It is recorded as **authorized**, not as
  **recommended**.

### The honest comparison

The CPU is **32.29%** of a frame — roughly six times this ceiling — and a
recompiler there is the only remaining change that can move the frame rate
materially (`docs/performance.md`). Anyone reaching for this ADR should first
have a reason not to spend the same effort on that instead.

## Notes for whoever implements this

`multiply_lane` is **per-lane**: `vu_compute` loops `for lane in 0..8` and
dispatches inside the loop. Vectorizing therefore means restructuring, not
annotating — the 48-bit accumulator (`[u64; 8]`) has no native vector type and
needs splitting into 16-bit planes, which is how `parallel-rsp` does it. Read
`ref-proj/parallel-rsp/` for the architecture; it is MIT and vendorable, but
prefer understanding it to copying it.
