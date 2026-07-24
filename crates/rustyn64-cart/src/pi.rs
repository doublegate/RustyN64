//! The PI (Peripheral Interface) DMA engine (T-14-001).
//!
//! Moves bytes between RDRAM and the cartridge address space. Pulled **forward
//! from Phase 5 into Phase 1** because n64-systemtest loads the rest of its own
//! ELF from cart through PI, so the Phase 1 exit criterion — and with it the
//! v0.2.0 cut criterion — is unreachable without it.
//!
//! # Register map
//!
//! From `n64brew_wiki/markdown/Peripheral Interface.md`:
//!
//! | Address | Register | Effect |
//! | --- | --- | --- |
//! | `0x0460_0000` | `PI_DRAM_ADDR` | RDRAM side of the transfer |
//! | `0x0460_0004` | `PI_CART_ADDR` | cart side |
//! | `0x0460_0008` | `PI_RD_LEN` | **write triggers** RDRAM → cart |
//! | `0x0460_000C` | `PI_WR_LEN` | **write triggers** cart → RDRAM |
//! | `0x0460_0010` | `PI_STATUS` | read: busy flags; write: reset / clear IRQ |
//!
//! # The two rules that bite
//!
//! - **Length is `len + 1` bytes.** Writing 0 transfers one byte, not zero. An
//!   implementation that transfers `len` is short by one on every single DMA,
//!   which corrupts the *last* byte of every block — a failure that looks like
//!   memory corruption rather than a DMA bug.
//! - **`RD` and `WR` are named from the cartridge's point of view**, so
//!   `PI_WR_LEN` — the one everything actually uses — moves data **cart →
//!   RDRAM**. Getting them the wrong way round makes the first ROM load write
//!   the ROM's own image over itself with uninitialised RDRAM.

use serde::{Deserialize, Serialize};

/// Base of the PI register block.
pub const PI_BASE: u32 = 0x0460_0000;

/// `PI_DRAM_ADDR`.
pub const PI_DRAM_ADDR: u32 = 0x0460_0000;
/// `PI_CART_ADDR`.
pub const PI_CART_ADDR: u32 = 0x0460_0004;
/// `PI_RD_LEN` — writing it starts an RDRAM → cart transfer.
pub const PI_RD_LEN: u32 = 0x0460_0008;
/// `PI_WR_LEN` — writing it starts a cart → RDRAM transfer.
pub const PI_WR_LEN: u32 = 0x0460_000C;
/// `PI_STATUS`.
pub const PI_STATUS: u32 = 0x0460_0010;
/// `PI_BSD_DOM1_LAT` — domain-1 latency. DOM2's block follows at `+0x10`.
pub const PI_BSD_DOM1_LAT: u32 = 0x0460_0014;
/// `PI_BSD_DOM1_PWD` — domain-1 pulse width.
pub const PI_BSD_DOM1_PWD: u32 = 0x0460_0018;
/// `PI_BSD_DOM1_PGS` — domain-1 page size (`2^(PGS+2)` bytes).
pub const PI_BSD_DOM1_PGS: u32 = 0x0460_001C;
/// `PI_BSD_DOM1_RLS` — domain-1 release.
pub const PI_BSD_DOM1_RLS: u32 = 0x0460_0020;
/// `PI_BSD_DOM2_LAT` — domain-2 latency (the SRAM / `FlashRAM` bus).
pub const PI_BSD_DOM2_LAT: u32 = 0x0460_0024;
/// `PI_BSD_DOM2_PWD` — domain-2 pulse width.
pub const PI_BSD_DOM2_PWD: u32 = 0x0460_0028;
/// `PI_BSD_DOM2_PGS` — domain-2 page size.
pub const PI_BSD_DOM2_PGS: u32 = 0x0460_002C;
/// `PI_BSD_DOM2_RLS` — domain-2 release (end of the register block).
pub const PI_BSD_DOM2_RLS: u32 = 0x0460_0030;

