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

- [ ] Every opcode 0x00-0x3F is recognised with the right length, so the stream never desyncs.
- [ ] The no-operation ranges (0x00-0x07, 0x10-0x23, 0x31) are consumed correctly.
- [ ] An unimplemented command consumes its words and is counted, rather than derailing the FIFO.
- [ ] The FIFO drains under the scheduler, not in one burst.

**Dependencies:** T-21-005
**Reference:** `n64brew_wiki/markdown/Reality Display Processor/Commands.md`
**Estimated complexity:** L

---

### T-31-002 — The synchronisation commands and the DP interrupt

**Description:** implement `Sync Load` (0x26), `Sync Pipe` (0x27), `Sync Tile` (0x28), and
`Sync Full` (0x29), with the last raising the DP interrupt through the MI.

**Acceptance criteria:**

- [ ] Each sync command has its real effect on pipeline state rather than being a no-op.
- [ ] `Sync Full` raises the DP interrupt and the CPU services it.
- [ ] The documented RDP hazards are respected rather than hidden behind extra syncs.

**Dependencies:** T-31-001
**Reference:** `n64brew_wiki/markdown/Reality Display Processor/Hazards.md`
**Estimated complexity:** M

---

### T-31-003 — Framebuffer state and the fill pipeline

**Description:** implement `Set Color Image` (0x3F), `Set Fill Color` (0x37), `Set Scissor`
(0x2D), and `Fill Rectangle` (0x36) — enough to write known pixels into RDRAM at a known
address.

**Acceptance criteria:**

- [ ] The colour image address, format, and width are honoured.
- [ ] `Fill Rectangle` writes exactly the scissored region, at every supported bit depth.
- [ ] The scissor rectangle clips correctly at all four edges.
- [ ] A unit test fills a known rectangle and verifies the RDRAM contents byte for byte.

**Dependencies:** T-31-001
**Reference:** `n64brew_wiki/markdown/Reality Display Processor/Pipeline.md` §Fill Pipeline
**Estimated complexity:** M

---

### T-31-004 — The VI register set and scan-out

**Description:** implement `VI_CTRL`, `VI_ORIGIN`, `VI_WIDTH`, `VI_V_INTR`, `VI_V_CURRENT`,
`VI_BURST`, `VI_V_TOTAL`, `VI_H_TOTAL`, `VI_H_VIDEO`, `VI_V_VIDEO`, and the scale registers, and
scan the framebuffer out to a presentable buffer.

**Acceptance criteria:**

- [ ] Every register reads and writes with hardware semantics.
- [ ] `VI_V_CURRENT` advances with the scan position and is readable mid-frame.
- [ ] The VI interrupt fires at the `VI_V_INTR` scanline and drives the MI.
- [ ] Scan-out honours origin, width, and the scale registers, producing correct geometry.
- [ ] The frame is emitted to the harness so `frame_hash` has something real to hash.

**Dependencies:** T-31-003
**Reference:** `n64brew_wiki/markdown/Video Interface.md`
**Estimated complexity:** L

---

### T-31-005 — The first golden frame

**Description:** capture a frame from a homebrew ROM that exercises fill and scan-out, commit it
as a golden, and wire the comparison into the harness so Layer 5 stops being scaffolding.

**Acceptance criteria:**

- [ ] A golden frame is committed under `tests/golden/`.
- [ ] `FrameComparison` compares a live frame against it and reports where it differs.
- [ ] The comparison runs in CI behind `test-roms`.
- [ ] `docs/STATUS.md`'s visual-golden row gains a real status.

**Dependencies:** T-31-004
**Reference:** `docs/testing-strategy.md` Layer 5
**Estimated complexity:** M

---

## Sprint review checklist

- [ ] All tickets checked off or explicitly deferred (with reason).
- [ ] A real ROM produces a stable frame that matches a committed golden.
- [ ] CHANGELOG.md updated.
- [ ] `docs/rdp.md` updated in the same change as the code it describes.
