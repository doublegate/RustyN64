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

/// Byte offset of `command_base` within the overlay header
/// (`RSPQ_OH_CMDBASE`, `third_party/libdragon-rsp/include/rsp_queue.inc:113`).
/// It has no linker symbol of its own — the header base `_ovl_data_start` does —
/// so the field offset is named here rather than repeated as a bare `+ 14`.
const OVL_HEADER_CMDBASE: usize = 14;

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
    /// The idle `break` sits at IMEM `0x14` (`rsp_queue.inc:403`); the PC parks
    /// at the sequential address after it.
    const IDLE_BREAK: u32 = 0x14;

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

    // The idle `break` (IMEM 0x14) leaves the RSP HALTED and BROKE, with the PC
    // parked at the sequential address after it (0x18). Asserting `broke()`
    // rather than only `halted()` is what proves a real `break` — a `SET_HALT`
    // write halts without it — and pinning the exact PC ties the witness to the
    // documented idle target rather than "somewhere non-zero".
    assert!(
        sys.bus.rsp.sp.halted() && sys.bus.rsp.sp.broke(),
        "the microcode must reach its idle `break` HALTED+BROKE — halted={}, broke={}, \
         steps={steps}, PC={:#x}",
        sys.bus.rsp.sp.halted(),
        sys.bus.rsp.sp.broke(),
        sys.bus.rsp.sp.pc()
    );
    assert_eq!(
        sys.bus.rsp.sp.pc(),
        IDLE_BREAK + 4,
        "the PC must park just past the idle `break`"
    );
    assert_eq!(
        sys.bus.rsp.su_regs[GP], 0,
        "`li $gp, 0` must have run, overwriting the sentinel"
    );
}

