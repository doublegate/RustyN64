//! The Bus owns everything mutable.
//!
//! RDRAM (main work RAM), the RSP, the RDP, the AI (audio), the cart (→ PI), the
//! controllers (→ SI), and the RCP interface register blocks
//! (SP / DP / VI / AI / PI / SI / RI / MI). The CPU borrows `&mut Bus` during
//! `tick()`. The RDP and AI see narrower bus traits
//! ([`rustyn64_rdp::VideoBus`], [`rustyn64_audio::AudioBus`]) which the Bus
//! implements. See `docs/architecture.md` (the load-bearing facts).
//!
//! Per the `TetaNES` postmortem (carried over from `RustyNES`): one owner for
//! all mutable state avoids the "CPU holds the RSP/RDP, but they also need the
//! CPU's memory bus"
//! borrow-checker fight. Each chip sees only the smaller trait it actually needs.

// The MI interrupt block is a row of orthogonal hardware-latch booleans that map
// 1:1 to real RCP IRQ lines; collapsing them into an enum would obscure the model.
#![allow(clippy::struct_excessive_bools)]
// Address math truncates by design when narrowing 32-bit physical addresses.
#![allow(clippy::cast_possible_truncation)]

use rustyn64_audio::{Audio, AudioBus};
use rustyn64_cart::{Cart, Cartridge, RdramBus};
use rustyn64_cpu::Bus as CpuBus;
use rustyn64_rdp::{Rdp, VideoBus};
use rustyn64_rsp::Rsp;

use crate::vi::Vi;

/// Base RDRAM size: 4 MiB (8 MiB with the Expansion Pak installed).
pub const RDRAM_SIZE: usize = 8 * 1024 * 1024;

/// The RCP MIPS-interface (MI) interrupt lines. Each bit, when set and unmasked,
/// drives the VR4300 IP2 interrupt. Skeleton — the masking register is a TODO.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MiInterrupt {
    /// SP (RSP) interrupt.
    pub sp: bool,
    /// SI (serial / PIF) interrupt.
    pub si: bool,
    /// AI (audio-buffer-done) interrupt.
    pub ai: bool,
    /// VI (vertical-blank) interrupt.
    pub vi: bool,
    /// PI (peripheral DMA-done) interrupt.
    pub pi: bool,
    /// DP (RDP-done) interrupt.
    pub dp: bool,
}

/// Pack the six interrupt lines into their register bit order.
///
/// `MI_INTERRUPT` and `MI_MASK` share it, which is why one packer serves both.
const fn pack_mi(l: MiInterrupt) -> u32 {
    (l.sp as u32)
        | ((l.si as u32) << 1)
        | ((l.ai as u32) << 2)
        | ((l.vi as u32) << 3)
        | ((l.pi as u32) << 4)
        | ((l.dp as u32) << 5)
}

impl MiInterrupt {
    /// `true` if any interrupt line is asserted.
    #[must_use]
    pub const fn any(self) -> bool {
        self.sp || self.si || self.ai || self.vi || self.pi || self.dp
    }
}

/// The RCP interface register state.
///
/// The SP / DP / VI / AI / PI / SI / RI / MI register blocks the CPU memory-maps
/// in `$0400_0000..$04FF_FFFF`. Skeleton: each is a placeholder for its real
/// register set (a roadmap phase).
#[derive(Clone, Copy, Debug, Default)]
pub struct RcpRegs {
    /// MI — MIPS interface (interrupt lines + mask + RCP version).
    pub mi_intr: MiInterrupt,
    /// MI interrupt mask (a line drives IP2 only when masked-in).
    pub mi_mask: MiInterrupt,
    /// `MI_MODE`'s storage bits (the repeat count and flags).
    pub mi_mode: u32,
    // TODO(T-CORE-02): SP_STATUS/DMA regs, DPC regs, VI_* scanout regs,
    // AI_* regs, PI_* DMA regs, SI_* joybus regs, RI_* RDRAM-controller regs.
}

/// Everything mutable lives here — the single owner.
pub struct Bus {
    /// Main system RDRAM (boxed slice: 8 MiB, heap-allocated without a stack
    /// temporary).
    pub rdram: alloc::boxed::Box<[u8]>,
    /// The PI DMA engine (T-14-001), pulled forward from Phase 5 because
    /// n64-systemtest loads the rest of its own ELF through it.
    pub pi: rustyn64_cart::pi::Pi,
    // DMEM and IMEM are **not** here: the RSP owns them (`Bus::rsp`), and this
    // Bus reaches them through `Rsp::mem_read`/`mem_write`. They were a separate
    // `spmem` slice on the Bus while the RSP was a stub, which meant the CPU and
    // the RSP addressed two different memories that happened to start equal.
    /// The `ISViewer` buffer, as guest-visible memory.
    isviewer: alloc::boxed::Box<[u8]>,
    /// Text the guest has flushed through the `ISViewer` channel.
    isviewer_out: alloc::vec::Vec<u8>,
    /// Text the guest has pushed through the **EMUX** `xlog` channel.
    ///
    /// Kept separate from [`Bus::isviewer_output`] deliberately: they are two
    /// independent console paths and n64-systemtest picks whichever the
    /// emulator advertises, so merging them would hide which one is live.
    emux_out: alloc::vec::Vec<u8>,
    /// Set once the guest has issued `EMUX xioctl(EXIT)`.
    emux_exited: bool,
    /// Whether this host advertises the EMUX extensions. **Off by default**:
    /// hardware has none, and offering them changes the guest's control flow.
    emux_enabled: bool,
    /// The value a PI direct-I/O write latched, visible to every PI-bus read
    /// until the write finalises. See [`Bus::pi_tick`].
    pi_write_latch: u32,
    /// RCP cycles remaining before the latched PI write finalises. Zero is idle.
    pi_write_countdown: u32,
    /// The RSP coprocessor.
    pub rsp: Rsp,
    /// The RDP rasterizer.
    pub rdp: Rdp,
    /// The Video Interface register file (`0x0440_0000`) and scan-out.
    pub vi: Vi,
    /// The Audio Interface.
    pub audio: Audio,
    /// The cartridge (PI/SI + saves).
    pub cart: Cart,
    /// The RCP interface register state.
    pub rcp: RcpRegs,
    /// Controller button/stick state, 4 ports (latched by the SI joybus).
    pub controllers: [u32; 4],
    /// Count of RCP chip-steps taken (diagnostic; used by the scheduler test).
    rcp_steps: u64,
}

impl core::fmt::Debug for Bus {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Bus")
            .field("rsp", &self.rsp)
            .field("rdp", &self.rdp)
            .field("audio", &self.audio)
            .field("cart", &self.cart)
            .field("rcp", &self.rcp)
            .field("controllers", &self.controllers)
            .finish_non_exhaustive()
    }
}

impl Default for Bus {
    fn default() -> Self {
        Self {
            // `vec![..].into_boxed_slice()` allocates straight on the heap —
            // no 8 MiB stack temporary (which `Box::new([0; N])` would create).
            pi: rustyn64_cart::pi::Pi::new(),
            isviewer: alloc::vec![0u8; 0x20 + Self::ISVIEWER_LEN].into_boxed_slice(),
            isviewer_out: alloc::vec::Vec::new(),
            emux_out: alloc::vec::Vec::new(),
            emux_exited: false,
            emux_enabled: false,
            pi_write_latch: 0,
            pi_write_countdown: 0,
            rdram: alloc::vec![0u8; RDRAM_SIZE].into_boxed_slice(),
            rsp: Rsp::new(),
            rdp: Rdp::new(),
            vi: Vi::new(),
            audio: Audio::new(),
            cart: Cart::new(),
            rcp: RcpRegs::default(),
            controllers: [0; 4],
            rcp_steps: 0,
        }
    }
}

impl Bus {
    /// Construct at power-on.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Advance the PI's asynchronous write by one RCP cycle.
    ///
    /// # Why a PI write is not immediate
    ///
    /// From N64brew *Memory map* (PI external bus):
    ///
    /// > All writes are performed **asynchronously** by the PI. Making a write
    /// > in this area will in fact just cause the PI to latch the value
    /// > internally, and release the VR4300 immediately. The write will then
    /// > happen in background. [...] While a write is ongoing, further writes
    /// > are ignored, and reads (from any address) return the 32-bit value that
    /// > is being written.
    ///
    /// The PI does not know a device is read-only, so a write into ROM follows
    /// the same path and is simply dropped by the ROM — which is why a value
    /// written to cart ROM is briefly readable and then gone.
    ///
    /// # The duration is bounded by the oracle, not derived from hardware
    ///
    /// How long finalisation takes depends on the PI domain timing registers
    /// (`LAT`/`PWD`/`PGS`/`RLS`), which are not modelled. n64-systemtest bounds
    /// it only *relatively*: the latched value must still be visible after 0
    /// loop iterations and gone after 110. [`Bus::PI_WRITE_CYCLES`] sits inside
    /// those bounds; it is **not** a hardware measurement. Accuracy ledger C-9.
    pub const fn pi_tick(&mut self) {
        if self.pi_write_countdown > 0 {
            self.pi_write_countdown -= 1;
        }
    }

    /// Is a PI direct-I/O write still in flight?
    const fn pi_io_busy(&self) -> bool {
        self.pi_write_countdown > 0
    }

