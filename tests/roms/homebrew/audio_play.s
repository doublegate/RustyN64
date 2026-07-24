# audio_play.s — a license-clean bare-metal N64 ROM for the RustyN64 harness.
#
# It exercises the REAL CPU -> RDRAM -> AI DMA path with no RSP microcode: it
# CPU-writes a deterministic 128-pair stereo PCM buffer into RDRAM at physical
# 0x0010_0000, then programs the Audio Interface (rate, address, length, enable)
# to DMA that buffer out to the DAC, and spins. The harness runs the real
# VR4300, lets the AI drain the buffer, and pins the emitted stereo stream
# byte-for-byte against the buffer the ROM wrote (Phase 4 — the first *real ROM*
# produces audio through the AI, without depending on the RSP audio microcode).
#
# This is our own code (MIT OR Apache-2.0), so the assembled `.z64` is
# committable. It boots only through the harness direct-load path
# (`rom::load_direct`, which copies the payload to 0x8000_1000 and jumps there);
# it carries no real IPL3, so it does not boot on hardware. That is deliberate:
# it is an AI fixture, not a game.
#
# Registers (o32): AI base 0xA450_0000 (KSEG1, uncached). PCM buffer 0xA010_0000
# (KSEG1 -> physical 0x0010_0000, which AI_DRAM_ADDR points at). 128 stereo
# pairs = 512 bytes.

.set noreorder
.set noat
.section .text
.globl _start
_start:
        # --- Generate the PCM buffer: 128 stereo sample-pairs at 0xA010_0000. ---
        # Pair i has left = i*256 (a rising ramp) and right = (127-i)*256 (a
        # falling ramp), packed big-endian as (left << 16) | right. The per-sample
        # arithmetic makes the golden prove the AI DMA read the right addresses in
        # order (a constant buffer could not distinguish "DMA ran" from "stuck").
        lui     $t5, 0xA010             # PCM write pointer (uncached -> phys 0x0010_0000)
        ori     $t6, $zero, 0           # pair index i = 0
        ori     $t7, $zero, 128         # pair count
gen:
        sll     $t8, $t6, 24            # left (= i << 8) positioned in bits 31:16
        ori     $t9, $zero, 127
        subu    $t9, $t9, $t6           # 127 - i
        sll     $t9, $t9, 8             # right = (127 - i) * 256, in bits 15:0
        or      $t8, $t8, $t9           # word = (left << 16) | right
        sw      $t8, 0($t5)             # store the stereo sample word
        addiu   $t5, $t5, 4
        addiu   $t6, $t6, 1
        bne     $t6, $t7, gen
        nop

        # --- Program the Audio Interface to DMA the buffer out. ---
        # Order matters: AI_DRAM_ADDR and AI_DACRATE and AI_CONTROL are set before
        # AI_LENGTH, because the AI_LENGTH write is what enqueues the buffer (and,
        # into an idle queue, starts it and raises the AI interrupt).
        lui     $t0, 0xA450             # AI register block base (KSEG1)

        lui     $t1, 0x0010             # PCM buffer physical base 0x0010_0000
        sw      $t1, 0x00($t0)          # AI_DRAM_ADDR

        ori     $t2, $zero, 1103        # DACRATE 1103 -> ~44.1 kHz on NTSC
        sw      $t2, 0x10($t0)          # AI_DACRATE

        ori     $t3, $zero, 1           # DMA enable (bit 0)
        sw      $t3, 0x08($t0)          # AI_CONTROL

        ori     $t4, $zero, 512         # length = 512 bytes = 128 stereo pairs
        sw      $t4, 0x04($t0)          # AI_LENGTH (enqueues, starts, raises IRQ)
spin:
        beq     $zero, $zero, spin      # done — halt in an infinite loop
        nop
