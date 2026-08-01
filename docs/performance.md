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
5c. **PGO (`-Cprofile-generate` / `-Cprofile-use`)** — **4.96% SLOWER** on this
   workload, with non-overlapping legs. See the codegen-levers section. Needed a
   benchmark *binary* to measure at all, because a `#[test]` cannot be built with
   `-Cprofile-use` under `panic = "abort"`.
5b. **`-C target-cpu=native`** — **neutral on this benchmark**, and the A-B-A caught
   it: the third baseline leg came in below both native legs. See the codegen-levers
   section below for the table and for what the result does *not* establish.
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

## Codegen levers: `target-cpu=native` measures neutral

Source-level structural waste is one search; **how the compiler emits the code** is a
separate one, and it was unexamined until now. Nothing in the workspace sets
`target-cpu`, so every build targets baseline `x86-64` while this host offers AVX2 and
BMI2 — which looks like free headroom.

It is not. A-B-A, `frame_cost_probe` on Super Mario 64, `--release --features
fast-scheduler`, environment as tabled in §Measured (2026-07-30):

| leg | variant | frame mean |
| --- | --- | --- |
| A | baseline (`x86-64`) | 93.777 / 93.689 ms |
| B | `RUSTFLAGS="-C target-cpu=native"` | 93.399 / 93.056 ms |
| A | baseline, again | **92.068 ms** |

**The third A leg came in below *both* B legs.** A two-leg comparison would have
reported a ~0.5% win; the return leg shows it was drift. Mean A 93.178 against mean B
93.227 — `0.9995x`, which is to say **neutral**.

**What that does and does not establish.** It establishes *no measurable gain on this
benchmark*. It does **not** establish that the emulator is generally not
instruction-selection-bound — a different title, or the same one in a different phase,
could weight the subsystems differently, and an aggregate frame time conflates every
cost in the frame. The tempting explanation — that the hot code is scalar integer
interpretation and pointer chasing rather than the vectorizable arithmetic AVX2 helps —
is **an inference, not a measurement**. It is consistent with the per-line survey below
(no vectorizable inner loop appears anywhere in it) but nothing here isolates the
mechanism, and confirming it would need differential profiling of the two builds rather
than one number from each. Flagged because explaining a null result with an untested
story is how a plausible mechanism becomes a repeated citation.

So it is **not** adopted, and the portability cost is real enough not to need the
performance argument: `target-cpu=native` may emit instructions an older host does not
implement, and such a binary **may** fail there with an illegal instruction. Not
guaranteed — it depends on which features LLVM actually selects and what the older host
supports — but it is a real risk to take for a measured nothing.

### PGO measures 5% SLOWER, which was not the expected answer

The prediction in the paragraph this replaces was that PGO was the most promising
untested lever, since interpreters are the workload it classically helps most. It was
tested. It is a regression.

First it needed a harness. `frame_cost_probe` is a `#[test]`, and a test **cannot** be
built with `-Cprofile-use` in this workspace — tests require `panic = "unwind"` while
the release profile is `panic = "abort"`, and `RUSTFLAGS` applies the flag to every
crate in the graph:

```text
error: the crate `rustyn64_frontend` requires panic strategy `abort`
       which is incompatible with this crate's strategy of `unwind`
```

That survives `cargo clean --release`, so it is structural rather than stale artifacts.
The *binary* path has no such requirement, which is why
`crates/rustyn64-frontend/examples/frame_bench.rs` exists: the same measurement shaped
as an example, so it can be instrumented and rebuilt. An example rather than a `[[bin]]`
so a plain `cargo build` does not carry a benchmark into every build.

