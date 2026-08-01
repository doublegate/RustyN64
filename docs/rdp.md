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
    pub fill_color: u32,         // Set Fill Color (FILL-mode color register)
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
to `0x0410_0000` by the Bus); the rasterizer behind it is **implemented** (Phase 3 —
texture / combiner / blender / coverage pipeline with VI scan-out, bit-matching
Angrylion across 164 conformance vectors). It has
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

**Not modeled yet** (all read back as 0, which the frozen `start-valid` case
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
in a burst. Every command is recognized, its length consumed, and a retired-work
counter (`commands_processed`) incremented. Dispatch to a handler currently
covers only the four sync commands (see below); every other opcode is a
recognized no-op until the rasterizer lands.

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
DMAs its list to RDRAM. Honoring the DMEM source (per *Edge cases* below)
arrives with a bus seam for DMEM reads.

### The sync commands and the DP interrupt (T-31-002)

The dispatcher (`Rdp::dispatch`, called by `tick` after a command is consumed)
handles the four synchronization commands; every other opcode is still a
recognized no-op. Provenance is N64brew *…/Commands* §0x26–0x29.

- **`Sync Load`** (0x26), **`Sync Pipe`** (0x27), **`Sync Tile`** (0x28) each
  stall the pipeline for a **fixed, unconditional** number of GCLK cycles — 25,
  50, and 33 respectively (`SYNC_LOAD_GCLK` / `SYNC_PIPE_GCLK` /
  `SYNC_TILE_GCLK`). The stall does not wait on an internal signal: the RDP burns
  the full time whether or not the sync was needed, which is exactly why these
  are constants rather than conditional waits. Modeled by a `stall` countdown
  (one GCLK per `tick`, one `tick` = one RCP/GCLK step) that holds the FIFO until
  it expires. These are documented values, so they live in the code with their
  citation, not in the accuracy ledger (which is for *undocumented* constants).
- **`Sync Full`** (0x29) **raises the DP interrupt** (`bus.raise_dp_interrupt()`
  → `MI_INTR.dp`, asserting IP2 once masked in) — the only part of the command
  implemented. On hardware it also waits for all staged pipeline/memory work and
  halts the pipeline counter; **neither is modeled** (there is no asynchronous
  pipeline work yet, and no pipeline counter), so the interrupt is raised as soon
  as the command is dispatched, after any *preceding* sync stall drains via the
  `stall` gate. The documented hazards — `Sync Full` must be the last command
  before `DP_END`, and no command may be submitted while it is in progress, or
  the RDP hangs — are **not yet enforced**: the FIFO drain does not reproduce the
  hang, so software that violates them will not fault here.

**Measured oracle effect:** the n64-systemtest failing-assertion count is
**unchanged at 93 suite-wide** (917 started) — the same as `v0.3.0`. Sync
dispatch flips no assertion, because every remaining failure needs the RDP
rasterizer (Phase 3) or the cart/PIF path (Phase 5), not sync handling; the
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
- **`Set Fill Color`** (0x37) latches the 32-bit FILL color register.
- **`Set Scissor`** (0x2D) latches the four `u10.2` bounds. The interlace
  `field`/`odd` bits are parsed-away (not modeled).
- **`Fill Rectangle`** (0x36) fills the rectangle ∩ scissor with the fill color.
  FILL mode "repeats the 32-bit value verbatim out to memory", which resolves per
  pixel by size: **32-bit** writes the whole color (4 bytes, big-endian);
  **16-bit** writes the upper half for even pixels and the lower half for odd (so
  memory is still the 32-bit value repeated); **8-bit** writes byte `x & 3`.
  Coordinates are `u10.2`; FILL floors the upper-left and draws through the pixel
  that **contains** the lower-right coordinate (inclusive), with the FILL/COPY
  `yl | 3` rule forcing the last scanline to fill whole. The **scissor** clip is
  **asymmetric**: its X lower-right is **inclusive** of the boundary pixel (but a
  rectangle entirely at or past the scissor's right edge draws nothing), while its Y
  lower-right is **exclusive**. A **4-bit** color image is not a valid FILL target
  (it crashes the real RDP), so the fill is skipped.

**The fill register is read only in FILL and COPY mode.** In 1-/2-cycle mode a
rectangle is an ordinary primitive and goes through the **combiner** (then
alpha-compare and dither, exactly as the triangle path does); it carries no shade
or texture block, so the combiner sees only its register inputs (prim/env/…) and
the fill register is never consulted. Oracle-confirmed (**R-21**): vector
`fill_rect_1cycle_16` renders a 1-cycle rectangle whose prim color and fill
register are deliberately different, and Angrylion produces the **prim** color.

Scope limits, honestly: the same question is **still open for a flat
`Fill Triangle`** (0x08 with no shade/texture block), which selects the combiner on
`shade.is_some() || tex.is_some()` rather than on the cycle type and so still takes
the fill register unconditionally. No vector exercises it yet (**R-21**).
The **integer-coordinate** edge rules — the rectangle's inclusive lower-right +
`yl | 3` (**R-3**) and the scissor's asymmetric inclusive-X / exclusive-Y clip with
the `allover` guard (**R-15**) — are **oracle-validated** against Angrylion by the
seeded-fuzz corpus (`tests/vectors/fuzz/`, 48 FILL rectangles + 48 scissor-clip
rectangles) and mutation-checked unit tests; the fuzz gate is what caught both the
half-open rectangle off-by-one and the scissor asymmetry (`docs/accuracy-ledger.md`).
What remains open: **sub-pixel** (fractional-coordinate) rect/scissor edges, which
the whole-pixel fuzz does not exercise.

**Measured oracle effect (as of `v0.3.0`, when this landed):** the n64-systemtest
failing-assertion count was **unchanged at 93 suite-wide** (917 started). The fill
pipeline flips no assertion on its own: the RDP-category tests verify rendered
output, which needs VI scan-out (T-31-004) and more of the pipeline before a fill
becomes observable to the suite. Measured, not assumed.

That 93 is a **historical** reading, deliberately kept rather than overwritten —
it is what this change measured at the time. The suite-wide count has since fallen
to **90** with the Phase-5 cart/PIF/SI work, and `docs/STATUS.md` is the single
source of truth for the current number.

### The texture-state commands (T-32-001)

The RDP gains its texture state — a 4 KiB TMEM and eight tile descriptors — and the
three commands that describe it without moving any texels. Provenance is N64brew
*…/Commands* §0x3D/0x35/0x32.

- **`Set Texture Image`** (0x3D) latches the RDRAM source for subsequent loads:
  `format` (55:53), `size` (52:51), `width` (41:32, field + 1 pixels), and
  `dramAddress` (23:0) — the same field layout as `Set Color Image`. The wiki notes
  the texture-image `format` has no effect on any operation (only the tile format
  matters); it is stored for completeness.
- **`Set Tile`** (0x35) decodes the descriptor at `index` (26:24): `format` (55:53),
  `size` (52:51), `line` (49:41, row stride in 64-bit TMEM words), `tmem_addr`
  (40:32, base in 64-bit words — word 0x100 = byte 0x800), `palette` (23:20, the
  high half of the TLUT address for CI4 only), and per-axis `clamp`/`mirror`/`mask`/
  `shift` with **T in bits 19:10** and **S in bits 9:0**. It preserves the tile-size
  coordinates, which are a disjoint part of the same descriptor.
- **`Set Tile Size`** (0x32) latches the clamp/mask/mirror extents for the descriptor
  at `index`: upper-left `SL`/`TL` (55:44 / 43:32) and lower-right `SH`/`TH`
  (23:12 / 11:0), all `u10.2`.

**TMEM is lazily allocated.** The 4 KiB buffer is an `Option<Box<[u8; 4096]>>` that
starts `None` (read as all-zero) and is allocated on the first write. This keeps
`Rdp`'s `Default` cheap, which matters because `Bus::rdp_tick` does a
`core::mem::take` every RCP step — a `None` placeholder swaps in without a 4 KiB
allocation or copy, while the real TMEM box moves by pointer. TMEM byte addresses
mask into the 4 KiB space.

Scope limits, honestly: this ticket is **pure state** — no texel is loaded (that is
`Load Block`/`Load Tile`/`Load TLUT`, T-32-002/003) and no pixel is sampled (the
sampler + `Texture Rectangle`, T-32-004). The oracle count stays **93** because
nothing rendered changes.

### The TMEM loads (T-32-002)

`Load Tile` (0x34) and `Load Block` (0x33) move texels from the current texture image
in RDRAM into the tile's TMEM region. The address arithmetic and the swizzle are
cross-verified against the N64brew wiki (*…/Commands*) and the ParaLLEl-RDP reference
(MIT — its read-side `texture.h` is the authoritative byte-placement statement).

- **`Load Tile`** copies a rectangle. `SL/TL/SH/TH` are `u10.2`; the `.2` fraction is
  floored and the span is **inclusive** (`SH − SL + 1` texels per row). The source row
  stride is the texture image `width`; the destination row stride is the tile's `line`
  (in 64-bit TMEM words). It updates the descriptor's tile size for rendering.
- **`Load Block`** streams a linear run. `SL/SH` are `u12.0`, `SH − SL + 1` is the count
  (inclusive), and a count over **2048** ([`LOAD_BLOCK_MAX_TEXELS`]) writes nothing. The
  low field is **`dxt`** (`u1.11`): the line index `(word · dxt) >> 11` over each 64-bit
  TMEM word decides parity.

**The swizzle** (matched to the sampler's read layout):

- **Odd-row 32-bit-word swap** — on an odd row (Load Tile) or odd dxt line (Load Block),
  the two 32-bit halves of the 64-bit TMEM word swap: `dst ^= 4` on the byte address.
- **32-bit RGBA split** (Load Tile) — R,G go to the low half of TMEM and B,A to the high
  half (offset 0x800), stepping two bytes per texel and masking to `0x7FF`. This is the
  wiki's "32-bit texels have a different TMEM layout".
- TMEM is allocated on the first write (the lazy `Option<Box<..>>`) via a shared
  `tmem_write` helper; loads past the 4 KiB end wrap to the start. A degenerate or
  inverted range (`SH < SL` or `TH < TL`) writes nothing, like every other
  unsupported path, rather than iterating a wrapped bogus width.

Scope (**open residual R-7**): `Load Tile` covers 8/16/32-bit texels and `Load Block`
covers 8/16-bit. There is **no 4-bit texel *load*** — a 4-bit texture-image load is
invalid on hardware (it crashes the RDP pipeline). Games load 4-bit textures by lying
about the format: an 8-bit texture image + 8-bit LOAD tile loads the packed bytes raw,
then a **separate 4-bit render tile** extracts nibbles at fetch. That canonical path is
**oracle-validated** (`tex_tri_i4_16` matches Angrylion byte-for-byte). Still deferred:
the **32-bit `Load Block` split** and a *direct* 4-bit LOAD tile with an 8-bit texture
image (the `ti_size`-vs-`tile.size` load granularity); an unsupported size writes nothing.
The supported paths are byte-exact against hand-computed expectations (five unit tests).

### The sampler and copy-mode Texture Rectangle (T-32-004)

The first **textured picture**: `Texture Rectangle` (0x24) blits a tile into the color
image in copy mode, closing the Sprint-2 texture path. This is the first **two-word**
command — `tick` now captures the command's RDRAM base address (before advancing the FIFO
pointer) and passes it to `dispatch`, so a handler can read its later words
(`bus.rdram_read_u32(cmd_base + 8)`).

- **The coordinate wrap** (`wrap_coord`) turns a raw `s10.5` texture coordinate into a
  tile-relative integer texel: clamp to `i16`, **shift** (codes 1–10 right, 11–15 left by
  `16−code`), subtract the tile origin `SL`, take the integer part (`>>5`), then **mirror**
  on alternate mask-sized spans and **mask** to `mask` bits (`mask == 0` = no wrap). Copy
  mode omits the clamp step. Matched to the ParaLLEl-RDP `texture.h` order.
- **Copy-mode Texture Rectangle** rasterizes the screen rectangle (lower-right inclusive),
  stepping `T` down Y and copying **4 pixels per cycle** across X: each cycle reads a 64-bit
  TMEM word (4 consecutive 16-bit texels) and writes them to 4 output pixels. So the base
  texel is evaluated at each cycle's first column (`base = wrap(s_start + (DsDx·cycle_col >>
  (5 + dx_shift)))`, `dx_shift = 2` for 16-bit), advancing `DsDx × 4` texels per cycle, and
  the within-cycle offset is a direct `+0..3` TMEM increment — **not** a per-pixel step. A 1:1
  blit (`DsDx = 4.0`) reduces to `s = col`; a non-1:1 blit (e.g. `DsDx = 2.0`) reads texels
  `0,1,2,3,2,3,4,5`, not the naive `0,0,1,1,2,2,3,3`. The raw 16-bit texel is copied verbatim
  into the color image, clipped to the scissor.

Provenance: the command encoding, copy pipeline, and wrap order are cross-verified against
the N64brew wiki and ParaLLEl-RDP (MIT). Validated by a **round-trip identity** test — a
`Load Tile` texture blitted back by `Texture Rectangle` reproduces the source byte-for-byte
(load and fetch share the odd-row swap) — a `wrap_coord` unit test, and the `tex_rect_copy_16`
/ `_offset_16` / `_8x8_16` (1:1) and `tex_rect_mag_16` (non-1:1 `DsDx = 2.0`) conformance
vectors against Angrylion.

Scope (**open residual R-8**): the 16-bit tile → 16-bit color image path is wired, including
the 4-pixels-per-cycle non-1:1 selection. `Texture Rectangle Flip` (0x25), the 8/32-bit and TLUT
copy paths, and the copy alpha-compare are deferred to the Sprint-3 fuzz; an
unsupported configuration draws nothing. The oracle count stays **93** — the n64-systemtest
categories that exercise rendered output need the full 1-/2-cycle pipeline (Sprint 3).

### Load TLUT and the texel-format decoders (T-32-003)

`Load TLUT` (0x30) and `Rdp::fetch_texel` — the palette load and the fetch half of the
texture pipeline (the clamp/mirror/filter/combiner is T-32-004 / Sprint 3). Decode is
matched to the ParaLLEl-RDP read layout (`texture.h`, MIT).

- **`Load TLUT`** quadruples each 16-bit texture-image entry into four adjacent TMEM `u16`
  slots — entry `i` at byte `tmem_addr*8 + i*8` — for an inclusive `(SH>>2) − (SL>>2) + 1`
  count, and latches the tile size. The base is written wherever `tmem_addr` points: the
  "upper half, 128-byte aligned" rule is a **programmer requirement**, not a hardware
  rejection (the sampler reads the palette from the upper half, so a misplaced TLUT is
  simply not found). Enforcing a rejection would invent behavior, so it is not done.
- **`fetch_texel(tile, s, t) -> [u8; 4]`** decodes RGBA16 (5551, 5→8 replication),
  RGBA32 (from the split TMEM: R,G low half, B,A high half), IA16/IA8/IA4, I8/I4 (alpha =
  intensity), and CI8/CI4 through the TLUT (CI4 folds `tile.palette` in as the high nibble
  of the index). The 4-bit formats select the high nibble for even `s`, the low for odd.
- **The palette lookup is gated on `Set Other Modes.tlut_en` (bit 47), not on the tile's
  format** (N64brew *…/Commands* §0x2F). A CI tile with `tlut_en` clear is **not**
  palette-mapped and renders black — pinned by `ci4_tlut_disabled_16`, which is
  byte-identical to `tex_tri_ci4_tlut_16` apart from that one bit and whose golden is all
  black where the other renders the full palette.

  Two limits are recorded rather than implied away. A **non-CI** tile with `tlut_en` **set**
  is still not palette-mapped, though hardware would sample it through the TLUT: no vector
  covers it and the RGBA/IA/I formats index the palette differently enough that deriving it
  from prose would be invention. And `tlut_type` (bit 46) is decoded but **IA16 palettes are
  deferred** — the lookup assumes RGBA16. Both stay wrong-but-known until a vector defines
  them.

**The read convention matches the loads.** TMEM is a natural big-endian byte array, so the
sampler applies only the odd-row 32-bit-word swap `^= (t & 1) << 2` — the endian twiddles
ParaLLEl-RDP applies to its host-word storage are intentionally absent on both the load and
fetch sides. **YUV16** decode is deferred (no oracle test needs it this sprint). There is
**no 4-bit texel *load*** (a 4-bit texture-image load is invalid on hardware; 4-bit textures
load as 8-bit and render with a separate 4-bit tile — validated against Angrylion by
`tex_tri_i4_16`, R-7); only the 32-bit `Load Block` split and a direct 4-bit LOAD tile with an
8-bit texture image are still deferred. 4-bit *fetch* is done. The
oracle count stays **93** — `fetch_texel` now has runtime callers (the texture rectangle,
T-32-004, and the textured triangle, T-33-004 2b-texture), but no systemtest drives the render path.

### The flat-fill triangle rasterizer (T-33-001)

The first Sprint-3 ticket and the foundation every later per-pixel ticket renders through:
the edge-walked triangle. `Fill Triangle` (0x08) and its shade/texture/Z variants
(0x09–0x0F) are decoded and rasterized, cross-verified against the N64brew wiki and the
ParaLLEl-RDP reference (MIT, `interpolate_x`).

- **Decode.** `yh/ym/yl` are `s11.2` (four sub-scanlines per pixel); the three edge base
  X's (`xh/xm/xl`) are `s11.16` and their slopes (`dxhdy/dxmdy/dxldy`) `s13.16`, read from the
  command's words 1–3 via the multi-word `cmd_base` seam. The `lmajor`/flip bit (55) selects
  the fill direction. The opcode's low three bits are `shade:texture:zbuffer` (shade = bit 58,
  texture = bit 57, zbuffer = bit 56), appending +8/+8/+2 coefficient words that `tick`
  already length-consumes.
- **Edge-walk.** For each scanline's four sub-scanlines, the edge X is
  `x0 + (y − yh_base) * slope`; the major edge `H` (yh→yl) provides one span bound and the
  active minor edge (`M` above `ym`, `L` below) the other, `flip` deciding which is left. The
  span is reduced to whole pixels (`>> 16`), scissor-clipped, and filled with the FILL-mode
  color (via the shared `fill_pixel`, the same write as `Fill Rectangle`).

Scope (**open residual R-9**): this is a **flat fill** in FILL cycle mode — the sub-pixel
coverage (ParaLLEl-RDP's `quantize_x` sticky-bit edge and the `do_offset` latch) and the
shade/texture/Z attribute interpolation are deferred; the 0x09–0x0F variants fill flat, their
coefficient words length-consumed only. The combiner (T-33-002), blender (T-33-003), and
Z/coverage (T-33-004) then color the triangle; the whole is graded bit-exact against the
ParaLLEl-RDP conformance vectors (T-33-005). Validated here by a right-triangle golden pinning
the edge-walk and the fixed-point decode. Oracle unchanged at **93**.

### The color combiner (T-33-002)

`Set Combine Mode` (0x3C) and the `(A − B) * C + D` evaluation — the per-pixel color mux,
cross-verified against the N64brew wiki and ParaLLEl-RDP (MIT, `combiner.h`).

- **Decode.** The single command word packs 16 input selects — RGB and alpha `A/B/C/D` for both
  cycles — into `CombineMode`. `Set Prim Color` (0x3A) and `Set Env Color` (0x3B) latch the two
  constant-color registers the combiner can select.
- **The equation.** Per channel, `(A − B) * C + D` with the RDP's fixed-point rules: `A/B/D` go
  through the asymmetric 9-bit `special_expand` (subtract the `0x80` bias, sign-extend to 9 bits,
  add it back), `C` is a plain 9-bit value, a `+0x80` rounding bias is applied **before** the
  `>> 8`, and `D` is added afterwards unscaled; the result is clamped with the 9-bit fold (which
  is why 256–383 saturate and 384–511 wrap). The "one" input is `0x100`, not `0xFF`.
- **Cycles.** 1-cycle mode uses only cycle 1's selects; 2-cycle mode evaluates cycle 0 (no
  inter-cycle clamp) and feeds its output as cycle 1's `Combined` input.

Scope (**partially-resolved residual R-10**): the common inputs (combined, texel0/1, primitive,
shade, environment, one, zero, and the C-slot alpha taps) are wired, and so are the
**register-sourced exotic** inputs — `PRIM_LOD_FRAC` (RGB mul-select 14, alpha mul-select 6, from
`Set Prim Color`) and the `Set Convert` constants `K4` (RGB sub-B select 7) and `K5` (RGB
mul-select 15), each validated byte-for-byte against Angrylion (`tex_tri_primlodfrac_16`,
`tex_tri_convert_k45_16`, and `tex_tri_convert_kneg_16` — the last a negative `K4 = −64`
proving K4/K5 are stored raw `0..511` and sign-extended in the combiner, not at decode). The
**chroma-key center/scale** (`Set Key GB`/`Set Key R` → RGB sub-B / mul select 6) are likewise
wired, validated by unit tests (decode + mux routing) and the `tex_tri_chromakey_16` conformance
vector byte-for-byte against Angrylion. The **chroma-key alpha compare** (`key_en`, Set Other Modes bit 40) is also wired: the combiner
outputs the sub-A chromabypass color and derives the pixel alpha from `chroma_key_min` over the
17-bit combined color + the `Set Key` widths (gated on `key_en`, common path byte-identical;
validated by `tex_tri_chromakey_alpha_16`). The remaining exotic inputs — **noise** (un-oracled),
the derivative-computed **LOD fraction**, and the **YUV convert `K0`–`K3`** — still read as zero
until the LOD/noise/YUV state lands. The arithmetic, the 16-field decode, the mux, and the 2-cycle
chaining are unit-tested against hand-computed values. `combine` now has its runtime caller — `combined_color` routes the
interpolated shade and sampled texel through it per pixel (T-33-004 2b) — but no systemtest drives
the render path, so the oracle stays **93**.

### The blender (T-33-003)

`Set Other Modes` (0x2F) and the divide-free blend `(P * a0 + M * (a1 + 1)) >> 5` — the per-pixel
translucency/fog stage that follows the combiner, cross-verified against the N64brew wiki and
ParaLLEl-RDP (MIT, `shaders/blender.h`).

- **Decode.** The single command word carries the render mode: the cycle type (bits 53:52), the
  two blender cycles' `P/A/M/B` selects (bits 31:16, MSB-first, 2 bits each), `force_blend`, the
  Z-test/update enables and Z-mode, the coverage-destination mode, `image_read_en`, and the
  alpha-compare enable — all decoded into `OtherModes` so nothing silently reads as its default,
  even though the blend equation consumes only the subset below today. `Set Blend Color` (0x39)
  and `Set Fog Color` (0x38) latch the two color registers the blender can select.
- **The equation.** Per channel, `P * a0 + M * (a1 + 1)` then `>> 5`, where `P`/`M` select an RGB
  triple (pixel/memory/blend/fog) and `a0 = A >> 3`, `a1 = B >> 3` map the 8-bit alpha selects to
  the 5-bit blend weights. The `+ 1` on the `M` term is real hardware. This is the divide-free
  form the RDP uses for every non-anti-aliased-edge pixel.
- **Cycles.** 1-cycle mode evaluates blend cycle 0 alone; 2-cycle mode feeds cycle 0's RGB back
  as the pixel color into cycle 1 (the alpha selects are unchanged between cycles).

- **Runtime wiring (T-33-004 PR-B 2b-blend).** `depth_span` now gives the blender its first
  runtime caller: for a shaded/textured triangle it reads the destination framebuffer pixel
  (`read_pixel`, the inverse of `write_pixel` for RGBA8888 and RGBA5551) and routes the combiner
  color through `blend` **when the depth test enabled blending** — which, until per-pixel coverage
  exists, means `force_blend` is set. This mirrors the reference blender's `!blend_en` fast-path:
  an opaque pixel keeps the combiner color and only a translucent (later, AA-edge) pixel blends
  with memory. A translucent-triangle integration test proves a 50/50 blend of red over a green
  background reaches `0x7F7F00` (plain red would mean the memory read never happened).

**Ordered RGB dither is now wired** (T-33-004 2c): after the combiner (and blender), each pixel's RGB
is dithered by the RDP's 4×4 ordered matrix — magic (`Set Other Modes` RGB dither mode 0, the
hardware default), bayer (mode 1), or off (mode 3, "constant 7"). `apply_rgb_dither` is a bit-exact
port of Angrylion `dither.c` `rgb_dither`: a channel rounds up to the next 5-bit level
(`(c & 0xf8) + 8`, saturating at 255) exactly where the matrix cell is below the channel's low 3
bits. It runs on both the no-Z and depth pixel paths in 1-/2-cycle mode (FILL/COPY bypass the
combiner and do not dither), and is validated byte-for-byte against Angrylion by the `dither_tri_32`
conformance vector. Noise dither (mode 2) reads the magic cell for now (**R-10** — no noise source).

Scope (**open residual R-11 / R-9**): the anti-aliased-edge divider LUT, the memory-alpha
interpenetrating-Z blend-shift path, the `color_on_cvg` divide interaction,
and the coverage write-back remain decoded-but-unused — they need the sub-pixel coverage
accumulator (slice 2c). The decode, the no-divide equation, the input muxes, the 2-cycle chaining,
and now the memory-read wiring are unit/integration-tested against hand-computed values; the
oracle stays **93** (no systemtest drives the render path).

### The Z-buffer machinery (T-33-004, PR-A)

The depth codec, the per-pixel depth test, and the depth-source commands — the pure, oracle-verified
pieces of Z-buffering, ahead of wiring them into the pixel pipeline. Cross-verified against
ParaLLEl-RDP (MIT, `z_encode.h`, `depth_test.h`).

- **The Z codec.** The N64 Z buffer uses an inverted floating-point encoding (more precision near the
  far plane): a 14-bit stored value ↔ an 18-bit UNORM. `z_decompress`/`z_compress` are exact inverses
  of `z_encode.h` (`exponent` in bits 13:11, `mantissa` in 10:0; `base = 0x40000 − (0x40000 >> exp)`);
  `dz` is stored as a 4-bit `log2` (`dz_decompress = 1 << n`, `dz_compress` an integer `log2` correct
  for powers of two — the hardware's cheap `log2`).
- **The depth test.** `depth_test` is a faithful port of `depth_test.h`: given the pixel's `z`/`dz` and
  the Z-buffer read (`DepthInputs`), it returns whether the pixel is written plus the blend/coverage
  state (`DepthResult`). All four Z modes are modeled — **opaque** (nearer-passes, with a coplanar
  same-surface coverage-increment path), **interpenetrating** (a decal-like intersect that *reduces*
  coverage), **transparent** (strictly-in-front), and **decal** (coplanar only) — including the
  stored-`dz` coplanar/precision-factor handling. Unit-tested by observable occluding-vs-occluded pairs
  per mode.
- **Depth-source commands.** `Set Depth Image` (0x3E) latches the Z-buffer base; `Set Primitive Depth`
  (0x2E) latches the `z`/`dz` used when `Set Other Modes` `z_source_sel` selects primitive depth (the
  only depth source for rectangle commands).

### The Z-buffer storage (T-33-004, PR-B part 1)

The Z-buffer read/write and the RDRAM **hidden ("9th") bits** those entries need. Each Z pixel is 18
bits: `zbuffer_write` compresses the depth (`z_compress`), packs the 14-bit result into bits 15:2 of the
16-bit halfword with `dz`'s **high** two bits in 1:0, and stores `dz`'s **low** two bits in the hidden
bits; `zbuffer_read` reverses it. Byte-exact against ParaLLEl-RDP's `store_vram_depth`/`load_vram_depth`.

- **The hidden bits.** RDRAM carries a 9th bit per byte (see *Behavior*, above). `RdramBus` gains
  `rdram_read_hidden`/`rdram_write_hidden` (default no-op, so non-Z impls are unaffected); the Bus backs
  them with a lazily-allocated array (one 2-bit value per 16-bit halfword), so only Z-buffered rendering
  pays for it. Validated by a Bus round-trip test and a full-`dz` Z-buffer round-trip (a `dz` whose low
  bits only survive via the hidden path).

### The first depth-tested triangle (T-33-004, PR-B part 2a)

The first per-pixel pipeline: `Fill Z-Buffered Triangle` (opcode bit 56) decodes the z-coefficient
suffix (`z`/`dzdx`/`dzde`, `s15.16`) via `decode_triangle_z`, and — when `Set Other Modes` enables the
depth test or update — `depth_span` runs the real per-pixel path instead of the flat fill.

- **Interpolate.** `interpolate_z` computes each pixel's 18-bit depth from the z-coefficients and the
  major-edge x — a faithful port of ParaLLEl-RDP's `interpolate_z` snap (`interpolation.h`) for the
  full-coverage, `do_offset == false` case (sub-pixel snapping is R-9).
- **Test and write.** `depth_test` compares the interpolated depth against the Z-buffer entry
  (`zbuffer_read`); only passing pixels write color, and `zbuffer_write` stores the new depth when
  `z_update` is set. This is `depth_test`/`zbuffer_*`'s first runtime caller.

Validated by an occluding-triangles test (a nearer triangle draws, a farther one is rejected, a
nearer-still one overwrites — both accept and reject paths) and a hand-computed `interpolate_z` test.

Scope: coverage is full (sub-pixel edge coverage is part 2c); the `dz` derivation is a first cut. The
oracle stays **93** (no systemtest ROM drives rendering yet).

### The first shaded triangle (T-33-004, PR-B part 2b)

`Fill Shaded Triangle` (opcode bit 58) now colors each pixel from the **combiner** fed the interpolated
shade, not the FILL register — the combiner's first runtime caller.

- **Decode.** `decode_shade` reads the 8-word shade block (RGBA base + per-x `dx` and per-major-edge `de`
  deltas, `s15.16`; the base's int part is 9-bit signed, the deltas' 16-bit) into `ShadeSetup`.
- **Interpolate and combine.** `interpolate_shade` (a port of ParaLLEl-RDP's `interpolate_rgba` snap)
  gives the per-pixel RGBA; `shaded_color` runs it through `Rdp::combine` with the prim/env registers,
  and `write_pixel` packs the result to the color image (RGBA8888 direct, RGBA5551 for 16-bit).

This applies standalone and combined with the depth test. Validated by a hand-computed
`decode_shade`/`interpolate_shade` test and a shaded-triangle test that renders the combiner output
(not the FILL color). **This closes the R-9 flat-fill for shaded triangles.** The oracle stays **93**.

### The first textured triangle (T-33-004, PR-B part 2b-texture)

`Fill Textured Triangle` (opcode bit 57) samples a tile per pixel into the combiner's `texel0`.

- **Decode.** `decode_texture` reads the 8-word texture block (`S`/`T` base + per-x/per-major-edge
  deltas, `s16.16`; `W` is the deferred perspective term) into `TexSetup`.
- **Sample and combine.** `interpolate_st` gives the per-pixel texture coordinate
  (perspective-correct when `persp_tex_en` is set — see *Perspective-correct texturing* below);
  `combined_color` samples the command's **base tile** via `sample_texel` (the tile
  shift/clamp/mask transform and the 3-point filter, then `fetch_texel`) and runs the combiner
  (with any shade). The base tile is the triangle command's `tile[2:0]` at **bits 50:48** (N64brew
  *Reality Display Processor / Commands*, the Edge Coefficients word-0 field table). Works
  standalone and with shade/depth.

Validated by a textured-triangle test that samples a loaded RGBA16 texel through a texel-passthrough
combiner.

**Perspective-correct texturing.** When `Set Other Modes` `persp_tex_en` (bit 51) is set, `interpolate_st`
interpolates `S`/`T`/`W` and runs the hardware perspective divide — a faithful port of ParaLLEl-RDP's
`perspective_divide` (the 64-entry reciprocal LUT, the normalization shift, the out-of-bounds
saturation, the `w <= 0` carry, the 17-bit clamp), validated by a hand-computed `perspective_divide`
test. The **tile coordinate transform** (shift → tile-origin subtraction → clamp → mask/mirror) is now
applied to the raw `s10.5` coordinate before the fetch (`sample_coord`, the ParaLLEl-RDP sampler order:
clamp active when `clamp_s || mask_s == 0`, over-`SH` clamps to `(SH>>2)−(SL>>2)`, clamp *before* mask),
validated against Angrylion by `tex_tri_clamp_16` and `tex_tri_wrap_16`. The N64's **3-point
bilinear** filter (`sample_type = 1`) is now modeled too (`bilinear_3point`: four texels blended
by `upper = (sfrac+tfrac) & 0x20`, the lower/upper triangle each a `+0x10 >> 5` round; the fraction
is zeroed when the coordinate clamps), validated by `tex_tri_bilinear_16`. The **mask-wrap seam** is handled too (`mask_coupled`: the
bilinear neighbor is `base + sdiff`/`tdiff` — `+1` / `0` at a seam / `-1` mirrored / wrap-to-0 —
not a bare `+1`; validated by `tex_tri_bilinear_wrap_16`). **2-cycle mode** samples a second texel from `base_tile + 1`
and swaps `texel0`/`texel1` before cycle 1 (validated by `tex_tri_2cycle_16`). The **primitive base
tile** is threaded from the triangle command's `tile[2:0]` field (**bits 50:48**, N64brew *Reality
Display Processor / Commands* Edge-Coefficients word-0 table; `(ewdata[0] >> 16) & 7` in Angrylion
`rasterizer.c`) into the sampler — `tiles[base_tile]` / `tiles[(base_tile + 1) & 7]`, not a
hardwired tile 0/1 — validated against Angrylion by `tex_tri_base_tile_16` (the ramp loaded into
tile 3 renders identically to tile 0; a `tiles[0]` read renders black). The **mid-texel** filter
(Set Other Modes bit 44) is modeled too: at the exact texel center (`sfrac == tfrac == 0x10`) the
four neighbors are averaged instead of the 3-point pick (validated against Angrylion by
`tex_tri_mid_texel_16` over a non-planar checkerboard, whose center carries the midpoint value a
3-point pick never produces).

**LOD fraction.** *Provenance: this rule is an **oracle-measured port**, not a documented hardware
fact — it is transcribed from the Angrylion study oracle (`tcoord.c`, `tclod_2cycle` +
`lodfrac_lodtile_signals` + `tclod_4x17_to_15`) and validated byte-for-byte by the conformance
vector `tex_tri_lodfrac_16`; see `docs/accuracy-ledger.md` **R-13** for the full disposition.*

In **2-cycle** mode the derivative-computed `lod_frac` is modeled: the LOD is the larger of the coordinate
deltas to the next pixel in **x** (`+dsdx`) and the next scanline in **y** (`+dsdy` — the true
vertical gradient from texture-block words 5/7, *not* the major-edge `de` the scanline walk uses),
each taken through the same perspective divide as the pixel's own coordinate; `lod_signals` then
maps it to the raw 9-bit fraction using `min_level` (`Set Prim Color` bits 12:8), `max_level`
(the triangle command's `level[2:0]`, bits 53:51), and `sharpen_tex_en`/`detail_tex_en`. It feeds
the combiner's `LODFrac` mul input (RGB select 13 / alpha select 0), validated against Angrylion by
`tex_tri_lodfrac_16`. Computation is gated on Angrylion's `dolod`, so a combine that does not select
it is unaffected.

**Mip tile selection.** With `tex_lod_en` (bit 48) the LOD also picks which tiles the two cycles
sample (`lod_mip_tiles`): a *distant* LOD pins the level to `max_level`; otherwise it is `l_tile`;
the pair straddles the mip boundary (`base+level`, `base+level+1`) and collapses to a single tile
where there is nothing to blend toward (distant, or magnifying without `sharpen_tex_en`);
`detail_tex_en` shifts both one level finer; indices wrap mod 8. Validated against Angrylion by
`tex_tri_mip_tile_16`. Scope (**open residual R-13**): only the **1-cycle** LOD form remains — it
compares the `x+1` and `x+2` taps and needs span-edge signals the rasterizer does not model, so it
reads zero rather than being approximated with the 2-cycle formula.

> **Authoring note for 2-cycle textured vectors:** set **both** `bi_lerp0` (bit 11) *and*
> `bi_lerp1` (bit 10). Cycle 1's filter is selected by `bi_lerp1`, and leaving it clear sends that
> cycle down the YUV color-convert path instead of the texel fetch.

### Sub-pixel coverage primitives (T-33-004, PR-B part 2c)

The RDP anti-aliases by sampling 8 sub-positions per pixel (4 Y-subpixels × 2 X-samples) against the
triangle's edges and counting how many fall inside — a bit-exact port of parallel-rdp `coverage.h`
and `span_setup.comp`, the pure primitives ahead of wiring them into the rasterizer.

- **`quantize_x`.** Snaps a `s.16` edge X to the 3-fraction-bit (`s.3`) coverage domain with the RDP
  sticky bit: any discarded fraction bit forces the low output bit set, so a truncated-but-nonzero
  coordinate never lands exactly on a sub-pixel boundary — which is what keeps the half-open `<` /
  `>=` edge tests exact. (parallel-rdp's `setup.xh` is `s.15` and quantizes with `>> 12`; our raw
  command edges are `s.16`, one fraction bit wider, so `>> 13` — the same `s.3` result.)
- **`compute_coverage`.** For a pixel column, tests the two X-samples of each of the 4 Y-subpixels
  against that Y-subpixel's `[xleft, xright)` span. The X-sample offsets alternate by Y-subpixel —
  `{0, 4}` for Y-subpixels 0/2, `{2, 6}` for 1/3 — the RDP's diamond pattern. Returns the 8-bit mask
  packed as bit `2·Ysub + Xsample` (the oracle's `clip_x0*(1,2,4,8) + clip_x1*(16,32,64,128)` — the
  two X-samples of each Y-subpixel land in adjacent bits, so the order is `Y0X0 Y0X1 Y1X0 Y1X1 …`,
  and bit 0 is the top-left sample). Its popcount is the coverage count (0–8).
- **`aa_enable`.** `Set Other Modes` bit 3 is now decoded (but not yet consumed). It will select
  the pixel-inclusion rule once the coverage integration wires `compute_coverage` in: with AA off
  the RDP will draw a pixel only when the first sub-sample (bit 0, the top-left) is covered; with AA
  on, any covered sub-sample keeps the pixel and its coverage weights the edge blend.

Both primitives are pinned by hand-computed unit tests derived from the oracle's arithmetic
(full/partial/empty masks, the sticky bit, the negative-coordinate arithmetic shift), **not** from
this port's own output. They are now **wired into the 1-/2-cycle rasterizer** (`pixel_coverage`): the
edge-walk builds per-Y-subpixel `s.3` edges, and each pixel is gated by its coverage mask — with AA
off, a pixel draws only when its top-left sub-sample is inside the span — with the coverage count
stored in the pixel's alpha/coverage bits (`(count − 1) & 7`). FILL/COPY mode keeps the whole-pixel
span, which is correct (FILL renders "without subpixel accuracy"). Validated against Angrylion by
`fill_tri_frac_16` (FILL rounds a fractional edge) and `shade_tri_frac_16` (a 1-cycle triangle whose
fractional left edge excludes a column and whose right edge leaves a column partially covered).
The **depth path** applies the same coverage (`depth_span` takes the edges too; `shade_depth_tri_frac_16`
renders identically to `shade_tri_frac_16` against Angrylion). **Ordered RGB dither is wired**
(T-33-004 2c, `dither_tri_32` — see the blender section). **Alpha-compare is wired** on both the
no-Z and depth paths (R-11, `alpha_compare_16` / `alpha_compare_z_16`). **`cvg_dest = full` is wired** (`cvg_dest_full_16` — a partial edge column stores full coverage
`0xf801`). Scope (**open residual R-9**): the coverage-weighted **interpenetration Z** path, the
**AA-edge blend**, and the **wrap/save `cvg_dest`** modes are not wired. The oracle stays **93**.

### The conformance gate (T-33-005)

The bit-exactness gate against Angrylion, the accuracy oracle. License-clean by construction: a
standalone generator (`crates/rustyn64-test-harness/vectors-gen/`, our own MIT code) drives the
Angrylion software RDP (non-commercial study license, fetched into gitignored `ref-proj/`, never
vendored) over hand-written RDP command lists and emits `.rvec` vectors carrying *only outputs* — the
command stream plus Angrylion's rendered framebuffer, both freely committable. `tests/rdp_conformance.rs`
replays each command stream through RustyN64's RDP and asserts a byte-for-byte framebuffer match.
Because the command bytes are stored big-endian (RustyN64's RDRAM layout) and the golden pixels are
row-major big-endian logical values (exactly what RustyN64 writes into RDRAM), the comparison is a
direct byte compare. Rendering is deterministic (`parallel = false`, no wall-clock/RNG), so a command
list always yields byte-identical output.

The FILL-rectangle and both flat-triangle vectors (`fill_tri_16`, `fill_tri_wide_16`) pass end to end.
The first triangle vector earned the gate its keep immediately: it caught the **4× edge-slope bug**
(`triangle_fill` applied the per-pixel slope against quarter-pixel sub-scanline units without the
`>> 2`), which the self-asserted `fill_triangle_flat_fills_a_right_triangle` unit test had masked with a
circular staircase golden. The fix — pre-shifting the three slopes `>> 2` at decode — is in place
(ledger **R-14**, closed), and the affected triangle unit tests were corrected against the oracle (their
`DxMDy` changed from `0.25` to `1.0`, the value for which the staircase is genuinely correct, confirmed
by `fill_tri_wide_16`). The corpus grows toward the ~150-vector cut criterion from here.

## State

Implemented (the FIFO pointers + image bases, plus the texture state below);
the rest is still marked TODO:

- **TMEM** — 4 KiB texture memory (**present**, T-32-001; lazily allocated),
  **loaded** by `Load Tile` / `Load Block` (T-32-002) with the odd-row swap and the
  32-bit split, its palettes by `Load TLUT` (T-32-003) into the upper half, and
  **decoded** to RGBA8888 by `fetch_texel` (T-32-003): RGBA16/32, IA16/8/4, I8/4,
  CI8/4 (via TLUT). Formats per `ref-docs/research-report.md` §4. YUV16 decode
  pending; 4-bit textures load as 8-bit and render with a 4-bit tile (validated, R-7).
- **8 tile descriptors** — format, size, line stride, TMEM address, palette,
  clamp/mirror + mask/shift per S/T axis, and the tile-size coords (**present**,
  T-32-001). Set by `Set Tile` (0x35) and `Set Tile Size` (0x32).
- **Texture-image registers** — the RDRAM load source (`Set Texture Image`, 0x3D):
  format, size, width, address (**present**, T-32-001).
- **Other-modes** — the big mode word: cycle type, the two blend cycles' `P/A/M/B`
  selects, `force_blend`, Z-mode + Z enables, coverage-dest mode, `image_read_en`,
  alpha-compare, RGB dither mode (**present**, T-33-003/T-33-004, via `Set Other
  Modes` 0x2F). The **RGB dither is wired** (magic/bayer, `dither_tri_32`); the
  AA/coverage-accumulate details are still Sprint-3 residual R-11.
- **Combiner latches** — the two-stage color/alpha mux input selects (**present**,
  T-33-002, via `Set Combine Mode` 0x3C).
- **Blender latches** — the `P/A/M/B` selects + blend/fog color registers
  (**present**, T-33-003). RGB dither is wired (T-33-004 2c); the AA-edge config is R-11.
- **Depth registers** — the Z-buffer base (`Set Depth Image` 0x3E) and the primitive
  `z`/`dz` (`Set Primitive Depth` 0x2E) (**present**, T-33-004 PR-A). The Z-buffer
  RDRAM read/write and coverage accumulation are R-12 (PR-B).
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

Per-mode behavior must be reproduced exactly — copy/fill take shortcuts that
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
`MASTER_HZ / field_hz / (VI_V_TOTAL + 1)` master ticks (accumulating the fractional
remainder), wrapping at `VI_V_TOTAL + 1`, and raises `MI_INTR.vi` once per field
when it lands on `VI_V_INTR` — the per-half-line step means a call spanning many
half-lines cannot skip it, and a `VI_V_INTR` beyond the field never fires.
`VI_CTRL.TYPE == 0` suppresses the interrupt, and the position is kept relative so
a mid-run `VI_V_TOTAL` change re-bases without a scale jump.

**A global lower bound skips the period division on about half the calls.**
`Vi::tick` runs every RCP step, and a call returns before the 64-bit divide when the
accumulator is below `VI_MIN_TICKS_PER_HALFLINE` — the smallest
period the whole programmable space admits, `MASTER_HZ / (60 * 1024) = 3051`, since
`total_halflines()` is `(VI_V_TOTAL & 0x3FF) + 1` (so 1..=1024) and `field_hz()` is
50 or 60. Below that bound the `while` provably cannot execute for **any** legal
programming, so this is exact rather than approximate, and it adds no state (a cache
would have changed the save-state layout, ADR 0005). A `const` assertion ties the
bound to the `0x3FF` mask and to NTSC being the faster rate, because a bound that
grew too large would swallow a half-line and delay the interrupt silently. Two tests
pin it: one walks all 1,024 encodable `VI_V_TOTAL` values, and one drives the scan
one master tick at a time and requires the same landing half-line as a single jump.

**Being a global bound, it is loose for any particular field length**, and that is the
cost of not adding state. NTSC programs a 5,952-tick half-line against the 3,051-tick
bound, so calls with the accumulator between the two still divide even though they
cannot advance: about **51%** of calls take the early-out, not almost all. Tightening
it would mean remembering the current period across calls, which is a field, hence a
save-state question (ADR 0005) rather than a free change.

The field cadence is
**region-aware** (R-6): `Vi::field_hz` picks the standard PAL **50 Hz** when the
field is PAL-length (`VI_V_TOTAL > 550`) and NTSC **60 Hz** otherwise — the same
`ispal` split the scan-out geometry uses, so cadence and geometry agree on the
region. **Still deferred under R-6:** the exact `H_TOTAL` sub-field timing and the
interlace/serrate `VI_V_INTR` bit-0 quirk. The VI dot clock (VCLK, ≈48.68 MHz NTSC)
is the sole fractional-domain crystal (`docs/scheduler.md`).

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

**Two scan-out methods exist.** `Bus::scanout` is the original 1:1 converter: it
reads `VI_ORIGIN`/`VI_WIDTH`/`VI_CTRL` and the active region from `VI_V_VIDEO`
(`(V_END − V_START)` half-lines → lines) and converts the framebuffer to RGBA8 —
**16-bit RGBA5551** (each 5-bit channel widened to 8 by replicating the high bits,
the 1-bit alpha to 0/255) and **32-bit RGBA8888** (a direct copy). `TYPE` 0/1 is
blank. It applies no geometry and no post-filters.

**`Bus::scanout_scaled` is the accurate replacement** (ledger **R-5**, slices
4a-4f), built and pinned slice-by-slice **bit-exact against Angrylion**
(`n64video_update_screen`; ParaLLEl-RDP reimplemented the same path,
`ref-docs/research-report.md` §4). It reproduces the DAC pipeline in `VI_CTRL`
order:

- **Geometry** — the `VI_H_VIDEO`/`VI_V_VIDEO` active span with NTSC/PAL
  horizontal overscan (`h_start − 108/128`) and the left/top clamps folded back
  into the 2.10 scale accumulator, and the ±8 horizontal-pass crop.
- **`VI_X_SCALE`/`VI_Y_SCALE` resampling** — the 2.10 fixed-point accumulator with
  a **5-bit bilinear lerp** (`vi_lerp3`) between the four surrounding texels when
  `aa_mode ≠ REPLICATE` and a fraction is non-zero; the exact nearest sample under
  REPLICATE (`aa_mode == 3`) or zero fraction.
- **Coverage post-filters (`aa_mode` 0/1, both source formats):** the **AA-edge**
  filter (`vi_video_filter`, 6-tap penultimate-min/max) on partial-coverage pixels
  (`cvg < 7`); the **de-dither** restore (8-tap ±1 nudge, `VI_CTRL` bit 16) on
  fully-covered pixels; and the **divot** median-of-three (`VI_CTRL` bit 4) across
  the pixel and its two horizontal neighbors — with the hardware's
  **all-fully-covered early-return** (`(cen & left & right) cvg == 7` ⇒ the center
  passes through unchanged, no median). The one format-specific primitive is
  `Bus::vi_read_cov`: **32-bit** coverage is alpha bits 7:5 (`(px >> 5) & 7`);
  **16-bit** combines the pixel's bit 0 with the two **hidden bits** of the 9-bit
  RDRAM plane (`((px & 1) << 2) | rdram_hidden`), then the same filters run on the
  5-bit→8-bit-unpacked channels. Every downstream filter is format-agnostic.
- **Gamma** (`VI_CTRL` bit 3, dither bit 2 clear) — the `sqrt` curve as a
  precomputed 256-entry LUT, applied last.

**How a source pixel is sampled, and why it is memoized.** The output walk asks for
each source column about three times — at a 2x horizontal upscale an even output pixel
samples column `sx` while the odd one samples `sx` and `sx + 1` — and every one of those
calls runs the whole filter chain above (under `aa_mode` 0 with `divot` and
`dither_filter` set, three divot taps of nine de-dither taps each, 27 `Bus::vi_read_cov`
calls). `ViSampler` therefore carries a **two-row memo** of already-filtered source
pixels, consulted by `Bus::vi_sample`; `Bus::vi_sample_direct` is the uncached filter
dispatch, and `Bus::vi_column` is the vertical lerp that both bilinear columns go
through.

The memo is **behavior-identical by construction, not by testing**: a filtered sample is
a pure function of RDRAM and the `ViCfg` fields, `scanout_scaled` takes `&self` so RDRAM
cannot change mid-scan-out, and `ViSampler` owns its `ViCfg` so a memo cannot be paired
with a different configuration. Two rows is exactly what the vertical lerp needs (`sy`
and `sy + 1`) and the walk only moves `sy` forward. Only the `aa_mode` 0/1 filtered path
is cached: under 2/3 a sample is two RDRAM reads and a format convert, cheaper than the
row bookkeeping.

Measured on Super Mario 64, `--release`, two runs each: the scan-out fell from
**21.64 ms to 7.82 ms** (2.77x) and the frame from 139.31 ms to 123.76 ms (1.126x,
7.18 → 8.08 FPS). `docs/performance.md` §Measured carries the provenance. Verified by
the 164 VI/Angrylion conformance vectors, which are byte-for-byte and were shown to
cover this path by mutation — an off-by-one in the memo index turns them red.

**Still deferred in `scanout_scaled` (R-5/R-6):** gamma-**dither** (bit 2, noise
based); the coverage filters under `aa_mode == 2` (RESAMP_ONLY forces `cvg = 7` on
hardware, so de-dither can still apply — currently gated to `aa_mode ≤ 1`); and the
R-6 field-rate / interlace serrate (only the progressive field is modeled).
`Bus::scanout_scaled` also has **no per-frame driver yet** — like `Bus::scanout` it
is a pure method the run loop does not call (the R-12 land-ahead-of-caller
precedent); the frontend wiring is a later slice.

**Stepping the RDP does not move it.** `Bus::rdp_tick` used to `core::mem::take` the
whole `Rdp` — 344 bytes, read and written, plus a default written into the vacated slot —
on **every RCP step**, purely so `tick` could borrow its owner. On most steps the RDP is
frozen, stalling, or looking at an empty FIFO and needs no bus at all, so that shuffle
bought nothing. `Rdp::tick_without_bus` answers those cases from the struct's own fields
and `Bus::rdp_tick` only takes when it hands back a `NeedsBus` — a token with no
public constructor, so the bus half cannot be called out of order at all;
`Rdp::tick` calls the same
helper, so there is one implementation of the early-outs. Measured on Super Mario 64,
`--release`, two runs each: **125.24 → 108.22 ms, 1.157x** (7.98 → 9.24 FPS), with the
scan-out unchanged. Graded by the Angrylion `.rvec` suite — the oracle that actually
drives an RDP command list — plus unit tests that pin each early-out to its own
condition and drive a queued command through the bus half end to end. The `.vivec` VI
conformance vectors are **not** evidence for this change: they feed the VI from RDRAM
and never step the RDP, so they would pass just as convincingly with the stepping
broken. They stay attached to the scan-out claim below, which is what they grade.

**Oracle effect:** not measured, and it cannot change the n64-systemtest count:
both scan-out methods are pure conversions with **no runtime driver** — nothing in
the run loop calls either during a suite run — so they are unreachable by it. The
suite-wide failing count therefore stands where T-31-004 pt 1 left it. The scan-out
is graded instead by the committed **`.vivec` Angrylion vectors**
(`crates/rustyn64-test-harness/tests/vi_conformance.rs`) and the harness golden
frame (T-31-005); the deferred paths track against the ParaLLEl-RDP fuzz suite.

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
  source; honor both.

## The GPU backend (`gpu-rdp`, default-off, wired as a display path)

[ADR 0014](adr/0014-gpu-backed-rdp.md) authorizes binding **parallel-rdp** (MIT)
as an alternate rasterizer backend, behind a default-off `gpu-rdp` feature on the
frontend, in a new `rustyn64-rdp-gpu` crate that is the only place `unsafe` is
permitted.

**Read the sizing before the design.** This rasterizer is **6.36% of a
`fast-exec` frame** (`docs/performance.md`), so eliminating it entirely is
**1.068x** against the 3.9x still needed for 60 FPS. **A GPU RDP is not a
throughput answer on the current workload.** Its case is completeness and
accuracy: the share is small partly *because this rasterizer is incomplete*
(`TODO(T-31-004)`, deferred per-command timing, residual R-18), and a finished
software pixel pipeline costs materially more than 6.36% — a liability the GPU
route avoids rather than incurs.

**Nothing here changes what this document specifies.** The software rasterizer
stays the oracle: the Angrylion `.rvec` vectors, the golden frames and
`rdp_conformance.rs` grade it and only it. A GPU result may be *compared* against
it, and that comparison is worth having, but a disagreement is a GPU-backend bug
until shown otherwise.

Three constraints worth knowing before reading ADR 0014:

- **Native-only.** The frontend's `wgpu 29` carries the `webgl` feature and WebGL
  has no compute shaders; parallel-rdp is Vulkan compute.
- **The first cut needs no Vulkan in the presenter.** Upstream's `scanout_sync()`
  returns a CPU-side RGBA8 buffer, which the existing presenter takes as-is.
- **Synchronization is where this gets dangerous.** A missing dirty-region sync is
  a *race*: a wrong pixel occasionally, on some machines, invisible to every
  deterministic gate. ADR 0004's contract binds — seed + ROM + input must still
  give bit-identical AV — and a backend that cannot honor it is not shippable
  whatever its frame rate.

ADR 0014 carries written **kill criteria**, which is the point of scoping it as an
experiment: determinism unachievable, no measured improvement on a real title, or
a Vulkan dependency that cannot be made optional.

### What exists today

The binding, a parity gate over it, and a frontend display path:

| piece | state |
| --- | --- |
| `vendor/parallel-rdp-standalone` | submodule, upstream MIT, attributed in `NOTICE` |
| `crates/rustyn64-rdp-gpu/shim/` | flat `extern "C"` surface, 8 entry points, POD-only |
| the shim's failure contract | every fallible entry point returns a status; nothing returns `void` |
| `crates/rustyn64-rdp-gpu/src/lib.rs` | `GpuRdp` — every `unsafe` block carries its `// SAFETY:` |
| `tests/smoke.rs` | renders a Fill Rectangle and checks the picture |
| `conformance_gpu` + `rdp_conformance_gpu.rs` | the parity gate — the whole `.rvec` corpus on both paths |
| CI job `gpu-rdp` | builds and links, and asserts which branch each GPU test took |

