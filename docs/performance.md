# Performance — RustyN64

**References:** `ref-docs/research-report.md` §3, §4, §challenge 7,
§Architecture options B; `docs/scheduler.md`; `docs/rsp.md`; `docs/rdp.md`.

## The bottleneck (where the time goes)

> **Partly superseded by measurement — see *Measured (2026-07-30)* below.** The
> prediction in this section was that the **RSP + CPU** dominate together. Measured on
> a real title in the render phase they do still lead, at 32.0% + 9.2% = 41.2% against
> **Bus + VI's 29.0%** — more still if the 10.1% of inlined `core`/`std` is attributed
> to whichever crate it runs on behalf of, which is mostly these two. So the shape is
> right and the weights are not. The RSP was
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

# A — frame timing (the FPS numbers). Run twice; take the mean line. No
# CARGO_PROFILE_RELEASE_DEBUG here: A produces no profile to attribute, and while
# `debug = 1` adds only line tables and does not change codegen, leaving it off
# keeps A's binary the one users actually build.
RUSTYN64_PROBE_ROM="$ROM" \
  cargo test -p rustyn64-frontend --release --test frame_cost_probe -- --ignored --nocapture

# B — render-phase profile. `-D 70000` discards the first 70 s, which is boot.
#
# That 70 s is calibrated to THIS host: it is the time the probe takes to reach the
# rendering phase here, not a property of the ROM. On a faster or slower machine it
# has to move. Check the capture rather than trusting it — if `perf report` shows
# `rustyn64-rdp` at ~0%, the window is still in boot and `-D` is too small.
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
test -x "$BIN" || { echo "no test binary — did the build fail?" >&2; exit 1; }
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

| quantity | before `646a3e0` | after `646a3e0` | after `6a6adfa` | kind |
| --- | --- | --- | --- | --- |
| frame cost | 155.17 / 155.09 ms (6.45 FPS) | 139.34 / 139.28 ms (7.18 FPS) | **125.32 / 125.16 ms (7.98 FPS)** | measured, ×2 runs |
| `Bus::scanout_scaled` | 35.53 / 35.50 ms (22.9% of a frame) | 21.64 / 21.64 ms (15.5%) | **7.88 / 7.88 ms (6.3%)** | measured, ×2 runs |
| effect, **step over the previous column** (each ratio against *that* column, not the baseline) | — | **1.114x** frame, **-39.1%** scan-out | **1.112x** frame, **-63.6%** scan-out | derived from each pair |
| effect, **cumulative from the baseline** | — | 1.114x frame, -39.1% scan-out | **1.24x** frame, **-77.8%** scan-out | derived |

