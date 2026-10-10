# salvaged/

Small, hand-written development artifacts rescued from a volatile `/tmp` before
a reboot would have wiped them. Nothing here is part of the build, and nothing
here is required to build, test, or run RustyN64.

Kept because each one is **hand-written and not reproducible** from the
repository: a one-off probe, an audit script, a patch, or the exact shell that
produced a number quoted in `docs/performance.md`.

## What is here

| Directory | Contents |
| --- | --- |
| `scripts/` | Repository audit helpers — the en-US spelling pass, a word-frequency audit, a code checker, a zip integrity check |
| `probes/` | One-off `.rs` / `.c` probes: struct sizes, segment translation, FIFO behavior, a SIMD availability check, a `Default` check |
| `patches/` | Two working diffs kept for their content, not to be applied |
| `bench/` | The A-B-A benchmark shells behind the measurements in `docs/performance.md` |

## `bench/` is provenance, not tooling

The productized harness is **`scripts/bench_aba.sh`** — use that one. The shells
here are the exact per-experiment scripts each recorded measurement was taken
with, kept so a number in `docs/performance.md` can be traced to the commands
that produced it.

Several of them **contain the measurement bug** described in that document:
they rebuild between legs, which biases whichever leg follows a build, because
a parallel release build is itself a multi-core job and `/proc/loadavg` is a
one-minute average still decaying when timing starts. They are kept *as the
record of how the affected numbers were taken*, not as a pattern to copy.
`measure_only.sh` and `aba_strict.sh` are the later, corrected shape.

## What was deliberately NOT salvaged

The scan surfaced ~1970 candidates and 612 "strong" ones. Almost all were strong
by **location** — they sat in this project's agent scratch tree, so the
directory name matched rather than anything about the file. Excluded on purpose:

- **Commercial ROMs** (~100 MB) and a framebuffer dump derived from one.
  `scripts/check_no_roms.sh` is the gate; this would have walked around it.
- **A 128 MB directory of synthetic test ROMs**, which the scan wanted to take
  as a single directory unit.
- **PGO profile data** (~37 MB) and rendered screenshots (~14 MB) — regenerable,
  and the screenshots are derived from commercial ROMs.
- **Benchmark output logs** (284 `.txt`) — every conclusion drawn from them is
  already written up in `docs/performance.md`, which is the durable record.
- **PR bodies and review replies** (214 `.md`) — that text lives on the pull
  requests.
- **Copies of tracked source** (`audio_*.rs`, `sched_*.rs`, `vi_B.rs`,
  `pipeline_orig.rs`, …) — recoverable with `git show`.
