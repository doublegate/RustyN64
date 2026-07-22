//! `rustyn64-rdp` — RDP (Reality Display Processor), the RCP rasterizer.
//!
//! The RDP consumes a command stream (from the RSP or the CPU via the DP FIFO)
//! and rasterizes triangles/rectangles into a framebuffer in RDRAM, running the
//! color-combiner + blender + Z/coverage pipeline. The Video Interface (VI)
//! then scans that framebuffer out. The accuracy bar is **LLE** — a faithful
//! per-pixel pipeline (the ParaLLEl-RDP / angrylion reference), not a
//! triangle-list HLE.
//!
//! [`Rdp::tick`] decodes the DP FIFO — recognising every command `0x00`–`0x3F`
//! and consuming each one's full length (via [`command`]) so the stream stays
//! aligned — and dispatches the sync commands and the **FILL pipeline** (Set
//! Color Image, Set Fill Color, Set Scissor, Fill Rectangle), which writes solid
//! rectangles into the framebuffer. The rest of the rasterizer (edge-walked
//! triangles, the texture engine with TMEM, the combiner/blender, dithering,
//! coverage AA) is the remainder of this roadmap phase.
//!
//! Part of the one-directional chip-crate graph (see `docs/architecture.md`):
//! this crate depends on **exactly one** chip crate, `rustyn64-cart`, purely for
//! its [`RdramBus`] memory-bus trait — the RDP reads texture and framebuffer
//! reaches its tile storage through `rustynes-mappers`. `#![no_std]` + `alloc`.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::cast_possible_truncation, clippy::cast_lossless)]
// Skeleton `tick` is deliberately non-`const` (it will drain the DP FIFO).
#![allow(clippy::missing_const_for_fn)]

extern crate alloc;

pub mod command;

pub use rustyn64_cart::RdramBus;

/// The narrow bus the RDP sees.
///
/// RDRAM access (for the framebuffer + texture fetches) plus the
/// DP-interrupt-raise hook. Extends [`RdramBus`] (`RustyNES`'s `PpuBus` analog)
/// with the IRQ notify the rasterizer needs on `SYNC_FULL` / DP-done.
pub trait VideoBus: RdramBus {
    /// Raise the DP (RDP-done) interrupt on the MI. Default no-op for ad-hoc
    /// test buses; `rustyn64-core` sets the live `MI_INTR.dp` line.
    fn raise_dp_interrupt(&mut self) {}
}

/// One RGBA8888 output pixel (post-VI-filter); the framebuffer the frontend
/// presents is a slice of these.
pub type Pixel = u32;

/// `DPC_STATUS.XBUS` — the DP reads commands from DMEM rather than RDRAM.
pub const DP_STATUS_XBUS: u32 = 0x1;
/// `DPC_STATUS.FREEZE` — the DP is halted; registers can be read/written freely
/// without the command FIFO advancing.
pub const DP_STATUS_FREEZE: u32 = 0x2;
/// `DPC_STATUS.END_VALID` (the wiki's `END_PENDING`, read bit 9) — an end
/// address is latched behind an in-flight transfer.
///
/// Defined for the read-back layout but **not yet driven**: setting it requires
/// tracking a transfer *in progress*, which only exists once the rasterizer
/// runs (`tick` is a stub). It therefore always reads 0 today, which is exactly
/// what n64-systemtest's frozen `start-valid` case expects; the set/clear
/// transition lands with the FIFO drain.
pub const DP_STATUS_END_VALID: u32 = 0x200;
/// `DPC_STATUS.START_VALID` — a start address is latched and pending; further
/// writes to `DPC_START` are ignored until it is consumed by a `DPC_END` write.
pub const DP_STATUS_START_VALID: u32 = 0x400;

/// The `DPC_START`/`DPC_END` register mask: a 24-bit, 8-byte-aligned RDRAM
/// address (n64-systemtest's `RDP START & END REG (masking)`).
pub const DPC_ADDR_MASK: u32 = 0x00FF_FFF8;

/// `Sync Load` (0x26) pipeline stall, in GCLK cycles.
///
/// Fixed and unconditional — the RDP always stalls this long, whether or not a
/// load is in flight (N64brew *Reality Display Processor/Commands* §0x26). One
/// `tick` is one GCLK.
pub const SYNC_LOAD_GCLK: u32 = 25;
/// `Sync Pipe` (0x27) pipeline stall, in GCLK cycles.
///
/// Fixed and unconditional (N64brew *…/Commands* §0x27).
pub const SYNC_PIPE_GCLK: u32 = 50;
/// `Sync Tile` (0x28) pipeline stall, in GCLK cycles.
///
/// Fixed and unconditional (N64brew *…/Commands* §0x28).
pub const SYNC_TILE_GCLK: u32 = 33;

// RDP command opcodes handled by the dispatcher (bits 61:56 of a command word).
const OP_SYNC_LOAD: u8 = 0x26;
const OP_SYNC_PIPE: u8 = 0x27;
const OP_SYNC_TILE: u8 = 0x28;
const OP_SYNC_FULL: u8 = 0x29;
const OP_SET_SCISSOR: u8 = 0x2D;
const OP_SET_TILE_SIZE: u8 = 0x32;
const OP_LOAD_BLOCK: u8 = 0x33;
const OP_LOAD_TILE: u8 = 0x34;
const OP_SET_TILE: u8 = 0x35;

/// Maximum texels a single `Load Block` may transfer (N64brew *…/Commands*
/// §Load Block); a load over this writes nothing into TMEM.
pub const LOAD_BLOCK_MAX_TEXELS: u32 = 2048;

/// Bytes per texel for a texel-size code, or `None` for 4-bit (`size` 0), which
/// is sub-byte and needs nibble addressing: 8bpp=1, 16bpp=2, 32bpp=4.
const fn bytes_per_texel(size: u8) -> Option<u32> {
    match size {
        1 => Some(1),
        2 => Some(2),
        3 => Some(4),
        _ => None,
    }
}
const OP_FILL_RECTANGLE: u8 = 0x36;
const OP_SET_FILL_COLOR: u8 = 0x37;
const OP_SET_TEXTURE_IMAGE: u8 = 0x3D;
const OP_SET_COLOR_IMAGE: u8 = 0x3F;

/// TMEM size in bytes — 4 KiB of on-chip texture memory.
///
/// Addressed as 512 64-bit words. The upper half (byte >= 0x800 / word >= 0x100)
/// holds TLUTs and the high halves of 32-bit / YUV textures (N64brew
/// *…/Commands* §Set Tile).
pub const TMEM_SIZE: usize = 4096;

/// One of the RDP's eight tile descriptors.
///
/// The format/size/addressing state that binds a region of TMEM to a texture,
/// set by `Set Tile` (0x35) and sized by `Set Tile Size` (0x32) / the load
/// commands. All fields are decoded straight from the command word (N64brew
/// *…/Commands* §0x35/§0x32).
// The four bools are the hardware's four independent clamp/mirror bit-flags (one
// clamp + one mirror per S/T axis); they are decoded straight from command bits
// 8/9/18/19, so an enum would misrepresent the register rather than clarify it.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TileDescriptor {
    /// Texel format: RGBA=0, YUV=1, CI=2, IA=3, I=4 (`Set Tile` bits 55:53).
    pub format: u8,
    /// Texel size code: 4bpp=0, 8bpp=1, 16bpp=2, 32bpp=3 (bits 52:51).
    pub size: u8,
    /// Row stride in 64-bit TMEM words (bits 49:41).
    pub line: u16,
    /// Base TMEM address in 64-bit words (bits 40:32); word 0x100 = byte 0x800.
    pub tmem_addr: u16,
    /// Palette index, the high half of the TLUT address for CI4 tiles only
    /// (bits 23:20).
    pub palette: u8,
    // T-axis fields precede S-axis, matching the command word's MSB→LSB order
    // (T in bits 19:10, S in bits 9:0) and the `set_tile` decoder.
    /// Clamp T when sampling outside the tile (bit 19).
    pub clamp_t: bool,
    /// Mirror T on every other wrap (bit 18).
    pub mirror_t: bool,
    /// Number of T integer-coordinate bits used for wrap; 0 = all (bits 17:14).
    pub mask_t: u8,
    /// T coordinate shift code, per the shift table (bits 13:10).
    pub shift_t: u8,
    /// Clamp S when sampling outside the tile (bit 9).
    pub clamp_s: bool,
    /// Mirror S on every other wrap (bit 8).
    pub mirror_s: bool,
    /// Number of S integer-coordinate bits used for wrap; 0 = all (bits 7:4).
    pub mask_s: u8,
    /// S coordinate shift code, per the shift table (bits 3:0).
    pub shift_s: u8,
    /// Tile-size upper-left S (`u10.2`), from `Set Tile Size` / the loaders.
    pub sl: u16,
    /// Tile-size upper-left T (`u10.2`).
    pub tl: u16,
    /// Tile-size lower-right S (`u10.2`).
    pub sh: u16,
    /// Tile-size lower-right T (`u10.2`).
    pub th: u16,
}