    /// Step the RSP.
    ///
    /// The chip stays **in place**. It used to be moved out with
    /// `core::mem::take` so that `Rsp::tick` could borrow the Bus, under a
    /// comment asserting "No allocation" — which was false: `take` needs
    /// `Default`, and constructing an `Rsp` allocates DMEM and IMEM, so every
    /// RCP step allocated and freed 8 KiB. `Rsp::tick` now *returns* what it
    /// wants done instead of borrowing its owner, so there is nothing to move.
    pub fn rsp_tick(&mut self) {
        let out = self.rsp.tick();
        if let Some(raise) = out.interrupt_change {
            self.rcp.mi_intr.sp = raise;
        }
        if let Some(dma) = out.dma {
            self.sp_dma(dma);
        }
        if let Some((off, val)) = out.dp_write {
            // The RSP's COP0 `c8`–`c15` *are* the RDP command registers; the RSP
            // crate cannot name `Rdp` (crate-graph rule), so it reports the write
            // as a DPC word offset and the Bus carries it out — the same seam the
            // CPU uses at `0x0410_0000`. This is how the rdpq microcode's
            // `mtc0 DP_END` submits a command list to the RDP.
            self.rdp.dpc_write(u32::from(off), val);
        }
        self.rcp_steps = self.rcp_steps.wrapping_add(1);
    }

    /// Step the RDP against this bus's narrow [`VideoBus`] view (split-borrow).
    pub fn rdp_tick(&mut self) {
        let mut rdp = core::mem::take(&mut self.rdp);
        rdp.tick(self);
        self.rdp = rdp;
    }

    /// Step the AI against this bus's narrow [`AudioBus`] view (split-borrow).
    pub fn audio_tick(&mut self) {
        let mut audio = core::mem::take(&mut self.audio);
        audio.tick(self);
        self.audio = audio;
    }

    /// Diagnostic: count of RCP-chip steps taken (RSP ticks). The scheduler's
    /// fractional-divisor test reads this to assert the 3:2 ratio.
    #[must_use]
    pub const fn rcp_steps_for_test(&self) -> u64 {
        self.rcp_steps
    }

    /// Map a CPU physical address into RDRAM (`0..RDRAM_SIZE`), or `None` if it
    /// targets a memory-mapped register region instead.
    /// Base of RSP DMEM. IMEM follows at `+0x1000`.
    pub const SPMEM_BASE: u32 = 0x0400_0000;

    /// RCP cycles a PI direct-I/O write stays latched before finalising.
    ///
    /// **This number is fitted, not measured.** Hardware finalisation depends on
    /// the PI domain timing registers (`LAT`/`PWD`/`PGS`/`RLS`), which are not
    /// modelled; n64-systemtest bounds the latch only relatively (visible after
    /// 0 decay-loop iterations, gone after 110). 100 was the best of the values
    /// tried against the suite.
    ///
    /// Treat that provenance as a warning, not a credential. The suite still
    /// fails `Write32, Read32 (same location)` on its **second** read, where
    /// hardware has finalised and we have not — a gap no single constant closes,
    /// because the real duration is not constant. Modelling the domain registers
    /// is the actual fix. Accuracy ledger C-9.
    pub const PI_WRITE_CYCLES: u32 = 100;

    /// Base of the **MI** register block (`0x0430_0000`).
    pub const MI_BASE: u32 = 0x0430_0000;

    /// `MI_VERSION`, the value *"most consoles report"*.
    ///
    /// Packed `RSP:RDP:RAC:IO`. Other values exist in the wild — `0x0101_0101`
    /// and `0x0201_0202` appear in emulators and docs, iQue reports
    /// `0x0202_b0b0` — so this is a **choice among documented observations**,
    /// not a derived constant. Retail NTSC hardware is what this emulator
    /// models, so it reports what retail hardware reports.
    pub const MI_VERSION_VALUE: u32 = 0x0202_0102;

    /// Is this address in the MI register block?
    ///
    /// The block is four registers, and *"accesses beyond `0x0430 0010` are
    /// mirrored, so only the least significant four bits are taken into account
    /// for address decoding"*. The window itself runs to `0x0440_0000`, where
    /// the VI begins.
    const fn is_mi_register(addr: u32) -> bool {
        addr >= Self::MI_BASE && addr < 0x0440_0000
    }

    /// Read an MI register, after the 4-bit mirroring.
    const fn mi_read(&self, addr: u32) -> u32 {
        let i = self.rcp.mi_intr;
        let m = self.rcp.mi_mask;
        match (addr >> 2) & 3 {
            0 => self.rcp.mi_mode,
            1 => Self::MI_VERSION_VALUE,
            2 => pack_mi(i),
            _ => pack_mi(m),
        }
    }

    /// Write an MI register, after the 4-bit mirroring.
    fn mi_write(&mut self, addr: u32, val: u32) {
        match (addr >> 2) & 3 {
            0 => {
                // Only the bits that are storage are kept. `ClearDP` (bit 11)
                // is an action rather than a mode, and the repeat/EBus/Upper
                // modes are RDRAM-transfer behaviour this emulator does not
                // model -- see the note in `docs/rsp.md`.
                self.rcp.mi_mode = (self.rcp.mi_mode & !0x7F) | (val & 0x7F);
                if val & (1 << 11) != 0 {
                    self.rcp.mi_intr.dp = false;
                }
            }
            // MI_VERSION is read-only, and MI_INTERRUPT is driven by the
            // devices -- a write to either does nothing.
            1 | 2 => {}
            _ => {
                // The mask uses clear/set pairs at `2n` / `2n + 1`, in the same
                // device order as the read layout. Unlike `SP_STATUS`, the wiki
                // does not state what both-bits-at-once does here, so this
                // applies clear before set rather than inventing a rule; if a
                // test ever pins it, it belongs in the ledger.
                let mut m = self.rcp.mi_mask;
                for (bit, line) in [(0u32, 0u32), (1, 1), (2, 2), (3, 3), (4, 4), (5, 5)] {
                    let clear = val & (1 << (bit * 2)) != 0;
                    let set = val & (1 << (bit * 2 + 1)) != 0;
                    let slot = match line {
                        0 => &mut m.sp,
                        1 => &mut m.si,
                        2 => &mut m.ai,
                        3 => &mut m.vi,
                        4 => &mut m.pi,
                        _ => &mut m.dp,
                    };
                    if clear {
                        *slot = false;
                    }
                    if set {
                        *slot = true;
                    }
                }
                self.rcp.mi_mask = m;
            }
        }
    }

    /// Base of the eight SP interface registers (`0x0404_0000`).
    pub const SP_REGS_BASE: u32 = 0x0404_0000;
    /// `SP_STATUS` (`0x0404_0010`), named because tests reach for it directly.
    pub const SP_STATUS: u32 = 0x0404_0010;
    /// `SP_PC` (`0x0408_0000`) — in its own window, not with the other eight.
    pub const SP_PC: u32 = 0x0408_0000;

    /// Is this one of the eight SP interface registers?
    const fn is_sp_register(addr: u32) -> bool {
        addr >= Self::SP_REGS_BASE && addr < Self::SP_REGS_BASE + 0x20
    }

    /// Base of the DP command registers (`0x0410_0000`): START, END, CURRENT,
    /// STATUS, then the (unmodelled) CLOCK/BUSY/PIPE/TMEM counters.
    pub const DP_REGS_BASE: u32 = 0x0410_0000;

    /// Is this one of the eight DP command (`DPC_*`) registers?
    const fn is_dp_register(addr: u32) -> bool {
        addr >= Self::DP_REGS_BASE && addr < Self::DP_REGS_BASE + 0x20
    }

    /// Base of the VI register block (`0x0440_0000`); the AI follows at
    /// `0x0450_0000`.
    pub const VI_REGS_BASE: u32 = 0x0440_0000;

    /// Is this address in the VI register block? The sixteen registers span
    /// `0x0440_0000..0x0440_0040`; the rest of the `0x044x_xxxx` window mirrors
    /// them (four-bit decode), which the word-offset mask in [`Vi::read`]
    /// handles.
    const fn is_vi_register(addr: u32) -> bool {
        addr >= Self::VI_REGS_BASE && addr < 0x0450_0000
    }

    /// Write a VI register; a write to `VI_V_CURRENT` acknowledges the VI
    /// interrupt (`MI_INTR.vi = false`).
    const fn vi_write(&mut self, addr: u32, val: u32) {
        if self.vi.write(addr >> 2, val) {
            self.rcp.mi_intr.vi = false;
        }
    }

    /// Apply a write to the SP register block, performing whatever it starts.
    ///
    /// Two effects can come from one write and they are collected separately:
    /// a length write starts a DMA, and a `SP_STATUS` write can raise or
    /// acknowledge the MI's SP line. Folding them into one return value would
    /// imply they are alternatives, and `SP_STATUS` is reachable by both.
    fn sp_register_write(&mut self, addr: u32, val: u32) {
        let index = (addr >> 2) & 7;
        if index == rustyn64_rsp::sp::reg::STATUS
            && let Some(raise) = rustyn64_rsp::sp::SpRegs::interrupt_change(val)
        {
            self.rcp.mi_intr.sp = raise;
        }
        if let Some(dma) = self.rsp.sp.write(index, val) {
            self.sp_dma(dma);
        }
    }
    /// DMEM + IMEM, 4 KiB each.
    pub const SPMEM_LEN: usize = 0x2000;

    /// End of the SP memory window — where the SP *registers* begin.
    ///
    /// The 8 KiB of real storage repeats for this whole range rather than
    /// ending at `0x0400_2000`; see [`rustyn64_rsp::Rsp::mem_read`] and
    /// accuracy ledger **C-30**, which records the provenance of the mirroring.
    pub const SPMEM_WINDOW_END: u32 = 0x0404_0000;

