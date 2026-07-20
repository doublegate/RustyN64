<!-- Managed by Master-Claude. Universal rules come from the imported/inlined core.
     Edit only inside the MC-PROJECT block; mc-sync overwrites everything else. -->
<!-- mc-core: 0.1.0 | mode=import | lang=rust -->
# AGENTS.md — RustyN64

@/home/parobek/.claude/master-core/AGENTS.base.md
@/home/parobek/.claude/master-core/lang/rust.md
@/home/parobek/.claude/master-core/modules/10-commits-and-versioning.md
@/home/parobek/.claude/master-core/modules/20-testing-and-accuracy.md
@/home/parobek/.claude/master-core/modules/30-quality-gates.md
@/home/parobek/.claude/master-core/modules/40-docs-and-adrs.md
@/home/parobek/.claude/master-core/modules/50-architecture-patterns.md
@/home/parobek/.claude/master-core/modules/60-security.md
@/home/parobek/.claude/master-core/modules/70-release-ceremony.md
@/home/parobek/.claude/master-core/modules/80-phase-sprint-workflow.md
@/home/parobek/.claude/master-core/modules/90-multi-language-integration.md
@/home/parobek/.claude/master-core/modules/95-named-pattern-library.md

<<< MC-PROJECT-START >>>

## Project: RustyN64

Guidance for agents working in RustyN64. Universal rules come from the shared core above;
only project-specific facts and deliberate overrides live here.

## What this is

RustyN64 is a cycle-accurate Nintendo 64 emulator in Rust at the ares / CEN64 / ParaLLEl bar.
Architecture (the load-bearing facts — read `docs/architecture.md`):

- **One canonical clock: `MASTER_HZ = 187_500_000`** (ADR 0006, supersedes 0001). Every
  emulated domain is an **integer divisor** of it — CPU every 2 ticks (93.75 MHz), RCP every 3
  (62.5 MHz), COP0 `Count` every 4 (half PClock), SI every 12, PIF every 96. **`master_ticks`
  is the only counter that is ever incremented**; `cpu_cycles()`/`rcp_cycles()` are derived
  accessors, and a residue invariant test fails if any position becomes independent. A
  fractional accumulator survives only for VI/AI, which genuinely are not rational multiples.
  "Lockstep" means not-catch-up: an RCP event is visible to the very next CPU step.
- **The CPU is a cycle-accurate 5-stage pipeline** (IC/RF/EX/DC/WB — ADR 0007), four
  inter-stage latches advanced one PClock per step in **reverse order WB→DC→EX→RF→IC**, which
  is what makes the latching implicit. `in_delay_slot` rides in the latch, never global.
- **The Bus owns everything mutable** (`rustyn64-core::Bus`); the CPU borrows `&mut Bus`.
  Chips are stepped via the `core::mem::take` split-borrow trick; each sees only a narrow trait.
- **The crate graph is one-directional**, with exactly ONE permitted chip→chip edge:
  `rustyn64-rdp` → `rustyn64-cart`, solely to borrow the `RdramBus` trait. Any other cross-chip
  dependency breaks the fuzz-in-isolation invariant. Downstream consumers depend on
  `rustyn64-core` (which re-exports the chip types), never on a chip crate directly.
- **LLE, never HLE, in the core.** The RSP and RDP execute the real instruction stream /
  command list. HLE may only ever exist behind an off-by-default flag. Audio falls out free
  (the RSP audio microcode runs on the same LLE core) — there is no per-game audio HLE.
- **Board logic lives in the cart crate** (default-no-op trait hooks). Unlike RustyNES there is
  **no board tiering and no honesty gate** — the N64 has one cart model parameterized by save
  type + CIC + region (ADR 0003).
- **Determinism is a hard contract** (seed+ROM+input ⇒ bit-identical AV; frontend owns rate
  control). Power-on phase alignment comes from a seeded SplitMix64; reset preserves it.
