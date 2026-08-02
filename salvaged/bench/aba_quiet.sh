#!/usr/bin/env bash
# Wait for a quiet machine, then A-B-A-B-A the idle batch. Refuses to report
# numbers taken under load: a competing job has already inflated one leg of this
# measurement by 60%, which is larger than the effect being measured.
set -uo pipefail
cd /home/parobek/Code/OSS_Public-Projects/RustyN64

for _ in $(seq 1 60); do
  L=$(cut -d' ' -f1 /proc/loadavg)
  awk -v l="$L" 'BEGIN {exit !(l < 1.5)}' && break
  sleep 20
done
L=$(cut -d' ' -f1 /proc/loadavg)
if awk -v l="$L" 'BEGIN {exit !(l >= 1.5)}'; then
  echo "STILL BUSY: load $L after 20 min — no numbers reported"
  exit 1
fi
echo "quiet: load $L"

run_leg() {
  cp "$2" crates/rustyn64-cpu/src/pipeline/fastexec.rs
  cp "$3" crates/rustyn64-cpu/src/lib.rs
  cp "$4" crates/rustyn64-core/src/scheduler.rs
  cargo build --release --example frame_bench --features fast-exec,fast-scheduler >/dev/null 2>&1 || { echo "$1 BUILD FAILED"; return 1; }
  # A discarded warm-up run: the first execution after a link is the one a busy
  # or cold machine distorts most, and it is not part of the comparison.
  RUSTYN64_PROBE_ROM="/tmp/claude-1000/-home-parobek-Code-OSS-Public-Projects-RustyN64/3c159115-07d9-4b69-a1e2-ea68c3260bd6/scratchpad/rom/Super Mario 64.z64" ./target/release/examples/frame_bench >/dev/null 2>&1
  for t in "Super Mario 64" "Banjo-Kazooie" "Mario Kart 64"; do
    printf "%s %-15.15s " "$1" "$t"
    RUSTYN64_PROBE_ROM="/tmp/claude-1000/-home-parobek-Code-OSS-Public-Projects-RustyN64/3c159115-07d9-4b69-a1e2-ea68c3260bd6/scratchpad/rom/$t.z64" ./target/release/examples/frame_bench | head -1
  done
}

A_FE=/tmp/rustyn64-bench/fe.head;      A_LIB=/tmp/rustyn64-bench/cpulib.head;  A_SCH=/tmp/rustyn64-bench/sched.base
B_FE=/tmp/rustyn64-bench/fe.batchtest; B_LIB=/tmp/rustyn64-bench/cpulib.batch; B_SCH=/tmp/rustyn64-bench/sched.batch
for i in 1 2; do
  run_leg "A$i" $A_FE $A_LIB $A_SCH
  run_leg "B$i" $B_FE $B_LIB $B_SCH
done
run_leg "A3" $A_FE $A_LIB $A_SCH
echo "final load: $(cut -d' ' -f1 /proc/loadavg)"
cp $B_FE crates/rustyn64-cpu/src/pipeline/fastexec.rs
cp $B_LIB crates/rustyn64-cpu/src/lib.rs
cp $B_SCH crates/rustyn64-core/src/scheduler.rs
