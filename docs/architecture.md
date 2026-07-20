# Architecture — RustyN64

**References:** `ref-docs/research-report.md` (the immutable corpus), plus the
per-subsystem docs linked below. `docs/STATUS.md` is the single source of truth
for current state.

This is the hub. Read it before any chip doc — reading a subsystem spec without
the load-bearing facts below in mind will mislead.

## Purpose

RustyN64 is a cycle-accurate Nintendo 64 emulator in pure Rust. The accuracy bar
is ares / CEN64 / Gopher64 / ParaLLEl: low-level emulation (LLE) of the
programmable coprocessors, one timeline co-scheduling three asynchronous compute
engines, and a bit-exact framebuffer the test ROMs define. The frontend is pure
Rust (`winit` + `wgpu` + `cpal` + `egui`), reusing the RustyNES shell.

The machine is, per `ref-docs/research-report.md` §Executive summary, two big
chips on a unified Rambus memory pool: the **NEC VR4300** CPU (MIPS III, 64-bit,
93.75 MHz) and the **Reality Coprocessor (RCP)** at 62.5 MHz — which itself
contains the **RSP** (a programmable MIPS+SIMD core running downloadable
microcode), the **RDP** (a fixed-function rasterizer writing the framebuffer into
RDRAM), and the eight memory-mapped sub-interfaces (SP, DP, MI, VI, AI, PI, RI,
SI) the CPU drives every subsystem through.

## The eight load-bearing facts

These cross-cutting decisions span multiple files. They are the reason the
per-chip docs read the way they do.

### 1. The VR4300 is the master clock; everything is lockstep

The scheduler advances one **master tick = one VR4300 cycle** per
`System::tick_one_unit()`; the RCP (RSP + RDP + interfaces) advances on a
**3:2 fractional accumulator** — 2 RCP ticks per 3 master ticks, per
`ref-docs/research-report.md` §1 (the R4300i `DivMode` 1.5:1 ratio). This is
**lockstep, not catch-up**: a mid-instruction RCP event (a DP-done IRQ, an SP
halt, an AI buffer drain) is visible to the very next CPU step. It is the central
architectural choice and the reason mid-frame coprocessor effects (framebuffer
read-back, mid-display-list SP_STATUS polling) work without per-quirk patches.
See `docs/scheduler.md` and ADR 0001.

### 2. The Bus owns everything mutable

`rustyn64-core::Bus` holds the RDRAM (8 MiB: 4 MiB base + 4 MiB Expansion Pak),
the RSP, the RDP, the AI, the cart (→ PI/SI), the controllers, and the RCP
interface register state (the MI interrupt lines + mask). The CPU borrows
`&mut Bus` during `Cpu::tick`. Per the TetaNES postmortem (carried over from
RustyNES), this single owner avoids the borrow-checker fight that "CPU holds the
coprocessor, but the coprocessor also needs the CPU bus" creates. Each chip sees
only the smaller trait it actually needs:

- `rustyn64_cpu::Bus` — full system memory view (`read_u8`/`write_u8`/
  `read_u32`/`write_u32` + `poll_irq_at_phase`).
- `rustyn64_cart::RdramBus` — shared RDRAM byte/word access (the DMA path).
- `rustyn64_rdp::VideoBus: RdramBus` — RDRAM + `raise_dp_interrupt`.
- `rustyn64_rsp::RspBus` — RDRAM + `raise_sp_interrupt`.
- `rustyn64_audio::AudioBus` — DMA-sample read + `raise_ai_interrupt`.

