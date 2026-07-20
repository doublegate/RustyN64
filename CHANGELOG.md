# Changelog

All notable changes to RustyN64 are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/), and this project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

The next rung is `v0.2.0 "Interpreter"` — the VR4300 (see
[`to-dos/VERSION-PLAN.md`](to-dos/VERSION-PLAN.md)).

### Added — reference corpus and the accuracy ledger

- **[`docs/accuracy-ledger.md`](docs/accuracy-ledger.md)** — referenced from five documents and
  never created until now. Records measured constants with their provenance, open residuals,
  ruled-out approaches, and contradictions between primary sources. Seeded with the four timing
  constants the hardware documentation does not supply (`M`, the exception epilogue, CP0I, RDRAM
  bank state) and the four source contradictions found during the ADR review. The governing rule:
  a constant is measured, never tuned until a ROM passes — a tuned constant makes every later
  timing result unfalsifiable.
- **[`ref-docs/2026-07-20-vr4300-timing-supplement.md`](ref-docs/2026-07-20-vr4300-timing-supplement.md)**
  — `ref-docs/` is an immutable corpus, so corrections to `research-report.md` land as a dated
  supplement rather than an in-place edit. Records the IC/RF/EX/DC/WB stage-name correction, the
  MClock-is-primary clock derivation, the documented cycle-cost tables, the errata with exact
  behaviour, the SysAD block-ordering rules, and an explicit list of what is *not* documented.
- `docs/glossary.md` gains a **Clocks** section, because "master clock" is overloaded three ways
  and the primary sources own one of them (MasterClock = 62.5 MHz).
- `docs/performance.md` states the project goal plainly — sustained fully cycle-accurate
  emulation at full speed — with the budget (~156M component-steps/s, ~32 host cycles each) and
  an honest account of why it is unproven rather than impossible.

### Changed — the timebase and CPU microarchitecture are settled (Phase 1 design)

Two ADRs land ahead of any Phase 1 code, because both decisions are of the kind that cannot be
retrofitted without rewriting the scheduler and every chip's step contract at once.

- **[ADR 0006](docs/adr/0006-one-canonical-master-clock.md) — one canonical 187.5 MHz master
  clock. Supersedes [ADR 0001](docs/adr/0001-master-clock-lockstep-scheduler.md)**, which is
  retained unmodified as the record of the first design. 187.5 MHz is the LCM of the 93.75 MHz
  CPU and 62.5 MHz RCP clocks, which makes *every* emulated domain an integer divisor: CPU
  every 2 ticks, RCP every 3, COP0 `Count` every 4 (half PClock), SI every 12, PIF every 96.
  The 3:2 fractional accumulator is gone; a fractional path survives only for VI and AI, which
  genuinely run off a different crystal. The load-bearing rule is not the unit but the
  ownership: **`master_ticks` is the only counter ever incremented**, every other cycle position
  is a derived accessor, and a residue invariant test fails if any position becomes independent.
- **[ADR 0007](docs/adr/0007-cycle-accurate-vr4300-pipeline.md) — the VR4300 is a cycle-accurate
  five-stage pipeline** (IC/RF/EX/DC/WB) of four inter-stage latches advanced one PClock per
  step in **reverse stage order** (WB → DC → EX → RF → IC), which is what makes the latching
  implicit and removes any need for double-buffered state. `in_delay_slot` travels with the
  instruction in the latch chain rather than as global CPU state, so `Cause.BD`/`EPC` survive a
  multi-cycle stall between a branch and its delay slot. Interlocks are `(cycles, resume_stage)`.

Three factual corrections came out of the hardware research, all in `docs/cpu.md`:

- The pipeline stages are **IC/RF/EX/DC/WB**, not the "IF/RF/EX/DF/WB" previously documented.
  The manual's entire interlock and exception taxonomy is named stage-relative, so this matters.
- "The CPU advances one issued instruction per master tick" was not a timing model. `DDIV`
  stalls the whole pipeline for 69 PCycles, and at minimum 5 PCycles are needed per instruction.
  The documented cycle-cost tables are now transcribed into `docs/cpu.md` §Cycle costs.
