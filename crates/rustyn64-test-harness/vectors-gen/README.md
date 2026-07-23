# RDP conformance vector generator (T-33-005)

This is the **licence-clean** golden-vector generator for the ParaLLEl-RDP /
Angrylion conformance gate (`../tests/rdp_conformance.rs`).

## What it does

`driver.c` drives the **Angrylion** software RDP (`angrylion-rdp-plus`, CPU-only,
the accuracy oracle) over a set of hand-written RDP command lists and dumps each
rendered framebuffer as a self-describing `.rvec` vector into
`../tests/vectors/`. The RustyN64 test harness then replays the *same* command
stream through its own RDP and asserts a byte-for-byte match.

## Why the source isn't committed

Angrylion-rdp-plus is under the **non-commercial MAME licence** — it is study-only
and must not be vendored into this MIT / Apache-2.0 project (see
`ref-proj/README.md`). So:

- **Committed:** this `driver.c` (our own MIT code — it only *calls* Angrylion's
  public API, contains no Angrylion source) and the emitted `.rvec` vectors
  (Angrylion's rendered **output** — a plain command-stream blob plus a golden
  framebuffer, both freely committable). Angrylion stays an external *output*
  oracle: outputs, never source.
- **Not committed:** Angrylion itself (fetched into the gitignored `ref-proj/`
  tree) and the built `driver` binary.

## Provenance (pin this to reproduce the corpus)

The committed vectors were generated against **`angrylion-rdp-plus` commit
`31bdb1f0a79dd726017a38432540c6b5db0fa117`** (the revision the `ref-proj/parallel-rdp`
submodule pins). Angrylion is deterministic, so regenerating at that commit reproduces the
goldens byte-for-byte; a *different* Angrylion revision could shift them, which would be a
reviewed golden change, not a silent one.

## Regenerating the vectors

```sh
# 1. Fetch the oracle into the gitignored ref-proj tree (once), at the pinned commit above.
git -C ../../../ref-proj/parallel-rdp submodule update --init --depth 1 angrylion-rdp-plus

# 2. Build the generator (needs a C/C++ toolchain + pthreads; no Vulkan/OpenGL).
make

# 3. Emit the vectors into the committed vectors dir.
./driver ../tests/vectors
```

Rendering is fully deterministic (`parallel = false`, no wall-clock / OS RNG), so
the same command list always yields byte-identical output.

## The `.rvec` format

A 9×`u32` big-endian header — magic `"RVEC"`, version, `fb_addr`, `width`,
`height`, `bpp`, `cmd_addr`, `cmd_len`, `fb_len` — followed by the command-list
bytes (big-endian words, matching RustyN64's RDRAM layout) and the golden
framebuffer (row-major big-endian logical pixel values, which is exactly what
RustyN64 writes into RDRAM, so the harness compares the framebuffer region
directly).

## Adding a vector

Add a command-list array + a `Vector` entry in `driver.c`, rebuild, re-run, and
reference the new `.rvec` from a test in `../tests/rdp_conformance.rs`.

**Always build a triangle's attribute blocks with the `SHADE_BLOCK` /
`SHADE_BLOCK_FLAT` / `Z_SUFFIX` macros**, never by hand. A `Fill Triangle`'s shade
/ texture / z block must be its full length (16 words for shade/tex, 4 for z); a
short block silently shortens the command, misaligns the following blocks, and
makes Angrylion render a **blank** frame — a bug that once cost hours. The macros
expand to the exact word count, so a block cannot be short by construction.

When adding a vector, regenerate into a scratch dir first and confirm the existing
vectors come back **byte-identical** (`cmp`) before overwriting `../tests/vectors/`
— that proves your change didn't perturb the committed goldens.
