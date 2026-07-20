# RustyN64 — STATUS (single source of truth)

This file is authoritative for per-suite pass counts, the board matrix, the
chip→crate map, and version policy. Everything else defers to it.

**Current release:** **v0.1.0 (SKELETON).** The workspace compiles, CI is green on
stubs, and the architecture (Bus + fractional master-clock scheduler + the
one-directional crate graph + the narrow chip bus traits) is in place. The
accuracy work — the LLE RSP, the LLE RDP, the VR4300 interpreter — has **not
started**; those are the major roadmap phases (`to-dos/ROADMAP.md`).

## What compiles today (v0.1.0)

- The Cargo workspace: all `rustyn64-*` crates build; `cargo test --workspace`
  passes on the skeleton unit tests.
- `rustyn64-core`: the `Bus` (owns RDRAM + every chip + the RCP/MI register
  state), the `System` run loop with the **3:2 fractional master-clock**
  accumulator and **seeded power-on phase alignment**, and the chip split-borrow
  stepping. The fractional-divisor and reset-preserves-phase tests pass.
- The chip crates: register-file / state skeletons with `tick` methods that are
  **LLE-shaped stubs** (decode/execute marked TODO).
- `rustyn64-cart`: real ROM-format detection + byte-order normalization
  (`.z64`/`.n64`/`.v64`), header parse, and the `SaveType`/`Cic`/`RomFormat`
  enums. PI/SI DMA, CIC handshake, and FlashRAM are stubbed.
- `rustyn64-test-harness`: the golden-log differ, `run_until_complete`, the
  accuracy scorer, and the frame comparator — all present; the golden source +
  probe battery are **stubbed** pending a reference trace.
- The chip stack is `#![no_std]` + `alloc` and cross-compiles to
  `thumbv7em-none-eabihf`; only the frontend carries `std` + `unsafe`.

## What is stubbed (the roadmap)

| Subsystem | State | Phase |
|---|---|---|
| VR4300 decode/execute, TLB, FPU, caches | stub | Phase 1 |
| RSP LLE (SU interpreter, then VU) | stub | Phase 2 |
| RDP LLE (software reference rasterizer) + VI scan-out | stub | Phase 3 |
| AI audio DMA double-buffer | stub | Phase 4 |
| PI/SI DMA, PIF/CIC boot, FlashRAM machine, saves | stub | Phase 5 |
| Frontend egui shell (binary prints a placeholder) | stub | Phase 6 |
| Accuracy battery / breadth / reach | stub | Phases 7–8 |

## Chip → crate map

| Crate | Chip / role | Spec doc |
|---|---|---|
| `rustyn64-cpu` | NEC VR4300 (MIPS III, TLB, FPU, SysAD) | `docs/cpu.md` |
| `rustyn64-rsp` | RSP (SU + VU, DMEM/IMEM, microcode) | `docs/rsp.md` |
| `rustyn64-rdp` | RDP rasterizer + VI scan-out | `docs/rdp.md` |
| `rustyn64-audio` | AI DAC + sample DMA | `docs/audio.md` |
| `rustyn64-cart` | PI cart + PIF/CIC + SI + saves | `docs/cart.md` |
| `rustyn64-core` | Bus + fractional master-clock scheduler | `docs/scheduler.md` |
| `rustyn64-frontend` | egui/wgpu/cpal/winit shell (bin `rustyn64`) | `docs/frontend.md` |
| `rustyn64-test-harness` | golden-log + accuracy + frame comparators | `docs/testing-strategy.md` |
| `rustyn64-netplay` | rollback netplay (frontend-side) | `docs/frontend.md` |
| `rustyn64-cheevos` | RetroAchievements FFI (later, off by default) | — |

## Accuracy

| Gate | Status |
|---|---|
| CPU/RSP golden-log (n64-systemtest reference trace) | not started (golden source stubbed) |
| n64-systemtest "Failed: 0" (CPU/COP0/TLB/RSP) | not started |
| ParaLLEl-RDP fuzz suite (RDP bit-exactness) | not started |
| Accuracy battery (AccuracyCoin-equivalent) | 0% (battery stubbed) |
| Visual golden / screenshots | not started |

See `docs/testing-strategy.md` for the oracle and the five test layers.

## Board / mapper matrix

**Tiered (Core / Curated / BestEffort) under an honesty gate: NO.** The N64 has
one cart model parameterized by save type + CIC + region, not hundreds of
mappers — so there is no board tiering and no honesty-gate test (ADR 0003). The
accuracy oracle is n64-systemtest pass/fail + the RDP fuzz suite, not a tier
matrix.

Save-type coverage target (per-game DB resolved): EEPROM 4k/16k, SRAM, FlashRAM,
Controller Pak (`docs/cart.md`).

## Version policy

- Start at **v0.1.0**; this is a clean N64 project. Additive features land behind
  default-off flags so the shipped / native / `no_std` / wasm builds stay
  byte-identical with the flags off.
- **Do NOT import RustyNES engine-lineage "v2.0" anchors as RustyN64 releases.**
  The fractional master-clock scheduler exists today (the v0.1.0 core). The
  *future* sub-cycle φ1/φ2 timebase refactor is ADR 0002 — a later milestone, not
  a current release, and the one expected to break byte-identity / save-state
  compatibility.
- v1.0.0 is the production cut (Phases 1–8 complete; README/CHANGELOG/docs/STATUS
  in sync; release matrix + Pages green). See `to-dos/ROADMAP.md`.
