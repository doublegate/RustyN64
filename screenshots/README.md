# Screenshots

Frames captured from RustyN64. **ROMs are never committed** (see `.gitignore` and the
`no-commercial-roms` CI job) — only the rendered output is, which the
`commercial-roms` policy in `CLAUDE.md` explicitly permits.

## `paper-mario-first-commercial-frame.png`

**The first rendered frame from a commercial cartridge** (ledger R-18), 2026-07-29.

- **Title:** Paper Mario (USA), FlashRAM, from the local gitignored corpus.
- **Path:** retail HLE boot (`rom::hle_boot`) → the game's own code → its graphics
  microcode on the LLE RSP → DPC seam → LLE RDP → `Bus::scanout_scaled`.
  Nothing here is HLE'd above the cartridge boundary; the picture is rasterised by
  the RDP from the game's own display list.
- **Geometry:** 625x237. That is the real VI output, not a bug — `VI_X_SCALE` is
  `0x200` (0.5 in 2.10), so the 320-wide framebuffer upscales to 640, less the
  8/7-pixel `minhpass`/`maxhpass` crop.
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
