# Screenshots

Frames captured from RustyN64. **ROMs are never committed** (see `.gitignore` and the
`no-commercial-roms` CI job) — only the rendered output is, which the
`commercial-roms` policy in `CLAUDE.md` explicitly permits.

## `super-mario-64-title.png`

**Super Mario 64's title screen** (ledger R-18), 2026-07-29 — a commercial N64
game rendering its own title screen through the full LLE path.

- **Title:** Super Mario 64 (USA), EEPROM 4k, from the local gitignored corpus.
- **Path:** retail HLE boot -> the game's own code -> its graphics microcode on
  the LLE RSP -> DPC seam -> LLE RDP -> `Bus::scanout_scaled`. Nothing above the
  cartridge boundary is HLE'd.
- **Captured at:** frame 360 of a 600-frame run; **125,278 RDP commands** issued
  by that point, 138,474 of 148,125 pixels lit.
- **What unblocked it:** SM64 was previously halted in its own assert path
  (`B -1` at `0x80246DD8`) because the PIF answered as a connected controller on
  **all four** joybus channels, so `osContInit` reported four pads on a one-pad
  console. Fixing the "no device" RX flag let it past the assert. See R-18.

## `banjo-kazooie-first-3d-scene.png`

Banjo-Kazooie rendering real 3D geometry after the same fix (133,625 RDP
commands, up from **zero**). **Kept deliberately as a known-imperfect frame:**
the geometry, textures and depth ordering are right, but the colours carry a
heavy blue/yellow cast — an open combiner/texel-format issue. It is committed as
evidence of *what currently happens*, not as a correctness target; do not treat
it as a golden.

## `paper-mario-first-commercial-frame.png`

**The first rendered frame from a commercial cartridge** (ledger R-18), 2026-07-29.

- **Title:** Paper Mario (USA), FlashRAM, from the local gitignored corpus.
- **Path:** retail HLE boot (`rom::hle_boot`) → the game's own code → its graphics
  microcode on the LLE RSP → DPC seam → LLE RDP → `Bus::scanout_scaled`.
  Nothing here is HLE'd above the cartridge boundary; the picture is rasterised by
  the RDP from the game's own display list.
- **Geometry:** 625x237, and **all 148,125 pixels are lit**. That is the real VI
  output for this NTSC title, not a bug — `VI_X_SCALE` is `0x200` (0.5 in 2.10),
  so the 320-wide framebuffer upscales to 640, less the 8/7-pixel
  `minhpass`/`maxhpass` crop. A PAL title scans out taller (576 lines); these
  dimensions are this capture's, not a fixed expectation.

  *Provenance for those constants* — none of them are asserted here. The 2.10
  fixed-point step semantics of `VI_X_SCALE`/`VI_Y_SCALE` are N64brew *Video
  Interface* §VI_X_SCALE, §VI_Y_SCALE; the horizontal overscan and the 8/7-pixel
  `minhpass`/`maxhpass` crop are the Angrylion VI pipeline geometry that ledger
  **R-5** implements and validates RGB byte-for-byte through the `.vivec`
  conformance vectors (13 VI probes in the accuracy battery). See
  `docs/accuracy-ledger.md` §R-5 and `Bus::scanout_scaled`'s rustdoc.
- **Two denominators, kept apart on purpose.** The same frame measures
  **75,840 / 75,840** through the unscaled 1:1 `Bus::scanout` (320x237) and
  **148,125 / 148,125** through `Bus::scanout_scaled` (625x237). Both are "fully
  lit"; quoting one figure beside the other frame's dimensions is a mistake this
  file made in its first revision.
- **Captured at:** frame 120 of a 300-frame run; the frame is stable through
  frame 270, so it is a held picture rather than a transient.
- **What it shows:** flat-shaded geometry with clean edge-walked slopes — a green
  quad with an orange top edge, a band of blue stripes, on a light-grey clear.
  It is **not** the Paper Mario title screen; it is early boot geometry.

### Why this file exists at all

Ledger R-18 records that "lit pixel count" was cited for weeks as evidence of
rendering and was **wrong**: uninitialised RDRAM is non-black, so a broken machine
scores 90%+ as easily as a working one. R-18 therefore admits only two kinds of
evidence — a byte-comparison against a committed golden, or **someone actually
looking at the image**. This file is the second kind, and it was checked against
that standard: Ocarina of Time scored 62,963 lit pixels on the same run and,
rendered and viewed, is pure noise. It is not committed. Paper Mario was viewed
and is real.

Do not add a screenshot here on a pixel count alone. Look at it first.
