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

- [x] `third_party/libdragon-rsp/` holds the exact `#include` closure of
      `rsp_rdpq.S` (traced by assembling — the `-MM` scan missed
      `rsp_rdpq.inc`/`_tri.inc`/`regdef.h`, exactly why the plan says "by
      assembling"), pinned to commit `35f85a0`, Unlicense + `NOTICE` present,
      the tree marked linter-exempt (`.markdownlintignore`).
- [x] The blob is byte-reproducible from the vendored source with the documented
      `mips64-elf-gcc … -Wl,-Trsp.ld` invocation (`assemble.sh`); the committed
      blob is byte-identical to a fresh build.
- [x] A CI job (`microcode blob integrity`) checks the committed blob against its
      `SHA256SUMS`, toolchain-free, catching a corrupted or hand-edited blob.
      *Follow-up:* full source→blob reassembly in CI needs `mips64-elf` on the
      runner; today it is the local `assemble.sh` gate documented in `NOTICE`.
- [x] Running the suite needs **no** toolchain — only regeneration does.

**Status:** **done** (this ticket). The blob assembles to 7864 bytes (DMEM
`0x0000..0x1000` + IMEM to `_text_end` `0x1eb8`); `tests/microcode.rs` ties its
length to the symbol map and asserts the entry point is real code.

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

- [x] The blob loads into DMEM+IMEM and the real microcode executes
      (`tests/microcode.rs::the_microcode_boots_to_its_idle_break`). The *full*
      `rsp_queue_t` boot state (command-list DRAM pointer, RDP output buffers) is
      **not needed for the idle break** and moves to T-24-003, where the queue is
      actually processed — the idle path runs entirely in the SU before any DMA.
- [x] The witness starts from a baseline that is itself unreachable as a pass —
      `SP_STATUS` running (`HALTED`/`BROKE` clear), PC at `_start` (0) — then
      asserts the transition: the kernel reaches its documented idle `break`
      (`rsp_queue.inc:403`, "No new commands yet, go to sleep"), so `BROKE` sets,
      the PC advances off `_start`, and `$gp` is zeroed by the `li $gp,0`
      prologue. A microcode that never ran stays at PC 0, not halted, and fails;
      zeroed IMEM nops out the step budget. Success and never-ran do not converge.
- [x] `docs/rspq-boot.md` records the boot ABI with source citations — the
      `rsp_queue_t` boot-state struct at `RSPQ_DATA_ADDRESS = 8`, which pointers
      `rspq_start` patches (`rspq.c:519–548`), the SP_STATUS signal set, and
      `SP_PC = _start`.

**Status:** **done.** The real libdragon rdpq microcode boots on our RSP and
reaches its documented idle `break`, witnessed non-vacuously
(`tests/microcode.rs::the_microcode_boots_to_its_idle_break`). The boot ABI is
documented (`docs/rspq-boot.md`). The key insight that made this a clean minimal
test: the idle `break` (`rsp_queue.inc:403`) fires after only the SU prologue and
the `SIG_MORE` check, *before* any DMA — so the full `rsp_queue_t`/command-queue
boot state is not needed to prove the microcode boots. That state (patched DRAM
pointers, the RDP output buffers) is needed only once the queue is actually
processed, which is T-24-003.

**Dependencies:** T-24-001
**Reference:** `docs/rspq-boot.md`; `include/rsp_queue.inc` (`_start` @391,
`RSPQ_Loop` @442, the `.data` layout @281–362); `src/rspq/rspq.c` `rspq_start`
(@519) and the `rsp_queue_t` struct (`rspq_internal.h:195`, `RSPQ_DATA_ADDRESS`
@218).
**Estimated complexity:** L

---

### T-24-003 — Feed a command list; capture the emitted RDP commands

**Description:** place a small, fixed RSPQ command list in RDRAM (chosen so every
emitted RDP command word is hand-verifiable — e.g. set-fill-color → fill-rect →
sync-full), run the RSP to drain, and capture the RDP command list the microcode
emits through the DPC path.

**Status: DONE.** The command-fetch/DMA/dispatch mechanism works end-to-end on
our RSP+Bus, and an `rdpq` overlay command now emits an RDP command through the
DPC seam. `the_kernel_dmas_and_dispatches_a_command_queue` (`tests/microcode.rs`)
covers the internal-command path (sets `SIG_MORE`, `DMAIn`s the queue into
`RSPQ_DMEM_BUFFER`, dispatches, returns to idle). `the_microcode_emits_an_rdp_command_through_the_dpc_seam`
covers the overlay path: the resident `rdpq` overlay is "registered" with three
DMEM writes (`RSPQ_CURRENT_OVL`, `RSPQ_OVERLAY_IDMAP`, the header `command_base`)
so `RSPQ_Loop` dispatches `0xC0` (`RDPQCmd_Passthrough8`) to the resident handler
without a DMA load; `RDPQ_Send` DMAs the 8 command bytes to the RDP buffer and
`RSPQCmd_RdpAppendBuffer`'s `mtc0 DP_END` reaches `Rdp::dpc_write` via
`StepResult::dp_write`. The seam wiring (RSP COP0 `c8`–`c15` → Bus → RDP) is the
new `StepResult::dp_write` field. Documented in `docs/rspq-boot.md` (§Dispatching
an `rdpq` overlay command) and `docs/rsp.md`/`docs/rdp.md`.

**Acceptance criteria:**

- [x] The **full initial state is pinned and deterministic**: DMEM/IMEM loaded
      from the T-24-001 blob at fixed addresses; RDRAM zeroed except the fixture
      command list and the RSPQ/RDP scratch it needs; `SP_PC` at the kernel
      `_start`; `SP_STATUS` running with `HALTED`/`BROKE` clear (ADR 0008's
      unreachable baseline, carried into this fixture); the RDP output-buffer
      base and length fixed and documented.
- [x] The fixture RSPQ command list is authored in Rust with each entry's meaning
      documented against the rdpq command table.
- [x] The RSP runs to a **defined completion condition** — the queue drains AND
      the kernel reaches its idle/`BREAK` site (not merely "the loop returned") —
      within a bounded step budget that fails loudly if exceeded.
- [x] The emitted RDP command words are captured from the DPC seam and witnessed
      (`DP_END` advanced to `buffer + 8`, `BROKE` set) before the capture is
      trusted. Mutation-checked: severing the Bus→RDP forwarding, or corrupting
      the overlay `command_base`, each turns the test red.

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

**Status: DONE.** `the_microcode_generates_a_set_fill_color_command`
(`tests/microcode.rs`) feeds `RDPQCmd_SetFillColor32` (`0xD6`), which the handler
*transforms* into a `SET_FILL_COLOR` RDP command (`[0xF700_0000, colour]`), and
byte-compares the emission against a golden vector derived from the **documented**
encoding — N64brew *Reality Display Processor/Commands* §`0x37 - Set Fill Color`
(opcode `0x37` in bits 61:56; colour[31:0] verbatim) — not from another emulator
and not from the microcode (the `0xD6 → 0xF7` word0 transform is itself the
witness that the microcode generated, rather than passed through, the command).

**Acceptance criteria:**

- [x] The golden RDP command bytes are derived from the documented encoding, with
      a per-command provenance note (wiki section / macro), committed as a golden
      vector changed only on intentional, reviewed behaviour change.
- [x] The harness asserts byte-equality over the **exact captured range** (the
      8-byte command in the RDP buffer + `DP_END` at `buffer + 8`), so a truncated
      or over-long emission fails rather than partially matching, with execution
      witnessed (no vacuous pass).
- [x] `docs/STATUS.md` records Phase 2 criterion 2 as **met**.

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