/// **T-24-003 (foundation): the kernel takes the `wakeup` path, DMAs a command
/// queue from RDRAM, dispatches it, and returns to idle.**
///
/// Unlike T-24-002's immediate break (`SIG_MORE` clear), this sets `SIG_MORE`
/// (signal 7) so the `bnez` at the entry is taken into `wakeup`: the kernel
/// reads the RDRAM queue address from `RSPQ_RDRAM_PTR` (DMEM `0xe0`), `DMAIn`s
/// the queue into the in-DMEM ring `RSPQ_DMEM_BUFFER` (`0xe8`), and dispatches
/// each command in `RSPQ_Loop`. The queue here is `[0x0000_0000, 0xDEAD_BEEF]`:
/// the first word is the internal `WaitNewInput` command, so with `SIG_MORE`
/// now cleared the kernel returns straight to the idle `break` — but the second
/// word is a marker the DMA must have physically moved into the DMEM ring.
///
/// The witness is non-vacuous *and* distinguishes this from T-24-002: reaching
/// `BROKE` at the idle break proves the kernel ran, and the marker appearing at
/// `RSPQ_DMEM_BUFFER + 4` proves the `wakeup` DMA actually executed — the
/// immediate-break path never DMAs, so that word would stay zero.
///
/// This is the command-fetch/DMA/dispatch mechanism; dispatching *rdpq overlay*
/// commands (which then emit RDP commands through the DPC seam) additionally
/// needs the overlay table registered — see
/// [`the_microcode_emits_an_rdp_command_through_the_dpc_seam`].
#[test]
fn the_kernel_dmas_and_dispatches_a_command_queue() {
    use rustyn64_core::System;
    use rustyn64_core::rsp::sp;

    // DMEM offsets from the committed symbol map (`microcode/symbols.txt`):
    // `RSPQ_RDRAM_PTR` (`rsp_queue.inc:355`, read at `:410`) and the command ring
    // `RSPQ_DMEM_BUFFER` (`:362`, the DMAIn target at `:420`/`:434`).
    const RSPQ_RDRAM_PTR: usize = 0xe0;
    const RSPQ_DMEM_BUFFER: usize = 0xe8;
    // `SP_STATUS` write bits (`sp::write_status`): `CLR_HALT` (bit 0), and set
    // signal 7 = `SIG_MORE` (read bit 0x4000, checked at `rsp_queue.inc:398`).
    const CLR_HALT: u32 = 1 << 0;
    const SET_SIG_MORE: u32 = 1 << 24;
    /// Where the fixture queue lives in RDRAM, and the marker in its word 1.
    const QUEUE_ADDR: u32 = 0x2000;
    const MARKER: u32 = 0xDEAD_BEEF;
    /// A DMEM-ring sentinel distinct from `MARKER` and 0, so the post-run check
    /// proves the DMA *overwrote* it rather than reading a pre-existing value.
    const RING_SENTINEL: u32 = 0x1234_5678;
    /// The idle `break` parks the PC at 0x18 (see the boot-to-idle test).
    const IDLE_PC: u32 = 0x18;
    const MAX_STEPS: usize = 600;

    let mut sys = System::new(0);
    sys.bus.rsp.dmem[..IMEM_LMA].copy_from_slice(&UCODE[..IMEM_LMA]);
    let imem_len = UCODE.len() - IMEM_LMA;
    sys.bus.rsp.imem[..imem_len].copy_from_slice(&UCODE[IMEM_LMA..]);

    // Queue: [WaitNewInput (0x0), marker]. Point the kernel at it.
    let qa = QUEUE_ADDR as usize;
    sys.bus.rdram[qa..qa + 4].copy_from_slice(&0u32.to_be_bytes());
    sys.bus.rdram[qa + 4..qa + 8].copy_from_slice(&MARKER.to_be_bytes());
    sys.bus.rsp.dmem[RSPQ_RDRAM_PTR..RSPQ_RDRAM_PTR + 4].copy_from_slice(&QUEUE_ADDR.to_be_bytes());
    // Pre-seed the ring word the DMA must overwrite, so the marker landing there
    // is provably the DMA's doing, not a leftover zero or coincidence.
    sys.bus.rsp.dmem[RSPQ_DMEM_BUFFER + 4..RSPQ_DMEM_BUFFER + 8]
        .copy_from_slice(&RING_SENTINEL.to_be_bytes());

    // Un-halt AND set SIG_MORE so the entry takes `wakeup`, not the idle break.
    sys.bus.rsp.sp.set_pc(0);
    sys.bus
        .rsp
        .sp
        .write(sp::reg::STATUS, CLR_HALT | SET_SIG_MORE);

    let mut steps = 0;
    while !sys.bus.rsp.sp.halted() && steps < MAX_STEPS {
        sys.bus.rsp_tick();
        steps += 1;
    }
    assert!(
        sys.bus.rsp.sp.halted(),
        "the kernel timed out after {MAX_STEPS} steps (never halted, PC={:#x})",
        sys.bus.rsp.sp.pc()
    );

    // Completion: HALTED *and* BROKE — the kernel reached its idle `break`, not a
    // bare halt — parked at the idle PC.
    assert!(
        sys.bus.rsp.sp.halted() && sys.bus.rsp.sp.broke(),
        "the kernel must halt at its idle break — halted={}, broke={}, pc={:#x}",
        sys.bus.rsp.sp.halted(),
        sys.bus.rsp.sp.broke(),
        sys.bus.rsp.sp.pc()
    );
    assert_eq!(sys.bus.rsp.sp.pc(), IDLE_PC, "returned to the idle break");

    // The `wakeup` DMA must have physically overwritten the ring sentinel with
    // the queue's word-1 marker — the immediate-break path (T-24-002) never DMAs.
    let dmaed = u32::from_be_bytes(
        sys.bus.rsp.dmem[RSPQ_DMEM_BUFFER + 4..RSPQ_DMEM_BUFFER + 8]
            .try_into()
            .unwrap(),
    );
    assert_eq!(
        dmaed, MARKER,
        "the `wakeup` DMA must have moved the queue into RSPQ_DMEM_BUFFER \
         (found {dmaed:#010x}, sentinel was {RING_SENTINEL:#010x})"
    );
}

