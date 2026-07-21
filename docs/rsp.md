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

### The scalar unit (implemented, T-21-004/T-21-005)

The SU is a *subset* of MIPS: 32-bit integer ALU/branch/load-store only — no
64-bit ops, no TLB, no most of COP0, no R4300-style multiply/divide. It addresses
only DMEM (data) and IMEM (instructions); there is no external bus except via the
SP DMA engine.

Absent, per N64brew *RSP CPU Core*: the whole multiply/divide unit (`MULT`,
`DIV`, `MFHI`, `MFLO`, … and `HI`/`LO` themselves); every 64-bit opcode and
`LD`/`SD`; `LWL`/`LWR`/`SWL`/`SWR`; `SYSCALL` and the trap family; and the
"likely" branches. There is **no exception mechanism at all**, which has a
consequence worth stating: `ADD` and `ADDI` cannot trap on overflow, so they are
indistinguishable from `ADDU`/`ADDIU`.

Two rules catch a MIPS core reused wholesale:

- **The PC is 12 bits and wraps.** Branch and jump targets lose every high bit,
  and running past `0xFFC` continues at `0x000`. `RSP Wrap around` puts two
  `nop`s at `0xFF8` and a `BREAK` at `0x000` and expects to stop at `0x4`.
- **Misaligned data accesses are correct, not faults.** `LW` at `0x001` returns
  the bytes at `0x1..=0x4`; addresses are masked to 12 bits and each byte wraps
  inside DMEM independently, so a word read at `0xFFE` takes two bytes from the
  end and two from the start. The same access on the VR4300 is an
  `AddressError` — this is the easiest place to get the RSP wrong.

`BREAK` halts the core and latches `SP_STATUS.BROKE`, leaving the PC past the
instruction; it raises the MI's SP interrupt only when `INTBREAK` is set. Note
the RSP can also stop itself by writing `SET_HALT`, and *that* does **not** set
`BROKE` — the latch records that a `BREAK` executed, not that the core stopped.

`MFC0`/`MTC0` reach `c0`–`c7`, which are the same physical SP registers the CPU
sees at `0x0404_0000`. `c8`–`c15` are the RDP's and read zero until Phase 3.

## Behavior

### Dual issue

One scalar op and one vector op can issue per cycle when properly interleaved —
peak "one SU + one VU opcode per clock" (`ref-docs/research-report.md` §3). The
LLE step fetches the IMEM word at `pc`, decodes the SU op and (for the COP2
escape / `LWC2`/`SWC2`) the VU op, runs `step_su` then `step_vu`, advances `pc`.

### The VU register file and SU/VU moves (implemented, Sprint 2)

The four COP2 moves — `MFC2` (0), `CFC2` (2), `MTC2` (4), `CTC2` (6) — are
selected by the `rs` field, whose top bit (word bit 25) is what separates them
from the computational instructions. That one bit also changes what the element
field *means*: a **byte offset** for a move, a **broadcast modifier** for a
computation. Conflating the two is the first thing to get wrong here.

Because the offset is in bytes, three edge cases exist that a lane-oriented
implementation cannot express, and all agree with a lane model at even offsets —
so a test using only aligned offsets cannot tell them apart:

- An **odd** offset straddles two lanes.
- `MTC2` at byte 15 writes **one** byte, from `rt[15..8]`, and does not wrap.
- `MFC2` at byte 15 **wraps** its second byte to byte 0 of the same register.

`CFC2`/`CTC2` name `VCO` (0), `VCC` (1) or `VCE` (2) and ignore the element
field. `VCE` is 8 bits; the other two are 16, and `CFC2` sign-extends from 16.

### Vector unit fixed-point math

The VU operates on 8 lanes of signed 1.15 fixed-point. The multiply family
(`VMULF`/`VMULU`, `VMACF`/`VMACU`, `VMUDL`/`VMUDM`/`VMUDN`/`VMUDH`,
`VMADL`/`VMADM`/`VMADN`/`VMADH`) feeds the **48-bit-per-lane accumulator**, read
out in three 16-bit slices (`ACCUM_LO`/`MD`/`HI`) via `VSAR`. Results clamp on
write-back: **signed** clamp to `[-32768, 32767]`; **unsigned** clamps negatives
to 0 and overflow to 65535 using a 15-bit threshold (`ref-docs/research-report.md`
§3). Getting saturation or accumulator slicing wrong drifts geometry and audio.

### The accumulator and the multiply family (partly implemented, Sprint 2)

`VMULF`/`VMULU`, `VMUDL`/`VMUDM`/`VMUDN`/`VMUDH`, the six `VMAC*`/`VMAD*`
accumulating forms, `VSAR` and the six bitwise operations execute. The compares,
the selects, `VRNDN`/`VRNDP`, `VMULQ` and `VRCP`/`VRSQ` do not yet, and report
so rather than writing a wrong result — `vu_compute` returns `false` and the
instruction retires inertly.