**The software scan-out remains the fallback, on every failure path.**
`produce_frame` tries the GPU and drops through to `Bus::scanout_scaled` when
there is no device, when the VI produces no picture (which is every frame before
a ROM programs it), when the geometry will not fit, or when the backend fails.
A failure additionally discards the backend so the next frame rebuilds it — but
*only* a real failure: conflating that with "no picture this frame" would rebuild
a Vulkan device sixty times a second at boot.

**Verified locally on an RTX 3090:** the device initializes, parallel-rdp's
compute shaders compile, a 64x32 Fill Rectangle rasterizes, and `scanout_sync`
returns a 640x240 RGBA8 frame in which 1,568 pixels carry exactly the fill color
and no pixel carries any other. That is the whole of the evidence — it is a
command list submitted by hand, not by the emulator.

### Parity against Angrylion — the census

The GPU backend is graded by the **same independent oracle as the software
rasterizer**: the 43 registered `.rvec` vectors, whose goldens are Angrylion's.
Not against the software rasterizer's own output — two implementations agreeing
proves nothing about either, while two independently matching a third does.

**42 of 43 vectors match on both paths.** Triangles (flat, shaded, textured,
Z-buffered, fractional, negative-slope, right-major), texture loads (Load Tile,
Load Block, TLUT, CI4/CI8, 4-bit), the combiner, the blender, dither, tile
clamp/mask/mirror/shift, and the 3-point filter all reproduce Angrylion
byte-for-byte through parallel-rdp.

