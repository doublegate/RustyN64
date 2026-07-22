# Sprint 4 — Booting a real graphics microcode

**Phase:** Phase 2 — RSP LLE
**Sprint goal:** discharge Phase 2's second exit criterion — *a real graphics
microcode boots and emits a plausible RDP command list* — by booting libdragon's
real `rdpq` microcode on the RSP and byte-comparing the RDP command list it emits
against a reference derived from hardware documentation.
**Design:** `docs/adr/0008-microcode-boot-harness.md` (the accepted decision;
this sprint is its task breakdown).
**Estimated duration:** 3–4 weeks
**Blocked on:** the `mips64-elf` toolchain (T-24-001); everything after it is
Rust and documentation.

## What the boot actually looks like (verified from libdragon source)

`rdpq` is **not standalone** — `src/rdpq/rsp_rdpq.S` opens with `#include
<rsp_queue.inc>` and `RSPQ_BeginOverlayHeader`, so it is an *overlay* on the
**RSPQ command-queue kernel**. The pieces, cited to the vendored-to-be source:

- **The kernel** (`include/rsp_queue.inc`). `_start` (rsp_queue.inc:391) checks
  `SP_STATUS.SIG_MORE`, DMAs a portion of the command list from RDRAM into the
  in-DMEM ring `RSPQ_DMEM_BUFFER` (rsp_queue.inc:362), and falls into
  `RSPQ_Loop` (rsp_queue.inc:442). The loop reads the next command word from the
  DMEM buffer, decodes an overlay + command index from its high byte, loads the
  overlay's handler address from `RSPQ_OVERLAY_TABLE` / `RSPQ_OVERLAY_IDMAP`
  (rsp_queue.inc:288–289), and jumps to it with `ra = RSPQ_Loop`
  (rsp_queue.inc:533). Handlers return by `j RSPQ_Loop`.
- **The DMEM layout** is fixed by the `.data` section from `_data_start`
  (rsp_queue.inc:281): the overlay table, the overlay id-map, the command ring
  `RSPQ_DMEM_BUFFER`, and the saved-state regions. The exact byte offsets come
  from the assembled symbol map (T-24-001), not from a hand count.
- **The overlay** (`src/rdpq/rsp_rdpq.S`). Its command table (the `0xC0`–`0xFF`
  block at the top of the file) maps RDPQ command bytes to handlers; handlers
  assemble RDP command words and call `RDPQ_Send` (rsp_rdpq.S:198/525), which
  drives `DPC_START`/`DPC_END` — the register file landed in #44 — to push the
  assembled commands to the RDP.
- **The C-side init** (`src/rspq/rspq.c`, `src/rdpq/rdpq.c`) normally sets up the
  initial DMEM state before the RSP runs: the command-list DRAM pointer, the
  populated overlay table (with `rdpq` registered), and the RDP output-buffer
  pointers. **Reproducing the minimal subset of that state in Rust** — not
  snapshotting a libdragon run — is the load-bearing task (T-24-002), per
  ADR 0008.

The RDP command stream leaves through the DPC FIFO, so the DPC register file is
the capture seam. This sprint is its first real exercise and will likely surface
the next DP behaviour to model (the FIFO drain, `CURRENT` advance, `SYNC_FULL` →
DP interrupt); those are tracked as they arise, not pre-built.

## Tickets

### T-24-001 — Vendor and assemble the microcode

**Description:** vendor the libdragon RSPQ kernel + `rdpq` overlay + their include
closure + `rsp.ld` into `third_party/libdragon-rsp/`, pinned to an exact upstream
commit, with the Unlicense text and a provenance `NOTICE`. Add a build step that
assembles the microcode with `mips64-elf` and commit the resulting DMEM+IMEM
blob and symbol map; a CI job re-assembles from source and checksums the blob so
it can never silently drift from source.

**Acceptance criteria:**

- [ ] `third_party/libdragon-rsp/` holds the exact `#include` closure of
      `rsp_rdpq.S` (traced by assembling, not guessed), pinned to a commit hash,
      Unlicense + `NOTICE` present, marked immutable/generated (linter-exempt).
- [ ] The blob is byte-reproducible from the vendored source with the documented
      `mips64-elf-gcc … -Wl,-Trsp.ld` invocation; the committed blob matches.
- [ ] A CI job regenerates the blob and fails on any mismatch (generated-vs-hand
      discipline applied to a binary).
- [ ] Running the suite needs **no** toolchain — only regeneration does.

**Dependencies:** the `mips64-elf` toolchain on `PATH`.
**Reference:** ADR 0008; `ref-proj/libdragon/rsp.ld`; `ref-proj/libdragon/n64.mk`
(the assemble rule + `-march=mips1 -mabi=32 -nostartfiles`).
**Estimated complexity:** M

---

### T-24-002 — Reproduce the boot state; boot to idle