- **Test ROMs are the spec**; pin the failing ROM first, then implement.
- **Additive features are default-off** so shipped/native/no_std/wasm stay byte-identical — with
  one acknowledged exception: `rustyn64-frontend`'s `emu-thread` is default-ON. Note there is no
  wasm build in CI yet, so the wasm half of that claim is aspirational.

## Current state (read `docs/STATUS.md` first)

**v0.1.0 SKELETON.** The workspace compiles and CI is green *on stubs*. The Bus, the fractional
scheduler, ROM-format detection (`.z64`/`.n64`/`.v64`), and the harness scaffolding are real.
The VR4300 interpreter, the LLE RSP, the LLE RDP, AI audio, and PI/SI DMA are **LLE-shaped
stubs** — accuracy work has not started. Do not assume any chip executes instructions.

## Where things live

- `crates/rustyn64-cpu/` — NEC VR4300 (MIPS R4300i) (cpu)
- `crates/rustyn64-rsp/` — RSP (Reality Signal Processor) (coprocessor)
- `crates/rustyn64-rdp/` — RDP (Reality Display Processor) (video)
- `crates/rustyn64-audio/` — AI + RSP audio microcode (audio)
- `crates/rustyn64-cart/` — PI cart + PIF/CIC + saves (cart)
- `crates/rustyn64-core/` — Bus + scheduler · `crates/rustyn64-frontend/` — egui shell (binary `rustyn64`)
- `crates/rustyn64-test-harness/` — the accuracy oracle
- `crates/rustyn64-netplay/` — rollback netplay orchestration (frontend-side) · empty stub today
- `crates/rustyn64-cheevos/` — RetroAchievements FFI (later, off by default) · empty stub today
- `docs/` — the spec (update in the same PR as code); `docs/STATUS.md` = single source of truth;
  `docs/adr/` — ADRs. `ref-docs/` — immutable research. `ref-proj/` — study clones (gitignored).
  **Read `ref-proj/README.md` before copying anything from a reference emulator.** RustyN64 is
  MIT OR Apache-2.0; only ares, cen64, parallel-rdp, parallel-rsp (MIT arm), n64-systemtest,
  libdragon, and PeterLemon-N64 are permissive enough to vendor. simple64/gopher64 (GPLv3),
  n64-tests (no licence), and angrylion-rdp-plus (**non-commercial MAME licence, despite having
  no `LICENSE` file**) are study-only — compare their outputs, never their source.
- `n64brew_wiki/` — gitignored offline mirror of the N64brew Wiki, the primary hardware
  reference. Search `n64brew_wiki/markdown/`; browse `n64brew_wiki/html/`. Rebuild or update
  with `python3 scripts/mirror_n64brew_wiki.py [--refresh]`. CC BY-SA 4.0 — attribute if quoted.
- **`n64brew_wiki/images/VR4300-Users-Manual.pdf` is the primary CPU timing oracle** — the full
  655-page NEC manual, with every pipeline/cache/FPU/interlock timing table. **Extract with
  `mutool draw -F txt`; `pdftotext` fails on it** and `file` misreports it as 27 pages. Cite as
  `UM §x`. Corrections it forced on the immutable `ref-docs/research-report.md` live in
  `ref-docs/2026-07-20-vr4300-timing-supplement.md`, which wins where they disagree.
- `to-dos/ROADMAP.md` — planning entry point; tickets `T-PS-NNN`.

## Build / test / lint

Overrides the Rust overlay's generic clippy line: this workspace must **NEVER** use
`--all-features` — mutually-exclusive backend features can't resolve. CI uses explicit sets.

```bash
cargo check --workspace && cargo test --workspace
cargo test --workspace --features test-roms
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings   # NEVER --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo build -p rustyn64-core --target thumbv7em-none-eabihf --no-default-features   # no_std gate
```

