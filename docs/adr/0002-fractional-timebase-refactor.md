# ADR 0002 — LLE coprocessors (RSP + RDP), with a future sub-cycle timebase

## Status

Accepted (LLE commitment) + Proposed (the future sub-cycle refactor).

This is the most consequential decision in the project. It has two linked parts:
**(A)** the LLE-vs-HLE commitment, decided now; **(B)** the future sub-cycle φ1/φ2
master-clock refactor, designed-for but deferred.

## Context

The N64's defining emulation choice is **Low-Level vs High-Level Emulation** of the
programmable coprocessors (`ref-docs/research-report.md` §3, §Architecture
options). The RSP is a general MIPS+SIMD core that runs game-supplied **microcode**,
so two games can use entirely different rendering/audio pipelines.

- **HLE** recognizes a microcode binary by signature/CRC and substitutes a native
  reimplementation of its graphics/audio task. Fast (the Project64 + plugins
  norm), but **per-game fragile**: it breaks on unknown/custom microcode
  (Factor 5 / Rare / Boss variants), mis-renders mid-frame coprocessor tricks, and
  needs perpetual per-game maintenance.
- **LLE** executes the actual RSP instruction stream (scalar + vector ISA) and
  rasterizes the actual RDP command list. It is the accuracy bar — ares, CEN64,
  Gopher64, and ParaLLEl-(RDP/RSP) are all LLE — and the only path that reproduces
  custom microcode and the bit-exact framebuffer the test ROMs demand
  (`ref-docs/research-report.md` §3, §4, §State of the art).

Separately, the v0.1 scheduler advances at **whole-master-tick** resolution
(ADR 0001). Some hard-tier test ROMs may eventually need **sub-cycle** bus-phase
resolution (φ1/φ2 of a single cycle) that whole-tick lockstep cannot represent.

## Decision

### Part A — LLE is the core (decided now)

RustyN64's core contract is **LLE RSP + LLE RDP**. The RSP executes the
instruction stream; the RDP rasterizes the real command list through a faithful
per-pixel pipeline.

- **Audio comes for free** — the RSP audio microcode runs on the same LLE RSP
  core, so there is no per-game audio HLE (`docs/audio.md`,
  `ref-docs/research-report.md` §5).
- **The RDP reference is software-first.** Ship a pure-Rust **software** LLE RDP
  (the angrylion analog) as the always-correct reference renderer and the
  fuzz-suite gate; a **wgpu-compute** RDP backend comes later, validated *against*
  that software reference, never replacing it as the oracle (§4, §Architecture
  options B; `docs/rdp.md`, `docs/performance.md`).
- **v0.1 stubs the LLE behind the chip traits** — `Rsp::tick` / `Rdp::tick` are
  LLE-shaped no-ops with the decode/execute marked TODO (`docs/STATUS.md`). The
  scheduler already co-schedules them on one timeline (ADR 0001), so filling them
  in is additive.
- **Clean-room from primary docs.** Implement from the R4300i datasheet, Dillonb's
  `rsp.md`, the n64brew wiki, and the test ROMs. Angrylion-Plus is GPL and
  ParaLLEl-RDP is per-repo; RustyN64 is MIT OR Apache-2.0, so those are **study /
  validate-against** references, not source to copy (`ref-docs/research-report.md`
  §External dependencies). Use their **conformance suites** as the spec.
- An HLE fast path may be added later behind an **off-by-default** feature flag for
  speed — never the default, never the accuracy oracle.

### Part B — the future sub-cycle timebase refactor (deferred)

If a hard-tier test ROM needs sub-cycle bus-phase resolution, that is a **distinct,
later** milestone: a one-clock + every-cycle-bus-access collapse with a φ1/φ2
access split (the Mesen2-style fractional master clock). Hard-tier residuals that
need it are **documented and deferred**, NOT point-fixed. This is the one milestone
expected to **break byte-identity / save-state compatibility**.

Do **not** conflate the two senses of "fractional master clock": the 3:2 fractional
accumulator already exists (the v0.1 scheduler, ADR 0001); this Part-B sub-cycle
refactor is a future thing — the RustyNES engine-lineage versioning trap, avoided
here by naming both explicitly (`docs/STATUS.md` §Version policy).

## Consequences

- **+** Custom microcode renders correctly without per-game HLE; mid-frame
  coprocessor effects are faithful; the test-ROM oracle is achievable.
- **+** Audio is correct by construction (it is just more microcode).
- **+** No GPL entanglement — clean-room MIT OR Apache-2.0.
- **−** More work and slower than HLE; the single-threaded RSP+CPU is the
  bottleneck (addressed by a later dynarec, `docs/performance.md`).
- **−** The software RDP must be bit-exact against Angrylion ("to pass we must get
  an exact match", §4) before any compute backend is trusted.
- The Part-B refactor, when/if it lands, intentionally breaks save-state
  compatibility — gated behind a major version bump.
