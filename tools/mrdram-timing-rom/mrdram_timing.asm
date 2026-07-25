// RustyN64 -- M(RDRAM) cached-load timing ROM (bass, ARM9 fork syntax).
//
// Measures the VR4300 D-cache line-fill cost on REAL HARDWARE via a differential:
// a loop of N cached loads that MISS (strided over a region larger than the 8 KiB
// D-cache) vs N loads that HIT (one address), timed with the COP0 Count register.
//   per_miss_count = (delta_miss - delta_hit) / N
// Count ticks once per 2 PClocks, so M(RDRAM) fill cost (PClocks) = per_miss * 2.
//
// Results are written both to fixed RDRAM words (for an emulator runner to read)
// and to the ISViewer text channel (readable by an EverDrive / 64drive so you can
// read them on hardware). Run it, read the three numbers, and the fill cost drops
// straight into accuracy-ledger C-1 -- replacing the value currently FITTED from
// ares/cen64.
//
// Build (see build.sh):   bass mrdram_timing.asm   ->   mrdram_timing.z64
// The IPL3 region is left blank: our emulator's `load_direct` skips it, and a
// flashcart supplies a bootcode + fixes the CRC for hardware (e.g. `chksum64`).

arch n64.cpu
endian msb
output "mrdram_timing.z64", create
fill 0x8000              // 32 KiB (header + blank IPL3 + code); pad for your
                         // flashcart if it needs a larger/power-of-two image

// MIPS register aliases (ARM9 bass does not predefine them).
constant r0 = 0
constant at = 1
constant v0 = 2
constant v1 = 3
constant a0 = 4
constant a1 = 5
constant a2 = 6
constant a3 = 7
constant t0 = 8
constant t1 = 9
constant t2 = 10
constant t3 = 11
constant t4 = 12
constant t5 = 13
constant t6 = 14
constant t7 = 15
constant t8 = 24
constant t9 = 25
constant sp = 29
constant ra = 31

// ---- COP0 + hardware constants ----
constant COUNT = 9       // COP0 register 9 (Count)
constant N = 1024        // loads per timing block
constant STRIDE = 16     // one D-cache line
constant DATA = 0x80100000   // cached KSEG0 scratch (phys 0x100000), 16 KiB swept
constant RESULTS = 0xA0002000  // uncached KSEG1 result words (phys 0x2000)
constant ISVIEWER_WLEN = 0xB3FF0014
constant ISVIEWER_BUF  = 0xB3FF0020

// ---- ROM header (0x40 bytes) ----
origin 0x00000000
base   0x80000000
  dw 0x80371240          // PI BSD DOM1 config + magic
  dw 0x0000000F          // clock rate
  dw 0x80001000          // ENTRY POINT (loader reads this at 0x08)
  dw 0x00001444          // release
  dw 0x00000000          // CRC1 (fix with chksum64 for hardware)
  dw 0x00000000          // CRC2
  dw 0x00000000
  dw 0x00000000
  db "RUSTYN64 MRDRAM  "  // 16 of the 20-byte name
  db "TIME"               // + 4 = 20
  dw 0x00000000          // media/id
  dw 0x0000004E          // 'N' region etc.

// ---- IPL3 area (0x40..0xFFF): left blank (see header note) ----
origin 0x00000040
  fill 0xFC0, 0x00

// ---- Code (ROM 0x1000 -> RDRAM 0x1000, entry 0x80001000) ----
origin 0x00001000
base   0x80001000
Start:
  // Stack (top of RDRAM-ish, not really used).
  lui  sp, 0x8020

  // Zero the ISViewer running-length counter. RDRAM is NOT pre-zeroed on
  // hardware, so PrintHex must not read garbage into it.
  lui  t8, ISVLEN >> 16
  ori  t8, t8, ISVLEN & 0xFFFF
  sw   r0, 0(t8)

  // ---- MISS block: N strided cached loads, each a new line ----
  lui  t1, DATA >> 16
  ori  t1, t1, DATA & 0xFFFF   // t1 = load pointer
  ori  t2, r0, N               // t2 = counter
  mtc0 r0, COUNT               // Count = 0
  nop
