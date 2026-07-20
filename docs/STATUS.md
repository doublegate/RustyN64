# RustyN64 — STATUS (single source of truth)

This file is authoritative for per-suite pass counts, the board matrix, the
chip→crate map, and version policy. Everything else defers to it.

**Current release:** **v0.1.0 (SKELETON)** — but the tree has moved well past it.
**Phase 1 Sprint 1 is complete** (T-11-001 … T-11-008): the scheduler now counts
one canonical 187.5 MHz master clock with every other cycle position derived
from it (ADR 0006), and the VR4300 is a real five-stage pipeline executing the
MIPS III integer instruction set (ADR 0007). The superseded ADR 0001 timebase —
a 93.75 MHz tick with a 3:2 fractional accumulator — is **gone from the tree**,
not merely deprecated.

The remaining accuracy work is unstarted: COP0, the TLB, the exception model
(Sprint 2), COP1 and the golden-log 0-diff (Sprint 3), then the LLE RSP and RDP
(Phases 2–3). See `to-dos/ROADMAP.md`.

**Read this before trusting any green checkmark:** CI passing means the
workspace compiles and its **135** tests pass. The CPU genuinely executes
instructions now — that is new — but every other chip is still an LLE-shaped
stub. Stubs are `TODO(T-XXX-NN)` comments inside no-op bodies that compile and
return, not `todo!()` panics, so nothing fails loudly. And of the test-ROM
corpora below, exactly one ROM is actually executed by a gate (`basic.z64`);
the rest are staged only. Availability of an oracle is not the same as a wired
gate.

## What works today

- The Cargo workspace: all `rustyn64-*` crates build; `cargo test --workspace`
  passes 135 tests.
- `rustyn64-core`: the `Bus` (owns RDRAM + every chip + the RCP/MI register
  state), and the **canonical 187.5 MHz scheduler** (ADR 0006) — `master_ticks`
  is the only incremented counter; CPU (÷2), RCP (÷3) and COP0 `Count` (÷4)
  positions are derived accessors. Seeded per-domain power-on phase offsets;
  reset re-derives the same phase. Pinned by a residue-invariant test in the
  default test path.
- `rustyn64-cpu`: a **five-stage pipeline** (IC/RF/EX/DC/WB) of four inter-stage
  latches advanced one PClock per step in reverse stage order, with the operand
  bypass network, the imprecise load-delay interlock, delay slots and
  branch-likely nullification, the MIPS III integer set (including the 64-bit
  `D*` forms and the unaligned `LWL`/`LWR`/`LDL`/`LDR` family), the documented
  errata reproduced-not-corrected, and a `SysAD` transaction model that cannot
  complete inside its address phase.
- The other chip crates: register-file / state skeletons with `tick` methods
  that are **LLE-shaped stubs** (decode/execute marked TODO).
- `rustyn64-cart`: real ROM-format detection + byte-order normalization
  (`.z64`/`.n64`/`.v64`), header parse, and the `SaveType`/`Cic`/`RomFormat`
  enums. PI/SI DMA, CIC handshake, and FlashRAM are stubbed.
- `rustyn64-test-harness`: the golden-log differ, `run_until_complete`, the
  accuracy scorer, and the frame comparator — all present; the golden source +
  probe battery are **stubbed** pending a reference trace.
- The chip stack is `#![no_std]` + `alloc` and cross-compiles to
  `thumbv7em-none-eabihf`; only the frontend carries `std` + `unsafe`.

## Project infrastructure

Distinct from emulation progress: the scaffolding around the code, and where it
actually stands.

| Area | State |
| --- | --- |
| Repository | `github.com/doublegate/RustyN64`, **public**. Version-controlled since 2026-07-19; before that the tree had no git history of its own. |
| CI | **Green, verified.** All jobs pass on `ubuntu`/`macOS`/`windows`: `setup`, `test` (fmt + clippy + test + `no_std`), `rustdoc` (`-D warnings`, an independent job so a doc break cannot ride in behind a green test job), `test-roms`, `no-commercial-roms`, `wasm-bindgen-pin`. Split light/full: the `test-roms` job and the macOS/Windows matrix run only on push-to-main, the merge queue, `release/*` PRs, dispatch, and a weekly cron. |
| Docs site | **Live** — <https://doublegate.github.io/RustyN64/>. rustdoc publishes to `/api/`; `/` is reserved for the Phase 6 wasm demo and currently redirects. |
| Release | `release.yml` builds all three targets, packages archives with licences, generates `SHA256SUMS`, and publishes on a `v*` tag. Guarded so the tag must match the workspace version. Never yet exercised — no tag has been cut. |
| wasm | Compiles for `wasm32-unknown-unknown`, but there is **no browser entry point** (no `wasm-bindgen` dep, no `#[wasm_bindgen(start)]`, no `index.html`), so `trunk build` cannot produce a demo. Phase 6. |
| Hardware reference | `n64brew_wiki/` — offline mirror of the N64brew Wiki (324 pages, 96 media, gitignored). Rebuild with `scripts/mirror_n64brew_wiki.py`. |
| Reference emulators | `ref-proj/` — 11 study clones (ares, cen64, gopher64, simple64, parallel-rdp/rsp, angrylion, n64-systemtest, n64-tests, libdragon, PeterLemon). **Licences vary and several forbid copying** — read `ref-proj/README.md` first. |

