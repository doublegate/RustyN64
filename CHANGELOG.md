# Changelog

All notable changes to RustyN64 are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/), and this project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

Nothing yet. The next rung is `v0.2.0 "Interpreter"` — the VR4300 (see
[`to-dos/VERSION-PLAN.md`](to-dos/VERSION-PLAN.md)).

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
