# RustyN64 agent reference: build, CI and repository layout

Moved verbatim from the AGENTS.md project block: full feature-flag semantics, the fish gate example, and the reference-material notes.

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
Edition 2024, toolchain pinned to 1.99.1. New public items need rustdoc or the doc gate fails.
Feature flags: `test-roms` (committed CC0/homebrew suites) and `commercial-roms` (local dumps;
ROMs gitignored under `tests/roms/external/`, only screenshots/`.snap` committed). `test-roms`
now gates exactly **one** probe (`crates/rustyn64-test-harness/src/lib.rs`
`#[cfg(feature = "test-roms")]`, which the CI `test-roms` job asserts ran) — no *production* code
is gated, so `--features test-roms` runs the same emulation tests as a bare `cargo test
--workspace` plus that one probe. `commercial-roms` still gates **zero** code (the commercial
tests are gated by `#[ignore]` + runtime file checks instead). Same for the per-crate `std`
features: every chip crate is unconditionally `#![no_std]`, so `--no-default-features` is a no-op.
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

```fish
if cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
    git commit -F msg.txt
else
    echo "GATES FAILED"
end
```

**CI is split light/full:** ordinary feature PRs get only fmt/clippy/test/rustdoc/no_std on
ubuntu. The `test-roms` job and the macOS/Windows matrix run ONLY on push-to-main, the merge
queue, `release/*` PRs, dispatch, and a weekly cron. CI runs clippy exactly once — there are no
per-feature clippy jobs, so feature-gated code is currently unlinted.

Linux frontend needs system deps (wgpu/winit/cpal). Arch/CachyOS:
`sudo pacman -S --needed libxkbcommon wayland alsa-lib systemd-libs`
