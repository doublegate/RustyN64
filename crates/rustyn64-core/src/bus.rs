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
use rustyn64_rsp::{Rsp, RspBus};

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
    /// RSP DMEM + IMEM as plain memory (`0x0400_0000..0x0400_2000`).
    ///
    /// The RSP does not execute yet, but its memory must be **readable**: boot
    /// code and IPL3 use DMEM as a handoff area, and n64-systemtest reads its
    /// RDRAM size and ELF offset straight out of it on its second and third
    /// instructions. A stub returning 0 made it build a memory map from zeros
    /// and jump into nothing.
    pub spmem: alloc::boxed::Box<[u8]>,
    /// The `ISViewer` buffer, as guest-visible memory.
    isviewer: alloc::boxed::Box<[u8]>,
    /// Text the guest has flushed through the `ISViewer` channel.
    isviewer_out: alloc::vec::Vec<u8>,
    /// The RSP coprocessor.
    pub rsp: Rsp,
    /// The RDP rasterizer.
    pub rdp: Rdp,
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
            spmem: alloc::vec![0u8; Self::SPMEM_LEN].into_boxed_slice(),
            isviewer: alloc::vec![0u8; 0x20 + Self::ISVIEWER_LEN].into_boxed_slice(),
            isviewer_out: alloc::vec::Vec::new(),
            rdram: alloc::vec![0u8; RDRAM_SIZE].into_boxed_slice(),
            rsp: Rsp::new(),
            rdp: Rdp::new(),
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

    /// Step the RSP against this bus's narrow [`RspBus`] view.
    ///
    /// The RSP is owned by the bus and `Rsp::tick` borrows `&mut impl RspBus`
    /// (which is the bus itself), so we move the chip out for the duration of
    /// the tick to satisfy the borrow checker, then move it back — the same
    /// split-borrow pattern used to step each chip against the Bus. No allocation.
    pub fn rsp_tick(&mut self) {
        let mut rsp = core::mem::take(&mut self.rsp);
        rsp.tick(self);
        self.rsp = rsp;
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
    /// DMEM + IMEM, 4 KiB each.
    pub const SPMEM_LEN: usize = 0x2000;

    /// Is this address in RSP DMEM/IMEM?
    const fn is_spmem(addr: u32) -> bool {
        addr >= Self::SPMEM_BASE && addr < Self::SPMEM_BASE + Self::SPMEM_LEN as u32
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

    /// Everything the guest has written to the `ISViewer` channel.
    #[must_use]
    pub fn isviewer_output(&self) -> &[u8] {
        &self.isviewer_out
    }

    /// Is this address in the PI register block?
    const fn is_pi_register(addr: u32) -> bool {
        addr >= rustyn64_cart::pi::PI_BASE && addr < rustyn64_cart::pi::PI_BASE + 0x34
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
            let w = self.pi.read(addr);
            return (w >> (8 * (3 - (addr & 3)))) as u8;
        }
        if Self::is_spmem(addr) {
            return self
                .spmem
                .get((addr - Self::SPMEM_BASE) as usize)
                .copied()
                .unwrap_or(0);
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
        // TODO(T-CORE-01): decode the remaining RCP register windows
        // (SP/DP/VI/AI/SI/RI/MI) and the PIF ROM/RAM.
        self.cart.pi_read(addr)
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
            if let Some(b) = self.spmem.get_mut((addr - Self::SPMEM_BASE) as usize) {
                *b = val;
            }
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

    fn write_u32(&mut self, addr: u32, val: u32) {
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
impl RspBus for Bus {
    fn rdram_read(&self, addr: u32) -> u8 {
        <Self as RdramBus>::rdram_read(self, addr)
    }
    fn rdram_write(&mut self, addr: u32, val: u8) {
        <Self as RdramBus>::rdram_write(self, addr, val);
    }
    fn raise_sp_interrupt(&mut self) {
        self.rcp.mi_intr.sp = true;
    }
}

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
}
