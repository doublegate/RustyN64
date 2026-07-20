# 2026-07-20 — VR4300 timing supplement and corrections to `research-report.md`

**Status:** dated supplement. Per `ref-docs/README.md` this corpus is immutable, so the
corrections below are recorded here rather than edited into `research-report.md`. Where this
file and `research-report.md` disagree, **this file wins** — it is sourced from the vendor
manual, which the report was summarising second-hand.

**Primary source:** `n64brew_wiki/images/VR4300-Users-Manual.pdf` — the NEC VR4300 User's Manual
(U10504EJ7V0UM00), 655 pages, already present in the local wiki mirror with an intact text
layer. Cited below as *UM §x*.

> **Extraction note.** `pdftotext` fails on this file and `file` misreports it as 27 pages.
> Use `mutool draw -F txt n64brew_wiki/images/VR4300-Users-Manual.pdf` — it yields ~809 KB of
> text across all 655 pages.

Secondary: `n64brew_wiki/markdown/{Clock Timing,VR4300,SysAD Interface,MI,RDRAM,Memory map}.md`
(CC BY-SA 4.0 — attribute if quoted).

---

## 1. Correction — the pipeline stages are IC / RF / EX / DC / WB

`research-report.md` §1 states the VR4300 pipeline as **"IF, RF, EX, DF, WB"**. That is wrong.
UM §4.1 Figure 4-1 names them:

- **IC** — Instruction Cache Fetch
- **RF** — Register Fetch
- **EX** — Execution
- **DC** — Data Cache Fetch
- **WB** — Write Back

This is not cosmetic. The manual's entire interlock and exception taxonomy is named
stage-relative — DCM (Data Cache Miss), DCB (Data Cache Busy), ICB (Instruction Cache Busy),
ITM (Instruction TLB Miss), LDI (Load Interlock), MCI (Multi-Cycle Interlock), CP0I (CP0 Bypass
Interlock) — and so are the priority rules (§4.7.4: WB > DC > EX > RF). Using `IF`/`DF` makes
every one of those citations unresolvable against the manual.

UM §4.1 also states that a PCycle itself has two phases, **F1 and F2**, and that "at least
5 PCycles are required to execute an instruction".

## 2. Correction — the CPU is derived from MClock, not the other way round

`research-report.md` treats the 93.75 MHz VR4300 clock as the machine's primary rate.
`Clock Timing.md` gives the actual derivation, with every rate as an exact fraction:

| Clock | Derivation | Exact | MHz |
| --- | --- | --- | --- |
| X2 crystal | — | 250/17 | 14.7058823529 |
| RCLK | X2 × 17 | 250 | 250 |
| **MClock** | RCLK ÷ 4 | 125/2 | **62.5** |
| **VR4300 PClock** | MClock × 3/2 (internal PLL, `DivMode = 0b01`) | 375/4 | **93.75** |
| SClock (CPU system interface) | = MClock, normally | 125/2 | 62.5 |
| Serial Interface | MClock ÷ 4 | 125/8 | 15.625 |
| Cartridge / PIF | SI ÷ 8 | 125/64 | 1.953125 |

**MClock is primary.** The VR4300 multiplies up from it. The VI video clock (VCLK, 48.681818 MHz
NTSC) derives from a *different* crystal (**X1**, 14.31818 MHz) and is therefore a genuinely
separate clock domain — it is not a rational multiple of the CPU/RCP pair.

Consequence recorded in [ADR 0006](../docs/adr/0006-one-canonical-master-clock.md): a
187.5 MHz canonical tick makes CPU (÷2), RCP (÷3), COP0 `Count` (÷4), SI (÷12) and PIF (÷96) all
integer divisors, while VI necessarily keeps a fractional accumulator.

## 3. Correction — "one instruction per cycle" is not a timing model

Any statement that the CPU retires one instruction per cycle is contradicted by the manual's own
tables. Documented costs, all in PCycles:

| Class | Cost | Source |
| --- | --- | --- |
| Integer ALU / most instructions | 1 | §7.5.6 (a latency statement; see caveat) |
| `MULT` / `MULTU` | 5 — **stalls the entire pipeline** | Table 3-12 |
| `DIV` / `DIVU` | 37 | Table 3-12 |
| `DMULT` / `DMULTU` | 8 | Table 3-12 |
| `DDIV` / `DDIVU` | 69 | Table 3-12 |
| FPU add / sub | 3 | Table 7-14 |
| FPU mul (S / D) | 5 / 8 | Table 7-14 |
| FPU div, sqrt (S / D) | 29 / 58 | Table 7-14 |
| Load-delay interlock (LDI) | 1 | §4.6.5 |
| Store data-cache-busy (DCB) | 1 | §4.6.7 |
| Instruction micro-TLB miss (ITM) | 3 | §4.6.2 |
| D-cache miss (fill) | **8–9 + M** | Table 11-1 |
| I-cache miss (fill) | **14–15 + M** | Table 11-2 |

