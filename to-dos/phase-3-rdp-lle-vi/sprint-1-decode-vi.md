# Sprint 1 — Command decode, the fill pipeline, and VI scan-out

**Phase:** Phase 3 — RDP LLE + VI
**Sprint goal:** the shortest honest path from an RSP-produced command list to a visible frame —
decode the command stream, rasterise the fill pipeline, and scan the framebuffer out through the
VI.
**Estimated duration:** 3 weeks

## Tickets

### T-31-001 — The DP FIFO and command decoder

**Description:** implement the DP command FIFO and a decoder over the full 0x00-0x3F opcode map,
dispatching each command with its correct word length even where the handler is not yet written.

**Acceptance criteria:**

- [x] Every opcode 0x00-0x3F is recognised with the right length, so the stream never desyncs.
- [x] The no-operation ranges (0x00-0x07, 0x10-0x23, 0x31) are consumed correctly.
- [x] An unimplemented command consumes its words and is counted, rather than derailing the FIFO.
- [x] The FIFO drains under the scheduler, not in one burst.

**Dependencies:** T-21-005
**Reference:** `n64brew_wiki/markdown/Reality Display Processor/Commands.md`
**Estimated complexity:** L

---

### T-31-002 — The synchronisation commands and the DP interrupt

**Description:** implement `Sync Load` (0x26), `Sync Pipe` (0x27), `Sync Tile` (0x28), and
`Sync Full` (0x29), with the last raising the DP interrupt through the MI.

**Acceptance criteria:**

- [x] Each sync command has its real effect on pipeline state rather than being a no-op.
- [x] `Sync Full` raises the DP interrupt and the CPU services it.
- [ ] The documented RDP hazards are respected rather than hidden behind extra syncs.
      **Deferred to Sprint 3** (see the CLOSED status below) — the render pipeline the hazards
      govern does not exist until the combiner/blender/texture-use path lands.

**Dependencies:** T-31-001
**Reference:** `n64brew_wiki/markdown/Reality Display Processor/Hazards.md`
**Estimated complexity:** M

---

### T-31-003 — Framebuffer state and the fill pipeline

**Description:** implement `Set Color Image` (0x3F), `Set Fill Color` (0x37), `Set Scissor`
(0x2D), and `Fill Rectangle` (0x36) — enough to write known pixels into RDRAM at a known
address.

**Acceptance criteria:**

- [x] The colour image address, format, and width are honoured.
- [x] `Fill Rectangle` writes exactly the scissored region, at every supported bit depth.
- [x] The scissor rectangle clips correctly at all four edges.
- [x] A unit test fills a known rectangle and verifies the RDRAM contents byte for byte.

**Dependencies:** T-31-001
**Reference:** `n64brew_wiki/markdown/Reality Display Processor/Pipeline.md` §Fill Pipeline
**Estimated complexity:** M

---

### T-31-004 — The VI register set and scan-out

**Description:** implement `VI_CTRL`, `VI_ORIGIN`, `VI_WIDTH`, `VI_V_INTR`, `VI_V_CURRENT`,
`VI_BURST`, `VI_V_TOTAL`, `VI_H_TOTAL`, `VI_H_VIDEO`, `VI_V_VIDEO`, and the scale registers, and
scan the framebuffer out to a presentable buffer.

**Acceptance criteria:**

- [x] Every register has read/write-through support (a write is stored and reads back). The
      per-register hardware **write-mask** semantics are deferred — ledger R-4 — so this is not
      yet full hardware semantics, only store-and-read-back.
- [x] `VI_V_CURRENT` advances with the scan position and is readable mid-frame.
- [x] The VI interrupt fires at the `VI_V_INTR` scanline and drives the MI.
- [x] Scan-out honours origin and width. **The X/Y_SCALE resampling and the AA/divot/de-dither
      filters are NOT implemented** — scan-out is a 1:1 copy (ledger R-5); geometry is correct
      only when the source matches the display resolution.
- [x] The frame is emitted to the harness so `frame_hash` has something real to hash.

**Dependencies:** T-31-003
**Reference:** `n64brew_wiki/markdown/Video Interface.md`
**Estimated complexity:** L

---

### T-31-005 — The first golden frame

**Description:** capture a frame from a homebrew ROM that exercises fill and scan-out, commit it
as a golden, and wire the comparison into the harness so Layer 5 stops being scaffolding.

**Acceptance criteria (DONE — PR #63, `93191db`, with deviations noted):**

- [x] A golden frame is committed — as an **inlined FNV-1a hash constant** (`GOLDEN_FILL_4X2`
      in `tests/golden_frame.rs`), not a file under `tests/golden/`. The harness comparator
      (`frame_hash` / `compare_to_golden`) is hash-based, so a committed constant is the
      equivalent pin; the test also asserts the constant equals the independently-recomputed
      digest of the expected bytes, so it cannot be stale.
- [x] `FrameComparison` compares a live frame against it (`compare_to_golden(&frame, GOLDEN_FILL_4X2)`).
- [x] The comparison runs in CI — **unconditionally** in the ordinary `cargo test --workspace`
      (light CI leg), not gated behind `test-roms`. This is strictly more coverage: the frame is
      synthetic (no committed ROM to gate), and `test-roms` gates zero code today.
- [x] `docs/STATUS.md`'s visual-golden row gains a real status.

**Deviation — synthetic command stream, not a homebrew ROM.** The frame is produced by a
hand-authored RDP FILL command list driven through `Bus::rdp_tick` + `Bus::scanout`, not by
booting a ROM: real cartridge boot is Phase 5 (cart/PIF). The path from the DP FIFO onward is
identical, so the synthetic frame exercises the whole Sprint-1 picture path. The real-ROM
golden (krom/240p through the full CPU→RDP→VI path) lands once cart boot exists.

**Dependencies:** T-31-004
**Reference:** `docs/testing-strategy.md` Layer 5
**Estimated complexity:** M

---

## Sprint review checklist

- [x] All tickets checked off or explicitly deferred (with reason).
- [x] A **synthetic** RDP FILL frame is stable and matches a committed golden hash. A **real
      ROM** doing the same landed later this phase (T-33-006, Sprint 3): a license-clean homebrew
      ROM boots on the VR4300 and renders a committed golden frame through the VI.
- [x] CHANGELOG.md updated.
- [x] `docs/rdp.md` updated in the same change as the code it describes.

## Sprint status — CLOSED 2026-07-22

All five tickets landed on `main` via PR: T-31-001 (#56), T-31-002 (#57), T-31-003 (#58),
T-31-004 (#59/#60/#61, split into register-file / scan-out / scan-position), the frontend
scan-out wiring (#62), and T-31-005 (#63). Two acceptance items are intentionally deferred,
not dropped:

- **VI X/Y_SCALE resampling and the AA/divot/de-dither filters** (part of T-31-004's "scale
  registers" and the overview's VI criterion) are **not** implemented — scan-out is a 1:1 copy.
  Tracked as **ledger R-5**. Per-register VI write masks are **ledger R-4**.
- **The documented RDP hazards** (T-31-002's third criterion) are deferred to Sprint 3, where
  the render pipeline that the hazards govern (texture-load-then-use) actually exists. In
  Sprint 1 the sync commands carry only their fixed pipeline stalls.

Next: **Sprint 2 — texture state, TMEM, and the texel formats.**