/// RDP state (skeleton).
///
/// Holds the command-FIFO pointers, the current render mode (other-modes),
/// scissor rectangle, the color-image / Z-image RDRAM addresses, the
/// texture-image source registers, the eight tile descriptors, and TMEM. The
/// texel loads and the sampler/combiner that consume this state land in the rest
/// of the sprint. `#[non_exhaustive]`: this render state grows every sprint
/// (other-modes and the combiner latches are still to come), so adding a field
/// must not be a breaking change. Construct via [`Rdp::new`]; the workspace
/// never uses a struct literal.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct Rdp {
    /// DP command FIFO start (`DPC_START`).
    pub cmd_start: u32,
    /// DP command FIFO end (`DPC_END`).
    pub cmd_end: u32,
    /// DP command FIFO current (`DPC_CURRENT`).
    pub cmd_current: u32,
    /// DP command FIFO status (`DPC_STATUS`): FREEZE, START/END-valid, XBUS,
    /// and (later) the busy/counter bits.
    pub status: u32,
    /// Color-image (framebuffer) base in RDRAM (`Set Color Image`, 0x3F).
    pub color_image: u32,
    /// Color-image pixel size code (`Set Color Image` size\[1:0\]): 0 = 4-bit,
    /// 1 = 8-bit, 2 = 16-bit, 3 = 32-bit. Bytes-per-pixel derive from it.
    pub color_image_size: u8,
    /// Color-image pixel format code (`Set Color Image` format\[2:0\]); the same
    /// format enumeration as textures. Stored for later pipeline stages — the
    /// FILL path writes the raw fill value and does not consult it.
    pub color_image_format: u8,
    /// Color-image width in pixels (`Set Color Image` width\[9:0\] + 1). The row
    /// stride is `width * bytes_per_pixel`.
    pub color_image_width: u16,
    /// Z-image base in RDRAM (`SET_Z_IMAGE`).
    pub z_image: u32,
    /// FILL-mode colour register (`Set Fill Color`, 0x37): a 32-bit value written
    /// verbatim to the color image. Its interpretation depends on the pixel size
    /// — one RGBA32, two RGBA16 (even pixel = upper half, odd = lower), or four
    /// 8-bit values repeating every four pixels.
    pub fill_color: u32,
    /// Scissor rectangle (`Set Scissor`, 0x2D), the four `u10.2` screen
    /// coordinates that bound every primitive: upper-left (x, y) and lower-right
    /// (x, y). Pixels outside it are neither processed nor written.
    pub scissor_ulx: u16,
    /// Scissor upper-left y (`u10.2`). See [`Rdp::scissor_ulx`].
    pub scissor_uly: u16,
    /// Scissor lower-right x (`u10.2`). See [`Rdp::scissor_ulx`].
    pub scissor_lrx: u16,
    /// Scissor lower-right y (`u10.2`). See [`Rdp::scissor_ulx`].
    pub scissor_lry: u16,
    /// Count of commands the FIFO decoder has retired. A **retired-work tally**,
    /// not a cycle position: nothing schedules against it (the residue
    /// invariant governs only `master_ticks`), it is derived from the command
    /// stream, and it exists so tests can witness that the decoder consumed the
    /// number of commands it should. Wraps rather than panicking.
    pub commands_processed: u64,
    /// GCLK cycles the pipeline is currently stalled, counted **down** one per
    /// `tick`; while non-zero the FIFO does not advance. Set by the sync
    /// commands to their documented fixed stalls ([`SYNC_LOAD_GCLK`] etc.). This
    /// is a stall countdown, not a cycle position — it is decremented, nothing
    /// derives a clock from it, and it does not touch the derive-don't-increment
    /// rule (only `master_ticks` is ever incremented; ADR 0006).
    pub stall: u32,
    /// Texture-image (`Set Texture Image`, 0x3D) format code — the RDRAM source
    /// for texture loads. The wiki notes this format has no effect on any
    /// operation (only the tile format matters); stored for completeness.
    pub tex_image_format: u8,
    /// Texture-image texel size code (0x3D size\[1:0\]): 4/8/16/32bpp = 0/1/2/3.
    /// Drives the per-texel address stride during a load.
    pub tex_image_size: u8,
    /// Texture-image width in pixels (0x3D width\[9:0\] + 1); the row stride of
    /// the RDRAM source during a load.
    pub tex_image_width: u16,
    /// Texture-image base address in RDRAM (0x3D dramAddress\[23:0\]).
    pub tex_image_addr: u32,
    /// The eight tile descriptors (`Set Tile` / `Set Tile Size` / the loaders).
    pub tiles: [TileDescriptor; 8],
    /// On-chip texture memory (4 KiB). **Lazily allocated**: `None` until the
    /// first byte is written, and read as all-zero while `None`. This keeps
    /// [`Rdp`]'s `Default` cheap, which matters because `Bus::rdp_tick` does a
    /// `core::mem::take` every RCP step — a `None` placeholder is swapped in with
    /// no 4 KiB allocation or copy, while the real TMEM box moves by pointer.
    tmem: Option<alloc::boxed::Box<[u8; TMEM_SIZE]>>,
}

impl Rdp {
    /// Construct at power-on.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Read a DP command register by word offset within the `0x0410_0000`
    /// block: 0 `DPC_START`, 1 `DPC_END`, 2 `DPC_CURRENT`, 3 `DPC_STATUS`. The
    /// clock/busy/counter registers (4..=7) are not modelled and read zero.
    #[must_use]
    pub const fn dpc_read(&self, offset: u32) -> u32 {
        match offset & 7 {
            0 => self.cmd_start,
            1 => self.cmd_end,
            2 => self.cmd_current,
            3 => self.status,
            _ => 0,
        }
    }

    /// Write a DP command register (word offsets as in [`Rdp::dpc_read`]).
    ///
    /// The FIFO uses a double-latch pinned by n64-systemtest's `RSP STATUS:
    /// start-valid` and documented in the N64brew wiki (*Reality Display
    /// Processor Interface*, the `DPC_END` section):
    ///
    /// - Writing `DPC_START` latches the (masked) address and sets `START_VALID`
    ///   **only if it was clear** — a second write while valid is ignored.
    /// - Writing `DPC_END` latches the end address, then branches on
    ///   `START_VALID` (the wiki's `START_PENDING`): if **set**, this is a fresh
    ///   transfer — copy the pending start into `DPC_CURRENT` and clear
    ///   `START_VALID`. If **clear**, it is an *incremental* transfer that
    ///   continues from the current position, so `DPC_CURRENT` is left alone
    ///   (rewinding it would reprocess already-consumed commands). On unfrozen
    ///   hardware the transfer also runs; while frozen only the latch happens.
    pub const fn dpc_write(&mut self, offset: u32, value: u32) {
        match offset & 7 {
            0 => {
                if self.status & DP_STATUS_START_VALID == 0 {
                    self.cmd_start = value & DPC_ADDR_MASK;
                    self.status |= DP_STATUS_START_VALID;
                }
            }
            1 => {
                self.cmd_end = value & DPC_ADDR_MASK;
                if self.status & DP_STATUS_START_VALID != 0 {
                    self.cmd_current = self.cmd_start;
                    self.status &= !DP_STATUS_START_VALID;
                }
            }
            3 => self.dpc_write_status(value),
            _ => {}
        }
    }

    /// Apply a `DPC_STATUS` write, whose bits are set/clear *commands* rather
    /// than the status layout read back. Only XBUS and FREEZE are modelled; the
    /// FLUSH/TMEM/PIPE/CMD/CLOCK-counter commands come with the FIFO drain.
    // TODO(T-RDP-01): when `SET_FLUSH` (pipeline flush) lands here, it must also
    // clear `self.stall` — a flush discards in-flight pipeline work, so a
    // leftover sync-stall countdown must not persist across it. Subsystem-scoped
    // (pre-ticket) rather than T-31-003, which is the fill pipeline, not flush.
    const fn dpc_write_status(&mut self, value: u32) {
        const CLEAR_XBUS: u32 = 0x1;
        const SET_XBUS: u32 = 0x2;
        const CLEAR_FREEZE: u32 = 0x4;
        const SET_FREEZE: u32 = 0x8;
        if value & CLEAR_XBUS != 0 {
            self.status &= !DP_STATUS_XBUS;
        }
        if value & SET_XBUS != 0 {
            self.status |= DP_STATUS_XBUS;
        }
        if value & CLEAR_FREEZE != 0 {
            self.status &= !DP_STATUS_FREEZE;
        }
        if value & SET_FREEZE != 0 {
            self.status |= DP_STATUS_FREEZE;
        }
    }

    /// Advance the RDP by one rasterization step: decode the command at
    /// `DPC_CURRENT` and consume its whole length, so the FIFO drains one
    /// command per scheduler tick rather than in a burst.
    ///
    /// Hot path: keep allocation-free. No-op while the FIFO is empty
    /// (`DPC_CURRENT >= DPC_END`) or the DP is frozen (`DPC_STATUS.FREEZE`).
    ///
    /// The command length comes from [`command::command_len_words`], which
    /// recognises every opcode `0x00`–`0x3F`; consuming the exact length is what
    /// keeps a multi-word primitive from desyncing the pointer. Today the
    /// decoder only advances and counts — no primitive is rasterized yet.
    ///
    /// Commands are read from RDRAM (the `XBUS` bit clear). The `XBUS`/DMEM
    /// command source is not yet wired: the `rdpq` microcode that drives us DMAs
    /// its list to RDRAM, so the RDRAM path is the one exercised. With `XBUS`
    /// set the decoder **stalls** rather than mis-reading RDRAM as the command
    /// stream — decoding DMEM commands out of RDRAM would treat parameter data
    /// as opcodes and desync.
    ///
    /// Dispatch so far (`dispatch`) covers the four sync commands and the FILL
    /// pipeline (Set Color Image, Set Fill Color, Set Scissor, Fill Rectangle).
    /// Everything else is still recognised-and-consumed only.
    pub fn tick<B: VideoBus>(&mut self, bus: &mut B) {
        // Frozen or DMEM-sourced (XBUS, not yet wired): the pipeline counter is
        // halted, so do not even burn a stall cycle.
        if self.status & (DP_STATUS_FREEZE | DP_STATUS_XBUS) != 0 {
            return;
        }
        // A prior sync is still stalling the pipeline — burn one GCLK and hold
        // the FIFO until the stall expires.
        if self.stall > 0 {
            self.stall -= 1;
            return;
        }
        if self.cmd_current >= self.cmd_end {
            return;
        }
        let word0_hi = bus.rdram_read_u32(self.cmd_current);
        let opcode = command::opcode_of(word0_hi);
        let len_bytes = command::command_len_words(opcode) * 8;
        // Consume a command only once it is present in full. The `rdpq`
        // microcode advances `DPC_END` incrementally as it fills the buffer, so
        // `DPC_END` can land mid-command; consuming a partially-written
        // multi-word primitive would decode against unwritten RDRAM. The guard
        // above guarantees `cmd_current < cmd_end`, so the subtraction cannot
        // underflow.
        if self.cmd_end - self.cmd_current < len_bytes {
            return;
        }
        // The low half of the first command word (every command handled so far
        // fits in one 64-bit word: opcode + flags in `hi`, payload in `lo`).
        let word0_lo = bus.rdram_read_u32(self.cmd_current.wrapping_add(4));
        self.cmd_current = self.cmd_current.wrapping_add(len_bytes);
        self.commands_processed = self.commands_processed.wrapping_add(1);
        self.dispatch(opcode, word0_hi, word0_lo, bus);
    }

