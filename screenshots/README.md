# Screenshots

Frames captured from RustyN64 running **commercial cartridges**, through the
full LLE path: retail HLE boot → the game's own code → its graphics microcode on
the LLE RSP → the DPC seam → the LLE RDP → `Bus::scanout_scaled`. Nothing above
the cartridge boundary is HLE'd.

**ROMs are never committed** (see `.gitignore` and the `no-commercial-roms` CI
job). Only the rendered output is, which the `commercial-roms` policy in
`CLAUDE.md` explicitly permits.

Reproduce with the committed census runner (`microcode_families.rs`), which
reads a locally-staged corpus and skips loudly when there is none. It is a
*different* test from `game_microcode.rs`: that one witnesses a retail title's
microcode executing on the LLE RSP and reaching the RDP, while this one censuses
the whole corpus for what actually reaches the screen. Both are T-71-003
evidence and both are committed.

```bash
cargo test -p rustyn64-test-harness --release --test microcode_families \
    -- --ignored --nocapture
```

## The selection rule: these were looked at

Ledger **R-18** records that "lit pixel count" was cited for weeks as evidence
of rendering and was **wrong** — uninitialised RDRAM is non-black, so a broken
machine scores 90%+ as easily as a working one. The corpus census proves it on
its own data: **Rayman 2 and Namco Museum 64 report zero RDP commands with
123,540 and 137,681 lit pixels.** Pure garbage, scanned out.

So every file here was **rendered to PNG and viewed** before being committed,
and frames that scored well but looked wrong were rejected — Blast Corps
(garbled colour blocks), Turok (glitchy plane), Wave Race 64 (ambiguous),
GoldenEye and WCW/nWo (see *Known defects* below). **Do not add a screenshot
here on a pixel count. Look at it first.**

## Rendering correctly

| File | Title | What it shows |
|---|---|---|
| `super-mario-64-title.png` | Super Mario 64 | The title screen — Mario's head, textured cap with the M logo, over the tiled *SUPER MARIO 64* background. 125,278 RDP commands. |
| `pokemon-snap-3d-landscape.png` | Pokémon Snap | A full 3D landscape: sky, hills, a river and foliage, textured and shaded. |
| `pokemon-stadium-n64-logo.png` | Pokémon Stadium | The *NINTENDO 64* wordmark with the coloured 3D "N" cube. |
| `mario-kart-64-attract-mode.png` | Mario Kart 64 | Attract mode — a checkered flag over the track, karts, sky and grass. |
| `castlevania-legacy-of-darkness-menu.png` | Castlevania: Legacy of Darkness | The *"Controller Pak not inserted"* dialog, fully legible with its cursor. A text/UI path rather than 3D geometry. |
| `bomberman-hero-3d-scene.png` | Bomberman Hero | A textured tower on a green landscape with a rainbow. |
| `bomberman-64-intro.png` | Bomberman 64 | The intro grid floor with the character sprite. |
| `super-smash-bros-3d-stage.png` | Super Smash Bros. | A textured 3D room — wallpaper, blocks and a character, with correct perspective. |
| `mario-golf-course.png` | Mario Golf | Mario on the green with his club, textured grass and trees. |
| `resident-evil-2-intro.png` | Resident Evil 2 | The R.P.D. building with a legible content-warning overlay — text composited over a pre-rendered background. |
| `wcw-nwo-revenge-thq-logo.png` | WCW/nWo Revenge | The *THQ INC.* logo, crisp and correctly oriented. |

## Kept deliberately as known-imperfect

These are committed as evidence of **what currently happens**, not as
correctness targets. Do not treat them as goldens.

(`paper-mario-first-commercial-frame.png` predates the rest — it was committed
when it was the *only* commercial frame that existed. It is listed here for
completeness of the set, not because this change produced it.)

| File | Title | What is wrong |
|---|---|---|
| `banjo-kazooie-first-3d-scene.png` | Banjo-Kazooie | Geometry, textures and depth ordering are right; the colours carry a heavy blue/yellow cast. Open combiner / texel-format issue. |
| `ocarina-of-time-night-sky.png` | Ocarina of Time | Hyrule Field at night — the moon over a dark horizon, recognisably correct, but the moon carries a **visible rectangular texture-clamp box**. An open clamp/border defect (R-13). |
| `paper-mario-first-commercial-frame.png` | Paper Mario | **The first frame ever rendered from a commercial cartridge** (2026-07-29), kept for that reason. 87 distinct RGBA5551 values; flat-shaded quads with clean edge-walked slopes, but early boot geometry rather than a title screen. |

## Known defects visible in rejected frames

Recorded here because the *rejected* captures localise real bugs:

- **Mirrored text.** GoldenEye 007's "Nintendo" logo and WCW vs. nWo's
  "WCW World Championship Wrestling" banner both render **left-right flipped**.
  Two independent titles showing the same flip points at the texture S-axis
  mirror path, not at either game. It is **tile-specific rather than global**:
  WCW/nWo Revenge — same publisher — renders its THQ logo correctly oriented,
  and that frame is committed above. So the defect is in how a particular tile's
  `mirror_s` is resolved, not in every textured quad.
- **Garbled colour blocks** (Blast Corps) and a **glitchy textured plane**
  (Turok) — both issue large command counts, so the failure is downstream of
  submission.

## Output resolution and VI scaling

Most frames are **625×237**. That is the real VI output for an NTSC title, not
a bug: `VI_X_SCALE = 0x200` is 0.5 in 2.10 fixed point, so a 320-wide
framebuffer upscales to 640, less the 8/7-pixel `minhpass`/`maxhpass` crop.
Titles differ where they program the VI differently (Banjo-Kazooie is 570×213).

*Provenance* — none of those constants are asserted here. The 2.10 step
semantics of `VI_X_SCALE`/`VI_Y_SCALE` are N64brew *Video Interface*
§VI_X_SCALE, §VI_Y_SCALE; the horizontal overscan and the 8/7-pixel crop are the
Angrylion VI geometry that ledger **R-5** implements and validates RGB
byte-for-byte through the `.vivec` conformance vectors. See
`docs/accuracy-ledger.md` §R-5 and `Bus::scanout_scaled`'s rustdoc.
