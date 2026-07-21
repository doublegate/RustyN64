# RSP (Reality Signal Processor) — RustyN64

**References:** `ref-docs/research-report.md` §3 (RSP, the LLE decision), §2 (SP
interface); Dillonb `n64-resources/rsp.md` (cited there); ADR 0002;
`crates/rustyn64-rsp/src/lib.rs`; `docs/architecture.md`; `docs/rdp.md`;
`docs/audio.md`.

This doc is the SPEC, not history — update it in the same PR as the code. Pin
behaviour against **n64-systemtest's RSP category** and Dillonb's hardware-
verified RSP tests FIRST.

## Purpose

The RSP is the RCP's programmable coprocessor at 62.5 MHz: a MIPS-derived scalar
unit (SU) fused with an 8-lane × 16-bit SIMD vector unit (VU, COP2). It runs
game-supplied **microcode** out of 4 KiB IMEM with 4 KiB DMEM scratch, driving
geometry (transform/light/clip a display list → RDP commands) and audio (decode
ADPCM, mix, resample → 16-bit stereo PCM). RustyN64 emulates it **LLE** — execute
the actual instruction stream — which is the only path that reproduces custom
microcode and gives correct audio "for free" (ADR 0002,
`ref-docs/research-report.md` §3).

## Interfaces

```rust
pub const SP_MEM_SIZE: usize = 4 * 1024; // DMEM and IMEM each

pub trait RspBus {
    fn rdram_read(&self, addr: u32) -> u8;        // SP-DMA source/target
    fn rdram_write(&mut self, addr: u32, val: u8);
    fn raise_sp_interrupt(&mut self);             // microcode signalled $MI
}

pub struct Rsp {
    pub su_regs: [u32; 32],     // scalar GPRs ($zero pinned)
    pub vu_regs: [[u16; 8]; 32],// 32 vector regs, 8 lanes × 16-bit
    pub vu_acc:  [u64; 8],      // 48-bit-per-lane accumulator (low 48 used)
    pub pc: u16,                // 12-bit IMEM PC
    pub halted: bool,           // SP_STATUS.halt
    pub dmem: Box<[u8; SP_MEM_SIZE]>,
    pub imem: Box<[u8; SP_MEM_SIZE]>,
}
impl Rsp {
    pub fn mem_read(&self, off: u32) -> u8;          // CPU-side DMEM/IMEM view
    pub fn mem_write(&mut self, off: u32, val: u8);
    pub fn tick<B: RspBus>(&mut self, bus: &mut B); // no-op while halted
}
```

### The RSP owns DMEM and IMEM

Both banks live in `Rsp`, and the Bus reaches them through `mem_read`/`mem_write`
rather than holding a parallel copy. The CPU-visible window and the RSP's own
addressing must be **the same storage**: two copies that merely start out equal
diverge the moment either side writes, and nothing detects it.

### The CPU-visible window

`0x0400_0000-0x0400_0FFF` is DMEM and `0x0400_1000-0x0400_1FFF` is IMEM, but the
8 KiB **repeats** for the whole range up to `0x0404_0000`, where the SP registers
begin (n64-systemtest `sp_memory::SW (out of bounds)` writes at `0x3E000` and
reads the result back at offset 0). Folding the offset is therefore the
behaviour, not a bounds-check standing in for one — recorded with its provenance
as accuracy ledger **C-30**, since the N64brew wiki documents only the first
8 KiB and the mirroring rests on the oracle. Each bank wraps within its own
4 KiB; nothing ever spills from one into the other.

Accesses to this window go through the RCP's internal bus, which **ignores the
access size** — a byte or halfword store writes the whole shifted 32-bit word and
a 64-bit store touches only four bytes. That rule is not RSP-specific and is
recorded once, in accuracy ledger **C-28**.

### The SP interface registers (implemented, T-21-002)

Eight registers at `0x0404_0000` and `SP_PC` at `0x0408_0000` — note `SP_PC` is
in its **own** window, not the ninth slot of the block. The same physical
registers are exposed to the RSP as COP0 `c0`–`c7`, so `crates/rustyn64-rsp/src/sp.rs`
holds one copy and both views reach it.

`SP_STATUS` reads as a flag word and writes as **set/clear command pairs**, two
bits per flag. That asymmetry is the design, not an encoding quirk: it lets
either processor change one flag with a single store, with no read-modify-write
to race. The rule that falls out — and that catches naive implementations —
is that writing a flag's **set and clear bits together leaves it unchanged**.
n64-systemtest checks it for every reachable flag.

| Read bit | Flag | | Write bits (clear, set) |
| --- | --- | --- | --- |
| 0 | `HALTED` | | 0, 1 |
| 1 | `BROKE` | | 2, — (a latch; hardware sets it) |
| 2 | `DMA_BUSY` | | — |
| 3 | `DMA_FULL` | | — |
| 4 | `IO_BUSY` | | — |
| 5 | `SSTEP` | | 5, 6 |
| 6 | `INTBREAK` | | 7, 8 |
| 7 + n | `SIG<n>` | | 9 + 2n, 10 + 2n |

The interrupt commands (clear 3, set 4) are **not** a `SP_STATUS` flag at all —
they raise and acknowledge the MI's SP line, which is why the register file
reports the change and the Bus applies it.

`SP_SEMAPHORE` is a mutex bit: a write releases it *whatever value is written*,
and a read returns the current value and then takes it. So the observable
sequence is write, then 0, then 1 for ever — a reader that sees 0 has just
acquired it.

