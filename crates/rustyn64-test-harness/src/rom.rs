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
    /// No ELF header, so there is no IPL3 handoff to synthesise.
    NoElfHeader,
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

/// Seed RSP DMEM with what **IPL3 would have written**, for a harness that
/// loads a ROM directly instead of running the boot ROM.
///
/// The direct-load path skips IPL3 entirely, which is fine for a ROM that only
/// needs its code in RDRAM — and wrong for one that reads IPL3's handoff area.
/// n64-systemtest does exactly that on its **second and third instructions**:
///
/// ```text
/// let memory_size       = SPMEM::read(0);
/// let elf_header_offset = (SPMEM::read(12) >> 16) << 8;
/// MemoryMap::init(memory_size, elf_header_offset);
/// ```
///
/// With DMEM reading zero it built a memory map from nothing and jumped into a
/// zero-filled region, NOP-sliding until it fell off the top of RDRAM — 191
/// instructions in, which is why raising the instruction budget never helped.
///
/// `elf_offset` is located by searching the image for the ELF magic rather than
/// hard-coded, so this does not silently rot if the ROM is rebuilt.
///
/// # Errors
///
/// [`LoadError::NoElfHeader`] if the image carries no ELF header, since then
/// there is nothing meaningful to hand over.
pub fn seed_ipl3_handoff(system: &mut System, rom: &[u8]) -> Result<usize, LoadError> {
    let elf_offset = find_elf_offset(rom).ok_or(LoadError::NoElfHeader)?;

    // Word 0: the RDRAM size IPL3 detected.
    let size = u32::try_from(system.bus.rdram.len()).unwrap_or(u32::MAX);
    write_spmem_word(system, 0, size);
    // Word 12: the ELF offset, in the packed form the suite unpacks with
    // `(v >> 16) << 8`. Storing the offset directly would be read as a
    // different number entirely.
    write_spmem_word(
        system,
        12,
        u32::try_from(elf_offset >> 8).unwrap_or(0) << 16,
    );
    Ok(elf_offset)
}

/// Find the ELF header's offset within a ROM image.
fn find_elf_offset(rom: &[u8]) -> Option<usize> {
    rom.windows(4).position(|w| w == b"\x7fELF")
}

/// Write a big-endian word into RSP DMEM.
fn write_spmem_word(system: &mut System, offset: usize, value: u32) {
    let bytes = value.to_be_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if let Some(dst) = system.bus.spmem.get_mut(offset + i) {
            *dst = *b;
        }
    }
}
