# Performance — RustyN64

**References:** `ref-docs/research-report.md` §3, §4, §challenge 7,
§Architecture options B; `docs/scheduler.md`; `docs/rsp.md`; `docs/rdp.md`.

## The bottleneck (where the time goes)

> **Partly superseded by measurement — see *Measured (2026-07-30)* below.** The
> prediction in this section was that the RSP + CPU dominate. Measured on a real title
> in the render phase, the **CPU is 31.6% and the RSP only 9.0%**, while **Bus + VI is
> 29.0%** — the VI scan-out filters were not on the predicted list at all. A prediction
> left unmarked reads as a finding, so it is marked.

Per `ref-docs/research-report.md` §4 and §challenge 7, once the RDP is on the GPU
(ParaLLEl-RDP: ~0.2 ms/frame, 2000–5000 VI/s on mid-range GPUs), the RDP is **not**
the bottleneck — the single-threaded **RSP + CPU** are. RustyN64 inherits this
shape: the LLE RSP interpreter (scalar + vector, one issue per cycle) and the
VR4300 interpreter dominate the frame budget. This drives the whole performance
plan.

## Strategy: correctness first, then accelerate as validated layers

1. **Interpreter-only, everywhere, first.** The VR4300 and RSP ship as
   interpreters — they are the **determinism oracle** (ADR 0004) and the thing the
   accuracy suites pin. Do not optimize before the suites pass.
2. **Software reference RDP first.** A pure-Rust software LLE RDP (the angrylion
   analog) is the always-correct renderer and the RDP fuzz-suite gate
   (`docs/rdp.md`, `ref-docs/research-report.md` §4, §Architecture options B).
3. **Then accelerate, each validated against the interpreter/reference:**
   - a **wgpu-compute RDP** backend (the ParaLLEl-RDP analog) — validated against
     the software RDP, never replacing it as the oracle;
   - an **RSP dynarec** (the ParaLLEl-RSP / dgb-n64 approach) — the interpreter
     stays the deterministic fallback (`ref-docs/research-report.md` §3,
     §challenge 7);
   - a **CPU dynarec** if the interpreter can't hit frame rate.

Acceleration is additive and off-by-default-equivalent: the interpreter path
remains byte-identical and is what the determinism contract is defined against.

## Hot-path discipline

- **No allocations** in `Cpu::tick`, `Rsp::tick`, `Rdp::tick`, `Audio::tick`, or
  the per-pixel RDP inner loop. Prefer fixed arrays; the Bus uses
  `core::mem::take` split-borrow (no heap) to step owned chips
  (`docs/architecture.md` fact 2).
- The chip stack is `#![no_std]` + `alloc`; only the frontend carries `std`.
- **Profile before abstracting.** `cargo bench` (criterion) per chip crate +
  `perf record` on a headless run. Each chip crate has a `benches/` harness
  (`crates/rustyn64-*/benches/`).

## The goal, stated plainly

**Sustained, fully cycle-accurate N64 emulation at full speed — the core and every
other component.** Not cycle-accurate *or* fast; both.

This is an unproven target. No public N64 core sustains full speed while fully
cycle-accurate. What is known:

- CEN64's pipeline is cycle-accurate, but its **bus is not** — memory accesses
  complete in zero emulated time against flat constants — and it is nonetheless
  widely regarded as too slow, from a stalled project benchmarked on 2013-era
  hardware. It never bought full accuracy for the price it paid.
- ares is fast because it is cycle-*approximate*: no pipeline, no interlocks,
  instruction-granular latencies from a table, deferred synchronization.
- A sibling project's canonical-clock rewrite cost **6–8%** in end-to-end frame
  time while its isolated CPU loop got ~35% faster — so the timebase model itself
  is a single-digit-percent tax, and the cost lands bus-side.

None of that establishes the goal is reachable; none of it establishes it is not.
It is an **open engineering risk with a measurement gate**, and the consequence is
that performance is a design input from the first line of Phase 1 rather than a
later optimization pass.

### The budget

93.75M CPU + 62.5M RSP = **156.25M component-steps/s**, before the RDP. On a
~5 GHz core that is roughly **32 host cycles per emulated component step**. This
figure is an *estimate*, not a measurement — it needs a real Sprint 1 benchmark
before it is worth defending. What it implies is already actionable: pipeline
latches cache-resident, no allocation in `tick`, no per-cycle branching on cold
conditions, and the reverse-order stage cascade written so the common case is
straight-line.

## Targets (provisional — refine after the interpreter lands)

| Metric | Target |
| --- | --- |
| Headless emulated frame | ≤ 16.67 ms (60 fps NTSC) on a modern desktop core |
| Host cycles per emulated component step | ≤ ~32 (estimate; measure in Sprint 1) |
| RSP interpreter | the watch item — measure first, dynarec if it misses |
| Software RDP at native res | interactive; quantify vs the compute backend need |
| wgpu-compute RDP | sub-ms/frame (the ParaLLEl-RDP reference point) |

These are interpreter-era goals; the dynarec/compute backends exist to close any
gap that remains.

## Measurement plan

- Per-chip criterion benches (`cargo bench -p rustyn64-cpu` / `-rsp` / `-rdp` /
  `-audio`).
- A headless "run N frames of ROM X" harness for end-to-end ms/frame.
- `perf record` / flamegraph on the headless run to find the top hot functions
  before any optimization (the RustyNES measure-first discipline).
- A perf-capture regression gate (≥X% Criterion regression fails CI) once the
  interpreter is stable.

## Open questions