**One vector diverges, and it is the reverse of the expected direction.**
`tex_tri_chromakey_alpha_16` exercises the `key_en` chroma-key alpha compare.
**parallel-rdp does not implement it** — this is checkable in its source, not
inferred from pixels:

- `op_set_other_modes` decodes bits 9–19 of `words[0]` and never bit 8, which is
  `key_en`. There is no `1 << 8` anywhere in that function.
- `op_set_key_r` / `op_set_key_gb` do store the key, but `Renderer::set_color_key`
  routes `key_center` / `key_scale` to the combiner's **mux inputs** (the
  KEY_CENTER / KEY_SCALE *sources*, a different feature). `key_width` — the
  comparison width the alpha compare needs — is written and **never read**.
- No shader under `parallel-rdp/shaders/` mentions a key at all.

RustyN64's software rasterizer implements it (PR #160) and matches Angrylion;
parallel-rdp renders the shade color as black. So the premise that a GPU backend
must be *caught up* to the software one is not quite right in either direction:
parallel-rdp is more complete almost everywhere and less complete here.

The gate asserts the **exact census**, not a threshold. A vector leaving the
known-gap set fails just as a vector joining it does — an unexplained improvement
is as much a signal as a regression.

**No entry point returns `void`.** Submission, VI programming, flush and idle all
return a status, and the Rust wrapper surfaces each as a `#[must_use] bool`. This
was a review finding and it was right: the C++ side can fail at runtime — device
loss, out of memory, a command buffer that cannot be allocated — and a `void`
turns that into a dropped RDP command that surfaces as a wrong picture many
frames later with nothing pointing back at the submission. The exception still
cannot cross into Rust; it just no longer vanishes.

