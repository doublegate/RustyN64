# RustyN64 — Version Plan (v0.1.0 to v1.0.0, and beyond)

This document is the release-cut map: it takes `to-dos/ROADMAP.md`'s phase spine and sequences it
into concrete, named, tagged releases — matching the depth and process RustyNES and RustySNES
each used to reach their own v1.0.0. `docs/STATUS.md` is the ground truth this ladder is checked
against at every rung.

## Where this project actually is

**RustyN64 has cut four releases so far** — `v0.1.0` "Foundation", `v0.2.0` "Interpreter"
(Phase 1, the complete VR4300), `v0.3.0` "Microcode" (Phase 2, the LLE RSP + real graphics
microcode emitting an RDP command list), and `v0.4.0` "Rasteriser" (Phase 3, the LLE RDP + VI —
164 conformance vectors bit-matching Angrylion and a real ROM rendering a golden frame), followed
by `v0.4.1`, a documentation-only patch that adds no new scope. Unlike
the point at which RustySNES's plan was written — where a large amount of working emulation had
accumulated inside one perpetual `[Unreleased]` section — this ladder was written *forwards* from
an empty tree. The VR4300, RSP, and RDP now execute; the AI and cart boot are still LLE-shaped
stubs (Phases 4–5).

That difference matters for how this ladder should be read. RustySNES's plan was written
*backwards* from a large body of shipped work that needed sequencing into tags. This one is
written *forwards*: every rung below except `v0.1.0` is a plan, not a record, and each will be
re-scoped against reality when it is reached. Do not treat the rung contents as commitments —
treat the **ordering** as the commitment, because the ordering is what the phase spine already
justifies (each layer rests on a verified one below it).

## Versioning rule

- **`v0.x.0`** (minor) = new scope — a phase-spine chunk or a themed feature set. Additive and
  default-off wherever the byte-identity convention in `docs/STATUS.md`'s version policy applies.
- **`v0.x.y`** (patch) = same-minor bugfixes, accuracy fixes, or dependency bumps only. No new
  scope. Cut as needed at any point before the next minor.
- **`v1.0.0`** and beyond follow the same rule at the next digit.
- A **MAJOR** bump (`v2.0.0`) is reserved for a public-API or save-state-format break. The
  sub-cycle bus-timing refactor (ADR 0005) is the only currently-anticipated candidate, and
  only *if* Phase 7's accuracy triage concludes it is warranted.
- **Every tag is annotated, but the annotation is short.** The long-form release note lives at
  `docs/release-notes/<tag>.md` and the release workflow uses it as the GitHub Release body; the
  tag annotation is a summary that points at it. Notes live in the repository so they can be
  reviewed in a PR, corrected without rewriting a published tag, and linted like any other
  document. Each note is dense technical prose grouped by area (CPU / RSP / RDP / audio / cart /
  frontend / infrastructure), carries a **Known limitations** section, and closes with an
  explicit accuracy statement ("oracle suites: N/N held", or "no accuracy gates active yet" while
  that remains true). See `docs/release-notes/README.md`.

## The ladder

Each rung maps to a phase in `to-dos/ROADMAP.md`. The mapping is deliberately one-to-one for the
core phases: this project's phases are large and sequential, so splitting them across tags would
produce releases that cannot boot anything.

### v0.1.0 "Foundation" — the architectural skeleton — cut immediately, retroactively

Everything built to date, tagged as-is rather than left in a perpetual `[Unreleased]`. Phase 0.

- The Cargo workspace, ten crates, the pinned 1.96 toolchain, edition 2024, `[workspace.lints]`
  at clippy `pedantic` + `nursery`.
- The Bus owning all mutable state, with five narrow per-chip traits and split-borrow stepping.
- The 3:2 fractional master-clock scheduler with seeded power-on phase and reset-preserves-phase.
- ROM-format detection and byte-order normalisation for `.z64` / `.n64` / `.v64`.
- CI: eight jobs across Linux, macOS, and Windows, split light/full.
- The published rustdoc site, the release workflow (written, never run), and the three-layer
  commercial-ROM guard.
- The acquired reference corpus: the N64brew Wiki mirror, eleven licence-classified study clones,
  and the test-ROM tiers.

**Cut criterion:** already met. The only reason it is untagged is that no tag has been pushed.

