#!/usr/bin/env bash
# Decide the idle batch on a genuinely quiet machine, or report nothing.
#
# The two previous attempts were both contaminated and the data said so rather
# than hiding it: the first had a leg 60% off, the second drifted 1.22 -> 2.63
# load with the A legs climbing monotonically (30.46 -> 31.39 -> 31.45) and one
# title's B legs spread 36%. Both effects are larger than the ~3-7% being
# measured, so neither run can decide anything.
#
# This one refuses to start above 0.8, aborts if the load climbs past 1.6
# mid-run, and discards a warm-up run after every link.
set -uo pipefail
cd /home/parobek/Code/OSS_Public-Projects/RustyN64

ROMDIR="/tmp/claude-1000/-home-parobek-Code-OSS-Public-Projects-RustyN64/3c159115-07d9-4b69-a1e2-ea68c3260bd6/scratchpad/rom"

for _ in $(seq 1 90); do
  L=$(cut -d' ' -f1 /proc/loadavg)
  awk -v l="$L" 'BEGIN {exit !(l < 0.8)}' && break
  sleep 20
done
L=$(cut -d' ' -f1 /proc/loadavg)
if awk -v l="$L" 'BEGIN {exit !(l >= 0.8)}'; then
  echo "STILL BUSY: load $L after 30 min — no numbers reported"
  exit 1
fi
echo "start load: $L"

run_leg() {
  cp "$2" crates/rustyn64-cpu/src/pipeline/fastexec.rs
  cp "$3" crates/rustyn64-cpu/src/lib.rs
  cp "$4" crates/rustyn64-core/src/scheduler.rs
  cargo build --release --example frame_bench --features fast-exec,fast-scheduler >/dev/null 2>&1 \
    || { echo "$1 BUILD FAILED"; return 1; }
  L=$(cut -d' ' -f1 /proc/loadavg)
  if awk -v l="$L" 'BEGIN {exit !(l > 1.6)}'; then
    echo "$1 ABORT: load rose to $L mid-run — remaining legs not reported"
    return 2
  fi
  # Discarded: the first execution after a link is the one a cold page cache or
  # a passing job distorts most, and it is not part of the comparison.
  RUSTYN64_PROBE_ROM="$ROMDIR/Super Mario 64.z64" ./target/release/examples/frame_bench >/dev/null 2>&1
  for t in "Super Mario 64" "Mario Kart 64"; do
    printf "%s %-15.15s " "$1" "$t"
    RUSTYN64_PROBE_ROM="$ROMDIR/$t.z64" ./target/release/examples/frame_bench | head -1
  done
}

A_FE=/tmp/rustyn64-bench/fe.head
A_LIB=/tmp/rustyn64-bench/cpulib.head
A_SCH=/tmp/rustyn64-bench/sched.base
B_FE=/tmp/rustyn64-bench/fe.batchtest
B_LIB=/tmp/rustyn64-bench/cpulib.batch
B_SCH=/tmp/rustyn64-bench/sched.batch

for i in 1 2 3; do
  run_leg "A$i" $A_FE $A_LIB $A_SCH || break
  run_leg "B$i" $B_FE $B_LIB $B_SCH || break
done
echo "end load: $(cut -d' ' -f1 /proc/loadavg)"

# Leave the tree on the batch; the decision to keep or revert is made from the
# numbers above, not here.
cp $B_FE crates/rustyn64-cpu/src/pipeline/fastexec.rs
cp $B_LIB crates/rustyn64-cpu/src/lib.rs
cp $B_SCH crates/rustyn64-core/src/scheduler.rs