The full cycle — instrument, run, `llvm-profdata merge` (18.6 MB from 50 raw files),
rebuild with `-Cprofile-use`, and A-B-A **in one sitting** (the legs are only
comparable to each other — a later verification run of the same binary read 101.6 ms
after an hour of heavy building, which is drift of the kind §"A-B-A, when a session
drifts under you" describes and is why the return legs are the control):

| leg | variant | mean over 120 frames |
| --- | --- | --- |
| A | baseline | 98.323 ms |
| B | **PGO** | **104.060 / 102.918 ms** |
| A | baseline, again | 98.764 / 98.704 ms |

**Mean A 98.597, mean B 103.489 — PGO is 4.96% slower.** The *worst* baseline leg is
still faster than the *best* PGO leg, so the legs do not overlap and this is not the
drift that caught `target-cpu=native`. The instrumented run itself cost 141.6 ms/frame,
the expected ~44% instrumentation overhead, and every run retired an identical
219,075,686 instructions, so the three builds executed exactly the same work.

**Why is inference, not measurement**, and it is worth flagging as such because the
tempting explanation is a story: this workspace already builds with `lto = "fat"` and
`codegen-units = 1`, which gives LLVM whole-program visibility, and PGO's
profile-driven inlining and block-layout decisions *override* choices fat LTO had
already made globally. That is consistent with the result and with PGO's usual caveat
about interacting badly with aggressive LTO — but nothing here isolates it, and
confirming it would need a PGO-vs-no-PGO comparison at `lto = "thin"` or off. Recorded
as a hypothesis so it is not cited later as a finding.

**Not adopted**, and the ruled-out list gains item 5c. Note what this does *not* say:
PGO is not useless in general, and a future build configuration — different LTO mode,
a dynarec with different hot-path structure — could change the answer. It says PGO is a
regression *for this configuration on this workload*, which is the only thing three
paired runs can say.

**With this, every reachable codegen lever is measured** — instruction selection
neutral, PGO negative, and the `fast-scheduler` block's 1.054x already merged. What
remains is architectural rather than a build setting.

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

## `fast-exec`: the instruction-granular path measures 1.53x

The first number from ADR 0013's second execution mode, and the first result in this
document that comes from changing **what is computed** rather than when.

**A-B-A, one sitting, `main` at the wiring commit**, Super Mario 64, 120 timed
frames after the VI comes up, `examples/frame_bench.rs`:

| leg | build | mean frame |
| --- | --- | --- |
| A1 | accurate | 100.582 ms |
| A2 | accurate | 99.847 ms |
| B1 | `--features fast-exec` | **64.822 ms** |
| B2 | `--features fast-exec` | **65.321 ms** |
| A3 | accurate | 99.712 ms |
| A4 | accurate | 99.598 ms |

**The legs do not overlap.** Every A is above 99.5 ms and every B below 65.4 ms, so
this is not drift: the four A readings span 0.99%, which is the ordinary
within-session spread this document records elsewhere, and the gap to B is thirty
times that.

Quoting the **conservative** pairing — best A over worst B — as this document does
throughout:

- **99.598 → 65.321 ms, 1.525x**
- **10.04 → 15.31 FPS**

The return legs matter here for the reason they always do: a two-leg comparison
would have reported 1.55x from A1, and the third-time-lucky lesson in §*Ruled out*
5b is that the return leg is what tells drift from effect.

**Why the accurate figure is ~99.6 ms and not the 93.06 ms recorded above.** That
earlier number was measured **with `fast-scheduler` enabled**; the accurate baseline
in the same pairing was 98.12 ms. So ~99.6 ms is that baseline plus ordinary
cross-session drift, not a regression. Comparing a featured build against an
unfeatured one is the mistake this note exists to prevent.

**What it costs.** The two modes retire different instruction counts over the same
120 frames — 173,254,496 versus 171,471,972, **+1.04%** — which is the timing
divergence ADR 0013 §4 requires to be measured and bounded rather than eliminated.
It is recorded as **C-16** in `docs/accuracy-ledger.md` with its method and the
conditions that would move it.

**What it does not do.** 60 FPS needs 16.67 ms. At 65.3 ms this is **3.92x** short,
and the ceiling arithmetic in §*The 60 FPS target is out of reach* is unchanged in
kind: the CPU pipeline was the largest single bucket and it has now been addressed,
so the remaining work is in the buckets that were never the biggest. The next
measurement to take is a fresh profile of the **fast-exec** frame — the shares
above were taken on the accurate path and no longer describe what this build
spends its time on.

## The fast-exec frame, profiled — and the plan's next phase is undercut by it

The shares in this document were all taken on the accurate path. They no longer
describe the `fast-exec` build, so here is a **paired** capture of both, on the
same probe, over the **same emulated frames**.

### Method, and the two things that had to be recalibrated

`gameplay_phase_probe` — the **render-phase** probe; `frame_cost_probe`'s window is
early boot and puts the RDP at 0.00%. Super Mario 64, hash verified against §Method
before any number below was quoted.

```bash
ROOT=$(git rev-parse --show-toplevel)
ROM="$ROOT/tests/roms/external/commercial/eeprom-4k/Super Mario 64.z64"

# The build under test. Drop `--features fast-exec` for the accurate leg.
BIN=$(CARGO_PROFILE_RELEASE_DEBUG=1 cargo test -p rustyn64-frontend --release \
        --features fast-exec --no-run --test gameplay_phase_probe \
        --message-format=json \
      | jq -r 'select(.reason == "compiler-artifact"
                     and .target.name == "gameplay_phase_probe"
                     and .executable != null) | .executable' | tail -n 1)

# -D 48600 for fast-exec, -D 70000 for the accurate leg: both land on frame ~763.
# See the calibration note below — these are NOT interchangeable.
RUSTYN64_PROBE_ROM="$ROM" RUSTYN64_PROBE_SKIP=900 \
  perf record -F 999 -D 48600 -o "$ROOT/target/perf_fastexec.data" -- \
  "$BIN" does_a_retail_title_reach_a_rendering_phase --ignored --nocapture

perf report -i "$ROOT/target/perf_fastexec.data" -s srcline --full-source-path \
  --no-children -g none --stdio
```

Two calibrations that a naive re-run gets wrong:

1. **`-D 70000` discards the entire `fast-exec` run.** The recorded delay is
   calibrated to a 82.5 s accurate run; `fast-exec` finishes the same 900 frames in
   57.3 s. Left alone, the capture would be empty.
2. **Matching wall-clock is not matching workload.** The delay has to land on the
   same *emulated frame*, or the two profiles cover different game content.
   Accurate `-D 70000` starts at frame ~763; `fast-exec` needs **`-D 48600`** to
   start there too.

   **That frame number is derived, not asserted, and the distinction matters.**
   `perf record -D` takes wall-clock milliseconds while `RUSTYN64_PROBE_SKIP`
   counts frames, so the alignment is arithmetic — `70.0 s / 91.65 ms = 763`
   and `48.6 s / 63.70 ms = 763` — using each leg's *whole-run mean* frame
   cost. Frame cost is not uniform across the run (boot frames are cheaper), so the
   real start frames differ from 763 and from each other by an unmeasured amount.
   Making this exact means having the probe print its frame index when the capture
   begins, and that is the right fix if these numbers ever need to be quoted more
   precisely than they are here. The control capture below is what bounds the error
   in the meantime. A first capture at `-D 45000` (frame ~706) is kept below as a
   control — it agrees to within a point on every bucket, which is what says the
   shares are stable rather than an artifact of where the window fell.

### The render-phase frame, and a third ratio

| | accurate | `fast-exec` |
| --- | --- | --- |
| in-window frame cost | **91.2 ms** | **63.4 ms** |
| ratio | — | **1.437x** |

**This is not the 1.53x figure above, and both are right.** That one is
`frame_bench`'s window (frames 37–156, early boot); this is the render phase. A
speedup is a property of a workload, not of a build, and quoting one number for
both windows is how a measurement becomes a slogan.

### Where the time goes now

| bucket | accurate | `fast-exec` (aligned) | control (`-D 45000`) | absolute change |
| --- | --- | --- | --- | --- |
| CPU | 50.24% | **32.29%** | 31.62% | **-55.3%** |
| RSP | 13.29% | **21.42%** | 22.16% | +12.1% |
| Bus | 11.07% | **18.06%** | 18.49% | +13.5% |
| stdlib / inlined | 7.78% | 11.57% | 11.78% | +3.5% |
| RDP | 4.47% | 6.36% | 6.88% | -1.0% |
| scheduler | 9.26% | **5.05%** | 5.11% | **-62.1%** |
| VI | 3.76% | 4.64% | 4.21% | -14.1% |
| AI | 0.37% | 0.03% | 0.11% | -94.4% |

The absolute column rescales by the 1.437x frame-cost ratio, so it answers *did
this code get faster*, which the share column cannot.

**The CPU and the scheduler are what moved**, which is what the change was for.
Together they were **59.5%** of the accurate frame and are now **37.3%** of a frame
that is itself 30% smaller.

### The RSP and Bus buckets appear to grow, and that is not yet explained

+12% and +13% in absolute terms.

**The expectation that they should be flat is itself unverified, and stating it as
fact would be the very mistake this section is about.** The reasoning is that
neither subsystem's *job* changes between modes — but the two runs demonstrably do
**not** execute identical work: they retire 1.04% more instructions (ledger C-16),
so the RSP and Bus are driven by a machine in a slightly different state. Matching
emulated frame numbers is not the same as matching emulated work. And a sampled
attribution rescaled by a frame-cost ratio cannot separate *this code got slower*
from *this code ran more often* from *the compiler charged it differently*.

*Leading hypothesis, not measurement:* **attribution shift from inlining.** With
`fast-exec` the five-stage cascade is never called, so LLVM inlines a different
graph, and memory accesses that previously folded into `pipeline.rs` may now be
charged to `bus.rs`. This document already records one instance of exactly that
(`step_due_here` reading as 63.9% because the whole pipeline inlined into it), so
it is the leading candidate — but it is untested, and *"a hot line is not a hot
operation"* applies to buckets too.

**What would settle it** is a work-unit count rather than a sampled share: RSP
instructions executed and bus accesses serviced, per frame, in both modes.

### Settled: the work is identical, so the shares moved for another reason

Those counters now exist (`work-counters`, `examples/work_bench.rs`). Super
Mario 64, 120 frames after the VI comes up:

| work unit, per frame | accurate | `fast-exec` + `fast-scheduler` | change |
| --- | --- | --- | --- |
| CPU instructions | 1,428,933 | 1,443,787 | **+1.04%** |
| RSP instructions | 294,983 | 294,983 | **0.00%** |
| Bus accesses | 78,103 | 78,111 | **+0.01%** |
| frame mean | 101.048 ms | 63.218 ms | -37.4% |

**The RSP executes the same instruction count to the instruction, and the Bus
services 8 more accesses in 78,000.** Neither subsystem does measurably more
work, so the +12.1% and +13.5% shares are **attribution, not workload** — which
confirms the inlining hypothesis above and retires it as a hypothesis.

**The CPU's +1.04% is the independent check that these counters measure what
they claim.** It reproduces ledger C-16's separately-derived `fast-exec`
divergence exactly, from an unrelated mechanism, and it is also the obvious
explanation for the Bus's 8 extra accesses.

So the two rows may now be read — as *the same work, charged differently*. What
they must still not be read as is "the RSP and Bus got slower". Nothing here
says a subsystem regressed, and the throughput figures say the opposite: RSP
work per millisecond went 2,919 → 4,666 and Bus 773 → 1,236, both ~1.60x, which
is the whole-frame ratio. Every subsystem got uniformly faster.

**A "bus access" is a dispatch, not a CPU request**, and the shape is asymmetric
(pinned in `the_cpu_bus_dispatch_shape_is_pinned`):

| operation | dispatches |
| --- | --- |
| `read_u8` | 1 |
| `read_u32` | **5** — itself, plus four byte reads |
| `write_u8` | 1 |
| `write_u32` | 1 — an RDRAM fast path, no decomposition |
| `write_sized` w4 / w8 | 1 / 2 |

`read_u32` composes an RDRAM word out of four `read_u8` while `write_u32` writes
one directly. **That asymmetry is a lead, not just a caveat**: the Bus is ~18% of
a frame, reads dominate bus traffic, and the read path is paying five dispatches
where the write path pays one.

### The single hottest line is one this change introduced

**`self.dc_wb = latch;` in `Pipeline::execute_one`** (`fastexec.rs`, line 334 at the
time of writing) — **6.83%**. It is the 120-byte `Latch` copy that stages an
instruction into the accurate path's commit machinery so `wb_stage` can be reused
verbatim.

The symbol is named ahead of the line number deliberately: a bare `file:line` in a
document is a claim that decays silently on the next edit, which is the failure mode
this repository keeps rediscovering. Search for the assignment, not the number.

That reuse is deliberate and it is why the two paths cannot disagree about COP0
writes, the TLB instructions, or retirement. Whether the copy is worth 6.8% is a
**separate question from whether that line is hot**: this project has been misled
by precisely this reading once already (the `Latch` refutation in §*Ruled out* 6,
where 16.1% on the latch copies turned out to be retired work charged to a stage's
final store). **The test before any refactor is to make the copy bigger** — pad
`Latch` and see whether the share tracks. If it does not, the line is a parking
spot, not a cost.

### What this means for the optimization plan, which is the point of measuring

**A naming collision to clear up first**, because a reviewer hit it: the items
below belong to the **optimization plan**, not to the project phases in
`docs/STATUS.md`. That file is the single source of truth for project phase state
and records its Phase 2 (RSP microcode) and Phase 4 (AI audio) as **complete**;
those are different work items that happened to share numbers with the plan's. The
plan's items are named rather than numbered from here on.

**The deficit-counter scheduler is undercut as a throughput play.**
`scheduler.rs` is now **5.05%**; eliminating it entirely buys 1.05x. That is not
what the plan projected, because the plan was sized against the 9.26% share.

**But the share bounds only the code it names.** `step_rcp` dispatches into the
RSP, RDP, AI, PI and VI on *every* RCP edge, and that cost is charged to those
buckets rather than to `scheduler.rs`. Coarse work units would remove visits, not
just dispatch arithmetic. So it is **not** refuted — what is refuted is sizing
it from `scheduler.rs`. The measurement it needs first is *how much of the RSP
bucket is a step that had nothing to do*.

**The RSP is now the largest single remaining bucket at 21.4%**, ahead of Bus at
18.1%, which makes the plan's RSP work (a pre-decoded threaded interpreter) the
larger lever of the two. On this evidence the plan's ordering is worth revisiting
before either is begun — a maintainer's call, recorded here rather than taken.

## The RSP's idle steps are ~38% of the render phase and bounded at 0.45%

The profile above ended with a question: *how much of the RSP bucket is a step that
had nothing to do?* It is the measurement the deficit-counter scheduler has to be
sized against, because that design's value is removing **visits**, and a visit is
only worth removing if it costs something.

Answer: **~38% of render-phase steps are idle, and removing them entirely is worth
0.45% on the window most favorable to it** — roughly half that where it would
actually run. The visits are nearly free; there is no reservoir there.

### How many steps are idle

Scratch instrumentation in `Bus::rsp_tick` counting halted versus executing steps
over the 900-frame `gameplay_phase_probe` run. The table below is in **RSP steps**
and the run is in **frames**; the two connect at roughly **1.0 M RCP steps per
frame** (900 M steps over 900 frames), which is what makes the 100 M-step intervals
read as ~100 frames each. The **cumulative** share is
misleading and is the reason this is tabled per interval rather than quoted as one
number — it starts above 80% and falls throughout, because boot is mostly a
halted RSP and the render phase is not:

| steps (millions) | idle in interval | share |
| --- | --- | --- |
| 0-100 | 82,856,525 | 82.86% |
| 100-200 | 70,561,600 | 70.56% |
| 200-300 | 90,565,572 | 90.57% |
| 300-400 | 64,101,175 | 64.10% |
| 400-500 | 46,854,407 | 46.85% |
| 500-600 | 40,118,503 | 40.12% |
| 600-700 | 40,948,739 | 40.95% |
| 700-800 | 38,661,898 | 38.66% |
| 800-900 | 37,707,875 | 37.71% |

The last four intervals sit at **37.7-41.0%** and are flat, which is what says
that is the steady state rather than a point on a curve. **Quoting the cumulative
56.9% would have overstated it by half**, and quoting the first interval's 82.9%
by more than double.

### Whether removing them is worth anything: 0.45% at best, and less in practice

`Rsp::su_step` already returns immediately when halted. What a caller could still
skip is the wrapper: `Bus::rsp_tick`'s call, the `StepResult::default()` it
constructs, the three `Option` tests on the result, and the step counter. So:

```rust
pub fn rsp_tick(&mut self) {
    if self.rsp.halted() {                                  // the experiment
        self.rcp_steps = self.rcp_steps.wrapping_add(1);
        return;
    }
    let out = self.rsp.tick();
    if let Some(raise) = out.interrupt_change { /* ... */ }  // skipped
    if let Some(dma) = out.dma { /* ... */ }                // skipped
    if let Some((off, val)) = out.dp_write { /* ... */ }    // skipped
    self.rcp_steps = self.rcp_steps.wrapping_add(1);
}
```

**This was first measured on the wrong harness, and the first answer was wrong.**
`gameplay_phase_probe` gave A 63.6/63.5, B 65.8/64.3, A 63.5/65.1 — overlapping
legs, recorded as *neutral*. A reviewer pointed out that the probe's spread (2.5%
within a leg) is wider than the effect being looked for, and that
`examples/frame_bench.rs` has a ~1% floor. That is correct, and re-running it
**reversed the result**:

