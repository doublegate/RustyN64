# Cartridge ROM format — RustyN64

**References:** `ref-docs/research-report.md` §6 (ROM format + header + boot);
`crates/rustyn64-cart/src/lib.rs` (`RomFormat`, `RomHeader`, `normalize_to_big_endian`);
`docs/cart.md`.

This doc is the SPEC, not history — update it in the same PR as the code.

## Purpose

N64 ROM dumps ship in three byte orders that the file extension does **not**
reliably indicate. The loader sniffs the first four bytes, normalizes the whole
image to canonical big-endian (`.z64`) in memory, then parses the header. This
doc pins the magic bytes, the header layout, and how the normalized image feeds
the boot sequence.

## Byte orders and magic

The first 4 bytes encode both the PI bus config and the byte order
(`ref-docs/research-report.md` §6):

| Magic (first 4 bytes) | Format | Byte order | Normalization |
| --- | --- | --- | --- |
| `80 37 12 40` | `.z64` | big-endian (native) | none |
| `37 80 40 12` | `.v64` | byte-swapped 16-bit halfwords | swap each 2-byte pair |
| `40 12 37 80` | `.n64` | little-endian (32-bit word swap) | reverse each 4-byte word |

`RomFormat::detect(magic)` returns the format or `None`;
`normalize_to_big_endian(raw, format)` produces the canonical image. **File
extension is unreliable** — always sniff (`ref-docs/research-report.md` §6).

## Header layout (offsets, big-endian / `.z64`)

| Offset | Size | Field |
| --- | --- | --- |
| `0x00` | 4 | PI / clock / endian config dword (the magic) |
| `0x04` | 4 | clock rate |
| `0x08` | 4 | boot address / entry PC |
| `0x0C` | 4 | release / library version |
| `0x10` | 4 | **CRC1** |
| `0x14` | 4 | **CRC2** |
| `0x18` | 8 | reserved |
| `0x20` | 20 | internal game **title** (space-padded ASCII) |
| `0x34` | 7 | reserved |
| `0x3B` | 4 | cartridge ID / game code (e.g. `NSME`) + region byte |
| `0x40` | 4032 | **IPL3 bootcode** |

`RomHeader::parse` extracts the title (`0x20..0x34`) and game code (`0x3B..0x3F`);
**save type and CIC are NOT in the header** — they are resolved from the per-game
DB by serial/CRC (`docs/cart.md`).

## Behavior

### Load → normalize → parse

```text
detect(magic) -> RomFormat   (else CartError::UnknownFormat)
normalize_to_big_endian      (in-place swap; .z64 is a no-op)
RomHeader::parse             (>= 0x40 bytes, else CartError::ShortHeader)
DB resolve save_type + cic   (by game_code / CRC)
```

### Region byte

The region is encoded in the cartridge ID region byte at the end of the game code
(`0x3E`). It selects the NTSC/PAL region timing table (VI/AI divisors, field
rate) — see `docs/compatibility.md`. Region is **data**, not a build fork.

### IPL3 / boot

After normalization, IPL3 lives at offset `0x40` (length 4032) and executes from
`0xA4000040` during boot (`docs/cart.md` §Boot). The HLE-boot stub copies the
first 1 MB into RDRAM, applies the per-CIC entry-point offset, and jumps to the
header entry PC.

## Edge cases and gotchas

- **Sniff, never trust the extension** — a `.n64`-named file may be big-endian
  and vice versa (`ref-docs/research-report.md` §6).
- **`.v64` swaps halfwords, `.n64` reverses 32-bit words** — different
  transforms; using the wrong one corrupts every instruction.
- **Truncated images** — anything shorter than `0x40` bytes is
  `CartError::ShortHeader`; reject early.
- **CRC1/CRC2 are the cart checksums** the IPL3 verifies; the per-game DB can also
  key off them when the game code collides.
- **Title is space-padded, not NUL-terminated** — trim trailing `0x20`.

## Test plan

- **Magic detection** — all three orders + a reject case (already unit-tested:
  `detects_rom_formats`).
- **Normalization round-trip** — a `.v64`/`.n64` image normalizes to the same
  bytes as the equivalent `.z64`.
- **Header parse** — title/game-code extraction; short-header error
  (`short_header_errors` unit test).
- **Region/CIC/save resolution** — once the per-game DB lands, a known serial
  resolves to the expected region + CIC + save type.

## Open questions

- Vendoring/refreshing the per-game CIC + save DB (micro-64 lists;
  `ref-docs/research-report.md` §6 sources).
