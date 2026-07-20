# RustyN64 — Deep Research Report

**Generated:** 2026-06-24
**Mode:** Autonomous
**Source count:** 18 primary, 9 secondary

## Executive summary

The Nintendo 64 (1996) is a 64-bit cartridge console built around two big
chips and a unified Rambus memory pool. The CPU is the **NEC VR4300**, a
licensed, cost-reduced variant of the MIPS R4300i implementing the
**MIPS III** 64-bit instruction set, clocked at **93.75 MHz**. The
**Reality Coprocessor (RCP)** — an SGI/Nintendo ASIC clocked at **62.5 MHz**
— is the heart of the machine: it contains the **Reality Signal Processor
(RSP)**, a MIPS-derived scalar core fused with a 8-lane-wide 16-bit SIMD
vector unit that runs downloadable **microcode** for geometry and audio; the
**Reality Display Processor (RDP)**, a fixed-function rasterizer that writes
the framebuffer into RDRAM; and the eight memory-mapped sub-interfaces
(**SP, DP, MI, VI, AI, PI, RI, SI**) through which the CPU drives every other
subsystem. Both the VR4300 clock and the RCP clock derive from a single
system timebase, so accurate emulation is fundamentally a **co-scheduling**
problem across three asynchronous engines (CPU, RSP, RDP) sharing one memory
bus ([N64brew VR4300](https://n64brew.dev/wiki/VR4300),
[Copetti, N64 Architecture](https://www.copetti.org/writings/consoles/nintendo-64/)).

The defining architectural choice for an accuracy-focused emulator is
**LLE versus HLE**. High-Level Emulation (HLE) recognizes a specific
microcode binary by signature and substitutes a native reimplementation of
its graphics or audio task — fast, but fragile and per-game, because the N64
let every studio ship custom microcode. Low-Level Emulation (LLE) actually
*executes* the RSP instruction stream (scalar + vector ISA) and feeds the
resulting RDP command list to a faithful rasterizer. The accuracy bar set by
the reference projects — **ares**, **CEN64**, **Gopher64**, and
**ParaLLEl-RDP** — is unambiguously LLE: it is the only path that reproduces
custom microcode, mid-frame coprocessor effects, and the bit-exact
framebuffer that test ROMs demand
([Emulation General Wiki, N64 emulators](https://emulation.gametechwiki.com/index.php/Nintendo_64_emulators),
[ParaLLEl-RDP](https://github.com/Themaister/parallel-rdp)). **RustyN64 should
commit to LLE RSP + LLE RDP**, with HLE reserved (if at all) as an optional,
clearly-flagged fast path — mirroring how RustyNES treats accuracy as the
contract and convenience features as additive opt-ins.

The principal engineering challenges are: (1) an LLE RSP that correctly
models the vector unit's saturation, accumulator, reciprocal-ROM, and the
SU/VU dual-issue pipeline; (2) an LLE RDP that is bit-exact for span
coverage, anti-aliasing, dither, color-combine and blend; (3) **cycle-level
co-scheduling** of CPU + RSP + RDP + DMA on a shared bus, which is what most
plugin-based emulators get wrong; (4) VR4300 fidelity — the 64-bit MIPS III
core, the 32-entry TLB, the cache model, and FPU edge cases; and (5) the
**timing of the eight interfaces** (DMA durations, interrupt latency, VI
scan-out cadence) that games rely on more than they should. The closed-form
specification for all of this exists: **n64-systemtest** (Rust, hardware-
verified), **Dillonb's** test corpus, and **PeterLemon's** bare-metal tests
form a self-validating oracle analogous to blargg/kevtris for the NES
([lemmy-64/n64-systemtest](https://github.com/lemmy-64/n64-systemtest),
[PeterLemon/N64](https://github.com/PeterLemon/N64)).

## Scope and goals

### In scope

- **VR4300 CPU core** — MIPS III ISA, 64-bit, 5-stage pipeline, COP0
  (system control / TLB / exceptions), COP1 (FPU), caches, SysAD bus model.
- **RCP** — the eight sub-interfaces (SP, DP, MI, VI, AI, PI, RI, SI) and
  their registers, DMA engines, and interrupt wiring.
- **RSP (LLE)** — scalar unit (MIPS subset) + vector unit (COP2, 8x16-bit
  SIMD), DMEM/IMEM, the SP DMA engine, microcode upload, run/halt.
- **RDP (LLE)** — triangle/span rasterization, texture/TMEM, color combiner,
  blender, Z-buffer, framebuffer write into RDRAM, the four cycle types.
- **VI scan-out** — reading RDRAM, anti-aliasing / divot / de-dither,
  scaling, NTSC/PAL cadence, DAC output.
- **AI audio** — the DAC-only AI fed by RSP-synthesized samples via DMA.
- **PI / PIF / CIC / SI** — cartridge DMA, boot/lockout, controller I/O,
  cartridge save backends (EEPROM 4k/16k, SRAM, FlashRAM, Controller Pak).
- **RDRAM** unified memory (4 MB + 4 MB Expansion Pak) and the 9th bit.
- **ROM formats** (.z64 / .n64 / .v64), the header, and the boot sequence.
- **Region timing** (NTSC / PAL / Dendy-equivalent) as data, not a fork.

### Out of scope (and why)

- **64DD** disk peripheral — Japan-only, tiny library; defer as later work.
- **HLE graphics/audio plugins** — explicitly a non-goal for the accuracy
  core; may be added later as an optional speed path, never the default.
- **Per-GPU shader backends beyond a reference renderer** — the first target
  is a correct software/compute LLE RDP, not maximal upscaling.

### Success criteria

1. **n64-systemtest** passes (CPU, COP0, TLB, RSP categories) with the same
   self-reported "Failed: 0" a real console produces
   ([n64-systemtest](https://github.com/lemmy-64/n64-systemtest)).
2. The **LLE RDP** is bit-exact against the Angrylion-Plus reference on the
   ParaLLEl-RDP conformance fuzz suite (~150 tests)
   ([ParaLLEl-RDP](https://github.com/Themaister/parallel-rdp)).
3. **Determinism** holds: same ROM + input + seed yields a bit-identical
   framebuffer and audio stream (the RustyNES contract, carried forward).
4. Commercial titles using **custom microcode** (e.g. Factor 5 / Rare audio
   and graphics microcode) render correctly *without* per-game HLE.

## Background and context

The N64 is unusual among fifth-generation consoles in three ways that shape
emulation. First, **unified memory**: there is no separate VRAM. CPU, RSP,
and RDP all read and write the same **RDRAM** pool over a narrow, very high-
clock Rambus channel, so memory contention and DMA timing are first-class
emulation concerns, not afterthoughts
([N64brew RDRAM](https://n64brew.dev/wiki/RDRAM)). Second, **programmability**:
the graphics/audio coprocessor (RSP) is a general MIPS core running software
("microcode") rather than fixed silicon, so two N64 games can use entirely
different rendering pipelines — which is exactly why HLE is brittle and LLE
is the accuracy bar ([Dillonb n64-resources/rsp.md](https://github.com/Dillonb/n64-resources/blob/master/rsp.md)).
Third, **derived clocks from one timebase**: the CPU and RCP run at a fixed
ratio off a shared system clock, so a faithful scheduler must advance all
engines in lockstep against that timebase rather than running them as
independent threads with periodic resync.

The whole machine is driven from a **14.31818 MHz** master crystal — the
NTSC colour-burst reference frequency — from which internal PLLs synthesize
the higher clocks ([Copetti](https://www.copetti.org/writings/consoles/nintendo-64/),
[RetroTech N64 troubleshooting](https://wiki.retrotechcollection.com/Nintendo_64_Troubleshooting_Guide)).
The two numbers that matter to the scheduler are **93.75 MHz** (VR4300) and
**62.5 MHz** (RCP) — a **3:2** ratio (the CPU runs 1.5x the RCP clock). That
ratio is the N64's analogue of the NES's "PPU is the master clock, CPU every
third dot" relationship, and it is the seed of the recommended scheduler
design below.

A competent NES-emulator engineer (the RustyNES author) already has the
right instincts: lockstep scheduling, a bus that owns mutable state,
determinism as a contract, and test ROMs as the spec. The N64 raises the
stakes because there are *three* compute engines (CPU, RSP, RDP) instead of
two (CPU, PPU), the coprocessors are programmable, and the ISA is full 64-bit
MIPS III with a TLB and an FPU. The rest of this report grounds each of those
deltas in primary sources.

## Technical deep-dive

### 1. VR4300 CPU (NEC VR4300 / MIPS R4300i, MIPS III ISA)

**Identity and clock.** The CPU is the **NEC VR4300** (`uPD30200`), a
licensed variant of the MIPS **R4300i**, itself a low-cost derivative of the
MIPS R4200/R4000 line. It implements the **MIPS III** 64-bit instruction set
and runs at **93.75 MHz**
([N64brew VR4300](https://n64brew.dev/wiki/VR4300),
[Copetti](https://www.copetti.org/writings/consoles/nintendo-64/)).
The R4300i datasheet specifies that the **PClock-to-MasterClock ratio** is
set by the `DivMode(1:0)` pins, giving ratios of **1:1, 1.5:1, 2:1, or 3:1**;
the N64 uses **1.5:1**, so with the RCP/system clock at 62.5 MHz the pipeline
clock is 93.75 MHz, and the system interface clock equals the MasterClock.
The chip uses a PLL to minimize input/internal clock skew
([R4300i Data Sheet, Rev 0.3](https://datasheets.chipdb.org/MIPS/R4300i_datasheet.pdf)).

**Pipeline.** Classic MIPS **5-stage** in-order scalar pipeline:
IF (instruction fetch), RF (register fetch/decode), EX (execute),
DF (data fetch / memory), WB (write-back). A single delay slot follows
branches and loads, per the MIPS architecture
([R4300i Data Sheet](https://datasheets.chipdb.org/MIPS/R4300i_datasheet.pdf),
[Copetti](https://www.copetti.org/writings/consoles/nintendo-64/)).

**Caches.** On-chip L1 totalling **24 KB**: a **16 KB instruction cache**
and an **8 KB data cache** (the data cache is write-back). They are
direct-mapped with short lines; the emulator must model cache coherency
against DMA (cart and RSP DMA writes can land in RDRAM behind the cache,
which games flush/invalidate explicitly)
([Copetti](https://www.copetti.org/writings/consoles/nintendo-64/),
[N64brew VR4300](https://n64brew.dev/wiki/VR4300)).

**COP0 (System Control).** The MMU/TLB and exception machinery live in COP0.
The **TLB has 32 entries**, each mapping a pair of pages with selectable page
sizes from **4 KB up to 16 MB**. COP0 holds `Status`, `Cause`, `EPC`,
`BadVAddr`, `EntryHi`/`EntryLo0/1`, `PageMask`, `Index`, `Random`, `Count`,
`Compare`, `Config`, and others. Crucially, several COP0 registers
(`EntryHi`, `BadVAddr`) are genuinely **64-bit**, which n64-systemtest
verifies explicitly
([n64-systemtest README](https://github.com/lemmy-64/n64-systemtest),
[n64.readthedocs MMU/exceptions](https://n64.readthedocs.io/)).

**COP1 (FPU).** The VR4300 contains an FPU exposed as **CP1**, with **32
floating-point registers** supporting IEEE-754 32-bit and 64-bit operations.
The R4300i datasheet notes the FPU "is identified as a coprocessor (CP1)
despite not being one." FPU edge cases (NaN handling, the unimplemented-
operation exception, FR-bit register-file aliasing in 32 vs 64-bit mode) are
common emulator bugs and are exercised by the test suites
([R4300i Data Sheet](https://datasheets.chipdb.org/MIPS/R4300i_datasheet.pdf)).

**SysAD bus.** The CPU talks to the RCP over the **SysAD** system interface —
a 32-bit multiplexed address/data bus (the R4300i is a cost-reduced part with
a narrower external bus than the full R4000) running at the system/MasterClock
frequency. Every CPU memory access to anything outside its caches becomes a
SysAD transaction that the RCP arbitrates against RSP and RDP traffic
([R4300i Data Sheet](https://datasheets.chipdb.org/MIPS/R4300i_datasheet.pdf)).

**Virtual address space.** The CPU uses the standard MIPS segment layout:
**KSEG0** (`0x80000000`–`0x9FFFFFFF`, cached, direct-mapped) and **KSEG1**
(`0xA0000000`–`0xBFFFFFFF`, uncached, direct-mapped) cover the physical map;
KUSEG/KSSEG/KSEG3 go through the TLB. Hardware registers are typically
accessed through KSEG1 (uncached)
([n64.readthedocs memory map](https://n64.readthedocs.io/)).

**Interrupts.** COP0 `Cause.IP[7:0]` versus `Status.IM[7:0]`: an interrupt is
serviced only when a pending bit and its mask bit are both set, and
`Status.IE=1`, `EXL=0`, `ERL=0`. One of those IP bits is wired to the RCP's
**MI** aggregate interrupt (see MI below); others are the timer (Count/Compare)
and software interrupts
([n64.readthedocs interrupts](https://n64.readthedocs.io/)).

### 2. The RCP and its eight interfaces (SP / DP / MI / VI / AI / PI / RI / SI)

The **Reality Coprocessor** is the central ASIC at **62.5 MHz**. It connects
the VR4300, RDRAM, and the cartridge bus, arbitrates all memory traffic, and
exposes eight memory-mapped register blocks in the `0x0400_0000`–`0x048F_FFFF`
range ([N64brew Reality Coprocessor](https://n64brew.dev/wiki/Reality_Coprocessor),
[n64.readthedocs memory map](https://n64.readthedocs.io/)). The CPU drives
every subsystem by writing these registers; each interface raises an
interrupt through the MI.

Memory-mapped layout (physical addresses,
[n64.readthedocs](https://n64.readthedocs.io/)):

| Range (physical) | Block |
|---|---|
| `0x0400_0000`–`0x0400_0FFF` | SP DMEM (4 KB) |
| `0x0400_1000`–`0x0400_1FFF` | SP IMEM (4 KB) |
| `0x0404_0000`–`0x040F_FFFF` | **SP** registers (RSP control + DMA) |
| `0x0410_0000`–`0x041F_FFFF` | **DP** command registers (RDP) |
| `0x0430_0000`–`0x043F_FFFF` | **MI** (MIPS Interface) |
| `0x0440_0000`–`0x044F_FFFF` | **VI** (Video Interface) |
| `0x0450_0000`–`0x045F_FFFF` | **AI** (Audio Interface) |
| `0x0460_0000`–`0x046F_FFFF` | **PI** (Peripheral Interface) |
| `0x0470_0000`–`0x047F_FFFF` | **RI** (RDRAM Interface) |
| `0x0480_0000`–`0x048F_FFFF` | **SI** (Serial Interface) |

- **SP (Signal Processor interface)** — controls the RSP: start/halt via
  `SP_STATUS`, the SP DMA engine (`SP_DMA_*`: DMEM/IMEM ↔ RDRAM), the program
  counter (`SP_PC`), and the broadcast semaphore. Raises the **SP interrupt**.
- **DP (Display Processor interface)** — feeds the RDP command FIFO
  (`DPC_START`/`DPC_END`/`DPC_CURRENT`/`DPC_STATUS`); commands can come from
  RDRAM or from DMEM. Raises the **DP interrupt** on command-buffer drain.
- **MI (MIPS Interface)** — the interrupt aggregator and RCP mode/version
  block (see section 8).
- **VI (Video Interface)** — scans the framebuffer out to video (section 4).
  Raises the **VI interrupt** (typically once per frame at a programmed scanline).
- **AI (Audio Interface)** — DMAs sample buffers to the DAC (section 5).
  Raises the **AI interrupt** when a buffer is consumed.
- **PI (Peripheral Interface)** — cartridge-bus DMA: copies cart ROM/SRAM/
  FlashRAM to/from RDRAM (`PI_DRAM_ADDR`, `PI_CART_ADDR`, `PI_RD_LEN`,
  `PI_WR_LEN`, `PI_STATUS`) plus the four DOM1/DOM2 bus-timing registers.
  Raises the **PI interrupt** on DMA completion (section 6).
- **RI (RDRAM Interface)** — low-level RDRAM controller configuration
  (mode/refresh/latency); used mainly during boot RDRAM initialization.
- **SI (Serial Interface)** — DMAs the 64-byte **PIF RAM** block to/from
  RDRAM, which is how controller polling and Controller-Pak/EEPROM access
  happen (`SI_DRAM_ADDR`, `SI_PIF_AD_RD64B`, `SI_PIF_AD_WR64B`, `SI_STATUS`).
  Raises the **SI interrupt** (section 6).

### 3. The RSP (Reality Signal Processor) — LLE focus

The RSP is a **programmable MIPS core** running at the RCP's 62.5 MHz with a
**custom SIMD coprocessor (COP2)**. It has two onboard 4 KB memories with no
external bus, plus a DMA engine to move code/data to and from RDRAM
([N64brew RSP](https://n64brew.dev/wiki/Reality_Signal_Processor),
[Dillonb rsp.md](https://github.com/Dillonb/n64-resources/blob/master/rsp.md)).

**Scalar Unit (SU).** A stripped-down R4300-style MIPS integer core: standard
32-bit integer ALU/branch/load-store, but a *subset* of the full ISA (no
64-bit ops, no TLB, no most COP0, no multiply/divide of the main CPU's kind).
It addresses only DMEM (4 KB) for data and IMEM (4 KB) for instructions.

**Vector Unit (VU, COP2).** 32 vector registers `VPR[0..31]`, each **128 bits
= 8 lanes of 16-bit** values (the layout mirrors x86 SSE2 EPI16). One vector
op can issue per cycle alongside one scalar op (dual-issue when properly
interleaved), giving "one scalar + one vector opcode per clock" peak
throughput ([Dillonb rsp.md](https://github.com/Dillonb/n64-resources/blob/master/rsp.md)).

Key VU state and quirks an LLE core must reproduce exactly:

- **48-bit-per-lane accumulator** (8 lanes), split into ACCUM_LO / ACCUM_MD /
  ACCUM_HI 16-bit slices read out via `VSAR`.
- **Control registers** `VCO` (carry/overflow, 16-bit), `VCC` (compare flags,
  16-bit), `VCE` (clip equality, 8-bit).
- **Multiply family** `VMULF/VMULU`, `VMACF/VMACU`, `VMUDL/VMUDM/VMUDN/VMUDH`,
  `VMADL/VMADM/VMADN/VMADH` — fixed-point 1.15 fraction multiplies and
  multiply-accumulates with precise rounding/clamping.
- **Saturation** — signed clamps to [−32768, 32767]; unsigned clamps negatives
  to 0 and overflow to 65535 using a 15-bit threshold.
- **Reciprocal/rsqrt** via **ROM lookup tables** (`VRCP`, `VRSQ`, plus the
  H/L two-part variants `VRCPH/VRCPL`, `VRSQH/VRSQL`) with DIV_IN/DIV_OUT
  staging — bit-exact tables are mandatory for geometry correctness.
- **Element/lane quirks** — single-lane ops have a documented hardware anomaly
  where, when `vt_elem(3)==0`, lower bits of the source-lane index are
  replaced by bits of the destination-lane index (operand aliasing).
- **Vector load/store** — `LBV/LSV/LLV/LDV` (sized, unaligned via
  `base + offset*size`), `LQV/SQV` and `LRV/SRV` (left/right 128-bit, like
  LWL/LWR), `LTV/STV` (diagonal transpose across an 8-register group),
  packed `LPV/LUV/SPV/SUV`.

([Dillonb rsp.md](https://github.com/Dillonb/n64-resources/blob/master/rsp.md))

**Microcode.** The RSP runs **microcode** — ordinary MIPS+COP2 assembly — that
the CPU (or a boot loader) DMAs into IMEM/DMEM via the SP DMA engine, then
starts by clearing the HALT bit in `SP_STATUS`. The two broad microcode
families are **graphics** (transform/light/clip a display list, emit RDP
commands) and **audio** (decode ADPCM, mix, resample, emit 16-bit stereo PCM).
Different games and middleware shipped different microcode (Fast3D, F3DEX,
F3DEX2, S2DEX, plus bespoke Factor 5 / Rare / Boss Game Studios variants),
which is precisely why HLE is per-game-fragile
([N64brew RSP](https://n64brew.dev/wiki/Reality_Signal_Processor),
[OSnews, Factor 5 microcode](https://www.osnews.com/story/129733/)).

**LLE vs HLE — the decision.**

- **HLE** detects a known microcode by signature/CRC and runs a native
  reimplementation of its task (e.g. "this is F3DEX2; call my triangle
  pipeline"). It is fast and was the historical norm (Project64 + plugins),
  but it breaks on unknown/custom microcode, mis-renders mid-frame tricks,
  and needs continual per-game maintenance.
- **LLE** runs the actual RSP instruction stream and produces the actual RDP
  command list, then rasterizes those commands. It is the accuracy bar:
  ares, CEN64, Gopher64, and ParaLLEl-(RDP/RSP) are all LLE
  ([Emulation General Wiki](https://emulation.gametechwiki.com/index.php/Nintendo_64_emulators)).

**Recommendation: RustyN64 = LLE RSP (+ LLE RDP).** An HLE path may exist
later behind an off-by-default feature flag for speed, but the *core contract*
is LLE. **What LLE means for the scheduler:** because the RSP shares RDRAM and
the SysAD/RCP bus with the CPU, and because games synchronize CPU code against
RSP progress (semaphores, SP_STATUS polls, DP interrupts), the RSP must be
**co-scheduled cycle-accurately with the VR4300 and RDP** — not run to
completion in a burst at frame boundaries. The RSP's single-threaded scalar
throughput is, with ParaLLEl-class RDP, the actual emulation bottleneck, which
is why production LLE emulators add an **RSP dynamic recompiler**
([Libretro, ParaLLEl RSP dynarec](https://www.libretro.com/index.php/parallel-n64-with-parallel-rsp-dynarec-release-fast-and-accurate-n64-emulation/)).

### 4. The RDP (Reality Display Processor) and VI scan-out

**RDP role.** The RDP is the fixed-function rasterizer. It consumes a command
stream (from RDRAM or DMEM, fed via the DP interface FIFO) and writes pixels
into a framebuffer in RDRAM. Its pipeline blocks are: **triangle/edge setup →
span/edge walking → texture fetch (TMEM) → texture filter → color combiner →
blender → Z-buffer / coverage write**
([N64brew Reality Display Processor](https://n64brew.dev/wiki/Reality_Display_Processor),
[RetroReversing RDP](https://www.retroreversing.com/n64rdp)).

- **TMEM** — **4 KB** of texture memory holding up to 8 tiles; when a TLUT
  (palette) is used, the first 2 KB of TMEM is the lookup table. Formats:
  RGBA (16/32-bit), IA, I, CI (color-indexed) at 4/8/16/32 bits-per-texel.
- **Color combiner** — programmable add/sub/multiply of color/alpha inputs
  (texture, shade, primitive, environment, etc.) across one or two stages.
- **Blender** — translucency, fog, anti-aliasing edge blend, and dithering.
- **Z-buffer** — depth test/write against a Z buffer in RDRAM.
- **Cycle types** — the RDP runs in **1-cycle**, **2-cycle**, **copy**, or
  **fill** modes; copy/fill are fast paths for rectangle blits and clears,
  1-/2-cycle are the full pipeline (2-cycle enables a second combiner/blender
  pass). Reproducing per-mode behaviour is required for bit-exactness
  ([RetroReversing RDP](https://www.retroreversing.com/n64rdp)).

**Framebuffer + the 9th bit.** RDRAM stores **9 bits per byte**; the extra
"hidden" bit is reserved for the RDP/VI and stores per-pixel **coverage**
(sub-pixel AA) in the color buffer. The VI later uses coverage to blend edges
([N64brew RDRAM Interface](https://n64brew.dev/wiki/RDRAM_Interface),
[ConsoleMods, removing blur](https://consolemods.org/wiki/N64:Removing_Blur)).

**VI scan-out.** The **Video Interface** reads the framebuffer from RDRAM at
`VI_ORIGIN`, in the format described by `VI_CONTROL`/`VI_STATUS` (bpp, AA mode,
gamma, dither, divot enable), scales it, applies its post-filters, and streams
to the video DAC. Its filters are subtle and game-visible:

- **Anti-aliasing** uses the per-pixel coverage bit to blend silhouette edges.
- **Divot filter** removes 1-pixel artifacts AA leaves on silhouette edges by
  taking the median of three neighbours
  ([N64brew Video Interface](https://n64brew.dev/wiki/Video_Interface)).
- **De-dither** examines 8 neighbours to undo the RDP's ordered dither (tuned
  for the "magic square" dither pattern); applied only on full-coverage pixels.
- Output is **640x480** or **320x240** (NTSC), up to 32-bit color
  ([Copetti](https://www.copetti.org/writings/consoles/nintendo-64/)).

**LLE RDP reality (ParaLLEl-RDP).** ParaLLEl-RDP is "a low-level Vulkan compute
emulation of the N64 RDP" targeting **bit-exact** parity with the
Angrylion-Plus software reference. Key facts for RustyN64:

- It is validated by a conformance suite of **~150 fuzz tests** that generate
  RDP command streams and compare fixed-point outputs; "to pass, we must get an
  exact match." The VI was reimplemented to be **bit-exact with Angrylion** too
  ([ParaLLEl-RDP](https://github.com/Themaister/parallel-rdp),
  [Libretro ParaLLEl-RDP rewrite](https://www.libretro.com/index.php/reviving-and-rewriting-parallel-rdp-fast-and-accurate-low-level-n64-rdp-emulation/)).
- "Serial C code will get you nowhere on a GPU" — it uses tile-based binning,
  ubershaders with Vulkan specialization constants, async-compute queues, and
  imports RDRAM directly as an SSBO (`VK_EXT_external_memory_host`) so the GPU
  reads/writes the emulated RDRAM over the bus
  ([Libretro ParaLLEl-RDP rewrite](https://www.libretro.com/index.php/reviving-and-rewriting-parallel-rdp-fast-and-accurate-low-level-n64-rdp-emulation/)).
- Performance: ~**0.2 ms/frame** on a GTX 1660 Ti, **2000–5000 VI/s** on
  mid-range GPUs — so the RDP is *not* the bottleneck once it is on the GPU;
  the single-threaded **RSP + CPU** are
  ([ParaLLEl-RDP](https://github.com/Themaister/parallel-rdp)).
- The hardest part is **shared-RDRAM coherency** — CPU/RSP can read pixels the
  RDP just wrote (framebuffer effects), which HLE plugins fudge with
  heuristics. **Recommendation:** RustyN64 should ship a pure-Rust **software
  LLE RDP** as the always-correct reference renderer (the analogue of
  Angrylion), and treat a compute-shader backend (wgpu) as an optional
  accelerated path validated *against* that reference, not the other way round.

### 5. The AI audio path

The **Audio Interface (AI)** is deliberately dumb: a DAC fed by DMA. It "does
absolutely no conversion on the samples"; all decode/mix/resample is done by
the **CPU or, in practice, the RSP audio microcode**, which writes finished
**16-bit signed stereo (L/R) PCM** into an RDRAM buffer. The CPU points the AI
at that buffer and length; the AI streams it to the DAC and raises the **AI
interrupt** when done, double-buffering for continuous output
([N64brew Audio Interface](https://n64brew.dev/wiki/Audio_Interface)).

**Sample rate** is derived: `sample_rate = video_clock / (DAC_rate + 1)`. For
example a `AI_DACRATE` of **1103** yields **44136 Hz** on NTSC (close to CD-
quality 44.1 kHz); games typically run 22–32 kHz to save RSP time
([N64brew Audio Interface](https://n64brew.dev/wiki/Audio_Interface)). The
audio microcode commonly decodes **ADPCM** sample banks and applies envelopes/
effects before mixing ([Copetti](https://www.copetti.org/writings/consoles/nintendo-64/)).

For an LLE emulator the audio path falls out for free: the RSP audio microcode
runs on the same LLE RSP core, so RustyN64 emulates audio correctly *by
emulating the RSP* — no per-game audio HLE needed. The frontend then resamples
the AI output stream to the host rate (a frontend stage, kept out of the
deterministic core, exactly as RustyNES keeps DRC/run-ahead in the frontend).

### 6. PI / PIF / CIC boot, SI, and the cartridge save model

**Boot sequence (IPL stages).** Power-on runs a three-stage Initial Program
Loader ([Copetti](https://www.copetti.org/writings/consoles/nintendo-64/),
[RetroReversing boot code](https://www.retroreversing.com/n64bootcode)):

1. **IPL1** — in the **PIF-NUS** internal boot ROM (`0x1FC0_0000`): brings up
   the CPU, the PI, and the RCP.
2. **IPL2** — runs in RSP memory; participates in validating the cartridge
   against the CIC.
3. **IPL3** — the **bootcode in the cartridge ROM** at offset **`0x40`, length
   4032 bytes**; it initializes RDRAM, computes a checksum over the first
   1 MB, and jumps to the game's entry point (it executes from `0xA4000040`).
   The standard CIC-NUS-6102/7101 bootcode (MD5 `2dacea29...`) covers ~88% of
   games ([RetroReversing boot code](https://www.retroreversing.com/n64bootcode)).

**PIF + CIC lockout.** Every cartridge carries a **CIC-NUS** lockout chip. On
insertion it is wired to the in-console **PIF-NUS** over two lines; the two run
a continuous handshake (seed/checksum), and the PIF can **halt the CPU** if the
check fails. Variants: NTSC **CIC-6101 / 6102 / 6103 / 6105 / 6106** (PAL
**7101–7106**); 6102/7101 is by far the most common. **6103/6106 change the
RAM entry point** (add `0x100000` / `0x200000` to the header entry point
respectively), and 6105 uses a different challenge protocol
([N64brew CIC-NUS](https://n64brew.dev/wiki/CIC-NUS),
[RetroReversing boot code](https://www.retroreversing.com/n64bootcode),
[micro-64 game CIC list](http://micro-64.com/database/gamecic.shtml)). An
emulator either runs the real PIF/CIC ROMs or, more commonly, **stubs the boot**
(load ROM, set up the RDRAM/CPU state IPL3 would, apply the per-CIC entry-point
adjustment, and seed the PIF RAM CIC-result byte the game polls).

**SI / controllers / PIF RAM.** Controller polling and accessory I/O go through
**64 bytes of PIF RAM** at the top of the PIF block. The CPU fills PIF RAM with
a command block (per-port commands: read controller, read/write Controller Pak,
read/write EEPROM), triggers an **SI DMA** (`SI_PIF_AD_RD64B`/`WR64B`), and the
PIF executes the joybus transactions and writes results back; the **SI
interrupt** signals completion
([n64.readthedocs SI](https://n64.readthedocs.io/),
[mikeryan/n64dev pif.S](https://github.com/mikeryan/n64dev/blob/master/src/boot/pif.S)).

**Save backends.** Four cartridge save technologies, detected per-game
([micro-64 save list](http://micro-64.com/database/gamesave.shtml),
[N64 Squid, EEPROM](https://n64squid.com/homebrew/libdragon/saving/eeprom/),
[Christopher Bonhage SaveTest-N64](https://christopherbonhage.com/SaveTest-N64/)):

| Type | Size | Access path | Notes |
|---|---|---|---|
| **EEPROM 4kbit** | 512 bytes | joybus via PIF/SI | battery-free serial EEPROM |
| **EEPROM 16kbit** | 2048 bytes | joybus via PIF/SI | same path, larger |
| **SRAM** | 32 KB (some 96 KB) | PI bus (DOM2) | needs battery; dies in 15–25 yr |
| **FlashRAM** | 128 KB | PI bus (DOM2) | command-driven flash, battery-free |
| **Controller Pak** | 32 KB | joybus via PIF/SI | external memory card, not cart-resident |

EEPROM can coexist with SRAM or FlashRAM, but **SRAM and FlashRAM cannot
coexist** ([micro-64 save list](http://micro-64.com/database/gamesave.shtml)).
RustyN64 must implement EEPROM/Controller-Pak as joybus devices behind the SI/
PIF path, and SRAM/FlashRAM as PI-bus DOM2 devices, with FlashRAM modelling its
small command state machine (erase/write/status).

**Cartridge ROM format.** The ROM begins with a header whose first 4 bytes
encode both the PI bus config and the **byte order** ([n64dev rom formats](http://n64dev.org/romformats.html),
[Ultimate Pop Culture, N64 ROM formats](https://ultimatepopculture.fandom.com/wiki/List_of_Nintendo_64_ROM_file_formats)):

| Magic (first 4 bytes) | Format | Byte order |
|---|---|---|
| `80 37 12 40` | **.z64** | big-endian (native "proper" order) |
| `37 80 40 12` | **.v64** | byte-swapped (16-bit) |
| `40 12 37 80` | **.n64** | little-endian (32-bit-swapped) |

Header fields (offsets, big-endian/.z64): `0x00` PI/clock/endian dword,
`0x04` **clock rate**, `0x08` **boot address / entry PC**, `0x0C` release,
`0x10` **CRC1**, `0x14` **CRC2**, `0x20` internal game **title** (20 bytes),
`0x3B` cartridge ID / region. **File extension is unreliable**, so RustyN64
must sniff the first 4 bytes and byte-swap to canonical .z64 in memory
([Ultimate Pop Culture, N64 ROM formats](https://ultimatepopculture.fandom.com/wiki/List_of_Nintendo_64_ROM_file_formats)).

### 7. The test-ROM oracle

The N64 has a mature, self-validating accuracy oracle — the analogue of
blargg/kevtris/AccuracyCoin for the NES.

- **n64-systemtest** (lemmy-64/Dillonb, **Rust**). The flagship suite: it
  "tests a wide variety of N64 features, from common to hardware quirks," runs
  fast, and is **self-judging** — "n64-systemtest itself decides whether it
  failed or succeeded. No need to compare images," finishing with a line like
  "Done! Tests: 262. Failed: 0." Coverage spans CPU instructions, **COP0**
  access (MFC0/DMFC0/MTC0/DMTC0, 64-bit register behaviour), atomics
  (LL/LD/SC/SCD), **exceptions** (ADD/DADD overflow, unaligned, TRAP/BREAK/
  SYSCALL), the **TLB**, multi-width memory access (8/16/32/64-bit) to RAM/ROM/
  SPMEM/PIF, COP0 hazards/timing, and **RSP** behaviour. It is **hardware-
  verified** on real consoles, so its expectations are ground truth
  ([n64-systemtest](https://github.com/lemmy-64/n64-systemtest)).
- **Dillonb's tests** (`Dillonb/n64-tests`) and the **n64-resources** docs —
  targeted CPU/RSP behaviour, with the RSP implementation hardware-verified
  over an EverDrive 64 USB link using `rsp-recorder`
  ([Dillonb/n64](https://github.com/Dillonb/n64),
  [Dillonb n64-resources](https://github.com/Dillonb/n64-resources/blob/master/rsp.md)).
- **PeterLemon/N64** ("krom") — a large corpus of bare-metal MIPS/RSP/RDP
  demos and tests (CPU, RSP, RDP, audio) built with the ARM9 `bass` assembler;
  the de-facto visual/behavioural reference for many edge cases
  ([PeterLemon/N64](https://github.com/PeterLemon/N64)).
- **ParaLLEl-RDP conformance suite** — the ~150-test bit-exact RDP fuzzer
  described in section 4; the spec for RDP rasterization
  ([ParaLLEl-RDP](https://github.com/Themaister/parallel-rdp)).

**Oracle strategy (RustyNES-style).** As with the NES, *when the docs and a
passing test ROM disagree, the ROM wins.* RustyN64 should treat
n64-systemtest's "Failed: 0" as the strict CPU/COP0/TLB/RSP gate, the
ParaLLEl-RDP fuzz suite as the RDP gate, and PeterLemon/Dillonb ROMs +
commercial screenshots as the regression corpus — all behind a `test-roms`
feature with committed CC0/homebrew ROMs and gitignored commercial dumps.

### 8. RDRAM, unified memory, and the MI interrupt model

**RDRAM.** The N64 uses **Rambus RDRAM** as a single unified pool — base
**4 MB** (`0x0000_0000`–`0x003F_FFFF`), expandable to **8 MB** with the
**Expansion Pak** (`0x0040_0000`–`0x007F_FFFF`); when no Expansion Pak is
present a **Jumper Pak** provides bus termination. RDRAM is **9 bits per byte**;
the 9th bit is RDP/VI-only (AA coverage / Z hidden bits). Titles like *Donkey
Kong 64* and *Majora's Mask* require the Expansion Pak. The RDRAM channel is
very high clock (commonly cited ~250 MHz data with the RCP at 1/4 of the
RAC/RDRAM clock band), and its narrow width makes **bus contention and DMA
timing** a real accuracy factor that register-transfer emulators (CEN64) model
explicitly ([N64brew RDRAM](https://n64brew.dev/wiki/RDRAM),
[N64brew RDRAM Interface](https://n64brew.dev/wiki/RDRAM_Interface),
[Copetti](https://www.copetti.org/writings/consoles/nintendo-64/)). The **RI**
interface configures the RDRAM controller (mode/refresh/latency) at boot.

**MI (MIPS Interface) interrupts.** The MI aggregates the six RCP interrupt
sources into the single CPU interrupt line. `MI_INTR_REG` (read-only,
`0x0430_0008`) holds the pending bits; `MI_INTR_MASK_REG` (`0x0430_000C`)
masks them; the CPU sees an interrupt when
`(MI_INTR_REG & MI_INTR_MASK_REG) != 0`. The six bits are
([n64.readthedocs MI](https://n64.readthedocs.io/)):

| Bit | Source |
|---|---|
| 0 | **SP** (RSP) |
| 1 | **SI** (serial / controller) |
| 2 | **AI** (audio buffer done) |
| 3 | **VI** (vertical / scanline) |
| 4 | **PI** (cart DMA done) |
| 5 | **DP** (RDP command buffer drained) |

`MI_MODE_REG` (`0x0430_0000`) also clears the DP interrupt (bit 11). Each
sub-interface raises/lowers its bit; the MI ORs them into one CPU IP line — so
RustyN64's interrupt model is: subsystem sets MI bit → MI ANDs with mask → CPU
COP0 `Cause.IP` set → serviced if `Status.IM`/IE/EXL/ERL allow
([n64.readthedocs interrupts](https://n64.readthedocs.io/)).

### 9. Regions (NTSC / PAL) as data

Region differences are **timing/data**, not separate cores
([Copetti](https://www.copetti.org/writings/consoles/nintendo-64/),
[RetroTech N64 troubleshooting](https://wiki.retrotechcollection.com/Nintendo_64_Troubleshooting_Guide),
[PiMyRetro PAL→NTSC](https://www.pimyretro.org/converting-a-pal-nintendo-64-motherboard-to-ntsc/)):

- **NTSC** — 60 Hz field rate, ~262.5 lines/field, master crystal
  **14.31818 MHz**, colour-burst 3.579545 MHz (crystal/4); default VI cadence
  produces ~60 frames/s.
- **PAL** — 50 Hz field rate, ~312.5 lines/field, different VI timing tables
  and a PAL-tuned clock multiply; CPU/RCP core clocks are effectively the same
  93.75/62.5 MHz, but the VI vertical/horizontal counters and the AI video
  clock divisor differ, so audio sample rate and frame pacing differ by region.

The emulator should carry per-region constants (field rate, lines, VI timing,
AI video-clock) as a small data table selected from the ROM's region byte, so a
single core runs all regions deterministically (the RustyNES "region as data"
model).

## State of the art / prior art

| Project | Lang | RSP | RDP | Approach / known for |
|---|---|---|---|---|
| **ares** (n64 core) | C++ | LLE | LLE (incl. ParaLLEl-RDP) | Cycle-accuracy oriented, modern, clean; "best/most accurate" but heavier and lower *compatibility* than mature emulators — known DK64 issues. ([Emulation General](https://emulation.gametechwiki.com/index.php/Nintendo_64_emulators), [Libretro forum](https://forums.libretro.com/t/ares-is-becoming-the-best-n64-emulator/42175)) |
| **CEN64** | C | LLE | LLE (software) | Register-transfer-level accuracy; "emulates the hardware down to the RTL." Not full-speed, but used to validate ROMs *in lieu of hardware*. The accuracy north star. ([Emulation General](https://emulation.gametechwiki.com/index.php/Nintendo_64_emulators)) |
| **Gopher64** | **Rust** | LLE | LLE (ParaLLEl-RDP) | Successor to simple64, written in Rust by the same author; adapts code from mupen64plus and ares; accuracy-focused (esp. DMA), already runs commercial games at decent speed. The closest existing analogue to RustyN64. ([gopher64](https://github.com/gopher64/gopher64), [Emulation General, simple64](https://emulation.gametechwiki.com/index.php/Simple64)) |
| **simple64** | C/C++ | LLE | LLE (ParaLLEl-RDP) | Cached-interpreter core, accuracy-focused DMA; **archived Feb 2025**, superseded by Gopher64. ([Emulation General, simple64](https://emulation.gametechwiki.com/index.php/Simple64)) |
| **mupen64plus** | C | HLE+LLE plugins | plugins (incl. ParaLLEl-RDP, GLideN64) | The plugin ecosystem standard; LLE optional. Broad compatibility, plugin-dependent accuracy. ([Emulation General](https://emulation.gametechwiki.com/index.php/Nintendo_64_emulators)) |
| **Dillonb/n64 (dgb-n64)** | C | LLE (+RSP dynarec) | LLE (ParaLLEl-RDP, Vulkan) | Experimental low-level emulator with a CPU recompiler; pairs with n64-systemtest. ([Dillonb/n64](https://github.com/Dillonb/n64)) |
| **ParaLLEl-RDP / ParaLLEl-RSP** | C++ | LLE RSP dynarec | LLE RDP (Vulkan compute) | The reference LLE RDP; bit-exact vs Angrylion; reused by ares/simple64/Gopher64/mupen64plus. ([ParaLLEl-RDP](https://github.com/Themaister/parallel-rdp)) |
| **Angrylion-Plus** | C | n/a | LLE RDP (software) | The bit-exact *software* RDP reference everything else is graded against. |
| **Project64** | C++ | HLE (plugins) | HLE (plugins) | Fast, broad compatibility, low accuracy ceiling; the HLE archetype RustyN64 deliberately diverges from. |

Takeaways for RustyN64: (a) the field has **converged on LLE** for accuracy;
(b) **Gopher64 proves a Rust LLE N64 is viable** and is the closest peer;
(c) **ParaLLEl-RDP/Angrylion define the RDP spec**; (d) **CEN64** is the RTL-
level correctness yardstick; (e) the open accuracy gaps are bus/DMA timing and
the long tail of custom microcode — both solved only by LLE + a good scheduler.

## Principal engineering challenges

1. **LLE RSP vector unit correctness.** The VU's saturation rules, 48-bit
   accumulator slicing, reciprocal/rsqrt ROM tables, clip/select semantics
   (VCH/VCL/VCR/VCE), and the single-lane element-aliasing quirk must be
   bit-exact or geometry/audio drift. *Mitigation:* implement straight from
   Dillonb's rsp.md + n64-systemtest's RSP category, table-drive the recip ROM,
   and pin each behaviour against the suite before optimizing. ([Dillonb rsp.md](https://github.com/Dillonb/n64-resources/blob/master/rsp.md))
2. **LLE RDP bit-exactness.** Span coverage, AA, ordered dither, the 1-/2-cycle/
   copy/fill modes, color-combine, and Z all have subtle fixed-point rules;
   "to pass we must get an exact match" against Angrylion. *Mitigation:* build a
   pure-Rust software RDP as the reference first; gate it on the ParaLLEl-RDP
   fuzz suite; only then add a wgpu-compute accelerator validated against it.
   ([ParaLLEl-RDP](https://github.com/Themaister/parallel-rdp))
3. **Cycle-accurate co-scheduling of CPU + RSP + RDP + DMA on shared RDRAM.**
   The hardest *systemic* problem: three engines and several DMA channels
   contend for one bus, and games synchronize on the resulting timing
   (SP_STATUS polls, DP/SP/PI/SI interrupts, framebuffer read-back). Running
   the RSP "to completion" per frame breaks mid-frame effects. *Mitigation:* a
   single fractional-master-clock scheduler advancing all engines in lockstep
   (see Architecture options); model DMA as time-consuming, not instantaneous.
4. **VR4300 fidelity.** 64-bit MIPS III, the **32-entry TLB** with variable
   page sizes, the cache + DMA coherency model, and FPU edge cases
   (NaN/unimplemented-op, FR-bit aliasing) are all bug-prone. *Mitigation:*
   n64-systemtest's CPU/COP0/TLB categories as the strict gate; model caches
   explicitly because games rely on flush/invalidate semantics. ([n64-systemtest](https://github.com/lemmy-64/n64-systemtest))
5. **Interface timing (DMA durations + interrupt latency).** PI/SI/SP/AI DMA
   completion timing and VI scanline-interrupt cadence are timing the CPU code
   waits on; instantaneous DMA desyncs audio and breaks busy-wait loops.
   *Mitigation:* derive DMA durations from byte counts and bus rates; schedule
   the completion interrupt at the correct future cycle, not immediately.
6. **Boot / CIC variants without the real PIF ROM.** Reproducing IPL3 state and
   the per-CIC entry-point offsets (6103 +`0x100000`, 6106 +`0x200000`) and
   PIF result byte so games boot from a stub. *Mitigation:* HLE the boot
   (well-documented), keep an optional real-IPL path. ([RetroReversing boot code](https://www.retroreversing.com/n64bootcode))
7. **Performance under LLE.** With ParaLLEl-class RDP, the **RSP + CPU** are the
   bottleneck; a naive RSP interpreter may not hit 60 fps. *Mitigation:* start
   interpreter-only for correctness, then add an RSP (and CPU) dynarec the way
   ParaLLEl-RSP and dgb-n64 do — keeping the interpreter as the determinism
   oracle. ([Libretro ParaLLEl RSP dynarec](https://www.libretro.com/index.php/parallel-n64-with-parallel-rsp-dynarec-release-fast-and-accurate-n64-emulation/))

## Architecture options

### A. HLE coprocessors (rejected for the core)

Detect microcode by signature; reimplement graphics/audio natively. **Pros:**
fast, simple CPU-side. **Cons:** per-game fragility, mid-frame effects wrong,
custom microcode unsupported, perpetual maintenance. **Verdict:** not the core;
acceptable only as an optional, off-by-default speed path layered on top of a
correct LLE core.

### B. LLE coprocessors (recommended)

Execute the RSP instruction stream; rasterize the real RDP command list.
**Pros:** accuracy, custom-microcode support, audio "for free," matches the
test-ROM oracle. **Cons:** more work, slower without a dynarec. **Verdict:**
**the RustyN64 core.** Ship a software LLE RDP reference renderer; add a wgpu-
compute RDP and RSP/CPU dynarec later, each validated against the interpreter.

### C. Scheduler: integer-lockstep vs fractional master clock

- **Integer lockstep** (the RustyNES NES model): pick the smallest engine as
  the master and advance others in whole-number ratios. The N64's clean **3:2**
  CPU:RCP ratio *could* be expressed this way, but RDRAM, the RDP, the DMA
  engines, the VI/AI video-clock divisors, and PAL timing introduce **non-
  integer** relationships that integer lockstep handles only with awkward
  per-quirk fudges.
- **Fractional master clock** (the Mesen2 / ares model, and the same direction
  RustyNES v2.0.0 "Timebase" is heading): pick a fine master-clock unit and
  advance every engine by its fractional cycle cost against that unit, with a
  priority-queue / event-driven dispatch for DMA completions and interrupts.
  **Recommended for RustyN64.** The N64's three asynchronous engines plus
  multiple timed DMA channels and region-dependent VI/AI divisors make a
  fractional, event-aware scheduler the natural and accurate fit; it is also
  what the LLE-RSP-co-scheduling requirement (challenge 3) demands.

**Recommendation:** **B + C-fractional.** LLE RSP + LLE RDP on a fractional-
master-clock, event-driven scheduler, with an interpreter-first core (the
determinism oracle) and dynarec/compute-shader acceleration added afterward as
validated, optional layers.

## External dependencies and integrations

- **wgpu** (or Vulkan via ash) — for an accelerated compute-shader RDP backend
  and for windowed presentation; matches RustyNES's `wgpu` frontend. Optional
  relative to the software reference RDP.
- **ParaLLEl-RDP / Angrylion-Plus** — *as references to study and validate
  against*, not necessarily to link. Angrylion-Plus is GPL; a clean-room Rust
  software RDP avoids license entanglement while using the conformance suite as
  the spec. ([ParaLLEl-RDP](https://github.com/Themaister/parallel-rdp))
- **n64-systemtest / Dillonb tests / PeterLemon ROMs** — test assets; commit
  the freely-redistributable ones, gitignore commercial dumps. ([n64-systemtest](https://github.com/lemmy-64/n64-systemtest))
- **cpal / winit / egui** — audio out, windowing, debugger UI — same stack as
  RustyNES, reusable wholesale.
- **A dynarec backend** (later) — `cranelift` or hand-rolled x86_64/aarch64 for
  CPU + RSP recompilation; the interpreter remains the deterministic fallback.

Licenses to watch: Angrylion-Plus (GPL), ParaLLEl-RDP (per-repo), mupen64plus/
ares (mixed GPL/ISC-ish) — RustyNES is MIT OR Apache-2.0, so prefer clean-room
implementation from primary docs + test ROMs over copying GPL source.

## Standards and compliance

- **Instruction set:** MIPS III (the R4300i implementation), per the
  [R4300i Data Sheet, Rev 0.3](https://datasheets.chipdb.org/MIPS/R4300i_datasheet.pdf)
  and the NEC VR4300 User's Manual.
- **Floating point:** IEEE-754 single/double via COP1.
- **Conformance suites (the "spec"):** **n64-systemtest** (CPU/COP0/TLB/RSP,
  self-judging, hardware-verified), the **ParaLLEl-RDP** ~150-test fuzz suite
  (RDP bit-exactness vs Angrylion), **Dillonb/n64-tests**, and **PeterLemon/N64**
  bare-metal tests. These are the closed-form definition of "accurate."
- **Video standards:** NTSC (60 Hz, 14.31818 MHz crystal, 3.579545 MHz
  colour-burst) and PAL (50 Hz) as data-driven region tables.

## Open questions

1. **Exact RDRAM/bus contention model.** How precisely must CPU/RSP/RDP/DMA
   bus arbitration be modelled for commercial-game correctness vs only
   n64-systemtest? CEN64 goes RTL-deep; ares is lighter. Needs a prototype to
   find the cost/accuracy knee. (Cross-validate CEN64 vs ares behaviour.)
2. **Boot strategy.** Stub IPL3 (HLE boot, documented) vs run real PIF/IPL
   ROMs (redistribution + accuracy). Likely stub-first with an optional real-
   IPL mode — confirm which commercial titles, if any, depend on real-PIF
   timing.
3. **RDP backend ordering.** Ship the pure-Rust software RDP first (always
   correct, the oracle) and add a wgpu-compute RDP later — confirm the software
   RDP can hit interactive speeds at native res, or whether the compute backend
   must come sooner.
4. **Cache-coherency depth.** How exact must the I/D-cache + DMA coherency model
   be? Games explicitly flush/invalidate; quantify which test ROMs gate this.
5. **Region timing tables.** Exact PAL VI/AI divisor values and line counts
   need pinning from n64brew + hardware logs before the region table is frozen.

## Source manifest

### Tier 1 — primary / authoritative

1. [R4300i Data Sheet, Rev 0.3 (April 1997)](https://datasheets.chipdb.org/MIPS/R4300i_datasheet.pdf)
   — MIPS R4300i datasheet: pipeline, caches, DivMode clock ratios, FPU/COP1,
   SysAD. (PDF; binary-encoded, cross-validated via secondary mirrors.)
2. [NEC VR4300/VR4305/VR4310 64-bit Microprocessor User's Manual](https://datasheets.chipdb.org/NEC/Vr-Series/Vr43xx/U10504EJ7V0UMJ1.pdf)
   — the VR4300 family programmer's manual.
3. [SGI R4300 RISC Processor Specification Rev 2.2 (July 1995)](https://ultra64.ca/files/documentation/silicon-graphics/SGI_R4300_RISC_Processor_Specification_REV2.2.pdf)
   — SGI's R4300 spec.
4. [N64brew Wiki — VR4300](https://n64brew.dev/wiki/VR4300) — CPU clock, ISA,
   caches, COP0/COP1, SysAD.
5. [N64brew Wiki — Reality Coprocessor](https://n64brew.dev/wiki/Reality_Coprocessor)
   — RCP role, clock, sub-interfaces.
6. [N64brew Wiki — Reality Signal Processor](https://n64brew.dev/wiki/Reality_Signal_Processor)
   — RSP SU/VU, DMEM/IMEM, DMA, microcode.
7. [N64brew Wiki — Reality Display Processor](https://n64brew.dev/wiki/Reality_Display_Processor)
   — RDP pipeline, TMEM, combiner, blender.
8. [N64brew Wiki — Video Interface](https://n64brew.dev/wiki/Video_Interface)
   — VI scan-out, AA, divot, de-dither, control register.
9. [N64brew Wiki — Audio Interface](https://n64brew.dev/wiki/Audio_Interface)
   — AI DAC, DMA, sample-rate formula.
10. [N64brew Wiki — RDRAM](https://n64brew.dev/wiki/RDRAM) and
    [RDRAM Interface](https://n64brew.dev/wiki/RDRAM_Interface) — unified
    memory, 9th bit, RI.
11. [N64brew Wiki — CIC-NUS](https://n64brew.dev/wiki/CIC-NUS) — lockout chip
    variants and PIF handshake.
12. [n64.readthedocs.io](https://n64.readthedocs.io/) — memory map, MI
    interrupt model + register addresses, exceptions, interface registers.
13. [lemmy-64/n64-systemtest (GitHub)](https://github.com/lemmy-64/n64-systemtest)
    — Rust, hardware-verified self-judging test suite (CPU/COP0/TLB/RSP).
14. [Dillonb/n64-resources — rsp.md](https://github.com/Dillonb/n64-resources/blob/master/rsp.md)
    — detailed RSP SU/VU/COP2/accumulator/instruction reference.
15. [Dillonb/n64 (dgb-n64, GitHub)](https://github.com/Dillonb/n64) — LLE
    emulator with CPU recompiler + ParaLLEl-RDP.
16. [Themaister/parallel-rdp (GitHub)](https://github.com/Themaister/parallel-rdp)
    — LLE Vulkan-compute RDP; bit-exact vs Angrylion; conformance suite.
17. [PeterLemon/N64 (GitHub)](https://github.com/PeterLemon/N64) — bare-metal
    MIPS/RSP/RDP test + demo corpus.
18. [micro-64 — Game CIC list](http://micro-64.com/database/gamecic.shtml) and
    [Game Save Methods list](http://micro-64.com/database/gamesave.shtml) —
    per-game CIC and save-type ground truth.

### Tier 2 — reliable secondary

19. [Copetti — Nintendo 64 Architecture: A Practical Analysis](https://www.copetti.org/writings/consoles/nintendo-64/)
    — thorough, well-sourced architecture overview.
20. [Libretro — Reviving and rewriting paraLLEl-RDP](https://www.libretro.com/index.php/reviving-and-rewriting-parallel-rdp-fast-and-accurate-low-level-n64-rdp-emulation/)
    — author's LLE-RDP design + performance notes.
21. [Libretro — ParaLLEl N64 with ParaLLEl RSP dynarec](https://www.libretro.com/index.php/parallel-n64-with-parallel-rsp-dynarec-release-fast-and-accurate-n64-emulation/)
    — RSP dynarec rationale (RSP/CPU as the bottleneck).
22. [Emulation General Wiki — Nintendo 64 emulators](https://emulation.gametechwiki.com/index.php/Nintendo_64_emulators)
    and [simple64](https://emulation.gametechwiki.com/index.php/Simple64) —
    prior-art accuracy comparison (ares/CEN64/Gopher64/simple64/mupen64plus).
23. [gopher64/gopher64 (GitHub)](https://github.com/gopher64/gopher64) — Rust
    LLE N64 emulator, the closest peer to RustyN64.
24. [RetroReversing — N64 RDP](https://www.retroreversing.com/n64rdp),
    [N64 RSP](https://www.retroreversing.com/n64rsp),
    [N64 Boot Code](https://www.retroreversing.com/n64bootcode) — subsystem
    walkthroughs.
25. [Ultimate Pop Culture — List of N64 ROM file formats](https://ultimatepopculture.fandom.com/wiki/List_of_Nintendo_64_ROM_file_formats)
    and [n64dev.org ROM formats](http://n64dev.org/romformats.html) — .z64/.n64/
    .v64 magic bytes + header offsets.
26. [N64 Squid — Saving on EEPROM (libdragon)](https://n64squid.com/homebrew/libdragon/saving/eeprom/)
    and [Christopher Bonhage — SaveTest-N64](https://christopherbonhage.com/SaveTest-N64/)
    — save-type access paths and detection.
27. [RetroTechCollection — N64 Troubleshooting (clocks)](https://wiki.retrotechcollection.com/Nintendo_64_Troubleshooting_Guide),
    [PiMyRetro — PAL→NTSC conversion](https://www.pimyretro.org/converting-a-pal-nintendo-64-motherboard-to-ntsc/),
    [OSnews — Factor 5 microcode](https://www.osnews.com/story/129733/) —
    master-clock derivation, region timing, custom-microcode evidence.

### Sources consulted but access-blocked (research trail)

- [N64brew VR4300/RCP/RSP pages](https://n64brew.dev/wiki/VR4300) returned HTTP
  403 to automated fetch; content cross-validated via search snippets +
  the miraheze mirror + Copetti + the R4300i datasheet.
- [hack64.net R4300 wiki](https://hack64.net/wiki/doku.php?id=r4300) —
  permission-denied to fetch; CPU facts sourced from the datasheet + N64brew.
- [n64dev.org/romformats.html](http://n64dev.org/romformats.html) — TLS cert
  name mismatch on fetch; header offsets corroborated via Ultimate Pop Culture.