## Test-ROM corpora

Full provenance and licence rules in `tests/roms/README.md`.

| Corpus | Licence | Tier | Staged |
| --- | --- | --- | --- |
| `n64-systemtest` | MIT | committed | 1 ROM, 2.7 MB — built from source |
| `krom` (PeterLemon) | Unlicense | external | 196 ROMs, 182 MB |
| `dillon-n64-tests` | none | external (run-only) | 26 ROMs, 38 MB |
| `240p Test Suite` | GPL-2.0-or-later | external | 1 ROM, 12 MB — built from source |
| commercial | copyrighted | external (never committed) | 66 ROMs, 1.5 GB |

Commercial ROMs are blocked by three independent guards (`.gitignore`,
`scripts/check_no_roms.sh` pre-commit, and the `no-commercial-roms` CI job); only
`tests/roms/n64-systemtest/` is allowlisted, and a committed ROM must ship its
upstream `LICENSE` beside it.

**Exactly one of these is executed by a gate today**: `basic.z64` from the
`dillon-n64-tests` corpus, which the harness runs end to end and judges by its
completion protocol (T-11-006). Everything else is staged only — the
n64-systemtest ROM cannot report a count until COP0/COP1/exceptions land
(Sprint 2), and the golden-log source still returns an empty `Vec`.

## What is stubbed (the roadmap)

| Subsystem | State | Phase |
| --- | --- | --- |
| VR4300 integer core, pipeline, delay slots, errata, SysAD | **done** (Sprint 1) | Phase 1 |
| VR4300 COP0, TLB, exception model | **done** (Sprint 2) | Phase 1 |
| VR4300 COP1 (FPU) | **partial** — see below | Phase 1 (Sprint 3) |
| CPU golden-log 0-diff | not started — no reference trace captured | Phase 1 (Sprint 3) |
| VR4300 I/D caches | stub | Phase 1 (Sprint 2, to observable depth) |
| RSP LLE (SU interpreter, then VU) | stub | Phase 2 |

**What "partial" means for COP1.** The register file (`FR` views), the control
registers, the data moves, and S/D `ADD`/`SUB`/`MUL`/`DIV` decode and execute.
Two things do **not** work, and neither is visible from a green `cargo test`:

- **Arithmetic is correct only in round-to-nearest-even.** `FCSR.RM`'s three
  directed modes and the `FS` denormal flush are ignored, because the operations
  use Rust's native `+`/`-`/`*`/`/`. This is accuracy-ledger **C-10** and is the
  single largest contributor to n64-systemtest's remaining failures.
- **Enabled FP traps do not raise.** `Cause`/`Flags` are set in `FCSR`, but an
  enabled exception does not become `Exception::FloatingPoint`.

`SQRT`, `ABS`, `NEG`, `MOV`, the conversions and the `C.cond.fmt` compares are
implemented in `fpu.rs` but **not yet decoded**, so they remain unreachable.
| RDP LLE (software reference rasterizer) + VI scan-out | stub | Phase 3 |
| AI audio DMA double-buffer | stub | Phase 4 |
| PI/SI DMA, PIF/CIC boot, FlashRAM machine, saves | stub | Phase 5 |
| Frontend egui shell (binary prints a placeholder) | stub | Phase 6 |
| Accuracy battery / breadth / reach | stub | Phases 7–8 |

## Chip → crate map

| Crate | Chip / role | Spec doc |
| --- | --- | --- |
| `rustyn64-cpu` | NEC VR4300 (MIPS III, TLB, FPU, SysAD) | `docs/cpu.md` |
| `rustyn64-rsp` | RSP (SU + VU, DMEM/IMEM, microcode) | `docs/rsp.md` |
| `rustyn64-rdp` | RDP rasterizer + VI scan-out | `docs/rdp.md` |
| `rustyn64-audio` | AI DAC + sample DMA | `docs/audio.md` |
| `rustyn64-cart` | PI cart + PIF/CIC + SI + saves | `docs/cart.md` |
| `rustyn64-core` | Bus + scheduler (ADR 0006 canonical 187.5 MHz clock) | `docs/scheduler.md` |
| `rustyn64-frontend` | egui/wgpu/cpal/winit shell (bin `rustyn64`) | `docs/frontend.md` |
| `rustyn64-test-harness` | golden-log + accuracy + frame comparators | `docs/testing-strategy.md` |
| `rustyn64-netplay` | rollback netplay (frontend-side) | `docs/frontend.md` |
| `rustyn64-cheevos` | RetroAchievements FFI (later, off by default) | — |

