//! `rustyn64-cart` — PI cart interface + PIF/CIC boot + cartridge saves.
//!
//! Models the Peripheral Interface (PI) DMA path to the cartridge ROM, the PIF
//! boot ROM + CIC seed handshake (SI side), and the on-cart save backends
//! (EEPROM 4k/16k, SRAM, `FlashRAM`, Controller Pak). This is a **skeleton** —
//! the real PI/SI DMA engines, the CIC challenge/response, and the `FlashRAM`
//! state machine are roadmap phases left as marked TODOs.
//!
//! Part of the one-directional chip-crate graph (see `docs/architecture.md`):
//! this crate does NOT depend on any other chip crate. The RDP depends on it
//! ONLY for the shared RDRAM memory-bus trait ([`RdramBus`]) — the N64 analog
//! of how `rustynes-ppu` reaches CHR/nametable storage through `rustynes-mappers`.
//! `#![no_std]` + `alloc`; only the frontend carries `std` + `unsafe`.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::cast_possible_truncation, clippy::cast_lossless)]
// Skeleton `tick`/hook methods are deliberately non-`const` (they will drive
// PI/SI DMA and the FlashRAM state machine).
#![allow(clippy::missing_const_for_fn)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;

/// The shared RDRAM memory bus, as seen by chips that DMA into/out of main RAM.
///
/// The RDP (framebuffer + texture fetches via the RDRAM) and the PI/SI DMA
/// engines all read and write RDRAM through this narrow trait. `rustyn64-core`
/// owns the concrete 8 MiB (4 MiB base + 4 MiB Expansion Pak) backing store and
/// implements this; keeping the trait in `rustyn64-cart` lets `rustyn64-rdp`
/// depend on exactly one chip crate, preserving the one-directional graph.
pub trait RdramBus {
    /// Read a byte from RDRAM at a physical address.
    fn rdram_read(&self, addr: u32) -> u8;
    /// Write a byte to RDRAM at a physical address.
    fn rdram_write(&mut self, addr: u32, val: u8);

    /// Read a big-endian 32-bit word from RDRAM. Default composes four byte
    /// reads; `rustyn64-core` overrides with a fast slice path.
    fn rdram_read_u32(&self, addr: u32) -> u32 {
        u32::from_be_bytes([
            self.rdram_read(addr),
            self.rdram_read(addr.wrapping_add(1)),
            self.rdram_read(addr.wrapping_add(2)),
            self.rdram_read(addr.wrapping_add(3)),
        ])
    }
}

/// On-cartridge non-volatile save backend type.
///
/// Detected from the per-game database (the IPL/ROM has no reliable in-header
/// save-type field, unlike the iNES mapper byte) — keyed off the cart serial /
/// CRC. `None` means the title saves only to the Controller Pak (or not at all).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Default)]
pub enum SaveType {
    /// No on-cart save chip.
    #[default]
    None,
    /// 4 Kbit serial EEPROM (512 bytes).
    Eeprom4k,
    /// 16 Kbit serial EEPROM (2 KiB).
    Eeprom16k,
    /// 256 Kbit battery-backed SRAM (32 KiB).
    Sram,
    /// 1 Mbit `FlashRAM` (128 KiB).
    FlashRam,
    /// Removable Controller Pak / Memory Pak (32 KiB, via the SI joybus).
    ControllerPak,
}

impl SaveType {
    /// Backing-store size in bytes (`0` for [`SaveType::None`]).
    #[must_use]
    pub const fn size_bytes(self) -> usize {
        match self {
            Self::None => 0,
            Self::Eeprom4k => 512,
            Self::Eeprom16k => 2 * 1024,
            Self::Sram | Self::ControllerPak => 32 * 1024,
            Self::FlashRam => 128 * 1024,
        }
    }
}

/// The CIC (boot-security copy-protection) lockout-chip variant.
///
/// The PIF and the CIC exchange a seeded challenge/response at boot; the variant
/// fixes the seed + checksum the IPL3 expects. Skeleton — the handshake itself
/// is a roadmap phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Default)]
pub enum Cic {
    /// 6101 (early NTSC: Star Fox 64).
    Cic6101,
    /// 6102 / 7101 (the common NTSC/PAL variant).
    #[default]
    Cic6102,
    /// 6103 / 7103.
    Cic6103,
    /// 6105 / 7105 (uses the X105 IPL2 ramp).
    Cic6105,
    /// 6106 / 7106.
    Cic6106,
}