The Bus steps each owned chip with the `core::mem::take` split-borrow trick
(`Bus::rsp_tick`/`rdp_tick`/`audio_tick`): move the chip out, tick it against
`&mut self` (which implements the chip's narrow trait), move it back — no
allocation. See `crates/rustyn64-core/src/bus.rs`.

### 3. The crate graph is one-directional

```text
rustyn64-cpu      (no chip deps)
rustyn64-rsp      (no chip deps)
rustyn64-cart     (no chip deps; defines RdramBus)
rustyn64-rdp      → rustyn64-cart        (only for the RdramBus trait)
rustyn64-audio    (no chip deps)
rustyn64-core     → all of the above     (ties them, re-exports their types)
rustyn64-frontend → rustyn64-core
rustyn64-test-harness → rustyn64-core
```

`rustyn64-cpu` has no RSP/RDP/AI dependency. `rustyn64-rdp` depends on exactly
one chip crate (`rustyn64-cart`) purely to borrow the `RdramBus` trait — the N64
analog of how `rustynes-ppu` reaches tile storage through `rustynes-mappers`.
`rustyn64-core` is the only crate that knows about every chip. **Result:** each
chip is fuzzable and benchmarkable in isolation. Adding a cross-chip dependency
breaks this invariant — don't. Downstream consumers depend on `rustyn64-core`
(which re-exports the chip types), never the chip crates directly.

### 4. The RCP is the integration point — LLE coprocessors, not HLE

The single most consequential decision (ADR 0002) is **LLE vs HLE** for the
RSP and RDP. Per `ref-docs/research-report.md` §3 and §Architecture options, the
N64 let every studio ship custom microcode, so HLE (recognize a microcode
binary by signature, substitute a native reimplementation) is per-game-fragile
and mis-renders mid-frame tricks. **RustyN64 commits to LLE**: execute the actual
RSP instruction stream (scalar + vector ISA), feed the resulting real RDP command
list to a faithful rasterizer. Audio falls out for free — the RSP audio microcode
runs on the same LLE RSP core, so there is no per-game audio HLE. An HLE fast path
may exist later behind an off-by-default flag, never as the default. The RSP and
RDP `tick` methods are LLE-shaped stubs today (`docs/STATUS.md`).

### 5. Board logic lives in the cart crate

Cartridge / board behaviour — PI/SI-mediated reads and writes, the CIC handshake,
save backing — lives behind the `rustyn64_cart::Cartridge` trait, not in the CPU.
All hooks default to no-op so a board implements only what it uses. Unlike the
NES (hundreds of mappers), the N64 has essentially one cart model parameterized
by save type (EEPROM 4k/16k, SRAM, FlashRAM, Controller Pak) and CIC variant —
which is why there is **no board tiering or honesty gate** (ADR 0003).

### 6. Determinism is a hard contract

Same seed + ROM + input sequence ⇒ bit-identical framebuffer and audio. CPU/RCP
initial phase alignment is randomized at power-on from a **seeded** SplitMix64
PRNG; reset preserves alignment. No system time, OS RNG, or thread scheduling in
the core. This is required for save-state round-trip, regression tests, TAS
replay, and netplay rollback. Netplay's dynamic rate control and run-ahead live
in the **frontend** (a resampler stage / snapshot-restore orchestration), never
in the core. See ADR 0004 and `docs/scheduler.md`.

### 7. The frontend is an always-on egui shell, not a bare window

`rustyn64-frontend` is winit + wgpu + cpal + egui, and egui runs **every frame**:
a persistent menu bar + status bar + tabbed Settings, with toggleable debugger
panels layered on top. The shell never holds the emu lock inside the egui
closure — menu interactions return a `MenuAction` dispatched after the egui pass.
On native the emulator runs on a dedicated thread; the winit thread only does UI
and present. Full spec in `docs/frontend.md`.

### 8. Test ROMs are the spec

When the docs and a passing test ROM disagree, the ROM wins — the docs get
updated. **n64-systemtest** (Rust, hardware-verified, self-judging) is the strict
CPU/COP0/TLB/RSP gate; the **ParaLLEl-RDP** ~150-test fuzz suite is the RDP
bit-exactness gate; **PeterLemon/Dillonb** ROMs + commercial screenshots are the
regression corpus. See `docs/testing-strategy.md` and ADR 0003.

## Chip → crate map

| Crate | Chip / role | Spec doc |
|---|---|---|
| `rustyn64-cpu` | NEC VR4300 (MIPS III, TLB, FPU, SysAD) | `docs/cpu.md` |
| `rustyn64-rsp` | RSP (SU + VU, DMEM/IMEM, microcode) | `docs/rsp.md` |
| `rustyn64-rdp` | RDP (rasterizer) + VI scan-out | `docs/rdp.md` |
| `rustyn64-audio` | AI DAC + sample DMA | `docs/audio.md` |
| `rustyn64-cart` | PI cart + PIF/CIC boot + SI + saves | `docs/cart.md`, `docs/cartridge-format.md` |
| `rustyn64-core` | Bus + scheduler + RCP interface regs | `docs/scheduler.md` |
| `rustyn64-frontend` | egui/wgpu/cpal/winit shell (binary `rustyn64`) | `docs/frontend.md` |
| `rustyn64-test-harness` | golden-log differ + accuracy scorer + frame comparator | `docs/testing-strategy.md` |
| `rustyn64-netplay` | rollback netplay orchestration (frontend-side) | `docs/frontend.md` |
| `rustyn64-cheevos` | RetroAchievements FFI (later, off by default) | — |

## Data flow (one frame)

1. The CPU runs game code: builds a display list + audio command list in RDRAM,
   then writes `SP_DMA_*` to DMA microcode + data into IMEM/DMEM and clears
   `SP_STATUS.halt` to start the RSP.
2. The **RSP** (LLE) executes the graphics microcode: transform/light/clip the
   display list, emit an RDP command list into RDRAM (or DMEM); the audio
   microcode decodes ADPCM and mixes 16-bit stereo PCM into an RDRAM buffer.
3. The CPU (or RSP) points the **DP** FIFO at the RDP command list; the **RDP**
   (LLE) rasterizes triangles/rectangles through the texture → combiner →
   blender → Z/coverage pipeline into the framebuffer in RDRAM, raising the DP
   interrupt on `SYNC_FULL`.
4. The **VI** scans the framebuffer out of RDRAM (AA / divot / de-dither),
   raising the VI interrupt at the programmed scanline (≈ once per frame).
5. The CPU points the **AI** at the PCM buffer; the AI DMAs it to the DAC,
   raising the AI interrupt on drain.
6. The **MI** ORs every subsystem's interrupt line (masked) into the CPU's IP2;
   the CPU services it through COP0.

All of steps 1–6 happen interleaved on one timeline — they are not phase-ordered
sequential stages. The lockstep scheduler is what keeps the interleaving exact.

## Architectural alternatives considered

- **HLE coprocessors** — rejected for the core (per-game fragility, mid-frame
  effects wrong, custom microcode unsupported). ADR 0002.
- **Integer lockstep** (the NES model) — rejected: RDRAM, DMA durations, VI/AI
  divisors, and PAL timing introduce non-integer relationships integer lockstep
  handles only with per-quirk fudges. Fractional master clock chosen instead.
  ADR 0001 + 0002.
- **Independent engine threads with periodic resync** — rejected: breaks the
  determinism contract and mid-frame coprocessor synchronization. One timeline.
  ADR 0004.

## Open questions

These are surfaced from `ref-docs/research-report.md` §Open questions and gate
deeper planning:

1. **RDRAM/bus contention depth** — how precisely must CPU/RSP/RDP/DMA bus
   arbitration be modelled for commercial-game correctness vs only
   n64-systemtest? Needs a prototype to find the cost/accuracy knee.
2. **Boot strategy** — stub IPL3 (HLE boot, documented) vs run the real PIF/IPL
   ROMs. Likely stub-first with an optional real-IPL mode. See `docs/cart.md`.
3. **RDP backend ordering** — software reference RDP first (always correct) then
   a wgpu-compute accelerator. See `docs/rdp.md` and `docs/performance.md`.
4. **Cache-coherency depth** — how exact must the I/D-cache + DMA coherency model
   be? See `docs/cpu.md`.
5. **Region timing tables** — exact PAL VI/AI divisor values need pinning before
   the region table is frozen. See `docs/compatibility.md`.
