# Booting the RSPQ + rdpq microcode (T-24-002 boot ABI)

How the real libdragon RSPQ command-queue kernel + `rdpq` overlay is brought up
on the RSP, reverse-engineered from the vendored source (`third_party/
libdragon-rsp/`) and libdragon's `src/rspq/rspq.c`. This is the spec the boot
harness reproduces in Rust; it does **not** snapshot a libdragon run (ADR 0008).
Every claim cites the source it came from.

## The image

`assemble.sh` produces one flat blob, `rsp_rdpq.bin` (see `docs/rdp.md` and
`third_party/libdragon-rsp/NOTICE.md`):

- **DMEM** = `blob[0x0000 .. 0x1000]` — the `.data` section (`_data_start`=0x000
  … `_data_end`=0x448) plus zero padding.
- **IMEM** = `blob[0x1000 .. 0x1eb8]` — the `.text` section (`_start`=IMEM 0x000
  … `_text_end`).

The DMEM half already contains the **default `rspq_data` template**: libdragon's
`rspq_start()` seeds its own copy with `memcpy(&rspq_data, rsp_queue.data +
RSPQ_DATA_ADDRESS, sizeof(rsp_queue_t))` (`rspq.c:620`), i.e. from this same
`.data`. So most of the boot state is *in the blob*; only a few pointers are
patched at runtime.

## The boot state: `rsp_queue_t` at DMEM `RSPQ_DATA_ADDRESS`

`RSPQ_DATA_ADDRESS = 8` (`rspq_internal.h:218`), which is exactly the
`RSPQ_OVERLAY_TABLE` symbol (`microcode/symbols.txt` → DMEM `0x008`). The
`rsp_queue_t` struct (`rspq_internal.h:195`, `__attribute__((aligned(16),
packed))`) begins with the overlay table and, further in, holds the fields the
kernel reads each loop:

| Field | Meaning | Set at boot by |
| --- | --- | --- |
| `rspq_ovl_table` | overlay id → handler table | the blob's `.data` (default) |
| `rspq_dram_lowpri_addr` | the low-priority command-queue RDRAM address | `PhysicalAddr(lowpri.cur)` (`rspq.c:621`) |
| `rspq_dram_highpri_addr` | the high-priority queue address | `rspq.c:622` |
| `rspq_rdp_buffers[2]` | the RDP dynamic-buffer RDRAM addresses | `rspq.c:624`… |
| `rspq_dram_addr` | the queue address currently being processed | `= rspq_dram_lowpri_addr` (`rspq.c:623`) |

