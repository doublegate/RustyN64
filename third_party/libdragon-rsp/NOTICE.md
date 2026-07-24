# Vendored libdragon RSP microcode

This directory contains the **RSPQ command-queue kernel**, the **`rdpq`
graphics overlay**, and the **audio mixer overlay** from
[libdragon](https://github.com/DragonMinded/libdragon), vendored verbatim so
RustyN64 can boot real microcode on its RSP — the `rdpq` overlay to compare the
RDP command list it emits against a hardware-documentation-derived golden
(Phase 2 criterion 2), and the mixer overlay to produce PCM audio for the AI
(Phase 4). See `docs/adr/0008-microcode-boot-harness.md`,
`to-dos/phase-2-rsp-lle/sprint-4-microcode-boot.md`, and `docs/audio.md`.

## Provenance

- **Upstream:** <https://github.com/DragonMinded/libdragon>
- **Commit:** `35f85a0797324a5ed0c723203e33ab3c1da94fdd`
  ("fix: RDP passthrough mem corruption")
- **Licence:** The Unlicense (public domain) — see `LICENSE.md`. No restrictions
  and no attribution required; this NOTICE records provenance as a matter of the
  project's immutable-reference discipline, not obligation.

## Contents

The files are **byte-identical to upstream** and must stay so — this is a
mirror, not a fork. To update, re-pin to a new upstream commit, re-copy, and
regenerate the blob (below); never hand-edit them.

- `src/rsp_rdpq.S` — the `rdpq` overlay (from upstream `src/rdpq/`).
- `src/rsp_mixer.S` — the audio mixer overlay (from upstream `src/audio/`). Its
  `#include` closure is a subset of `rsp_rdpq.S`'s — only `<rsp_queue.inc>` (and
  what that pulls in), all already present in `include/`.
- `include/` — the exact `#include` closure of `rsp_rdpq.S` (traced by
  assembling): `rsp_queue.inc` (the kernel), `rsp.inc`, `rsp_dma.inc`,
  `rsp_assert.inc`, `rspq_constants.h`, `rdpq_constants.h`, `rdpq_macros.h`,
  `rsp_rdpq.inc`, `rsp_rdpq_tri.inc`. `regdef.h` and `stdint.h` come from the
  `mips64-elf` toolchain, not from here.
- `rsp.ld` — the RSP linker script (DMEM at `0x0000`, IMEM at `0x1000`).
- `assemble.sh` — regenerates the committed blob from this source.

## The generated blob

`crates/rustyn64-test-harness/microcode/rsp_rdpq.bin` + `symbols.txt`,
`rsp_mixer.bin` + `rsp_mixer.symbols.txt`, and `SHA256SUMS` are **generated
artifacts** — the DMEM+IMEM images the RSP loads, produced by `assemble.sh` with
the `mips64-elf` toolchain. Never hand-edit them; edit or re-pin this source and
re-run the script. A CI job verifies both blobs against `SHA256SUMS`.
