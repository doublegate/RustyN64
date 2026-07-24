#!/usr/bin/env bash
# Assemble the homebrew fixtures into committable big-endian .z64 images for the
# RustyN64 harness. Requires a bare-metal MIPS toolchain (mips64-elf-* or set
# MIPS_CC/MIPS_OBJCOPY).
#
# The .z64 layout the harness direct-load expects:
#   0x000..0x040  cartridge header (magic + entry point at 0x08)
#   0x040..0x1000 IPL3 area (zero here — the harness skips it)
#   0x1000..      the assembled code, linked at virtual 0x8000_1000
#
# Outputs (checked in; regenerate with this script):
#   render_fill.z64  — CPU -> RDRAM -> VI scan-out fixture (T-33-006)
#   audio_play.z64   — CPU -> RDRAM -> AI DMA fixture (Phase 4)
set -euo pipefail
cd "$(dirname "$0")"

CC="${MIPS_CC:-mips64-elf-gcc}"
OBJCOPY="${MIPS_OBJCOPY:-mips64-elf-objcopy}"

# The code is position-independent (only PC-relative branches and immediate
# constants — no absolute j/jal), so it is LINKED at a low address purely so
# `objcopy -O binary` does not try to pad the file out to a KSEG0 offset. The
# byte stream is identical to one linked at the real entry; the header records
# the true entry point (0x8000_1000) the harness jumps to.
LINK_ADDR=0x1000

command -v "$CC" >/dev/null || { echo "need $CC (set MIPS_CC)"; exit 1; }
command -v "$OBJCOPY" >/dev/null || { echo "need $OBJCOPY (set MIPS_OBJCOPY)"; exit 1; }
command -v python3 >/dev/null || { echo "need python3 for the .z64 packaging step"; exit 1; }

# Build one fixture: $1 = source stem, $2 = 20-byte internal ROM name.
build_rom() {
    local stem="$1" name="$2"
    trap "rm -f '$stem.elf' '$stem.bin'" RETURN

    "$CC" -march=vr4300 -mabi=32 -mno-abicalls -fno-pic -EB -nostdlib \
          -Wl,-Ttext="$LINK_ADDR" -Wl,-e,_start -o "$stem.elf" "$stem.s"
    "$OBJCOPY" -O binary --only-section=.text "$stem.elf" "$stem.bin"

    ROM_STEM="$stem" ROM_NAME="$name" python3 - <<'PY'
import os, struct
stem = os.environ["ROM_STEM"]
name = os.environ["ROM_NAME"].encode()
code = open(f"{stem}.bin", "rb").read()
hdr = bytearray(0x1000)                            # header + IPL3 area, zero-filled
hdr[0x00:0x04] = bytes([0x80, 0x37, 0x12, 0x40])   # PI BSD DOM1 config (z64 magic)
hdr[0x04:0x08] = struct.pack(">I", 0x0000000F)     # clock rate
hdr[0x08:0x0C] = struct.pack(">I", 0x80001000)     # entry point (sign-extends in the loader)
hdr[0x0C:0x10] = struct.pack(">I", 0x00001444)     # release
assert len(name) <= 20, "internal name must fit 20 bytes"
hdr[0x20:0x20 + len(name)] = name                  # 20-byte internal name
rom = bytes(hdr) + code
if len(rom) % 2:                                   # halfword-align the image
    rom += b"\x00"
open(f"{stem}.z64", "wb").write(rom)
print(f"wrote {stem}.z64 ({len(rom)} bytes, {len(code)} bytes of code)")
PY
}

build_rom render_fill "RUSTYN64 RENDER FILL"
build_rom audio_play  "RUSTYN64 AUDIO PLAY"