The DMA registers are **double-buffered**: an address write stages a pending
value, and reads keep reporting the ongoing or last-completed transfer until a
length write starts one. After completion the pointers sit past the data and the
length field reads `0xFF8`, because hardware decrements it per 64-bit word and
ends at `-8`. Both length registers report the same transfer regardless of the
direction programmed. The length field rounds **up** to a multiple of 8 — writing
anything from 0 to 7 transfers exactly 8 bytes.

The CPU DMAs microcode + data in, then clears `SP_STATUS.halt`; the RSP runs
until a `BREAK` (which sets halt and, if enabled, raises the SP interrupt).

## State

Beyond the skeleton fields, the full VU needs (per `ref-docs/research-report.md`
§3, marked TODO in the crate):

- **`VCO`** (16-bit carry/overflow), **`VCC`** (16-bit compare flags), **`VCE`**
  (8-bit clip-equality) control registers.
- **DIV_IN / DIV_OUT** staging latches for the two-part reciprocal ops.
- **The reciprocal/rsqrt ROM tables** (bit-exact) for `VRCP`/`VRSQ` +
  `VRCPH`/`VRCPL`/`VRSQH`/`VRSQL`.
- SP-DMA **length/skip/count** latches (the DMA supports a strided "skip"
  pattern).

The SU is a *subset* of MIPS: 32-bit integer ALU/branch/load-store only — no
64-bit ops, no TLB, no most of COP0, no R4300-style multiply/divide. It addresses
only DMEM (data) and IMEM (instructions); there is no external bus except via the
SP DMA engine.

## Behavior

### Dual issue

One scalar op and one vector op can issue per cycle when properly interleaved —
peak "one SU + one VU opcode per clock" (`ref-docs/research-report.md` §3). The
LLE step fetches the IMEM word at `pc`, decodes the SU op and (for the COP2
escape / `LWC2`/`SWC2`) the VU op, runs `step_su` then `step_vu`, advances `pc`.

### Vector unit fixed-point math

The VU operates on 8 lanes of signed 1.15 fixed-point. The multiply family
(`VMULF`/`VMULU`, `VMACF`/`VMACU`, `VMUDL`/`VMUDM`/`VMUDN`/`VMUDH`,
`VMADL`/`VMADM`/`VMADN`/`VMADH`) feeds the **48-bit-per-lane accumulator**, read
out in three 16-bit slices (`ACCUM_LO`/`MD`/`HI`) via `VSAR`. Results clamp on
write-back: **signed** clamp to `[-32768, 32767]`; **unsigned** clamps negatives
to 0 and overflow to 65535 using a 15-bit threshold (`ref-docs/research-report.md`
§3). Getting saturation or accumulator slicing wrong drifts geometry and audio.

### Reciprocal / rsqrt via ROM

`VRCP`/`VRSQ` (and the H/L two-part variants) are **table lookups**, not
arithmetic; the tables must be bit-exact or transformed vertices land wrong
(`ref-docs/research-report.md` §3). The H/L pairs stage a 32-bit operand/result
through `DIV_IN`/`DIV_OUT`.

### Vector load/store

Sized element loads `LBV`/`LSV`/`LLV`/`LDV` (address = `base + offset*size`,
unaligned allowed); 128-bit `LQV`/`SQV` + left/right `LRV`/`SRV` (LWL/LWR-style
partial); diagonal `LTV`/`STV` transposing across an 8-register group; packed
`LPV`/`LUV`/`SPV`/`SUV` (`ref-docs/research-report.md` §3).

## Edge cases and gotchas

- **The single-lane element-aliasing quirk.** When `vt_elem(3) == 0`, lower bits
  of the *source*-lane index are replaced by bits of the *destination*-lane index
  — a documented hardware operand-aliasing anomaly that must be reproduced
  (`ref-docs/research-report.md` §3, §challenge 1).
- **Clamp threshold is 15-bit for unsigned.** The unsigned saturation uses a
  15-bit threshold, not a naive `> 65535` test; off-by-one here fails the VU
  tests.
- **The recip ROM is data, not a formula.** Table-drive it from the documented
  values; do not approximate.
- **`BREAK` sets halt, optionally IRQs.** A microcode `BREAK` halts the RSP and
  raises the SP interrupt only if the enable bit is set in `SP_STATUS`.
- **SP DMA "skip".** The DMA can transfer strided blocks (length + skip + count);
  a flat memcpy is wrong for microcode that relies on it.
- **DMEM/IMEM wrap at 4 KiB.** The 12-bit PC and DMEM addresses wrap; no fault on
  overflow.
- **LLE means the audio path is free.** Do NOT add a separate audio HLE — the
  audio microcode runs on this same core (`docs/audio.md`,
  `ref-docs/research-report.md` §5).

## Test plan

- **n64-systemtest RSP category** — the hardware-verified gate.
- **Dillonb RSP tests** — captured over an EverDrive 64 USB link with
  `rsp-recorder`; the per-instruction VU/SU oracle (`ref-docs/research-report.md`
  §7).
- **Per-op vectors** — saturation boundaries, accumulator slicing (`VSAR`), the
  recip-ROM lookups, the element-aliasing quirk, every vector load/store variant.
- **Microcode integration** — run real F3DEX2 / audio microcode and check the
  emitted RDP command list / PCM buffer byte-for-byte against a reference.

## Open questions

- **Interpreter vs dynarec.** Per `ref-docs/research-report.md` §3 + §challenge 7,
  the single-threaded RSP+CPU is the emulation bottleneck under LLE; start
  interpreter-only (the determinism oracle) and add an RSP dynarec later
  (`docs/performance.md`), keeping the interpreter as the fallback.
- **Microcode coverage breadth** — which custom (Factor 5 / Rare / Boss) microcode
  variants need explicit regression ROMs.
