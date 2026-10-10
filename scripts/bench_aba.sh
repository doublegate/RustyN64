#!/usr/bin/env bash
# Decide the idle batch with NO COMPILATION inside the measurement window.
#
# Why this harness exists. The three previous attempts were all contaminated,
# and the cause was self-inflicted: each leg ran `cargo build` and then measured.
# A parallel release build is itself a multi-core job, and `/proc/loadavg` is a
# 1-minute exponential average, so it is still decaying from MY OWN build when
# the timing starts. That is why the first leg after every link came in high and
# read like a "cold start", and why a load gate checked right after the build
# passed and then aborted mid-run.
#
# So: build both binaries first, stash them, wait for quiet ONCE, then alternate
# with nothing but the benchmark running.
set -uo pipefail
cd /home/parobek/Code/OSS_Public-Projects/RustyN64

# Commercial ROMs are never committed; point this at a local dump directory.
ROMDIR="${RUSTYN64_ROMDIR:?set RUSTYN64_ROMDIR to a directory of .z64 dumps}"
OUT=/tmp/rustyn64-bench
BIN=target/release/examples/frame_bench

# $1 = leg name, $2 = a directory mirroring the crates/ paths this leg changes.
build_into() {
  (cd "$2" && find . -type f -name '*.rs' -print0) | while IFS= read -r -d "" f; do
    cp "$2/$f" "$f"
  done
  cargo build --release --example frame_bench --features fast-exec,fast-scheduler >/dev/null 2>&1 \
    || { echo "$1 BUILD FAILED"; exit 1; }
  cp "$BIN" "$OUT/frame_bench.$1"
}

echo "building both legs (outside the measurement window)"
# Point these at snapshots of the two trees being compared. Snapshot with `cp`,
# never `git checkout` -- that would discard any unrelated uncommitted work.
build_into A "${A_SNAPSHOT:?set A_SNAPSHOT to a baseline source snapshot dir}"
build_into B "${B_SNAPSHOT:?set B_SNAPSHOT to a candidate source snapshot dir}"

echo "waiting for the build's own load to decay"
for _ in $(seq 1 90); do
  L=$(cut -d' ' -f1 /proc/loadavg)
  awk -v l="$L" 'BEGIN {exit !(l < 0.7)}' && break
  sleep 20
done
L=$(cut -d' ' -f1 /proc/loadavg)
if awk -v l="$L" 'BEGIN {exit !(l >= 0.7)}'; then
  echo "STILL BUSY: load $L after 30 min — no numbers reported"
  exit 1
fi
echo "start load: $L"

# Warm-up, discarded: first touch of each freshly-copied binary.
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
  echo "  (load after round $i: $(cut -d' ' -f1 /proc/loadavg))"
done
echo "end load: $(cut -d' ' -f1 /proc/loadavg)"
