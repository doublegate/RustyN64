# RDP (Reality Display Processor) and VI scan-out — RustyN64

**References:** `ref-docs/research-report.md` §4 (RDP + VI + ParaLLEl-RDP), §8
(RDRAM 9th bit); ADR 0002; `crates/rustyn64-rdp/src/lib.rs`;
`docs/architecture.md`; `docs/rsp.md`; `docs/performance.md`.

This doc is the SPEC, not history — update it in the same PR as the code. The RDP
gate is **bit-exactness** against the Angrylion-Plus reference on the ParaLLEl-RDP
conformance fuzz suite (`docs/testing-strategy.md`).

## Purpose

The RDP is the RCP's fixed-function rasterizer. It consumes a command stream
(from the RSP or the CPU, fed via the DP interface FIFO) and writes pixels into a
framebuffer in RDRAM, running the texture → color-combiner → blender → Z/coverage
pipeline. The **Video Interface (VI)** then scans that framebuffer out to the DAC,
applying anti-aliasing / divot / de-dither filters. RustyN64 emulates both **LLE**
(a faithful per-pixel pipeline, the angrylion / ParaLLEl-RDP reference), not a
triangle-list HLE (ADR 0002).

## Interfaces

```rust
pub trait VideoBus: RdramBus {        // RdramBus: rdram_read/write(_u32)
    fn raise_dp_interrupt(&mut self); // SYNC_FULL / DP-done → MI_INTR.dp
}

pub type Pixel = u32;                 // RGBA8888 output, post-VI-filter

pub struct Rdp {
    pub cmd_start: u32,   // DPC_START
    pub cmd_end: u32,     // DPC_END
    pub cmd_current: u32, // DPC_CURRENT
    pub status: u32,      // DPC_STATUS (FREEZE, START/END-valid, XBUS, ...)
    pub color_image: u32,        // Set Color Image base in RDRAM
    pub color_image_size: u8,    // pixel size code: 1=8b, 2=16b, 3=32b (0=4b)
    pub color_image_format: u8,  // pixel format code (texture-format enum)
    pub color_image_width: u16,  // width in pixels (field + 1)
    pub z_image: u32,            // SET_Z_IMAGE base in RDRAM
    pub fill_color: u32,         // Set Fill Color (FILL-mode colour register)
    pub scissor_ulx: u16,        // Set Scissor, u10.2 upper-left x
    pub scissor_uly: u16,        // .. upper-left y
    pub scissor_lrx: u16,        // .. lower-right x
    pub scissor_lry: u16,        // .. lower-right y
    pub commands_processed: u64, // retired-work tally (decoded commands)
    pub stall: u32,              // GCLK cycles the pipeline is stalled (sync cmds)
}
impl Rdp {
    pub const fn dpc_read(&self, offset: u32) -> u32;      // 0x0410_0000 block
    pub const fn dpc_write(&mut self, offset: u32, v: u32);
    pub fn tick<B: VideoBus>(&mut self, bus: &mut B);      // drain part of DP FIFO
}
```

The DP interface registers (`ref-docs/research-report.md` §2): `DPC_START`/
`DPC_END` bracket a command list in RDRAM (or DMEM); `DPC_CURRENT` advances as the
RDP consumes it; `DPC_STATUS` carries the run/freeze/flush bits. The RDP raises
the DP interrupt when the command buffer drains (`SYNC_FULL`).

