# Sprint 3 — The colour combiner, blender, Z/coverage, and the fuzz 0-diff

**Phase:** Phase 3 — RDP LLE + VI
**Sprint goal:** complete the per-pixel pipeline — edge-walked triangles, the colour combiner, the
blender, and Z/coverage — and prove it bit-exact against Angrylion via the ParaLLEl-RDP conformance
suite. This is the **v0.4.0 cut gate**: a real ROM renders a stable frame matching a committed
golden, AND the fuzz suite bit-matches the reference (`to-dos/VERSION-PLAN.md` §v0.4.0).
**Estimated duration:** the largest sprint — several work sessions.

## Reference

Encodings and per-pixel arithmetic come from the N64brew wiki (`Reality Display Processor/`) and
the ParaLLEl-RDP reference (MIT — `shaders/`, `rdp_device.cpp`, `rdp_renderer.cpp`). The
conformance harness is `ref-proj/parallel-rdp/rdp_conformance.cpp` + `conformance_utils.hpp`.
**Licence:** ParaLLEl-RDP is MIT (readable); its bundled `angrylion-rdp-plus` is **non-commercial
MAME-licensed — study outputs, never vendor its source** (`ref-proj/README.md`). See
[[rdp-tmem-load-recipe]] and [[rdp-texture-rect-recipe]] for the texture path already built.

## Tickets

### T-33-001 — The edge-walked triangle rasteriser

**Description:** the triangle setup and span walk — `Fill Triangle` (0x08) and its shade/texture/Z
variants (0x09–0x0F). Decode the edge coefficients (the three edges, `xh/xm/xl` + slopes, `yh/ym/yl`),
walk spans between the major and minor edges per scanline, and interpolate the per-vertex attributes
(shade RGBA, S/T/W, Z) declared by the shade/texture/zbuffer flag bits. Flat-fill first, then the
interpolated attributes. This is the foundation every later ticket renders through.

**Acceptance criteria:**

- [x] The base triangle (0x08) fills the correct pixels (major/minor edge span walk, left/right-major),
      byte-exact against a hand-computed right triangle (`fill_triangle_flat_fills_a_right_triangle`).
- [ ] The shade (bit 58 → 0x0C–0x0F), texture (bit 57 → 0x0A/0x0B/0x0E/0x0F), and Z (bit 56 →
      0x09/0x0B/0x0D/0x0F) coefficient blocks are decoded and interpolated per pixel; the multi-word
      length matches `command_len_words`. **Deferred to R-9** — the variants flat-fill for now
      (coefficient words length-consumed only); attribute interpolation lands with the combiner
      (T-33-002) and Z (T-33-004). *(The earlier flag map here was wrong; corrected above.)*
- [x] Scissor clipping applies; the `lmajor` flag selects the walk direction.

**Complexity:** XL

---

### T-33-002 — The colour combiner (`Set Combine Mode`, 0x3C)

**Description:** the two-cycle colour/alpha mux. Decode the sub-A/B/C/D selects for colour and alpha
in both cycles, and evaluate `(A − B) * C + D` per pixel from the combiner inputs (combined, texel0/1,
shade, primitive, environment, …).

**Acceptance criteria:**

- [x] Every mux input select decodes and routes correctly (checked against the ParaLLEl-RDP input
      tables) — the 16-field decode is unit-tested with distinct per-field values. The **common**
      inputs are wired; the **exotic** inputs (noise, LOD frac, key/convert) are deferred to
      ledger **R-10** (they need the LOD/key/convert state) and read as zero.
- [x] `(A − B) * C + D` is evaluated with the correct clamping/rounding per cycle, asserted by the
      **observable output colour** for known inputs (a texel passthrough, a lerp, and the clamp
      fold) computed by hand — not a self-reported flag.
