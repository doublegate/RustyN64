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
- **LL/SC link bit** (`LLbit`) — set by `LL`/`LLD`, cleared by `ERET`; `SC`/`SCD`
  *test* it and succeed only if it is set. **Not** cleared by an intervening
  store, and **not** cleared by `SC` itself — see the gotcha below.
  `LLAddr` (COP0 reg 17) holds `PA(31:4)` of the last `LL` and is diagnostic
  only: nothing in the CPU reads it back (UM §5.4.7).

### COP0 register file (implemented, T-12-001)

Table-driven in `crates/rustyn64-cpu/src/cop0.rs`, because nearly every rule here
is data rather than logic:

- **Exactly eight registers are 64 bits wide**: `EntryLo0`, `EntryLo1`,
  `Context`, `BadVAddr`, `EntryHi`, `EPC`, `XContext`, `ErrorEPC` (UM Table 1-2,
  p. 46). There is no generating rule — it is the registers holding an address
  or a TLB entry. A wrong entry is invisible until 64-bit software runs.
- **Writable-bit masks per register.** Hardware discards writes to hardwired,
  reserved and hardware-owned bits, and a write to a masked-off bit **preserves**
  the old value rather than zeroing it. The two easiest to get wrong:
  `Cause` is read-only *except* `IP1:IP0`, and `Status.DS.TS` is read-only while
  the rest of `DS` is not.
- **`Config`'s hardwired fields are merged on read**, not seeded at construction:
  bits 23:16 = `0b00000110` and 14:4 = `0b110_0100_0110` (UM Fig. 5-16, p. 152).
  Seeding them would let a too-wide write mask erase them permanently.
- **`EC` is read-only**, sampled from the `DivMode` pins. We use `0b111` (1:1.5),
  which matches the N64's 62.5 : 93.75 MHz and is *"allowed with the 100 MHz
  model only"* — an **inference**, ledger U-6, not a documented fact.
- **Writing `Wired` forces `Random` to 31** (UM §5.4.2, p. 147) — a side effect
  belonging to neither register alone, which is why it is easy to lose. `Random`
  ranges over `[Wired, 31]` **inclusive**: `Wired` itself is the first
  replaceable entry and `TLBWR` must be able to select it.
- **`LLAddr` is COP0 register 17 and has exactly one storage location.** `LL`
  writes it there; `Pipeline::ll_addr()` reads it back from there. It briefly had
  a second copy on `Pipeline`, which would have let `MFC0 $rt, $17` disagree with
  the CPU's own link address.
- **Reserved registers 7, 21–25, 31 read zero and discard writes — by choice.**
  The manual says only *"Reserved for future use"*. Ledger **U-1**; a test pins
  the choice so changing it is deliberate, not the behaviour.
- **`PRId.Imp = 0x0B`; `Rev` is 0** because the manual gives no value for any
  specific part and warns against depending on it. Ledger **U-3**.

**Stage placement is the manual's, not a convenience.** A COP0 **read happens in
`DC`** and a COP0 **write happens in `WB`**, because UM §4.6.9 defines the CP0
bypass interlock as firing when a write reaches `WB` while the next instruction
reads in `DC`. Performing both in `EX` would make that interlock unexpressible —
the same mistake ADR 0007 exists to prevent one level up. The interlock itself
lands with T-12-005 alongside the cache model.

### Exception dispatch (implemented, T-12-002)

`crates/rustyn64-cpu/src/exception.rs`. The epilogue is UM Fig. 6-14 (p. 201):

1. `Cause.ExcCode` / `Cause.CE`.
2. `BadVAddr` — **address errors and TLB exceptions only**. UM §6.3.2 (p. 164)
   carries an explicit Caution that a Bus Error does *not* write it: the address
   was fine, the transaction failed.
3. `EntryHi` / `Context` / `XContext` — TLB exceptions only (T-12-004).
4. **If `EXL` was 0**: `Cause.BD` and `EPC`. Otherwise both are left untouched.
5. `EXL ← 1`.
6. `PC ← vector`, and the pipeline stalls **2 PCycles** (UM §4.7, p. 114).

