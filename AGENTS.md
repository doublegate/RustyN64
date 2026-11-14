<!-- Managed by Master-Claude. Universal rules come from the imported/inlined core.
     Edit only inside the MC-PROJECT block; mc-sync overwrites everything else. -->
<!-- mc-core: 0.2.0 | mode=import | lang=rust -->
# AGENTS.md — RustyN64

@~/.claude/master-core/AGENTS.base.md
@~/.claude/master-core/lang/rust.md
@~/.claude/master-core/modules/10-commits-and-versioning.md
@~/.claude/master-core/modules/20-testing-and-accuracy.md
@~/.claude/master-core/modules/30-quality-gates.md
@~/.claude/master-core/modules/40-docs-and-adrs.md
@~/.claude/master-core/modules/50-architecture-patterns.md
@~/.claude/master-core/modules/60-security.md
@~/.claude/master-core/modules/70-release-ceremony.md
@~/.claude/master-core/modules/80-phase-sprint-workflow.md
@~/.claude/master-core/modules/90-multi-language-integration.md
@~/.claude/master-core/modules/91-agent-system-architecture.md
@~/.claude/master-core/modules/95-named-pattern-library.md

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

## Current state

Read `docs/STATUS.md` first (authoritative numbers). Phases 1 and 2 COMPLETE (v0.3.0); next is v0.4.0
"Rasterizer" (Phase 3). Exit criteria, per-chip status, soft-float rationale: `docs/agents/project-state.md`.
Only the CPU and RSP execute; the RDP and AI are LLE-shaped stubs, so a green `cargo test` proves nothing about them.

**The n64-systemtest runner is committed** at
`crates/rustyn64-test-harness/tests/systemtest.rs`, `#[ignore]`d because a full pass takes ~2
minutes in `--release`:

```bash
cargo test -p rustyn64-test-harness --release --test systemtest -- --ignored --nocapture
```

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
  n64-tests (no license), and angrylion-rdp-plus (**non-commercial MAME license, despite having
  no `LICENSE` file**) are study-only — compare their outputs, never their source.
- `n64brew_wiki/` — gitignored offline mirror of the N64brew Wiki, the primary hardware
  reference. Search `n64brew_wiki/markdown/`; browse `n64brew_wiki/html/`. Rebuild or update
  with `python3 scripts/mirror_n64brew_wiki.py [--refresh]`. CC BY-SA 4.0 — attribute if quoted.
- `n64brew_wiki/images/VR4300-Users-Manual.pdf` is the primary CPU timing oracle (cite `UM §x`); extract
  with `mutool draw -F txt`, never `pdftotext`. Supplement details: `docs/agents/build-and-layout.md`.
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
Edition 2024, toolchain pinned to 1.99.1. New public items need rustdoc or the doc gate fails.
Feature flags: `test-roms` (gates exactly one harness probe, no production code) and `commercial-roms`
(local dumps under gitignored `tests/roms/external/`; gates zero code). Details: `docs/agents/build-and-layout.md`.
markdownlint runs via `.pre-commit-config.yaml` (cli pinned **v0.49.1**) — it is NOT in any CI job.
**So any change touching `*.md` must run it locally**, or the only gate it has never runs at all:

```bash
pre-commit run markdownlint --all-files      # uses the pinned rev; a newer local
                                             # binary reports ungated rules
```

**Never pipe a gate into `tail`/`grep`/`head` when its exit status is what decides the next step.**
A pipeline reports the *filter's* status, so a failing gate reads as passing. This has let two
clippy-red commits through and hidden a rustdoc failure inside an `&&` chain that then printed
`ALL-GATES-OK`. Put the whole gate in one conditional with no filters inside it, and silence noise
with `>/dev/null` (which preserves status) rather than a pipe. The interactive shell here is
**fish**, which has no `pipefail`:

(worked fish `if ... && ...; else; end` example in `docs/agents/build-and-layout.md`).