One guard here is **not** exercised by any test: `prdp_set_vi_register` refuses an
index outside `VIRegister` rather than casting it, and the `ViRegister` enum
cannot express an out-of-range value, so the check is reachable only from C. It
is worth having and it is not evidence of anything.

### Wired into the machine — as a display backend

The frontend's `gpu-rdp` feature (default-off, native-only, deliberately **not**
in `full`) presents frames rendered by parallel-rdp instead of by the software VI
scan-out. What that means precisely:

**It is a display backend, not a replacement rasterizer**, and the distinction is
load-bearing. `rustyn64-core` is `#![no_std]` and `#![forbid(unsafe_code)]`, so
the `Bus` cannot own a Vulkan device — that is the crate graph working, not an
obstacle. The software rasterizer therefore still runs and still writes into the
Bus's RDRAM, which is what keeps a game's framebuffer read-backs working. The GPU
renders the *same* command stream a second time and that is what reaches the
screen. Two rasterizers run; the machine's state comes from one and the picture
from the other.

**The command stream arrives by a tap, not a re-read.** `Bus::rdp_tap` (the
`rdp-tap` feature on `rustyn64-core`) records every command word the Bus feeds
the RDP, captured by **diffing the FIFO pointer** across `tick_with_bus` rather
than decoding the command a second time. Re-reading the list from RDRAM would not
work at all: by the time the frontend looks, `DPC_CURRENT` has reached `DPC_END`
and the game has usually overwritten the buffer.

