# render_fill.s — a license-clean bare-metal N64 ROM for the RustyN64 harness.
#
# It exercises the REAL CPU -> RDRAM -> VI scan-out path with no RDP/RSP/texture
# involvement: it programs the Video Interface for a 32x24, 16-bit (RGBA5551)
# framebuffer at physical 0x0020_0000, then CPU-writes a deterministic per-pixel
# gradient into it and spins. The harness scans the framebuffer out through the
# VI and pins the result as a golden frame (T-33-006 — the first *real ROM*
# renders a frame).
#
# This is our own code (MIT OR Apache-2.0), so — unlike the gitignored external
# test corpus — the assembled `.z64` is committable. It boots only through the
# harness direct-load path (`rom::load_direct`, which copies the payload to
# 0x8000_1000 and jumps there); it carries no real IPL3, so it does not boot on
# hardware. That is deliberate: it is a rasteriser/scan-out fixture, not a game.
#
# Registers (o32): VI base 0xA440_0000 (KSEG1, uncached). Framebuffer 0xA020_0000
# (KSEG1 -> physical 0x0020_0000, which VI_ORIGIN points at). 32*24 = 768 pixels.

.set noreorder
.set noat
.section .text
.globl _start
_start:
        lui     $t0, 0xA440             # VI register block base (KSEG1)

        lui     $t1, 0x0020             # framebuffer physical base 0x0020_0000
        sw      $t1, 0x04($t0)          # VI_ORIGIN

        ori     $t2, $zero, 32          # width in pixels
        sw      $t2, 0x08($t0)          # VI_WIDTH

        ori     $t3, $zero, 48          # V_END=48, V_START=0 -> height (48/2)=24
        sw      $t3, 0x28($t0)          # VI_V_VIDEO

        ori     $t4, $zero, 2           # TYPE=2 -> 16-bit RGBA5551 (also VI-on)
        sw      $t4, 0x00($t0)          # VI_CTRL

        # Fill 768 pixels with a gradient: pixel i takes colour
        #   ((i & 0x1F) << 11) | 0x0001   -> red ramps 0..31 across each row of 32,
        # so the frame is a repeating horizontal red gradient with alpha=1. The
        # per-pixel arithmetic makes the golden prove the CPU actually ran (a
        # blank or solid frame could not distinguish "ran" from "never ran").
        lui     $t5, 0xA020             # framebuffer write pointer (uncached)
        ori     $t6, $zero, 0           # pixel index i = 0
        ori     $t7, $zero, 768         # pixel count
fill:
        andi    $t8, $t6, 0x1F          # i & 0x1F  (0..31)
        sll     $t8, $t8, 11            # -> R field (bits 15:11)
        ori     $t8, $t8, 0x0001        # set alpha bit (bit 0)
        sh      $t8, 0($t5)             # store RGBA5551 pixel
        addiu   $t5, $t5, 2
        addiu   $t6, $t6, 1
        bne     $t6, $t7, fill
        nop
spin:
        beq     $zero, $zero, spin      # done — halt in an infinite loop
        nop