### v0.2.0 "Interpreter" — the VR4300 — Phase 1

The CPU executes MIPS III, including the TLB, COP0, the FPU, and the documented errata, and
0-diffs against a golden trace.

- **The timebase and microarchitecture rework lands here (T-11-001), before any instruction
  work.** The scheduler moves to the canonical 187.5 MHz clock with integer divisors and a
  single incremented counter (ADR 0006, superseding ADR 0001's 93.75 MHz tick and 3:2 fractional
  accumulator), and the CPU becomes a cycle-accurate five-stage pipeline (ADR 0007). Both are
  MINOR-compatible here only because nothing depends on the old shape yet — no save-state format
  exists, and no chip executes instructions. **This is the last release at which either change is
  free**; after save states ship, the same change is a MAJOR bump with a format epoch break,
  which is exactly what a sibling project had to pay.
- The residue invariant test lands with it, and stays in the default test path permanently.
- The first release where a test ROM actually runs: `run_until_complete` stops returning
  `Timeout` and n64-systemtest reports a real number.
- The determinism regression test lands here, closing the ADR 0004 gap that `docs/STATUS.md`
  currently records as specified-but-unexercised. It cannot land earlier — there is nothing to
  run twice.
- **Cut criterion:** n64-systemtest `Failed: 0` on the CPU/COP0/TLB categories.

### v0.3.0 "Microcode" — the LLE RSP — Phase 2

The scalar and vector units execute real game microcode under master-clock lockstep.

- The reciprocal and reciprocal-square-root ROM tables are data, not computed approximations.
- The SP interface, DMA double-buffering, and the halt/break/interrupt handshake.
- **Cut criterion:** n64-systemtest RSP category `Failed: 0`, and a real graphics microcode boots
  and emits a plausible RDP command list.

### v0.4.0 "Rasteriser" — the LLE RDP and VI — Phase 3

The first release that produces a picture.

- The software reference rasteriser through the full per-pixel pipeline, and VI scan-out.
- The first committed golden frame, which makes Layer 5 real rather than scaffolding.
- **Cut criterion:** a real ROM renders a stable frame matching a committed golden, and the
  ParaLLEl-RDP fuzz suite bit-matches Angrylion.

### v0.5.0 "Resonance" — AI audio — Phase 4 — CUT (2026-07-24)

Sound, from the buffer the RSP audio microcode produces.

- The AI register set, double-buffered DMA, the derived DAC rate, and the delayed-carry hardware
  bug reproduced rather than corrected.
- The real libdragon audio-mixer microcode brought up on the LLE RSP, and the frontend resampler.
- **Cut criterion — met:** the real mixer microcode produces a golden-verified, deterministic
  mixed PCM buffer on the RSP (`mixer_microcode.rs`), and a real bare-metal ROM plays a
  deterministic PCM stream through the AI without underrun (`audio_play_rom.rs`). A real *game*
  driving the full CPU→RSP-mixer→AI→frontend path awaits cartridge boot (Phase 5, v0.6.0), so
  that end-to-end variant of the criterion is honestly deferred there — as the mixer ROM boot
  (which needs DFS/wav64/display) could not run in Phase 4.

### v0.6.0 "Cartridge" — boot and saves — Phase 5

The first release where a commercial cartridge boots.

- The PI bus with domain timing and open-bus behaviour, the SI joybus, the CIC handshake, and
  all four save backends plus the Controller Pak.
- **Cut criterion:** a commercial ROM from each save-type folder boots to its title screen and
  survives a save/reload cycle.

### v0.7.0 "Shell" — frontend integration — Phase 6

The first release that is *playable*.

- Real scan-out, real audio drain, real controller input through the SI.
- Save-states, rewind, and run-ahead — all frontend-side, per ADR 0004.
- The wasm browser entry point: the `wasm-bindgen` dependency, the `#[wasm_bindgen(start)]`
  entry, and an `index.html`, none of which exist today despite the crate compiling for
  `wasm32`.
- **Cut criterion:** a commercial ROM is playable natively with picture, sound, and control, and
  save-state restore continues bit-identically.

### v0.8.0 "Breadth" — the accuracy battery — Phase 7

The release where the corpus staged in Phase 0 is finally used for what it was selected for.

