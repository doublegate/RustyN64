# RustyN64 Documentation Index

**RustyN64 version:** v0.1.0 (SKELETON)

This index maps the `docs/` tree for RustyN64 — the cycle-accurate Nintendo 64 emulator. The
single source of truth for per-subsystem state, the accuracy gates, and version policy is
[`STATUS.md`](STATUS.md).

Read [`architecture.md`](architecture.md) before any chip doc: reading a subsystem spec without
the eight load-bearing facts in mind will mislead.

---

## Subsystem specifications

The core "spec" docs — kept in sync with the code in the same PR as a change. These are the
spec, not a history log; when a passing test ROM and a doc disagree, the ROM wins and the doc is
corrected.

| Document | Subsystem |
|----------|-----------|
| [cpu.md](cpu.md) | NEC VR4300 — MIPS III instruction set, TLB, COP0, FPU, SysAD, the documented errata |
| [rsp.md](rsp.md) | RSP — the scalar unit, the 8-lane vector unit, DMEM/IMEM, downloadable microcode |
| [rdp.md](rdp.md) | RDP — the command list, the texture/combiner/blender pipeline, and VI scan-out |
| [audio.md](audio.md) | AI — the DAC, sample DMA, and the rate derivation |
| [cart.md](cart.md) | PI cart, PIF/CIC boot, SI joybus, and the four save backends |
| [cartridge-format.md](cartridge-format.md) | ROM header, the `.z64`/`.n64`/`.v64` byte orders, save-type and CIC detection |
| [scheduler.md](scheduler.md) | The 3:2 fractional master-clock scheduler and the lockstep contract |
| [architecture.md](architecture.md) | Cross-cutting design — the eight load-bearing facts |
| [frontend.md](frontend.md) | The `rustyn64` shell (winit + wgpu + cpal + egui), audio ring, pacing |

## Cross-cutting references

| Document | Topic |
|----------|-------|
| [STATUS.md](STATUS.md) | **Single source of truth** — per-subsystem state, infrastructure, corpora, accuracy gates, version policy |
| [testing-strategy.md](testing-strategy.md) | The oracle, the five test layers, the corpus tiers, and the commercial-ROM guards |
| [compatibility.md](compatibility.md) | Regions, memory configuration, per-game save and CIC, the custom-microcode risk |
| [performance.md](performance.md) | Where the time goes, and the correctness-before-acceleration rule |
| [glossary.md](glossary.md) | N64 hardware and emulation terminology |

---

## Subdirectories

| Directory | Contents |
|-----------|----------|
| [adr/](adr/) | Architecture Decision Records (Michael Nygard format), `0001`-`0004` — the fractional master-clock lockstep scheduler, the LLE-coprocessor decision plus the deferred sub-cycle timebase refactor, the no-board-tiering/no-honesty-gate decision, and the determinism contract. |

## Related, outside `docs/`

| Location | Contents |
|----------|----------|
| [`../ref-docs/`](../ref-docs/) | Immutable primary research — never rewritten in place; corrections land as new dated supplemental files. |
| `../n64brew_wiki/` | Gitignored offline mirror of the N64brew Wiki (324 pages, 96 media) — the primary hardware reference. Search `markdown/`, browse `html/`, rebuild with `scripts/mirror_n64brew_wiki.py`. CC BY-SA 4.0. |
| `../ref-proj/` | Gitignored study clones of reference emulators and test suites. **Licences vary and several forbid copying** — read `ref-proj/README.md` before lifting anything. |
| [`../tests/roms/README.md`](../tests/roms/README.md) | The test-ROM corpus tiers, their verified licences, and the three-layer commercial-ROM guard. |
| [`../to-dos/ROADMAP.md`](../to-dos/ROADMAP.md) | The phase spine, Phase 0 through Phase 8 — the planning entry point. |
| [`../to-dos/phase-*/`](../to-dos) | Per-phase overviews and sprint ticket breakdowns (`T-PS-NNN`, P = phase, S = sprint). |
| [`../CHANGELOG.md`](../CHANGELOG.md) | Release history. |

---

## External references

- [N64brew Wiki](https://n64brew.dev/wiki/Main_Page) — the primary hardware specification
- [ares](https://github.com/ares-emulator/ares) · [CEN64](https://github.com/n64dev/cen64) —
  the cycle-accuracy reference bar
- [ParaLLEl-RDP](https://github.com/Themaister/parallel-rdp) — the RDP bit-exactness fuzz suite
- [n64-systemtest](https://github.com/lemmy-64/n64-systemtest) — the strict, self-judging
  CPU/COP0/TLB/RSP gate
- [Dillon's n64-tests](https://github.com/Dillonb/n64-tests) ·
  [PeterLemon/N64](https://github.com/PeterLemon/N64) — targeted and bare-metal test corpora
- [libdragon](https://github.com/DragonMinded/libdragon) — the open homebrew SDK

---

**Source of truth:** [STATUS.md](STATUS.md) · **Release history:** [`../CHANGELOG.md`](../CHANGELOG.md)
