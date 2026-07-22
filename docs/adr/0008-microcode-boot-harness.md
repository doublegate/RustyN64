# 0008 — The microcode-boot harness (Phase 2 criterion 2)

Status: **Proposed** — accepted on merge of the PR that introduces this file;
immutable thereafter (design; implementation staged)
Date: 2026-07-21
Deciders: repo owner
Supersedes: none · Superseded by: none

## Context

`to-dos/VERSION-PLAN.md` §v0.3.0 has two exit criteria. The first —
n64-systemtest's RSP category `Failed: 0` — is **met** (PRs #41–#45). The second
is:

> a real graphics microcode boots and emits a plausible RDP command list.

This ADR settles *how* that second criterion is discharged, because the choice
shapes a multi-PR effort and pulls in a new toolchain.

Two constraints from the owner fix the design:

1. **A real graphics microcode**, not a hand-written toy — specifically
   libdragon's `rdpq` (`src/rdpq/rsp_rdpq.S`, Unlicense / public domain per
   `ref-proj/libdragon/LICENSE.md`, on the vendor-ok list in
   `ref-proj/README.md`). The claim that it is the *only* vendorable real N64
   graphics microcode is **reasoned, not exhaustively surveyed**: the stock
   Nintendo microcodes (F3DEX and kin) are proprietary and cannot be committed,
   and `ref-proj/README.md`'s licence table lists no other permissively-licensed
   graphics microcode. If a second one surfaces this line is what to revisit.
2. **The golden reference is grounded in hardware documentation / datasheets /
   errata — never another emulator's output.** We do not diff against ares,
   cen64, or parallel-rdp; we diff against the RDP command encoding as N64brew
   *Reality Display Processor/Commands*
   (`n64brew_wiki/markdown/Reality Display Processor/Commands.md`) and
   libdragon's own `rdpq_macros.h` bit-layout headers define it.

### What "boot the real rdpq microcode" actually entails

`rdpq` is **not standalone**. `src/rdpq/rsp_rdpq.S` opens with `#include
<rsp_queue.inc>` and `RSPQ_BeginOverlayHeader`: it is an *overlay* on libdragon's
**RSPQ command-queue engine** (`src/rspq/rsp_queue.S`). The real boot is:

- the RSPQ kernel runs in IMEM, DMAs a **command queue** from RDRAM, and
  dispatches each command to the resident overlay;
- the `rdpq` overlay's command handlers assemble RDP command words into an
  output buffer and hand them to the RDP via `RDPQ_Send`, which drives
  `DPC_START`/`DPC_END` — the register file we landed in #44;
- the RSP idles/BREAKs when the queue drains.

Normally libdragon's **C side** (`rspq_init`, `rdpq_init`, overlay registration)
initialises the DMEM state — queue pointers, the RDP output-buffer pointers, the
overlay tables — before the RSP ever runs. Reproducing enough of that state is
the load-bearing research task, and it is why this is staged rather than a single
PR.

### Toolchain

The RSP linker script (`rsp.ld`) targets `elf32-bigmips` and lays the microcode
out as DMEM (`0x0000`) + IMEM (`0x1000`) — exactly the two blocks our RSP loads.
Assembling `rsp_*.S` needs the C preprocessor (the microcode `#include`s C
headers of `#define`s), the GNU assembler, and `ld` with `rsp.ld`. libdragon's
`n64.mk` uses target triple `mips64-elf` and assembles with `mips64-elf-gcc
-march=mips1 -mabi=32 -nostartfiles -Wl,-Trsp.ld`. The RSP vector opcodes are
assembler *macros* in the vendored includes, so **stock `mips64-elf` binutils
suffices** — no patched assembler.

Nothing MIPS is installed on this machine today. AUR carries
`mips64-elf-gcc 14.4.0` and `mips64-elf-binutils`.

## Decision

Discharge criterion 2 with a **committed golden-vector harness** built on the
**real, vendored libdragon rspq+rdpq microcode**, assembled from source, with the
**expected RDP command stream derived from hardware documentation**.

Concretely:

- **Vendor** the libdragon RSP microcode (rspq kernel + rdpq overlay + their
  include tree + `rsp.ld`) into `third_party/libdragon-rsp/`, pinned to an exact
  upstream commit, with the Unlicense text and a `NOTICE`. Public-domain, so no
  attribution is required, but we record provenance anyway (immutable-reference
  discipline, `40-docs-and-adrs`).
- **Assemble** it from that vendored source with the `mips64-elf` toolchain, and
  **commit the resulting DMEM+IMEM blob** so the test needs no toolchain to run.
  A CI job re-assembles from source and checksums the blob to prevent silent
  drift (the blob is generated-from-source, never hand-edited — the
  `Managed-Block Generated-vs-Hand-Authored` pattern applied to a binary).
- **Reproduce the boot state in Rust**, not by snapshotting a libdragon run.
  Understanding and re-creating the specific DMEM/RDRAM fields the microcode
  reads keeps the harness self-contained and free of any emulator dependency —
  the enduring choice. The exact field set is the Stage-1 research deliverable.
