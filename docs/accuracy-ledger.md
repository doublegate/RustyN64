# Accuracy ledger — RustyN64

**References:** ADR 0005 (what defers here), ADR 0006, ADR 0007;
`ref-docs/2026-07-20-vr4300-timing-supplement.md` (the undocumented-constants list);
`docs/testing-strategy.md`; `docs/engineering-lessons.md` §3.3.

## What this file is for

Three things, and nothing else:

1. **Measured constants** — numbers the hardware documentation does not supply, which we fitted
   from test ROMs. Each records *how* it was measured, so it is falsifiable.
2. **Open residuals** — known-wrong behaviour we have chosen to document rather than point-fix.
3. **Ruled-out approaches** — attempts that failed, with the reason, so nobody rediscovers them.

The rule that gives this file its value: **an entry here is honest, a per-quirk patch is not.**
When a ROM fails and the fix would be a special case, the entry goes here instead (ADR 0005).

Equally: **a measured constant is never adjusted to make a specific ROM pass.** The moment a
number is tuned rather than measured, every later timing result built on it becomes
unfalsifiable — the whole suite silently stops being evidence. If a constant looks wrong, measure
it again and say so; do not nudge it.

## Status

**Phase 1 is complete, and still nothing here has been *measured*.** That is the honest headline:
every entry resolved so far was resolved by **citation** — it turned out to be documented after all
(C-2, C-3, C-7, C-22, S-1, S-3, U-1, U-3, U-4) — or by implementing behaviour the sources do
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

### C-1 — `M`, memory access time in PCycles

The single most load-bearing unknown. Both documented cache-miss formulas are parameterised on
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
cancelled — a first-party microbenchmark (the accuracy-battery / `T-71-001` shape), or the
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
   constant. The `±1` is the documented PClock:SClock = **1.5** synchronisation jitter (UM Table
   10-1; PClock = 1.5 × MasterClock = 93.75 MHz on the N64).