    /// Is this address in the RSP DMEM/IMEM window?
    const fn is_spmem(addr: u32) -> bool {
        addr >= Self::SPMEM_BASE && addr < Self::SPMEM_WINDOW_END
    }

    /// Is this address handled by a device on the RCP's **internal** bus?
    ///
    /// `0x0400_0000-0x04FF_FFFF`, the range N64brew *Memory map* describes as
    /// dispatched inside the RCP without going to an external bus. What matters
    /// here is the shared consequence: every device in it ignores the access
    /// size (see [`CpuBus::write_sized`]).
    ///
    /// The PI and SI external-bus windows share that size-blindness on hardware
    /// and are deliberately **not** included — the PI already models its own
    /// bus quirks separately, and folding both into one rule without the cart
    /// tests to check it against would be a change made blind. Phase 5.
    const fn is_rcp_internal(addr: u32) -> bool {
        matches!(addr, 0x0400_0000..=0x04FF_FFFF)
    }

    /// Base of the **`ISViewer`** debug window, in cart address space.
    ///
    /// Not real N64 hardware — it is a flashcart/emulator convention that
    /// n64-systemtest uses to report results (`ref-proj/n64-systemtest/src/isviewer.rs`).
    /// The suite probes for it by writing a magic word to the buffer and reading
    /// it back; if the round-trip fails it falls back to a framebuffer console
    /// we cannot read. So this window is what turns "the suite runs" into "the
    /// suite reports".
    pub const ISVIEWER_BASE: u32 = 0x13FF_0000;
    /// Writing this register flushes `len` bytes from the buffer.
    pub const ISVIEWER_WRITE_LEN: u32 = 0x13FF_0014;
    /// The text buffer.
    pub const ISVIEWER_BUF: u32 = 0x13FF_0020;
    /// Bytes of buffer modelled — the suite writes in `0x200` chunks.
    pub const ISVIEWER_LEN: usize = 0x1000;

    /// Is this address inside the `ISViewer` window?
    const fn is_isviewer(addr: u32) -> bool {
        addr >= Self::ISVIEWER_BASE && addr < Self::ISVIEWER_BASE + 0x20 + Self::ISVIEWER_LEN as u32
    }

    /// The raw `ISViewer` backing memory, for diagnostics.
    #[must_use]
    pub fn isviewer_raw(&self) -> &[u8] {
        &self.isviewer
    }

    /// Everything the guest has written to the `ISViewer` channel.
    #[must_use]
    pub fn isviewer_output(&self) -> &[u8] {
        &self.isviewer_out
    }

    /// Text the guest has pushed through the EMUX `xlog` channel.
    #[must_use]
    pub fn emux_output(&self) -> &[u8] {
        &self.emux_out
    }

    /// Offer the EMUX extensions to the guest.
    ///
    /// Opt-in, because hardware has none: enabling this changes which console
    /// backend n64-systemtest selects and therefore the instructions it
    /// executes. Worth it for a test harness (the `xlog` console needs no PI or
    /// `ISViewer` emulation and runs ~9x faster); wrong for anything claiming to
    /// reproduce a real console.
    pub const fn enable_emux(&mut self) {
        self.emux_enabled = true;
    }

    /// Has the guest requested termination via `EMUX xioctl(EXIT)`?
    #[must_use]
    pub const fn emux_exited(&self) -> bool {
        self.emux_exited
    }

    /// Is this address on the **PI external bus** — the memory-mapped window
    /// through which the CPU reaches cart ROM, SRAM and `FlashRAM`?
    ///
    /// Ranges from N64brew *Memory map*: `0x0500_0000-0x1FBF_FFFF` and
    /// `0x1FD0_0000-0x7FFF_FFFF`. Addresses outside them are DMA-only.
    const fn is_pi_bus(addr: u32) -> bool {
        matches!(addr, 0x0500_0000..=0x1FBF_FFFF | 0x1FD0_0000..=0x7FFF_FFFF)
    }

    /// Map a PI-bus address through the **16-bit-bus off-by-two**.
    ///
    /// The PI external bus is 16 bits wide and the RCP ignores access size, so
    /// every VR4300 read becomes two 16-bit bus reads: the MSB at the CPU's
    /// address with bit 0 ignored, then the LSB at `address + 2`. The RCP thus
    /// returns the word starting at `addr & !1`, while the CPU selects its byte
    /// lane assuming a word at `addr & !3`. **That two-byte disagreement is the
    /// bug**, and it is hardware behaviour, not an approximation:
    ///
    /// > effectively a 16-bit read at `0x1000'0002` returns the 16-bit word at
    /// > `0x1000'0004`
    /// > — N64brew, *Memory map*, PI external bus
    ///
    /// Working it through, `byte = (addr & !1) + (addr & 3)`, which collapses to
    /// "add two when bit 1 is set". A halfword load needs no special case
    /// because it is issued as two byte reads and both land correctly; a **word**
    /// load must bypass this entirely, which is why [`Bus::read_u32`] reads the
    /// PI window raw.
    const fn pi_bus_byte(addr: u32) -> u32 {
        if addr & 2 != 0 {
            addr.wrapping_add(2)
        } else {
            addr
        }
    }

    /// Is this address in the PI register block?
    const fn is_pi_register(addr: u32) -> bool {
        addr >= rustyn64_cart::pi::PI_BASE && addr < rustyn64_cart::pi::PI_BASE + 0x34
    }

    /// Carry out an SP DMA the register file has programmed.
    ///
    /// The engine lives in `rustyn64-rsp` and returns a description; the copy
    /// happens **here**, because the RSP does not own RDRAM and a chip reaching
    /// back into its owner is the dependency cycle `docs/architecture.md` exists
    /// to prevent. The PI works the same way.
    ///
    /// `skip` applies to the RDRAM side only. The SP side is contiguous and
    /// **wraps within its own 4 KiB bank** — a single transfer never spans DMEM
    /// and IMEM (N64brew *RSP Interface*: *"if the transfer hits the end of
    /// either memory area, it wraps around to the beginning of it"*).
    pub fn sp_dma(&mut self, dma: rustyn64_rsp::sp::Dma) {
        // Bit 12 selects the bank and is held fixed for the whole transfer;
        // only the 12-bit offset advances, so it wraps inside that bank.
        let bank = dma.sp_addr & 0x1000;
        let mut mem = dma.sp_addr & 0xFFF;
        let mut dram = dma.ram_addr;

        for _ in 0..dma.rows {
            for _ in 0..dma.row_len {
                let m = bank | (mem & 0xFFF);
                if let Some(off) = Self::rdram_offset(dram) {
                    if dma.to_dram {
                        self.rdram[off] = self.rsp.mem_read(m);
                    } else {
                        self.rsp.mem_write(m, self.rdram[off]);
                    }
                }
                mem = mem.wrapping_add(1);
                dram = dram.wrapping_add(1);
            }
            // The RDRAM pointer steps over the gap between rows; the SP side
            // does not.
            dram = dram.wrapping_add(dma.skip);
        }

        // Hardware leaves the pointers past the data, and the length field at
        // `0xFF8`. Instantaneous for now: the transfer is a value, so charging
        // it real time later is a scheduling change rather than a rewrite.
        self.rsp
            .sp
            .complete_dma(bank | (mem & 0xFFF), dram & 0x00FF_FFFF);
    }

    /// Write a PI register and perform any transfer it starts.
    ///
    /// The copy happens **here**, not in the PI engine, because the PI does not
    /// own RDRAM — the Bus does. Having the engine reach back into its owner is
    /// the cycle this architecture exists to avoid, so the engine returns a
    /// description of the transfer and the owner carries it out.
    pub fn pi_write_word(&mut self, addr: u32, val: u32) {
        let started = self.pi.write(addr, val);
        // Mirror the PI's interrupt state into the MI on EVERY write, not only
        // on completion. A `PI_STATUS` write that clears the interrupt starts no
        // transfer, so an early return here left the MI line asserted -- `IP2`
        // stuck high forever, hanging any interrupt-driven loader.
        self.rcp.mi_intr.pi = self.pi.interrupt();
        let Some(t) = started else {
            return;
        };
        // Instantaneous for now. The transfer is a value, so charging it real
        // time later is a scheduling change rather than a rewrite -- which is
        // the same reason `SysAD` is a state machine rather than a function.
        for i in 0..t.len {
            if t.to_dram {
                let b = self.cart.pi_read(t.cart.wrapping_add(i));
                if let Some(off) = Self::rdram_offset(t.dram.wrapping_add(i)) {
                    self.rdram[off] = b;
                }
            } else {
                let b = Self::rdram_offset(t.dram.wrapping_add(i)).map_or(0, |off| self.rdram[off]);
                self.cart.pi_write(t.cart.wrapping_add(i), b);
            }
        }
        self.pi.complete();
        // Completion raises the PI line into the MI, which the CPU sees as IP2.
        self.rcp.mi_intr.pi = self.pi.interrupt();
    }

    const fn rdram_offset(addr: u32) -> Option<usize> {
        // KSEG0/KSEG1 are stripped by the (future) TLB; the physical RDRAM
        // window is `$0000_0000..$007F_FFFF`.
        let phys = (addr & 0x1FFF_FFFF) as usize;
        if phys < RDRAM_SIZE { Some(phys) } else { None }
    }
}

// --- The CPU's view of the whole machine. ---
use rustyn64_cart::pi;