- **Fixture:** a small, fixed RSPQ command list (e.g. set-fill-color →
  fill-rectangle → sync-full) chosen so every emitted RDP command word is
  hand-verifiable.
- **Golden:** the expected RDP command bytes, derived from the **documented RDP
  command encoding** — N64brew *Reality Display Processor/Commands* plus
  libdragon's `rdpq_macros.h` field layouts. Committed as a golden vector with a
  provenance note; changed only on an intentional, reviewed behaviour change
  (`Golden-Vector Parity`, module 20).
- **Witness execution** before trusting a match: assert the microcode actually
  ran (IMEM executed, the output buffer is non-empty, `DPC_END` advanced) so an
  empty run cannot masquerade as a pass (the vacuous-pass hazard).

### Why not the alternatives

- **Hand-written toy microcode** — rejected by constraint 1; it would not be a
  *real* graphics microcode and is weak evidence for the criterion.
- **Snapshot libdragon's boot DMEM/RDRAM as an opaque fixture** — tolerable for
  the *input* (the golden is the output), but it makes the fixture a black box
  produced by running libdragon somewhere, which is exactly the external
  dependency the "enduring, self-contained" goal wants gone. Reproducing the
  init in Rust is more work once and less fragile forever.
- **Diff against another emulator** — rejected by constraint 2.
- **Assemble in CI from source with no committed blob** — faithful, but forces
  the `mips64-elf` toolchain onto every `cargo test` run. The commit-blob +
  CI-reassembly-checksum hybrid gives the same drift-safety without the local
  toolchain tax.

## Consequences

- **New vendored tree** `third_party/libdragon-rsp/` (public domain) and a
  committed microcode blob under `tests/`. Both are marked immutable/generated
  and exempt from formatters/linters.
- **New optional dev dependency**: the `mips64-elf` toolchain, needed only to
  *regenerate* the blob (contributors and the drift-check CI job), never to run
  the test suite.
- The DPC register file (#44) is the seam the emitted commands flow through; this
  harness is its first real exercise and will likely surface the next slice of DP
  behaviour to model (the FIFO drain, `CURRENT` advance, `SYNC_FULL` → DP
  interrupt) — tracked as it arrives, not pre-built.
- **v0.3.0 is not cut until this passes.** It is additive and behind the test
  harness, so it does not touch shipped behaviour or determinism.

## Staged implementation plan

Each stage is its own PR through the normal ceremony.

- **Stage 1 — toolchain + vendor + assemble + boot-state research.** Install
  `mips64-elf` (owner action, below). Vendor rspq+rdpq + includes + `rsp.ld`
  pinned to a commit. Add the assemble step and commit the blob + CI checksum.
  Document the rspq/rdpq boot ABI: the DMEM fields the kernel reads, the
  command-queue layout, and the `RDPQ_Send` → DPC path. Ship a test that boots
  the microcode and witnesses it reaching idle (no output comparison yet). The
  witness must have a **defined pre-run baseline that is itself unreachable as a
  pass** — otherwise a no-run path trivially satisfies the post-run assertion and
  the two states converge (the converging-paths hazard,
  `docs/engineering-lessons.md`). Concretely: before launch, zero `DPC_END` /
  `DPC_CURRENT` / the RDP output buffer **and** set `SP_STATUS` to *running*
  (`HALTED` and `BROKE` clear) with the PC at the kernel entry, not the idle
  handler. Then launch and assert the **transition**: `BROKE` becomes set and the
  PC has advanced to the kernel's idle/`BREAK` site. A microcode that never
  executed stays not-`BROKE` with the PC at entry and fails — so success requires
  the kernel actually to have run. Stronger still where cheap: also assert a DMEM
  cell the boot path is known to write, so "ran" is witnessed by an effect, not
  only by the halt state.
- **Stage 2 — feed a command list, capture the output.** Reproduce the boot
  state in Rust, place the fixture RSPQ command list in RDRAM, run to drain, and
  capture the RDP command list the microcode emits through the DPC path.
- **Stage 3 — golden byte-compare.** Derive the expected RDP command bytes from
  the documented encoding, commit the golden vector, and assert byte-equality
  with execution witnessed. This is the criterion.

## Owner action required (toolchain install)

Install the `mips64-elf` cross toolchain (assembler + linker + gcc driver for the
preprocessor). On CachyOS/Arch, via an AUR helper in a separate terminal:

```bash
paru -S mips64-elf-binutils mips64-elf-gcc
```

(These are the stock GNU cross tools; libdragon's own `build-toolchain.sh` is the
heavier fallback if the AUR build misbehaves, but stock binutils is expected to
assemble the RSP macros.) Once installed, Stage 1 proceeds: `mips64-elf-as` /
`mips64-elf-ld` / `mips64-elf-objcopy` on `PATH` is all the harness needs to
regenerate the blob.