**The `EXL` gate in step 4 is the whole point of `EXL`** and the one thing here
that a passing test suite can still get wrong. UM §6.3.7 (p. 174): *"The EXL bit
... is set to 1 to keep the processor from overwriting the address of the
exception-causing instruction contained in the EPC register in the event of
another exception."* An implementation that always writes `EPC` passes every
single-exception test and corrupts every nested one — and nesting is the *normal*
path for TLB refill handlers (UM §6.4.8, p. 188). Note the gate covers `BD` too:
a stale `ExcCode` beside a fresh `BD` misreports which exception was in a delay
slot.

In a delay slot, `EPC` gets **`pc - 4`** — the branch, not the delay-slot
instruction — so the handler resumes where the branch is re-evaluated.

#### The vector table

| Kind | `BEV=0` | `BEV=1` |
| --- | --- | --- |
| Reset / soft reset / NMI | `0xBFC0_0000` | `0xBFC0_0000` |
| TLB refill, **`EXL=0`** | `0x8000_0000` | `0xBFC0_0200` |
| XTLB refill, **`EXL=0`** | `0x8000_0080` | `0xBFC0_0280` |
| Everything else, **and any refill with `EXL=1`** | `0x8000_0180` | `0xBFC0_0380` |

Two manual defects to know about, both pinned by tests so they cannot be
"fixed" back:

- **UM Fig. 6-15 (p. 203) says a refill with `EXL=1` uses `0x080`. It is wrong**
  — contradicted by Tables 6-3/6-4, by §6.4.8 twice, and by Fig. 6-14. Ledger
  **S-3**; CEN64 routes to `0x180` and is right.
- **UM p. 181's prose gives the `BEV=1` general vector as `0x8000_0180`**, a
  typo: Table 6-4's `BEV=1` base is `0xBFC0_0200`, so it is `0xBFC0_0380`. The
  64-bit value in the same sentence is correct and proves it.

Cold reset leaves `BEV` **set** (UM §6.4.4), so a freshly reset CPU vectors into
the boot ROM rather than into RDRAM.

#### `ERET`

`ERL` set → resume at `ErrorEPC`, clear **`ERL`**; otherwise resume at `EPC`,
clear **`EXL`**. Clearing the wrong one either strands the CPU in kernel mode or
returns to the wrong address. `ERET` **always clears `LLbit`** — the other half of
the `LL`/`SC` contract, which had nothing clearing it until now — and has **no
delay slot**, alone among the control transfers.

### Interrupts and the timer (implemented, T-12-003)

Two distinct steps, and conflating them is a real bug:

1. **Assertion** — the `Cause.IP` bits track what hardware is asserting,
   *regardless of masks*, because software polls `Cause` directly. Folding this
   into recognition makes a masked line invisible to `MFC0 Cause`.
2. **Recognition** — `Status.IE` **and** `Status.EXL` clear **and** `Status.ERL`
   clear **and** `Cause.IP & Status.IM` non-zero (UM §6.1 p. 160, §6.3.5 p. 168,
   Fig. 14-4 p. 357). Dropping the `EXL`/`ERL` terms works until an interrupt
   arrives inside a handler, and then re-enters it forever.

Recognition is sampled once per PCycle in `DC`, gated on the previous PCycle
having been a run cycle (UM §4.7.1). That remains the **only** recognition
predicate in the tree.

**The interrupt lines**, from libdragon `cop0.h` (public domain; ledger U-4 —
the CPU manual cannot say, since this is board wiring):

| Bit | Source |
| --- | --- |
| `IP0`, `IP1` | software only — no hardware path |
| **`IP2`** | **the RCP's aggregate line from the MI** |
| `IP3` | CART |
| `IP4` | PRENMI |
| `IP7` | the `Count`/`Compare` timer |

#### `Count` is derived, not incremented

ADR 0006 permits exactly one incremented counter in the core, and it is
`master_ticks`. `Count` is therefore **affine**: the scheduler supplies the
timeline (`count_ticks`, at half PClock — every 4th master tick), and COP0 adds
an epoch that an `MTC0 Count` re-bases. So the register is guest-writable
*without* ever being incremented, and cannot drift from the master clock.