**Description:** document the rspq/rdpq boot ABI from the vendored source + the
symbol map (the DMEM fields the kernel reads, the command-queue layout, the
overlay-table format, the `RDPQ_Send` → DPC path), and reproduce the minimal
boot state in Rust. Ship a test that boots the microcode and witnesses it
reaching idle — **no output comparison yet**.

**Acceptance criteria:**

- [ ] The blob loads into DMEM+IMEM; the minimal DMEM boot state (overlay table
      with `rdpq`, the command-list DRAM pointer, the RDP output-buffer pointers)
      is constructed in Rust, grounded in `rspq_init`/`rdpq_init`, not snapshotted.
- [ ] The witness starts from a baseline that is itself unreachable as a pass —
      `SP_STATUS` running (`HALTED`/`BROKE` clear), PC at the kernel `_start`, not
      the idle handler — then asserts the transition (`BROKE` set, PC at the
      idle/`BREAK` site), plus a DMEM cell the boot path is known to write.
      Success and never-ran states must not converge (ADR 0008; engineering
      lessons).
- [ ] `docs/rsp.md` (or a new `docs/rspq-boot.md`) records the boot ABI with
      source citations.

**Dependencies:** T-24-001
**Reference:** `include/rsp_queue.inc` (`_start` @391, `RSPQ_Loop` @442, the
`.data` layout @281–362); `src/rspq/rspq.c` architectural overview.
**Estimated complexity:** L

---

### T-24-003 — Feed a command list; capture the emitted RDP commands

**Description:** place a small, fixed RSPQ command list in RDRAM (chosen so every
emitted RDP command word is hand-verifiable — e.g. set-fill-color → fill-rect →
sync-full), run the RSP to drain, and capture the RDP command list the microcode
emits through the DPC path.

**Acceptance criteria:**

- [ ] The **full initial state is pinned and deterministic**: DMEM/IMEM loaded
      from the T-24-001 blob at fixed addresses; RDRAM zeroed except the fixture
      command list and the RSPQ/RDP scratch it needs; `SP_PC` at the kernel
      `_start`; `SP_STATUS` running with `HALTED`/`BROKE` clear (ADR 0008's
      unreachable baseline, carried into this fixture); the RDP output-buffer
      base and length fixed and documented.
- [ ] The fixture RSPQ command list is authored in Rust with each entry's meaning
      documented against the rdpq command table.
- [ ] The RSP runs to a **defined completion condition** — the queue drains AND
      the kernel reaches its idle/`BREAK` site (not merely "the loop returned") —
      within a bounded step budget that fails loudly if exceeded.
- [ ] The emitted RDP command words are captured from the DPC seam over an
      **exact, documented range** (`DPC_START..DPC_END` in the output buffer), not
      a heuristic scan; execution is witnessed (`DPC_END` advanced past
      `DPC_START`, `BROKE` set) before the capture is trusted.

**Dependencies:** T-24-002
**Reference:** `src/rdpq/rsp_rdpq.S` (the `0xC0`–`0xFF` command table, `RDPQ_Send`
@198/525); `docs/rdp.md` (the DPC register file).
**Estimated complexity:** L

---

### T-24-004 — The golden byte-compare (the criterion)

**Description:** derive the expected RDP command byte stream for the fixture from
the **documented RDP command encoding** — N64brew *Reality Display
Processor/Commands* plus libdragon's `rdpq_macros.h` field layouts, **never
another emulator** — commit it as a golden vector with provenance, and assert
byte-equality with the captured output. This ticket *is* Phase 2's second exit
criterion.

**Acceptance criteria:**

- [ ] The golden RDP command bytes are derived from the documented encoding, with
      a per-command provenance note (wiki section / macro), committed as a golden
      vector changed only on intentional, reviewed behaviour change.
- [ ] The harness asserts byte-equality over the **exact captured range** from
      T-24-003 (length and offsets fixed, not "whatever was produced"), so a
      truncated or over-long emission fails rather than partially matching, with
      execution witnessed (no vacuous pass).
- [ ] `docs/STATUS.md` records Phase 2 criterion 2 as **met**, and `docs/
      accuracy-ledger.md` carries any residual.

**Dependencies:** T-24-003
**Reference:** `n64brew_wiki/markdown/Reality Display Processor/Commands.md`;
`ref-proj/libdragon/include/rdpq_macros.h`; module 20 (*Golden vectors*).
**Estimated complexity:** M

---

## Definition of done (sprint / phase close)

- T-24-001…004 complete; the golden byte-compare passes with execution witnessed.
- Both Phase 2 exit criteria hold (RSP category `Failed: 0` — already met — **and**
  the microcode boot/emit compare).
- `docs/STATUS.md` updated; then the v0.3.0 phase-close ceremony
  (`to-dos/VERSION-PLAN.md` §v0.3.0): pre-release gate, annotated tag, notes.
  **Not before both criteria hold.**
