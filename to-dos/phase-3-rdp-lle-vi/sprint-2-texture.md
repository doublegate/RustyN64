# Sprint 2 — Texture state, TMEM, and the texel formats

**Phase:** Phase 3 — RDP LLE + VI
**Sprint goal:** give the RDP a texture memory and the commands that fill and describe it, then
decode every texel format and sample a tile — the state and data path a textured primitive needs,
short of the colour combiner (Sprint 3). The visible milestone is a **Texture Rectangle in copy
mode** putting a real texture on screen.
**Estimated duration:** 3 weeks

## Reference

The command encodings below are taken verbatim from
`n64brew_wiki/markdown/Reality Display Processor/Commands.md` (the per-command field tables) and
recorded in `docs/rdp.md`. All six texture commands are single 64-bit words. Opcode is
`bits[61:56]`. Format values: RGBA=0, YUV=1, CI=2, IA=3, I=4+. Size values: 4bpp=0, 8bpp=1,
16bpp=2, 32bpp=3. TMEM is 4 KiB (0x1000 bytes); Set Tile `address`/`line` are counted in **64-bit
TMEM words** (word 0x100 = byte 0x800 = the half-way split); TLUTs live in the **upper half**
(word ≥ 0x100), aligned to 16 TMEM words, each loaded entry quadrupled.

## Tickets

### T-32-001 — TMEM, the tile descriptors, and the texture-state commands

**Description:** add the RDP's texture state — a 4 KiB TMEM buffer and eight tile descriptors — and
implement the three commands that describe it without moving any texels: `Set Texture Image`
(0x3D), `Set Tile` (0x35), and `Set Tile Size` (0x32).

**Encodings:**

- **Set Texture Image (0x3D):** `format[55:53]`, `size[52:51]`, `width[41:32]` (pixels − 1),
  `dramAddress[23:0]`. The DRAM source for subsequent loads. (Wiki: format has no effect; size and
  width drive load addressing.)
- **Set Tile (0x35):** `format[55:53]`, `size[52:51]`, `line[49:41]` (TMEM words per row),
  `address[40:32]` (TMEM word), `index[26:24]`, `palette[23:20]` (CI4 only), then per-axis
  `clamp/mirror/mask[3:0]/shift[3:0]` — T in `[19:10]`, S in `[9:0]`.
- **Set Tile Size (0x32):** `SL[55:44]`, `TL[43:32]` (u10.2), `index[26:24]`, `SH[23:12]`,
  `TH[11:0]` (u10.2).

**Acceptance criteria:**

- [ ] `Rdp` owns a 4 KiB TMEM and `[TileDescriptor; 8]`, both observably zero/reset at power-on.
      TMEM storage may be lazily allocated (zero-initialised on first write) rather than an inline
      `[u8; 4096]`, so the per-tick `core::mem::take` in `Bus::rdp_tick` stays cheap; the observable
      contract is "reads as zero until written, resets to zero at power-on".
- [ ] Each of the three commands decodes every field into the right descriptor / texture-image
      register, verified field-by-field by a unit test that seeds a distinguishable value in each.
- [ ] `Set Tile` writes only the addressed descriptor (index masked to `0-7`); the others are
      untouched.
- [ ] No texel is moved and no framebuffer pixel changes — this ticket is pure state.

**Dependencies:** T-31-003 (dispatch seam)
**Reference:** `docs/rdp.md` §Texture state; Commands.md 0x3D/0x35/0x32
**Estimated complexity:** M

---

### T-32-002 — Load Block and Load Tile (RDRAM → TMEM)

**Description:** move texels from the current texture image in RDRAM into TMEM. `Load Tile` (0x34)
copies a rectangle; `Load Block` (0x33) streams a linear run with the dxt odd-line word-swap.

**Encodings:**

- **Load Tile (0x34):** `SL[55:44]`, `TL[43:32]` (u10.2), `tile[26:24]`, `SH[23:12]`, `TH[11:0]`
  (u10.2). RDRAM row width = `SH − SL`; TMEM row stride = the tile's `line`. Updates the
  descriptor's tile size to `(SL, TL, SH, TH)` for rendering.
- **Load Block (0x33):** `SL[55:44]`, `TL[43:32]` (u12.0), `tile[26:24]`, `SH[23:12]` (u12.0),
  `dxt[11:0]` (u1.11). `SH − SL` = texel count (max 2048). dxt is added to a 1.11 counter per
  64-bit TMEM word; when it crosses 1 the current line is odd and its two 32-bit halves are
  swapped on load. Loads past TMEM end wrap to the start.

