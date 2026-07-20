//! Direct ROM loading, bypassing the boot sequence (T-11-006).
//!
//! On hardware, IPL3 copies the ROM into RDRAM and jumps to its entry point.
//! Phase 1 has no CIC handshake and no PI DMA, so the harness does that copy
//! itself — which is exactly what the phase plan anticipated ("test ROMs are
//! loaded directly, with the boot sequence stubbed").
//!
//! This is a **harness** facility, deliberately not a core one. The core must
//! never depend on it, or the determinism contract would acquire a load-path
//! dependency (`docs/engineering-lessons.md` §3.4).

use rustyn64_core::System;

/// Bytes IPL3 copies from the cartridge into RDRAM.
///
/// The documented boot behaviour — *"copy 0x100000 bytes from 0x10001000 to
/// 0x00001000"* (`ref-proj/n64-tests/README.md`). It is **not** derived from any
/// particular ROM's size; hard-coding an end offset happens to work for
/// `basic.z64` and breaks on the next ROM.
pub const IPL3_COPY_BYTES: usize = 0x10_0000;

/// Where the copy starts in the ROM image (past the 4 KiB header + IPL3).
pub const ROM_PAYLOAD_OFFSET: usize = 0x1000;

/// Where it lands in RDRAM.
pub const RDRAM_LOAD_OFFSET: usize = 0x1000;

/// Why a ROM could not be loaded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoadError {
    /// Smaller than the header, so there is no payload at all.
    TooSmall,
}

/// Load `rom` into RDRAM the way IPL3 would, and set the PC to `entry`.
///
/// Copies at most [`IPL3_COPY_BYTES`], clamped to what the ROM actually contains
/// **and** to what RDRAM can hold — a commercial ROM is up to 64 MiB against
/// 8 MiB of RDRAM, so an unclamped copy would panic on the very ROMs the
/// commercial corpus is made of.
///
/// # Errors
///
/// [`LoadError::TooSmall`] if the image has no payload past the header.
pub fn load_direct(system: &mut System, rom: &[u8], entry: u64) -> Result<usize, LoadError> {
    let payload = rom.get(ROM_PAYLOAD_OFFSET..).ok_or(LoadError::TooSmall)?;
    let capacity = system.bus.rdram.len().saturating_sub(RDRAM_LOAD_OFFSET);
    let n = payload.len().min(IPL3_COPY_BYTES).min(capacity);
    system.bus.rdram[RDRAM_LOAD_OFFSET..RDRAM_LOAD_OFFSET + n].copy_from_slice(&payload[..n]);
    system.cpu.set_pc(entry);
    Ok(n)
}

/// Read the entry point from a ROM header (big-endian, offset `0x08`).
///
/// # Errors
///
/// [`LoadError::TooSmall`] if the header is truncated.
pub fn entry_point(rom: &[u8]) -> Result<u64, LoadError> {
    let b = rom.get(8..12).ok_or(LoadError::TooSmall)?;
    Ok(u64::from(u32::from_be_bytes([b[0], b[1], b[2], b[3]])))
}