`646a3e0` skips the zero-weight bilinear taps (PR #211); `6a6adfa` memoizes the filtered
source pixel across output pixels (PR #216). Cumulatively **155.13 → 125.24 ms** (each the mean of its paired runs above), a
**1.24x** speed-up, with the scan-out down from **35.52 ms to 7.88 ms** (paired means, as everywhere else here). (The percentage
columns above have different denominators — each is a share of *its own* frame — so the
durations are the comparable figures.)

**Noise, measured rather than assumed.** Back-to-back runs of one binary agree to
**0.05-0.13%**. The *same tree* measured about thirty minutes apart differed by **~1%**
(123.5 against 125.2 ms) — machine state drifts across a session. So the ±0.5% in the
method table is a **within-session** figure: pair a before and after in one sitting, and
treat any cross-session comparison as ±1%.

The 1.09x quoted in `646a3e0`'s own commit message came from comparing runs taken in
different sessions (151.7 → 138.7). The paired figure above, 1.114x, is the one to cite;
the difference between them is exactly the drift that pairing exists to remove.

| quantity | value | kind |
| --- | --- | --- |
| debug-build frame cost, same tree and window | 1.216 / 1.214 s (0.82 FPS) | measured, ×2 runs |
| **debug vs release** | **8.7x** | paired, same tree |
| 60 FPS budget | 16.7 ms | arithmetic (`1/60`) |
| **gap to 60 FPS from 125.24 ms** | **~7.5x** | derived |

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

### Where the time goes now (after `6a6adfa`)

The table above is the shape *before* the scan-out memo. Re-profiled on `main` at
`2abc817`, same method, the render window has changed shape rather than merely shrunk:

| bucket | before `6a6adfa` | **after** | of a 125.24 ms frame |
| --- | --- | --- | --- |
| CPU (`rustyn64-cpu/`) | 32.0% | **41.0%** | ~51.3 ms |
| — of which `pipeline.rs` | 22.6% | **28.9%** | **~36.2 ms** |
| Bus + VI | 29.0% | **12.2%** | ~15.3 ms |
| `core`/`std` inlined | 10.1% | 11.6% | ~14.5 ms |
| RSP | 9.2% | 11.4% | ~14.3 ms |
| `scheduler.rs` | 6.4% | 8.1% | ~10.1 ms |
| RDP | 4.6% | 6.2% | ~7.8 ms |
| audio | 2.6% | 3.0% | ~3.8 ms |
| unresolved, libc, kernel | 5.9% | 6.4% | ~8.0 ms |
| **total** | 99.8% | **99.9%** | (rows are rounded; `pipeline.rs` is a sub-row of CPU and not counted again) |

**This is the measurement that settles what the remaining work has to be.** The VI is no
longer the second-largest bucket — it has gone from 29.0% to 12.2% — and the CPU is now
**41.0%**, with the five-stage pipeline alone at **28.9%**, about **36 ms of a 125 ms
frame**. The 60 FPS budget is 16.7 ms, so `pipeline.rs` on its own is more than twice it,
and the whole CPU crate is **3.1x** it. Deleting every other bucket entirely — VI, RSP,
RDP, audio, scheduler, libc — would still leave ~51 ms, or 19.5 FPS.

No change that keeps per-cycle dispatch reaches 16.7 ms from there, which is ADR 0011's
argument stated in measured milliseconds rather than in prospect.

**Every bucket above 3% has now been looked at per line, and the two that hold a
concentrated target are written up below.** The RSP's 11.4% is *not* one of them: its
hottest attributable line is 0.65% and its largest entry is 1.17% of inlined code with no
line at all, so it is thinly spread instruction execution with no structural target —
which is why ADR 0011 scopes the fast path to the VR4300 first and leaves the RSP for
later. The same holds for the rest of the CPU: the 41.0% is the **whole** `crates/rustyn64-cpu/`
share, latch copying included, so setting that 14.66% aside leaves ~26% of genuine
instruction execution with no concentrated site either.

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
  upper bound and not a forecast. **Most of the scan-out term has since been eliminated**
  — **35.52 ms down to 7.88 ms**, quoted as durations because the two percentages have
  different denominators (22.9% of the old frame, 6.3% of the new one) — and the
  cumulative gain is
  1.24x against that 1.64x bound, so what remains inside the model is the latch copying
  and little else.

  Note what the ceiling does *not* say: eliminating the scan-out **completely** — a
  physical impossibility, since something has to produce the pixels — would leave
  117.4 ms, or 8.5 FPS. The 16.7 ms budget is below the cost of the CPU pipeline alone,
  which is the whole argument for ADR 0011: no dispatch-preserving change reaches it.

  ADR 0011 quotes **1.66x** for this, from `1 / (1 - 0.396)` with the single-run 23.6%
  scan-out share. The paired share is 22.9%, so the ceiling is 1.64. The difference is
  immaterial to 0011's argument — its point is that ~8-9x is unreachable against a
  ceiling under 1.7 — but a derived figure that no longer follows from its inputs is
  the kind of thing that gets re-quoted, so it is corrected here; ADR 0011 is immutable,
  so its copy is marked superseded in ADR 0012, under *"0011's measured table is
  superseded by the paired re-measurement"*, which lands separately.

### SM64's VI configuration (measured, not assumed)

`VI_CTRL = 0x00013016`: bpp 2 (RGBA5551), `aa_mode` 0 (coverage path active),
`divot` 1, `dither_filter` 1, gamma 0. `VI_X_SCALE` gives `x_add = 512` (a 2x
horizontal upscale, so `xfrac` alternates zero / non-zero); `VI_Y_SCALE` gives
`y_add = 1024` (so `yfrac` is constant). This is what made the zero-weight bilinear
tap skip worth 39.1% of the scan-out, and the arithmetic checks out exactly: `yfrac` is
always 0 and `xfrac` alternates 0 / 16, so the four-tap form averaged
`(1 + 4) / 2 = 2.5` filter chains per output pixel and the skip brings that to
`(1 + 2) / 2 = 1.5`. The predicted 1.67x on the scan-out against a measured 1.64x
(35.52 → 21.64 ms) is as close as this kind of accounting gets, which is the reason to write it down: a
speed-up that matches a mechanism is a result, and one that does not is a coincidence
waiting to be explained.

### The latch copies, anatomized (open — the largest in-model target left)

ADR 0011 recorded that "latch copy/zero instructions" were ~16% of runtime and that
removing them is "not safely possible", from a `perf annotate` view. Per-**line**
attribution of the post-`6a6adfa` render capture says where that 16% actually sits:

Line numbers are those of **commit `2abc817`**, the tree the capture was taken on, so
they stay resolvable as a permalink even after they drift in `main`; the function and
the statement are the durable part either way.

All six are in `crates/rustyn64-cpu/src/pipeline.rs`.

| function | statement | line at capture | share |
| --- | --- | --- | --- |
| `ex_stage` | `self.ex_dc = out;` (EX → DC store) | 2061 | 3.68% |
| `dc_stage` | `self.dc_wb = out;` (DC → WB store) | 913 | 2.87% |
| `ex_stage` | `let mut out = self.rf_ex;` (EX load) | 1903 | 2.58% |
| `ic_stage` | `self.ic_rf = Latch { ... }` (IC store) | 2351 | 2.15% |
| `dc_stage` | `let mut out = self.ex_dc;` (DC load) | 848 | 2.10% |
| `rf_stage` | `self.rf_ex = out;` (RF store) | 2147 | 1.28% |
| **six copy sites** | | | **14.66%** |

Grouped by stage rather than by site: `ex_stage` **6.26%**, `dc_stage` **4.97%**,
`ic_stage` 2.15%, `rf_stage` 1.28%. Each stage pays a load and a store except `IC`,
which only stores, and `WB`, which consumes `dc_wb` in place.

Plus 3.77% attributed to `pipeline.rs:0` (inlined, no line), so ~15-18% of the frame —
about **19-22 ms of 125** — is moving `Latch` values. That is more than the entire VI
scan-out cost after the memo.

**`Latch` is 120 bytes and its contents account for exactly that**, which is the
padding question rather than a dodge of it: `size_of::<Latch>()` **includes** whatever
alignment padding the layout needs, and the component figures are each `size_of` of that
component. They sum to the whole, so there is no padding left over — ADR 0011's
"zero-padding-optimal" claim holds, now measured rather than asserted.

| contents | bytes |
| --- | --- |
| `decoded: Decoded` | 16 |
| `abort: Option<Exception>` | 2 |
| `write_back: WriteBack` | 24 |
| `mem: Option<MemOp>` | 24 |
| `cop0: Option<Cop0Access>` | 24 |
| the six scalars (`occupied`, `pc`, `word`, `in_delay_slot`, `rs_val`, `rt_val`) | 30 |
| **`size_of::<Latch>()`** | **120** |

The scalar row is the remainder, and 30 is also their naive sum (1 + 8 + 4 + 1 + 8 + 8) —
so `repr(Rust)` has packed the two `bool`s into gaps rather than padding them out. That
is the useful reading of "no padding": there is no field ordering that would make this
struct smaller.

**That breakdown is now pinned in code**, because `repr(Rust)` layout is not stable
across compiler versions and this table would otherwise decay into a claim about a
toolchain nobody is using. `crates/rustyn64-cpu/src/pipeline.rs` carries
`const _: () = assert!(size_of::<Latch>() == 120, …)` next to the struct; if it fires,
the instruction is to re-measure rather than to change the number, since either a field
was added or the layout algorithm moved and the "no padding" conclusion needs
re-deriving. There are two assertions: **no padding**, which holds on every target and
is the property this breakdown rests on, and **120 where a `u64` aligns to 8**, written
as an implication so it pins the figure without breaking a cross-compile to an ABI the
figure does not describe.

**120 is the size on this workspace's targets, not a universal fact.** Every field is
fixed-width, but that does not make the layout width-independent — it is `u64`
*alignment* that varies. On `x86_64`, `thumbv7em-none-eabihf` and
`wasm32-unknown-unknown` (all three built in CI) it is 120; on 32-bit x86, where `u64`
aligns to 4, it is **108**. The assert firing on a newly added target is the guard
working: a different ABI is exactly when this breakdown must be re-measured.

**`#[repr(C)]` would be the wrong way to get that stability**, measured: it lays fields
out in declaration order, so the two `bool`s can no longer occupy alignment gaps and the
struct becomes **128 bytes**. On something copied four times per emulated cycle that adds
~1.2 ms a frame — to the very copies this section is about.

**What the byte breakdown adds to 0011's analysis.** The last three fields — **72 of the
120 bytes** — are *produced at `EX`*. In `ic_rf` and `rf_ex` they are structurally always
`None`/`WriteBack::None`, so those two latches copy 72 bytes of provably-empty payload,
twice each per cycle. That is the `:2351`, `:2147`, and `:1903` rows above — **6.01%** of
the frame, moving nothing.

This is **not** the hazard 0011 ruled out. That hazard is specific to the `DC` path:
`dc_stage`'s error branch re-reads `self.ex_dc` *after* `abort_with` has stamped it, which
entangles the `:848` / `:2061` / `:913` copies with abort propagation. The upstream pair
carries no such entanglement.

**Untested hypothesis, and it must stay labeled one until measured:** splitting `Latch`
into a front half (`occupied`, `pc`, `word`, `in_delay_slot`, `abort`, `decoded`,
`rs_val`, `rt_val` — 48 bytes) and an `EX`-onward payload would cut the two upstream
copies by **60%**, because that is the share of the bytes those latches carry for
nothing.

Two figures, and they are not the same claim: 60% of the 6.01% is 3.61%, so the
**expected** gain is `1 / (1 - 0.0361)` = **1.037x**. The **ceiling**, if those copies
disappeared outright rather than shrinking, is `1 / (1 - 0.0601)` = **1.064x** — which
nothing proposed here achieves, since the front half still has to move. Real, and nowhere near
the ~7.5x the frame budget needs — which is the point of recording it here rather than
acting on it: it is worth doing *after* the dispatch question is settled, not instead of
it.

Any attempt carries this repository's worst failure mode — `docs/engineering-lessons.md`
records four pipeline changes that compiled, passed every test, and did nothing — so it
needs the CPU golden-log 0-diff and n64-systemtest, not just `cargo test`.

### The Bus split-borrow moves 1.35 GB a frame (open)

**`core::mem::replace` is 5.32% of the frame on its own** — specifically the
`read_via_copy` / `write_via_move` pair inside it, which is the durable way to find these
samples. In the **Rust standard library's** `library/core/src/mem/mod.rs` (inside the
toolchain, not a file in this repository) that pair sat at lines 930 and 929, worth 4.21%
and 1.11%, under **`rustc 1.96.0`** — the exactly-pinned toolchain
(`rust-toolchain.toml`), which is the only reason a stdlib line number is quotable here
at all. On any other toolchain, search for the pair.

`take` is what the code calls and `replace` is what the profile shows because
`mem::take(x)` **is** `mem::replace(x, Default::default())` — which is also where the
second write comes from: the default has to be written into the vacated slot. About **6.7 ms of 125**, and all of it is the
Bus split-borrow — `Bus::rdp_tick` and `Bus::audio_tick` in
`crates/rustyn64-core/src/bus.rs` (lines 539 and 548 as of commit `2abc817`, same
convention as the table above):

```rust
pub fn rdp_tick(&mut self) {
    // `take` needs `Rdp: Default`; it writes a fresh default in place of the
    // value it hands back, so this is a read AND a write, not a move.
    let mut rdp = core::mem::take(&mut self.rdp);   // read 344 + write 344
    rdp.tick(self);
    self.rdp = rdp;                                 // write 344
}
```

`Rdp` is **344 bytes** and `Audio` is **88** (`size_of`-measured). Each `tick` therefore
touches roughly `3 x size_of` bytes, and the scheduler runs them **every RCP step** —
about 1.04 M steps a frame:

| | bytes moved per frame |
| --- | --- |
| `rdp_tick` | ~1.07 GB |
| `audio_tick` | ~0.27 GB |
| **total** | **~1.35 GB** |

At 125 ms a frame that is ~10.8 GB/s of memory traffic to satisfy the borrow checker,
which is consistent with the 5.32% the profile attributes to `core::mem`.

**The fix pattern is already in this repository.** `Bus::rsp_tick` (same file, line 519
at that commit)
used to do exactly this and no longer does; its comment records that the `take` was worse than the "no
allocation" claim above it, because `take` needs `Default` and constructing an `Rsp`
allocated its 8 KiB of scratch — 4 KiB DMEM and 4 KiB IMEM — **every RCP step**. `Rsp::tick` now *returns* what it
wants done instead of borrowing its owner. `Rdp` and `Audio` were not converted.

Two candidate routes, neither measured, both **hypotheses**:

- **Return-a-request**, as the RSP did. Clean for the AI; awkward for the RDP, which
  reads and writes RDRAM throughout rasterization rather than at the end.
- **Take only when there is work.** The RDP is idle on most RCP steps, so a predicate
  ahead of the `take` would remove the shuffle for the common case. The risk is entirely
  in "exact" — skipping a step that would have done something is a correctness bug — so
  the predicate has to come from `Rdp::tick`'s own early-outs rather than from intuition.
  Reading them, the tick returns **having touched nothing at all** in exactly two cases,
  and both are pure reads of `self`:

  - `status & (DP_STATUS_FREEZE | DP_STATUS_XBUS) != 0` — the pipeline counter is halted;
  - `cmd_current >= cmd_end` — the command FIFO is empty.

  Its third early-out, `stall > 0`, does **not** qualify: it decrements `stall`. But it
  does not need the bus either, so it can be handled before the `take` as well rather
  than being a reason to keep it. The fourth, a partially-written multi-word command
  (`cmd_end - cmd_current < len_bytes`), is **not** decidable without the bus — it reads
  the opcode from RDRAM to learn the length, and the RDP caches neither the opcode nor
  the length (`Rdp` holds only `cmd_start` / `cmd_current` / `cmd_end`, and `tick`
  re-reads `word0_hi` every time) — so that case must still take.

  `Audio::tick` is different and simpler: it writes `self.last_tick = now`
  unconditionally on entry, so it is *never* idle by this definition and needs the
  return-a-request route instead.

  Any predicate must be re-derived if those early-outs change, which is the argument for
  putting it next to them rather than in the Bus.

Upper bound if the whole 5.32% went: **1.056x** — **and that ceiling turned out to be
wrong**, which is worth more than the estimate was.

Implementing the RDP half alone (`Rdp::tick_without_bus`, so `rdp_tick` only takes when
the step actually needs the bus) measured **125.24 → 108.22 ms, 1.157x**. A result that
beats its own ceiling is a broken model, not a windfall, and the model's hole is
identifiable: the profile attributed the `read_via_copy` / `write_via_move` intrinsics
inside `mem::replace`, and **not** the construction of the `Rdp::default()` that `take`
writes into the vacated slot. That default is built inline in `rdp_tick` and charged
elsewhere in the attribution, so 5.32% was only the memcpy half of the cost.

The lesson generalizes past this line: a share read off a profile bounds the code the
profiler *named*, not the operation a reader has in mind. `take` is one word and two
distinct costs.

**Both are isolated, and they compose.** They address disjoint shares, so:

| | latch split | split-borrow | together |
| --- | --- | --- | --- |
| expected | 1.037x (3.61%) | 1.056x (5.32%) | **1.098x** → ~114 ms, 8.8 FPS |
| ceiling | 1.064x (6.01%) | 1.056x (5.32%) | **1.128x** → ~111 ms, 9.0 FPS |

**The split-borrow column is superseded**: its RDP half shipped and measured 1.157x on
its own, taking the frame to **108.22 ms (9.24 FPS)**.

Two later revisions of that change measured **105.53 ms** and **105.74 ms** — one adding
a release-build precondition guard, the next replacing it with a compile-time token, so
they differ from each other in real work and from the first only in code a `--release`
build compiles out. Three builds, two functional differences, one number: **the 2.5%
tracks when the measurement was taken, not what was measured.** It is cross-session drift
of exactly the kind the method section warns about, which is why the figure quoted above
is the conservative **108.22 ms** rather than the best one seen. The audio half is still open, and
so is the latch split — whose 1.037x estimate should now be read with the same suspicion,
since it was built the same way from the same kind of profile share.

Cumulatively the frame has gone **155.13 → 108.22 ms, 1.433x**, and the gap to 60 FPS is
**6.5x**. That is the case for doing them after the dispatch question rather than
instead of it — and the Angrylion `.rvec` vectors and
the audio goldens are what would catch an idle predicate that is wrong.

### A-B-A, when a session drifts under you

Measuring `#[inline]` on `Rdp::tick_without_bus` — a cross-crate call on the hot path —
produced this, in one sitting, minutes apart:

| leg | | frame |
| --- | --- | --- |
| A | without `#[inline]` | 105.84 / 105.64 ms |
| B | **with** `#[inline]` | 107.50 / 107.57 ms |
| A | without, again | 107.35 / 107.45 ms |

Two legs would have reported `#[inline]` as a **1.7% regression** and it is nothing of
the kind: the third leg matches B, so the machine simply got ~1.7% slower partway through
and stayed there. `#[inline]` is **neutral**, which is the expected answer under
`lto = "fat"` — LLVM already has the callee's body across the crate boundary, so the hint
adds nothing.

**Repeat the baseline after the change, not just before it.** Back-to-back runs of one
binary agree to 0.05-0.13%, but a session drifts by ~1-2% over tens of minutes, which is
the same size as a real optimization. A before/after pair cannot tell those apart; A-B-A
can.

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
   `#[inline(always)]` made the scan-out **36% worse** (35.5 → 48.4 ms). Also,
   `#[inline]` on `Rdp::tick_without_bus`, a cross-crate call on the hot path:
   **neutral**, by the A-B-A above. Under `lto = "fat"` an inline hint has nothing to
   add — that is now two independent results, and the general form is *this workspace's
   release profile has already done the inlining*.
5. **Reordering `vi_divot`'s early-out to test coverage first** — **10% worse**, measured.

   The *explanation* offered for that regression — that `cvg == 7` is rare, so the
   AA-edge filter rather than the 8-tap de-dither is the hot path — is a **hypothesis,
   not a measurement**. A single aggregate regression cannot separate "the reordered test
   is rarely satisfied" from "the reordered test is itself more expensive". Settling it
   needs a coverage histogram over a real frame, which belongs with the scan-out
   memoization work rather than here. Until then, do not aim an optimization using it.
6. **Shrinking `Latch` to cut the inter-stage copies** — the premise does not survive a
   sensitivity test. Six source lines in `pipeline.rs` are all the same thing (`self.ex_dc =
   out`, `let mut out = self.rf_ex`, and their peers) and together they are **16.1%** of a
   rendering frame, which reads as "the copies are expensive". They are not: **adding**
   `[u64; 9]` of padding to `Latch`, taking it 120 → 192 bytes and the bytes moved per
   emulated cycle 840 → 1344 (+60%), measured **1.1% faster**, not slower —
   103.45/103.18 ms → 102.29/102.03 ms.

   So `perf` is charging each stage's retired work to the store that ends it, and the 16.1%
   is not a transfer cost that shrinking the struct would recover. This retires the sizing
   behind the "split `Latch` into a front half and an EX-onward payload" task, which the
   line attribution had made look like ~1.07x.

   **Provenance.** Experiment A (`frame_cost_probe`, `where_does_a_frame_go`), 30 frames
   from the first live-VI frame, on the environment tabled in §Measured (2026-07-30) —
   same machine, `rustc 1.96.0`, same SM64 dump. Tree at commit `9631c06` with the padding
   applied as an uncommitted patch: `probe_pad: [u64; 9]` added to `Latch`, the two
   non-struct-update construction sites extended, and `pipeline.rs`'s size assertion
   retargeted 120 → 192. Built with `cargo build -p rustyn64-frontend --release --tests`.
   Two runs padded; the unpadded pair is the B leg of the DAC-period A-B-A above, run
   minutes earlier on the same tree minus the patch. The patch was reverted afterwards
   and the tree verified clean; it is not in any commit.

   **Two weaknesses, because they bound what may be concluded.**

   *It is a two-leg comparison, not A-B-A.* There is no return leg, and 1.1% sits inside
   the 1–2% a session drifts. **So "1.1% faster" is not a claim** — the padded build and
   the unpadded one are indistinguishable here. What the data does carry is the *absence
   of the predicted regression*: if the copies were transfer-bound at 16.1%, +60% of
   traffic predicts roughly **+10%** frame time, and nothing remotely that size appeared.
   Refuting a 10% prediction does not require resolving 1%.

   *Padding that nothing reads can be narrowed by LLVM*, so this cannot prove every added
   byte was actually moved. Evidence against copy width driving the cost, then, rather
   than proof — but the codegen did change (the timings moved outside the back-to-back
   spread), so it was not a no-op build.

   Together that is enough to say the refactor should not be undertaken *on this
   evidence*. A sound positive test is the split itself, which is the work being
   questioned; do it only if some other measurement first shows the copies matter.

## The AI's split-borrow move, and a ceiling that was wrong for the second time

`Bus::audio_tick` took the whole `Audio` out of the Bus with `core::mem::take` on **every
RCP step** — `size_of::<Audio>()` is **88 bytes**, so a `Default` is written into the
vacated slot, 88 bytes move out, and 88 move back, ~264 bytes of traffic and the drop of
the vacated value each time. The DAC emits nothing on ~1,949 of every 1,950 steps, so
almost all of it was waste. `Bus::audio_tick` now asks `Audio::tick_without_bus` first and
only takes when it hands back a `NeedsBus`.

A-B-A, one sitting, `frame_cost_probe` on Super Mario 64, `--release`, environment as
tabled in §Measured (2026-07-30):

| leg | variant | frame mean |
| --- | --- | --- |
| A | take on every step | 105.391 / 106.086 ms |
| B | **take only when a sample is due** | **97.854 / 97.309 ms** |
| A | take on every step, again | 105.447 ms |

Three A legs spanning 0.66%, bracketing B, so the session did not drift. **105.64 → 97.58
ms, 1.083x**; the conservative pairing — the *best* A against the *worst* B, `105.391 /
97.854` — gives **1.077x**.

**It beat its predicted size by ~5x, which is a broken model rather than a windfall — and
it is the same broken model as #219's.** The profile attributed 2.68% to
`core/src/mem/mod.rs` (the `mem::replace` copy intrinsics, *shared* between the RSP, RDP,
and AI takes) plus 1.01% to the `audio_tick` line, so ~1.5% was the expectation. What that
share does not name is the rest of a take-and-restore: constructing the `Default`, writing
it into the slot, writing the real value back, and dropping the vacated one — for `Audio`
that last part is drop glue for the `Vec` sink, on every step.

The generalization already recorded for #219 — *a profile share bounds the code the
profiler named* — now has a second independent confirmation, so treat it as the rule and
not the exception. **Corollary for the remaining split-borrows: do not size one from its
`mem::replace` share.** The RSP's take is the last one, and it is a different case — the
RSP executes microcode on essentially every step, so it genuinely needs the bus and there
is no idle majority to skip.

## The VI divided for a half-line period it almost never needed

Re-profiling after the AI work put **`crates/rustyn64-core/src/vi.rs:167` at 5.00% — the
single hottest line in the frame.** That line is
`MASTER_HZ / (field_hz() * total_halflines())`, a 64-bit divide, and `Vi::tick` runs on
every RCP step. A half-line is ~5,952 master ticks against a step every 3, so a half-line
elapses on roughly **one call in 1,984** and the rest divided to compare against an
accumulator that had not reached the period.

Fixed with a bound rather than a cache, so no state was added and the save-state layout is
untouched (ADR 0005). `total_halflines()` is `(VI_V_TOTAL & 0x3FF) + 1`, so the field is
**1..=1024** half-lines, and `field_hz()` is 50 or 60. The divisor is therefore at most
61,440 and

```text
VI_MIN_TICKS_PER_HALFLINE = MASTER_HZ / (60 * 1024) = 3051
```

is a true lower bound over the **entire** programmable space. Below it the `while` cannot
execute for any legal register programming, so the divide is skipped — **exact, not
approximate**. A `const` assertion ties the constant to the `0x3FF` mask and to NTSC being
the faster field rate, so widening either breaks the build instead of silently making the
bound too large, which would swallow a half-line and make the VI interrupt late.

A-B-A, one sitting, environment as tabled in §Measured (2026-07-30):

| leg | variant | frame mean |
| --- | --- | --- |
| A | divide every step | 96.547 / 97.214 ms |
| B | **skip below the bound** | **94.925 / 93.642 ms** |
| A | divide every step, again | 97.310 / 98.117 ms |

**This one is noisier than the others and the number is quoted loosely on purpose.** The
four A legs climb monotonically (96.5 → 98.1, a 1.6% upward drift over the sequence) and
the two B legs differ by 1.37%, both wider than the 0.05–0.13% back-to-back spread. What is
solid is the *sign*: both A brackets sit entirely above both B legs. Taking the adjacent
brackets (A2, A3) against B gives **~1.03x**; the conservative pairing, best A over worst B
(`96.547 / 94.925`), gives **1.017x**.

**The result matches the model once the model is right, and getting there took a review.**
The first version of this section said the bound skipped nearly every call, which would
predict ~1.053x from a 5.00% share, and then explained the shortfall by saying the profile
had over-predicted. Both halves were wrong. The bound is **global**, so it is loose for any
particular field length: NTSC's period is 5,952 ticks against a 3,051-tick bound, and a call
whose accumulator lies between the two still divides even though it cannot advance. So

```text
skip fraction = 3051 / 5952 = 51.3%
expected      = 5.00% x 0.513 = 2.56%  ->  1.026x
```

against **~1.03x** measured. The share did not over-predict; the skip fraction was
mis-modeled, and blaming the profiler was the easy answer that stopped the inquiry one step
early. Recorded because "the profile over-predicted" is the kind of explanation that sounds
like a finding and forecloses the arithmetic that would have refuted it.

**Remaining headroom here is the other 48.7%**, and it is not free: tightening the bound
means remembering the current period across calls, which is a field, hence a save-state
question (ADR 0005). Not obviously worth it for ~2.4% — noted rather than pursued.

**A dead guard and a wrong comment, found on the way.** `Vi::tick` returned early on
`per_hl == 0` under the comment "no timing until `VI_V_TOTAL` is programmed". That branch is
unreachable — the divisor is at least 50 against a 187.5 MHz numerator — and the comment
misdescribed it besides: an unprogrammed `VI_V_TOTAL` is *one* half-line, a period of
3,750,000 ticks, very long and never zero. The guard stays as a divide-by-zero backstop with
a `debug_assert` making the claim checkable, which is the same treatment ledger R-16's guard
gets in the AI.

## The fast scheduler replays the edge pattern instead of re-deriving it

ADR 0011's first executing block. A domain steps when `(tick + phase) % divider == 0`
and the phases are power-on constants, so which domains are due repeats every
`lcm(CPU_DIVIDER, RCP_DIVIDER) = 6` ticks and never changes mid-run — yet the
accurate loop re-derives it on every edge (`next_edge` for the position, then two
`is_edge` tests to attribute it). The fast path computes that shape once and
replays it, which is a different **enumeration** of the same edges, not a different
schedule.

| | |
| --- | --- |
| Harness | `crates/rustyn64-core/tests/fast_scheduler_differential.rs`, `probe_scheduler_dispatch_cost` (`#[ignore]`d) |
| Command | `cargo test -p rustyn64-core --release --features fast-scheduler --test fast_scheduler_differential -- --ignored --nocapture` |
| Workload | `System::new(SEED)` run to **60,000,000** master ticks, both paths, from an identical power-on state |
| Method | best of 3 per path, in one process, alternating accurate/fast each repetition |
| Environment | as tabled in §Measured (2026-07-30) — same machine, `rustc 1.96.0` |
| Result | accurate **1.8113 s**, fast **1.7147 s** → **1.0563x** |

**What the number is and is not.** `run_until` executes the whole machine, so this
is a **whole-system** figure rather than a share of one — but it is measured
*headlessly*, with no VI scan-out and no frontend, so it is not a frame time.

**The frame-level figure, measured through the frontend.** With the feature
forwarded from `rustyn64-frontend` (`--features fast-scheduler`, still default-off),
A-B-A on the same probe and environment as the entries above:

| leg | variant | frame mean |
| --- | --- | --- |
| A | accurate (default build) | 98.532 / 98.086 ms |
| B | **`--features fast-scheduler`** | **93.643 / 92.472 ms** |
| A | accurate, again | 97.753 ms |

**98.12 → 93.06 ms, 1.0544x** (conservative pairing, best A over worst B: 1.0439x);
**10.19 → 10.75 FPS**. The A legs span 0.80% and the B legs 1.27% — wider than the
back-to-back floor, so the magnitude is loose — but all three A legs sit above both
B legs, so the sign is not in question. It corroborates the headless 1.0563x and is
slightly smaller, which is what the scan-out sitting outside `run_until` predicts.

**Limitations, stated because best-of-3 in one process is weaker than the A-B-A
protocol used for the frame measurements above.** No ROM is loaded, so this is the
reset-vector workload rather than a title's instruction mix; the two paths alternate
within one process rather than across separate builds; and there is no return leg.
It is a dispatch-cost differential between two implementations of the same
enumeration, not a hardware-accuracy result — which is why it lives here and not in
`docs/accuracy-ledger.md`, whose scope is measured hardware constants and residuals.

The feature stays **default-off**, so no shipped build is affected by any of this.

## The search for structural waste is exhausted — evidence, so it is not re-run

Every bucket at or above 3% of a rendering frame has now been examined **line by
line**, on the profile taken from `main` at 9e41f77 (64,357 samples, render phase).
The conclusion is that **no remaining bucket contains a hotspot**, and the wins this
project found were never hotspots to begin with.

| bucket | share | hottest single line | what it is |
| --- | --- | --- | --- |
| `pipeline.rs` | 36.1% | 4.42% (`ex_dc = out`) | retired work charged to a stage's final store — **not** copy cost; see "Ruled out" item 6 |
| RSP `vu.rs` + `su.rs` | 13.6% | **0.80%** (`su.rs`'s `Rsp::r`) | `r0`-pinned register read, then multiply-family match arms — interpreter dispatch |
| `bus.rs` | 10.8% | **0.84%** | memory-access dispatch, spread over the whole address map |
| `scheduler.rs` | 9.6% | 1.66% | edge derivation — **addressed** by the `fast-scheduler` block |
| `decode.rs` | 3.6% | 0.36% | instruction decode, spread across the opcode space |
| `vi.rs` | 3.4% | 2.34% | the half of `ticks_per_halfline` a global bound cannot skip |

**The shape of every win this project actually landed** was a *structural* observation,
not a hot line: a value recomputed every step that almost never changes (the AI's DAC
period, the VI's half-line period, the scheduler's edge attribution), or work done
before the check that made it unnecessary (both split-borrow takes). Searching for the
tallest bar found none of them — and once, in the `Latch` case, actively misled.

**That class is now empty.** There is no other per-step `MASTER_HZ /` divide in the
core; the two split-borrows that had an idle majority are done, and the RSP's does
not have one, because it executes microcode on essentially every step. What is left in each bucket above is
the emulator computing what it is required to compute.

**So the next increment is not an optimization.** Going further means changing *what*
is computed rather than when or how often — the dynarec question, with its own ADR,
its own correctness surface, and `unsafe` confined to where the repo already permits
it. [`docs/adr/0011-optional-fast-path-scheduler.md`](adr/0011-optional-fast-path-scheduler.md)
deliberately leaves that open; nothing here decides it.

**Do not re-run this survey** expecting a different answer without a workload change.
It is reproducible in full — the flags matter, and an incomplete command is not a
reproducible one:

```bash
CARGO_PROFILE_RELEASE_DEBUG=1 cargo build --release --tests -p rustyn64-frontend
RUSTYN64_PROBE_ROM=/path/rom.z64 RUSTYN64_PROBE_SKIP=1400 \
  perf record -D 70000 -F 999 -g --call-graph=dwarf,4096 -o render.data \
  -- target/release/deps/gameplay_phase_probe-* --ignored --nocapture \
  does_a_retail_title_reach_a_rendering_phase
perf report -i render.data -s srcline --full-source-path --no-children -g none \
  --stdio --percent-limit 0
```

`-D 70000` is a delay in **milliseconds** — 70 s, which is what discards the boot
phase and leaves a rendering-phase profile; `-F 999` is the sample rate in Hz.
Aggregate per file by summing the per-line percentages.

## The render-phase map, re-measured after the VI and RDP work

The attribution the earlier plan worked from was taken at **138.7 ms/frame**, before the
scan-out memo and the RDP split-borrow skip. Both cut buckets that map named, so it is
stale for exactly the parts that changed and must not be used to size the next step.

Re-measured on the current head: Super Mario 64, `--release` with
`CARGO_PROFILE_RELEASE_DEBUG=1`, `gameplay_phase_probe` advanced 1,400 frames,
`perf record -D 70000 -F 999 --call-graph=dwarf`, **67,278 samples**, attributed with
`perf report -s srcline --full-source-path --no-children -g none` and summed per file.
The `-D` delay is what keeps this a *rendering* profile: a boot-window capture reports the
RDP at 0.00%.

| subsystem | old map (138.7 ms) | **now** |
| --- | --- | --- |
| CPU (`pipeline` 31.8 + decode/cop0/addr/cache/exec/regs/alu/softfloat/mem/fpr) | 31.6% | **44.6%** |
| Bus + VI (`bus.rs` 11.4 + `vi.rs` 2.6) | 29.0% | **14.0%** |
| RSP (`vu.rs` 6.7 + `su.rs` 6.5) | 9.0% | **13.2%** |
| scheduler | 6.4% | **8.9%** |
| RDP | 7.5% | **4.3%** |
| AI (audio) | not broken out | **4.0%** |
| core/stdlib (inlined arithmetic, `mem`, conversions) | — | 8.7% |
| **subtotal** | 83.5% | **97.7%** |
| unattributed remainder | 16.5% | 2.3% |
| **total** | **100%** | **100%** |

The two columns are not equally complete, and the remainders say so. The "now" column
resolves all but 2.3% — frontend, allocator, and per-file shares below 0.28% that were not
worth a row. The old column's 16.5% is larger because it was recorded as five coarse
subsystem buckets with no AI or stdlib row at all; that is a limit of the old record, not a
gap that has since been closed. **Do not read the old column as a like-for-like baseline**:
use it for the direction of travel, and the "now" column for arithmetic.

Bus + VI has halved and the CPU is now the clear majority. Nothing "got slower": these are
shares of a smaller frame.

## The 60 FPS target is out of reach for this execution model — measured, not estimated

At 103.3 ms a frame, 16.7 ms means **6.19x**: 83.8% of all work must disappear.

The fast-path scheduler (ADR 0011) targets the CPU pipeline and the scheduler's per-edge
dispatch. Those are **44.6% + 8.9% = 53.5%** of the frame. Set both to *zero* — a fast path
so good the CPU costs nothing at all — and the frame still contains the other 46.5%:

- ceiling, both buckets free: 103.3 x 0.465 = **48.0 ms → 20.8 FPS (2.15x)**
- realistic, if a block interpreter removes ~2/3 of the CPU bucket and most of dispatch:
  **~66 ms → ~15 FPS (1.57x)**

So ADR 0011 is worth doing — 1.5x is the largest single win left on the board — but the
task list's description of it as "the only path to 60 FPS" is **wrong, and this measurement
retires that claim**. There is no path to 60 FPS through it, because there is no path to
60 FPS through eliminating any one subsystem: no bucket above is 84% of the frame, and the
four non-CPU ones total 40% on their own.

The measured bound above is arithmetic and stands on its own. What follows is a *proposed
route*, not a consequence of it.

The usual way emulators close a gap of this size is dynamic recompilation of the VR4300 and
RSP. ADR 0011 deliberately leaves the fast path's mechanism open, so a dynarec is within
what that ADR permits rather than a departure from it. Its costs are the reason it is not
scheduled here, not a prohibition:

- It is a second execution architecture, with a second correctness surface, on top of the
  block-based fast path ADR 0011 already asks for.
- Emitting and running host code needs `unsafe`. Every chip crate and `rustyn64-core` carry
  `#![forbid(unsafe_code)]`, so it could not live in any of them — but the repo already
  permits `unsafe` in the frontend and FFI, so this is a **placement and review question**,
  not an impossibility. It would want its own ADR.

Worth stating either way: cycle-accurate LLE N64 emulation is not usually real-time. CEN64,
named in this project's accuracy bar, does not run full speed either.

**What this does not say.** It does not say optimization is finished. It says the *goal
number* needs restating: "as fast as the cycle-accurate model can go" is reachable and is
what the remaining work delivers; "60 FPS with the cycle-accurate model" is not, on this
host, by these means.

## The AI recomputed its DAC period 1.04 M times a frame

`Audio::tick` opened by computing `period_ticks()` — `MASTER_HZ / sample_rate`, a **64-bit
divide** — and only then asked whether a sample was due. The scheduler calls it on every RCP
step, ~1.04 M times a frame, while at ~32 kHz the period is ~5,859 master ticks, so roughly
**1,950 of every 1,951 calls divided and threw the quotient away**. One source line,
`crates/rustyn64-audio/src/lib.rs:489`, was **3.67%** of a rendering frame — the largest
single line outside the CPU pipeline.

The fix is ordering, not caching: return before the divide when
`next_sample_tick != 0 && now < next_sample_tick`. Behavior-identical rather than
approximately so — on that path the old code either returned at the `period == 0` guard or
fell into a `while` whose condition is exactly the negation of the new test, and neither
route touches a field. A memo field was rejected: the quotient is derived from
`sample_rate`, and caching derived state in a serialized struct changes the save-state
layout (ADR 0005) to buy what the reordering buys for free.

A-B-A, one sitting, `frame_cost_probe` on Super Mario 64, `--release`:

| leg | | frame mean |
| --- | --- | --- |
| A | before | 107.413 / 107.587 ms |
| B | **after** | 103.447 / 103.175 ms |
| A | before, again | 107.652 ms |

Three A legs within **0.22%**, so the session did not drift; **107.55 → 103.31 ms, 1.041x**
(conservative pairing of the worst B against the best A gives 1.038x). The profile predicted
3.67% and the change delivered 4.0% — the small excess is consistent with also freeing the
divider unit, and is not claimed as anything more.

**Coverage note, because the guard was mutation-checked and the suite failed the check.**
Breaking the early-out grossly (never emit again) is caught by seven existing tests. The
*off-by-one* — `<=` instead of `<`, which defers every sample by one RCP step — left the
entire workspace green, because the other AI tests advance `now` in strides of a full period
and never land on the boundary. That is audio which is correct but late, the same
correct-but-late class ADR 0011 names for the fast-path bailout, and it now has a test
(`a_sample_due_exactly_now_is_emitted_on_this_call`) that goes red under the mutation and
green without it.
