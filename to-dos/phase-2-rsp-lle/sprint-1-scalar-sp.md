# Sprint 1 — The scalar unit, DMEM/IMEM, and the SP interface

**Phase:** Phase 2 — RSP LLE
**Sprint goal:** the RSP boots from an uploaded microcode image, executes its scalar ISA against
DMEM/IMEM, DMAs to and from RDRAM, and halts and interrupts correctly through the SP interface.
**Estimated duration:** 3 weeks

## Tickets

### T-21-001 — DMEM/IMEM and the SP address space

**Description:** implement the two 4 KiB on-chip memories and the CPU-visible address space that
lets the VR4300 read and write them directly, with the wrapping behaviour hardware has.

**Acceptance criteria:**

- [ ] DMEM and IMEM are 4 KiB each and addressable from both the RSP and the CPU.
- [ ] Addresses wrap within each memory rather than spilling between them.
- [ ] CPU-side access works at 8, 16, 32, and 64 bits.
- [ ] n64-systemtest's SPMEM access category passes.

**Dependencies:** T-11-003
**Reference:** `docs/rsp.md`; `n64brew_wiki/markdown/Reality Signal Processor/Interface.md`
**Estimated complexity:** M

---

### T-21-002 — The SP interface registers

**Description:** implement `SP_DMA_SPADDR`, `SP_DMA_RAMADDR`, `SP_DMA_RDLEN`, `SP_DMA_WRLEN`,
`SP_STATUS`, `SP_DMA_FULL`, `SP_DMA_BUSY`, `SP_SEMAPHORE`, and `SP_PC`, including the
double-buffered DMA the hardware supports.

**Acceptance criteria:**

- [ ] Every register reads and writes with hardware semantics, including the write-1-to-clear
      bits in `SP_STATUS`.
- [ ] DMA is double-buffered: a second transfer can be queued while one is in flight, and the
      full/busy bits report it.
- [ ] `SP_SEMAPHORE` implements its test-and-set behaviour.
- [ ] Writing `SP_PC` sets the start address, and clearing halt begins execution there.

**Dependencies:** T-21-001
**Reference:** `n64brew_wiki/markdown/Reality Signal Processor/Interface.md`
**Estimated complexity:** L

---

### T-21-003 — DMA between DMEM/IMEM and RDRAM

**Description:** implement the transfer engine in both directions, including the skip/count
stride form and the alignment rules, taking the correct time so polling loops behave.

**Acceptance criteria:**

- [ ] Both directions transfer correctly at every legal length.
- [ ] The stride form transfers the right rows with the right skip.
- [ ] Alignment rules match hardware, including what happens on a misaligned request.
- [ ] Transfers take realistic time; `SP_DMA_BUSY` clears only when they finish.

**Dependencies:** T-21-002
**Reference:** `docs/rsp.md` §DMA
**Estimated complexity:** L

---

### T-21-004 — The scalar unit ISA

**Description:** implement the RSP's MIPS subset — no 64-bit operations, no TLB, DMEM/IMEM
addressing only — with the RSP's own branch and delay-slot behaviour.

**Acceptance criteria:**

- [ ] The supported integer, logical, shift, load/store, and branch instructions execute
      correctly.
- [ ] Instructions the RSP does *not* implement behave as hardware does rather than as the
      VR4300 would.
- [ ] Delay slots behave per the RSP pipeline, which is not identical to the main CPU's.
- [ ] `SP_STATUS` halt and broke are reachable from the instruction stream.

**Dependencies:** T-21-003
**Reference:** `n64brew_wiki/markdown/Reality Signal Processor/CPU Core.md`
**Estimated complexity:** L

---

### T-21-005 — Halt, break, and the MI interrupt path

**Description:** wire `SP_STATUS`'s halt, broke, and interrupt-on-break behaviour to the MI line
so the CPU's wait loops terminate, preserving the lockstep property that an SP event is visible
to the very next CPU step.

**Acceptance criteria:**

- [ ] `BREAK` halts the RSP and sets the broke bit.
- [ ] Interrupt-on-break raises the SP line through the MI, and the CPU services it via IP2.
- [ ] A halt is visible to the CPU step immediately following it, not at the next batch
      boundary. *(This is the ADR 0001 lockstep contract, and the easiest place to break it.)*
- [ ] n64-systemtest's RSP category reports a real number.

**Dependencies:** T-21-004
**Reference:** `docs/scheduler.md`; `docs/adr/0001-master-clock-lockstep-scheduler.md`
**Estimated complexity:** M

---

## Sprint review checklist

- [ ] All tickets checked off or explicitly deferred (with reason).
- [ ] The RSP boots an uploaded microcode image and halts under its own control.
- [ ] CHANGELOG.md updated.
- [ ] `docs/rsp.md` updated in the same change as the code it describes.