impl CpuBus for Bus {
    fn read_u8(&mut self, addr: u32) -> u8 {
        if let Some(off) = Self::rdram_offset(addr) {
            return self.rdram[off];
        }
        if Self::is_pi_register(addr) {
            // PI registers are 32-bit; a byte read selects within the word.
            let mut w = self.pi.read(addr);
            // `IOBUSY` covers the asynchronous direct-I/O write as well as DMA;
            // software polls it to know when a cart write has landed.
            if addr & !3 == rustyn64_cart::pi::PI_STATUS && self.pi_io_busy() {
                w |= rustyn64_cart::pi::STATUS_IO_BUSY;
            }
            return (w >> (8 * (3 - (addr & 3)))) as u8;
        }
        if Self::is_spmem(addr) {
            return self.rsp.mem_read(addr - Self::SPMEM_BASE);
        }
        // The SP interface registers. Word-granular behind a byte read: the
        // whole block lives on the RCP's internal bus, which returns the
        // aligned word and lets the CPU select within it.
        if Self::is_sp_register(addr) {
            let w = self.rsp.sp.read((addr >> 2) & 7);
            return (w >> (8 * (3 - (addr & 3)))) as u8;
        }
        if Self::is_mi_register(addr) {
            return (self.mi_read(addr) >> (8 * (3 - (addr & 3)))) as u8;
        }
        if Self::is_dp_register(addr) {
            return (self.rdp.dpc_read((addr >> 2) & 7) >> (8 * (3 - (addr & 3)))) as u8;
        }
        if Self::is_vi_register(addr) {
            return (self.vi.read(addr >> 2) >> (8 * (3 - (addr & 3)))) as u8;
        }
        if addr & !3 == Self::SP_PC {
            return (self.rsp.sp.pc() >> (8 * (3 - (addr & 3)))) as u8;
        }
        if Self::is_isviewer(addr) {
            // Readable as ordinary memory, which is what makes the suite's
            // write-magic-then-read-back probe succeed and select this channel
            // instead of the framebuffer console. Bounds-checked for the same
            // reason as the write path: the address is guest-controlled.
            return self
                .isviewer
                .get((addr - Self::ISVIEWER_BASE) as usize)
                .copied()
                .unwrap_or(0);
        }
        if Self::is_pi_bus(addr) {
            // A write in flight shadows the whole bus: reads from ANY address
            // return the value being written, not the device's data.
            if self.pi_io_busy() {
                return self.pi_write_latch.to_be_bytes()[(addr & 3) as usize];
            }
            return self.cart.pi_read(Self::pi_bus_byte(addr));
        }
        // TODO(T-CORE-01): decode the remaining RCP register windows
        // (SP/DP/VI/AI/SI/RI/MI) and the PIF ROM/RAM.
        self.cart.pi_read(addr)
    }

    /// Read an aligned big-endian word.
    ///
    /// Overridden for the **PI external bus** only. The default composes four
    /// [`Bus::read_u8`] calls, which would apply the 16-bit-bus off-by-two to
    /// each byte independently and mangle bytes 2 and 3 of every word. A word
    /// access puts its own address on the bus, so `addr & !1 == addr` and the
    /// word is simply the four bytes there.
    fn read_u32(&mut self, addr: u32) -> u32 {
        // ISViewer lives INSIDE the PI bus range and is claimed first, exactly
        // as it is on the byte path. Letting the cart branch win here routes the
        // debug channel's read-back to ROM and breaks the detection handshake
        // the suite uses to select it.
        if Self::is_pi_bus(addr) && !Self::is_isviewer(addr) {
            if self.pi_io_busy() {
                return self.pi_write_latch;
            }
            return u32::from_be_bytes([
                self.cart.pi_read(addr),
                self.cart.pi_read(addr.wrapping_add(1)),
                self.cart.pi_read(addr.wrapping_add(2)),
                self.cart.pi_read(addr.wrapping_add(3)),
            ]);
        }
        // Registers with a **side effect on read** must be read exactly once.
        //
        // `SP_SEMAPHORE` takes the mutex when read, so composing a word out of
        // four byte reads took it four times: the first byte saw 0 and the rest
        // saw 1, and the assembled word came back as 1 where hardware returns 0.
        // n64-systemtest's `SP Semaphore Register (CPU only)` catches exactly
        // that. On hardware the RCP returns the whole aligned word for one
        // access regardless of size, so one access is the correct model.
        if Self::is_sp_register(addr) {
            return self.rsp.sp.read((addr >> 2) & 7);
        }
        if Self::is_mi_register(addr) {
            return self.mi_read(addr);
        }
        if Self::is_dp_register(addr) {
            return self.rdp.dpc_read((addr >> 2) & 7);
        }
        if Self::is_vi_register(addr) {
            return self.vi.read(addr >> 2);
        }
        u32::from_be_bytes([
            self.read_u8(addr),
            self.read_u8(addr.wrapping_add(1)),
            self.read_u8(addr.wrapping_add(2)),
            self.read_u8(addr.wrapping_add(3)),
        ])
    }

    fn write_u8(&mut self, addr: u32, val: u8) {
        if let Some(off) = Self::rdram_offset(addr) {
            self.rdram[off] = val;
            return;
        }
        if Self::is_pi_register(addr) {
            // PI registers are **32-bit only**, and a byte write to one is not
            // something real code does. Assembling a word by read-modify-write
            // is actively wrong for two of them:
            //
            //   * the length registers *trigger* on write, so a byte-wise RMW
            //     starts a DMA per byte with a partly assembled length;
            //   * `PI_STATUS`'s read bits (busy, interrupt) do not correspond to
            //     its write bits (reset, clear-interrupt), so reading it back to
            //     fill in the other three bytes fabricates command strobes from
            //     status flags.
            //
            // Only the address registers can be safely assembled, so only they
            // are. A byte write to anything else is dropped rather than guessed
            // at -- an explicit nothing beats a plausible wrong action.
            if matches!(addr & !3, pi::PI_DRAM_ADDR | pi::PI_CART_ADDR) {
                let shift = 8 * (3 - (addr & 3));
                let w = (self.pi.read(addr) & !(0xFF << shift)) | (u32::from(val) << shift);
                self.pi_write_word(addr, w);
            }
            return;
        }
        if Self::is_spmem(addr) {
            self.rsp.mem_write(addr - Self::SPMEM_BASE, val);
            return;
        }
        if Self::is_isviewer(addr) {
            if let Some(b) = self.isviewer.get_mut((addr - Self::ISVIEWER_BASE) as usize) {
                *b = val;
            }
            return;
        }
        // TODO(T-CORE-01): decode + dispatch the remaining RCP register windows.
        self.cart.pi_write(addr, val);
    }

    /// Model the RCP's **size-blind** write path.
    ///
    /// Everything on the RCP's internal bus latches the whole 32-bit word the
    /// VR4300 put on `SysAD`, ignoring both the access size and the low two
    /// address bits (N64brew *Memory map* §Physical Memory Map accesses). The
    /// VR4300 has already shifted the source register into the byte lane the
    /// address selects, so a narrow store writes that shifted register —
    /// **including the bits above the stored byte**, which is why the effect
    /// looks like zero-fill rather than a partial update.
    ///
    /// n64-systemtest states the rule outright in its own header comment
    /// (`src/tests/sp_memory/mod.rs`): *"SH/SB are broken: they overwrite the
    /// whole 32 bit, filling everything that isn't written with zeroes. SD is
    /// broken: it only writes the upper 32 bit of the value, touching only 4
    /// bytes."* With `$3 = 0x1234_5678`, `SB $3, 5(spmem)` leaves `0x5678_0000`
    /// in the word at offset 4 — the register shifted left 16, not the byte
    /// `0x78`.
    ///
    /// RDRAM is excluded because the RI passes the low address bits and the
    /// access size on to the RDRAM devices, which build a real byte mask from
    /// them; only the RCP's internal path throws that information away.
    fn write_sized(&mut self, addr: u32, width: u64, value: u64) {
        // Unsupported widths do nothing, matching the default `write_sized`.
        // `StoreKind::width` only ever yields 1/2/4/8 so nothing reaches this
        // today, but without the guard the internal-bus arm below would accept
        // any width and *store* -- so the two paths would disagree about what a
        // width of 3 means, which is exactly the kind of divergence that is
        // discovered years later through a corrupted byte lane.
        if !matches!(width, 1 | 2 | 4 | 8) {
            return;
        }
        if !Self::is_rcp_internal(addr) {
            // RDRAM, the PI/SI external buses and the ISViewer keep byte-exact
            // semantics -- see `is_rcp_internal` for why the external buses are
            // not folded in here yet.
            match width {
                1 => self.write_u8(addr, value as u8),
                2 => {
                    self.write_u8(addr, (value >> 8) as u8);
                    self.write_u8(addr.wrapping_add(1), value as u8);
                }
                4 => self.write_u32(addr, value as u32),
                8 => {
                    self.write_u32(addr, (value >> 32) as u32);
                    self.write_u32(addr.wrapping_add(4), value as u32);
                }
                _ => {}
            }
            return;
        }
        let word = match width {
            // 64-bit: the two words go out MSB-first and the RCP takes the
            // first, dropping the second entirely -- so a `SD` touches four
            // bytes, not eight.
            8 => (value >> 32) as u32,
            4 => value as u32,
            // Narrow: the register as the VR4300 placed it on the bus.
            //
            // Saturating, not because the invariant is in doubt but because it
            // is enforced somewhere else. MIPS requires natural alignment and
            // the CPU raises `AddressError` before a misaligned store ever
            // reaches the bus, so `width + (addr & 3) <= 4` holds for every
            // access that gets here — but this is a public trait method, and a
            // caller that breaks the invariant should get a defined byte lane
            // rather than an underflow that panics in debug and silently
            // becomes an over-wide shift in release.
            w => {
                let lane = 4u32.saturating_sub(w as u32).saturating_sub(addr & 3);
                (value as u32) << (8 * lane)
            }
        };
        self.write_u32(addr & !3, word);
    }