**Acceptance criteria:**

- [ ] `Load Tile` copies the exact rectangle into TMEM at the tile's address and `line` stride,
      byte-for-byte against a hand-computed expectation.
- [ ] `Load Block` streams `SH − SL` texels and applies the dxt odd-line 32-bit swap. On a load
      over 2048 texels it writes nothing: N64brew *…/Commands* §Load Block states such a load
      "fail\[s\]" with nothing written into TMEM — this is the documented source, and the
      over-limit path is asserted separately and re-checked against the fuzz oracle in Sprint 3
      (if the hardware truly does a partial write, the fuzz result supersedes this). **Pin both
      sides of the boundary**: exactly 2048 texels loads fully, 2049 writes nothing (guards an
      off-by-one in the limit check).
- [ ] Both update the descriptor's tile size as documented.
- [ ] A property test: a Load Tile followed by reading TMEM at the mapped address round-trips the
      source bytes for 4/8/16/32-bit texel sizes.

**Dependencies:** T-32-001
**Reference:** `docs/rdp.md` §TMEM loads; Commands.md 0x34/0x33
**Estimated complexity:** L

---

### T-32-003 — Load TLUT and the texel-format decoders

**Description:** load a palette and decode every texel format from TMEM to RGBA8888 — the fetch
half of the sampler. `Load TLUT` (0x30) quadruples each entry into high TMEM.

**Scope:** decode RGBA16 (5551), RGBA32 (8888), IA4/IA8/IA16, I4/I8, CI4/CI8 (via TLUT), and
document YUV16 (decode deferred if no oracle test needs it this sprint).

**Acceptance criteria:**

- [ ] `Load TLUT` writes `SH − SL + 1` entries (an **inclusive** count — the typical
      `(0, 0, count-1, 0)` gives `count` entries), each quadrupled, into the addressed high-TMEM
      region, and latches `(SL, TL, SH, TH)` into the tile descriptor. The base must be in the
      **upper** TMEM half (word >= 0x100) **and** aligned to 16 TMEM words (128 bytes): the
      acceptance test covers a base below 0x100 (rejected) *and* a misaligned high-half base
      (rejected), not only the lower bound.
- [ ] A `fetch_texel(tile, s, t) -> [u8; 4]` returns the correct RGBA8888 for each listed format,
      checked against hand-derived values (5→8 bit replication for RGBA16; CI4 uses the descriptor
      `palette` as the high half of the TLUT address).
- [ ] The exactness of the widening is pinned by a unit test per format.

**Dependencies:** T-32-002
**Reference:** `docs/rdp.md` §Texel formats; Commands.md 0x30
**Estimated complexity:** L

---

### T-32-004 — The texture sampler and Texture Rectangle (copy mode)

**Description:** sample a tile at `(s, t)` with clamp/mirror/mask/shift, and rasterise `Texture
Rectangle` (0x24) / `Texture Rectangle Flip` (0x25) into the colour image in **copy mode** — the
first textured picture. This needs the multi-word dispatch extension (Texture Rectangle is 2
words), so `dispatch` gains access to the command's base address to read the second word.

**Acceptance criteria:**

- [ ] `dispatch` can read a multi-word command's later words (the tick advance is refactored so the
      base address is available), covered by a decode test that would desync without it.
- [ ] The sampler applies mask (wrap), clamp, mirror, and shift to S and T per the shift table.
- [ ] Texture Rectangle in copy mode writes the sampled texels into the scissored region of the
      colour image, verified byte-for-byte for a small textured rect.
- [ ] A golden frame (`golden_frame.rs` sibling) pins a textured rectangle end-to-end.

**Dependencies:** T-32-003
**Reference:** `docs/rdp.md` §Texture rectangle; Commands.md 0x24/0x25; Pipeline.md §Copy
**Estimated complexity:** L

---

## Deferred to Sprint 3

- The colour combiner (`Set Combine Mode` 0x3C) and blender (`Set Other Modes` 0x2F) — a
  combiner-driven textured primitive (1-cycle/2-cycle) rather than copy mode.
- Z-buffering, coverage, primitive depth (0x2E).
- The documented RDP hazards (texture-load-then-use) — carried from Sprint 1.
- The ParaLLEl-RDP fuzz suite 0-diff vs Angrylion (the v0.4.0 cut gate).

## Sprint review checklist

- [ ] All tickets checked off or explicitly deferred (with reason).
- [ ] A textured rectangle renders to a committed golden.
- [ ] CHANGELOG.md updated.
- [ ] `docs/rdp.md` updated in the same change as the code it describes.
