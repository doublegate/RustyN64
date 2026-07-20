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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Pi {
    dram_addr: u32,
    cart_addr: u32,
    /// Set while a transfer is outstanding.
    busy: bool,
    /// The PI interrupt line, which the MI aggregates into `Cause.IP2`.
    interrupt: bool,
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
        }
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
            // The length registers read back as 0x7F on hardware; the domain
            // registers are not modelled. Returning 0 for both is a documented
            // simplification, not a hardware fact.
            _ => 0,
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
            _ => None,
        }
    }

    /// Begin a transfer of `len + 1` bytes.
    const fn start(&mut self, len: u32, to_dram: bool) -> Transfer {
        self.busy = true;
        Transfer {
            dram: self.dram_addr,
            cart: self.cart_addr,
            // "+1" is the rule everything gets wrong once: writing 0 transfers
            // ONE byte. Being short by one corrupts the last byte of every
            // block, which presents as memory corruption rather than a DMA bug.
            len: (len & 0x00FF_FFFF) + 1,
            to_dram,
        }
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
