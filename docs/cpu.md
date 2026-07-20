# NEC VR4300 (MIPS R4300i) — RustyN64

**References:** **`n64brew_wiki/images/VR4300-Users-Manual.pdf`** — the primary
timing oracle, 655 pages, already in the local mirror. Extract its text with
`mutool draw -F txt`; `pdftotext` fails on this file. Cited below as *UM §x*.
Also: `n64brew_wiki/markdown/VR4300.md` (errata, load-delay interlock),
`Clock Timing.md`, `SysAD Interface.md`; `ref-docs/research-report.md` §1, §8;
ADR 0006 (the clock), ADR 0007 (the pipeline); `crates/rustyn64-cpu/src/lib.rs`;
`docs/scheduler.md`; `docs/cart.md`.

This doc is the SPEC, not history — update it in the same PR as the code. Pin
behaviour against **n64-systemtest** FIRST (test-ROM-is-spec), then implement
until it passes.

## Purpose

The VR4300 (NEC `uPD30200`) is the N64's main CPU: a licensed, cost-reduced MIPS
R4300i implementing the **MIPS III** 64-bit instruction set at **93.75 MHz**. It
runs game code, builds display/audio lists in RDRAM, drives every subsystem by
writing RCP registers, and services the RCP's aggregate interrupt. It is the
master tick unit of the scheduler (`docs/scheduler.md`).

## Interfaces

The CPU borrows the system memory bus during `tick`. The trait it sees
(`crates/rustyn64-cpu/src/lib.rs`):

`BusPhase` names the two halves of a SysAD transaction after the hardware's own
encoding: `SYSCMD` bit 4 is documented as "Command or Data"
(`n64brew_wiki/markdown/SysAD Interface.md`). It describes the **bus protocol**
and nothing else.

In particular it carries **no interrupt semantics.** An earlier revision of this
doc paired it with a `poll_irq_at_phase(BusPhase)` hook, on the assumption that
interrupts are sampled at a particular half of a bus transaction. No such
coupling is documented anywhere — not in the User's Manual, not on the wiki. The
documented rule (UM §4.7.1) is per-PCycle and gated on stall state: *"NMI and
interrupt exception requests are accepted only if the previous PCycle was a run
cycle."* That hook was therefore shaped on the wrong axis and has been removed;
see ADR 0007 and `docs/engineering-lessons.md` §3.2.

SysAD runs at SClock = MClock = 62.5 MHz, so one bus cycle is 1.5 PCycles — 3
master ticks against the CPU's 2 (ADR 0006). This is *not* the deferred ADR 0005
refactor, which concerns resolution finer than one PClock.

```rust
pub enum BusPhase { Command, Data }

pub trait Bus {
    fn read_u8(&mut self, addr: u32) -> u8;
    fn write_u8(&mut self, addr: u32, val: u8);
    fn read_u32(&mut self, addr: u32) -> u32;   // big-endian; core overrides fast
    fn write_u32(&mut self, addr: u32, val: u32);
    // NOTE: no `poll_irq_at_phase`. Interrupts are sampled once per PClock in
    // the DC stage, gated on the previous PCycle having been a run cycle.
}

pub struct Cpu {
    pub gpr: [u64; 32], // gpr[0] hard-wired zero
    pub hi: u64, pub lo: u64,
    pub pc: u64,        // power-on 0xBFC0_0000 (KSEG1 PIF boot ROM)
    pub cycles: u64,
}
impl Cpu {
    pub const fn new() -> Self;
    pub fn tick<B: Bus>(&mut self, bus: &mut B); // ONE PClock, not one instruction
}
```

Addresses handed to the `Bus` are **post-TLB physical** addresses; virtual→
physical translation (KSEG0/KSEG1 direct map + TLB lookup) happens inside the CPU
crate before the bus access.

## State

Architectural state the full core must hold (the skeleton has only the GPR file,
HI/LO, PC, and a cycle counter; the rest are marked TODOs):

- **GPR file** — 32 × 64-bit; `gpr[0]` reads zero, writes discarded.
- **HI / LO** — 64-bit multiply/divide result pair.
- **PC + branch-delay latch** — MIPS branches and loads have a single delay slot;
  the delay-slot instruction executes before the branch target. Model the
  in-delay-slot flag and the pending target.
- **COP0 (System Control)** — `Status`, `Cause`, `EPC` (64-bit), `BadVAddr`
  (64-bit), `EntryHi` (64-bit) / `EntryLo0` / `EntryLo1`, `PageMask`, `Index`,
  `Random`, `Wired`, `Count`, `Compare`, `Config`, `Context`, `XContext`,
  `LLAddr`, `PRId`, plus the **32-entry TLB** (each entry maps a page pair, page
  sizes 4 KB…16 MB via `PageMask`). Per `ref-docs/research-report.md` §1,
  several COP0 registers are genuinely 64-bit and n64-systemtest verifies this.
- **COP1 (FPU)** — 32 FP registers, `FCR0` (revision) + `FCR31` (control/status:
  rounding mode, enables, cause/flag bits). The `FR` bit in `Status` selects
  32-vs-64-bit register-file aliasing.
