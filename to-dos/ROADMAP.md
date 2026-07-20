# RustyN64 — Roadmap

Entry point for project planning. Each phase below links to its overview; each phase contains
sprints; each sprint contains tickets with stable IDs `T-PS-NNN` (P = phase, S = sprint).
Reference ticket IDs in commit messages. `docs/STATUS.md` is the authoritative current-state
record; this file frames the phase line.

Companion documents: [`VERSION-PLAN.md`](VERSION-PLAN.md) sequences this phase spine into named,
tagged releases, and [`LOCKSTEP-CHECKLIST.md`](LOCKSTEP-CHECKLIST.md) is the pass run at the start
of scoping each release to catch drift against the two sibling projects.

Status markers here are plain text, not emoji — project policy (`CONTRIBUTING.md`).

## Status

- **Current phase:** Phase 1 (CPU golden log) — not started. Phase 0 is complete: the workspace
  compiles, all eight CI jobs are green across Linux/macOS/Windows, the docs site publishes, and
  the test-ROM corpora are staged. No chip executes instructions yet.
- **Release:** v0.1.0 (SKELETON), untagged. The architecture is in place — the Bus owns all
  mutable state, the 3:2 fractional master-clock scheduler runs, the crate graph is
  one-directional — but every chip `tick` is an LLE-shaped stub. See `docs/STATUS.md` for the
  honest per-subsystem state.

## The phase spine

### Phase 0 — Foundation: COMPLETE

**Goal:** the Cargo workspace and crate skeletons compile; CI green on stubs; the accuracy
oracle and hardware references acquired and organised.
**Exit:** `cargo test --workspace` green; fmt/clippy/rustdoc/`no_std` gates green; test-ROM
corpora staged with licence tiers enforced.
→ [overview](phase-0-foundation/overview.md)

### Phase 1 — CPU golden log: NOT STARTED

**Goal:** the VR4300 (MIPS III / R4300i) interpreter executes the full instruction set —
including the TLB, COP0, and the FPU — and 0-diffs against a golden instruction trace.
**Exit:** n64-systemtest reports `Failed: 0` for the CPU/COP0/TLB categories; the golden-log
differ finds no divergence over the captured trace.
→ [overview](phase-1-cpu-golden-log/overview.md)

### Phase 2 — RSP LLE: NOT STARTED

**Goal:** the RSP scalar unit and vector unit execute real game microcode under master-clock
lockstep, driven through the SP interface.
**Exit:** n64-systemtest RSP category `Failed: 0`; the RSP runs a real graphics microcode boot
without desync.
→ [overview](phase-2-rsp-lle/overview.md)

### Phase 3 — RDP LLE + VI: NOT STARTED

**Goal:** the software reference RDP rasterises the real command list through the texture,
combiner, blender, and Z/coverage pipeline; the VI scans the framebuffer out.
**Exit:** a stable rendered frame from a real ROM; the ParaLLEl-RDP fuzz suite bit-matches the
Angrylion reference.
→ [overview](phase-3-rdp-lle-vi/overview.md)

### Phase 4 — AI audio: NOT STARTED

**Goal:** the AI DMAs the PCM buffer produced by the RSP audio microcode to the host, with the
delayed-carry DAC behaviour modelled.
**Exit:** audio plays from a real ROM without underrun; the AI timing units match hardware.
→ [overview](phase-4-ai-audio/overview.md)

### Phase 5 — Cart boot + saves: NOT STARTED

**Goal:** the PI cart pipeline, the PIF/CIC boot handshake, the SI joybus, and all four save
backends round-trip.
**Exit:** a commercial ROM boots to its title screen; every save backend survives a
write/reload cycle.
→ [overview](phase-5-cart-boot-saves/overview.md)

### Phase 6 — Frontend integration: NOT STARTED

**Goal:** the egui shell wired to the real scan-out, audio drain, and controller input;
save-states, rewind, and run-ahead in the frontend; a browser entry point for the wasm build.
**Exit:** playable native and wasm; the determinism contract intact, with rate control in the
frontend only.
→ [overview](phase-6-frontend-integration/overview.md)

### Phase 7 — Accuracy breadth: NOT STARTED

**Goal:** drive the accuracy battery across the game corpus — custom microcode, every save type,
NTSC/PAL region timing as data.
**Exit:** the battery reaches its target pass rate; hard residuals documented, not point-fixed.
→ [overview](phase-7-accuracy-breadth/overview.md)

### Phase 8 — Reach: NOT STARTED

**Goal:** netplay, RetroAchievements, TAS tooling, Lua scripting, and shaders — every one
additive and off by default.
**Exit:** each feature ships behind a default-off flag, with the default build byte-identical.
→ [overview](phase-8-reach/overview.md)

## Milestones beyond the phases

- **v1.0.0** — the production cut: Phases 1-8 complete; README, CHANGELOG, `docs/`, and
  `docs/STATUS.md` in sync; the release matrix and Pages green. Of those readiness items, Pages
  is already green and the release workflow is written but has never run, because no tag has
  been cut.
- **Beyond v1.0** — the sub-cycle φ1/φ2 timebase refactor (ADR 0002), *only if* hard residuals
  from Phase 7 warrant it. The one release expected to break byte-identity and save-state
  compatibility; it will be announced in advance.

## Cross-phase dependencies

- Phase 2 (RSP) and Phase 3 (RDP) both need Phase 1's CPU: microcode and display lists are built
  by CPU code before either coprocessor sees them.
- Phase 4 (audio) depends on Phase 2, not on separate DSP work — the audio microcode runs on the
  same LLE RSP core, so there is no per-game audio HLE (ADR 0002).
- Phase 5 (boot) is what makes commercial ROMs testable at all. Until it lands, Phases 1-3 are
  driven only by homebrew test ROMs that skip the CIC handshake.
- Phase 6 depends on Phases 3 and 4 for anything to present, and on Phase 5 for input.
- Phase 7 depends on everything, and is where the corpus staged in Phase 0 is finally used.
- The determinism contract (ADR 0004) constrains every phase: no wall-clock, OS entropy, or
  thread-scheduling input inside the core, ever.

## Open questions

These gate deeper planning and are carried from `ref-docs/research-report.md`:

- **RDRAM/bus contention depth** — how precisely must CPU/RSP/RDP/DMA arbitration be modelled for
  commercial-game correctness, versus for n64-systemtest alone? Needs a prototype to find the
  cost/accuracy knee. Bears on Phases 1 and 3.
- **Boot strategy** — stub IPL3 (documented HLE boot) versus running the real PIF/IPL ROMs.
  Likely stub-first with an optional real-IPL mode. Phase 5.
- **RDP backend ordering** — the software reference lands first and stays the oracle; the
  wgpu-compute accelerator is validated against it and never replaces it. Phase 3, then Phase 7.
- **Cache-coherency depth** — how exact must the I/D-cache and DMA coherency model be? Phase 1.
- **Region timing tables** — exact PAL VI/AI divisors need pinning before the region table is
  frozen. Phase 7.
