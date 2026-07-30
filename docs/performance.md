# Performance — RustyN64

**References:** `ref-docs/research-report.md` §3, §4, §challenge 7,
§Architecture options B; `docs/scheduler.md`; `docs/rsp.md`; `docs/rdp.md`.

## The bottleneck (where the time goes)

> **Partly superseded by measurement — see *Measured (2026-07-30)* below.** The
> prediction in this section was that the **RSP + CPU** dominate together. Measured on
> a real title in the render phase they do still lead, at 32.0% + 9.2% = 41.2% against
> **Bus + VI's 29.0%** — so the shape is right and the weights are not. The RSP was
> predicted co-dominant and is **9.2%**, less than a third of the pair. And the VI
> scan-out filters, the single largest source file in the profile, were not on the
> list at all. A prediction left unmarked reads as a finding, so it is marked.

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

Two **different experiments** produce the numbers below, and they must not be read as
one. Experiment **A** times frames just after the VI comes up; experiment **B** profiles
a window several hundred frames later, once the title is actually drawing. Mixing them
is how the first draft of this section came to attribute a boot-phase sample count to a
render-phase table.

| | A — frame timing | B — render-phase profile |
| --- | --- | --- |
| Harness | `crates/rustyn64-frontend/tests/frame_cost_probe.rs` | `crates/rustyn64-frontend/tests/gameplay_phase_probe.rs` |
| Window | 30 frames from the first frame the VI is live (frame 36 for SM64) | the tail of a 900-frame run (`RUSTYN64_PROBE_SKIP=900`), reached by mashing START/A |
| Samples | 9,431 (`perf record -F 999`) | **58,326** (`cpu/cycles:u`) |
| Duration | one probe run, ~9 s | **58.9 s of samples**, after `-D 70000` discards the first 70 s — the process runs ~130 s in total, so the sampled tail is roughly the last 400 of the 900 frames |
| Answers | ms/frame, and the scan-out's share of it | which subsystem owns the time |

Needs `perf` and `jq` on the host beyond the usual toolchain. The capture lands in
`target/`, which is already ignored, so a profiling run leaves the tree clean.

```bash
# `$ROOT` rather than `$PWD`: these run from anywhere in the workspace.
ROOT=$(git rev-parse --show-toplevel)
# Adjust to wherever the dump lives; check it against the SHA-256 below first.
ROM="$ROOT/tests/roms/external/commercial/eeprom-4k/Super Mario 64.z64"

# A — frame timing (the FPS numbers). Run twice; take the mean line.
RUSTYN64_PROBE_ROM="$ROM" \
  cargo test -p rustyn64-frontend --release --test frame_cost_probe -- --ignored --nocapture

# B — render-phase profile. `-D 70000` discards the first 70 s, which is boot.
#
# `perf` has to be pointed at the test binary, and cargo names it with a content
# hash under `target/release/deps/`, so the path is asked for rather than guessed.
# The test is named explicitly too: `--ignored` alone would profile every ignored
# test in that binary, and the capture would silently become a profile of something
# else the day a second one is added.
# The filter is load-bearing: the stream also carries the frontend `bin` artifact,
# so taking the last executable points perf at `target/release/rustyn64` instead.
# Matching on the target *name* keeps it a single path even if more test targets are
# built later.
BIN=$(CARGO_PROFILE_RELEASE_DEBUG=1 cargo test -p rustyn64-frontend --release \
        --no-run --test gameplay_phase_probe --message-format=json \
      | jq -r 'select(.reason == "compiler-artifact"
                     and .target.name == "gameplay_phase_probe"
                     and .executable != null) | .executable' | tail -n 1)
test -n "$BIN" || { echo "no test binary — did the build fail?" >&2; exit 1; }
RUSTYN64_PROBE_ROM="$ROM" \
RUSTYN64_PROBE_SKIP=900 perf record -F 999 -D 70000 -o "$ROOT/target/perf_play.data" -- \
  "$BIN" does_a_retail_title_reach_a_rendering_phase --ignored --nocapture
perf report -i "$ROOT/target/perf_play.data" -s srcline --full-source-path \
  --no-children -g none --stdio
```