    fn write_u32(&mut self, addr: u32, val: u32) {
        // SP DMA registers. Handled here, at word granularity, for the same
        // reason as the PI: the default byte-wise path would fire four DMAs for
        // one `sw` to a length register.
        // A PI direct-I/O write latches and returns immediately; the transfer
        // finalises in the background. Further writes while one is in flight are
        // ignored -- not queued.
        if Self::is_pi_bus(addr) && !Self::is_isviewer(addr) {
            if !self.pi_io_busy() {
                self.pi_write_latch = val;
                self.pi_write_countdown = Self::PI_WRITE_CYCLES;
            }
            return;
        }
        if Self::is_sp_register(addr) {
            self.sp_register_write(addr, val);
            return;
        }
        if Self::is_mi_register(addr) {
            self.mi_write(addr, val);
            return;
        }
        if Self::is_dp_register(addr) {
            self.rdp.dpc_write((addr >> 2) & 7, val);
            return;
        }
        if Self::is_vi_register(addr) {
            self.vi_write(addr, val);
            return;
        }
        if addr & !3 == Self::SP_PC {
            self.rsp.sp.set_pc(val);
            return;
        }
        if addr == Self::ISVIEWER_WRITE_LEN {
            // Flushing is triggered by the LENGTH write, not by the buffer
            // writes -- so the guest assembles a whole line and then publishes
            // it. Capturing on buffer writes instead would interleave partial
            // lines and make the output unreadable.
            let n = (val as usize).min(Self::ISVIEWER_LEN);
            let base = (Self::ISVIEWER_BUF - Self::ISVIEWER_BASE) as usize;
            let bytes = &self.isviewer[base..base + n];
            self.isviewer_out.extend_from_slice(bytes);
            return;
        }
        if Self::is_isviewer(addr) {
            // Bounds-checked, not indexed. `addr` comes from guest code, so a
            // word write starting in the last three bytes of the window --
            // which `is_isviewer` accepts -- would index past the slice and
            // **panic the emulator**. A guest must never be able to do that.
            let off = (addr - Self::ISVIEWER_BASE) as usize;
            if let Some(dst) = self.isviewer.get_mut(off..off + 4) {
                dst.copy_from_slice(&val.to_be_bytes());
            }
            return;
        }
        // **A PI register write must be a single WORD write.**
        //
        // The default `write_u32` composes four `write_u8` calls, and PI
        // registers were handled byte-wise -- so a normal guest `sw` to
        // `PI_WR_LEN` started **four DMAs**, one per byte, each with a partly
        // assembled length. Every PI transfer was wrong, and the failure looks
        // like memory corruption rather than a DMA bug.
        if Self::is_pi_register(addr) {
            self.pi_write_word(addr, val);
            return;
        }
        if let Some(off) = Self::rdram_offset(addr) {
            // The fast path, avoiding four bounds checks for the common case.
            let b = val.to_be_bytes();
            if off + 3 < self.rdram.len() {
                self.rdram[off..=off + 3].copy_from_slice(&b);
                return;
            }
        }
        let b = val.to_be_bytes();
        for (i, byte) in b.iter().enumerate() {
            self.write_u8(addr.wrapping_add(i as u32), *byte);
        }
    }

    fn emux_enabled(&self) -> bool {
        self.emux_enabled
    }

    fn emux_log(&mut self, bytes: &[u8]) {
        self.emux_out.extend_from_slice(bytes);
    }

    fn emux_exit(&mut self) {
        self.emux_exited = true;
    }

    fn poll_irq(&mut self) -> bool {
        // IP2 asserts when an unmasked MI line is set. The run-cycle gate and the
        // DC-stage sampling point live in the CPU pipeline (ADR 0007); this only
        // reports the level.
        let i = self.rcp.mi_intr;
        let m = self.rcp.mi_mask;
        (i.sp && m.sp)
            || (i.si && m.si)
            || (i.ai && m.ai)
            || (i.vi && m.vi)
            || (i.pi && m.pi)
            || (i.dp && m.dp)
    }
}

// --- The shared RDRAM bus (used by the RDP/RSP/AI DMA paths). ---
impl RdramBus for Bus {
    fn rdram_read(&self, addr: u32) -> u8 {
        Self::rdram_offset(addr).map_or(0, |off| self.rdram[off])
    }

    fn rdram_write(&mut self, addr: u32, val: u8) {
        if let Some(off) = Self::rdram_offset(addr) {
            self.rdram[off] = val;
        }
    }
}

// --- The RDP's narrow view. ---
impl VideoBus for Bus {
    fn raise_dp_interrupt(&mut self) {
        self.rcp.mi_intr.dp = true;
    }
}

// --- The RSP's narrow view. ---
// --- The AI's narrow view. ---
impl AudioBus for Bus {
    fn ai_dma_read_u32(&self, addr: u32) -> u32 {
        <Self as RdramBus>::rdram_read_u32(self, addr)
    }
    fn raise_ai_interrupt(&mut self) {
        self.rcp.mi_intr.ai = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rdram_round_trips_through_cpu_view() {
        let mut bus = Bus::new();
        CpuBus::write_u8(&mut bus, 0x0000_1234, 0xAB);
        assert_eq!(CpuBus::read_u8(&mut bus, 0x0000_1234), 0xAB);
    }

    #[test]
    fn dp_interrupt_sets_mi_line() {
        let mut bus = Bus::new();
        VideoBus::raise_dp_interrupt(&mut bus);
        assert!(bus.rcp.mi_intr.dp);
        assert!(bus.rcp.mi_intr.any());
    }

    #[test]
    fn masked_irq_drives_ip2() {
        let mut bus = Bus::new();
        bus.rcp.mi_intr.ai = true;
        bus.rcp.mi_mask.ai = true;
        assert!(CpuBus::poll_irq(&mut bus));
    }

    /// **A `Sync Full` command drives the DP interrupt through to the CPU.** A
    /// `0x29` command word placed in RDRAM and consumed by `rdp_tick` raises
    /// `MI_INTR.dp`; once the DP line is masked in it asserts IP2, which is how
    /// the CPU comes to service the RDP-done interrupt. This is the end-to-end
    /// path for Phase 3's `Sync Full` — the RDP dispatcher, the `VideoBus` seam,
    /// the MI line, and the mask, together.
    #[test]
    fn a_sync_full_command_drives_the_dp_interrupt_to_ip2() {
        let mut bus = Bus::new();
        // A Sync Full command (opcode 0x29 in bits 61:56) at RDRAM 0x100.
        bus.rdram[0x100] = 0x29;
        // Point the DP FIFO at it: a single 8-byte command.
        bus.rdp.dpc_write(0, 0x100); // DPC_START (sets START_VALID)
        bus.rdp.dpc_write(1, 0x108); // DPC_END  (copies START -> CURRENT)

        assert!(!bus.rcp.mi_intr.dp, "DP line clear before the command runs");
        bus.rdp_tick();
        assert!(bus.rcp.mi_intr.dp, "Sync Full raised the DP line");

        bus.rcp.mi_mask.dp = true;
        assert!(CpuBus::poll_irq(&mut bus), "the masked DP line asserts IP2");
    }

    /// **VI registers round-trip through the CPU bus, and a `VI_V_CURRENT` write
    /// acknowledges the VI interrupt.** The block is at `0x0440_0000`; a write to
    /// `VI_V_CURRENT` (+0x10) clears `MI_INTR.vi`, the interrupt-ack path.
    #[test]
    fn vi_registers_round_trip_and_v_current_acks_the_interrupt() {
        let mut bus = Bus::new();
        // VI_ORIGIN (+0x04) is an ordinary latch.
        CpuBus::write_u32(&mut bus, Bus::VI_REGS_BASE + 0x04, 0x0010_0000);
        assert_eq!(
            CpuBus::read_u32(&mut bus, Bus::VI_REGS_BASE + 0x04),
            0x0010_0000,
            "VI_ORIGIN round-trips"
        );
        // A pending VI interrupt is cleared by writing VI_V_CURRENT (+0x10).
        bus.rcp.mi_intr.vi = true;
        CpuBus::write_u32(&mut bus, Bus::VI_REGS_BASE + 0x10, 0x42);
        assert!(
            !bus.rcp.mi_intr.vi,
            "writing VI_V_CURRENT acks the interrupt"
        );
        // ... and the write did not latch into V_CURRENT.
        assert_eq!(CpuBus::read_u32(&mut bus, Bus::VI_REGS_BASE + 0x10), 0);
    }
}

#[cfg(test)]
mod pi_tests {
    use super::*;
    use rustyn64_cart::pi::{PI_CART_ADDR, PI_DRAM_ADDR, PI_STATUS, PI_WR_LEN};

