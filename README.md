<!-- markdownlint-disable MD033 MD041 -->
<div align="center">

# RustyN64

> **Precise. Pure. Powerful.**

</div>

<p align="center">
  <a href="https://github.com/doublegate/RustyN64/actions"><img src="https://github.com/doublegate/RustyN64/workflows/CI/badge.svg" alt="Build Status"></a> <a href="#license"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg" alt="License: MIT OR Apache-2.0"></a> <a href="https://github.com/doublegate/RustyN64/releases"><img src="https://img.shields.io/badge/version-v0.1.0-blue.svg" alt="Version"></a> <a href="rust-toolchain.toml"><img src="https://img.shields.io/badge/rust-1.97-orange.svg" alt="Rust: 1.97"></a><br>
  <a href="#compatibility-and-accuracy"><img src="https://img.shields.io/badge/status-phase%201%20in%20progress-yellow.svg" alt="Status: Phase 1 in progress"></a> <a href="#compatibility-and-accuracy"><img src="https://img.shields.io/badge/accuracy-basic.z64%205%2F5-yellow.svg" alt="Accuracy: basic.z64 5/5"></a> <a href="https://doublegate.github.io/RustyN64/"><img src="https://img.shields.io/badge/pages-rustdoc-success.svg" alt="GitHub Pages"></a><br>
  <a href="#platform-support"><img src="https://img.shields.io/badge/platform-Windows%20%7C%20Linux%20%7C%20macOS-lightgrey.svg" alt="Platform"></a>
</p>

## Overview