| leg | `frame_bench`, ms/frame |
| --- | --- |
| A (unmodified) | 64.231, 64.392, 64.262, 64.401, 65.407 |
| B (early-out) | 63.818, 63.896, 63.927, 63.941, ~~71.507~~ |

**One reading is excluded and named**: B's 71.507 ms is 11.8% above the rest of its
own leg, which is contamination rather than variance — something else was running.
Every other reading is kept, including A's 65.407, which is only 1.8% high and has
no such excuse.

On that data the legs **do not overlap**: A's minimum (64.231) sits above B's
maximum (63.941). Conservatively, **64.231 -> 63.941 ms = 0.45%**.

**What that does and does not establish.** The retained legs differ by 0.45% and do
not overlap; that is an *observation*, not a demonstrated causal improvement. Five
readings a side, an effect within a factor of two of the harness's own floor, and
one excluded sample do not carry that weight. What it does establish is an **upper
bound**: whatever removing idle visits is worth, it is not more than this. Two
reasons the true figure is lower still:

- `frame_bench`'s window is **early boot**, where 80%+ of RSP steps are halted (the
  first rows of the table above). The render phase runs at 38%, so expect roughly
  half of 0.45% there.
- It is within a factor of two of this harness's own noise floor, which is why five
  readings per leg were needed to see it at all.

