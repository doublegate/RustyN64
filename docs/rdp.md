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
covers 8/16-bit. **4-bit** texels (nibble addressing, pairs with the CI4/I4 decoders in
T-32-003) and the **32-bit `Load Block` split** are deferred; an unsupported size writes
nothing. The supported paths are byte-exact against hand-computed expectations (five unit
tests). The oracle count stays **93** — a load is observable only once the sampler
(T-32-004) reads TMEM.

### The sampler and copy-mode Texture Rectangle (T-32-004)

The first **textured picture**: `Texture Rectangle` (0x24) blits a tile into the colour
image in copy mode, closing the Sprint-2 texture path. This is the first **two-word**
command — `tick` now captures the command's RDRAM base address (before advancing the FIFO
pointer) and passes it to `dispatch`, so a handler can read its later words
(`bus.rdram_read_u32(cmd_base + 8)`).

- **The coordinate wrap** (`wrap_coord`) turns a raw `s10.5` texture coordinate into a
  tile-relative integer texel: clamp to `i16`, **shift** (codes 1–10 right, 11–15 left by
  `16−code`), subtract the tile origin `SL`, take the integer part (`>>5`), then **mirror**
  on alternate mask-sized spans and **mask** to `mask` bits (`mask == 0` = no wrap). Copy
  mode omits the clamp step. Matched to the ParaLLEl-RDP `texture.h` order.
- **Copy-mode Texture Rectangle** rasterises the screen rectangle (lower-right inclusive),
  stepping `S` across X and `T` down Y. The horizontal step is scaled by the
  4-pixels-per-cycle factor (`>> (5 + dx_shift)`, `dx_shift = 2` for a 16-bit image), so a
  1:1 blit's `DsDx = 4.0` advances one texel per pixel. The raw 16-bit texel is copied
  verbatim into the colour image (a direct 16-bit copy), clipped to the scissor.

Provenance: the command encoding, copy pipeline, and wrap order are cross-verified against
the N64brew wiki and ParaLLEl-RDP (MIT). Validated by a **round-trip identity** test — a
`Load Tile` texture blitted back by `Texture Rectangle` reproduces the source byte-for-byte
(load and fetch share the odd-row swap) — plus a `wrap_coord` unit test.

Scope (**open residual R-8**): the 16-bit tile → 16-bit colour image path is wired.
`Texture Rectangle Flip` (0x25), the 8/32-bit and TLUT copy paths, the exact non-1:1
sub-texel selection, and the copy alpha-compare are deferred to the Sprint-3 fuzz; an
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
  simply not found). Enforcing a rejection would invent behaviour, so it is not done.
- **`fetch_texel(tile, s, t) -> [u8; 4]`** decodes RGBA16 (5551, 5→8 replication),
  RGBA32 (from the split TMEM: R,G low half, B,A high half), IA16/IA8/IA4, I8/I4 (alpha =
  intensity), and CI8/CI4 through the TLUT (CI4 folds `tile.palette` in as the high nibble
  of the index). The 4-bit formats select the high nibble for even `s`, the low for odd.

**The read convention matches the loads.** TMEM is a natural big-endian byte array, so the
sampler applies only the odd-row 32-bit-word swap `^= (t & 1) << 2` — the endian twiddles
ParaLLEl-RDP applies to its host-word storage are intentionally absent on both the load and
fetch sides. **YUV16** decode is deferred (no oracle test needs it this sprint); **4-bit
loading** (nibble `Load Tile`/`Load Block`) remains R-7, though 4-bit *fetch* is done. The
oracle count stays **93** — `fetch_texel` now has runtime callers (the texture rectangle,
T-32-004, and the textured triangle, T-33-004 2b-texture), but no systemtest drives the render path.

### The flat-fill triangle rasteriser (T-33-001)

The first Sprint-3 ticket and the foundation every later per-pixel ticket renders through:
the edge-walked triangle. `Fill Triangle` (0x08) and its shade/texture/Z variants
(0x09–0x0F) are decoded and rasterised, cross-verified against the N64brew wiki and the
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
  colour (via the shared `fill_pixel`, the same write as `Fill Rectangle`).

