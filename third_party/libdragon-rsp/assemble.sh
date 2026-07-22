#!/usr/bin/env bash
# Regenerate the committed rdpq microcode blob from the vendored source.
#
# Requires the mips64-elf cross toolchain on PATH (mips64-elf-gcc / -objcopy).
# On Arch/CachyOS: `paru -S mips64-elf-binutils mips64-elf-gcc`.
#
# The blob is a *generated artifact*: never hand-edit it. Edit the vendored
# source (kept byte-identical to upstream — see NOTICE) or, more usually, re-pin
# to a new upstream commit, then re-run this script and commit both together.
#
# Output (all reproducible, no machine-specific content):
#   <repo>/crates/rustyn64-test-harness/microcode/rsp_rdpq.bin   the DMEM (0x0000..)
#     + IMEM (0x1000..) image laid out by rsp.ld, flattened by objcopy -O binary
#   .../symbols.txt   the boot-relevant symbol addresses (via nm, path-free)
#   .../SHA256SUMS    integrity of the blob
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

elf="$(mktemp)"
trap 'rm -f "$elf"' EXIT

# Same flags libdragon's n64.mk uses for RSP ucode assembly.
"$CC" -march=mips1 -mabi=32 -Wa,--fatal-warnings \
    -I"$here/include" -I"$here/src" \
    -nostartfiles -Wl,-T"$here/rsp.ld" -Wl,--gc-sections \
    -o "$elf" "$here/src/rsp_rdpq.S"

"$OBJCOPY" -O binary "$elf" "$out/rsp_rdpq.bin"
"$NM" -n "$elf" > "$out/symbols.txt"

( cd "$out" && sha256sum rsp_rdpq.bin > SHA256SUMS )
echo "wrote $out/rsp_rdpq.bin ($(stat -c%s "$out/rsp_rdpq.bin") bytes) + symbols.txt + SHA256SUMS"