    /// Act on a just-consumed command. Only the sync commands are handled so
    /// far; every other opcode is a recognised no-op until its handler lands.
    ///
    /// - `Sync Load`/`Pipe`/`Tile` (0x26/0x27/0x28) each stall the pipeline for
    ///   a fixed, unconditional number of GCLK cycles (25/50/33) — the RDP waits
    ///   the full time whether or not the sync was needed, which is why the
    ///   stall is a constant and not a wait on an internal signal.
    /// - `Sync Full` (0x29) **raises the DP interrupt** (`raise_dp_interrupt`) —
    ///   the only part of the command implemented. On hardware it first waits for
    ///   all staged pipeline/memory work and halts the pipeline counter; neither
    ///   is modelled (there is no asynchronous pipeline work yet, and no pipeline
    ///   counter), so the interrupt is raised as soon as the command is
    ///   dispatched. A *preceding* sync stall still delays this dispatch via the
    ///   `stall` gate above (checked before a command is dispatched), so a queued
    ///   stall drains before the interrupt fires.
    ///
    /// On stall resolution: per-command *execution* cost is not modelled yet —
    /// every command is consumed in a single placeholder `tick` — so the `stall`
    /// set here is the documented pipeline stall *layered on top of* that one
    /// consume tick, not a claim about total command latency (the next command
    /// resumes after `1 + N` ticks). The stall itself is exactly the documented
    /// N GCLK; exact per-command base timing is deferred to the command-timing
    /// model.
    ///
    /// The FILL-pipeline arms take the command's two 32-bit halves (`hi` =
    /// RDRAM bits 63:32, `lo` = 31:0). `Fill Rectangle` writes the fill colour
    /// into the color image, clipped to the scissor — the FILL-mode path (the
    /// cycle-type gate arrives with `Set Other Modes`, so `Fill Rectangle` is a
    /// solid FILL fill for now; 1-/2-cycle rectangles route through the blender,
    /// not this code).
    fn dispatch<B: VideoBus>(&mut self, opcode: u8, hi: u32, lo: u32, bus: &mut B) {
        match opcode {
            OP_SYNC_LOAD => self.stall = SYNC_LOAD_GCLK,
            OP_SYNC_PIPE => self.stall = SYNC_PIPE_GCLK,
            OP_SYNC_TILE => self.stall = SYNC_TILE_GCLK,
            OP_SYNC_FULL => bus.raise_dp_interrupt(),
            OP_SET_COLOR_IMAGE => {
                // format[2:0] = hi 23:21, size[1:0] = hi 20:19, width[9:0] = hi
                // 9:0 (minus one), dramAddress[23:0] = lo 23:0.
                self.color_image_format = ((hi >> 21) & 0x7) as u8;
                self.color_image_size = ((hi >> 19) & 0x3) as u8;
                self.color_image_width = ((hi & 0x3FF) as u16).wrapping_add(1);
                self.color_image = lo & 0x00FF_FFFF;
            }
            OP_SET_FILL_COLOR => self.fill_color = lo,
            OP_SET_SCISSOR => {
                // upper-left x/y = hi 23:12 / 11:0, lower-right x/y = lo 23:12 /
                // 11:0 (all u10.2). The field/odd interlace bits (lo 25/24) are
                // not modelled yet.
                self.scissor_ulx = ((hi >> 12) & 0xFFF) as u16;
                self.scissor_uly = (hi & 0xFFF) as u16;
                self.scissor_lrx = ((lo >> 12) & 0xFFF) as u16;
                self.scissor_lry = (lo & 0xFFF) as u16;
            }
            OP_FILL_RECTANGLE => self.fill_rectangle(hi, lo, bus),
            OP_SET_TEXTURE_IMAGE => {
                // Same field layout as Set Color Image: format[55:53] = hi
                // 23:21, size[52:51] = hi 20:19, width[41:32] = hi 9:0 (minus
                // one), dramAddress[23:0] = lo 23:0.
                self.tex_image_format = ((hi >> 21) & 0x7) as u8;
                self.tex_image_size = ((hi >> 19) & 0x3) as u8;
                self.tex_image_width = ((hi & 0x3FF) as u16).wrapping_add(1);
                self.tex_image_addr = lo & 0x00FF_FFFF;
            }
            OP_SET_TILE => self.set_tile(hi, lo),
            OP_SET_TILE_SIZE => self.set_tile_size(hi, lo),
            OP_LOAD_TILE => self.load_tile(hi, lo, bus),
            OP_LOAD_BLOCK => self.load_block(hi, lo, bus),
            // TODO(T-31-004): remaining opcodes are recognised and
            // length-consumed by `tick`, but not yet dispatched — an
            // intentional, documented no-op at this stage, not a silent discard.
            // Handlers arrive per ticket (VI scan-out, then texture / combiner /
            // blender), and `docs/rdp.md` is the authoritative list of what is
            // dispatched versus recognised-only, so a later missing arm is caught
            // against that spec rather than passing silently here.
            _ => {}
        }
    }

    /// Bytes per pixel for the current color-image size, or `None` for the
    /// 4-bit mode, which cannot be a FILL-mode render target (it would crash the
    /// real RDP — N64brew *…/Commands* §Set Color Image hazards).
    const fn color_image_bpp(&self) -> Option<u32> {
        match self.color_image_size {
            1 => Some(1), // 8-bit
            2 => Some(2), // 16-bit
            3 => Some(4), // 32-bit
            _ => None,    // 4-bit: crash on the real RDP
        }
    }

    /// Render a `Fill Rectangle` in FILL mode: write the 32-bit fill colour into
    /// the color image over the rectangle, clipped to the scissor.
    ///
    /// FILL mode "repeats the 32-bit value verbatim out to memory", which
    /// resolves per pixel by size (N64brew *…/Commands* §Set Fill Color):
    /// 32-bit writes the whole colour; 16-bit takes the upper half for even
    /// pixels and the lower half for odd; 8-bit takes byte `x & 3`. Coordinates
    /// are `u10.2`; FILL mode floors the upper-left and rounds the lower-right up
    /// (a half-open pixel span — N64brew *…/Commands* §Fill Rectangle). The exact
    /// sub-pixel edge rules, and the scissor's inclusive-right/exclusive-lower
    /// FILL rule, are an **open residual** (`docs/accuracy-ledger.md` R-3):
    /// byte-exact for aligned rectangles, validated bit-for-bit against Angrylion
    /// via the ParaLLEl-RDP fuzz suite (Sprint 3) and superseded there if it
    /// diverges.
    fn fill_rectangle<B: VideoBus>(&self, hi: u32, lo: u32, bus: &mut B) {
        let Some(bpp) = self.color_image_bpp() else {
            return; // 4-bit target: the real RDP crashes; we skip.
        };
        // No color image configured yet (width is field+1, so a real Set Color
        // Image never yields 0). Rendering before it is a documented hazard —
        // the real RDP writes to an unspecified location — so we write nothing
        // rather than smear every row onto offset 0 with a zero stride.
        if self.color_image_width == 0 {
            return;
        }
        // Rectangle: lower-right x/y = hi 23:12 / 11:0, upper-left x/y = lo 23:12
        // / 11:0 (all u10.2). Floor the upper-left, round the lower-right up.
        let rx0 = ((lo >> 12) & 0xFFF) >> 2;
        let ry0 = (lo & 0xFFF) >> 2;
        let rx1 = (((hi >> 12) & 0xFFF) + 3) >> 2;
        let ry1 = ((hi & 0xFFF) + 3) >> 2;
        // Scissor: floor upper-left, round lower-right up.
        let sx0 = u32::from(self.scissor_ulx) >> 2;
        let sy0 = u32::from(self.scissor_uly) >> 2;
        let sx1 = (u32::from(self.scissor_lrx) + 3) >> 2;
        let sy1 = (u32::from(self.scissor_lry) + 3) >> 2;
        // Intersection of rectangle and scissor (half-open).
        let x0 = rx0.max(sx0);
        let y0 = ry0.max(sy0);
        let x1 = rx1.min(sx1);
        let y1 = ry1.min(sy1);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let stride = u32::from(self.color_image_width) * bpp;
        let color = self.fill_color.to_be_bytes();
        for y in y0..y1 {
            let row = self.color_image.wrapping_add(y * stride);
            for x in x0..x1 {
                let addr = row.wrapping_add(x * bpp);
                match bpp {
                    4 => {
                        for (i, b) in color.iter().enumerate() {
                            bus.rdram_write(addr.wrapping_add(i as u32), *b);
                        }
                    }
                    2 => {
                        // Even pixel: upper 16 bits; odd pixel: lower 16 bits.
                        let half = if x & 1 == 0 { 0 } else { 2 };
                        bus.rdram_write(addr, color[half]);
                        bus.rdram_write(addr.wrapping_add(1), color[half + 1]);
                    }
                    // 8-bit: one of four values, repeating every four pixels.
                    1 => bus.rdram_write(addr, color[(x & 3) as usize]),
                    // `color_image_bpp` yields only 1, 2, or 4; unreachable.
                    _ => {}
                }
            }
        }
    }

    /// Apply a `Set Tile` (0x35): decode the descriptor at `index` (bits 26:24)
    /// from the command word. Pure state — no texel is moved (that is the load
    /// commands). Field layout: format 55:53, size 52:51, line 49:41, TMEM
    /// address 40:32; then per-axis clamp/mirror/mask/shift with T in bits 19:10
    /// and S in bits 9:0 (N64brew *…/Commands* §0x35).
    fn set_tile(&mut self, hi: u32, lo: u32) {
        let index = ((lo >> 24) & 0x7) as usize;
        self.tiles[index] = TileDescriptor {
            format: ((hi >> 21) & 0x7) as u8,
            size: ((hi >> 19) & 0x3) as u8,
            line: ((hi >> 9) & 0x1FF) as u16,
            tmem_addr: (hi & 0x1FF) as u16,
            palette: ((lo >> 20) & 0xF) as u8,
            clamp_t: (lo >> 19) & 1 != 0,
            mirror_t: (lo >> 18) & 1 != 0,
            mask_t: ((lo >> 14) & 0xF) as u8,
            shift_t: ((lo >> 10) & 0xF) as u8,
            clamp_s: (lo >> 9) & 1 != 0,
            mirror_s: (lo >> 8) & 1 != 0,
            mask_s: ((lo >> 4) & 0xF) as u8,
            shift_s: (lo & 0xF) as u8,
            // Set Tile does not touch the tile-size coords; preserve them.
            ..self.tiles[index]
        };
    }

