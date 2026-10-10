#!/usr/bin/env bash
set -uo pipefail
cd /home/parobek/Code/OSS_Public-Projects/RustyN64
run_leg() {
  cp "$2" crates/rustyn64-cpu/src/pipeline/fastexec.rs
  cp "$3" crates/rustyn64-cpu/src/pipeline.rs
  cargo build --release --example frame_bench --features fast-exec,fast-scheduler >/dev/null 2>&1 || { echo "$1 BUILD FAILED"; return 1; }
  for r in "/tmp/claude-1000/-home-parobek-Code-OSS-Public-Projects-RustyN64/3c159115-07d9-4b69-a1e2-ea68c3260bd6/scratchpad/rom"/*.z64; do
    printf "%s %-22.22s " "$1" "$(basename "$r" .z64)"
    RUSTYN64_PROBE_ROM="$r" ./target/release/examples/frame_bench | head -1
  done
}
for i in 1 2; do
  run_leg "A$i" /tmp/rustyn64-bench/fastexec.base /tmp/rustyn64-bench/pipeline.head
  run_leg "B$i" /tmp/rustyn64-bench/fastexec.idle_real /tmp/rustyn64-bench/pipeline.idle_real
done
cp /tmp/rustyn64-bench/fastexec.idle_real crates/rustyn64-cpu/src/pipeline/fastexec.rs
cp /tmp/rustyn64-bench/pipeline.idle_real crates/rustyn64-cpu/src/pipeline.rs