That the tap therefore never captures a partly-written command is an
**implementation fact of this emulator, not a hardware claim** — the provenance
is `Rdp::tick_with_bus`, which refuses to consume a command until `DPC_END` has
passed its full length, and whose stated reason is that libdragon's `rdpq`
advances `DPC_END` incrementally as it fills the buffer. It is not measured
against hardware and carries no ledger entry. What the tap gets from the diff is
that it cannot *disagree* with that behavior, whatever it is — a tap that
restated the rule would be free to drift from it.

The field is `#[serde(skip)]`, so the save-state layout is **identical** with the
feature on or off and this stays out of ADR 0005's announced-in-advance
format-break territory.

**Cost, measured rather than estimated** (`tests/gpu_present_cost.rs`, RTX 3090,
release):

| path | per frame |
| --- | --- |
| GPU present | **0.72–0.93 ms** (first frame in a fresh process ~1.37 ms) |
| software `scanout_scaled` | 0.75 ms |

Essentially at parity — which was not true of the first version. Seeding the
GPU's RDRAM went through an 8 MiB staging buffer and then a second copy into the
mapped buffer; fusing the byte-order swap directly into the mapped write is
**3.3×** faster (A-B-A: 3.10/2.65 ms staged against 0.79/0.64 ms fused). The
whole-RDRAM snapshot is kept deliberately: it is correct by construction, with no
dirty-region tracker to get subtly wrong, which is the hazard §5 above names.

