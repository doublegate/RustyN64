//! The four on-cartridge save backends + the Controller Pak.
//!
//! Two access paths (`docs/cart.md`, `ref-docs/research-report.md` §6):
//! - **PI-bus (DOM2 @ `0x0800_0000`):** SRAM (flat) and FlashRAM (a command
//!   state machine). Driven through `SaveDevice::pi_read`/`pi_write`.
//! - **Joybus (SI/PIF):** EEPROM 4k/16k and the Controller Pak (flat blocks).
//!   Driven through `SaveDevice::eeprom_read_block` etc. by the joybus module.
//!
//! Every backend round-trips a write and reload byte-for-byte — the accuracy
//! oracle (the RustyNES battery-save analog). FlashRAM is modeled as its real
//! erase/program/status machine (n64brew `Flash.md`), not a flat buffer, because
//! a game that issues an erase-then-program sequence corrupts a flat store.
#![allow(
    clippy::doc_markdown,
    reason = "save-backend prose names FlashRAM/SRAM/EEPROM/DMA/CIR constantly"
)]

use alloc::boxed::Box;
use alloc::vec;

use serde::{Deserialize, Serialize};

use crate::SaveType;

/// PI base of the DOM2 save window (SRAM / FlashRAM).
pub const SAVE_PI_BASE: u32 = 0x0800_0000;
/// FlashRAM Command Internal Register, at `base + 0x10000`.
pub const FLASH_CIR: u32 = 0x0801_0000;

/// FlashRAM geometry: 8 sectors × 128 pages × 128 bytes = 128 KiB.
const FLASH_SIZE: usize = 128 * 1024;
const FLASH_PAGE: usize = 128;
const FLASH_SECTOR_PAGES: usize = 128;
const FLASH_SECTOR: usize = FLASH_SECTOR_PAGES * FLASH_PAGE;

/// The FlashRAM chip's operating mode (n64brew `Flash.md`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
enum FlashMode {
    /// Power-on: reads at the base return the data array.
    #[default]
    ReadArray,
    /// Reads return the 8-bit status word (erase/program busy+ok).
    Status,
    /// Reads (via DMA) return the 64-bit silicon ID.
    SiliconId,
    /// Writes at the base fill the 128-byte page buffer.
    LoadPage,
}

/// The FlashRAM state machine.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FlashRam {
    array: Box<[u8]>,
    #[serde(with = "serde_big_array::BigArray")]
    page_buffer: [u8; FLASH_PAGE],
    mode: FlashMode,
    /// The last-set erase target (a page offset within the sector to erase, or
    /// `None` for a pending chip erase). Consumed by the `Erase` (`0x78`) command.
    erase_sector: Option<usize>,
    chip_erase: bool,
    /// The 8-bit status word read in [`FlashMode::Status`].
    status: u8,
}

/// FlashRAM status bits (`Flash.md` §Status Mode). We report operations as
/// instantly complete (BUSY clear, OK set) — the emulated erase/program are not
/// timed, so software that polls sees success immediately.
const FLASH_STATUS_ERASE_OK: u8 = 0x08;
const FLASH_STATUS_PROGRAM_OK: u8 = 0x04;

impl FlashRam {
    fn new() -> Self {
        Self {
            // Erased flash reads back all-ones.
            array: vec![0xFF; FLASH_SIZE].into_boxed_slice(),
            page_buffer: [0xFF; FLASH_PAGE],
            mode: FlashMode::default(),
            erase_sector: None,
            chip_erase: false,
            status: 0,
        }
    }

