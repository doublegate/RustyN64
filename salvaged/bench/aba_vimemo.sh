#!/usr/bin/env bash
# A-B-A-B for the VI cov_cells memo. A = HEAD (no second memo layer),
# B = the memo. Interleaved so session drift lands on both legs.
set -uo pipefail
cd /home/parobek/Code/OSS_Public-Projects/RustyN64
ROM="/tmp/claude-1000/-home-parobek-Code-OSS-Public-Projects-RustyN64/3c159115-07d9-4b69-a1e2-ea68c3260bd6/scratchpad/rom/Super Mario 64.z64"
SRC=crates/rustyn64-core/src/bus.rs
FEAT="fast-exec,fast-scheduler"

run_leg() {
  local name="$1" snap="$2"
  cp "$snap" "$SRC"
  cargo build --release --example frame_bench --features "$FEAT" >/dev/null 2>&1 || { echo "$name BUILD FAILED"; return 1; }
  RUSTYN64_PROBE_ROM="$ROM" ./target/release/examples/frame_bench | sed "s/^/$name /"
}

for i in 1 2; do
  run_leg "A$i" /tmp/rustyn64-bench/bus.preVImemo
  run_leg "B$i" /tmp/rustyn64-bench/bus.withVImemo
done

# Leave the tree on the memo version.
cp /tmp/rustyn64-bench/bus.withVImemo "$SRC"
