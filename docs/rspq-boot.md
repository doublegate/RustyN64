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

## Reaching idle

`_start` (`rsp_queue.inc:391`) checks `SIG_MORE`, DMAs a portion of the command
list at `rspq_dram_addr` into the in-DMEM ring, and enters `RSPQ_Loop`
(`:442`), dispatching each command through `rspq_ovl_table`. The kernel goes
idle when the queue signals it has no more work; the exact terminating command
and the resulting halt/`BREAK` state are the remaining T-24-002 finding, pinned
against a run rather than assumed.

## Witnessing the boot (the T-24-002 test)

Per ADR 0008, the witness must start from a state **unreachable as a pass** —
`SP_STATUS` running (`HALT`/`BROKE` clear), `SP_PC` at `_start`, not the idle
handler — and assert the *transition*: the kernel actually executed (its
prologue `li $gp, 0` ran, `SP_PC` advanced into `RSPQ_Loop`, and the defined
idle/`BREAK` state was reached). A microcode that never ran stays at `_start`,
not idle, and fails — success and never-ran must not converge.

## Where this runs

Because the kernel DMAs its command list from RDRAM, the boot is driven at the
**`System` level** (Bus + RDRAM + RSP), the same entry the n64-systemtest runner
uses — not the standalone `Rsp`. The command list lives in RDRAM; the emitted RDP
commands leave through the DPC seam (`docs/rdp.md`), which T-24-003 captures.