    /// Write the Command Internal Register (`0x0801_0000`), driving the machine.
    fn write_cir(&mut self, cmd: u32) {
        match cmd >> 24 {
            0xF0 => self.mode = FlashMode::ReadArray,
            0xE1 => self.mode = FlashMode::SiliconId,
            0xB4 => {
                self.mode = FlashMode::LoadPage;
                self.page_buffer = [0xFF; FLASH_PAGE];
            }
            0x3C => {
                self.chip_erase = true;
                self.erase_sector = None;
            }
            0x4B => {
                // Sector Erase Setup: the low bits index a page in the sector.
                // Clamp to the 8 real sectors — a guest can write any 16-bit page
                // index, and an out-of-range sector base would panic the erase.
                self.chip_erase = false;
                let page = (cmd & 0xFFFF) as usize;
                let sector = (page / FLASH_SECTOR_PAGES).min(FLASH_SIZE / FLASH_SECTOR - 1);
                self.erase_sector = Some(sector * FLASH_SECTOR);
            }
            0x78 => {
                // Execute the pending erase → all-ones, then Status mode.
                if self.chip_erase {
                    self.array.fill(0xFF);
                } else if let Some(base) = self.erase_sector {
                    let end = (base + FLASH_SECTOR).min(self.array.len());
                    self.array[base..end].fill(0xFF);
                }
                self.chip_erase = false;
                self.erase_sector = None;
                self.status = FLASH_STATUS_ERASE_OK;
                self.mode = FlashMode::Status;
            }
            0xA5 => {
                // Page Program: copy the page buffer to page `XXX`, then Status.
                let page = (cmd & 0xFFFF) as usize;
                let base = page * FLASH_PAGE;
                if base + FLASH_PAGE <= self.array.len() {
                    // Programming clears bits (AND), matching real flash — a page
                    // must be erased (all-ones) before it can be reprogrammed.
                    for (a, &b) in self.array[base..base + FLASH_PAGE]
                        .iter_mut()
                        .zip(self.page_buffer.iter())
                    {
                        *a &= b;
                    }
                }
                self.status = FLASH_STATUS_PROGRAM_OK;
                self.mode = FlashMode::Status;
            }
            _ => {}
        }
    }

    /// Read a byte at the flash base window (`off` = byte offset from base).
    fn read(&self, off: usize) -> u8 {
        match self.mode {
            FlashMode::ReadArray => self.array.get(off).copied().unwrap_or(0xFF),
            FlashMode::Status => {
                // The status burst is the pattern `00 <status>` repeated, so the
                // status byte sits at every ODD offset (1, 3, 5, …), 0 at even
                // (n64brew `Flash.md` §Status Mode).
                if off & 1 == 1 { self.status } else { 0 }
            }
            // A minimal, stable silicon ID (byte-indexed chip). Real IDs vary;
            // this is a documented stand-in read via the 8-byte DMA path.
            FlashMode::SiliconId => [0x11, 0x11, 0x80, 0x00, 0xC2, 0x00, 0x1D, 0x00]
                .get(off & 7)
                .copied()
                .unwrap_or(0),
            FlashMode::LoadPage => self
                .page_buffer
                .get(off & (FLASH_PAGE - 1))
                .copied()
                .unwrap_or(0xFF),
        }
    }

    /// Write a byte at the flash base window (fills the page buffer in
    /// [`FlashMode::LoadPage`]; ignored otherwise).
    fn write(&mut self, off: usize, val: u8) {
        if self.mode == FlashMode::LoadPage {
            self.page_buffer[off & (FLASH_PAGE - 1)] = val;
        }
    }
}

/// A cartridge save backend, sized + typed from the resolved [`SaveType`].
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub enum SaveDevice {
    /// No on-cart save.
    #[default]
    None,
    /// Serial EEPROM (joybus), flat backing store (512 B or 2 KiB).
    Eeprom(Box<[u8]>),
    /// Battery SRAM (PI DOM2), flat 32 KiB.
    Sram(Box<[u8]>),
    /// FlashRAM (PI DOM2), the erase/program/status machine.
    Flash(FlashRam),
    /// Controller Pak (joybus), flat 32 KiB.
    ControllerPak(Box<[u8]>),
}