Caveat on the 1-cycle baseline: §7.5.6's supporting sentence ("All CPU/FPU instruction delay
times that are not mentioned in these tables have a latency of one pipeline clock cycle") is a
*latency* claim inside the FPU chapter, used above as an *issue cost*. Defensible against §4.1's
throughput model, but an inference rather than a direct citation.

Two rules the table cannot convey:

- **FPU latency for a dependent consumer = execution rate + 1**, because no EX-to-RF bypass is
  performed for those results (§7.5.6). The table gives rates.
- **FPU ops exit early on trivial operands** (§7.5.6): add/sub terminate on cycle 2 on a source
  exception or a zero/infinity operand; multiply also finishes in 2 if either operand is a power
  of two; divide and sqrt exit on cycle 2 for zero/infinity results.

The cache-miss figures are sums of the Table 11-1/11-2 rows. The 1-cycle spread in each is the
*"1 to 2 PCycles: synchronize with SClock"* row — a **vendor-documented indeterminacy** arising
from the 3:2 PClock:SClock beat, and the hardware basis for the seeded power-on phase in ADR 0004.

## 4. Caches — line sizes and organisation

| | I-cache | D-cache |
| --- | --- | --- |
| Capacity | 16 KB | 8 KB |
| Line size | **32 bytes (8 words)** | **16 bytes (4 words)** |
| Associativity | direct-mapped | direct-mapped |
| Indexing | virtually-indexed, physically-tagged | virtually-indexed, physically-tagged |
| Write policy | n/a | **write-back** |

Source UM §11.2. A cached read costs 1 PCycle; a cached **write costs 2** (address+tag, then
data RAM), which is what makes DCB a real interlock.

Note from `Memory map.md`: **RDRAM is the only region where the RCP supports cached access.**
Cache requests to any other range are ignored and "will thus freeze the CPU requiring a hard
reset".

## 5. Correction — interrupt sampling has no SysAD-phase relationship

An earlier design in this project assumed interrupts are sampled at a particular half of a SysAD
transaction. **No such coupling is documented anywhere** — not in the manual, not on the wiki.

The documented rule is per-PCycle and gated on stall state (UM §4.7.1, verbatim):

> "NMI and interrupt exception requests are accepted only if the previous PCycle was a run cycle."

The stage is also documented: UM Figure 4-12 places `INTR` in the **DC** column, and §4.7.6
"DC-Stage Interlock and Exception Priorities" lists the interrupt exception among them.

The MI aggregate arrives on **Int0 = `Cause.IP2`**. `MI_MASK` is level-sensitive: masking does
not prevent a line being latched in `MI_INTERRUPT`, and enabling a mask while its line is
already set signals the CPU immediately (`MI.md`).

## 6. The load-delay interlock is imprecise, and the imprecision is the spec

From `VR4300.md` § Microarchitecture → Load Delay Interlock. The VR4300 stalls 1 cycle after a
load if the next instruction *appears* to use the result, and the detection is deliberately
sloppy:

- It matches the load's `rt` against the next instruction's `rs` **or** `rt` field, **whether or
  not that field is actually used as a source**.
- So `lw t0, …` followed by `lui t0, …` stalls, and two consecutive loads into the same register
  stall.
- GPR loads interlock only with non-float instructions; FPR loads only with float instructions.
- A load into `$zero` **never** interlocks.

Emulating the precise behaviour instead of the imprecise one is a bug.

## 7. What is NOT documented — measure these, do not guess

Recorded so nobody re-derives them from wishful thinking. Each belongs in
`docs/accuracy-ledger.md` as a measured constant with its provenance.

1. **`M` — memory access time in PCycles.** Both cache-miss formulas are parameterised on it and
   no source gives a value. Informal hints only: RDRAM "about 10-20+ clock wait time"; RCP
   registers "5-6 PClock cycles"; MI registers "about 2"; RSP DMEM/IMEM "4-5". For scale, CEN64
   charges a flat 38 PClocks for an uncached word and ares uses different constants; the two most
   accurate emulators disagree and neither derived theirs from a spec.
2. **The exception epilogue cost.** Commonly quoted as 2 PCycles, but no figure appears in UM
   §4.7 or chapter 6. The number is CEN64's, and its source comments *"TODO: Is the cycle count
   just the killing of IC/RF, or do we actually delay an additional two cycles?"*
3. **CP0I (CP0 Bypass Interlock) cost.** Named in UM Table 4-3 and §4.6.9; no cycle count found.
4. **RDRAM row-hit vs row-miss vs dirty-row-miss costs.** Described qualitatively (Ack/NAck,
   close-and-reload, "takes even longer if dirty") with no cycle counts, and the `RasInterval` /
   `Delay` register values IPL3 programs are not translated into cycles.
5. **Whether the 1-vs-2-PCycle SClock resync is deterministic** given a known power-on phase, or
   genuinely indeterminate per transaction.