    /// A `PI_WR_LEN` write must copy **cart → RDRAM**, `len + 1` bytes, and
    /// raise the PI interrupt line into the MI.
    ///
    /// This is the path n64-systemtest uses to load the rest of its own ELF, so
    /// it is the difference between the suite reporting a number and not
    /// starting at all.
    #[test]
    fn a_pi_wr_len_write_copies_cart_to_rdram_and_raises_the_interrupt() {
        let mut bus = Bus::new();
        // A cart whose ROM is a recognisable ramp.
        let mut rom = alloc::vec![0u8; 0x100];
        rom[..4].copy_from_slice(&[0x80, 0x37, 0x12, 0x40]); // .z64 magic
        for (i, b) in rom.iter_mut().enumerate().skip(0x40) {
            *b = i as u8;
        }
        bus.cart = rustyn64_cart::Cart::load(&rom).expect("loadable");

        bus.pi_write_word(PI_DRAM_ADDR, 0x1000);
        bus.pi_write_word(PI_CART_ADDR, 0x1000_0040);
        bus.pi_write_word(PI_WR_LEN, 15); // 16 bytes

        for i in 0..16u32 {
            assert_eq!(
                bus.rdram[(0x1000 + i) as usize],
                (0x40 + i) as u8,
                "byte {i} of the DMA"
            );
        }
        assert_eq!(bus.rdram[0x1000 + 16], 0, "and exactly 16, not 17");
        assert!(bus.rcp.mi_intr.pi, "completion raises the PI line");
        assert_eq!(
            bus.pi.read(PI_STATUS) & rustyn64_cart::pi::STATUS_DMA_BUSY,
            0,
            "and the DMA is no longer busy"
        );
    }

    /// `len + 1`: a length write of 0 moves **one** byte. Off by one here
    /// corrupts the last byte of every block, which presents as memory
    /// corruption rather than as a DMA bug.
    #[test]
    fn a_zero_length_write_transfers_exactly_one_byte() {
        let mut bus = Bus::new();
        let mut rom = alloc::vec![0u8; 0x80];
        rom[..4].copy_from_slice(&[0x80, 0x37, 0x12, 0x40]);
        rom[0x40] = 0xAB;
        rom[0x41] = 0xCD;
        bus.cart = rustyn64_cart::Cart::load(&rom).expect("loadable");

        bus.pi_write_word(PI_DRAM_ADDR, 0x2000);
        bus.pi_write_word(PI_CART_ADDR, 0x1000_0040);
        bus.pi_write_word(PI_WR_LEN, 0);

        assert_eq!(bus.rdram[0x2000], 0xAB, "one byte moved");
        assert_eq!(bus.rdram[0x2001], 0x00, "and only one");
    }

    /// The PI registers are reachable through the ordinary CPU bus, which is how
    /// guest code drives them.
    #[test]
    fn the_pi_registers_are_reachable_from_the_cpu_bus() {
        let mut bus = Bus::new();
        // Word-wise, as real code does. Note the value read back is rounded
        // DOWN to a doubleword -- the DRAM side ignores bits 2:0.
        bus.pi_write_word(PI_DRAM_ADDR, 0x1234);
        assert_eq!(bus.read_u32(PI_DRAM_ADDR), 0x1230, "doubleword-aligned");
        // An already-aligned value survives untouched.
        bus.pi_write_word(PI_DRAM_ADDR, 0x1238);
        assert_eq!(bus.read_u32(PI_DRAM_ADDR), 0x1238);
        // And byte reads select within the word.
        assert_eq!(bus.read_u8(PI_DRAM_ADDR + 3), 0x38);
        assert_eq!(bus.read_u8(PI_DRAM_ADDR + 2), 0x12);
    }

    /// **A guest `sw` to a length register must start exactly ONE DMA.**
    ///
    /// The default `write_u32` composes four `write_u8` calls. With PI registers
    /// handled byte-wise, a normal word store started **four** transfers, each
    /// with a partly assembled length — so every PI transfer was wrong, and the
    /// symptom was memory corruption rather than anything that looked like DMA.
    #[test]
    fn a_word_store_to_a_length_register_starts_exactly_one_dma() {
        let mut bus = Bus::new();
        let mut rom = alloc::vec![0u8; 0x200];
        rom[..4].copy_from_slice(&[0x80, 0x37, 0x12, 0x40]);
        for (i, b) in rom.iter_mut().enumerate().skip(0x40) {
            *b = i as u8;
        }
        bus.cart = rustyn64_cart::Cart::load(&rom).expect("loadable");

        bus.write_u32(PI_DRAM_ADDR, 0x1000);
        bus.write_u32(PI_CART_ADDR, 0x1000_0040);
        // The write that matters: through the ordinary CPU word path.
        bus.write_u32(PI_WR_LEN, 7); // 8 bytes

        for i in 0..8u32 {
            assert_eq!(
                bus.rdram[(0x1000 + i) as usize],
                (0x40 + i) as u8,
                "byte {i}"
            );
        }
        assert_eq!(
            bus.rdram[0x1000 + 8],
            0,
            "exactly 8 bytes -- a per-byte trigger would have run four transfers \
             with lengths 0x07000000+1, 0x00070000+1, ... and scribbled far past here"
        );
    }

    /// **Clearing the PI interrupt must lower the MI line.** Only a completion
    /// used to update it, and a `PI_STATUS` clear starts no transfer — so the
    /// line stayed asserted, `IP2` stuck high, and any interrupt-driven loader
    /// hung forever.
    #[test]
    fn clearing_the_pi_interrupt_lowers_the_mi_line() {
        let mut bus = Bus::new();
        let mut rom = alloc::vec![0u8; 0x80];
        rom[..4].copy_from_slice(&[0x80, 0x37, 0x12, 0x40]);
        bus.cart = rustyn64_cart::Cart::load(&rom).expect("loadable");

        bus.write_u32(PI_WR_LEN, 0);
        assert!(bus.rcp.mi_intr.pi, "completion raised it");

        bus.write_u32(PI_STATUS, rustyn64_cart::pi::STATUS_W_CLR_INTR);
        assert!(!bus.pi.interrupt(), "the PI cleared its own flag");
        assert!(
            !bus.rcp.mi_intr.pi,
            "and the MI line must follow -- otherwise IP2 stays high forever"
        );
    }

    /// A **byte** write to a trigger or status register is dropped, not
    /// assembled. `PI_STATUS`'s read bits (busy, interrupt) do not correspond to
    /// its write bits (reset, clear-interrupt), so reading it back to fill in
    /// the other three bytes fabricates command strobes out of status flags.
    #[test]
    fn byte_writes_to_the_trigger_and_status_registers_are_dropped() {
        let mut bus = Bus::new();
        let mut rom = alloc::vec![0u8; 0x80];
        rom[..4].copy_from_slice(&[0x80, 0x37, 0x12, 0x40]);
        bus.cart = rustyn64_cart::Cart::load(&rom).expect("loadable");

        bus.write_u8(PI_WR_LEN + 3, 0xFF);
        assert!(!bus.rcp.mi_intr.pi, "no DMA was started by a byte write");

        // Raise the interrupt, then confirm a byte write to STATUS cannot
        // fabricate a clear-interrupt strobe out of the busy/interrupt bits.
        bus.write_u32(PI_WR_LEN, 0);
        assert!(bus.rcp.mi_intr.pi);
        bus.write_u8(PI_STATUS + 3, 0x00);
        assert!(bus.rcp.mi_intr.pi, "a byte write to STATUS did nothing");

        // The address registers CAN be assembled, since they only latch.
        bus.write_u8(PI_DRAM_ADDR + 3, 0x18);
        assert_eq!(bus.read_u32(PI_DRAM_ADDR) & 0xFF, 0x18);
    }

    /// **A guest must not be able to panic the emulator.** A word write starting
    /// in the last three bytes of the `ISViewer` window is accepted by the range
    /// check but would index past the backing slice.
    ///
    /// The address and value both come from guest code, so this is reachable by
    /// any ROM, not just a malformed one.
    #[test]
    fn a_word_write_at_the_end_of_the_isviewer_window_does_not_panic() {
        let mut bus = Bus::new();
        let last = Bus::ISVIEWER_BASE + 0x20 + Bus::ISVIEWER_LEN as u32 - 1;
        for addr in [last - 3, last - 2, last - 1, last] {
            bus.write_u32(addr, 0xDEAD_BEEF);
            let _ = bus.read_u32(addr);
        }
        // ...and reads just past the window are 0 rather than a panic.
        assert_eq!(bus.read_u8(last), 0xEF, "the aligned tail write landed");
    }

    /// The `ISViewer` window must round-trip a written word, because that is
    /// exactly the probe n64-systemtest uses to decide whether the channel
    /// exists — `isviewer::detect()` writes `0x12345678` and reads it back. If
    /// it fails, the suite falls back to a framebuffer console we cannot read.
    #[test]
    fn the_isviewer_window_round_trips_the_detection_magic() {
        let mut bus = Bus::new();
        bus.write_u32(Bus::ISVIEWER_BUF, 0x1234_5678);
        assert_eq!(
            bus.read_u32(Bus::ISVIEWER_BUF),
            0x1234_5678,
            "detect() must succeed or the suite picks the framebuffer instead"
        );
    }