impl SaveDevice {
    /// Construct the backend for a resolved save type (erased/zeroed state).
    #[must_use]
    pub fn new(save_type: SaveType) -> Self {
        let flat = |n: usize| vec![0u8; n].into_boxed_slice();
        match save_type {
            SaveType::None => Self::None,
            SaveType::Eeprom4k => Self::Eeprom(flat(512)),
            SaveType::Eeprom16k => Self::Eeprom(flat(2 * 1024)),
            SaveType::Sram => Self::Sram(flat(32 * 1024)),
            SaveType::FlashRam => Self::Flash(FlashRam::new()),
            SaveType::ControllerPak => Self::ControllerPak(flat(32 * 1024)),
        }
    }

    /// PI-bus read (SRAM / FlashRAM) at PI address `addr`. Returns `None` for a
    /// backend not on the PI bus, so the caller can fall through to open bus.
    #[must_use]
    pub fn pi_read(&self, addr: u32) -> Option<u8> {
        let off = addr.wrapping_sub(SAVE_PI_BASE) as usize;
        match self {
            Self::Sram(store) => Some(store.get(off).copied().unwrap_or(0)),
            Self::Flash(flash) => Some(flash.read(off)),
            _ => None,
        }
    }

    /// PI-bus write (SRAM / FlashRAM). The FlashRAM CIR at `0x0801_0000` is a
    /// 32-bit command; the Bus assembles the word and calls [`Self::flash_cir`].
    pub fn pi_write(&mut self, addr: u32, val: u8) {
        match self {
            Self::Sram(store) => {
                let off = addr.wrapping_sub(SAVE_PI_BASE) as usize;
                if let Some(b) = store.get_mut(off) {
                    *b = val;
                }
            }
            Self::Flash(flash) => flash.write(addr.wrapping_sub(SAVE_PI_BASE) as usize, val),
            _ => {}
        }
    }

    /// Write the FlashRAM Command Internal Register (a whole 32-bit word).
    pub fn flash_cir(&mut self, cmd: u32) {
        if let Self::Flash(flash) = self {
            flash.write_cir(cmd);
        }
    }

    /// Joybus EEPROM read of an 8-byte block (`block` index) into `out`.
    pub fn eeprom_read_block(&self, block: u8, out: &mut [u8; 8]) {
        if let Self::Eeprom(store) = self {
            let base = block as usize * 8;
            for (i, o) in out.iter_mut().enumerate() {
                *o = store.get(base + i).copied().unwrap_or(0);
            }
        }
    }

    /// Joybus EEPROM write of an 8-byte block.
    pub fn eeprom_write_block(&mut self, block: u8, data: &[u8; 8]) {
        if let Self::Eeprom(store) = self {
            let base = block as usize * 8;
            for (i, &d) in data.iter().enumerate() {
                if let Some(b) = store.get_mut(base + i) {
                    *b = d;
                }
            }
        }
    }

    /// Joybus Controller-Pak read of a 32-byte block at `addr` (byte offset).
    pub fn cpak_read(&self, addr: u16, out: &mut [u8; 32]) {
        if let Self::ControllerPak(store) = self {
            let base = addr as usize;
            for (i, o) in out.iter_mut().enumerate() {
                *o = store.get(base + i).copied().unwrap_or(0);
            }
        }
    }

    /// Joybus Controller-Pak write of a 32-byte block at `addr`.
    pub fn cpak_write(&mut self, addr: u16, data: &[u8; 32]) {
        if let Self::ControllerPak(store) = self {
            let base = addr as usize;
            for (i, &d) in data.iter().enumerate() {
                if let Some(b) = store.get_mut(base + i) {
                    *b = d;
                }
            }
        }
    }