/// ROM image byte order, derived from the magic in the first four bytes.
///
/// `.z64` is big-endian (native), `.n64` is little-endian (byte-swapped),
/// `.v64` is byte-swapped within each 16-bit halfword. The loader normalizes
/// everything to big-endian internally.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum RomFormat {
    /// `.z64` — big-endian, the canonical internal order.
    Z64BigEndian,
    /// `.n64` — little-endian (32-bit word swap).
    N64LittleEndian,
    /// `.v64` — byte-swapped halfwords.
    V64ByteSwapped,
}

impl RomFormat {
    /// Detect the format from the leading four magic bytes, or `None` if the
    /// header is too short / unrecognized.
    #[must_use]
    pub fn detect(magic: &[u8]) -> Option<Self> {
        match magic {
            [0x80, 0x37, 0x12, 0x40, ..] => Some(Self::Z64BigEndian),
            [0x40, 0x12, 0x37, 0x80, ..] => Some(Self::N64LittleEndian),
            [0x37, 0x80, 0x40, 0x12, ..] => Some(Self::V64ByteSwapped),
            _ => None,
        }
    }
}

/// Parsed cartridge header (the first 0x40 bytes of the ROM image).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RomHeader {
    /// Internal game title (0x20..0x34, space-padded ASCII).
    pub title: [u8; 20],
    /// Cartridge serial / game code (0x3B..0x3F, e.g. `NSME`).
    pub game_code: [u8; 4],
    /// Detected save backend (resolved via the per-game DB, not the header).
    pub save_type: SaveType,
    /// CIC lockout-chip variant (resolved from the IPL3 checksum / DB).
    pub cic: Cic,
}

impl RomHeader {
    /// Parse a normalized big-endian header. Skeleton: only the title / game
    /// code are extracted; `save_type` + `cic` are DB-resolved elsewhere.
    ///
    /// # Errors
    /// Returns [`CartError::ShortHeader`] if `rom` is shorter than 0x40 bytes.
    pub fn parse(rom: &[u8]) -> Result<Self, CartError> {
        if rom.len() < 0x40 {
            return Err(CartError::ShortHeader);
        }
        let mut title = [0u8; 20];
        title.copy_from_slice(&rom[0x20..0x34]);
        let mut game_code = [0u8; 4];
        game_code.copy_from_slice(&rom[0x3B..0x3F]);
        // TODO(T-CART-02): resolve save_type + cic from the per-game DB by serial/CRC.
        Ok(Self {
            title,
            game_code,
            save_type: SaveType::None,
            cic: Cic::default(),
        })
    }
}

/// Error type for cartridge loading / parsing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CartError {
    /// The ROM image is shorter than a 0x40-byte header.
    ShortHeader,
    /// The leading magic bytes matched no known `.z64`/`.n64`/`.v64` order.
    UnknownFormat,
}

/// The cartridge / board trait (the N64 analog of `RustyNES`'s `Mapper`).
///
/// Board-specific behavior — PI/SI-mediated reads and writes, save backing,
/// optional bootstrap — lives behind it, not in the CPU. All hooks default to
/// no-op so a board implements only what it uses.
pub trait Cartridge {
    /// PI-side read from the cartridge address space (`$1000_0000..`).
    fn pi_read(&mut self, addr: u32) -> u8 {
        let _ = addr;
        0
    }
    /// PI-side write into the cartridge address space (save SRAM/FlashRAM, regs).
    fn pi_write(&mut self, addr: u32, val: u8) {
        let _ = (addr, val);
    }
    /// SI-side joybus exchange (controllers + Controller Pak). Default no-op.
    fn si_exchange(&mut self, _channel: u8, _tx: &[u8], _rx: &mut [u8]) {}
    /// Per-CPU-cycle hook (counter-driven cart hardware). Default no-op.
    fn notify_cpu_cycle(&mut self) {}
    /// The active save backend for this cartridge.
    fn save_type(&self) -> SaveType {
        SaveType::None
    }
}

