#!/usr/bin/env bash
# Measure two ALREADY-BUILT binaries. No compilation anywhere in this script.
#
# The absolute load gate was miscalibrated: `frame_bench` is itself a running
# process, so `/proc/loadavg` sits near 1.0 *because of the measurement* and a
# "< 0.7 before starting" rule can never be satisfied on a machine that is
# otherwise idle. The previous clean run started at 0.63 and ended at 1.46 with
# sub-1% leg spreads — most of that rise was the benchmark itself.
#
# So the gate here is loose, and the DATA is what disqualifies a run: three
# interleaved rounds, and leg spreads reported alongside the means.
set -uo pipefail
ROMDIR="/tmp/claude-1000/-home-parobek-Code-OSS-Public-Projects-RustyN64/3c159115-07d9-4b69-a1e2-ea68c3260bd6/scratchpad/rom"
OUT=/tmp/rustyn64-bench

for _ in $(seq 1 45); do
  L=$(cut -d' ' -f1 /proc/loadavg)
  awk -v l="$L" 'BEGIN {exit !(l < 1.1)}' && break
  sleep 20
done
echo "start load: $(cut -d' ' -f1 /proc/loadavg)"

for leg in A B; do
  RUSTYN64_PROBE_ROM="$ROMDIR/Super Mario 64.z64" "$OUT/frame_bench.$leg" >/dev/null 2>&1
done

for i in 1 2 3; do
  for leg in A B; do
    for t in "Super Mario 64" "Mario Kart 64"; do
      printf "%s%d %-15.15s " "$leg" "$i" "$t"
      RUSTYN64_PROBE_ROM="$ROMDIR/$t.z64" "$OUT/frame_bench.$leg" | head -1
    done
  done
done
echo "end load: $(cut -d' ' -f1 /proc/loadavg)"