    /// Apply a `Set Tile Size` (0x32): the clamp/mask/mirror extents for the
    /// descriptor at `index` (bits 26:24). Upper-left `SL`/`TL` in bits 55:44 /
    /// 43:32, lower-right `SH`/`TH` in bits 23:12 / 11:0, all `u10.2` (N64brew
    /// *…/Commands* §0x32).
    fn set_tile_size(&mut self, hi: u32, lo: u32) {
        let index = ((lo >> 24) & 0x7) as usize;
        let tile = &mut self.tiles[index];
        tile.sl = ((hi >> 12) & 0xFFF) as u16;
        tile.tl = (hi & 0xFFF) as u16;
        tile.sh = ((lo >> 12) & 0xFFF) as u16;
        tile.th = (lo & 0xFFF) as u16;
    }

    /// Write one byte into TMEM, allocating the 4 KiB backing box on first use.
    ///
    /// `offset` is a byte address, masked into the 4 KiB space (loads past the end
    /// wrap to the start — N64brew *…/Commands* §Load Tile). This is the single
    /// allocation seam the load commands share, so `get_or_insert_with` is not
    /// repeated per command.
    fn tmem_write(&mut self, offset: usize, byte: u8) {
        let tmem = self
            .tmem
            .get_or_insert_with(|| alloc::boxed::Box::new([0u8; TMEM_SIZE]));
        tmem[offset & (TMEM_SIZE - 1)] = byte;
    }

    /// Apply a `Load Tile` (0x34): copy a rectangle of texels from the current
    /// texture image in RDRAM into the tile's TMEM region, then update the tile
    /// size for rendering. `SL`/`TL`/`SH`/`TH` (bits 55:44 / 43:32 / 23:12 / 11:0)
    /// are `u10.2`; the `.2` fraction is floored and the span is **inclusive**
    /// (`SH − SL + 1` texels per row). Rows advance by the tile's `line` stride.
    ///
    /// The TMEM byte placement mirrors the sampler exactly: an **odd-row 32-bit
    /// word swap** (`dst ^= (t & 1) << 2`) applies to every size, and **32-bit
    /// RGBA is split** — R,G into the low half of TMEM, B,A into the high half,
    /// stepping two bytes per texel and masking to `0x7FF`. Both are the read-side
    /// layout in the ParaLLEl-RDP reference (MIT); see `docs/rdp.md` §TMEM loads.
    ///
    /// Scope: 8/16/32-bit texels (`size` 1/2/3). 4-bit (`size` 0) loading needs
    /// nibble addressing and lands with the CI4/I4 decoders (T-32-003); an
    /// unsupported size writes nothing (**open residual R-7**).
    fn load_tile<B: VideoBus>(&mut self, hi: u32, lo: u32, bus: &B) {
        let index = ((lo >> 24) & 0x7) as usize;
        let sl = ((hi >> 12) & 0xFFF) >> 2;
        let tl = (hi & 0xFFF) >> 2;
        let sh = ((lo >> 12) & 0xFFF) >> 2;
        let th = (lo & 0xFFF) >> 2;
        // Reject a degenerate/inverted range the same way as every other unsupported
        // path (write nothing) rather than letting the `& 0xFFF` wrap produce a large
        // bogus width/height: `sh < sl` would otherwise iterate ~4095 texels of
        // garbage. A well-formed load has SL <= SH and TL <= TH.
        if th < tl || sh < sl {
            return;
        }
        let width = (sh - sl + 1) & 0xFFF;
        let height = th - tl + 1;
        if width == 0 {
            return;
        }
        let tile = self.tiles[index];
        let Some(dst_bpt) = bytes_per_texel(tile.size) else {
            return; // 4-bit (or unmapped) not loaded here — R-7.
        };
        // The texture image should match the tile size (documented hazard). A 4-bit
        // (or unmapped) source size has no byte stride, so bail rather than fall back
        // to the tile stride and read out of bounds — R-7.
        let Some(src_bpt) = bytes_per_texel(self.tex_image_size) else {
            return;
        };
        let split = tile.size == 3; // 32-bit RGBA uses the split TMEM layout.
        let tmem_base = u32::from(tile.tmem_addr) * 8;
        let stride = u32::from(tile.line) * 8;
        let tex_w = u32::from(self.tex_image_width);
        for t in 0..height {
            let swap = (t & 1) << 2;
            for s in 0..width {
                let src_pixel = (tl + t) * tex_w + (sl + s);
                let src = self.tex_image_addr.wrapping_add(src_pixel * src_bpt);
                if split {
                    // R,G -> low half; B,A -> high half (offset by 0x800). The swap is
                    // applied per final byte so it stays correct regardless of alignment.
                    let bo = (tmem_base + stride * t + s * 2) & 0x7FF;
                    self.tmem_write((bo ^ swap) as usize, bus.rdram_read(src));
                    self.tmem_write(
                        ((bo + 1) ^ swap) as usize,
                        bus.rdram_read(src.wrapping_add(1)),
                    );
                    self.tmem_write(
                        ((bo + 0x800) ^ swap) as usize,
                        bus.rdram_read(src.wrapping_add(2)),
                    );
                    self.tmem_write(
                        ((bo + 0x801) ^ swap) as usize,
                        bus.rdram_read(src.wrapping_add(3)),
                    );
                } else {
                    let base = tmem_base + stride * t + s * dst_bpt;
                    for i in 0..dst_bpt {
                        // XOR the swap into each final byte address, not the base.
                        self.tmem_write(
                            ((base + i) ^ swap) as usize,
                            bus.rdram_read(src.wrapping_add(i)),
                        );
                    }
                }
            }
        }
        // Load Tile updates the descriptor's tile size for rendering.
        let tile = &mut self.tiles[index];
        tile.sl = ((hi >> 12) & 0xFFF) as u16;
        tile.tl = (hi & 0xFFF) as u16;
        tile.sh = ((lo >> 12) & 0xFFF) as u16;
        tile.th = (lo & 0xFFF) as u16;
    }

    /// Apply a `Load Block` (0x33): stream a linear run of texels from the current
    /// texture image into the tile's TMEM region. `SL`/`SH` (bits 55:44 / 23:12)
    /// are `u12.0` integer texels; `SH − SL + 1` is the count (**inclusive**), and
    /// a count over [`LOAD_BLOCK_MAX_TEXELS`] writes nothing. The low field
    /// (bits 11:0) is **`dxt`** (`u1.11`): a running counter `T = (word * dxt) >>
    /// 11` over each 64-bit TMEM word decides line parity, and an odd line swaps
    /// that word's two 32-bit halves (`dst ^= 4`).
    ///
    /// Scope: 8/16-bit texels (`size` 1/2). The 32-bit split path and 4-bit are
    /// deferred (**open residual R-7**); an unsupported size writes nothing.
    fn load_block<B: VideoBus>(&mut self, hi: u32, lo: u32, bus: &B) {
        let index = ((lo >> 24) & 0x7) as usize;
        let slo = (hi >> 12) & 0xFFF;
        let tlo = hi & 0xFFF;
        let shi = (lo >> 12) & 0xFFF;
        let dxt = lo & 0xFFF;
        // An inverted range writes nothing (as in `load_tile`); without this an
        // extreme `shi < slo` (slo >= 2049) wraps into a valid-looking count.
        if shi < slo {
            return;
        }
        let count = (shi - slo + 1) & 0xFFF;
        if count == 0 || count > LOAD_BLOCK_MAX_TEXELS {
            return; // over the limit: nothing written (§Load Block).
        }
        let tile = self.tiles[index];
        let Some(bpt) = bytes_per_texel(tile.size) else {
            return;
        };
        if tile.size == 3 {
            return; // 32-bit block load (split) deferred — R-7.
        }
        // A 4-bit (or unmapped) source size has no byte stride — bail rather than
        // fall back to the tile stride and read out of bounds (R-7).
        let Some(src_bpt) = bytes_per_texel(self.tex_image_size) else {
            return;
        };
        let tex_w = u32::from(self.tex_image_width);
        let src_base = self
            .tex_image_addr
            .wrapping_add((tex_w * tlo + slo) * src_bpt);
        let tmem_base = u32::from(tile.tmem_addr) * 8;
        for s in 0..count {
            let src = src_base.wrapping_add(s * src_bpt);
            let byte_off = s * bpt;
            // Line parity from the dxt counter over 64-bit TMEM words.
            let word = byte_off / 8;
            let line = (word * dxt) >> 11;
            let swap = (line & 1) << 2;
            let base = tmem_base + byte_off;
            for i in 0..bpt {
                // XOR the swap into each final byte address, not the base.
                self.tmem_write(
                    ((base + i) ^ swap) as usize,
                    bus.rdram_read(src.wrapping_add(i)),
                );
            }
        }
    }

    /// Read one byte of TMEM.
    ///
    /// `offset` is a **byte** address (0..[`TMEM_SIZE`]), masked into the 4 KiB
    /// space — *not* the 64-bit-word address that `Set Tile`'s `tmem_addr` /
    /// `line` use; a word address must be multiplied by 8 first (word 0x100 =
    /// byte 0x800). An unwritten (lazily-unallocated) TMEM reads as zero.
    #[must_use]
    pub fn tmem_byte(&self, offset: usize) -> u8 {
        self.tmem
            .as_ref()
            .map_or(0, |t| t[offset & (TMEM_SIZE - 1)])
    }
}

