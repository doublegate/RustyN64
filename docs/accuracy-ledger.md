# Accuracy ledger — RustyN64

**References:** ADR 0005 (what defers here), ADR 0006, ADR 0007;
`ref-docs/2026-07-20-vr4300-timing-supplement.md` (the undocumented-constants list);
`docs/testing-strategy.md`; `docs/engineering-lessons.md` §3.3.

## What this file is for

Three things, and nothing else:

1. **Measured constants** — numbers the hardware documentation does not supply, which we fitted
   from test ROMs. Each records *how* it was measured, so it is falsifiable.
2. **Open residuals** — known-wrong behavior we have chosen to document rather than point-fix.
3. **Ruled-out approaches** — attempts that failed, with the reason, so nobody rediscovers them.

**Host-performance figures do not belong here** — frame times, FPS, profile shares, and
ruled-out *optimizations* live in `docs/performance.md`, which carries its own
"Ruled out by measurement — do not retry" list and its own provenance rules. Its
§*Where the optimization program ended* (2026-08-01) is the closing index of that work,
including the two declined ADRs (**0016** RSP SIMD, **0017** CPU recompiler) — neither of
which is an entry here, because declining an optimization changes no emulated
behavior. The three
categories above are about the emulated machine: constants the hardware documentation
does not supply, behavior known to be wrong, and accuracy approaches that failed. A
compiler flag that changed no emulated behavior is not an entry here however carefully
it was measured, and filing it here would blur what this file is for. Stated explicitly
because review has asked for the move three times, which is a sign the boundary was
discoverable only by reading both files.

The rule that gives this file its value: **an entry here is honest, a per-quirk patch is not.**
When a ROM fails and the fix would be a special case, the entry goes here instead (ADR 0005).

Equally: **a measured constant is never adjusted to make a specific ROM pass.** The moment a
number is tuned rather than measured, every later timing result built on it becomes
unfalsifiable — the whole suite silently stops being evidence. If a constant looks wrong, measure
it again and say so; do not nudge it.

## Status

**Phase 1 is complete, and still nothing here has been *measured*.** That is the honest headline:
every entry resolved so far was resolved by **citation** — it turned out to be documented after all
(C-2, C-3, C-7, C-22, S-1, S-3, U-1, U-3, U-4) — or by implementing behavior the sources do
describe. Not one constant has been obtained by measuring this emulator against hardware, because
the instrument for that is n64-systemtest's default-off `timing` set and it has not been run.

So the file's shape has changed less than the code has. `M` (**C-1**) still has no value; the
cache-miss costs that depend on it are still uncharged; the RDRAM bank-state costs (**C-4**) are
untouched. The FPU execution rates (**C-29**) were added as *documented* numbers, and both oracles
are insensitive to them — they are unfalsified rather than verified, which is a weaker claim and is
recorded as one.

What the preamble asks of a reader is unchanged: a constant here without a provenance line is a
bug, and a number that appeared without one is the failure this file exists to prevent.

---

## 1. Measured constants

| # | Constant | Value | How measured | Status |
| --- | --- | --- | --- | --- |
| C-1 | `M` — memory access time (PCycles) | **RCP register (uncached): 22** (measured); **D-cache fill: 40** (fitted, ares); **I-cache fill: 46** (fitted, = D-fill + UM Table 11-2 base offset; cen64 uses 48) | RCP: CPUTIMINGNTSC mult/div differential (0.4%). Cache fills: **FITTED** — no hardware cached-miss oracle exists; verified only for self-consistency by the first-party microbenches | **RCP-register `M` measured; D- + I-cache fills fitted + charged (I-cache via a unit-test seam); RDRAM bank-state (C-4) + a true cache-fill measurement open** |
| C-2 | Exception epilogue cost (PCycles) | **2** | ~~measurement~~ **documented** — UM §4.7 p. 114 | **resolved; not a measured constant** |
| C-3 | CP0I (CP0 bypass interlock) cost | **1** | **documented** — UM §4.6.9 p. 113 | **resolved; not a measured constant** |
| C-7 | ITM (instruction micro-TLB miss) penalty | **3** | **documented** — UM §4.6.2 p. 107 | **resolved; not a measured constant** |
| C-4 | RDRAM row-hit / row-miss / dirty-miss | — | — | **not yet measured** |
| C-5 | `DIV` quotient when divisor bits 63 and 31 differ | *32x35 division* | **guessed** | needs hardware |
| C-6 | Divide-by-zero `HI`/`LO` values | conventional | **guessed** | needs hardware |

| C-16 | `fast-exec` timing divergence — instructions retired over 120 frames, versus the accurate path | **+1.04%** (173,254,496 vs 171,471,972) | **measured**, Super Mario 64, `examples/frame_bench.rs`, 120 frames after the VI comes up | **the ADR 0013 §4 bound; re-measure whenever the cost model changes** |

### C-1 — `M`, memory access time in PCycles

The single most load-bearing unknown. Both documented cache-miss formulas are parameterized on
it: D-cache fill = **8–9 + M**, I-cache fill = **14–15 + M** (UM Tables 11-1/11-2). No source
gives a value.

Informal hints, all explicitly hedged by their authors and none usable as a number: RDRAM "about
10-20+ clock wait time"; RCP registers "5-6 PClock cycles"; MI registers "about 2"; RSP
DMEM/IMEM "4-5".

For scale, the reference emulators collapse the whole access into one constant and **disagree**:
CEN64 charges 38 PClocks for an uncached word, 44 for a D-cache fill, 48 for an I-cache fill
(under the source comment `// Currently using fixed values....`); ares charges 40 for a D-cache
fill. Neither derived theirs from a spec. Note CEN64's 44 = 8 + 38, which is consistent with the
Table 11-1 sum plus its own word delay — weak corroboration that the formula reading is right.

`M` is almost certainly **not a single number** — it should vary with target region (RDRAM vs
RCP register vs SP memory vs cart) and with RDRAM bank state (C-4). Expect a small table, not a
scalar. Recording it as a scalar first is acceptable; recording it as a scalar *permanently* is
how a fitted constant becomes a fudge factor.

**Owner:** T-11-008.

**Oracle status (2026-07-24, gap-analysis Stage C).** The measuring instrument for `M` (and
C-29) is n64-systemtest's `timing` set, which is gated behind a `--features timing` build and
is **not** in the committed base ROM. That build is now reproducible on this machine:

```bash
rustup toolchain install nightly-2022-07-10 --component rust-src   # the pinned toolchain
cargo +stable install nust64                                        # the ROM packager
cd ref-proj/n64-systemtest && cargo run --release --features timing # -> target/.../n64-systemtest.z64
```

The ROM builds cleanly (2.7 MB, header reports `timing=1`). **But the full timing suite does not
terminate in the emulator**: it starts the base 917 tests and then a timing-dependent path hangs
(no end-of-run summary at 12×10⁹ master ticks / ~350 s wall-clock, vs the base ROM finishing its
917 in ~125 s). This is itself a signal — the emulator's cycle timing is wrong enough that a
timing test loops — and it makes a clean baseline **chicken-and-egg**: the measurement the timing
set would give is what the C-1/C-29/T-11-002/003 work needs, but that same work is what lets the
suite run to completion. n64-systemtest's `timing` feature is monolithic (`timing = ["quick"]`),
so a single-test curated ROM is not available to sidestep the hang. **Next step (Stage D):**
diagnose the post-917 hang (likely a `Count`/`Compare` or interlock timing loop), then re-run for
the baseline. The toolchain is no longer the blocker; the emulator's timing is.

**A working curated oracle now exists (update 2026-07-24).** Rather than fight the n64-systemtest
hang, the PeterLemon `CPUTIMINGNTSC` / `CP1TIMINGNTSC` ROMs (krom, Unlicense, committed under
`tests/roms/peterlemon-timing/`) each time a fixed loop of one instruction with `Count`, compare
against a **hardware-expected value baked into the ROM**, and draw green (pass) / red (fail). They
**run to a verdict in the emulator in seconds** (no hang), driven by
`crates/rustyn64-test-harness/tests/peterlemon_timing.rs`. **Provisional aggregate baseline** (not
a measurement of `M`): `CPUTIMINGNTSC` draws an **all-red** frame today (0 green / 11 111 red glyph
pixels). That is an *aggregate* pass/fail — it says the emulator's `Count` deltas do **not** match
the ROM's baked-in expected values for the instructions it covers, but a red verdict alone does not
isolate `M`, nor prove each instruction is individually wrong: the mismatch could be a fixed
per-loop offset, the loop-overhead timing, or a genuine per-instruction error, and this ROM
compares *absolute* deltas that conflate them. So **C-1 stays "not yet measured"** — deriving `M`
needs the differential *measured-vs-expected* `Count` deltas (read the ROM's `COUNTWORD` /
per-instruction values), which the green/red frame does not surface. What the oracle gives today is
a fast, non-hanging, falsifiable target the Stage-D timing work drives toward all-green, and the
place to read those differential deltas from. (`CP1TIMINGNTSC`, the C-29 FPU oracle, executes
cleanly ~10⁹ instructions but its slower battery needs a larger budget to draw its full grid — a
Stage-D follow-up.)

**First differential measurement (update 2026-07-24).** The `cpu_timing_differential` diagnostic
now reads `COUNTWORD` — through the **write-back D-cache** (`Dcache::hits`/`read`), because it is a
KSEG0 store whose value is stale in raw RDRAM (a real reading-method fix; see
`docs/engineering-lessons.md`). Result for the last covered instruction:

> **measured = 304 180 (0x0004_A434)** vs **expected = 56 092 (0x0000_DB1C)**, **ratio ≈ 5.42**.

The ROM counts how many `add / lw VI_V_CURRENT / sync / bne / addiu` loop iterations fit in a fixed
VI-scanline window (line 0 → 512). **The VI tick rate is verified correct** (`MASTER_HZ / (60 · 525)
≈ 5952` master ticks/half-line), so the ~5.42× excess is **CPU-side**: we fit 5.42× *more*
iterations than hardware because our loop iteration is ~5.42× too cheap. The loop's dominant
hardware cost is the **uncached VI-register `lw` + `sync`** — memory/MMIO access latency, i.e.
**`M`** — which we currently charge ~1 cycle for. So the 5.42× is a concrete, falsifiable
consequence of the unmeasured `M`. `M` stays "not yet measured" as a *value*, but it now has a
**numeric target** off a repeatable oracle, not just an all-red frame.

**Guard-rail — do NOT fit `M` to this ratio (correcting the sentence this entry first carried).**
An earlier draft said "driving it toward 1.0 is the way to *measure* `M`". That is wrong, and it is
exactly the fitted-constant trap this whole file exists to prevent. The 5.42× is a **joint** measurement
of the entire `add / lw / sync / bne / addiu` loop: the uncached `lw` latency (`M`), the `sync` cost,
and every instruction's base cost, all at once. Tuning a single `M` until the ratio hits 1.0 would
silently bury `sync` + the per-instruction error inside "`M`", producing a fudge factor that every
later timing result then rests on. **To isolate `M` cleanly you need a differential-of-differentials**:
a probe that measures the `Count` delta of a loop with `N` uncached reads for two values of `N`, so
the *slope* `(delta(N₂) − delta(N₁)) / (N₂ − N₁)` is the per-read cost with the loop overhead
canceled — a first-party microbenchmark (the accuracy-battery / `T-71-001` shape), or the
n64-systemtest `timing` set's targeted per-access tests (blocked on the post-917 hang, above). This
oracle **signals** the gap and bounds it; it does not, by itself, *isolate* `M`.

**And the slope is still not `M` on its own.** It is the whole added read's cost:
`slope = base_pipeline_cost(lw) + M(region)`. So `M(region) = slope − base_pipeline_cost(lw)`, and
because `M` is not a single number (it varies with target region — RDRAM vs RCP register vs SP
memory — and RDRAM bank state, C-4), **any measurement must record the access region and cache
state** it was taken under, alongside the oracle's expected-vs-measured deltas. Recording the raw
slope as `M` would recreate the fitted-constant error at one remove — it would fold the `lw`'s own
pipeline cost into "`M`".

**MEASURED — RCP-register `M` = 22 PClocks (update 2026-07-25, once the R-19 fix let the timing
suite complete).** The clean isolation the guard-rail asked for was found in CPUTIMINGNTSC itself,
without a variable-`N` ROM. Every block times a different instruction in the *same* loop
`[test, lw VI_V_CURRENT, sync, bne, addiu]`, and the multi-cycle **mult/div** test instructions
have **documented** stall costs (UM Table 3-12: `mult`/`multu` 5, `dmult`/`dmultu` 8, `div`/`divu`
37, `ddiv`/`ddivu` 69). Those known costs are the varying axis a variable-`N` read loop would have
provided. Fitting `expected_i = W / (B + c_i)` across the nine instructions (a linear regression of
`1/expected_i` on `c_i`) recovers, **independently of any single absolute count**:

> **window `W` ≈ 1.52 × 10⁶ PClocks**, **hardware base-loop `B` ≈ 25.5 PClocks/iter** (model
> error < 1.5% on the high-leverage mult/div points; the reproducible arithmetic is
> `peterlemon_timing.rs::m_is_derived_from_the_multdiv_differential_not_fitted`).

Our own base loop is **exactly 4.00 PClocks** (four one-cycle instructions, no memory latency —
confirmed by `cpu_timing_differential` reading 304 180 = `W`/(4+1) before any charge, and by the
`Random` timing tests passing so every base instruction cost is right). So the missing
`B − 4 ≈ 21.5 PClocks` **is** the uncached `lw VI_V_CURRENT` latency `M`. Charged as **22** (integer
PClocks) at the uncached-read site (`pipeline::read_width`, RCP MMIO range `0x0400_0000..=
0x04FF_FFFF`, `Pipeline::M_RCP_REGISTER`), the ROM's absolute count moves **304 180 → 56 330** vs the
hardware-baked **56 092 — a 0.4% match**. That agreement is the **independent confirmation** the
guard-rail demanded, *not* the fit target: 22 came from the mult/div slope, and the absolute count
was then checked as a consequence.

**Scope + honesty.** This is `M` for an **uncached RCP-register read** (measured on VI; the sibling
RCP registers share the RCP bus and are charged the same, flagged pending their own vectors). It
conflates the RCP-access latency with `sync`'s wait, which cannot be separated by this loop —
recorded as a combined "uncached RCP access + sync" cost, honest about the conflation.

**D-cache fill `M(RDRAM)` — two-regime MODEL grounded in documentation; the cold anchor derived
from an external estimate (~60 PClocks); the charged row-hit **40** still PROVISIONAL (update
2026-07-25).** `M(RDRAM)` is **not a scalar**. What is now grounded in documented hardware is the
*shape* — the two regimes, the fill formula, the clock — and the *cold* anchor; the specific warm
number 40 remains a provisional row-hit-typical estimate (corroborated by two emulators, not
measured), and it is charged **unconditionally for every D-cache miss** — there is no row-state
dispatch yet (that is C-4). Three independent sources establish the shape:

1. **The CPU-side formula (VR4300 UM Tables 11-1/11-2, verified by summing the rows).** A D-cache
   fill is `1 + 1 + (1..2) + 2 + M + 2 + 1 = 8..9 + M` PClocks; an I-cache fill is `…+ 8 + 1 =
   14..15 + M` (the extra **+6** is the I-line's 8-word transfer vs the D-line's critical-doubleword
   2-word restart — UM §11.3.2/11.3.3). `M` is defined *only* as *"Time needed to access memory,
   measured in PClock cycles"* and **given no value** — the UM is explicit that the processor
   releases the SysAD bus to slave state and waits an **external-agent-determined** number of SClock
   cycles (UM §12.6.2/12.6.8), which is exactly why `M` is a memory-system parameter, not a CPU
   constant. The `±1` is the documented PClock:SClock = **1.5** synchronization jitter (UM Table
   10-1; PClock = 1.5 × MasterClock = 93.75 MHz on the N64).

