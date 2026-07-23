# Changelog

All notable changes to RustyN64 are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/), and this project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

The next rung is `v0.4.0 "Rasteriser"` — the LLE RDP and VI, the first picture
(see [`to-dos/VERSION-PLAN.md`](to-dos/VERSION-PLAN.md)).

### Changed — pin the toolchain to exactly 1.96.0

- **The Rust toolchain and MSRV are pinned to the exact patch release `1.96.0`**
  (edition 2024 unchanged), down from the `1.97` minor-series pin. `rust-toolchain.toml`,
  `Cargo.toml`'s `rust-version`, and every CI/release/docs workflow (`dtolnay/rust-toolchain@1.96.0`)
  now name one exact version, so local, CI, and release builds all resolve the same
  compiler rather than whatever `1.96.x`/`1.97.x` patch a runner happens to have. This
  is a reproducibility guarantee for the eventual libretro core (a floating patch pin
  bit the RustyNES libretro release); the whole workspace compiles, lints, tests, docs,
  and builds `no_std` cleanly on 1.96.0.

### Added — sub-pixel coverage on 1-/2-cycle triangles (Phase 3, T-33-004 2c)

- **1-cycle/2-cycle triangles now rasterise with sub-pixel coverage.** The
  edge-walk computes per-Y-subpixel `s.3` edges (`quantize_x`), and each pixel's
  coverage is evaluated with `compute_coverage`: with anti-aliasing off a pixel
  draws only when its top-left sub-sample is inside the span (so a fractional
  edge correctly excludes a barely-touched column), and the coverage count is
  stored in the pixel's alpha/coverage bits (`(count − 1) & 7`, the `cvg_dest`
  clamp write-back). FILL/COPY mode is unchanged — it rounds to whole pixels
  (the wiki: "without subpixel accuracy"), which the whole-pixel span already
  matches. Verified byte-for-byte against Angrylion by two new conformance
  vectors: `fill_tri_frac_16` (FILL-mode fractional edges round to whole pixels)
  and `shade_tri_frac_16` (a 1-cycle shaded triangle whose fractional left edge
  excludes column 2 and whose right edge leaves column 6 *partially* covered —
  `0xf800` vs the fully-covered `0xf801`). The self-asserted shaded/textured unit
  tests were updated: the degenerate top vertex is now excluded by the AA-off
  rule, and the stored alpha holds coverage, so they check the combiner RGB at a
  drawn interior pixel. Scope (ledger R-9): the depth path, the coverage-weighted
  AA-edge blend, the other `cvg_dest` modes, alpha-compare, and dither remain.
  Oracle 93.

### Fixed — the 4× triangle edge-slope bug (Phase 3, ledger R-14)

- **Triangle edges now advance at the correct rate.** `triangle_fill` was
  multiplying the per-pixel edge slopes (`DxHDy`/`DxMDy`/`DxLDy`) by the
  sub-scanline offset in quarter-pixel units (`y = line*4 + sub`) without the
  compensating `>> 2`, so every triangle edge advanced 4× too fast. The three
  slopes are now pre-shifted `>> 2` at decode (matching parallel-rdp's
  `setup.dxhdy = raw >> 2`), since they are dx **per pixel row** while the
  edge-walk steps per quarter-pixel. Verified byte-for-byte against Angrylion by
  the `fill_tri_16` and `fill_tri_wide_16` conformance vectors (the latter added
  here — a multi-column staircase). The self-asserted triangle unit tests that had
  baked in the buggy staircase were corrected: their `DxMDy` changed from `0.25`
  to `1.0` (the value for which that staircase is the *correct* output, confirmed
  by the oracle), so the geometry is no longer circular. Oracle 93.

### Added — the RDP conformance gate + a bug it immediately caught (Phase 3, T-33-005)

- **The ParaLLEl-RDP / Angrylion conformance gate is live.** A licence-clean
  golden-vector pipeline: a standalone driver (`crates/rustyn64-test-harness/vectors-gen/`,
  our own MIT code) runs the **Angrylion** software RDP (the non-commercial
  study oracle, fetched into gitignored `ref-proj/`, never vendored) over
  hand-written RDP command lists and emits `.rvec` vectors carrying *only
  outputs* — the command stream plus Angrylion's rendered framebuffer, both
  freely committable. `tests/rdp_conformance.rs` replays each command stream
  through RustyN64's own RDP and asserts a byte-for-byte match. A FILL-rectangle
  vector passes (proving the pipeline and byte order end to end).
- **The gate immediately found a real rasteriser bug.** Its second vector — a
  flat Fill Triangle — exposed that `triangle_fill` multiplies the per-pixel edge
  slope (`DxHDy`/`DxMDy`/`DxLDy`) by the sub-scanline offset in **quarter-pixel**
  units without the compensating `>> 2`, so every triangle edge advances **4× too
  fast**. The wiki (`…/Commands` §Edge Coefficients — the slopes are "change in x
  per change in y" with `yh/ym/yl` in `s11.2` *screen* pixels) and parallel-rdp
  (`span_setup.comp:167`, where `setup.dxhdy = raw >> 2`) both confirm the slope
  is per-pixel; applying `>> 2` makes the triangle vector match byte-for-byte
  (verified). The self-asserted `fill_triangle_flat_fills_a_right_triangle` unit
  test had baked in the buggy staircase — the exact circular-golden trap this gate
  exists to break. The bug is fixed in the R-14 entry above (the vector passes, no
  longer `#[ignore]`d). Oracle 93.

### Added — sub-pixel coverage primitives (Phase 3, T-33-004 PR-B 2c-coverage)

- **The RDP's sub-pixel coverage mask is now computed.** `compute_coverage` ports
  parallel-rdp's `coverage.h` exactly: 4 Y-subpixels × 2 X-samples (at the diamond
  offsets `{0, 4}` / `{2, 6}` that alternate by Y-subpixel) tested against the
  per-subpixel half-open span edges, yielding the 8-bit mask whose popcount is the
  coverage count (0–8). `quantize_x` snaps a `s.16` edge X to the 3-fraction-bit
  coverage domain with the RDP sticky bit (any discarded fraction forces the low
  bit, keeping the half-open edge tests exact). `Set Other Modes` now decodes
  `aa_enable` (bit 3). Verified by hand-computed unit tests (full/partial/empty
  masks, the sticky bit, negative-coordinate shift) derived from the oracle's
  arithmetic — **not** from this implementation's own output.
- **Scope (ledger R-9):** these are the pure, oracle-verified primitives; the
  rasteriser still unions the four sub-scanlines into a whole-pixel bounding span,
  so `compute_coverage` has **no runtime caller yet**. Wiring it in changes every
  triangle's edge pixels (the exact top-left-sample rule kills a degenerate top row
  the union approximation drew), so the pixel-inclusion rewrite — and its re-derived
  goldens — is deferred to the coverage-integration slice, to be validated against
  the ParaLLEl-RDP conformance vectors (T-33-005) rather than against
  hand-derived goldens that could share a systematic error with the port. Oracle 93.

### Added — the memory-read blender on triangles (Phase 3, T-33-004 PR-B 2b-blend)

- **The RDP blends translucent triangles against the framebuffer.** `depth_span`
  now reads the destination pixel (`read_pixel`, the inverse of `write_pixel` for
  both RGBA8888 and RGBA5551) and routes the combiner colour through `blend` when
  the depth test enables blending — gated on `Set Other Modes` `force_blend`, which
  mirrors the reference blender's `!blend_en` fast-path so opaque pixels keep the
  combiner colour and only translucent (or, later, AA-edge) pixels blend with memory.
  This gives `blend` its first runtime caller (closing part of ledger R-11). Verified
  by a translucent-triangle integration test: a shaded triangle (combiner → red,
  alpha `0x80`) over a green background blends 50/50 to `0x7F7F00`, proving the
  memory read ran — plain red would mean it did not. Scope (ledger R-9/R-11): the
  AA-edge divider LUT, the interpenetrating-Z blend-shift, alpha-compare, dither, and
  coverage write-back need the sub-pixel coverage accumulator (slice 2c). Oracle 93.

### Added — perspective-correct texturing (Phase 3, T-33-004 PR-B 2b-perspective)

- **The RDP divides texture coordinates by W.** When `Set Other Modes` `persp_tex_en`
  (bit 51) is set, `interpolate_st` now interpolates `S`/`T`/`W` and runs the
  hardware perspective divide — a faithful port of ParaLLEl-RDP's `perspective_divide`
  (`perspective.h`): the 64-entry reciprocal LUT (`perspective_get_lut`), the
  normalisation shift, the `temp_mask` out-of-bounds saturation, the `w <= 0` carry,
  and the 17-bit clamp. `TexSetup` gains the `W` channel and `decode_texture` reads
  it; `OtherModes` gains `persp_tex_en`. Validated by a hand-computed
  `perspective_divide` test (the LUT reciprocal/shift at `w = 0x4000`/`0x2000`, the
  carry) plus LUT-boundary transcription checks. The non-perspective path
  (`persp_tex_en` clear) is unchanged. Scope (ledger R-13): the exact tile
  shift/clamp/mask for triangle coordinates remains for the conformance pass. Oracle 93.

### Added — the first textured triangle (Phase 3, T-33-004 PR-B part 2b-texture)

- **The RDP samples textures on triangles.** `Fill Textured Triangle` (opcode bit
  57) now decodes the 8-word texture coefficient block (`decode_texture`: `S`/`T`
  base + per-x/per-major-edge deltas, `s16.16`), and each pixel's combiner input
  `texel0` comes from `fetch_texel` at the interpolated coordinate (`interpolate_st`),
  sampled from tile 0. Works standalone and combined with shade/depth. Verified by a
  textured-triangle test that samples a loaded RGBA16 texel (`0xF801` → red) through a
  texel-passthrough combiner, writing `0xFF0000FF` rather than the FILL register.
  Scope (ledger R-13): the coordinate uses the **non-perspective** path
  (`no_perspective_divide`, integer `s16.16 >> 16`) — the perspective divide
  (`W` reciprocal LUT) and the exact tile shift/clamp/mask for triangle coordinates
  are the next slice; the flat-coordinate case (which this test exercises) is
  scale-independent and correct. Oracle stays 93.

### Added — the first shaded triangle (Phase 3, T-33-004 PR-B part 2b)

- **The RDP renders shaded triangles through the colour combiner.** `Fill Shaded
  Triangle` (opcode bit 58) now decodes the 8-word shade coefficient block
  (`decode_shade`: RGBA base + per-x/per-major-edge deltas, `s15.16`), and each
  pixel's colour comes from the combiner fed the interpolated shade
  (`interpolate_shade`, a port of ParaLLEl-RDP's `interpolate_rgba` snap) rather
  than the FILL register — the combiner's first runtime caller. The colour is packed
  to the framebuffer format (`write_pixel`: RGBA8888 direct, RGBA5551 for 16-bit).
  This works standalone and combined with the depth test. Verified by a
  `decode_shade`/`interpolate_shade` unit test (hand-computed base colour) and a
  shaded-triangle integration test that renders the combiner output, not the FILL
  colour. Scope: this closes the R-9 flat-fill for shaded triangles; texture
  (texel0/1) and the memory-read blender are the next 2b/2c slices, and the `dz`
  and sub-pixel coverage remain first-cut. Oracle stays 93.

### Added — the first depth-tested triangle (Phase 3, T-33-004 PR-B part 2a)

- **The RDP renders depth-tested triangles — the first per-pixel pipeline.** The
  `Fill Z-Buffered Triangle` variants (opcode bit 56) now decode the z-coefficient
  suffix (`z`, `dzdx`, `dzde`, `s15.16`) and, when `Set Other Modes` enables the
  depth test/update, run the real per-pixel path: `interpolate_z` computes each
  pixel's 18-bit depth (a faithful port of ParaLLEl-RDP's `interpolate_z` snap for
  the full-coverage case), `depth_test` compares it against the Z buffer, and only
  passing pixels write colour (`zbuffer_write` stores the new depth when `z_update`
  is set). This gives the depth machinery and Z-buffer storage built earlier their
  first runtime caller. Verified by an occluding-triangles test — a nearer triangle
  draws, a farther one is rejected, a nearer-still one overwrites (both the accept
  and reject paths), plus a hand-computed `interpolate_z` unit test. Scope (ledger
  R-9/R-12): the colour still comes from the FILL register (the combiner/blender
  routing and sub-pixel edge coverage are parts 2b/2c); `dz` derivation is a first
  cut. `Set Combine Mode`/non-Z triangles are unaffected. Oracle stays 93 (no
  systemtest ROM drives rendering yet).

### Added — the Z-buffer storage and RDRAM hidden bits (Phase 3, T-33-004 PR-B part 1)

- **The RDP can read and write the Z buffer, including the RDRAM hidden bits.**
  Each Z pixel is 18 bits: `Rdp::zbuffer_write` compresses the depth, packs the
  14-bit result into bits 15:2 of the 16-bit RDRAM halfword with `dz`'s high two
  bits in 1:0, and stores `dz`'s low two bits in the RDRAM **hidden ("9th") bits**;
  `zbuffer_read` reverses it — byte-exact against ParaLLEl-RDP's `store_vram_depth`/
  `load_vram_depth`. To carry those low `dz` bits accurately, `RdramBus` gains
  `rdram_read_hidden`/`rdram_write_hidden` (default no-op, so non-Z bus impls are
  unaffected), and the Bus backs them with a lazily-allocated store (one 2-bit value
  per 16-bit halfword — allocated only once Z-buffered rendering writes to it).
  Validated by a Bus hidden-bit round-trip and a full-`dz` Z-buffer round-trip (a
  `dz` whose low bits survive only via the hidden path). Scope (ledger R-12): the
  coverage accumulator and the per-pixel combiner→blender→depth routing land in PR-B
  part 2 (closing R-9). No runtime caller yet, so the oracle stays 93.

### Added — the Z-buffer machinery (Phase 3, T-33-004 PR-A)

- **The RDP has a depth codec, per-pixel depth test, and depth-source commands.**
  The N64's inverted-floating-point Z encoding (14-bit stored ↔ 18-bit UNORM) is
  implemented as exact inverses of ParaLLEl-RDP's `z_encode.h` (`z_compress`/
  `z_decompress`, plus the 4-bit `log2` `dz` codec). `depth_test` is a faithful port
  of `depth_test.h`: given a pixel's `z`/`dz` and the Z-buffer read (`DepthInputs`),
  it returns the pass/fail plus blend/coverage state (`DepthResult`) across all four
  Z modes — opaque (with the coplanar same-surface path), interpenetrating (the
  coverage-reducing intersect), transparent, and decal — including the stored-`dz`
  coplanar/precision-factor handling. `Set Depth Image` (0x3E) latches the Z-buffer
  base and `Set Primitive Depth` (0x2E) the `z`/`dz` for `z_source_sel`. Validated
  by hand-computed codec boundary values, a `z_compress ∘ z_decompress` round-trip,
  and observable occluding-vs-occluded depth pairs per mode (six new `rdp` tests).
  Scope (ledger R-12): the Z-buffer RDRAM read/write (the hidden-bit storage), the
  coverage accumulator, and the per-pixel combiner→blender→depth routing land in
  PR-B (which closes the R-9 flat-fill). No runtime caller yet, so the oracle stays 93.

### Added — the blender (Phase 3, T-33-003)

- **The RDP evaluates the blender.** `Set Other Modes` (0x2F) decodes the render
  mode — the cycle type, the two blender cycles' `P/A/M/B` selects, `force_blend`,
  the Z enables/mode, the coverage-destination mode, `image_read_en`, and the
  alpha-compare enable — into `OtherModes`, and `blend` evaluates the divide-free
  `(P * a0 + M * (a1 + 1)) >> 5` per channel, where `P`/`M` mux an RGB triple
  (pixel/memory/blend/fog) and `a0 = A >> 3`, `a1 = B >> 3` map the alpha selects
  to the 5-bit blend weights (the `+ 1` on the `M` term is real hardware). 1-cycle
  mode uses blend cycle 0 alone; 2-cycle feeds cycle 0's RGB forward as cycle 1's
  pixel input. `Set Blend Color` (0x39) and `Set Fog Color` (0x38) latch the
  colour registers. Cross-verified against the wiki and ParaLLEl-RDP (MIT) and
  unit-tested bit-for-bit on hand-computed values. Scope (ledger R-11): the
  anti-aliased-edge divider LUT, memory-alpha blend-shift, alpha-compare, dither,
  `color_on_cvg`, and coverage write-back are decoded but unused until the pixel
  pipeline routes through the blender with real framebuffer/coverage/Z (T-33-004).
  `blend` gains that runtime caller in the 2b-blend entry above; the oracle stays
  93 (no systemtest drives the render path).

### Added — the colour combiner (Phase 3, T-33-002)

- **The RDP evaluates the colour combiner.** `Set Combine Mode` (0x3C) decodes
  the 16 RGB/alpha `A/B/C/D` input selects for both cycles, and `combine`
  evaluates `(A − B) * C + D` per channel with the RDP's fixed-point rules — the
  asymmetric 9-bit `special_expand` for `A/B/D`, a plain 9-bit `C`, a `+0x80`
  rounding bias before the `>> 8`, `D` added unscaled, and the 9-bit clamp fold.
  1-cycle mode uses only cycle 1; 2-cycle chains cycle 0 into cycle 1's
  `Combined`. `Set Prim Color` (0x3A) and `Set Env Color` (0x3B) latch the
  constant-colour registers. Cross-verified against the wiki and ParaLLEl-RDP
  (MIT) and unit-tested bit-for-bit on hand-computed values. Scope (ledger R-10):
  the common inputs are wired; the exotic inputs (noise, LOD fraction, key/convert
  constants) read as zero until that state lands. `combine` gains its runtime
  caller in the shaded/textured-triangle entries above; the oracle stays 93 (no
  systemtest drives the render path).

### Added — the flat-fill triangle rasteriser (Phase 3, T-33-001)