- **`Bus::poll_irq_at_phase(BusPhase)` is removed, not completed.** It was shaped on the
  assumption that interrupt sampling is tied to the SysAD command/data phase; no such coupling
  is documented anywhere. The real rule is per-PCycle, gated on the previous PCycle having been
  a run cycle. Wiring the parameter up to *something* would have encoded a fiction that looked
  correct. `BusPhase` is retained for the bus protocol, with no interrupt semantics attached.

The full 655-page NEC VR4300 User's Manual turned out to be in the local wiki mirror already
(`n64brew_wiki/images/VR4300-Users-Manual.pdf`) with an intact text layer — it is now cited as
the primary timing oracle. Extract with `mutool draw -F txt`; `pdftotext` fails on it.

`docs/scheduler.md`, `docs/cpu.md`, `docs/architecture.md`, `docs/engineering-lessons.md`,
`README.md`, `AGENTS.md`, and the Phase 1 plan are updated to match. Sprint 1 gains T-11-008
(the SysAD transaction model and the measurement of `M`, the undocumented memory access time)
and its estimate rises from 3 to 5 weeks.

### Changed — dependency and toolchain refresh

Everything moved to the newest mutually-compatible versions available.

- Rust toolchain 1.96 → **1.97.1** (`rust-toolchain.toml`, workspace `rust-version`, and the
  `dtolnay/rust-toolchain` pin in all three workflows).
- egui / egui-wgpu / egui-winit 0.34 → **0.35** (MSRV 1.92, satisfied). `Panel::show_inside` is
  deprecated in favour of `Panel::show`; both call sites in `ui_shell` updated.
- `directories` 5 → **6**, `pollster` 0.4 → **1.0**, plus patch/minor moves across `winit`,
  `cpal`, `gilrs`, `rfd`, `bytemuck`, `thiserror`, `bitflags`, and `insta` via `cargo update`.
- GitHub Actions: `checkout` v4 → **v7**, `upload-artifact` v4 → **v7**, `download-artifact`
  v4 → **v8**, `configure-pages` v5 → **v6**, `upload-pages-artifact` v3 → **v5**,
  `deploy-pages` v4 → **v5**. `Swatinem/rust-cache` stays on the floating `v2` (currently 2.9.1).
- `markdownlint-cli` v0.39.0 → **v0.49.1**, which adds MD060 (table-column-style). Delimiter
  rows are normalised to padded form across 15 documents, and MD060 is pinned to
  `style: "padded"` rather than the default per-table inference — inference classified two
  single-row tables with 300-character cells as "aligned" and demanded header padding that
  cannot be met.

**`wgpu` is deliberately held at 29.** wgpu 30 is released, but no published `egui-wgpu`
accepts it — 0.35.0 requires `wgpu = "^29.0"`, and egui's unreleased `main` still pins 29.0.
Bumping wgpu alone would not fail resolution; cargo would link both wgpu 29 (for egui-wgpu) and
wgpu 30 (for the frontend), making the two `wgpu::Device` types distinct and breaking the build
with "expected `wgpu::Device`, found `wgpu::Device`". The rationale is recorded at the
dependency itself, with `cargo tree -p wgpu -d` given as the check.

### Added

- [`docs/engineering-lessons.md`](docs/engineering-lessons.md) — failure patterns carried over
  from two prior cycle-accurate emulators, generalised to this machine rather than copied.
  Ordered by when each lesson must be acted on: structural decisions that are cheap now and
  expensive-to-impossible later, then what "green" is permitted to mean, then debugging
  discipline. Phase-specific entries are also filed as risks in the relevant phase overview.
- Explicit `timeout-minutes` on every job in all three workflows (CI 30, Pages 20, release 45).
  Jobs previously inherited GitHub's 6-hour default, where a hang holds a concurrency slot and
  presents as "CI is slow" rather than as a hang.
- Phase 1 exit criterion requiring an instruction's memory access to be a point the scheduler can
  interleave around, with `Bus::poll_irq_at_phase` reaching a genuinely per-phase branch pinned by
  a test. Retrofitting that structure later means rewriting the scheduler and every chip's step
  contract at once.
- `docs/testing-strategy.md`: re-blessing a visual golden now requires justification against an
  external reference (ParaLLEl-RDP, Angrylion, hardware docs), never against our own previous
  output, with the reference named in the commit message.

### Changed

- ADR 0005 now requires a residual to be classified as an absolute or differential measurement
  before it can be attributed to sub-cycle bus timing. Differential measurements are invariant
  under uniform re-phasing and cannot be evidence for the refactor.