- **RDP backend ordering** — can the software RDP hit interactive native-res
  speed, or must the wgpu-compute backend come sooner
  (`ref-docs/research-report.md` §Open questions 3; `docs/rdp.md`)?
- **Bus-arbitration cost** — how much CPU/RSP/RDP/DMA contention modeling is
  needed before it becomes a measurable cost (`docs/scheduler.md` open question).
- **Dynarec backend** — `cranelift` vs hand-rolled x86_64/aarch64 for the RSP/CPU
  recompilers (`ref-docs/research-report.md` §External dependencies).

## Measured (2026-07-30)

Provenance for every performance figure quoted elsewhere in this repo, including in
**ADR 0011**. Recorded because the project's rule is that a number is *measured, never
tuned*, and a measured number without its method is not falsifiable.

**These are host-performance figures, not emulation-accuracy constants.** They live here
rather than in `docs/accuracy-ledger.md` deliberately: that file's stated scope is
measured hardware constants, open residuals, and ruled-out approaches. Host FPS is none
of those, and mixing it in would blur what the ledger is for.

### Method

| | |
| --- | --- |
| Machine | development workstation, CachyOS, Linux 7.1.5 (single machine — none of this is cross-platform) |
| Build | `--release` (`opt-level = 3`, `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`) |
| ROM | Super Mario 64 (local dump; commercial ROMs are never committed, ADR 0008) |
| Harness | `crates/rustyn64-frontend/tests/frame_cost_probe.rs`, `#[ignore]`d |
| Window | 30 frames, taken **after the VI is live** (frame 36 for SM64) |
| Profiler | `perf record -F 999`, `CARGO_PROFILE_RELEASE_DEBUG=1`, attributed by `perf report -s srcline` |
| Noise | +-0.5% run to run; every figure below is the mean of 30 frames, and each measurement was run twice |

### Frame cost

| quantity | value | kind |
| --- | --- | --- |
| frame cost, before the VI tap fix | 150.5-151.7 ms (6.59-6.64 FPS) | measured |
| frame cost, after | **138.7 ms (7.21 FPS)** | measured |
| `Bus::scanout_scaled`, before | 35.5-35.7 ms (23.6% of a frame) | measured |
| `Bus::scanout_scaled`, after | **21.3 ms (15.4%)** | measured |
| debug-build frame cost | 784.7 ms (1.27 FPS) | measured |
| **debug vs release ratio** | **7.7x** | derived from the two above |
| 60 FPS budget | 16.7 ms | arithmetic (`1/60`) |
| gap to 60 FPS | ~8.3x | derived |

### Render-phase attribution

`perf`, 9,431 samples, **render phase only** (`perf record -D 70000` skips the boot
samples; SM64 begins rendering ~frame 400, established by
`crates/rustyn64-frontend/tests/gameplay_phase_probe.rs`).

| subsystem | boot phase | render phase | kind |
| --- | --- | --- | --- |
| Bus + VI | 18.6% | **29.0%** | measured |
| CPU core | 38.9% | 31.6% | measured |
| RSP | 11.2% | 9.0% | measured |
| RDP | 0.00% | 7.5% | measured |
| scheduler | 0.00% | 6.4% | measured |

**The boot-phase column is an artifact and is shown only as a warning.** `RDP 0.00%`
and `scheduler.rs 0.00%` cannot be true of a running system; they are what a
pre-render window measures. Profile the render phase.

Symbol-level attribution is useless here — `lto = "fat"` inlines the whole CPU
pipeline and the RSP/RDP steps into `System::step_due_here` (13,317 annotated
lines), which is why it appeared to be 63.9% of runtime on its own.

### Derived, not measured

Marked separately because they are arithmetic over the above, and quoting them as
measurements would overstate them:

- **1.56 M CPU and 1.04 M RCP steps per emulated frame** — from `MASTER_HZ / 60`
  divided by the ADR 0006 divisors (2 and 3), not counted at runtime.
- **~1.66x in-model ceiling** — Amdahl over the largest identified in-model targets
  (latch copy ~16%, VI scan-out 23.6%), i.e. `1 / (1 - 0.40)`. It assumes both are
  eliminated *entirely*, so it is an upper bound and not a forecast.

### SM64's VI configuration (measured, not assumed)

`VI_CTRL = 0x00013016`: bpp 2 (RGBA5551), `aa_mode` 0 (coverage path active),
`divot` 1, `dither_filter` 1, gamma 0. `VI_X_SCALE` gives `x_add = 512` (a 2x
horizontal upscale, so `xfrac` alternates zero / non-zero); `VI_Y_SCALE` gives
`y_add = 1024` (so `yfrac` is constant). This is what made the zero-weight bilinear
tap skip worth 40% of the scan-out.

### Ruled out by measurement — do not retry

1. **Per-tick `u64` modulo in the scheduler** — divides are **< 2%**.
2. **Hoisting the duplicate `next_edge()` in `run_until`** — **neutral**; LLVM already
   common-subexpression-eliminates it.
3. **Removing the double latch copy** — unsafe (`dc_stage`'s error branch re-reads
   `self.ex_dc` *after* `abort_with` stamps it), and `Latch` is already
   zero-padding-optimal at 120 bytes. Upper bound 1.19x.
4. **Inlining the VI leaf readers** — `#[inline]` declined by LLVM;
   `#[inline(always)]` made the scan-out **36% worse**.
5. **Reordering `vi_divot`'s early-out to test coverage first** — **10% worse**, which
   is itself a finding: it only regresses if `cvg == 7` is *rare*, so
   `vi_video_filter` (AA edge) dominates rather than the 8-tap de-dither.