- **The RDP rasterises triangles.** `Fill Triangle` (0x08) and its shade/texture/Z
  variants (0x09–0x0F) are decoded and flat-filled: the three edges (major `H`
  yh→yl, minor `M` yh→ym, minor `L` ym→yl) are walked per sub-scanline
  (`s11.2` Y, `s11.16` X, `s13.16` slopes), the span between the major edge and
  the active minor edge is reduced to whole pixels and scissor-clipped, and the
  FILL-mode colour is written — the foundation every later per-pixel ticket
  renders through. `lmajor`/flip selects the fill direction. The edge-walk and
  the fixed-point decode are cross-verified against the N64brew wiki and
  ParaLLEl-RDP (MIT) and pinned by a right-triangle golden. Scope (ledger R-9):
  flat fill only — the sub-pixel coverage and the shade/texture/Z attribute
  interpolation are deferred to T-33-002…005 (the combiner/blender/Z and the
  conformance fuzz); the 0x09–0x0F coefficient words are length-consumed only.
  Oracle unchanged at 93.

### Added — the first textured picture: copy-mode Texture Rectangle (Phase 3, T-32-004)

- **The RDP draws a texture.** `Texture Rectangle` (0x24) blits a tile into the
  colour image in copy mode — the first textured picture and the close of Sprint
  2's texture path. `wrap_coord` turns a raw `s10.5` coordinate into a
  tile-relative texel via shift → subtract-`SL` → mirror → mask (the copy-mode
  order, no clamp), matched to the ParaLLEl-RDP layout. The rectangle steps `S`
  across X (scaled by the 4-pixels-per-cycle factor so a 1:1 blit's `DsDx = 4.0`
  is one texel/pixel) and `T` down Y, copying the raw 16-bit texel into the
  colour image, scissor-clipped. This is the first **two-word** command: `tick`
  now passes the command's RDRAM base to `dispatch` so multi-word handlers read
  their later words. Validated by a round-trip identity test (a `Load Tile`
  texture blitted back reproduces the source byte-for-byte) plus a `wrap_coord`
  unit test. Scope (ledger R-8): the 16-bit → 16-bit path; `Flip`, the 8/32-bit
  and TLUT copy paths, non-1:1 sub-texel selection, and the copy alpha-compare
  are deferred to the Sprint-3 fuzz. Oracle unchanged at 93.

### Added — RDP texel-format decoders and Load TLUT (Phase 3, T-32-003)

- **The RDP decodes texels and loads palettes.** `Rdp::fetch_texel(tile, s, t)`
  returns RGBA8888 for RGBA16/32, IA16/8/4, I8/4, and CI8/4 (through the TLUT,
  with CI4 folding the tile palette in as the high index nibble) — the fetch half
  of the texture pipeline. `Load TLUT` (0x30) quadruples each 16-bit entry into
  high TMEM. Decode is matched to the ParaLLEl-RDP read layout: TMEM is a natural
  big-endian byte array with only the odd-row `^= (t & 1) << 2` swap (no host-word
  endian twiddles, consistent with the loads). Six unit tests pin the per-format
  widening, the RGBA32 split, the odd-row swap, the TLUT lookup, and dispatch
  routing. YUV16 and 4-bit *loading* remain deferred (R-7); 4-bit *fetch* is done.
  The `Load TLUT` base is written wherever `tmem_addr` points (the upper-half /
  alignment rule is a programmer requirement, not a hardware rejection — enforcing
  one would invent behaviour). Oracle unchanged at 93 (`fetch_texel` gains its
  runtime callers in the texture-rectangle and textured-triangle entries; no
  systemtest drives the render path).

### Added — RDP TMEM loads: Load Tile and Load Block (Phase 3, T-32-002)

- **The RDP now moves texels from RDRAM into TMEM.** `Load Tile` (0x34) copies an
  inclusive `SH − SL + 1` rectangle at the tile's `line` stride; `Load Block`
  (0x33) streams a linear run, enforcing the 2048-texel limit (over it writes
  nothing) and deriving line parity from the `dxt` (u1.11) counter. Both apply the
  **odd-row 32-bit-word swap** (`dst ^= 4`), and `Load Tile` implements the
  **32-bit RGBA split** (R,G to the low half of TMEM, B,A to the high half). The
  address arithmetic and swizzle are cross-verified against the N64brew wiki and
  the ParaLLEl-RDP reference (MIT). TMEM is allocated on first write via a shared
  `tmem_write` helper. Five unit tests pin the byte layout. Scope (ledger R-7):
  8/16/32-bit `Load Tile`, 8/16-bit `Load Block`; 4-bit and the 32-bit `Load
  Block` split are deferred. Oracle unchanged at 93 (a load is observable only
  once the sampler reads TMEM, T-32-004).

### Added — RDP texture state: TMEM, tile descriptors, state commands (Phase 3, T-32-001)

- **The RDP gains its texture state and the three commands that describe it.**
  `Rdp` now owns a 4 KiB TMEM and eight `TileDescriptor`s, and the dispatcher
  handles `Set Texture Image` (0x3D), `Set Tile` (0x35), and `Set Tile Size`
  (0x32) — decoding every field per the N64brew command tables. This is pure
  state: no texel is loaded and no pixel changes yet (the loaders and sampler are
  T-32-002..004). TMEM is a lazily-allocated `Option<Box<[u8; 4096]>>` so the
  per-RCP-tick `core::mem::take` in `Bus::rdp_tick` stays cheap (a `None`
  placeholder, no 4 KiB allocation or copy). Field-by-field unit tests pin each
  decode; the n64-systemtest oracle is unchanged at 93 (nothing rendered changed).

### Added — the first golden frame (Phase 3, T-31-005)

- **The Sprint-1 picture path is pinned by a committed golden.** A harness
  integration test (`tests/golden_frame.rs`) drives an RDP FILL command list into
  a framebuffer in RDRAM, scans it out through the VI to RGBA8, and asserts the
  frame is byte-exact and its FNV-1a hash matches a committed golden
  (`compare_to_golden`) — so any regression in the command decoder, the FILL
  pipeline, or the VI scan-out changes the frame and fails the test.
  Mutation-checked (changing the fill colour fails it). The frame comes from a
  synthetic command stream rather than a commercial ROM (cartridge boot is Phase
  5); the path from the DP FIFO onward is identical.

### Changed — the frontend presents the VI scan-out (Phase 3)

- **`EmuCore::produce_frame` now blits the core's framebuffer**, not a
  placeholder gradient: it calls `Bus::scanout` each frame and presents the
  RGBA8 result, falling back to a black frame at the default resolution while the
  VI is off (cold boot / no ROM). The "first picture" display loop —
  RDP FILL → framebuffer → VI scan-out → frontend blit — is now closed.

### Added — VI scan-position timing and interrupt (Phase 3, T-31-004 part 3)

- **`VI_V_CURRENT` advances and the VI interrupt fires.** `Vi::tick` (called each
  RCP step) advances the scan half-line one at a time off the elapsed
  `master_ticks` (accumulating the fractional remainder), wrapping at
  `VI_V_TOTAL + 1`, and raises `MI_INTR.vi` when the position lands on
  `VI_V_INTR`. The per-half-line step means a call spanning many half-lines cannot
  skip it, and a `VI_V_INTR` beyond the field never fires. `VI_CTRL.TYPE == 0`
  suppresses it, and the position is kept relative so a mid-run `VI_V_TOTAL`
  change re-bases without a scale jump. A `System::reset` rebases the scan
  timeline (`Vi::reset_scan`) so the interrupt keeps firing across a reset. This
  is the vsync signal games wait on; covered by unit tests (advance/wrap,
  once-per-field firing, disabled-VI, unreachable `VI_V_INTR`, mid-run
  `VI_V_TOTAL` change) and scheduler integration tests (interrupt during a run,
  and acknowledge → reset → fires again).
- **Scope:** the field cadence is anchored to nominal 60 Hz NTSC (open residual
  **R-6**) — the VI dot clock is off a separate crystal the wiki gives only
  roughly, so the sub-field `H_TOTAL` timing, PAL's 50 Hz, and the interlace
  `VI_V_INTR` bit-0 quirk are deferred. **Oracle:** n64-systemtest unchanged at 93
  failing (Phase 1 still 0) — the VI interrupt now firing during a run does not
  regress the CPU categories, and no VI assertion flips yet (needs the masks and
  the exact timing).

### Added — VI framebuffer scan-out (Phase 3, T-31-004 part 2)

- **`Bus::scanout` converts the framebuffer to a presentable RGBA8 frame.** It
  reads `VI_ORIGIN`/`VI_WIDTH`/`VI_CTRL` and the active height from `VI_V_VIDEO`,
  and converts **16-bit RGBA5551** (5→8-bit channel expansion, 1-bit alpha to
  0/255) and **32-bit RGBA8888** (direct copy); a blanked VI (`TYPE == 0`) scans
  out nothing. This is what turns the FILL pipeline's pixels into a picture.
- **Scope:** a 1:1 scan — `VI_X_SCALE`/`VI_Y_SCALE` resampling and the
  AA/divot/de-dither post-filters are deferred (open residual R-5), and there is
  no per-frame driver yet (the scheduler tick that calls it lands with
  `V_CURRENT`). Byte-for-byte unit-tested at 16- and 32-bit; the 5→8 expansion is
  mutation-checked.

### Added — the VI register file (Phase 3, T-31-004 part 1)

- **The Video Interface register block is implemented** (`rustyn64_core::vi::Vi`,
  wired to `0x0440_0000`): the sixteen registers `VI_CTRL`…`VI_STAGED_DATA` read
  and write through the CPU bus. Writing `VI_V_CURRENT` acknowledges the VI
  interrupt (`MI_INTR.vi = false`); cold-boot state is all-zero, so the VI is off
  (`VI_CTRL.TYPE == 0`).
- **Staged for the follow-up VI tickets:** per-register write masks (stored
  full-width for now, to be pinned against n64-systemtest rather than guessed).
  (The scan position, the `VI_V_INTR` interrupt, and framebuffer scan-out landed
  in parts 2–3, above.)

### Added — the RDP FILL pipeline (Phase 3, T-31-003)

- **The RDP writes solid rectangles into the framebuffer.** `Set Color Image`
  (0x3F), `Set Fill Color` (0x37), `Set Scissor` (0x2D), and `Fill Rectangle`
  (0x36) now dispatch, so a FILL-mode rectangle writes the fill colour into RDRAM,
  clipped to the scissor. Per pixel size: 32-bit writes the whole colour, 16-bit
  alternates the colour's halves (even = upper, odd = lower), 8-bit writes byte
  `x & 3` — i.e. the 32-bit fill value repeated verbatim. A 4-bit target is
  skipped (it crashes the real RDP). Byte-for-byte unit-tested at 8/16/32-bit and
  for four-edge scissor clipping; the clip and half-selection are mutation-checked.
- **Scope:** the FILL-mode path only — the cycle-type gate (`Set Other Modes`)
  and the exact sub-pixel edge rules (validated against Angrylion via the fuzz
  suite, Sprint 3) are later work. **Oracle:** n64-systemtest unchanged at 93
  failing suite-wide (same as `v0.3.0`); a fill becomes observable to the suite
  only once VI scan-out lands (T-31-004).

### Added — the DP FIFO command decoder (Phase 3, T-31-001)

- **`Rdp::tick` drains the DP FIFO.** It decodes the command at `DPC_CURRENT`,
  recognises every opcode `0x00`–`0x3F` from the N64brew command map, and
  advances the pointer by each command's **full length** — one command per
  scheduler tick — so a multi-word primitive (a triangle, a texture rectangle)
  is consumed whole and the stream never desyncs. No opcode is rasterized yet:
  each command is recognised, its length consumed, and a retired-work counter
  (`commands_processed`) incremented; dispatch and the fill pipeline follow.
- **Length rules** (`rustyn64_rdp::command`): one 64-bit word for every command
  except the variable-length **Fill Triangle** forms (`0x08`–`0x0F`, a 4-word
  base plus shade/texture/z coefficient blocks selected by the opcode's low
  three bits) and the two-word **Texture Rectangle** pair (`0x24`/`0x25`).
  Exhaustively unit-tested across the whole map, and a mixed-list walk asserts
  the decoder lands `DPC_CURRENT` exactly on `DPC_END`.
- **Two stall conditions** keep the decoder off invalid data: a command is
  consumed only once it is present in full (so an incrementally-advanced
  `DPC_END` that lands mid-command waits for the rest rather than decoding
  unwritten RDRAM), and `XBUS` mode (DMEM command source, not yet wired) stalls
  rather than mis-reading RDRAM.

### Added — the RDP sync commands and the DP interrupt (Phase 3, T-31-002)

- **`Sync Full` (0x29) raises the DP interrupt.** The RDP dispatcher calls
  `raise_dp_interrupt` → `MI_INTR.dp`, which asserts the VR4300 IP2 line once the
  DP line is masked in — the end-to-end path (dispatcher → `VideoBus` seam → MI →
  mask) is covered by a core integration test that drives a real `Sync Full`
  command through `rdp_tick`.
- **`Sync Load`/`Pipe`/`Tile` (0x26/0x27/0x28) stall the pipeline** for their
  documented fixed, unconditional GCLK counts (25/50/33), modelled by a `stall`
  countdown that holds the FIFO until it expires (one `tick` = one GCLK). These
  are documented constants (N64brew command map), cited in code rather than
  fitted — no accuracy-ledger entry.
- **Oracle effect:** n64-systemtest failing-assertion count unchanged at 93
  suite-wide (same as `v0.3.0`) — sync dispatch flips no assertion, as the
  remaining failures need the RDP rasteriser (Phase 3) or cart/PIF (Phase 5).

## [0.3.0] — 2026-07-22 — "Microcode"

**Phase 2 is complete.** The **LLE RSP** runs — the scalar unit, the 8-lane vector
unit, SP DMA, and the halt/break/interrupt handshake — and libdragon's real `rdpq`
microcode boots on it and emits an RDP command list. Both of Phase 2's exit
criteria are met as **oracle results with committed runners**, not
self-assessments:

| Criterion | Result |
| --- | --- |
| n64-systemtest `Failed: 0` (RSP category) | **met** — 0 RSP-prefixed failures of 917 tests started (suite-wide down 413 → 93) |
| A real graphics microcode emits an RDP command list | **met** — libdragon's `rdpq` boots and emits, witnessed byte-for-byte |

```bash
cargo test -p rustyn64-test-harness --release --test systemtest -- --ignored
cargo test -p rustyn64-test-harness --test microcode
```

Phase 1's criteria remain met (CPU/COP0/TLB/COP1 `Failed: 0` and the ares
golden-log 0-diff). The 93 assertions that still fail suite-wide are cart/PIF
(Phase 5) and the RDP rasteriser (Phase 3), each that phase's criterion.

### Added — the LLE RSP scalar + vector units (#35–#43)