/// `PI_STATUS` bit 0 — a DMA is in progress.
pub const STATUS_DMA_BUSY: u32 = 1 << 0;
/// `PI_STATUS` bit 1 — an I/O transfer is in progress.
pub const STATUS_IO_BUSY: u32 = 1 << 1;
/// `PI_STATUS` bit 3 — the PI interrupt is asserted.
pub const STATUS_INTERRUPT: u32 = 1 << 3;

/// `PI_STATUS` write bit 0 — reset the controller and abort any DMA.
pub const STATUS_W_RESET: u32 = 1 << 0;
/// `PI_STATUS` write bit 1 — clear the PI interrupt.
pub const STATUS_W_CLR_INTR: u32 = 1 << 1;

/// A transfer the PI has been asked to perform.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Transfer {
    /// RDRAM address.
    pub dram: u32,
    /// Cartridge address.
    pub cart: u32,
    /// Byte count — already `len + 1`, so this is the real length.
    pub len: u32,
    /// Direction: cart → RDRAM (a `PI_WR_LEN` write).
    pub to_dram: bool,
}

/// The PI register file and DMA state.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Pi {
    dram_addr: u32,
    cart_addr: u32,
    /// Set while a transfer is outstanding.
    busy: bool,
    /// The PI interrupt line, which the MI aggregates into `Cause.IP2`.
    interrupt: bool,
    /// Per-domain bus timing (index 0 = DOM1, 1 = DOM2), each a register the
    /// game/IPL2 programs. LAT/PWD are 8-bit, PGS 4-bit, RLS 2-bit. They store
    /// and read back here; deriving DMA duration from them (the non-instant
    /// completion) is a follow-up that reads these fields.
    dom_lat: [u8; 2],
    dom_pwd: [u8; 2],
    dom_pgs: [u8; 2],
    dom_rls: [u8; 2],
}

