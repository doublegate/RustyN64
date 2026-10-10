#!/usr/bin/env bash
set -uo pipefail
cd /home/parobek/Code/OSS_Public-Projects/RustyN64
ROM="/tmp/claude-1000/-home-parobek-Code-OSS-Public-Projects-RustyN64/3c159115-07d9-4b69-a1e2-ea68c3260bd6/scratchpad/rom/Super Mario 64.z64"
SRC=crates/rustyn64-cpu/src/pipeline/fastexec.rs
run_leg() {
  cp "$2" "$SRC"
  cargo build --release --example frame_bench --features fast-exec,fast-scheduler >/dev/null 2>&1 || { echo "$1 BUILD FAILED"; return 1; }
  RUSTYN64_PROBE_ROM="$ROM" ./target/release/examples/frame_bench | head -1 | sed "s/^/$1 /"
}
for i in 1 2; do
  run_leg "A$i-1x" /tmp/rustyn64-bench/fastexec.base
  run_leg "B$i-2x" /tmp/rustyn64-bench/fastexec.double
done
cp /tmp/rustyn64-bench/fastexec.base "$SRC"