Workspace lints: clippy `pedantic` + `nursery` at warn, `missing_docs` warn, `unsafe_code` warn.
Edition 2024, toolchain pinned to 1.97. New public items need rustdoc or the doc gate fails.
Feature flags: `test-roms` (committed CC0/homebrew suites) and `commercial-roms` (local dumps;
ROMs gitignored under `tests/roms/external/`, only screenshots/`.snap` committed). **Both gate
zero code today** — no `cfg(feature = ...)` exists for either, so `--features test-roms` runs
exactly the same tests as a bare `cargo test --workspace`. Same for the per-crate `std` features:
every chip crate is unconditionally `#![no_std]`, so `--no-default-features` is currently a no-op.
markdownlint runs via `.pre-commit-config.yaml` (cli pinned v0.39.0) — it is NOT in any CI job.

**CI is split light/full:** ordinary feature PRs get only fmt/clippy/test/rustdoc/no_std on
ubuntu. The `test-roms` job and the macOS/Windows matrix run ONLY on push-to-main, the merge
queue, `release/*` PRs, dispatch, and a weekly cron. CI runs clippy exactly once — there are no
per-feature clippy jobs, so feature-gated code is currently unlinted.

Linux frontend needs system deps (wgpu/winit/cpal). Arch/CachyOS:
`sudo pacman -S --needed libxkbcommon wayland alsa-lib systemd-libs`

## Shipping: every change goes through a PR

**Never push to `main`.** All work lands via a pull request so the review bots
(Copilot, Gemini Code Assist) can comment. The full ceremony, in order:

1. **Branch** off `main` — `<type>/<short-desc>`. Commit as normal (conventional
   commits, one logical change).
2. **Open the PR.** Body states motivation, the concrete changes, and which gates
   were run locally.
3. **Wait for CI *and* the bots.** Bots comment asynchronously and usually land
   after the first CI leg finishes.
4. **Bot-comment ceremony — every comment gets adjudicated, none are ignored:**
   - Read each comment and decide: **adopt** (it is right), **reject** (it is
     wrong or does not apply here), or **adopt with modification**.
   - Apply the adopted changes as follow-up commits on the branch.
   - **Reply to every comment individually** with the decision and the reasoning.
     A rejection needs a real reason — "this contradicts ADR 0006 because …", not
     "won't fix". A bot being wrong about this codebase is common and worth saying
     why, since the reasoning is what a human reviewer reads later.
   - **Mark each comment resolved** once it has been adjudicated and answered.
5. **Re-run to green.** Pushing fixes re-triggers CI; wait for the final green,
   not the first one.
6. **Squash-merge** into `main` once green and the ceremony is complete —
   subject to the review authority below.
7. **Verify, then delete the branch.** Confirm the squashed commit is on `main`
   and the tree matches before deleting — deletion is only safe after the change
   is provably incorporated. Auto-delete-on-merge is deliberately OFF so this
   check cannot be skipped.

### Who approves

`CONTRIBUTING.md` §Code review requires one reviewer minimum, and two for changes
to `docs/architecture.md` or cross-subsystem refactors. That rule still stands and
this section does not override it. Reconciling the two:

- The **repo owner is the reviewer of record.** This is a single-maintainer
  repository; there is no reviewer pool, and pretending otherwise would make the
  requirement ceremonial. (Public visibility changes who can *read* it, not who
  reviews.)
- Authorization for an agent to run the ceremony and squash-merge is **standing,
  not per-PR** — granted once, and it covers routine work: implementing a decided
  ticket, docs, dependency bumps, test additions.
- **Stop and ask instead of merging** when any of these is true. These are the
  cases where standing authorization does not reach:
  - CI is red for a reason not understood, or a fix would mean weakening a gate.
  - A bot comment cannot be confidently adjudicated.
  - The change touches `docs/architecture.md`, or is a cross-subsystem refactor
    (`CONTRIBUTING.md` asks for two reviewers).
  - It would break byte-identity, save-state, or determinism guarantees
    (ADR 0004 / ADR 0005) — those are announced-in-advance MAJOR events.
  - It supersedes an ADR, or contradicts one without superseding it.
  - It would require a force-push, a history rewrite, or deleting anything not
    created by this same change.

