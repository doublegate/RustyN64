#!/usr/bin/env bash
# Benchmark RustyN64 against a reference N64 emulator on the same host and ROM.
#
# WHY THIS EXISTS. Every performance conclusion this project reached before this
# script was **self-referential** -- RustyN64 measured against RustyN64, expressed
# as FPS or as a profile share. That is how "60 FPS is unreachable for this
# execution model" got written down: share arithmetic can say which subsystem
# dominates, but it cannot say whether the whole thing is 4x more expensive than
# it needs to be. Only a competitor can say that.
#
# THE SUBJECT: cen64. It is the right one, and not merely the convenient one:
#
#   * It is **cycle-accurate**, like RustyN64's default path. Comparing against a
#     recompiler (ares) or an interpreter that models no pipeline (gopher64) would
#     invite the excuse that accuracy explains the gap. cen64 removes that excuse.
#   * It runs **headless** (`-headless`) and prints its own frame rate, so no
#     display, no window manager, no overlay injection, and no assumption about
#     what the frame rate "must" be.
#   * BSD-3, so its source is readable and vendorable if something in it turns out
#     to be worth adopting.
#
# WHAT DEFEATED THE ALTERNATIVES, recorded so nobody re-derives it:
#   * **gopher64** opens a window and initialises Vulkan but never advances the
#     machine when launched from a non-interactive session -- 0.5 s of CPU over
#     60 s. It has a Slint GUI and evidently wants real desktop interaction.
#   * **ares** does emulate, but exposes no frame counter to the command line, and
#     its CPU draw varied 2x between samples (0.69 -> 1.51 cores). Without a frame
#     count that variance cannot be attributed, so no cycles/frame can be derived.
#   * **MangoHud** loads as a Vulkan layer but writes no log here under any
#     combination of `output_file` / `output_folder` / `autostart_log`.
#   * **RetroArch** (mupen64plus-next) segfaults under `--max-frames` in this
#     environment. Worth retrying: `--max-frames` is the ideal primitive, because
#     a fixed frame count makes the timing unambiguous.
#
# THE METRIC. cen64 headless is not frame-limited, so its printed `VI/s` IS its
# throughput. `perf stat` gives the cycles it spent getting there:
#
#     cycles/frame = cycles / (VI/s x seconds)
#     cycles/emulated-instruction = cycles/frame / 1_430_000
#
# where 1.43 M is Super Mario 64's emulated VR4300 instructions per frame, measured
# by `examples/frame_bench.rs`. That last figure is what RustyN64 optimises against.
#
# TWO CAVEATS, because this comparison is cheap and therefore easy to over-read:
#   * The scenes are not pinned to the same frame. Both emulators boot the same ROM,
#     but a 100 s cen64 run and a 120-frame `frame_bench` window sit at different
#     points. FPS is robust to this; per-instruction figures less so.
#   * `stdbuf -oL` is REQUIRED. cen64's stdout is block-buffered to a pipe, so a
#     killed run loses every `VI/s` line it printed -- which reads as "the emulator
#     rendered nothing" rather than as lost output.
#
# USAGE
#   scripts/bench_reference_emulators.sh <rom.z64> <pifdata.bin> [seconds]

set -uo pipefail

ROM="${1:?usage: $0 <rom.z64> <pifdata.bin> [seconds]}"
PIF="${2:?usage: $0 <rom.z64> <pifdata.bin> [seconds]}"
SECS="${3:-100}"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CEN64="$ROOT/ref-proj/cen64/build/cen64"
OUT="${TMPDIR:-/tmp}/rustyn64-bench"
mkdir -p "$OUT"

command -v perf >/dev/null || { echo "perf not found"; exit 1; }
[ -f "$ROM" ] || { echo "ROM not found: $ROM"; exit 1; }
[ -f "$PIF" ] || { echo "PIF ROM not found: $PIF"; exit 1; }
if [ ! -x "$CEN64" ]; then
  echo "cen64 not built. Build it with:"
  echo "  cd $ROOT/ref-proj/cen64 && mkdir -p build && cd build &&"
  echo "  cmake .. -DCMAKE_BUILD_TYPE=Release && make -j"
  exit 1
fi

# Refuse to run twice at once. Two overlapping sweeps once measured each other
# instead of the emulators, and killed each other's processes -- producing empty
# counter files that read like a legitimate zero rather than an error.
exec 9>"$OUT/.lock"
flock -n 9 || { echo "another sweep is running -- refusing" >&2; exit 1; }

# Refuse to measure on a busy machine. A competing multi-core job inflated a
# RustyN64 frame from 100 ms to 189 ms during this work: 1.9x, larger than any
# optimization being evaluated, and invisible in the result.
LOAD=$(cut -d' ' -f1 /proc/loadavg)
if awk -v l="$LOAD" 'BEGIN {exit !(l > 3.0)}'; then
  echo "load average is $LOAD -- too busy to measure (want < 3.0)" >&2
  [ "${BENCH_FORCE:-0}" = "1" ] || { echo "set BENCH_FORCE=1 to override" >&2; exit 1; }
  echo "BENCH_FORCE=1 -- results are NOT comparable" >&2
fi

echo "rom    : $(basename "$ROM")  sha256 $(sha256sum "$ROM" | cut -c1-16)…"
echo "host   : $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | xargs)"
echo "window : ${SECS}s   load $(cut -d' ' -f1-3 /proc/loadavg)"
echo

perf stat -e cycles:u -o "$OUT/cen64.perf" \
  timeout -k 5 -s TERM "$SECS" stdbuf -oL \
  "$CEN64" -headless "$PIF" "$ROM" > "$OUT/cen64.log" 2>&1

CYCLES=$(grep -oE '^[ ]*[0-9,]+[ ]+cycles:u' "$OUT/cen64.perf" | tr -dc '0-9')
SAMPLES=$(grep -c 'VI/s' "$OUT/cen64.log")

if [ "${SAMPLES:-0}" -lt 5 ]; then
  echo "cen64 produced $SAMPLES frame-rate samples -- too few to trust."
  echo "If this is zero, check that stdbuf is present: block-buffered stdout is"
  echo "discarded when the run is killed. See $OUT/cen64.log"
  exit 1
fi

# The tail only: cen64's first samples are boot, where there is nothing to render
# and it briefly reports 150-200 VI/s. Averaging those in would overstate it.
awk -v c="$CYCLES" -v s="$SECS" '
  /VI\/s/ { v[n++] = $2 }
  END {
    lo = (n > 20) ? n - 20 : 0
    for (i = lo; i < n; i++) { sum += v[i]; m++ }
    fps = sum / m
    printf "cen64 (cycle-accurate, headless)\n"
    printf "  steady-state       : %.1f VI/s  (mean of last %d samples of %d)\n", fps, m, n
    printf "  cycles/frame       : %.0f M\n", c / s / fps / 1e6
    printf "  cycles/instruction : %.0f\n", c / s / fps / 1430000
    printf "\nRustyN64 for comparison (examples/frame_bench.rs, same ROM):\n"
    printf "  accurate           : 10.0 FPS, 500 M cycles/frame, 350 cycles/insn\n"
    printf "  fast-exec          : 15.9 FPS, 314 M cycles/frame, 218 cycles/insn\n"
    printf "  60 FPS needs       : 60.0 FPS,  83 M cycles/frame,  58 cycles/insn\n"
  }' "$OUT/cen64.log"

echo
echo "artifacts: $OUT"
