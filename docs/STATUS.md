# RustyN64 — STATUS (single source of truth)

This file is authoritative for per-suite pass counts, the board matrix, the
chip→crate map, and version policy. Everything else defers to it.

**Current release:** **v0.4.0 "Rasteriser"** — this commit is the v0.4.0 release; the
`v0.4.0` tag is cut from it on merge to `main`.

**Phases 1, 2, and 3 are complete.** All six exit criteria (two per phase) are met, and each is an
oracle result with a committed runner rather than a self-assessment:

| Criterion | Result | Reproduce |
| --- | --- | --- |
| Conformance suite bit-matches Angrylion (**Phase 3** criterion 1) | **met** — 164 committed `.rvec` vectors (FILL / scissor / shaded / textured triangles, combiner, blender, dither, alpha-compare, coverage, copy texrects) replay byte-exact vs the Angrylion oracle; the seeded fuzzer found and fixed R-3 and R-15, and R-13 (textured triangles) is resolved | `cargo test -p rustyn64-test-harness --test rdp_conformance` |
| A real ROM renders a stable golden frame (**Phase 3** criterion 2, T-33-006) | **met** — a committed license-clean homebrew ROM boots on the VR4300, CPU-fills a framebuffer, and the VI scans it out to a verified 32×24 golden frame, bit-identical across two boots | `cargo test -p rustyn64-test-harness --test real_rom_frame` |
| n64-systemtest `Failed: 0` (CPU/COP0/TLB/COP1) | **met** — 0 of 917 tests fail in those categories; 93 fail suite-wide, all cart/PIF/MI/RDP (Phase 3+) | `cargo test -p rustyn64-test-harness --release --test systemtest -- --ignored` |
| n64-systemtest `Failed: 0` (**RSP** category, Phase 2) | **met** — across 917 tests started, 0 RSP-prefixed failures (the suite-wide total, of which the RSP category was the bulk, fell from 413 to 93); the full VU ISA, load/store, reserved opcodes, `BREAK` semantics, and the DPC registers landed in #41–#44 | same runner; dump per-test to confirm none are `RSP`-prefixed |
| Real graphics microcode emits an RDP command list (**Phase 2** criterion 2) | **met** — libdragon's combined RSPQ+`rdpq` blob boots, dispatches an `rdpq` overlay command to its resident handler, and emits an RDP command (bytes DMA'd to RDRAM + `DP_END` advanced through the DPC seam) | `cargo test -p rustyn64-test-harness --test microcode` |
| CPU golden-log 0-diff | **met** — retired-instruction stream identical to ares from the ELF entry | `cargo test -p rustyn64-test-harness --release --test golden_log -- --ignored` |

The VR4300 is complete: the canonical 187.5 MHz clock (ADR 0006), the five-stage pipeline (ADR
0007), MIPS III including the 64-bit forms, COP0, the TLB and micro-ITLB, the exception model,
interrupts, the primary I- and D-caches, the privilege-aware segment map, `Status.RE`, and COP1 on a
soft-float core.

