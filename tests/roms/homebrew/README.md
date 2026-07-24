# Homebrew ROM fixtures

Small N64 ROMs written **for this project** (MIT OR Apache-2.0, same as the rest
of the tree) and used as test fixtures. Because they are our own code — not a
commercial dump or an unlicensed corpus — the assembled `.z64` is **committed**
(the `.gitignore` negates `*.z64` for this directory, and `scripts/check_no_roms.sh`
allowlists it). Each ships its source and a build script; the `.z64` is the
committed build output, regenerable from the source.

These boot only through the harness direct-load path (`rustyn64_test_harness::rom::load_direct`,
which copies the payload to the entry point and jumps there). They carry no real
IPL3, so they do **not** boot on hardware — they are rasteriser / scan-out
fixtures, not games.

## `render_fill`

- **Source:** `render_fill.s` · **Build:** `./build.sh` (needs a bare-metal MIPS
  toolchain — `mips64-elf-gcc` / `mips64-elf-objcopy`, or set `MIPS_CC`/`MIPS_OBJCOPY`).
- **What it does:** programs the Video Interface for a 32×24, 16-bit (RGBA5551)
  framebuffer at physical `0x0020_0000`, then CPU-writes a per-pixel red gradient
  (`pixel i = ((i & 0x1F) << 11) | 1`) and spins.
- **Used by:** `crates/rustyn64-test-harness/tests/real_rom_frame.rs` — boots it on
  the real VR4300, runs to fill completion, scans out through the real VI, and
  asserts the exact gradient (T-33-006, the first real ROM to render a frame).

Rebuild after editing the source:

```sh
cd tests/roms/homebrew && ./build.sh
```
