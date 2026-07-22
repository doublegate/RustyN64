//! `rustyn64-rdp` — RDP (Reality Display Processor), the RCP rasterizer.
//!
//! The RDP consumes a command stream (from the RSP or the CPU via the DP FIFO)
//! and rasterizes triangles/rectangles into a framebuffer in RDRAM, running the
//! color-combiner + blender + Z/coverage pipeline. The Video Interface (VI)
//! then scans that framebuffer out. The accuracy bar is **LLE** — a faithful
//! per-pixel pipeline (the ParaLLEl-RDP / angrylion reference), not a
//! triangle-list HLE.
//!
//! The DP FIFO is **decoded but not yet rasterized**: [`Rdp::tick`] recognises
//! every command `0x00`–`0x3F` and consumes each one's full length (via
//! [`command`]), so the stream stays aligned — but no primitive is drawn yet.
//! The rasterizer proper (edge walking, the texture engine with TMEM, the
//! combiner/blender, dithering, coverage AA) is the rest of this roadmap phase.
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

/// RDP state (skeleton).
///
/// Holds the command-FIFO pointers, the current render mode (other-modes),
/// scissor rectangle, and the color-image / Z-image RDRAM addresses. TMEM and
/// the per-tile descriptors come with the texture engine in a later phase.
#[derive(Debug, Default, Clone)]
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
    /// Color-image (framebuffer) base in RDRAM (`SET_COLOR_IMAGE`).
    pub color_image: u32,
    /// Z-image base in RDRAM (`SET_Z_IMAGE`).
    pub z_image: u32,
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
    // TODO(T-31-003): the fill pipeline — TMEM (4 KiB), the 8 tile descriptors,
    // other-modes bits, the combiner + blend-mode latches, the scissor rect —
    // see `docs/rdp.md`.
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
    /// Dispatch so far (`dispatch`) covers the four sync commands: `Sync
    /// Load`/`Pipe`/`Tile` set the fixed pipeline stall that gates the next
    /// command, and `Sync Full` raises the DP interrupt. Everything else is
    /// still recognised-and-consumed only.
    // TODO(T-31-003): dispatch the fill pipeline — Set Color Image, Set Fill
    // Color, Set Scissor, Fill Rectangle — into `color_image`.
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
        self.cmd_current = self.cmd_current.wrapping_add(len_bytes);
        self.commands_processed = self.commands_processed.wrapping_add(1);
        self.dispatch(opcode, bus);
    }

    /// Act on a just-consumed command. Only the sync commands are handled so
    /// far; every other opcode is a recognised no-op until its handler lands.
    ///
    /// - `Sync Load`/`Pipe`/`Tile` (0x26/0x27/0x28) each stall the pipeline for
    ///   a fixed, unconditional number of GCLK cycles (25/50/33) — the RDP waits
    ///   the full time whether or not the sync was needed, which is why the
    ///   stall is a constant and not a wait on an internal signal.
    /// - `Sync Full` (0x29) waits for staged pipeline/memory work to finish,
    ///   then raises the DP interrupt on the MI. With no asynchronous pipeline
    ///   work modelled yet, "staged work" is already complete, so the interrupt
    ///   is raised immediately; a following `Sync Pipe`-style stall would have
    ///   drained first via the `stall` gate above.
    fn dispatch<B: VideoBus>(&mut self, opcode: u8, bus: &mut B) {
        match opcode {
            OP_SYNC_LOAD => self.stall = SYNC_LOAD_GCLK,
            OP_SYNC_PIPE => self.stall = SYNC_PIPE_GCLK,
            OP_SYNC_TILE => self.stall = SYNC_TILE_GCLK,
            OP_SYNC_FULL => bus.raise_dp_interrupt(),
            _ => {}
        }
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
}