- [x] 1-cycle and 2-cycle modes both work — 2-cycle chaining (cycle 0 → cycle 1's `Combined`) is
      unit-tested. Wiring the combiner into the triangle pipeline (with the `Set Other Modes`
      cycle-type gate + shade/texture attributes) lands with T-33-003/004.

**Complexity:** L

---

### T-33-003 — The blender (`Set Other Modes`, 0x2F)

**Description:** decode the big other-modes word (cycle type, blend mode `P*A + M*B`, coverage mode,
alpha compare, dither, Z mode) and evaluate the blend equation against the framebuffer, with coverage,
alpha-compare, and the dither modes.

**Acceptance criteria:**

- [x] `Set Other Modes` (0x2F) fields decode (cycle type, the two blend cycles' `P/A/M/B` selects,
      `force_blend`, coverage-dest, `image_read_en`, alpha-compare, Z enables/mode) — unit-tested
      with distinct per-field values so a swapped bit range surfaces. `Set Blend Color` (0x39) /
      `Set Fog Color` (0x38) latch the colour registers.
- [x] The blend equation writes the correct value — the RDP's divide-free `(P * a0 + M * (a1 + 1)) >> 5`
      (with `a0 = A >> 3`, `a1 = B >> 3`), asserted **value-for-value** by hand-computed muxed inputs
      and by a 2-cycle chain whose result differs from cycle 0 alone (so a no-op cannot pass). The
      literal `P*A + M*B` in the original description was the schematic form; the hardware weights are
      5-bit and the `M` term carries a `+1` (N64brew *…/Blender*, ParaLLEl-RDP `blender.h`).
- [~] **Deferred to R-11 / T-33-004.** Alpha-compare, the coverage/dither modes, the AA-edge divider,
      and the memory-alpha blend-shift need the per-pixel framebuffer read + coverage accumulator,
      which reach the blender only once the triangle pipeline routes combiner→blender per pixel
      (T-33-004). The memory-read blender itself is now wired (T-33-004 PR-B 2b-blend, gated on
      `force_blend`); the remaining coverage/AA/alpha-compare/dither paths land with slice 2c. The
      oracle stays 93 — no systemtest drives the render path (ledger **R-11**).

**Complexity:** L

---

### T-33-004 — Z-buffering, coverage, and primitive depth (0x2E)

**Description:** the Z-buffer test/update (the N64's 15.3 float-ish Z encoding), coverage accumulation
at edges, and `Set Primitive Depth` (0x2E).

Split into **PR-A** (the Z machinery — pure, oracle-verified functions) and **PR-B** (the Z-buffer
storage + coverage + per-pixel pipeline routing), per the split-large-tickets rule.

**Acceptance criteria:**

- [x] **PR-A.** Z encode/decode matches the hardware format (exact inverses of ParaLLEl-RDP
      `z_encode.h`, validated by boundary values + a `z_compress ∘ z_decompress` round-trip); the Z
      test honours the Z mode, asserted by **observable occluding-vs-occluded pairs** per mode (opaque,
      transparent, decal, and `z_compare` off) at the `depth_test` function level — a pixel that must
      and must not pass. `Set Depth Image` (0x3E) and `Set Primitive Depth` (0x2E) latch. Six new tests.
- [~] **PR-B.** The Z-buffer **RDRAM read/write** (the 18-bit-per-pixel hidden-bit storage) actually
      updates the buffer for an occluding-vs-occluded pair. Coverage accumulates at partially-covered
      edge pixels and feeds the blender. Primitive depth is used when the triangle carries no per-vertex
      Z. This is where `depth_test`/the codec get their runtime caller (routing combiner→blender→depth
      per pixel), which also closes the R-9 flat-fill. Ledger **R-12**.
      - [x] **part 1** — Z-buffer RDRAM read/write + hidden bits (`RdramBus` methods + lazy Bus store),
            round-trip tested (PR #75).
      - [x] **part 2a** — per-pixel depth test + Z-write in the rasteriser (`decode_triangle_z`,
            `interpolate_z`, `depth_span`); occluding-triangle pair proves accept+reject (PR #77).
      - [x] **part 2b** — combiner→blender routing: shade (`interpolate_shade`), texture
            (`interpolate_st` + `fetch_texel`, non-perspective and perspective divide), and the
            **memory-read blender** (`read_pixel` + `blend`, gated on `force_blend`) all wired per
            pixel; closes the R-9 flat-fill for shaded/textured/translucent triangles.
      - [~] **part 2c** — sub-pixel coverage accumulator (`quantize_x` sticky-bit edge rounding), the
            coverage-driven AA blend + `cvg_dest` write-back, primitive-depth `z_source_sel`, and
            alpha-compare/dither. Feeds the conformance gate (T-33-005).
            - [x] **2c-coverage primitives** — `compute_coverage` (4×2 diamond-sample 8-bit mask) +
                  `quantize_x` (`s.16`→`s.3` sticky snap) + `aa_enable` decode, a bit-exact port of
                  `coverage.h`/`span_setup.comp`, pinned by hand-computed unit tests. No runtime
                  caller yet (rasteriser still unions sub-scanlines) — the exact-inclusion rewrite
                  changes every triangle's edge pixels, so its re-derived goldens are validated
                  against the T-33-005 conformance vectors, not hand-derived expectations.
            - [ ] **inclusion rewrite** — replace the union-bbox span with per-Y-subpixel edges +
                  `compute_coverage` pixel gating; re-derive the affected triangle goldens.
            - [ ] AA-edge blend (coverage-weighted `uBlenderDividerLUT`), `cvg_dest` write-back,
                  `z_source_sel` prim-depth, alpha-compare, dither.

**Complexity:** L

---

### T-33-005 — The ParaLLEl-RDP conformance 0-diff (the cut gate)

**Description:** the bit-exactness gate. **Licence-clean approach:** run ParaLLEl-RDP's conformance
harness (`rdp_conformance.cpp`, which drives Angrylion as the reference) **externally** to generate
`(command-stream → golden-framebuffer)` vectors, commit **both halves of each vector** — the RDP
command stream (the input) and the golden framebuffer (the output) — and have the RustyN64 harness
replay each command stream through its RDP and compare its framebuffer to the committed golden.
"Outputs, not source" means the corpus contains **no Angrylion source or binary**: the command
stream is a plain byte blob and the golden framebuffer is Angrylion's rendered *output*, both freely
committable, keeping Angrylion an external output oracle (`ref-proj/README.md`; module 20
golden-vector rule) and never a vendored dependency.

**Fixed vectors, not live fuzzing.** The gate replays a **committed, deterministic** corpus so CI is
reproducible; the *generation* uses the conformance harness's randomised command synthesis, but the
committed vectors are frozen (a seed + generator version is recorded so the corpus can be
regenerated/expanded, but CI never runs Angrylion live). A future live-fuzz mode is out of scope for
the cut gate.

**IMPLEMENTATION NOTE (revised from the plan above).** The generator does **not** build ParaLLEl-RDP's
`rdp_conformance.cpp` (that needs the whole Granite/Vulkan engine). Instead it drives **Angrylion
directly** — a standalone ~200-line MIT driver (`crates/rustyn64-test-harness/vectors-gen/driver.c`)
that links only Angrylion's CPU-only `n64video.c` + `parallel.cpp` (no Vulkan/OpenGL/Granite), pokes a
command list into RDRAM, calls `n64video_process_list()`, and reads the framebuffer back. Vectors are
hand-authored command lists (deterministic, `parallel=false`), not randomised. The `.rvec` container
(9×u32 BE header + command bytes + golden framebuffer) lives under `tests/vectors/`; the golden pixels
are stored raw big-endian at small resolutions (the 8×8 vectors are 204–228 B each — well within
budget, no compression/LFS needed yet). **Reproducibility:** the corpus was generated against Angrylion
(`angrylion-rdp-plus`) pinned at commit `31bdb1f0a79dd726017a38432540c6b5db0fa117`; a different
Angrylion revision could shift the goldens, so that commit is the recorded provenance. See
`vectors-gen/README.md`.

**Acceptance criteria:**

- [x] A committed vector corpus under `tests/vectors/` (`.rvec` = command-stream + golden framebuffer)
      generated by the Angrylion driver; the generator is committed and documented (reproducible: fetch
      the Angrylion submodule, `make`, `./driver`).
- [x] A harness runner (`tests/rdp_conformance.rs`) replays each vector and asserts a byte-exact
      framebuffer match. **FILL rectangle passes.**
- [~] Expand the corpus toward ~150 vectors — the v0.4.0 cut criterion. **In progress (12 vectors
      passing + 1 ignored WIP):** `tex_rect_copy_16` + `tex_rect_offset_16` (COPY-mode Texture Rectangle,
      1:1 origin and offset blits — the **first texture path validated against Angrylion**, clean because
      copy mode bypasses the combiner/texel pipeline; non-1:1/Flip/8-32bit copy need RustyN64 impl first),
      `fill_rect_16`, `fill_tri_16`, `fill_tri_wide_16`, `fill_tri_neg_16`,
      `fill_tri_frac_16` (FILL rounds), `shade_tri_frac_16` (1-cycle sub-pixel coverage),
      `shade_depth_tri_frac_16` (the depth path applies the same coverage), `shade_tri_32` (32-bit
      RGBA8888 colour path, dither off), `dither_tri_32` (the default **magic** dither over a flat
      `0x112233` shade), `shade_grad_tri_32` (a **Gouraud gradient** — dx.R and de.G non-zero — the
      first to validate `interpolate_shade` against the oracle). The generator now has **command-block
      macros** (`SHADE_BLOCK`/`SHADE_BLOCK_FLAT`/`Z_SUFFIX`) that expand to the exact word count, so
      the short-block bug (a shade/z block written too short → misaligned suffix → blank frame) cannot
      recur. A **`tex_tri_16`** vector (11th, committed **`#[ignore]`d**) was added to exercise
      `interpolate_st` against Angrylion via a new **v2 `.rvec` preload** region (a texture placed in
      RDRAM before the command list). It pins a **real** divergence RustyN64 does not model (ledger
      R-13, settled by Angrylion instrumentation after two retracted mis-diagnoses): the vector is
      well-formed (tile configured, S advances, texel 0 fetched as red), but Angrylion's output differs
      because of the **1-cycle TEXEL0 pipeline** (`texel0/texel1` swap) and the **s10.5 coordinate
      scale**, neither modelled yet. The v2 preload plumbing itself is verified (all-white → white).
      **Dither is now implemented** (`apply_rgb_dither`, a bit-exact port of
      Angrylion `dither.c` `rgb_dither`): the default **dither is ON** (RGB dither mode 0 = "magic"),
      so non-extreme colours round up per pixel where the 4×4 matrix cell is below the channel's low 3
      bits; earlier vectors that predate this disabled dither (Set Other Modes hi bits 7:4 = `1111` →
      `0x2F0000F0`). The 2c sub-pixel coverage is wired for both the shaded/textured **and depth**
      1-/2-cycle paths (inclusion + coverage write-back), and the ordered RGB dither is wired on both;
      remaining: the interpenetration-Z coverage, the AA-edge blend, other `cvg_dest` modes,
      alpha-compare, noise dither (R-10). NB: constructing shade+z / textured vectors requires the
      FULL 16-u32 shade block — a short block silently misaligns the z-suffix and Angrylion renders blank.
      The first triangle vector caught
      the 4× edge-slope bug (ledger **R-14**), now **fixed** — the slopes are pre-shifted `>> 2` at
      decode and the affected triangle unit-test goldens were corrected against the oracle. Next:
      shaded / textured / depth / scissor triangle vectors, then the 2c coverage inclusion rewrite
      (which needs vectors with *fractional* edges, where the union approximation and Angrylion
      diverge), then breadth across combiner / blend / format / Z modes toward ~150.

**Complexity:** XL — the long pole.

---

### T-33-006 — A real ROM renders a stable golden frame

**Description:** the other half of the cut criterion — a real (homebrew/test) ROM's command stream
renders a stable frame matching a committed golden, through the full RSP→RDP→VI path (the rdpq
microcode already boots on the RSP and emits a command list, Phase 2).

**Acceptance criteria:**

- [ ] A real ROM's RDP output is pinned by a committed golden frame (extends `golden_frame.rs`).
- [ ] The frame is stable across runs (determinism, ADR 0004).

**Complexity:** M

---

## Carried residuals to close here (fuzz-validated)

- **R-3** fill rounding, **R-5** VI scale/AA filters, **R-7** 4-bit / 32-bit-block loads,
  **R-8** Texture Rectangle Flip / non-16-bit copy / alpha-compare — each re-checked against the
  conformance vectors and either fixed or re-ledgered with evidence.

## Sprint review checklist

- [ ] All tickets checked off or explicitly deferred (with reason).
- [ ] The ParaLLEl-RDP conformance suite bit-matches the reference (or every diff is ledgered).
- [ ] A real ROM renders a stable frame matching a committed golden.
- [ ] CHANGELOG.md updated; `docs/rdp.md` kept in sync; the phase-close release ceremony runs
      (VERSION-PLAN §v0.4.0 — the annotated tag + notes on `main`).