**What the harness must patch** (everything else comes from the blob's default):
`rspq_dram_lowpri_addr`, `rspq_dram_highpri_addr`, `rspq_dram_addr` (all → the
fixture command-queue RDRAM address) and `rspq_rdp_buffers[0..1]` (→ the RDP
output buffers whose commands T-24-003 will capture). Field byte offsets are
computed from the packed layout and pinned against the blob in the harness, not
hand-guessed.

## The bring-up sequence (`rspq_start`, `rspq.c:519`)

1. **Load the microcode** — `rsp_load(&rsp_queue)`: DMEM ← blob DMEM half, IMEM ←
   blob IMEM half.
2. **Overlay the patched state** — `rsp_load_data(&rspq_data, sizeof(rsp_queue_t),
   RSPQ_DATA_ADDRESS)` (`rspq.c:532`): write the `rsp_queue_t` (with the DRAM
   addresses patched, above) into DMEM at offset 8.
3. **A dummy overlay header** goes just past the data (`rspq.c:543`) — needed for
   the overlay machinery; the harness reproduces it if the boot requires it.
4. **Set the SP_STATUS signals** (`rspq.c:548`): clear `SIG0`, `SIG1`,
   `SIG_HIGHPRI_RUNNING`, `SIG_SYNCPOINT`, `SIG_HIGHPRI_REQUESTED`, `SIG_MORE`;
   set `SIG_BUFDONE_LOW`, `SIG_BUFDONE_HIGH`. (`INTR_BREAK` is deliberately left
   off — "we don't need it".)
5. **Run** — `__rsp_run_async(0)`: `SP_PC ← 0` (the ucode entry = `_start`, IMEM
   0x000) and clear `SP_STATUS.HALT`.

## Two scopes: the minimal boot (T-24-002) vs the full bring-up (T-24-003)

The kernel entry doubles as its idle check (`rsp_queue.inc:391`,
`RSPQCmd_WaitNewInput`):

```text
_start:  li  $gp, 0
         mfc0 t0, SP_STATUS
         andi t0, SIG_MORE          # 0x4000
         bnez t0, wakeup            # SIG_MORE set -> process the queue
         li   t0, CLEAR_SIG_MORE    # (delay slot)
         break                      # SIG_MORE clear -> "go to sleep" (idle)
wakeup:  ...                        # fetch + DMA the queue, then RSPQ_Loop
```

- **Minimal boot (T-24-002, implemented).** `rspq_start` clears `SIG_MORE` at
  boot (`rspq.c:548`), so with no queued work the `bnez` is not taken and the
  kernel falls through to `break` — its documented idle state ("No new commands
  yet, go to sleep", `rsp_queue.inc:403`). This path runs **entirely in the SU,
  before any DMA**, so its only preconditions are: the blob in DMEM+IMEM,
  `SP_PC = _start`, `SIG_MORE` clear, and the RSP un-halted. **No `rsp_queue_t`
  patching, command queue, or Bus/RDRAM state is required.** The `break` lands at
  IMEM `0x14`.
- **Full bring-up (T-24-003, implemented).** To actually *process* a command
  list, the CPU writes it to RDRAM, patches the `rsp_queue_t` DRAM pointers
  (above), sets `SIG_MORE`, and un-halts. The kernel then takes `wakeup`, DMAs
  the queue into the DMEM ring, and dispatches through `rspq_ovl_table` into
  `RSPQ_Loop` (`:442`). This scope needs the patched boot state and a
  `System`-level run with RDRAM. Two tests cover it:
  `the_kernel_dmas_and_dispatches_a_command_queue` (an *internal* command —
  `WaitNewInput`, overlay 0) and `the_microcode_emits_an_rdp_command_through_the_dpc_seam`
  (an *`rdpq` overlay* command that emits an RDP command; see below).

## Dispatching an `rdpq` overlay command without a DMA load (T-24-003)

Overlay 0 (internal commands, high nibble 0) dispatches with no extra state. An
`rdpq` command (`0xC0`–`0xFF`) normally makes `RSPQ_Loop` (`rsp_queue.inc:472`)
DMA the overlay's code+data in from RDRAM (`rspq_load_overlay`, `:536`). But our
blob is the **combined** kernel+`rdpq` image (ADR 0008), so the `rdpq` handler
code is *already resident* in IMEM — we only need the kernel to believe `rdpq` is
the currently-loaded overlay, so the `bne ovl_index, current_ovl` at `:489` skips
the load and jumps to the resident handler. That is exactly what libdragon's
`rspq_overlay_register_internal` (`src/rspq/rspq.c:800`) sets up; three DMEM
writes reproduce it (no `System`-level `rspq_init` needed):

| DMEM symbol | Value | Source |
| --- | --- | --- |
| `RSPQ_CURRENT_OVL` | `0xC` | reloaded each dispatch (`:472`); `RDPQ_OVL_ID = 0xC<<28` (`rdpq.h:159`) |
| `RSPQ_OVERLAY_IDMAP[0xC..=0xF]` | `0xC` | `rspq.c:851`; read at `:476` by high nibble |
| `_ovl_data_start + 14` (`command_base`) | `id<<5` = `0x180` | `rspq.c:858`; `sub cmd_index, command_base` at `:493` ⇒ command 0 = `RDPQCmd_Passthrough8` |

`RDPQCmd_Passthrough8` (`rsp_rdpq.S:135`) then forwards its two command words to
the RDP: `RDPQ_Write8` stages them, `RDPQ_Send` (`rsp_rdpq.inc:60`) DMAs them to
`RDPQ_CURRENT` in RDRAM, and `RSPQCmd_RdpAppendBuffer` (`rsp_queue.inc:819`) runs
`mtc0 a0, COP0_DP_END`. That `MTC0` is the RSP↔RDP seam — it reaches the DPC via
`StepResult::dp_write` → `Bus::rsp_tick` → `Rdp::dpc_write` (`docs/rsp.md`,
`docs/rdp.md`). The test seeds the dynamic-buffer state `RDPQ_Send` reads
(`RDPQ_CURRENT`, `RDPQ_SENTINEL`, `RDPQ_DYNAMIC_BUFFERS`) so the buffer has room,
and witnesses both the command bytes in RDRAM and `DP_END` advanced past them.

## Witnessing the minimal boot (the T-24-002 test)

Per ADR 0008, the witness starts from a state **unreachable as a pass** —
`SP_STATUS` running (`HALT`/`BROKE` clear), `SP_PC` at `_start` (0), and `$gp`
holding a sentinel — and asserts the *transition*:

- `BROKE` is set (not merely `HALTED`: a `SET_HALT` write would halt without a
  `break`), so the kernel reached its idle `break`;
- `SP_PC` advanced off `_start`;
- `$gp` was overwritten to 0 by the `li $gp, 0` prologue (the sentinel is what
  makes this non-vacuous — the register file starts zeroed).

A microcode that never ran stays at `_start` with `$gp` still the sentinel, not
`BROKE`; zeroed IMEM nops out the step budget. Success and never-ran cannot
converge. (`tests/microcode.rs::the_microcode_boots_to_its_idle_break`.)

## Where this runs

Because the kernel DMAs its command list from RDRAM, the boot is driven at the
**`System` level** (Bus + RDRAM + RSP), the same entry the n64-systemtest runner
uses — not the standalone `Rsp`. The command list lives in RDRAM; the emitted RDP
commands leave through the DPC seam (`docs/rdp.md`), which T-24-003 captures.