**`VMACF` adds no rounding constant**, where `VMULF` adds `0x8000`. It is the
most confusable difference in the family and the accumulator is the only place
it shows: the oracle's lane 3 moves from `0xC000` to `0x1_0000`, a delta of
exactly `2 × 8192`. Reusing `VMULF`'s expression lands on `0x1_8000`.

`VMADL` and `VMADN` extract differently from `VMADM`/`VMADH`: when `acc >> 16`
fits in a signed 16-bit value the result is the accumulator's **low** slice
untouched, and otherwise it saturates to `0x0000`/`0xFFFF` by sign. So the low
bits are returned or discarded wholesale on a test applied to a *different* part
of the accumulator — neither clamp expresses it, and reusing one looks right for
small values and fails at the boundary.

The accumulator is **one 48-bit register per lane**, not three 16-bit ones.
`VSAR` slicing it into `ACC_HI`/`ACC_MD`/`ACC_LO` invites the latter, but the
multiplies write across the full 48 bits and the extraction that produces `vd`
reads a 32-bit window *spanning* two slices — split storage loses the carries
between them.

`VMULF`'s rule was derived from n64-systemtest's own expected vectors rather
than recalled: **acc = 2·vs·vt + 0x8000**, with `vd` a signed clamp of
`acc >> 16`. The rounding constant lands in the *accumulator*, which is only
visible because the suite reads `ACC_LO` back as `0x8000` for a zero product.
The last lane of its vector is what pins the clamp: the 48-bit accumulator is
positive there, `acc >> 16` is `0x8000` = 32768, one past the signed maximum,
and the result saturates to `0x7FFF`.

**These became observable only once `LQV`/`SQV` landed**, which is why they are
in the same sprint. Every VU test in n64-systemtest loads its operands with
`LQV` and reads results back with `SQV`, so the computational group moved the
suite count by *zero* until the vector load/store group existed — at which point
the count fell from 247 to 224 in one step. Until then these instructions were
pinned only by unit tests written against the oracle's published vectors, which
is weaker evidence than a passing suite; that gap is now closed for the
implemented opcodes and remains open for the rest.

### Reciprocal / rsqrt via ROM

`VRCP`/`VRSQ` (and the H/L two-part variants) are **table lookups**, not
arithmetic; the tables must be bit-exact or transformed vertices land wrong
(`ref-docs/research-report.md` §3). The H/L pairs stage a 32-bit operand/result
through `DIV_IN`/`DIV_OUT`.

### Vector load/store (partly implemented)

Encoding: `LWC2`/`SWC2` | `base` (25..21) | `vt` (20..16) | `opcode` (15..11) |
`element` (10..7) | `offset` (6..0). The offset is a **signed 7-bit** field
scaled by the access size, not the 16-bit immediate an ordinary load carries —
reading `imm` whole gives a wildly wrong address.

`element` is a **byte** index naming the first byte the operation touches, so a
non-zero element moves *fewer* bytes rather than shifting a full-width window.

Implemented: the scalar group (`LBV`/`LSV`/`LLV`/`LDV` and their stores, sizes
1/2/4/8 with the offset scaled to match), `LQV`/`SQV`, and `LRV`/`SRV`.

**The register side never wraps.** n64-systemtest states it outright: *"the
element specifier specifies the starting element. If there isn't enough room
after e, there is no wrap-around but the number of bytes loaded is reduced."*
Masking the byte index with 15 — the obvious reading of a 16-byte register —
silently wraps to byte 0 and corrupts the far end of the vector. The **DMEM**
side does wrap, and only for `LSV`/`LLV`/`LDV`: *"only three instructions can
overflow"*, the rest stay inside 16 bytes by alignment.

`LQV` and `LRV` are the pair that reconstructs a misaligned 128-bit load, and
neither alone can. `LQV` runs from the address up to the next 16-byte boundary;
`LRV` runs from the *previous* boundary up to the address and lands at the **far
end** of the register — 8 bytes go to `VPR[8..15]`, not `VPR[0..7]`. Writing
them from byte 0, the natural mirror of `LQV`, puts the right-hand half in the
left-hand slots and the pair stops reconstructing anything.

Not yet implemented, and reported rather than approximated: the packed
`LPV`/`LUV`/`SPV`/`SUV`, the strided `LHV`/`LFV`/`SHV`/`SFV`, and the
transposing `LTV`/`STV`/`SWV`. (`LWV` does not exist on hardware — the suite
records that it *"does nothing"*.)

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
