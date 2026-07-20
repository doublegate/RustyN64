# ADR 0002 — LLE coprocessors (RSP + RDP)

## Status

Accepted.

Originally filed as `0002-fractional-timebase-refactor.md`, carrying both this decision and a
deferred sub-cycle timebase refactor under one number with two different statuses. The refactor
is split out to [ADR 0005](0005-sub-cycle-bus-timing-refactor.md) and this ADR renamed to match
its actual subject, because the two were being cited interchangeably —
`docs/architecture.md` cited "ADR 0002" for the LLE decision while `docs/STATUS.md` cited it for
the timebase, and both were correct.

## Context

The N64's defining emulation choice is **Low-Level vs High-Level Emulation** of the programmable
coprocessors (`ref-docs/research-report.md` §3, §Architecture options). The RSP is a general
MIPS+SIMD core that runs game-supplied **microcode**, so two games can use entirely different
rendering and audio pipelines.

- **HLE** recognises a microcode binary by signature or CRC and substitutes a native
  reimplementation of its graphics or audio task. Fast — the Project64-plus-plugins norm — but
  **per-game fragile**: it breaks on unknown or custom microcode, mis-renders mid-frame
  coprocessor tricks, and needs perpetual per-game maintenance.
- **LLE** executes the actual RSP instruction stream (scalar + vector ISA) and rasterises the
  actual RDP command list. It is the accuracy bar — ares, CEN64, Gopher64, and ParaLLEl-RSP/RDP
  are all LLE — and the only path that reproduces custom microcode and the bit-exact framebuffer
  the test ROMs demand (§3, §4, §State of the art).

The custom-microcode problem is not hypothetical. Factor 5 shipped their own microcode in
*Star Wars: Rogue Squadron*, *Battle for Naboo*, and *Indiana Jones and the Infernal Machine*;
Boss Game Studios did the same in *World Driver Championship* and *Top Gear Rally 2*. Those are
the titles HLE plugins historically could not run, and all five are staged in the commercial
corpus specifically so this decision can be proven rather than asserted
(`tests/roms/external/commercial/README.md`).

## Decision

RustyN64's core contract is **LLE RSP + LLE RDP**. The RSP executes the instruction stream; the
RDP rasterises the real command list through a faithful per-pixel pipeline.

- **Audio comes for free.** The RSP audio microcode runs on the same LLE RSP core, so there is no
  per-game audio HLE and no separate audio DSP to write (`docs/audio.md`, §5). If audio is wrong
  and the AI is right, the bug is in the RSP.
- **The RDP reference is software-first.** Ship a pure-Rust **software** LLE RDP as the
  always-correct reference renderer and the fuzz-suite gate. A **wgpu-compute** backend comes
  later, validated *against* that software reference, and never replaces it as the oracle
  (§4, §Architecture options B; `docs/rdp.md`, `docs/performance.md`).
- **v0.1 stubs the LLE behind the chip traits.** `Rsp::tick` and `Rdp::tick` are LLE-shaped no-ops
  with decode/execute marked TODO (`docs/STATUS.md`). The scheduler already co-schedules them on
  one timeline (ADR 0001), so filling them in is additive.
- **An HLE fast path may be added later behind an off-by-default feature flag** for speed — never
  the default, never the accuracy oracle.

### Clean-room, and the actual licence position

Implement from primary documentation — the R4300i datasheet, the N64brew Wiki, Dillon's
`n64-resources`, and the test ROMs. RustyN64 is MIT OR Apache-2.0, so the reference
implementations are **study / validate-against** references, and their conformance suites are the
spec.

The per-repo terms matter, and were recorded incorrectly in the original version of this ADR,
which claimed "Angrylion-Plus is GPL and ParaLLEl-RDP is per-repo". Both were wrong, and the
angrylion error *understated* the restriction. Verified against the clones in `ref-proj/`:

| Reference | Actual licence | What that permits |
|---|---|---|
| **angrylion-rdp-plus** | **MAME licence** — ships `MAME License.txt` and **no `LICENSE` file at all**; `CREDITS.txt` states "The code comes under MAME license" | **Non-commercial.** Stricter than the GPL, and incompatible with MIT OR Apache-2.0 in *both* directions. Read to understand behaviour, then write your own — which the MAME licence text itself instructs. Compare outputs, never source. |
| **ParaLLEl-RDP** | **MIT** (`Copyright (c) 2020 Themaister`) | Permissive. Vendorable with attribution, though the value here is its ~150-test fuzz suite as a bit-exactness oracle rather than its source. |
| **CEN64** | BSD-3-Clause | Permissive. The closest reference for bus-level timing behaviour. |
| **ares** | ISC | Permissive; the one full-system reference that may genuinely be vendored from. |

The missing `LICENSE` file in angrylion-rdp-plus reads as unlicensed-and-therefore-free and is
the single most dangerous misreading available in `ref-proj/`. It is also the reference software
rasteriser the RDP will be graded against, so it is exactly the tree someone will want to open
while chasing a bit-exactness failure. Full per-repo classification is in `ref-proj/README.md`.

## Consequences

### Positive

- Custom microcode renders correctly without per-game HLE; mid-frame coprocessor effects are
  faithful; the test-ROM oracle is achievable at all.
- Audio is correct by construction — it is just more microcode.
- No copyleft or non-commercial entanglement: clean-room under MIT OR Apache-2.0.
- The decision is falsifiable. If the five Factor 5 and Boss Game Studios titles render, LLE did
  what it was chosen for; if they do not, the RSP is wrong. That is a better test than any
  self-authored probe.

### Negative / costs

- Substantially more work than HLE, and slower. The single-threaded RSP-plus-CPU path is the
  expected bottleneck, addressed later by a dynarec rather than by weakening the model
  (`docs/performance.md`).
- The software RDP must be bit-exact against the Angrylion reference output before any compute
  backend can be trusted, and "exact match" is the stated bar (§4).
- The RDP command surface is combinatorially large. "Works on this ROM" is not coverage, which is
  why the ParaLLEl-RDP fuzz suite rather than visual inspection is the gate.

### Risks

- **The oracle is a licence hazard.** Grading against angrylion's *output* is fine; reading its
  source to fix a mismatch is not — and that temptation peaks exactly when a bit-exactness
  failure is most frustrating.
- **A fast GPU backend can quietly become the oracle.** Once the accelerator is faster and looks
  right, grading against it is tempting. The software reference stays the definition of correct.
- **HLE re-enters through the back door.** A "temporary" signature-matched fast path for one
  stubborn title would silently reintroduce per-game fragility. Phase 8 is explicitly not a
  loophole for this (`to-dos/phase-8-reach/overview.md`).