/// PI cart + PIF/CIC + save state — the skeleton concrete board.
#[derive(Debug, Default)]
pub struct Cart {
    /// The normalized (big-endian) ROM image.
    rom: Vec<u8>,
    /// Parsed header (title / game code / save+CIC selection).
    header: RomHeader,
    /// Battery-backed save backing store (sized by [`RomHeader::save_type`]).
    save: Box<[u8]>,
    // TODO(T-CART-01): PI DMA engine state, PIF RAM (64 bytes), CIC seed/state,
    // FlashRAM mode machine — see `docs/cart.md`.
}

impl Cart {
    /// Construct an empty cart at power-on.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Load + normalize a raw ROM image of any supported byte order.
    ///
    /// # Errors
    /// [`CartError::UnknownFormat`] for an unrecognized magic, or
    /// [`CartError::ShortHeader`] for a truncated header.
    pub fn load(raw: &[u8]) -> Result<Self, CartError> {
        let format = RomFormat::detect(raw).ok_or(CartError::UnknownFormat)?;
        let rom = normalize_to_big_endian(raw, format);
        let header = RomHeader::parse(&rom)?;
        let save = alloc::vec![0u8; header.save_type.size_bytes()].into_boxed_slice();
        Ok(Self { rom, header, save })
    }

    /// The parsed cartridge header.
    #[must_use]
    pub const fn header(&self) -> &RomHeader {
        &self.header
    }

    /// The save backing store (empty for [`SaveType::None`]).
    #[must_use]
    pub const fn save(&self) -> &[u8] {
        // `Box<[u8]>` derefs to `&[u8]`; expose read access for the frontend.
        &self.save
    }

    /// Advance one unit of cart time (PI/SI DMA progress).
    ///
    /// Hot path: keep allocation-free.
    pub fn tick(&mut self) {
        // TODO(T-CART-01): step any in-flight PI/SI DMA transfer.
    }
}

impl Cartridge for Cart {
    fn pi_read(&mut self, addr: u32) -> u8 {
        // Skeleton cart-ROM read at the PI domain-1 base (`$1000_0000`).
        let off = (addr as usize).wrapping_sub(0x1000_0000);
        self.rom.get(off).copied().unwrap_or(0)
    }

    fn save_type(&self) -> SaveType {
        self.header.save_type
    }
}

/// Normalize a raw ROM image to internal big-endian (`.z64`) order.
fn normalize_to_big_endian(raw: &[u8], format: RomFormat) -> Vec<u8> {
    match format {
        RomFormat::Z64BigEndian => raw.to_vec(),
        RomFormat::V64ByteSwapped => {
            let mut out = raw.to_vec();
            for pair in out.chunks_exact_mut(2) {
                pair.swap(0, 1);
            }
            out
        }
        RomFormat::N64LittleEndian => {
            let mut out = raw.to_vec();
            for word in out.chunks_exact_mut(4) {
                word.swap(0, 3);
                word.swap(1, 2);
            }
            out
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

    #[test]
    fn detects_rom_formats() {
        assert_eq!(
            RomFormat::detect(&[0x80, 0x37, 0x12, 0x40]),
            Some(RomFormat::Z64BigEndian)
        );
        assert_eq!(
            RomFormat::detect(&[0x40, 0x12, 0x37, 0x80]),
            Some(RomFormat::N64LittleEndian)
        );
        assert_eq!(
            RomFormat::detect(&[0x37, 0x80, 0x40, 0x12]),
            Some(RomFormat::V64ByteSwapped)
        );
        assert_eq!(RomFormat::detect(&[0, 0, 0, 0]), None);
    }

    #[test]
    fn save_sizes() {
        assert_eq!(SaveType::Eeprom4k.size_bytes(), 512);
        assert_eq!(SaveType::FlashRam.size_bytes(), 128 * 1024);
        assert_eq!(SaveType::None.size_bytes(), 0);
    }

    #[test]
    fn short_header_errors() {
        assert_eq!(RomHeader::parse(&[0u8; 8]), Err(CartError::ShortHeader));
    }

    #[test]
    fn constructs() {
        let cart = Cart::new();
        assert_eq!(cart.save_type(), SaveType::None);
    }

    #[test]
    fn version_is_non_empty() {
        assert!(!version().is_empty());
    }
}