**Not landed**, on the size of the effect alone: an upper bound of 0.45% measured on
the most favorable window, against a reason for reaching for it — that idle visits
are expensive — which this is the evidence against. The value here is the bound, not
the patch.

(An earlier draft also claimed the change breaks `rcp_steps_for_test`. **That was
wrong** — the experimental patch increments `rcp_steps` on the halted return, so the
count is preserved and no test is affected. A reason invented to reinforce a
conclusion already reached on other grounds; caught in review, and removed rather
than quietly dropped.)

### What that does to the deficit-counter scheduler's case

Its two justifications were sized separately, and both are now measured:

1. **`scheduler.rs` per-edge dispatch arithmetic — 5.05%.** Eliminating it entirely
   buys 1.05x.
2. **Per-edge chip visits, whose cost is charged to the chip buckets —
   measured here as ~nothing for the RSP**, the largest of them.

That is the second justification failing on its largest single case. It is not
proof for the RDP, AI, PI or VI, whose visits were not measured this way — but the
RSP was where the argument was strongest, since it is 21.4% of the frame and 38%
of its steps do nothing.

**What that supports, stated no more strongly than the evidence allows:** the
*halted-visit overhead* inside the RSP bucket is small, so the bucket is
predominantly work done while the RSP is running — microcode execution and the
wrapper around it. This experiment touched **one** wrapper path; it does not
decompose the running 62% into dispatch versus arithmetic versus register access,
and it says nothing at all about the RDP, AI, PI or VI, whose visits were never
measured this way.

