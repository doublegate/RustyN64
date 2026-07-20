# ADR 0007 — Cycle-accurate VR4300: a five-stage pipeline of inter-stage latches

## Status

Accepted (2026-07-20). Depends on [ADR 0006](0006-one-canonical-master-clock.md).

## Context

The project targets **sustained, fully cycle-accurate emulation at full speed**. That rules out
the two shapes the CPU skeleton could otherwise have taken, and it has to be settled before the
interpreter is written: a CPU written as one indivisible `tick` per instruction cannot later
express "a device observed the bus partway through this instruction", and retrofitting that
means rewriting the scheduler and every chip's step contract at once, after accuracy work is
already standing on the old shape (`docs/engineering-lessons.md` §1.2).

Three things the hardware documentation settles, which the current docs get wrong:

**The stages are IC / RF / EX / DC / WB.** `docs/cpu.md` said "IF, RF, EX, DF, WB" when this ADR
was drafted and has since been corrected; `ref-docs/research-report.md` §1 still says it and is
immutable, so the correction is recorded here and in `docs/cpu.md` rather than edited in place. The VR4300 User's Manual §4.1 Figure 4-1 names them
**IC** (Instruction Cache Fetch), **RF** (Register Fetch), **EX** (Execution), **DC** (Data
Cache Fetch), **WB** (Write Back). This is not cosmetic: every interlock and exception in the
manual's taxonomy is named against these stages (DCM = Data Cache Miss, DCB = Data Cache Busy,
ICB = Instruction Cache Busy, LDI = Load Interlock, CP0I = CP0 Bypass Interlock), and the
priority rules are stated stage-relative.

**One instruction per tick is not a timing model.** `docs/cpu.md` said the CPU
"advances one issued instruction per master tick" when this ADR was drafted. The manual's Table 3-12 gives `DDIV` a
**69-PCycle** full-pipeline stall, and Tables 11-1/11-2 give cache misses as 8–9 + M and
14–15 + M PCycles. At minimum "at least 5 PCycles are required to execute an instruction"
(§4.1).

**Interrupt sampling is not tied to the SysAD command/data phase.** The current
`Bus::poll_irq_at_phase(BusPhase)` hook is shaped on the assumption that it is. No such
coupling is documented — not in the manual, not on the wiki. The documented rule (§4.7.1) is
per-PCycle and gated on stall state: *"NMI and interrupt exception requests are accepted only
if the previous PCycle was a run cycle."* The hook is therefore plumbing on the wrong axis and
is replaced, not merely wired up.

## Decision

**Model the pipeline as four inter-stage latches advanced once per PClock in reverse stage
order.**

### Latches, not stages

Five stages have four boundaries. Carry the state on the boundaries:

```rust
struct Latch {
    pc: u64,
    abort: Option<Exception>,   // NOT named `fault` -- see below
    in_delay_slot: bool,
}
struct Pipeline { ic_rf: Latch, rf_ex: Latch, ex_dc: Latch, dc_wb: Latch, /* ... */ }
```

The field is `abort: Option<Exception>` rather than `fault`, deliberately. UM §4.5 defines
**fault** as the *union* of interlocks and exceptions (Figure 4-11: Faults = Interlocks ∪
Exceptions, split into Stalls vs Abort), and CEN64 follows that wider usage — its
`VR4300_FAULT_LDI`/`_DCB`/`_ICB` sit alongside `_INTR` and `_SYSC`. What travels in the latch
here is only the *aborting* subset. Using `fault` for the narrow meaning would contradict the
manual's taxonomy in the very ADR that corrects `IF`/`DF` to `IC`/`DC`.

`in_delay_slot` travels **with the instruction**, set when the branch is decoded and read at
exception time several cycles later. It is not global CPU state: a multi-cycle stall between a
branch and its delay slot desynchronises a global flag, and that is the classic bug in this
area. `Cause.BD` and `EPC` then fall out correctly for free.

### Reverse step order: WB → DC → EX → RF → IC

Each stage reads its input latch and writes its output latch. Running downstream-first means a
stage's input latch still holds the previous cycle's value when it is read, so no value can
propagate two stages in one cycle. **The reverse order is the latching** — it removes the need
for double-buffered state entirely. This is a load-bearing invariant, not a style choice, and
it is documented as such in `docs/cpu.md`.

### Stalls as `(cycles, resume_stage)`

An interlock is "stall N cycles, then restart the cascade from stage S". A stage that cannot
complete returns an interlock rather than advancing; because the order is reversed, that
naturally back-pressures every upstream stage in the same cycle. Aborting conditions take the
other path: an exception is stamped into its own latch **and every latch upstream of it**, which
is the kill-younger-instructions step. In the manual's vocabulary both are *faults* (§4.5); they
differ in whether the outcome is a stall or an abort.