    /// Text is captured on the **length** write, not on the buffer writes, so a
    /// whole line is published at once. Capturing per buffer write would
    /// interleave partial lines and make the output unreadable.
    #[test]
    fn text_is_captured_on_the_length_write_not_the_buffer_writes() {
        let mut bus = Bus::new();
        // "OK!\n" packed big-endian, as `isviewer::pack` does.
        bus.write_u32(Bus::ISVIEWER_BUF, u32::from_be_bytes(*b"OK!\n"));
        assert!(
            bus.isviewer_output().is_empty(),
            "nothing published until the length write"
        );
        bus.write_u32(Bus::ISVIEWER_WRITE_LEN, 4);
        assert_eq!(bus.isviewer_output(), b"OK!\n");

        // And a second line appends rather than replacing.
        bus.write_u32(Bus::ISVIEWER_BUF, u32::from_be_bytes(*b"two\n"));
        bus.write_u32(Bus::ISVIEWER_WRITE_LEN, 4);
        assert_eq!(bus.isviewer_output(), b"OK!\ntwo\n");
    }

    /// A length longer than the buffer is clamped rather than panicking — the
    /// value comes from guest code and must not be trusted.
    #[test]
    fn an_oversized_length_write_is_clamped() {
        let mut bus = Bus::new();
        bus.write_u32(Bus::ISVIEWER_WRITE_LEN, 0xFFFF_FFFF);
        assert_eq!(bus.isviewer_output().len(), Bus::ISVIEWER_LEN);
    }

    /// **The RSP powers up halted.** Reading `SP_STATUS` as zero claims a
    /// running RSP, which is false; n64-systemtest's `StartupTest` reads `0x1`.
    #[test]
    fn sp_status_reports_the_rsp_halted_at_power_on() {
        let mut bus = Bus::new();
        assert_eq!(
            bus.read_u32(Bus::SP_STATUS) & rustyn64_rsp::sp::STATUS_HALTED,
            rustyn64_rsp::sp::STATUS_HALTED,
            "the RSP idles halted until the CPU clears it"
        );
    }

    /// **`SP_RD_LEN` moves RDRAM into SPMEM**, and the length word is not a
    /// plain byte count: bits 11:0 are bytes-per-row minus one.
    #[test]
    fn an_sp_dma_moves_rdram_into_spmem() {
        let mut bus = Bus::new();
        for (i, b) in bus.rdram[0x100..0x108].iter_mut().enumerate() {
            *b = 0xA0 + i as u8;
        }
        bus.write_u32(Bus::SP_REGS_BASE + 4, 0x100);
        bus.write_u32(Bus::SP_REGS_BASE, 0);
        bus.write_u32(Bus::SP_REGS_BASE + 8, 7); // 8 bytes, one row
        assert_eq!(
            core::array::from_fn::<u8, 8, _>(|i| bus.rsp.mem_read(i as u32)),
            [0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7]
        );
    }

    /// `SP_WR_LEN` is the other direction — SPMEM into RDRAM.
    #[test]
    fn an_sp_dma_moves_spmem_into_rdram() {
        let mut bus = Bus::new();
        for i in 0..8u32 {
            bus.rsp.mem_write(i, 0x50 + i as u8);
        }
        bus.write_u32(Bus::SP_REGS_BASE + 4, 0x200);
        bus.write_u32(Bus::SP_REGS_BASE, 0);
        bus.write_u32(Bus::SP_REGS_BASE + 12, 7);
        assert_eq!(
            &bus.rdram[0x200..0x208],
            &[0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57]
        );
    }

    /// **`count` and `skip` are real fields, not padding.** A 2D block copy
    /// moves `count + 1` rows and steps the RDRAM pointer by `skip` between
    /// them, while SPMEM stays contiguous. Reading only bits 11:0 silently
    /// drops every row after the first.
    #[test]
    fn an_sp_dma_honours_the_count_and_skip_fields() {
        let mut bus = Bus::new();
        // Two rows of 8, separated by an 8-byte gap in RDRAM.
        for i in 0..8 {
            bus.rdram[0x300 + i] = 0x10 + i as u8;
            bus.rdram[0x310 + i] = 0x20 + i as u8;
        }
        bus.write_u32(Bus::SP_REGS_BASE + 4, 0x300);
        bus.write_u32(Bus::SP_REGS_BASE, 0);
        // length = 7 (8 bytes), count = 1 (two rows), skip = 8.
        bus.write_u32(Bus::SP_REGS_BASE + 8, 7 | (1 << 12) | (8 << 20));
        assert_eq!(
            core::array::from_fn::<u8, 8, _>(|i| bus.rsp.mem_read(i as u32)),
            [0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17]
        );
        assert_eq!(
            core::array::from_fn::<u8, 8, _>(|i| bus.rsp.mem_read(8 + i as u32)),
            [0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27],
            "the second row must land contiguously in SPMEM"
        );
    }

    /// `SP_MEM_ADDR` bit 12 selects **IMEM**, and the 12-bit offset wraps within
    /// whichever half was chosen rather than spilling across into the other.
    #[test]
    fn sp_mem_addr_bit_12_selects_imem() {
        let mut bus = Bus::new();
        bus.rdram[0x400] = 0x99;
        bus.write_u32(Bus::SP_REGS_BASE + 4, 0x400);
        bus.write_u32(Bus::SP_REGS_BASE, 0x1000); // IMEM
        bus.write_u32(Bus::SP_REGS_BASE + 8, 7);
        assert_eq!(bus.rsp.mem_read(0x1000), 0x99, "landed in IMEM");
        assert_eq!(bus.rsp.mem_read(0), 0, "and NOT in DMEM");
    }

    /// Read a word out of SPMEM the way the CPU does, for the tests below.
    fn spmem_word(bus: &mut Bus, off: u32) -> u32 {
        bus.read_u32(Bus::SPMEM_BASE + off)
    }

    /// **A byte store to the RCP's internal bus writes 32 bits.**
    ///
    /// The values are n64-systemtest's, not ours (`sp_memory::SB`): with
    /// `$3 = 0x1234_5678`, storing a byte at offsets 0, 5, 10 and 15 leaves the
    /// register *shifted into the addressed lane* in each of the four words,
    /// wiping the rest. Byte-exact semantics would leave `0x7800_0000`,
    /// `0x0078_0000`, `0x0000_7800`, `0x0000_0078` instead -- so this test fails
    /// in all four words if the size-blind path is lost.
    #[test]
    fn a_byte_store_to_spmem_writes_the_whole_shifted_word() {
        let mut bus = Bus::new();
        for (i, off) in [0u32, 5, 10, 15].iter().enumerate() {
            bus.write_sized(Bus::SPMEM_BASE + off, 1, 0x1234_5678);
            let _ = i;
        }
        assert_eq!(spmem_word(&mut bus, 0), 0x7800_0000);
        assert_eq!(spmem_word(&mut bus, 4), 0x5678_0000);
        assert_eq!(spmem_word(&mut bus, 8), 0x3456_7800);
        assert_eq!(spmem_word(&mut bus, 12), 0x1234_5678);
    }

    /// The same rule for halfwords, and it **destroys the untouched half** --
    /// `sp_memory::SH` presets `0xDEAD_BEEF`/`0xBADD_ECAF` and expects both gone.
    #[test]
    fn a_halfword_store_to_spmem_writes_the_whole_shifted_word() {
        let mut bus = Bus::new();
        bus.write_u32(Bus::SPMEM_BASE, 0xDEAD_BEEF);
        bus.write_u32(Bus::SPMEM_BASE + 4, 0xBADD_ECAF);

        bus.write_sized(Bus::SPMEM_BASE, 2, 0x1234_5678);
        bus.write_sized(Bus::SPMEM_BASE + 6, 2, 0x1234_5678);

        assert_eq!(spmem_word(&mut bus, 0), 0x5678_0000);
        assert_eq!(spmem_word(&mut bus, 4), 0x1234_5678);
    }

    /// **A 64-bit store touches four bytes, not eight.** The RCP takes the first
    /// word off the bus and drops the second (`sp_memory::SD`), so the preset
    /// second word must survive intact -- which is what distinguishes this from
    /// a plain 64-bit write.
    #[test]
    fn a_doubleword_store_to_spmem_writes_only_the_upper_word() {
        let mut bus = Bus::new();
        bus.write_u32(Bus::SPMEM_BASE, 0xDEAD_BEEF);
        bus.write_u32(Bus::SPMEM_BASE + 4, 0xBADD_ECAF);

        bus.write_sized(Bus::SPMEM_BASE, 8, 0xABCD_EF98_7654_3210);

        assert_eq!(spmem_word(&mut bus, 0), 0xABCD_EF98);
        assert_eq!(
            spmem_word(&mut bus, 4),
            0xBADD_ECAF,
            "the low word is dropped on the floor, not stored"
        );
    }

    /// **RDRAM is not size-blind.** The RI passes the low address bits and the
    /// access size to the RDRAM devices, which build a real byte mask. Without
    /// this the size-blind rule would corrupt every ordinary narrow store, so
    /// the exclusion is load-bearing rather than an optimisation.
    #[test]
    fn a_byte_store_to_rdram_writes_one_byte() {
        let mut bus = Bus::new();
        bus.write_u32(0x100, 0xDEAD_BEEF);
        bus.write_sized(0x101, 1, 0x1234_5678);
        assert_eq!(bus.read_u32(0x100), 0xDE78_BEEF);
    }

    /// The 8 KiB of DMEM+IMEM **repeats** up to `0x0404_0000`, where the SP
    /// registers begin. n64-systemtest writes at `0x3E000` and reads the result
    /// back at offset 0 (`sp_memory::SW (out of bounds)`).
    #[test]
    fn the_spmem_window_repeats_every_8_kib() {
        let mut bus = Bus::new();
        bus.write_u32(Bus::SPMEM_BASE, 0x0123_4567);
        bus.write_u32(Bus::SPMEM_BASE + 0x1000, 0x89AB_CDEF);
        bus.write_u32(Bus::SPMEM_BASE + 0x3E000, 0x7654_3210);

        assert_eq!(
            spmem_word(&mut bus, 0),
            0x7654_3210,
            "0x3E000 is offset 0 seen for the 31st time"
        );
        assert_eq!(spmem_word(&mut bus, 0x1000), 0x89AB_CDEF, "IMEM untouched");
        assert_eq!(spmem_word(&mut bus, 0x3E000), 0x7654_3210);
    }