/// Returns the crate version string.
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    struct NullBus;
    impl RdramBus for NullBus {
        fn rdram_read(&self, _addr: u32) -> u8 {
            0
        }
        fn rdram_write(&mut self, _addr: u32, _val: u8) {}
    }
    impl VideoBus for NullBus {}

    #[test]
    fn empty_fifo_tick_is_noop() {
        let mut rdp = Rdp::new();
        let mut bus = NullBus;
        rdp.tick(&mut bus);
        assert_eq!(rdp.cmd_current, 0);
    }

    #[test]
    fn version_is_non_empty() {
        assert!(!version().is_empty());
    }

    /// **`DPC_STATUS` writes are set/clear commands.** `SET_FREEZE` (0x8) raises
    /// FREEZE; `CLEAR_FREEZE` (0x4) drops it. n64-systemtest's `RDP START & END
    /// REG` freezes the DP precisely so it can poke the registers.
    #[test]
    fn status_write_sets_and_clears_freeze() {
        let mut rdp = Rdp::new();
        rdp.dpc_write(3, 0x8); // SET_FREEZE
        assert_ne!(rdp.dpc_read(3) & DP_STATUS_FREEZE, 0, "freeze set");
        rdp.dpc_write(3, 0x4); // CLEAR_FREEZE
        assert_eq!(rdp.dpc_read(3) & DP_STATUS_FREEZE, 0, "freeze cleared");
    }

    /// **`DPC_START`/`END` mask to a 24-bit, 8-aligned address**, and writing
    /// `END` copies the latched start into `CURRENT`.
    #[test]
    fn start_end_mask_and_current_follows_start() {
        let mut rdp = Rdp::new();
        rdp.dpc_write(3, 0x8); // freeze
        rdp.dpc_write(0, 0x12FF_FFFF); // START
        rdp.dpc_write(1, 0x12FF_FFFF); // END
        assert_eq!(rdp.dpc_read(0), 0x00FF_FFF8, "START masked");
        assert_eq!(rdp.dpc_read(1), 0x00FF_FFF8, "END masked");
        assert_eq!(rdp.dpc_read(2), 0x00FF_FFF8, "CURRENT = START after END");
    }

    /// **The `START_VALID` double-latch.** Writing START sets `START_VALID`; a
    /// second write while valid is *ignored*; writing END consumes it (clears
    /// `START_VALID`, leaves `END_VALID` clear while frozen). This is the exact
    /// sequence `RSP STATUS: start-valid` walks.
    #[test]
    fn start_valid_latch_ignores_a_second_start_write() {
        let mut rdp = Rdp::new();
        rdp.dpc_write(3, 0x8); // freeze
        assert_eq!(rdp.dpc_read(3) & DP_STATUS_START_VALID, 0, "clear at entry");

        rdp.dpc_write(0, 0x1238); // START
        assert_ne!(
            rdp.dpc_read(3) & DP_STATUS_START_VALID,
            0,
            "set after write"
        );
        assert_eq!(rdp.dpc_read(0), 0x1238);

        rdp.dpc_write(0, 0x12_3450); // ignored while valid
        assert_eq!(rdp.dpc_read(0), 0x1238, "second START write ignored");

        rdp.dpc_write(1, 0x1238); // END consumes the latch
        assert_eq!(rdp.dpc_read(3) & DP_STATUS_START_VALID, 0, "cleared by END");
        assert_eq!(
            rdp.dpc_read(3) & DP_STATUS_END_VALID,
            0,
            "END_VALID clear while frozen"
        );
        assert_eq!(rdp.dpc_read(2), 0x1238, "CURRENT = START");
    }

    /// **An END-only write is an incremental transfer: `CURRENT` is not
    /// rewound.** With `START_VALID` clear (the first transfer already
    /// consumed), writing a new END extends the buffer from where the DMA
    /// stopped — reloading `CURRENT` from `START` would reprocess commands
    /// already transferred (N64brew *Interface*, `DPC_END`: "If `START_PENDING`
    /// is 0, the write is considered an incremental transfer").
    #[test]
    fn an_end_only_write_extends_without_rewinding_current() {
        let mut rdp = Rdp::new();
        rdp.dpc_write(3, 0x8); // freeze
        rdp.dpc_write(0, 0x1000); // START
        rdp.dpc_write(1, 0x1000); // END consumes START -> START_VALID clear
        rdp.cmd_current = 0x1000; // pretend the transfer reached the end

        rdp.dpc_write(1, 0x1040); // incremental END, no new START
        assert_eq!(rdp.dpc_read(1), 0x1040, "END extended");
        assert_eq!(rdp.dpc_read(2), 0x1000, "CURRENT not rewound to START");
    }

    /// **A frozen DP does not advance the FIFO**, so registers stay put even
    /// with `cmd_current < cmd_end`.
    #[test]
    fn a_frozen_dp_does_not_tick() {
        let mut rdp = Rdp::new();
        rdp.status = DP_STATUS_FREEZE;
        rdp.cmd_current = 0x10;
        rdp.cmd_end = 0x40;
        let mut bus = NullBus;
        rdp.tick(&mut bus);
        assert_eq!(rdp.cmd_current, 0x10, "frozen: CURRENT unchanged");
    }

    /// A bus backed by a byte buffer, so the decoder can walk a real command
    /// list out of "RDRAM" and we can assert the pointer lands exactly on
    /// `DPC_END`.
    struct SliceBus {
        mem: Vec<u8>,
        dp_raised: bool,
    }
    impl RdramBus for SliceBus {
        fn rdram_read(&self, addr: u32) -> u8 {
            self.mem.get(addr as usize).copied().unwrap_or(0)
        }
        fn rdram_write(&mut self, addr: u32, val: u8) {
            if let Some(b) = self.mem.get_mut(addr as usize) {
                *b = val;
            }
        }
    }
    impl VideoBus for SliceBus {
        fn raise_dp_interrupt(&mut self) {
            self.dp_raised = true;
        }
    }

    /// Append a command: its opcode in bits 61:56 of the first word, then
    /// `words` total 64-bit words with the remainder zero-filled. The word count
    /// is supplied **explicitly by the caller**, independent of the production
    /// decoder, so a walk over the buffer is a genuine check of
    /// `command_len_words` rather than a tautology built from it.
    fn push_cmd(buf: &mut Vec<u8>, opcode: u8, words: u32) {
        buf.extend_from_slice(&(u32::from(opcode) << 24).to_be_bytes());
        for _ in 4..words * 8 {
            buf.push(0);
        }
    }

    /// **The decoder consumes every command whole and never desyncs.** A mixed
    /// list exercising all three length classes — a 1-word set-state, a 22-word
    /// shade+texture+z triangle, a no-op, a 2-word texture rectangle, and
    /// `Sync Full` — drains one command per tick and lands `DPC_CURRENT` exactly
    /// on `DPC_END`. The expected lengths are stated here from the N64brew
    /// command map, so a wrong decoder length overshoots or stops short.
    #[test]
    fn decoder_consumes_each_command_whole_without_desync() {
        // (opcode, documented 64-bit-word length) — independent of the decoder.
        let fixtures = [
            (0x3F_u8, 1), // Set Color Image
            (0x0F, 22),   // Fill Triangle (STZ) = shade + texture + z
            (0x00, 1),    // No Operation
            (0x24, 2),    // Texture Rectangle
            (0x29, 1),    // Sync Full
        ];
        let mut mem = Vec::new();
        for &(op, words) in &fixtures {
            push_cmd(&mut mem, op, words);
        }
        let total = u32::try_from(mem.len()).unwrap();
        let mut bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.cmd_end = total;

        let mut ticks = 0u32;
        while rdp.cmd_current < rdp.cmd_end && ticks < 1000 {
            rdp.tick(&mut bus);
            ticks += 1;
        }
        assert_eq!(rdp.cmd_current, total, "consumed exactly to DPC_END");
        assert_eq!(ticks, 5, "one command retired per scheduler tick");
        assert_eq!(rdp.commands_processed, 5, "every command counted");
    }

    /// **A multi-word primitive is consumed in a single tick**, by its full
    /// decoded length — an unimplemented command advances the FIFO past all its
    /// words rather than treating each word as a fresh command.
    #[test]
    fn a_multiword_command_is_consumed_in_one_tick() {
        let mut mem = Vec::new();
        push_cmd(&mut mem, 0x0E, 20); // Fill Triangle (ST) = shade + texture: 20 words
        let mut bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.cmd_end = 20 * 8;
        rdp.tick(&mut bus);
        assert_eq!(rdp.cmd_current, 20 * 8, "whole 20-word triangle at once");
        assert_eq!(rdp.commands_processed, 1);
    }

    /// **A partially-written command is not consumed until it is complete.** If
    /// `DPC_END` lands mid-command — as it does while the `rdpq` microcode fills
    /// the buffer and advances `DPC_END` incrementally — the decoder stalls
    /// rather than executing against unwritten RDRAM, then consumes the command
    /// whole once the rest of its words arrive.
    #[test]
    fn a_partial_command_is_not_consumed_until_complete() {
        let mut mem = Vec::new();
        push_cmd(&mut mem, 0x0F, 22); // 22-word triangle
        let mut bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.cmd_end = 10 * 8; // DPC_END only reached word 10 of 22
        rdp.tick(&mut bus);
        assert_eq!(rdp.cmd_current, 0, "stalled: partial command not consumed");
        assert_eq!(rdp.commands_processed, 0);

        rdp.cmd_end = 22 * 8; // the rest of the command arrives
        rdp.tick(&mut bus);
        assert_eq!(rdp.cmd_current, 22 * 8, "consumed whole once complete");
        assert_eq!(rdp.commands_processed, 1);
    }

    /// **XBUS mode reads commands from DMEM, which is not yet wired**, so the
    /// decoder must not mis-read RDRAM as the command stream. With `XBUS` set it
    /// stalls, leaving `DPC_CURRENT` and the counter untouched.
    #[test]
    fn xbus_mode_does_not_decode_rdram() {
        let mut mem = Vec::new();
        push_cmd(&mut mem, 0x3F, 1);
        let mut bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.status = DP_STATUS_XBUS;
        rdp.cmd_end = 8;
        rdp.tick(&mut bus);
        assert_eq!(rdp.cmd_current, 0, "XBUS: RDRAM not decoded");
        assert_eq!(rdp.commands_processed, 0);
    }

    /// Drive a single command through `tick` and return the resulting state.
    fn run_one(opcode: u8) -> (Rdp, SliceBus) {
        let mut mem = Vec::new();
        push_cmd(&mut mem, opcode, 1);
        let mut bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.cmd_end = 8;
        rdp.tick(&mut bus);
        (rdp, bus)
    }

    /// **`Sync Full` (0x29) raises the DP interrupt.** The dispatcher calls
    /// `raise_dp_interrupt` on the bus, which the live `Bus` turns into
    /// `MI_INTR.dp`; here the test bus records the raise.
    #[test]
    fn sync_full_raises_the_dp_interrupt() {
        let (rdp, bus) = run_one(OP_SYNC_FULL);
        assert!(bus.dp_raised, "Sync Full raised the DP interrupt");
        assert_eq!(rdp.commands_processed, 1);
        assert_eq!(rdp.stall, 0, "Sync Full does not stall the pipeline itself");
    }

    /// **The other sync commands do not raise an interrupt** — only `Sync Full`
    /// does. They each set the documented fixed pipeline stall instead.
    #[test]
    fn sync_load_pipe_tile_set_the_documented_stall() {
        for (opcode, expected) in [
            (OP_SYNC_LOAD, SYNC_LOAD_GCLK),
            (OP_SYNC_PIPE, SYNC_PIPE_GCLK),
            (OP_SYNC_TILE, SYNC_TILE_GCLK),
        ] {
            let (rdp, bus) = run_one(opcode);
            assert!(!bus.dp_raised, "opcode {opcode:#04x} raised no interrupt");
            assert_eq!(rdp.stall, expected, "opcode {opcode:#04x} stall cycles");
        }
    }

    /// **A sync stall holds the FIFO for exactly its GCLK count.** After a
    /// `Sync Pipe` (50 GCLK) the next command is not consumed until 50 further
    /// ticks have elapsed — the pipeline is unavailable for exactly that long,
    /// as the command is an unconditional fixed-length stall.
    #[test]
    fn a_sync_pipe_stall_holds_the_fifo_for_50_gclk() {
        let mut mem = Vec::new();
        push_cmd(&mut mem, OP_SYNC_PIPE, 1); // sets stall = 50
        push_cmd(&mut mem, 0x00, 1); // a following no-op
        let mut bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.cmd_end = 16;

        rdp.tick(&mut bus); // consumes Sync Pipe, sets stall = 50
        assert_eq!(rdp.commands_processed, 1);
        assert_eq!(rdp.stall, SYNC_PIPE_GCLK);

        // The next 50 ticks burn the stall and do not advance the FIFO.
        for i in 0..SYNC_PIPE_GCLK {
            rdp.tick(&mut bus);
            assert_eq!(rdp.commands_processed, 1, "still stalled at tick {i}");
            assert_eq!(rdp.stall, SYNC_PIPE_GCLK - 1 - i);
        }
        // Stall expired: the following command is consumed on the next tick.
        rdp.tick(&mut bus);
        assert_eq!(rdp.commands_processed, 2, "FIFO resumes after the stall");
    }

    /// **A frozen DP does not burn stall cycles.** The freeze guard is checked
    /// before the stall countdown, so a non-zero `stall` is held — not
    /// decremented — while frozen, and resumes counting down only once the DP is
    /// unfrozen. The plain `a_frozen_dp_does_not_tick` test leaves `stall` at
    /// zero and so cannot catch a regression that decremented it under freeze.
    #[test]
    fn a_frozen_dp_holds_its_stall_countdown() {
        let mut rdp = Rdp::new();
        let mut bus = NullBus;
        rdp.stall = 10;
        rdp.status = DP_STATUS_FREEZE;
        rdp.tick(&mut bus);
        assert_eq!(rdp.stall, 10, "frozen: stall countdown held, not burned");

        rdp.status = 0; // unfreeze
        rdp.tick(&mut bus);
        assert_eq!(rdp.stall, 9, "unfrozen: countdown resumes");
    }

    /// **A preceding stall delays the `Sync Full` interrupt.** With `Sync Pipe`
    /// (50 GCLK) queued before `Sync Full`, the DP interrupt stays low for all
    /// 50 stall ticks and rises only once the stall drains and `Sync Full` is
    /// dispatched — the stall-before-interrupt ordering the dispatch doc claims.
    /// (Were the stall gate absent, `Sync Full` would dispatch on the very next
    /// tick and the interrupt would rise during the loop.)
    #[test]
    fn a_preceding_stall_delays_the_sync_full_interrupt() {
        let mut mem = Vec::new();
        push_cmd(&mut mem, OP_SYNC_PIPE, 1);
        push_cmd(&mut mem, OP_SYNC_FULL, 1);
        let mut bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.cmd_end = 16;

        rdp.tick(&mut bus); // consume Sync Pipe -> stall = 50
        assert_eq!(rdp.stall, SYNC_PIPE_GCLK);
        assert!(!bus.dp_raised, "no interrupt while the stall is set");

        for i in 0..SYNC_PIPE_GCLK {
            rdp.tick(&mut bus);
            assert!(!bus.dp_raised, "interrupt still low during stall tick {i}");
        }
        // Stall drained: the next tick dispatches Sync Full and raises.
        rdp.tick(&mut bus);
        assert!(
            bus.dp_raised,
            "interrupt raised only after the stall drains"
        );
        assert_eq!(rdp.commands_processed, 2);
    }

    // --- The FILL pipeline (T-31-003) ---

    // The command list lives here; the color image is based at RDRAM 0, well
    // below it, so the two never overlap in the shared test buffer.
    const CMD_BASE: u32 = 0x4000;

    fn push_word(buf: &mut Vec<u8>, hi: u32, lo: u32) {
        buf.extend_from_slice(&hi.to_be_bytes());
        buf.extend_from_slice(&lo.to_be_bytes());
    }

    // Command builders. Screen coordinates are given in whole pixels; the wire
    // format is u10.2, so each is shifted left by two.
    fn set_color_image(format: u32, size: u32, width: u32, addr: u32) -> (u32, u32) {
        let hi =
            (u32::from(OP_SET_COLOR_IMAGE) << 24) | (format << 21) | (size << 19) | (width - 1);
        (hi, addr)
    }
    fn set_fill_color(color: u32) -> (u32, u32) {
        (u32::from(OP_SET_FILL_COLOR) << 24, color)
    }
    fn set_scissor(ulx: u32, uly: u32, lrx: u32, lry: u32) -> (u32, u32) {
        let hi = (u32::from(OP_SET_SCISSOR) << 24) | (ulx << 14) | (uly << 2);
        let lo = (lrx << 14) | (lry << 2);
        (hi, lo)
    }
    fn fill_rect(ulx: u32, uly: u32, lrx: u32, lry: u32) -> (u32, u32) {
        let hi = (u32::from(OP_FILL_RECTANGLE) << 24) | (lrx << 14) | (lry << 2);
        let lo = (ulx << 14) | (uly << 2);
        (hi, lo)
    }

    /// Run a command list through the FIFO (color image at RDRAM 0, commands at
    /// `CMD_BASE`) and return the RDP plus the memory the fill wrote into.
    fn run_commands(words: &[(u32, u32)]) -> (Rdp, SliceBus) {
        let mut mem = alloc::vec![0u8; CMD_BASE as usize + words.len() * 8];
        let mut list = Vec::new();
        for &(hi, lo) in words {
            push_word(&mut list, hi, lo);
        }
        mem[CMD_BASE as usize..CMD_BASE as usize + list.len()].copy_from_slice(&list);
        let mut bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.cmd_current = CMD_BASE;
        rdp.cmd_end = CMD_BASE + u32::try_from(list.len()).unwrap();
        let mut guard = 0;
        while rdp.cmd_current < rdp.cmd_end && guard < 10_000 {
            rdp.tick(&mut bus);
            guard += 1;
        }
        (rdp, bus)
    }

    /// **`Set Color Image` parses format, size, width, and address.** Width is
    /// the encoded field plus one; the address is masked to 24 bits.
    #[test]
    fn set_color_image_parses_its_fields() {
        let (rdp, _) = run_commands(&[set_color_image(0, 3, 320, 0x0010_0000)]);
        assert_eq!(rdp.color_image_format, 0);
        assert_eq!(rdp.color_image_size, 3);
        assert_eq!(rdp.color_image_width, 320);
        assert_eq!(rdp.color_image, 0x0010_0000);
    }

    /// **`Set Fill Color` and `Set Scissor` store their values.**
    #[test]
    fn set_fill_color_and_scissor_store_state() {
        let (rdp, _) = run_commands(&[set_fill_color(0xDEAD_BEEF), set_scissor(2, 3, 6, 7)]);
        assert_eq!(rdp.fill_color, 0xDEAD_BEEF);
        assert_eq!(rdp.scissor_ulx, 2 << 2);
        assert_eq!(rdp.scissor_uly, 3 << 2);
        assert_eq!(rdp.scissor_lrx, 6 << 2);
        assert_eq!(rdp.scissor_lry, 7 << 2);
    }

    /// **A 32-bit FILL writes the whole colour to every pixel**, four bytes
    /// each, big-endian — the memory is the fill value repeated verbatim.
    #[test]
    fn fill_rectangle_32bpp_writes_the_colour_verbatim() {
        let (_, bus) = run_commands(&[
            set_color_image(0, 3, 4, 0), // 32-bit, width 4, base 0
            set_fill_color(0xAABB_CCDD),
            set_scissor(0, 0, 4, 2),
            fill_rect(0, 0, 4, 2),
        ]);
        // 4 px * 2 rows * 4 bytes = 32 bytes, all AA BB CC DD.
        for chunk in bus.mem[0..32].chunks_exact(4) {
            assert_eq!(chunk, [0xAA, 0xBB, 0xCC, 0xDD]);
        }
        assert_eq!(bus.mem[32], 0, "nothing written past the rectangle");
    }

    /// **A 16-bit FILL alternates the colour's halves per pixel** — even pixels
    /// take the upper 16 bits, odd pixels the lower — so memory is still the
    /// 32-bit value repeated.
    #[test]
    fn fill_rectangle_16bpp_alternates_halves() {
        let (_, bus) = run_commands(&[
            set_color_image(0, 2, 4, 0), // 16-bit, width 4
            set_fill_color(0xAABB_CCDD),
            set_scissor(0, 0, 4, 1),
            fill_rect(0, 0, 4, 1),
        ]);
        // px0 even -> AABB, px1 odd -> CCDD, px2 -> AABB, px3 -> CCDD.
        assert_eq!(
            bus.mem[0..8],
            [0xAA, 0xBB, 0xCC, 0xDD, 0xAA, 0xBB, 0xCC, 0xDD]
        );
    }

    /// **An 8-bit FILL writes one of the four colour bytes per pixel**, cycling
    /// every four pixels.
    #[test]
    fn fill_rectangle_8bpp_cycles_four_bytes() {
        let (_, bus) = run_commands(&[
            set_color_image(4, 1, 4, 0), // 8-bit (I8), width 4
            set_fill_color(0xAABB_CCDD),
            set_scissor(0, 0, 4, 1),
            fill_rect(0, 0, 4, 1),
        ]);
        assert_eq!(bus.mem[0..4], [0xAA, 0xBB, 0xCC, 0xDD]);
    }

    /// **The scissor clips the fill on all four edges.** A rectangle larger than
    /// the scissor only writes the scissored region; the right and lower edges
    /// are exclusive, so the boundary pixels just outside stay clear.
    #[test]
    fn fill_rectangle_is_clipped_to_the_scissor() {
        // 32-bit, width 8. Scissor keeps x in [2,6), y in [1,3).
        let (_, bus) = run_commands(&[
            set_color_image(0, 3, 8, 0),
            set_fill_color(0x1122_3344),
            set_scissor(2, 1, 6, 3),
            fill_rect(0, 0, 8, 4), // larger than the scissor on every side
        ]);
        let px = |x: u32, y: u32| {
            let a = (y * 8 + x) as usize * 4;
            &bus.mem[a..a + 4]
        };
        // Inside the scissor: written.
        assert_eq!(px(2, 1), [0x11, 0x22, 0x33, 0x44], "inside top-left");
        assert_eq!(px(5, 2), [0x11, 0x22, 0x33, 0x44], "inside bottom-right");
        // Outside each edge: clear.
        assert_eq!(px(1, 1), [0, 0, 0, 0], "left of scissor");
        assert_eq!(px(6, 1), [0, 0, 0, 0], "right edge exclusive");
        assert_eq!(px(2, 0), [0, 0, 0, 0], "above scissor");
        assert_eq!(px(2, 3), [0, 0, 0, 0], "lower edge exclusive");
    }

    /// **A 4-bit color image is not a FILL target** — the real RDP crashes, so
    /// the fill is skipped and no memory is written.
    #[test]
    fn fill_rectangle_4bit_target_writes_nothing() {
        let (_, bus) = run_commands(&[
            set_color_image(0, 0, 4, 0), // 4-bit
            set_fill_color(0xFFFF_FFFF),
            set_scissor(0, 0, 4, 2),
            fill_rect(0, 0, 4, 2),
        ]);
        assert!(bus.mem[0..16].iter().all(|&b| b == 0), "no fill at 4-bit");
    }

    /// **Degenerate and empty rectangles write nothing.** An inverted rectangle
    /// (`ulx > lrx`), an inverted scissor (`ul* > lr*`), and a rectangle disjoint
    /// from the scissor each yield an empty pixel span, so no pixel is written.
    /// The span emptiness — not the early-return, which is a redundant fast path
    /// over Rust's empty `for` ranges — is what these assert.
    #[test]
    fn fill_rectangle_degenerate_bounds_write_nothing() {
        // Inverted rectangle: upper-left past lower-right.
        let (_, bus) = run_commands(&[
            set_color_image(0, 3, 8, 0),
            set_fill_color(0xFFFF_FFFF),
            set_scissor(0, 0, 8, 4),
            fill_rect(6, 3, 2, 1), // ulx>lrx, uly>lry
        ]);
        assert!(bus.mem[0..128].iter().all(|&b| b == 0), "inverted rect");

        // Inverted scissor: upper-left past lower-right.
        let (_, bus) = run_commands(&[
            set_color_image(0, 3, 8, 0),
            set_fill_color(0xFFFF_FFFF),
            set_scissor(6, 3, 2, 1), // sulx>slrx, suly>slry
            fill_rect(0, 0, 8, 4),
        ]);
        assert!(bus.mem[0..128].iter().all(|&b| b == 0), "inverted scissor");

        // Rectangle entirely to the left of the scissor: empty intersection.
        let (_, bus) = run_commands(&[
            set_color_image(0, 3, 8, 0),
            set_fill_color(0xFFFF_FFFF),
            set_scissor(4, 0, 8, 4),
            fill_rect(0, 0, 3, 4), // rx1 = 3 <= scissor sx0 = 4
        ]);
        assert!(bus.mem[0..128].iter().all(|&b| b == 0), "disjoint rect");
    }

    /// **A fill with no configured width writes nothing.** `color_image_width`
    /// is 0 only before `Set Color Image` (which always yields field + 1 ≥ 1);
    /// with a valid pixel size but zero width the guard skips the fill rather
    /// than smearing every row onto offset 0 with a zero stride. Reached here by
    /// setting the state directly, since the command stream cannot produce it.
    #[test]
    fn fill_rectangle_without_a_valid_width_writes_nothing() {
        let mut mem = alloc::vec![0u8; CMD_BASE as usize + 8];
        let (hi, lo) = fill_rect(0, 0, 4, 4);
        mem[CMD_BASE as usize..CMD_BASE as usize + 4].copy_from_slice(&hi.to_be_bytes());
        mem[CMD_BASE as usize + 4..CMD_BASE as usize + 8].copy_from_slice(&lo.to_be_bytes());
        let mut bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.color_image_size = 3; // valid 32-bit size, but width left at 0
        rdp.fill_color = 0xFFFF_FFFF;
        rdp.scissor_lrx = 4 << 2;
        rdp.scissor_lry = 4 << 2;
        rdp.cmd_current = CMD_BASE;
        rdp.cmd_end = CMD_BASE + 8;
        rdp.tick(&mut bus);
        assert!(bus.mem[0..64].iter().all(|&b| b == 0), "width 0: no write");
    }

    // ---- T-32-001: texture state (TMEM, tile descriptors, state commands) ----

    /// **TMEM is zero at power-on and unallocated.** The lazy `Option<Box<..>>`
    /// reads as all-zero while `None`, so a fresh RDP sees a blank TMEM without
    /// having paid a 4 KiB allocation.
    #[test]
    fn tmem_is_zero_and_unallocated_at_power_on() {
        let rdp = Rdp::new();
        assert!(rdp.tmem.is_none(), "no TMEM box allocated at power-on");
        assert_eq!(rdp.tmem_byte(0), 0);
        assert_eq!(rdp.tmem_byte(0x800), 0, "high half zero too");
        assert_eq!(rdp.tmem_byte(TMEM_SIZE - 1), 0, "last byte zero");
    }

    /// **`Set Tile` (0x35) decodes every field into the addressed descriptor.**
    /// Each field is seeded with a distinct value so a swapped bit range shows
    /// up as a wrong field, not a coincidental match. Word built here from the
    /// N64brew field table, independent of the decoder.
    #[test]
    fn set_tile_decodes_all_fields() {
        // format=3 size=2 line=0x1F addr=0x100 | index=5 palette=0xA
        // clamp_t=1 mirror_t=0 mask_t=7 shift_t=3 | clamp_s=0 mirror_s=1
        // mask_s=5 shift_s=9
        let mut rdp = Rdp::new();
        rdp.set_tile(0x3570_3F00, 0x05A9_CD59);
        let expected = TileDescriptor {
            format: 3,
            size: 2,
            line: 0x1F,
            tmem_addr: 0x100,
            palette: 0xA,
            clamp_t: true,
            mirror_t: false,
            mask_t: 7,
            shift_t: 3,
            clamp_s: false,
            mirror_s: true,
            mask_s: 5,
            shift_s: 9,
            sl: 0,
            tl: 0,
            sh: 0,
            th: 0,
        };
        assert_eq!(rdp.tiles[5], expected, "descriptor 5 fully decoded");
        // Only the addressed descriptor is touched.
        for (i, t) in rdp.tiles.iter().enumerate() {
            if i != 5 {
                assert_eq!(*t, TileDescriptor::default(), "tile {i} untouched");
            }
        }
    }

    /// **`Set Tile Size` (0x32) decodes SL/TL/SH/TH for the addressed
    /// descriptor** and leaves the format/addressing fields alone.
    #[test]
    fn set_tile_size_decodes_coords() {
        let mut rdp = Rdp::new();
        // Seed descriptor 2's addressing fields to non-zero first, so the
        // preservation check is real: if Set Tile Size wrongly cleared them this
        // catches it, whereas asserting `== 0` on a fresh descriptor could not.
        rdp.set_tile(0x3570_3F00, 0x02A9_CD59); // index 2: format=3 size=2 line=0x1F addr=0x100
        // SL=0x123 TL=0x045 | index=2 SH=0x678 TH=0x0AB
        rdp.set_tile_size(0x3212_3045, 0x0267_80AB);
        let t = rdp.tiles[2];
        assert_eq!(
            (t.sl, t.tl, t.sh, t.th),
            (0x123, 0x045, 0x678, 0x0AB),
            "coords updated"
        );
        assert_eq!(t.format, 3, "Set Tile Size preserves format");
        assert_eq!(t.size, 2, "preserves size");
        assert_eq!(t.line, 0x1F, "preserves line");
        assert_eq!(t.tmem_addr, 0x100, "preserves tmem_addr");
    }

    /// **`Set Tile` preserves the tile-size coords** — the two commands write
    /// disjoint parts of the same descriptor, so a `Set Tile` after a `Set Tile
    /// Size` must not clear SL/TL/SH/TH (the `..self.tiles[index]` spread).
    #[test]
    fn set_tile_preserves_tile_size_coords() {
        let mut rdp = Rdp::new();
        rdp.set_tile_size(0x3212_3045, 0x0267_80AB); // seeds tiles[2] coords
        rdp.set_tile(0x3570_3F00, 0x02A9_CD59); // index bits -> descriptor 2
        let t = rdp.tiles[2];
        assert_eq!(t.format, 3, "Set Tile applied");
        assert_eq!(
            (t.sl, t.tl, t.sh, t.th),
            (0x123, 0x045, 0x678, 0x0AB),
            "tile-size coords survive a later Set Tile"
        );
    }

    /// **`Set Texture Image` (0x3D) decodes format/size/width/addr.** Same field
    /// layout as Set Color Image; width is a field+1 pixel count.
    #[test]
    fn set_texture_image_decodes_fields() {
        // format=4 size=1 width_field=0x13F (-> 0x140) addr=0x654321
        let mut rdp = Rdp::new();
        let mut bus = NullBus;
        rdp.dispatch(OP_SET_TEXTURE_IMAGE, 0x3D88_013F, 0x0065_4321, &mut bus);
        assert_eq!(rdp.tex_image_format, 4);
        assert_eq!(rdp.tex_image_size, 1);
        assert_eq!(rdp.tex_image_width, 0x140, "width is field + 1");
        assert_eq!(rdp.tex_image_addr, 0x0065_4321);
    }

    /// **The dispatcher routes 0x35 and 0x32 to the right handlers.** The
    /// field-level tests call `set_tile` / `set_tile_size` directly; this drives
    /// the actual `dispatch` entry point, so a mis-wired opcode arm (0x35 sent to
    /// the size handler, or 0x32 to the tile handler) is caught, not only a
    /// decode bug. The inputs distinguish the two: `Set Tile` sets `format = 3`
    /// which `Set Tile Size` must leave alone, and `Set Tile Size` sets coords
    /// that `Set Tile` does not.
    #[test]
    fn dispatch_routes_set_tile_and_set_tile_size() {
        let mut rdp = Rdp::new();
        let mut bus = NullBus;
        rdp.dispatch(OP_SET_TILE, 0x3570_3F00, 0x02A9_CD59, &mut bus); // index 2
        assert_eq!(rdp.tiles[2].format, 3, "0x35 routed to set_tile");
        assert_eq!(rdp.tiles[2].line, 0x1F, "and decoded its fields");
        rdp.dispatch(OP_SET_TILE_SIZE, 0x3212_3045, 0x0267_80AB, &mut bus); // index 2
        assert_eq!(
            (rdp.tiles[2].sl, rdp.tiles[2].th),
            (0x123, 0x0AB),
            "0x32 routed to set_tile_size"
        );
        assert_eq!(
            rdp.tiles[2].format, 3,
            "set_tile_size did not clobber the tile format (distinct routing)"
        );
    }

    // ---- T-32-002: TMEM loads (Load Tile 0x34, Load Block 0x33) ----

    /// **`Load Tile` copies a 16-bit row and latches the tile size.** Row 0 is
    /// even, so no odd-row swap applies; the four texels land verbatim, and the
    /// descriptor's `SL/TL/SH/TH` are updated for rendering.
    #[test]
    fn load_tile_16bit_copies_a_row_and_sets_size() {
        let mut mem = alloc::vec![0u8; 0x200];
        let src = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
        mem[0x100..0x108].copy_from_slice(&src);
        let bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.tiles[0].size = 2; // 16-bit
        rdp.tiles[0].line = 1; // 8 bytes/row
        rdp.tex_image_size = 2;
        rdp.tex_image_width = 4;
        rdp.tex_image_addr = 0x100;
        // SL=0 TL=0 index=0 SH=3 (field 0xC) TH=0.
        rdp.load_tile(0x0000_0000, 0x0000_C000, &bus);
        for (i, &b) in src.iter().enumerate() {
            assert_eq!(rdp.tmem_byte(i), b, "tmem[{i}]");
        }
        assert_eq!(
            (rdp.tiles[0].sl, rdp.tiles[0].sh),
            (0, 0xC),
            "Load Tile latches the tile size"
        );
    }

    /// **`Load Tile` applies the odd-row 32-bit-word swap.** Row 1 is odd, so each
    /// texel's TMEM byte address gains bit 2 (a `^ 4`), swapping the two 32-bit
    /// halves of the 64-bit word, while row 0 is unswapped.
    #[test]
    fn load_tile_swaps_odd_rows() {
        let mut mem = alloc::vec![0u8; 0x200];
        let row0 = [0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7];
        let row1 = [0xB0, 0xB1, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7];
        mem[0x100..0x108].copy_from_slice(&row0);
        mem[0x108..0x110].copy_from_slice(&row1);
        let bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.tiles[0].size = 2;
        rdp.tiles[0].line = 1;
        rdp.tex_image_size = 2;
        rdp.tex_image_width = 4;
        rdp.tex_image_addr = 0x100;
        // SL=0 TL=0 SH=3 TH=1 (field 4) -> 4x2.
        rdp.load_tile(0x0000_0000, 0x0000_C004, &bus);
        for (i, &b) in row0.iter().enumerate() {
            assert_eq!(rdp.tmem_byte(i), b, "row 0 verbatim [{i}]");
        }
        // Row 1 base = line*8 = 8; texel s at (8 + s*2) ^ 4.
        assert_eq!((rdp.tmem_byte(0xC), rdp.tmem_byte(0xD)), (0xB0, 0xB1));
        assert_eq!((rdp.tmem_byte(0xE), rdp.tmem_byte(0xF)), (0xB2, 0xB3));
        assert_eq!((rdp.tmem_byte(0x8), rdp.tmem_byte(0x9)), (0xB4, 0xB5));
        assert_eq!((rdp.tmem_byte(0xA), rdp.tmem_byte(0xB)), (0xB6, 0xB7));
    }

    /// **`Load Tile` splits a 32-bit texel across TMEM.** R,G go to the low half
    /// and B,A to the high half (offset 0x800) — the documented 32-bit layout.
    #[test]
    fn load_tile_32bit_splits_rg_low_ba_high() {
        let mut mem = alloc::vec![0u8; 0x200];
        mem[0x100..0x104].copy_from_slice(&[0x11, 0x22, 0x33, 0x44]); // R G B A
        let bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.tiles[0].size = 3; // 32-bit RGBA
        rdp.tiles[0].line = 1;
        rdp.tex_image_size = 3;
        rdp.tex_image_width = 1;
        rdp.tex_image_addr = 0x100;
        rdp.load_tile(0x0000_0000, 0x0000_0000, &bus); // 1x1
        assert_eq!(rdp.tmem_byte(0), 0x11, "R low half");
        assert_eq!(rdp.tmem_byte(1), 0x22, "G low half");
        assert_eq!(rdp.tmem_byte(0x800), 0x33, "B high half");
        assert_eq!(rdp.tmem_byte(0x801), 0x44, "A high half");
    }

    /// **`Load Block` streams texels and pins both sides of the 2048 limit.** A
    /// small 8-bit block copies verbatim (dxt 0 → one line, no swap); a 2049-texel
    /// load writes nothing, while exactly 2048 loads.
    #[test]
    fn load_block_streams_and_enforces_the_limit() {
        let mut mem = alloc::vec![0u8; 0x200];
        mem[0x100..0x104].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        let bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.tiles[0].size = 1; // 8-bit
        rdp.tex_image_size = 1;
        rdp.tex_image_width = 4;
        rdp.tex_image_addr = 0x100;
        // SL=0 index=0 SH=3 dxt=0.
        rdp.load_block(0x0000_0000, 0x0000_3000, &bus);
        for (i, &b) in [0xDE, 0xAD, 0xBE, 0xEF].iter().enumerate() {
            assert_eq!(rdp.tmem_byte(i), b, "block[{i}]");
        }

        // Over the limit (count = 2049): nothing written / allocated.
        let mut over = Rdp::new();
        over.tiles[0].size = 1;
        over.tex_image_size = 1;
        over.tex_image_width = 4096;
        over.tex_image_addr = 0x100;
        over.load_block(0x0000_0000, 0x0080_0000, &bus); // SH field 2048
        assert!(
            over.tmem.is_none(),
            "a 2049-texel load writes nothing (over the 2048 limit)"
        );

        // Exactly at the limit (count = 2048): loads.
        let mut edge = Rdp::new();
        edge.tiles[0].size = 1;
        edge.tex_image_size = 1;
        edge.tex_image_width = 4096;
        edge.tex_image_addr = 0x100;
        edge.load_block(0x0000_0000, 0x007F_F000, &bus); // SH field 2047 -> 2048 texels
        assert!(edge.tmem.is_some(), "exactly 2048 texels loads");
        assert_eq!(edge.tmem_byte(0), 0xDE, "and writes the first texel");
    }

    /// **`Load Block` uses dxt to swap odd lines.** With `dxt = 0x800` the line
    /// index `(word * dxt) >> 11` is odd for odd 64-bit words, so the second group
    /// of four 16-bit texels (word 1) is swapped while the first (word 0) is not.
    #[test]
    fn load_block_dxt_swaps_odd_lines() {
        let mut mem = alloc::vec![0u8; 0x200];
        // 8 x 16-bit texels = 16 bytes: word 0 = texels 0..4, word 1 = texels 4..8.
        let data: [u8; 16] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x40, 0x41, 0x42, 0x43, 0x44, 0x45,
            0x46, 0x47,
        ];
        mem[0x100..0x110].copy_from_slice(&data);
        let bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.tiles[0].size = 2; // 16-bit
        rdp.tex_image_size = 2;
        rdp.tex_image_width = 8;
        rdp.tex_image_addr = 0x100;
        // SL=0 index=0 SH=7 (field 7<<... u12.0 so field=7) dxt=0x800.
        rdp.load_block(0x0000_0000, 0x0000_7800, &bus);
        // Word 0 (texels 0..4) unswapped at bytes 0..8.
        for (i, &b) in data[..8].iter().enumerate() {
            assert_eq!(rdp.tmem_byte(i), b, "word0[{i}] unswapped");
        }
        // Word 1 (texels 4..8) swapped: byte_off (8..16) ^ 4.
        for (i, &b) in data[8..].iter().enumerate() {
            assert_eq!(rdp.tmem_byte((8 + i) ^ 4), b, "word1 byte {i} swapped");
        }
    }

    /// **An unsupported (4-bit) texel size writes nothing.** Both loads bail on a
    /// `size` of 0 (ledger R-7) — a silent no-op is invisible to a "does not
    /// panic" test, so this asserts the *effect*: TMEM is never allocated. Source
    /// bytes are present, so a load that ran would allocate and write.
    #[test]
    fn load_with_unsupported_size_writes_nothing() {
        let mut mem = alloc::vec![0u8; 0x200];
        mem[0x100..0x108].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let bus = SliceBus {
            mem,
            dp_raised: false,
        };
        // 4-bit tile (size 0): Load Tile writes nothing.
        let mut a = Rdp::new();
        a.tiles[0].size = 0;
        a.tex_image_size = 0;
        a.tex_image_width = 4;
        a.tex_image_addr = 0x100;
        a.load_tile(0x0000_0000, 0x0000_C000, &bus);
        assert!(a.tmem.is_none(), "4-bit Load Tile allocates/writes nothing");
        // 4-bit tile: Load Block writes nothing.
        let mut b = Rdp::new();
        b.tiles[0].size = 0;
        b.tex_image_size = 0;
        b.tex_image_width = 4;
        b.tex_image_addr = 0x100;
        b.load_block(0x0000_0000, 0x0000_3000, &bus);
        assert!(
            b.tmem.is_none(),
            "4-bit Load Block allocates/writes nothing"
        );
    }

    /// **An inverted range writes nothing.** `SH < SL` (like `TH < TL`) is a
    /// degenerate command; without the guard the `& 0xFFF` wrap would iterate a
    /// large bogus width. Asserts the effect: TMEM stays unallocated.
    #[test]
    fn load_tile_rejects_inverted_range() {
        let mut mem = alloc::vec![0u8; 0x200];
        mem[0x100..0x110].copy_from_slice(&[0xFF; 16]);
        let bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.tiles[0].size = 2;
        rdp.tiles[0].line = 1;
        rdp.tex_image_size = 2;
        rdp.tex_image_width = 8;
        rdp.tex_image_addr = 0x100;
        // SL=10 (field 10<<2=0x28) SH=2 (field 2<<2=8): sh < sl.
        rdp.load_tile(0x0002_8000, 0x0000_8000, &bus);
        assert!(
            rdp.tmem.is_none(),
            "sh < sl writes nothing rather than a wrapped-width rectangle"
        );
    }
}
