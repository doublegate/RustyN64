# Glossary — RustyN64

**References:** `ref-docs/research-report.md` (the corpus these terms come from).

Domain terms for the Nintendo 64 and its emulation. Cited sections point at the
research report.

## Clocks — read this before writing "master clock" anywhere

The term is overloaded three ways and the hardware documentation owns one of them.
Always state the rate.

- **MClock / MasterClock** — **62.5 MHz**, the RCP clock, derived RCLK ÷ 4. This is
  what the primary sources (`Clock Timing.md`, VR4300 UM Fig 4-1) mean by
  "MasterClock", and it is the meaning a reader arrives with.
- **PClock** — **93.75 MHz**, the VR4300's internal pipeline clock, multiplied up
  from MClock by the CPU's PLL at `DivMode = 0b01` (1:1.5). One **PCycle** is one
  PClock period, and it internally has two phases, **F1** and **F2** (UM §4.1).
- **SClock** — the VR4300's system-interface clock, normally equal to MClock
  (62.5 MHz). This is the rate SysAD transactions run at, so **one SysAD cycle is
  1.5 PCycles**.
- **TClock** — transmit clock to external agents; always equal to MasterClock.
- **RCLK** — 250 MHz, crystal X2 × 17; the source MClock divides down from.
- **the master tick (this project)** — **187.5 MHz**, the LCM of PClock and MClock,
  and the only counter the emulator increments. CPU every 2 ticks, RCP every 3,
  COP0 `Count` every 4, SI every 12, PIF every 96 (ADR 0006). Beware: 187.5 MHz
  *also* appears in the sources as the `DivMode = 0b11` overclocked PClock, which
  the N64 does not use.
- **VCLK** — the VI video clock, **48.681818 MHz** NTSC, derived from a *different*
  crystal (X1) and therefore not a rational multiple of the above. It is the one
  domain that keeps a fractional accumulator.

## Components

- **RCP (Reality Coprocessor)** — the SGI/Nintendo ASIC at 62.5 MHz that contains
  the RSP, the RDP, and the eight memory-mapped interfaces; it arbitrates all
  memory traffic between the CPU, RDRAM, and the cartridge bus (§2).
- **RSP (Reality Signal Processor)** — the RCP's programmable MIPS-derived
  coprocessor: a scalar unit (SU) + an 8-lane × 16-bit SIMD vector unit (VU),
  running downloadable microcode for geometry and audio (§3).
- **RDP (Reality Display Processor)** — the RCP's fixed-function rasterizer;
  consumes a command stream and writes pixels into the framebuffer in RDRAM (§4).
- **SU (Scalar Unit)** — the RSP's MIPS-subset integer core (no 64-bit ops, no
  TLB); addresses only DMEM/IMEM (§3).
- **VU (Vector Unit)** — the RSP's COP2 SIMD unit: 32 registers × 8 lanes of
  16-bit, a 48-bit-per-lane accumulator, and reciprocal/rsqrt ROM tables (§3).
- **DMEM / IMEM** — the RSP's two 4 KiB on-chip memories: data and instruction;
  no external bus except via SP DMA (§3).
- **microcode** — the MIPS+COP2 program the CPU DMAs into IMEM/DMEM for the RSP to
  run; graphics (Fast3D/F3DEX/F3DEX2/S2DEX + custom) or audio (§3).
- **VI (Video Interface)** — scans the framebuffer out of RDRAM to the DAC,
  applying anti-aliasing / divot / de-dither; raises the VI interrupt (§4).
- **AI (Audio Interface)** — the DAC-only audio interface; DMAs an RDRAM PCM
  buffer to the DAC, does no conversion itself; raises the AI interrupt (§5).
- **PI (Peripheral Interface)** — cartridge-bus DMA (ROM/SRAM/FlashRAM ↔ RDRAM);
  raises the PI interrupt on completion (§2, §6).
- **SI (Serial Interface)** — DMAs the 64-byte PIF RAM block; the controller /
  EEPROM / Controller-Pak path; raises the SI interrupt (§2, §6).
- **RI (RDRAM Interface)** — the low-level RDRAM controller config
  (mode/refresh/latency), used at boot (§2, §8).
- **MI (MIPS Interface)** — the interrupt aggregator: ORs the six RCP interrupt
  sources (SP/SI/AI/VI/PI/DP) into the CPU's single IP2 line, gated by the mask
  (§8).
- **SP (Signal Processor interface)** — the RSP control block: start/halt
  (`SP_STATUS`), the SP DMA engine, `SP_PC`, the semaphore; raises the SP
  interrupt (§2).
- **DP (Display Processor interface)** — feeds the RDP command FIFO
  (`DPC_START`/`END`/`CURRENT`/`STATUS`); raises the DP interrupt on drain (§2).
- **RDRAM** — the unified Rambus memory pool (4 MiB base + 4 MiB Expansion Pak);
  9 bits per byte, the 9th reserved for RDP/VI coverage/Z (§8).
- **TLB (Translation Lookaside Buffer)** — the VR4300's 32-entry MMU; each entry
  maps a page pair, page sizes 4 KB…16 MB, in COP0 (§1).
- **COP0 (System Control coprocessor)** — the VR4300's MMU/TLB + exception
  machinery (`Status`, `Cause`, `EPC`, `EntryHi`, `Count`/`Compare`, …) (§1).
- **COP1 (FPU)** — the VR4300's floating-point coprocessor: 32 FP registers,
  IEEE-754 single/double, `FCR31` control/status (§1).
- **SysAD** — the VR4300's 32-bit multiplexed system bus to the RCP; every
  non-cached access becomes a SysAD transaction the RCP arbitrates (§1).
- **CIC (CIC-NUS)** — the cartridge lockout chip; runs a seed/checksum handshake
  with the in-console PIF; variants (6101/6102/6103/6105/6106 + PAL 71xx) change
  the boot entry point / challenge (§6).
- **PIF (PIF-NUS)** — the in-console boot + serial-I/O chip; holds IPL1, runs the
  CIC handshake, and executes joybus transactions via the 64-byte PIF RAM (§6).
- **IPL1/2/3** — the three-stage Initial Program Loader; IPL3 is the cart's own
  4032-byte bootcode at ROM offset `0x40` (§6).
- **LLE (Low-Level Emulation)** — execute the actual RSP instruction stream + the
  real RDP command list; the accuracy bar (§3, ADR 0002).
- **HLE (High-Level Emulation)** — recognize a microcode by signature and
  substitute a native reimplementation; fast but per-game fragile; NOT the
  RustyN64 core (§3, ADR 0002).
- **dynarec (dynamic recompiler)** — JIT translation of CPU/RSP code to host
  instructions; a later, validated acceleration layer over the interpreter
  (§challenge 7, `docs/performance.md`).
- **Expansion Pak** — the +4 MiB RDRAM module bringing the total to 8 MiB;
  required by some titles (§8).
- **Controller Pak** — the removable 32 KiB memory card, accessed via the SI
  joybus (§6).
- **coverage (9th bit)** — the per-pixel sub-pixel anti-aliasing value stored in
  RDRAM's hidden 9th bit, used by the VI to blend edges (§4, §8).