**The presented geometry differs between backends.** parallel-rdp scans out the
whole VI raster (640x240 for an NTSC field); `Bus::scanout_scaled` crops to the
active span. Both are legitimate; they are not the same picture size, and the
frontend's geometry test asserts per-backend rather than pretending otherwise.

**Explicitly NOT done, and each is a separate piece of work:**

- **The GPU does not replace the software rasterizer.** Both run. This costs
  throughput rather than saving it, which ADR 0014 predicted and the numbers
  above confirm.
- **No shared RDRAM.** The binding allocates and owns its own RDRAM, reached
  only through `with_rdram` / `with_rdram_mut`, which is what makes the safety
  argument tractable — and is *not* how a running machine's RDRAM is owned. (It
  borrowed a buffer at first; owning it is what makes the page alignment a
  property of construction rather than of the caller remembering.) The Bus's
  RDRAM is **snapshotted** into it each frame, not shared.
- **No dirty-region synchronization**, and therefore **no determinism claim**.
  ADR 0004 binds the core, and the core is untouched — the software rasterizer's
  output is still what lands in RDRAM and still what a save-state captures — so
  nothing here breaks that contract. But the GPU backend makes no determinism
  claim of its own and cannot until synchronization is real.

One correction to the plan this came from: the plan proposed `bindgen`. It is
not used and not needed — the shim is eight functions, so hand-written
declarations are shorter than the tooling to generate them, and they are the
thing a reviewer must read either way.

Their risk is **drift from the header, and nothing here detects it.** An earlier
version of this paragraph claimed `cargo test --features gpu-rdp` caught it; that
was wrong, and the correction is the point. Linking resolves *symbols*. A
declaration whose parameter types, order, or return type disagree with the C
header links exactly as cleanly as a correct one and then corrupts the stack at
run time. The eight declarations are held correct by review against
`prdp_shim.h`, which is why that header is kept short enough to diff by eye. If
this surface grows, that stops being sufficient and `bindgen` becomes the right
answer after all.

## Test plan

- **ParaLLEl-RDP conformance fuzz suite (~150 tests)** — generates RDP command
  streams and compares fixed-point outputs; "to pass we must get an exact match"
  (`ref-docs/research-report.md` §4, §7). This is the RDP gate.
- **PeterLemon RDP demos** — the de-facto visual/behavioral reference for many
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
