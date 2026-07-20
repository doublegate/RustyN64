<!-- markdownlint-disable MD033 MD041 -->
<div align="center">

# RustyN64

**A cycle-accurate Nintendo 64 emulator in Rust (LLE RSP/RDP).**

</div>

RustyN64 is a cycle-accurate Nintendo 64 emulator in pure Rust, architected at the
ares / CEN64 / Gopher64 / ParaLLEl accuracy bar: one fractional master-clock timeline
co-scheduling three asynchronous compute engines, a Bus that owns everything mutable,
a one-directional `no_std + alloc` chip-crate graph, low-level emulation of the
programmable coprocessors, a hard determinism contract, and test-ROM-is-spec.

> **Status: v0.1.0 SKELETON — not playable.** The workspace compiles and CI is green,
> but that means the architecture is in place and 42 unit tests pass, *not* that any
> chip emulates anything. The VR4300 interpreter, the LLE RSP, the LLE RDP, AI audio,
> and PI/SI DMA are all stubs; the `rustyn64` binary opens a shell and presents a test
> pattern. **`docs/STATUS.md` is the single source of truth** — read it before
> assuming a feature works.

- **API docs:** <https://doublegate.github.io/RustyN64/>
- **Architecture:** `docs/architecture.md` (read this before any chip doc)
- **Roadmap:** `to-dos/ROADMAP.md` — nine phases from foundation to release

## Crates

| Crate | Role |
|---|---|
| `rustyn64-cpu` | NEC VR4300 (MIPS III, TLB, FPU, SysAD) |
| `rustyn64-rsp` | RSP — SU + VU, DMEM/IMEM, downloadable microcode |
| `rustyn64-rdp` | RDP rasterizer + VI scan-out |
| `rustyn64-audio` | AI DAC + sample DMA |
| `rustyn64-cart` | PI cart + PIF/CIC boot + SI + saves |
| `rustyn64-core` | the Bus + fractional master-clock scheduler tie crate |
| `rustyn64-frontend` | the `winit + wgpu + cpal + egui` shell (binary `rustyn64`) |
| `rustyn64-test-harness` | golden-log differ, accuracy scorer, frame comparator |
| `rustyn64-netplay` | rollback netplay orchestration (placeholder) |
| `rustyn64-cheevos` | RetroAchievements FFI (placeholder) |

## Platform support

| Target | Status |
|---|---|
| Linux x86_64 | built and tested in CI |
| macOS aarch64 | built and tested in CI |
| Windows x86_64 | built and tested in CI |
| `thumbv7em-none-eabihf` | chip stack cross-compiles (`no_std` gate) |
| `wasm32-unknown-unknown` | compiles, but no browser entry point yet (Phase 6) |

Toolchain is pinned to Rust 1.96 (edition 2024) via `rust-toolchain.toml`.

## Build / test

```bash
cargo check --workspace
cargo test --workspace
cargo test --workspace --features test-roms
cargo run --release -p rustyn64-frontend -- path/to/rom.z64
```

Never use `--all-features` — mutually-exclusive backend features cannot resolve.
CI uses explicit feature sets.

On Linux the frontend needs system libraries for wgpu/winit/cpal:

```bash
# Arch / CachyOS
sudo pacman -S --needed libxkbcommon wayland alsa-lib systemd-libs
# Debian / Ubuntu
sudo apt-get install -y libxkbcommon-dev libwayland-dev libxkbcommon-x11-dev \
                        libasound2-dev libudev-dev
```

## Test ROMs

The oracle is layered — unit tests, a CPU/RSP golden-log differ, the test-ROM corpus,
an accuracy battery, and visual golden frames. See `docs/testing-strategy.md`.

`tests/roms/` holds a committed tier (permissively licensed only, each ROM shipping
its upstream `LICENSE`) and a gitignored external tier. **Commercial ROMs are never
committed** — three independent guards enforce it (`.gitignore`, a pre-commit hook,
and a CI job). See `tests/roms/README.md` before adding a corpus.

## Contributing

See `CONTRIBUTING.md`. In short: a chip change touches the chip code *and* that chip's
`docs/<chip>.md` in the same commit, test ROMs are the spec, and the quality gate
(`fmt`, `clippy -D warnings`, tests, rustdoc, the `no_std` cross-build) must be green.

## License

RustyN64 is dual-licensed under **MIT OR Apache-2.0**. See `LICENSE-MIT` and
`LICENSE-APACHE`.