- **The scalar unit executes** (T-21-002/004/005, #36): the MIPS subset the RSP
  actually has, with the two rules that catch a CPU core reused wholesale — the
  **12-bit PC that wraps** and **misaligned data accesses that are correct, not
  faults**. `BREAK` halts the core; DMEM/IMEM are a Harvard pair (T-21-001, #35).
- **The vector unit** (Sprint 2, #37–#39, #41): the full 8-lane VU — the register
  file and SU/VU moves, the 48-bit-per-lane accumulator and the multiply family
  (single and accumulating forms), add/subtract/carry, the compare/select group
  (`VLT`/`VEQ`/`VMRG`/…), the clip compares (`VCL`/`VCH`/`VCR`), `VRND`/`VMULQ`/
  `VMACQ`, and the reciprocal/rsqrt units driven from committed ROM tables. A
  **read-before-write snapshot** models the hardware reading the whole broadcast
  operand before writing any lane, so a destructive broadcast cannot corrupt itself.
- **The whole vector load/store family** (#38, #41): `LQV`/`SQV`, `LRV`/`SRV`,
  `LPV`/`LUV`, `SWV`, and the element-wrap edge cases, with the store register-wrap
  and full-count fixes n64-systemtest pins.
- **The reserved "VZERO" opcode family** (#42): 19 undocumented COP2 encodings
  that all write `ACC_LO = vs + vt`, zero `vd`, and touch no flags.
- **`BREAK` in a taken branch's delay slot** halts at the branch target, not the
  sequential address (#43).

### Added — the RDP DPC command registers and the RSP↔RDP seam (#44, #53)

- **The DPC command-register file** (#44): `DPC_START`/`END`/`CURRENT`/`STATUS` at
  `0x0410_0000`, the `START_VALID` double-latch and `FREEZE` — completing the
  RSP-category criterion. Provenance N64brew *Reality Display Processor/Interface*.
- **The RSP↔RDP command seam** (#53): the RSP's COP0 registers `c8`–`c15` **are**
  the RDP command registers, but `rustyn64-rsp` may not name `rustyn64-rdp` (the
  crate-graph rule in `docs/architecture.md`), so an `MTC0` to them is reported as
  the new `su::StepResult::dp_write` and `Bus::rsp_tick` forwards it to
  `Rdp::dpc_write`. DP access is restricted to `c8`–`c15`; `c16`–`c31` read zero.
  `Rdp::DPC_ADDR_MASK` is now public.

### Added — the real microcode boot harness (ADR 0008; #46–#53)

**Phase 2's second exit criterion.** Rather than a toy microcode, the harness runs
libdragon's real combined RSPQ+`rdpq` blob (vendored under
`third_party/libdragon-rsp/`, Unlicense; assembled with `mips64-elf-gcc`, with the
blob, symbol map, and `SHA256SUMS` committed and gated in CI). Golden references are
grounded in **hardware docs, never another emulator** (ADR 0008).

- **Boots to its idle `break`** (T-24-002, #51), reproducing the `rspq_start`
  boot state in Rust, witnessed from an unreachable-as-pass baseline.
- **DMAs and dispatches a command queue** (T-24-003 foundation, #52).
- **Emits an RDP command list** (T-24-003/004, #53): an `rdpq` overlay command is
  dispatched to its resident handler (the overlay "registered" with three DMEM
  writes, reproducing `rspq_overlay_register_internal`), the command bytes are
  DMA'd to an RDRAM output buffer, and `DP_END` is advanced through the DPC seam.
  Two golden cases: `RDPQCmd_Passthrough8` (raw 8-byte forward) and
  `RDPQCmd_SetFillColor32` (the microcode *generates* a `SET_FILL_COLOR` command,
  byte-compared against N64brew's documented `0x37` encoding). Spec:
  `docs/rspq-boot.md`, `docs/rsp.md`, `docs/rdp.md`.

### Changed — infrastructure

- **Antigravity PR reviewer** (#49, #50): a self-hosted-runner review workflow
  (Gemini via Ultra) posting first-pass reviews as the `github-actions` bot, with
  job-level concurrency + `flock`/retry so CodeRabbit comments no longer cancel it.

## [0.2.0] — 2026-07-21 — "Interpreter"

**Phase 1 is complete.** The VR4300 executes MIPS III — including the TLB, COP0, the FPU and the
documented errata — and both of Phase 1's exit criteria are met as **oracle results with committed
runners**, not self-assessments:

| Criterion | Result |
| --- | --- |
| n64-systemtest `Failed: 0` (CPU/COP0/TLB/COP1) | **met** — 0 failing of 917 tests started |
| CPU golden-log 0-diff | **met** — retired-instruction stream identical to ares |

```bash
cargo test -p rustyn64-test-harness --release --test systemtest -- --ignored
cargo test -p rustyn64-test-harness --release --test golden_log -- --ignored
```

413 assertions still fail suite-wide; every one is RSP/RCP, which is **v0.3.0's** criterion.

The per-change entries below record how the CPU got from 99 failing assertions to 0. They are kept
as written — each states what a specific change moved, which is what a changelog is for.

### Added — the CPU golden-log 0-diff, and EMUX

**Phase 1's second exit criterion is met:** `RustyN64` reproduces ares's retired-instruction stream
exactly. `tests/golden/n64-systemtest.log` is captured from ares at the ELF entry, with a provenance
header naming the reference build, ROM hash and start PC; `--test golden_log` is the gate.

The claim is deliberately narrow, and stated as such: *given identical initial state, `RustyN64`
retires the same instructions in the same order as the reference.* That is the **tandem-verification**
shape used by RISC-V co-simulation harnesses — align at a boundary, compare only deltas. It says
nothing about boot or timing. Ledger **C-26**.

`Count`, `Random` and `Compare` are excluded from comparison because there is no correct value:
libdragon's IPL3 zeroes `Count` mid-boot then accumulates timing-dependent PI/SI waits, and its own
`pi_wait()` feeds the result to `entropy_add()`. Upstream treats a boot-relative `Count` as entropy.

**EMUX** (`xdetect`/`xlog`/`xioctl`) is implemented behind `Bus::emux_enabled`, **off by default**
because hardware has none — ares gates its own EMUX on `homebrewMode` for the same reason, and
advertising capabilities changes which console n64-systemtest selects, and therefore the instruction
stream. The systemtest harness opts in: `xlog` needs no PI/SI/`ISViewer` emulation (~9x faster) and
`xioctl(EXIT)` replaces a tick budget with a definite end-of-run. Ledger **C-27**.

### Fixed — an in-flight `C.cond.fmt` is forwarded to `BC1`

**This clears Phase 1's cut criterion: n64-systemtest's CPU/COP0/TLB/COP1 categories are at
`Failed: 0`**, across 917 tests started — reproducible with
`cargo test -p rustyn64-test-harness --release --test systemtest -- --ignored`, which is committed
alongside it.

`BC1` resolves in `EX` while `C.cond.fmt` commits `FCSR.C` in `WB`, so an adjacent pair sampled the
previous condition — and the ROM emits exactly that pair with no separating instruction.

Fixed by a forwarding path, not a stall. `stall_for` freezes every stage, so holding the branch
delays the compare's `WB` by the same amount and the gap never closes; an interlock was written,
traced, and shown to fire once and change nothing. The load interlock is not a counter-example — it
works because its consumer reads through the bypass network, and the FP condition had no such path.

Re-evaluating the pending compare is sound because it reads two FP registers and writes only
`FCSR.C`; nothing between it and the branch can change those registers. Flags are discarded, since a
forwarding path must not raise the compare's trap on the branch's behalf. Ledger **C-25**.

**Phase 1's categories: 1 → 0.**

### Added — `BC1F`/`BC1T`/`BC1FL`/`BC1TL`, branch on the FP condition

They were not implemented: the branch decoded to `Cop1Unimplemented` and retired as a no-op, so a
program branching on a compare simply fell through. COP1 `rs = 0o10`, bits 17:16 as `nd:tf` — the
four encodings are true/false crossed with likely/not. Target arithmetic and branch-likely
nullification are shared with every other branch.

`FCSR.C` is passed into `execute` as a parameter rather than reached for: that function is pure and
has no view of coprocessor state, and a parameter makes every call site a compile error until it
supplies one.

**Still outstanding**, and now the sole Phase 1 failure: `BC1` reads the condition in `EX` while
`C.cond.fmt` writes it in `WB`, so an adjacent pair samples the previous condition. Hardware
interlocks; we do not yet. A first attempt is documented in ledger **R-2** along with why it is not
sufficient — the fix must come from tracing where the compare actually is, not from choosing a stall
count that makes the ROM pass.

**Phase 1's categories: 1 remaining.**

### Fixed — the unaligned family takes the byte swap under `Status.RE`

`LWL`/`LWR`/`SWL`/`SWR` and their doubleword siblings address **individual bytes**, so `RE` moves
them by `addr ^ 7` rather than by their container's width. One XOR relocates the container and
complements the byte index at once: `LWL 0` becomes container 4 with byte index 3, since `0 ^ 7 == 7`,
`7 & !3 == 4` and `7 & 3 == 3`.

Derived from the ROM's own expected tables rather than guessed — `SWL` at offset 0 writes a single
byte, `rt`'s most significant, into the doubleword's *last* byte, which no width-based swap produces.
This was left deliberately open in the earlier `RE` change for exactly that reason.

**Phase 1's categories: 3 → 1.**

### Fixed — tininess is detected before rounding, not after

Underflow was decided from the *packed* result: subnormal or zero raised it, normal did not. IEEE 754
permits tininess to be judged before or after rounding, and the VR4300 judges it **before**.

The two differ exactly when a directed rounding mode lifts a tiny result back into the normal range.
`FLT_MIN / (1 + 1ulp)` under round-toward-+∞ yields `FLT_MIN` — a perfectly normal number — and
hardware still raises underflow, because the value it rounded *from* was tiny. n64-systemtest's
`DIV.S` set contains that case in both signs; the result value was already right, only the flag was
missing.

Mutation-checked, and the mutation takes down three tests rather than one, which is the useful
signal: the flag is now decided in one place instead of being re-derived per exit path.

**Phase 1's categories: 5 → 3.**

### Fixed — a TLB tag is masked by `PageMask`, not divided by the page size

`EntryHi`'s tag keeps every bit `PageMask` does **not** cover. It was stored as `VA / pair_size`,
which clears the tag's *low* bits — right for all six legal page sizes, since those are contiguous
runs from bit 13, and wrong once a canonicalised mask has a hole. `0b11_11_11_11_00` covers bits
22:15 and leaves 14:13 alone; division cannot express that.

The tag is now held **in place**, masked rather than divided, so `vpn2_of` and the read-back agree by
construction. Mutation-checked: restoring the divide turns the new test red on exactly the
holed-mask row and no other.

**Phase 1's categories: 6 → 5.** All TLB items pass.

### Fixed — a `PageMask` pair stores only its higher bit

`PageMask` bits 24:13 are six 2-bit pairs, and an entry does not store twelve independent bits: a
pair reads back as `11` exactly when its **higher** bit was written, and `00` otherwise. So `0b10`
becomes `0b11` and `0b01` is discarded.

The natural implementation — keep the value, mask to 24:13 — is wrong in both directions and
silently so: it accepts page sizes the hardware has no encoding for, and reports back a mask that
was never stored. Canonicalisation happens on **write**, which is where the information is actually
lost. Mutation-checked: replacing it with the plain mask turns the new test red.

**Phase 1's categories: 7 → 6.**

### Fixed — a to-integer conversion refuses an integer source format

`CVT.W.W`, `CVT.W.L`, `CVT.L.W`, `CVT.L.L` and the whole `ROUND`/`TRUNC`/`CEIL`/`FLOOR` family from
`.W`/`.L` are **not instructions**. They were reaching the `Cop1Unimplemented` fallthrough, which
deliberately does not raise, so they retired silently — n64-systemtest saw no exception where it
expects Unimplemented Operation.

They now decode into the arithmetic path and are refused there, alongside the subnormal and 2^53
cases. The refusal is checked **before** the source is read as a float: an integer source format has
no float to widen, and reading one anyway produces a plausible number for an instruction that does
not exist.

**Phase 1's categories: 11 → 7.**

### Fixed — integer-to-float conversion honours `FCSR.RM`

`CVT.S.W`, `CVT.S.L`, `CVT.D.W` and `CVT.D.L` were each a Rust `as` cast plus a round-trip inexact
check. `as` rounds to nearest-even *unconditionally*, so the mode was ignored — while the round-trip
check still reported `inexact` correctly, which made the flags right and the value wrong. Flags
agreeing is not evidence that the value does.

All four are now `softfloat::from_int`, which is the shared rounding point with a zero exponent and
no sticky bit — an integer is just `sign × |v| × 2^0`. Routing it through the same `round_pack` as
every other operation is what makes the mode impossible to forget.

The four old converters were **deleted**, not left unused: an unused function that quietly gets an
operation wrong is the inert-API hazard `docs/engineering-lessons.md` §3.2 describes.
`long_convertible` stays, since the VR4300 range restriction is a separate rule. Ledger **C-24**.

**Phase 1's categories: 16 → 11.**

### Fixed — `PRId.Rev`, the `Random` counter, and `BadVAddr` on an unaligned fault

Three unrelated one-line rules, each with a reason it stayed wrong:

- **`PRId` now reads `0x0B22`.** The Rev field was recorded as undocumented and left zero — true of
  the User's Manual, false of the N64brew wiki this project mirrors, which names `0x10`/`0x22`/`0x40`
  for early, later and iQue parts. Ledger **U-3** is superseded by **C-22**; this is the third time
  a decayed "undocumented" claim has been cited as if it described the hardware.
- **`Random` is a plain 6-bit down-counter** whose reload fires on `== Wired`, not `<= Wired`, and
  whose decrement wraps 0 → 63. The two readings agree for `Wired <= 31` and diverge above it, where
  the old one pinned the register at 31 forever. Ledger **C-23**.
- **`BadVAddr` reports the address the instruction named**, not the container address translated on
  its behalf. A fault on `SWL 0x12345001` reported `0x12345000`.

**Phase 1's categories: 19 → 16.**

### Fixed — under `FR = 0`, `fs` and `ft` resolve differently

A floating-point arithmetic instruction ignores the low bit of `fs` and does **not** ignore the low
bit of `ft`. The destination `fd` is used as-is in both modes.

The manual declines to specify this — an odd register with `FR = 0` is "undefined" — so the rule is
measured against n64-systemtest and recorded as ledger **C-21**. Two rows settle it, and no single
mapping satisfies both: `SQRT.S $13, $31` yields `sqrt(16)`, so `fs = 31` read FGR30; `ADD.S $2,
$28, $31` yields `-10 + -16`, so `ft = 31` read FGR31. The suite then says it outright in its own
assertion messages.

This does **not** revise C-14, which governs `MTC1`/`LWC1` and the doubleword coprocessor moves —
those do reach an odd register's high half. Separate accessors keep the two classes apart.

**Phase 1's categories: 23 → 19.**

### Added — the 64-bit operations are Reserved in 32-bit User and Supervisor mode

`DADD`, `DSLL`, `LD`, `SD` and the rest of the MIPS III doubleword set raise Reserved Instruction
when the current mode's `UX`/`SX` bit is clear and the mode is not Kernel. Kernel may use them at
any width, which is why this cannot be a property of `Status.KX` alone.

Both halves of that condition are load-bearing, and each is mutation-checked: gating on the width
bit alone reserves them for a 32-bit *kernel*, and gating on the mode alone reserves them for a
64-bit *user* program — the second of which nothing common does, so it would sit unnoticed behind
the rows that pass.

Deliberately excluded: `DMFC0`/`DMTC0` and `DMFC1`/`DMTC1`. Doubleword moves to and from a
coprocessor follow that coprocessor's own usability and reserved-encoding rules (ledger **C-18**)
and raise different exceptions; folding them in would make an unusable COP0 report the wrong cause.

**Phase 1's categories: 24 → 23.**

### Fixed — the segment map depends on the privilege mode, and on the addressing width

`KSEG0` does not exist in User mode. Until now the map was a function of the address alone, so a
user program reached `0x8000_1000` exactly as the kernel does; the address-space check, not the TLB,
is what is supposed to stop it. The map is now a function of `(address, mode, width)`:

- **User** sees `USEG` alone; **Supervisor** sees `SUSEG` and `SSEG`; **Kernel** sees the whole map.
  Anything else is an address error, raised **before** the TLB is consulted. That distinction
  matters: folding it into a TLB refill would send the offending program to the refill handler,
  where a well-behaved kernel maps the page and grants the access it was never allowed to make.
- With `Status.KX`/`SX`/`UX` set, each mapped segment widens to 2^40 and the space grows holes — an
  address inside a segment's region but past its size faults. Kernel additionally gains **XKPHYS**,
  eight 2^32 direct windows differing only in cacheability.

**Under 32-bit addressing an address must be the sign extension of its low word.**
`0x0000_0000_8000_1000` is not a shorthand for `KSEG0`; it is an address error, and n64-systemtest
asserts that directly. The old code truncated to `u32`, which accepted it silently — and the
project's own reset vector and ROM entry point were stored in the truncated form, so both are
corrected here too.

**Phase 1's categories: 27 → 24.**

### Added — `Status.RE`, reverse endian in User mode

Reversing endianness on a 64-bit datapath is a permutation of byte lanes within the doubleword,
expressed as an XOR of the low address bits: a doubleword does not move, a word moves by 4, a
halfword by 6, a byte by 7. Kernel and Supervisor are unaffected, and so is any access taken while
`EXL`/`ERL` forces kernel mode.

**Instruction fetch is swapped too** — it is a 4-byte access like any other. That is why the test
ROM emits its reverse-endian programs with each instruction *pair* exchanged.

The swap is applied to the **physical** address, after translation, which touches only bits 2:0 and
is therefore exactly equivalent to swapping the virtual address first — and it keeps `BadVAddr` raw
on a fault, which the suite asserts directly.

The `LWL`/`LWR`/`SWL`/`SWR` family under `RE` is **not** done: it addresses individual bytes, so it
needs the byte-granular rule rather than its container's width. Two assertions still fail there.

**Phase 1's categories: 31 → 27.**

### Added — the primary caches, and `CACHE` now has state to operate on

A 16 KiB instruction cache (32-byte lines) and an 8 KiB write-back data cache (16-byte lines),
both direct-mapped, in `crates/rustyn64-cpu/src/cache.rs`. Instruction fetch and every cached load
and store run through them; a cached store is a write-allocate that leaves the line dirty until a
`CACHE` operation or an eviction forces it out.

All thirteen `CACHE` operations now act: index invalidate / load tag / store tag, hit invalidate,
hit write-back, hit write-back invalidate, create-dirty-exclusive and I-cache fill. `Index_Load_Tag`
and `Index_Store_Tag` move a line's tag through COP0 `TagLo`.

Three details each of which passes a plausible-looking wrong implementation:

- **Invalidation keeps the tag.** Only the valid bit clears; `Index_Load_Tag` still reports the PFN.
- **The two caches encode a valid line differently** — `PState` 2 for the I-cache, 3 for the
  D-cache. One shared constant satisfies any test that only checks "non-zero".
- **The dirty bit has no `TagLo` field**, so a clean valid D-cache line and a dirty one read back
  identically. That is the hardware, not an omission.

Indexing is by **physical** address where the hardware indexes virtually, which makes cache aliases
impossible rather than merely unlikely — a deliberate deviation, recorded as ledger **D-6**.
Ledger **D-5** (`CACHE` as an address-translating no-op) is superseded, not edited: it named the
boundary — "sound only while no cache state exists to become stale" — that this work crossed.

**Phase 1's categories: 40 → 31.**

### Added — COP2 as a single latch, and a not-taken `BGEZAL` links correctly

COP2 is not a register file: it is one 64-bit latch that every `MTC2`/`DMTC2` writes and every
`MFC2`/`DMFC2` reads, with the register index ignored. `MFC2` returns the low half sign-extended.
Ledger **C-20** — the same shape as the reserved COP0 registers (C-15).

A **not-taken** `BGEZAL` in another jump's delay slot now links through the same `next_pc` path as
the taken forms; it was the last place the old `pc + 8` formula survived.

**Phase 1's categories: 42 → 40.**

### Fixed — a jump-and-link inside a delay slot links past the outer target

The link register receives the address of the instruction that runs after the jump's delay slot.
That is `pc + 8` only when the jump is not *itself* in a delay slot; when it is, its own delay slot
never runs and the link is the outer jump's `target + 4`.

The fix removes a formula rather than adding one: `EX` now fills the link from the live `next_pc`,
which is that address by construction in both cases. Ledger **C-19**.

**Phase 1's categories: 45 → 42.**

### Fixed — the doubleword control moves now trap, and differently per coprocessor

`DCFC1`/`DCTC1` raise a floating-point exception with `FCSR.Cause` set to unimplemented-operation
only; `DCFC2`/`DCTC2` raise Reserved Instruction with `Cause.CE = 2`. All four previously fell into
the catch-all unimplemented arm and retired silently.

`Cause.CE` is not only for Coprocessor Unusable — it names the coprocessor for a reserved encoding
inside a *usable* one too, which needed a distinct `Exception::CoprocessorReserved`. Ledger
**C-18**.

**Phase 1's categories: 49 → 45.**

### Fixed — `FCR0.Imp` is `0x0A`, and `CTC1` can raise on its own

`FCR0` reports implementation `0x0A`, not the `0x0B` this implementation used. `0x0B` is correct
for `PRId.Imp` — the *CPU's* register — and the two were conflated. The N64brew Wiki states `0x0B`
for `FCR0` too and is wrong; n64-systemtest and cen64 independently give `0x0A00`. Accuracy ledger
**S-4**, which is about how to use the wiki rather than a reason to stop.

`CTC1` writing `FCSR` with a Cause bit whose Enable is also set now raises an FP exception
immediately, reporting the `CTC1` itself as `ExceptPC`. Ledger **C-17**.

**Phase 1's categories: 53 → 49.**

### Fixed — reserved COP0 registers are a shared write latch; `EntryLo` is 30 bits wide

COP0 registers 7, 21..=25 and 31 are not storage: a write goes nowhere and a read returns the value
of the most recent `MTC0`/`DMTC0` to **any** COP0 register. This replaces the guess recorded as
accuracy-ledger **U-1** ("discards writes and reads zero"), which the manual's silence licensed and
which n64-systemtest documents as wrong. Ledger **C-15**.

`EntryLo0`/`EntryLo1` are writable to bit **29**, not bit 25. The architectural fields account only
for bits 25:0, and the mask had been derived from them; bits 29:26 store and read back exactly as
written. Ledger **C-16** — a field table and a writable-bits mask are different documents.

**Phase 1's categories: 60 → 53.**

### Fixed — `FR = 0` addresses whole even registers, not FGR pairs

With `Status.FR = 0` an FPR addresses **FGR `n & !1` in its entirety**; odd FGRs are unaddressable,
and a 32-bit access picks the low half for an even register number and the **high** half for an odd
one. The previous model assembled a value from two registers' low halves — which round-trips
perfectly through `DMTC1`/`DMFC1` and is still wrong, because hardware never touches the odd
register.

Two more behaviours fell out of the same tests: a single-precision **arithmetic** result *clears*
the other half of its destination while `MTC1`/`LWC1` *preserve* it (now `write_s_arith` vs
`write_s`), and `MOV.S` moves **all 64 bits** rather than the formatted half.

Alongside it, C-13's subnormal-result policy gained its missing half: a result that underflows
**past** the subnormal grid to zero is refused too. `is_subnormal` and `flags.underflow` are both
needed — neither implies the other, since IEEE signals underflow only when tiny *and inexact*.

A float-to-`.L` conversion also refuses a magnitude of **`2^53`** or more — far narrower than
`i64`, and bracketed by the suite rather than assumed.

**Phase 1's categories: 99 → 60.** The entire odd-index cluster reached zero. Accuracy ledger
**C-14**.

### Added — `SQRT` and a correctly-rounded `CVT.S.D`

`softfloat` gains `convert` (format narrowing/widening) and `sqrt`, both correctly rounded with
exact IEEE flags under all four `FCSR.RM` modes, and both verified bit-for-bit against the native
operators across 100,000 cases.

`CVT.S.D` is arithmetic — it rounds 53 significand bits into 24, so it can be inexact, overflow, or
land in the subnormal range, and each depends on the rounding mode. It previously used `v as f32`,
which reports none of that. `SQRT` (COP1 funct 4) is now decoded and executed; it was the last
implemented-but-unreachable operation.

`sqrt`'s sticky bit is exact rather than estimated: `u128::isqrt` returns the floor of the root,
and the root is exact precisely when `q * q == n`, so that comparison **is** the sticky bit.

**n64-systemtest: 584 → 508.** `SQRT.S`/`SQRT.D` reached zero; `CVT.S.fmt` fell 21 → 10.

### Added — the unmaskable unimplemented-operation cause for subnormals

The VR4300 has no subnormal datapath: rather than producing a subnormal it raises `FCSR.Cause.E`
(bit 17) and traps. Now modelled for a subnormal operand, a subnormal result with `FCSR.FS` clear,
a subnormal result with `FS` set but underflow/inexact enabled, and an MSB-clear NaN operand. With
`FS` set and those enables clear it flushes — to `±0` under nearest and toward-zero, but to the
smallest **normal** of that sign under a mode that rounds away from zero.

Out-of-range float-to-integer conversions now raise unimplemented rather than the IEEE Invalid.

**n64-systemtest: 1,098 → 584.** `ADD.S`, `SUB.S`, `ADD.D`, `DIV.D`, `ABS.*` and `NEG.*` are at
zero failures; the `CVT.W`/`CVT.L` families fell off the list. Accuracy ledger **C-13**.

### Fixed — `ABS`/`NEG` are not sign flips, and are not `MOV`

They classify their operand — Invalid on a signalling NaN, unimplemented on a subnormal or
MSB-clear NaN — and they **replace** `FCSR.Cause`, whereas `MOV` transports the bits and leaves
`FCSR` untouched. Treating all three alike was worth 52 assertions. The earlier finding that `MOV`
must not touch `Cause` stands; it just does not generalise to its neighbours.

### Added — the COP1 compares and conversions, and a corrected NaN convention

`C.cond.fmt`, the `CVT` family, and `ROUND`/`TRUNC`/`CEIL`/`FLOOR` to `.W`/`.L` now decode and
execute. They were implemented in `fpu.rs` all along but unreachable: decode admitted only
`funct 0..=3` and `5..=7` in the `.S`/`.D` formats, and never admitted the **integer** source
formats `.W`/`.L` at all — so every integer-to-float conversion was a silent no-op too.

`ROUND`/`TRUNC`/`CEIL`/`FLOOR` take their rounding mode from the **opcode** and ignore `FCSR.RM`;
`CVT.W`/`CVT.L` consult it. That is the entire difference between the two families, and getting it
wrong would be invisible whenever `RM` happened to match.

**n64-systemtest: 2,682 → 1,468.**

### Fixed — the VR4300 NaN convention is inverted from IEEE-754:2008

A NaN is **signalling** when its significand's MSB is **set** — the legacy MIPS convention, the
opposite of IEEE-754:2008. `0x7FC0_0000`, which Rust produces as `f32::NAN` and everything else
calls quiet, raises Invalid on this processor.

Established from the oracle's own expectations, which name their constants the IEEE way and then
assert the opposite behaviour; corroborated independently by the fact that the VR4300's default
NaN *result* is `0x7FBF_FFFF` (MSB clear), which under IEEE would be a signalling NaN that
re-traps on first use, and under this convention is an ordinary quiet one.

**n64-systemtest: 1,468 → 1,098**, taking the compare block from 42 failures apiece to **zero
across all sixteen tests**. Accuracy ledger **C-12**.

### Added — soft-float arithmetic with exact IEEE flags and all four rounding modes

New `crates/rustyn64-cpu/src/softfloat.rs`. Both formats and all four arithmetic operations are
computed from unpacked `(sign, significand, exponent)` triples in `u128` and rounded **once** at
the end; discarded bits are folded into a sticky bit rather than dropped, which is what makes
`inexact` exact rather than approximate. `FCSR.RM` falls out of the same step, so the directed
rounding modes now apply to arithmetic as well as conversions.

**n64-systemtest: 2,794 → 2,682.**

Verified against an independent oracle — Rust's own `f32`/`f64` operators — with the requirement
that in round-to-nearest the result is *bit-identical* across three corpora: 40,000 random bit
patterns, 40,000 draws from the ordinary numeric range, and 20,000 around the subnormal boundary.
The flags come from the same rounding step as the value, so a bit-exact value is real evidence
about the guard/sticky bookkeeping the flags are read from; testing the flags alone would have
been self-referential. Rounding-mode results are pinned separately against vectors transcribed
from n64-systemtest.

The `f64`-comparison shortcut was deliberately **not** used: the exact sum of two `f32`s can span
~277 significand bits, so an `f64` sum is itself rounded and the comparison silently becomes a
guess in exactly the range the oracle probes.

Removed as superseded: `fpu::{round_f64_to_f32, next_up_f32, next_down_f32}` and the private
`classify_f32`/`classify_f64`. `round_f64_to_f32` in particular was a double-rounding trap once a
correct path existed.

### Fixed — `MOV`/`ABS`/`NEG` cleared `FCSR.Cause`, erasing the previous operation's result

Introduced by the `MOV.fmt` change below and found by the soft-float work: wiring exact flags in
moved the oracle by **zero**, with the suite reporting `flags: inexact` but `causes: ""` — the
sticky half surviving and the per-operation half gone, which is the signature of a later
instruction overwriting `Cause` rather than of a flag never raised.

Because the compiler emits `MOV.fmt` to move an FP return value, a `MOV` sits between almost every
arithmetic operation and the `CFC1` that reads its result, so it erased exactly the bits the
program was about to inspect. These three instructions cannot raise, so they now write nothing to
`FCSR`. Worth **112** assertions on its own, and pinned by a named regression test.

### Added — enabled floating-point traps (`Exception::FloatingPoint`)

A COP1 condition whose `FCSR.Enable` bit is set now raises an FP exception (`ExcCode` 15) instead
of merely recording the bits. Four things change on the trapping path, each separately observable
and each pinned by a mutation-tested unit test:

- `fd` is **not** written — the trap is precise, so the destination keeps its old value;
- the sticky `Flags` field is **not** accumulated, only `Cause` is written;
- the instruction does not retire, so it does not tick `Random`;
- `Cause.CE` is 0, not a coprocessor number.

**Measured effect: n64-systemtest 2,795 → 2,794.** Reported plainly because the number is small
and the reason matters: a trap needs a *raised* condition, and `fpu::classify_*` sets `inexact`
only as a side effect of overflow and never sets `underflow` at all. The trap path is not the
blocker — flag detection is. That is now accuracy-ledger **C-11**, together with why the obvious
`f64` round-trip fix is a fitted constant rather than a measurement: it is correct in the normal
range and wrong in exactly the range n64-systemtest probes.

### Fixed — `MOV.fmt` was a silent no-op, and it was not an FPU bug (T-12-007)

`MOV.fmt` is COP1 funct **6**. The decoder admitted only `funct <= 3` to the FP arithmetic path
and sent everything else to `Cop1Unimplemented`, which executes as a no-op. Compilers emit
`MOV.fmt` for every FP argument and every FP return value, so callees read stale operands and
callers read a register the callee never wrote. `ABS` (5) and `NEG` (7) share the arm and were
equally absent. All three now decode and execute; `SQRT` (4) remains unwired.

n64-systemtest failures: **2,897 → 2,795**.

This had been read as an FPU arithmetic fault for nine rounds — see
[`docs/accuracy-ledger.md`](docs/accuracy-ledger.md) C-10, which records each wrong hypothesis and
what refuted it. The arithmetic was correct the whole time: `ADD.S` wrote the right value to the
right register, and the suite then reported a *different* register, because the move that was
supposed to carry the result out of the callee did nothing. Every `Result after <op>` failure was
measured against a value the instruction under test never produced.

What finally located it was a **correlated capture** — arming on the suite's own
`Running COP1: ADD.S...` marker so the captured instruction is provably the failing test's, and
dumping the *instruction stream* rather than the registers. Nine earlier probes watched state and
inferred cause; two of their conclusions had to be retracted. The eight words either side of the
site named the bug immediately.

### Fixed — `Random` never advanced, so every `TLBWR` overwrote the same entry

`Cop0::tick_random` was implemented and **never called from the pipeline** — only from a unit test.
`Random` therefore sat at 31 forever, and since `TLBWR` writes the entry `Random` names, every
refill overwrote the same one. UM §5.4.2 is explicit: *"decrements as each instruction executes."*

A stuck counter is **invisible to any test that calls `tick_random` itself**, which is exactly what
the COP0 unit tests did — they exercised the decrement logic thoroughly while nothing checked that
anything ever called it. The new test drives it through `advance` instead.

Found by chasing an infinite TLB-refill loop in n64-systemtest. **It did not fix that loop** — the
suite still shows one distinct `EPC` and one `ExcCode` — so this is a genuine defect found *beside*
the bug being hunted rather than the bug itself, and it is reported that way.

### Diagnosed — why n64-systemtest reports nothing (T-12-007)

Not waiting; **lost**. Probing for the first divergence found it **191 retired instructions in**:
execution jumps from `0x800A15F4` into a zero-filled region and NOP-slides until it falls off the
top of RDRAM, which is why raising the instruction budget from 6M to 45M never helped.

The cause is in the suite's own `entrypoint()` — `memory_size` and `elf_header_offset` come from
**SP DMEM**, which is a stub returning 0. IPL3 writes the detected RDRAM size there at boot; we do
not, so the suite builds its memory map from zeros and jumps into nothing.

SP DMEM is now readable (`Bus::spmem`) and seeded by `rom::seed_ipl3_handoff` with what IPL3 would
have written. That was **necessary but not sufficient**: the suite runs far longer and still
diverges at the same instruction, because of a second, larger problem underneath.

**n64-systemtest is an ELF and the harness does a flat copy.** Its `PT_LOAD` segments target
`0x8000_0000` onward, while `load_direct` places `ROM[0x1000 + k]` at `entry + k`. Every address in
the image is therefore wrong, which is why the first jump — to a perfectly valid `0x8018368C` —
lands in zeros. The ROM-header entry point is *not* the problem; the load mapping is.

`rom::load_elf` now does that: it parses the ELF at the magic-located offset and loads each
`PT_LOAD` to its `vaddr`, zeroing `memsz - filesz` for BSS. `0x1AD150` bytes load correctly and
execution no longer jumps into zeros.

**The instruction-6 fault was the stack pointer.** Disassembling the entry showed a standard
prologue — `ADDIU $29, $29, -0x50` then `SW $31, 0x4c($29)` — and with `$29` at zero that store
targets `0xFFFF_FFFF_FFFF_FFFC`, a KSEG3 address, TLB-mapped, refill. IPL3 leaves `$sp` at the top
of SP DMEM; the direct-load path set no registers at all. Loading the image correctly is not enough
if the register state it was compiled against is missing.

`seed_ipl3_handoff` now sets `$sp`, which moved the fault to instruction ~25: `ExcCode = 11`
(Coprocessor Unusable) — a COP1 instruction with `CU1` clear. IPL3 leaves `Status` at
`0x3400_0000` (`CU1 | CU0 | FR`), the value the COP0 work had already cross-checked against a real
boot capture. Seeding it also clears `ERL` and `BEV`.

**The suite now runs 108,000,000 instructions with zero exceptions** — and still prints nothing.
That is a materially different failure from every previous round: it is no longer lost, faulting,
or NOP-sledding.

A PC histogram showed `0x8000_0180` — the general exception vector — hottest by a wide margin, and
I read that as the suite executing its tests, since n64-systemtest raises exceptions by the
thousand on purpose. **That reading was wrong.** A second probe found exactly **one distinct
`EPC`** (`0x8018_32E8`, 2,000,000 hits) and exactly **one `ExcCode` (2 = `TLBL`)`.

**It is an infinite exception loop**: a single load faults, the handler returns, and it faults
again. A hot exception vector looks identical to a busy test suite from a histogram alone — what
distinguishes them is whether `EPC` *moves*, and it never does. Worth recording, because "which
instruction" is the question this whole diagnosis was built on and I stopped one step short of
asking it.

The remaining problem is therefore a **TLB refill that never resolves**, and disassembling the
faulting address identified it precisely. `0x8018_32E8` holds `0x42800060` — COP0 CO-class, funct
`0x20`, which is n64-systemtest's **emux probe opcode**. The suite executes it deliberately to ask
"am I running under emux?", expecting a Reserved Instruction exception anywhere else.

Our decoder correctly leaves it `Reserved`, so the RI fires and `EPC` is set. But the reported
`ExcCode` is **2 (`TLBL`)**, not 10 (`RI`) — meaning a **second fault happens inside the handler**,
with `EPC` surviving exactly as the `EXL` gate requires. That also explains why `0x180` (general,
`EXL=1`) was hotter than `0x000` (refill, `EXL=0`).

### Fixed — n64-systemtest now boots and runs its full corpus

Four defects, each surfaced by the oracle and each previously unreachable.

**1. COP0 CO `funct` 0x20-0x3F decoded to `Reserved` instead of retiring inertly.**
n64-systemtest probes for the `emux` emulator by executing `COP0 CO funct 0x20` from
`init_allocator`, inside `entrypoint` — **before** `main` installs any exception handler. We raised
Reserved Instruction; the RI dispatched to an uninstalled `0x8000_0180`, where zeros decode as
`SLL $0, $0, 0` (`NOP`), so the CPU NOP-slid from `0x180` into `.text` at `0x8000_0400` and faulted
there on a load through a zero base. `EXL` was set by then, so `EPC` retained the *first*
exception, which is why the reported `ExcCode` (`TLBL`) and `EPC` (the RI site) disagreed and made
this read as a TLB bug for three rounds.

If a real VR4300 raised RI there, the suite would derail on every N64 it has ever run on, before
printing a line. The range is not a guess either: the suite's probe constant is named
`XDETECT_CODE_EXTENSIONS_20_3F`. Added `Op::Cop0Extension`, executed as a no-op that notably does
**not** write the target GPR. Ledger **C-8** — an inference, not a manual citation.

**2. `IP7` latched at power-on, because the timer tested equality rather than an edge.**
`Count` and `Compare` both reset to zero, so `Count == Compare` held on the very first step and
`IP7` latched before a single instruction retired. The timer fires when `Count` *becomes* equal to
`Compare` — once per wrap — so the poll is now an edge (`Cop0::timer_edge`). The suite pinned this
precisely: `Cause` during an AdEL read `0x8010` instead of `0x10`.

**3. `Context`/`XContext` were gated on TLB exceptions.** Hardware fills `BadVPN2` from the same
latch that feeds `BadVAddr`, so an **address error** updates them too, with no TLB lookup involved.
The suite expects `Context = 0x0052_0000` for `BadVAddr = 0xA400_1A42`, exactly
`(BadVAddr >> 13) << 4`. `EntryHi` stays TLB-gated — it is the TLB's own match register, and
writing it on an address error would corrupt the entry a later `TLBWR` installs.

**4. `SP_STATUS` was unmodelled and read as zero**, claiming a running RSP. The RSP comes out of
reset **halted**; only that power-on `halt` bit is modelled, which is honest about the RSP still
being an LLE-shaped stub while no longer asserting something false.

With these, `StartupTest` and the whole unaligned-access group pass, and the suite proceeds through
its corpus (~863 KB of output) instead of hanging on its first print. The remaining failures are
dominated by one unimplemented area — the **cart address space is not mapped**, so every
`cart:`/`cart_memory:` read returns zero — plus 32-bit address sign-extension checks. Those are
next; `Failed: 0` is not yet met, so v0.2.0 stays uncut.

**6. COP2 encodings decoded to `Reserved`.** The VR4300 has a COP2 unit, so `MFC2`/`MTC2`/`DMFC2`/
`DMTC2` are architecturally *valid*: with `Status.CU2` clear they raise **Coprocessor Unusable**
(`ExcCode 11`), not Reserved Instruction (`10`) — the same distinction already drawn for COP1.
Getting it wrong produced n64-systemtest's "Exception storm detected. Aborting.": the suite saw
five unexpected exceptions in a row, tripped its recovery limit, and **truncated the entire run**.
Fixing it removed the abort, so the suite now reaches its later groups — which is why the reported
failure count *rose* (2,551 to 2,909) while the emulator strictly improved. The earlier count was
a floor, exactly as suspected.

**7. PI address registers never advanced after a transfer.** Hardware walks `PI_DRAM_ADDR` and
`PI_CART_ADDR` as the DMA proceeds, so software reads back the address *past* the block it just
moved — which is how a driver chains transfers without rewriting the address each time. Leaving
them put makes every chained DMA re-send the first block. The oracle checks the delta directly:
after a `0x10`-byte transfer it expects `PI_CART_ADDR` to have moved by `0x10`, and for a written
length of 1 (a two-byte transfer) by `0x2`. Failure count 2,909 to 2,907.

**8. SP DMA was entirely unimplemented.** `SP_RD_LEN`/`SP_WR_LEN` now move data between RDRAM and
SPMEM, honouring the packed length word — bits 11:0 bytes-per-row minus one, 19:12 rows minus one,
31:20 the RDRAM-side inter-row skip. Treating the word as a plain byte count transfers megabytes
for a routine 8-byte copy; reading only bits 11:0 silently drops every row after the first, which
is the failure mode for anything doing a 2D block copy. `SP_MEM_ADDR` bit 12 selects IMEM, and the
12-bit offset wraps **within** the selected 4 KiB half rather than spilling into the other.

**9. The seeded `$sp` was in KSEG1, and had been over-claimed.** It pointed at the top of SP DMEM
(`0xA400_1FF0`), described in a comment as what IPL3 leaves behind — but the only evidence for that
was that *some* valid stack had stopped a crash, which any address would have done. The oracle
narrowed it: the SP-DMA tests build their source data in a **stack array** and call
`MemoryMap::uncached_mut` on it, which asserts the address is KSEG0. A KSEG1 stack fails that
assert and panics the suite outright. The stack is now KSEG0, above `MemoryMap::HEAP_END` so it
cannot collide with the heap; the exact address remains a documented inference.

With these the suite runs past the SP-DMA group entirely and reaches the TLB tests.

**10. `Cause.CE` was left stale across exceptions.** `CE` (bits 29:28) names the coprocessor for a
Coprocessor Unusable exception. It was written *only* for that exception, on the reasoning that it
is meaningless otherwise and that clearing it would erase a value the handler had not read yet.
Plausible, and wrong: it leaves `CE` stale, so any exception following a Coprocessor Unusable
reports the old unit number. `TLB: Read after 4k page, expect TLBL` read `Cause = 0x2000_0008`
where hardware gives `0x8` — a `CE = 2` left behind by the COP2 usability check added in this same
batch. `CE` is now written on every exception: the unit for CpU, zero otherwise. Failure count
2,932 to **2,901**, and the run reaches deeper into the TLB group.

**11. PI external-bus sub-word reads ignored the 16-bit-bus off-by-two.** The PI bus is 16 bits
wide and the RCP ignores access size, so every VR4300 read becomes two 16-bit bus reads — the MSB
at the CPU's address with bit 0 ignored, then the LSB at `address + 2`. The RCP therefore returns
the word starting at `addr & !1` while the CPU selects its byte lane assuming a word at
`addr & !3`, and that two-byte disagreement is a **hardware bug we must reproduce**: a 16-bit read
at `0x1000_0002` returns the halfword at `0x1000_0004`. Byte reads take the same shift; word reads
do not, since a word access puts its own address on the bus. `Bus::read_u32` is now overridden to
read the PI window raw, because the four-`read_u8` default would apply the shift per byte and
mangle bytes 2 and 3 of every word.

Two things this nearly got wrong, both caught by checking rather than reasoning. The mechanism is
**not** an advancing address latch, which is what the two failing data points suggested — the
N64brew *Memory map* page states the real cause. And it needs no `CpuBus::read_u16`: a halfword
load is already issued as two byte reads, and both land correctly under the byte rule. ISViewer
lives *inside* the PI range and must keep claiming its window first, or the debug channel's
read-back handshake breaks — which the existing round-trip test caught immediately.

This is the fourth time in this project that a comment asserting a rule has turned out to assert
more than the evidence supported — alongside the `$sp` placement, the `DIV` numerator condition,
and the `CACHE` translation split. The pattern is specific enough to be worth naming: prose that
explains *why not* to do something is as testable as code, and goes unchecked far longer.

Wrong turns worth recording, since each cost a round and each would have taken real work to
"fix": `$gp` unset (`_gp` is `ABS 0x0`, so `$gp = 0` is correct); lost installer stores (the
installer was never *reached*); and an early panic (the print was `init_allocator`'s legitimate
`println!`). Every round that guessed at a mechanism was wrong; every round that asked what was
actually in memory, or actually executed, at the faulting moment was right.

### Added — a merge-conflict-marker guard (CI + pre-commit)

A `|||||||` diff3 marker was committed into `CHANGELOG.md` during a rebase: the resolution handled
`<<<<<<<`, `=======` and `>>>>>>>` but not the diff3 middle section. **It would have shipped into
the release notes** — a stray marker line is valid Markdown, and valid inside a comment, so
nothing downstream complained.

`scripts/check_no_conflict_markers.sh` now runs in CI and pre-commit, matching the three-layer
shape the commercial-ROM guard already uses: the local hook can be skipped with `--no-verify`, the
CI job cannot.

### Added — FPU arithmetic (T-13-002)

`ADD`/`SUB`/`MUL`/`DIV` in single and double precision, plus `ABS`/`NEG`, as pure functions that
return a value **and the `FCSR` flags they raised** — rather than mutating `FCSR`, which belongs to
`Cop1Control`. An arithmetic helper that reached into it would have to own it.

Three distinctions that are easy to collapse, each pinned:

- **Signalling vs quiet NaN.** Only a signalling NaN raises Invalid. IEEE puts the quiet bit at the
  top of the mantissa, so `is_nan()` cannot tell them apart, and treating every NaN as signalling
  raises Invalid on ordinary quiet-NaN propagation. The bit sits at a different position for `f64`,
  so the double case is not a free consequence of the single one.
- **`x/0` vs `0/0`.** `DivByZero` and Invalid are different flags and a handler distinguishes them;
  `0/0` is an undefined form, not a division fault. `x/0` is also *not* an overflow, despite the
  infinite result.
- **A NaN from non-NaN inputs** — `inf - inf`, `0 * inf` — is Invalid even though neither operand
  was a NaN.

Flags populate **both** the `Cause` and sticky `Flags` fields, since hardware sets them together;
writing only one leaves software unable to distinguish "raised now" from "raised at some point".

**`C.cond.fmt` derives all sixteen conditions from the bit encoding** rather than enumerating
mnemonics. The 4-bit field is systematic (UM Table 7-11): bit 3 raises Invalid when unordered, and
bits 2/1/0 select less / equal / unordered. So `C.EQ` is 2, `C.OLT` is 4, `C.OLE` is 6, and every
signalling variant is its ordinary form plus 8. Sixteen hand-written cases invites getting one
wrong; deriving them makes all sixteen correct or none.

Two things that fall out of the encoding and are worth stating: **`Greater` matches no bit** —
`fs > ft` is "none of less, equal or unordered", which is why three condition bits suffice — and
**the signalling forms raise Invalid on a merely quiet NaN**, which is the entire difference
between `C.EQ` and `C.SEQ` and means the quiet/signalling test used elsewhere is not sufficient
here on its own.

**Conversions** (`CVT`, and the shared rounding the `ROUND`/`TRUNC`/`CEIL`/`FLOOR` forms use)
carry one VR4300-specific rule worth calling out. UM §7.5.2:

> *"When converting a long integer to a single- or double-precision floating-point number
> (`CVT.[S,D].L`), bits 63:55 of the 64-bit integer must be all zeroes or ones, otherwise the
> VR4300 processor raises a floating-point instruction exception."*

That is a **hardware limitation, not IEEE behaviour** — the value is representable and the
processor simply declines. Converting it anyway produces a *correct* number where hardware traps,
so software's fixup path never runs and the divergence surfaces far downstream from its cause. It
raises `Unimplemented` (`FCSR` bit 17), which is deliberately **not** Invalid: "this processor
cannot do this" is a different thing from "the operation is undefined", and conflating them sends
the handler down the numerical-error path.

Two implementation notes that bit during development:

- Rust's float→int cast **saturates**, so `i32::MAX as f32 as i32` is `i32::MAX` again and a
  round-trip inexactness check silently never fires for exactly the value most likely to be
  inexact. The checks go through `f64` instead.
- `Nearest` is **ties-to-even**, not the round-half-away-from-zero that most `round` functions
  implement and that no MIPS mode selects. This crate is `no_std`, so `trunc`/`floor`/`ceil`/
  `round_ties_even` are implemented here rather than pulling in `libm` for four functions.

**The FP multiplication erratum is modelled, deliberately not reproduced.** `fpu::Stepping` says
which console this is, and `mul_erratum_triggers` says when the erratum would fire — but selecting
the affected stepping changes **no arithmetic**, because *what wrong value the erratum produces
has never been characterised*. The trigger is documented, the affected steppings are documented,
the output is not; it sits in the timing supplement's undocumented-constants list.

Inventing a plausible wrong value would be exactly the fitted-constant failure the ledger's
preamble forbids — every later result built on it would stop being evidence. Recorded as ledger
**U-7**. The switch exists now so that when the output *is* characterised it goes in one place
rather than being threaded through afterwards.

Four of five mutations fail the suite. The fifth — `ABS` written as `f32::abs` rather than an
explicit sign-bit clear — is **equivalent**, including for NaN payloads, and is documented as a
readability choice rather than implied to be load-bearing.

### Added — the `ISViewer` result channel (T-12-007, partial)

n64-systemtest reports results through `ISViewer`, a flashcart/emulator convention at
`0x13FF_0000` in cart space rather than real N64 hardware. The suite **probes for it** — writing
`0x12345678` to the buffer and reading it back — and falls back to a framebuffer console we cannot
read if the round-trip fails. So this window is what turns *"the suite runs"* into *"the suite
reports"*.

Text is captured on the **length** write, not on the buffer writes, so a whole line is published
at once; capturing per buffer write would interleave partial lines. An oversized length is clamped
rather than panicking — the value comes from guest code.

**Measured progress on the suite** (each figure is instructions retired before it stops
progressing):

| After | Retired | Outcome |
| --- | --- | --- |
| — | 2 | `ExcCode = TLBS`, `BadVAddr = 0` |
| the `ERL` fix | 30,679 | ran past the 1 MiB IPL3 window |
| PI DMA | 3,000,000+ | **no exceptions**; executing its own ELF from high RDRAM |
| `ISViewer` | 6,000,000+ | no exceptions, **no output yet** |

The suite therefore still does not report a count: it runs cleanly but does not reach its
reporting stage, which points at further unimplemented hardware (VI/RSP initialisation) rather
than at the channel. Recorded as a measurement rather than a claim of progress toward `Failed: 0`.

### Added — the PI DMA engine (T-14-001), pulled forward from Phase 5

n64-systemtest loads the rest of its own ELF from cart through PI, so the Phase 1 exit criterion —
and with it the **v0.2.0 cut criterion** — was unreachable without it. Pulling PI forward keeps
that criterion an honest oracle result instead of weakening it or letting the harness diverge from
hardware by staging the whole ROM itself.

`PI_DRAM_ADDR` / `PI_CART_ADDR` / `PI_RD_LEN` / `PI_WR_LEN` / `PI_STATUS`, with the two rules that
bite both pinned:

- **Length is `len + 1` bytes.** Writing 0 transfers **one** byte. An implementation short by one
  corrupts the *last* byte of every block — which presents as memory corruption rather than as a
  DMA bug, and so gets debugged in the wrong place.
- **`RD`/`WR` are named from the cartridge's point of view**, so `PI_WR_LEN` — the one everything
  actually uses — moves data **cart → RDRAM**. Reversed, the first ROM load writes uninitialised
  RDRAM over the ROM image.

**Review found four defects in the first version, two of them serious**, and all four are now
pinned by mutation-checked tests:

- **A guest `sw` to a length register started four DMAs.** The default `write_u32` composes four
  `write_u8` calls, and PI registers were handled byte-wise — so every PI transfer was wrong, with
  four partly-assembled lengths, and the symptom was memory corruption rather than anything that
  looked like DMA. `write_u32` is now overridden so a PI write is one word write.
- **Clearing the PI interrupt never lowered the MI line.** Only completion updated it, and a
  `PI_STATUS` clear starts no transfer — so `IP2` stayed high forever and any interrupt-driven
  loader would hang. The MI line now mirrors the PI's state on *every* write.
- **Byte writes to the trigger and status registers are dropped, not assembled.** `PI_STATUS`'s
  read bits (busy, interrupt) do not correspond to its write bits (reset, clear-interrupt), so
  reading it back to fill in the other three bytes fabricates command strobes out of status flags.
  Only the address registers — which merely latch — can be safely assembled.
- **`PI_DRAM_ADDR` is doubleword-aligned**, not halfword. Masking only bit 0 lets a transfer start
  mid-doubleword and silently shifts every byte of it.

The engine **returns a description of the transfer rather than performing it**, and the Bus
carries it out. The PI does not own RDRAM — having it reach back into its owner is precisely the
cycle this architecture exists to avoid. It also means charging the transfer real time later is a
scheduling change rather than a rewrite, the same reasoning as `SysAD` being a state machine.

Completion raises the PI line into the MI, which the CPU sees as `IP2`.

### Added — the floating-point register file, moves and FP loads/stores (T-13-001)

32 physical 64-bit **FGRs**, with the `Status.FR` view applied on access rather than assumed:

The view applies to **64-bit accesses only**:

| `FR` | 64-bit (double / `DMxC1`) view |
| --- | --- |
| 1 | FPR *n* **is** FGR *n* — 32 independent 64-bit registers |
| 0 | 64-bit values use **even** indices; the value is the FGR **pair** `FGR[n+1]:FGR[n]` |

`FR = 0` does not make half the register file disappear — **single precision is unaffected**, with
all 32 indices valid under both settings. It changes how a *double* is laid out across the file.

Storing 32 `u64`s and indexing directly is right for `FR = 1` and silently wrong for `FR = 0`,
where a double written to FPR 2 must land in FGRs 2 **and** 3. Both modes occur on real N64
software — IPL3 leaves `FR` set, but plenty of games clear it. And because single precision is
identical under both, the bug survives casual testing: every `.S` operation works and only doubles
break. A single-precision write therefore
**preserves** the upper half of its FGR, since with `FR = 0` that half is the other word of a
double the program never touched.

Plus `MFC1`/`DMFC1`/`MTC1`/`DMTC1` — which **apply the `FR` view rather than moving the physical
register**, per UM Ch. 17's pseudocode: with `FR = 0` and an even `fs`, `data <- FGR[fs+1] ||
FGR[fs]`, the pair, exactly like `LDC1`. Only an *odd* `fs` with `FR = 0` is undefined, and it is
undefined rather than a Reserved Instruction exception. A raw-move implementation round-trips
through `DMTC1`/`DMFC1` correctly and disagrees with `SDC1`, so the test asserts across both
paths — and
`LWC1`/`LDC1`/`SWC1`/`SDC1`, which obey the same alignment, translation and `CU1` rules as the
integer forms. The move-to and FP load/store forms write no general register — `rt` names an FPR,
so giving them a GPR destination would corrupt an integer register.

FP *arithmetic* is still not implemented; this is the file it will operate on.

### Fixed — `Status.ERL = 1` makes KUSEG unmapped (found by running n64-systemtest)

> *"If the ERL bit of the Status register is 1, the user address area is a 2 GB area that cannot
> be cached without TLB mapping (i.e., the virtual addresses are used as physical addresses as
> is)."* — UM §5.2.2, p. 129

**Cold reset sets `ERL`**, so this is the state every boot ROM starts in. Without the rule, the
first store to a low address takes a TLB refill before any mapping could possibly exist.

Found by *running the oracle*, not by reading: n64-systemtest died **two instructions in** with
`ExcCode = TLBS`, `BadVAddr = 0`. With the rule implemented it retires **30,679** instructions
with no exception. No amount of re-reading the TLB code would have surfaced this — the TLB was
behaving exactly as written; the missing piece was upstream of it.

The rule covers the **user area only**: mapped kernel segments stay mapped, so a blanket
"`ERL` ⇒ direct" would silently unmap KSSEG and KSEG3 too. Both directions are pinned and both
mutations fail the suite.

### Changed — README and `.gitignore` brought up to date

`README.md` described the superseded fractional scheduler, claimed Phase 1 had not started, and
carried a "no chip emulates anything" banner that stopped being true several tickets ago. It now
describes the canonical 187.5 MHz clock, the five-stage pipeline, the integer set, COP0, the TLB
and the exception model — and is equally explicit that **everything else is still a stub** and
that a green test run does not mean a subsystem works. The accuracy table reports the one gate
that produces a real number (`basic.z64`, 5/5) instead of asserting that none do, and points at
the accuracy ledger. Release status now names the v0.1.0 tag and the v0.2.0 cut criterion rather
than saying no release has been cut.

Kept deliberately out of the README: per-release detail. That belongs here.

`.gitignore` gained `*.preedit` / `*.pre` (snapshots taken before instrumenting a file, so cleanup
restores a known copy rather than discarding unrelated uncommitted work) and `um.txt` (the
extracted VR4300 manual text — a large derived artifact whose extraction is *not* reproducible by
the obvious command, since `pdftotext` scrambles that PDF and only `mutool` works, which is
precisely why someone would be tempted to commit it).

### Added — `CACHE` executes instead of raising (T-12-005, partial)

`CACHE` decoded to `Reserved` and therefore **raised**. IPL3 and libdragon both issue it, so that
blocked every real ROM — the last hard blocker of its kind in Sprint 2.

It now decodes, resolves its effective address — so it can still raise a TLB fault, like any other
memory instruction — and performs no data transfer. Its `rt` slot is the **operation selector**,
not a destination; decoding it as a load clobbers whichever GPR the cache-op encoding names, so
the register destroyed would depend on which operation was requested.

**Only the address-addressed operations translate.** `op4..2` 0–2 are `Index_*`, defined *"at the
index specified"*, so they never consult the TLB; 3 and 4–6 are defined in terms of *"the specified
address"* and do. Translating unconditionally raised spurious TLB refills on exactly the code that
matters — cache-init walks every index with an arbitrary base, before any mapping exists to satisfy
it. Caught in review, where the first version's *comment* described the distinction while the code
ignored it and the test asserted the wrong side of it.

**The cache depth question is answered explicitly, and the answer is zero.** Phase 1 listed
"how exact must the cache model be" as an open question. Cache *contents* are not modelled at all,
so invalidate and write-back have nothing to act on — which is observationally sound **only**
because no cache state exists to become stale. Recorded as ledger **D-5**, together with the point
it stops being sound: Phase 5, when cart/RSP DMA writes land in RDRAM behind a cache games
explicitly flush.

Cache-miss costs are **deferred with the model** rather than applied to a cache that does not
exist, and **`M` remains unmeasured with no value** (ledger C-1). That criterion is met by *not*
producing a number: a fitted `M` would make every later timing result unfalsifiable.

### Added — COP1 control registers and coprocessor usability (T-12-006)

**Control registers only** — `CTC1`/`CFC1` on `FCR31` and `FCR0`. FPU arithmetic is Sprint 3, and
the module says so in its own docs so the ticket cannot grow into Sprint 3 by stealth.

It exists for one reason: n64-systemtest calls `set_fcsr(...)` — `ctc1::<31>` — as the **fourth
statement** of `entrypoint()`, so without it the suite dies three statements in and every COP0 and
TLB test in Sprint 2 is unreachable behind it.

**Coprocessor Unusable** is checked in `EX` with `Cause.CE` naming the unit. Two rules pinned in
both directions: COP0 is usable from kernel mode regardless of `CU0` — otherwise a handler could
not run before `Status` was set up — but that exemption is *not* a blanket bypass, and user mode
without `CU0` still raises.

A valid-but-unimplemented COP1 encoding decodes to `Cop1Unimplemented`, **not** `Reserved`, and
does not raise when `CU1` is set. That makes Sprint 3's arithmetic an addition rather than a
behaviour change; raising here would look correct until the FPU landed.

All seven mutations were confirmed to fail the suite.

### Added — the TLB and the instruction micro-TLB (T-12-004)

32 fully-associative joint-TLB entries mapping even/odd page pairs, plus the **two-entry
instruction micro-TLB** in front of it. A micro-TLB miss is a **stall** (3 PCycles); a JTLB miss
is an **exception**. Modelling only the JTLB would not approximate that cost — it deletes the
structure the cost occurs in.

`KUSEG`, `KSSEG` and `KSEG3` are now genuinely TLB-mapped. Previously they were masked to their
low 29 bits, which silently aliased every access onto real memory instead of faulting.

**The rules that pass ordinary tests while being wrong**, each pinned:

- **`V` does not participate in matching** (UM §5.4.9). An invalid entry still *matches*, so it
  raises TLB Invalid — which shares an `ExcCode` with a refill and differs **only in vector**.
  Checking `V` while matching sends a protection fault to the refill handler, and also stops TLB
  shutdown firing on duplicates involving an invalid entry, which UM Fig. 6-6 requires.
- **`G` is the AND of both halves**, not the OR. An OR makes far too many entries global, and a
  global entry matches every `ASID` — so it presents as address-space leakage rather than a
  missing translation.
- **`PFN` is always in 4 KiB units** whatever the page size, so a large page's frame number has
  low bits masked off rather than scaled.
- **Only `C == 2` is uncached**; the VR4400's other coherency encodings collapse on a part with
  no coherency protocol.
- **`TLBWI` can overwrite a wired entry; `TLBWR` cannot** — and the protection is structural
  (`Random` never goes below `Wired`), not a check. Guarding both makes wired entries unwritable.

**Ledger D-4 added: TLB entries reset to distinct tags, not zero.** All-zero is not a usable state
— 32 entries at `VPN2 = 0`, with `V` out of the matching rule, means the first access to virtual
page-pair 0 matches all 32 and triggers TLB shutdown. Found by a test, not by reasoning.

`addr::translate` was **deleted** rather than kept "for unmapped-only paths" once nothing called
it: an unused function that quietly gets translation wrong is the inert-API hazard
`docs/engineering-lessons.md` §3.2 describes.

**Review caught a bug the entire test suite was structurally blind to.** TLB matching compared the
raw 64-bit address, so a sign-extended kernel address — KSEG3 is `0xFFFF_FFFF_E000_0000`, not
`0xE000_0000` — pitted its sign extension against a `VPN2` holding only VA(39:13). **No mapped
kernel address could ever match**, however correct the entry. Every TLB test used KUSEG, which has
no sign extension, so nothing could see it. Matching now compares the architectural `R` and
`VPN2` fields, and there are regressions for both translating and probing a KSEG3 address.

Three more from the same review: `EntryHi.R` was left zero on a fault (which makes the handler
install an entry that can never match, so an infinite refill loop rather than a wrong value);
`TLBP` masked the region away before probing; and **TLB shutdown set a flag nobody could
observe** — the ticket's own criterion said `Status.TS ← 1` and *TLB unusable*, and neither half
was propagated.

All ten original TLB mutations plus six of the seven fix mutations fail the suite. One initially survived — nothing
checked that TLB Invalid takes the *general* vector — and there is now a test for the one
distinction `Cause` cannot express.

### Added — interrupts, `Count`/`Compare`, and the MI line (T-12-003)

Assertion and recognition are kept **separate**, because they are different things: `Cause.IP`
tracks what hardware asserts regardless of masks — software polls it — while recognition applies
`IE`, `EXL`, `ERL` and `IM`. Folding them together makes a masked interrupt invisible to
`MFC0 Cause`.

Dropping the `EXL`/`ERL` terms is the classic version of this bug: it works until an interrupt
arrives inside a handler, and then re-enters it forever. All four terms are pinned individually.

**`Count` is derived, never incremented.** ADR 0006 permits exactly one incremented counter and it
is `master_ticks`; the scheduler supplies the `Count` timeline (half PClock) and COP0 adds an
epoch that `MTC0 Count` re-bases. So the register is guest-writable *without* being incremented,
and cannot drift from the master clock. `Cpu::tick_at` is the seam. The scheduler's `count_ticks`
doc comment predicted this exact split and is now correct rather than aspirational.

**`IP7` is latched**, not a level: `Count == Compare` sets it and only a `Compare` write clears it
(UM §6.4.18, p. 200). The first version of this modelled it as a level tied to the equality, which
looked tidier and was wrong — the existence of a *documented* clearing mechanism is itself the
evidence that it latches, since a level would self-clear and need none. Worse, a level silently
**drops** any timer interrupt raised while `EXL` is set, because the equality holds for one tick
and the handler never sees it. Caught in review; now pinned by a test that fails against the level
implementation.

**Ledger U-4 closed:** the MI drives **`IP2`**. Neither the CPU manual (board-level) nor the
N64brew mirror states it; libdragon does — `C0_INTERRUPT_RCP = C0_INTERRUPT_2` — and is public
domain, so it is citable rather than merely observed. It also gives `IP3` = CART, `IP4` = PRENMI,
`IP7` = timer.

All seven mutations of the interrupt path were confirmed to fail the suite.

### Added — exception dispatch and `ERET` (T-12-002)

Exceptions were previously stamped into pipeline latches and never *dispatched* — nothing wrote
`EPC`/`Cause` or redirected the PC. Now the full epilogue runs (UM Fig. 6-14, p. 201) and the
pipeline stalls the documented 2 PCycles.

**The `EXL` gate is the part worth calling out.** `EPC` and `Cause.BD` are written **only when
`EXL` was already 0**; with it set, both are left untouched. That is the entire purpose of `EXL`
(UM §6.3.7, p. 174), and an implementation that always writes `EPC` passes every single-exception
test while corrupting every nested one — where nesting is the *normal* path for TLB refill
handlers, not an edge case. Pinned both directly and through the pipeline.

`AddressError` now carries its direction, because `AdEL` (4) and `AdES` (5) are different
`ExcCode` values and a handler distinguishes them. An instruction fetch is a load.

The vector table implements ledger **S-3**: a refill arriving with `EXL` set takes the **general**
vector, not `0x080`. UM Fig. 6-15 says `0x080` and is contradicted by two tables, by §6.4.8 twice,
and by Fig. 6-14. A second manual typo is pinned too — p. 181's prose gives the `BEV=1` general
vector as `0x8000_0180` where Table 6-4 makes it `0xBFC0_0380`.

`ERET` **completes the Sprint 1 `LL`/`SC` contract**: it is the only thing besides cache
invalidation that clears `LLbit` (UM §3.1), so until now `LL`; `ERET`; `SC` wrongly succeeded. It
also has no delay slot, alone among the control transfers, so the already-fetched next instruction
is squashed.

All ten mutations of the epilogue were confirmed to fail the suite, including removing the `EXL`
gate, restoring the `0x080` vector, `ERET` clearing `EXL` instead of `ERL`, and `ERET` gaining a
delay slot.

### Added — the COP0 register file (T-12-001)

Table-driven, because nearly every accuracy rule in COP0 is data rather than logic — "register N
is 64 bits wide", "bits 23:16 of `Config` are hardwired". Written as data each rule is asserted
directly against the manual; written as `match` arms they become 32 places to forget one.

- **Exactly eight 64-bit-wide registers**, pinned element by element: there is no generating rule,
  and a wrong entry is invisible until 64-bit software runs.
- **Per-register writable-bit masks.** A write to a masked-off bit preserves the old value rather
  than zeroing it — hardware discards the write, which is not the same thing. `Cause` is read-only
  except `IP1:IP0`; `Status.DS.TS` is read-only while the rest of `DS` is not.
- **`Config` hardwired fields are merged on read**, so no write path can erase them. Seeding them
  at construction would let a too-wide mask destroy them permanently.
- **Cross-checked against the real N64 IPL boot values**, which decompose exactly: `0x0006E463`
  reproduces both hardwired constants bit-for-bit with `BE=1`, `K0=3`. That is the cheapest
  evidence available that the field positions are right, and it comes from outside the manual.
- **`LLAddr` now has one storage location.** Sprint 1 put it on `Pipeline` because COP0 did not
  exist; it is COP0 register 17, and two copies of one architectural value would have let
  `MFC0 $rt, $17` disagree with the CPU's own link address.

A second mask table, `ARCH_MASK`, records which bits each register can ever *return*. It is
applied on read **and** on `set_hardware`, so a value that is not architecturally representable
cannot enter the file, let alone leave it. This closes a latent bug for T-12-002: exception
dispatch bypasses the write masks by design and will feed `set_hardware` raw faulting addresses,
which would otherwise have put non-zero bits in `EntryHi.Fill` — a field that architecturally
reads zero — and non-zero upper halves into 32-bit registers.

`MFC0`/`DMFC0` read in **DC** and `MTC0`/`DMTC0` write in **WB**, because UM §4.6.9 defines the
CP0 bypass interlock in terms of a write reaching WB while the next instruction reads in DC.
Doing both in EX would make that interlock unexpressible — the same mistake ADR 0007 exists to
prevent one level up.

Undocumented behaviour is marked as such rather than invented: reserved registers 7/21–25/31
(ledger U-1) and `PRId.Rev` (U-3) are explicit guesses with tests pinning the *choice*, not the
hardware. All nine mutations of the mask and width tables were confirmed to fail the suite.

### Added — `LL` / `SC` / `LLD` / `SCD`, closing a Sprint 1 gap (T-11-003)

The synchronisation pair was listed in T-11-003's acceptance criteria and in the Phase 1 exit
criteria, and had **not** been implemented — the four opcodes decoded to `Reserved`. Found while
verifying Sprint 1's criteria against the code rather than against the ticket's own checkboxes.

- `LL`/`LLD` arm the link bit and record `PA(31:4)` in `LLAddr` (COP0 reg 17, diagnostic-only per
  UM §5.4.7). `SC`/`SCD` test it, store only if set, and write 1/0 to `rt` **either way**.
- A misaligned `SC` **raises** rather than reporting failure — *"If this instruction both fails
  and causes an exception, the exception takes precedence"* (UM §16 p. 487). A misaligned `LL`
  leaves the link disarmed.

### Fixed — three "undocumented" timing constants were documented all along

A research pass for Sprint 2 re-opened the VR4300 manual and found that three constants recorded
across `docs/accuracy-ledger.md`, `docs/cpu.md` and the timing supplement as *undocumented* are
stated plainly in the sections those notes cited as lacking them:

| Constant | Value | Where it actually is |
| --- | --- | --- |
| Exception epilogue stall | **2 PCycles** | UM §4.7 p. 114 — the section's opening sentence |
| CP0I (CP0 bypass interlock) | **1 PCycle** | UM §4.6.9 p. 113 |
| ITM (instruction micro-TLB miss) | **3 PCycles** | UM §4.6.2 p. 107 — never looked for |

The ledger said *"no figure appears in UM §4.7 or chapter 6"* while the figure is §4.7's first
paragraph. The cause was search shape, not misreading: numbers were looked for in tables, and
these are in prose. CEN64's 2 is therefore corroboration rather than the origin.

Ledger entries C-2 and C-3 are reclassified from *not yet measured* to documented-with-citation,
C-7 is added for ITM, and the general lesson is recorded as `docs/engineering-lessons.md` §3.3b:
*"undocumented" is a claim about a document, and unlike a claim about behaviour, nothing ever
fails when it is wrong* — so it spreads between files and licenses fitted constants. The
`ref-docs/` correction lands as a new dated supplement, since that corpus is immutable.

### Fixed — ledger S-3 resolved: the `EXL=1` vector is `0x180`

Recorded as MIPS-docs-versus-CEN64. It is neither — **the manual contradicts itself**. Tables
6-3/6-4 (p. 181) define the refill offsets only for `EXL=0`, and §6.4.8 says twice (pp. 187, 188)
that with `EXL=1` you take the common vector. One flowchart, Fig. 6-15 (p. 203), says `0x080` and
is wrong. CEN64 is right, and its source comment that `0x080` *"doesn't make any sense"* is a
reaction to that figure. Kept as a ledger entry rather than deleted, because Fig. 6-15 is still
in the manual and the next reader will find it.

### Added — Sprint 2 planned (T-12-001 … T-12-007)

`to-dos/phase-1-cpu-golden-log/sprint-2-cop0-tlb-exceptions.md`: COP0, the TLB (including the
two-entry instruction micro-TLB in front of the JTLB), the exception model, `CACHE`, and COP1
*control* access. The goal is n64-systemtest reporting a genuine number — the first oracle this
project did not write itself. Its blockers were established by reading the suite's source rather
than assuming: `entrypoint()` calls `CTC1 $31` as its fourth statement, and `main()` immediately
installs handlers at all three exception vectors.

### Added — `SYNC` retires as a NOP instead of raising

Found by the same audit: `SYNC` (SPECIAL funct `0o17`) decoded to `Reserved`, so it would have
raised a reserved-instruction exception on code that runs fine on hardware — and compilers emit
it. *"all load/store instructions in this processor are executed in program order since the SYNC
instruction is handled as a NOP"* (UM §3.1).

The audit's other findings were all legitimately out of Sprint 1 scope — COP0 (`0o20`), COP1
(`0o21`), `CACHE` (`0o57`) and the coprocessor load/store forms belong to Sprints 2 and 3 — or
genuinely unassigned encodings that *should* raise.

### Fixed — `docs/cpu.md` had the link-bit clearing rule wrong

The spec said *"Any intervening store (or ERET) clears the link"*. The manual's list is
exhaustive and does not include stores: *"set by the LL instruction, cleared by an ERET, and
tested by the SC instruction"* (UM §3.1). `SC` is a **tester, not a clearer** — clearing it there
looks right, matches other architectures, and makes the second iteration of an `LL`/`SC` retry
loop fail forever. Now pinned by `sc_does_not_clear_the_link_bit`, and all five mutations of the
new behaviour were checked to fail the suite before the tests were kept.

`ERET` is the one thing that does clear it, and it arrives with the exception model in Sprint 2;
until then nothing clears the bit, which `docs/cpu.md` now states rather than leaving implied.

### Added — the `SysAD` transaction model (T-11-008, partial)

`sysad.rs` models the CPU↔RCP bus as a **packet protocol** — an address cycle carrying a
command, then a data cycle carrying the payload, with an unbounded wait between them where real
RDRAM latency lives. Neither reference emulator models this: both complete the access atomically
and charge a flat constant, and they disagree on the value.

- A `Transaction` is a **state machine** that is structurally incapable of completing in its
  address phase — pinned by `a_transaction_can_never_complete_in_its_address_phase`, since the
  whole point is that a device can observe the bus mid-transaction.
- The inter-phase wait is a **caller-supplied parameter**, not a constant, so it cannot be
  quietly tuned. The caller must justify what it passes.
- Block transfer orderings, including the **sub-block quirk**: a D-cache 128-bit read whose
  address bit 4 is set returns the addressed 64 bits and then the 64 bits *below* it — not the
  ones after. I-cache reads are always sequential regardless.

**`SYSCMD` bit-4 polarity resolved, and it was never a contradiction.** Ledger entry S-1 recorded
the manual and the wiki as disagreeing. Reading both carefully, they **agree on every bit**: a
request has bit 4 clear, a data beat has it set. They differ only in that the wiki calls the
*data-identifier* cycle "Command". No test ROM was needed. Kept as a ledger entry rather than
deleted, because the next reader will hit the same apparent conflict.

**Two criteria are deferred and say so.** The RCP is not yet stepped *between* the phases — the
model supports it, but driving it needs the scheduler to own the transaction rather than `DC`
completing inline, which is a `Bus`-trait change belonging with the cache model in Sprint 2. And
`M` is still unmeasured, exactly as this ticket's own note predicted: `basic.z64` is too short to
constrain it. `M` stays an explicit ledger entry with **no value** rather than a fitted-looking
number without provenance.

### Added — the determinism contract is exercised (T-11-007)

ADR 0004 said "same seed + ROM ⇒ bit-identical output" and nothing checked it. Four tests now do.

- **Bit-identical across runs** — the *whole* machine, not a summary: registers, `HI`/`LO`, PC,
  all three cycle positions, and a content hash of all of RDRAM. A partial hash can hide a
  divergence in a field that only later leaks into the hashed region. Repeated eleven times,
  because an entropy dependency surfaces intermittently rather than on the very next run.
- **Different seeds produce different machines**, added beyond the stated criteria — without it,
  a build that ignored the seed entirely would pass the first test.
- **Reset is reproducible** regardless of what ran before it.
- **A source-level guard** rejects `std::time`, `SystemTime`, `Instant::now`, `getrandom`,
  `thread::spawn`, `HashMap` and `HashSet` anywhere in the core crates. Deliberately *not*
  behavioural: such dependencies are intermittent, so a run-twice test can pass for months
  before the first divergence. This fails on the commit that introduces one, naming file and line.

The content hash is FNV-1a rather than `DefaultHasher`, whose output is randomised per process
— using it would have made the determinism test itself nondeterministic.

Mutation-tested: ignoring the seed fails the second test, and naming a banned construct in any
core crate fails the fourth.

### Added — the documented errata are recorded and pinned (T-11-005)

`docs/cpu.md` gains a **"reproduced, not corrected"** section stating each VR4300 erratum as
intended behaviour, with the manual-vs-hardware divergence spelled out and the pinning test
named. The manual documents none of these; the wiki is the only source.

- `SRA`/`SRAV` leak the upper 32 bits — **all consoles**, never known to be fixed.
- `MULT` is 64-bit × **35-bit**; `DIV` is 32-bit ÷ **35-bit** (divisor sign-extended on bit 34).
- Two new tests: `div_reproduces_the_35_bit_divisor_sign_extension_erratum` and
  `srav_shares_the_sra_erratum` — the variable shift form shares the erratum, and implementing
  one correctly and the other "properly" is an easy inconsistency.

All four errata guards are **mutation-tested**: "correcting" `sra` per the manual fires both the
`SRA` and `SRAV` guards, and reducing `div` to a plain 32-bit division fires its own.

**The FP multiplication bug is deferred to Sprint 3** and recorded rather than silently dropped.
It needs COP1, and it is the only erratum that is *not* universal — NUS-01/02/03 only — so it
also needs the console revision as a machine parameter. Its exact corrupted output is
undocumented and will have to be characterised against hardware.

### Added — the first ROM actually runs (T-11-006)

**`basic.z64` executes end to end and passes all five of its hardware-verified cases** — delay-slot
semantics, `J` in a delay slot, `BEQL` nullification, `BNEL` nullification, and `LWU`+`DADDI`.
59 instructions retired, 129 master ticks. `docs/STATUS.md` gains its first real accuracy number.

This is the first time the emulator has executed a ROM at all, and it validates the delay-slot
and branch-likely work against something other than our own expectations.

- `addr.rs` — virtual→physical translation, the prerequisite the plan had never written down.
  KSEG0/KSEG1 are unmapped and become a subtraction; the mapped segments are masked with a
  `TODO` until the TLB lands. `Cached` is returned now because the *segment* determines it and
  that information is unrecoverable once KSEG0 and KSEG1 have become the same number.
- `rom.rs` — direct loading, doing what IPL3 does: copy `0x10_0000` bytes from ROM offset
  `0x1000`, clamped to both the ROM's length and RDRAM's capacity. A commercial ROM is up to
  64 MiB against 8 MiB of RDRAM, so an unclamped copy panics on exactly the corpus it is for.
- `run_until_complete` implements Dillon's `r30` protocol for real. The pass sentinel is `-1` as
  a **64-bit** value; matching only the low 32 bits would also match `0xFFFF_FFFF`, which is a
  *failure* index.

**The test witnesses execution rather than trusting the sentinel.** `basic.z64` ends with
`BNE $30, $0, TestsFailed` / `ADDI $30, $0, -1`, so if `r30` is still 0 the branch falls through
and the pass sentinel is written **anyway** — "ran and passed" and "never ran" are identical at
the sentinel. The test therefore also asserts the PC entered the test subroutines and that a
plausible number of instructions retired. Mutation-tested: pointing the subroutine address
somewhere unreachable fails it with "this is a vacuous pass".

The test **skips rather than fails** when the ROM is absent — Dillon's suite has no licence, so
it is external-tier and CI has no copy. A gate that is red by default stops being read.

### Added — branches, jumps, and the trap family (T-11-004)

The full control-flow set: `J`/`JAL`/`JR`/`JALR`, every conditional branch including the eight
**branch-likely** forms, the twelve conditional traps, `SYSCALL` and `BREAK`.

**The delay slot now does real work**, and the reverse cascade turns out to make it fall out
cleanly. By the time `EX` resolves a branch, `IC` has already fetched the delay slot — that *is*
the architectural delay slot, not a modelling artefact. Because `EX` runs before `IC` in the same
cycle, writing the target to `next_pc` in `EX` makes the very next fetch land on it with exactly
one delay slot in between. No wrong-path fetch needs squashing.

**Branch-likely nullifies its delay slot when not taken**; an ordinary branch does not. Getting
that backwards silently runs or skips one instruction per untaken branch — invisible until a
loop's trip count is wrong. Mutation-tested.

Two bugs found by a fetch trace rather than by reasoning:

- The redirect was never applied at all — an earlier edit had failed to write, and everything
  still compiled and passed, because the test program's branch target coincided with the
  sequential path. The trace showed `next_pc` marching straight through.
- `in_delay_slot` read `ic_rf`, which `rf_stage` has already vacated by the time `IC` runs in the
  reverse cascade. The flag was silently always false. It now reads `rf_ex`.

**`in_delay_slot` is pinned by its own test**, because mutation-testing showed it was *not yet
load-bearing*: forcing it to `false` broke nothing, since its only consumer is `Cause.BD`/`EPC`
at exception time and COP0 arrives in Sprint 2. Rather than delete it — it is genuinely needed
and must ride in the latch — it is now verified from the moment it is written.

The reserved-instruction tests are repointed at encodings the VR4300 leaves **architecturally**
unassigned (primary opcodes `0o35`/`0o36`, SPECIAL funct `0o01`) rather than at instructions this
project merely hasn't implemented. They had to be moved three times — `LW`, then `BEQ` — because
they were tracking implementation progress instead of the architecture.

### Fixed — a stale test-ROM sentinel, and T-11-006 re-scoped

Investigating whether Sprint 1 could actually close turned up a documentation error that would
have cost real debugging time, and a dependency the plan had wrong.

- **The n64-systemtest completion string in our docs was from 2021.** `docs/testing-strategy.md`
  and `tests/roms/README.md` both quoted `Done! Tests: 262. Failed: 0`. That string does **not**
  appear in the committed v2.1.0 ROM — verified with `strings`, zero occurrences. The real
  summary has the form `Finished in <T>s. Base: Failed <F> of <N> tests (<P>% success rate)`,
  whose counts vary per run — so the sentinel must match the stable pattern
  `Failed (\d+) of (\d+) tests` rather than any literal line. Anything written against the old
  string would have matched nothing, silently. Both documents corrected, and the three text
  sinks the ROM actually uses (emux COP0 hooks, ISViewer, SC64) are now documented — it writes
  to **no fixed RDRAM address**, and with no sink implemented `text_out` is a silent no-op.
- **T-11-006 re-scoped.** n64-systemtest cannot report anything in Sprint 1: it dies at
  `src/main.rs:68` on `CTC1 $31`, the third statement after entry, and needs COP1 control,
  COP0, MI, PI, VI, a heap and exception vectors first — a large fraction of its tests fault
  deliberately and would hang rather than fail. No flag avoids this; category selection is
  compile-time. Retargeted at **`basic.z64`**, which is uniquely suitable: it is the only
  Dillon ROM that does not PI-DMA itself at startup, its result protocol is a single GPR
  (`r30`: 0 running, `-1` pass, `1..=5` failing index), and it needs only the integer core plus
  the T-11-004 branch/jump family. The n64-systemtest goal moves to T-11-009 in Sprint 2.
- Two prerequisites nobody had written down are now acceptance criteria: **KSEG0/KSEG1 segment
  stripping** (nothing does it today, so no ROM can execute) and a **direct-load path**. Also
  recorded that Dillon's suite has no licence, so its test must **skip** rather than fail when
  the ROM is absent.
- T-11-008 notes that fitting `M` needs a ROM that runs long enough to measure — realistically
  n64-systemtest's default-off `timing` set, which is Sprint 2. The transaction model is Sprint
  1 work; the measurement may not be.

### Added — loads, stores, and the unaligned family (T-11-003)

- `mem.rs` — load/store data shaping as pure byte-level transforms. Width and signedness rules
  (`LW` sign-extends into the 64-bit register, `LWU` does not — confusing that pair silently
  breaks any address above 2 GiB), alignment requirements, and the `LWL`/`LWR`/`LDL`/`LDR` and
  `SWL`/`SWR`/`SDL`/`SDR` merges.
- The unaligned family is **big-endian-directional**, and getting a shift backwards produces
  plausible values that are wrong only at unaligned addresses. It is tested as a *pair* — the
  way it is actually used — plus a store-then-load round-trip across every byte offset, which is
  the strongest statement that the shift directions agree with each other.
- `EX` resolves the effective address (base + **sign**-extended offset) and `DC` performs the
  access. That split is the point of the pipeline: `DC` is the cycle the scheduler interleaves
  the RCP around.
- An unaligned *aligned-form* access raises `AddressError`; the `LWL`/`LWR` family is exempt by
  construction, since being usable at any byte offset is the reason it exists.

**The load-delay interlock is live** (UM §4.6.5) — it finally has something to interlock against.
It reproduces the hardware's documented imprecision, and it compares against the `ex_dc` latch
rather than `rf_ex`: in the reverse cascade `EX` runs before `RF`, so by the time `RF` executes,
the instruction that was in `EX` has already moved on. Checking the wrong latch made it silently
never fire, which is what the integration test caught. Mutation-tested.

### Added — decode and `EX`-stage wiring: the CPU executes (T-11-002, second part)

The pipeline stops moving empty latches and starts running programs.

- `decode.rs` — MIPS III decode for the integer subset. **Total**: every 32-bit pattern decodes
  to something, with anything unrecognised becoming `Op::Reserved` rather than panicking. A guest
  can execute arbitrary bytes. Unimplemented opcodes decode to `Reserved` (which raises a
  reserved-instruction exception) rather than to a `NOP`, so a missing opcode is loud instead of
  silently producing wrong results.
- `regs.rs` — the register file, split out so `$zero`'s hardwiring lives in exactly one place.
  A scattered `if rd != 0` is how a write to `$zero` eventually slips through.
- `exec.rs` — `EX` execution as a pure function of `(op, operands, HI/LO)`, bridging decode to
  the ALU. Carries the immediate sign/zero-extension asymmetry: arithmetic forms sign-extend,
  logical forms zero-extend, so `ORI $t0, $0, 0xFFFF` yields `0xFFFF` and not all-ones.
- The pipeline stages now do their jobs: `IC` fetches and decodes, `RF` reads, `EX` executes and
  raises the multiply/divide stall, `WB` commits.

**The operand bypass network (UM §4.6) — found by the end-to-end test, missed by every unit
test.** Without it, back-to-back dependent instructions read stale registers, and `LUI`+`ORI` —
the standard way to build a 32-bit constant — produced `0xFFFF` instead of `0x7FFFFFFF`. Every
one of the 46 unit tests passed while the CPU could not run a six-instruction program. The
bypass is mutation-tested: removing it fails the end-to-end test.

Three integration tests that exercise the whole machine rather than a piece of it: a real
program computing through `ADDIU`/`MULT`/`MFLO`/`ADDU`/`SLL` into the register file; `$zero`
surviving an instruction that targets it; and an overflowing `ADD` aborting without committing.

### Added — the integer ALU (T-11-002, first part)

`crates/rustyn64-cpu/src/alu.rs`: the arithmetic, logical, shift and multiply/divide families as
**pure functions**, deliberately free of pipeline and register-file state so every rule can be
tested without constructing a machine. Decode and the `EX`-stage wiring are the remainder of
T-11-002 and follow separately.

- 32-bit arithmetic (`ADD`/`ADDU`/`SUB`/`SUBU`) trapping on signed overflow where specified, and
  the 64-bit `D*` forms trapping at 64-bit boundaries.
- The logical family (`AND`/`OR`/`XOR`/`NOR`/`LUI`) and all shifts including the `D*` forms.
- `MULT`/`MULTU`/`DIV`/`DIVU` and the `D*` forms writing `HI`/`LO`, with the documented
  full-pipeline stalls (5 / 37 / 8 / 69 `PCycle`s, UM Table 3-12) — these are not background
  operations.
- **Every 32-bit result is sign-extended into the 64-bit register**, the rule that dominates
  MIPS III and the most common source of emulator bugs in it, since it stays invisible until
  software inspects the upper half.
- The `MFHI`/`MFLO` hazard is recorded as a **non-interlocked** two-instruction window producing
  hardware's wrong result — modelling it as a stall would add timing hardware does not have *and*
  hide the value software can observe.

**Two errata reproduced rather than corrected**, each pinned by a test that fails if it is
"fixed":

- `SRA`/`SRAV` leak the upper 32 bits instead of sign-extending bit 31. Present on every console
  and never known to be fixed, so software can depend on it.
- `MULT` acts as a 64-bit by **35-bit** signed multiply (second operand sign-extended on bit 34).
  Invisible for well-formed inputs, which is why ordinary compiler output never trips it.

Two behaviours are **guesses and are recorded as such** in `docs/accuracy-ledger.md` (C-5, C-6)
rather than left looking authoritative: the `DIV` quotient when divisor bits 63 and 31 differ
(N64brew calls this "currently unclear"), and the architecturally-undefined divide-by-zero
values. What is tested and non-negotiable is that neither panics — a guest can do both at will.

### Fixed — rustdoc gets its own CI job

`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` was the **last step of the
`test` job**, after the slow test run. Two consequences, both observed rather than theorised:

- It was the most likely thing to be lost to `cancel-in-progress`. A commit with a broken
  intra-doc link (public docs linking to a private item) went through with its run marked
  **`cancelled`, not failed** — rustdoc never executed. The gate did not fail; it did not run.
- A rustdoc failure reported as `test (ubuntu-latest) failed`, pointing at the wrong subsystem.

Now a dedicated `rustdoc (-D warnings)` job with no `needs:`, so it starts immediately, runs in
parallel with the tests, and finishes in well under a minute — fast enough to be useful feedback
and small enough to rarely be mid-flight when a supersede happens. Verified by reintroducing the
exact defect that slipped through: the job's command rejects it, and `cargo test` is blind to it.

### Added — the ADR 0007 five-stage pipeline (T-11-001, second half)

`crates/rustyn64-cpu/src/pipeline.rs`. **Structure, not instructions** — the stages move latches
and account for time; decode and execute are T-11-002 onward. What is real here is the shape,
which is the part that cannot be retrofitted without rewriting every consumer.

- Four inter-stage `Latch`es (`ic_rf`, `rf_ex`, `ex_dc`, `dc_wb`), each carrying `pc`, `word`,
  `occupied`, `in_delay_slot`, and `abort`. Five stages have four boundaries; the state lives on
  the boundaries.
- `Pipeline::advance` runs **WB → DC → EX → RF → IC**. Each stage reads its input latch before
  any upstream stage writes it, so no value moves two stages in one cycle and no double buffering
  is needed — the reverse order *is* the latching.
- `Stall { cycles, cause }` with `Interlock` naming all eight documented interlocks (LDI, DCB,
  DCM, ICB, ITM, MCI, **COp**, CP0I — UM Table 4-3) so a stall is always attributable in a trace.
  ADR 0007's `resume_stage` is deliberately **absent** until it can be load-bearing: `advance`
  always runs the full cascade today, so a stored `resume` would be read by nothing — the same
  hazard `poll_irq_at_phase` was removed for (`engineering-lessons.md` §3.2).
- An abort raises a pending flush, so the instruction fetched later in the *same* cycle is a
  bubble rather than a live wrong-path fetch that would escape the flush entirely.
- `stall_for(0)` is ignored rather than recorded — a zero-cycle stall would still consume a cycle
  and mark it not-a-run-cycle, silently inserting a bubble *and* suppressing interrupt acceptance
  on the following cycle.
- `Exception` is deliberately **not** named `Fault`: UM §4.5 defines a fault as interlocks ∪
  exceptions, and only the aborting subset rides in a latch.
- `abort_from` stamps an exception into its own latch and every latch **upstream** — the
  kill-younger-instructions step. Older instructions are untouched.
- Interrupts are sampled once per `PClock` in **DC** (UM Figure 4-12, §4.7.6) and accepted only
  if the previous `PCycle` was a run cycle (§4.7.1). Exactly one recognition predicate exists.
- `load_interlocks` reproduces the hardware's documented **imprecision** — matching on the `rs`
  *or* `rt` encoded field whether or not it is used as a source, exempting `$zero`, and not
  crossing the GPR/FPR boundary. Emulating precise behaviour here would be the bug.

Seven pipeline tests, two of which are the structural guards and both **mutation-tested**:

- `a_value_advances_exactly_one_stage_per_cycle` — reversing the cascade to run forwards fails it.
- `delay_slot_flag_survives_a_multi_cycle_stall` — the Phase 1 exit criterion. Dropping the flag
  in transit fails it. A global `in_delay_slot` bool passes a naive test and fails this one.
- `an_abort_survives_the_cascade` — removing the flush fails it.
- Plus stall-freezes-the-pipeline, the interrupt run-cycle gate, abort-kills-younger-only,
  aborted-instructions-do-not-retire, zero-cycle-stall-is-not-a-stall, and the load-interlock
  imprecision cases.

Two existing tests had premises invalidated by this change and were corrected rather than
patched around: `Cpu::tick` no longer retires an instruction per call (it takes 5 `PCycle`s to
fill the pipeline), and the scheduler's step count now derives from `cpu_cycles()` rather than
`Cpu::retired`, since retirement lags stepping by the pipeline depth and the two are no longer
interchangeable. The residue invariant's third term likewise moved to an inter-domain
(CPU vs RCP) comparison, keeping it a property of the clock rather than of the CPU.

### Changed — the ADR 0006 scheduler rework (T-11-001, first half)

The canonical master clock is now **implemented**, not just decided.

- `MASTER_HZ` is **187,500,000**. `master_ticks: u64` is the only counter in the core that is
  ever incremented; `cpu_cycles()`, `rcp_cycles()` and `count_ticks()` are derived accessors,
  not fields. The 3:2 fractional accumulator (`rcp_accum`, `RCP_NUM`, `RCP_DEN`) is deleted.
- Every domain is an integer divisor, exported and asserted exact: `CPU_DIVIDER` 2,
  `RCP_DIVIDER` 3, `COUNT_DIVIDER` 4 (half PClock), `SI_DIVIDER` 12, `PIF_DIVIDER` 96.
- **Per-domain seeded phase offsets.** `Phases { cpu, rcp }` are constants derived from the
  SplitMix64 seed — not counters — so the ownership rule holds while the seeded power-on phase
  stays meaningful. A single shared offset would have made every seed produce an identical
  interleaving from tick 6 onward.
- `tick_one_unit()` is replaced by `step_to_next_edge()` and `run_until(target)`, which advance
  **edge to edge**. The master tick is a time base and is never iterated; `run_until` steps every
  edge in `(now, target]` and deliberately does not overshoot by one.
- `Cpu::cycles` is renamed **`Cpu::retired`** — a retired-work tally, which is the one kind of
  counter still allowed to be incremented because nothing schedules against it. `Cpu::tick` is
  documented as advancing **one PClock, not one instruction**.
- `Bus::poll_irq_at_phase(BusPhase)` is replaced by `Bus::poll_irq()`. `BusPhase` survives to
  describe the bus protocol, with its doc comment stating plainly that it carries no interrupt
  semantics and why.

Seven new tests, two of which are the guards that make the rest trustworthy:

- **`residue_invariants_never_move`** — samples the affine offsets between `master_ticks` and
  every derived position across 64 periods and asserts they never move.
- **`seeds_produce_distinct_interleavings`** — fails if the per-domain phases ever collapse.
- Plus `every_divider_is_exact`, `three_cpu_and_two_rcp_steps_per_six_ticks` (all seeds),
  `same_seed_same_timeline`, `edges_are_never_skipped`, `reset_preserves_phase`.

Both guards were **mutation-tested**: introducing an independently-incremented counter fails the
residue test, and collapsing the phases fails the interleaving test. A guard that cannot fail is
not a guard.

### Added — reference corpus and the accuracy ledger

- **[`docs/accuracy-ledger.md`](docs/accuracy-ledger.md)** — referenced from five documents and
  never created until now. Records measured constants with their provenance, open residuals,
  ruled-out approaches, and contradictions between primary sources. Seeded with the four timing
  constants the hardware documentation does not supply (`M`, the exception epilogue, CP0I, RDRAM
  bank state) and the four source contradictions found during the ADR review. The governing rule:
  a constant is measured, never tuned until a ROM passes — a tuned constant makes every later
  timing result unfalsifiable.
- **[`ref-docs/2026-07-20-vr4300-timing-supplement.md`](ref-docs/2026-07-20-vr4300-timing-supplement.md)**
  — `ref-docs/` is an immutable corpus, so corrections to `research-report.md` land as a dated
  supplement rather than an in-place edit. Records the IC/RF/EX/DC/WB stage-name correction, the
  MClock-is-primary clock derivation, the documented cycle-cost tables, the errata with exact
  behaviour, the SysAD block-ordering rules, and an explicit list of what is *not* documented.
- `docs/glossary.md` gains a **Clocks** section, because "master clock" is overloaded three ways
  and the primary sources own one of them (MasterClock = 62.5 MHz).
- `docs/performance.md` states the project goal plainly — sustained fully cycle-accurate
  emulation at full speed — with the budget (~156M component-steps/s, ~32 host cycles each) and
  an honest account of why it is unproven rather than impossible.

### Changed — the timebase and CPU microarchitecture are settled (Phase 1 design)

Two ADRs land ahead of any Phase 1 code, because both decisions are of the kind that cannot be
retrofitted without rewriting the scheduler and every chip's step contract at once.

- **[ADR 0006](docs/adr/0006-one-canonical-master-clock.md) — one canonical 187.5 MHz master
  clock. Supersedes [ADR 0001](docs/adr/0001-master-clock-lockstep-scheduler.md)**, which is
  retained unmodified as the record of the first design. 187.5 MHz is the LCM of the 93.75 MHz
  CPU and 62.5 MHz RCP clocks, which makes *every* emulated domain an integer divisor: CPU
  every 2 ticks, RCP every 3, COP0 `Count` every 4 (half PClock), SI every 12, PIF every 96.
  The 3:2 fractional accumulator is gone; a fractional path survives only for VI and AI, which
  genuinely run off a different crystal. The load-bearing rule is not the unit but the
  ownership: **`master_ticks` is the only counter ever incremented**, every other cycle position
  is a derived accessor, and a residue invariant test fails if any position becomes independent.
- **[ADR 0007](docs/adr/0007-cycle-accurate-vr4300-pipeline.md) — the VR4300 is a cycle-accurate
  five-stage pipeline** (IC/RF/EX/DC/WB) of four inter-stage latches advanced one PClock per
  step in **reverse stage order** (WB → DC → EX → RF → IC), which is what makes the latching
  implicit and removes any need for double-buffered state. `in_delay_slot` travels with the
  instruction in the latch chain rather than as global CPU state, so `Cause.BD`/`EPC` survive a
  multi-cycle stall between a branch and its delay slot. Interlocks are `(cycles, resume_stage)`.

Three factual corrections came out of the hardware research, all in `docs/cpu.md`:

- The pipeline stages are **IC/RF/EX/DC/WB**, not the "IF/RF/EX/DF/WB" previously documented.
  The manual's entire interlock and exception taxonomy is named stage-relative, so this matters.
- "The CPU advances one issued instruction per master tick" was not a timing model. `DDIV`
  stalls the whole pipeline for 69 PCycles, and at minimum 5 PCycles are needed per instruction.
  The documented cycle-cost tables are now transcribed into `docs/cpu.md` §Cycle costs.
- **`Bus::poll_irq_at_phase(BusPhase)` is removed, not completed.** It was shaped on the
  assumption that interrupt sampling is tied to the SysAD command/data phase; no such coupling
  is documented anywhere. The real rule is per-PCycle, gated on the previous PCycle having been
  a run cycle. Wiring the parameter up to *something* would have encoded a fiction that looked
  correct. `BusPhase` is retained for the bus protocol, with no interrupt semantics attached.

The full 655-page NEC VR4300 User's Manual turned out to be in the local wiki mirror already
(`n64brew_wiki/images/VR4300-Users-Manual.pdf`) with an intact text layer — it is now cited as
the primary timing oracle. Extract with `mutool draw -F txt`; `pdftotext` fails on it.

`docs/scheduler.md`, `docs/cpu.md`, `docs/architecture.md`, `docs/engineering-lessons.md`,
`README.md`, `AGENTS.md`, and the Phase 1 plan are updated to match. Sprint 1 gains T-11-008
(the SysAD transaction model and the measurement of `M`, the undocumented memory access time)
and its estimate rises from 3 to 5 weeks.

### Changed — dependency and toolchain refresh

Everything moved to the newest mutually-compatible versions available.

- Rust toolchain 1.96 → **1.97.1** (`rust-toolchain.toml`, workspace `rust-version`, and the
  `dtolnay/rust-toolchain` pin in all three workflows).
- egui / egui-wgpu / egui-winit 0.34 → **0.35** (MSRV 1.92, satisfied). `Panel::show_inside` is
  deprecated in favour of `Panel::show`; both call sites in `ui_shell` updated.
- `directories` 5 → **6**, `pollster` 0.4 → **1.0**, plus patch/minor moves across `winit`,
  `cpal`, `gilrs`, `rfd`, `bytemuck`, `thiserror`, `bitflags`, and `insta` via `cargo update`.
- GitHub Actions: `checkout` v4 → **v7**, `upload-artifact` v4 → **v7**, `download-artifact`
  v4 → **v8**, `configure-pages` v5 → **v6**, `upload-pages-artifact` v3 → **v5**,
  `deploy-pages` v4 → **v5**. `Swatinem/rust-cache` stays on the floating `v2` (currently 2.9.1).
- `markdownlint-cli` v0.39.0 → **v0.49.1**, which adds MD060 (table-column-style). Delimiter
  rows are normalised to padded form across 15 documents, and MD060 is pinned to
  `style: "padded"` rather than the default per-table inference — inference classified two
  single-row tables with 300-character cells as "aligned" and demanded header padding that
  cannot be met.

**`wgpu` is deliberately held at 29.** wgpu 30 is released, but no published `egui-wgpu`
accepts it — 0.35.0 requires `wgpu = "^29.0"`, and egui's unreleased `main` still pins 29.0.
Bumping wgpu alone would not fail resolution; cargo would link both wgpu 29 (for egui-wgpu) and
wgpu 30 (for the frontend), making the two `wgpu::Device` types distinct and breaking the build
with "expected `wgpu::Device`, found `wgpu::Device`". The rationale is recorded at the
dependency itself, with `cargo tree -p wgpu -d` given as the check.

### Added

- [`docs/engineering-lessons.md`](docs/engineering-lessons.md) — failure patterns carried over
  from two prior cycle-accurate emulators, generalised to this machine rather than copied.
  Ordered by when each lesson must be acted on: structural decisions that are cheap now and
  expensive-to-impossible later, then what "green" is permitted to mean, then debugging
  discipline. Phase-specific entries are also filed as risks in the relevant phase overview.
- Explicit `timeout-minutes` on every job in all three workflows (CI 30, Pages 20, release 45).
  Jobs previously inherited GitHub's 6-hour default, where a hang holds a concurrency slot and
  presents as "CI is slow" rather than as a hang.
- Phase 1 exit criterion requiring an instruction's memory access to be a point the scheduler can
  interleave around, with `Bus::poll_irq_at_phase` reaching a genuinely per-phase branch pinned by
  a test. Retrofitting that structure later means rewriting the scheduler and every chip's step
  contract at once.
- `docs/testing-strategy.md`: re-blessing a visual golden now requires justification against an
  external reference (ParaLLEl-RDP, Angrylion, hardware docs), never against our own previous
  output, with the reference named in the commit message.

### Changed

- ADR 0005 now requires a residual to be classified as an absolute or differential measurement
  before it can be attributed to sub-cycle bus timing. Differential measurements are invariant
  under uniform re-phasing and cannot be evidence for the refactor.
- Phase 5 records the per-game database as a bounded-authority data file: it may describe
  cartridge identity only, the core never consults it, and a frontend-only reproduction is
  treated as a load-path problem until proven otherwise.

### Fixed

- `Bus::poll_irq_at_phase` documented as not yet phase-sensitive. It ignores its `BusPhase`
  argument in both the trait default and the `rustyn64-core` implementation, so the signature
  implied per-phase interrupt sampling that does not exist yet.

## [0.1.0] "Foundation" — 2026-07-20

The architectural skeleton. The workspace compiles, CI is green across three platforms, the
reference corpus is acquired and licence-classified — and **no chip executes an instruction yet**.
This tag exists so the foundation is a fixed, citable point rather than an ever-growing
`[Unreleased]` section.

### Added

- Initial workspace scaffold (cycle-accurate emulator architecture, ported from RustyNES).
- Always-on egui shell (menu bar + status bar + stub debugger window) wired to the
  `winit + wgpu + cpal + egui` frontend, presenting a cleared/test-pattern frame at the
  N64 VI dimensions with the N64 controller input map (digital + analog stick).
- Release automation: a `v*` tag now publishes a GitHub Release with per-target archives
  (Linux x86_64, macOS aarch64, Windows x86_64) containing the `rustyn64` binary, both
  licences, `NOTICE`, `README`, and `CHANGELOG`, plus a `SHA256SUMS` manifest. The tag is
  checked against the workspace version before anything is published.
- Documentation site: pushes to `main` publish the rustdoc API reference to GitHub Pages
  under `/api/`, with `/` reserved for the wasm demo that lands in Phase 6.
- `scripts/mirror_n64brew_wiki.py` builds a gitignored offline mirror of the N64brew Wiki
  at `n64brew_wiki/` (see its `README.md`). 324 pages and 96 media files, with parallel
  HTML / Markdown / wikitext trees and `--refresh` for revision-based incremental updates.
- Test-ROM corpora under `tests/roms/`, split into a committed tier (permissively licensed,
  each ROM shipping its upstream `LICENSE`) and a gitignored external tier:
  - **committed** — `n64-systemtest` (MIT), the self-judging CPU/COP0/TLB/RSP gate, built
    from source since upstream publishes no prebuilt ROM;
  - **external** — PeterLemon/`krom` (196 ROMs), Dillon's n64-tests (26), the 240p Test
    Suite (built from source in a container), and a commercial regression corpus organised
    by save type.
- `scripts/check_no_roms.sh` plus a `no-commercial-roms` CI job: commercial ROMs are blocked
  by three independent guards (`.gitignore`, a pre-commit hook over the staged file list,
  and a server-side CI scan). The hook also enforces that any allowlisted ROM ships a
  `LICENSE` beside it.
- Full phase and sprint planning in `to-dos/`: the ROADMAP gains a status section, a phase spine
  with per-phase goal/exit criteria, cross-phase dependencies, and the open questions that gate
  deeper planning. All nine phase overviews adopt the seven-section skeleton (Goal, Exit criteria,
  Scope, Sprints, Dependencies, Risks, Reference docs), and ten sprint files mint **49 tickets**
  (`T-PS-NNN`, P = phase, S = sprint) with acceptance criteria, dependencies, spec references, and
  complexity. Phase 0's tickets are checked off against what actually shipped; the rest are
  forward plans grounded in the register-level detail from the wiki mirror.
- `docs/DOCUMENTATION_INDEX.md`, the docs map both sibling projects carry — subsystem specs,
  cross-cutting references, subdirectories, and the material outside `docs/`.
- Reference emulators and test suites cloned for study under the gitignored `ref-proj/`
  (ares, cen64, gopher64, simple64, parallel-rdp, parallel-rsp, angrylion-rdp-plus,
  n64-systemtest, n64-tests, libdragon, PeterLemon/N64). Per-repo licences are verified and
  recorded in `ref-proj/README.md`, which classifies each as vendor-ok or study-only.

### Changed

- Modernized the frontend against egui 0.34 / wgpu 29 / cpal 0.18 (`Panel::*::show_inside`,
  `MenuBar::new().ui`, `ui.close`, `Context::run_ui`, `CurrentSurfaceTexture`, the
  `experimental_features` / `multiview_mask` / `immediate_size` descriptor fields).
- Aligned the `to-dos/` phase overviews + ROADMAP to the N64 phase set (RSP LLE, RDP LLE+VI,
  AI audio, cart boot + saves, frontend integration, accuracy breadth).
- Filled out `CONTRIBUTING.md` (Rust-only) and fixed the markdownlint pre-commit hook to
  pass `--config .markdownlint.json`.
- `docs/STATUS.md` gained a project-infrastructure table and a test-ROM corpus table, and
  its accuracy table gained an "oracle available?" column. Several gates now have their ROM
  staged locally while remaining not-started, and collapsing those two states into one
  would misrepresent progress.
- `docs/testing-strategy.md` documents the corpus tiers, the commercial-ROM guards, and the
  per-corpus licensing that decides which tier a corpus lands in.
- `README.md` rebuilt to the structure RustyNES and RustySNES share — centred title block with a
  three-row badge set, Overview, Why RustyN64, Highlights, Features, Quick Start, Default
  Controls, Architecture with crate and layout tables, Compatibility and Accuracy, Performance,
  Platform Support, Documentation, Current Release, Roadmap, Contributing, License,
  Acknowledgments, Citation, and the shared footer. Adapted honestly for a pre-alpha: the
  accuracy badges read "not started" rather than borrowing the siblings' 0-diff claims, the
  Highlights table separates working from stubbed, and the Performance section states that no
  measurements exist rather than inventing any.
- `.gitignore` covers output our own workflows produce when run locally (`_site/` from pages.yml,
  `dist/` and the release archives from release.yml) plus `site/` for a future docs handbook.
- `docs/adr/0004-determinism-contract.md` gained the `Consequences` section the Nygard format and
  the project's own docs rules require — it was the only ADR without one. Records the costs the
  contract imposes and the fact that it is currently specified but unexercised.
- Root `Cargo.toml` excludes `ref-proj/` and `n64brew_wiki/` from the workspace. Cargo's
  upward workspace discovery otherwise makes any nested project there — `n64-systemtest`,
  `gopher64` — resolve *this* workspace as its root and fail to parse our members.

### Fixed

- CI, release, and Pages jobs now install the Linux system headers the frontend needs
  (`libasound2-dev`, `libudev-dev`, `libxkbcommon-dev`, `libwayland-dev`). `cpal` pulls
  `alsa-sys` and `gilrs` pulls `libudev-sys`, whose build scripts call `pkg-config`; those
  headers are absent from the GitHub runner images, so every Linux job that compiled the
  workspace would have failed in a build script.
- `release.yml` pins the toolchain to 1.96 instead of `@stable`. `rust-toolchain.toml`
  takes precedence over the action's default, so the previous config installed one
  toolchain and built with another.
- The nine phase overviews cited "the skill's references/roadmap_template.md" for their exit
  criteria — a dangling reference to the generator skill, which is not part of this repository, so
  no phase had a stated exit bar. Every phase now carries real, checkable criteria.
- `AGENTS.md` claimed the repository used three incompatible ticket-ID schemes. It does not:
  `T-PS-NNN` is a template where P is the phase digit and S the sprint digit, and the overviews
  instantiate it correctly as `T-01-NNN` through `T-81-NNN`. Only the pre-ticket code TODOs use a
  separate subsystem-scoped form.
- `README.md` cited the "Mesen2 / ares / higan" accuracy bar, which belongs to the NES/SNES
  projects. The N64 reference set is ares / CEN64 / Gopher64 / ParaLLEl, per
  `docs/architecture.md`. It also listed only 8 of the 10 workspace crates, and its
  quick-start implied the binary plays games — it currently opens a shell and presents a
  test pattern.
- `crates/rustyn64-frontend/web/Trunk.toml` pinned the wasm-bindgen CLI to 0.2.100 while
  `Cargo.lock` resolved the library to 0.2.126. Trunk requires these to be equal, so the
  wasm build would have failed at bindgen time. A new `wasm-bindgen-pin` CI job now
  compares the two and fails on drift, which is otherwise invisible until someone runs
  `trunk build`.

[Unreleased]: https://github.com/doublegate/RustyN64/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/doublegate/RustyN64/releases/tag/v0.1.0