- **Caches** — 16 KB instruction cache (32-byte lines) + 8 KB write-back data
  cache (16-byte lines), 24 KB L1 total, both direct-mapped, virtually-indexed
  and physically-tagged (UM §11.2). Model coherency against DMA: cart/RSP DMA writes land in
  RDRAM behind the cache, and games explicitly `CACHE`-flush/invalidate.
- **LL/SC link bit** — set by `LL`/`LLD`, cleared by intervening stores; `SC`/
  `SCD` succeed only if still set.

## Behavior

### Pipeline and timing

Classic MIPS **5-stage** in-order scalar pipeline with a single branch/load delay
slot. The stages are **IC** (Instruction Cache Fetch), **RF** (Register Fetch),
**EX** (Execution), **DC** (Data Cache Fetch), **WB** (Write Back) — VR4300
User's Manual §4.1, Figure 4-1. Note this corrects the "IF/RF/EX/DF/WB" naming in
`ref-docs/research-report.md` §1; every interlock and exception in the manual's
taxonomy is named stage-relative (DCM, DCB, ICB, LDI, CP0I), so the names are
load-bearing.

The CPU advances **one PClock per step**, not one instruction — at least 5
PCycles are required to execute an instruction (§4.1), and `DDIV` stalls the
whole pipeline for 69 (Table 3-12). The pipeline is modelled as four inter-stage
latches advanced in **reverse stage order (WB → DC → EX → RF → IC)**, which is
what makes the latching implicit: a stage reads its input latch before any
upstream stage writes it, so no value propagates two stages in one cycle. That
ordering is a load-bearing invariant — see ADR 0007.

Under ADR 0006 the scheduler counts 187.5 MHz master ticks; the CPU steps every
2nd tick (93.75 MHz PClock) and the RCP every 3rd (62.5 MHz). COP0 `Count` runs
at **half PClock** (46.875 MHz = every 4th master tick).

### Cycle costs (documented, in PCycles)

Transcribed from the User's Manual; implement these directly rather than fitting
them. Baseline is 1 PCycle for essentially all integer ALU work (UM §7.5.6).

| Class | Cost | Source |
| --- | --- | --- |
| `MULT` / `MULTU` | 5 | UM Table 3-12 |
| `DIV` / `DIVU` | 37 | UM Table 3-12 |
| `DMULT` / `DMULTU` | 8 | UM Table 3-12 |
| `DDIV` / `DDIVU` | 69 | UM Table 3-12 |
| FPU add / sub | 3 | UM Table 7-14 |
| FPU mul (S / D) | 5 / 8 | UM Table 7-14 |
| FPU div, sqrt (S / D) | 29 / 58 | UM Table 7-14 |
| Load-delay interlock (LDI) | 1 | `VR4300.md`; UM §4.6.3 |
| Store data-cache-busy (DCB) | 1 | UM §4.4, §11.2 |
| Instruction micro-TLB miss (ITM) | 3 | UM §4.6.2 |
| Exception epilogue | 2 | UM §4.7 |
| D-cache miss | 7–8 + M | UM Table 11-1 |
| I-cache miss | 13–14 + M | UM Table 11-2 |

Integer multiply and divide **stall the entire pipeline** for the listed count
(UM Table 3-12) — they are not background operations.

Two rules the table alone does not convey:

- **FPU latency for a dependent consumer is the execution rate + 1**, because no
  EX-to-RF bypass is performed for those results (UM §7.5.6). The numbers above
  are rates.
- **FPU ops exit early on trivial operands.** Add/sub terminate on the second
  cycle on a source exception or if an operand is zero or infinity; multiply also
  finishes in two cycles if either operand is a power of two; divide and sqrt
  exit on the second cycle for zero/infinity results (UM §7.5.6).