- Phase 5 records the per-game database as a bounded-authority data file: it may describe
  cartridge identity only, the core never consults it, and a frontend-only reproduction is
  treated as a load-path problem until proven otherwise.

### Fixed

- `Bus::poll_irq_at_phase` documented as not yet phase-sensitive. It ignores its `BusPhase`
  argument in both the trait default and the `rustyn64-core` implementation, so the signature
  implied per-phase interrupt sampling that does not exist yet.

## [0.1.0] "Foundation" — 2026-07-20

The architectural skeleton. The workspace compiles, CI is green across three platforms, the
reference corpus is acquired and licence-classified — and **no chip executes an instruction yet**.
This tag exists so the foundation is a fixed, citable point rather than an ever-growing
`[Unreleased]` section.

### Added

- Initial workspace scaffold (cycle-accurate emulator architecture, ported from RustyNES).
- Always-on egui shell (menu bar + status bar + stub debugger window) wired to the
  `winit + wgpu + cpal + egui` frontend, presenting a cleared/test-pattern frame at the
  N64 VI dimensions with the N64 controller input map (digital + analog stick).
- Release automation: a `v*` tag now publishes a GitHub Release with per-target archives
  (Linux x86_64, macOS aarch64, Windows x86_64) containing the `rustyn64` binary, both
  licences, `NOTICE`, `README`, and `CHANGELOG`, plus a `SHA256SUMS` manifest. The tag is
  checked against the workspace version before anything is published.
- Documentation site: pushes to `main` publish the rustdoc API reference to GitHub Pages
  under `/api/`, with `/` reserved for the wasm demo that lands in Phase 6.
- `scripts/mirror_n64brew_wiki.py` builds a gitignored offline mirror of the N64brew Wiki
  at `n64brew_wiki/` (see its `README.md`). 324 pages and 96 media files, with parallel
  HTML / Markdown / wikitext trees and `--refresh` for revision-based incremental updates.
- Test-ROM corpora under `tests/roms/`, split into a committed tier (permissively licensed,
  each ROM shipping its upstream `LICENSE`) and a gitignored external tier:
  - **committed** — `n64-systemtest` (MIT), the self-judging CPU/COP0/TLB/RSP gate, built
    from source since upstream publishes no prebuilt ROM;
  - **external** — PeterLemon/`krom` (196 ROMs), Dillon's n64-tests (26), the 240p Test
    Suite (built from source in a container), and a commercial regression corpus organised
    by save type.
- `scripts/check_no_roms.sh` plus a `no-commercial-roms` CI job: commercial ROMs are blocked
  by three independent guards (`.gitignore`, a pre-commit hook over the staged file list,
  and a server-side CI scan). The hook also enforces that any allowlisted ROM ships a
  `LICENSE` beside it.
- Full phase and sprint planning in `to-dos/`: the ROADMAP gains a status section, a phase spine
  with per-phase goal/exit criteria, cross-phase dependencies, and the open questions that gate
  deeper planning. All nine phase overviews adopt the seven-section skeleton (Goal, Exit criteria,
  Scope, Sprints, Dependencies, Risks, Reference docs), and ten sprint files mint **49 tickets**
  (`T-PS-NNN`, P = phase, S = sprint) with acceptance criteria, dependencies, spec references, and
  complexity. Phase 0's tickets are checked off against what actually shipped; the rest are
  forward plans grounded in the register-level detail from the wiki mirror.
- `docs/DOCUMENTATION_INDEX.md`, the docs map both sibling projects carry — subsystem specs,
  cross-cutting references, subdirectories, and the material outside `docs/`.
- Reference emulators and test suites cloned for study under the gitignored `ref-proj/`
  (ares, cen64, gopher64, simple64, parallel-rdp, parallel-rsp, angrylion-rdp-plus,
  n64-systemtest, n64-tests, libdragon, PeterLemon/N64). Per-repo licences are verified and
  recorded in `ref-proj/README.md`, which classifies each as vendor-ok or study-only.

### Changed

- Modernized the frontend against egui 0.34 / wgpu 29 / cpal 0.18 (`Panel::*::show_inside`,
  `MenuBar::new().ui`, `ui.close`, `Context::run_ui`, `CurrentSurfaceTexture`, the
  `experimental_features` / `multiview_mask` / `immediate_size` descriptor fields).