2. **The cold-access latency — an EXTERNAL ESTIMATE, not a primary capture.** Copetti's N64
   architecture write-up (<https://www.copetti.org/writings/consoles/nintendo-64/>) gives *"the
   delay between initiating a memory transaction to finding the value in cache … around 640 ns"* —
   a full cold fill (cross-confirmed loosely by Beyond3D/community, but no logic-analyzer capture
   backs a cycle count; treat 640 ns as a secondary-source estimate). Derivation: at the documented
   93.75 MHz PClock, `640 ns × 93.75 MHz = 60.0 PClocks` for the full cold D-cache fill ⇒
   `M(cold) ≈ 60 − 8 = 52`. This is the best-grounded number for the **row-miss (random-access)
   regime**, but its provenance is one architecture article, not silicon.

3. **The bank-state structure (N64brew RDRAM Interface + `Clock Timing`).** Each 1 MiB bank holds
   one open 2 KiB row; an access to the open row Acks (**hit, fast**), a different row NAcks and
   must close+reload (**miss, slow**), and a *dirty* row is *"even longer"* — with hardware bank
   tracking in `RI_BANK_STATUS` (BankValidBits/BankDirtyBits). A 2 KiB row spans **128** D-cache
   lines, so **sequential/streaming access is row-hit-dominated** (1 miss then 127 hits per row)
   while pointer-chasing is all row-miss. This is why the warm fill is far below the 60-PClock cold
   figure — and why a single constant cannot be cycle-accurate. Full model: **C-4**.

**Charged value — provisional, and why it is charged anyway.** Every D-cache miss is charged a
single **40 PClocks** (`Pipeline::M_DCACHE_FILL`) ⇒ `M ≈ 32`, regardless of bank state — the
emulator does not yet know whether a given miss hits or misses the open RDRAM row (no row-state
dispatch; that is C-4). 40 is therefore a **provisional row-hit-typical estimate**, not a measured
value: the UM justifies the *formula* and the *+6* transfer delta but **not** this warm-row number,
and the reason it is 40 rather than something invented is that both reference emulators independently
land there (ares 40 → `M≈32`, cen64 44 → `M≈36`) — corroboration for the row-hit regime, not a
measurement. It stays open until a per-regime measurement (hardware, or the two ROMs below) replaces
it. The **cold/row-miss ~60-PClock** anchor (item 2) and the dirty-writeback (`> 60`) are recorded
as the other two regimes but **not charged**; charging them needs the device-dependent
**RasInterval** cycle values, which are *undocumented* (N64brew: IPL3 just *"setup optimal RAS
timing"* from the per-device geometry) — so inventing them would be the fitted-constant trap, and
they stay open under **C-4**. The `M_ICACHE_FILL = M_DCACHE_FILL + 6`
relationship is enforced **at compile time** — `M_ICACHE_FILL` is *defined* as
`Self::M_DCACHE_FILL + 6` (the UM's 8-word I-line vs 2-word D critical doubleword), so a future
measured `M(RDRAM)` moves both together and they cannot desynchronize. The first-party **cached-miss
microbench** (`cache_miss_microbench.rs`) confirms the charge lands (D 39.99, I 46.05 PClocks) and
guards the constant, and the two hardware ROMs (#139/#140) put the true **per-regime** numbers one
console-run away. **The I-cache fill (fitted 46) is now charged
too — behind a deliberate test seam (update 2026-07-25).** An I-cache miss fires on *every* cold
fetch, and the CPU crate's fine-grained pipeline unit tests step fixed cycle counts for a free-fetch
model AND assert on the interlock / FPU stalls a fill would confound (charging it globally broke ~74
of them, several *fundamentally* — a run-cycle-counting shim would have masked the very stalls those
tests exist to check). So the I-cache stall is `#[cfg(not(test))]` in `icache_fill`: **active in real
execution and in every integration test** — the i-cache microbench, the systemtest, golden-log,
residue — where the pipeline runs as a dependency with `cfg(test)` false, but skipped in this crate's
own units. It is verified there: the `cache_miss_microbench.rs` I-cache test runs a straight-line
block twice the 16 KiB I-cache (every line misses) and, subtracting the verified 1-PClock base,
measures **46.05 PClocks/fill**; the systemtest still completes (Phase-1 `Failed: 0`, 90 suite-wide,
`xioctl(EXIT)`, ~33 s vs ~31 s) and golden-log 0-diff / residue / determinism hold. The D-cache
fill, by contrast, fires only on a rare cached load and is charged unconditionally (two units
absorbed it). To make the eventual hardware measurement one console-run away, two bare-metal
timing ROMs are authored in `tools/mrdram-timing-rom/` (MIT OR Apache-2.0, blank IPL3):
`mrdram_timing.z64` measures the D-cache fill via a COP0-`Count` miss-vs-hit differential, and
`icache_timing.z64` measures the I-cache fill by timing a straight-line block larger than the
16 KiB I-cache and subtracting the verified 1-PClock base. Both emit their raw numbers over
ISViewer for a flashcart to read, and each has an emulator runner
(`mrdram_timing_rom.rs`, `icache_timing_rom.rs`) that boots the ROM through `load_direct` and
asserts it reads back the charged constant (the I-cache ROM measures 46.09 in-emulator, the
charged 46) — proof the measurement path is correct end-to-end, and a guard tying each ROM to
its constant. **`M(RDRAM)` as a true measurement, and the RDRAM bank-state model (C-4), remain
open.** No regression from the D-cache charge: golden-log 0-diff (it keys on retired instructions,
not stalls), the residue invariant, determinism, and the 950-test functional suite (Phase-1
`Failed: 0`, still 90 suite-wide, `Random` timing tests pass, runs to `xioctl(EXIT)`) are all
unchanged; two unit tests that stepped fixed cycles for a cached load had their budgets widened.

### C-16 — the `fast-exec` timing divergence bound

ADR 0013 §4 requires the divergence between the two execution modes to be
**measured, bounded, and recorded here before it ships** — not required to be zero,
because a relaxed timing model that agreed exactly would not be a relaxed timing
model. This is that measurement.

**What is measured.** Instructions retired over an identical run: the same ROM, the
same seed, the same 120 frames after the VI comes up. Both modes present the same
number of frames, so a difference in retired instructions is a difference in how
much emulated work fell inside a frame — which is exactly the timing model's
effect, expressed in the one quantity both modes count identically.

| mode | instructions retired, 120 frames |
| --- | --- |
| accurate (five-stage cascade) | 171,471,972 |
| `fast-exec` (instruction-granular) | 173,254,496 |
| **divergence** | **+1,782,524 — +1.04%** |

**Method.** `cargo run --release --example frame_bench [--features fast-exec]`,
Super Mario 64, 120 timed frames after 36 warm-up frames. Both figures are stable
to the digit across repeated runs (the counter is deterministic; only the wall
clock varies), so this is a reading rather than a sample.

**Why it is above zero, and what would move it.** The instruction-granular model
charges the documented per-class costs but not the five-stage structures the
accurate path carries — the bypass network, the load interlock, `prev_was_run`, the
flush cascade (`docs/cpu.md` §The instruction-granular path). Those are the missing
cycles. The gap is dominated by the *load-delay interlock*, which the accurate path
charges and this does not; adding it back would narrow the bound at some cost in
throughput, and that trade is not yet measured.

**When to re-measure.** Whenever the cost model changes — a new stall, a changed
constant, a measured `M(RDRAM)` replacing the fitted cache fills (C-1). A bound
that is not re-derived after the model moves is a number describing a model that no
longer exists.

**What this does *not* bound.** Architectural agreement, which is a separate and
stricter claim graded by `crates/rustyn64-cpu/tests/fast_exec_differential.rs` —
equality of GPRs, `HI`/`LO`, COP0, the FPRs, `FCSR`, and memory at instruction
retirement boundaries. A timing divergence of 1% is sanctioned; a divergence in
what an instruction computes is not.

### C-2 — exception epilogue cost — **RESOLVED, and this entry was wrong**

**2 PCycles, and the manual says so.** UM §4.7 (p. 114), the opening sentence of the section:

> *"When a pipeline exception condition occurs, the pipeline stalls for 2 PCycles and the
> instruction causing the exception as well as all those that follow it in the pipeline are
> aborted."*

This entry previously read *"**Not documented**: no figure appears in UM §4.7 or chapter 6"* —
naming the exact section the figure is in. The mistake was searching §4.7's *tables* and
Chapter 6's exception-processing prose, and never reading §4.7's own first paragraph.

So CEN64's 2 is **independent corroboration**, not the origin, and its source comment asking
*"do we actually delay an additional two cycles?"* is answered: yes.

**This is not a measured constant and does not belong in this section's spirit** — it is kept
here only so the correction is visible where the wrong claim was. The same error propagated to
`docs/cpu.md` and `ref-docs/2026-07-20-vr4300-timing-supplement.md`; both are corrected (the
latter by a new dated supplement, since `ref-docs/` is immutable).

**The lesson, which is the part worth keeping:** *"undocumented"* is a claim **about** the
manual, and it decays. Once written down it gets copied between files and stops being
re-checked — three files asserted it here. Before recording anything as undocumented, cite the
specific pages checked; before *relying* on such a record, re-check it.

### C-3 — CP0I — **RESOLVED, same cause as C-2**

**1 PCycle.** UM §4.6.9 (p. 113): *"This interlock causes a pipeline stall for one PCycle to
allow the CP0 register to be written in the WB stage before allowing any CP0 register to be read
in the DC stage."* The trigger is equally specific: an instruction that caused an exception
reaches WB while the subsequent instruction in DC requests a read of any CP0 register.

This entry previously said *"no cycle count located in the manual text"* while citing §4.6.9,
which is the paragraph containing it.

Separately, and still true: n64-systemtest's `cop0hazard` set is default-off upstream because
the *hazard* rules are not fully derived by anyone. That is a different question from this
interlock's cost — CP0 hazards are explicitly **not interlocked** (UM Ch. 19), so they are a
software-visible ordering constraint rather than a stall. Sprint 2 decides whether to model them.

### C-7 — ITM, the instruction micro-TLB miss penalty — **documented**

**3 PCycles.** UM §4.6.2 (p. 107): *"A miss penalty of 3 PCycles is incurred when the micro-TLB
is updated from the JTLB."*

Worth stating the structure, because it is easy to conflate: the VR4300 has a **two-entry
instruction micro-TLB (ITLB)** in front of the 32-entry joint TLB. A micro-TLB miss is a
**stall**; a JTLB miss is an **exception**. Modeling only the JTLB loses this cost entirely.
Whether Sprint 2 models the micro-ITLB separately is an open decision recorded in that sprint's
plan.

### C-4 — RDRAM bank state

**The structural model is now fully documented; only the per-device cycle values are missing.**
Each 1 MiB bank holds one open **2 KiB row**. An access to the open row Acks (**hit**); an access
to a different row NAcks, closing the current row and loading the new one (**miss**); a *dirty*
row must be written back to the array first (**even longer**). The controller tracks this in
`RI_BANK_STATUS` (`BankValidBits[7:0]` / `BankDirtyBits[7:0]`, one per bank) so it always knows
which requests will miss and how long to wait before resending (N64brew *RDRAM Interface* §Bank
Status Tracking). A 2 KiB row spans **128** D-cache lines (16 B each), so sequential access is
row-hit-dominated and random access is all row-miss — the mechanism behind C-1's two fill regimes.

**Cycle anchors (see C-1):** full D-cache fill ≈ **40 PClocks** row-hit / **60 PClocks** row-miss
(the latter measured from copetti's ~640 ns cold access × 93.75 MHz PClock); dirty-writeback `> 60`.

**What is still open:** the programmable timing registers — `RasInterval`
(`RowPrecharge`/`RowSense`/`RowImpRestore`/`RowExpRestore`, 5-bit fields) and `Delay`
(`AckWinDelay`/`ReadDelay`/`AckDelay`/`WriteDelay`) — are documented bitwise, and the `Delay` boot
values are known (IPL3 writes `0x18082838` = AckWin 5 / Read 7 / Ack 3 / Write 1), but the
`RasInterval` values are **device-geometry-dependent and not published** (IPL3 only *"setup optimal
RAS timing"* from the per-device descriptor). Translating those into a per-regime cycle model — the
step from C-1's two scalar anchors to a real bank-state charge — needs either the RAS values or a
hardware capture. Until then a single row-hit-typical scalar is charged (C-1). Interacts with C-1.

### C-5 — `DIV` with mismatched divisor sign bits

The `MULT`/`DIV` sign-extension erratum is documented, but with one hole. When
bits 63 and 31 of the divisor **differ**, the quotient written to `LO` is
described as incorrect and *"it is currently unclear how the outputs of this last
case are arrived at"* — unknown to N64brew, not merely undocumented by NEC.

`alu::div` currently performs the 32x35 division in that case as well. **That is a
guess**, recorded here so it is not mistaken for the documented behavior. `HI` is
better founded: `remainder = (int32_t)(dividend - quotient * divisor)` computed in
64-bit, which the wiki does state.

**Owner:** T-11-005 (the errata ticket), characterized against hardware or
n64-systemtest.

### C-6 — divide-by-zero `HI`/`LO`

Architecturally *undefined* on MIPS. `alu::div`/`divu`/`ddiv`/`ddivu` use the
conventional emulator interpretation (`LO` = ±1 or all-ones, `HI` = dividend).
Unverified against hardware. What *is* non-negotiable and tested is that it does
not panic — a guest program can divide by zero at will.

---

## 1b. Genuinely undocumented — needs a hardware pin, not a guess

Distinct from section 1: these are not constants to fit, they are *behaviors* the manual
declines to define. Each must be pinned against n64-systemtest or hardware before any
implementation choice here is treated as correct.

| # | Question | What the manual says | Owner |
| --- | --- | --- | --- |
| U-1 | Reserved COP0 registers 7, 21..=25, 31 | **RESOLVED — measured** — they are a shared write latch, see C-15 | resolved |
| U-2 | `TLBP` low `Index` bits on a miss (we leave them **zero**) | Only that `Index.P` (bit 31) is set (UM §5.4.11 p. 158); the remaining bits are unstated | Sprint 2 |
| U-3 | The N64's full `PRId` value | **RESOLVED — see C-22.** Recorded verbatim: *"`Imp = 0x0B` for the VR4300 series; the `Rev` field is unstated and the manual warns against depending on it (UM §5.4.5 p. 151)"*. That was true of the manual and false of the N64brew wiki this project mirrors, which names `0x10`/`0x22`/`0x40` — the decay this table exists to make visible | resolved |
| U-4 | ~~Which `Int[4:0]` line the MI drives~~ | **RESOLVED** — `IP2`. Not in the CPU manual (board-level) nor in the N64brew mirror, but stated by libdragon: `#define C0_INTERRUPT_RCP C0_INTERRUPT_2` (`ref-proj/libdragon/include/cop0.h`), which also gives `IP3` = CART, `IP4` = PRENMI, `IP7` = timer. libdragon is public domain, so this is citable rather than merely observed | **closed** |
| U-5 | 32-bit address calculation that overflows the sign-extended range | *"The address calculated at this time is invalid, and the result is undefined"* (UM §5.2.3 p. 130, §5.2.4 p. 134) — an explicit refusal to define | **RESOLVED (Phase 1).** Not by defining the undefined case, but by finding that the suite *does* define the surrounding rule: an address in 32-bit mode must be the sign extension of its low word, and one that is not raises AdEL before the TLB is consulted (`addr::is_compat`). n64-systemtest asserts it directly |
| U-6 | `Config.EC` on the N64 | `0b111` (1:1.5) is allowed *"with the 100 MHz model only"* (UM Appendix A note 1, p. 628), and the N64's ratio is 1:1.5 — so `0b111` is a strong **inference**, but the manual never names the N64 | Sprint 2 |
| U-7 | The **corrupted output** of the FP multiplication erratum | The *trigger* is documented (`VR4300.md`: a multiply whose preceding multiply had a NaN, zero or infinity operand) and so are the affected steppings (NUS-01/02/03), but **what wrong value is produced has never been characterized** — recorded in `ref-docs/2026-07-20-vr4300-timing-supplement.md` as an undocumented constant. `Stepping::Early` can therefore be *selected* but changes no arithmetic; inventing a plausible wrong value would be the fitted-constant failure this file's preamble forbids | Sprint 3 modeled the switch and the trigger. Needs an affected console, or a hardware capture, before the output can be reproduced |
| U-8 | FPU rounding modes and the `inexact` / `underflow` flags are **partial** | `FCSR.RM` is honored by the conversions but **not** by `add`/`sub`/`mul`/`div`, which use Rust's operators and are nearest-even only. Likewise `inexact` is set for overflow and conversions but not for ordinary rounding, and `underflow` only for conversions that flush to zero. Both need the *exact* result before rounding, which the hardware float operators do not expose | Needs soft-float arithmetic or per-operation re-rounding. Recorded so a caller does not trust a bit that never sets — the module's own doc table says which flags are complete. **RESOLVED (Phase 1)** by the soft-float core: `FCSR.RM` is honored by `add`/`sub`/`mul`/`div`, and `inexact`/`underflow` are detected from the exact pre-rounding result. See **C-11 RESOLVED**, which also records the second bug the fix uncovered |

U-6 is the one to watch: it is consistent with ADR 0006's clock derivation, which makes it
tempting to promote to a fact. It is an inference from a part-number restriction, and it stays
labeled as one until something reads the register on hardware.

## 2. Open residuals

> **On the n64-systemtest counts quoted below.** Several entries state the suite's
> failing-assertion count as it stood *when that entry was written* (marked
> *(as-at — see the note above this table)*). Those notes are left unrewritten, per
> this ledger's immutability discipline — but they are **not current**: Phase 5 took
> the count from 93 to **90** (see "Measured n64-systemtest impact" below).
> **`docs/STATUS.md` is the single authoritative source for the current count.**

Residuals whose record outgrew a table cell (over 4,000 characters) have their full
text under [`residuals/`](residuals/README.md) and are linked from their row here.
Those files are append-only too: superseded claims stay, marked, rather than being
edited away.

| # | Symptom | Suspected mechanism | Classification | Status |
| --- | --- | --- | --- | --- |
| R-14 | **RESOLVED** — `triangle_fill` was rendering triangle edges **4× too steep/fast**: a slope that should widen a triangle 0.25 px per pixel-row widened it 1 px per row. Found by the T-33-005 conformance gate on its first triangle vector (Angrylion rendered a near-vertical line, RustyN64 a staircase) | The edge slopes `DxHDy`/`DxMDy`/`DxLDy` are **dx per pixel-row** (N64brew Wiki *Reality Display Processor / Commands* **§0x08 through 0x0F – Fill Triangle**, the Base Command word tables — mirrored at `n64brew_wiki/markdown/Reality Display Processor/Commands.md`. The `dxhdy`/`dxmdy`/`dxldy` fields read "Integer part of **change in x per change in y** of line connecting …" in `s13.16`, and `yh`/`ym`/`yl` are "`s11.2` format" *screen* y coordinates. **Citation corrected 2026-07-28:** this previously cited a "§Edge Coefficients", which does **not exist** in the page — the quoted wording and the fixed-point formats were right, the section name was not). `triangle_fill` evaluated the edge at each sub-scanline as `xh + (y − yh_base) · dxhdy` with `y = line·4 + sub` in **quarter-pixel** units, but never divided the contribution by 4. **Fix:** the three slopes are pre-shifted `>> 2` at decode (parallel-rdp `span_setup.comp:167`: `setup.dxhdy = raw >> 2`) | absolute — a fixed-point unit error, oracle-confirmed | **Closed.** The fix landed; `fill_tri_16` and `fill_tri_wide_16` conformance vectors now pass byte-for-byte against Angrylion. The self-asserted triangle unit tests that baked in the buggy staircase were corrected — their `DxMDy` changed from `0.25` to `1.0` (the value for which the staircase is the *correct* output), which the `fill_tri_wide_16` vector independently confirms against the oracle. Did not change the n64-systemtest count (no systemtest drives the render path); the oracle stays **93** *(as-at — see the note above this table)* |
| R-1 | **RESOLVED** — see C-21. The failing instruction was `ADD.S $1, $29, $30`, not the `ADD.S $0` the assertion names; a correlated capture separated cause from visible effect by exactly the pipeline depth | — | absolute | Closed |
| R-2 | **RESOLVED** — `BC1` implemented, and the compare forwarded to it (C-25) | — | absolute | Closed |
| R-6 | **[HISTORICAL BASELINE — the "only NTSC is modeled" claim is SUPERSEDED by the PAL 50 Hz RESOLUTION in the disposition column; retained per the append-mostly rule.]** The VI scan cadence (T-31-004) *was* anchored to a **nominal 60 Hz field rate** (`VI_FIELD_HZ`) with only NTSC modeled — the per-half-line period was `MASTER_HZ / 60 / (VI_V_TOTAL + 1)`, and `VI_V_CURRENT` / the `VI_V_INTR` interrupt derive from that | The VI dot clock is off a separate crystal the N64brew wiki gives only *roughly* (*Video Interface* §Clocks: "roughly 12.3 megapixels/sec", ×4 ≈ 49 MHz VI clock; the exact NTSC value is not stated). Rather than fit an imprecise dot-clock frequency, the field rate is anchored to the standard NTSC 60 Hz and the half-line count taken from the software-programmed `VI_V_TOTAL` — so the cadence is correct to the field, and only the sub-field phase (which `H_TOTAL`/`H_TOTAL_LEAP` set exactly) is nominal. The interlace `VI_V_INTR` bit-0 quirk (§VI_V_INTR) is also not modeled | absolute — a clock-rate anchor, not a fitted per-ROM constant | **Open.** Correct to the field: `VI_V_CURRENT` advances and wraps at `VI_V_TOTAL + 1`, and the VI interrupt fires once per field at `VI_V_INTR` (pinned by the `vi` unit tests and a scheduler integration test). Deferred: the exact `H_TOTAL`/leap sub-field timing, PAL's 50 Hz field rate, and the interlace `V_INTR` quirk. To be validated against n64-systemtest's `timing`/VI groups when they are run. **PARTIALLY RESOLVED (2026-07-25) — PAL 50 Hz field rate.** `Vi::field_hz` now selects PAL **50 Hz** (`VI_FIELD_HZ_PAL`) when the field is PAL-length (`VI_V_TOTAL > 550`) and NTSC 60 Hz otherwise, so `ticks_per_halfline = MASTER_HZ / field_hz / (VI_V_TOTAL + 1)` scans PAL games at the right cadence — the same `ispal` split the scan-out geometry (R-5 slice 4a) already uses, so cadence and region agree — the `> 550` boundary is now the shared `VI_PAL_V_TOTAL_THRESHOLD` constant (one definition for both `Vi::field_hz` and `bus::scanout_scaled`). **Provenance:** 50 Hz is the documented PAL broadcast standard (N64brew *Video Interface*; counterpart to the anchored NTSC 60 Hz), not a fitted value; the `> 550` split is a wide-margin discriminator between NTSC's ~525-half-line and PAL's ~625-half-line fields (N64brew *Video Interface* §Clocks region field lengths), not a measured edge. Pinned by `a_pal_length_field_scans_at_50hz` (a PAL field advances one half-line in `MASTER_HZ/50/625 = 6000` ticks, distinct from the 60 Hz `5000`; mutation forcing always-60 fails it) and `the_pal_threshold_is_exactly_550` (V_TOTAL 550 = NTSC, 551 = PAL — pins the exact `>` boundary), with NTSC fields unaffected. **n64-systemtest impact: not measured** — the suite runs NTSC and has no VI field-timing group, so the PAL cadence is unreachable by it (the count is unchanged for that reason, not measured against it). **Still deferred:** the exact `H_TOTAL`/leap sub-field timing and the interlace/serrate `V_INTR` bit-0 quirk |
| R-10 | Color-combiner **exotic inputs** — noise, LOD fraction, chroma key and YUV convert — beyond the common input set. | They need the LOD pipeline, the `Set Key`/`Set Convert` registers and a noise source, none of which existed when the combiner landed. | absolute — a coverage boundary, not a fitted constant | **Partially resolved.** Chroma key (`key_en`) and the `Set Key` combiner inputs are implemented and oracle-pinned; noise is deliberately **not** attempted, because Angrylion fakes it with a shadertoy PRNG rather than modeling hardware. **Full record: [R-10](residuals/R-10.md).** |
| R-13 | Triangle **texturing** — the perspective divide, bilinear sampling, and LOD/mip tile selection. | The non-perspective path and the sampler wiring were the tractable first step; perspective and LOD needed the fuller pipeline. | absolute — a coverage boundary, not a fitted constant | **Partially resolved.** Clamp/mask/mirror/shift, the 3-point filter, the mid-texel bilinear and the primitive base tile are implemented and oracle-pinned; `mirror_s` is verified byte-for-byte against Angrylion. LOD/mip selection under a live stream remains open. **Full record: [R-13](residuals/R-13.md).** |
| R-12 | The Z-buffer machinery — the depth **codec** and **`depth_test`** with the depth-source commands (**PR-A**), the **Z-buffer RDRAM read/write** + **hidden bits** (**PR-B part 1**), and the **per-pixel depth test + Z-write** in the triangle rasterizer (**PR-B part 2a**: z-suffix decode, `interpolate_z`, `depth_span`) — is in place; the **combiner→blender color routing** (part 2b, the color is still the FILL register) and the **coverage accumulator** at edges (part 2c) are not yet wired, and the `dz` derivation is a first-cut integer gradient | These land — and are tested — ahead of the pipeline integration, which is the larger, riskier surface (the flat-fill→per-pixel rewrite that also closes R-9). Splitting keeps each PR reviewable (the project's split-large-tickets rule). The hidden-bit RAM is modeled accurately (additive default-no-op `RdramBus` methods + a lazily-allocated Bus store) rather than approximated, so the exact `dz` precision the conformance gate needs is preserved | absolute — a coverage boundary, not a fitted constant | **Open.** The codec is validated by boundary values + a `z_compress ∘ z_decompress` round-trip; `depth_test` by observable occluding-vs-occluded pairs per Z mode; the storage by a Bus hidden-bit round-trip and a full-`dz` Z-buffer round-trip (nine `rdp` + one `core` unit tests). `depth_test`/`zbuffer_*` have **no runtime caller** yet, so the oracle stays **93** *(as-at — see the note above this table)*. The coverage and routing land in **PR-B part 2** and are validated against the ParaLLEl-RDP conformance vectors (T-33-005) |
| R-11 | The blender (T-33-003) implements the divide-free `(P * a0 + M * (a1 + 1)) >> 5` with the `P/A/M/B` input muxes, both cycles, and `force_blend`; the **anti-aliased-edge divider LUT** (`uBlenderDividerLUT` — the coverage-weighted divide the RDP uses on partially-covered edge pixels), the **memory-alpha interpenetrating-Z blend-shift** path, **alpha-compare**, **dither**, the **`color_on_cvg`** early-return, and the **coverage write-back** (`cvg_dest`) are decoded but unused | These paths need the framebuffer read (`image_read_en` memory color), the coverage accumulator, and the Z buffer — none of which reach the blender until the triangle pipeline routes combiner→blender per pixel (T-33-004). The no-divide form is the one every non-edge pixel uses, so it is the honest first target; emitting nothing for the deferred paths (rather than a fabricated divide) keeps the gap falsifiable | absolute — a coverage boundary, not a fitted constant | **Open.** The no-divide equation (the `>> 5` fold and the `+ 1` on the `M` term), the `Set Other Modes` (0x2F) field decode, the `P/A/M/B` muxes, and the 2-cycle forward chain are validated bit-for-bit against hand-computed values (four `rdp` unit tests). `blend` now **has a runtime caller** — `depth_span` (T-33-004 PR-B 2b-blend) reads the framebuffer pixel and routes the combiner color through `blend` when the depth test enables blending, gated on `force_blend` (the reference's `!blend_en` fast-path keeps opaque pixels on the combiner color). A translucent-triangle integration test proves a 50/50 blend of the combiner color over a pre-filled background. **Dither is now present** (T-33-004 2c): the ordered RGB dither (magic/bayer matrix) is applied to the combined color on both the no-Z and depth pixel paths, validated byte-for-byte against Angrylion by `dither_tri_32`. **Alpha-compare is now present on BOTH the no-Z and depth paths** (`Set Other Modes` bit 0): `alpha_compare_passes` gates the pixel write on `combiner_alpha >= Set-Blend-Color alpha`, evaluated before coverage overwrites the alpha byte — validated byte-for-byte against Angrylion by `alpha_compare_16` (no-Z) and `alpha_compare_z_16` (a z-suffixed triangle with `z_update` on / `z_compare` off) plus a boundary unit test. On the depth path the gate sits after the depth test and `continue`s before both the color write and the z-write, which is observably equivalent to the RDP's pre-depth ordering because the compare is depth-independent (a pixel is written and its depth stored only when both depth and alpha pass). Still deferred here: the dithered-threshold variant (`dither_alpha_en`); the AA-edge divider LUT, the interpenetrating-Z blend-shift, `color_on_cvg`, and coverage write-back — these need the sub-pixel coverage accumulator (slice 2c). The oracle stays **93** (no systemtest drives the render path) *(as-at — see the note above this table)*. The deferred paths are validated against the ParaLLEl-RDP conformance vectors (T-33-005) |
| R-9 | The triangle rasterizer now interpolates **depth** (T-33-004 PR-B 2a), **shade** (2b), and **texture** (`S/T/W` → `fetch_texel`, 2b-texture) per pixel, and routes the combiner color through the **memory-read blender** when the depth test enables blending (2b-blend, gated on `force_blend`) — depth-tested, Gouraud-shaded, textured, and translucent triangles render; what remains is the bit-exact **sub-pixel coverage** (ParaLLEl-RDP's `quantize_x` sticky-bit edge rounding and the `do_offset` last-subpixel latch) and the coverage-driven AA blend, so each edge is still reduced to whole pixels (`>> 16`), and the `dz` derivation is a first-cut gradient | Landing the edge-walk, then the depth and shade interpolators (each hand-verified), then texture and the sub-pixel coverage is the tractable order; the coverage rule and the full combiner→blender→memory surface are a combinatorial space best pinned by the conformance fuzz (T-33-005) | absolute — a coverage boundary, not a fitted constant | **Open.** The flat fill, the depth test (occluding-triangle pairs), the shade interpolation (hand-computed base color + a combiner-routed shaded triangle), the texture sample, and the memory-read blender (a 50/50 translucent triangle over a pre-filled background) are each unit/integration-tested. The sub-pixel coverage **primitives** — `compute_coverage` (the 4×2 diamond-sample 8-bit mask) and `quantize_x` (the `s.16`→`s.3` sticky-bit snap) — are now a bit-exact port of parallel-rdp `coverage.h`/`span_setup.comp`, pinned by hand-computed unit tests derived from the oracle's arithmetic (full/partial/empty masks, the sticky bit). The primitives are now **wired into the 1-/2-cycle rasterizer**: the edge-walk builds per-Y-subpixel `s.3` edges, `pixel_coverage` gates each pixel (AA-off top-left-sample rule) and stores the coverage count in the pixel alpha (`(count − 1) & 7`, the `cvg_dest` clamp write-back). FILL/COPY mode keeps the whole-pixel span (correct — FILL renders "without subpixel accuracy"). This is validated against Angrylion: `fill_tri_frac_16` (FILL rounds fractional edges to whole pixels) and `shade_tri_frac_16` (a 1-cycle shaded triangle excluding a fractional-edge column and leaving a partially-covered column at reduced coverage) pass byte-for-byte. The **depth path** applies the same coverage (`shade_depth_tri_frac_16` — a z-suffixed fractional triangle — renders identically to `shade_tri_frac_16`, byte-exact vs Angrylion). **Ordered RGB dither is now wired** (T-33-004 2c): the combined RGB is dithered per pixel by the magic/bayer 4×4 matrix before write-back (`apply_rgb_dither`, a bit-exact port of Angrylion `dither.c` `rgb_dither`), validated byte-for-byte by `dither_tri_32` (the RDP default magic dither over a `0x112233` shade). **Alpha-compare and `cvg_dest = full` are now wired**: `pixel_coverage` stores full coverage (7) when `cvg_dest = 2` (validated by `cvg_dest_full_16` — a fractional triangle whose partial edge column stores `0xf801` instead of the clamp `0xf800`), and the alpha-compare write gate is on both pixel paths (R-11). Remaining slice-2c residual: the coverage-weighted **interpenetration Z** path, the **AA-edge blend**, and the **wrap/save `cvg_dest`** modes (these need the memory-read coverage accumulator) are not wired — each to be pinned by further conformance vectors |
| R-8 | Copy-mode `Texture Rectangle` (T-32-004) is wired for a **16-bit tile → 16-bit color image** (the first-picture path), including the **4-pixels-per-cycle sub-texel selection** under non-1:1 `DsDx`; `Texture Rectangle Flip` (0x25), the 8/32-bit and TLUT copy paths, and the copy alpha-compare are not modeled — an unsupported configuration draws nothing | The full copy pipeline (per-format `dx_shift`/`s_offset` 64-bit-group fetch, the 8-bit high-word replication quirk, the RGBA5551 alpha-on-LSB test) is a combinatorial surface best pinned by the bit-exact fuzz rather than by hand. The 16-bit 1:1 path is the one a first textured frame needs, and its horizontal step is scaled by `>> (5 + dx_shift)` so a canonical `DsDx = 4.0` advances one texel per pixel | absolute — a coverage boundary, not a fitted constant | **Open — but the 16-bit 1:1 copy is now ORACLE-VALIDATED.** Beyond the internal **round-trip identity** test (`Load Tile` loads a 4×2 texture and `Texture Rectangle` blits it back byte-for-byte; load and fetch share the odd-row swap) and the `wrap_coord` unit test, the `tex_rect_copy_16` conformance vector (T-33-005) now drives the full command sequence — Set Texture Image → Set Tile → Set Tile Size → Load Tile → COPY-mode Texture Rectangle — through **Angrylion** and RustyN64 matches the golden byte-for-byte. This is the **first texture path checked against the oracle** (copy mode bypasses the combiner and the 1-cycle texel pipeline, so it is clean where the `tex_tri_16` triangle path is not — R-13). **The 4-pixels-per-cycle sub-texel selection under non-1:1 `DsDx` is now modeled and oracle-validated too:** COPY mode reads a 64-bit TMEM word (4 consecutive texels) per cycle, so the base texel is evaluated at each cycle's first column (advancing `DsDx × 4` texels/cycle) and the within-cycle offset is a direct `+0..3` TMEM increment, **not** a per-pixel step. The `tex_rect_mag_16` vector (a 2× magnify, `DsDx = 2.0`) reads texels `0,1,2,3,2,3,4,5` and RustyN64 matches Angrylion byte-for-byte; the 1:1 case (`DsDx = 4.0`) is the special case `s = col` and the round-trip + `tex_rect_copy_16`/`_offset_16`/`_8x8_16` vectors still pass. **n64-systemtest impact: not measured** — the copy path has no runtime driver in the suite (nothing calls `texture_rectangle` during a systemtest run), so it is unreachable by n64-systemtest and the oracle count stays **93** *(as-at — see the note above this table)*. Still deferred (each an unsupported config that draws nothing): Flip 0x25, 8/32-bit, TLUT copy, and copy alpha-compare — validated against further conformance vectors as they land |
| R-7 | The TMEM loads (T-32-002) cover **8/16/32-bit** texels for `Load Tile` and **8/16-bit** for `Load Block`. **There is no 4-bit texel *load*** — a 4-bit *texture-image* load is invalid on hardware (Angrylion sets `rdp_pipeline_crashed` and bails, `tex.c:526-533`); games load 4-bit textures by setting an **8-bit** texture image + 8-bit LOAD tile, loading half as many texels raw, then rendering with a **separate 4-bit tile** that extracts nibbles at fetch. That canonical path already works. Genuinely deferred: the **32-bit split** path of `Load Block`, and a *direct* 4-bit LOAD tile paired with an 8-bit texture image (the `ti_size`-vs-`tile.size` load granularity — Angrylion keys the copy stride off `ti_size` and the destination short-index off `tile.size` via `sshorts = s >> 2`) | The earlier "4-bit loading needs nibble addressing" framing was a **misconception**: 4-bit texels are never DMA'd as nibbles — the load is a raw byte stream sized by the texture image, and the nibble semantics live only in `fetch_texel` (already implemented for I4/IA4/CI4). RustyN64's bail on a 4-bit texture image thus *matches* the hardware's invalid-load behavior. Writing nothing for the two genuinely-deferred cases keeps them falsifiable rather than emitting fabricated texels | absolute — a coverage boundary, not a fitted constant | **The canonical 4-bit path is now ORACLE-VALIDATED (2026-07-25).** `tex_tri_i4_16` (T-33-005) drives the full hardware idiom — 8 I4 texels packed two-per-byte, an **8-bit** `Load Tile`, then a **4-bit** (format I, size 0) render tile sampled across the triangle — and RustyN64's existing 8-bit load + 4-bit `fetch_texel` reproduce **Angrylion byte-for-byte**. Non-vacuous by construction: the eight intensities are DESCENDING and non-zero (texel 0 = white `0xFFFF`, not black), so an all-zero (no-op) load would render black and fail. Since the load is format-agnostic (a raw byte copy), this validates the load mechanism for **all** 4-bit formats; the I4/IA4/CI4 decoders are separately unit-tested. The two remaining gaps above are still `Open` (no runtime driver in n64-systemtest, so they do not change the suite-wide failing count — `docs/STATUS.md` is authoritative for it), to be pinned by further conformance vectors. The supported sizes remain byte-exact against hand-computed expectations, including the odd-row 32-bit-word swap, the 32-bit `Load Tile` split, and the `Load Block` dxt line-parity swap (five `rdp` unit tests) |
| R-5 | VI scan-out geometry and the analog post-filters — `VI_X_SCALE`/`VI_Y_SCALE` resampling plus AA, divot and de-dither. | The pixel conversion is exact and cited; the resampling geometry and the coverage-gated filters were the deferred part. | absolute — a resampling/filter geometry choice, not a timing interval | **Largely resolved.** `Bus::scanout_scaled` implements the geometry, bilinear lerp, gamma and the coverage filters, validated RGB byte-for-byte against Angrylion through the 13 committed `.vivec` vectors. **Full record: [R-5](residuals/R-5.md).** |
| R-4 | The VI register file (T-31-004) stores the **full 32-bit value** written to each register; the per-register write masks the hardware enforces (`VI_ORIGIN` 24-bit, `VI_WIDTH` 12-bit, `VI_V_INTR` 10-bit, the multi-field `VI_CTRL`/`VI_H_VIDEO`/scale registers, …) are not applied | The masks are documented as *field widths* in N64brew *Video Interface* per register, but the exact discard behavior on write (which reserved bits read back 0 vs. retain) is what n64-systemtest's VI-register group actually pins, and that has not been run against a masked implementation | absolute — a register-decode fact, not a timing interval | **Open.** In-range writes (every value the register's own fields can hold) round-trip correctly, which the T-31-004 unit tests pin; out-of-range bits are retained rather than dropped. To be measured against n64-systemtest's VI group and masked per register when that group is exercised (measure, don't guess). No assertion currently exercises it (count unchanged at 93 *(as-at — see the note above this table)*) |
| R-3 | FILL-mode `Fill Rectangle` (T-31-003) rasterizes the rectangle with an **inclusive lower-right pixel** — floor the upper-left, and draw through the pixel that *contains* the lower-right coordinate; in FILL/COPY mode the low two bits of `yl` are forced set before the shift (`yl \| 3`) so the final scanline fills whole. The **scissor** clips separately with an **asymmetric** lower-right (inclusive X, exclusive Y — see R-15) | The N64brew wiki says only "upper-left rounded down, lower-right rounded up"; the exact edge behavior is what the Angrylion oracle pins, and it is *inclusive of the lower-right pixel* (a rect `(0,3)-(1,4)` draws columns 0 **and** 1), plus the FILL/COPY `yl \| 3` (Angrylion `rasterizer.c` `rdp_fill_rect`). The earlier `(coord + 3) >> 2` half-open span dropped the boundary row/column — a realization of the cited prose that disagreed with the hardware. Read from the oracle's output, not computed | absolute — a rasterization geometry rule, not a timing interval, so the differential/re-phasing test is N/A | **Closed for the integer-coordinate FILL rect (oracle-validated).** The inclusive lower-right + `yl \| 3` rule is now pinned bit-for-bit against Angrylion by the **seeded-fuzz corpus** (`tests/vectors/fuzz/`, 48 random FILL rectangles sweeping color/size/position with a full-image scissor, all byte-exact) and a mutation-checked unit test (`fill_rectangle_lower_right_edge_is_inclusive`). The fuzz gate **found** the pre-fix off-by-one. Still unverified: **sub-pixel** (fractional-coordinate) rect edges, which the whole-pixel fuzz does not exercise; the **scissor** lower-right rounding is resolved separately (**R-15**, asymmetric X/Y). No n64-systemtest assertion drives the render path (count unchanged at 93 *(as-at — see the note above this table)*) |
| R-16 | The AI's `AI_STATUS` `COUNT` (bits 14:1), `WC` (bit 19), and `BC` (bit 16) readbacks, and the `AI_BITRATE` bit-clock timing, are a **best-effort** derived model, not oracle-pinned. `COUNT` is derived from `last_tick × video_clock / MASTER_HZ` as a sawtooth from `DACRATE/2` (wiki §COUNT); `WC` toggles on its second half-period; **`BC` is now modeled** (see the resolution) | These are the DAC's internal sample/bit-clock phase, which the CPU "cannot reliably sample rapidly enough" (wiki §BC) — so no public capture pins their exact value, and n64-systemtest has **no AI coverage** to gate against (verified by grep of `ref-proj/n64-systemtest`). The flags software actually polls (`FULL`/`BUSY`/`ENABLED`) are exact (ares `io.cpp`); only the sub-sample readback is nominal | absolute — a register-readback phase, no oracle | **Open (striven, ungated).** `FULL`/`BUSY`/`ENABLED` are unit-tested exactly; `COUNT`/`WC` are derived but unverifiable without a hardware logic-analyzer capture (the user asked to strive for cycle-exactness and to search for capture data — none was found). To be pinned if an AI-status capture or an n64-systemtest AI group ever surfaces. Emits a plausible value rather than a fabricated constant. **`BC` MODELED (2026-07-28).** `AI_BITRATE` is documented as "Half of bit clock period of I²S output to DAC", and the bit clock as "the Video clock, divided by two, divided by one more than this number" (N64brew Wiki *Audio Interface* §AI_BITRATE, mirrored at `n64brew_wiki/markdown/Audio Interface.md`) — so one half-period is `BITRATE + 1` video clocks and BCLK toggles once per half-period. `BC` (bit 16) is derived from the same video-clock tick count `COUNT`/`WC` already use, and `BITRATE = 0` stops the clock as documented. The readback gate is **`AI_BITRATE` alone** — the wiki says the counter "always ticks unless `AI_BITRATE` is 0" and never conditions either clock on `AI_DACRATE`, so BCLK keeps running with no DAC rate programmed (pinned by `the_bit_clock_runs_without_a_dac_rate`); `COUNT`/`WC` additionally need a DAC rate to have a range and report nothing without one. **It stays UNGATED, and the source itself is hedged:** the wiki says only that this is "(probably)" toggled per bit and that it "is believed" to be the BCLK line, precisely because the CPU "cannot reliably sample it rapidly enough even when `BITRATE` is set to 15" — i.e. it is not **CPU-observable**. Note the precise scope: that is a limit on *software* sampling, **not** a claim that BC is unpinnable in principle — an external logic-analyzer probe on the BCLK line could establish it, and this entry would be updated if such a capture surfaced. What is claimed here is only that **no public capture is known** and none is asserted. Pinned by two tests: the transition count over a fixed span must equal the documented half-period relation derived independently from `video_clock` and `BITRATE` (mutation-checked — using `BITRATE` instead of `BITRATE + 1`, or dropping the bit, both fail), and a `BITRATE` of 0 must hold the line low. **n64-systemtest impact: not measured** — no AI test drives this path (the suite has no AI coverage), so the oracle count is **unchanged at 93** *(as-at — see the note above this table)* **A SEPARATE, NON-PHASE DEFECT ON THIS SAME REGISTER — FIXED 2026-07-30.** Everything above concerns the `COUNT`/`WC`/`BC` *phase* fields. The `FULL` flag (bit 31), which is exact and which software actually polls, had a livelock of its own: **`AI_STATUS.FULL` could latch and never clear**, so a game waiting for a free DMA slot spun forever. Found by root-causing **World Driver Championship**, which issued 176,085 RDP commands under `hle_boot` and **zero** under `real_pif_boot` (the one title the boot-path census newly implicated). Measured, not guessed: the dominant retiring instruction under real-PIF was `LW a0, 0xC(t6)` at `0x8007_A8A4` with `t6 = 0xA450_0000` — i.e. **`AI_STATUS`** — followed by `AND t7, a0, at` against `0x8000_0000` and a `BEQ` that only exits when bit 31 is clear. Counting *transitions* rather than sampling the bit (the sticky-register lesson from this row's Banjo-Kazooie `Cause.ExcCode` mistake) settled it: over 300 frames the flag transitioned **48 times under HLE and exactly ONCE under real-PIF**, latching at ~frame 30 and never clearing again. **Mechanism:** `recompute_rate` mapped an unprogrammed `AI_DACRATE` to `sample_rate = 0`, `period_ticks()` to 0, and `tick()` then returned *before* `emit_sample` — and `emit_sample` is the **only** place a drained transfer is retired (`dma_count -= 1`). So a stopped DAC makes the two-deep FIFO unable to advance, and `FULL` (`dma_count > 1`) becomes permanent. WDC queues two buffers and polls `FULL` *before* programming the DAC, so it hit exactly that window. **The fix invents nothing, and the source matters:** hardware has no stopped-DAC state to model — the DAC counter runs off the video clock from power-on whatever `AI_DACRATE` holds — and **ares (ISC, vendorable, so readable under `ref-proj/README.md`)** makes the same choice structurally: `AI::power()` sets `dac.frequency = 44100` and `AI::main()` calls `sample()` unconditionally, so its byte-for-byte equivalent retirement block runs from power-on. An unprogrammed `AI_DACRATE` therefore now falls back to `DEFAULT_DAC_HZ = 44_100`, taken from ares and **labeled a modeling default, not a measured value** — `AI_DACRATE`'s reset value is not documented in anything this project mirrors, so no rate can be derived for that window. Nothing observable should depend on the exact number, because every title programs the register before it plays anything; if a future output does depend on it, that dependency is the bug. The old zero-gate was avoiding a real failure — `dac_rate == 0` computing `video_clock / 1` ≈ 48 MHz and flooding the sink — and a default rate avoids both that and the latch. **`Audio::new()` had to change too**, and the reason is worth recording: it initialized `sample_rate: 0` *literally*, and `recompute_rate` is only called on a `AI_DACRATE` or region write, so the first version of this fix left a machine that never programmed the AI with a stopped DAC anyway — the defect survived its own fix until the constructor matched. **Measured effect:** World Driver Championship under `real_pif_boot` goes **0 → 120,015 RDP commands** and from `scanout 0x0` to a real **625x237** frame; `FULL` transitions **1 → 46**. Full workspace suite **810 pass**, so the Phase-4 golden PCM stream is unaffected. Pinned by `full_clears_even_when_dacrate_was_never_programmed`, mutation-checked (restoring either half of the fix turns it red). One pre-existing test was corrected rather than deleted: `set_region_before_dacrate_keeps_rate_zero` asserted the rate was exactly 0, which **over-specified its own stated purpose** — its comment says it exists to prevent a ~48 MHz rate, and that protection is kept as an order-of-magnitude bound (`0 < rate < 100_000`) in the renamed `set_region_before_dacrate_does_not_fabricate_the_video_clock_rate`. **Still open, and not claimed as fixed:** WDC now *renders* but ends its run in a `B -1` self-loop at `0x8000_28C0`, which is a different halt reached after 120k commands — this entry closes the AI livelock, not that title. n64-systemtest impact: **not measured, and it cannot be** — the suite has **zero AI coverage**, so no assertion in it exercises this path and the failing-assertion count is unchanged at **90** for that reason, not because a before/after comparison was run. Stated as *not measured* rather than *none*, because "none" reads as a measurement that came back clean. This fix is validated by ares' structure, the mutation-checked unit test, and WDC's measured unblocking — not by the oracle. **CORPUS EFFECT MEASURED, AND IT IS NARROW — record the negative result.** The committed 66-title census was re-run after the fix and is **unchanged: 29 render, 24 issue zero RDP commands**, exactly the pre-fix figures. So **no other silent title was blocked on this latch**; the fix helps exactly one title on one boot path. That was foreseeable and should have been foreseen before the run was started: the census boots through `hle_boot`, where WDC was already rendering 176,085 commands, so that census **cannot show the improvement it was launched to measure** — it can only answer the separate question of whether anything else shared the latch, and the answer is no. Recorded rather than quietly dropped, because "a fix landed and the headline number did not move" is the shape of result most likely to be left out. **A DELIBERATE SIMPLIFICATION IN THIS FIX, recorded so it is not mistaken for fidelity (raised in review).** The fallback branches on `dac_rate == 0`, which **conflates "never programmed" with "software explicitly wrote 0"**. ares keeps the two apart: `AI::power()` sets 44100, while its `AI_DACRATE` write honors a literal zero — `dac.frequency = max(1, system.videoFrequency() / (io.dacRate + 1))` (`ares/n64/ai/io.cpp:61`), i.e. a written 0 asks for a ~48 MHz DAC and ares gives it. Distinguishing them requires a `dac_rate_programmed` flag, which adds a field to a serialized struct and therefore **changes the save-state layout (ADR 0005)** — an announced-in-advance change, not one to make in passing while fixing an unrelated livelock. It is unobservable in practice, since no title requests a ~48 MHz DAC, and the reset-state behavior (the half that caused the WDC livelock) is what this fix corrects. Ledgered here rather than left as an undocumented conflation, with the exact ares line that fidelity would require. **`real_pif_boot` CENSUS — the boot-path divergence is CLOSED, and one claim above is RETRACTED (2026-07-30).** Re-censusing all 66 titles under `real_pif_boot` gives **29 render, 24 issue zero RDP commands**, against **29 render, 25 zero** before the fix. The two paths now agree **exactly** — `hle_boot` is also 29/24 — so the sole boot-path regression the #202 census found (World Driver Championship, the one title `real_pif_boot` made strictly worse) is closed, and no title is now boot-path sensitive in the render/silent partition. **RETRACTION:** this row and #205's description said WDC gains "a real 625x237 frame". That was wrong, and wrong by the exact mechanism R-18 spent weeks unlearning, run in reverse — the scratch probe printed `scanout_scaled`'s **dimensions** and never counted content, and dimensions were read as a picture. Measured properly: WDC issues **175,815** RDP commands (from 0, near-exact parity with its 176,085 under HLE) with **0 lit pixels at every 60-frame sample**. The VI *is* programmed — the single-shot probe's 625x237 establishes that, so this is a real black frame and not a blanked VI — and R-18 already recorded the same shape under HLE: *"World Driver Championship 45 commands and exactly one distinct value (`0x0001`) — it clears and draws nothing."* So the correct claim is narrow and worth stating narrowly: **the AI livelock is removed and WDC's real-PIF behavior now matches its HLE behavior — it submits a full command stream and rasterizes black on both paths.** It does not render, and this fix never made it render. Why the count moves for zero-command but not for render: WDC clears the 1,000-command floor and fails the 1,000-lit-pixel floor, which is the two-term rule working as designed. |
| C-34 | The console **region** (and so the AI's video clock) is now selected from the cartridge's **destination code** (ROM header `0x3E`), rather than every cartridge booting NTSC. The destination characters come from the N64brew Wiki *ROM Header* §Standard-header table; the **50/60 Hz classification of each territory is the broadcast standard, not an N64-documented fact**, and there is **no oracle** for region detection | The machinery (`Region`, `Region::video_clock`, `Ai::set_region`, both region video clocks) already existed and was already documented, but **nothing drove it** — `set_region` was reachable only from the AI crate's own unit tests, so a PAL cartridge played at the NTSC rate. Wiring the existing selector is strictly better than leaving documented state unread; the classification is the weakest link and is labeled as such rather than presented as measured | absolute — a documented mapping, explicitly ungated | **Open (modeled, ungated) — 2026-07-28.** `Region::from_destination_code` maps the header byte, and `boot::apply_cartridge_region` applies it on **both** boot paths (`hle_boot` and `real_pif_boot`). Three cases are decided explicitly rather than guessed, and are pinned by name in the unit test: **`B` Brazil → NTSC** (Brazil broadcast PAL-**M**, a *60 Hz* standard, so the AI divisor behaves as NTSC); **`C` China → NTSC** (a PAL territory with no known retail N64 — the default is kept rather than inventing a 50 Hz cartridge that never shipped); **`A` "All" → NTSC** (names no single region). Every unrecognized byte also returns NTSC, so an unknown or homebrew code is **behavior-preserving**. Validated by a table test over all documented characters plus a mutation-checked end-to-end boot test (`a_pal_destination_code_retunes_the_ai_at_boot`: two ROMs differing **only** in byte `0x3E` must boot to different AI sample rates — removing the `apply_cartridge_region` call makes them identical and fails). **No oracle gates this**: no test ROM checks region detection, so it is modeled-and-ledgered in the same posture as R-16/R-17 rather than claimed as verified. **n64-systemtest impact: NOT MEASURED** — the suite has no region coverage, so it cannot establish an impact either way; the separately-observed suite total is unchanged at 90 |
| R-21 | `Fill Rectangle` (0x36) always writes the **`SET_FILL_COLOR` register**, whatever the cycle type. On hardware only FILL mode does that — in 1-/2-cycle mode the rectangle is rasterized through the **combiner/blender** like any other primitive | `fill_rectangle` calls `fill_pixel` unconditionally; the cycle type is never consulted. Every committed conformance vector that exercises `Fill Rectangle` sets FILL mode, so the non-FILL path has no coverage in either direction and the gap was invisible | absolute — a coverage boundary, not a fitted constant | **Open (found 2026-07-28, not yet oracled).** Surfaced *accidentally* while building the microcode end-to-end test: that test's queue initially mis-packed `SET_OTHER_MODES` (the cycle type went into word1 instead of word0 bits 21:20, so the RDP never entered FILL mode) and **the picture was still correct**, which is only possible because `Fill Rectangle` ignores the cycle type. A reviewer flagged the mis-packing; chasing it found this. The queue is fixed and the test now asserts the *emitted* `SET_OTHER_MODES` carries FILL, so the two are no longer confounded. **RESOLVED 2026-07-28 (oracle-confirmed).** Vector `fill_rect_1cycle_16` settles it: a 1-cycle rectangle with prim `(0x22,0x44,0x66)`, a combine selecting prim for RGB and alpha, and a *deliberately different* green fill register `0x07C1` renders **`0x2219` in all 64 pixels** — the prim color in RGBA5551, not the fill register. So hardware routes a non-FILL rectangle through the combiner and never reads the fill register. `fill_rectangle` now takes the combiner path (with alpha-compare and dither, as the triangle path does) unless the cycle type is `CYCLE_TYPE_COPY` or `CYCLE_TYPE_FILL`. Mutation-checked: reverting the guard reproduces `07 C1` against golden `22 19` at pixel (0,0), and *only* that vector regresses. A second vector `fill_rect_2cycle_16` pins the **2-cycle** branch, which the 1-cycle vector leaves untested because `combine()` takes a distinct path there (cycle 0 runs first and feeds cycle 1 as the `COMBINED` input): its combine makes cycle 0 emit the env color and cycle 1 select `COMBINED`, so env `0x8D73` means the chain ran, black means cycle 0 was skipped, and green `0x07C1` means the cycle type was ignored. Angrylion renders **`0x8D73`** and we already match. **Fallout worth recording:** six existing tests — the five `fill_rectangle_*` unit tests and the `golden_frame` end-to-end test — were named for FILL-mode behavior but **never selected FILL mode**, so they were passing on this bug. They now emit a `Set Other Modes` with `cycle_type = FILL` and test what their names claim. **Still open:** the same question for a *flat* `Fill Triangle` (0x08) with no shade/texture block, which likewise takes the fill register unconditionally (the presence of a shade/texture block selects the combiner there, not the cycle type); no vector exercises it yet. **n64-systemtest impact: not measured** — the suite has no RDP render-path coverage |
| R-17 | The AI DMA models the sample **rate** exactly (`MASTER_HZ / (video_clock / (DACRATE + 1))` per sample-pair) but charges **no DMA setup/arbitration latency and no RDRAM bank-state cost**: the transfer begins, and the start-interrupt fires, at the derived sample boundary rather than after the real DMA-engine delay. The underrun behavior is a defined **hold-and-decay** (integer `× 63/64` per sample) rather than the analog decay curve | The AI DMA is "directly connected to the DAC" and "progresses as samples are physically put through the DAC" (wiki §DMA), so the per-sample rate is the dominant timing term and is exact; the fixed setup latency `M` and the RDRAM bank costs are the same unmeasured constants flagged for the CPU/PI (they belong in this ledger with provenance when measured, never tuned). The decay shape is deterministic and no-`std`-friendly; ares uses `exp(-1/(freq·0.003))` | absolute — an unmeasured latency, not a differential re-phasing | **Open.** The rate and the FIFO/interrupt sequencing are unit- and integration-tested; the setup latency stays unmeasured (measure, don't guess) and the decay is defined-but-unpinned. No AI-timing oracle exists in the committed suites, so nothing gates it yet — to be validated against the project64 `DoubleShot` PCM ROM (Sprint 2) and any AI-timing capture that surfaces. **n64-systemtest impact: not measured** — no AI test drives the DMA-timing path, so the oracle count is **unchanged at 93** *(as-at — see the note above this table)* |
| R-15 | The **scissor** lower-right bound in FILL mode is **asymmetric**: the **X** bound is **inclusive** of its boundary pixel while the **Y** bound is **exclusive**, and a rectangle lying entirely at or past the scissor's right edge draws nothing (`allover`). `fill_rectangle` previously clipped both bounds exclusively (`(coord + 3) >> 2`) | Isolated cleanly against the Angrylion oracle by a scissor-clip fuzz batch (rectangles extending past the scissor on each edge). The X clip keeps the pixel containing `scissor.xl` (a rect spanning past `xl = 8.0` fills column 8), but the Y clip drops row `scissor.yl >> 2` (a scissor `yl = 5.0` fills up to row 4). The asymmetry is `edgewalker_for_prims`: the rectangle's `yl` is `\| 3`'d (FILL/COPY) so its own last scanline fills, but the scissor's raw `clip.yl` makes `invaly = k >= yllimit` drop that boundary row; the horizontal clip (`curover = xlsc >= clip.xl << 1`, `allover` ⇒ `!validline`) keeps the boundary column unless the whole span is over it. Read from the oracle's output, not computed | absolute — a rasterization geometry rule, oracle-confirmed | **Closed for the integer-coordinate FILL scissor (oracle-validated).** `fill_rectangle` now clips X inclusive with the `allover` guard (`rect_xh >= scissor.xl` ⇒ nothing) and Y exclusive, plus a hard width clamp. Pinned by a 48-vector scissor-clip fuzz family (`tests/vectors/fuzz/fz_scis_*`, all byte-exact) and the reconciled `fill_rectangle_is_clipped_to_the_scissor` unit test (which previously asserted an unverified exclusive X edge). Sub-pixel (fractional-coordinate) scissor edges remain unexercised. No n64-systemtest driver (count 93 *(as-at — see the note above this table)*) |
| R-18 | A **commercial ROM boots and executes real code but does not reach video** (the Phase 5 capstone). | Recorded at the time as an un-modeled retail OS-boot runtime; the actual causes proved to be an `ERET` interrupt livelock in the CPU and a PIF that reported four controllers on a one-pad console. | absolute — a coverage boundary across subsystems, not a fitted constant or a timing interval | **Capstone met.** Super Mario 64 renders its title screen and 29 of 66 staged titles render through their own microcode. 24 titles still issue zero RDP commands — they *halt*, each for its own reason, and each is a separate root-cause hunt. **Full record: [R-18](residuals/R-18.md).** |
| R-22 | The **RI register block** (`0x0470_0000..0x0470_0020`) is modeled as **plain storage**: writes stick, reads return them. Hardware read behavior differs for at least three of the eight — N64brew *RDRAM Interface* documents `RI_CURRENT_LOAD` as intended write-only, its read returning "a collection of bits from other registers" (`RI_ERROR` Ack, `RI_MODE` STOP_R, `RI_SELECT` TSEL[0], and two bits marked TOVERIFY), while `RI_ERROR` and `RI_BANK_STATUS` reflect controller state rather than the last write | Storage is enough for the only consumer today: IPL3 reads `RI_SELECT` and branches on it. Modeling the readback quirks would mean inventing the parts the wiki itself marks TOVERIFY | absolute — a coverage boundary | **Open, deliberately.** n64-systemtest has **no RI group**, so there is no oracle in the vendored set; per measure-don't-tune these stay honest storage rather than fabricated behavior. Decoding the block at all is what R-18 needed. The separate **RDRAM device registers** (`0x03F0_0000`) remain undecoded |
| R-23 | **CIC-6105 titles do not boot through `hle_boot`** (Banjo-Tooie, Ocarina of Time, Majora's Mask, and the rest of the 4.5% 6105 share). They boot correctly through `real_pif_boot` | The 6105 IPL3 is a *different program*: it opens with a self-descrambling XOR loop — `LW t0, -0xFF0(t1)` / `LW t2, 0x44(t3)` / `XOR t2, t2, t0` / `SW t2, -0xFF0(t1)` — over `t1`/`t3` that only the **real IPL2** leaves set. `hle_boot` seeds `sp` and `s3`-`s7` but not those, so the first load faults to KSEG3 and the machine sleds exactly as R-18 did. The values are not in the wiki's IPL1/IPL2 listing, so seeding them would be inventing a constant | absolute — a coverage boundary | **RESOLVED 2026-07-29.** Closed without inventing anything: the missing registers were **measured** by running the console's real IPL1/IPL2 out of a PIF ROM dump via `real_pif_boot`, capturing the register file at IPL3's entry (`0xA400_0040`), and keeping only the values **identical across ROMs of different CIC variants** (Banjo-Tooie/6105 vs Super Mario 64/6102): `at=1`, `a2=0xA400_1F0C`, `a3=0xA400_1F08`, `t0=0xC0`, `t2=0x40`, `t3=0xA400_0000`, `s4=1`, `ra=0xA400_1550`. `v0`/`v1`/`a0`/`a1`/`t4`-`t9` are deliberately **excluded** — they carry IPL2's running checksum of that cartridge's IPL3 and differ per ROM, so freezing them would fabricate a value the boot computes. `t3` is the decisive one: 6105's IPL3 descrambles itself by reading `0x44(t3)` = DMEM + 0x40, its own image. Corroborated independently — Banjo-Tooie under HLE now halts at **`pc=0x800329a8`, the exact PC `real_pif_boot` reaches**, with retired counts within 0.3%. All four staged 6105 titles boot (Banjo-Tooie, Donkey Kong 64, Ocarina of Time, Majora's Mask), the capstone's 6105 skip is deleted, and the T-71-003 witness set **doubled from 4 titles to 8** — Ocarina of Time alone now executes 733 distinct RSP instructions and submits **17 900 RDP commands**. n64-systemtest unchanged at 90. **Superseded scope note:** detects 6105 from the cartridge header and skips those titles with a message naming this row, rather than passing quietly; the real-PIF capstone boots them and asserts on them. Closing this means either deriving the IPL2 exit state or preferring `real_pif_boot` when a PIF ROM is available |
| R-19 | **The emulator hung on the n64-systemtest case `TLB: Execute mapped branch with a non-mapped delay slot`** — a mapped branch whose delay slot lies in an unmapped page. | A genuine loop rather than slowness, and the committed `systemtest` gate masked it by asserting Phase-1 *category* results only. | absolute — a hang is a coverage boundary, not a fitted constant | **Resolved 2026-07-24**, discovered and traced during the Stage-C/D timing work. **Full record: [R-19](residuals/R-19.md).** |
| R-20 | **64-bit addressing mode is not implemented** — the n64-systemtest `tlb64` group reports **18 failures** (14 `LW TLB Miss or Address Exception (64 bit addressing mode)` cases where `EntryHi`/`Context`/`XContext` read back `0` instead of the 64-bit VPN2, plus 4 `Loads from 32/64 bit address while using 64 bit addressing mode` returning wrong data). These tests run only in 64-bit addressing mode (`Status.KX/SX/UX = 1`) and exercise the `XKPHYS`/`XKSEG` segments and the `R` (region) field of the 64-bit `EntryHi`/`Context`/`XContext` decomposition | The emulator's segment map and TLB-miss register write-back model the **32-bit** address decomposition; the 64-bit `R:VPN2` layout (bits 63:62 region + the wider VPN2) and the wide-address segment ranges are not decoded, so a 64-bit TLB miss leaves `EntryHi`/`Context` at their reset `0`. **This cluster was masked by R-19**: the `tlb64` tests run *after* the delay-slot test that hung, so the suite never reached them — Phase 1's `Failed: 0` was only ever true *up to the hang point*, which is precisely the vacuous-pass failure mode the R-19 gate now witnesses against (`emux_exited`) | absolute — an address-decode / register-decode fact, not a timing interval | **Open — newly exposed, Stage D (CPU accuracy).** A genuine 64-bit-addressing feature gap (region-field decode + wide segment map + 64-bit miss write-back), not a regression from the R-19 fix (the fix touches only branch-delay-slot control flow). Pin against the `tlb64` group and implement the `R:VPN2` decomposition + `XKPHYS`/`XKSEG` ranges; read the expected `EntryHi`/`Context` values as a table from the suite's own assertions (do not compute against them — engineering-lessons §3.x). Surfaced 2026-07-24 the moment the suite could complete. **Progress 2026-07-24: 14 of 18 closed.** Root cause of the 14 `LW TLB Miss…(false, …)` cases was that **`EntryHi`'s VPN2/R was not written on a data address error** — the UM (§6.4.7) calls it "undefined", but the oracle pins `(VPN2 << 13) \| (R << 62)` from the faulting address, exactly as `Context`/`XContext` are already filled (which is why only `EntryHi` mismatched). Fixed by gating the `EntryHi` write on `writes_bad_vaddr` (address errors included), deleting the superseded `writes_tlb_context`, and replacing the wrong `an_address_error_leaves_entry_hi_alone` unit test with `an_address_error_writes_entry_hi_vpn2_and_region` (mutation-checked). Suite-wide 108→94. **Closed 2026-07-24 (18/18).** The last 4 were the `do_all_loads` battery (`Loads from 0x80/0xA0/0x90/0x98 … in 64-bit mode`). A focused reproduction harness (call `Pipeline::access_unaligned` directly with the four base addresses in 64-bit kernel mode, compare per-load against the ROM's `EXPECTED`) pinned the bug precisely and proved it **mode-independent**: **`mem::lwr` was unconditionally sign-extending**, but the VR4300 sign-extends `LWR` only for the **full-word** case (`byte == 3`, which writes bit 31); a **partial** `LWR` (bytes 0–2) leaves bits 63:32 of `rt` UNCHANGED. The `tlb64` battery exposes it because its sentinel's upper half (`0xBEEF_0000`) is non-zero — `LWL`, `LDL`, `LDR` all passed (they always write bit 31 or the whole register). Fixed in `mem::lwr` (sign-extend iff `byte == 3`, else preserve `rt & 0xFFFF_FFFF_0000_0000`), pinned by the mutation-checked `a_partial_lwr_preserves_rt_upper_half_and_only_the_full_word_sign_extends`. Result: **Phase 1 categories `Failed: 0` with the suite running to `xioctl(EXIT)`; suite-wide 94 → 90** (the rest are RSP/RCP/RDP, later phases). With R-20 closed, `tests/systemtest.rs` gained the `emux_exited` **completion witness** promised in R-19, so this class of mid-suite hang can never hide behind a partial Phase-1 zero again |

Every entry must carry a **classification** of the failing measurement as **absolute** or
**differential** before any mechanism is proposed (ADR 0005, `engineering-lessons.md` §1.3). A
differential measurement — the interval between two events on the same clock — is invariant
under uniform re-phasing, so an entire family of plausible fixes is ruled out for free. A sibling
project implemented and rolled back five successive re-phasings before recognizing this. One line
here saves that.

---

## 3. Ruled-out approaches

| # | Approach | Applied to | Why it cannot work | Date |
| --- | --- | --- | --- | --- |
| — | none yet | — | — | — |

Record an approach here after **two** rollbacks, not after five (`engineering-lessons.md` §3.3).
An unrecorded dead end gets rediscovered by the next person, or by the same person in six months.

---

## 4. Contradictions in the sources

Not our bugs, but they will look like ours if undocumented.

| # | Contradiction | Sources | Resolution |
| --- | --- | --- | --- |
| S-1 | `SYSCMD` bit 4 polarity: command = 0 or 1? | UM §12.11.1 vs `SysAD Interface.md` cheat sheet | **RESOLVED — not a contradiction at all**; see below |
| S-2 | Pipeline stage names | `ref-docs/research-report.md` §1 says IF/RF/EX/DF/WB; UM §4.1 Fig 4-1 says IC/RF/EX/DC/WB | **resolved** — manual wins; see `ref-docs/2026-07-20-vr4300-timing-supplement.md` §1 |
| S-3 | Exception vector for an exception with `EXL` already set | UM Fig. 6-15 (p. 203) says `0x080`; UM Table 6-4 + §6.4.8 say `0x180`; CEN64 routes to `0x180` | **RESOLVED — `0x180`**; the manual contradicts *itself*, and Fig. 6-15 is the defective source. See below |
| S-4 | D-cache fill cost | CEN64 charges 44 PClocks; ares charges 40 | **unresolved** — neither is spec-derived; supersede both with C-1 |

### S-3 — resolved: the contradiction is *inside* the manual, and `0x180` wins

Recorded as MIPS-docs-vs-CEN64. It is neither: the VR4300 manual disagrees with itself, and the
majority of it says `0x180`.

**For `0x180` — three places, two of them normative tables:**

- **Tables 6-3/6-4 (p. 181)** define the refill offsets *only* for `EXL=0`: the rows are labeled
  `TLB Miss, EXL=0` → `0x000` and `XTLB Miss, EXL=0` → `0x080`. Everything else is `Other` →
  `0x180`. There is no `EXL=1` refill row to select.
- **§6.4.8 (p. 187)**, *Processing*: *"All TLB Miss exceptions use these two special vectors when
  the EXL bit is set to 0 in the Status register, and they use the common exception vector when
  the EXL bit is set to 1 in the Status register."*
- **§6.4.8 (p. 188)**, *Servicing*, describing a nested refill: *"This second exception goes to
  the common exception vector because the EXL bit of the Status register is set."*

**For `0x080` — one flowchart:** Fig. 6-15 (p. 203) has a branch `EXL = 0?` whose **No** arm
leads to a box reading *"General Purpose Exception, Vec. Off. = 0x080"*. That figure is wrong. It
contradicts both tables, the §6.4.8 prose twice, and Fig. 6-14 (p. 201), which is the
general-purpose handler and unconditionally uses `+ 0x180`.

**So CEN64 is right**, and its source comment that `0x080` *"doesn't make any sense"* is a
reaction to exactly this figure. Resolution is by document, not by measurement, so no test ROM is
required — but a pin is still worth having as a regression gate, and n64-systemtest exercises it
directly (it installs handlers at all three of `0x000`, `0x080` and `0x180`).

Kept rather than deleted: Fig. 6-15 is still in the manual, so the next reader will find `0x080`
and have to re-derive this. **Owner:** Sprint 2, with the pin.

### S-1 — resolved: the sources agree on the bits and disagree on the English

Recorded as a contradiction, and it is not one. Reading both carefully:

- **UM §12.11.1**: *"During address cycles \[`SysCmd4` = 0\] … contains a System
  interface command"*; *"During data cycles \[`SysCmd4` = 1\]"*.
- **The wiki cheat sheet**: read/write **requests** carry bit 4 = **0** (its
  column says "Data req"); data-carrying cycles carry bit 4 = **1** (its column
  says "Command").

So both sources put a request at bit 4 clear and a data beat at bit 4 set. They
differ only in that the wiki calls the *data-identifier* cycle "Command". No test
ROM is needed. We follow the manual's naming, since it is the vendor spec and the
rest of the CPU crate cites it.

Worth keeping as an entry rather than deleting: the next reader will hit the same
apparent conflict, and "resolved, and here is why it looked wrong" is more useful
than silence.

---

### C-8 — COP0 CO `funct` 0x20-0x3F retires as a no-op

**Claim.** A COP0 CO-class instruction whose `funct` is in `0x20..=0x3F` retires
with no architectural effect, rather than raising Reserved Instruction.

**Basis: inference, not a manual citation.** The VR4300 manual does not enumerate
this range. The inference is from n64-systemtest's own structure: it probes for
the `emux` emulator by executing `COP0 CO funct 0x20` from `init_allocator`,
inside `entrypoint` — **before** `main` installs any exception handler. An RI
there would derail the suite on every N64 it has ever run on, before it printed a
line. The suite's constant for the probe is named
`XDETECT_CODE_EXTENSIONS_20_3F`, i.e. emux claims exactly this range as extension
space, which only works if hardware leaves it inert.

**Untested.** Whether the target GPR is written (and with what) is unknown; we
leave it untouched, so a probe reads back its prior value and concludes emux is
absent. That is the correct outcome here but is not evidence about hardware.

**Confirm with:** a hardware run of an `XDETECT` word with a known GPR value.

### C-9 — PI direct-I/O write latch duration is fitted, not measured

**Claim.** A PI direct-I/O write latches its value and shadows every PI-bus read
for `Bus::PI_WRITE_CYCLES` (100) RCP cycles.

**Documented part.** The *behavior* is from N64brew *Memory map* (PI external
bus): writes are asynchronous, the PI latches the value and releases the CPU
immediately, `PI_STATUS.IOBUSY` reports the in-flight write, further writes are
ignored, and reads from **any** address return the value being written. The PI
does not know a device is read-only, so ROM writes follow the same path and are
dropped by the ROM.

**Undocumented part — the duration.** Hardware finalization depends on the PI
domain timing registers (`LAT`/`PWD`/`PGS`/`RLS`), which we do not model.
n64-systemtest bounds the latch only relatively: visible after 0 decay-loop
iterations, gone after 110. The constant was chosen by trying values against the
suite and keeping the best.

**Known-wrong, deliberately.** `cart-writing: Write32, Read32 (same location)`
still fails on its **second** read, where hardware has finalized and we have not.
No single constant closes that, because the real duration is not constant. This
is recorded as a fitted approximation rather than presented as accurate.

**Confirm with:** modeling the PI domain timing registers and deriving the
finalization time, then deleting the constant.

### C-10 — FP arithmetic is correct only in round-to-nearest-even

**Claim.** `fpu::{add,sub,mul,div}_{s,d}` compute with Rust's native `+`/`-`/
`*`/`/`, which round to nearest-even unconditionally.

**Why that is wrong.** The VR4300's `FCSR.RM` selects one of four rounding modes
(nearest, toward zero, toward +inf, toward -inf), and `FCSR.FS` flushes denormal
results to zero. Neither is consulted. Every operation whose exact result is not
representable therefore has a wrong last bit under any mode except `RM = 0`.

**Evidence.** n64-systemtest sweeps rounding modes and reports 63 `Result after
MUL.S`, 54 `Result after DIV.S`, 39 `Result after ADD.S` failures (and the `.D`
equivalents) — on operations that *are* wired and do execute.

**Not fixable by wiring.** The arithmetic core itself is mode-blind. `no_std`
Rust has no `fesetround`, so directed rounding has to be produced explicitly:
compute exactly in wider precision and round per `RM`, or use a soft-float
implementation. Both need their own golden vectors.

**Note the asymmetry.** `to_i32`/`to_i64`/`round_f64` already take a `Rounding`
argument, so the conversions were written mode-aware and the arithmetic was not.
The gap has existed since Sprint 3 and was invisible because nothing decoded to
the arithmetic until COP1 was wired.

**A fix was attempted and reverted.** Routing `ADD.S`/`SUB.S`/`MUL.S` through an
exact `f64` computation rounded per `RM` changed **nothing** the oracle measures
(2,897 before and after) and made `ADD.S` marginally worse, 39 failures to 40.
Two lessons, both recorded rather than discarded:

1. The exactness argument (53 significand bits ≥ 2×24+2) holds only in the
   **normal** range. An `f64` value that is subnormal as an `f32` has already
   lost bits to the narrower exponent range, so converting it double-rounds. A
   correct implementation must never leave the target format — i.e. soft-float.
2. **The rounding mode is not what these tests are failing on.** The hypothesis
   was plausible and measurably wrong, so the cause of the ~250 `Result after
   <op>` failures is still unidentified. Do not assume `RM` next time.

The helper (`fpu::round_f64_to_f32`, `next_up_f32`, `next_down_f32`) is retained
with its tests: it is correct in the normal range and will be needed.

**Measured, and it is not an arithmetic problem at all.** The verbatim failure:

```text
'COP1: ADD.S' with '(false, Nearest, 0.0, 2e0, Ok((, 2e0)))' failed:
  a=1.2795344e-28 b=2e0 (0x11223344 vs 0x40000000)
```

`0x11223344` is the test's **sentinel**, unchanged — the destination is never
written. And the mode is `Nearest`, so `RM` was never implicated. Both earlier
hypotheses (unwired operations, exception behavior) and the rounding hypothesis
are now all excluded by measurement.

A neighboring case is more informative still: `Upper bits of 32 bit operation
(half mode)` reports `0x1111_40C0_0000` against an expected `0x40C0_0000`. There
the low word **is** correct (`0x40C00000` = 6.0) and the *upper* half of the FGR
retains its sentinel. So the arithmetic works and the write-back width or path is
wrong — a 32-bit FP result apparently must not leave the upper half intact.

**Next:** determine why `fd` is unwritten in the main path while the "upper bits"
case does write. Candidates:

- ~~the result never leaves the FPR because `SWC1` does not store, or the
  operands are never loaded by `LWC1`~~ — **eliminated**: `LWC1`/`LDC1`/`SWC1`/
  `SDC1` are all decoded *and* executed (`Pipeline`, the FP load/store arm), so
  the transfer path exists.
- the `Cop1Access::Arith` request is dropped between EX and WB, so `fp_arith`
  never runs for these cases;
- or it runs and writes, but the test reads the register through a path whose
  view disagrees — note the failing tuple begins `(false, …)`, and the
  neighboring failure is explicitly labeled **"half mode"**, which is what
  `Status.FR = 0` is called. Under `FR = 0` a 32-bit result and a 64-bit read
  disagree about which FGR half holds it, and `0x1111_40C0_0000` — correct low
  word, sentinel upper half — is exactly that shape.

The second and third are distinguishable in one run: dump the FPR immediately
after an `ADD.S` retires and compare against what the test reads back. **Do not
assume the third is right because it is the tidiest** — that reasoning has now
failed nine times in this ticket.

**Targeted run: the arithmetic is CORRECT; the write-back is not.** Breaking on a
real `ADD.S fd=4, fs=0, ft=2` and reading the raw FGRs afterwards:

```text
fd_raw = 0x0011_0011_4000_0000   <- low word 0x40000000 = 2.0, correct
fs_raw = 0x0000_1111_4000_0000
```

The low word is right. The **upper half retains its sentinel**, and the suite
expects `0x4000_0000` — matching the `Upper bits of 32 bit operation (half mode)`
case, which reports `0x1111_40C0_0000` against an expected `0x40C0_0000`. So
every hypothesis about the *arithmetic* was aimed at the wrong half of the
register.

**Re-reading the same probe output shows the operands are wrong too.** For the
case `(false, Nearest, 0.0, 2e0, …)`:

```text
fs_raw = 0x0000_1111_4000_0000   low = 0x40000000 = 2.0   <- correct operand
ft_raw = 0x0000_0000_0123_4567   low = 0x01234567         <- a SENTINEL, not 0.0
```

`ft` never received `0.0`; it still holds a fill pattern. The result was only
**coincidentally** correct, because `2.0 + 3e-38` rounds to `2.0` — which is
exactly the kind of accident that makes a broken path look healthy.

So the operand **load** looks implicated as well as the write-back.

**But both conclusions rest on a comparison that may not be valid.** The probe
captured the *first five* `ADD.S` sites in the run and compared their registers
against a failure message from a *specific* test case. Nothing correlates the
two: those `ADD.S` instances may belong to entirely different tests, possibly
ones that pass. `LWC1` has since been read and is correct
(`write_s(ft, v)`, preserving the upper half as it must), which is evidence
against the operand-load theory and a reason to distrust the pairing.

**Treat as established:** the FPU is not validated, and both "the arithmetic is
correct" and "the operands are wrong" are unproven.

**The probe has to correlate.** Break on the `ADD.S` reached from the failing
test — identify it by symbol or by the operand values the test names — rather
than on the first `ADD.S` encountered. Uncorrelated captures produced two
confident and unfounded conclusions here, one of which was used to justify a code
change.

**Correlated run: zero hits.** Scanning the whole run for an `ADD.S` whose
operands are `0.0` and `2.0` — the pair the failing case names — found **none**.
Taken at face value that says the failing test's `ADD.S` **never executes**, which
would explain the untouched `0x11223344` sentinel far better than any theory
about the FPU, and would move the investigation upstream to whatever aborts each
case before it reaches its instruction.

**One caveat keeps this from being conclusive.** The probe samples `fs`/`ft` when
the PC *reaches* the `ADD.S`, but the pipeline is five stages deep, so a load
feeding those registers may still be in flight — a real `ADD.S` with the right
operands could read as a miss. Confirm by sampling at **retirement** rather than
fetch, or by counting `ADD.S` executions of any operands and comparing against the
number of `COP1: ADD.S` cases the suite reports. Only then is "never executes"
established.

Stated this way deliberately: the two previous conclusions in this entry were
recorded as facts on comparable evidence and both had to be retracted.

**Falsification test run; the lead is REFUTED.** `ADD.S` is fetched **3,074**
times against **70** reported `COP1: ADD.S` cases, so the instruction executes
freely. The zero correlated hits was precisely the pipeline artifact flagged
above — operands sampled at fetch have not been loaded yet by instructions still
in flight.

The caveat did its job: the hypothesis died on its own test instead of becoming a
third retraction. That is the only method in this entry that has worked.

**Where COP1 actually stands.** Excluded by measurement, not argument:
unwired operations; exception behavior; rounding mode; a write-back width fix;
operand-load failure; and now "the instruction never runs". The cause remains
**unidentified**, and the honest position is that a correlated capture at
*retirement* — matching the specific failing case — has still not been performed.
Every shortcut around that has cost a wrong answer.

**A fix was attempted and reverted.** Writing the full 64-bit FGR
(`write_raw`, zeroing the upper half) moved the failure count by **nothing**
(2,897 either way) and bypasses the `FR` view — the precise mistake ledger U-7
records. Under `FR = 0` a `.S` destination is not simply "FGR *fd* low half", so
`write_raw` cannot be right even where it looks right. The correct change has to
express what a single-precision *arithmetic* write-back does **through** the
`FR` view, and that is not yet known.

**Run done. FPR writes do occur**, so the `Arith` request is not being dropped
wholesale — candidate two is weakened. (Watching all 32 raw FGRs, values change
during the COP1 phase; pairs appearing to change together are an artifact of
`step_to_next_edge` advancing several cycles per observation, not aliasing.)

That leaves the `FR = 0` view as the live candidate, but it is **not confirmed**:
"writes happen somewhere" is much weaker than "the write for *this* `ADD.S` lands
where the test reads". The next probe must be *targeted*, not global — break on
the specific `ADD.S`, then read back `fd` through both `read_s` and `read_d` and
compare with the `0x11223344` the test sees. A global FGR watch cannot answer it,
which is worth stating because this run looked informative and was not.

### C-10 RESOLVED — the cause is `MOV.S`, and it is not in the FPU at all

The correlated capture at retirement was finally performed, and it identifies the
cause outright. Two things made it work where nine previous attempts failed:

1. **The correlation trigger is the suite's own progress marker.** Capture arms
   only after `Running COP1: ADD.S...` appears in the ISViewer stream, so the
   captured `ADD.S` is provably the failing test's. Every earlier probe took the
   first `ADD.S` in the run and hoped.
2. **The capture is of the instruction stream, not of the registers.** Dumping
   `(pc, word)` either side of the site answered in one run a question that four
   register-watching probes could not.

The site is `0x8000_5FE4`, and the eight words around it are the whole story:

```text
80005FD4  46006006   MOV.S $f0, $f12     <- argument 1
80005FD8  46007086   MOV.S $f2, $f14     <- argument 2
80005FDC  C424FA10   LWC1  $f4, ...      <- the 0x4B3C614E sentinel
80005FE0  00000000   nop                 <- the test's BRANCH_INSTRUCTION slot
80005FE4  46020100   ADD.S $f4, $f0, $f2 <- the instruction under test
80005FE8  00000000   nop
80005FEC  03E00008   jr    $ra
80005FF0  46002006   MOV.S $f0, $f4      <- delay slot: THE RETURN VALUE
```

`MOV.fmt` is COP1 funct **6**. The decoder admits funct `<= 3` to `Op::FpArith`
and sends everything else to `Op::Cop1Unimplemented`, which executes as a no-op.
So **all three `MOV.S` in this one function do nothing**:

- the two operand moves never run, so `$f0`/`$f2` hold whatever a previous test
  left — which is why the probe saw `$f2 = 0x0000_0000_0123_4567`, a stale fill
  pattern, and read it as "the operand load is broken";
- the return move never runs, so the caller reads a `$f0` the callee never wrote.
  `0x1122_3344` is simply what was in `$f0` at that moment, left by the earlier
  `full_vs_half_mode` tests. It is **not** a sentinel belonging to this test —
  the string `0x11223344` does not occur anywhere in `AddS`'s source.

`ADD.S` itself is fine: the trace shows `$f4` going `0x0011_0011_4B3C_614E` →
`0x0011_0011_4000_0000`, i.e. exactly `2.0`, the expected result. **Every one of
the ~250 `Result after <op>` failures was reported against a value the tested
instruction never produced.** The constant `a = 0x11223344` across all 30-odd
`ADD.S` cases regardless of operands was the tell, and it was visible in the very
first capture of this entry.

What this retires:

- the `FR = 0` view — the last live candidate — is **excluded**. `Status.FR` is 1
  here (IPL3 leaves it set), and the leading `false` in the failing tuple is
  `flush_denorm_to_zero`, **not** `FR`. That misreading survived three rounds.
- "the arithmetic is correct" is now **confirmed** rather than retracted, on
  evidence that actually correlates.
- "the operands are wrong" is confirmed *as an observation* and **misattributed**
  as a diagnosis: the operands are stale because the moves that set them no-op,
  not because `LWC1` is broken.

**Method note, since this entry is mostly a record of being wrong.** Nine
hypotheses were formed by reasoning about the FPU and every one was wrong. The
tenth was formed by reading the eight instructions the test actually executes,
and it was right immediately. The prior probes all watched *state* and inferred
*cause*; this one read the *code*. When a value looks stale, dump the instruction
stream that was supposed to write it before theorizing about the writer.

**Fix:** decode and execute the remaining COP1 funct space — funct 4-7
(`SQRT`, `ABS`, `MOV`, `NEG`) first, since `MOV` is load-bearing for every
compiled FP call, then the conversions and `C.cond.fmt`.

### C-11 — the IEEE flags are barely detected, which is what gates the FP traps

**Claim.** `fpu::classify_f32`/`classify_f64` set `invalid`, `div_by_zero` and
`overflow`, and set `inexact` **only as a side effect of overflow**. `underflow`
is never set at all.

**Why it matters more than it looks.** Enabled FP traps were implemented (COP1
`Cause`/`Enable` are compared, `Exception::FloatingPoint` is raised, `fd` is left
unwritten, the sticky `Flags` are not accumulated, the instruction does not
retire — all four pinned by mutation-tested unit tests). Against the oracle it
moved n64-systemtest by **one assertion**, 2,795 → 2,794.

That is not a defect in the trap path; it is the trap path being unreachable. A
trap fires only when a *raised* condition meets a *set* enable, and `inexact` is
the condition most of the suite's cases raise. With `inexact` undetected, both
halves of every such case fail: the untrapped half on
`FCSR after <op> with exceptions disabled`, and the trapped half by never
trapping.

The verbatim shape, for `f32::MIN + (-1.0)`:

```text
'COP1: ADD.S' with '(false, Nearest, -3.4028235e38, -1e0, Ok((inexact, …)))'
   a = FCSR { flags: ,        causes: "" }
   b = FCSR { flags: inexact, causes: " inexact" }
```

The *value* is right; only the flags are missing.

**Why it is not a small fix.** Detecting `inexact` requires knowing the exact
result, which the native `f32`/`f64` operators discard. For `MUL.S` the exact
product of two `f32`s fits an `f64` exactly (≤48 significand bits, exponent well
inside `f64`'s range), so a compare-after-round works. **For `ADD.S`/`SUB.S` it
does not**: the exact sum of `2^127` and `2^-149` needs ~277 significand bits, so
the `f64` sum is itself rounded and the comparison silently becomes a guess. A
correct implementation needs an error-free transformation (2Sum) or a soft-float
path that never leaves the target format — the same conclusion C-10 reached for
directed rounding, arrived at from a different direction.

**Recorded rather than fitted.** The tempting move is to declare `inexact` on an
`f64` round-trip mismatch and take the numbers. That is exactly the "fitted
constant" this file exists to refuse: it would be right in the normal range,
wrong in the range that the suite deliberately probes, and every later FP result
would stop being evidence.

**Not yet handled either:** the unmaskable **unimplemented-operation** cause
(bit 17). The VR4300 raises it for subnormal operands and results, which this
FPU computes normally instead; the suite's `expected_unimplemented` cases fail
for that reason and not because of the enables.

### C-11 RESOLVED — soft-float, and the fix uncovered a second bug

`crates/rustyn64-cpu/src/softfloat.rs` computes both formats and all four
operations from unpacked `(sign, significand, exponent)` triples in `u128`,
rounding **once** at the end. Discarded bits are folded into a sticky bit rather
than dropped, which is what makes `inexact` exact rather than approximate.
`FCSR.RM` falls out of the same step, closing the rounding-mode half of C-10
as well.

n64-systemtest: **2,794 → 2,682**.

**How it is known to be right.** The soft-float is checked against an
independent oracle — Rust's own `f32`/`f64` operators — with the requirement
that in round-to-nearest its result is *bit-identical* for every case in three
corpora: 40,000 uniformly random bit patterns (which are mostly extreme
exponents), 40,000 draws from the ordinary numeric range (where cancellation
happens), and 20,000 around the subnormal boundary. The flags come from the same
rounding step as the value, so a value that matches bit-for-bit is real evidence
that the guard/sticky bookkeeping the flags are read from is right. Testing the
flags alone would have been self-referential: there is no second implementation
here for them to disagree with. Rounding-mode results are pinned separately
against vectors transcribed from n64-systemtest.

**The measurement did not move on the first attempt, and that was the useful
part.** Wiring the soft-float in produced 2,794 → 2,794, with the suite
reporting `flags: inexact` but `causes: ""`. The sticky half was surviving and
the per-operation half was gone — the signature of *a later instruction
overwriting `Cause`*, not of a flag never raised. The culprit was mine: the
`ABS`/`MOV`/`NEG` path added in the previous change cleared `FCSR.Cause`, on no
evidence. Because the compiler emits `MOV.fmt` to move an FP return value, a
`MOV` sits between almost every arithmetic operation and the `CFC1` that reads
its result, so it erased exactly the bits the program was about to inspect.
`MOV`/`ABS`/`NEG` now leave `FCSR` untouched: the architectural rule is that
`Cause` is written by operations that *can* raise, and these cannot. That alone
was worth 112 assertions, and it is pinned by a named regression test.

Twice now in this ticket an invented value has cost more than the feature it was
attached to (the other being ledger U-7's premise). Both were written as
plausible-looking one-liners with no citation.

**What remains, and it is not flags.** Every surviving `ADD.S` failure is a
subnormal case: either `Err(())` — the suite expecting the unmaskable
unimplemented-operation cause — or an `FS = 1` flush-to-zero case whose result
is rounding-mode dependent. The normal range passes.

**Where things stood at the time of this entry** (kept in past tense, because a
ledger read top-to-bottom should show what was believed *when* each entry was
written, not be silently back-edited): the dominant remaining block was the
still-undecoded COP1 funct space — `C.cond.fmt` and the `CVT`/`ROUND`/`TRUNC`/
`FLOOR`/`CEIL` conversions, roughly 1,700 of the 2,682. Both are now wired and
the compares pass outright; see **C-12** below, and `docs/STATUS.md` for the
current count.

### C-12 — the VR4300's NaN convention is inverted from IEEE-754:2008

**Claim.** A NaN is **signaling** when its significand's most significant bit
is **set**, and quiet when clear — the *legacy MIPS* convention, the opposite of
IEEE-754:2008 and of every modern language. `0x7FC0_0000`, which Rust produces
as `f32::NAN` and which everything else calls quiet, raises Invalid on this
processor.

**How it was established.** From n64-systemtest's own expectations, which name
their constants by the IEEE convention and then assert the opposite behavior.
For a *non-signaling* compare (`C.EQ`, `C.F`, …) it expects:

| Operand | IEEE name | Expected | Implies |
| --- | --- | --- | --- |
| `0x7FC0_0000` (MSB set) | "quiet" | **Invalid raised** | signaling here |
| `0x7FBF_FFFF` (MSB clear) | "signaling" | no flags | quiet here |

The *signaling* compare forms (`C.SF`, `C.SEQ`, …) raise Invalid for both,
which is the ordinary IEEE rule for those forms and therefore does **not**
distinguish the conventions — checking only those would have left the question
open. It is the non-signaling forms that settle it.

**The corroboration that makes it more than a curve fit.** The processor's own
default NaN result is `0x7FBF_FFFF` / `0x7FF7_FFFF_FFFF_FFFF`, MSB **clear**.
Read as IEEE, that is a processor whose invalid-operation result is a
*signaling* NaN — which would re-trap the instant anything touched it. Read
under this convention it is exactly what it must be: quiet. Two independent
facts, from different tests, agreeing on the same inversion.

**Effect:** n64-systemtest 1,468 → **1,098**, and it took the compare block from
42 failures apiece to **zero across all sixteen**.

**Where it bites.** `fpu::is_snan_{f32,f64}` and `softfloat::unpack`. Both now
name the bit for its *position* rather than calling it a "quiet bit", because a
constant named `quiet_bit` that is tested for signaling is a trap for the next
reader. The tests name their patterns `vr_snan`/`vr_qnan` for the same reason,
and one asserts `is_snan_f32(f32::NAN)` explicitly — that is the case most
likely to be "fixed" back to IEEE by someone who has not read this entry.

**Adjacent, and since RESOLVED in C-13:** an **IEEE-signaling / VR4300-quiet**
NaN operand (MSB clear) to an arithmetic operation raises **unimplemented
operation** rather than nothing — the VR4300 cannot propagate one in hardware.
When this entry was written that was still open and the arithmetic tests failed
on NaN inputs; **C-13** implements it, and they no longer do.

Marked rather than rewritten, per this file's own rule: what each entry believed
when it was written is the record worth keeping.

### C-13 — the VR4300 cannot compute with subnormals, and says so

**Claim.** This FPU has no subnormal datapath. Rather than producing a
subnormal, or silently flushing one, it raises the **unmaskable
unimplemented-operation cause** (`FCSR.Cause.E`, bit 17) and traps. There are
four distinct occasions, and they are not interchangeable:

| Occasion | Applies to |
| --- | --- |
| A **subnormal operand** | `ADD`/`SUB`/`MUL`/`DIV`, `ABS`/`NEG`, the conversions |
| A **subnormal result** with `FCSR.FS` clear | the same, plus narrowing `CVT.S.D` |
| A **subnormal result** with `FS` set *and* underflow or inexact **enabled** | as above |
| An **MSB-clear NaN** operand (quiet by this processor's convention, C-12) | arithmetic, `ABS`/`NEG`, conversions |

Only with `FS` set and both of those enables clear does it flush — and **where
it flushes to depends on the rounding mode**: `±0` under nearest and
toward-zero, but the smallest **normal** of that sign under a mode that rounds
away from zero, because zero is on the wrong side of the true result.

**Effect:** n64-systemtest 1,098 → **584**. `ADD.S`, `SUB.S`, `ADD.D`, `DIV.D`,
`ABS.*` and `NEG.*` reached zero failures; the `CVT.W`/`CVT.L` families and
`CVT.D.fmt` fell off the list entirely.

**Three things this surfaced that are easy to get wrong:**

1. **`MOV` is not `ABS`/`NEG`.** All three look like sign-or-bit manipulation
   and only `MOV` is: `ABS`/`NEG` classify their operand, raise Invalid on a
   signaling NaN, and **replace** `FCSR.Cause`, while `MOV` transports the
   bits and leaves `FCSR` completely alone. The oracle settles it by
   *construction* rather than by description — `MOV.S` is driven through
   `test_floating_point_f32_which_preserves_cause_bits` and `ABS.S`/`NEG.S`
   through the ordinary harness that asserts `Cause` was cleared. Treating all
   three alike was worth 52 assertions. Note the earlier finding that `MOV`
   must *not* touch `Cause` (C-10) remains correct; it simply does not
   generalize to its neighbors.
2. **Compares are exempt.** "This FPU cannot do subnormals" sounds like it
   should be universal and is not: `C.cond.fmt` compares a subnormal as an
   ordinary number and raises nothing. Applying the rule there would have
   regressed all sixteen compare tests, which had just reached zero.
3. **An out-of-range float-to-integer conversion is unimplemented, not
   Invalid.** IEEE says Invalid and `fpu::to_i32` reports that; the VR4300
   declines instead. The translation happens at the call site, so the IEEE
   answer stays available to anything that wants it.

**Both follow-ups from this entry are now CLOSED.** `CVT.S.D` routes through a
narrowing `softfloat::convert` that rounds once and honors `FCSR.RM`, and
`SQRT` is implemented in `softfloat::sqrt` and decoded. n64-systemtest 584 →
**508**; `SQRT.S`/`SQRT.D` reached zero and `CVT.S.fmt` fell 21 → 10.

`sqrt`'s sticky bit is exact rather than estimated: `u128::isqrt` returns the
floor of the root, and the root is exact precisely when `q * q == n`, so that
comparison **is** the sticky bit. (An earlier version of this sentence claimed
it avoided re-squaring, which the code never did — the same wrong claim reached
three files before review caught it.)

### C-14 — `FR = 0` is not the "FGR pair" model

**Claim.** With `Status.FR = 0` the register file presents **16** usable 64-bit
registers: FPR *n* addresses **FGR `n & !1` in its entirety**, and odd FGRs are
not addressable at all. A 32-bit access picks a half of that register — the low
half for an even register number, the **high** half for an odd one.

**What it replaces.** This module implemented the natural reading of "`FR = 0`
uses register pairs": the value is `FGR[n+1]:FGR[n]`, assembled from two
registers' *low halves*. That model round-trips through `DMTC1`/`DMFC1`
perfectly, which is why it survived — every test that wrote and read through the
same path agreed with it.

**What refutes it.** n64-systemtest writes an odd register in half mode and then
reads *both* registers back in full mode:

```text
MTC1 $1, <0x01234567>          ; half mode
DMFC1(0) == 0x01234567_89ABCDEF ; landed in FGR0's HIGH half
DMFC1(1) == 0x44445555_66667777 ; UNCHANGED -- the pair model writes here
```

The second line is the one that matters: under the pair model FGR1 is where the
value goes, so an implementation cannot satisfy both.

**A second behavior fell out of the same tests.** A single-precision
**arithmetic** result *clears* the other half of its destination, while
`MTC1`/`LWC1` *preserve* it. Both write 32 bits to the same place, so one
`write_s` for both is the natural implementation — and the difference is
invisible until something reads the register at a different width, which is
exactly what `DMFC1` after an `ADD.S` does. They are now `write_s` and
`write_s_arith`.

**And a third:** `MOV.S` moves **all 64 bits**, not the formatted half. The
suite reads the destination after a `MOV.S` and expects the *source's* upper
half there. It is a whole-register transfer that happens to be spelled `.S`.

**A second, independent fix landed alongside it.** C-13's subnormal-result
policy triggered on *"the result is subnormal"*, which misses a result that
underflows **past** the subnormal grid to zero — `f64::MIN_POSITIVE` narrowed to
`f32`, or `MIN_POSITIVE` squared. Both conditions are needed and neither implies
the other: `is_subnormal` misses the rounds-to-zero case, and `flags.underflow`
misses an *exact* subnormal, because IEEE signals underflow only when tiny **and
inexact**. Replacing the first test with the second rather than adding to it was
tried and regressed the oracle from 89 to **131**, caught immediately by the
existing tests. Worth 22 assertions once correct.

**A third fix, in the same area.** A float-to-`.L` conversion refuses a
magnitude of **`2^53`** or more — far narrower than `i64`, and bracketed by the
suite rather than assumed: `9007198717870080` converts and `9007199254740992`
does not, both comfortably inside `i64`. `2^53` is the last integer a `double`
represents exactly, so the natural reading is that the conversion runs through
double precision internally and declines whatever it cannot hold. Worth 7
assertions.

The limit is applied to `.W` targets too, where it is **unobservable** — `2^53`
is far outside `i32`, so such a value is refused either way. It was first
guarded on the target width; the guard was removed when a mutation test could
not distinguish the two. An undistinguishable branch is one that rots.

**Effect:** Phase 1's categories 99 → **60**; the whole odd-index cluster
(`MTC1`/`MFC1`/`DMTC1`/`DMFC1`/`LWC1`/`SWC1`/`LDC1`/`SDC1` "with odd index in
32 bit mode", plus the half-mode comparison and 64-bit-index tests) reached
zero.

**Note this supersedes a documented guess.** `fpr.rs` previously recorded
forcing an odd index even as "a documented choice for an architecturally
undefined case (UM Ch. 17), not a hardware fact". The choice was reasonable and
the case is not undefined on this part — the suite defines it.

### S-4 — the N64brew Wiki's `FCR0.Imp` is wrong

**The wiki says:** *"FCR0 bits [15:8] is the implementation number ... All
VR4300 units will report 0x0B (11) for the implementation number"*
(`n64brew_wiki/markdown/VR4300.md`).

**Two independent sources say `0x0A`:**

- n64-systemtest asserts `CFC1 $0 == 0xA00`, and it runs on real hardware.
- cen64 hardcodes `0xa00` with the comment *"fpu version of both 0xb22 and
  0xb10 N64s"* — checked against two console revisions.

`0x0B` **is** correct for `PRId.Imp`, the *CPU's* revision register, and the
most likely explanation is a conflation of the two. They identify different
units and the near-identical values make the mistake easy — this implementation
made exactly it, with a comment reading "matching `PRId`".

**Why this one is worth an entry rather than a quiet fix.** `AGENTS.md`
designates the wiki as the primary hardware reference. It is community-edited
and CC BY-SA, and it is wrong here, so a single-value claim from it wants a
second source before it becomes code. That is a statement about how to *use* the
reference, not a reason to stop using it.

### C-15 — the reserved COP0 registers are one shared write latch

**Claim.** COP0 registers 7, 21..=25 and 31 are not storage. A write goes
nowhere; a read returns the value of the most recent `MTC0`/`DMTC0` to **any**
COP0 register.

So writing register 7 and reading it back returns what was written — and the
same sequence with *any other* COP0 write in between returns **that** value
instead.

**This resolves ledger U-1**, which recorded "discards writes and reads zero" as
an arbitrary choice because the manual documents only an absence (UM Table 1-2,
p. 46). It was a reasonable guess and it was wrong; n64-systemtest documents the
behavior in its own test comments and exercises it directly.

**The oracle is built to defeat the obvious cheat.** It sweeps five written
values against three interposed ones, precisely so an implementation that stores
per-register and echoes the first value cannot pass. Our replacement test does
the same in miniature: the second assertion is the one that distinguishes a
latch from storage.

### C-20 — COP2 is one 64-bit latch, not a register file

**Claim.** COP2 is not populated on the VR4300. What remains is a **single**
64-bit value: every `MTC2`/`DMTC2` writes it and every `MFC2`/`DMFC2` reads it,
with the register index **ignored**. `MTC2` writes all 64 bits despite being
nominally a 32-bit move; `MFC2` returns the low half sign-extended and `DMFC2`
the whole thing.

**Evidence.** n64-systemtest writes with one index and reads back with several
others — including 30 and 31 — and gets the same value every time. Its own
comment on a neighboring test says as much: *"it's unlikely that there are
actually 32 registers"*.

**Index-independence is the whole test.** A real 32-entry register file passes
a write-then-read-same-index check perfectly, so the assertion that matters
reads back through a *different* index.

**The same shape as ledger C-15.** This processor's answer to "a coprocessor
that is not really there" is a single latch, and it gives that answer twice —
once for the reserved COP0 registers, once for COP2. Worth knowing before
implementing either: the natural design (an array) is wrong both times.

### C-19 — a jump-and-link inside a delay slot links past the OUTER target

**Claim.** The link register receives *the address of the instruction that runs
after this jump's delay slot*. That is `pc + 8` only when the jump is not itself
in a delay slot. When it is, its own delay slot never executes — the outer jump
redirected a cycle earlier — so the next instruction is the outer **target**,
and the link is `target + 4`.

n64-systemtest states it in the assertion text rather than leaving it to be
inferred: *"JAL in delay slot writes target address+4 of original jump into
delay slot"*. It covers `JAL` in `J`, `JAL` in `JALR`, `JALR` in `JALR`, and a
**not-taken** `BGEZAL` in a `J` — the last mattering because the linking forms
link whether or not they branch.

**The fix is a deletion, not a formula.** `execute` computed `pc + 8`; `EX` now
fills the value from the live `next_pc`, which *is* that address by
construction in both cases. A second formula for the nested case would be a
second thing to keep in agreement; reading the pipeline's own pointer cannot
disagree with it.

**Order matters and is pinned.** `next_pc` must be read **before** this
instruction's own redirect is applied — reading it after gives the jump's own
target, which is wrong for every jump including ordinary ones. Both orderings
are mutation-tested.

### C-18 — the doubleword control moves decline differently per coprocessor

**Claim.** `DCFC1`/`DCTC1` and `DCFC2`/`DCTC2` are structurally identical — the
64-bit control moves of their respective coprocessors — and the VR4300 refuses
them in **different ways**:

| Encoding | Unit usable | Result |
| --- | --- | --- |
| `DCFC1` / `DCTC1` | `CU1` set | **Floating-point exception**, `FCSR.Cause` = unimplemented **only** |
| `DCFC1` / `DCTC1` | `CU1` clear | Coprocessor Unusable, `FCSR` untouched |
| `DCFC2` / `DCTC2` | `CU2` set | **Reserved Instruction**, with `Cause.CE = 2` |
| `DCFC2` / `DCTC2` | `CU2` clear | Coprocessor Unusable |

Giving all four one behavior is the natural mistake, which is why the test
covers both in a single case.

**`Cause.CE` is not only for Coprocessor Unusable.** It names the coprocessor
for a reserved encoding *inside a usable one* too. Only the first use is
obvious, and n64-systemtest compares the whole `Cause` register — so a missing
`CE` reads as an entirely wrong exception rather than a detail. That needed a
distinct `Exception::CoprocessorReserved { unit }`, since a plain
`ReservedInstruction` leaves `CE` at zero by design.

**Note what these are not:** a silent no-op. They previously fell into the
catch-all `Cop1Unimplemented` arm, which retires without effect — the
decoded-but-no-op shape this project has been bitten by twice.

### C-17 — `CTC1` can raise an FP exception on its own

**Claim.** Writing `FCSR` with a Cause bit whose corresponding Enable is also
set meets the trap condition immediately. No arithmetic has to run: the `CTC1`
itself is the faulting instruction, and n64-systemtest checks that `ExceptPC`
points at it.

Bit 17 (Unimplemented) is unmaskable and traps regardless of the enables, so it
is tested outside the enable comparison.

Easy to miss because `FCSR` looks like storage — the trap check lives with the
*arithmetic*, so a control-register write is not an obvious place to put one.

### C-16 — `EntryLo0`/`EntryLo1` are writable to bit 29, not bit 25

**Claim.** Both registers accept `0x3FFF_FFFF`. The architectural fields —
PFN (25:6), C (5:3), D (2), V (1), G (0) — account only for bits 25:0, and the
mask was set to that width. Bits 29:26 are writable too and read back exactly as
written.

**Evidence.** n64-systemtest writes a sweep including `0x0F000000` and
`0xFFFFFFFF` and expects `value & 0x3FFF_FFFF` back for each
(`tests/tlb/mod.rs`). Deriving the mask from the field diagram instead silently
dropped four bits on every write-back.

A reminder that a *field* table and a *writable-bits* mask are different
documents: the first says what the hardware interprets, the second what it
stores.

### C-21 — `FR = 0` maps `fs` and `ft` differently, and the manual declines to say so

**Claim.** Under `Status.FR = 0`, a floating-point *arithmetic* instruction resolves its two
operand register fields by **different rules**: the low bit of `fs` is ignored, and the low bit of
`ft` is not. The destination `fd` is used as-is in both modes.

**Why it is measured, not documented.** The manual is explicit that it will not say: *"If the FR
bit is 0, an odd-numbered register cannot be specified"* (UM §7.5.3), and per-instruction, *"If an
odd number is specified, the operation is undefined"* (UM §16). Undefined in the manual is still
deterministic in silicon, so the oracle here is n64-systemtest's measured table, and this entry
records it as a measurement rather than as documentation.

**Evidence.** Two rows of `Upper bits of 32 bit operation (half mode)` cannot be satisfied by any
single mapping:

- `SQRT.S $13, $31` yields `sqrt(16) = 4`, so `fs = 31` read **FGR30**.
- `ADD.S $2, $28, $31` yields `-10 + -16 = -26`, so `ft = 31` read **FGR31**.

`Comparisons in half mode with odd register indices` then states it outright in its own assertion
messages: *"Lowest bit of fs should be ignored"* and *"Lowest bit of ft should not be ignored"*.

**What this supersedes.** C-14 established that `FR = 0` addresses whole even registers and that a
32-bit access reaches an odd register's **high** half. That remains correct for `MTC1`/`LWC1` and
the doubleword coprocessor moves — the instruction classes it was derived from. It does **not**
extend to the arithmetic operand ports, which is the assumption this entry corrects. Two mappings
for two instruction classes is surprising; separate accessors (`read_s_fs`/`read_s_ft`) exist so a
call site cannot silently pick the wrong one.

**Cost of getting it wrong.** Folding an odd arithmetic destination into its even partner leaves the
odd FGR holding its previous value, which the suite detects directly by observing that FGR1 keeps
its preload after `ADD.D $1`.

---

### C-22 — `PRId.Rev` is documented after all, and U-3 had decayed

**Claim.** `PRId` reads `0x0B22`: implementation `0x0B` for the VR4300 series, revision `0x22`.

**What this supersedes.** Ledger **U-3** recorded the Rev field as undocumented and left it zero.
That was a true statement about the *User's Manual* and a false one about the N64brew wiki, which
this project mirrors and treats as a primary hardware reference: *"retail N64 units have so far been
found to report either 0x10 (1.0, early units) or 0x22 (2.2, later units), and the iQue Player
reports 0x40"* (`n64brew_wiki/markdown/VR4300.md`).

This is the third instance of the same failure mode in this project, and the reason
`docs/engineering-lessons.md` §3.3b exists: **"undocumented" is a claim about a document, and it
decays.** Nothing fails when it goes stale, so it survives review and gets cited as if it were a
claim about the hardware. Re-open the source before relying on such a record.

`0x22` is the later stepping, which is what `fpu::Stepping::Fixed` (the default) denotes. The two
want to be selected together by a console-revision constructor; wiring that before anything can
choose `Early` would be inert API.

### C-23 — `Random` is a plain 6-bit down-counter, and the reload is `==` not `<=`

**Claim.** `Random` decrements each instruction, wrapping 0 → 63, and reloads 31 when it **equals**
`Wired`.

**Why the distinction is invisible until it isn't.** For `Wired <= 31` the `==` and `<=` readings
agree — the counter walks 31 down to `Wired` either way. They diverge only once `Wired` exceeds 31,
which software can arrange because the field is six bits: under `<=` the counter is immediately at
or below the floor and pins at 31 forever, under `==` it walks 31 → 0 → 63 → `Wired` and covers the
whole range.

**Evidence.** n64-systemtest sets `Wired` to 32 and above and requires `Random` to span at least
`[10..54]`; we reported `[31..31]`. Note what makes this checkable at all: the suite samples a
*range*, because sampling a single value cannot distinguish a pinned counter from a slow one.

---

### C-24 — integer-to-float conversion honors `FCSR.RM`, and a Rust `as` cast does not

**Claim.** `CVT.S.W`, `CVT.S.L`, `CVT.D.W` and `CVT.D.L` round according to `FCSR.RM`.

**Evidence.** n64-systemtest converts `0x4996_02D3` (1234567891) under round-toward-zero and
expects `0x4E93_2C05`; nearest-even gives `0x4E93_2C06`. Likewise `CVT.D.L` of
`0x007F_FFFF_FFFF_FFFE` toward zero expects `0x435F_FFFF_FFFF_FFFF`, not `0x4360_0000_0000_0000`.

**Why it was wrong.** Each converter was a Rust `as` cast plus a round-trip inexact check. `as`
rounds to nearest-even *unconditionally*, so the mode was ignored — and the round-trip check
correctly reported `inexact`, which made the flags right and the value wrong. Flags agreeing is not
evidence the value does.

**What the fix removed.** All four converters were **deleted** rather than left unused once the
pipeline moved to `softfloat::from_int`. An unused function that quietly gets an operation wrong is
the inert-API hazard `docs/engineering-lessons.md` §3.2 describes; `addr.rs` deleted a stale
`translate` for the same reason, and that precedent is why this was not simply left in place.
`long_convertible` stays — the VR4300 range restriction is a separate rule and is still consulted.

The conversion is now one line: an integer is `sign × |v| × 2^0`, so it is the shared rounding
point with a zero exponent and no sticky bit. Routing it through the same `round_pack` as every
other operation is what makes the mode impossible to forget.

---

### C-25 — an in-flight `C.cond.fmt` is forwarded to `BC1`, not stalled for

**Claim.** `BC1` reads the condition an in-flight compare is about to commit, by re-evaluating that
compare from its latched operands, rather than waiting for `WB`.

**Why a stall cannot do it.** `stall_for` freezes every stage. Holding the branch therefore delays
the compare's `WB` by exactly the same number of cycles, and the gap never closes. This was not
deduced — an interlock on `ex_dc`/`dc_wb` was implemented and traced: it fires once, is then
satisfied while the commit still has not happened, and the branch runs early anyway.

**Why the load interlock is not a counter-example.** It stalls one cycle *and* its consumer reads
through the bypass network. The stall buys `DC` time; forwarding delivers the value. The FP
condition had no forwarding path at all, which is what this adds.

**Why re-evaluating is sound.** A compare reads two FP registers and writes only `FCSR.C`. Nothing
between it and the branch can change those registers — a branch has no destination — so the early
evaluation yields precisely the value `WB` will commit. Flags are discarded: this is a forwarding
path, and raising from it would make the branch report the *compare's* trap.

`ex_dc` is consulted before `dc_wb` because it holds the younger instruction, and the most recent
compare is the one whose value stands.

---

### C-26 — the golden log is a TANDEM-VERIFICATION claim, not a claim about boot

**Claim.** `tests/golden/n64-systemtest.log` records the retired-instruction PC stream captured from
**ares** starting at the ELF entry `0xFFFF_FFFF_800A_15E8`, and `RustyN64` reproduces it exactly.

**What that does and does not prove.** It proves: *given identical initial state, `RustyN64` retires
the same instructions in the same order as an independent, mature reference.* It proves nothing
about boot, about timing, or about anything before the sync point. This is the discipline hardware
verification calls **tandem verification** / step-and-compare co-simulation (RISC-V's RVVI/RVFI
harnesses are the same shape): align two models at a boundary, and treat only deltas from that
boundary as the claim.

**Why the boundary is the ELF entry.** Everything earlier is PIF ROM and libdragon's IPL3 —
copyrighted Nintendo code plus a bootloader — which must not enter the repository. It is also where
`RustyN64` begins executing, so the streams are directly comparable without a cartridge subsystem.

**Why `Count`, `Random` and `Compare` are excluded.** Not convenience — there is no correct value.
libdragon's IPL3 (`boot/ipl3.c`) zeroes `Count` mid-boot and then accumulates PI/SI busy-waits whose
length is a property of the host's timing model; libdragon's own `pi_wait()` passes the result to
`entropy_add()`, i.e. upstream treats a boot-relative `Count` as a source of **entropy**.
n64-systemtest's startup test declines to assert `Count` at all and will not even pin `Wired` or
`Index` ("Usually 0, but also seen 33"). Comparing one would encode the reference's timing model as
though it were hardware. Safe only because those registers have dedicated tests in the COP0
category — a separate gate.

**Corroboration obtained along the way.** cen64, booting the real PIF ROM, reaches the sync point
with `Status = 0x3400_0000` — exactly what `seed_ipl3_handoff` synthesizes. The handoff model was
independently confirmed rather than merely assumed.

### C-27 — EMUX is implemented and DEFAULT-OFF, because hardware has none

**Claim.** COP0 CO `funct` 0x20-0x3F is n64-systemtest's EMUX emulator-extension space
(`xdetect 0x20`, `xlog 0x25`, `xioctl 0x2C`), implemented behind `Bus::emux_enabled`, **off by
default**.

**Why the default is load-bearing, and how we learned it.** Implementing EMUX and advertising it
unconditionally broke the golden-log 0-diff at record 304 — immediately after the `xdetect` probe.
The cause: `ares`'s every EMUX handler opens with `if(!system.homebrewMode) return;`, and homebrew
mode is **off by default**, so ares's `xdetect` is a no-op that leaves its destination register
untouched. Real hardware behaves the same way — the range is inert (ledger **C-8**), which is
precisely why emux could claim it. Advertising capabilities makes n64-systemtest switch console
backends, which changes the retired-instruction stream.

So EMUX is opt-in, matching ares exactly. A default build behaves like hardware; the systemtest
harness opts in and gets a console needing no PI/SI/`ISViewer` emulation (~9x faster) plus
`xioctl(EXIT)` as a definite end-of-run signal instead of a tick budget.

**A bug this surfaced.** The first `xlog` read guest memory straight off the bus and printed blanks
where hex digits belonged (`Heap range:          to`). The string had just been formatted by cached
stores and was still sitting in dirty D-cache lines. Reading *through* the D-cache fixed it — an
independent confirmation that the cache model is right, found because the log channel had to obey
the same rules a guest `LB` does.

### C-28 — the RCP's internal bus is size-blind, and RDRAM is not

**Claim.** Every device in `0x0400_0000-0x04FF_FFFF` ignores the access size and the low two
address bits, latching the whole 32-bit word the VR4300 placed on `SysAD`. A narrow store there
writes the *source register shifted into the addressed byte lane*, wiping the rest of the word; a
64-bit store writes only the upper word and touches four bytes. RDRAM is exempt.

**Basis: documented, and independently stated by the oracle.** N64brew *Memory map* SS Physical
Memory Map accesses gives the mechanism and the worked example -- with `S0 = 0x1234_5678` and
`A0 = 0x0400_0001`, `SB S0, 0(A0)` puts `0x3456_7800` on the bus and the RCP writes it to
`A0 & ~3`. n64-systemtest states the same rule in its own words at the head of
`src/tests/sp_memory/mod.rs`: *"SH/SB are broken: they overwrite the whole 32 bit, filling
everything that isn't written with zeroes. SD is broken: it only writes the upper 32 bit of the
value, touching only 4 bytes."* Two independent sources, one of them executable.

**Why RDRAM differs, and why that asymmetry is the whole point.** The RI forwards the low address
bits and the access size to the RDRAM devices, which build a real byte mask from them; only the
RCP's internal path discards that information. So the correct narrowing is a property of the
**target**, not of the instruction -- which is why `Bus::write_sized` carries the width and the
untruncated register to the bus rather than letting the CPU narrow first. A CPU that narrows
eagerly cannot express this, and the bug is invisible until something reads back a neighboring
byte it never wrote.

**Scope, stated rather than assumed.** The PI and SI external-bus windows share the size-blindness
on hardware (same wiki section), and are **not** covered here: the PI already models its own bus
quirks separately, and merging them without the cart tests to check against would be a change made
blind. Phase 5 owns that. The 64-bit *read* case -- which hangs the VR4300 outright, because the
RCP never puts a second word on the bus -- is not modeled either; nothing tests it, since a test
for it would hang the console.

---

### C-29 — the FPU rates are charged; the early exit on trivial operands is not

**Claim.** COP1 arithmetic stalls the pipeline for its **UM Table 7-14** rate — `ADD`/`SUB` 3,
`MUL` 5/8, `DIV`/`SQRT` 29/58, the `ROUND`/`TRUNC`/`CEIL`/`FLOOR` family 5, the `CVT` forms 1/2/5
depending on the *source* format, and 1 (no stall) for `ABS`/`MOV`/`NEG`/`C.cond`. What is **not**
modeled is the documented early exit.

**Basis: documented, transcribed from the table itself.** Extracted from the manual with
`mutool draw -F txt` and asserted row by row in `fpu::tests::the_fpu_delay_table_matches_the_manual`.
The manual's *"latency is the execution rate plus one … an EX-to-RF bypass is not performed"* is not
added anywhere: the stall holds every stage, so a dependent consumer spends its own cycle after the
stall drains and reaches rate + 1 without a second rule.

**The deviation.** UM §7.5.6, and Table 7-14's own note 2, say a multicycle operation whose result
is *obvious* completes in **two** cycles instead of its full rate: add/sub on a zero or infinity
operand or a source exception, multiply when either operand is a power of two, divide and sqrt when
the result is zero or infinity, and the convert instructions for trivial cases. None of that is
modeled, so those operands are charged the full rate and the model runs **slower than hardware**
on them — never faster, which keeps the error one-directional and bounds it: at worst 27 PCycles
for a `DIV.S` by infinity, 56 for the double.

**Why it is deferred rather than guessed.** The trigger conditions are documented but the exact
operand classes are prose rather than a table, and charging two cycles for a case the hardware does
not consider trivial would be as wrong as charging 29 for one it does. This needs the timing set
n64-systemtest ships default-off to measure against, which is the same instrument C-1 (`M`) is
waiting on. Until then the honest position is the documented rate.

**Not observable today.** Both oracles are unchanged by adding these stalls: the golden log holds
its 0-diff over 50,027 records and n64-systemtest's Phase 1 categories stay at 0. That is expected
— the golden log compares retired instruction streams, not cycle counts — and it means these rates
are currently *unfalsified* rather than *verified*.

---

### C-30 — the SP memory window mirrors its 8 KiB up to `0x0404_0000`

**Claim.** DMEM and IMEM are 4 KiB each at `0x0400_0000` and `0x0400_1000`, and that 8 KiB of real
storage **repeats** for the whole range up to `0x0404_0000`, where the SP registers begin.

**Basis: the oracle for the repetition, the address map for where it ends.** The two halves of this
claim do not share a source, and the *Bounded* note below keeps them apart. For the repetition
itself the oracle is the only source: the N64brew wiki's *RSP Interface* documents the first
8 KiB and stops — it gives the DMEM and IMEM ranges and says nothing about what lies between
`0x0400_2000` and `0x0404_0000`. The mirroring comes from n64-systemtest, in two independent
forms: its own source comment (`src/tests/sp_memory/mod.rs`) states *"Going out of bounds wraps the
memory around (until the real end of 0x04040000)"* and *"SPMEM DMEM and IMEM repeat from 0x04000000
to 0x04040000"*, and its `spmem: SW (out of bounds)` test **executes** the claim: it writes
`0x7654_3210` at offset `0x3E000`, then reads it back at offset 0 and at `0x3E000`, and separately
checks that offset `0x1000` (IMEM) was untouched. `0x3E000 & 0x1FFF == 0`, i.e. the 31st repetition.

**Why this is recorded rather than treated as obvious.** Masking an address is the natural
implementation of *both* "it mirrors" and "we do not bounds-check", and those are different claims
about hardware. This entry exists so the folding in `Rsp::mem_read` is understood as modeled
behavior with a source, not as a missing guard — and so that if the range is ever found to fault
or to alias differently, there is a claim to retract rather than an accident to rediscover.

**Bounded, and the two bounds have different evidence — which is the point of separating them.**

- *That the window repeats at all, and the 8 KiB period*: **oracle**. `spmem: SW (out of bounds)`
  writes at `0x3E000` and reads the value back at offset 0, with the IMEM half untouched.
- *That the repetition stops at `0x0404_0000`*: **not** from that test, which never probes the
  boundary. It comes from the address map — N64brew *RSP Interface* places the SP registers at
  `0x0404_0000` — so the window ends where the next device begins. Nothing here has *tested* the
  last repetition before that address, and this entry should not be read as claiming otherwise.
  A boundary test is the way to close it.

What the RSP's own DMA sees is separate again, and is the per-bank 4 KiB wrap the wiki does
document.

---

### C-31 — the `VRCP`/`VRSQ` ROM tables are **generated exactly**, not stored as literals

**Claim.** The RSP's 512-entry reciprocal and inverse-square-root ROMs are produced at
construction by exact integer arithmetic, and are bit-identical to the hardware tables.

**This contradicts a rule written in `docs/rsp.md`**, which says *"the recip ROM is data, not a
formula. Table-drive it from the documented values; do not approximate."* The contradiction is
deliberate and is recorded here rather than resolved silently in either direction.

**Why the rule exists, and why it does not bite here.** The rule guards against *approximation* —
computing a reciprocal in floating point, or with a truncated series, gets the low bits subtly
wrong and transformed vertices land in the wrong place. What ares does (`ares/n64/rsp/rsp.cpp`,
ISC, on the vendorable list in `ref-proj/README.md`) is not an approximation: for the reciprocal
it is `(1 << 34) / (index + 512)`, rounded by `+ 1 >> 8`, in 64-bit integers; for the inverse
square root it is the **smallest** `b ≥ 2¹⁷` with `a·(b+1)² ≥ 2⁴⁴` — one *past* the last value
satisfying the strict inequality, which is what the loop actually computes and is not what its
comment claims (see the off-by-one note below). Both are exact integer
constructions with no rounding freedom, so they reproduce the ROM rather than estimating it.

**Built at compile time.** The generators are `const fn`s producing `static` arrays, so the
artifact in the binary *is* a table and nothing computes a reciprocal at run time. An earlier
revision of this entry described per-call generation; that was a performance bug (the
inverse-square-root search ran ~131,000 iterations of two 64-bit multiplications **per `VRSQ`**,
software-emulated on `thumbv7em`), and moving it to const evaluation also brings the
implementation much closer to what `docs/rsp.md`'s rule asks for.

**Why generate rather than paste.** A 512-entry literal table is 512 opportunities for a
transcription error, and a wrong entry is invisible until some specific divisor is used — the
worst failure profile available. The generator is eight lines that can be read against the source
they came from. The trade is real and goes the other way too: a bug in the generator is *also*
invisible until exercised, and it applies to every entry at once rather than to one.

**An off-by-one this caught.** ares's comment above its search reads *"find the largest b where
b < 1.0 / sqrt(a)"* — but the loop is `while cond { b += 1 }`, which walks *through* the last
satisfying value and stops one past it, so the table holds one **more** than the comment says.
Reimplementing the comment's predicate as a bisection produced `26964` where the scan gives
`26965`. It was caught only because the bisection was pinned against values captured from the
original scan: every *property* the other tests check — monotonicity, the odd/even interleave, the
16-bit range — holds just as well one step to the left. A reference implementation's comment is a
claim about its code, and decays the same way ours do.

**What makes it falsifiable.** The tables are pinned by tests against values n64-systemtest
expects, so an error in the generator shows up as a failing oracle assertion rather than as a
quietly wrong vertex. Until those assertions exist for both tables across their range, this entry
is a claim about bit-exactness that has been **spot-checked, not proven** — and it should be read
that way.

**Attribution.** The construction is ares's, used under ISC. `ref-proj/README.md` records that
ares is among the projects permissive enough to draw from; simple64, gopher64, n64-tests and
angrylion-rdp-plus are not, and were not consulted.

---

### C-32 — the HLE boot state and the CIC seeds are cited constants, not measured

**Claim.** `rom::hle_boot` reaches the state a retail IPL3 expects without running the real
IPL1/IPL2 or the CIC challenge: it copies the cartridge's *own* IPL3 (`ROM 0x40..0x1000`) into
DMEM and jumps to it, and stands in for the skipped handshake with a fixed set of seeded values.
Every one of those values is a **cited or documented constant** — none is fitted to make a ROM
pass, and none is a timing interval.

**The seeded state and its provenance.**

- **CIC seed word → PIF RAM `0x24..0x28`.** Per CIC: 6101 = `0x0004_3F3F`, 6102 = `0x0000_3F3F`,
  6103 = `0x0000_783F`, 6105 = `0x0000_913F`, 6106 = `0x0000_853F`. The CIC-specific byte
  (`0x3F`/`0x78`/`0x91`/`0x85`) and the `0x3F` boot-parameter byte are the IPL3 seeds documented
  on the **N64brew *CIC-NUS* wiki** (§Seeds / IPL3), mirrored under `n64brew_wiki/markdown/`.
- **COP0 `Status = 0x3400_0000`** (`CU1|CU0|FR` set) and **`Config = 0x7006_E463`** (the
  IPL3-left value, `K0 = 3` cached) — the post-IPL3 register state; corroborated against the
  boot sequence in **cen64 `si/cic.c`** and the N64brew *PIF-NUS* boot description.
- **GPRs `s3..s7`** = `rom_type = 0` (cart), `tv_type = 1` (NTSC), `reset_type = 0` (cold),
  `s6` = the CIC seed byte, `s7 = 0` — the boot argument block IPL3 hands the game's entry.
- **PI `DOM1_{LAT,PWD,PGS,RLS}`** are decoded from the ROM header's first word exactly as IPL2
  programs them (N64brew *Peripheral Interface* §BSD domain registers), not invented.

**Why this is HLE and where the real path lives.** This deliberately skips the PIF ROM
(IPL1/IPL2) and the CIC lockout challenge — the seed injection is their observable *result*, not
a reimplementation. The copyright-clean, CI-able default; the real-PIF/IPL path is an
off-by-default local-only mode (ADR 0009). `simple64`'s `bootrom_hle.c` was
**studied, not copied** (it is GPLv3, study-only per `ref-proj/README.md`); the drawn-from
sources are the N64brew wiki and cen64 (both citable/vendorable).

**Measured n64-systemtest impact.** The Phase 5 cart/PIF/SI subsystem this boot path unlocks
drops the suite-wide failing-assertion count from **93 → 90** (−3), measured with the committed
runner (`cargo test -p rustyn64-test-harness --release --test systemtest -- --ignored`,
917 tests started, 90 failing; Phase-1 categories still `Failed: 0`). This 90 is the **current
authoritative count** — the older "count unchanged at 93" notes in the entries above were true
when written and are left as-is (historical, per the ledger's immutability discipline). The
remaining 90 are the RDP rasterizer (Phase 3 render path, no systemtest driver) and the retail
OS-boot runtime (R-18), neither in Phase 5's scope.

**Falsifiability.** The boot state is pinned by `hle_boot_seeds_retail_state`, which asserts the
IPL3 copy, the CIC seed word in PIF RAM, the `s4`/`s6` argument bytes, the full PI DOM1 tuple
(incl. RLS), and the DMEM entry PC — so changing any seeded constant fails a test. What it does
**not** prove is that these are the *only* values a real IPL3 leaves; that is bounded by the
real-PIF path (off by default) and by whatever n64-systemtest boot-state coverage is run.

---

### C-33 — the real-PIF boot executes IPL1/IPL2/IPL3, with the PIF-SM5 behaviorally modeled

**Claim.** `rom::real_pif_boot` boots a retail cartridge the way hardware does: the CPU runs the
**real IPL1 and IPL2** from the console's PIF boot ROM (mapped at `0x1FC0_0000`) starting at the
reset vector `0xBFC0_0000`, then jumps into the cartridge's own IPL3 — no state seeding, no HLE
jump. This is the faithful counterpart to the HLE path (C-32); it is off by default, local-only,
and never CI-gated (it needs the copyrighted PIF ROM, which is never committed).

**What is executed vs. modeled.**

- **Executed for real (guest code):** IPL1 (52 instructions from the PIF ROM: RSP/PI/AI reset,
  copy IPL2 to IMEM, jump), IPL2 (from IMEM: read the seed hand-off, lock the PIF ROM, compute
  the 6-byte IPL2 checksum over IPL3, have the PIF verify it, jump to IPL3), and the cart's IPL3.
  The disassembly of the local dump (`bfc00000`+ / `bfc006xx`) was traced to confirm the exact
  handshake.
- **Modeled behaviorally (the PIF-SM5, from `PIF-NUS.md`, not by running its 4-bit firmware):**
  the power-on hand-off (the two seeds written to PIF RAM `0x24-0x27`), and the reset-mode
  command byte (`0x3F`): `0x10` ROM lockout (the PIF-ROM window then reads 0), `0x20` acquire
  (latch the checksum IPL2 wrote to `0x32-0x37`, zero it, set the `0x80` ack IPL2 spins on), and
  `0x40` run (compare against the CIC's checksum; a mismatch freezes the CPU via NMI —
  `Bus::boot_nmi_halt`, `Scheduler::step_due_here`).
- **The CIC** is identified from the cartridge IPL3's **CRC-32** (`Cic::from_ipl3`, cen64's
  `si/cic.c` fingerprint table — the core reads only the ROM's own code, never a per-game DB;
  ADR 0003/0004). Its boot outputs are documented constants: the per-CIC IPL2/IPL3 seeds and the
  6-byte IPL2 checksum (`Cic::boot_secrets`, `PIF-NUS.md` §checksum table). The IPL2-seed byte is
  the **corrected** value (6103 = `0x78`, 6105 = `0x91`, …), not cen64's legacy all-`0x3F` seed
  byte — the real IPL2 consumes it, so the legacy value would compute a mismatch.

**Not modeled (bounded, and not needed to boot).** The SM5 firmware itself is not emulated
(there is no decapped-ROM dependency); the post-boot **running challenge** — the 6105/7105 "X105"
protocol behind command `0x08` — is not modeled. Per `CIC-NUS.md` the running challenge is a
dummy bit-inversion on every CIC except 6105/7105 and *"no known software relies on"* it, and the
**boot** IPL2 checksum is the same algorithm for all CICs — so booting does not need either.

**Validated locally, and what the validation proves.** The
`a_commercial_rom_boots_through_the_real_pif` capstone boots the first ROM of every save-type
folder (6102/6103/6105 CICs) and asserts each reaches game execution in RDRAM (KSEG0), with the
PIF ROM locked and **no NMI freeze** — i.e. IPL2's computed checksum matched the
hardware-documented CIC value. That match is a strong independent check: it can only hold if the
CPU reproduced IPL2's checksum **bit-exactly** over the real IPL3 *and* the seed/CIC detection
were right. The `boot_command` handshake is unit-tested with a mutation check — a deliberately
wrong checksum must NMI-halt (`boot_run_halts_on_a_wrong_checksum`), so the match is not vacuous.

**n64-systemtest impact: none.** The real-PIF path is off by default and n64-systemtest uses the
ELF/HLE load path; CIC detection on its homebrew IPL3 (CRC absent from the table) falls back to
6102 as before. The suite-wide count stays **90** (measured; see C-32). This entry is a new
faithful capability, not a change to any gated result.

---

---

## 5. Deliberate deviations from hardware

Behavior we model differently *on purpose*, so it is never mistaken for a bug.

| # | Deviation | Why | Bounded by |
| --- | --- | --- | --- |
| D-1 | Power-on CPU/RCP phase comes from a seeded PRNG, not from real indeterminacy | The determinism contract requires reproducibility; the hardware's own indeterminacy is documented (UM Table 11-1's "1 to 2 PCycles: synchronize with SClock") and is modeled as a *parameter* rather than eliminated | ADR 0004, ADR 0006 |
| D-5 | **SUPERSEDED by D-6** (the caches are modeled as of T-11-003). Recorded verbatim because the reasoning was sound while it held, and because the boundary it named — "stops being sound the moment something can observe staleness" — is exactly what came due. `CACHE` was an **address-translating no-op**: it decodes, translates (so it can raise a TLB fault) and does nothing else | The cache *contents* are not modeled, so invalidate and write-back have nothing to act on. This is observationally sound **only** because no cache state exists to become stale — the depth decision the Phase 1 open question asked for. What matters now is that `CACHE` does not *raise*: IPL3 and libdragon both issue it, so a `Reserved` decode blocks every real ROM | Was bounded by Phase 5 DMA coherency; that is **not** what retired it. It came due earlier, at n64-systemtest's `DCACHE:`/`ICACHE:` groups, which observe staleness without any DMA — a reminder that a bound named in a ledger entry is the *earliest case thought of*, not the earliest that exists. DMA coherency remains open under D-6/T-11-003, as does `M` (C-1) |
| D-4 | TLB entries reset to **distinct** `VPN2` tags, not to zero | All-zero is not a usable state: with 32 entries at `VPN2 = 0` and `V` not participating in matching, the first access to virtual page-pair 0 matches all 32 and triggers **TLB shutdown**. Reset contents are undefined (UM §6.4.4) and ADR 0004 forbids entropy, so a fixed non-coinciding set is chosen — which is what real hardware's arbitrary power-on contents almost always are | Pinned by `a_fresh_tlb_does_not_shut_down_on_the_first_low_access`; revisit if n64-systemtest probes uninitialized entries |
| D-3 | `Count` and `Compare` both reset to a deterministic **0**, so the timer matches at power-on and latches `IP7` | Both reset values are **undefined** (UM §6.4.4, p. 183) and ADR 0004 forbids entropy, so *some* fixed pair must be chosen; 0/0 is the least surprising. The consequence is a timer interrupt pending before software writes `Compare` — masked in practice, since cold reset also leaves `IE` clear and `ERL` set | ADR 0004; IPL3 writes `Compare` during boot, so no real ROM observes it. Revisit if n64-systemtest's startup set disagrees |
| D-6 | The primary caches are indexed by **physical** address; the hardware indexes them virtually | A virtually-indexed, physically-tagged cache lets two virtual addresses for one physical address occupy two lines — a cache alias, which software must then flush around. Physical indexing removes aliases, and keeps a virtual address out of a structure that otherwise needs only a physical one. It is **not** strictly safer: it is a behavioral divergence in both directions | Two observable differences, both untested here: a program that deliberately constructs an alias, and an `Index_*` operation on a **TLB-mapped** page, where translation preserves only the low 12 bits while the D-cache index reaches bit 12 and the I-cache bit 13. The tested scope is KSEG0, where the two indexings coincide — every test that motivated the cache model works there. Revisit if a ROM observes either case |
| D-2 | The VR4300 errata are **reproduced**, not fixed | They are observable behavior that software depends on; `sra`/`srav` in particular affects every console | ADR 0007; pinned by named tests that fail if "corrected" |