**Phase 2 (v0.3.0) — both exit criteria met; released as v0.3.0.** The RSP-category
`Failed: 0` criterion is **met** (above). The second — *a real graphics
microcode boots and emits a plausible RDP command list* — is **met** too:
libdragon's real combined RSPQ+`rdpq` microcode (vendored, `third_party/
libdragon-rsp/`) boots to its idle break (T-24-002), processes a DMA'd command
queue (T-24-003 foundation), and an `rdpq` overlay command
(`RDPQCmd_Passthrough8`) is dispatched to its resident handler and **emits an
RDP command** — the 8 command bytes are DMA'd to an RDRAM output buffer and
`DP_END` is advanced through the DPC seam (T-24-003). Witnessed non-vacuously by
`tests/microcode.rs::the_microcode_emits_an_rdp_command_through_the_dpc_seam`.
The next accuracy phases are the LLE RDP rasterizer (Phase 3) and cart/PIF
(Phase 5). See `to-dos/ROADMAP.md`.

**Read this before trusting any green checkmark:** CI passing means the
workspace compiles and its **386** tests pass. The CPU genuinely executes
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
- `rustyn64-rsp` and `rustyn64-rdp` **execute**: the RSP runs real microcode
  (scalar + full vector unit) and the RDP rasterises the command list through the
  texture / combiner / blender / coverage pipeline with VI scan-out. `rustyn64-audio`
  (AI) **implements the interface** (Phase 4, Sprint 1) — the register block at
  `0x0450_0000`, the two-deep DMA FIFO, the derived DAC rate, the interrupt-on-start,
  and the delayed-carry bug. The **real libdragon mixer microcode** (`rsp_mixer.S`)
  now runs on the LLE RSP and produces a verified mixed PCM buffer (Phase 4, Sprint 2),
  and a real bare-metal ROM plays PCM through the AI end to end — what remains for a
  playable game is the frontend audio drain + resampler (Sprint 3).
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
| Release | `release.yml` builds all three targets, packages archives with licences, generates `SHA256SUMS`, and publishes on a `v*` tag. Guarded so the tag must match the workspace version. Exercised for real: `v0.1.0`–`v0.4.0` are all tagged and released. |
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

**Three gates execute real results today.** `basic.z64` from the
`dillon-n64-tests` corpus runs end to end, judged by its completion protocol
(T-11-006). The **n64-systemtest** ROM runs under the committed `--test
systemtest` runner and reports a real count (Phase 1 categories `Failed: 0`; RSP
category `Failed: 0`; 93 suite-wide). The **golden-log** gate (`--test
golden_log`) replays 50,027 retired records at 0 diff against ares. A fourth,
the **synthetic visual golden** (`--test golden_frame`, T-31-005), executes the
FILL → VI scan-out path against a committed frame hash. The rest of the corpus
(the real-ROM krom/240p visual goldens, the accuracy battery, commercial ROMs)
is staged only — an oracle on disk that no gate executes yet.

## What is stubbed (the roadmap)

| Subsystem | State | Phase |
| --- | --- | --- |
| VR4300 integer core, pipeline, delay slots, errata, SysAD | **done** (Sprint 1) | Phase 1 |
| VR4300 COP0, TLB, exception model | **done** (Sprint 2) | Phase 1 |
| VR4300 COP1 (FPU) | **partial** — see below | Phase 1 (Sprint 3) |
| CPU golden-log 0-diff | **done** (T-HARNESS-01) — `tests/golden/n64-systemtest.log`, captured from ares at the ELF entry; gate is `--test golden_log` | Phase 1 |
| VR4300 I/D caches | **done** (T-11-003) — tags, data, all `CACHE` ops; DMA coherency outstanding | Phase 1 |
| RSP scalar unit + SP interface | **implemented** (T-21-002/004/005) — the SU executes, `BREAK` halts (incl. in a taken branch's delay slot), DMA and the register file work. Spec `docs/rsp.md`; regressions in `su::tests` and n64-systemtest `RSP BREAK`/`SP …` | Phase 2 |
| RSP vector unit (COP2, accumulator, `VRCP`/`VRSQ`) | **implemented** — the full VU: multiplies, accumulating forms, add/sub/carry, compares, the clip compares (`VCL`/`VCH`/`VCR`), `VMRG`/`VRND`/`VMULQ`/`VMACQ`, the reciprocals, the whole vector load/store family, and the reserved "VZERO" opcodes. Spec `docs/rsp.md`; regressions in `vu`'s `compare_tests`/`clip_tests`/`vzero_tests`/… and the n64-systemtest RSP category | Phase 2 |
| RDP DPC command registers | **implemented** — `DPC_START`/`END`/`CURRENT`/`STATUS` at `0x0410_0000`, the `START_VALID` double-latch + `FREEZE`; driven both by the CPU **and** by the RSP microcode's COP0 `c8`–`c15` (routed via `StepResult::dp_write` → `Bus::rsp_tick` → `Rdp::dpc_write`, the RSP not being allowed to name `Rdp`). The rasterizer behind them is **implemented** (Phase 3). Provenance N64brew *Reality Display Processor/Interface*; spec `docs/rdp.md`; regressions in `rustyn64-rdp` tests + n64-systemtest `RSP STATUS: start-valid` + `microcode::…emits_an_rdp_command…` | Phase 2 / Phase 3 |

**What "partial" means for COP1.** The register file (`FR` views), the control
registers, the data moves, S/D `ADD`/`SUB`/`MUL`/`DIV`, `ABS`/`MOV`/`NEG`, the
compares and the conversions decode and execute. Two things do **not** work,
and neither is visible from a green `cargo test`:

- **`BC1F`/`BC1T` are decoded but not executed.** They reach
  `Op::Cop1Unimplemented`, which retires as a **silent no-op** rather than
  raising — so an FP branch never redirects. The compare tests read `FCSR.C`
  through `CFC1` and pass without them, but real code branches on it. This is
  the decoded-but-no-op shape that has already cost two investigations.
- **37 COP1 assertions remain**, in the `CVT.S`/`CVT.L` families — narrower
  edge cases rather than a missing operation.

What **is** done: the unmaskable unimplemented-operation cause (bit 17) is
produced for subnormal operands and results, for `FS = 1` with underflow or
inexact enabled, for MSB-clear NaN operands, and for out-of-range integer
conversions (ledger C-13); the arithmetic runs on a soft-float core
(`crates/rustyn64-cpu/src/softfloat.rs`) that produces exact IEEE flags and
honours all four `FCSR.RM` modes, verified bit-for-bit against Rust's native
operators over 100,000 cases; enabled FP traps raise `Exception::FloatingPoint`,
leave `fd` unwritten, do not accumulate the sticky `Flags`, and do not retire;
and the compares and conversions decode and execute — **all sixteen
`C.cond.fmt` tests pass outright**. NaN classification follows the VR4300's
inverted convention (ledger C-12), not IEEE-754:2008.

**`SQRT` (funct 4) is the only COP1 operation that is neither decoded nor
implemented**, so it is not an instance of the pattern below. The conversions
and the `C.cond.fmt` compares *were* — implemented in `fpu.rs` and unreachable —
until this sprint, and `ABS`, `MOV` and `NEG` before them; `MOV` alone cost
~100 failures, because a
*decoded-but-no-op* instruction is invisible to `cargo test` and the compiler
emits one at every FP call boundary. That pattern has now cost two separate
investigations; when adding a decode arm, enumerate the neighbouring funct
space rather than only the encoding that prompted the change.
| RDP LLE (software reference rasterizer) + VI scan-out | **done** — texture / combiner / blender / coverage pipeline; 164 conformance vectors bit-match Angrylion; a real ROM renders a golden frame (T-33-006) | Phase 3 |
| AI audio DMA double-buffer | **interface done** (Sprint 1) — registers, FIFO, derived DAC rate, IRQ-on-start, delayed-carry bug. **Real mixer microcode produces PCM on the RSP** (Sprint 2); awaits the frontend drain/resampler (Sprint 3) | Phase 4 |
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
| CPU/RSP golden-log (reference trace) | **yes** — `tests/golden/n64-systemtest.log`, captured from a patched ares | **MET: 0 diff** over 50,027 retired records |
| n64-systemtest, **CPU/COP0/TLB/COP1** categories (Phase 1's criterion) | **yes** — ROM committed, and the runner with it | **MET: `Failed: 0`**, across 917 tests started. Reproduce with `cargo test -p rustyn64-test-harness --release --test systemtest -- --ignored`. 93 assertions still fail suite-wide, down from 413; **none are RSP-prefixed** (the RSP category is Phase 2's criterion and is now 0), leaving the cart/PIF (Phase 5), the RDP rasterizer (Phase 3) and the MI's RDRAM repeat mode |
| n64-systemtest, **RSP** category (Phase 2's criterion) | **yes** — same runner | **MET: `Failed: 0`** across 917 tests started — every RSP-prefixed test passes (verified by dumping per-test failures; 0 begin with `RSP`). The full VU ISA, vector load/store, reserved opcodes, `BREAK`-in-delay-slot, and the DPC registers landed in #41–#44 |
| ParaLLEl-RDP fuzz suite (RDP bit-exactness) | source cloned, suite not set up | not started |
| Accuracy battery (first-party probe set) | probes not authored | 0% (battery stubbed) |
| Visual golden / screenshots | **yes** — krom + 240p + commercial staged | **first frame MET** (T-31-005) — a synthetic RDP FILL list rendered through the full command-decode → FILL → VI scan-out path is pinned byte-exact against a committed golden hash (`--test golden_frame`). Real-ROM krom/240p goldens await cartridge boot (Phase 5) |

The distinction matters: "oracle available" means the ROM is on disk; it says
nothing about whether the emulator can execute it. Both must be true before a
gate reports a real number — true today for `basic.z64`, n64-systemtest, the
golden log, the synthetic `golden_frame`, and the **first real-ROM visual golden**
(`real_rom_frame.rs`, a homebrew ROM that CPU-renders a frame through the VI); not
yet for an RDP-driven real-ROM frame or the accuracy battery.

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
  release-readiness items, **Pages is already green** and the release matrix has
  now run for real: `v0.1.0`–`v0.4.0` are all tagged, and `release.yml` has
  published checksummed binaries across the three-target matrix.