- Aligned the `to-dos/` phase overviews + ROADMAP to the N64 phase set (RSP LLE, RDP LLE+VI,
  AI audio, cart boot + saves, frontend integration, accuracy breadth).
- Filled out `CONTRIBUTING.md` (Rust-only) and fixed the markdownlint pre-commit hook to
  pass `--config .markdownlint.json`.
- `docs/STATUS.md` gained a project-infrastructure table and a test-ROM corpus table, and
  its accuracy table gained an "oracle available?" column. Several gates now have their ROM
  staged locally while remaining not-started, and collapsing those two states into one
  would misrepresent progress.
- `docs/testing-strategy.md` documents the corpus tiers, the commercial-ROM guards, and the
  per-corpus licensing that decides which tier a corpus lands in.
- `README.md` rebuilt to the structure RustyNES and RustySNES share — centred title block with a
  three-row badge set, Overview, Why RustyN64, Highlights, Features, Quick Start, Default
  Controls, Architecture with crate and layout tables, Compatibility and Accuracy, Performance,
  Platform Support, Documentation, Current Release, Roadmap, Contributing, License,
  Acknowledgments, Citation, and the shared footer. Adapted honestly for a pre-alpha: the
  accuracy badges read "not started" rather than borrowing the siblings' 0-diff claims, the
  Highlights table separates working from stubbed, and the Performance section states that no
  measurements exist rather than inventing any.
- `.gitignore` covers output our own workflows produce when run locally (`_site/` from pages.yml,
  `dist/` and the release archives from release.yml) plus `site/` for a future docs handbook.
- `docs/adr/0004-determinism-contract.md` gained the `Consequences` section the Nygard format and
  the project's own docs rules require — it was the only ADR without one. Records the costs the
  contract imposes and the fact that it is currently specified but unexercised.
- Root `Cargo.toml` excludes `ref-proj/` and `n64brew_wiki/` from the workspace. Cargo's
  upward workspace discovery otherwise makes any nested project there — `n64-systemtest`,
  `gopher64` — resolve *this* workspace as its root and fail to parse our members.

### Fixed

- CI, release, and Pages jobs now install the Linux system headers the frontend needs
  (`libasound2-dev`, `libudev-dev`, `libxkbcommon-dev`, `libwayland-dev`). `cpal` pulls
  `alsa-sys` and `gilrs` pulls `libudev-sys`, whose build scripts call `pkg-config`; those
  headers are absent from the GitHub runner images, so every Linux job that compiled the
  workspace would have failed in a build script.
- `release.yml` pins the toolchain to 1.96 instead of `@stable`. `rust-toolchain.toml`
  takes precedence over the action's default, so the previous config installed one
  toolchain and built with another.
- The nine phase overviews cited "the skill's references/roadmap_template.md" for their exit
  criteria — a dangling reference to the generator skill, which is not part of this repository, so
  no phase had a stated exit bar. Every phase now carries real, checkable criteria.
- `AGENTS.md` claimed the repository used three incompatible ticket-ID schemes. It does not:
  `T-PS-NNN` is a template where P is the phase digit and S the sprint digit, and the overviews
  instantiate it correctly as `T-01-NNN` through `T-81-NNN`. Only the pre-ticket code TODOs use a
  separate subsystem-scoped form.
- `README.md` cited the "Mesen2 / ares / higan" accuracy bar, which belongs to the NES/SNES
  projects. The N64 reference set is ares / CEN64 / Gopher64 / ParaLLEl, per
  `docs/architecture.md`. It also listed only 8 of the 10 workspace crates, and its
  quick-start implied the binary plays games — it currently opens a shell and presents a
  test pattern.
- `crates/rustyn64-frontend/web/Trunk.toml` pinned the wasm-bindgen CLI to 0.2.100 while
  `Cargo.lock` resolved the library to 0.2.126. Trunk requires these to be equal, so the
  wasm build would have failed at bindgen time. A new `wasm-bindgen-pin` CI job now
  compares the two and fails on drift, which is otherwise invisible until someone runs
  `trunk build`.

[Unreleased]: https://github.com/doublegate/RustyN64/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/doublegate/RustyN64/releases/tag/v0.1.0