Scope (**open residual R-9**): this is a **flat fill** in FILL cycle mode — the sub-pixel
coverage (ParaLLEl-RDP's `quantize_x` sticky-bit edge and the `do_offset` latch) and the
shade/texture/Z attribute interpolation are deferred; the 0x09–0x0F variants fill flat, their
coefficient words length-consumed only. The combiner (T-33-002), blender (T-33-003), and
Z/coverage (T-33-004) then colour the triangle; the whole is graded bit-exact against the
ParaLLEl-RDP conformance vectors (T-33-005). Validated here by a right-triangle golden pinning
the edge-walk and the fixed-point decode. Oracle unchanged at **93**.

### The colour combiner (T-33-002)

`Set Combine Mode` (0x3C) and the `(A − B) * C + D` evaluation — the per-pixel colour mux,
cross-verified against the N64brew wiki and ParaLLEl-RDP (MIT, `combiner.h`).

- **Decode.** The single command word packs 16 input selects — RGB and alpha `A/B/C/D` for both
  cycles — into `CombineMode`. `Set Prim Color` (0x3A) and `Set Env Color` (0x3B) latch the two
  constant-colour registers the combiner can select.
- **The equation.** Per channel, `(A − B) * C + D` with the RDP's fixed-point rules: `A/B/D` go
  through the asymmetric 9-bit `special_expand` (subtract the `0x80` bias, sign-extend to 9 bits,
  add it back), `C` is a plain 9-bit value, a `+0x80` rounding bias is applied **before** the
  `>> 8`, and `D` is added afterwards unscaled; the result is clamped with the 9-bit fold (which
  is why 256–383 saturate and 384–511 wrap). The "one" input is `0x100`, not `0xFF`.
- **Cycles.** 1-cycle mode uses only cycle 1's selects; 2-cycle mode evaluates cycle 0 (no
  inter-cycle clamp) and feeds its output as cycle 1's `Combined` input.

Scope (**open residual R-10**): the common inputs (combined, texel0/1, primitive, shade,
environment, one, zero, and the C-slot alpha taps) are wired; the **exotic** inputs — noise, LOD
fraction, the key/convert constants — read as zero until the LOD/key/convert state lands. The
arithmetic, the 16-field decode, the mux, and the 2-cycle chaining are unit-tested against
hand-computed values. `combine` now has its runtime caller — `combined_color` routes the
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
  and `Set Fog Color` (0x38) latch the two colour registers the blender can select.
- **The equation.** Per channel, `P * a0 + M * (a1 + 1)` then `>> 5`, where `P`/`M` select an RGB
  triple (pixel/memory/blend/fog) and `a0 = A >> 3`, `a1 = B >> 3` map the 8-bit alpha selects to
  the 5-bit blend weights. The `+ 1` on the `M` term is real hardware. This is the divide-free
  form the RDP uses for every non-anti-aliased-edge pixel.
- **Cycles.** 1-cycle mode evaluates blend cycle 0 alone; 2-cycle mode feeds cycle 0's RGB back
  as the pixel colour into cycle 1 (the alpha selects are unchanged between cycles).

- **Runtime wiring (T-33-004 PR-B 2b-blend).** `depth_span` now gives the blender its first
  runtime caller: for a shaded/textured triangle it reads the destination framebuffer pixel
  (`read_pixel`, the inverse of `write_pixel` for RGBA8888 and RGBA5551) and routes the combiner
  colour through `blend` **when the depth test enabled blending** — which, until per-pixel coverage
  exists, means `force_blend` is set. This mirrors the reference blender's `!blend_en` fast-path:
  an opaque pixel keeps the combiner colour and only a translucent (later, AA-edge) pixel blends
  with memory. A translucent-triangle integration test proves a 50/50 blend of red over a green
  background reaches `0x7F7F00` (plain red would mean the memory read never happened).

