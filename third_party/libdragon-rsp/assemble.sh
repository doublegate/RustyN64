#!/usr/bin/env bash
# Regenerate the committed microcode blobs from the vendored source.
#
# Requires the mips64-elf cross toolchain on PATH (mips64-elf-gcc / -objcopy).
# On Arch/CachyOS: `paru -S mips64-elf-binutils mips64-elf-gcc`.
#
# The blobs are *generated artifacts*: never hand-edit them. Edit the vendored
# source (kept byte-identical to upstream — see NOTICE) or, more usually, re-pin
# to a new upstream commit, then re-run this script and commit both together.
#
# Output (all reproducible, no machine-specific content):
#   <repo>/crates/rustyn64-test-harness/microcode/rsp_rdpq.bin    the rdpq DMEM
#     (0x0000..) + IMEM (0x1000..) image laid out by rsp.ld, flattened by objcopy
#   .../symbols.txt                the rdpq boot-relevant symbol addresses (nm)
#   .../rsp_mixer.bin              the audio-mixer overlay blob (same layout)
#   .../rsp_mixer.symbols.txt      the mixer symbol addresses
#   .../SHA256SUMS                 integrity of both blobs
# The linker .map is intentionally NOT committed: it embeds machine-specific temp
# object paths and is therefore not reproducible.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
out="$here/../../crates/rustyn64-test-harness/microcode"
mkdir -p "$out"

CC="${MIPS64_ELF_GCC:-mips64-elf-gcc}"
OBJCOPY="${MIPS64_ELF_OBJCOPY:-mips64-elf-objcopy}"
NM="${MIPS64_ELF_NM:-mips64-elf-nm}"

for tool in "$CC" "$OBJCOPY" "$NM"; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "error: $tool not found on PATH (install the mips64-elf toolchain)" >&2
        exit 1
    }
done

# Assemble one overlay: $1 = source basename, $2 = blob name, $3 = symbols name.
# Each overlay `#include`s <rsp_queue.inc>, so the blob is the full RSPQ kernel +
# that overlay — a standalone bootable DMEM+IMEM image.
build_overlay() {
    local src="$1" blob="$2" syms="$3"
    local elf; elf="$(mktemp)"
    # Same flags libdragon's n64.mk uses for RSP ucode assembly.
    "$CC" -march=mips1 -mabi=32 -Wa,--fatal-warnings \
        -I"$here/include" -I"$here/src" \
        -nostartfiles -Wl,-T"$here/rsp.ld" -Wl,--gc-sections \
        -o "$elf" "$here/src/$src"
    "$OBJCOPY" -O binary "$elf" "$out/$blob"
    "$NM" -n "$elf" > "$out/$syms"
    rm -f "$elf"
    echo "wrote $out/$blob ($(stat -c%s "$out/$blob") bytes) + $syms"
}

build_overlay rsp_rdpq.S  rsp_rdpq.bin  symbols.txt
build_overlay rsp_mixer.S rsp_mixer.bin rsp_mixer.symbols.txt

( cd "$out" && sha256sum rsp_rdpq.bin rsp_mixer.bin > SHA256SUMS )
echo "wrote $out/SHA256SUMS (both blobs)"
