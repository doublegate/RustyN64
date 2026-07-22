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
    pub color_image: u32, // SET_COLOR_IMAGE base in RDRAM
    pub z_image: u32,     // SET_Z_IMAGE base in RDRAM
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

### VI scan-out

The VI reads the framebuffer at `VI_ORIGIN` in the format `VI_CONTROL`/
`VI_STATUS` describe (bpp, AA mode, gamma, dither, divot enable), scales it,
applies post-filters, and streams to the DAC (`ref-docs/research-report.md` §4):

- **Anti-aliasing** — blends silhouette edges using the per-pixel coverage bit.
- **Divot filter** — removes 1-pixel AA artifacts on silhouette edges via the
  median of three neighbours.
- **De-dither** — examines 8 neighbours to undo the RDP's ordered ("magic
  square") dither; applied only on full-coverage pixels.
- Output is 320×240 or 640×480 (NTSC), up to 32-bit color.

The VI must be **bit-exact with Angrylion** too — ParaLLEl-RDP reimplemented it to
that standard (`ref-docs/research-report.md` §4).

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
