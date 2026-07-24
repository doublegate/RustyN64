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
//! shared RDRAM access path that the RDP also borrows.
//! `#![no_std]` + `alloc`; only the frontend carries `std` + `unsafe`.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::cast_possible_truncation, clippy::cast_lossless)]
// Skeleton `tick`/hook methods are deliberately non-`const` (they will drive
// PI/SI DMA and the FlashRAM state machine).
#![allow(clippy::missing_const_for_fn)]

extern crate alloc;

/// The PI (Peripheral Interface) DMA engine + BSD domain timing registers.
pub mod pi;
/// The PIF RAM + the SI joybus frame executor (controllers, EEPROM, Pak).
pub mod pif;
/// The four on-cartridge save backends (SRAM, FlashRAM, EEPROM, Controller Pak).
pub mod save;

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

    /// Read the RDRAM "hidden" bits for the 16-bit halfword at `addr` — the 9th
    /// bit RDRAM carries per byte, which the RDP Z-buffer uses for the low 2 bits
    /// of the per-pixel `dz`. Returns the 2-bit value (`0..=3`).
    ///
    /// Default returns 0, so an impl that does not model the hidden bits behaves
    /// as if they read back clear (which is also the power-on state).
    fn rdram_read_hidden(&self, addr: u32) -> u8 {
        let _ = addr;
        0
    }

    /// Write the RDRAM hidden bits (`0..=3`) for the halfword at `addr`. Default
    /// no-op for impls that do not model them.
    fn rdram_write_hidden(&mut self, addr: u32, val: u8) {
        let _ = (addr, val);
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

/// The boot secrets a CIC hands the PIF at power-on.
///
/// From `n64brew_wiki/markdown/PIF-NUS.md` §checksum table: the two 8-bit seeds
/// and the 6-byte IPL2 checksum. On the real-PIF path the PIF writes the seeds
/// into PIF RAM (IPL2 reads them to run its own checksum) and keeps the checksum
/// to compare against the value IPL2 computes — a mismatch NMI-halts the CPU.
/// These are documented constants, not the SM5 firmware; the seed bytes
/// cross-check the per-CIC seed values the HLE boot path already injects.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CicBootSecrets {
    /// The 8-bit IPL2 seed (used by IPL2's checksum over the cart's IPL3).
    pub ipl2_seed: u8,
    /// The 8-bit IPL3 seed (used by IPL3's checksum over the first MiB).
    pub ipl3_seed: u8,
    /// The 6-byte IPL2 checksum the PIF expects the CPU to reproduce.
    pub ipl2_checksum: [u8; 6],
}

/// Standard CRC-32 (reflected, poly `0xEDB8_8320`) — the fingerprint used to
/// identify a cartridge's CIC from its IPL3 (cen64 `si/cic.c` `si_crc32`).
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                0xEDB8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

impl Cic {
    /// Identify the CIC from the cartridge IPL3 (`rom[0x40..0x1000]`) by its
    /// CRC-32 — the standard fingerprint (cen64 `si/cic.c`, N64brew *CIC-NUS*).
    /// The core never consults a per-game database (ADR 0003/0004); this reads
    /// only the ROM's own boot code. An unknown IPL3 (homebrew, e.g.
    /// n64-systemtest, which ships its own) falls back to 6102 — the seed only
    /// feeds the boot handshake, and a custom IPL3 does not depend on the stock
    /// checksum path (the same fallback cen64 documents).
    #[must_use]
    pub fn from_ipl3(rom: &[u8]) -> Self {
        let Some(ipl3) = rom.get(0x40..0x1000) else {
            return Self::Cic6102;
        };
        // CRC values: cen64 `si/cic.c`. 7102 and the three iQue variants share
        // the 6101 seed; 8303 (64DD), the common 6102 fingerprint `0x90BB_6CB5`,
        // and every unknown/homebrew IPL3 all resolve to 6102 (the last arm).
        match crc32(ipl3) {
            0x6170_A4A1 | 0x009E_9EA3 | 0xCD19_FEF1 | 0xB98C_ED9A | 0xE71C_2766 => Self::Cic6101,
            0x0B05_0EE0 => Self::Cic6103,
            0x98BC_2C86 => Self::Cic6105,
            0xACC8_580A => Self::Cic6106,
            // `0x90BB_6CB5` (6102) folds into this default — the same variant.
            _ => Self::Cic6102,
        }
    }