/// **T-24-003 — the rdpq microcode emits an RDP command list (Phase 2 criterion 2).**
///
/// The full arc: a real `rdpq` overlay command is fetched from an RDRAM queue,
/// dispatched to its (resident) handler, and the handler *emits an RDP command*
/// — the 8 command bytes are DMA'd into an RDRAM output buffer and `DP_END` is
/// advanced past them through the DPC seam. That is what "a real graphics
/// microcode boots and emits a plausible RDP command list" means for v0.3.0.
///
/// **Registering the overlay without a load.** Our blob is the *combined*
/// kernel+`rdpq` image (ADR 0008), so the `rdpq` handler code is already
/// resident in IMEM. libdragon's `rspq_overlay_register` (`rspq.c:800`) would
/// DMA an overlay in; here we only need the kernel to *believe* `rdpq` is the
/// currently-loaded overlay so `RSPQ_Loop` (`rsp_queue.inc:442`) skips the load
/// and jumps straight to the resident handler. Three DMEM writes do that, each
/// reproducing exactly what `rspq_overlay_register_internal` writes:
/// - `RSPQ_CURRENT_OVL` (`:472`, reloaded every dispatch) ← the overlay id.
/// - `RSPQ_OVERLAY_IDMAP[hi]` (`:476`) ← the id, for each high-nibble the
///   overlay spans. `RDPQ_OVL_ID = 0xC << 28` (`rdpq.h:159`) ⇒ id `0xC`.
/// - the overlay header's `command_base` (`_ovl_data_start + 14`, `:479`) ←
///   `id << 5` (`rspq.c:858`) = `0xC << 5` = `0x180`. `0xC0`'s command index is
///   `(0xC0.. >> 23) & 0x1FE = 0x180`, so `sub cmd_index, command_base` (`:493`)
///   is 0 — the first overlay command, `RDPQCmd_Passthrough8`.
///
/// **The emission seam.** `RDPQCmd_Passthrough8` (`rsp_rdpq.S:135`) forwards its
/// two command words verbatim to the RDP: `RDPQ_Write8` stages them, then
/// `RDPQ_Send` (`rsp_rdpq.inc:60`) DMAs them to `RDPQ_CURRENT` in RDRAM and
/// tail-calls `RSPQCmd_RdpAppendBuffer` (`rsp_queue.inc:819`), whose
/// `mtc0 a0, COP0_DP_END` is the RSP↔RDP hand-off. The RSP crate cannot name
/// `Rdp`, so that `MTC0` surfaces as `StepResult::dp_write` and the Bus forwards
/// it to `Rdp::dpc_write` — the same register file the CPU drives at
/// `0x0410_0000`. We witness both halves: the exact command bytes in RDRAM and
/// `DP_END` advanced to `buffer + 8`.
///
/// Non-vacuous: the output buffer is pre-seeded with a sentinel (so the bytes
/// must have been DMA'd, not read as leftovers) and the command words carry a
/// distinctive payload in their free low bits (`0xC0DE_AD00`, `0x0000_BEEF`), so
/// neither assertion can pass on zeros or a never-ran microcode.
#[test]
fn the_microcode_emits_an_rdp_command_through_the_dpc_seam() {
    use rustyn64_core::System;
    use rustyn64_core::rdp::DPC_ADDR_MASK;
    use rustyn64_core::rsp::sp;

    const CLR_HALT: u32 = 1 << 0;
    const SET_SIG_MORE: u32 = 1 << 24;
    const IDLE_PC: u32 = 0x18;
    const MAX_STEPS: usize = 1500;

    // `RDPQ_OVL_ID = 0xC << 28` (rdpq.h:159): the overlay id is 0xC, and
    // `command_base = id << 5` (rspq.c:858).
    const RDPQ_OVL_ID: u8 = 0xC;
    const COMMAND_BASE: u16 = 0x180; // = (RDPQ_OVL_ID as u16) << 5

    // The RDRAM command queue and the RDP dynamic output buffer (distinct, both
    // 8-aligned, well inside RDRAM). The buffer has room for the 8-byte command.
    const QUEUE_ADDR: u32 = 0x2000;
    const BUF_BASE: u32 = 0x3000;
    const BUF_ROOM: u32 = 0x100;
    const BUF_SENTINEL: u8 = 0xA5;

    // The rdpq command: id 0xC0 (`RDPQCmd_Passthrough8`) in the high byte, a
    // distinctive payload in the free low bits so the emitted bytes are unique.
    const CMD_W0: u32 = 0xC0DE_AD00;
    const CMD_W1: u32 = 0x0000_BEEF;

    let mut sys = System::new(0);
    sys.bus.rsp.dmem[..IMEM_LMA].copy_from_slice(&UCODE[..IMEM_LMA]);
    let imem_len = UCODE.len() - IMEM_LMA;
    sys.bus.rsp.imem[..imem_len].copy_from_slice(&UCODE[IMEM_LMA..]);

    // --- Register the resident rdpq overlay (three DMEM patches, see doc). ---
    let cur_ovl = sym("RSPQ_CURRENT_OVL");
    sys.bus.rsp.dmem[cur_ovl..cur_ovl + 2].copy_from_slice(&u16::from(RDPQ_OVL_ID).to_be_bytes());
    // The overlay spans the high nibbles it owns; 0xC0 only reads IDMAP[0xC],
    // but register all four (0xC..=0xF) as the real registration would.
    let idmap = sym("RSPQ_OVERLAY_IDMAP");
    for hi in RDPQ_OVL_ID..=0xF {
        sys.bus.rsp.dmem[idmap + usize::from(hi)] = RDPQ_OVL_ID;
    }
    let cmd_base_off = sym("_ovl_data_start") + OVL_HEADER_CMDBASE;
    sys.bus.rsp.dmem[cmd_base_off..cmd_base_off + 2].copy_from_slice(&COMMAND_BASE.to_be_bytes());

    // --- Seed the RDP dynamic-buffer state RDPQ_Send reads (DMEM). ---
    let cur = sym("RDPQ_CURRENT");
    sys.bus.rsp.dmem[cur..cur + 4].copy_from_slice(&BUF_BASE.to_be_bytes());
    let sentinel = sym("RDPQ_SENTINEL");
    sys.bus.rsp.dmem[sentinel..sentinel + 4].copy_from_slice(&(BUF_BASE + BUF_ROOM).to_be_bytes());
    let dyn_bufs = sym("RDPQ_DYNAMIC_BUFFERS");
    sys.bus.rsp.dmem[dyn_bufs..dyn_bufs + 4].copy_from_slice(&BUF_BASE.to_be_bytes());
    sys.bus.rsp.dmem[dyn_bufs + 4..dyn_bufs + 8]
        .copy_from_slice(&(BUF_BASE + BUF_ROOM).to_be_bytes());

    // --- The command queue: [passthrough(0xC0), WaitNewInput(0x0) terminator]. ---
    let qa = QUEUE_ADDR as usize;
    sys.bus.rdram[qa..qa + 4].copy_from_slice(&CMD_W0.to_be_bytes());
    sys.bus.rdram[qa + 4..qa + 8].copy_from_slice(&CMD_W1.to_be_bytes());
    sys.bus.rdram[qa + 8..qa + 12].copy_from_slice(&0u32.to_be_bytes());
    let rdram_ptr = sym("RSPQ_RDRAM_PTR");
    sys.bus.rsp.dmem[rdram_ptr..rdram_ptr + 4].copy_from_slice(&QUEUE_ADDR.to_be_bytes());

    // Pre-seed the RDP output buffer so the DMA'd command is provably new.
    let bb = BUF_BASE as usize;
    sys.bus.rdram[bb..bb + 8].copy_from_slice(&[BUF_SENTINEL; 8]);

    // Un-halt AND set SIG_MORE so the entry takes `wakeup`.
    sys.bus.rsp.sp.set_pc(0);
    sys.bus
        .rsp
        .sp
        .write(sp::reg::STATUS, CLR_HALT | SET_SIG_MORE);

    let mut steps = 0;
    while !sys.bus.rsp.sp.halted() && steps < MAX_STEPS {
        sys.bus.rsp_tick();
        steps += 1;
    }
    assert!(
        sys.bus.rsp.sp.halted(),
        "the kernel timed out after {MAX_STEPS} steps (PC={:#x})",
        sys.bus.rsp.sp.pc()
    );
    assert!(
        sys.bus.rsp.sp.halted() && sys.bus.rsp.sp.broke(),
        "the kernel must return to its idle break after emitting — halted={}, broke={}, pc={:#x}",
        sys.bus.rsp.sp.halted(),
        sys.bus.rsp.sp.broke(),
        sys.bus.rsp.sp.pc()
    );
    assert_eq!(sys.bus.rsp.sp.pc(), IDLE_PC, "returned to the idle break");

    // Witness 1: the 8 command bytes were DMA'd into the RDP output buffer.
    let mut expected = [0u8; 8];
    expected[..4].copy_from_slice(&CMD_W0.to_be_bytes());
    expected[4..].copy_from_slice(&CMD_W1.to_be_bytes());
    assert_eq!(
        &sys.bus.rdram[bb..bb + 8],
        &expected,
        "RDPQ_Send must DMA the passthrough command into the RDP buffer \
         (sentinel was {BUF_SENTINEL:#04x} throughout)"
    );

    // Witness 2: DP_END was advanced past the command through the DPC seam —
    // the RSP's `mtc0 DP_END` reached `Rdp::dpc_write` via the Bus.
    let dp_end = sys.bus.rdp.dpc_read(1);
    assert_eq!(
        dp_end,
        (BUF_BASE + 8) & DPC_ADDR_MASK,
        "RSPQCmd_RdpAppendBuffer must advance DP_END to buffer+8 \
         (got {dp_end:#010x}); a stale 0 means the COP0 write never reached the RDP"
    );
}

