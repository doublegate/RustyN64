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
    /// The ELF header is self-inconsistent — a program-header entry smaller
    /// than the 32 bytes a 32-bit `PT_LOAD` occupies, or a `memsz` below its
    /// own `filesz`.
    ///
    /// Reported rather than worked around: both are impossible in a
    /// well-formed image, so seeing one means the header being parsed is not
    /// the one intended.
    MalformedElf,
    /// A `PT_LOAD` segment addresses memory outside RDRAM.
    ///
    /// **Deliberately fatal.** Silently skipping the out-of-range bytes is how
    /// this module's original defect presented — the ROM appeared to load and
    /// then executed zeros, with nothing anywhere reporting that a segment had
    /// gone missing. A harness that quietly drops part of the program is worse
    /// than one that refuses to start.
    SegmentOutsideRdram {
        /// The virtual address that could not be placed.
        vaddr: u32,
    },
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
/// **Sign-extended** to 64 bits. The header field is 32 bits and every N64 entry
/// point is in a kernel segment, so its top bit is set — and under 32-bit
/// addressing `0x0000_0000_8000_1000` is not a shorthand for
/// `0xFFFF_FFFF_8000_1000`, it is an address error. Zero-extending it makes the
/// very first fetch fault.
///
/// # Errors
///
/// [`LoadError::TooSmall`] if the header is truncated.
pub fn entry_point(rom: &[u8]) -> Result<u64, LoadError> {
    let b = rom.get(8..12).ok_or(LoadError::TooSmall)?;
    let raw = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    Ok(i64::from(raw.cast_signed()).cast_unsigned())
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

    // **COP0 `Status`.** IPL3 leaves it at `0x3400_0000` — `CU1 | CU0 | FR` —
    // which is the value the COP0 work already cross-checked against a real
    // boot capture. Without it the first COP1 instruction raises Coprocessor
    // Unusable, which is what happened at instruction ~26 (`ExcCode = 11`,
    // `EPC = 0x800A_1650`).
    //
    // Note this also clears `ERL` and `BEV`, both of which cold reset sets. That
    // matters twice over: `BEV = 1` sends every exception to PIF ROM, which is
    // unemulated here, so an unhandled fault would vanish into zeros rather than
    // being reported.
    system
        .cpu
        .pipeline
        .cop0
        .set_hardware(rustyn64_core::cpu::cop0::reg::STATUS, 0x3400_0000);

    // **Insert the cartridge.** The ELF loader above models what IPL3 leaves in
    // RDRAM, but on hardware the cart itself is still physically present and
    // readable through the PI domain-1 window at `0x1000_0000`. Loading only
    // RDRAM leaves `Cart::rom` empty, so every cart read returns zero -- which
    // is what made the entire `cart:` / `cart_memory:` group of n64-systemtest
    // fail with `a=0x0`.
    if let Ok(cart) = rustyn64_core::cart::Cart::load(rom) {
        system.bus.cart = cart;
    }

    // **COP0 `Config.K0` = 3.** IPL3 leaves the KSEG0 cache-coherency field at 3
    // (cached, non-coherent, write-back); a cold VR4300 does not define it, so
    // like `Status` this is IPL3's doing rather than a reset value, and belongs
    // here rather than in `Cop0::new`.
    //
    // n64-systemtest's StartupTest reads the whole register and expects
    // `0x7006_E463`; without this it sees `0x7006_E460` and fails on the low
    // three bits alone.
    {
        let cfg = system
            .cpu
            .pipeline
            .cop0
            .read(rustyn64_core::cpu::cop0::reg::CONFIG);
        system.cpu.pipeline.cop0.set_hardware(
            rustyn64_core::cpu::cop0::reg::CONFIG,
            (cfg & !0b111) | 0b011,
        );
    }

    // **The stack pointer**, in **KSEG0 RDRAM**, above the heap.
    //
    // Every compiled entry point relies on `$sp` immediately -- n64-systemtest's
    // is `ADDIU $29, $29, -0x50` then `SW $31, 0x4c($29)` -- so with `$sp` at 0
    // that store goes to `0xFFFF_FFFF_FFFF_FFFC`, a TLB-mapped KSEG3 address,
    // and faults six instructions in.
    //
    // # Why KSEG0 specifically
    //
    // This previously pointed at the top of SP DMEM (`0xA400_1FF0`), described
    // in a comment as what IPL3 leaves behind. That was **over-claimed**: the
    // only evidence at the time was that *some* valid stack stopped a crash,
    // which any address would have done.
    //
    // n64-systemtest then narrowed it. Its SP-DMA tests build their source data
    // in a **stack array** and call `MemoryMap::uncached_mut` on it, which
    // asserts `addr & 0xE000_0000 == 0x8000_0000`. A KSEG1 stack fails that
    // assert and panics the suite outright -- so the stack must be KSEG0.
    //
    // The exact address remains an inference: it sits above `MemoryMap::HEAP_END`
    // (`0x0030_0000`, which the suite prints at startup) so the stack and heap
    // cannot collide, and inside the 8 MiB we report in SPMEM word 0.
    system.cpu.regs.write(29, 0xFFFF_FFFF_8078_0000);

    // Byte offset 0x00 (DMEM word index 0): the RDRAM size IPL3 detected.
    let size = u32::try_from(system.bus.rdram.len()).unwrap_or(u32::MAX);
    write_spmem_word(system, 0x00, size);
    // Byte offset **0x0C** — DMEM word index 3, not word 12. The suite's
    // `SPMEM::read`/`write` take a byte address (they mask with `0xFFC` and
    // cast to `*u32`), and it reads this one as `SPMEM::read(12)`.
    //
    // The value is the ELF offset in the packed form the suite unpacks with
    // `(v >> 16) << 8`. Storing the offset directly would be read as a
    // different number entirely.
    write_spmem_word(
        system,
        0x0C,
        u32::try_from(elf_offset >> 8).unwrap_or(0) << 16,
    );
    Ok(elf_offset)
}