### Cycle costs

All in PCycles. Most are transcribed directly from the User's Manual — but **not all of them
are documented**, and the table says which. A row marked undocumented is a value to be measured
and recorded in the accuracy ledger, never a number to quote as if the manual supplied it. See
Risks for the two that bite: `M`, and the exception epilogue.

| Class | Cost | Source |
|---|---|---|
| Integer ALU / most instructions | 1 | §7.5.6 (see note) |
| `MULT` / `MULTU` | 5 (full pipeline stall) | Table 3-12 |
| `DIV` / `DIVU` | 37 | Table 3-12 |
| `DMULT` / `DMULTU` | 8 | Table 3-12 |
| `DDIV` / `DDIVU` | 69 | Table 3-12 |
| FPU add/sub | 3 | Table 7-14 |
| FPU mul (S / D) | 5 / 8 | Table 7-14 |
| FPU div, sqrt (S / D) | 29 / 58 | Table 7-14 |
| Load-delay interlock (LDI) | 1 | §4.6.5; `VR4300.md` |
| Store data-cache-busy (DCB) | 1 | §4.6.7 |
| Instruction micro-TLB miss (ITM) | 3 | §4.6.2 |
| Instruction cache busy (ICB) | — | §4.6.3 |
| Multi-cycle interlock (MCI) | — | §4.6.4 |
| Data cache miss (DCM) | 1 + the fill below | §4.6.6 |
| CP0 bypass interlock (CP0I) | **undocumented** | §4.6.9 names it, gives no count |
| D-cache miss (fill) | 8–9 + M | Table 11-1 |
| I-cache miss (fill) | 14–15 + M | Table 11-2 |

The cache-miss figures are the sum of the table rows, and the 1-cycle spread is the
*"1 to 2 PCycles: synchronize with SClock"* row — the vendor-documented PClock:SClock phase beat
that ADR 0006's seeded phase models. CEN64 independently corroborates the D-cache figure: its
`DCACHE_ACCESS_DELAY` of 44 is exactly `8 + 38`, its `MEMORY_WORD_DELAY`.

**Note on the 1-cycle baseline:** UM §7.5.6's supporting sentence ("All CPU/FPU instruction delay
times that are not mentioned in these tables have a latency of one pipeline clock cycle") is a
*latency* statement inside the FPU chapter, used here as an *issue cost* for integer ALU ops.
Defensible, and consistent with §4.1's throughput model, but it is an inference rather than a
direct citation.

Two rules that are easy to miss and are part of this decision:

- **FPU latency = execution rate + 1** for a dependent consumer, because no EX-to-RF bypass is
  performed for those results (§7.5.6). The table above gives rates, not latencies.
- **The load-delay interlock is imprecise on real hardware and the imprecision is reproduced.**
  It stalls whenever the next instruction's `rs` *or* `rt` field equals the load's `rt`,
  whether or not that field is actually used as a source. So a load followed by `LUI` into the
  same register stalls, and two consecutive loads into the same register stall. GPR loads
  interlock only with non-float instructions and vice versa, and a load into `$zero` never
  interlocks (`n64brew_wiki/markdown/VR4300.md`, § Microarchitecture → Load Delay Interlock).

### Interrupts sampled per PCycle, gated on run-vs-stall

`Bus::poll_irq_at_phase(BusPhase)` is **removed**. Interrupts are sampled once per PClock in the
**DC** stage — which is documented, not merely CEN64's choice: UM Figure 4-12 places `INTR` in
the DC column and §4.7.6 "DC-Stage Interlock and Exception Priorities" lists the interrupt
exception among them. They are accepted only when the previous PCycle was a run cycle (§4.7.1),
against the documented predicate (an unmasked `Cause.IP` bit with `Status.IE` set and `EXL`/`ERL` clear). Exactly one
recognition predicate exists in the tree; CEN64 carries two subtly different ones and that is a
known source of one-cycle discrepancies.

`BusPhase` itself is retained but demoted to what it actually is: the SysAD **bus protocol's**
command-vs-data cycle, used by the bus model below, with no interrupt semantics attached.

### The SysAD split is modelled at SClock, and this is where we exceed the references

SClock = MClock = **62.5 MHz** = 2/3 of PClock, so one SysAD cycle is 1.5 PCycles — under
ADR 0006, 3 master ticks against the CPU's 2. A bus transaction is a command cycle followed by
a data cycle, handshaked by `EOK`/`Pvalid`/`Evalid`, with the wait between them unbounded and
that is where real RDRAM latency lives.