Scope (**open residual R-11 / R-9**): the anti-aliased-edge divider LUT, the memory-alpha
interpenetrating-Z blend-shift path, alpha-compare, dither, the `color_on_cvg` divide interaction,
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
  state (`DepthResult`). All four Z modes are modelled — **opaque** (nearer-passes, with a coplanar
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
  (`zbuffer_read`); only passing pixels write colour, and `zbuffer_write` stores the new depth when
  `z_update` is set. This is `depth_test`/`zbuffer_*`'s first runtime caller.

Validated by an occluding-triangles test (a nearer triangle draws, a farther one is rejected, a
nearer-still one overwrites — both accept and reject paths) and a hand-computed `interpolate_z` test.

Scope: coverage is full (sub-pixel edge coverage is part 2c); the `dz` derivation is a first cut. The
oracle stays **93** (no systemtest ROM drives rendering yet).

### The first shaded triangle (T-33-004, PR-B part 2b)

`Fill Shaded Triangle` (opcode bit 58) now colours each pixel from the **combiner** fed the interpolated
shade, not the FILL register — the combiner's first runtime caller.

- **Decode.** `decode_shade` reads the 8-word shade block (RGBA base + per-x `dx` and per-major-edge `de`
  deltas, `s15.16`; the base's int part is 9-bit signed, the deltas' 16-bit) into `ShadeSetup`.
- **Interpolate and combine.** `interpolate_shade` (a port of ParaLLEl-RDP's `interpolate_rgba` snap)
  gives the per-pixel RGBA; `shaded_color` runs it through `Rdp::combine` with the prim/env registers,
  and `write_pixel` packs the result to the colour image (RGBA8888 direct, RGBA5551 for 16-bit).

This applies standalone and combined with the depth test. Validated by a hand-computed
`decode_shade`/`interpolate_shade` test and a shaded-triangle test that renders the combiner output
(not the FILL colour). **This closes the R-9 flat-fill for shaded triangles.** The oracle stays **93**.

### The first textured triangle (T-33-004, PR-B part 2b-texture)

`Fill Textured Triangle` (opcode bit 57) samples a tile per pixel into the combiner's `texel0`.

- **Decode.** `decode_texture` reads the 8-word texture block (`S`/`T` base + per-x/per-major-edge
  deltas, `s16.16`; `W` is the deferred perspective term) into `TexSetup`.
- **Sample and combine.** `interpolate_st` gives the per-pixel coordinate (the **non-perspective**
  path — the integer part of the interpolated `s16.16`); `combined_color` samples tile 0 with
  `fetch_texel` and runs the combiner (with any shade). Works standalone and with shade/depth.

Validated by a textured-triangle test that samples a loaded RGBA16 texel through a texel-passthrough
combiner.

**Perspective-correct texturing.** When `Set Other Modes` `persp_tex_en` (bit 51) is set, `interpolate_st`
interpolates `S`/`T`/`W` and runs the hardware perspective divide — a faithful port of ParaLLEl-RDP's
`perspective_divide` (the 64-entry reciprocal LUT, the normalisation shift, the out-of-bounds
saturation, the `w <= 0` carry, the 17-bit clamp), validated by a hand-computed `perspective_divide`
test. Scope (**open residual R-13**): the exact tile shift/clamp/mask for triangle coordinates and the
LOD/`texel1` path remain for the conformance pass. The oracle stays **93**.

### Sub-pixel coverage primitives (T-33-004, PR-B part 2c)

The RDP anti-aliases by sampling 8 sub-positions per pixel (4 Y-subpixels × 2 X-samples) against the
triangle's edges and counting how many fall inside — a bit-exact port of parallel-rdp `coverage.h`
and `span_setup.comp`, the pure primitives ahead of wiring them into the rasteriser.

- **`quantize_x`.** Snaps a `s.16` edge X to the 3-fraction-bit (`s.3`) coverage domain with the RDP
  sticky bit: any discarded fraction bit forces the low output bit set, so a truncated-but-nonzero
  coordinate never lands exactly on a sub-pixel boundary — which is what keeps the half-open `<` /
  `>=` edge tests exact. (parallel-rdp's `setup.xh` is `s.15` and quantises with `>> 12`; our raw
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
this port's own output. They are now **wired into the 1-/2-cycle rasteriser** (`pixel_coverage`): the
edge-walk builds per-Y-subpixel `s.3` edges, and each pixel is gated by its coverage mask — with AA
off, a pixel draws only when its top-left sub-sample is inside the span — with the coverage count
stored in the pixel's alpha/coverage bits (`(count − 1) & 7`). FILL/COPY mode keeps the whole-pixel
span, which is correct (FILL renders "without subpixel accuracy"). Validated against Angrylion by
`fill_tri_frac_16` (FILL rounds a fractional edge) and `shade_tri_frac_16` (a 1-cycle triangle whose
fractional left edge excludes a column and whose right edge leaves a column partially covered).
Scope (**open residual R-9**): the **depth path** still uses full coverage, and the coverage-weighted
**AA-edge blend**, the other `cvg_dest` modes, **alpha-compare**, and **dither** are not wired. The
oracle stays **93**.

### The conformance gate (T-33-005)

The bit-exactness gate against Angrylion, the accuracy oracle. Licence-clean by construction: a
standalone generator (`crates/rustyn64-test-harness/vectors-gen/`, our own MIT code) drives the
Angrylion software RDP (non-commercial study licence, fetched into gitignored `ref-proj/`, never
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
  CI8/4 (via TLUT). Formats per `ref-docs/research-report.md` §4. YUV16 and 4-bit
  loading pending (R-7).
- **8 tile descriptors** — format, size, line stride, TMEM address, palette,
  clamp/mirror + mask/shift per S/T axis, and the tile-size coords (**present**,
  T-32-001). Set by `Set Tile` (0x35) and `Set Tile Size` (0x32).
- **Texture-image registers** — the RDRAM load source (`Set Texture Image`, 0x3D):
  format, size, width, address (**present**, T-32-001).
- **Other-modes** — the big mode word: cycle type, the two blend cycles' `P/A/M/B`
  selects, `force_blend`, Z-mode + Z enables, coverage-dest mode, `image_read_en`,
  alpha-compare (**present**, T-33-003, via `Set Other Modes` 0x2F). The dither and
  AA/coverage-accumulate details are still Sprint-3 residual R-11.
- **Combiner latches** — the two-stage color/alpha mux input selects (**present**,
  T-33-002, via `Set Combine Mode` 0x3C).
- **Blender latches** — the `P/A/M/B` selects + blend/fog colour registers
  (**present**, T-33-003). AA-edge / dither config is R-11.
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
