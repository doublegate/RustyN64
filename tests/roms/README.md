# tests/roms — RustyN64

The N64 test-ROM oracle. Two tiers, governed by `docs/testing-strategy.md`:

- **Committed tier** (this directory, minus `external/`) — only
  **permissively-licensed** corpora. Every committed ROM ships its upstream
  `LICENSE` verbatim, and that is *enforced*, not merely documented:
  `scripts/check_no_roms.sh` fails if an allowlisted ROM has no `LICENSE` beside
  it.
- **External tier** (`external/`) — **gitignored, never committed.** Copyleft,
  unlicensed, and commercial corpora live here for local verification only.
  Stage your commercial dumps there; commit ONLY the screenshots and `.snap`
  golden baselines they produce, NEVER the ROMs. Confirm any path with
  `git check-ignore tests/roms/external/anything`.

Licensing below was verified by reading each upstream's actual `LICENSE` (or
confirming its absence through the GitHub licence API), not by trusting a badge.

## How the tiers are enforced

Three independent guards, described in full in `docs/testing-strategy.md`:

1. **`.gitignore`** broad-excludes `*.z64` / `*.n64` / `*.v64` / `*.ndd`
   everywhere, re-includes only the committed-tier paths, then hard-excludes
   `/tests/roms/external/` last, so no negation can leak a commercial dump back
   in.
2. **`scripts/check_no_roms.sh`** (pre-commit) checks the *staged* list, so it
   still catches `git add -f`, which bypasses `.gitignore` silently. Its
   `ALLOW_RE` must be kept in step with the `.gitignore` re-includes.
3. **the `no-commercial-roms` CI job** re-runs that script over the whole tracked
   tree, server-side and unskippable.

Promoting a corpus to the committed tier is a **licensing decision**: it must be
redistributable (MIT / BSD / Zlib / CC0 / public domain). Copyleft and unlicensed
corpora do not get a negation, however convenient that would be.

## Committed corpora (permissive — in the git tree)

| Corpus | Upstream | Licence (verified) | Contents | Footprint |
|---|---|---|---|---|
| `n64-systemtest/` | [lemmy-64/n64-systemtest](https://github.com/lemmy-64/n64-systemtest) | **MIT** (`LICENSE`, "Copyright (c) 2021 lemmy-64") | `n64-systemtest.z64` + `LICENSE` | 2.7 MB |

### n64-systemtest

**The strict CPU/COP0/TLB/RSP gate** (`docs/testing-strategy.md` Layer 3). It is
*self-judging* — it decides pass/fail itself and ends with a line like
`Done! Tests: 262. Failed: 0`, so no framebuffer comparison is needed. Covers
MFC0/DMFC0/MTC0/DMTC0 64-bit register semantics, LLD/LD/SC/SCD, exceptions
(overflow, unaligned access, TRAP, BREAK, SYSCALL), the TLB, 8/16/32/64-bit
access to RAM / ROM / SPMEM / PIF, and the RSP.

Built from source at upstream commit
`f2db2b92da9ddf281848f17c87b84c4aeea07c2f`:

```bash
cargo +stable install nust64
cd ref-proj/n64-systemtest && cargo run --release
# -> target/mips-nintendo64-none/release/n64-systemtest.z64
```

It pins `nightly-2022-07-10` through its own `rust-toolchain.toml`; rustup
fetches that automatically. The root `Cargo.toml` carries
`exclude = ["ref-proj", "n64brew_wiki"]` specifically so this builds — without
it, cargo's upward workspace discovery makes n64-systemtest resolve *our*
workspace and fail, because that 2022 toolchain predates stable
`workspace-inheritance`.

`sha256: fa933e06d05b9200377fb33af9876ce92e8e882619b3494b5a6e594fa5cc28d1`

Rebuilding with different `--features` (`timing`, `cycle`, `cop0hazard`) yields a
different ROM; the committed one is the default set.

## External corpora (gitignored — local only)

| Corpus | Upstream | Licence (verified) | Why external | Footprint |
|---|---|---|---|---|
| `external/krom/` | [PeterLemon/N64](https://github.com/PeterLemon/N64) | Unlicense (public domain) | Permissive, but upstream is 2.0 GB; even this curated subset is 182 MB | 182 MB, 196 ROMs |
| `external/dillon-n64-tests/` | [Dillonb/n64-tests](https://github.com/Dillonb/n64-tests) | **NONE** — no licence file, no README statement | No licence means no grant to redistribute | 38 MB, 26 ROMs |
| `external/commercial/` | personal cartridge dumps | copyrighted | Never redistributable | 1.5 GB, 66 ROMs |
| `external/240p/` | [ArtemioUrbina/240pTestSuite](https://github.com/ArtemioUrbina/240pTestSuite) | **GPL-2.0** | Copyleft, incompatible with the committed tier's permissive rule | not yet fetched |

### krom (PeterLemon/N64)

Bare-metal CPU/RSP/RDP/audio tests in assembly, prebuilt as `.N64`/`.n64`. A
**curated subset** of the 2.0 GB upstream corpus — the emulator-relevant
directories only, leaving ~1.8 GB of graphics demos in
`ref-proj/PeterLemon-N64/`.

| Directory | ROMs | Covers |
|---|---:|---|
| `CPUTest/` | 114 | VR4300 instructions, COP0, PI DMA alignment |
| `RSPTest/` | 56 | RSP scalar + vector ISA |
| `CP1/` | 12 | FPU / COP1 |
| `EMU/` | 7 | emulator-behaviour probes |
| `Interrupt/` | 3 | MI interrupt lines |
| `RDPTest/` | 2 | RDP rasterizer |
| `RDRAMTest/` | 1 | RDRAM addressing |
| `Initialize/` | 1 | boot / IPL |

Public domain, so this *could* be committed; it is external purely on size.

### dillon-n64-tests

Targeted CPU/RSP tests, hardware-verified, prebuilt. Fetched from the upstream
`latest` release (`dillon-n64-tests.zip`, published 2024-10-13) rather than
built, since building needs the ARM9 `bass` assembler fork plus libdragon's
`chksum64`.

**Run-only.** With no licence there is no grant to redistribute these: using them
locally as an oracle is fine, committing or shipping them is not.

### 240p Test Suite (not yet fetched)

Artemio's N64 240p Test Suite is the video-timing and calibration reference,
built on libdragon. **GPL-2.0**, so external tier regardless of size.

Not staged: the repo ships source only — no prebuilt ROM and no GitHub releases —
and binaries are distributed through itch.io, which needs an interactive
download. To add it, take the NTSC build from
<https://artemiourbina.itch.io/240p-test-suite>, or build `240psuite/N64/` with a
libdragon MIPS toolchain, then drop the ROM in `tests/roms/external/240p/`.

## Adding a corpus

1. Verify the licence by reading the upstream `LICENSE`. Absence of a licence is
   **not** public domain — it means no grant.
2. Permissive (MIT / BSD / Zlib / CC0 / public domain) → committed tier: add a
   `.gitignore` re-include, add the path to `ALLOW_RE` in
   `scripts/check_no_roms.sh`, and commit the upstream `LICENSE` beside the ROM.
3. Anything else → `external/`, and record it in the table above.
4. Update the licence table in `docs/testing-strategy.md` in the same change.
