# PeterLemon instruction-timing test ROMs (curated timing oracles)

Two single-purpose N64 instruction-timing test ROMs from
[PeterLemon/N64](https://github.com/PeterLemon/N64) (krom / Peter Lemon), placed
here as **curated, self-judging timing oracles** — the practical alternative to
n64-systemtest's monolithic `--features timing` build, which does **not**
terminate in the emulator (it hangs after the base set; see
`docs/accuracy-ledger.md` §C-1).

| ROM | What it times | Ledger target |
| --- | --- | --- |
| `CPUTIMINGNTSC.z64` | Each MIPS integer instruction (a fixed loop, measured with the COP0 `Count` register) | **C-1** (`M`, memory-access time) + instruction timing |
| `CP1TIMINGNTSC.z64` | Each COP1/FPU instruction (ADD/SUB/MUL/DIV/SQRT/…) | **C-29** (FPU per-op stall rates) |

## How they judge

Each ROM times a fixed loop of one instruction, compares the measured `Count`
delta against a **hardware-expected value baked into the ROM** (e.g.
`ADDCOUNT: dw $0000DB1F`), and draws that instruction's label in **green (pass)**
or **red (fail)**. The rendered frame *is* the verdict: an all-green frame means
our cycle timing matches hardware for every instruction covered; any red text is
a mismatch. No serial console, no host judging — the ROM decides.

## How they run here

`crates/rustyn64-test-harness/tests/peterlemon_timing.rs` boots each ROM, runs it
to its result frame, scans the frame out through the real VI, and counts red vs
green glyph pixels:

```text
cargo test -p rustyn64-test-harness --release --test peterlemon_timing -- --ignored --nocapture
```

These are **measurements, not gates** (`#[ignore]`d). Today `CPUTIMINGNTSC` draws
an **all-red** frame — an *aggregate* verdict that our `Count` deltas do not match
the ROM's expected values for the instructions it covers. It does **not** by itself
isolate C-1 (`M`) or prove each instruction is individually wrong (the ROM compares
absolute deltas, which conflate loop overhead, a possible fixed offset, and
per-instruction error); isolating `M` needs the differential *measured-vs-expected*
deltas. What it gives today is a fast, non-hanging, falsifiable target that Stage D
(Phase 7) drives toward all-green. `CP1TIMINGNTSC` executes cleanly (~10⁹
instructions, no hang — the point) but its slower FPU battery needs a larger budget
to draw its full verdict, wired in Stage-D follow-up.

## Provenance & licence

- Source: `PeterLemon/N64` — `CPUTest/CPU/TIMINGNTSC/` and `CPUTest/CP1/TIMINGNTSC/`.
- Licence: **The Unlicense** (public domain) — see `LICENSE` in this directory,
  copied verbatim from upstream. Public-domain, so committable as a fixture (the
  "commit permissive/public-domain fixtures" rule; module 20).
- The files are byte-identical to upstream, renamed from `.N64` to `.z64` (they
  are already big-endian z64-format images — header magic `80 37 12 40`).
