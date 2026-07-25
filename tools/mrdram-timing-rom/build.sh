#!/bin/sh
# Assemble the M(RDRAM) cached-load timing ROM with bass (ARM9 fork).
#
# bass is not vendored. Fetch + build it once (it needs one modern-g++ fix), then
# point BASS at the binary. The n64 architecture tables must sit next to the
# binary (bass looks in <binary-dir>/architectures/).
#
#   git clone --depth 1 https://github.com/ARM9/bass.git
#   # modern g++: nall/arithmetic/natural.hpp throws std::runtime_error without
#   # including <stdexcept> -- add `#include <stdexcept>` at its top.
#   ( cd bass/bass && make )
#   mkdir -p bass/bass/out/architectures
#   cp bass/bass/data/architectures/*.arch bass/bass/out/architectures/
#   export BASS=$PWD/bass/bass/out/bass
#
# Then, from this directory:
#   sh build.sh
set -e
cd "$(dirname "$0")"          # run from this directory regardless of caller
: "${BASS:=bass}"
"$BASS" mrdram_timing.asm
echo "built mrdram_timing.z64 ($(wc -c < mrdram_timing.z64) bytes)"
echo "For hardware: fix the header CRC (e.g. chksum64) and load via your flashcart."