6. **Bus arbitration cost** when CPU, RSP, RDP and DMA contend for RDRAM. Nothing anywhere.
7. **`SYSCMD` bit-4 polarity — the sources contradict each other.** UM §12.11.1 says command =
   `SysCmd4` **0**; the wiki's cheat sheet says command = bit 4 **1**. Pin with a test ROM before
   encoding either.
8. **`div` quotient when divisor bits 63 and 31 differ** — explicitly "currently unclear" to
   N64brew (see §8).
9. **The exact corrupted output of the FP multiplication bug** — only trigger conditions and
   affected board revisions are documented.

## 8. Errata — exact described behaviour

The manual documents none of these; `VR4300.md` § Known Bugs is the only source.

**32-bit shift-right-arithmetic bug.** Applies to `sra`/`srav` on *all* consoles; implement
unconditionally. The manual claims the low 32 bits are filled with copies of bit 31 and then
sign-extended. In practice the most significant bits are filled from the *upper 32 bits of the
register* first, then the new bit 31 is sign-extended — leaking 64-bit state that should be
inaccessible, in both 32- and 64-bit mode.

```text
manual:   rd = (uint64_t)(int32_t)((int32_t)rt >> sa)
hardware: rd = (uint64_t)(int32_t)((int64_t)rt >> sa)
```

With `rt = 0x0123456789ABCDEF`, `sa = 16`: the manual predicts `0xFFFFFFFFFFFF89AB`; hardware
gives `0x00000000456789AB`.

**`mult` / `div` sign-extension bugs** (processor revision 2.2). When inputs are not properly
sign-extended 32-bit values: `mult` acts as a 64-bit × **35-bit** signed multiply (the second
operand sign-extended on bit 34). `div` acts as 32-bit ÷ 35-bit (dividend sign-extended on bit
31, divisor on bit 34) — **except when bits 63 and 31 of the divisor differ**, where the `LO`
quotient is simply wrong and how it is arrived at is unknown. `HI` still satisfies
`remainder = (int32_t)(dividend - quotient * divisor)` computed in 64-bit.

**FP multiplication bug.** Board-revision conditional — NUS-01, NUS-02, NUS-03 only (fixed in
later steppings). A `mul` in a branch delay slot can corrupt a *subsequent* multiply when the
operands include NaN, zero or infinity. GCC's `-mfix4300` inserts two `nop`s after every
`mul.s`/`mul.d`/`mult`. The exact corrupted output is not documented.

`PRId` correlates: processor id always `0x0B`; revision `0x10` (1.0, early) or `0x22` (2.2,
later) on retail, `0x40` on iQue. `FCR0` implementation `0x0B`, revision `0x00`.

## 9. SysAD — protocol facts worth having in one place

SClock = MClock = 62.5 MHz, so **one SysAD cycle = 1.5 PCycles**. `SysAD Interface.md`: "all
access between the RCP and CPU is at the masterclock rate (62.5mhz)".

A transaction is a command cycle then a data cycle, handshaked by `EOK` / `Pvalid` / `Evalid`,
with the wait between them **unbounded** — there is no fixed command→data latency, and that gap
is where real RDRAM latency lives.

Block-transfer ordering, which is easy to get wrong:

- **D-cache 128-bit reads** are issued 64-bit-aligned. If address bit 4 is **clear**, sequential
  ordering; if **set**, *sub-block* ordering — the requested 64 bits arrive first, then the
  64 bits *below* it.
- **I-cache 256-bit reads** are always 256-bit aligned and always sequential (8 × 32-bit beats).
- **D-cache write-backs** are always 128-bit aligned; the full line is written on a dirty evict.
- 8/16/24-bit write data is address-aligned on the data bus **and repeated**, so the RCP needs no
  alignment logic.
- 40/48/56-bit writes are split into two 32-bit writes, LSB first.

All reads of 8/16/24/32 bits are performed as 32-bit reads; the CPU does the shifting internally.

## 10. COP0 `Count` runs at half PClock

UM §6.3.3 Figure 6-3: *"Count : latest count value (incremented at frequency half PClock)"* —
46.875 MHz. `Count` is architecturally writable via `MTC0`, so it cannot be modelled as a pure
function of elapsed time; it must be affine and re-based on write.

All three reference emulators implement the halving by storing at PClock and shifting right on
read. Forgetting that shift is a documented source of 2x timing bugs, and n64-systemtest's
`timing` feature set keys off this rate.

## 11. Upstream test-suite caveat

`n64-systemtest`'s `cycle` and `cop0hazard` feature sets are **default-off upstream**, with the
authors' stated reason: *"There isn't much test coverage to fully derive the rules yet (and most
likely this wouldn't be implementable by a dynamic recompiler)."* Exact COP0 hazard cycle rules
are therefore not settled by anyone. Do not plan a v1.0 accuracy gate around passing them.