/// Find the ELF header's offset within a ROM image.
///
/// The search starts at [`ROM_PAYLOAD_OFFSET`], **not** at zero. The first
/// 4 KiB is the cartridge header plus IPL3 — compiled code and a title string,
/// neither of which is an ELF — so scanning it can only produce a false
/// positive, never a true one. Four bytes is a short enough pattern that an
/// incidental match there is a real possibility, and a false hit would make
/// every subsequent header field garbage.
fn find_elf_offset(rom: &[u8]) -> Option<usize> {
    let payload = rom.get(ROM_PAYLOAD_OFFSET..)?;
    payload
        .windows(4)
        .position(|w| w == b"\x7fELF")
        .map(|p| p + ROM_PAYLOAD_OFFSET)
}

/// Write a big-endian word into RSP DMEM.
fn write_spmem_word(system: &mut System, offset: u32, value: u32) {
    for (i, b) in value.to_be_bytes().into_iter().enumerate() {
        let i = u32::try_from(i).unwrap_or(0);
        system.bus.rsp.mem_write(offset + i, b);
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
/// [`LoadError::NoElfHeader`] when the image carries no ELF magic;
/// [`LoadError::TooSmall`] when a header runs past the end of the image;
/// [`LoadError::MalformedElf`] when the program-header geometry is
/// self-inconsistent; and [`LoadError::SegmentOutsideRdram`] when a `PT_LOAD`
/// addresses memory RDRAM does not have.
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

    // Validate the program-header geometry BEFORE walking it. Each entry is
    // read as a fixed 32 bytes, so a `phentsize` below that would make
    // consecutive entries overlap and every field after the first garbage —
    // and `phentsize == 0` would re-read entry zero `phnum` times. Neither is
    // possible in a well-formed 32-bit ELF, so it is an error rather than
    // something to clamp.
    if phentsize < 32 {
        return Err(LoadError::MalformedElf);
    }

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
        // `memsz < filesz` means the segment claims less memory than it has
        // file bytes. The BSS loop below would silently do nothing (a reversed
        // Rust range is empty), so without this the image loads "successfully"
        // while being nonsense.
        if memsz < filesz {
            return Err(LoadError::MalformedElf);
        }

        // Segment offsets are relative to the ELF, not to the ROM.
        let src = rom
            .get(base + off..base + off + filesz)
            .ok_or(LoadError::TooSmall)?;
        for (k, byte) in src.iter().enumerate() {
            // Out-of-RDRAM is FATAL, not skipped -- see
            // `LoadError::SegmentOutsideRdram`. Dropping the byte here is what
            // made the original "jumps into zeros" defect so hard to find.
            let dst =
                rdram_slot(system, vaddr, k).ok_or(LoadError::SegmentOutsideRdram { vaddr })?;
            *dst = *byte;
        }
        // BSS: memsz beyond filesz is zeroed, not inherited.
        for k in filesz..memsz {
            let dst =
                rdram_slot(system, vaddr, k).ok_or(LoadError::SegmentOutsideRdram { vaddr })?;
            *dst = 0;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The synthetic segment's byte at index `i`.
    ///
    /// Never zero, so a byte that was never written is distinguishable from
    /// one the loader deliberately placed — the same reason the BSS test
    /// pre-fills RDRAM.
    fn seg_byte(i: usize) -> u8 {
        u8::try_from(i % 251).expect("modulo 251 fits a u8") | 1
    }

    /// Build a minimal but well-formed big-endian 32-bit MIPS ELF wrapped in an
    /// N64 ROM image, with one `PT_LOAD` segment.
    ///
    /// Synthesised rather than taken from a fixture so the expected placement is
    /// stated by the test itself. A fixture would only prove the loader agrees
    /// with whatever it happened to do when the fixture was captured.
    fn rom_with_one_pt_load(vaddr: u32, filesz: usize, memsz: usize) -> Vec<u8> {
        const EHDR: usize = 0x34;
        const PHDR: usize = 32;
        let mut rom = vec![0u8; ROM_PAYLOAD_OFFSET];
        // A plausible cartridge header: entry point at 0x08.
        rom[8..12].copy_from_slice(&vaddr.to_be_bytes());

        let mut elf = vec![0u8; EHDR + PHDR];
        elf[0..4].copy_from_slice(b"\x7fELF");
        elf[0x1C..0x20].copy_from_slice(&u32::try_from(EHDR).unwrap().to_be_bytes()); // e_phoff
        elf[0x2A..0x2C].copy_from_slice(&u16::try_from(PHDR).unwrap().to_be_bytes()); // e_phentsize
        elf[0x2C..0x2E].copy_from_slice(&1u16.to_be_bytes()); // e_phnum

        let p = EHDR;
        let seg_off = EHDR + PHDR;
        elf[p..p + 4].copy_from_slice(&1u32.to_be_bytes()); // PT_LOAD
        elf[p + 4..p + 8].copy_from_slice(&u32::try_from(seg_off).unwrap().to_be_bytes());
        elf[p + 8..p + 12].copy_from_slice(&vaddr.to_be_bytes());
        elf[p + 16..p + 20].copy_from_slice(&u32::try_from(filesz).unwrap().to_be_bytes());
        elf[p + 20..p + 24].copy_from_slice(&u32::try_from(memsz).unwrap().to_be_bytes());

        // Segment contents: a recognisable ascending pattern.
        elf.extend((0..filesz).map(seg_byte));
        rom.extend(elf);
        rom
    }

    /// `PT_LOAD` bytes land at the segment's **own** `vaddr`, and the `memsz`
    /// tail beyond `filesz` is zeroed rather than inherited.
    ///
    /// The destination is pre-filled with a non-zero pattern so "BSS was
    /// zeroed" is distinguishable from "BSS was never written" — with a fresh
    /// zeroed RDRAM the two are identical and the test proves nothing.
    #[test]
    fn a_pt_load_segment_lands_at_its_vaddr_and_its_bss_is_zeroed() {
        const VADDR: u32 = 0x8000_2000;
        const FILESZ: usize = 64;
        const MEMSZ: usize = 96;
        let rom = rom_with_one_pt_load(VADDR, FILESZ, MEMSZ);

        let mut sys = crate::system_for_tests(0);
        let phys = (VADDR & 0x1FFF_FFFF) as usize;
        // Fills PAST `memsz` too, so the final overrun assertion has a
        // non-zero guard byte to check. Filling only the segment would leave
        // that byte at RDRAM's own zero and the assertion would pass whatever
        // the loader did.
        for b in &mut sys.bus.rdram[phys..phys + MEMSZ + 8] {
            *b = 0xAA;
        }

        let loaded = load_elf(&mut sys, &rom).expect("well-formed ELF");
        assert_eq!(loaded, MEMSZ, "reports memsz, not filesz");

        for i in 0..FILESZ {
            assert_eq!(
                sys.bus.rdram[phys + i],
                seg_byte(i),
                "file byte {i} misplaced"
            );
        }
        for i in FILESZ..MEMSZ {
            assert_eq!(sys.bus.rdram[phys + i], 0, "BSS byte {i} not zeroed");
        }
        assert_eq!(
            sys.bus.rdram[phys + MEMSZ],
            0xAA,
            "the loader wrote past memsz"
        );
    }

    /// The DMEM handoff words IPL3 leaves behind, at the byte offsets the suite
    /// reads them from — `0x00` for the RDRAM size and `0x0C` for the packed
    /// ELF offset.
    ///
    /// The packing matters: the suite unpacks with `(v >> 16) << 8`, so storing
    /// the offset directly would be read as an entirely different number, and
    /// nothing downstream would report a problem.
    #[test]
    fn the_handoff_words_are_written_where_the_suite_reads_them() {
        const VADDR: u32 = 0x8000_2000;
        let rom = rom_with_one_pt_load(VADDR, 16, 16);
        let mut sys = crate::system_for_tests(0);

        let elf_offset = seed_ipl3_handoff(&mut sys, &rom).expect("has an ELF");
        assert_eq!(elf_offset, ROM_PAYLOAD_OFFSET, "the ELF starts the payload");

        let word = |o: u32| {
            u32::from_be_bytes([
                sys.bus.rsp.mem_read(o),
                sys.bus.rsp.mem_read(o + 1),
                sys.bus.rsp.mem_read(o + 2),
                sys.bus.rsp.mem_read(o + 3),
            ])
        };
        assert_eq!(
            word(0x00) as usize,
            sys.bus.rdram.len(),
            "0x00 = detected RDRAM size"
        );
        // Round-trip through the suite's own unpacking expression.
        assert_eq!(
            ((word(0x0C) >> 16) << 8) as usize,
            elf_offset,
            "0x0C must survive `(v >> 16) << 8`"
        );
    }

    /// The ELF search skips the cartridge header and IPL3. A stray `\x7fELF` in
    /// that region must not be mistaken for the payload's header.
    #[test]
    fn the_elf_search_ignores_a_false_magic_in_the_header_area() {
        let mut rom = rom_with_one_pt_load(0x8000_2000, 16, 16);
        rom[0x40..0x44].copy_from_slice(b"\x7fELF");
        assert_eq!(
            find_elf_offset(&rom),
            Some(ROM_PAYLOAD_OFFSET),
            "must find the payload's ELF, not the decoy"
        );
    }

    /// A `PT_LOAD` outside RDRAM is an error, not a silent skip. Quietly
    /// dropping it is how the original "executes zeros" defect hid.
    #[test]
    fn a_segment_outside_rdram_is_reported_rather_than_dropped() {
        // Far past 8 MiB of RDRAM.
        const VADDR: u32 = 0x8400_0000;
        let rom = rom_with_one_pt_load(VADDR, 16, 16);
        let mut sys = crate::system_for_tests(0);
        assert_eq!(
            load_elf(&mut sys, &rom),
            Err(LoadError::SegmentOutsideRdram { vaddr: VADDR })
        );
    }

    /// Self-inconsistent program-header geometry is rejected up front rather
    /// than producing overlapping reads.
    #[test]
    fn malformed_program_header_geometry_is_rejected() {
        let mut rom = rom_with_one_pt_load(0x8000_2000, 16, 16);
        let e = ROM_PAYLOAD_OFFSET;
        // e_phentsize = 8, below the 32 bytes each entry is read as.
        rom[e + 0x2A..e + 0x2C].copy_from_slice(&8u16.to_be_bytes());
        let mut sys = crate::system_for_tests(0);
        assert_eq!(load_elf(&mut sys, &rom), Err(LoadError::MalformedElf));

        // memsz < filesz: the BSS range would be empty and the image would
        // "load" fine.
        let mut rom = rom_with_one_pt_load(0x8000_2000, 64, 64);
        let p = e + 0x34;
        rom[p + 20..p + 24].copy_from_slice(&16u32.to_be_bytes());
        let mut sys = crate::system_for_tests(0);
        assert_eq!(load_elf(&mut sys, &rom), Err(LoadError::MalformedElf));
    }
}
