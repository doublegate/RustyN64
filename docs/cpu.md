# NEC VR4300 (MIPS R4300i) — RustyN64

**References:** `ref-docs/research-report.md` §1, §8 (interrupts); the R4300i Data
Sheet + NEC VR4300 User's Manual (cited there); `crates/rustyn64-cpu/src/lib.rs`;
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

```rust
pub enum BusPhase { Phi1, Phi2 }

pub trait Bus {
    fn read_u8(&mut self, addr: u32) -> u8;
    fn write_u8(&mut self, addr: u32, val: u8);
    fn read_u32(&mut self, addr: u32) -> u32;   // big-endian; core overrides fast
    fn write_u32(&mut self, addr: u32, val: u32);
    fn poll_irq_at_phase(&mut self, phase: BusPhase) -> bool; // MI ∩ mask
}

pub struct Cpu {
    pub gpr: [u64; 32], // gpr[0] hard-wired zero
    pub hi: u64, pub lo: u64,
    pub pc: u64,        // power-on 0xBFC0_0000 (KSEG1 PIF boot ROM)
    pub cycles: u64,
}
impl Cpu {
    pub const fn new() -> Self;
    pub fn tick<B: Bus>(&mut self, bus: &mut B); // one issued instruction
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
- **Caches** — 16 KB instruction cache + 8 KB write-back data cache (24 KB L1
  total), direct-mapped. Model coherency against DMA: cart/RSP DMA writes land in
  RDRAM behind the cache, and games explicitly `CACHE`-flush/invalidate.
- **LL/SC link bit** — set by `LL`/`LLD`, cleared by intervening stores; `SC`/
  `SCD` succeed only if still set.

## Behavior

### Pipeline and timing

Classic MIPS **5-stage** in-order scalar pipeline (IF, RF, EX, DF, WB) with a
single branch/load delay slot (`ref-docs/research-report.md` §1). The CPU
advances one issued instruction per master tick in the lockstep scheduler;
multi-cycle operations (multiply/divide, cache misses, FPU) consume the modelled
extra cycles by advancing `cycles` / stalling the issue.

### Virtual address space

Standard MIPS segment layout (`ref-docs/research-report.md` §1):

| Segment | Range | Mapping |
|---|---|---|
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
COP0 timer (`Count` == `Compare` → IP7) and the two software interrupts. The
`poll_irq_at_phase` hook samples the live MI∩mask at the correct bus phase.

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
