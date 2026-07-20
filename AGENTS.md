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

- **The timing master is the VR4300 cycle** (`MASTER_HZ = 93_750_000`); the RCP
  (`RCP_HZ = 62_500_000`) advances on a **3:2 fractional accumulator** — 2 RCP ticks per 3
  master ticks — NOT an integer divisor. "Lockstep" here means not-catch-up: an RCP event is
  visible to the very next CPU step. Integer lockstep was explicitly rejected (ADR 0001/0002).
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
- `n64brew_wiki/` — gitignored offline mirror of the N64brew Wiki, the primary hardware
  reference. Search `n64brew_wiki/markdown/`; browse `n64brew_wiki/html/`. Rebuild or update
  with `python3 scripts/mirror_n64brew_wiki.py [--refresh]`. CC BY-SA 4.0 — attribute if quoted.
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
Edition 2024, toolchain pinned to 1.96. New public items need rustdoc or the doc gate fails.
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

## Conventions

- A chip change touches the chip code AND its `docs/<chip>.md` in the same commit.
- `unsafe` is allowed only in the frontend and FFI. Enforced: every chip crate and `-core` carry
  `#![forbid(unsafe_code)]`. There is zero `unsafe` in the tree today — keep it that way.
- Stubs are `TODO(T-XXX-NN)` comments in no-op bodies that still compile, NOT `todo!()`. So a
  green `cargo test` does not mean a subsystem works — check `docs/STATUS.md`.
- Ticket IDs are inconsistent across the repo (`T-PS-NNN` in CONTRIBUTING/ROADMAP, `T-01-NNN` in
  phase overviews, `T-CPU-01` in code TODOs) and no tickets exist yet. Ask before minting one.
- Never commit commercial ROMs.
- Versioning starts clean at v0.1.0 — RustyNES "v2.0 / engine-lineage" anchors are NOT this
  project's releases.

<<< MC-PROJECT-END >>>