    /// The [`CicBootSecrets`] for this variant. All modelled variants are the
    /// NTSC members; the 7xxx PAL twins share the same seeds and checksums, so
    /// region is a separate axis (the PIF SM5 ROM, not the CIC, is region-locked).
    #[must_use]
    pub const fn boot_secrets(self) -> CicBootSecrets {
        let (ipl2_seed, ipl3_seed, ipl2_checksum) = match self {
            Self::Cic6101 => (0x3F, 0x3F, [0x45, 0xCC, 0x73, 0xEE, 0x31, 0x7A]),
            Self::Cic6102 => (0x3F, 0x3F, [0xA5, 0x36, 0xC0, 0xF1, 0xD8, 0x59]),
            Self::Cic6103 => (0x78, 0x78, [0x58, 0x6F, 0xD4, 0x70, 0x98, 0x67]),
            Self::Cic6105 => (0x91, 0x91, [0x86, 0x18, 0xA4, 0x5B, 0xC2, 0xD3]),
            Self::Cic6106 => (0x85, 0x85, [0x2B, 0xBA, 0xD4, 0xE6, 0xEB, 0x74]),
        };
        CicBootSecrets {
            ipl2_seed,
            ipl3_seed,
            ipl2_checksum,
        }
    }
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
        // The CIC is fingerprinted from the ROM's own IPL3 (not a per-game DB).
        // TODO(T-CART-02): resolve save_type from the per-game DB by serial/CRC.
        Ok(Self {
            title,
            game_code,
            save_type: SaveType::None,
            cic: Cic::from_ipl3(rom),
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

/// The cartridge trait. One cart model, parameterized by save type, CIC, and
/// region (ADR 0003) — the N64 has no mapper equivalent.
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

/// PI cart + PIF/CIC + save state — the concrete board.
#[derive(Debug, Default)]
pub struct Cart {
    /// The normalized (big-endian) ROM image.
    rom: Vec<u8>,
    /// Parsed header (title / game code / save+CIC selection).
    header: RomHeader,
    /// The active save backend (PI-bus SRAM/FlashRAM or joybus EEPROM/Pak).
    save: save::SaveDevice,
    /// The PIF: its 64-byte RAM + the joybus executor for controllers/accessories.
    pif: pif::Pif,
}

/// PI domain-1 base (the cartridge ROM window).
const ROM_PI_BASE: u32 = 0x1000_0000;

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
        let save = save::SaveDevice::new(header.save_type);
        let mut pif = pif::Pif::new();
        // A Controller-Pak cart advertises its pak on port 0 so the game's
        // accessory probe (`0x00` info → status 1) finds it.
        pif.set_pak_present(0, header.save_type == SaveType::ControllerPak);
        Ok(Self {
            rom,
            header,
            save,
            pif,
        })
    }

    /// The PIF's 64-byte RAM (the SI DMA copies it to/from RDRAM).
    #[must_use]
    pub const fn pif_ram(&self) -> &[u8; pif::PIF_RAM_LEN] {
        self.pif.ram()
    }

    /// Load the whole PIF RAM (an SI 64-byte write from RDRAM).
    pub fn pif_load(&mut self, bytes: &[u8; pif::PIF_RAM_LEN]) {
        self.pif.load(bytes);
    }

    /// A CPU direct read/write byte of PIF RAM.
    #[must_use]
    pub fn pif_read(&self, off: usize) -> u8 {
        self.pif.read(off)
    }

    /// Write a CPU direct byte of PIF RAM.
    pub const fn pif_write(&mut self, off: usize, val: u8) {
        self.pif.write(off, val);
    }

    /// Execute the pending joybus frame (on an SI read), using the four packed
    /// controller port words and the cart's save backend.
    pub fn pif_execute(&mut self, controllers: &[u32; 4]) {
        self.pif.execute(controllers, &mut self.save);
    }

    /// Install the PIF boot ROM (IPL1/IPL2) for the real-PIF boot path.
    pub fn pif_load_boot_rom(&mut self, bytes: &[u8]) {
        self.pif.load_boot_rom(bytes);
    }

    /// Read a byte of the PIF boot ROM (`0x1FC0_0000 + off`); 0 when absent (HLE).
    #[must_use]
    pub fn pif_boot_rom_read(&self, off: usize) -> u8 {
        self.pif.boot_rom_read(off)
    }

    /// Is a real PIF boot ROM installed (real-PIF boot path active)?
    #[must_use]
    pub const fn pif_has_boot_rom(&self) -> bool {
        self.pif.has_boot_rom()
    }

    /// Register the CIC's IPL2 checksum for the real-PIF boot verify.
    pub const fn pif_set_boot_checksum(&mut self, checksum: [u8; 6]) {
        self.pif.set_boot_checksum(checksum);
    }

    /// Process a reset-mode PIF command-byte write (real-PIF boot); returns `true`
    /// if the checksum verify failed and the CPU must be NMI-halted.
    pub fn pif_boot_command(&mut self) -> bool {
        self.pif.boot_command()
    }

    /// Warm-reset the transient PIF boot state (unlock the ROM, drop the latched
    /// checksum) so a reset can re-run IPL1→IPL2.
    pub const fn pif_reset_boot(&mut self) {
        self.pif.reset_boot();
    }

    /// The parsed cartridge header.
    #[must_use]
    pub const fn header(&self) -> &RomHeader {
        &self.header
    }

    /// The persistable save backing bytes (empty for [`SaveType::None`]).
    #[must_use]
    pub fn save(&self) -> &[u8] {
        self.save.backing()
    }

    /// Mutable access to the save device (the joybus module drives EEPROM/Pak).
    #[must_use]
    pub fn save_device_mut(&mut self) -> &mut save::SaveDevice {
        &mut self.save
    }

    /// A whole-word PI write (direct-I/O or DMA). Routes the `FlashRAM` Command
    /// Internal Register (`0x0801_0000`) as a 32-bit command; other DOM2-window
    /// writes fall to the byte path. ROM-window writes are ignored (read-only).
    pub fn pi_write_word(&mut self, addr: u32, word: u32) {
        if addr & !3 == save::FLASH_CIR {
            self.save.flash_cir(word);
        } else {
            for (i, b) in word.to_be_bytes().into_iter().enumerate() {
                self.pi_write(addr.wrapping_add(i as u32), b);
            }
        }
    }

    /// Advance one unit of cart time (PI/SI DMA progress).
    pub fn tick(&mut self) {
        // TODO(T-CART-01): step any in-flight PI/SI DMA transfer.
    }
}

impl Cartridge for Cart {
    fn pi_read(&mut self, addr: u32) -> u8 {
        // The DOM2 save window (SRAM / FlashRAM) takes priority, but **only**
        // within that window — otherwise a SRAM/FlashRAM cart's `pi_read` would
        // answer for the ROM window too (the save's flat store returns `0` past
        // its end), and the game could never read its own ROM through PI DMA.
        if (save::SAVE_PI_BASE..ROM_PI_BASE).contains(&addr)
            && let Some(b) = self.save.pi_read(addr)
        {
            return b;
        }
        let off = (addr as usize).wrapping_sub(ROM_PI_BASE as usize);
        self.rom.get(off).copied().unwrap_or(0)
    }

