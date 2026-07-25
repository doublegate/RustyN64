// RustyN64 -- I-cache-fill timing ROM (bass, ARM9 fork syntax).
//
// The companion to mrdram_timing.asm (which measures the D-cache fill). This one
// measures the VR4300 **I-cache** line-fill cost on real hardware: it executes a
// straight-line block of N one-PClock instructions that is LARGER than the 16 KiB
// instruction cache, so every 32-byte fetch line (8 instructions) misses. Timed
// with COP0 Count:
//   fill_PClocks = (delta * 2 - N) / (N / 8)
// -- subtract the verified 1-PClock-per-instruction base, divide by the number of
// line fills. (Count ticks once per 2 PClocks; a 32-byte I-cache line is 8 * 4 B.)
//
// Results go to fixed RDRAM words (phys 0x10000, past the code) and the ISViewer
// text channel, as in the D-cache ROM. See README.md; build with build.sh.

arch n64.cpu
endian msb
output "icache_timing.z64", create
fill 0x18000             // 96 KiB: header + blank IPL3 + setup + the 32 KiB block

// MIPS register aliases.
constant r0 = 0
constant at = 1
constant t0 = 8
constant t1 = 9
constant t2 = 10
constant t3 = 11
constant t7 = 15
constant t8 = 24
constant t9 = 25
constant sp = 29
constant a0 = 4
constant a1 = 5
constant a2 = 6
constant a3 = 7
constant ra = 31

constant COUNT = 9
constant N = 8192                // instructions in the straight block (32 KiB > 16 KiB I-cache)
// Result/scratch addresses must sit PAST the loaded code: the 32 KiB block runs
// from RDRAM 0x1000 to ~0x9100, so use phys 0x10000 (64 KiB) and up.
constant RESULTS = 0xA0010000    // uncached result words (phys 0x10000)
constant ISVLEN = 0xA0010100     // our running text length (uncached)
constant ISVIEWER_WLEN = 0xB3FF0014
constant ISVIEWER_BUF  = 0xB3FF0020

// ---- ROM header ----
origin 0x00000000
base   0x80000000
  dw 0x80371240
  dw 0x0000000F
  dw 0x80001000          // entry
  dw 0x00001444
  dw 0x00000000          // CRC1 (fix with chksum64 for hardware)
  dw 0x00000000
  dw 0x00000000
  dw 0x00000000
  db "RUSTYN64 ICACHE "
  db "TIME"
  dw 0x00000000
  dw 0x0000004E

origin 0x00000040
  fill 0xFC0, 0x00

// ---- Code ----
origin 0x00001000
base   0x80001000
Start:
  lui  sp, 0x8020

  // zero the ISViewer length (RDRAM is not pre-zeroed on hardware)
  lui  t8, ISVLEN >> 16
  ori  t8, t8, ISVLEN & 0xFFFF
  sw   r0, 0(t8)

  ori  t0, r0, 0               // $t0 accumulates (the block increments it)
  mtc0 r0, COUNT               // Count = 0
  nop
  jal  StraightBlock           // run the cold straight-line block
  nop
  mfc0 t3, COUNT               // t3 = delta (Count units)
  nop

  // results: delta and N (the runner / reader computes the fill)
  lui  t1, RESULTS >> 16
  ori  t1, t1, RESULTS & 0xFFFF
  sw   t3, 0(t1)               // [0] = delta
  ori  t2, r0, N
  sw   t2, 4(t1)              // [4] = N

  ori  a0, t3, 0
  jal  PrintHex
  nop
  ori  a0, t2, 0
  jal  PrintHex
  nop
  jal  IsvFlush
  nop
Spin:
  j Spin
  nop

// ---- the straight-line block: N one-PClock instructions, all fetch-miss ----
// Emitted at assemble time. `addiu t0,t0,1` has no interlock (its result is not
// read by the next), so each is exactly 1 PClock -- the base the runner subtracts.
StraightBlock:
variable ii = 0
while ii < N {
  addiu t0, t0, 1
  ii = ii + 1
}
  jr   ra
  nop

// ---- ISViewer helpers (same as mrdram_timing.asm) ----
PrintHex:
  lui  t7, ISVIEWER_BUF >> 16
  ori  t7, t7, ISVIEWER_BUF & 0xFFFF
  lui  t8, ISVLEN >> 16
  ori  t8, t8, ISVLEN & 0xFFFF
  lw   t9, 0(t8)
  ori  a1, r0, 8
PhLoop:
  srl  a2, a0, 28
  andi a2, a2, 0xF
  sltiu a3, a2, 10
  bne  a3, r0, PhDigit
  addiu a2, a2, 0x30
  addiu a2, a2, 7
PhDigit:
  addu at, t7, t9
  sb   a2, 0(at)
  addiu t9, t9, 1
  sll  a0, a0, 4
  addiu a1, a1, -1
  bne  a1, r0, PhLoop
  nop
  addu at, t7, t9
  ori  a2, r0, 0x20
  sb   a2, 0(at)
  addiu t9, t9, 1
  sw   t9, 0(t8)
  jr   ra
  nop

IsvFlush:
  lui  t8, ISVLEN >> 16
  ori  t8, t8, ISVLEN & 0xFFFF
  lw   t9, 0(t8)
  lui  t7, ISVIEWER_WLEN >> 16
  ori  t7, t7, ISVIEWER_WLEN & 0xFFFF
  sw   t9, 0(t7)
  jr   ra
  nop