    /// **`SP_SEMAPHORE` is taken once per access, not once per byte.**
    ///
    /// The register has a side effect on read, so composing a word from four
    /// byte reads took the mutex four times and returned 1 where hardware
    /// returns 0 — n64-systemtest's `SP Semaphore Register (CPU only)` fails on
    /// exactly that. The suite also checks that the value written is irrelevant.
    #[test]
    fn reading_the_semaphore_as_a_word_takes_it_exactly_once() {
        const SEMAPHORE: u32 = Bus::SP_REGS_BASE + 0x1C;
        for written in [0u32, 1, 0xFFFF_FFFF] {
            let mut bus = Bus::new();
            bus.write_u32(SEMAPHORE, written);
            assert_eq!(bus.read_u32(SEMAPHORE), 0, "the first word read acquires");
            assert_eq!(bus.read_u32(SEMAPHORE), 1, "and it stays taken");
        }
    }

    /// A `SP_STATUS` write raises and acknowledges the **MI's SP line**, and the
    /// guest can see it in `MI_INTERRUPT`.
    #[test]
    fn sp_status_drives_the_mi_interrupt_line() {
        const SET_INTR: u32 = 1 << 4;
        const CLR_INTR: u32 = 1 << 3;
        const MI_INTERRUPT: u32 = Bus::MI_BASE + 0x08;
        let mut bus = Bus::new();

        bus.write_u32(Bus::SP_STATUS, SET_INTR);
        assert_eq!(bus.read_u32(MI_INTERRUPT) & 1, 1, "SP line raised");
        bus.write_u32(Bus::SP_STATUS, CLR_INTR);
        assert_eq!(bus.read_u32(MI_INTERRUPT) & 1, 0, "and acknowledged");

        // Set and clear together leaves it alone, as for every other flag.
        bus.write_u32(Bus::SP_STATUS, SET_INTR);
        bus.write_u32(Bus::SP_STATUS, SET_INTR | CLR_INTR);
        assert_eq!(bus.read_u32(MI_INTERRUPT) & 1, 1, "unchanged");
    }

    /// `MI_MASK` writes as clear/set pairs and reads back as a flag word, and
    /// only a masked-in line reaches `IP2`.
    #[test]
    fn the_mi_mask_gates_the_interrupt_line() {
        const MI_MASK: u32 = Bus::MI_BASE + 0x0C;
        const SET_SP: u32 = 1 << 1;
        const CLR_SP: u32 = 1 << 0;
        let mut bus = Bus::new();

        bus.write_u32(Bus::SP_STATUS, 1 << 4); // raise SP
        assert!(!bus.poll_irq(), "an unmasked line must not reach IP2");

        bus.write_u32(MI_MASK, SET_SP);
        assert_eq!(bus.read_u32(MI_MASK) & 1, 1, "the mask reads back");
        assert!(bus.poll_irq(), "masked in, so IP2 asserts");

        bus.write_u32(MI_MASK, CLR_SP);
        assert!(!bus.poll_irq(), "masked out again");
    }

    /// The MI block is four registers **mirrored** on the low four address bits,
    /// so `MI_VERSION` is readable at every `+0x10` step.
    #[test]
    fn the_mi_registers_mirror_every_sixteen_bytes() {
        let mut bus = Bus::new();
        assert_eq!(bus.read_u32(Bus::MI_BASE + 0x04), Bus::MI_VERSION_VALUE);
        assert_eq!(
            bus.read_u32(Bus::MI_BASE + 0x14),
            Bus::MI_VERSION_VALUE,
            "mirrored one block up"
        );
        assert_eq!(bus.read_u32(Bus::MI_BASE + 0x1004), Bus::MI_VERSION_VALUE);
    }

    /// **The PI external bus is 16 bits wide and the RCP ignores access size**,
    /// so a byte or halfword read returns data two bytes further on than the
    /// address asked for, while a word read does not. This is a hardware bug we
    /// must reproduce, not an approximation.
    ///
    /// n64-systemtest pins all three against a ROM beginning
    /// `01 23 45 67 89 AB CD EF`.
    #[test]
    fn a_pi_bus_sub_word_read_lands_two_bytes_late() {
        let mut bus = Bus::new();
        // A `.z64` header, then a byte-index pattern from 0x40 on.
        let mut rom = alloc::vec![0u8; 0x1000];
        rom[0..4].copy_from_slice(&[0x80, 0x37, 0x12, 0x40]);
        for (i, b) in rom.iter_mut().enumerate().skip(0x40) {
            *b = (i & 0xFF) as u8;
        }
        bus.cart = rustyn64_cart::Cart::load(&rom).expect("valid z64");
        let base = 0x1000_0040u32;

        // A WORD read is unaffected: the access puts its own address on the bus.
        assert_eq!(
            bus.read_u32(base),
            0x4041_4243,
            "a word read is the four bytes at its own address"
        );

        // A BYTE read at offset 2 returns the byte at offset 4.
        assert_eq!(bus.read_u8(base + 2), 0x44, "offset 2 reads byte 4");
        assert_eq!(bus.read_u8(base + 3), 0x45, "offset 3 reads byte 5");
        // ...but offsets 0 and 1 are unaffected: bit 1 is clear.
        assert_eq!(bus.read_u8(base), 0x40);
        assert_eq!(bus.read_u8(base + 1), 0x41);

        // A HALFWORD needs no special case -- it is two byte reads, and both
        // land correctly by the same rule.
        let hi = (u16::from(bus.read_u8(base + 2)) << 8) | u16::from(bus.read_u8(base + 3));
        assert_eq!(hi, 0x4445, "halfword at offset 2 reads offset 4");
    }

    /// The quirk is confined to the PI window. RDRAM must be untouched, or every
    /// ordinary load in the machine shifts by two bytes.
    #[test]
    fn the_pi_off_by_two_does_not_leak_into_rdram() {
        let mut bus = Bus::new();
        for (i, b) in bus.rdram[0..8].iter_mut().enumerate() {
            *b = i as u8;
        }
        assert_eq!(bus.read_u8(0x0000_0002), 2, "RDRAM is NOT shifted");
        assert_eq!(bus.read_u32(0x0000_0000), 0x0001_0203);
    }

    /// **A PI direct-I/O write latches and shadows the whole bus.** While it is
    /// in flight, reads from *any* PI address return the value being written --
    /// including from ROM, which the PI has no way of knowing is read-only.
    #[test]
    fn a_pi_write_is_latched_and_shadows_reads_until_it_finalises() {
        let mut bus = Bus::new();
        let mut rom = alloc::vec![0u8; 0x1000];
        rom[0..4].copy_from_slice(&[0x80, 0x37, 0x12, 0x40]);
        for (i, b) in rom.iter_mut().enumerate().skip(0x40) {
            *b = (i & 0xFF) as u8;
        }
        bus.cart = rustyn64_cart::Cart::load(&rom).expect("valid z64");
        let base = 0x1000_0040u32;
        assert_eq!(bus.read_u32(base), 0x4041_4243, "ROM before the write");

        bus.write_u32(base, 0xBADC_0FFE);
        assert_eq!(
            bus.read_u32(base),
            0xBADC_0FFE,
            "the latched value is read back"
        );
        assert_eq!(
            bus.read_u32(base + 0x100),
            0xBADC_0FFE,
            "and shadows a DIFFERENT address too -- it is the bus, not the cell"
        );

        // ...and it decays: the ROM value returns once the write finalises.
        for _ in 0..Bus::PI_WRITE_CYCLES {
            bus.pi_tick();
        }
        assert_eq!(
            bus.read_u32(base),
            0x4041_4243,
            "ROM is back; ROM ignored the write"
        );
    }

    /// `PI_STATUS.IOBUSY` reports the asynchronous write, which is how software
    /// knows when a cart write has landed.
    #[test]
    fn a_pi_write_sets_io_busy_until_it_finalises() {
        let mut bus = Bus::new();
        let st = rustyn64_cart::pi::PI_STATUS;
        assert_eq!(bus.read_u32(st) & rustyn64_cart::pi::STATUS_IO_BUSY, 0);
        bus.write_u32(0x1000_0000, 0xDEAD_BEEF);
        assert_ne!(
            bus.read_u32(st) & rustyn64_cart::pi::STATUS_IO_BUSY,
            0,
            "IOBUSY is set while the write is in flight"
        );
        for _ in 0..Bus::PI_WRITE_CYCLES {
            bus.pi_tick();
        }
        assert_eq!(bus.read_u32(st) & rustyn64_cart::pi::STATUS_IO_BUSY, 0);
    }

    /// A second write while one is in flight is **ignored**, not queued.
    #[test]
    fn a_pi_write_during_another_is_ignored() {
        let mut bus = Bus::new();
        bus.write_u32(0x1000_0000, 0xAAAA_AAAA);
        bus.write_u32(0x1000_0000, 0xBBBB_BBBB);
        assert_eq!(
            bus.read_u32(0x1000_0000),
            0xAAAA_AAAA,
            "the FIRST write still owns the bus"
        );
    }
}