**RustyN64 is a cycle-accurate Nintendo 64 emulator written in pure Rust.** Following the
lineage of [`RustyNES`](https://github.com/doublegate/RustyNES) and
[`RustySNES`](https://github.com/doublegate/RustySNES), it targets the ares / CEN64 / Gopher64 /
ParaLLEl accuracy bar: one canonical 187.5 MHz master-clock timeline co-scheduling three asynchronous
compute engines, a Bus that owns everything mutable, low-level emulation of the programmable
coprocessors, and a hard determinism contract.

> ### Status: Phase 1 in progress — not playable
>
> **The VR4300 executes instructions.** The canonical master clock, the five-stage pipeline, the
> MIPS III integer set, COP0, the TLB and the exception model are implemented, and Dillon's
> `basic.z64` runs end to end and passes 5/5.
>
> **Everything else is still a stub.** The LLE RSP, the LLE RDP, AI audio, and PI/SI DMA do not
> execute anything; the FPU has control registers but no arithmetic; the `rustyn64` binary opens
> a shell and presents a test pattern. Stubs are no-op `TODO(...)` bodies rather than `todo!()`
> panics, so **a green test run does not mean a subsystem works**.
>
> **[`docs/STATUS.md`](docs/STATUS.md) is the single source of truth.** Read it before assuming
> any feature works.

---

## Why RustyN64?

RustyN64 applies the accuracy-first architecture proven in its two sibling projects to a
markedly harder machine. The N64 is not an 8- or 16-bit console with a CPU and a video chip: it
is two large chips on a unified Rambus memory pool, with a programmable SIMD coprocessor running
game-supplied microcode and a fixed-function rasteriser fed by a command list.

**Key differentiators:**

- **The right timebase for this machine.** The VR4300 cycle is the master tick
  (`MASTER_HZ = 187_500_000`), the LCM of the 93.75 MHz CPU and 62.5 MHz RCP clocks. Every
  emulated domain is an **integer divisor** of it: the CPU steps every 2nd tick, the RCP every
  3rd, COP0 `Count` every 4th, SI every 12th, PIF every 96th — so drift is unrepresentable
  rather than merely avoided. `master_ticks` is the only counter ever incremented; every other
  cycle position is derived from it and pinned by a residue invariant test. Only VI and AI,
  which genuinely run off a different crystal, keep a fractional accumulator (ADR 0006).
- **LLE, never HLE, in the core.** The N64 let every studio ship custom microcode, so
  signature-matching HLE is per-game-fragile and mis-renders mid-frame effects. The RSP executes
  the real instruction stream and the RDP rasterises the real command list. Audio falls out free,
  because the audio microcode runs on the same LLE core — there is no per-game audio HLE
  (ADR 0002).
- **Determinism as a hard contract.** Same seed, ROM, and input sequence yield a bit-identical
  framebuffer and audio. Power-on CPU/RCP phase — genuinely indeterminate on hardware — is
  modelled as a *seeded* parameter rather than live entropy, so it is reproducible and
  save-stateable (ADR 0004).
- **Honest status reporting.** This README, `docs/STATUS.md`, and the accuracy tables all
  distinguish "the oracle ROM is staged" from "the gate passes". Today no gate passes, and every
  document says so.
- **Safe, modular Rust.** The chip stack is `no_std + alloc` with a one-directional crate graph,
  so each chip is independently fuzzable and benchmarkable. Every chip crate and `-core` carries
  `#![forbid(unsafe_code)]`; there is zero `unsafe` in the tree.

---

## Highlights

| Feature | Status |
| --- | --- |
| **Canonical 187.5 MHz master clock** | **Working** — one incremented counter; CPU/RCP/`Count` derived from it, seeded power-on phase, reset-preserves-phase; pinned by a residue-invariant test (ADR 0006) |
| **Bus owns all mutable state** | **Working** — RDRAM + every chip + MI lines; five narrow per-chip traits; `core::mem::take` split-borrow stepping |
| **One-directional crate graph** | **Working** — exactly one permitted chip-to-chip edge (`rdp` → `cart`, for `RdramBus`) |
| **ROM format handling** | **Working** — `.z64` / `.n64` / `.v64` detection and byte-order normalisation, header parse, save-type and CIC enums |
| **VR4300 five-stage pipeline** | **Working** — four inter-stage latches, reverse cascade, operand bypass, imprecise load interlock, delay slots and branch-likely (ADR 0007) |
| **VR4300 MIPS III integer set** | **Working** — incl. the 64-bit `D*` forms, unaligned `LWL`/`LWR`/`LDL`/`LDR`, `LL`/`SC`, and the documented errata reproduced rather than corrected |
| **COP0, exceptions, interrupts** | **Working** — full register file, exception epilogue and vectors, `ERET`, `Count`/`Compare` timer, MI interrupt line |
| **TLB + instruction micro-TLB** | **Working** — 32 joint entries, page pairs, 4K–16M sizes, TLB shutdown; a micro-TLB miss is a stall, a JTLB miss an exception |
| **VR4300 FPU (COP1)** | Control registers only — arithmetic is Phase 1 Sprint 3 |
| **LLE RSP (scalar + vector units)** | Stub — Phase 2 |
| **LLE RDP + VI scan-out** | Stub — Phase 3 |
| **AI audio** | Stub — Phase 4 |
| **PI/SI DMA, PIF/CIC boot, saves** | Stub — Phase 5 |
| **egui shell** | Partial — the shell, input map, and framebuffer plumbing are real; it presents a test pattern |
| **Test-ROM oracle** | **Partly wired** — Dillon's `basic.z64` runs end to end and passes 5/5; n64-systemtest is committed but not yet reporting; krom, 240p and a 66-ROM commercial corpus staged locally |
| **Hardware reference** | **Working** — offline N64brew Wiki mirror (324 pages, 96 media) rebuilt by a script |
| **`no_std` chip stack** | **Working** — cross-compiles to `thumbv7em-none-eabihf` |
| **wasm** | Compiles for `wasm32-unknown-unknown`; no browser entry point yet — Phase 6 |
| **Netplay / RetroAchievements** | Placeholder crates — Phase 8 |

---

## Features

### Emulation core

- **One canonical master clock.** A single 187.5 MHz counter is the *only* incremented cycle
  position in the core; the VR4300 (÷2), the RCP (÷3) and COP0 `Count` (÷4) are all **derived**
  from it, so they cannot drift apart (ADR 0006). A residue-invariant test in the default test
  path is what keeps it that way. "Lockstep" here means *not catch-up*: an RCP event — a DP-done
  IRQ, an SP halt, an AI buffer drain — is visible to the very next CPU step, which is what makes
  mid-frame coprocessor effects work without per-quirk patches.
- **A cycle-accurate five-stage pipeline.** The VR4300 is four inter-stage latches advanced one
  PClock per step in reverse stage order (`WB → DC → EX → RF → IC`), so the reverse order *is*
  the latching (ADR 0007). `DC` is the cycle the scheduler interleaves the RCP around — the
  reason a device can observe the bus partway through an instruction, which a
  one-`tick`-per-instruction CPU cannot express at all.
- **The MIPS III integer set, COP0, the TLB and the exception model.** Delay slots and
  branch-likely nullification, the imprecise load-delay interlock, the operand bypass network,
  `LL`/`SC`, a 32-entry TLB with its two-entry instruction micro-TLB, and the documented VR4300
  errata **reproduced rather than corrected** — each pinned by a named test that fails if someone
  "fixes" it.
- **The Bus owns everything mutable.** `rustyn64-core::Bus` holds RDRAM (8 MiB: 4 MiB base plus
  Expansion Pak), the RSP, RDP, AI, cart, controllers, and the MI interrupt lines. The CPU
  borrows `&mut Bus`; each chip sees only the narrow trait it needs. Chips are stepped with a
  `core::mem::take` split-borrow — no allocation, no `Rc`/`RefCell`.
- **One cart model, no board tiering.** Unlike the NES with its hundreds of mappers, the N64 has
  a single cart parameterised by save type, CIC variant, and region — so there is no board tier
  matrix and no honesty gate here (ADR 0003).

### Test infrastructure

The oracle is staged ahead of the emulator, so accuracy work is graded from day one rather than
retrofitted:

- **`n64-systemtest`** (MIT, committed) — the strict CPU/COP0/TLB/RSP gate. Self-judging: it
  reports `Failed: 0` itself, so no image comparison is needed. Built from source, since upstream
  publishes no prebuilt ROM.
- **External corpora** (gitignored) — PeterLemon/krom (196 ROMs), Dillon's n64-tests (26), the
  240p Test Suite (built from source in a container), and a 66-ROM commercial regression corpus
  organised by save type, with save types resolved by MD5 against the mupen64plus catalogue
  rather than guessed.
- **Three-layer ROM guard** — commercial ROMs cannot enter the repository. `.gitignore` covers
  the ordinary case, a pre-commit hook covers `git add -f` (which bypasses `.gitignore`
  silently) and renamed files (via a size ceiling), and a CI job re-scans the whole tracked tree
  server-side. Verified against a real bypass attempt, not assumed.
- **Offline hardware reference** — an N64brew Wiki mirror with parallel HTML, Markdown, and
  wikitext trees, so register-level detail is greppable without a network round-trip.

---

## Quick Start

### Build from source

**Prerequisites:**

- **Rust 1.97** — pinned via `rust-toolchain.toml` and auto-installed by
  [rustup](https://rustup.rs).
- **Linux desktop dependencies** for `winit` / `wgpu` / `cpal` / `egui` (see below).
- **Git.**

```bash
# Clone the repository
git clone https://github.com/doublegate/RustyN64.git
cd RustyN64

# Build the workspace (release)
cargo build --release --workspace

# Run — the shell opens and presents a test pattern; no game will play yet
cargo run --release -p rustyn64-frontend -- path/to/rom.z64
```

Never use `--all-features` on this workspace — mutually-exclusive backend features cannot
resolve. CI uses explicit feature sets.

### Quality gates

All of these gate in CI and must be green:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings   # NEVER --all-features
cargo test --workspace
cargo test --workspace --features test-roms
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo build -p rustyn64-core --target thumbv7em-none-eabihf --no-default-features
bash scripts/check_no_roms.sh
```

### Platform-specific dependencies

**Ubuntu / Debian:**

```bash
sudo apt-get install -y libxkbcommon-dev libwayland-dev libxkbcommon-x11-dev libasound2-dev libudev-dev
```

**CachyOS / Arch:**

```bash
sudo pacman -S --needed libxkbcommon wayland alsa-lib systemd-libs
```

**macOS / Windows:** no extra system dependencies are required for the default build.

---

## Default Controls

The input map is implemented in the shell, though nothing yet consumes it through the SI joybus
(Phase 6).

| Action | Key |
| --- | --- |
| Analog stick | Arrow keys |
| D-pad | I / J / K / L |
| A / B | X / Z |
| Z trigger | Space |
| Start | Enter |
| C-buttons | T / F / G / H |
| L / R | Q / E |

`rustyn64 --help` prints usage and the keymap; `--version` prints the version.

---

## Architecture

Read [`docs/architecture.md`](docs/architecture.md) before any chip doc — it carries the eight
load-bearing facts that explain why the per-chip specs read the way they do.

Three of those facts, in brief:

1. **One canonical master clock; everything is lockstep.** The master tick is 187.5 MHz — the
   LCM of the 93.75 MHz CPU and 62.5 MHz RCP clocks — so every emulated domain is an integer
   divisor (CPU 2, RCP 3, COP0 `Count` 4, SI 12, PIF 96) and drift is unrepresentable rather
   than merely avoided. `master_ticks` is the only counter ever incremented (ADR 0006).
2. **The Bus owns everything mutable.** One owner, narrow per-chip traits, split-borrow stepping
   — which avoids the "CPU holds the coprocessor, but the coprocessor needs the CPU bus" cycle.
3. **The crate graph is one-directional,** with exactly one permitted chip-to-chip edge:
   `rustyn64-rdp` → `rustyn64-cart`, solely to borrow the `RdramBus` trait. Everything else
   depends on `rustyn64-core`, which re-exports the chip types.

### Workspace crates

| Crate | Role |
| --- | --- |
| `rustyn64-cpu` | NEC VR4300 (MIPS III, TLB, FPU, SysAD) |
| `rustyn64-rsp` | RSP — scalar unit + vector unit, DMEM/IMEM, downloadable microcode |
| `rustyn64-rdp` | RDP rasteriser + VI scan-out |
| `rustyn64-audio` | AI DAC + sample DMA |
| `rustyn64-cart` | PI cart + PIF/CIC boot + SI + saves |
| `rustyn64-core` | The Bus + canonical master-clock scheduler tie crate |
| `rustyn64-frontend` | The `winit + wgpu + cpal + egui` shell (binary `rustyn64`) |
| `rustyn64-test-harness` | Golden-log differ, accuracy scorer, frame comparator |
| `rustyn64-netplay` | Rollback netplay orchestration (placeholder) |
| `rustyn64-cheevos` | RetroAchievements FFI (placeholder) |

### Project layout

```text
crates/         Cargo workspace: the crates above
docs/           Subsystem specs, ADRs, and STATUS.md (single source of truth)
ref-docs/       Immutable deep-research N64 hardware reference
ref-proj/       Gitignored study clones of reference emulators (licence-classified)
n64brew_wiki/   Gitignored offline mirror of the N64brew Wiki
tests/roms/     Committed permissive corpus + gitignored external/ (never committed)
scripts/        The wiki mirror tool and the commercial-ROM guard
to-dos/         ROADMAP.md plus per-phase overviews and sprint ticket breakdowns
```

---

## Compatibility and Accuracy

**One gate reports a real number; the rest do not.** The distinction that matters here is that
"oracle available" means the ROM is on disk — it says nothing about whether the emulator can
execute it.

| Gate | Oracle available? | Status |
| --- | --- | --- |
| Dillon `basic.z64` (control flow) | **Yes** — external tier | **Passing** — 5/5 |
| Determinism (ADR 0004) | n/a — self-checking | **Passing** — exercised, not merely specified |
| n64-systemtest `Failed: 0` (CPU/COP0/TLB/RSP) | **Yes** — ROM committed | Not yet reporting |
| CPU/RSP golden-log (reference trace) | No — needs a cen64/ares capture | Not started |
| ParaLLEl-RDP fuzz suite (RDP bit-exactness) | Source cloned, suite not set up | Not started |
| Accuracy battery | Probes not authored | 0% (battery stubbed) |
| Visual golden / screenshots | **Yes** — krom + 240p + commercial staged | Not started |

Where the hardware documentation is silent, or contradicts itself, the project records that
rather than guessing quietly: [`docs/accuracy-ledger.md`](docs/accuracy-ledger.md) tracks measured
constants with their provenance, genuinely undocumented behaviour awaiting a hardware pin,
deliberate deviations, and contradictions **within the vendor manual itself**. A measured constant
is never tuned to make a ROM pass — the moment one is, every later timing result built on it stops
being evidence.

> A note on test counts: RustyN64 will be validated by closed-form test ROMs (n64-systemtest,
> the ParaLLEl-RDP fuzz suite, krom, Dillon's tests) and a commercial-ROM regression corpus — not
> by a headline unit-test number. When a doc and a passing test ROM disagree, **the ROM wins** —
> that is this project's definition of "cycle-accurate."

---

## Performance

**No performance measurements exist yet**, and publishing any would be meaningless while every
chip is a stub. The strategy is recorded in [`docs/performance.md`](docs/performance.md):
correctness first, then accelerate validated layers. The software reference RDP lands before any
wgpu-compute backend, and stays the oracle that backend is graded against (ADR 0002).

---

## Platform Support

| Platform | Status |
| --- | --- |
| **Linux x64** | Built and tested in CI |
| **macOS ARM64** | Built and tested in CI |
| **Windows x64** | Built and tested in CI |
| **`thumbv7em-none-eabihf`** | Chip stack cross-compiles (`no_std` gate) |
| **WebAssembly** | Compiles; no browser entry point yet (Phase 6) |

### System requirements

- **Rust 1.97 stable** (pinned via `rust-toolchain.toml`; auto-installed by `rustup`).
- A GPU with a `wgpu`-supported backend (Vulkan / Metal / DX12).
- Linux additionally needs the alsa/udev/xkbcommon/wayland development headers listed above.

---

## Documentation

| Document | Description |
| --- | --- |
| [Project status](docs/STATUS.md) | **Single source of truth** — per-subsystem state, infrastructure, corpora, version policy |
| [Architecture](docs/architecture.md) | The eight load-bearing facts — read before any chip doc |
| [Scheduler](docs/scheduler.md) | The canonical 187.5 MHz master-clock model |
| [Testing strategy](docs/testing-strategy.md) | The oracle, the five test layers, and the corpus tiers |
| [Roadmap](to-dos/ROADMAP.md) | The phase spine, Phase 0 through Phase 8 |
| [Version plan](to-dos/VERSION-PLAN.md) | The named release ladder from `v0.1.0` to `v1.0.0` and beyond |
| [Lockstep checklist](to-dos/LOCKSTEP-CHECKLIST.md) | The process for tracking the two sibling projects when scoping each release |
| [CHANGELOG.md](CHANGELOG.md) | Version history |
| [API documentation](https://doublegate.github.io/RustyN64/) | Published rustdoc for every workspace crate |

### Hardware and subsystem specs

| Component | Location |
| --- | --- |
| CPU (VR4300) | [docs/cpu.md](docs/cpu.md) |
| RSP | [docs/rsp.md](docs/rsp.md) |
| RDP + VI | [docs/rdp.md](docs/rdp.md) |
| Audio (AI) | [docs/audio.md](docs/audio.md) |
| Cartridge / PI / saves | [docs/cart.md](docs/cart.md), [docs/cartridge-format.md](docs/cartridge-format.md) |
| Frontend | [docs/frontend.md](docs/frontend.md) |
| Compatibility | [docs/compatibility.md](docs/compatibility.md) |
| Glossary | [docs/glossary.md](docs/glossary.md) |

Architecture Decision Records live in [`docs/adr/`](docs/adr/) in Michael Nygard format.
Immutable primary research lives in [`ref-docs/`](ref-docs/) — never rewritten in place;
corrections land as new dated supplemental files.

---

## Current Release

**v0.1.0 "Foundation"** is the current tag. The next rung is **v0.2.0 "Interpreter"**, whose cut
criterion is n64-systemtest reporting `Failed: 0` for the CPU/COP0/TLB categories — a real oracle
result, not a self-assessment. The release ladder is [`to-dos/VERSION-PLAN.md`](to-dos/VERSION-PLAN.md);
long-form notes live in [`docs/release-notes/`](docs/release-notes/).

- **Authoritative current state:** [`docs/STATUS.md`](docs/STATUS.md).
- **Full history:** [`CHANGELOG.md`](CHANGELOG.md).

## Roadmap

Nine phases. **Phase 0 is complete** and **Phase 1 is in progress**:

- **Phase 0 — Foundation** *(complete)* — workspace, CI, the Bus and scheduler, and the acquired
  and licence-classified reference corpus.
- **Phase 1 — CPU golden log** *(in progress)* — the VR4300 to a 0-diff trace and
  n64-systemtest `Failed: 0`. The integer core, COP0, the TLB and the exception model are in;
  the FPU and the golden-log differ are not.
- **Phase 2 — RSP LLE** — the scalar and vector units running real microcode.
- **Phase 3 — RDP LLE + VI** — the software reference rasteriser and scan-out; first picture.
- **Phase 4 — AI audio** — the interface and its timing; the microcode already runs on the RSP.
- **Phase 5 — Cart boot + saves** — PI, SI/joybus, CIC, and all four save backends.
- **Phase 6 — Frontend integration** — real scan-out, audio, and input; save-states and the wasm
  entry point.
- **Phase 7 — Accuracy breadth** — the battery across the commercial corpus; the accuracy ledger.
- **Phase 8 — Reach** — netplay, achievements, TAS, scripting, shaders — all off by default.

The full spine, per-phase exit criteria, and the ticket tree live in
[`to-dos/ROADMAP.md`](to-dos/ROADMAP.md) and the per-phase overviews.

---

## Contributing

Contributions of all kinds are welcome — code, testing, documentation, and design. Please read
[`CONTRIBUTING.md`](CONTRIBUTING.md) for the quality-gate contract, the conventional-commit
format, and the chip-behavior-change rule (a chip change touches both the code and its
`docs/<chip>.md` in the same PR).

### Quick contribution workflow

```bash
# 1. Fork and clone, then create a feature branch
git checkout -b feat/my-feature

# 2. Make changes and run the quality gates
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check

# 3. Commit using conventional commits, then push and open a PR
git commit -m "feat(cpu): implement <thing>"
git push origin feat/my-feature
```

Test ROMs are the spec: pin the failing ROM expectation first, then implement until it passes.
Never commit commercial ROMs — three independent guards enforce this, and they are tested.

---

## License

RustyN64 is dual-licensed under your choice of:

- **[MIT License](LICENSE-MIT)** — permissive, allows commercial use.
- **[Apache License 2.0](LICENSE-APACHE)** — permissive with a patent grant.

Unless you state otherwise, any contribution you submit is dual-licensed as above.

**Test ROMs** under `tests/roms/` are individually licensed and tiered: the committed tier is
permissive only (currently `n64-systemtest`, MIT, shipping its upstream `LICENSE`), and
everything copyleft, unlicensed, or commercial stays in the gitignored external tier. **No
commercial Nintendo ROMs are included, and they will never be bundled** — dumps used for
regression testing are the user's responsibility and must come from cartridges they legally own.

---

## Acknowledgments

RustyN64 stands on the shoulders of giants:

- The **[N64brew Wiki](https://n64brew.dev/wiki/Main_Page)** community for the hardware
  documentation this project is built against (CC BY-SA 4.0).
- **[ares](https://github.com/ares-emulator/ares)**, **[CEN64](https://github.com/n64dev/cen64)**,
  **[Gopher64](https://github.com/gopher64/gopher64)**, and
  **[ParaLLEl-RDP](https://github.com/Themaister/parallel-rdp)** as the accuracy reference bar.
- **[lemmy-64's n64-systemtest](https://github.com/lemmy-64/n64-systemtest)**,
  **[Dillon's n64-tests](https://github.com/Dillonb/n64-tests)**, and
  **[PeterLemon/N64](https://github.com/PeterLemon/N64)** as the closed-form definition of
  "cycle-accurate" used by this project.
- **[libdragon](https://github.com/DragonMinded/libdragon)** and Artemio Urbina's
  **[240p Test Suite](https://github.com/ArtemioUrbina/240pTestSuite)** for the homebrew toolchain
  and the video-timing reference.
- **[RustyNES](https://github.com/doublegate/RustyNES)** and
  **[RustySNES](https://github.com/doublegate/RustySNES)**, this project's own predecessors, for
  the Bus-owns-everything architecture, the frontend shell, and the CI/docs infrastructure
  pattern ported here.

---

## Citation

If you use RustyN64 in academic research, please cite:

```bibtex
@software{rustyn642026,
  author  = {RustyN64 Contributors},
  title   = {RustyN64: A Cycle-Accurate Nintendo 64 Emulator in Rust},
  year    = {2026},
  version = {0.1.0},
  url     = {https://github.com/doublegate/RustyN64},
  note    = {Cycle-accurate N64 emulator on a canonical 187.5 MHz master-clock scheduler with
             low-level emulation of the RSP and RDP; a Bus-owns-everything architecture,
             a one-directional no_std chip-crate graph, and a hard determinism contract;
             pure-Rust winit/wgpu/cpal/egui frontend. v0.1.0 is an architectural skeleton:
             the scheduler and bus are implemented, the chips are not}
}
```

---

<p align="center">
  <strong>Built with Rust. Powered by passion for retro gaming.</strong><br>
  <sub>Preserving video game history, one frame at a time.</sub>
</p>

<p align="center">
  <a href="#quick-start">Get Started</a> ·
  <a href="docs/STATUS.md">Current Status</a> ·
  <a href="CONTRIBUTING.md">Contribute</a> ·
  <a href="docs/">Documentation</a>
</p>
