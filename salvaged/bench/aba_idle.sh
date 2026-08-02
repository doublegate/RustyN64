#!/usr/bin/env bash
set -uo pipefail
cd /home/parobek/Code/OSS_Public-Projects/RustyN64
SRC=crates/rustyn64-cpu/src/pipeline/fastexec.rs
run_leg() {
  cp "$2" "$SRC"
  cargo build --release --example frame_bench --features fast-exec,fast-scheduler >/dev/null 2>&1 || { echo "$1 BUILD FAILED"; return 1; }
  for r in "/tmp/claude-1000/-home-parobek-Code-OSS-Public-Projects-RustyN64/3c159115-07d9-4b69-a1e2-ea68c3260bd6/scratchpad/rom"/"Super Mario 64.z64" "/tmp/claude-1000/-home-parobek-Code-OSS-Public-Projects-RustyN64/3c159115-07d9-4b69-a1e2-ea68c3260bd6/scratchpad/rom"/"Mario Kart 64.z64"; do
    RUSTYN64_PROBE_ROM="$r" ./target/release/examples/frame_bench | head -1 | sed "s|^|$1 $(basename "$r" .z64) |"
  done
}
for i in 1 2; do
  run_leg "A$i-base" /tmp/rustyn64-bench/fastexec.base
  run_leg "B$i-idle" /tmp/rustyn64-bench/fastexec.idleskip
done
cp /tmp/rustyn64-bench/fastexec.base "$SRC"
