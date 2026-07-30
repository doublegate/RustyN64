# VR4300 cache-fill timing ROMs

Two bare-metal N64 ROMs that **measure the VR4300 cache line-fill costs on real
hardware** — the memory latencies that accuracy-ledger **C-1** currently carries
as values *fitted* from ares/cen64 (no hardware oracle exists in the emulation
community's test corpus). Run them on a console and they yield the real numbers.

- **`mrdram_timing.z64`** — the **D-cache** line-fill cost (a differential of
  cached loads that miss vs. hit).
- **`icache_timing.z64`** — the **I-cache** line-fill cost (a straight-line
  instruction block larger than the 16 KiB I-cache, so every fetch line misses).

The D-cache ROM is described in full below; the I-cache ROM is its companion and
shares the build, header convention, and ISViewer output — see
[I-cache variant](#i-cache-variant) at the end.

## What the D-cache ROM measures, and how

The D-cache miss cost is `8..=9 + M(RDRAM)` PClocks (VR4300 User's Manual
Table 11-1). This ROM isolates it with a **differential**, timed by the COP0
`Count` register:

- **Miss loop:** `N = 1024` cached loads striding one D-cache line (16 B) across a
  16 KiB region — twice the 8 KiB D-cache — so every load misses.
- **Hit loop:** `N` loads from one address, which hit after the first fill.
- `delta_miss - delta_hit ≈ N × fill_cost`. `Count` ticks once per **2 PClocks**,
  so `M_fill (PClocks) = (delta_miss - delta_hit) / N × 2`.

The base per-load and loop overhead cancel in the differential, so what is left
is the fill cost alone.

## Output

The ROM writes four 32-bit words to uncached RDRAM at physical `0x2000`
(`delta_miss`, `delta_hit`, `delta_miss - delta_hit`, `N`) **and** prints the
first three as hex via the **ISViewer** text channel (`0x13FF_0000`), which
EverDrive-64 / 64drive expose to a connected PC. So you can read the result
either way. `fill_cost = word[2] / word[3] × 2` PClocks.

## Build

`bass` (ARM9 fork) is not vendored; see `build.sh` for the one-time fetch/build
(it needs a one-line `#include <stdexcept>` fix on modern g++) and the
architecture-table placement. Then:

```sh
BASS=/path/to/bass sh build.sh   # -> mrdram_timing.z64 (32 KiB) + icache_timing.z64 (96 KiB)
```

Both assembled ROMs are committed for convenience.

## Verify in the emulator

`crates/rustyn64-test-harness/tests/mrdram_timing_rom.rs` boots the ROM through
`load_direct` and reads the result words. In the emulator it necessarily reads
back **our** charged D-cache fill (`M_DCACHE_FILL` = 40), which both proves the
ROM's measurement path is correct end-to-end and guards the constant:

```sh
cargo test -p rustyn64-test-harness --release --test mrdram_timing_rom -- --nocapture
# -> D-cache fill = 39.96 PClocks
```

## Run on hardware

1. **CRC**: the header CRC1/CRC2 are zero; fix them for a real console
   (`chksum64 mrdram_timing.z64`, or your flashcart's auto-fix).
2. **Boot**: the ROM uses entry `0x8000_1000` with code at ROM `0x1000` (the
   PeterLemon / emulator convention). The IPL3 region is intentionally **blank** —
   Nintendo's IPL3 is copyrighted and is not embedded. Load it through a flashcart
   that supplies a bootcode for homebrew (EverDrive-64, 64drive, SummerCart64); if
   yours expects the standard `0x8000_0400` load address, prepend an open bootcode
   (e.g. libdragon's IPL3) configured for `0x8000_1000`, or adjust the entry to
   match your loader.
3. **Read the result** via ISViewer (or your flashcart's RAM viewer at phys
   `0x2000`), compute `word[2] / word[3] × 2`, and that is the real
   `M(RDRAM)`-inclusive D-cache fill in PClocks. Drop it into ledger C-1 and the
   emulator's `Pipeline::M_DCACHE_FILL`, and the fitted value becomes a measured
   one.

## I-cache variant

`icache_timing.asm` → `icache_timing.z64` measures the **I-cache** line-fill cost
(`M_ICACHE_FILL`, ledger C-1, currently fitted at 46 PClocks). Rather than a
load differential it runs a **straight-line block of `N = 8192` `addiu`
instructions** (32 KiB, larger than the 16 KiB I-cache), so every 32-byte fetch
line (8 instructions) misses. Each `addiu` has no interlock, so its execute cost
is exactly one PClock — the base that is subtracted:

```text
fill_PClocks = (delta * 2 - N) / (N / 8)
```

(`delta` is the COP0-`Count` span of the block; `Count` ticks once per 2
PClocks; there are `N/8` line fills.) It writes `delta` and `N` to uncached
RDRAM at phys `0x10000` / `0x10004` (past the 32 KiB code block) and prints both
via ISViewer, exactly like the D-cache ROM. Header convention, blank IPL3, and
hardware-run steps are identical — just read `delta` and `N` and apply the
formula. Its emulator runner is
`crates/rustyn64-test-harness/tests/icache_timing_rom.rs`:

```sh
cargo test -p rustyn64-test-harness --release --test icache_timing_rom -- --nocapture
# -> I-cache fill = 46.09 PClocks (our charged M_ICACHE_FILL)
```

The residual 0.09 over the charged 46 is fixed jal/jr/pipeline-fill overhead not
captured by the `N × 1` base, diluted across 1024 fills; on hardware the block
dominates identically, so the measured number is the real fill cost.

## License

Authored for RustyN64: MIT OR Apache-2.0 (the project's license). Built with
`bass` (ISC/public-domain lineage); no Nintendo code is included.
