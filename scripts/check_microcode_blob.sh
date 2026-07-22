#!/usr/bin/env bash
# Verify the committed rdpq microcode blob matches its recorded checksum.
#
# The blob (crates/rustyn64-test-harness/microcode/rsp_rdpq.bin) is a generated
# artifact — assembled from the vendored source in third_party/libdragon-rsp/ by
# that directory's assemble.sh (needs the mips64-elf toolchain). This gate is
# toolchain-free: it catches accidental corruption or a hand-edit of the blob by
# checking it against the committed SHA256SUMS. Regenerating the blob from source
# (which also refreshes SHA256SUMS) is the local step whenever the vendored
# source is re-pinned; see third_party/libdragon-rsp/NOTICE.md.
set -euo pipefail

dir="crates/rustyn64-test-harness/microcode"
cd "$(git rev-parse --show-toplevel)/$dir"

if [ ! -f SHA256SUMS ] || [ ! -f rsp_rdpq.bin ]; then
    echo "error: missing $dir/rsp_rdpq.bin or SHA256SUMS" >&2
    exit 1
fi

sha256sum -c SHA256SUMS
echo "microcode blob matches its recorded checksum"