`Count == Compare` raises `IP7`, modelled as a **level** rather than an edge —
which is why *"Writing a value to the Compare register, as a side effect, clears
the timer interrupt"* (UM §6.3.4, p. 165) falls out for free: the write moves the
comparand away and the match stops holding.

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
| Load-delay interlock (LDI) | 1 | UM §4.6.5; `VR4300.md` |
| Store data-cache-busy (DCB) | 1 | UM §4.6.7 |
| Instruction micro-TLB miss (ITM) | 3 | UM §4.6.2 |
| D-cache miss (fill) | 8–9 + M | UM Table 11-1 |
| I-cache miss (fill) | 14–15 + M | UM Table 11-2 |

The cache figures are the sum of the table rows; the 1-cycle spread is the
*"1 to 2 PCycles: synchronize with SClock"* row — the PClock:SClock phase beat
that ADR 0006's seeded phase models. CEN64 corroborates the D-cache number: its
`DCACHE_ACCESS_DELAY` of 44 is exactly 8 + its `MEMORY_WORD_DELAY` of 38.

**The exception epilogue cost IS documented, and this doc previously said it was
not.** UM §4.7 (p. 114), the section's opening sentence:

> *"When a pipeline exception condition occurs, the pipeline stalls for **2
> PCycles** and the instruction causing the exception as well as all those that
> follow it in the pipeline are aborted."*

The earlier note here — and ledger entry C-2, and the timing supplement — all
claimed no figure appeared in UM §4.7, which is precisely where it appears. The
error came from searching Chapter 6 (exception *processing*) and the §4.7 tables
rather than reading §4.7's prose. CEN64's 2 is therefore **corroboration**, not
the source; its source comment asking whether the delay is real is answered.

Two more interlock costs that were likewise wrongly filed as undocumented:

- **CP0I (CP0 bypass interlock) = 1 PCycle** — *"This interlock causes a pipeline
  stall for one PCycle to allow the CP0 register to be written in the WB stage
  before allowing any CP0 register to be read in the DC stage"* (UM §4.6.9,
  p. 113). It fires when an instruction that caused an exception reaches WB while
  the next instruction in DC reads any CP0 register.
- **ITM (instruction micro-TLB miss) = 3 PCycles** — *"A miss penalty of 3
  PCycles is incurred when the micro-TLB is updated from the JTLB"* (UM §4.6.2,
  p. 107). Note this is the **two-entry instruction micro-TLB** in front of the
  32-entry JTLB, not the JTLB itself: a micro-TLB miss is a *stall*, a JTLB miss
  is an *exception*. Whether to model the micro-ITLB separately is an open
  Sprint 2 decision, recorded in that sprint's plan rather than decided here.

The general lesson, recorded because it cost three files: *"undocumented"* is a
claim about the manual that has to be checked against the manual, not inherited
from a previous note. See `docs/engineering-lessons.md`.

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
- **`LL`/`SC` link-bit clearing — this doc previously had it wrong.** The
  manual's list is exhaustive and short: *"The load link bit (`LLbit`) is set by
  the LL instruction, cleared by an ERET, and tested by the SC instruction. The
  only operation to the `LLbit` that can be implemented is a reset due to cache
  invalidation"* (UM §3.1). So an intervening ordinary store does **not** clear
  it, and neither does `SC`. Clearing it in `SC` is the natural-looking mistake —
  several architectures do work that way — and it makes the second iteration of
  a retry loop fail forever. Pinned by `sc_does_not_clear_the_link_bit`.

  What the manual *does* say about intervening accesses is weaker and different:
  a cache miss between `LL` and `SC` "hinders execution of the SC instruction",
  so software is told not to put loads or stores there at all (UM §16 p. 453).
  That is a caution to programmers, not an architectural clear.

  `ERET` is the one clearer, and it lands with the exception model in Sprint 2.
  Until then nothing clears the bit — correct as far as it goes, and incomplete.
- **`SC` is a store with a register destination.** It writes 1/0 to `rt` whether
  or not the store happened, which is a shape no other integer instruction has.
  Decoding it with the store form (no destination) silently loses the flag, and
  folding it into `is_load` stalls a cycle the hardware does not.