MissLoop:
  lw   r0, 0(t1)               // cached load (misses -- strided)
  addiu t1, t1, STRIDE         // next line
  addiu t2, t2, -1
  bne  t2, r0, MissLoop
  nop
  mfc0 t3, COUNT               // t3 = delta_miss (Count = 0 at start)
  nop

  // ---- HIT block: N loads from ONE address (hits after first fill) ----
  lui  t1, DATA >> 16
  ori  t1, t1, DATA & 0xFFFF
  lw   r0, 0(t1)               // warm the line: MissLoop evicted it, so pre-fill
  nop                          //   it OUTSIDE the timed window -> pure hits
  ori  t2, r0, N
  mtc0 r0, COUNT
  nop
HitLoop:
  lw   r0, 0(t1)               // same address -- hits
  nop
  addiu t2, t2, -1
  bne  t2, r0, HitLoop
  nop
  mfc0 t4, COUNT               // t4 = delta_hit
  nop

  // ---- results ----
  // per_miss (Count units) = (delta_miss - delta_hit) / N ; *2 = PClocks.
  subu t5, t3, t4              // t5 = delta_miss - delta_hit

  lui  t0, RESULTS >> 16
  ori  t0, t0, RESULTS & 0xFFFF
  sw   t3, 0(t0)               // [0] = delta_miss
  sw   t4, 4(t0)               // [4] = delta_hit
  sw   t5, 8(t0)               // [8] = delta_miss - delta_hit  (= N * per_miss)
  ori  t6, r0, N
  sw   t6, 12(t0)             // [12] = N

  // ---- ISViewer: emit the raw numbers as 8 hex digits each on hardware ----
  // (kept simple: a small hex printer)
  ori  a0, t3, 0
  jal  PrintHex
  nop
  ori  a0, t4, 0
  jal  PrintHex
  nop
  ori  a0, t5, 0
  jal  PrintHex
  nop
  jal  IsvFlush
  nop

Spin:
  j Spin
  nop

// ---- ISViewer helpers ----
// A tiny append-to-buffer text writer + hex formatter. `PrintHex` appends 8 hex
// chars + a space; `IsvFlush` commits the buffer length.
constant ISVLEN = 0xA0001F00     // our own running length counter (uncached)
PrintHex:
  // a0 = value. Append 8 hex nibbles (MSB first) then a space.
  lui  t7, ISVIEWER_BUF >> 16
  ori  t7, t7, ISVIEWER_BUF & 0xFFFF   // t7 = buf base
  lui  t8, ISVLEN >> 16
  ori  t8, t8, ISVLEN & 0xFFFF
  lw   t9, 0(t8)                // t9 = current length
  ori  a1, r0, 8               // 8 nibbles
PhLoop:
  srl  a2, a0, 28              // top nibble
  andi a2, a2, 0xF
  sltiu a3, a2, 10
  bne  a3, r0, PhDigit
  addiu a2, a2, 0x30          // '0'..'9'
  addiu a2, a2, 7             // 'A'..'F' (0x41-0xA = 0x37, +0x30 above => +7 more)
PhDigit:
  addu at, t7, t9
  sb   a2, 0(at)
  addiu t9, t9, 1
  sll  a0, a0, 4
  addiu a1, a1, -1
  bne  a1, r0, PhLoop
  nop
  addu at, t7, t9             // trailing space
  ori  a2, r0, 0x20
  sb   a2, 0(at)
  addiu t9, t9, 1
  sw   t9, 0(t8)              // save length
  jr   ra
  nop

IsvFlush:
  lui  t8, ISVLEN >> 16
  ori  t8, t8, ISVLEN & 0xFFFF
  lw   t9, 0(t8)
  lui  t7, ISVIEWER_WLEN >> 16
  ori  t7, t7, ISVIEWER_WLEN & 0xFFFF
  sw   t9, 0(t7)             // write length -> ISViewer emits the text
  jr   ra
  nop