**CI is split light/full:** ordinary feature PRs get only fmt/clippy/test/rustdoc/no_std on
ubuntu. The `test-roms` job and the macOS/Windows matrix run ONLY on push-to-main, the merge
queue, `release/*` PRs, dispatch, and a weekly cron. CI runs clippy exactly once — there are no
per-feature clippy jobs, so feature-gated code is currently unlinted.

Linux frontend needs system deps (wgpu/winit/cpal). Arch/CachyOS:
`sudo pacman -S --needed libxkbcommon wayland alsa-lib systemd-libs`

## Shipping: every change goes through a PR

**Never push to `main`.** All work lands via a pull request so the review bots
(Copilot, Gemini Code Assist) can comment. The full ceremony, in order:
see `docs/agents/shipping.md` (branch, PR, CI and bots, adjudicate and reply to every bot comment,
re-run to green, squash-merge, verify then delete the branch), plus review authority and bot-suggestion
policy. Standing authorization covers routine work only:

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

## Conventions

- A chip change touches the chip code AND its `docs/<chip>.md` in the same commit.
- **Never increment a cycle counter except `master_ticks`.** Every other cycle position is a
  derived accessor (ADR 0006). A new `cycles`/`ticks` field on a chip struct needs justification
  in review; the one legitimate use is a *retired-work* tally that nothing schedules against. The
  residue invariant test exists to catch this and must stay in the default `cargo test` path.
- **VR4300 NaN classification is INVERTED from IEEE-754:2008** (MSB set = signaling; `f32::NAN` raises
  Invalid). Never "correct" it back to IEEE; ledger **C-12**, details in `docs/agents/conventions.md`.
- Say "master clock" only with its rate. The sources use **MasterClock = 62.5 MHz**; this
  project's master tick is **187.5 MHz**; ADR 0001 used it for 93.75 MHz. See `docs/glossary.md`.
- `unsafe` is allowed in the frontend and FFI, and **nowhere else without an ADR**; every chip
  crate and `-core` carry `#![forbid(unsafe_code)]`. Exceptions (`rustyn64-rdp-gpu` FFI shim,
  ADR 0014; `rustyn64-rsp` intrinsics, ADR 0016): `docs/agents/conventions.md`.
- Stubs are `TODO(T-XXX-NN)` comments in no-op bodies that still compile, NOT `todo!()`. So a
  green `cargo test` does not mean a subsystem works — check `docs/STATUS.md`.
- Ticket IDs are `T-PS-NNN`, where **P is the phase digit and S the sprint digit** — so
  phase 1 sprint 1 mints `T-11-001`, phase 3 sprint 2 mints `T-32-004`. CONTRIBUTING/ROADMAP
  state the template; the phase overviews instantiate it. Code TODOs use a separate
  subsystem-scoped form (`T-CPU-01`, `T-HARNESS-02`) for pre-ticket scaffolding.
- Never commit commercial ROMs.
- Versioning starts clean at v0.1.0 (now at v0.3.0) — RustyNES "v2.0 / engine-lineage" anchors are NOT this
  project's releases.
- The remaining rules (comment-vs-implementation, "undocumented" claims, never inventing values or
  side effects, silent-no-op decodes, capture correlation, reverse-cascade latches, mutation-checking
  guards, 32-bit sign extension, the segment map, stall vs bypass, subnormals, measured timing
  constants) and their incidents: `docs/agents/conventions.md`.

## Agent reference docs (read when the topic applies; not imported)

- `docs/agents/build-and-layout.md`: full "where things live" and build/lint/CI section, feature-flag semantics, fish gate example.
- `docs/agents/project-state.md`: phase status and exit criteria, what each chip executes, soft-float rationale, systemtest runner notes.
- `docs/agents/shipping.md`: PR ceremony steps, bot-comment adjudication, who approves, branch-protection rationale.
- `docs/agents/conventions.md`: every convention with its incident history (pipeline, FPU, addressing, testing discipline, `unsafe` exceptions).

<<< MC-PROJECT-END >>>

