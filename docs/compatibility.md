# Compatibility — RustyN64

**References:** `ref-docs/research-report.md` §9 (regions), §8 (RDRAM /
Expansion Pak), §6 (CIC / saves), §Scope; `docs/cart.md`; `docs/STATUS.md`.

## Purpose

Which hardware revisions, regions, and software characteristics are in scope, and
how region/board variation is handled as **data**, not separate cores. The board
matrix proper lives in `docs/STATUS.md`.

## Regions (NTSC / PAL as data)

Region differences are timing/data, not separate cores
(`ref-docs/research-report.md` §9). The core clocks (93.75 MHz VR4300,
62.5 MHz RCP) are region-independent; only the VI vertical/horizontal counters and
the AI video-clock divisor differ:

| Region | Field rate | Lines/field | AI video clock | Notes |
| --- | --- | --- | --- | --- |
| NTSC | 60 Hz | ~262.5 | 48_681_812 Hz | 14.31818 MHz crystal, 3.579545 MHz colour-burst |
| PAL | 50 Hz | ~312.5 | 49_656_530 Hz | PAL VI timing tables differ; the AI clock detunes the same DACRATE |

The **AI video clock** (Hz) is the divisor for the DAC rate (`sample_rate =
video_clock / (AI_DACRATE + 1)`); it is **implemented** as
`rustyn64_audio::{VIDEO_CLOCK_NTSC, VIDEO_CLOCK_PAL}` (`Region::video_clock`) with
documented provenance (project64/N64-Tests + the N64brew wiki — see `docs/audio.md`).
The `Region` defaults to NTSC and is wired from the ROM region byte at load.

Carry per-region constants (field rate, line count, VI timing, AI video-clock) in
a small data table selected from the ROM region byte (`docs/cartridge-format.md`
§Region byte), so one core runs all regions deterministically. Dendy-equivalent
behaviour, if needed, is another row — not a fork.

## Memory configuration

- **Base RDRAM:** 4 MiB (`0x0000_0000`–`0x003F_FFFF`).
- **Expansion Pak:** +4 MiB (`0x0040_0000`–`0x007F_FFFF`), total 8 MiB. The Bus
  allocates the full 8 MiB backing store (`RDRAM_SIZE`); games that require the
  Expansion Pak (e.g. Donkey Kong 64, Majora's Mask) detect it via RDRAM probing.
- **9 bits per byte** — the hidden 9th bit is RDP/VI-only (AA coverage / Z),
  modelled as a parallel coverage plane (`docs/rdp.md`,
  `ref-docs/research-report.md` §8).

## Save + CIC per game

Save type (EEPROM 4k/16k / SRAM / FlashRAM / Controller Pak) and CIC variant are
**per-game**, resolved from a per-game DB by serial/CRC — there is no reliable
in-header field (`docs/cart.md`, `ref-docs/research-report.md` §6). The CIC
entry-point offsets (6103 +`0x100000`, 6106 +`0x200000`) must be applied at boot.

## Board model — why no tiering

Unlike the NES (hundreds of mappers needing a Core/Curated/BestEffort honesty
gate), the N64 has essentially **one cart model** parameterized by save type, CIC,
and region. So there is no board tiering and no honesty-gate test (ADR 0003); the
accuracy oracle is n64-systemtest pass/fail + the RDP fuzz suite
(`docs/testing-strategy.md`).

## The compatibility risk: custom microcode

The N64's defining compatibility hazard is **custom RSP microcode**
(`ref-docs/research-report.md` §3, §Scope success criteria 4). HLE emulators break
on unknown/custom microcode (Factor 5 / Rare / Boss variants); RustyN64's LLE core
runs the actual instruction stream, so custom microcode renders correctly
*without* per-game HLE. This is the whole reason for the LLE commitment (ADR 0002).
HLE graphics/audio plugins are explicitly out of scope for the accuracy core.

## In / out of scope

In scope (`ref-docs/research-report.md` §Scope): VR4300, the full RCP + eight
interfaces, LLE RSP + LLE RDP, VI scan-out, AI audio, PI/PIF/CIC/SI, all four save
backends, RDRAM (+ Expansion Pak), the three ROM formats, NTSC/PAL region timing.

Out of scope (deferred): the **64DD** disk peripheral (Japan-only, tiny library),
**HLE graphics/audio plugins** (never the default), and **per-GPU upscaling
backends** beyond a correct reference renderer.

## Open questions

- Exact PAL VI/AI divisor values + line counts to freeze the region table
  (`ref-docs/research-report.md` §Open questions 5).
- Which titles, if any, need real-PIF timing rather than the HLE-boot stub
  (`docs/cart.md`).
