# Contributing

Thanks for your interest in contributing to RustyN64. This is a pure-Rust,
accuracy-focused Nintendo 64 emulator; contributions should keep the workspace
lint-clean and the determinism contract intact.

## Development setup

- Install [rustup](https://rustup.rs).
- The toolchain is pinned in `rust-toolchain.toml` (Rust 1.97, edition 2024);
  `rustup` auto-installs it on first build.
- `cargo check --workspace` to verify the workspace compiles.
- `cargo test --workspace` to run the unit + integration tests.
- `cargo test --workspace --features test-roms` to add the accuracy oracle once
  reference traces / test ROMs are wired (roadmap).
- On Linux the frontend pulls in wgpu/winit/cpal system deps:
  - Debian/Ubuntu: `sudo apt-get install -y libxkbcommon-dev libwayland-dev
    libxkbcommon-x11-dev libasound2-dev libudev-dev`
  - Arch/CachyOS: `sudo pacman -S --needed libxkbcommon wayland alsa-lib systemd-libs`

## Workflow

1. Pick a ticket from `to-dos/` (or open an issue first if your work isn't
   already represented there). Tickets have stable IDs `T-PS-NNN`.
2. Create a branch: `<type>/<short-description>` (e.g.,
   `feat/vr4300-load-store`, `fix/rdp-triangle-edge-walk`).
3. Make changes. Keep commits focused. A chip-behavior change touches BOTH the
   chip code and that chip's `docs/<chip>.md` — don't let them drift.
4. Run the local quality gate before pushing.
5. Open a PR. Reference the ticket(s) and any relevant `docs/` files.

## Quality gate

Before opening a PR, ensure (these all run in CI and must be green):

- [ ] `cargo fmt --all --check` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] `cargo build -p rustyn64-core --target thumbv7em-none-eabihf
      --no-default-features` passes (the chip stack must stay `no_std + alloc`)
- [ ] `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` passes
- [ ] New public items have rustdoc
- [ ] `unsafe` carries a `// SAFETY:` comment (frontend + FFI only)
- [ ] No emojis in code, comments, or commits (project policy)

Never use `--all-features` (mutually-exclusive backend features can't resolve);
CI uses explicit feature sets.

## Documentation expectations

- New subsystems get a doc in `docs/`.
- Architecture-affecting changes update `docs/architecture.md`.
- `docs/STATUS.md` is the single source of truth for per-suite pass counts, the
  board matrix, and the chip-to-crate map; keep it accurate.
- User-visible changes are noted in `CHANGELOG.md` under `[Unreleased]`.
- Ticket completion is reflected in the relevant `to-dos/` sprint file.

## Accuracy work

- Test ROMs are the spec. When the docs and a passing test ROM disagree, the ROM
  wins and the docs get updated.
- Pin the failing test-ROM expectation first, then implement until it passes.
- Never commit commercial Nintendo ROMs. Your own dumps live in the gitignored
  `tests/roms/external/`.

## Commit messages

Use [Conventional Commits](https://www.conventionalcommits.org):
`<type>(<scope>): <subject>`

Types: `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `perf`, `build`,
`ci`. Imperative subject ≤ 72 chars; blank line, then an optional body
explaining the why (not the what — the diff shows the what).

## Code review

- One reviewer minimum; two for changes to `docs/architecture.md` or
  cross-subsystem refactors.
- Reviewers focus on correctness, design, and adherence to the relevant `docs/`
  specification.
- Discussion is preferred over deferral; if a comment can't be resolved in
  review, file a follow-up ticket explicitly.
- Every change ships via a pull request, and the automated reviewers (Copilot,
  Gemini Code Assist) are part of that review. Their comments are adjudicated,
  answered individually, and resolved — see `AGENTS.md` §Shipping, which also
  records who holds merge authority and when to stop and ask instead.
