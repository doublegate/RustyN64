//! T-24-001 — the vendored `rdpq` microcode blob is intact and well-formed.
//!
//! This is the first half of Phase 2 criterion 2 (`docs/adr/0008`): the real
//! libdragon RSPQ+`rdpq` microcode is vendored (`third_party/libdragon-rsp/`)
//! and assembled to `microcode/rsp_rdpq.bin`, the DMEM (`0x0000..`) + IMEM
//! (`0x1000..`) image the RSP loads. Booting it and comparing the emitted RDP
//! command list is T-24-002…004.
//!
//! Every invariant here is **derived from the committed symbol map**
//! (`microcode/symbols.txt`, parsed at compile time) rather than from a
//! hand-copied constant, so the blob is checked against the linker's own view of
//! the layout, not against itself. Byte-for-byte integrity and source→blob
//! reproducibility are the CI `sha256sum -c` + `assemble.sh` gates (which need
//! the `mips64-elf` toolchain); this needs none.

/// The committed DMEM+IMEM image, embedded at compile time.
const UCODE: &[u8] = include_bytes!("../microcode/rsp_rdpq.bin");

/// The committed symbol map (`nm -n` output), the source of every address below.
const SYMBOLS: &str = include_str!("../microcode/symbols.txt");

/// `rsp.ld`: DMEM occupies `0x0000..0x1000`, IMEM begins at `0x1000`.
const IMEM_LMA: usize = 0x1000;

/// The load-address (LMA) offset of a symbol, read from the committed map.
///
/// `nm -n` lines are `<hex-addr> <type> <name>`; `rsp.ld` places `.data` at
/// `0xA4000000` and `.text` at `0xA4001000`, and the RSP ignores the top bits,
/// so the LMA in the flat blob is `addr & 0xFFFF`. Panics if the symbol is
/// absent — a missing symbol means the map and this test have diverged, which is
/// itself a failure worth surfacing loudly.
fn sym(name: &str) -> usize {
    for line in SYMBOLS.lines() {
        let mut it = line.split_whitespace();
        let (Some(addr), Some(_ty), Some(n)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        if n == name {
            let addr = usize::from_str_radix(addr, 16).expect("hex address in symbols.txt");
            return addr & 0xFFFF;
        }
    }
    panic!("symbol `{name}` not found in microcode/symbols.txt");
}

#[test]
fn the_blob_layout_matches_the_linker_symbol_map() {
    let data_start = sym("_data_start");
    let data_end = sym("_data_end");
    let start = sym("_start");
    let text_end = sym("_text_end");

    // `objcopy -O binary` spans the lowest LMA (`_data_start` = 0) to the highest
    // (`_text_end`), so the blob length *is* `_text_end` — read from the map, not
    // from the blob, which is what makes this non-circular. A different length
    // means the blob is stale, truncated, or hand-edited: regenerate it with
    // third_party/libdragon-rsp/assemble.sh.
    assert_eq!(data_start, 0, "_data_start is the base of the image");
    assert_eq!(
        UCODE.len(),
        text_end,
        "blob length must equal _text_end from the symbol map"
    );

    // The kernel `.data` must fit under IMEM, and `_start` must open the IMEM
    // half exactly — the layout the boot code (T-24-002) will rely on.
    assert!(
        data_start < data_end && data_end <= IMEM_LMA,
        "the .data section [{data_start:#x}, {data_end:#x}) must sit below IMEM ({IMEM_LMA:#x})"
    );
    assert_eq!(
        start, IMEM_LMA,
        "_start must be the first IMEM byte (RSP SP_PC 0x000)"
    );
    assert!(start < text_end, "the .text section must be non-empty");
}

#[test]
fn the_entry_point_is_real_code_and_the_data_section_is_populated() {
    let start = sym("_start");
    let data_end = sym("_data_end");

    // `_start`'s first instruction is real kernel code, so the entry word must be
    // non-zero — a zero here would mean the IMEM half is padding, i.e. the
    // microcode never got linked in. (Byte-exact integrity is the CI sha256 gate;
    // this asserts the *structure* the boot relies on.)
    let entry = u32::from_be_bytes([
        UCODE[start],
        UCODE[start + 1],
        UCODE[start + 2],
        UCODE[start + 3],
    ]);
    assert_ne!(entry, 0, "_start must be a real instruction, not padding");

    // The `.data` section (overlay table + `rdpq` header) is populated up to
    // `_data_end`, read from the map rather than hard-coded.
    let data_nonzero = UCODE[..data_end].iter().any(|&b| b != 0);
    assert!(
        data_nonzero,
        "the .data section [0, {data_end:#x}) must be populated (overlay table + header)"
    );
}
