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
    const fn rdram_offset(addr: u32) -> Option<usize> {
        // KSEG0/KSEG1 are stripped by the (future) TLB; the physical RDRAM
        // window is `$0000_0000..$007F_FFFF`.
        let phys = (addr & 0x1FFF_FFFF) as usize;
        if phys < RDRAM_SIZE { Some(phys) } else { None }
    }
}

// --- The CPU's view of the whole machine. ---
impl CpuBus for Bus {
    fn read_u8(&mut self, addr: u32) -> u8 {
        if let Some(off) = Self::rdram_offset(addr) {
            return self.rdram[off];
        }
        // TODO(T-CORE-01): decode the RCP register windows (SP/DP/VI/AI/PI/SI/RI/MI),
        // the PI cart domains (→ `cart.pi_read`), and the PIF ROM/RAM.
        self.cart.pi_read(addr)
    }

    fn write_u8(&mut self, addr: u32, val: u8) {
        if let Some(off) = Self::rdram_offset(addr) {
            self.rdram[off] = val;
            return;
        }
        // TODO(T-CORE-01): decode + dispatch the RCP register + PI/SI write windows.
        self.cart.pi_write(addr, val);
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