- **`SYNC` is a NOP, not a reserved encoding.** *"the SYNC instruction is handled
  as a NOP"* (UM §3.1), which is also why the VR4300 needs no memory barrier
  model: loads and stores already execute in program order. Decoding it to
  `Reserved` would raise on code that runs on hardware.
- **`CACHE` (`0o57`) is still `Reserved`** — deliberately, until the cache model
  lands in Sprint 2. Note this means it currently *raises*; IPL3 and libdragon
  both use it, so this is a known blocker for anything past a bare test ROM.
- **`LL` to an uncached address is undefined** (UM §16 p. 453). Not currently
  detected; if a test ROM ever depends on it, it becomes an accuracy-ledger
  entry rather than a special case.
- **SysAD is the only window out.** Every non-cached access becomes a SysAD
  transaction the RCP arbitrates against RSP/RDP/DMA traffic
  (`ref-docs/research-report.md` §1) — relevant to the bus-contention model
  (`docs/scheduler.md` open question).

## The documented errata — reproduced, not corrected

The VR4300 has known hardware bugs that software can observe and depend on.
**Implementing the manual's described behaviour instead of the hardware's is the
bug.** Each is pinned by a named test that fails if it is "fixed", so the intent
survives a well-meaning future reader.

The manual documents none of these; `n64brew_wiki/markdown/VR4300.md` § Known
Bugs is the only source, and the results are for processor revision 2.2.

### `SRA` / `SRAV` leak the upper 32 bits — **all consoles**

The manual says an arithmetic right shift fills the high bits of the low word
with copies of bit 31, then sign-extends bit 31 into the upper half. Hardware
instead fills from the **upper 32 bits of the register** first, then sign-extends
the new bit 31 — leaking 64-bit state that should be inaccessible, in both 32-
and 64-bit mode.

```text
manual:   rd = (uint64_t)(int32_t)((int32_t)rt >> sa)
hardware: rd = (uint64_t)(int32_t)((int64_t)rt >> sa)
```

`rt = 0x0123456789ABCDEF`, `sa = 16`: the manual predicts `0xFFFFFFFFFFFF89AB`;
hardware gives `0x00000000456789AB`. Not known to have ever been fixed, and
present on more consoles than the FP multiply bug — so software can rely on it.
Tests: `sra_reproduces_the_vr4300_erratum`, `srav_shares_the_sra_erratum`.

### `MULT` is 64-bit × **35-bit**

When inputs are not properly sign-extended 32-bit values, the second operand is
sign-extended on **bit 34** before a 64-bit multiply, and the first is taken as a
full 64-bit value. For well-formed inputs it reduces to the expected 32×32
multiply, which is why ordinary compiler output never trips over it.
Test: `mult_reproduces_the_35_bit_sign_extension_erratum`.

### `DIV` is 32-bit ÷ **35-bit**

The dividend is sign-extended on bit 31 and the divisor on **bit 34**.
Test: `div_reproduces_the_35_bit_divisor_sign_extension_erratum`.

**One case is unknown even to N64brew**: when bits 63 and 31 of the divisor
differ, the `LO` quotient is documented as incorrect and *"it is currently
unclear how the outputs of this last case are arrived at"*. `HI` is better
founded — `remainder = (int32_t)(dividend - quotient * divisor)` computed in
64-bit. What we do there is a **guess**, recorded as such in
`docs/accuracy-ledger.md` C-5 rather than left looking authoritative.

### The FP multiplication bug — **deferred to Sprint 3**

Board-revision conditional: NUS-01, NUS-02 and NUS-03 only, fixed in later
steppings. A `mul` in a branch delay slot can corrupt a *subsequent* multiply
when operands include NaN, zero or infinity. GCC's `-mfix4300` inserts two `nop`s
after every `mul.s`/`mul.d`/`mult`.

Not implemented: it needs COP1, which is Sprint 3. The exact corrupted output is
also not documented — only the trigger conditions and the affected revisions —
so it will need characterising against hardware, and belongs in the accuracy
ledger when it lands. Modelling it will also require the console revision to be a
machine parameter, since unlike the others this erratum is **not** universal.

`PRId` correlates: processor id always `0x0B`; revision `0x10` (1.0, early units)
or `0x22` (2.2, later) on retail, `0x40` on iQue.

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
