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

/// Load a ROM whose payload is an **ELF**, honouring its program headers.
///
/// # Why a flat copy is not enough
///
/// [`load_direct`] models IPL3's behaviour for an ordinary ROM: copy a fixed
/// prefix into RDRAM at the entry point. That is wrong for a ROM whose payload
/// is a linked ELF, because the ELF's segments each carry their **own** load
/// address, and they are not laid out contiguously from the entry point.
///
/// n64-systemtest is such a ROM. Its segments target `0x8000_0000` onward while
/// its header entry is `0x800A_15E8`, so a flat copy puts every address in the
/// image at the wrong place — and the very first jump, to a perfectly valid
/// address, lands in zeros. The header entry point is *correct*; the mapping was
/// not, which is a distinction worth keeping in mind when a ROM appears to jump
/// somewhere absurd.
///
/// # `memsz` vs `filesz`
///
/// A segment's `memsz` may exceed its `filesz`; the difference is **BSS** and
/// must be zeroed rather than left as whatever the previous ROM wrote. Skipping
/// it gives a program uninitialised statics, which fails far from its cause.
///
/// # Errors
///
/// [`LoadError::NoElfHeader`] when the image carries no ELF magic, and
/// [`LoadError::TooSmall`] when a header runs past the end of the image.
pub fn load_elf(system: &mut System, rom: &[u8]) -> Result<usize, LoadError> {
    let base = find_elf_offset(rom).ok_or(LoadError::NoElfHeader)?;
    let hdr = rom.get(base..base + 0x34).ok_or(LoadError::TooSmall)?;

    // 32-bit big-endian ELF, which is what the MIPS toolchain emits here.
    let be32 =
        |b: &[u8], o: usize| -> u32 { u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]) };
    let be16 = |b: &[u8], o: usize| -> u16 { u16::from_be_bytes([b[o], b[o + 1]]) };

    let phoff = be32(hdr, 0x1C) as usize;
    let phentsize = be16(hdr, 0x2A) as usize;
    let phnum = be16(hdr, 0x2C) as usize;

    let mut loaded = 0usize;
    for i in 0..phnum {
        let o = base + phoff + i * phentsize;
        let ph = rom.get(o..o + 32).ok_or(LoadError::TooSmall)?;
        // PT_LOAD only; PT_NULL, PT_NOTE and friends carry nothing to place.
        if be32(ph, 0) != 1 {
            continue;
        }
        let off = be32(ph, 4) as usize;
        let vaddr = be32(ph, 8);
        let filesz = be32(ph, 16) as usize;
        let memsz = be32(ph, 20) as usize;

        // Segment offsets are relative to the ELF, not to the ROM.
        let src = rom
            .get(base + off..base + off + filesz)
            .ok_or(LoadError::TooSmall)?;
        for (k, byte) in src.iter().enumerate() {
            let Some(dst) = rdram_slot(system, vaddr, k) else {
                continue;
            };
            *dst = *byte;
        }
        // BSS: memsz beyond filesz is zeroed, not inherited.
        for k in filesz..memsz {
            if let Some(dst) = rdram_slot(system, vaddr, k) {
                *dst = 0;
            }
        }
        loaded += memsz;
    }
    Ok(loaded)
}

/// Resolve `vaddr + k` to a byte of RDRAM, or `None` if it falls outside.
fn rdram_slot(system: &mut System, vaddr: u32, k: usize) -> Option<&mut u8> {
    let addr = vaddr.wrapping_add(u32::try_from(k).ok()?);
    // KSEG0/KSEG1 are unmapped: strip the segment to get the physical address.
    let phys = (addr & 0x1FFF_FFFF) as usize;
    system.bus.rdram.get_mut(phys)
}