    /// The persistable backing bytes (for the host save file). FlashRAM exposes
    /// its data array; the others their flat store. Empty for [`Self::None`].
    #[must_use]
    pub fn backing(&self) -> &[u8] {
        match self {
            Self::None => &[],
            Self::Eeprom(s) | Self::Sram(s) | Self::ControllerPak(s) => s,
            Self::Flash(f) => &f.array,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sram_round_trips() {
        let mut d = SaveDevice::new(SaveType::Sram);
        d.pi_write(SAVE_PI_BASE + 0x1234, 0xAB);
        d.pi_write(SAVE_PI_BASE + 0x1235, 0xCD);
        assert_eq!(d.pi_read(SAVE_PI_BASE + 0x1234), Some(0xAB));
        assert_eq!(d.pi_read(SAVE_PI_BASE + 0x1235), Some(0xCD));
        assert_eq!(d.backing()[0x1234], 0xAB);
    }

    #[test]
    fn eeprom_block_round_trips() {
        let mut d = SaveDevice::new(SaveType::Eeprom4k);
        let data = [1, 2, 3, 4, 5, 6, 7, 8];
        d.eeprom_write_block(3, &data);
        let mut out = [0u8; 8];
        d.eeprom_read_block(3, &mut out);
        assert_eq!(out, data);
        // A different block is untouched.
        let mut other = [0xFFu8; 8];
        d.eeprom_read_block(4, &mut other);
        assert_eq!(other, [0; 8]);
    }

    #[test]
    fn controller_pak_block_round_trips() {
        let mut d = SaveDevice::new(SaveType::ControllerPak);
        let data = [0x5A; 32];
        d.cpak_write(0x0100, &data);
        let mut out = [0u8; 32];
        d.cpak_read(0x0100, &mut out);
        assert_eq!(out, data);
    }

    /// **FlashRAM erase → load-page → program round-trips through its real
    /// state machine.** A flat store would mis-handle this: program only clears
    /// bits, so the page must be erased (all-ones) first.
    #[test]
    fn flashram_erase_load_program_round_trips() {
        let mut d = SaveDevice::new(SaveType::FlashRam);
        // Erased flash reads all-ones (ReadArray is the power-on mode).
        assert_eq!(d.pi_read(SAVE_PI_BASE), Some(0xFF));

        // Sector-erase setup for sector 0, then execute.
        d.flash_cir(0x4B00_0000);
        d.flash_cir(0x7800_0000);
        // Now in Status mode: the low status byte reports ERASE_OK.
        assert_eq!(d.pi_read(SAVE_PI_BASE + 3), Some(FLASH_STATUS_ERASE_OK));

        // Load the 128-byte page buffer, program it into page 0.
        d.flash_cir(0xB400_0000);
        for i in 0..FLASH_PAGE {
            d.pi_write(SAVE_PI_BASE + i as u32, i as u8);
        }
        d.flash_cir(0xA500_0000);
        assert_eq!(d.pi_read(SAVE_PI_BASE + 3), Some(FLASH_STATUS_PROGRAM_OK));

        // Back to ReadArray: page 0 holds the programmed bytes.
        d.flash_cir(0xF000_0000);
        assert_eq!(d.pi_read(SAVE_PI_BASE), Some(0));
        assert_eq!(d.pi_read(SAVE_PI_BASE + 5), Some(5));
        assert_eq!(d.backing()[7], 7);
    }

    /// FlashRAM program is an AND (clears bits only) — a bit cannot be set from
    /// 0 to 1 without an intervening erase.
    #[test]
    fn flashram_program_only_clears_bits() {
        let mut d = SaveDevice::new(SaveType::FlashRam);
        d.flash_cir(0xB400_0000);
        d.pi_write(SAVE_PI_BASE, 0x0F); // program 0x0F over the erased 0xFF
        d.flash_cir(0xA500_0000);
        d.flash_cir(0xF000_0000);
        assert_eq!(d.pi_read(SAVE_PI_BASE), Some(0x0F));
        // Program 0xF0 without erasing: AND with 0x0F → 0x00, not 0xF0.
        d.flash_cir(0xB400_0000);
        d.pi_write(SAVE_PI_BASE, 0xF0);
        d.flash_cir(0xA500_0000);
        d.flash_cir(0xF000_0000);
        assert_eq!(
            d.pi_read(SAVE_PI_BASE),
            Some(0x00),
            "program only clears bits"
        );
    }
}
