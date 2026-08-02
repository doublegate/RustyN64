#!/usr/bin/env bash
set -uo pipefail
cd /home/parobek/Code/OSS_Public-Projects/RustyN64
run_leg() {
  cp "$2" crates/rustyn64-core/src/scheduler.rs
  cp "$3" crates/rustyn64-cpu/src/lib.rs
  cp "$4" crates/rustyn64-cpu/src/cop0.rs
  cargo build --release --example frame_bench --features fast-exec,fast-scheduler >/dev/null 2>&1 || { echo "$1 BUILD FAILED"; return 1; }
  for t in "Super Mario 64" "Banjo-Kazooie"; do
    printf "%s %-15.15s " "$1" "$t"
    RUSTYN64_PROBE_ROM="/tmp/claude-1000/-home-parobek-Code-OSS-Public-Projects-RustyN64/3c159115-07d9-4b69-a1e2-ea68c3260bd6/scratchpad/rom/$t.z64" ./target/release/examples/frame_bench | head -1
  done
}
for i in 1 2; do
  run_leg "A$i" /tmp/rustyn64-bench/sched.base /tmp/rustyn64-bench/cpulib.head /tmp/rustyn64-bench/cop0.head
  run_leg "B$i" /tmp/rustyn64-bench/sched.batch /tmp/rustyn64-bench/cpulib.batch /tmp/rustyn64-bench/cop0.batch
done
cp /tmp/rustyn64-bench/sched.batch crates/rustyn64-core/src/scheduler.rs
cp /tmp/rustyn64-bench/cpulib.batch crates/rustyn64-cpu/src/lib.rs
cp /tmp/rustyn64-bench/cop0.batch crates/rustyn64-cpu/src/cop0.rs