| | |
| --- | --- |
| Machine | Intel Core i9-10850K (10C/20T, 3.6 GHz base), 62 GiB RAM, RTX 3090; CachyOS, Linux 7.1.5. **One machine — none of this is cross-platform, and the frontend is CPU-bound, so the GPU is listed only for completeness.** |
| Toolchain | `rustc 1.96.0 (ac68faa20 2026-05-25)`, the pinned exact version |
| Build | `--release` (`opt-level = 3`, `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`) |
| ROM | Super Mario 64, 8,388,608 bytes, SHA-256 `17ce077343c6133f8c9f2d6d6d9a4ab62c8cd2aa57c40aea1f490b4c8bb21d91`. The image itself is never committed (ADR 0008), but its **hash is**, so a different revision or a bad dump cannot silently invalidate these figures. Verify with `sha256sum` before quoting any number here. |
| Attribution | `perf report -s srcline` with `CARGO_PROFILE_RELEASE_DEBUG=1`. **Use `--full-source-path`**: bare `srcline` prints basenames, and `lib.rs` then collides across crates — which is exactly how the RDP's share was first overstated by summing it with the audio crate's. |
| Noise | ±0.5% run to run; every figure is the mean of 30 frames, and each measurement was run twice |

### Frame cost

Every row below is a **paired** measurement: the before and after come from two runs
each, back to back on an otherwise idle machine, in the same window and on the same
host. Unpaired quantities that once appeared here — a debug figure from one window
divided by a release figure from another — are marked as superseded rather than deleted,
because the arithmetic that produced them is what makes them wrong.

| quantity | before `646a3e0` | after `646a3e0` | kind |
| --- | --- | --- | --- |
| frame cost | 155.17 / 155.09 ms (6.44 FPS) | **139.34 / 139.28 ms (7.18 FPS)** | measured, ×2 runs |
| `Bus::scanout_scaled` | 35.53 / 35.50 ms (22.9% of a frame) | **21.64 / 21.64 ms (15.5%)** | measured, ×2 runs |
| the VI tap fix's effect | — | **1.114x** on the frame, **-39.1%** on the scan-out | derived from the pair |

Run-to-run spread in `--release` is **0.05%** across these pairs, well inside the ±0.5%
the method claims, so a change under ~1% is still not a result.

The 1.09x quoted in `646a3e0`'s own commit message came from comparing runs taken in
different sessions (151.7 → 138.7). The paired figure above, 1.114x, is the one to cite;
the difference between them is exactly the drift that pairing exists to remove.

| quantity | value | kind |
| --- | --- | --- |
| debug-build frame cost, same tree and window | 1.216 / 1.214 s (0.82 FPS) | measured, ×2 runs |
| **debug vs release** | **8.7x** | paired, same tree |
| 60 FPS budget | 16.7 ms | arithmetic (`1/60`) |
| **gap to 60 FPS from 139.3 ms** | **~8.3x** | derived |

The **7.7x** debug ratio quoted in ADR 0011 is **superseded**. It was self-consistent —
784.7 ms debug against 101.6 ms release — but both came from the pre-`#209` probe, which
timed frames 37-66, i.e. before the VI is programmed. Against the VI-live window the
paired ratio is 8.7x. The actionable rule is unchanged and is the only part worth
remembering: **never quote a debug figure**, it is roughly an order of magnitude out.

### Render-phase attribution

Self time, grouped by the crate each source line belongs to. Both columns are
**re-derived from the retained capture files** rather than copied from a report, and each
column sums to ~100% so that nothing is quietly excluded — an attribution table whose
rows total 84% is not interpretable, because the reader cannot tell whether the remainder
is noise or the actual bottleneck.

| bucket | A window (boot) | B window (render) |
| --- | --- | --- |
| Bus + VI (`bus.rs`, `vi.rs`) | 18.6% | **29.0%** |
| CPU (`rustyn64-cpu/`, of which `pipeline.rs` is 22.6 in B) | 38.9% | 32.0% |
| `core`/`std` inlined into the above (`uint_macros.rs`, `mem::swap`, iterators…) | 20.7% | 10.1% |
| RSP (`su.rs`, `vu.rs`) | 11.3% | 9.2% |
| `scheduler.rs` | 0.0% | 6.4% |
| unresolved, libc, kernel | 6.1% | 5.9% |
| RDP | 1.9% | 4.6% |
| audio | 0.0% | 2.6% |
| other `rustyn64-*` | 2.4% | 0.0% |
| **total** | **99.9%** | **99.8%** |

Two corrections to the first version of this table, both found by re-deriving it:

- **RDP is 4.6% in the render window, not 7.5%,** and **1.9% in boot, not 0.00%.** The
  7.5% was `lib.rs` under basename attribution — the RDP's 4.6% plus the audio crate's
  2.6%, which share a filename. Hence the `--full-source-path` requirement above.
- The `core`/`std` row was missing entirely, and it is not a rounding remainder: at
  **20.7% in boot** it is the second-largest bucket there. Those samples are inlined
  library code (`u32::wrapping_*`, `core::mem::swap` on the pipeline latches, range
  iterators) executing on behalf of the crate above it, so they are real work — just not
  attributable to one subsystem by source path alone.

