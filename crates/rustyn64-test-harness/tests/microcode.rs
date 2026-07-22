//! T-24-001 — the vendored `rdpq` microcode blob is intact and well-formed.
//!
//! This is the first half of Phase 2 criterion 2 (`docs/adr/0008`): the real
//! libdragon RSPQ+`rdpq` microcode is vendored (`third_party/libdragon-rsp/`)
//! and assembled to `microcode/rsp_rdpq.bin`, the DMEM (`0x0000..`) + IMEM
//! (`0x1000..`) image the RSP loads. Booting it and comparing the emitted RDP
//! command list is T-24-002…004.
//!
//! The layout invariants here are **derived from the committed symbol map**
//! (`microcode/symbols.txt`, parsed at compile time) rather than from
//! hand-copied constants, so the blob is checked against the linker's own view
//! of the layout, not against itself; the only fixed constants are the IMEM
//! window (`rsp.ld`) and the `_start` prologue opcode (constructed from the MIPS
//! encoding, not copied from the blob). Byte-for-byte integrity and source→blob
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
    // `.text` must be non-empty AND fit inside the 4 KiB IMEM window `rsp.ld`
    // allocates (`0x1000..0x2000`) — a blob whose text overflowed IMEM could not
    // be loaded, and `start < text_end` alone would not catch it.
    assert!(start < text_end, "the .text section must be non-empty");
    assert!(
        text_end <= IMEM_LMA + 0x1000,
        "the .text section must fit inside IMEM (ends by {:#x}, got {text_end:#x})",
        IMEM_LMA + 0x1000
    );
}

#[test]
fn the_entry_point_is_real_code_and_the_data_section_is_populated() {
    let start = sym("_start");
    let data_start = sym("_data_start");
    let data_end = sym("_data_end");

    // `_start` opens with `li $gp, 0` — the RSPQ kernel prologue (`rsp_queue.inc`;
    // confirmed with `mips64-elf-objdump`). `li $gp, 0` is `addiu $gp, $zero, 0`.
    // Build the expected word from the MIPS encoding rather than copying it out
    // of the blob, so this is a real check (a corrupted or unrelated entry word
    // fails), not a tautology. (Full byte-exact integrity is the CI sha256 gate;
    // this asserts the one instruction the boot must open with.)
    let entry = u32::from_be_bytes([
        UCODE[start],
        UCODE[start + 1],
        UCODE[start + 2],
        UCODE[start + 3],
    ]);
    // `addiu $gp, $zero, 0`: MIPS I-type opcode 0x09, rt = $gp (28), rs/imm 0.
    let li_gp_0 = (0x09_u32 << 26) | (28_u32 << 16);
    assert_eq!(
        entry, li_gp_0,
        "_start must open with `li $gp, 0` ({li_gp_0:#010x}); got {entry:#010x}"
    );

    // The `.data` section (overlay table + `rdpq` header) spans
    // `[_data_start, _data_end)` — both read from the map — and must be
    // populated, not padding.
    let data_nonzero = UCODE[data_start..data_end].iter().any(|&b| b != 0);
    assert!(
        data_nonzero,
        "the .data section [{data_start:#x}, {data_end:#x}) must be populated (overlay table + header)"
    );
}

/// **T-24-002: the real microcode boots and reaches its idle `break`.**
///
/// The RSPQ kernel entry (`rsp_queue.inc:391`) is `li $gp, 0; mfc0 t0,
/// SP_STATUS; andi t0, SIG_MORE; bnez wakeup; …; break`. With `SIG_MORE` clear
/// at boot (which `rspq_start` sets, `rspq.c:548`), the branch is not taken and
/// the kernel falls through to `break` — its documented idle state
/// ("No new commands yet, go to sleep"). That path runs entirely in the SU
/// before any DMA, so no command queue or Bus state is needed to witness it.
///
/// The baseline is *unreachable as a pass* (ADR 0008): the RSP starts **running**
/// (`HALTED`/`BROKE` clear) with the PC at `_start` (0). The test then asserts
/// the transition — it halts via `BROKE`, the PC has advanced off `_start`, and
/// `$gp` was zeroed by the prologue. A microcode that never executed stays at
/// PC 0, not halted, and fails.
#[test]
fn the_microcode_boots_to_its_idle_break() {
    use rustyn64_core::System;

    /// `$gp` — the register the prologue zeroes (`rsp_dmem_buf_ptr`).
    const GP: usize = 28;
    /// A generous bound: the SU-only path to the idle `break` is a handful of
    /// instructions, so anything near this many steps means it never got there.
    const MAX_BOOT_STEPS: usize = 1000;
    /// A sentinel `$gp` distinct from the prologue's result, so the zero-check
    /// below is only satisfied if `li $gp, 0` actually executed (not vacuously
    /// true because the register file starts zeroed).
    const GP_SENTINEL: u32 = 0xDEAD_BEEF;

    assert!(
        UCODE.len() >= IMEM_LMA,
        "the blob must be at least one full DMEM ({IMEM_LMA:#x}) long"
    );

    let mut sys = System::new(0);
    sys.bus.rsp.dmem[..IMEM_LMA].copy_from_slice(&UCODE[..IMEM_LMA]);
    let imem_len = UCODE.len() - IMEM_LMA;
    sys.bus.rsp.imem[..imem_len].copy_from_slice(&UCODE[IMEM_LMA..]);

    // Unreachable-as-pass baseline: running (not halted, not broke), PC at
    // `_start`, `$gp` holding a sentinel. The idle path is taken only when
    // `SP_STATUS.SIG_MORE` is clear, which `System::new` leaves it — asserted
    // here so the precondition is explicit rather than an unstated default.
    sys.bus.rsp.su_regs[GP] = GP_SENTINEL;
    sys.bus.rsp.sp.set_pc(0);
    sys.bus.rsp.sp.set_halted(false);
    assert!(!sys.bus.rsp.sp.halted(), "baseline: the RSP starts running");
    assert!(!sys.bus.rsp.sp.broke(), "baseline: BROKE is clear");
    assert_eq!(
        sys.bus.rsp.sp.status() & 0x4000,
        0,
        "baseline requires SIG_MORE (0x4000) clear so the kernel takes the idle path"
    );

    let mut steps = 0;
    while !sys.bus.rsp.sp.halted() && steps < MAX_BOOT_STEPS {
        sys.bus.rsp.tick();
        steps += 1;
    }

    // The transition. `broke()` (not just `halted()`) is what proves the kernel
    // reached a real `break`: a `SET_HALT` write would halt without it.
    assert!(
        sys.bus.rsp.sp.broke(),
        "the microcode must reach its idle `break` (BROKE) — halted={}, steps={steps}, PC={:#x}",
        sys.bus.rsp.sp.halted(),
        sys.bus.rsp.sp.pc()
    );
    assert_ne!(sys.bus.rsp.sp.pc(), 0, "the PC must advance off _start");
    assert_eq!(
        sys.bus.rsp.su_regs[GP], 0,
        "`li $gp, 0` must have run, overwriting the sentinel"
    );
}