2. **The cold-access latency — an EXTERNAL ESTIMATE, not a primary capture.** Copetti's N64
   architecture write-up (<https://www.copetti.org/writings/consoles/nintendo-64/>) gives *"the
   delay between initiating a memory transaction to finding the value in cache … around 640 ns"* —
   a full cold fill (cross-confirmed loosely by Beyond3D/community, but no logic-analyser capture
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
measured `M(RDRAM)` moves both together and they cannot desynchronise. The first-party **cached-miss
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
**stall**; a JTLB miss is an **exception**. Modelling only the JTLB loses this cost entirely.
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
guess**, recorded here so it is not mistaken for the documented behaviour. `HI` is
better founded: `remainder = (int32_t)(dividend - quotient * divisor)` computed in
64-bit, which the wiki does state.

**Owner:** T-11-005 (the errata ticket), characterised against hardware or
n64-systemtest.

### C-6 — divide-by-zero `HI`/`LO`

Architecturally *undefined* on MIPS. `alu::div`/`divu`/`ddiv`/`ddivu` use the
conventional emulator interpretation (`LO` = ±1 or all-ones, `HI` = dividend).
Unverified against hardware. What *is* non-negotiable and tested is that it does
not panic — a guest program can divide by zero at will.

---

## 1b. Genuinely undocumented — needs a hardware pin, not a guess

Distinct from section 1: these are not constants to fit, they are *behaviours* the manual
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
| U-7 | The **corrupted output** of the FP multiplication erratum | The *trigger* is documented (`VR4300.md`: a multiply whose preceding multiply had a NaN, zero or infinity operand) and so are the affected steppings (NUS-01/02/03), but **what wrong value is produced has never been characterised** — recorded in `ref-docs/2026-07-20-vr4300-timing-supplement.md` as an undocumented constant. `Stepping::Early` can therefore be *selected* but changes no arithmetic; inventing a plausible wrong value would be the fitted-constant failure this file's preamble forbids | Sprint 3 modelled the switch and the trigger. Needs an affected console, or a hardware capture, before the output can be reproduced |
| U-8 | FPU rounding modes and the `inexact` / `underflow` flags are **partial** | `FCSR.RM` is honoured by the conversions but **not** by `add`/`sub`/`mul`/`div`, which use Rust's operators and are nearest-even only. Likewise `inexact` is set for overflow and conversions but not for ordinary rounding, and `underflow` only for conversions that flush to zero. Both need the *exact* result before rounding, which the hardware float operators do not expose | Needs soft-float arithmetic or per-operation re-rounding. Recorded so a caller does not trust a bit that never sets — the module's own doc table says which flags are complete. **RESOLVED (Phase 1)** by the soft-float core: `FCSR.RM` is honoured by `add`/`sub`/`mul`/`div`, and `inexact`/`underflow` are detected from the exact pre-rounding result. See **C-11 RESOLVED**, which also records the second bug the fix uncovered |

U-6 is the one to watch: it is consistent with ADR 0006's clock derivation, which makes it
tempting to promote to a fact. It is an inference from a part-number restriction, and it stays
labelled as one until something reads the register on hardware.

## 2. Open residuals

> **On the n64-systemtest counts quoted below.** Several entries state the suite's
> failing-assertion count as it stood *when that entry was written* (marked
> *(as-at — see the note above this table)*). Those notes are left unrewritten, per
> this ledger's immutability discipline — but they are **not current**: Phase 5 took
> the count from 93 to **90** (see "Measured n64-systemtest impact" below).
> **`docs/STATUS.md` is the single authoritative source for the current count.**

| # | Symptom | Suspected mechanism | Classification | Status |
| --- | --- | --- | --- | --- |
| R-14 | **RESOLVED** — `triangle_fill` was rendering triangle edges **4× too steep/fast**: a slope that should widen a triangle 0.25 px per pixel-row widened it 1 px per row. Found by the T-33-005 conformance gate on its first triangle vector (Angrylion rendered a near-vertical line, RustyN64 a staircase) | The edge slopes `DxHDy`/`DxMDy`/`DxLDy` are **dx per pixel-row** (N64brew Wiki *Reality Display Processor / Commands* **§0x08 through 0x0F – Fill Triangle**, the Base Command word tables — mirrored at `n64brew_wiki/markdown/Reality Display Processor/Commands.md`. The `dxhdy`/`dxmdy`/`dxldy` fields read "Integer part of **change in x per change in y** of line connecting …" in `s13.16`, and `yh`/`ym`/`yl` are "`s11.2` format" *screen* y coordinates. **Citation corrected 2026-07-28:** this previously cited a "§Edge Coefficients", which does **not exist** in the page — the quoted wording and the fixed-point formats were right, the section name was not). `triangle_fill` evaluated the edge at each sub-scanline as `xh + (y − yh_base) · dxhdy` with `y = line·4 + sub` in **quarter-pixel** units, but never divided the contribution by 4. **Fix:** the three slopes are pre-shifted `>> 2` at decode (parallel-rdp `span_setup.comp:167`: `setup.dxhdy = raw >> 2`) | absolute — a fixed-point unit error, oracle-confirmed | **Closed.** The fix landed; `fill_tri_16` and `fill_tri_wide_16` conformance vectors now pass byte-for-byte against Angrylion. The self-asserted triangle unit tests that baked in the buggy staircase were corrected — their `DxMDy` changed from `0.25` to `1.0` (the value for which the staircase is the *correct* output), which the `fill_tri_wide_16` vector independently confirms against the oracle. Did not change the n64-systemtest count (no systemtest drives the render path); the oracle stays **93** *(as-at — see the note above this table)* |
| R-1 | **RESOLVED** — see C-21. The failing instruction was `ADD.S $1, $29, $30`, not the `ADD.S $0` the assertion names; a correlated capture separated cause from visible effect by exactly the pipeline depth | — | absolute | Closed |
| R-2 | **RESOLVED** — `BC1` implemented, and the compare forwarded to it (C-25) | — | absolute | Closed |
| R-6 | **[HISTORICAL BASELINE — the "only NTSC is modelled" claim is SUPERSEDED by the PAL 50 Hz RESOLUTION in the disposition column; retained per the append-mostly rule.]** The VI scan cadence (T-31-004) *was* anchored to a **nominal 60 Hz field rate** (`VI_FIELD_HZ`) with only NTSC modelled — the per-half-line period was `MASTER_HZ / 60 / (VI_V_TOTAL + 1)`, and `VI_V_CURRENT` / the `VI_V_INTR` interrupt derive from that | The VI dot clock is off a separate crystal the N64brew wiki gives only *roughly* (*Video Interface* §Clocks: "roughly 12.3 megapixels/sec", ×4 ≈ 49 MHz VI clock; the exact NTSC value is not stated). Rather than fit an imprecise dot-clock frequency, the field rate is anchored to the standard NTSC 60 Hz and the half-line count taken from the software-programmed `VI_V_TOTAL` — so the cadence is correct to the field, and only the sub-field phase (which `H_TOTAL`/`H_TOTAL_LEAP` set exactly) is nominal. The interlace `VI_V_INTR` bit-0 quirk (§VI_V_INTR) is also not modelled | absolute — a clock-rate anchor, not a fitted per-ROM constant | **Open.** Correct to the field: `VI_V_CURRENT` advances and wraps at `VI_V_TOTAL + 1`, and the VI interrupt fires once per field at `VI_V_INTR` (pinned by the `vi` unit tests and a scheduler integration test). Deferred: the exact `H_TOTAL`/leap sub-field timing, PAL's 50 Hz field rate, and the interlace `V_INTR` quirk. To be validated against n64-systemtest's `timing`/VI groups when they are run. **PARTIALLY RESOLVED (2026-07-25) — PAL 50 Hz field rate.** `Vi::field_hz` now selects PAL **50 Hz** (`VI_FIELD_HZ_PAL`) when the field is PAL-length (`VI_V_TOTAL > 550`) and NTSC 60 Hz otherwise, so `ticks_per_halfline = MASTER_HZ / field_hz / (VI_V_TOTAL + 1)` scans PAL games at the right cadence — the same `ispal` split the scan-out geometry (R-5 slice 4a) already uses, so cadence and region agree — the `> 550` boundary is now the shared `VI_PAL_V_TOTAL_THRESHOLD` constant (one definition for both `Vi::field_hz` and `bus::scanout_scaled`). **Provenance:** 50 Hz is the documented PAL broadcast standard (N64brew *Video Interface*; counterpart to the anchored NTSC 60 Hz), not a fitted value; the `> 550` split is a wide-margin discriminator between NTSC's ~525-half-line and PAL's ~625-half-line fields (N64brew *Video Interface* §Clocks region field lengths), not a measured edge. Pinned by `a_pal_length_field_scans_at_50hz` (a PAL field advances one half-line in `MASTER_HZ/50/625 = 6000` ticks, distinct from the 60 Hz `5000`; mutation forcing always-60 fails it) and `the_pal_threshold_is_exactly_550` (V_TOTAL 550 = NTSC, 551 = PAL — pins the exact `>` boundary), with NTSC fields unaffected. **n64-systemtest impact: not measured** — the suite runs NTSC and has no VI field-timing group, so the PAL cadence is unreachable by it (the count is unchanged for that reason, not measured against it). **Still deferred:** the exact `H_TOTAL`/leap sub-field timing and the interlace/serrate `V_INTR` bit-0 quirk |
| R-10 | The colour combiner (T-33-002) models the common inputs (combined, texel0/1, primitive, shade, environment, one, zero, and the C-slot alpha taps); the **exotic inputs** — noise, LOD fraction / prim-LOD-fraction, the chroma-key centre/scale, and the convert (`K4`/`K5`) constants — are not modelled and read as **zero** | These inputs need the LOD pipeline (mip level fraction), the key/convert registers (`Set Key`/`Set Convert`), and a noise source, none of which exist yet; they appear in a small minority of combine modes. Reading them as zero is a bounded, documented gap, not a fabricated value | absolute — a coverage boundary, not a fitted constant | **Partially resolved (2026-07-25) — see the RESOLUTION below.** The `(A − B) * C + D` arithmetic (the `special_expand` asymmetric 9-bit fold, the `+0x80`-before-`>>8` rounding, D added unscaled) and the clamp are validated bit-for-bit against hand-computed values; the 16-field decode, the input mux, and the 2-cycle chaining are unit-tested. The remaining exotic inputs land with the LOD/key/noise state and are validated against the ParaLLEl-RDP conformance vectors (T-33-005). **RESOLUTION (2026-07-25) — the register-sourced exotic inputs are wired.** `PRIM_LOD_FRAC` (RGB mul-select 14, alpha mul-select 6, extracted from `Set Prim Color` word-0 low byte; `min_level`, bits 12:8, stays deferred — it is not stored, and lands with its LOD consumer rather than as unread state) and the `Set Convert` (`0x2C`) constants `K4` (RGB sub-B select 7) and `K5` (RGB mul-select 15, both raw 9-bit from `lo[17:9]`/`lo[8:0]`) now route through the combiner instead of reading zero. Validated byte-for-byte against Angrylion by three new non-vacuous conformance vectors — `tex_tri_primlodfrac_16` (`One * prim_lod_frac`, golden `0x8421` gray vs black if unwired), `tex_tri_convert_k45_16` (`(One − K4) * K5` with bit-8-clear K4/K5, golden `0x94a5` vs black if unwired), and `tex_tri_convert_kneg_16` (a **negative** `K4 = 0x1C0` = −64, golden `0x5295` gray — correct only if the raw 448 is `special_9bit`-expanded to −64; a raw-positive read clamps black) — plus two mutation-checked unit tests (`combine_cycle_routes_prim_lod_frac`, `combine_cycle_routes_convert_k4_k5`). The sign path is faithful to Angrylion, which likewise stores K4/K5 **raw 0..511** (`rdp_set_convert`) and sign-extends in the equation: sub-B via `special_9bit_exttable` (`combiner.c:481`, reproduced bit-for-bit by RustyN64's `special_expand`) and the mul via `SIGNF(c, 9)` (RustyN64's `sext9`). **n64-systemtest impact: none** — the suite has no RDP-combiner coverage, so its failing-assertion count is unchanged (90); R-10 is validated only by the Angrylion conformance vectors. **RESOLUTION (2026-07-25) — the chroma-key combiner inputs are wired.** `Set Key GB` (`0x2A`) and `Set Key R` (`0x2B`) now decode the per-channel key **centre** and **scale** (bit-layout ported from Angrylion `rdp_set_key_gb`/`rdp_set_key_r`: GB `lo` = `centre_g[31:24] scale_g[23:16] centre_b[15:8] scale_b[7:0]`, R `lo` = `width_r[31:16] centre_r[15:8] scale_r[7:0]`), and they route through the combiner as **KeyCentre** (RGB sub-B select 6) and **KeyScale** (RGB mul-select 6) instead of reading zero — matching Angrylion `combiner.c` cases 6. The key **width** is not stored: it drives only the deferred chroma-key alpha compare, not the combiner mux, so it lands with that consumer (the `min_level` precedent). Validated by two mutation-checked unit tests — `set_key_decodes_centre_and_scale_per_channel` (distinct per-channel values pin the decode field positions) and `combine_cycle_routes_chroma_key` (`(One − centre) * scale >> 8` with per-channel centre `[32,64,96]`/scale `[64,128,192]` → `[56,96,120]`; unwiring either input changes the result) — **and byte-for-byte against Angrylion** by the end-to-end conformance vector `tex_tri_chromakey_16` (centre `[0x20,0x40,0x60]`, scale `[0x40,0x80,0xC0]` → RGBA5551 `0x3b1f`, black if unwired), which exercises the decode + mux through the real RDP pipeline. **n64-systemtest impact: none** — no RDP-combiner coverage; count unchanged (90). **Still open** (genuinely need machinery that does not exist yet, read as zero): **noise** (RGB sub-A select 7 — needs a per-pixel noise source), the **derivative-computed `lod_frac`** (RGB mul-select 13 / alpha mul-select 0 — needs the LOD/mip pipeline, pairs with R-13's mip tile selection), and the **YUV convert `K0`–`K3`** coefficients (the `Set Convert` hi word, for the YUV texture path). **RESOLUTION (2026-07-26) — the chroma-key alpha compare (`key_en`) is wired.** `Set Other Modes` bit 40 (`hi >> 8 & 1`) decodes to `OtherModes.key_en`, and `Set Key GB`/`R` now also store the per-channel `key_width` (GB `hi[23:12]/[11:0]`, R `lo[31:16]`). When `key_en`, `Rdp::combine` takes the Angrylion `combiner_1cycle` key path (gated so the common path stays byte-identical — all prior 31 `.rvec` vectors unchanged): the RGB output is the sub-A **chromabypass** colour (clamped), and the pixel alpha is `chroma_key_min` over the **pre-`>>8` 17-bit** combined colour (`combine_channel_17bit` = `((A−B)*C + (D<<8) + 0x80) & 0x1ffff`, matching `color_combiner_equation`) and the key widths — per channel `SIGN(col,17)` folded (`-k`, or `-k+0x10` when the low nibble is 8), `+ (width<<4)`, `min`-of-3, clamp `[0,0xff]`. Validated byte-for-byte against Angrylion **end-to-end** by `tex_tri_chromakey_alpha_16`, which makes the key alpha **observable** via alpha-compare (bit 0) at a `Set Blend Color` threshold of `0x80`: the Shade triangle is written only where `chroma_key_min >= 0x80` (the combine yields exactly `0x80`, so it is drawn — a `chroma_key_min` off by −1 shifts it below the threshold and the triangle vanishes, mutation-verified; clearing `key_en` outputs the combined colour instead of Shade). The `chroma_key_min` fold is **additionally** unit-tested directly with hand-computed values including the bit-16-set (negative) branch (`chroma_key_min_folds_and_takes_the_minimum`). **n64-systemtest impact: none** (no RDP-combiner coverage; count 90). **RESOLUTION (2026-07-26) — the derivative `lod_frac` input is wired** (RGB mul-select 13 / alpha mul-select 0), computed by the 2-cycle LOD ported under R-13 and validated by `tex_tri_lodfrac_16`; see R-13 for the full disposition. **Still open under R-10:** noise (un-oracled — Angrylion fakes it) and the YUV `K0`–`K3` convert |
| R-13 | Triangle **texturing** (T-33-004 PR-B 2b) samples the tile per pixel via `decode_texture` + `interpolate_st` + `fetch_texel`, with **both** the non-perspective path and the **perspective divide** (the 64-entry `perspective.h` reciprocal LUT + normalisation shift + out-of-bounds saturation + `w <= 0` carry, gated on `persp_tex_en`) now implemented; the tile shift/clamp/mask, the 3-point bilinear, the mask-wrap seam, the 2-cycle `texel1` (tile+1), the primitive base tile, **`mid_texel`**, the 2-cycle **`lod_frac`**, and the LOD-driven **mip tile selection** are all resolved too (see the dated RESOLUTIONs below), leaving only the **1-cycle** LOD form | The non-perspective path and the sampler wiring are the tractable first step (the flat-coordinate case is scale-independent, so a textured triangle can be validated end-to-end without the divide); the perspective LUT is a precision-critical port best hand-verified in isolation, and the triangle coordinate wrap/clamp is a combinatorial surface for the conformance fuzz | absolute — real texture-path modelling gaps, oracle-confirmed | **RESOLVED (2026-07-23) — see the RESOLUTION at the end of this cell. Retained below is the (thrice-corrected) investigation trail. Open — a REAL divergence is pinned (after two mis-diagnoses, now settled by direct instrumentation).** The `tex_tri_16` conformance vector (committed **`#[ignore]`d**) is the first to drive `interpolate_st` against Angrylion, and its golden differs from RustyN64. Two earlier revisions of this entry were **wrong** and are retracted: (a) a `v >> 16`-vs-s10.5 coordinate claim, and (b) a "malformed vector / unconfigured tile / `SSS = 0`" claim — the latter came from reading the wrong vector's debug output (a global counter had captured an earlier *shade* triangle, not this textured one). Correctly instrumented, the vector is **well-formed**: at its own sample time tile 0 is configured (`size = 2`, `format = 0`), the S coordinate advances (`SSS = 0,1,2,3,4,5` across the drawn columns), and Angrylion **fetches texel 0 = `(255,0,0,255)` = red correctly**. The mismatch is therefore a **real** RDP behaviour RustyN64 does not yet model, from two effects seen in the reference: (1) the **1-cycle TEXEL0 pipeline** — Angrylion's `combiner_1cycle` swaps `texel0_color = texel1_color` (combiner.c) before the combine, the documented hardware quirk that a `TEXEL0` reference in 1-cycle mode is *pipelined*, so a texel0-passthrough does **not** emit the just-fetched texel; and (2) the **s10.5 texel-coordinate scale** — `SSS` spans only `0..5` in `s.5` units (< one texel), so Angrylion point-samples texel 0 across the whole triangle, whereas RustyN64's `interpolate_st` (`v >> 16`) advances one texel per pixel. RustyN64 models neither. The v2 `.rvec` **preload** plumbing is independently verified (an all-white texture renders white; Angrylion's 16-bit `tmem_formatting` loads `tmemidx0 = 0xF801`). **DEFINITIVE CORRECTION (2026-07-23, source-verified — the two "effects" above are BOTH retracted, the third mis-diagnosis of this entry).** (1) The **1-cycle TEXEL0 pipeline is NOT a net offset.** `combiner_1cycle` (`combiner.c:173`) does **not** swap texels; the swap `texel0_color = texel1_color` lives in `combiner_2cycle_cycle1` (`combiner.c:348`, 2-cycle only) and, as a *lookahead*, in `render_spans_1cycle_complete` (`rasterizer.c:336`) — but there it is net-zero: `texel1` is computed at pixel *j*'s next coordinate, then reused as pixel *j+1*'s `texel0`, so `texel0` at every pixel equals that pixel's OWN sampled texel. There is no offset to model. (2) The claim that **Angrylion "fetches red texel 0 correctly"** is **wrong**: the committed `tex_tri_16` golden is uniform `0x0001` (RGB 0 + coverage) across the whole triangle — Angrylion samples a **black** texel at *every* covered pixel, not the ramp. So the real divergence is that Angrylion's texture sample yields **zero** where RustyN64 fetches the loaded texel (`0xF801`). **The coordinate scale is RULED OUT** by a decisive probe (2026-07-23): a *constant*-coordinate variant (`dsdx = dsde = 0`, so `S = T = 0` at every pixel — sampling texel 0 = red everywhere) renders the **same** all-black `0x0001` triangle in Angrylion. If the coordinate were the divergence, a fixed `S = 0` would show red; it does not. So the fault is in the **texture load / tile / sampler configuration for the triangle 1-cycle path**, not the coordinate — Angrylion's triangle sampler returns black even for texel 0. (The copy-mode path samples the loaded texels fine, but it is a *different* sampler — a direct TMEM blit, not `texture_pipeline_cycle` — so it does not prove the triangle sampler config.) Next: isolate load-vs-sampler by driving a **COPY-mode** rectangle through the *identical* 8×1 texture + `Set Tile` + `Load Tile` (if that shows the ramp, the load is fine and the triangle tile/sampler config — `Set Tile` line/mask/format, `Set Tile Size`, and the `Set Other Modes` sample/texture bits — is the fault); then build a **correct** textured-triangle reference whose Angrylion output is verified to show the ramp, and only then compare RustyN64. Do NOT implement against the current `tex_tri_16`, whose oracle output is a degenerate all-black frame. **RECONCILIATION (later same-day, source-verified — partially walks back the "coordinate ruled out" above):** the `0x0001` black is *not* caused by the coordinate, that stands — but there ARE **two separate** issues, not one. (A) A **real RustyN64 coordinate gap IS confirmed**: `texture_pipeline_cycle` (`tex.c:182`) takes `sfrac = sss1 & 0x1f` and hands `sss1` *with its 5 fractional bits* to `fetch_texel_quadro` (`tex.c:246`), which shifts `>> 5` for the integer texel — so the RDP texel coordinate is **s.5** and RustyN64's `interpolate_st` (`v >> 16` used directly as the texel index, no `>> 5`) advances 32× too fast. The original "s10.5" note was right about this; only the *tex_tri black* is unrelated to it. (B) The **black itself** is that Angrylion's texel-0 fetch returns zero even at `SSS = 0` (texel 0) with the combiner (`add_rgb1 = 1 = texel0`, `combiner.c:540`), tile (0, loaded), and render path (`render_spans_1cycle_notexel1`) all verified to match — so the fault is inside `fetch_texel`/`tmem.c` addressing (the load-vs-sample TMEM layout for the 8×1 tile), still open. Fix order: root-cause (B) against `fetch_texel_quadro`/`tcshift`/`tcmask` or a hand-built known-good textured vector, then implement (A)'s `>> 5` and validate. **RESOLUTION (2026-07-23):** both landed. (B) The all-black frame was a **vector bug, not a RustyN64 gap** — `Set Other Modes` had `bi_lerp0 = 0` (bit 11), which selects the RDP's **YUV colour-convert** texture path (`tex.c` `texture_pipeline_cycle`, the `!bilerp` branch: `TEX->r = t3.b + ((k*_tf·t3.g + 0x80) >> 8)`); with the convert coefficients unset that computes an RGBA texel's output from its chroma (`t3.b = 0` for red) → black. Setting `bi_lerp0 = 1` selects the normal RGBA fetch. (A) RustyN64's `interpolate_st` returned `v >> 16` as the texel index directly; the RDP coordinate is **s.5**, so the index is `(v >> 16) >> 5` — added the `>> 5`. With both, the corrected `tex_tri_16` (advancing S, which the `>> 5` collapses to a solid-red triangle) and a new constant-coordinate `tex_tri_fixed_16` **both pass byte-for-byte vs Angrylion** and `tex_tri_16` is un-ignored. One self-asserted unit test (`shaded_and_textured_triangle_reads_texture_past_shade`) was corrected to the s.5 convention (its `S = 1` became `S = 32`). **TILE COORDINATE TRANSFORM RESOLVED (2026-07-25).** The triangle sampler now applies the tile **shift → tile-origin subtraction → clamp → mask/mirror** to the raw `s10.5` coordinate before `fetch_texel` (`sample_coord`, wired into `combined_color`), a bit-exact port of the ParaLLEl-RDP sampler order (`tcshift_cycle` → `TRELATIVE(SL<<3)` → `tcclamp_cycle_light` → `tcmask`): clamp is active when `clamp_s \|\| mask_s == 0`, the over-`SH` test is against the **raw absolute** `SH` (pre-subtraction) and substitutes the **relative** width `(SH>>2)−(SL>>2)`, and it sits *before* the mask (masking first corrupts the over-max/negative detection). Validated byte-for-byte against Angrylion by two new vectors — `tex_tri_clamp_16` (a 4-texel tile, `clamp_s`, `S` running past `SH` → `R,G,B,W,W,W`) and `tex_tri_wrap_16` (`mask_s = 2` → `R,G,B,W,R,G`) — plus a mutation-checked `sample_coord` unit test (shift/subtract/clamp/mask/mirror/negative-clamp). `interpolate_st` now returns the pre-`>>5` `s10.5` coordinate so the shift and tile-size clamp operate on the true value; the self-asserted `shaded_and_textured_triangle_reads_texture_past_shade` was given a valid `SH`/`TH` (the clamp needs a tile size). **BILINEAR RESOLVED (2026-07-25).** The N64's characteristic **3-point (triangular)** filter is now modelled (`sample_type` bit 45 decoded; `bilinear_3point` + `sample_axis`, wired into `sample_texel`): the base coordinate runs shift → subtract → clamp (zeroing the sub-texel fraction on clamp, per `tcclamp_cycle`) → mask, then four texels `(s,t)/(s+1,t)/(s,t+1)/(s+1,t+1)` are blended by `upper = (sfrac+tfrac) & 0x20` — the lower-left triangle uses `t0,t1,t2`, the upper-right `t3,t2,t1` with inverted fractions, each channel a `+0x10 >> 5` round (a faithful port of ParaLLEl-RDP `texture_pipeline_cycle`). Validated byte-for-byte against Angrylion by `tex_tri_bilinear_16` (an 8×8 gradient sampled at 0.5 texel/pixel in **both** axes, so R interpolates `0,2,4,6,8,10` where point sampling would step `0,0,4,4,8,8`, and pixels hit **both** triangle branches), plus mutation-checked `bilinear_3point` (both triangles, hand-computed) and `sample_axis` (fraction capture + zero-on-clamp) unit tests. **MASK-WRAP SEAM RESOLVED (2026-07-25).** The bilinear neighbour is no longer a hardcoded `+1`: `mask_coupled` ports `tcmask_coupled`'s `sdiff`/`tdiff` — `+1` normally, `0` at a wrap seam (the "duplicate the last texel" quirk), `-base` at a mirror-off period end (neighbour wraps to 0), `-1` in a mirrored half; the neighbour is `base + diff`, *not* re-masked (`is_t` uses the T `-(base & 0xff)`). Validated byte-for-byte against Angrylion by `tex_tri_bilinear_wrap_16` (a 2-texel `mask_s = 1` tile whose `S = 1.5` seam column blends green+red — the wrapped texel 0 — not green+black; non-vacuous because the wrapped texel differs from the pre-fix unloaded read) plus a mutation-checked `mask_coupled` unit test. **2-CYCLE `texel1` RESOLVED (2026-07-25).** 2-cycle mode now samples a **second texel from `tile+1`** at the same coordinate (`combined_color`), and `combine` swaps `texel0`/`texel1` before cycle 1 (`combiner_2cycle_cycle1`), so cycle 1's `TEXEL0` reads `tile+1`. Validated byte-for-byte against Angrylion by `tex_tri_2cycle_16` (two 1-texel tiles red/green, both cycles output `TEXEL0` → the pixel is green because the swap brings `tile+1` into cycle 1; red without the swap) plus a mutation-checked `combine_two_cycle_swaps_texels` unit test. **PRIMITIVE BASE TILE RESOLVED (2026-07-26).** The sampler no longer hardwires tiles 0/1: `triangle_fill` decodes the command's base tile (`(hi >> 16) & 7` = bits 50:48, `ewdata[0]` in Angrylion `rasterizer.c:1887`) and threads it through `combined_color`/`depth_span`, which sample `tiles[base_tile]` (and `tiles[(base_tile + 1) & 7]` for the 2-cycle `texel1`). Validated byte-for-byte against Angrylion by `tex_tri_base_tile_16`: the ramp is loaded into **tile 3** at a non-zero TMEM word and the triangle names tile 3, so its golden is identical to `tex_tri_16` (same picture, different tile) while the pre-fix `tiles[0]` read samples the unloaded low TMEM and renders black (mutation-witnessed: got `[00,01]` vs golden `[F8,01]`). The 2-cycle `base + 1` path is guarded by `tex_tri_2cycle_16`, which still passes. **MID-TEXEL RESOLVED (2026-07-26).** `Set Other Modes` bit 44 (`mid_texel`) now feeds `bilinear_3point`: when set and the bilinear sample lands exactly on the texel centre (`sfrac == tfrac == 0x10`), the four neighbours are averaged (`t3 + ((((t1+t2)<<6) − (t3<<7) + ((!t3+t0)<<6) + 0xc0) >> 8)`, Angrylion `tex.c` `center`/`centerrg` case) instead of the 3-point triangle pick — which, unlike the triangle pick, uses all four texels including `t0`. Validated byte-for-byte against Angrylion by `tex_tri_mid_texel_16`: the bilinear setup of `tex_tri_bilinear_16` over a **non-planar checkerboard** (a smooth gradient is planar, so its centre average equals its 3-point pick and bit 44 would be invisible — a vacuous golden), whose golden carries the midpoint value `0x8001` (R=16) that a 3-point pick of the checkerboard extremes never produces. Mutation-witnessed: disabling the centre branch turns pixel (3,3) from the golden `[80,01]` to `[F8,01]` (the 3-point extreme). Plus a mutation-checked `bilinear_3point` unit test (`t0` off the gradient plane so the centre average `98` differs from the 3-point `48`). **n64-systemtest impact: none** — the suite has no RDP texture-path coverage (nothing renders a textured triangle during a systemtest run), so its failing-assertion count is unchanged (90); mid-texel is validated only by the Angrylion conformance vector. **LOD FRACTION RESOLVED (2026-07-26).** The derivative-computed `lod_frac` is now modelled for **2-cycle** mode — a port of Angrylion `tclod_2cycle` + `lodfrac_lodtile_signals` (`tcoord.c`) down to their `lf` output. The texture setup gained the per-**Y** derivative (`DsDy`/`DtDy`/`DwDy`, block words 5/7, previously unparsed — the LOD's second delta pair uses the true vertical gradient, *not* the major-edge `de` the scanline walk steps by); `Set Prim Color`'s `min_level` (bits 12:8) and the triangle command's `level[2:0]` (`max_level`, bits 53:51) are now stored; and `Set Other Modes` `tex_lod_en`/`sharpen_tex_en`/`detail_tex_en` (bits 48/49/50) are decoded. The LOD is the larger of the `+dsdx` and `+dsdy` coordinate deltas (each through the same perspective divide as the pixel's own coordinate, `lod_delta` = `tclod_4x17_to_15`'s magnitude fold), then `lod_frac_of` maps it to the raw 9-bit fraction — sign-extended downstream by `sext9`, matching how `K4`/`K5` are stored. It reaches the combiner through the previously-dead **RGB mul-select 13 / alpha mul-select 0** (closing that half of R-10). Computation is gated on Angrylion's `dolod` (`tex_lod_en` or the combiner actually selecting `LODFrac`), so the common path is unchanged and pays nothing. Validated byte-for-byte against Angrylion by `tex_tri_lodfrac_16`: a 2-cycle textured triangle with `dx.S = 48` but `dy.S = 112` and `de = 0`, so the LOD settles at **112** and — with `level = 2`, which keeps it out of the "distant" saturation — the fraction is the real weight `((112 << 3) >> 1) & 0xff = 0xc0`, emitted as the pixel colour (golden `0xc631`). Three mutation checks: unwiring select 13 renders black; reading `de` instead of `dy` for the Y tap gives LOD 48 → `0x80`; dropping the `l_tile` shift gives `0x80`. (The first draft of this vector left `dy = 0`, which made the `de`-vs-`dy` mutation **pass** — the vector was rebuilt with `dy` dominating so the tap is genuinely pinned.) Plus hand-computed `lod_delta` and `lod_frac_of` unit tests covering the fold, the max, the saturation marker, and each `lf` branch (in-range / distant / lodclamp / magnify / sharpen / `min_level` floor). **n64-systemtest impact: none** (no RDP texture-path coverage; count stays 90). Still open: the **1-cycle** LOD form (`tclod_1cycle_current_simple` compares the `x+1` and `x+2` taps and needs the span-edge signals `endspan`/`longspan`/`midspan`/`validline` the rasteriser does not model — deferred rather than approximated with the 2-cycle formula, and it reads zero meanwhile) **MIP TILE SELECTION RESOLVED (2026-07-28).** With `tex_lod_en` (bit 48) the 2-cycle sampler now reads the mip pair the LOD selects instead of `base`/`base+1` — a port of the tile-selection tail of Angrylion `tclod_2cycle`, via `lod_mip_tiles`: a *distant* LOD pins the level to `max_level`; otherwise it is `l_tile`; the pair straddles the mip boundary (`base+level`, `base+level+1`) and collapses to one tile where there is nothing to blend toward (distant, or magnifying without `sharpen_tex_en`); `detail_tex_en` shifts both one level finer; every index wraps mod 8. `lod_frac_of` was generalised to `lod_signals`, returning all four Angrylion outputs (`frac`/`l_tile`/`magnify`/`distant`) rather than discarding three. Validated byte-for-byte against Angrylion by `tex_tri_mip_tile_16`: the LOD-112/`level = 2` setup gives `l_tile = 1`, so from base tile 0 the pair is (1, 2) and — with three 1-texel tiles holding red/green/blue and a `TEXEL0` passthrough — the pixel is **blue** (tile 2, via the cycle-1 swap); disabling the selection samples the un-selected pair and renders green, mutation-verified. Plus a hand-computed `lod_mip_tiles` unit test covering the straddle, the distant pin-and-collapse, plain-magnify collapse, the `sharpen` exception, the `detail` shift, and the mod-8 wrap. **A vector-authoring trap worth recording:** the first draft set only `bi_lerp0` (bit 11) and not **`bi_lerp1` (bit 10)**, so cycle 1's `texture_pipeline_cycle` took the YUV colour-convert path and Angrylion rendered **white** — the same class of vector bug as the original `tex_tri_16` `bi_lerp0` mistake. A 2-cycle textured vector must set both. **n64-systemtest impact: none** (no RDP texture-path coverage; count stays 90). Still open under R-13: only the **1-cycle** LOD form |
| R-12 | The Z-buffer machinery — the depth **codec** and **`depth_test`** with the depth-source commands (**PR-A**), the **Z-buffer RDRAM read/write** + **hidden bits** (**PR-B part 1**), and the **per-pixel depth test + Z-write** in the triangle rasteriser (**PR-B part 2a**: z-suffix decode, `interpolate_z`, `depth_span`) — is in place; the **combiner→blender colour routing** (part 2b, the colour is still the FILL register) and the **coverage accumulator** at edges (part 2c) are not yet wired, and the `dz` derivation is a first-cut integer gradient | These land — and are tested — ahead of the pipeline integration, which is the larger, riskier surface (the flat-fill→per-pixel rewrite that also closes R-9). Splitting keeps each PR reviewable (the project's split-large-tickets rule). The hidden-bit RAM is modelled accurately (additive default-no-op `RdramBus` methods + a lazily-allocated Bus store) rather than approximated, so the exact `dz` precision the conformance gate needs is preserved | absolute — a coverage boundary, not a fitted constant | **Open.** The codec is validated by boundary values + a `z_compress ∘ z_decompress` round-trip; `depth_test` by observable occluding-vs-occluded pairs per Z mode; the storage by a Bus hidden-bit round-trip and a full-`dz` Z-buffer round-trip (nine `rdp` + one `core` unit tests). `depth_test`/`zbuffer_*` have **no runtime caller** yet, so the oracle stays **93** *(as-at — see the note above this table)*. The coverage and routing land in **PR-B part 2** and are validated against the ParaLLEl-RDP conformance vectors (T-33-005) |
| R-11 | The blender (T-33-003) implements the divide-free `(P * a0 + M * (a1 + 1)) >> 5` with the `P/A/M/B` input muxes, both cycles, and `force_blend`; the **anti-aliased-edge divider LUT** (`uBlenderDividerLUT` — the coverage-weighted divide the RDP uses on partially-covered edge pixels), the **memory-alpha interpenetrating-Z blend-shift** path, **alpha-compare**, **dither**, the **`color_on_cvg`** early-return, and the **coverage write-back** (`cvg_dest`) are decoded but unused | These paths need the framebuffer read (`image_read_en` memory colour), the coverage accumulator, and the Z buffer — none of which reach the blender until the triangle pipeline routes combiner→blender per pixel (T-33-004). The no-divide form is the one every non-edge pixel uses, so it is the honest first target; emitting nothing for the deferred paths (rather than a fabricated divide) keeps the gap falsifiable | absolute — a coverage boundary, not a fitted constant | **Open.** The no-divide equation (the `>> 5` fold and the `+ 1` on the `M` term), the `Set Other Modes` (0x2F) field decode, the `P/A/M/B` muxes, and the 2-cycle forward chain are validated bit-for-bit against hand-computed values (four `rdp` unit tests). `blend` now **has a runtime caller** — `depth_span` (T-33-004 PR-B 2b-blend) reads the framebuffer pixel and routes the combiner colour through `blend` when the depth test enables blending, gated on `force_blend` (the reference's `!blend_en` fast-path keeps opaque pixels on the combiner colour). A translucent-triangle integration test proves a 50/50 blend of the combiner colour over a pre-filled background. **Dither is now present** (T-33-004 2c): the ordered RGB dither (magic/bayer matrix) is applied to the combined colour on both the no-Z and depth pixel paths, validated byte-for-byte against Angrylion by `dither_tri_32`. **Alpha-compare is now present on BOTH the no-Z and depth paths** (`Set Other Modes` bit 0): `alpha_compare_passes` gates the pixel write on `combiner_alpha >= Set-Blend-Color alpha`, evaluated before coverage overwrites the alpha byte — validated byte-for-byte against Angrylion by `alpha_compare_16` (no-Z) and `alpha_compare_z_16` (a z-suffixed triangle with `z_update` on / `z_compare` off) plus a boundary unit test. On the depth path the gate sits after the depth test and `continue`s before both the colour write and the z-write, which is observably equivalent to the RDP's pre-depth ordering because the compare is depth-independent (a pixel is written and its depth stored only when both depth and alpha pass). Still deferred here: the dithered-threshold variant (`dither_alpha_en`); the AA-edge divider LUT, the interpenetrating-Z blend-shift, `color_on_cvg`, and coverage write-back — these need the sub-pixel coverage accumulator (slice 2c). The oracle stays **93** (no systemtest drives the render path) *(as-at — see the note above this table)*. The deferred paths are validated against the ParaLLEl-RDP conformance vectors (T-33-005) |
| R-9 | The triangle rasteriser now interpolates **depth** (T-33-004 PR-B 2a), **shade** (2b), and **texture** (`S/T/W` → `fetch_texel`, 2b-texture) per pixel, and routes the combiner colour through the **memory-read blender** when the depth test enables blending (2b-blend, gated on `force_blend`) — depth-tested, Gouraud-shaded, textured, and translucent triangles render; what remains is the bit-exact **sub-pixel coverage** (ParaLLEl-RDP's `quantize_x` sticky-bit edge rounding and the `do_offset` last-subpixel latch) and the coverage-driven AA blend, so each edge is still reduced to whole pixels (`>> 16`), and the `dz` derivation is a first-cut gradient | Landing the edge-walk, then the depth and shade interpolators (each hand-verified), then texture and the sub-pixel coverage is the tractable order; the coverage rule and the full combiner→blender→memory surface are a combinatorial space best pinned by the conformance fuzz (T-33-005) | absolute — a coverage boundary, not a fitted constant | **Open.** The flat fill, the depth test (occluding-triangle pairs), the shade interpolation (hand-computed base colour + a combiner-routed shaded triangle), the texture sample, and the memory-read blender (a 50/50 translucent triangle over a pre-filled background) are each unit/integration-tested. The sub-pixel coverage **primitives** — `compute_coverage` (the 4×2 diamond-sample 8-bit mask) and `quantize_x` (the `s.16`→`s.3` sticky-bit snap) — are now a bit-exact port of parallel-rdp `coverage.h`/`span_setup.comp`, pinned by hand-computed unit tests derived from the oracle's arithmetic (full/partial/empty masks, the sticky bit). The primitives are now **wired into the 1-/2-cycle rasteriser**: the edge-walk builds per-Y-subpixel `s.3` edges, `pixel_coverage` gates each pixel (AA-off top-left-sample rule) and stores the coverage count in the pixel alpha (`(count − 1) & 7`, the `cvg_dest` clamp write-back). FILL/COPY mode keeps the whole-pixel span (correct — FILL renders "without subpixel accuracy"). This is validated against Angrylion: `fill_tri_frac_16` (FILL rounds fractional edges to whole pixels) and `shade_tri_frac_16` (a 1-cycle shaded triangle excluding a fractional-edge column and leaving a partially-covered column at reduced coverage) pass byte-for-byte. The **depth path** applies the same coverage (`shade_depth_tri_frac_16` — a z-suffixed fractional triangle — renders identically to `shade_tri_frac_16`, byte-exact vs Angrylion). **Ordered RGB dither is now wired** (T-33-004 2c): the combined RGB is dithered per pixel by the magic/bayer 4×4 matrix before write-back (`apply_rgb_dither`, a bit-exact port of Angrylion `dither.c` `rgb_dither`), validated byte-for-byte by `dither_tri_32` (the RDP default magic dither over a `0x112233` shade). **Alpha-compare and `cvg_dest = full` are now wired**: `pixel_coverage` stores full coverage (7) when `cvg_dest = 2` (validated by `cvg_dest_full_16` — a fractional triangle whose partial edge column stores `0xf801` instead of the clamp `0xf800`), and the alpha-compare write gate is on both pixel paths (R-11). Remaining slice-2c residual: the coverage-weighted **interpenetration Z** path, the **AA-edge blend**, and the **wrap/save `cvg_dest`** modes (these need the memory-read coverage accumulator) are not wired — each to be pinned by further conformance vectors |
| R-8 | Copy-mode `Texture Rectangle` (T-32-004) is wired for a **16-bit tile → 16-bit colour image** (the first-picture path), including the **4-pixels-per-cycle sub-texel selection** under non-1:1 `DsDx`; `Texture Rectangle Flip` (0x25), the 8/32-bit and TLUT copy paths, and the copy alpha-compare are not modelled — an unsupported configuration draws nothing | The full copy pipeline (per-format `dx_shift`/`s_offset` 64-bit-group fetch, the 8-bit high-word replication quirk, the RGBA5551 alpha-on-LSB test) is a combinatorial surface best pinned by the bit-exact fuzz rather than by hand. The 16-bit 1:1 path is the one a first textured frame needs, and its horizontal step is scaled by `>> (5 + dx_shift)` so a canonical `DsDx = 4.0` advances one texel per pixel | absolute — a coverage boundary, not a fitted constant | **Open — but the 16-bit 1:1 copy is now ORACLE-VALIDATED.** Beyond the internal **round-trip identity** test (`Load Tile` loads a 4×2 texture and `Texture Rectangle` blits it back byte-for-byte; load and fetch share the odd-row swap) and the `wrap_coord` unit test, the `tex_rect_copy_16` conformance vector (T-33-005) now drives the full command sequence — Set Texture Image → Set Tile → Set Tile Size → Load Tile → COPY-mode Texture Rectangle — through **Angrylion** and RustyN64 matches the golden byte-for-byte. This is the **first texture path checked against the oracle** (copy mode bypasses the combiner and the 1-cycle texel pipeline, so it is clean where the `tex_tri_16` triangle path is not — R-13). **The 4-pixels-per-cycle sub-texel selection under non-1:1 `DsDx` is now modelled and oracle-validated too:** COPY mode reads a 64-bit TMEM word (4 consecutive texels) per cycle, so the base texel is evaluated at each cycle's first column (advancing `DsDx × 4` texels/cycle) and the within-cycle offset is a direct `+0..3` TMEM increment, **not** a per-pixel step. The `tex_rect_mag_16` vector (a 2× magnify, `DsDx = 2.0`) reads texels `0,1,2,3,2,3,4,5` and RustyN64 matches Angrylion byte-for-byte; the 1:1 case (`DsDx = 4.0`) is the special case `s = col` and the round-trip + `tex_rect_copy_16`/`_offset_16`/`_8x8_16` vectors still pass. **n64-systemtest impact: not measured** — the copy path has no runtime driver in the suite (nothing calls `texture_rectangle` during a systemtest run), so it is unreachable by n64-systemtest and the oracle count stays **93** *(as-at — see the note above this table)*. Still deferred (each an unsupported config that draws nothing): Flip 0x25, 8/32-bit, TLUT copy, and copy alpha-compare — validated against further conformance vectors as they land |
| R-7 | The TMEM loads (T-32-002) cover **8/16/32-bit** texels for `Load Tile` and **8/16-bit** for `Load Block`. **There is no 4-bit texel *load*** — a 4-bit *texture-image* load is invalid on hardware (Angrylion sets `rdp_pipeline_crashed` and bails, `tex.c:526-533`); games load 4-bit textures by setting an **8-bit** texture image + 8-bit LOAD tile, loading half as many texels raw, then rendering with a **separate 4-bit tile** that extracts nibbles at fetch. That canonical path already works. Genuinely deferred: the **32-bit split** path of `Load Block`, and a *direct* 4-bit LOAD tile paired with an 8-bit texture image (the `ti_size`-vs-`tile.size` load granularity — Angrylion keys the copy stride off `ti_size` and the destination short-index off `tile.size` via `sshorts = s >> 2`) | The earlier "4-bit loading needs nibble addressing" framing was a **misconception**: 4-bit texels are never DMA'd as nibbles — the load is a raw byte stream sized by the texture image, and the nibble semantics live only in `fetch_texel` (already implemented for I4/IA4/CI4). RustyN64's bail on a 4-bit texture image thus *matches* the hardware's invalid-load behaviour. Writing nothing for the two genuinely-deferred cases keeps them falsifiable rather than emitting fabricated texels | absolute — a coverage boundary, not a fitted constant | **The canonical 4-bit path is now ORACLE-VALIDATED (2026-07-25).** `tex_tri_i4_16` (T-33-005) drives the full hardware idiom — 8 I4 texels packed two-per-byte, an **8-bit** `Load Tile`, then a **4-bit** (format I, size 0) render tile sampled across the triangle — and RustyN64's existing 8-bit load + 4-bit `fetch_texel` reproduce **Angrylion byte-for-byte**. Non-vacuous by construction: the eight intensities are DESCENDING and non-zero (texel 0 = white `0xFFFF`, not black), so an all-zero (no-op) load would render black and fail. Since the load is format-agnostic (a raw byte copy), this validates the load mechanism for **all** 4-bit formats; the I4/IA4/CI4 decoders are separately unit-tested. The two remaining gaps above are still `Open` (no runtime driver in n64-systemtest, so they do not change the suite-wide failing count — `docs/STATUS.md` is authoritative for it), to be pinned by further conformance vectors. The supported sizes remain byte-exact against hand-computed expectations, including the odd-row 32-bit-word swap, the 32-bit `Load Tile` split, and the `Load Block` dxt line-parity swap (five `rdp` unit tests) |
| R-5 | **[HISTORICAL BASELINE — SUPERSEDED by the dated RESOLUTIONs in the disposition column, which are the current normative answer. Retained per the immutable-reference rule.]** VI scan-out (T-31-004) *was* a **1:1 copy** — `VI_X_SCALE`/`VI_Y_SCALE` resampling and the AA / divot / de-dither post-filters were not applied, and the height was derived directly from `VI_V_VIDEO`'s active half-lines rather than from the scale-accumulated framebuffer walk (this is the live `Bus::scanout`; the accurate `Bus::scanout_scaled` now supersedes it — see the RESOLUTIONs) | The framebuffer→RGBA8 pixel conversion is exact and cited: the pixel *format* is selected by `VI_CTRL.TYPE[1:0]` (N64brew *Video Interface* §VI_CTRL — 2 = RGBA5551, 3 = RGBA8888), the RGBA5551 bit layout (R[15:11] G[10:6] B[5:1] A[0]) is the N64 16-bit colour format (N64brew *Reality Display Processor/Commands* §Set Color Image, texture/format enum; *Video DAC*), and the 5→8-bit widening by high-bit replication is the standard N64 convention (the value the VI DAC emits). What is **deferred**: the geometric resampling — the VI accumulates a sub-pixel step of `VI_X_SCALE`/`VI_Y_SCALE` per pixel/line (N64brew *Video Interface* §VI_X_SCALE, §VI_Y_SCALE) — and the analog post-filters `AA_MODE`/`DIVOT_ENABLE`/de-dither (§VI_CTRL), which only matter once scaled or anti-aliased content is scanned | absolute — a resampling/filter geometry choice, not a timing interval | **Open.** Byte-exact for a 1:1, unfiltered scan of a framebuffer whose width matches `VI_WIDTH` — which is what the FILL pipeline produces and the T-31-004 unit tests pin. Scaling and the post-filters **will be** validated bit-for-bit against Angrylion via the ParaLLEl-RDP fuzz suite / VI golden frames (Sprint 3), and superseded here if they diverge — this entry stays open until then. **n64-systemtest impact: not measured** — `Bus::scanout` has no runtime driver (nothing in the run loop calls it), so it is unreachable by the suite and cannot change the count, which stands at 90 (`docs/STATUS.md` is authoritative). **PARTIALLY RESOLVED (2026-07-25) — Slice 1, the scale accumulator + geometry.** A VI oracle now exists (earlier "no VI oracle" assumption is retired): the Angrylion driver's `vdac_write` captures `vi_process_full`'s output after `n64video_update_screen`, emitting `.vivec` goldens (`crates/rustyn64-test-harness/vectors-gen/driver.c`, format = 15-u32 header + logical source + golden RGBA8). A new `Bus::scanout_scaled` reproduces the hardware geometry bit-for-bit: the **2.10 fixed-point X/Y accumulator** (`line_x = x_offs >> 10`, source index `stride*srcY + srcX`), the **NTSC 108-px horizontal overscan** (`h_start -= 108`) with the left/top clamps folding the crop into the accumulator start, the `minhpass=8`/`maxhpass=hres-7` overscan crop, the `PRESCALE 640×625` clamp, and the **truncating RGBA5551→8** conversion the VI actually uses (`(px>>8)&0xF8` — *not* `expand5`'s high-bit replication; corrects the "5→8 by replication" claim above for the VI path specifically). Validated by `vi_scale_1x_16` (1:1, the overscan makes output column 0 sample source column 8) and `vi_scale_down2x_16` (2× downscale, the accumulator steps two source pixels) — both byte-for-byte (RGB) vs Angrylion — plus mutation-checked unit tests (`scanout_scaled_geometry_and_truncating_convert`, and the conformance mutation dropping the overscan offset goes red). This slice is `aa_mode = REPLICATE` (nearest, no lerp). **Slice 2 (2026-07-25) — the 5-bit bilinear lerp.** `aa_mode != REPLICATE` with a non-zero fraction now bilinearly resamples: four texels `(sx,sy)/(sx+1,sy)/(sx,sy+1)/(sx+1,sy+1)`, vertical-lerped per column by `yfrac` then horizontal-lerped by `xfrac`, each channel `a + (((b-a)*frac + 16) >> 5)` (`vi_lerp3`, a port of Angrylion `vi_vl_lerp`; `frac` = fraction bits `[9:5]`). Validated byte-for-byte vs Angrylion by `vi_scale_bilinear_16` (2× upscale, `aa_mode = RESAMP_ONLY`, `xfrac`/`yfrac` alternating 0/0x10 — the clean 50 % blend) and `vi_scale_bilinear_odd_16` (scale `0x240`, fractions ≢ 0 mod 4 so the `+16` rounding actually flips the result — dropping the `+16` fails *this* vector while the 0x200 one stays green, because its products are all multiples of 32), plus a mutation-checked `vi_lerp3` unit test. Bilinear applies to the 16-bit path in this slice; **32-bit source bilinear followed in slice 4b** (`vi_scale_bilinear_32`, oracle-validated), so bilinear now applies to both source formats. **Slice 3 (2026-07-25) — the gamma curve.** `gamma_enable` (VI_CTRL bit 3), with `gamma_dither` (bit 2) clear, applies the sqrt gamma table to the final RGB: `gamma(v) = sqrt(v << 6) << 1` (`vi_gamma` / `vi_integer_sqrt`, a port of Angrylion `vi_gamma_init`). Validated byte-for-byte vs Angrylion by `vi_gamma_1x_16` (`VI_STATUS = 0x030A`, nearest + gamma; `gamma(0x40) = 0x80`, non-vacuous vs the raw sample) plus a mutation-checked `vi_gamma_curve` unit test. The **dithered** gamma variants (bit 2 set, cases 1 and 3) are noise-based and stay deferred. **Slice 4a (2026-07-25) — the PAL active-span geometry.** `v_sync > 550` selects the PAL branch — the horizontal overscan is 128 px not 108 and `vstartoffset` is 44 not 34. **Provenance:** these are Angrylion source constants, not fitted values — `angrylion-rdp-plus/src/core/n64video/vi.c:688` (`h_start -= ispal ? 128 : 108`, `ispal = v_sync > V_SYNC_NTSC + 25 = 550`) and `vi.c:700` (`vstartoffset = ispal ? 44 : 34`); the `vi_pal_geometry_16` golden is generated *by that Angrylion source* through `crates/rustyn64-test-harness/vectors-gen/driver.c` (`make ANGRYLION_CORE=… driver && ./driver …/tests/vectors`), so the values are traceable to the oracle's code, not only to the fixture. That code was present in `scanout_scaled` since slice 1 but unverified; it is now pinned by `vi_pal_geometry_16` (`v_sync = 625`, `h_start = 115` so PAL's −128 clamps to sample source column 13 while a mis-applied NTSC −108 would sample column 8) byte-for-byte vs Angrylion, and a mutation forcing the PAL overscan to 108 fails it. **Slice 4b (2026-07-25) — 32-bit RGBA8888 source with bilinear.** The `.vivec` harness now carries a 32-bit source framebuffer (driver `bpp` field + `rdram_put_fb32`), and `scanout_scaled` dispatches the per-pixel fetch on the format (`vi_fetch16`/`vi_fetch32`) so the bilinear + gamma path runs for both — the 16- and 32-bit paths are now one unified loop. Validated by `vi_scale_bilinear_32` (`VI_STATUS = 0x0203`, 2× upscale) byte-for-byte vs Angrylion; a mutation breaking the 32-bit stride multiply fails it. **Slice 4c (2026-07-25) — the de-dither restore filter.** `aa_mode` 0/1 now reads the real per-pixel coverage (32-bit: alpha bits 7:5 = `(px>>5)&7`), and a fully-covered pixel (`cvg == 7`) with `dither_filter_enable` (VI_CTRL bit 16) takes the **de-dither** `restore_filter32`: over the 3×3-minus-centre 8 taps, each channel is nudged ±1 toward the neighbour's top-5-bit value (a port of Angrylion `restore.c`; the `vi_restore_table` reduces to `+1` if centre5 < neighbour5, `-1` if greater). Validated by `vi_dedither_32` (`VI_STATUS = 0x00010003`, all `cvg == 7`, 1:1 scale so no lerp) byte-for-byte vs Angrylion — non-vacuous (output col 0 = `0x1b` vs the raw `0x20`, the row-0 top taps reading 0) — with a mutation flipping the correction sign failing it. **Slice 4d (2026-07-25) — the AA edge filter.** A partial-coverage pixel (`cvg < 7`) now takes `video_filter32` (Angrylion `video.c`): it gathers the fully-covered pixels among its 6 taps (the up/down diagonals and the two-away left/right), takes the per-channel penultimate min/max (`vi_video_max`, the exact single-pass runner-up algorithm, ported verbatim for its tie-handling), and pulls the centre toward their midpoint — `centre + (((penmin + penmax − 2·centre)·(7 − cvg)) + 4 >> 3)` masked to 8 bits (unsigned two's-complement wrap). Validated by `vi_aa_edge_32` (`VI_STATUS = 0x00000003`, 32-bit with every 4th column partial so each partial pixel's taps are fully covered; the partial pixels are a fixed dark colour, *not* the smooth gradient's local midpoint, so the filter changes them at INTERIOR pixels too rather than only where the top-boundary taps break the gradient symmetry — a CodeRabbit catch) byte-for-byte vs Angrylion — with both a blend-coefficient mutation and a skip-the-filter (return-raw) mutation failing it. **Slice 4e (2026-07-25) — the divot median filter.** With `divot_enable` (VI_CTRL bit 4), a pixel whose 3 horizontal neighbours are not all fully covered takes the per-channel **median** of the post-de-dither/AA-edge values of itself and its left/right source-column neighbours (Angrylion `divot_filter`, the branch-expanded median-of-3 ported for its tie-handling); it is skipped (`(c.a & l.a & r.a) == 7`) where all three are fully covered, so it only touches partial-coverage edges. The coverage-exposing `vi_fetch32_cov` feeds it. Validated by `vi_divot_32` (`VI_STATUS = 0x00000013`, the every-4th-column-partial source) byte-for-byte vs Angrylion — non-vacuous (the partial columns and their neighbours differ from the non-divot AA-edge output, the median ≠ the AA blend), and a mutation forcing the median (removing the median-of-3) fails it. The all-fully-covered **early-return** is separately made observable (CodeRabbit #155): a **non-monotonic** fully-covered probe triplet (source columns 17/18/19 row 10, values low/high/mid per channel so the median ≠ the centre) lands on output pixel (10,10) with zero scale fraction; deleting the early-return computes the median there and fails the vector (`got [80,90,A0]` vs golden centre `[F0,E0,D0]`), where a monotonic gradient would have hidden it (median = centre). **Slice 4f (2026-07-25) — the 16-bit RGBA5551 coverage path.** The coverage machinery is now format-generic. A single primitive `Bus::vi_read_cov(x, y, bpp)` returns raw RGB8 + 3-bit coverage — 32-bit from alpha bits 7:5 (`(px>>5)&7`), **16-bit** from the 9th-bit **hidden plane** (`((px & 1) << 2) \| rdram_hidden`, the pixel's bit 0 as the coverage MSB and the two hidden bits as the low bits, so `cvg == 7` needs bit 0 set **and** hidden `0b11`); every downstream filter (de-dither / AA-edge / divot) then operates on 8-bit channels, so `vi_fetch_cov`, `vi_video_filter`, and `vi_divot` gained a `bpp` argument and the 32-bit path is byte-identical (all ten prior vectors stay green). The hidden read reuses the pre-existing `Bus::rdram_hidden` plane (2 bits/halfword) that until now only served the Z-buffer `dz` path; RustyN64's default-0 borders match the oracle because the harness memsets Angrylion's default-3 plane to 0 and sets the source region explicitly. The `.vivec` format gained **version 2** (a trailing hidden-bits plane, one byte per source pixel) — the driver populates Angrylion's `rdram_hidden` via a new `extern` and emits the plane, the loader packs it into `Bus.rdram_hidden`. Validated byte-for-byte vs Angrylion by three vectors, all with the same geometry as their 32-bit twins but `type = 2`: `vi_dedither_16` (`0x00010002`, all fully covered → the 5-bit de-dither unpack), `vi_aa_edge_16` (`0x00000002`, every 4th column partial → the AA-edge over hidden-plane coverage), and `vi_divot_16` (`0x00000012`, the partial pattern + a non-monotonic fully-covered probe triplet so the divot early-return is observable). Mutation-checked: forcing coverage to 7 (ignoring the hidden read) fails `vi_aa_edge_16`+`vi_divot_16`; neutralising the de-dither nudge fails `vi_dedither_16`+`vi_dedither_32`; removing the divot bypass fails `vi_divot_16`+`vi_divot_32`. **Still deferred within `scanout_scaled` (later slices):** the **gamma-dither** variants (`aa_mode` 0/1 coverage-gated / noise-based), and the **coverage filters under `aa_mode == 2`** (RESAMP_ONLY forces `cvg = 7` on hardware so de-dither can still apply; currently gated to `aa_mode ≤ 1`), and the **field-rate half of R-6** (the 50 Hz PAL field cadence + interlace/serrate + exact H_TOTAL, which live in the `Vi::tick` scan timing, not this scan-out geometry). `scanout_scaled` is **now wired into the frontend** (2026-07-25): `emu::EmuCore::produce_frame` calls `scanout_scaled` instead of the 1:1 `Bus::scanout`, so the presented picture uses the accurate scale-resample + filters — the R-12-style land-ahead-of-caller is now complete for the live path. `Bus::scanout` is retained for the harness frame tests (`composite_frame`/`real_rom_frame`/`golden_frame`/`commercial_boot`), which migrate as R-18 needs them. **`commercial_boot` has since MIGRATED (2026-07-29)** — it now measures through `Bus::scanout_scaled`, so it is a caller of the accurate path, not the 1:1 one; the preceding clause is retained as the historical statement. The frontend's `FB_MAX 640×480` backing store bounds the presented output via two guards: `scanout_scaled` returns `(0, 0)` (→ black) when `w * h * 4` overflows `frame.rgba`, and `presentable_geometry` rejects any dimension past `FB_MAX` (in practice a tall height — `scanout_scaled`'s width is prescale-bounded to `FB_MAX_W`, but its height can reach the 625-line prescale). **n64-systemtest impact: not measured** — `scanout_scaled` now runs in the frontend's `EmuCore::produce_frame` (wired 2026-07-25), but n64-systemtest is a headless CPU/RSP oracle with no VI presentation, so it does not drive the frontend and the scan-out stays unreachable by the suite; the failing-assertion count (90) is unchanged for that reason, not measured against it |
| R-4 | The VI register file (T-31-004) stores the **full 32-bit value** written to each register; the per-register write masks the hardware enforces (`VI_ORIGIN` 24-bit, `VI_WIDTH` 12-bit, `VI_V_INTR` 10-bit, the multi-field `VI_CTRL`/`VI_H_VIDEO`/scale registers, …) are not applied | The masks are documented as *field widths* in N64brew *Video Interface* per register, but the exact discard behaviour on write (which reserved bits read back 0 vs. retain) is what n64-systemtest's VI-register group actually pins, and that has not been run against a masked implementation | absolute — a register-decode fact, not a timing interval | **Open.** In-range writes (every value the register's own fields can hold) round-trip correctly, which the T-31-004 unit tests pin; out-of-range bits are retained rather than dropped. To be measured against n64-systemtest's VI group and masked per register when that group is exercised (measure, don't guess). No assertion currently exercises it (count unchanged at 93 *(as-at — see the note above this table)*) |
| R-3 | FILL-mode `Fill Rectangle` (T-31-003) rasterises the rectangle with an **inclusive lower-right pixel** — floor the upper-left, and draw through the pixel that *contains* the lower-right coordinate; in FILL/COPY mode the low two bits of `yl` are forced set before the shift (`yl \| 3`) so the final scanline fills whole. The **scissor** clips separately with an **asymmetric** lower-right (inclusive X, exclusive Y — see R-15) | The N64brew wiki says only "upper-left rounded down, lower-right rounded up"; the exact edge behaviour is what the Angrylion oracle pins, and it is *inclusive of the lower-right pixel* (a rect `(0,3)-(1,4)` draws columns 0 **and** 1), plus the FILL/COPY `yl \| 3` (Angrylion `rasterizer.c` `rdp_fill_rect`). The earlier `(coord + 3) >> 2` half-open span dropped the boundary row/column — a realisation of the cited prose that disagreed with the hardware. Read from the oracle's output, not computed | absolute — a rasterisation geometry rule, not a timing interval, so the differential/re-phasing test is N/A | **Closed for the integer-coordinate FILL rect (oracle-validated).** The inclusive lower-right + `yl \| 3` rule is now pinned bit-for-bit against Angrylion by the **seeded-fuzz corpus** (`tests/vectors/fuzz/`, 48 random FILL rectangles sweeping colour/size/position with a full-image scissor, all byte-exact) and a mutation-checked unit test (`fill_rectangle_lower_right_edge_is_inclusive`). The fuzz gate **found** the pre-fix off-by-one. Still unverified: **sub-pixel** (fractional-coordinate) rect edges, which the whole-pixel fuzz does not exercise; the **scissor** lower-right rounding is resolved separately (**R-15**, asymmetric X/Y). No n64-systemtest assertion drives the render path (count unchanged at 93 *(as-at — see the note above this table)*) |
| R-16 | The AI's `AI_STATUS` `COUNT` (bits 14:1), `WC` (bit 19), and `BC` (bit 16) readbacks, and the `AI_BITRATE` bit-clock timing, are a **best-effort** derived model, not oracle-pinned. `COUNT` is derived from `last_tick × video_clock / MASTER_HZ` as a sawtooth from `DACRATE/2` (wiki §COUNT); `WC` toggles on its second half-period; **`BC` is now modelled** (see the resolution) | These are the DAC's internal sample/bit-clock phase, which the CPU "cannot reliably sample rapidly enough" (wiki §BC) — so no public capture pins their exact value, and n64-systemtest has **no AI coverage** to gate against (verified by grep of `ref-proj/n64-systemtest`). The flags software actually polls (`FULL`/`BUSY`/`ENABLED`) are exact (ares `io.cpp`); only the sub-sample readback is nominal | absolute — a register-readback phase, no oracle | **Open (striven, ungated).** `FULL`/`BUSY`/`ENABLED` are unit-tested exactly; `COUNT`/`WC` are derived but unverifiable without a hardware logic-analyser capture (the user asked to strive for cycle-exactness and to search for capture data — none was found). To be pinned if an AI-status capture or an n64-systemtest AI group ever surfaces. Emits a plausible value rather than a fabricated constant. **`BC` MODELLED (2026-07-28).** `AI_BITRATE` is documented as "Half of bit clock period of I²S output to DAC", and the bit clock as "the Video clock, divided by two, divided by one more than this number" (N64brew Wiki *Audio Interface* §AI_BITRATE, mirrored at `n64brew_wiki/markdown/Audio Interface.md`) — so one half-period is `BITRATE + 1` video clocks and BCLK toggles once per half-period. `BC` (bit 16) is derived from the same video-clock tick count `COUNT`/`WC` already use, and `BITRATE = 0` stops the clock as documented. The readback gate is **`AI_BITRATE` alone** — the wiki says the counter "always ticks unless `AI_BITRATE` is 0" and never conditions either clock on `AI_DACRATE`, so BCLK keeps running with no DAC rate programmed (pinned by `the_bit_clock_runs_without_a_dac_rate`); `COUNT`/`WC` additionally need a DAC rate to have a range and report nothing without one. **It stays UNGATED, and the source itself is hedged:** the wiki says only that this is "(probably)" toggled per bit and that it "is believed" to be the BCLK line, precisely because the CPU "cannot reliably sample it rapidly enough even when `BITRATE` is set to 15" — i.e. it is not **CPU-observable**. Note the precise scope: that is a limit on *software* sampling, **not** a claim that BC is unpinnable in principle — an external logic-analyser probe on the BCLK line could establish it, and this entry would be updated if such a capture surfaced. What is claimed here is only that **no public capture is known** and none is asserted. Pinned by two tests: the transition count over a fixed span must equal the documented half-period relation derived independently from `video_clock` and `BITRATE` (mutation-checked — using `BITRATE` instead of `BITRATE + 1`, or dropping the bit, both fail), and a `BITRATE` of 0 must hold the line low. **n64-systemtest impact: not measured** — no AI test drives this path (the suite has no AI coverage), so the oracle count is **unchanged at 93** *(as-at — see the note above this table)* |
| C-34 | The console **region** (and so the AI's video clock) is now selected from the cartridge's **destination code** (ROM header `0x3E`), rather than every cartridge booting NTSC. The destination characters come from the N64brew Wiki *ROM Header* §Standard-header table; the **50/60 Hz classification of each territory is the broadcast standard, not an N64-documented fact**, and there is **no oracle** for region detection | The machinery (`Region`, `Region::video_clock`, `Ai::set_region`, both region video clocks) already existed and was already documented, but **nothing drove it** — `set_region` was reachable only from the AI crate's own unit tests, so a PAL cartridge played at the NTSC rate. Wiring the existing selector is strictly better than leaving documented state unread; the classification is the weakest link and is labelled as such rather than presented as measured | absolute — a documented mapping, explicitly ungated | **Open (modelled, ungated) — 2026-07-28.** `Region::from_destination_code` maps the header byte, and `boot::apply_cartridge_region` applies it on **both** boot paths (`hle_boot` and `real_pif_boot`). Three cases are decided explicitly rather than guessed, and are pinned by name in the unit test: **`B` Brazil → NTSC** (Brazil broadcast PAL-**M**, a *60 Hz* standard, so the AI divisor behaves as NTSC); **`C` China → NTSC** (a PAL territory with no known retail N64 — the default is kept rather than inventing a 50 Hz cartridge that never shipped); **`A` "All" → NTSC** (names no single region). Every unrecognised byte also returns NTSC, so an unknown or homebrew code is **behaviour-preserving**. Validated by a table test over all documented characters plus a mutation-checked end-to-end boot test (`a_pal_destination_code_retunes_the_ai_at_boot`: two ROMs differing **only** in byte `0x3E` must boot to different AI sample rates — removing the `apply_cartridge_region` call makes them identical and fails). **No oracle gates this**: no test ROM checks region detection, so it is modelled-and-ledgered in the same posture as R-16/R-17 rather than claimed as verified. **n64-systemtest impact: NOT MEASURED** — the suite has no region coverage, so it cannot establish an impact either way; the separately-observed suite total is unchanged at 90 |
| R-21 | `Fill Rectangle` (0x36) always writes the **`SET_FILL_COLOR` register**, whatever the cycle type. On hardware only FILL mode does that — in 1-/2-cycle mode the rectangle is rasterised through the **combiner/blender** like any other primitive | `fill_rectangle` calls `fill_pixel` unconditionally; the cycle type is never consulted. Every committed conformance vector that exercises `Fill Rectangle` sets FILL mode, so the non-FILL path has no coverage in either direction and the gap was invisible | absolute — a coverage boundary, not a fitted constant | **Open (found 2026-07-28, not yet oracled).** Surfaced *accidentally* while building the microcode end-to-end test: that test's queue initially mis-packed `SET_OTHER_MODES` (the cycle type went into word1 instead of word0 bits 21:20, so the RDP never entered FILL mode) and **the picture was still correct**, which is only possible because `Fill Rectangle` ignores the cycle type. A reviewer flagged the mis-packing; chasing it found this. The queue is fixed and the test now asserts the *emitted* `SET_OTHER_MODES` carries FILL, so the two are no longer confounded. **RESOLVED 2026-07-28 (oracle-confirmed).** Vector `fill_rect_1cycle_16` settles it: a 1-cycle rectangle with prim `(0x22,0x44,0x66)`, a combine selecting prim for RGB and alpha, and a *deliberately different* green fill register `0x07C1` renders **`0x2219` in all 64 pixels** — the prim colour in RGBA5551, not the fill register. So hardware routes a non-FILL rectangle through the combiner and never reads the fill register. `fill_rectangle` now takes the combiner path (with alpha-compare and dither, as the triangle path does) unless the cycle type is `CYCLE_TYPE_COPY` or `CYCLE_TYPE_FILL`. Mutation-checked: reverting the guard reproduces `07 C1` against golden `22 19` at pixel (0,0), and *only* that vector regresses. A second vector `fill_rect_2cycle_16` pins the **2-cycle** branch, which the 1-cycle vector leaves untested because `combine()` takes a distinct path there (cycle 0 runs first and feeds cycle 1 as the `COMBINED` input): its combine makes cycle 0 emit the env colour and cycle 1 select `COMBINED`, so env `0x8D73` means the chain ran, black means cycle 0 was skipped, and green `0x07C1` means the cycle type was ignored. Angrylion renders **`0x8D73`** and we already match. **Fallout worth recording:** six existing tests — the five `fill_rectangle_*` unit tests and the `golden_frame` end-to-end test — were named for FILL-mode behaviour but **never selected FILL mode**, so they were passing on this bug. They now emit a `Set Other Modes` with `cycle_type = FILL` and test what their names claim. **Still open:** the same question for a *flat* `Fill Triangle` (0x08) with no shade/texture block, which likewise takes the fill register unconditionally (the presence of a shade/texture block selects the combiner there, not the cycle type); no vector exercises it yet. **n64-systemtest impact: not measured** — the suite has no RDP render-path coverage |
| R-17 | The AI DMA models the sample **rate** exactly (`MASTER_HZ / (video_clock / (DACRATE + 1))` per sample-pair) but charges **no DMA setup/arbitration latency and no RDRAM bank-state cost**: the transfer begins, and the start-interrupt fires, at the derived sample boundary rather than after the real DMA-engine delay. The underrun behaviour is a defined **hold-and-decay** (integer `× 63/64` per sample) rather than the analog decay curve | The AI DMA is "directly connected to the DAC" and "progresses as samples are physically put through the DAC" (wiki §DMA), so the per-sample rate is the dominant timing term and is exact; the fixed setup latency `M` and the RDRAM bank costs are the same unmeasured constants flagged for the CPU/PI (they belong in this ledger with provenance when measured, never tuned). The decay shape is deterministic and no-`std`-friendly; ares uses `exp(-1/(freq·0.003))` | absolute — an unmeasured latency, not a differential re-phasing | **Open.** The rate and the FIFO/interrupt sequencing are unit- and integration-tested; the setup latency stays unmeasured (measure, don't guess) and the decay is defined-but-unpinned. No AI-timing oracle exists in the committed suites, so nothing gates it yet — to be validated against the project64 `DoubleShot` PCM ROM (Sprint 2) and any AI-timing capture that surfaces. **n64-systemtest impact: not measured** — no AI test drives the DMA-timing path, so the oracle count is **unchanged at 93** *(as-at — see the note above this table)* |
| R-15 | The **scissor** lower-right bound in FILL mode is **asymmetric**: the **X** bound is **inclusive** of its boundary pixel while the **Y** bound is **exclusive**, and a rectangle lying entirely at or past the scissor's right edge draws nothing (`allover`). `fill_rectangle` previously clipped both bounds exclusively (`(coord + 3) >> 2`) | Isolated cleanly against the Angrylion oracle by a scissor-clip fuzz batch (rectangles extending past the scissor on each edge). The X clip keeps the pixel containing `scissor.xl` (a rect spanning past `xl = 8.0` fills column 8), but the Y clip drops row `scissor.yl >> 2` (a scissor `yl = 5.0` fills up to row 4). The asymmetry is `edgewalker_for_prims`: the rectangle's `yl` is `\| 3`'d (FILL/COPY) so its own last scanline fills, but the scissor's raw `clip.yl` makes `invaly = k >= yllimit` drop that boundary row; the horizontal clip (`curover = xlsc >= clip.xl << 1`, `allover` ⇒ `!validline`) keeps the boundary column unless the whole span is over it. Read from the oracle's output, not computed | absolute — a rasterisation geometry rule, oracle-confirmed | **Closed for the integer-coordinate FILL scissor (oracle-validated).** `fill_rectangle` now clips X inclusive with the `allover` guard (`rect_xh >= scissor.xl` ⇒ nothing) and Y exclusive, plus a hard width clamp. Pinned by a 48-vector scissor-clip fuzz family (`tests/vectors/fuzz/fz_scis_*`, all byte-exact) and the reconciled `fill_rectangle_is_clipped_to_the_scissor` unit test (which previously asserted an unverified exclusive X edge). Sub-pixel (fractional-coordinate) scissor edges remain unexercised. No n64-systemtest driver (count 93 *(as-at — see the note above this table)*) |
| R-18 | A **commercial ROM boots and executes real code but does not reach video** (Phase 5 capstone). Through the retail HLE boot (`rom::hle_boot`) the game's own IPL3 runs, the CPU fetches the cartridge's instruction stream, and the PC advances through hundreds of millions of retired instructions across varied routines — but no frame is scanned out: over ~10 s of emulated time `VI_CTRL` stays 0, `VI_ORIGIN` is never set, and **no interrupt of any kind fires** (SM64 witnessed at `retired ≈ 9.4×10⁸`, all MI interrupt lines clear) | The retail OS-boot runtime the game waits on is not yet modelled. A commercial title's boot is interrupt-driven: after its OS initialises, its main loop blocks on the **VI vblank interrupt**, which the emulator only raises once the game programs `VI_CTRL`/`VI_V_INTR` — and the game does not reach that programming, indicating an earlier dependency (the **RI/RDRAM interface** registers used for RDRAM sizing, and/or the OS thread/interrupt setup). This is a cross-subsystem gap spanning the VI vblank loop, the RI registers, and the F3DEX graphics microcode — all **outside the Phase 5 cart/boot/saves boundary** (ADR 0003; the cart phase delivers PI/SI/PIF/CIC + saves, not the OS runtime) | absolute — a coverage boundary across subsystems, not a fitted constant or a timing interval | **Open — characterised, not a regression.** The committable Phase 5 gate (n64-systemtest cart/PIF/SI, save round-trips, homebrew boot) is met; the commercial capstone is asserted at its honest achievable level — `a_commercial_rom_boots_and_executes` (local, `#[ignore]`d) proves the ROM boots and retires ≥ 10⁶ real instructions without panicking, and *reports* the lit-pixel count (0) rather than asserting it. Reaching a title frame is deferred to the VI/RI/F3DEX work of a later phase and validated then. This gap was surfaced by the capstone exactly as the plan's escalation gate intended: **ship v0.6.0 on the committable gates + an honest "boots and executes" capstone, not a faked pass or an unbounded chase.** n64-systemtest impact: none — the boot/video path has no systemtest driver; the suite-wide count is **90** (see C-32) **SUBSTANTIALLY RESOLVED 2026-07-29 — the root cause was NOT the theory above.** It was `hle_boot` never seeding **`sp`**. IPL1 sets it before handing off (N64brew *IPL2* §IPL1 listing, `0xBFC000D0`: `ORI sp, sp, 0x1FF0  # sp = 0xA4001FF0`), and `hle_boot` skips IPL1/IPL2 without standing in for it. With `sp = 0`, IPL3's opening `ADDIU sp, sp, -24` / `SW s3, 0(sp)` prologue stored to `0xFFFF_FFE8` — KSEG3, TLB-mapped, no entries — taking a TLB-refill exception to `0x8000_0000` in empty RDRAM and executing a **NOP sled to the end of memory**. That is why the game never programmed `VI_CTRL`: it never ran at all. The symptom hid perfectly behind the capstone's own metric, because a sledding machine still retires ~180 million instructions. Found by tracing the instruction stream rather than the state (`docs/engineering-lessons.md`), and by noticing `retired` was *identical across four different games* — the 'same value regardless of input' signature. With `sp` seeded (and the **RI register block** decoded so IPL3's `RI_SELECT` read is coherent), retail titles now boot into their own code: Super Mario 64 reaches `pc=0x80246ddc` with 928 KiB of RDRAM populated, Star Fox 64 submits **122** RDP commands, World Driver Championship **45**, and Super Mario 64, Star Fox 64 and World Driver Championship upload graphics microcode into IMEM. **"Lit pixels" has been RETIRED as evidence — it never meant what earlier revisions of this row implied.** It counts non-black scanned-out pixels, and *uninitialised RDRAM is non-black*. Rogue Squadron and Jet Force Gemini score 68 527 and 69 479 of 75 840 (90-92%) with 4 790 and 6 203 distinct colours, which sounds like a picture; **rendered to PNG and looked at, both are pure noise** — RDRAM garbage scanned out, no rendered content whatever, and neither title even runs microcode. This is the `retired > 1_000_000` failure again: a metric a broken machine satisfies exactly as easily as a working one, cited for weeks because nobody opened the image. Only two things are evidence of video now: **(a)** a byte-comparison against a committed golden frame, and **(b)** a human or oracle actually viewing the output. The capstone still *reports* the count, but as a diagnostic, never as a pass condition. **Where the video gap actually is — localised, and NOT where an earlier revision said.** That revision blamed the "RDP → VI presentation path"; **wrong**. Ocarina of Time's framebuffer at `VI_ORIGIN` is uniformly `0x0001` — RGBA5551 with R=G=B=0 and the coverage bit set — so **the VI is faithfully presenting a genuinely black buffer** and `scanout` is correct. Nor is submission at fault: a DPC-seam opcode census over 300 frames shows the real F3DEX stream arriving — **7,412 `TRIANGLE` (0x0F: shade + texture + Z)**, 1,630 `TEXTURE_RECTANGLE`, 4,374 `LOAD_BLOCK`, 7,705 `SET_TILE`, 1,589 `SET_COMBINE`, of 74,508 commands. So geometry, textures and tile state all reach the RDP, and the frame still ends as the clear colour: **the RDP rasterises real geometry to black**. That is the gap. **Narrowed again, by inspecting the RDP state Ocarina actually leaves.** The **Z path is refuted**: `z_compare_en = false` and `z_update_en = false`, so nothing is depth-rejected (`SET_Z_IMAGE` *is* issued, `z_image = 0x12c700`, but the compare is off). The **combiner is refuted as a *collapse***, and is in fact the key evidence: `cyc1` decodes to `rgb_a=15, rgb_b=15, rgb_c=31, rgb_d=1` = `(0 - 0) x 0 + TEXEL0`, a pure **texture pass-through** — so the pixel colour *is* the texel, and a black frame means **the texel fetch resolves to 0**. The tiles are `fmt=2, size=0` = **CI4**, 4-bit colour-indexed textures resolved through a **TLUT**. Both TMEM halves are populated after 4,374 `LOAD_BLOCK`s — 1,508/2,048 non-zero bytes in the texture half, 760/2,048 in the TLUT half — and TLUT entry 0 reads `0x0000` (black), with entries alternating `0000 ffff 0000 ffff` at the 8-byte stride `tlut_lookup` uses. So the remaining gap is the **CI4 + TLUT texel path**: real indices and a real palette are present, and the resolution yields black. **That probe has been run. Its result is PROVISIONAL and is recorded as such.** A CI4-with-TLUT vector was authored (eight indices 0..7 against eight distinct non-zero palette entries) and replayed: RustyN64 produced `f801 07c1 003f ffff ffc1 07ff` — red, green, blue, white, yellow, cyan, exactly the authored TLUT — while **Angrylion produced mostly `0x0001`** from the same command list. Angrylion is the oracle, so the disagreement means the authored **`Load Tlut` encoding is wrong**, not that our decoder is right: Angrylion loaded a near-empty palette and we were more permissive about where the entries came from. The vector was therefore **deliberately not committed** — committing that golden would pin an authoring error as the spec. **What this does and does not establish:** it shows the CI4 index→palette mapping resolves against whatever TMEM *we* loaded, which makes a totally-dead CI4 decoder unlikely; it does **not** establish that the load path is correct, because the probe never exercised a verified `Load Tlut`. So CI4 is *weakly* de-prioritised as R-18's cause, not eliminated. **A separate, independently real defect surfaced:** `Set Other Modes.tlut_en` — **bit 47**, N64brew *Reality Display Processor/Commands* §0x2F, *"tlut_en: Enables Texture Look-Up Table (TLUT) sampling"*, with `tlut_type` at bit 46 selecting RGBA16 vs IA16 — is **not decoded at all**. Our TLUT lookup was driven purely by the tile's *format* field, so a CI tile with `tlut_en` clear still got a palette lookup and a non-CI tile with it set did not: wrong in both directions. **FIXED 2026-07-29.** `tlut_en` (bit 47) and `tlut_type` (bit 46) are now decoded, and the colour-index lookup is gated on `tlut_en` rather than on the format. The oracle settled the `tlut_en = 0` behaviour rather than it being guessed: `ci4_tlut_disabled_16` is byte-identical to `tex_tri_ci4_tlut_16` apart from that single bit, and the two goldens are **the full palette versus all black** — so an un-TLUT'd CI tile renders black. That is reproduced as the observed result, *not* as a mechanism claim: §0x2F does not document what the hardware does with un-TLUT'd index data, so no reinterpretation of the index bits is invented. `tlut_type`'s **IA16** palettes remain deferred (the lookup assumes RGBA16) — decoded so the flag is no longer silently ignored, but unimplemented until a vector exists. Battery is 53 probes (40 RDP + 13 VI); mutation-checked by removing the gate. **Only one of the two directions is fixed:** a CI tile with `tlut_en` clear is no longer palette-mapped, but a **non-CI tile with `tlut_en` set is still not** palette-mapped though hardware would sample it through the TLUT. No vector covers that case and the RGBA/IA/I formats index the palette differently enough that implementing it from prose would be inventing behaviour, so it stays wrong-but-recorded until a vector defines it. **SETTLED 2026-07-29.** The `Load Tlut` encoding was verified against N64brew *…/Commands* §0x30 and the first attempt had **two** errors: `lower_right.s` is command bits **23:12** (`lo >> 12`, so an 8-entry palette is `(8-1) << 2 = 0x1C` shifted by 12 — I had shifted by 14, decoding as 29 entries), and the section's own *Hazards* require the TLUT tile to be **4-bit** and neither RGBA nor YUV (I had set it 16-bit; only the *texture image* is 16-bit). Re-authored correctly, Angrylion now renders the intended palette — `f801 07c1 003f ffff ffc1 07ff` — and **RustyN64 matches it byte-for-byte**. The vector `tex_tri_ci4_tlut_16` is committed (38 RDP vectors, 51 battery probes) and mutation-checked: changing `tlut_lookup`'s stride from the quadrupled 8 to 4 turns it red at pixel (3,2). So the **CI4 + TLUT path is now genuinely ELIMINATED** as R-18's cause — not weakly de-prioritised — and the colour-indexed path finally has oracle coverage it never had. **`tlut_en` remains undecoded** (above), still a real defect but demonstrably not this one: the vector passes because our lookup keys off the CI format, which happens to coincide with `tlut_en` being set here. **R-18's video cause is therefore still open**, and the remaining texture suspects are the ones this vector does *not* cover — `Load Block` vs `Load Tile` addressing at real texture sizes, the odd-line swap, and mip/LOD tile selection under a live command stream. **`Load Block` (0x33) has ZERO oracle coverage**, which is the sharpest of those: every committed texture vector loads through `Load Tile` (0x34), while Ocarina issues **4,374 `Load Block`s** of 74,508 commands — it is the dominant texture-load path in a live retail stream and is entirely unpinned. Reading the implementation, `load_block` *does* handle the two details §0x33 calls out — coordinates as **u12.0** (not `Load Tile`'s u10.2) and the **dxt-driven odd-line 32-bit word swap** (`line = (word * dxt) >> 11`, `swap = (line & 1) << 2`) — so a static read finds nothing wrong. **Two attempts to author a `Load Block` vector were made and both DISCARDED (not committed).** Recording the evidence so the next attempt starts from data rather than from my summary of it. Candidate `load_block_odd_line_16`: 16 distinct non-zero RGBA16 texels at `0x3000`; `Set Texture Image` `0x3D100007 0x00003000` (16-bit, width 8); load tile 7 `0x35100400 0x07000000` (16-bit, line 2 words, tmem 0); `Load Block` `0x33000000 0x0700F800` (uls=0, ult=0, lrs=15, dxt=0x800); render tile 0 `0x35100402 0x00000030` (16-bit, line 2 words, tmem word 2, mask_s=3). **Observed:** Angrylion's row 7 came back `003f 0001 0001 0001 f83f 8421 ffc1 07ff` — containing `0001` fill (unwritten TMEM) interleaved with line-0 texels — and at pixel (0,1) Angrylion gave `003f` where RustyN64 gave `0841` (line 1's first texel). **What that establishes:** the golden is unusable as an oracle, because part of it reflects TMEM the load never wrote. **What it does NOT establish:** *why*. The plausible **hypothesis** is that the authored layout is wrong — tile `line`, `tmem_addr`, or the texel count's inclusivity — but that is unverified, and the divergence is therefore **not attributable to RustyN64 either**. **That question is now SETTLED, and `Load Block` finally has coverage.** §0x33's prose says *"`lower_right.s - upper_left.s` determines the number of texels"* (no `+1`) while `Load Tile` is inclusive, so the readings disagreed and prose could not decide it. A **minimal** vector could: `load_block_count_16` uses `uls = 0, lrs = 1` — a load of either one texel or two — on a single line with `dxt = 0`, so neither the odd-line swap nor any multi-line layout can confound it (precisely what sank the two discarded attempts). **Angrylion loads two.** The count is therefore **inclusive** and `load_block`'s `shi - slo + 1` is correct; RustyN64 matches the golden byte-for-byte, and the vector is committed (39 RDP vectors, 52 battery probes) and mutation-checked — dropping the `+1` turns it red. So this hypothesis joins the others as **refuted**, and `Load Block` is no longer wholly unpinned, though the coverage it now has is minimal: multi-line loads, the odd-line swap under a real `dxt`, and non-16-bit texel sizes all remain untested. **CORRECTION (same day).** This row briefly claimed "the RSP is never started — no title unhalts it, so those RDP commands come from the CPU driving the DPC directly". **That was wrong**, and wrong for an instructive reason: it was measured off `Rsp::halted` / `Rsp::pc`, two `pub` struct fields that are **never written**. The authoritative state is `SP_STATUS` (`sp.halted()` / `sp.pc()`), which is what `su_step` itself gates on. Sampling the dead fields reports "halted forever at PC 0" for a *running* RSP — the inert-API hazard of `docs/engineering-lessons.md` §3.2, and it produced two confident wrong conclusions in one session. The fields are now private with `Rsp::halted()` / `Rsp::pc()` accessors delegating to `SP_STATUS`; they are kept in the struct only because removing them would change the save-state layout (ADR 0005). Measured correctly, **retail microcode executes**: Castlevania Legacy of Darkness visits 805 distinct RSP PCs, 007 TWINE 459, Beetle Adventure Racing 356, Star Fox 64 331, Super Mario 64 236, Mega Man 64 229, World Driver Championship 148 — see T-71-003's witness, `tests/game_microcode.rs`. **Provenance for `T-71-003`'s witness parameters** (recorded because a threshold without it is a fitted constant). `MIN_DISTINCT_PCS = 32` separates two **measured** populations more than an order of magnitude apart: titles whose RSP never runs measure **0** distinct PCs (Blast Corps, Bomberman 64, Donkey Kong 64, Jet Force Gemini, Rogue Squadron), while titles whose microcode runs measure **148-815** (World Driver Championship 148, Mega Man 64 229, Super Mario 64 236, Star Fox 64 331, Beetle Adventure Racing 356, 007 TWINE 459, Castlevania Legacy of Darkness 805). `SAMPLE_TICKS = 24` is a sampling cadence, not a hardware value: it is 8 RCP steps (the RCP advances every 3 master ticks, ADR 0006), sampling can only **under**-count so every figure is a lower bound, and re-running at the finest possible cadence of 3 (8× finer) leaves the verdict unchanged — 805→815, 459→463, 356→356, 229→258, same four witnesses. `FRAMES = 90` doubled to 180 yields an **identical** witness set with unchanged counts. **Corpus evidence for the capstone's `MIN_RDRAM_NONZERO` floor** (recorded here because a threshold with no provenance is a fitted constant): a booted title leaves **787 KiB - 1.23 MiB** of RDRAM non-zero (Mega Man 64 539 KiB at the low end, World Driver Championship 1.23 MiB at the high), while a machine that faults out of IPL3 leaves **exactly 0**. The capstone's floor is therefore derived as `IPL3_COPY_BYTES / 4` (256 KiB) from the documented 1 MiB IPL3 copy, comfortably between the two populations. **R-18 remainder — diagnosed to a point, and the limits of that diagnosis stated.** Five staged titles still do not run microcode, in three groups. (1) **CIC-6103/6106** — Banjo-Kazooie, 1080 Snowboarding, F-Zero X — *do* boot and execute game code (Banjo-Kazooie reaches `pc=0x80268fcc` with 1.2 MiB of RDRAM populated) and fill IMEM to 4094 bytes, but never leave `SP_STATUS.halt` even over **600 frames / 10 s emulated**, so this is a stall, not a slow init. (2) **Blast Corps, Bomberman 64** boot but never load microcode at all (IMEM stays 0). (3) **Rogue Squadron** loads no microcode; its high lit-pixel count is **noise**, not output (see the lit-pixel retirement above), so it renders nothing at all. **Three hypotheses have now been tested and REFUTED; record them so they are not re-chased.** (a) *KSEG0 under 64-bit addressing* — the segment map returns `Direct` for `0xFFFF_FFFF_8028_4C78` in wide kernel mode, pinned by `r18_kseg0_is_direct_in_wide_kernel_mode`. (b) *"These titles sit in the exception vector"* — a **correlated** capture (armed on first entry to `0x8000_0000..0x8000_0200`, per `docs/engineering-lessons.md`) showed **no exception at all**: `ExcCode=0`, `EPC=0`, `Cause=0`. The instruction stream shows IPL3 executing `LUI t4,0x8000` / `ADDIU t4,t4,0` / `JR t4` — for **CIC-6103/6106 the game's entry point simply IS `0x8000_0000`**, so PCs like `0x8000_018c` are game code, not a vector. An earlier revision of this row read those PCs as an exception loop; that was wrong, and it was wrong because the first reading sampled `Cause`/`EPC` uncorrelated, long after the fact. (c) *R-18's original "the VI vblank interrupt never fires" theory* — F-Zero X programs the VI (`VI_CTRL=0x3102`, `VI_V_INTR=2`), and the interrupt **does** fire and reach the CPU: 1228 `MI_INTR.vi` and 3304 `Cause.IP2` samples over 120 frames with `MI_MASK=0x3f`, comparable to Super Mario 64's 1024/7219 — and SM64 runs microcode fine. The VI interrupt path is working. **What the evidence does still show:** Banjo-Kazooie alternates between game code (`0x80268fxx`) and `0x8000_0184` at *identical* sample counts, which is the signature of a tight fault-and-return loop; 1080 spins at one PC; F-Zero X spins while receiving interrupts. So the remaining cause is per-title and downstream of boot, VI and addressing — not one shared subsystem gap. **Genuinely still open:** several titles (Blast Corps, Bomberman 64, Donkey Kong 64, Jet Force Gemini) never load microcode into IMEM at all, and Rogue Squadron never starts the RSP, so their boots stall earlier than the RSP seam. n64-systemtest is unchanged at 90 (it uses the ELF load path, not IPL3). **FIRST RENDERED COMMERCIAL FRAME — 2026-07-29.** R-18's headline claim is now falsified by a picture: **Paper Mario renders real geometry through the full LLE path** (retail HLE boot -> the game's own code -> its graphics microcode on the LLE RSP -> DPC seam -> LLE RDP -> `Bus::scanout_scaled`), committed at `screenshots/paper-mario-first-commercial-frame.png`. Its colour image holds **87 distinct RGBA5551 values** (dominant `0xE739` = (28,28,28,1) at 72,156 of 76,800), and the frame is fully lit on **both** scan-out paths — **75,840 / 75,840** at 320x237 through the unscaled 1:1 `Bus::scanout` and **148,125 / 148,125** at 625x237 through the presented `Bus::scanout_scaled` (the two denominators are kept apart deliberately; quoting one beside the other path's dimensions is an error this row's first revision made) — held stably from frame 120 through 270 of a 300-frame run — flat-shaded quads with clean edge-walked slopes on a light-grey clear, **viewed, not inferred**. So the earlier conclusion that 'the RDP rasterises real geometry to black' was **title-specific, not a pipeline defect**: the pipeline produces a correct picture end-to-end. **The remaining gap is coverage, and it stratifies by title**, measured over 120 frames: Ocarina of Time 27,651 RDP commands but 98% of its colour image is `0x0001` (the black clear) and only 1,440 px lit; Majora's Mask 2,310 commands; World Driver Championship 45 commands and **exactly one distinct value** (`0x0001`) — it clears and draws nothing; Super Mario 64 and Banjo-Tooie submit **zero** commands. **Two claims in this row's opening column are now WRONG and are retained only as the historical observation** (per the immutable-reference rule, cf. R-5): `VI_CTRL` does **not** stay 0 and `VI_ORIGIN` is **not** never set — both are programmed by every title that boots (SM64 `VI_CTRL = 0x13016`, Mario Kart 64 `0x3116`, both TYPE=2), and interrupts do fire. **The noise trap was re-tested, not merely re-asserted:** on the same run Ocarina scored 62,963 lit pixels at frame 30 and, rendered and viewed, is pure uninitialised-RDRAM noise — indistinguishable from Paper Mario's real frame by pixel count alone, which is why the screenshot policy requires looking. **Measurement defect fixed in the same change:** `commercial_boot` measured through the superseded 1:1 `Bus::scanout` rather than `Bus::scanout_scaled`, the path the frontend actually presents, so every lit-pixel number this row ever quoted was from a buffer no user sees; and `Bus::scanout_scaled`'s own rustdoc still claimed it was 'not yet wired into the frontend', false since #158. **A suspicion raised and withdrawn by measurement:** the 625-wide scan-out looked like `PRESCALE_H` leaking into the width term; it is **correct** — `VI_X_SCALE = 0x200` is 0.5 in 2.10, so 320 upscales to 640, less the 8/7-px `minhpass`/`maxhpass` crop = 625. **THE NON-RENDERING TITLES ARE WAITING, NOT FAULTING — measured 2026-07-29, and it corrected two of my own claims in the same session.** With a picture now proven possible (Paper Mario), the question became why the others do not, and the `scanout_dims` field added alongside split them into groups that a lit-pixel count had merged: **VI never enabled** (Banjo-Kazooie, Banjo-Tooie, 1080 Snowboarding), **VI enabled but zero RDP commands** (Conker `VI_CTRL=0x3116` w=292, Super Mario 64 `0x13016`, Jet Force Gemini `0x1311E`), and **real frame scanned but black** (007 `625x237` at `0/148125`, World Driver Championship). **CORRECTION 1 — the 'hard fault loop' was a sticky-register artefact.** Sampling `Cause.ExcCode` showed AdES on 10,575 of 12,000 samples for Banjo-Kazooie, which reads as a permanent fault loop and was reported as one. `Cause` is **sticky** — it survives the handler returning — so counting *transitions* instead of samples gives the real number: **exactly 1** address-error transition over **1,332,906,106** retired instructions (first at 14,432,774), and **exactly 1** for 1080 over **1,995,894,174** retired (first at 7,724,174). One fault, then over a billion instructions of spinning. The fault is **not** what stops these titles, and is very likely handled normally. This is the same failure mode as `retired > 1_000_000` and 'lit pixels': a number that a broken machine and a working one both produce. **CORRECTION 2 — our AdES is CORRECT in both cases; the emulator is not at fault where I first looked.** Banjo-Kazooie faults on `SD t0, 88(k0)` at `epc=0xFFFF_FFFF_8026_8FC0` (`k0`/`k1` = libultra's exception preamble saving thread context) to `0xFFFF_FFFF_8028_4C78` — 8-byte aligned and correctly sign-extended, so neither documented AdES cause applies. `Status = 0x6D016CAA` gives **`KSU = 1` (Supervisor), `SX = 0`**, and **KSEG0 does not exist in Supervisor mode**, so the address-space check *must* reject it. 1080 faults at `epc=0xFFFF_FFFF_A400_02F4` (IPL3 executing from **DMEM**) on `0xFFFF_FFFF_A400_02F3` — `Status = 0x34000002`, `KSU = 0` (Kernel), and the address is simply **odd**, so AdES is again correct. **Three emulator-side hypotheses were refuted by reading the code rather than patching it:** `EXL`/`ERL` forcing Kernel regardless of `KSU` is implemented correctly (`pipeline.rs` `access_mode`); the 64-bit-operation reservation is correct (`sixty_four_bit_is_reserved` — never reserved in Kernel, whatever `KX` says); and `kernel_segment` classifies `0xFFFF_FFFF_8xxx_xxxx` as CKSEG0 through its `0xFFFF_FFFF_8000_0000..=` arm. **`CpU` exceptions are benign — established by a CONTROL, not by argument:** Jet Force Gemini raises 174 and Paper Mario, which renders correctly in the same run, raises 18. It is libultra enabling the FPU per thread on demand. **What is genuinely open:** all four non-rendering titles **spin**, and two of them (Banjo-Tooie, Jet Force Gemini) never raise an address error at all — so the shared cause is something they are *waiting* for that never arrives, not a fault. The open questions are therefore (a) why Banjo-Kazooie's `Status.KSU` is 1 when libultra runs in Kernel mode, (b) what 1080's IPL3 is storing to an odd DMEM address, and (c) **what all four are blocked on** — the interrupt/DMA-completion path is the first place to look, since a title that never gets its completion signal spins exactly like this. Note the grouping above is provisional: Jet Force Gemini *does* enable the VI, so 'blanked VI' was too coarse a bucket for it. |
| R-22 | The **RI register block** (`0x0470_0000..0x0470_0020`) is modelled as **plain storage**: writes stick, reads return them. Hardware read behaviour differs for at least three of the eight — N64brew *RDRAM Interface* documents `RI_CURRENT_LOAD` as intended write-only, its read returning "a collection of bits from other registers" (`RI_ERROR` Ack, `RI_MODE` STOP_R, `RI_SELECT` TSEL[0], and two bits marked TOVERIFY), while `RI_ERROR` and `RI_BANK_STATUS` reflect controller state rather than the last write | Storage is enough for the only consumer today: IPL3 reads `RI_SELECT` and branches on it. Modelling the readback quirks would mean inventing the parts the wiki itself marks TOVERIFY | absolute — a coverage boundary | **Open, deliberately.** n64-systemtest has **no RI group**, so there is no oracle in the vendored set; per measure-don't-tune these stay honest storage rather than fabricated behaviour. Decoding the block at all is what R-18 needed. The separate **RDRAM device registers** (`0x03F0_0000`) remain undecoded |
| R-23 | **CIC-6105 titles do not boot through `hle_boot`** (Banjo-Tooie, Ocarina of Time, Majora's Mask, and the rest of the 4.5% 6105 share). They boot correctly through `real_pif_boot` | The 6105 IPL3 is a *different program*: it opens with a self-descrambling XOR loop — `LW t0, -0xFF0(t1)` / `LW t2, 0x44(t3)` / `XOR t2, t2, t0` / `SW t2, -0xFF0(t1)` — over `t1`/`t3` that only the **real IPL2** leaves set. `hle_boot` seeds `sp` and `s3`-`s7` but not those, so the first load faults to KSEG3 and the machine sleds exactly as R-18 did. The values are not in the wiki's IPL1/IPL2 listing, so seeding them would be inventing a constant | absolute — a coverage boundary | **RESOLVED 2026-07-29.** Closed without inventing anything: the missing registers were **measured** by running the console's real IPL1/IPL2 out of a PIF ROM dump via `real_pif_boot`, capturing the register file at IPL3's entry (`0xA400_0040`), and keeping only the values **identical across ROMs of different CIC variants** (Banjo-Tooie/6105 vs Super Mario 64/6102): `at=1`, `a2=0xA400_1F0C`, `a3=0xA400_1F08`, `t0=0xC0`, `t2=0x40`, `t3=0xA400_0000`, `s4=1`, `ra=0xA400_1550`. `v0`/`v1`/`a0`/`a1`/`t4`-`t9` are deliberately **excluded** — they carry IPL2's running checksum of that cartridge's IPL3 and differ per ROM, so freezing them would fabricate a value the boot computes. `t3` is the decisive one: 6105's IPL3 descrambles itself by reading `0x44(t3)` = DMEM + 0x40, its own image. Corroborated independently — Banjo-Tooie under HLE now halts at **`pc=0x800329a8`, the exact PC `real_pif_boot` reaches**, with retired counts within 0.3%. All four staged 6105 titles boot (Banjo-Tooie, Donkey Kong 64, Ocarina of Time, Majora's Mask), the capstone's 6105 skip is deleted, and the T-71-003 witness set **doubled from 4 titles to 8** — Ocarina of Time alone now executes 733 distinct RSP instructions and submits **17 900 RDP commands**. n64-systemtest unchanged at 90. **Superseded scope note:** detects 6105 from the cartridge header and skips those titles with a message naming this row, rather than passing quietly; the real-PIF capstone boots them and asserts on them. Closing this means either deriving the IPL2 exit state or preferring `real_pif_boot` when a PIF ROM is available |
| R-19 | **The emulator hangs on the n64-systemtest test `TLB: Execute mapped branch with a non-mapped delay slot`** — a mapped branch whose delay slot lies in a page not currently in the TLB. Both the committed **base** ROM and the `--features timing` ROM stop dead there: `started = 917`, `emux_exited = false`, no test after it ever starts, at an 8×10⁹-tick budget (~2× a normal base completion). It is a genuine loop, not slowness. | **The committed `systemtest` gate masks it**: `tests/systemtest.rs` asserts Phase-1 *category* `Failed: 0` (those results are captured before test 917) and witnesses `started > 0`, but never requires the ROM to run to `xioctl(EXIT)` — so a mid-suite hang is invisible (the failure mode engineering-lessons §2.2 warns about, one level up). **Fully traced 2026-07-24 — every architectural field is CORRECT, so the defect is NOT in the delivered exception state.** The loop oscillates between `pc = 0x1234_5000` (the non-mapped delay-slot fetch) and `pc = 0x8000_0180` (the general vector), sustaining `EXL = 1`, with: `BadVAddr = 0x1234_5000` (✓ the delay slot), `EPC = 0x1234_4FFC` (✓ the branch, `pc − 4`), `Cause.BD = 1` (✓), `Cause.ExcCode = 2` = `TLBL` (✓), `Context`/`XContext` `BadVPN2 = 0x0_91A2` (✓ `= BadVAddr >> 13`), `EntryHi` VPN2 `= 0x1234_4000` + ASID (✓), 32-bit mode (`Status.KX/SX/UX = 0`). The general vector is correct *given* `EXL = 1` (a refill with `EXL` set uses `0x180`, S-3). `ERET` clears `EXL` correctly (tested). **So `EPC`, `BD`, `Cause`, `BadVAddr`, `Context`, `XContext`, `EntryHi`, the vector, and `ERET` are all right** — the earlier "vector/EPC/EXL is off" guess is disproven. The remaining suspects are in the finer *sequencing* the trace hasn't yet caught: (a) the **`EXL = 0` first fault** (does it reach the refill vector `0x8000_0000` and n64-systemtest's *test* handler, or does the refill handler's own page-table load fault nested straight to the general/"unexpected-exception spin" handler?); and (b) whether n64-systemtest's handler **maps the page and ERETs** expecting the fetch to now hit — in which case a stale **micro-ITLB** (not refilled from the JTLB after the map) would keep the fetch missing. | absolute — a hang is a coverage boundary, not a fitted constant | **Open — fully characterised, not yet root-caused. Blocks the `timing` suite from completing (so it blocks the clean `M` measurement, C-1).** **The test + handler are now understood** (`tlb/exceptions.rs:388` + `exception_handler.rs:247`): the JALR is the last instruction of the mapped page, its delay slot is the first of the next (unmapped) page; the test runs it under `expect_exception(TLBL, -4, …)`, which sets `EXCEPTION_SKIP = -4`, so the handler resumes at `return_to = exceptpc + skip*4 = EPC − 16` — back inside the mapped block, expecting the block's own code there to escape back to the `0x80…` test. n64-systemtest asserts `exceptpc == fault_address − 4` (line 436), and **our `EPC = 0x1234_4FFC` matches that exactly** — a third confirmation the exception state is right. So the loop is not a wrong `EPC`/vector; it is that after the skip-return our CPU re-reaches the JALR and re-faults instead of escaping. **RESOLVED 2026-07-24 (root-caused by a full pipeline-latch trace, not by reasoning).** The defect was NOT in the exception state (all correct, as characterised) but in the **branch-redirect vs. exception-vector race** in `ex_stage`. Sequence: the delay-slot fetch (`0x1234_5000`) faults and the exception is dispatched at the end of the cycle, setting `next_pc = 0x8000_0180` — but the JALR is still sitting **unexecuted** in `rf_ex`. On the next active cycle the JALR reaches EX and unconditionally applied its redirect (`*next_pc = r.target`), and this JALR's target is its **own address** (`v1 = 0x1234_4FFC`), so it clobbered the vector, re-fetched itself, re-faulted its delay slot, and looped forever — exactly the two-state oscillation the latch trace showed (JALR + aborted delay slot circulating, never retiring). Fix (`pipeline.rs::resolve_branch_control`): a branch whose delay slot has aborted (its `ic_rf` latch carries `in_delay_slot` + an abort) **still writes its link** — from the architectural `pc + 8`, since `next_pc` now holds the vector — but its **redirect is suppressed**, so the exception PC wins. This is hardware-accurate: the older branch retires and links (n64-systemtest asserts `RA == fault_address + 4`) while the precise exception on the younger delay slot takes over control flow. With the fix the delay-slot test passes and **the full suite runs to `xioctl(EXIT)` for the first time (950 tests, ~30 s), so `emux_exited` is now `true`.** No regression: golden-log 0-diff, determinism, residue-invariant, and all workspace tests stay green. Completing the run unmasked a distinct pre-existing cluster the hang had hidden — see **R-20**. Discovered + traced + fixed 2026-07-24 during the Stage-C/D timing work |
| R-20 | **64-bit addressing mode is not implemented** — the n64-systemtest `tlb64` group reports **18 failures** (14 `LW TLB Miss or Address Exception (64 bit addressing mode)` cases where `EntryHi`/`Context`/`XContext` read back `0` instead of the 64-bit VPN2, plus 4 `Loads from 32/64 bit address while using 64 bit addressing mode` returning wrong data). These tests run only in 64-bit addressing mode (`Status.KX/SX/UX = 1`) and exercise the `XKPHYS`/`XKSEG` segments and the `R` (region) field of the 64-bit `EntryHi`/`Context`/`XContext` decomposition | The emulator's segment map and TLB-miss register write-back model the **32-bit** address decomposition; the 64-bit `R:VPN2` layout (bits 63:62 region + the wider VPN2) and the wide-address segment ranges are not decoded, so a 64-bit TLB miss leaves `EntryHi`/`Context` at their reset `0`. **This cluster was masked by R-19**: the `tlb64` tests run *after* the delay-slot test that hung, so the suite never reached them — Phase 1's `Failed: 0` was only ever true *up to the hang point*, which is precisely the vacuous-pass failure mode the R-19 gate now witnesses against (`emux_exited`) | absolute — an address-decode / register-decode fact, not a timing interval | **Open — newly exposed, Stage D (CPU accuracy).** A genuine 64-bit-addressing feature gap (region-field decode + wide segment map + 64-bit miss write-back), not a regression from the R-19 fix (the fix touches only branch-delay-slot control flow). Pin against the `tlb64` group and implement the `R:VPN2` decomposition + `XKPHYS`/`XKSEG` ranges; read the expected `EntryHi`/`Context` values as a table from the suite's own assertions (do not compute against them — engineering-lessons §3.x). Surfaced 2026-07-24 the moment the suite could complete. **Progress 2026-07-24: 14 of 18 closed.** Root cause of the 14 `LW TLB Miss…(false, …)` cases was that **`EntryHi`'s VPN2/R was not written on a data address error** — the UM (§6.4.7) calls it "undefined", but the oracle pins `(VPN2 << 13) \| (R << 62)` from the faulting address, exactly as `Context`/`XContext` are already filled (which is why only `EntryHi` mismatched). Fixed by gating the `EntryHi` write on `writes_bad_vaddr` (address errors included), deleting the superseded `writes_tlb_context`, and replacing the wrong `an_address_error_leaves_entry_hi_alone` unit test with `an_address_error_writes_entry_hi_vpn2_and_region` (mutation-checked). Suite-wide 108→94. **Closed 2026-07-24 (18/18).** The last 4 were the `do_all_loads` battery (`Loads from 0x80/0xA0/0x90/0x98 … in 64-bit mode`). A focused reproduction harness (call `Pipeline::access_unaligned` directly with the four base addresses in 64-bit kernel mode, compare per-load against the ROM's `EXPECTED`) pinned the bug precisely and proved it **mode-independent**: **`mem::lwr` was unconditionally sign-extending**, but the VR4300 sign-extends `LWR` only for the **full-word** case (`byte == 3`, which writes bit 31); a **partial** `LWR` (bytes 0–2) leaves bits 63:32 of `rt` UNCHANGED. The `tlb64` battery exposes it because its sentinel's upper half (`0xBEEF_0000`) is non-zero — `LWL`, `LDL`, `LDR` all passed (they always write bit 31 or the whole register). Fixed in `mem::lwr` (sign-extend iff `byte == 3`, else preserve `rt & 0xFFFF_FFFF_0000_0000`), pinned by the mutation-checked `a_partial_lwr_preserves_rt_upper_half_and_only_the_full_word_sign_extends`. Result: **Phase 1 categories `Failed: 0` with the suite running to `xioctl(EXIT)`; suite-wide 94 → 90** (the rest are RSP/RCP/RDP, later phases). With R-20 closed, `tests/systemtest.rs` gained the `emux_exited` **completion witness** promised in R-19, so this class of mid-suite hang can never hide behind a partial Phase-1 zero again |

Every entry must carry a **classification** of the failing measurement as **absolute** or
**differential** before any mechanism is proposed (ADR 0005, `engineering-lessons.md` §1.3). A
differential measurement — the interval between two events on the same clock — is invariant
under uniform re-phasing, so an entire family of plausible fixes is ruled out for free. A sibling
project implemented and rolled back five successive re-phasings before recognising this. One line
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

- **Tables 6-3/6-4 (p. 181)** define the refill offsets *only* for `EXL=0`: the rows are labelled
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

**Documented part.** The *behaviour* is from N64brew *Memory map* (PI external
bus): writes are asynchronous, the PI latches the value and releases the CPU
immediately, `PI_STATUS.IOBUSY` reports the in-flight write, further writes are
ignored, and reads from **any** address return the value being written. The PI
does not know a device is read-only, so ROM writes follow the same path and are
dropped by the ROM.

**Undocumented part — the duration.** Hardware finalisation depends on the PI
domain timing registers (`LAT`/`PWD`/`PGS`/`RLS`), which we do not model.
n64-systemtest bounds the latch only relatively: visible after 0 decay-loop
iterations, gone after 110. The constant was chosen by trying values against the
suite and keeping the best.

**Known-wrong, deliberately.** `cart-writing: Write32, Read32 (same location)`
still fails on its **second** read, where hardware has finalised and we have not.
No single constant closes that, because the real duration is not constant. This
is recorded as a fitted approximation rather than presented as accurate.

**Confirm with:** modelling the PI domain timing registers and deriving the
finalisation time, then deleting the constant.

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
hypotheses (unwired operations, exception behaviour) and the rounding hypothesis
are now all excluded by measurement.

A neighbouring case is more informative still: `Upper bits of 32 bit operation
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
  neighbouring failure is explicitly labelled **"half mode"**, which is what
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
freely. The zero correlated hits was precisely the pipeline artefact flagged
above — operands sampled at fetch have not been loaded yet by instructions still
in flight.

The caveat did its job: the hypothesis died on its own test instead of becoming a
third retraction. That is the only method in this entry that has worked.

**Where COP1 actually stands.** Excluded by measurement, not argument:
unwired operations; exception behaviour; rounding mode; a write-back width fix;
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
during the COP1 phase; pairs appearing to change together are an artefact of
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
stream that was supposed to write it before theorising about the writer.

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

**Claim.** A NaN is **signalling** when its significand's most significant bit
is **set**, and quiet when clear — the *legacy MIPS* convention, the opposite of
IEEE-754:2008 and of every modern language. `0x7FC0_0000`, which Rust produces
as `f32::NAN` and which everything else calls quiet, raises Invalid on this
processor.

**How it was established.** From n64-systemtest's own expectations, which name
their constants by the IEEE convention and then assert the opposite behaviour.
For a *non-signalling* compare (`C.EQ`, `C.F`, …) it expects:

| Operand | IEEE name | Expected | Implies |
| --- | --- | --- | --- |
| `0x7FC0_0000` (MSB set) | "quiet" | **Invalid raised** | signalling here |
| `0x7FBF_FFFF` (MSB clear) | "signalling" | no flags | quiet here |

The *signalling* compare forms (`C.SF`, `C.SEQ`, …) raise Invalid for both,
which is the ordinary IEEE rule for those forms and therefore does **not**
distinguish the conventions — checking only those would have left the question
open. It is the non-signalling forms that settle it.

**The corroboration that makes it more than a curve fit.** The processor's own
default NaN result is `0x7FBF_FFFF` / `0x7FF7_FFFF_FFFF_FFFF`, MSB **clear**.
Read as IEEE, that is a processor whose invalid-operation result is a
*signalling* NaN — which would re-trap the instant anything touched it. Read
under this convention it is exactly what it must be: quiet. Two independent
facts, from different tests, agreeing on the same inversion.

**Effect:** n64-systemtest 1,468 → **1,098**, and it took the compare block from
42 failures apiece to **zero across all sixteen**.

**Where it bites.** `fpu::is_snan_{f32,f64}` and `softfloat::unpack`. Both now
name the bit for its *position* rather than calling it a "quiet bit", because a
constant named `quiet_bit` that is tested for signalling is a trap for the next
reader. The tests name their patterns `vr_snan`/`vr_qnan` for the same reason,
and one asserts `is_snan_f32(f32::NAN)` explicitly — that is the case most
likely to be "fixed" back to IEEE by someone who has not read this entry.

**Adjacent, and since RESOLVED in C-13:** an **IEEE-signalling / VR4300-quiet**
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
   signalling NaN, and **replace** `FCSR.Cause`, while `MOV` transports the
   bits and leaves `FCSR` completely alone. The oracle settles it by
   *construction* rather than by description — `MOV.S` is driven through
   `test_floating_point_f32_which_preserves_cause_bits` and `ABS.S`/`NEG.S`
   through the ordinary harness that asserts `Cause` was cleared. Treating all
   three alike was worth 52 assertions. Note the earlier finding that `MOV`
   must *not* touch `Cause` (C-10) remains correct; it simply does not
   generalise to its neighbours.
2. **Compares are exempt.** "This FPU cannot do subnormals" sounds like it
   should be universal and is not: `C.cond.fmt` compares a subnormal as an
   ordinary number and raises nothing. Applying the rule there would have
   regressed all sixteen compare tests, which had just reached zero.
3. **An out-of-range float-to-integer conversion is unimplemented, not
   Invalid.** IEEE says Invalid and `fpu::to_i32` reports that; the VR4300
   declines instead. The translation happens at the call site, so the IEEE
   answer stays available to anything that wants it.

**Both follow-ups from this entry are now CLOSED.** `CVT.S.D` routes through a
narrowing `softfloat::convert` that rounds once and honours `FCSR.RM`, and
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

**A second behaviour fell out of the same tests.** A single-precision
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
behaviour in its own test comments and exercises it directly.

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
comment on a neighbouring test says as much: *"it's unlikely that there are
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

Giving all four one behaviour is the natural mistake, which is why the test
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

### C-24 — integer-to-float conversion honours `FCSR.RM`, and a Rust `as` cast does not

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
with `Status = 0x3400_0000` — exactly what `seed_ipl3_handoff` synthesises. The handoff model was
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
eagerly cannot express this, and the bug is invisible until something reads back a neighbouring
byte it never wrote.

**Scope, stated rather than assumed.** The PI and SI external-bus windows share the size-blindness
on hardware (same wiki section), and are **not** covered here: the PI already models its own bus
quirks separately, and merging them without the cart tests to check against would be a change made
blind. Phase 5 owns that. The 64-bit *read* case -- which hangs the VR4300 outright, because the
RCP never puts a second word on the bus -- is not modelled either; nothing tests it, since a test
for it would hang the console.

---

### C-29 — the FPU rates are charged; the early exit on trivial operands is not

**Claim.** COP1 arithmetic stalls the pipeline for its **UM Table 7-14** rate — `ADD`/`SUB` 3,
`MUL` 5/8, `DIV`/`SQRT` 29/58, the `ROUND`/`TRUNC`/`CEIL`/`FLOOR` family 5, the `CVT` forms 1/2/5
depending on the *source* format, and 1 (no stall) for `ABS`/`MOV`/`NEG`/`C.cond`. What is **not**
modelled is the documented early exit.

**Basis: documented, transcribed from the table itself.** Extracted from the manual with
`mutool draw -F txt` and asserted row by row in `fpu::tests::the_fpu_delay_table_matches_the_manual`.
The manual's *"latency is the execution rate plus one … an EX-to-RF bypass is not performed"* is not
added anywhere: the stall holds every stage, so a dependent consumer spends its own cycle after the
stall drains and reaches rate + 1 without a second rule.

**The deviation.** UM §7.5.6, and Table 7-14's own note 2, say a multicycle operation whose result
is *obvious* completes in **two** cycles instead of its full rate: add/sub on a zero or infinity
operand or a source exception, multiply when either operand is a power of two, divide and sqrt when
the result is zero or infinity, and the convert instructions for trivial cases. None of that is
modelled, so those operands are charged the full rate and the model runs **slower than hardware**
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
about hardware. This entry exists so the folding in `Rsp::mem_read` is understood as modelled
behaviour with a source, not as a missing guard — and so that if the range is ever found to fault
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
remaining 90 are the RDP rasteriser (Phase 3 render path, no systemtest driver) and the retail
OS-boot runtime (R-18), neither in Phase 5's scope.

**Falsifiability.** The boot state is pinned by `hle_boot_seeds_retail_state`, which asserts the
IPL3 copy, the CIC seed word in PIF RAM, the `s4`/`s6` argument bytes, the full PI DOM1 tuple
(incl. RLS), and the DMEM entry PC — so changing any seeded constant fails a test. What it does
**not** prove is that these are the *only* values a real IPL3 leaves; that is bounded by the
real-PIF path (off by default) and by whatever n64-systemtest boot-state coverage is run.

---

### C-33 — the real-PIF boot executes IPL1/IPL2/IPL3, with the PIF-SM5 behaviourally modelled

**Claim.** `rom::real_pif_boot` boots a retail cartridge the way hardware does: the CPU runs the
**real IPL1 and IPL2** from the console's PIF boot ROM (mapped at `0x1FC0_0000`) starting at the
reset vector `0xBFC0_0000`, then jumps into the cartridge's own IPL3 — no state seeding, no HLE
jump. This is the faithful counterpart to the HLE path (C-32); it is off by default, local-only,
and never CI-gated (it needs the copyrighted PIF ROM, which is never committed).

**What is executed vs. modelled.**

- **Executed for real (guest code):** IPL1 (52 instructions from the PIF ROM: RSP/PI/AI reset,
  copy IPL2 to IMEM, jump), IPL2 (from IMEM: read the seed hand-off, lock the PIF ROM, compute
  the 6-byte IPL2 checksum over IPL3, have the PIF verify it, jump to IPL3), and the cart's IPL3.
  The disassembly of the local dump (`bfc00000`+ / `bfc006xx`) was traced to confirm the exact
  handshake.
- **Modelled behaviourally (the PIF-SM5, from `PIF-NUS.md`, not by running its 4-bit firmware):**
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

**Not modelled (bounded, and not needed to boot).** The SM5 firmware itself is not emulated
(there is no decapped-ROM dependency); the post-boot **running challenge** — the 6105/7105 "X105"
protocol behind command `0x08` — is not modelled. Per `CIC-NUS.md` the running challenge is a
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

Behaviour we model differently *on purpose*, so it is never mistaken for a bug.

| # | Deviation | Why | Bounded by |
| --- | --- | --- | --- |
| D-1 | Power-on CPU/RCP phase comes from a seeded PRNG, not from real indeterminacy | The determinism contract requires reproducibility; the hardware's own indeterminacy is documented (UM Table 11-1's "1 to 2 PCycles: synchronize with SClock") and is modelled as a *parameter* rather than eliminated | ADR 0004, ADR 0006 |
| D-5 | **SUPERSEDED by D-6** (the caches are modelled as of T-11-003). Recorded verbatim because the reasoning was sound while it held, and because the boundary it named — "stops being sound the moment something can observe staleness" — is exactly what came due. `CACHE` was an **address-translating no-op**: it decodes, translates (so it can raise a TLB fault) and does nothing else | The cache *contents* are not modelled, so invalidate and write-back have nothing to act on. This is observationally sound **only** because no cache state exists to become stale — the depth decision the Phase 1 open question asked for. What matters now is that `CACHE` does not *raise*: IPL3 and libdragon both issue it, so a `Reserved` decode blocks every real ROM | Was bounded by Phase 5 DMA coherency; that is **not** what retired it. It came due earlier, at n64-systemtest's `DCACHE:`/`ICACHE:` groups, which observe staleness without any DMA — a reminder that a bound named in a ledger entry is the *earliest case thought of*, not the earliest that exists. DMA coherency remains open under D-6/T-11-003, as does `M` (C-1) |
| D-4 | TLB entries reset to **distinct** `VPN2` tags, not to zero | All-zero is not a usable state: with 32 entries at `VPN2 = 0` and `V` not participating in matching, the first access to virtual page-pair 0 matches all 32 and triggers **TLB shutdown**. Reset contents are undefined (UM §6.4.4) and ADR 0004 forbids entropy, so a fixed non-coinciding set is chosen — which is what real hardware's arbitrary power-on contents almost always are | Pinned by `a_fresh_tlb_does_not_shut_down_on_the_first_low_access`; revisit if n64-systemtest probes uninitialised entries |
| D-3 | `Count` and `Compare` both reset to a deterministic **0**, so the timer matches at power-on and latches `IP7` | Both reset values are **undefined** (UM §6.4.4, p. 183) and ADR 0004 forbids entropy, so *some* fixed pair must be chosen; 0/0 is the least surprising. The consequence is a timer interrupt pending before software writes `Compare` — masked in practice, since cold reset also leaves `IE` clear and `ERL` set | ADR 0004; IPL3 writes `Compare` during boot, so no real ROM observes it. Revisit if n64-systemtest's startup set disagrees |
| D-6 | The primary caches are indexed by **physical** address; the hardware indexes them virtually | A virtually-indexed, physically-tagged cache lets two virtual addresses for one physical address occupy two lines — a cache alias, which software must then flush around. Physical indexing removes aliases, and keeps a virtual address out of a structure that otherwise needs only a physical one. It is **not** strictly safer: it is a behavioural divergence in both directions | Two observable differences, both untested here: a program that deliberately constructs an alias, and an `Index_*` operation on a **TLB-mapped** page, where translation preserves only the low 12 bits while the D-cache index reaches bit 12 and the I-cache bit 13. The tested scope is KSEG0, where the two indexings coincide — every test that motivated the cache model works there. Revisit if a ROM observes either case |
| D-2 | The VR4300 errata are **reproduced**, not fixed | They are observable behaviour that software depends on; `sra`/`srav` in particular affects every console | ADR 0007; pinned by named tests that fail if "corrected" |