**The boot column is a trap, not a baseline.** `scheduler.rs` reads 0.0% there and 6.4%
in the render window; the RDP reads 1.9% against 4.6%. A window taken before the title
draws under-reports precisely the subsystems that dominate once it does. Profile the
render phase.

Symbol-level attribution is useless here — `lto = "fat"` inlines the whole CPU
pipeline and the RSP/RDP steps into `System::step_due_here` (13,317 annotated
lines), which is why it appeared to be 63.9% of runtime on its own in the A-window
capture's call-graph report. Source-line attribution is the only usable view.

### Derived, not measured

Marked separately because they are arithmetic over the above, and quoting them as
measurements would overstate them:

- **1.56 M CPU and 1.04 M RCP steps per emulated frame** — from `MASTER_HZ / 60`
  divided by the ADR 0006 divisors (2 and 3), not counted at runtime.
- **~1.64x in-model ceiling** — Amdahl over the largest identified in-model targets as
  they stood *before* the VI tap fix (latch copy ~16%, VI scan-out 22.9%), i.e.
  `1 / (1 - 0.389)` = 1.637. It assumes both are eliminated *entirely*, so it is an
  upper bound and not a forecast; part of it has since been collected (the scan-out is
  now 15.5% of a frame), which is why the measured gain was 1.114x and not more.

  ADR 0011 quotes **1.66x** for this, from `1 / (1 - 0.396)` with the single-run 23.6%
  scan-out share. The paired share is 22.9%, so the ceiling is 1.64. The difference is
  immaterial to 0011's argument — its point is that ~8-9x is unreachable against a
  ceiling under 1.7 — but a derived figure that no longer follows from its inputs is
  the kind of thing that gets re-quoted, so it is corrected here; ADR 0011 is immutable,
  so its copy is marked superseded in ADR 0012, under *"0011's measured table is
  superseded by the paired re-measurement"*, which lands separately.

  Note what the ceiling does *not* say: eliminating the scan-out **completely** — a
  physical impossibility, since something has to produce the pixels — would leave
  117.7 ms, or 8.5 FPS. The 16.7 ms budget is below the cost of the CPU pipeline alone.

### SM64's VI configuration (measured, not assumed)

`VI_CTRL = 0x00013016`: bpp 2 (RGBA5551), `aa_mode` 0 (coverage path active),
`divot` 1, `dither_filter` 1, gamma 0. `VI_X_SCALE` gives `x_add = 512` (a 2x
horizontal upscale, so `xfrac` alternates zero / non-zero); `VI_Y_SCALE` gives
`y_add = 1024` (so `yfrac` is constant). This is what made the zero-weight bilinear
tap skip worth 39.1% of the scan-out, and the arithmetic checks out exactly: `yfrac` is
always 0 and `xfrac` alternates 0 / 16, so the four-tap form averaged
`(1 + 4) / 2 = 2.5` filter chains per output pixel and the skip brings that to
`(1 + 2) / 2 = 1.5`. The predicted 1.67x on the scan-out against a measured 1.64x
(35.53 → 21.64 ms) is as
close as this kind of accounting gets, which is the reason to write it down: a
speed-up that matches a mechanism is a result, and one that does not is a coincidence
waiting to be explained.

### Ruled out by measurement — do not retry

1. **Per-tick `u64` modulo in the scheduler** — divides are **< 2%** of the annotated
   instructions in the A-window capture.
2. **Hoisting the duplicate `next_edge()` in `run_until`** — **no change outside the
   ±0.5% noise floor**, and the raw before/after pair was not retained, so this entry
   rests on the mechanism rather than on a quoted delta: LLVM already
   common-subexpression-eliminates the second call. That is the falsifiable half —
   disassemble `run_until` and count the calls — and it is why re-measuring was judged
   not worth a build. Treat the entry as "no measurable win", not as a number.
3. **Removing the double latch copy** — unsafe: in `crates/rustyn64-cpu/src/pipeline.rs`,
   `dc_stage`'s error branch re-reads `self.ex_dc` *after* `abort_with` has stamped it,
   and `Latch` is already zero-padding-optimal at 120 bytes. Upper bound 1.19x.
4. **Inlining the VI leaf readers** — `#[inline]` declined by LLVM;
   `#[inline(always)]` made the scan-out **36% worse** (35.5 → 48.4 ms).
5. **Reordering `vi_divot`'s early-out to test coverage first** — **10% worse**, measured.

   The *explanation* offered for that regression — that `cvg == 7` is rare, so the
   AA-edge filter rather than the 8-tap de-dither is the hot path — is a **hypothesis,
   not a measurement**. A single aggregate regression cannot separate "the reordered test
   is rarely satisfied" from "the reordered test is itself more expensive". Settling it
   needs a coverage histogram over a real frame, which belongs with the scan-out
   memoization work rather than here. Until then, do not aim an optimization using it.