- The battery authored and reporting a real pass rate; the commercial corpus triaged.
- The custom-microcode families rendering correctly — the Factor 5 and Boss Game Studios titles
  that break HLE, and therefore the concrete payoff of ADR 0002.
- `docs/accuracy-ledger.md` created, mapping every residual to a disposition.
- Region timing moved into a table, closing the PAL open question.
- **Cut criterion:** the battery hits its target pass rate, and every residual has a ledger entry.

### v0.9.0 "Threshold" — residual closure and release engineering

The rung that exists because the previous eight will leave things behind. Deliberately reserved
rather than assumed away: RustySNES added its own `v0.9.0 "Threshold"` for exactly this reason
when Phase 7/8 leftovers surfaced during scoping.

- Whatever `v0.8.0`'s triage classified as fixable-but-not-yet-fixed.
- Release engineering: actually exercising the release workflow end to end, which has never run.
- Documentation parity: README, CHANGELOG, `docs/`, and `docs/STATUS.md` reconciled.
- **Cut criterion:** no known-fixable residual outstanding, and a real tag has produced real
  artefacts.

### v1.0.0 — the production cut

Phases 1-8 complete, all planning documents in sync, the release matrix and Pages green.

- **Cut criterion:** as in `to-dos/ROADMAP.md`'s milestone entry. Of those items, Pages is
  already green today; the release matrix is written but untested.

## Post-v1.0 — Reach (deferred)

Phase 8 is deliberately *not* in the v1.0.0 gate, which is a divergence from RustyNES's own
v1.0.0 (it front-loaded netplay, achievements, TAS, scripting, and a debugger into the 1.0 bar).
The reasoning is the machine, not the ambition: the N64's LLE RSP and RDP are a far larger
correctness surface than the NES's CPU and PPU, and shipping reach features on an emulator whose
accuracy battery has not stabilised would invert the phase spine's whole premise.

- **`v1.1.0` onward** — rollback netplay, RetroAchievements, TAS tooling, Lua scripting, and the
  shader pipeline, each additive and default-off, each proven byte-identical with its flag off.
- **`v2.0.0`** — reserved for the sub-cycle bus-timing refactor (ADR 0005), *only if* the
  v0.8.0 residual triage concludes it is warranted. This is the one release expected to break
  byte-identity and save-state compatibility, and it will be announced in advance.

## Standards adopted for every release from v0.1.0 onward

- **Commits:** Conventional Commits, plus naming the concrete mechanism in the body rather than
  just the feature — the exact clock ratio, the exact register, the exact opcode — and
  referencing the `T-PS-NNN` ticket.
- **Release notes:** the annotated tag body IS the release note, at the technical depth this
  project's `CHANGELOG.md` already uses.
- **Docs-as-spec:** a chip change touches the chip code and its `docs/<chip>.md` in the same
  commit. A behaviour diff whose spec is untouched is rejected.
- **Honest status:** every rung updates `docs/STATUS.md`, and a rung that misses part of its
  scope says so in the tag body rather than quietly narrowing the claim. The
  "oracle available" versus "gate passes" distinction in `docs/STATUS.md` is load-bearing and
  must not be collapsed.
- **Continuous research:** at each rung, re-consult `n64brew_wiki/` and the permissive study
  clones (`ares`, `cen64`, `parallel-rdp`) for that rung's specific hardware behaviour. The
  sourcing in the phase overviews is a starting point to re-verify at implementation time, not a
  final citation. Respect `ref-proj/README.md`'s per-repo terms — several clones are study-only.
- **Sibling-lockstep check:** run `to-dos/LOCKSTEP-CHECKLIST.md` once at the start of scoping
  each release, so drift against RustyNES's and RustySNES's continuing development is caught and
  folded in — or explicitly deferred — before it accumulates.
- **Version bump:** every `chore(release)` closeout bumps `[workspace.package] version` in the
  root `Cargo.toml`, then runs `cargo check --workspace` to regenerate `Cargo.lock`.
  `env!("CARGO_PKG_VERSION")` feeds `--version`, so a missed bump makes the binary under-report
  its own version — a mistake RustySNES made across two releases before catching it.
- **Tag/manifest agreement:** the release workflow already refuses to publish when the tag and
  the workspace version disagree. Do not weaken that check.
