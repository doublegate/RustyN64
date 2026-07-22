//! T-24-001 — the vendored `rdpq` microcode blob is intact and well-formed.
//!
//! This is the first half of Phase 2 criterion 2 (`docs/adr/0008`): the real
//! libdragon RSPQ+`rdpq` microcode is vendored (`third_party/libdragon-rsp/`)
//! and assembled to `microcode/rsp_rdpq.bin`, the DMEM (`0x0000..`) + IMEM
//! (`0x1000..`) image the RSP loads. Booting it and comparing the emitted RDP
//! command list is T-24-002…004.
//!
//! What this test pins is that the committed blob is the *actual linker output*,
//! not a stale or truncated copy, by tying it to the assembled symbol map
//! (`microcode/symbols.txt`) rather than to itself. Byte-for-byte integrity and
//! source→blob reproducibility are the CI `sha256sum -c` + `assemble.sh` gates,
//! which need the `mips64-elf` toolchain; this needs none.

/// The committed DMEM+IMEM image, embedded at compile time.
const UCODE: &[u8] = include_bytes!("../microcode/rsp_rdpq.bin");

/// `rsp.ld`: DMEM occupies `0x0000..0x1000`, IMEM begins at `0x1000`.
const IMEM_LMA: usize = 0x1000;

/// `_text_end` from `microcode/symbols.txt` (LMA, i.e. `0xa4001eb8 & 0xffff`).
/// The `objcopy -O binary` image runs from the lowest LMA (`0x0000`, `_data_start`)
/// to the highest (`_text_end`), so the blob length **is** that address. Reading
/// it from the map rather than from the blob is what makes this non-circular.
const TEXT_END_LMA: usize = 0x1eb8;

#[test]
fn the_blob_length_matches_the_linker_symbol_map() {
    assert_eq!(
        UCODE.len(),
        TEXT_END_LMA,
        "the committed blob must be exactly _text_end bytes long ({TEXT_END_LMA:#x}); \
         a different length means it is stale, truncated, or hand-edited — regenerate \
         it with third_party/libdragon-rsp/assemble.sh"
    );
    assert!(
        UCODE.len() > IMEM_LMA,
        "the blob must reach into the IMEM region past {IMEM_LMA:#x}"
    );
}

#[test]
fn the_entry_point_is_real_code_and_the_data_section_is_populated() {
    // `_start` is at IMEM `0x1000` (RSP `SP_PC` 0x000). Its first instruction is
    // real kernel code, so the entry word must be non-zero — a zero here would
    // mean the IMEM half is padding, i.e. the microcode never got linked in.
    let entry = u32::from_be_bytes([
        UCODE[IMEM_LMA],
        UCODE[IMEM_LMA + 1],
        UCODE[IMEM_LMA + 2],
        UCODE[IMEM_LMA + 3],
    ]);
    assert_ne!(
        entry, 0,
        "_start (IMEM 0x000) must be a real instruction, not padding"
    );

    // The `.data` section carries the RSPQ overlay table + `rdpq` header from
    // `_data_start` (0x000); `_data_end` is 0x448, so the DMEM half is not empty.
    let data_nonzero = UCODE[..IMEM_LMA].iter().any(|&b| b != 0);
    assert!(
        data_nonzero,
        "the DMEM data section must be populated (overlay table + header)"
    );
}