    fn pi_write(&mut self, addr: u32, val: u8) {
        // Byte writes reach SRAM and the FlashRAM page buffer; the ROM is
        // read-only (writes ignored). The FlashRAM CIR is word-only — see
        // `pi_write_word`.
        if (save::SAVE_PI_BASE..ROM_PI_BASE).contains(&addr) && addr & !3 != save::FLASH_CIR {
            self.save.pi_write(addr, val);
        }
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

    #[test]
    fn crc32_matches_the_standard_check_vector() {
        // The canonical CRC-32 check value for the ASCII string "123456789".
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn an_unknown_ipl3_falls_back_to_6102() {
        // An all-zero IPL3 is not in the fingerprint table — homebrew ships its
        // own boot code, so 6102 is the documented fallback (cen64).
        let rom = alloc::vec![0u8; 0x1000];
        assert_eq!(Cic::from_ipl3(&rom), Cic::Cic6102);
        // Too short to hold an IPL3 → same fallback, no panic.
        assert_eq!(Cic::from_ipl3(&[0u8; 0x40]), Cic::Cic6102);
    }

    #[test]
    fn boot_secrets_match_the_pif_nus_table() {
        // N64brew PIF-NUS §checksum table — the IPL2 seeds differ per CIC (the
        // real IPL2 consumes them), and the 6-byte checksum is the authenticator.
        assert_eq!(
            Cic::Cic6102.boot_secrets().ipl2_checksum,
            [0xA5, 0x36, 0xC0, 0xF1, 0xD8, 0x59]
        );
        assert_eq!(Cic::Cic6102.boot_secrets().ipl2_seed, 0x3F);
        assert_eq!(Cic::Cic6103.boot_secrets().ipl2_seed, 0x78);
        assert_eq!(Cic::Cic6105.boot_secrets().ipl2_seed, 0x91);
        assert_eq!(Cic::Cic6106.boot_secrets().ipl2_seed, 0x85);
        // In every known CIC the IPL2 and IPL3 seeds coincide.
        for cic in [
            Cic::Cic6101,
            Cic::Cic6102,
            Cic::Cic6103,
            Cic::Cic6105,
            Cic::Cic6106,
        ] {
            let s = cic.boot_secrets();
            assert_eq!(s.ipl2_seed, s.ipl3_seed, "seeds coincide for {cic:?}");
        }
    }
}