On that reading the lever likely to reach the bucket is a faster interpreter (the
plan's pre-decoded threaded design) rather than fewer visits — but *likely* is the
right word until the running share is decomposed, which is the next measurement
rather than a conclusion of this one.

### Ruled out

**Do not re-try the `rsp_tick` halted early-out.** Bounded at **0.45%** on the window
most favorable to it, and about half that where it would actually run — an upper
bound near the harness floor, not a demonstrated win.
The analogous change *was* a win for the RDP and the AI (#219/#221) because those
avoid a `core::mem::take` of a large struct; the RSP's wrapper has no such
payload, so the pattern does not transfer. Matching the shape of a past win is not
evidence.

## The pre-decoded threaded interpreter, sized before building it

The plan's RSP item is gopher64's shape: a pre-decoded table with one entry per
IMEM word, a function pointer per entry, re-decoded on IMEM invalidation. It is
~3,400 lines of work, so it was sized first. The two halves are **pre-decoding**
(never decode the same word twice) and **threading** (a function-pointer table
instead of an opcode `match`).

### Pre-decoding is worth ~0.29% of a frame

Sized by **making the cost bigger**, the test this document already prescribes for
a hot line: three extra `decode` calls per RSP step, and see what the frame does.

| | ms/frame |
| --- | --- |
| baseline | 64.231, 64.392, 64.262, 64.401 |
| +3 decodes per step | 64.958, 65.404, 65.573 |

Conservatively `64.401 -> 64.958 = +0.86%` for three, so **one decode is ~0.29% of a
frame** and a perfect decode cache saves that much. `Rsp::decode` is eight
bit-field extractions in a `const fn`; there was never much there.

**The first version of this experiment understated it by roughly 4x** and would
have been reported as ~0.2%. It consumed the extra decodes with `acc.op ^= e.op &
0`, and `& 0` is a no-op the optimizer folds — taking the decodes with it. Only
`black_box` on the **result** keeps them. A `black_box` on the input is not enough,
because an unused result is still dead.

### The threading half was NOT measured, and saying so is the point

Two attempts to size the opcode dispatch the same way both failed: a duplicate
`match d.op` with trivial arms (`n => n + 1`) is recognized by LLVM as arithmetic
and never becomes a jump table, so it measured **at or below** baseline — a number
that means "the experiment did not run", not "dispatch is free". It is recorded as
**unmeasured**.

What can be said without measuring is structural, and is reasoning rather than
evidence: `match d.op` over a dense `0..63` lowers to a jump table, which is one
indirect branch; a `[fn; 64]` table indexed by opcode is also one indirect branch,
at the same site. The thing that makes a *true* threaded interpreter fast is a
separate dispatch at the end of **each handler**, giving the predictor one site per
opcode — and that needs guaranteed tail calls, which stable Rust does not have.
gopher64 dispatches from a loop, so its win is the pre-decoding, which is the half
measured above.

### Recommendation: do not build it, and the RSP's target is the vector unit

On the evidence: the measurable half is worth **0.29%**, and the other half has no
mechanism in stable Rust to be better than what is already emitted. That does not
justify ~3,400 lines and a second RSP execution surface.

Where the RSP's 21.4% actually sits, by file:

| file | % of frame | % of the RSP bucket |
| --- | --- | --- |
| `su.rs` (scalar unit) | 10.38% | 48.5% |
| `vu.rs` (vector unit) | 8.54% | 39.9% |
| `sp.rs` (DMA, status, halt) | 2.00% | 9.3% |
| `lib.rs` | 0.50% | 2.3% |

**`vu.rs` is 8.5% of the whole frame**, and it is the one part of the RSP whose work
is inherently data-parallel — eight 16-bit lanes per operation, which is what SIMD
is for. That is the plan's *other* RSP idea and it is the one this measurement
supports. It carries its own question (`std::arch` needs `unsafe`, which
`rustyn64-rsp` forbids; `core::simd` is unstable), which is a design decision
rather than a measurement.

### Ruled out

**Do not build the pre-decoded threaded interpreter for the RSP** on the current
evidence. Pre-decoding measured at 0.29%; threading unmeasured but structurally
equivalent to the existing dispatch in stable Rust. Re-open it only if guaranteed
tail calls stabilize, or if a decomposition of `su.rs`'s 10.38% finds the cost in
dispatch after all — which the two failed experiments above did **not** establish
either way.

## The SIMD question is premature: the VU is not vectorized, and the blocker is shape

The plan asks where `unsafe` may live so the RSP's vector unit can use SIMD
(`std::arch` needs it; `rustyn64-rsp` carries `#![forbid(unsafe_code)]`;
`core::simd` is unstable). **That is the wrong question to answer first.** The VU is
not vectorized today, and the reason is the shape of the code rather than the
tools available to it.

### Evidence: 30 sixteen-bit-lane instructions in the entire shipped binary

Counted in the disassembly of `target/release/examples/frame_bench` (fat LTO,
whole program):

| form | count |
| --- | --- |
| `vpaddw`, `vpsubw`, `vpmullw` | 6, 1, 2 |
| `vpsllw`, `vpsrlw` | 7, 5 |
| `vpackssdw`, `vpunpcklwd` | 7, 2 |
| `vpmulhw`, `vpcmpgtw`, `vpcmpeqw`, `vpsraw`, `vpminsw`, `vpmaxsw` | 0 |
| **total 16-bit-lane arithmetic** | **30** |

The VU has ~80 opcode arms, each operating on **eight `u16` lanes**. A vectorized
VU would show dozens of these per family; thirty in the whole binary is incidental
— plausibly the RDP's color math or the VI's filter, not the RSP.

The binary is **not** SIMD-free in general: it carries 95 `vpand`, 85 `vpor`, 76
`vpxor`, 36 `vpaddq`, 29 `vpaddd` and so on. LLVM vectorizes this codebase where it
can. It does not do so for the thing most obviously shaped like a vector.

**A first version of this check was misleading and is recorded rather than
discarded.** Emitting assembly for `rustyn64-rsp` alone (`cargo rustc --emit=asm`)
showed **zero** SIMD instructions of any kind, which is a stronger claim — and not a
valid one, because that build has no LTO and none of the inlining the shipped
binary does. The conclusion survived; the evidence for it had to be replaced.

### Why: the opcode dispatch is inside the lane loop

`Rsp::vu_compute` is shaped like this:

```rust
for lane in 0..8 {
    let out = match op {                       // <- ~12 arms here
        0x00..=0x0F => self.multiply_lane(op, lane, ..),   // <- ~10 more arms inside
        0x10 | 0x11 => { ..self.vu_ctrl.vco.. }
        ..
    };
    ..
}
```

Two properties, either of which alone defeats the vectorizer:

- **The dispatch is per lane.** A multiply runs the opcode match **sixteen** times
  per instruction — eight outer, eight inner. There is no straight-line body of
  eight identical operations for LLVM to widen.
- **Each iteration touches `&mut self`**: `set_acc_low(lane, ..)`, `self.vu_ctrl.vco`,
  `self.vu_regs[..][lane]`. Cross-lane state read and written inside the loop is
  what alias analysis cannot prove independent.

### What that means for the decision

**The `unsafe` question does not need answering yet.** The prerequisite is a
restructure that needs neither `unsafe` nor an unstable feature: hoist the dispatch
**out** of the lane loop, so each opcode family gets a tight eight-iteration body
over plain arrays. That is the shape LLVM autovectorizes on its own.

Whether it actually does is **empirical and unmeasured**, and would want one
experiment on one family before the other ~80 arms are touched.

**And there is no gate to run it against, which has to be fixed first.** The RSP
category did reach `Failed: 0` at v0.3.0 (Phase 2's cut criterion), but the
committed runner does **not** assert it:
`crates/rustyn64-test-harness/tests/systemtest.rs` excludes `"RSP"` and `"SP "`
through its `LATER_PHASES` list, so `phase_1_categories_report_no_failures` would
stay green through an arbitrarily broken vector unit. An earlier draft of this
section cited that suite as the refactor's gate; **it is not one.** Caught in
review.

So the first slice of any VU work is an **RSP-scoped assertion** of the same shape
as the Phase 1 one — named ROM, `Failed: 0` acceptance, and a witness that the
category actually ran, since an empty run reports zero failures just as
convincingly. Building the gate before the thing it grades is a rule this project
already has, and this is a case where it was about to be skipped.

**If the restructure does not vectorize**, the question returns — and the plan's
recorded answer is wrong for this case. Moving the VU into a new crate that permits
`unsafe` is not available: the VU's state (`vu_regs`, `vu_acc`, `vu_ctrl`) lives in
`Rsp`, and splitting it out is the chip-to-chip dependency the crate graph forbids
(`docs/architecture.md`). The realistic options would be `core::simd` behind a
nightly-only feature, or a narrowly scoped `unsafe` exception for `rustyn64-rsp`
— an ADR-level decision either way, and not one this measurement makes.

## The VU family hoist: built, accurate, and measured NEUTRAL

The experiment the SIMD section asked for. It was run against the RSP gate that
now exists, and the result **reverts** under this document's own rule.

### Which family, measured rather than guessed

An opcode histogram in `Rsp::vu_compute`, Super Mario 64, 900 frames — **125 M VU
compute ops, ~139 k per frame**:

| op | share | |
| --- | --- | --- |
| `0x0e` | 17.8% | multiply family |
| `0x0f` | 15.2% | multiply family |
| `0x0d` | 8.7% | multiply family |
| `0x10` | 8.7% | `VADD` |
| `0x04` | 7.9% | multiply family |
| `0x00` | 6.0% | multiply family |
| `0x06` | 5.3% | multiply family |
| `0x11` | 4.4% | `VSUB` |
| `0x23` | 3.4% | |
| `0x15` | 3.1% | `VSUBC` |

**The multiply family (`0x00..=0x0F`) is ~61% of every VU compute op** — and it is
the one that dispatched **twice** per lane, once on the outer match and again
inside `multiply_lane`, so the opcode was decoded **sixteen times per
instruction**. If dispatch-in-the-loop were the constraint, this is where it would
show.

### The change, and that it is correct

A dedicated eight-lane loop for `op <= 0x0F`, removing eight of the sixteen
dispatches and leaving LLVM a loop body with one call rather than a sixty-way
branch. It returns directly: the multiplies write no `VCC`, no `VCO`, and fall
outside the `0x28..=0x2D` accumulator range, so none of the shared tail applies.

**Accuracy held**, on the gate added for exactly this:

```text
RSP categories: 0 failing across 224 RSP tests started
(suite ran to xioctl(EXIT); 90 failing suite-wide across 950 started).
```

### And it is worth nothing

Six readings a side, `frame_bench`, `--features fast-exec`, **one sitting, in the
order A x3 -> B x3 -> B x3 -> A x3** (the two B blocks are contiguous because the
rebuild between configurations is what costs time, not the runs). Host, OS,
toolchain, profile and ROM hash are the ones recorded once in §Method; revision is
`main` at the RSP-gate commit. Command:

```bash
RUSTYN64_PROBE_ROM="$ROM" cargo run --release --example frame_bench \
  --features fast-exec
```

| leg | ms/frame |
| --- | --- |
| A (baseline) | 64.114, 64.303, 64.183, 64.110, 63.744, 63.899 |
| B (hoisted) | 63.840, 64.182, 64.040, 63.473, 63.775, 63.517 |

| | A | B |
| --- | --- | --- |
| range | 63.744 — 64.303 | 63.473 — 64.182 |
| mean | 64.059 | 63.804 |

**The ranges overlap heavily.** The means differ by 0.40% in the hoist's favor,
while the conservative pairing (best A against worst B) says it is **0.68%
slower**. When the sign of the result depends on which pairing you quote, the
result is **neutral** — the same standard applied to the `rsp_tick` early-out
above, and the same conclusion.

**Reverted**, per this document's standing rule: *revert anything neutral or
worse*, as was done for the `next_edge` hoist, the `vi_divot` reorder,
`target-cpu=native` and PGO.

### What it settles

This was the strongest available test of *"the per-lane dispatch is what stops the
VU going fast"*, run on the family that is 61% of the work and carries double the
dispatch. It moved nothing.

**What is established**, and no more than this: hoisting the **multiply family's**
dispatch out of the lane loop is neutral on **this workload** (Super Mario 64's
render phase). Taken with the decode sizing above (0.29% for a perfect decode
cache), two of the three things a pre-decoded threaded interpreter would do have
now been measured, and both are worth about nothing here.

**What is hypothesis, and is recorded as such** — each needing its own measurement
before it is acted on:

- That the VU's 8.54% is *predominantly the arithmetic itself*. Plausible after
  two negative results, but neither experiment decomposed the per-lane body.
- That **no** interpreter restructuring helps. Not established: one family was
  hoisted, not all of them, and a full per-opcode specialization (which also
  hoists `multiply_lane`'s inner match) is a different change from this one.
- That the remaining families can only measure smaller. They are smaller in
  frequency and singly dispatched, which is a reason to expect it — not a
  measurement of it.
- That SIMD is what remains. It is the obvious candidate given the hardware is
  eight `u16` lanes, and it is untried.

What this **does** do is remove the cheapest of those candidates from the front of
the queue. The `unsafe`/nightly decision (§*The SIMD question*) is no longer
deferrable behind "restructure first, it is free" — because the free restructure
was tried on its best case and did nothing. That decision remains ADR-level and
this measurement does not make it.

### Ruled out

**Do not hoist VU opcode families out of the lane loop for performance without a
new reason.** Measured neutral on the largest family (61% of ops, double
dispatch), with accuracy verified. The remaining families are smaller in frequency
and singly dispatched, which is a reason to *expect* a smaller effect rather than a
measurement of one — so this rules out repeating the same experiment, not the
different change of specializing `multiply_lane` per opcode.

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

## The GPU backend staged all 8 MiB of RDRAM every frame — 2.54% of a frame

The `gpu-rdp` path copied and byte-swapped **the whole of RDRAM into the GPU's
own RDRAM every frame**, unconditionally, because it had no way to know what had
changed. Measured earlier at ~2.1 ms of the ~2.4 ms a GPU frame cost.

`Bus` now carries a per-page dirty map (`rdp-tap`, 4 KiB pages, 2,048 flags for
8 MiB) marked at all six RDRAM write sites, and the frontend stages only the
pages something wrote.

**How much of RDRAM a real frame actually touches**, which is what makes the
result interpretable rather than just favorable — Super Mario 64, measured:

| | pages of 2,048 | % of RDRAM |
| --- | --- | --- |
| busiest frames | 138–139 | 6.8% |
| alternating frames | 27–28 | 1.3% |

So the stage sends ~0.1–0.5 MB instead of 8 MB.

### A-B-A, `frame_bench --features gpu-rdp`, Super Mario 64

| leg | full stage | dirty pages |
| --- | --- | --- |
| 1 | 96.725 | 93.828 |
| 2 | 97.003 | 94.098 |
| 3 | *109.564* | 94.229 |
| 4 | 97.315 | 94.064 |
| 5 | 96.687 | — |

**Conservative pairing (worst B 94.229 against best A 96.687): 2.54%, 1.026x.**
The four clean A legs span 0.65% and the B legs 0.43%, and the two sets **do not
overlap**.

**Leg A3 (109.564 ms) is excluded and named rather than dropped silently.** It is
13% off the other four A legs, which agree within 0.65% — that is not the ~1%
this harness drifts by, so something outside the measurement happened. Four
clean A legs is enough to pair against; three would not have been.

**The mechanism predicts the result**, which is the check that matters more than
the spread: removing a ~2.1 ms stage from a ~97 ms frame is 2.16%, and 2.54% was
measured. Agreement at that level says the saving is the stage and not something
else moving.

### Two harness corrections this forced

- **`examples/frame_bench.rs` does not run on its default ROM.** The committed
  `render_fill.z64` never brings the VI up inside `MAX_WARM = 300` frames, so the
  harness aborts on its own liveness assertion. It works on a commercial ROM —
  Super Mario 64 comes up at `warm=36`, exactly the figure in the file's own
  comment. Every number here was taken with `RUSTYN64_PROBE_ROM` set. The default
  is not a working default and should be fixed or replaced.
- **`tests/gpu_present_cost.rs` measures a floor, not a frame.** It never runs
  the machine, so nothing is ever dirty and the staging is skipped entirely —
  which is why its number fell from ~0.78 ms to ~0.45 ms on this change alone.
  Its module doc now says so and points here. A number in a test gets quoted.

### The GPU tests need `--test-threads=1`

The backend is one Vulkan device **per thread** (`GpuRdp` is `!Send`), so a
parallel test harness creates several and their thread-local destructors race
`vkDestroyDevice` against another thread's creation. That produced a **SIGSEGV
once in roughly five runs** — intermittent, so it would have arrived as CI flake
rather than as a reproducible failure. CI now serializes them. Production is
unaffected: exactly one thread ever calls `present`.

## The whole GPU present path is 4.0–4.4% of a frame — which retires the shared-device plan

The plan for the GPU backend led with **sharing one Vulkan device with wgpu**, on
the grounds that the frame crosses PCIe twice in opposite directions: read back
from parallel-rdp's device to the host, then uploaded to wgpu's unrelated one.
That is a true description of the code. It was never sized.

`crates/rustyn64-frontend/examples/gpu_phase_bench.rs` sizes it. It splits
`gpu_rdp::present` into the four phases that different planned work would remove,
and reports each as a share of a real frame:

| phase | what it is | removed by |
| --- | --- | --- |
| `stage` | byte-swap the dirty RDRAM pages into the backend | already done (#245) |
| `submit` | feed the command stream and VI registers | nothing planned |
| `scanout` | wait on the GPU fence, then read the frame back | async RDP (the wait), shared device (the read-back) |
| `copy` | copy the read-back pixels into the caller's buffer | shared device |

### Super Mario 64, `--features gpu-rdp,fast-exec,fast-scheduler`

| run | frame mean | stage | submit | scanout | copy | **total** |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | 63.645 ms | 0.262% | 0.575% | 3.442% | 0.147% | **4.427%** |
| 2 | 62.392 ms | 0.224% | 0.549% | 3.098% | 0.144% | **4.015%** |
| 3 | *72.122 ms* | *0.356%* | *0.825%* | *4.211%* | *0.242%* | *5.634%* |

**Run 3 is excluded and named rather than dropped silently.** Its frame mean is
15% above runs 1 and 2, which agree within 2% — that is not this harness's ~1%
drift, so something outside the measurement happened.

**If the entire present path became free, the ceiling is 1.042–1.046x.** That is
every remaining GPU-side idea combined — shared device, asynchronous RDP, the
lot.

### Splitting `scanout`, because two different plans target its two halves

`scanout` bundles *waiting for the GPU to finish rasterizing* with *transferring
the result back*. An asynchronous RDP removes the first; a shared device removes
the second. A scratch probe calling `idle()` immediately before `scanout_sync`
and charging that wait to `submit` split them:

| | ms/frame |
| --- | --- |
| wait for RDP rasterization to complete | ~1.06 |
| VI scan-out pass + read-back + the two host memcpys | ~1.39 |

So **the read-back is at most the smaller half of the larger phase**, and the
host copies are `copy`'s 0.15%.

### The conclusion, and it inverts the plan's order

**Sharing a Vulkan device with wgpu is not worth building.** Its unique
contribution — the read-back and the host copies, *not* the GPU's rasterization
time — is on the order of **1% of a frame at the most generous reading, and
0.15% at the most defensible one**. Against that:

- It is the **highest-cost slice in the GPU plan**: cross-thread Vulkan device
  sharing, raw `wgpu-hal` interop under `unsafe`, raising the device limits
  (`gfx.rs` currently requests `downlevel_webgl2_defaults` on a native Vulkan
  device), and resolving device ownership between the emulation thread and the
  winit thread — `GpuRdp` is `!Send` and lives in thread-local storage precisely
  to avoid that question.
- Half of the "double crossing" it targets was **never on the frame budget**.
  The upload to wgpu happens on the winit thread, concurrently with emulation on
  the emu thread. Removing work that overlaps with the bottleneck does not make
  the bottleneck shorter.
- One other GPU item *appeared* to be worth **6.36%** (retiring the software
  rasterizer) — roughly **6x** the return, without needing a shared device at
  all. **That figure did not survive being measured either**; see "Retiring the
  software rasterizer is worth 1.2–3.2%" below, which sizes it at **1.23%**
  conservatively. It is left here as written because it is what this comparison
  rested on at the time, and correcting it silently would hide that the
  conclusion above was reached against a number that later moved.

**A correction to the first version of this section**, which claimed *two* items
worth 11.0% by adding a GPU VI scan-out's 4.64% to that 6.36%. **The VI figure
was already spent.** `EmuCore::produce_frame` returns as soon as the GPU
produces a picture, *before* `Bus::scanout_scaled` — so under `gpu-rdp` the
software VI scan-out does not run at all, and moving the VI to the GPU cannot
recover a cost that is not being paid. The 4.64% came from a profile of a
configuration that does not have this backend, and quoting it here was reading a
figure from one configuration into another. Measured below rather than argued.

The general lesson is the one this file keeps re-learning: *a mechanism that is
real is not thereby worth removing*. "The frame crosses PCIe twice" was an
accurate description of the code and a poor guide to where the time was.

### On the instrumentation itself

The phase counters are eight `Instant::now()` calls per frame against a ~63 ms
frame, and the frame means above sit within the documented baseline, so no
regression is visible. They are kept rather than reverted because they are the
gate for the remaining GPU work: an asynchronous RDP and a GPU VI scan-out both
have to show up in `scanout`, and a change that does not move the phase it
claims to move has not done what it says.

## The GPU display backend is 2.1% faster than the software path

Nobody had measured this. `docs/rdp.md` and the notes around #243 quoted
*0.72–0.93 ms against software's 0.75 ms* — a near-tie — but those came from
`tests/gpu_present_cost.rs`, which never runs the machine (see the correction
above), and they compared the **present call** rather than the **frame**.

The frame is what matters, because the two paths do not do the same work: the
GPU path pays a present (4.0–4.4%) and in exchange **skips the software VI
scan-out entirely** — `produce_frame` returns before `scanout_scaled` whenever
the GPU produced a picture.

### A-B-A, `examples/frame_bench.rs`, Super Mario 64, `fast-exec,fast-scheduler`

The same example built both ways, so this is one binary shape with one feature
changed, not two different harnesses.

| A — software | B — `gpu-rdp` |
| --- | --- |
| 66.551 | 62.863 |
| *82.971* | 62.533 |
| 64.378 | 62.841 |
| 64.735 | 62.970 |
| 64.977 | *66.856* |
| 64.507 | 62.452 |
| 64.314 | — |
| 64.694 | — |

**Conservative pairing (worst clean B 62.970 against best A 64.314): 2.09%,
1.021x.** The clean B legs span 0.8% and the clean A legs 3.5%, and the two sets
**do not overlap** — every B leg is faster than every A leg.

Two legs are excluded and named rather than dropped silently: A's 82.971 (25%
off) and B's 66.856 (6% off). Both are far outside this harness's ~1% drift.

**The mechanism accounts for the result**, and the arithmetic below names which
statistic each number comes from — two different ones are defensible here and
they do not agree, so quoting a figure without its basis would be picking the
flattering one by accident.

| basis | A | B | difference |
| --- | --- | --- | --- |
| mean of the clean legs | 64.879 | 62.732 | **2.15 ms** |
| conservative pairing (best A, worst clean B) | 64.314 | 62.970 | **1.34 ms** |

The present path itself measures **2.55 ms** on this configuration. Since the
GPU path pays that and still finishes ahead, the software VI scan-out it skips
is worth the sum:

- **4.70 ms** on the mean basis (2.55 + 2.15), ~7.2% of a frame;
- **3.89 ms** on the conservative basis (2.55 + 1.34), ~6.0% of a frame.

Both are **inferred** from the difference, not measured directly — the direct
measurement would be timing `scanout_scaled` itself, which nothing here does.
They bracket the 4.64% the fast-exec profile attributes to the VI, which is the
agreement worth having: the two are arrived at by unrelated methods (wall clock
against sampled attribution), and the standing caution that *a profile share
bounds only the code it names* is why they were not expected to match exactly.

### What this settles

- **The GPU backend is a speedup, not a wash.** It was adopted for accuracy and
  for retiring the software rasterizer's remaining gaps; that it also wins on
  frame time was assumed and is now measured.
- **A GPU VI scan-out recovers nothing here**, because the software VI is
  already skipped. It remains interesting for *accuracy* — parallel-rdp's VI is
  a different implementation and the geometry already differs — but it is not a
  performance item, and the VI parity census should be built for that reason or
  not at all.
- **Retiring the software rasterizer is the only remaining GPU-side
  performance item**, and unlike the VI it is genuinely unspent: the software
  RDP still executes every command, because games read the framebuffer back out
  of RDRAM and only the software path writes it there. **Sized at 6.36% here and
  since measured at 1.23%** — see the section below; that figure came from the
  same profile-of-another-configuration mistake as the VI's.

## Retiring the software rasterizer is worth 1.2–3.2%, not 6.36%

The GPU plan's last large performance item was **A4**: stop running the software
rasterizer on the render path, and have the GPU write the framebuffer back into
RDRAM instead — via parallel-rdp's own `CoherencyOperation` / `masked_memcpy`
(upstream symbols in `vendor/parallel-rdp-standalone`, not RustyN64 APIs;
nothing in this tree calls them today). It was sized at
**6.36%** — the RDP bucket in the `fast-exec` profile.

**That figure is from a configuration without this backend**, which is the same
mistake the VI figure above turned out to be. Measured in the configuration A4
would actually change:

### Upper bound, `frame_bench --features fast-exec,fast-scheduler,gpu-rdp`

The three rasterizing dispatch arms (`OP_FILL_RECTANGLE`,
`OP_TEXTURE_RECTANGLE{,_FLIP}`, the `0x08..=0x0F` triangles) were stubbed to
no-ops in a scratch tree, leaving every state-setting arm and the command
consumption intact. Super Mario 64:

Frame means, milliseconds:

| A — rasterizer present (ms) | B — rasterization skipped (ms) |
| --- | --- |
| 61.716 | 60.635 |
| 61.388 | 59.409 |
| 61.623 | 59.439 |
| 61.506 | 58.968 |

**The sets do not overlap** — every B leg beats every A leg — so the effect is
real. But it is small:

| basis | difference |
| --- | --- |
| conservative (worst B 60.635 vs best A 61.388) | **1.23%**, 0.753 ms |
| clean-leg means (61.558 vs 59.613) | **3.16%**, 1.946 ms |

The B legs span 2.83% against A's 0.53%, which is why the conservative pairing
is the number to quote.

### Why this is an upper bound A4 cannot reach

Deleting the rasterization is **strictly cheaper than replacing it**. A4 does
not remove that work, it moves it: the GPU must write its result back into the
Bus's RDRAM every frame, and that write-back costs something this measurement
gives away for free. The real figure is below 1.23%.

### And the cost side is the highest in the plan

Unlike everything else in Part A, A4 **changes what lands in RDRAM** — the
machine's own state, not just what is presented:

- **ADR 0004 comes into scope**, exactly as ADR 0015 predicted it would. The
  determinism contract binds the core, and the core's framebuffer would start
  coming from a GPU.
- **The two rasterizers are not byte-identical.** The 42/43 census grades
  parallel-rdp against *Angrylion*, not against RustyN64's software path, and
  the one known gap — `key_en` chroma-key alpha compare (#160) — is a case where
  the GPU is **less** complete than the software rasterizer it would replace.
- **Timing changes.** The software RDP executes commands as the machine runs;
  the GPU renders at frame end. A game that reads its framebuffer mid-frame sees
  a different picture.
- It needs `rustyn64-core` to accept GPU-written RDRAM, against a crate graph
  that is `#![no_std]` and `#![forbid(unsafe_code)]` by design.

**A4 is not worth building for performance.** Paying an ADR, a determinism
re-derivation, and a known accuracy regression for under 1.23% is the trade this
document exists to refuse. It may still be worth building for **accuracy** one
day — the software rasterizer is incomplete — but that is a different
justification and it should be argued on its own terms, with the census as the
gate rather than the frame time.

### Where that leaves Part A

| item | outcome |
| --- | --- |
| dirty-region RDRAM upload | **shipped**, 2.54% |
| share one Vulkan device | retired — ~0.15% at the highest cost |
| async RDP | open, bounded by the ~1.06 ms rasterization wait (~1.7%) |
| retire the software rasterizer | retired for performance — under 1.23% |
| GPU VI scan-out | retired — recovers nothing; accuracy question only |
| upscaling | quality-only by design; consumes idle GPU time, no FPS effect |

**Everything the GPU can still offer totals under about 2%.** The remaining
89% — CPU, RSP, Bus — is where the frame actually is, and none of it is work a
GPU can do.

## The read path paid five bus dispatches where the write path paid one — 1.12%

The work-unit census above pinned an asymmetry: a word read of RDRAM cost
**five** bus dispatches (`read_u32` itself, plus four `read_u8`) while a word
write cost **one**, because `write_u32` had an RDRAM fast path and `read_u32`
did not.

That is a lead a *request*-count metric would have hidden entirely, by reporting
both as 1.

### The change

`read_u32` gains the symmetric fast path, as a single range lookup:

```rust
Self::rdram_offset(addr)
    .and_then(|off| self.rdram.get(off..off + 4))
    .and_then(|s| <[u8; 4]>::try_from(s).ok())
```

One bounds check rather than four, and no manual index arithmetic to reason
about. Safe to skip `read_u8` for this range because its RDRAM arm is a pure
`self.rdram[off]` with no side effect — unlike its PI arm, which folds in
`IOBUSY`. A fast path over a side-effecting read is how the `SP_SEMAPHORE` bug
in this same function happened, and that is why the whole SP register block is
handled before any byte composition.

### A-C-A-C, `frame_bench --features fast-exec,fast-scheduler`, Super Mario 64

Interleaved rather than run in two blocks, so a monotonic drift over the session
cannot be mistaken for the effect. Frame means, milliseconds:

| A — byte composition (ms) | C — fast path (ms) |
| --- | --- |
| 63.826 | 62.861 |
| 63.845 | 62.931 |
| 64.069 | 62.672 |
| 63.773 | 62.749 |
| 64.040 | 62.713 |

**Conservative pairing (worst C 62.931 against best A 63.773): 1.32%, 1.013x.**
On the means it is 1.76%. The A legs span 0.46% and the C legs 0.41%, and **the
two sets do not overlap** — every C leg is faster than every A leg.

An earlier form using four separate index reads measured 1.12% conservatively.
The slice form is at least as fast and is better code; the difference between
the two is not distinguishable from this harness's drift, and is not claimed.

**Accuracy did not move**: `n64-systemtest` reports Phase 1 `0 failing` and RSP
`0 failing`, 90 suite-wide, identical to before.

### What the guards do and do not cover

`the_fast_path_agrees_with_byte_composition` compares the two implementations
directly over misaligned addresses, a 4 KiB page straddle, and the last words of
RDRAM. Mutation-checked: flipping the byte order fails it, and replacing
`.get(off..off + 4)` with an unchecked index panics on the end-of-RDRAM cases —
the partial-word rejection is the whole reason for the range form.

**MMIO is protected twice over, and no SINGLE mutation reveals it.** That took
three attempts to establish and is worth recording in full:

| mutation | result |
| --- | --- |
| hoist the fast path above every register branch | passes |
| widen `rdram_offset` to accept every address | passes |
| **both together** | **fails** |

The register branches run first **and** `rdram_offset` returns `None` outside
the 8 MiB window. Either alone is sufficient, so breaking one leaves the other
holding.

Two corrections came out of that. The first version of the code comment and the
test doc claimed the *placement* is what keeps MMIO safe — it is one of two
protections, not the one under test. And an intermediate mutation that "passed"
turned out to be **too narrow to reach any register block** (`RDRAM_SIZE * 8` is
64 MiB; the blocks start at `$0400_0000`), which would have been recorded as
evidence of a property that had simply not been exercised. A mutation that does
not fire proves nothing until you have checked it could have.

## Which VU operations actually run — 62% of the work is one dispatch function

`vu.rs` is **143 functions and ~8.5% of a frame**, and vectorizing it means
writing `unsafe` intrinsics. Hand-vectorizing 143 functions to recover the cost
of the few that matter would be the expensive way round, so the same method that
worked for the Bus applies here: **count first**.

`work-counters` now keeps a 64-slot histogram of COP2 computational `funct`
values, reported by `examples/work_bench.rs`.

### Super Mario 64, 120 frames, 14,577,323 COP2 computational ops

That is ~121,478 per frame against 294,983 RSP instructions per frame — so
**41% of everything the RSP executes is a VU computation**. Only **32 of the 64
possible `funct` values ever appear**.

| funct | instruction | share | cumulative |
| --- | --- | --- | --- |
| `0x0e` | `VMADN` | 17.60% | 17.60% |
| `0x0f` | `VMADH` | 14.05% | 31.65% |
| `0x0d` | `VMADM` | 8.93% | 40.58% |
| `0x04` | `VMUDL` | 8.60% | 49.18% |
| `0x11` | `VSUB` | 5.43% | 54.61% |
| `0x06` | `VMUDN` | 5.14% | 59.76% |
| `0x15` | `VSUBC` | 4.11% | 63.87% |
| `0x32` | `VRCPH`/`VRSQH` class | 3.69% | 67.56% |
| `0x10` | `VADD` | 3.52% | 71.07% |
| `0x05` | `VMUDM` | 3.29% | 74.36% |
| `0x33` | `VMOV` | 3.19% | 77.55% |
| `0x1d` | `VSAR` | 3.14% | 80.69% |

**Four operations are half the work. Twelve are 81%.**

### The result that decides the shape of the work

**`funct 0x00..=0x0F` — the whole multiply / multiply-accumulate family — is
61.64%, and it is dispatched by a single function, `multiply_lane`.**

So vectorizing *one* function covers **62% of the VU's computational work**, and
that function is the natural SIMD target anyway: eight independent 16x16 lane
products into a 48-bit accumulator is exactly what a vector unit does.

This also bounds the ambition honestly. The VU is ~8.5% of a frame; 62% of it is
~5.3%. Even a perfect vectorization of `multiply_lane` cannot exceed that, and
the real figure will be lower because the dispatch, the register reads and the
accumulator writeback do not vanish. **Any SIMD work here must be measured
against that ceiling, not against the 8.5%.**

### Why this is measured rather than assumed

The alternative was to read `vu.rs` and vectorize what looked hot. Three
separate figures in this document turned out to belong to configurations that
were not being run — the VI's 4.64%, the RDP's 6.36%, and the shared-device
plan's "double PCIe crossing". Reading is how those happened.