impl Pi {
    /// Power-on state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            dram_addr: 0,
            cart_addr: 0,
            busy: false,
            interrupt: false,
            dom_lat: [0; 2],
            dom_pwd: [0; 2],
            dom_pgs: [0; 2],
            dom_rls: [0; 2],
        }
    }

    /// Map a PI register address to its `(domain, field)` if it names a BSD
    /// timing register. `domain` is 0 (DOM1, base `0x14`) or 1 (DOM2, base
    /// `0x24`); `field` is 0=LAT, 1=PWD, 2=PGS, 3=RLS at `+0/4/8/C`.
    const fn dom_field(addr: u32) -> Option<(usize, usize)> {
        let off = (addr & !3).wrapping_sub(PI_BSD_DOM1_LAT);
        // DOM1 spans 0x00..0x10 from LAT; DOM2 the next 0x10.
        let (domain, within) = if off < 0x10 {
            (0, off)
        } else if off < 0x20 {
            (1, off - 0x10)
        } else {
            return None;
        };
        Some((domain, (within / 4) as usize))
    }

    /// Is the PI asserting its interrupt?
    #[must_use]
    pub const fn interrupt(&self) -> bool {
        self.interrupt
    }

    /// Read a PI register.
    #[must_use]
    pub const fn read(&self, addr: u32) -> u32 {
        match addr & !3 {
            PI_DRAM_ADDR => self.dram_addr,
            PI_CART_ADDR => self.cart_addr,
            PI_STATUS => {
                let mut s = 0;
                if self.busy {
                    // Both flags together: software polls `io_busy` (as
                    // n64-systemtest's ISViewer does) and `dma_busy`
                    // interchangeably to mean "the PI is occupied".
                    s |= STATUS_DMA_BUSY | STATUS_IO_BUSY;
                }
                if self.interrupt {
                    s |= STATUS_INTERRUPT;
                }
                s
            }
            // The BSD domain timing registers store and read back (masked to
            // their field widths). The length registers read back as 0x7F on
            // hardware (N64brew *Peripheral Interface* §PI_RD_LEN/PI_WR_LEN);
            // returning 0 for those is a documented simplification, not modelled.
            addr => {
                if let Some((d, field)) = Self::dom_field(addr) {
                    match field {
                        0 => self.dom_lat[d] as u32,
                        1 => self.dom_pwd[d] as u32,
                        2 => (self.dom_pgs[d] & 0xF) as u32,
                        _ => (self.dom_rls[d] & 0x3) as u32,
                    }
                } else {
                    0
                }
            }
        }
    }

    /// Write a PI register, returning a [`Transfer`] if the write started one.
    ///
    /// The transfer is **returned rather than performed** because the PI does
    /// not own RDRAM — the Bus does. Performing it here would need the engine to
    /// hold a reference back to the Bus that owns it, which is the cycle the
    /// whole architecture is built to avoid.
    pub const fn write(&mut self, addr: u32, val: u32) -> Option<Transfer> {
        match addr & !3 {
            PI_DRAM_ADDR => {
                // Bits 2:0 are ignored: the RDRAM side is **doubleword**
                // aligned, not halfword. Masking only bit 0 lets a transfer
                // start mid-doubleword, which silently shifts every byte of a
                // DMA whose caller relied on the hardware aligning it.
                self.dram_addr = val & 0x00FF_FFF8;
                None
            }
            PI_CART_ADDR => {
                self.cart_addr = val & 0xFFFF_FFFE;
                None
            }
            PI_RD_LEN => Some(self.start(val, false)),
            PI_WR_LEN => Some(self.start(val, true)),
            PI_STATUS => {
                if val & STATUS_W_RESET != 0 {
                    self.busy = false;
                }
                if val & STATUS_W_CLR_INTR != 0 {
                    self.interrupt = false;
                }
                None
            }
            addr => {
                // The BSD domain timing registers store the written value
                // (masked to their field widths). Deriving DMA duration from
                // them is a follow-up; the DMA still completes immediately.
                if let Some((d, field)) = Self::dom_field(addr) {
                    match field {
                        0 => self.dom_lat[d] = val as u8,
                        1 => self.dom_pwd[d] = val as u8,
                        2 => self.dom_pgs[d] = (val & 0xF) as u8,
                        _ => self.dom_rls[d] = (val & 0x3) as u8,
                    }
                }
                None
            }
        }
    }

    /// Begin a transfer of `len + 1` bytes.
    const fn start(&mut self, len: u32, to_dram: bool) -> Transfer {
        self.busy = true;
        // "+1" is the rule everything gets wrong once: writing 0 transfers
        // ONE byte. Being short by one corrupts the last byte of every
        // block, which presents as memory corruption rather than a DMA bug.
        let len = (len & 0x00FF_FFFF) + 1;
        let t = Transfer {
            dram: self.dram_addr,
            cart: self.cart_addr,
            len,
            to_dram,
        };
        // **Both address registers ADVANCE by the transfer length.** Hardware
        // walks them as the DMA proceeds, so software reads back the address
        // *past* the block it just moved -- which is how a driver chains
        // transfers without rewriting the address each time.
        //
        // Leaving them put makes every chained DMA re-transfer the first block,
        // and n64-systemtest checks the delta directly: after a 0x10-byte
        // transfer it expects `PI_CART_ADDR` to have moved by `0x10`, and for a
        // written length of 1 (two bytes) by `0x2`.
        //
        // Advanced here rather than on completion because this DMA is
        // instantaneous; the observable end state is identical, and charging it
        // real time later moves this line rather than rewriting it.
        self.dram_addr = self.dram_addr.wrapping_add(len);
        self.cart_addr = self.cart_addr.wrapping_add(len);
        t
    }

    /// Mark the current transfer complete and raise the PI interrupt.
    ///
    /// Separate from [`Pi::write`] because completion is a *timing* event: the
    /// Bus performs the copy and then tells the PI it is done, which is where a
    /// non-instant DMA will later charge its cycles.
    pub const fn complete(&mut self) {
        self.busy = false;
        self.interrupt = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Length is `len + 1`.** Writing 0 transfers one byte.
    #[test]
    fn a_transfer_is_len_plus_one_bytes() {
        let mut pi = Pi::new();
        let t = pi.write(PI_WR_LEN, 0).expect("started");
        assert_eq!(t.len, 1, "writing 0 transfers ONE byte, not zero");
        let t = pi.write(PI_WR_LEN, 0xFF).expect("started");
        assert_eq!(t.len, 0x100);
    }

    /// `RD`/`WR` are named from the **cartridge's** point of view, so `WR_LEN`
    /// moves cart → RDRAM. Reversing them makes the first ROM load overwrite the
    /// image with uninitialised RDRAM.
    #[test]
    fn wr_len_moves_cart_to_dram_and_rd_len_the_other_way() {
        let mut pi = Pi::new();
        assert!(
            pi.write(PI_WR_LEN, 0).expect("started").to_dram,
            "PI_WR_LEN loads INTO RDRAM"
        );
        assert!(
            !pi.write(PI_RD_LEN, 0).expect("started").to_dram,
            "PI_RD_LEN writes out to the cart"
        );
    }

    /// Only the length writes start a transfer; the address writes do not.
    #[test]
    fn only_a_length_write_starts_a_transfer() {
        let mut pi = Pi::new();
        assert!(pi.write(PI_DRAM_ADDR, 0x1000).is_none());
        assert!(pi.write(PI_CART_ADDR, 0x1000_0000).is_none());
        assert!(pi.write(PI_STATUS, 0).is_none());
        let t = pi.write(PI_WR_LEN, 15).expect("started");
        assert_eq!(t.dram, 0x1000, "the addresses latched first are used");
        assert_eq!(t.cart, 0x1000_0000);
        assert_eq!(t.len, 16);
    }

    /// **Both address registers advance by the transfer length.** Software reads
    /// back the address past the block it just moved, which is how a driver
    /// chains transfers; leaving them put makes every chained DMA re-send the
    /// first block. n64-systemtest checks the delta directly.
    #[test]
    fn a_transfer_advances_both_address_registers_by_its_length() {
        let mut pi = Pi::new();
        pi.write(PI_DRAM_ADDR, 0x1000);
        pi.write(PI_CART_ADDR, 0x1000_0000);
        pi.write(PI_WR_LEN, 0x0F); // 0x10 bytes
        assert_eq!(pi.read(PI_DRAM_ADDR), 0x1010, "moved by the LENGTH, 0x10");
        assert_eq!(pi.read(PI_CART_ADDR), 0x1000_0010);

        // A written length of 1 is a TWO-byte transfer, so the delta is 2.
        let mut pi = Pi::new();
        pi.write(PI_CART_ADDR, 0x1000_0000);
        pi.write(PI_WR_LEN, 1);
        assert_eq!(pi.read(PI_CART_ADDR), 0x1000_0002);
    }

    /// Busy is visible through **both** status flags, because software polls
    /// them interchangeably — `n64-systemtest`'s `ISViewer` waits on `io_busy`.
    #[test]
    fn busy_is_visible_through_both_status_flags() {
        let mut pi = Pi::new();
        assert_eq!(pi.read(PI_STATUS) & (STATUS_DMA_BUSY | STATUS_IO_BUSY), 0);
        pi.write(PI_WR_LEN, 0);
        let s = pi.read(PI_STATUS);
        assert_ne!(s & STATUS_DMA_BUSY, 0);
        assert_ne!(s & STATUS_IO_BUSY, 0, "ISViewer polls io_busy specifically");
        pi.complete();
        assert_eq!(pi.read(PI_STATUS) & (STATUS_DMA_BUSY | STATUS_IO_BUSY), 0);
    }

    /// Completion raises the PI interrupt, and only a `STATUS` write with the
    /// clear bit takes it down. A DMA that never raises leaves software that
    /// waits on the interrupt hung forever.
    #[test]
    fn completion_raises_an_interrupt_that_only_software_clears() {
        let mut pi = Pi::new();
        pi.write(PI_WR_LEN, 0);
        assert!(!pi.interrupt());
        pi.complete();
        assert!(pi.interrupt(), "completion raises the PI interrupt");
        assert_ne!(pi.read(PI_STATUS) & STATUS_INTERRUPT, 0);

        // Another DMA does not clear it...
        pi.write(PI_WR_LEN, 0);
        assert!(pi.interrupt(), "still asserted");
        // ...only the explicit clear does.
        pi.write(PI_STATUS, STATUS_W_CLR_INTR);
        assert!(!pi.interrupt());
    }

    /// **The BSD domain timing registers store and read back**, masked to their
    /// field widths (LAT/PWD 8-bit, PGS 4-bit, RLS 2-bit), for both DOM1 and
    /// DOM2 independently. IPL2 programs DOM1 from the ROM header (LAT 64, PWD
    /// 18, PGS 7, RLS 3 for official ROMs — N64brew *Peripheral Interface*
    /// §Domains), so a game that reads them back must see what it wrote.
    #[test]
    fn the_bsd_domain_timing_registers_store_and_read_back() {
        let mut pi = Pi::new();
        // DOM1 to the official-ROM values.
        pi.write(PI_BSD_DOM1_LAT, 64);
        pi.write(PI_BSD_DOM1_PWD, 18);
        pi.write(PI_BSD_DOM1_PGS, 7);
        pi.write(PI_BSD_DOM1_RLS, 3);
        assert_eq!(pi.read(PI_BSD_DOM1_LAT), 64);
        assert_eq!(pi.read(PI_BSD_DOM1_PWD), 18);
        assert_eq!(pi.read(PI_BSD_DOM1_PGS), 7);
        assert_eq!(pi.read(PI_BSD_DOM1_RLS), 3);
        // PGS is 4-bit and RLS 2-bit — the high bits are dropped.
        pi.write(PI_BSD_DOM1_PGS, 0xFF);
        assert_eq!(pi.read(PI_BSD_DOM1_PGS), 0xF);
        pi.write(PI_BSD_DOM1_RLS, 0xFF);
        assert_eq!(pi.read(PI_BSD_DOM1_RLS), 0x3);
        // DOM2 is independent of DOM1 across every field.
        pi.write(PI_BSD_DOM2_LAT, 0x20);
        assert_eq!(pi.read(PI_BSD_DOM2_LAT), 0x20);
        pi.write(PI_BSD_DOM2_PWD, 0x21);
        assert_eq!(pi.read(PI_BSD_DOM2_PWD), 0x21);
        pi.write(PI_BSD_DOM2_PGS, 0xFF);
        assert_eq!(pi.read(PI_BSD_DOM2_PGS), 0xF, "DOM2 PGS is 4-bit");
        assert_eq!(
            pi.read(PI_BSD_DOM1_LAT),
            64,
            "DOM2 write did not touch DOM1"
        );
        assert_eq!(
            pi.read(PI_BSD_DOM1_PWD),
            0x12,
            "DOM2 write did not touch DOM1 PWD"
        );
        assert_eq!(pi.read(PI_BSD_DOM2_RLS), 0, "unwritten DOM2 RLS reads 0");
    }

    /// The RDRAM address ignores bits 2:0 — the DRAM side is **doubleword**
    /// aligned. Masking only bit 0 lets a transfer start mid-doubleword and
    /// silently shifts every byte of it.
    #[test]
    fn the_dram_address_is_doubleword_aligned() {
        let mut pi = Pi::new();
        for probe in [0x1001u32, 0x1002, 0x1004, 0x1007] {
            pi.write(PI_DRAM_ADDR, probe);
            assert_eq!(
                pi.write(PI_WR_LEN, 0).expect("started").dram,
                0x1000,
                "{probe:#X} must round down to the doubleword"
            );
        }
    }
}