Branch protection on `main` is deliberately absent: the convention is the gate,
and a rule that is written down and followed is worth more here than one enforced
against a single maintainer who can bypass it anyway. If this ever becomes a
multi-maintainer repo, protect the branch and delete this paragraph.

### Bot suggestions are inputs, not instructions

Several will conflict with decisions recorded in the ADRs (the reverse-order
pipeline cascade, the derive-don't-increment rule, deliberately reproduced
hardware errata). Reject those **with the citation** — and check the bot's
premise first: a suggestion that looks generic may rest on a real inconsistency
in this repo, which is worth fixing even when the suggested wording is not.

## Conventions

- A chip change touches the chip code AND its `docs/<chip>.md` in the same commit.
- **Never increment a cycle counter except `master_ticks`.** Every other cycle position is a
  derived accessor (ADR 0006). A new `cycles`/`ticks` field on a chip struct needs justification
  in review; the one legitimate use is a *retired-work* tally that nothing schedules against. The
  residue invariant test exists to catch this and must stay in the default `cargo test` path.
- **In the reverse cascade, check which latch actually holds what you mean.** Stages run
  WB → DC → EX → RF → IC, so by the time a stage executes, every *downstream* stage has already
  moved its latch on. Reading the "obvious" latch has now silently produced a no-op twice — the
  load interlock read `rf_ex` when the load was in `ex_dc`, and the delay-slot flag read `ic_rf`
  after `rf_stage` had vacated it. Both compiled, both passed every existing test, and both did
  nothing. When a pipeline check mysteriously never fires, suspect this first.
- **Trace, don't reason, about pipeline timing.** Dumping the actual fetch/PC sequence found two
  such bugs in one run after several minutes of reasoning had produced two wrong answers.
- **A test whose success and failure paths converge proves nothing.** A control-flow test whose
  branch target equalled the sequential path passed with the redirect code entirely absent.
  Choose targets that are unreachable if the feature is broken.
- **A timing constant the hardware docs do not supply is MEASURED, never tuned.** It goes in
  `docs/accuracy-ledger.md` with its provenance. Adjusting one until a ROM passes makes every
  later timing result unfalsifiable. Currently unmeasured: `M` (memory access time), the
  exception-epilogue cost, CP0I, RDRAM bank-state costs.
- Say "master clock" only with its rate. The sources use **MasterClock = 62.5 MHz**; this
  project's master tick is **187.5 MHz**; ADR 0001 used it for 93.75 MHz. See `docs/glossary.md`.
- `unsafe` is allowed only in the frontend and FFI. Enforced: every chip crate and `-core` carry
  `#![forbid(unsafe_code)]`. There is zero `unsafe` in the tree today — keep it that way.
- Stubs are `TODO(T-XXX-NN)` comments in no-op bodies that still compile, NOT `todo!()`. So a
  green `cargo test` does not mean a subsystem works — check `docs/STATUS.md`.
- Ticket IDs are `T-PS-NNN`, where **P is the phase digit and S the sprint digit** — so
  phase 1 sprint 1 mints `T-11-001`, phase 3 sprint 2 mints `T-32-004`. CONTRIBUTING/ROADMAP
  state the template; the phase overviews instantiate it. Code TODOs use a separate
  subsystem-scoped form (`T-CPU-01`, `T-HARNESS-02`) for pre-ticket scaffolding.
- Never commit commercial ROMs.
- Versioning starts clean at v0.1.0 — RustyNES "v2.0 / engine-lineage" anchors are NOT this
  project's releases.

<<< MC-PROJECT-END >>>