## Accuracy

**The first gate now reports a real number.** `basic.z64` from Dillon's n64-tests
runs end to end and passes all five of its hardware-verified cases — delay-slot
semantics, `J` in a delay slot, `BEQL` nullification, `BNEL` nullification, and
`LWU`+`DADDI`. 59 instructions retired, 129 master ticks (T-11-006).

That is a small number of tests, but it is the first time this emulator has
executed a ROM at all, and it independently validates the delay-slot and
branch-likely work against something other than our own expectations.

**The ADR 0004 determinism contract is now exercised** (T-11-007) rather than
merely written down: same seed + ROM produces a bit-identical machine across
repeated runs, different seeds produce different machines so the check is not
vacuous, reset is reproducible, and a source-level guard rejects wall-clock, OS
entropy, threads and unordered collections anywhere in the core.

| Gate | Oracle available? | Status |
| --- | --- | --- |
| **Dillon `basic.z64` (control flow)** | **yes** — external tier | **PASSING** — 5/5 |
| **Determinism (ADR 0004)** | n/a — self-checking | **PASSING** — exercised, not just specified |
| CPU/RSP golden-log (reference trace) | no — needs a cen64/ares capture | not started (golden source returns empty) |
| n64-systemtest `Failed: 0` (CPU/COP0/TLB/RSP) | **yes** — ROM committed | **runs; 2,897 failing** — blocked on COP1 rounding (ledger C-10) |
| ParaLLEl-RDP fuzz suite (RDP bit-exactness) | source cloned, suite not set up | not started |
| Accuracy battery (first-party probe set) | probes not authored | 0% (battery stubbed) |
| Visual golden / screenshots | **yes** — krom + 240p + commercial staged | not started |

The distinction matters: "oracle available" means the ROM is on disk; it says
nothing about whether the emulator can execute it. Both must be true before a
gate reports a real number, and today the second never is.

See `docs/testing-strategy.md` for the oracle and the five test layers.

## Cart model matrix

**Tiered (Core / Curated / BestEffort) under an honesty gate: NO.** The N64 has
one cart model parameterized by save type + CIC + region, not hundreds of
mappers — so there is no board tiering and no honesty-gate test (ADR 0003). The
accuracy oracle is n64-systemtest pass/fail + the RDP fuzz suite, not a tier
matrix.

Save-type coverage target (per-game DB resolved): EEPROM 4k/16k, SRAM, FlashRAM,
Controller Pak (`docs/cart.md`). All five backends have regression ROMs staged
under `tests/roms/external/commercial/`, one folder per backend, with save types
resolved by MD5 against the mupen64plus catalogue rather than guessed. None are
implemented yet (Phase 5).

## Version policy

- Start at **v0.1.0**; this is a clean N64 project. Additive features land behind
  default-off flags so the shipped / native / `no_std` / wasm builds stay
  byte-identical with the flags off.
- **Do NOT import RustyNES engine-lineage "v2.0" anchors as RustyN64 releases.**
- **Three different things get called "finer timing"** and must never be conflated
  in release notes or docs, coarse to fine:
  1. The **canonical 187.5 MHz master clock** (ADR 0006) — CPU every 2 ticks, RCP
     every 3. **Implemented** (T-11-001). ADR 0001's 93.75 MHz tick with its 3:2
     fractional accumulator is gone from the tree.
  2. The **SysAD command/data split** at SClock, 62.5 MHz (ADR 0007). Note this is
     *coarser* than one PClock, so it is not sub-cycle resolution. **Modelled**
     (T-11-008) — the transaction exists and cannot complete in its address
     phase, but the scheduler does not yet step the RCP between phases (Sprint 2).
  3. **Resolution finer than one PClock** — the deferred ADR 0005 refactor. Does
     not exist, is not scheduled, and is the one expected to break byte-identity
     and save-state compatibility.
- v1.0.0 is the production cut (Phases 1–8 complete; README/CHANGELOG/docs/STATUS
  in sync; release matrix + Pages green). See `to-dos/ROADMAP.md`. Of those
  release-readiness items, **Pages is already green** and the release matrix is
  written but untested — no tag has been cut, so `release.yml` has never run.