**`M` is not documented anywhere.** It is the memory access time in PCycles, and
both cache-miss formulas are parameterised on it. The only figures available are
informal estimates (RDRAM "about 10-20+ clock wait time"; RCP registers "5-6
PClock cycles"; MI registers "about 2"; RSP DMEM/IMEM "4-5"). It must be fitted
against test ROMs and recorded in the accuracy ledger as a measured constant —
never quietly tuned until a ROM passes. For scale, CEN64 charges a flat 38/44/48
PClocks for uncached/D-fill/I-fill and ares charges 40 for a D-fill; the two most
accurate N64 emulators disagree, and neither number came from a spec.

### The interlock taxonomy

UM Table 4-3 names them, and the names are what the manual's priority rules key
off: ITM (instruction TLB miss), ICB (instruction cache busy), **LDI** (load
interlock), MCI (multi-cycle interlock), DCM (data cache miss), DCB (data cache
busy), COp (cache op), **CP0I** (CP0 bypass interlock — named, but no cycle count
located; derive it).

Priority runs **WB > DC > EX > RF** (UM §4.7.4): a later stage's exception or
stall request always outranks an earlier stage's. When RF and DC both request a
stall in the same cycle, DC wins, because both need the same resources (the
system interface and the TLB) — UM §4.7.3.

### Virtual address space

Standard MIPS segment layout (`ref-docs/research-report.md` §1):

| Segment | Range | Mapping |
| --- | --- | --- |
| KUSEG | `0x0000_0000`–`0x7FFF_FFFF` | TLB-mapped |
| KSEG0 | `0x8000_0000`–`0x9FFF_FFFF` | direct, **cached** |
| KSEG1 | `0xA000_0000`–`0xBFFF_FFFF` | direct, **uncached** |
| KSSEG/KSEG3 | `0xC000_0000`–`0xFFFF_FFFF` | TLB-mapped |

Hardware registers are reached through KSEG1 (uncached). The skeleton bus strips
`addr & 0x1FFF_FFFF` to the physical RDRAM window; the full TLB path replaces that
for mapped segments.

### Interrupts

Per `ref-docs/research-report.md` §1, §8: an interrupt is serviced only when a
`Cause.IP[n]` pending bit and its `Status.IM[n]` mask bit are both set **and**
`Status.IE=1`, `EXL=0`, `ERL=0`. The RCP's aggregate interrupt arrives on one IP
bit (IP2, driven by the MI — `(MI_INTR & MI_INTR_MASK) != 0`). Others are the
COP0 timer (`Count` == `Compare` → IP7) and the two software interrupts. Sampling
happens once per PClock in the DC stage and is accepted only if the previous
PCycle was a run cycle (UM §4.7.1) — exactly one recognition predicate exists in
the tree, since carrying two subtly different ones is a known source of
one-cycle discrepancies in other emulators.

### Exceptions

Address-error (unaligned), TLB refill/invalid/modified, integer overflow
(`ADD`/`ADDI`/`SUB`/`DADD`/...), `TRAP`, `BREAK`, `SYSCALL`, coprocessor-unusable,
FP exceptions, and the FPU **unimplemented-operation** exception. On exception:
save PC (or branch PC) to `EPC`, set `Cause.ExcCode` + `BD`, set `Status.EXL`,
vector to the exception entry point. n64-systemtest exercises overflow, unaligned,
and the TRAP/BREAK/SYSCALL family explicitly.

## Edge cases and gotchas

- **64-bit COP0 registers.** `EntryHi`, `BadVAddr`, `EPC`, `Context`/`XContext`
  hold 64-bit values; truncating them to 32-bit fails n64-systemtest's COP0
  category (`ref-docs/research-report.md` §1, §7).
- **The branch-delay slot is real.** The instruction after a branch executes;
  exceptions in a delay slot set `Cause.BD` and save the branch PC to `EPC`.
- **FPU `FR`-bit register aliasing.** With `FR=0`, the 32 FP registers alias as
  16 even/odd pairs for doubles; with `FR=1` all 32 are independent 64-bit. A
  common emulator bug (`ref-docs/research-report.md` §1).
- **FPU NaN + unimplemented-op.** Quiet/signaling NaN propagation and the
  unimplemented-operation exception (for denormals / out-of-range conversions)
  are edge cases the suites hit.
- **Cache ↔ DMA coherency.** The data cache is write-back; DMA writes bypass it.
  Games flush/invalidate explicitly — model `CACHE` ops and the dirty-line
  write-back, or framebuffer/DMA data goes stale (`ref-docs/research-report.md`
  §1, §Open questions 4).
- **`LL`/`SC` link-bit clearing.** Any intervening store (or ERET) clears the
  link; `SC` must then fail and write 0 to its target register.
- **SysAD is the only window out.** Every non-cached access becomes a SysAD
  transaction the RCP arbitrates against RSP/RDP/DMA traffic
  (`ref-docs/research-report.md` §1) — relevant to the bus-contention model
  (`docs/scheduler.md` open question).

## Test plan

- **Golden-log:** capture `(pc, gpr, cycle)` per retired instruction and 0-diff
  against a reference VR4300 trace of n64-systemtest (the `GoldenLogDiffer`,
  `docs/testing-strategy.md`).
- **n64-systemtest categories (the strict gate):** CPU instructions, COP0 access
  (MFC0/DMFC0/MTC0/DMTC0 + 64-bit behaviour), atomics (LL/LD/SC/SCD), exceptions
  (overflow/unaligned/TRAP/BREAK/SYSCALL), the TLB, multi-width (8/16/32/64-bit)
  memory access to RAM/ROM/SPMEM/PIF, and COP0 hazards/timing. "Failed: 0" is the
  bar (`ref-docs/research-report.md` §7).
- **FPU:** IEEE-754 single/double op vectors, NaN propagation, FR-bit modes,
  rounding modes, the unimplemented-op exception.
- **TLB:** refill/invalid/modified exceptions across all page sizes; `Random`/
  `Wired` index behaviour; `TLBP`/`TLBR`/`TLBWI`/`TLBWR`.

## Open questions

- **Interpreter vs dynarec ordering.** Start interpreter-only (the determinism
  oracle), add a CPU dynarec later if perf needs it (`docs/performance.md`;
  `ref-docs/research-report.md` §challenge 7).
- **Cache-model depth** — exactly which test ROMs gate the I/D-cache + DMA
  coherency model (`ref-docs/research-report.md` §Open questions 4).