Neither reference implements this. CEN64 completes the entire access atomically in zero emulated
time and charges a flat constant (`// Currently using fixed values....` — 38 PClocks uncached,
44 D-cache fill, 48 I-cache fill); ares charges different constants (40 for a D-cache fill).
**The two most accurate N64 emulators disagree on that number and neither derived it from a
spec.** Modelling the transaction properly is the specific place this project can be better
rather than equal, and it is what makes the bus access an addressable point the scheduler
interleaves around — the Phase 1 exit criterion.

## Consequences

### Positive

- The Phase 1 exit criterion is satisfied structurally rather than by a bolt-on: the DC stage
  *is* the interleavable point.
- `Cause.BD` / `EPC` correctness in delay slots is a property of the latch design, not of
  careful bookkeeping at each exception site.
- The documented cost tables are encodable immediately; most of the timing model is transcription
  from a primary source rather than reverse engineering.
- Bus contention and back-to-back access behaviour become expressible, which is where both
  references currently lose accuracy.

### Negative / costs

- Substantially more work than an instruction-stepped core, and the pipeline must be right
  before the instruction count grows.
- ~5 stage advances per instruction against a budget of roughly **32 host cycles per emulated
  component step** at full speed. That figure is an estimate, not a measurement: 93.75M CPU +
  62.5M RSP = 156.25M component-steps/s against a ~5 GHz core, before the RDP. It needs a real
  benchmark in Sprint 1 to become a number worth defending. Hot-path discipline is a design input from the first line, not
  a later optimisation pass: latches cache-resident, no per-cycle branching on cold conditions,
  no allocation in `tick`.
- Two mechanisms for time (integer divisors, plus VI's accumulator) and two for bus cost (modelled
  SysAD cycles, plus `M` until it is measured).

### Risks

- **The exception epilogue cost is NOT documented, despite being widely used as if it were.**
  CEN64 charges 2 PCycles and its own source says it does not know whether that is right:
  *"TODO: Is the cycle count just the killing of IC/RF, or do we actually delay an additional two
  cycles?"* (`ref-proj/cen64/vr4300/fault.c`). No figure appears in UM §4.7 or Chapter 6. Treat it
  exactly like `M`: a fitted constant recorded in the accuracy ledger with its provenance, never a
  number quoted as though the manual supplied it.
- **`M` — memory access time in PCycles — is undocumented.** Both cache-miss formulas are
  parameterised on it and no source gives a value; the only hints are informal ("RDRAM has about
  10-20+ clock wait time"; RCP registers "5-6 PClock cycles"; MI registers "about 2 cycles";
  RSP DMEM/IMEM "4-5"). It must be fitted against test ROMs and recorded in the accuracy ledger
  as a measured constant, never quietly tuned to make a ROM pass.
- **n64-systemtest's `cycle` and `cop0hazard` sets are default-off upstream** because, in the
  authors' words, there is not yet enough coverage to derive the rules. Passing them is not a
  v1.0 gate; the CPU/COP0/TLB categories are.
- **Performance is unproven at this accuracy level.** No public N64 core sustains full speed
  fully cycle-accurate. A sibling project's equivalent rewrite measured **6–8% slower**
  end-to-end (its isolated CPU loop got ~35% faster — the cost is bus-side), so the timebase
  model itself is a single-digit-percent cost rather than an order-of-magnitude one. The
  discouraging CEN64 evidence is from a stalled C project benchmarked on 2013-era hardware whose
  bus was not cycle-accurate anyway. Treated as an open engineering risk with a measurement gate,
  not as a settled impossibility — and not as refuted either.
- **SYSCMD bit-4 polarity is contradictory between sources** — the manual says command =
  `SysCmd4` 0, the wiki cheat sheet says 1. Pin with a test ROM before encoding either.

## References

- `n64brew_wiki/images/VR4300-Users-Manual.pdf` — the primary timing oracle. §4.1 (stages),
  §4.6–4.7 (interlocks, exception priority), §7.5.6 + Tables 7-13/7-14 (FPU), Tables 3-12/3-13
  (mul/div, branch), §11 + Tables 11-1/11-2 (cache, miss costs), §12 (SysAD).
  Extract text with `mutool draw -F txt`; `pdftotext` fails on this file.
- `n64brew_wiki/markdown/VR4300.md` — load-delay interlock imprecision; the errata.
- `n64brew_wiki/markdown/SysAD Interface.md` — wire protocol, SYSCMD encoding, block ordering.
- `ref-proj/cen64/vr4300/pipeline.c` (BSD-3-Clause) — the reverse-order latch technique.
  Studied for architecture; not copied.
- `ref-proj/ares/ares/n64/cpu/` (ISC) — the cycle-approximate contrast, and the accuracy-struct
  pattern for a future fast path.
