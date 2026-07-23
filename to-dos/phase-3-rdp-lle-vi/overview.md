# Phase 3 — RDP LLE + VI

## Goal

The software reference RDP in `rustyn64-rdp` consumes the real command list the RSP emitted and
rasterises it through the full per-pixel pipeline — texture fetch, the colour combiner, the
blender, and the Z/coverage logic — writing pixels into the framebuffer in RDRAM. The VI then
scans that framebuffer out. This is the first phase that produces a picture, and the first that
can be graded against an image.

The software reference is always-correct and stays the oracle. A wgpu-compute accelerator is
Phase 7 work, validated *against* this renderer, never replacing it (ADR 0002).

## Exit criteria

- [ ] The command decoder handles the full opcode map 0x00-0x3F, including the fill and shaded
      triangle forms (0x08-0x0F), the texture rectangles (0x24/0x25), and the fill rectangle
      (0x36).
- [ ] All five pipeline modes are implemented: 1-cycle, 2-cycle, fill, copy, and load.
- [ ] The colour combiner implements the full mux (`Set Combine Mode`, 0x3C) for both cycles.
- [ ] The blender implements the `Set Other Modes` (0x2F) blend equation, including coverage,
      alpha compare, and the dither modes.
- [ ] Texture state is exact: `Set Tile` (0x35), `Set Tile Size` (0x32), `Load Tile` (0x34),
      `Load Block` (0x33), and `Load TLUT` (0x30), with TMEM addressing and all texel formats.
- [ ] The synchronisation commands behave: `Sync Load` (0x26), `Sync Pipe` (0x27),
      `Sync Tile` (0x28), and `Sync Full` (0x29) — the last raising the DP interrupt.
- [ ] Z-buffering and coverage are correct, including the scissor rectangle (0x2D) and primitive
      depth (0x2E).
- [ ] The RDP hazards documented upstream are respected, rather than papered over with syncs.
- [ ] The VI scans out correctly: `VI_CTRL`, `VI_ORIGIN`, `VI_WIDTH`, `VI_V_INTR`,
      `VI_V_CURRENT`, `VI_H_VIDEO`/`VI_V_VIDEO`, and `VI_X_SCALE`/`VI_Y_SCALE`, raising the VI
      interrupt at the programmed scanline.
- [ ] The ParaLLEl-RDP fuzz suite bit-matches the Angrylion reference across its ~150 tests.
- [ ] A real ROM renders a stable, correct frame, verified against a committed golden.

## Scope

In-scope:

- The RDP command decoder and the DP FIFO.
- The rasteriser: edge-walk, texture, combiner, blender, Z/coverage.
- TMEM and every texel format.
- The VI: scan-out, the interrupt, and the AA/divot/de-dither filters.
- Golden-frame capture and comparison in the harness.

Out-of-scope:

- The wgpu-compute RDP accelerator (Phase 7). This phase produces the reference it will be
  graded against.
- Audio (Phase 4) and controller input (Phase 5), even though a rendered frame makes their
  absence obvious.
- Upscaling, texture packs, and shaders (Phase 8).

## Sprints

- [Sprint 1 — Command decode, the fill pipeline, and VI scan-out](sprint-1-decode-vi.md) —
  the shortest path from a command list to a visible frame. **Status:** COMPLETE (2026-07-22).
- [Sprint 2 — Texture state, TMEM, and the texel formats](sprint-2-texture.md) —
  the state and data path a textured primitive needs, up to a copy-mode Texture Rectangle.
  **Status:** COMPLETE (2026-07-23) — the first textured picture.
- [Sprint 3 — The colour combiner, the blender, Z/coverage, and the fuzz 0-diff](sprint-3-pipeline.md) —
  the per-pixel pipeline and the ParaLLEl-RDP conformance bit-exactness gate (the v0.4.0 cut criterion).
  **Status:** planned; in progress.

## Dependencies

Phase 2 complete: without the LLE RSP there is no real command list to rasterise. The
`rustyn64-rdp` crate already depends on `rustyn64-cart` for the `RdramBus` trait — the single
permitted chip-to-chip edge in the graph.

## Risks

- **The combiner and blender are a combinatorial surface** — the mux has far more legal
  configurations than any single game exercises, so "works on this ROM" is not coverage.
  Mitigated by the ParaLLEl-RDP fuzz suite, which is a bit-exactness oracle rather than a
  visual one.
- **The reference renderer is the oracle, so its bugs are invisible** — a wrong pixel that
  matches nothing upstream will be graded as correct by our own goldens. Mitigated by grading
  against Angrylion's output rather than our own captures.
- **Angrylion is licence-poisoned** — it is the natural thing to read while chasing a
  bit-exactness failure, and it is non-commercial MAME-licensed. Compare outputs, never source
  (`ref-proj/README.md`).
- **Performance will look alarming** — a per-pixel software rasteriser at N64 fill rates is
  slow, and the temptation is to optimise before it is correct. Mitigated by ADR 0002's ordering:
  correctness first, acceleration later and validated.

## Reference docs

- [docs/rdp.md](../../docs/rdp.md) — the rasteriser spec.
- [docs/testing-strategy.md](../../docs/testing-strategy.md) — Layer 5, visual goldens.
- [docs/performance.md](../../docs/performance.md) — where the time goes and when to care.
- `n64brew_wiki/markdown/Reality Display Processor/Commands.md` — the full opcode map.
- `n64brew_wiki/markdown/Reality Display Processor/Pipeline.md` — the five pipeline modes.
- `n64brew_wiki/markdown/Reality Display Processor/Hazards.md`
- `n64brew_wiki/markdown/Video Interface.md` — the VI register set.