/// **T-24-004 — the microcode *generates* a documented RDP command (the golden
/// byte-compare, Phase 2 criterion 2).**
///
/// Where [`the_microcode_emits_an_rdp_command_through_the_dpc_seam`] passes an
/// RDP command through verbatim, this feeds `RDPQCmd_SetFillColor32` (`0xD6`) —
/// an rdpq command the handler *transforms* into an RDP command. With a 32-bit
/// target (`RDPQ_TARGET_BITDEPTH == 3`), `RDPQ_WriteSetFillColor`
/// (`rsp_rdpq.inc:704`) synthesises the opcode word `lui a0, 0xF700` and forwards
/// the colour, emitting the two words `[0xF700_0000, colour]`.
///
/// The golden vector is derived from the **documented** encoding, not from the
/// microcode (which would be circular): N64brew *Reality Display Processor/
/// Commands* §`0x37 - Set Fill Color` fixes `command[5:0] = 0x37` in bits 61:56
/// and `colour[31:0]` in bits 31:0. Bits 63:62 are documented don't-cares
/// (shown `—`); libdragon canonically drives them `11`, so the command byte is
/// `0xC0 | 0x37 = 0xF7` and word0 = `0xF700_0000`. Word1 is the colour verbatim.
/// A change to either constant is an intentional, reviewed behaviour change.
#[test]
fn the_microcode_generates_a_set_fill_color_command() {
    use rustyn64_core::System;
    use rustyn64_core::rdp::DPC_ADDR_MASK;
    use rustyn64_core::rsp::sp;

    const CLR_HALT: u32 = 1 << 0;
    const SET_SIG_MORE: u32 = 1 << 24;
    const IDLE_PC: u32 = 0x18;
    const MAX_STEPS: usize = 1500;

    const RDPQ_OVL_ID: u8 = 0xC;
    const COMMAND_BASE: u16 = 0x180; // = (RDPQ_OVL_ID as u16) << 5
    const TARGET_32BIT: u8 = 3; // RDPQ_TARGET_BITDEPTH == 3 -> RGBA32 target

    const QUEUE_ADDR: u32 = 0x2000;
    const BUF_BASE: u32 = 0x3000;
    const BUF_ROOM: u32 = 0x100;
    const BUF_SENTINEL: u8 = 0x5A;

    // rdpq command 0xD6 (`RDPQCmd_SetFillColor32`); a1 carries the RGBA32 colour.
    const CMD_W0: u32 = 0xD600_0000;
    const COLOR: u32 = 0xAABB_CCDD;
    // Golden RDP command bytes, from N64brew §0x37 (see doc): opcode 0x37 in
    // bits 61:56 (libdragon's 0xF7 with the don't-care top bits set), colour verbatim.
    const GOLDEN_W0: u32 = 0xF700_0000;
    const GOLDEN_W1: u32 = COLOR;

    let mut sys = System::new(0);
    sys.bus.rsp.dmem[..IMEM_LMA].copy_from_slice(&UCODE[..IMEM_LMA]);
    let imem_len = UCODE.len() - IMEM_LMA;
    sys.bus.rsp.imem[..imem_len].copy_from_slice(&UCODE[IMEM_LMA..]);

    // Register the resident rdpq overlay (see the emit test's doc for the model).
    let cur_ovl = sym("RSPQ_CURRENT_OVL");
    sys.bus.rsp.dmem[cur_ovl..cur_ovl + 2].copy_from_slice(&u16::from(RDPQ_OVL_ID).to_be_bytes());
    let idmap = sym("RSPQ_OVERLAY_IDMAP");
    for hi in RDPQ_OVL_ID..=0xF {
        sys.bus.rsp.dmem[idmap + usize::from(hi)] = RDPQ_OVL_ID;
    }
    let cmd_base_off = sym("_ovl_data_start") + OVL_HEADER_CMDBASE;
    sys.bus.rsp.dmem[cmd_base_off..cmd_base_off + 2].copy_from_slice(&COMMAND_BASE.to_be_bytes());
    // 32-bit target so the fill colour is forwarded, not down-converted to 16-bit.
    sys.bus.rsp.dmem[sym("RDPQ_TARGET_BITDEPTH")] = TARGET_32BIT;

    // Seed the RDP dynamic-buffer state RDPQ_Send reads.
    let cur = sym("RDPQ_CURRENT");
    sys.bus.rsp.dmem[cur..cur + 4].copy_from_slice(&BUF_BASE.to_be_bytes());
    let sentinel = sym("RDPQ_SENTINEL");
    sys.bus.rsp.dmem[sentinel..sentinel + 4].copy_from_slice(&(BUF_BASE + BUF_ROOM).to_be_bytes());
    let dyn_bufs = sym("RDPQ_DYNAMIC_BUFFERS");
    sys.bus.rsp.dmem[dyn_bufs..dyn_bufs + 4].copy_from_slice(&BUF_BASE.to_be_bytes());
    sys.bus.rsp.dmem[dyn_bufs + 4..dyn_bufs + 8]
        .copy_from_slice(&(BUF_BASE + BUF_ROOM).to_be_bytes());

    // Queue: [SetFillColor32(0xD6), colour, WaitNewInput(0x0) terminator].
    let qa = QUEUE_ADDR as usize;
    sys.bus.rdram[qa..qa + 4].copy_from_slice(&CMD_W0.to_be_bytes());
    sys.bus.rdram[qa + 4..qa + 8].copy_from_slice(&COLOR.to_be_bytes());
    sys.bus.rdram[qa + 8..qa + 12].copy_from_slice(&0u32.to_be_bytes());
    let rdram_ptr = sym("RSPQ_RDRAM_PTR");
    sys.bus.rsp.dmem[rdram_ptr..rdram_ptr + 4].copy_from_slice(&QUEUE_ADDR.to_be_bytes());

    let bb = BUF_BASE as usize;
    sys.bus.rdram[bb..bb + 8].copy_from_slice(&[BUF_SENTINEL; 8]);

    sys.bus.rsp.sp.set_pc(0);
    sys.bus
        .rsp
        .sp
        .write(sp::reg::STATUS, CLR_HALT | SET_SIG_MORE);

    let mut steps = 0;
    while !sys.bus.rsp.sp.halted() && steps < MAX_STEPS {
        sys.bus.rsp_tick();
        steps += 1;
    }
    assert!(
        sys.bus.rsp.sp.halted(),
        "timed out (PC={:#x})",
        sys.bus.rsp.sp.pc()
    );
    assert!(
        sys.bus.rsp.sp.halted() && sys.bus.rsp.sp.broke() && sys.bus.rsp.sp.pc() == IDLE_PC,
        "must return to the idle break after generating (halted={}, broke={}, pc={:#x})",
        sys.bus.rsp.sp.halted(),
        sys.bus.rsp.sp.broke(),
        sys.bus.rsp.sp.pc()
    );

    // Golden byte-compare: the microcode-generated SET_FILL_COLOR command.
    let mut golden = [0u8; 8];
    golden[..4].copy_from_slice(&GOLDEN_W0.to_be_bytes());
    golden[4..].copy_from_slice(&GOLDEN_W1.to_be_bytes());
    assert_eq!(
        &sys.bus.rdram[bb..bb + 8],
        &golden,
        "SetFillColor32 must generate the documented SET_FILL_COLOR (0x37) command \
         {golden:02x?} into the RDP buffer"
    );
    assert_eq!(
        sys.bus.rdp.dpc_read(1),
        (BUF_BASE + 8) & DPC_ADDR_MASK,
        "DP_END must advance past the generated command through the DPC seam"
    );
}