**The `DPC_*` register file is implemented** (`Rdp::dpc_read`/`dpc_write`, wired
to `0x0410_0000` by the Bus); the rasterizer behind it is still a stub. It has
**two drivers**: the CPU at `0x0410_0000`, and the RSP microcode's COP0 `c8`–`c15`
(the RSP reports each `MTC0` as `StepResult::dp_write` and `Bus::rsp_tick`
forwards it here — the RSP crate cannot name `Rdp`; see `docs/rsp.md`). The `rdpq`
microcode's `RSPQCmd_RdpAppendBuffer` reaching this file via `mtc0 DP_END` is what
"emits a plausible RDP command list" (Phase 2 criterion 2, T-24-003), witnessed
by `test-harness/tests/microcode.rs::the_microcode_emits_an_rdp_command…`.
Provenance for every rule below is the N64brew wiki, *Reality Display Processor/Interface*
(`n64brew_wiki/markdown/Reality Display Processor/Interface.md`), cross-checked
against n64-systemtest's `RSP STATUS: start-valid` and `RDP START & END REG
(masking)`. The submission is a **double-latch**:

- `DPC_START`/`DPC_END` writes mask to `0x00FF_FFF8` — a 24-bit, 8-aligned RDRAM
  address (*Interface* §DPC_START/§DPC_END, `START[23:0]`/`END[23:0]`).
- Writing `DPC_START` latches the address and sets `START_VALID` (the wiki's
  `START_PENDING`) **only if it was clear** — a second write while valid is
  *ignored*, so software cannot clobber a queued start.
- Writing `DPC_END` latches the end, then branches on `START_VALID` (*Interface*
  §DPC_END): if **set**, it is a fresh transfer, so the pending start is copied
  into `DPC_CURRENT` and `START_VALID` clears; if **clear**, it is an
  *incremental* transfer that continues from `DPC_CURRENT`, which is therefore
  left alone (rewinding it would reprocess already-consumed commands). On
  unfrozen hardware the transfer also runs; while frozen only the latch happens.
- `DPC_STATUS` writes are set/clear **commands** (`SET_FREEZE`=0x8/`CLEAR_FREEZE`
  =0x4, `SET_XBUS`=0x2/`CLEAR_XBUS`=0x1), distinct from the status bits read back
  (*Interface* §DPC_STATUS write layout). `FREEZE` (read bit 1) halts `tick`,
  which is what lets software read and rewrite the registers without the FIFO
  moving.

**Not modelled yet** (all read back as 0, which the frozen `start-valid` case
tolerates, but none are driven): the `SET_FLUSH`/`CLR_FLUSH`,
`CLR_TMEM_BUSY`/`CLR_PIPE_BUSY`, `CLR_CMD_CTR`, and `CLR_CLOCK_CTR` status
commands, and the `END_VALID`/`CMD_BUSY`/`PIPE_BUSY`/`CBUF_READY` read bits.
These need a running transfer to have meaning, so they arrive with the FIFO
drain and the rasterizer — not with this register file.

### The command decoder (T-31-001)

`Rdp::tick` now drains the FIFO: while `DPC_CURRENT < DPC_END` and the DP is not
frozen, it reads the command word at `DPC_CURRENT` from RDRAM, decodes the opcode
(bits 61:56), and advances `DPC_CURRENT` by the command's **full length**. It
consumes one command per scheduler tick, so the FIFO drains gradually rather than
in a burst. Every command is recognised, its length consumed, and a retired-work
counter (`commands_processed`) incremented. Dispatch to a handler currently
covers only the four sync commands (see below); every other opcode is a
recognised no-op until the rasterizer lands.

Two stall conditions keep the decoder from acting on data that is not a valid
command yet:

- **A command is consumed only once it is present in full.** The `rdpq`
  microcode advances `DPC_END` incrementally as it fills the buffer, so `DPC_END`
  can land mid-command; if `DPC_END - DPC_CURRENT` is less than the decoded
  length the decoder stalls, then consumes the command whole once the rest of its
  words arrive. Consuming a partially-written multi-word primitive would decode
  against unwritten RDRAM.
- **`XBUS` stalls the decoder.** When `DPC_STATUS.XBUS` selects DMEM as the
  command source (not yet wired), the decoder does not fall back to reading
  RDRAM — that would treat DMEM-bound parameter data as RDRAM opcodes and desync.

Length rules
(`command::command_len_words`, provenance N64brew *Reality Display
Processor/Commands*):

- Every command is **one 64-bit word** except the two below — including the
  no-operation ranges (`0x00`–`0x07`, `0x10`–`0x23`, `0x31`), so an
  unimplemented or reserved opcode consumes exactly its header and the pointer
  stays aligned.
- **Fill Triangle** (`0x08`–`0x0F`): a 4-word base plus optional coefficient
  blocks. The opcode's low three bits *are* the enable flags — bit 2 shade
  (+8 words), bit 1 texture (+8), bit 0 z-buffer (+2), appended in that order —
  the same bits 58/57/56 the command word also names. So `0x08` is 4 words and
  `0x0F` is 22.
- **Texture Rectangle** / **Flip** (`0x24`/`0x25`): 2 words.

Commands are read from RDRAM (the `XBUS` bit clear); the `XBUS`/DMEM command
source is not yet wired, because the `rdpq` microcode that drives the DP today
DMAs its list to RDRAM. Honouring the DMEM source (per *Edge cases* below)
arrives with a bus seam for DMEM reads.

### The sync commands and the DP interrupt (T-31-002)

The dispatcher (`Rdp::dispatch`, called by `tick` after a command is consumed)
handles the four synchronisation commands; every other opcode is still a
recognised no-op. Provenance is N64brew *…/Commands* §0x26–0x29.

- **`Sync Load`** (0x26), **`Sync Pipe`** (0x27), **`Sync Tile`** (0x28) each
  stall the pipeline for a **fixed, unconditional** number of GCLK cycles — 25,
  50, and 33 respectively (`SYNC_LOAD_GCLK` / `SYNC_PIPE_GCLK` /
  `SYNC_TILE_GCLK`). The stall does not wait on an internal signal: the RDP burns
  the full time whether or not the sync was needed, which is exactly why these
  are constants rather than conditional waits. Modelled by a `stall` countdown
  (one GCLK per `tick`, one `tick` = one RCP/GCLK step) that holds the FIFO until
  it expires. These are documented values, so they live in the code with their
  citation, not in the accuracy ledger (which is for *undocumented* constants).
- **`Sync Full`** (0x29) **raises the DP interrupt** (`bus.raise_dp_interrupt()`
  → `MI_INTR.dp`, asserting IP2 once masked in) — the only part of the command
  implemented. On hardware it also waits for all staged pipeline/memory work and
  halts the pipeline counter; **neither is modelled** (there is no asynchronous
  pipeline work yet, and no pipeline counter), so the interrupt is raised as soon
  as the command is dispatched, after any *preceding* sync stall drains via the
  `stall` gate. The documented hazards — `Sync Full` must be the last command
  before `DP_END`, and no command may be submitted while it is in progress, or
  the RDP hangs — are **not yet enforced**: the FIFO drain does not reproduce the
  hang, so software that violates them will not fault here.

**Measured oracle effect:** the n64-systemtest failing-assertion count is
**unchanged at 93 suite-wide** (917 started) — the same as `v0.3.0`. Sync
dispatch flips no assertion, because every remaining failure needs the RDP
rasteriser (Phase 3) or the cart/PIF path (Phase 5), not sync handling; the
`Sync Full` interrupt has no isolated systemtest that was failing on its absence.
Run: `cargo test -p rustyn64-test-harness --release --test systemtest --
--ignored`.

### The FILL pipeline (T-31-003)

The dispatcher handles the four state/render commands that let it write solid
rectangles into the framebuffer — the simplest of the RDP's pipelines. Provenance
is N64brew *…/Commands* §0x3F/0x37/0x2D/0x36 and *…/Pipeline* §Fill Pipeline.

- **`Set Color Image`** (0x3F) latches the framebuffer base, the pixel `size`
  (0 = 4-bit, 1 = 8-bit, 2 = 16-bit, 3 = 32-bit), the `format`, and the `width`
  (encoded field + 1). The row stride is `width * bytes_per_pixel`.
- **`Set Fill Color`** (0x37) latches the 32-bit FILL colour register.
- **`Set Scissor`** (0x2D) latches the four `u10.2` bounds. The interlace
  `field`/`odd` bits are parsed-away (not modelled).
- **`Fill Rectangle`** (0x36) fills the rectangle ∩ scissor with the fill colour.
  FILL mode "repeats the 32-bit value verbatim out to memory", which resolves per
  pixel by size: **32-bit** writes the whole colour (4 bytes, big-endian);
  **16-bit** writes the upper half for even pixels and the lower half for odd (so
  memory is still the 32-bit value repeated); **8-bit** writes byte `x & 3`.
  Coordinates are `u10.2`; FILL floors the upper-left and rounds the lower-right
  up (a half-open pixel span), and the scissor clips all four edges. A **4-bit**
  color image is not a valid FILL target (it crashes the real RDP), so the fill
  is skipped.

Scope limits, honestly: `Fill Rectangle` implements the **FILL-mode** path only —
the cycle-type gate arrives with `Set Other Modes`, so a 1-/2-cycle rectangle
(which routes through the blender, not the fill register) is not yet distinguished.
The exact sub-pixel edge/rounding rules (inclusive-right/exclusive-lower subtleties
between the rectangle and the scissor) are an **open residual** recorded in
`docs/accuracy-ledger.md` as **R-3**: byte-exact for aligned rectangles (which the
unit tests pin), validated bit-for-bit against Angrylion via the ParaLLEl-RDP fuzz
suite (Sprint 3) and superseded there if it diverges. The floor-upper-left /
ceil-lower-right rule itself is cited (N64brew §Fill Rectangle); only its exact
edge combination with the scissor is unverified.

**Measured oracle effect:** the n64-systemtest failing-assertion count is
**unchanged at 93 suite-wide** (917 started), same as `v0.3.0`. The fill pipeline
flips no assertion on its own: the RDP-category tests verify rendered output,
which needs VI scan-out (T-31-004) and more of the pipeline before a fill becomes
observable to the suite. Measured, not assumed.

## State

Beyond the skeleton FIFO pointers + image bases (the rest is marked TODO):

- **TMEM** — 4 KiB texture memory, up to 8 tiles; with a TLUT (palette) the first
  2 KiB is the lookup table. Formats: RGBA (16/32-bit), IA, I, CI at
  4/8/16/32 bpp (`ref-docs/research-report.md` §4).
- **8 tile descriptors** — format, size, line stride, TMEM address, palette,
  clamp/mirror/wrap + mask/shift per S/T axis.
- **Other-modes** — the big mode word: cycle type, combiner mux selects, blend
  mode, Z-mode, AA/coverage mode, dither selects, alpha-compare.
- **Combiner latches** — the two-stage color/alpha mux input selects.
- **Blender latches** — translucency / fog / AA-edge / dither config.
- **Scissor rectangle** + the fill/primitive/environment/fog/blend colors.

## Behavior

### The pipeline (per primitive)

Per `ref-docs/research-report.md` §4: **triangle/edge setup → span/edge walking →
texture fetch (TMEM) → texture filter → color combiner → blender → Z-test +
coverage write**. The combiner does programmable add/sub/multiply of color/alpha
inputs (texture, shade, primitive, environment, …) across one or two stages; the
blender does translucency, fog, AA-edge blend, and dithering; the Z-buffer
test/writes depth against a Z image in RDRAM.

### Cycle types

The RDP runs in one of four modes (`ref-docs/research-report.md` §4):

| Mode | Use |
| --- | --- |
| **1-cycle** | full pipeline, one combiner/blender pass |
| **2-cycle** | full pipeline, a second combiner/blender pass |
| **copy** | fast rectangle blit (texture → framebuffer, no pipeline) |
| **fill** | fast solid-color fill (clears) |

Per-mode behaviour must be reproduced exactly — copy/fill take shortcuts that
change the output vs running the full pipeline.

### The framebuffer and the 9th bit

RDRAM stores **9 bits per byte**; the hidden 9th bit holds per-pixel **coverage**
(sub-pixel AA) in the color buffer, and hidden Z bits in the Z buffer
(`ref-docs/research-report.md` §4, §8). The VI later uses coverage to blend
silhouette edges. Model the 9th bit as a parallel coverage plane.

### VI registers and scan-out

**The VI register file is implemented** (T-31-004, `rustyn64_core::vi::Vi`, wired
to `0x0440_0000` by the Bus): the sixteen registers `VI_CTRL`…`VI_STAGED_DATA`,
read and written through the CPU bus. All-size stores route through the Bus's
size-blind RCP-internal path (`is_rcp_internal` covers `0x044x_xxxx`), so every
access lands in the register file. One register has a side effect: **writing
`VI_V_CURRENT` acknowledges the VI interrupt** (`MI_INTR.vi = false`). Cold-boot
state is all-zero, so `VI_CTRL.TYPE == 0` and the VI is off.

**The scan position and the VI interrupt are driven by the scheduler**
(`Vi::tick`, called each RCP step): `VI_V_CURRENT` advances one half-line every
`MASTER_HZ / 60 / (VI_V_TOTAL + 1)` master ticks (accumulating the fractional
remainder), wrapping at `VI_V_TOTAL + 1`, and raises `MI_INTR.vi` once per field
when it lands on `VI_V_INTR` — the per-half-line step means a call spanning many
half-lines cannot skip it, and a `VI_V_INTR` beyond the field never fires.
`VI_CTRL.TYPE == 0` suppresses the interrupt, and the position is kept relative so
a mid-run `VI_V_TOTAL` change re-bases without a scale jump. The field cadence is
anchored to nominal 60 Hz NTSC (open residual **R-6**; the exact `H_TOTAL`
sub-field timing, PAL's 50 Hz, and the interlace `VI_V_INTR` bit-0 quirk are
deferred). The VI dot clock (VCLK, ≈48.68 MHz NTSC) is the sole fractional-domain
crystal (`docs/scheduler.md`).

**Still deferred:**

- **Per-register write masks are not applied** — the registers store the full
  32-bit value written (open residual **R-4**); the masks the hardware enforces
  are pinned against n64-systemtest rather than guessed.

**Measured oracle effect:** the committed n64-systemtest runner reports the
suite-wide failing count **unchanged at 93 of 917**, and Phase 1 stays at 0 —
confirming the VI interrupt now firing during a run does not regress the CPU/COP0/
TLB/COP1 categories. No VI-category assertion flips yet: those need the exact
write-masks (R-4) and the sub-field/interlace timing (R-6), both deferred. Run:
`cargo test -p rustyn64-test-harness --release --test systemtest -- --ignored`.

**The scan-out conversion is implemented** (`Bus::scanout`): it reads
`VI_ORIGIN`/`VI_WIDTH`/`VI_CTRL` and the active region from `VI_V_VIDEO`
(`(V_END − V_START)` half-lines → lines), and converts the framebuffer to RGBA8 —
**16-bit RGBA5551** (each 5-bit channel widened to 8 by replicating the high bits,
the 1-bit alpha to 0/255) and **32-bit RGBA8888** (a direct copy). `TYPE` 0/1 is
blank. What is **not** applied yet is the geometry and the analog post-filters —
an open residual (`docs/accuracy-ledger.md` **R-5**):

- **`VI_X_SCALE`/`VI_Y_SCALE` resampling** — the scan is currently 1:1.
- **Anti-aliasing** — blends silhouette edges using the per-pixel coverage bit.
- **Divot filter** — removes 1-pixel AA artifacts on silhouette edges via the
  median of three neighbours.
- **De-dither** — examines 8 neighbours to undo the RDP's ordered ("magic
  square") dither; applied only on full-coverage pixels.

The full scan-out (with scaling and the filters) must be **bit-exact with
Angrylion** — ParaLLEl-RDP reimplemented it to that standard
(`ref-docs/research-report.md` §4); R-5 tracks the gap. `Bus::scanout` has no
per-frame driver yet — the scheduler tick that calls it lands with `V_CURRENT`.

**Oracle effect:** not measured for this change, and it cannot change the count:
`Bus::scanout` is a pure conversion method with **no runtime driver** — nothing in
the run loop calls it during an n64-systemtest run — so it is unreachable by the
suite. The suite-wide failing count therefore stands at 93 (from T-31-004 pt 1's
measurement). The scan-out is graded instead by the harness golden frame
(T-31-005) and, for the deferred scaling/filters, the ParaLLEl-RDP fuzz suite
(R-5).

## Edge cases and gotchas

- **"Serial C gets you nowhere on a GPU."** ParaLLEl-RDP uses tile-based binning,
  ubershaders, and imports RDRAM as an SSBO. RustyN64's *reference* is a
  pure-Rust **software** RDP (the angrylion analog) first; a wgpu-compute backend
  is a later, *validated-against-the-reference* optional path — not the other way
  round (`ref-docs/research-report.md` §4, §Architecture options B;
  `docs/performance.md`).
- **Shared-RDRAM coherency is the hardest part.** CPU/RSP can read pixels the RDP
  just wrote (framebuffer effects); HLE plugins fudge this with heuristics, LLE
  must get it right because the RDP, CPU, and RSP share one RDRAM on one timeline
  (`ref-docs/research-report.md` §4, §challenge 3; `docs/scheduler.md`).
- **Coverage AA is in the 9th bit.** Dropping it loses edge AA and breaks the VI
  divot/de-dither stages downstream.
- **Ordered dither is a specific pattern.** The de-dither filter is tuned for the
  RDP's "magic square" dither — both must match.
- **Copy/fill skip the pipeline.** Don't route fill-mode through the combiner;
  the bit-exact output differs.
- **The DP command list can live in DMEM or RDRAM** — `DPC_STATUS` selects the
  source; honour both.

## Test plan

- **ParaLLEl-RDP conformance fuzz suite (~150 tests)** — generates RDP command
  streams and compares fixed-point outputs; "to pass we must get an exact match"
  (`ref-docs/research-report.md` §4, §7). This is the RDP gate.
- **PeterLemon RDP demos** — the de-facto visual/behavioural reference for many
  edge cases (`ref-docs/research-report.md` §7).
- **Per-mode unit vectors** — 1-/2-cycle/copy/fill outputs; combiner mux
  permutations; blend modes; Z-test boundaries; coverage/AA on a known triangle.
- **VI golden frames** — AA / divot / de-dither against an Angrylion reference
  scan-out; the visual golden corpus (`docs/testing-strategy.md`).

## Open questions

- **Backend ordering** — confirm the software RDP can hit interactive speed at
  native res, or whether the wgpu-compute backend must come sooner
  (`ref-docs/research-report.md` §Open questions 3; `docs/performance.md`).
- **How much of the RDRAM-coherency model commercial games actually need** vs
  what the fuzz suite alone gates.
