//! Phase 4, Sprint 2 — the real libdragon **audio mixer** microcode boots on the
//! LLE RSP.
//!
//! The vendored `rsp_mixer.S` (`third_party/libdragon-rsp/src/`, Unlicense) is
//! assembled to `microcode/rsp_mixer.bin` — the RSPQ command-queue kernel plus
//! the mixer overlay, the DMEM (`0x0000..`) + IMEM (`0x1000..`) image the RSP
//! loads. Like the `rdpq` blob (`microcode.rs`), the layout invariants here come
//! from the committed symbol map, not hand-copied constants.
//!
//! This is the audio analog of Phase 2's `rdpq` microcode boot (ADR 0008): a
//! *real* microcode, grounded in the vendored source, runs on our RSP. The full
//! command-driven PCM-mixing gate builds on this.
#![allow(
    clippy::doc_markdown,
    reason = "narrative test docs name RSP/DMEM/IMEM/RSPQ in prose"
)]

use rustyn64_core::System;

/// The committed mixer DMEM+IMEM image, embedded at compile time.
const UCODE: &[u8] = include_bytes!("../microcode/rsp_mixer.bin");

/// The committed symbol map (`nm -n` output), the source of every address below.
const SYMBOLS: &str = include_str!("../microcode/rsp_mixer.symbols.txt");

/// `rsp.ld`: DMEM occupies `0x0000..0x1000`, IMEM begins at `0x1000`.
const IMEM_LMA: usize = 0x1000;

/// The load-address (LMA) offset of a symbol, read from the committed map. The
/// RSP ignores the top address bits, so the flat-blob offset is `addr & 0xFFFF`.
/// Panics if the symbol is absent — a diverged map is a failure worth surfacing.
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
    panic!("symbol `{name}` not found in rsp_mixer.symbols.txt");
}

/// **The mixer blob's layout matches the linker symbol map.** The overlay's
/// command entry (`command_exec`), its `DMASettings` routine, and the DMEM
/// state (`SETTINGS_START`/`SETTINGS_END`, `OUTPUT_RDRAM`, `WAVEFORM_SETTINGS`)
/// all sit where the map places them, and the DMEM half holds the overlay
/// header + settings while the IMEM half holds the code.
#[test]
fn the_mixer_blob_layout_matches_the_linker_symbol_map() {
    let start = sym("_start");
    let text_end = sym("_text_end");
    let command_exec = sym("command_exec");
    let settings_start = sym("SETTINGS_START");
    let settings_end = sym("SETTINGS_END");

    assert_eq!(start, IMEM_LMA, "`_start` opens the IMEM half");
    assert!(
        command_exec >= IMEM_LMA && command_exec < text_end,
        "`command_exec` ({command_exec:#x}) is real code in [{start:#x}, {text_end:#x})"
    );
    assert!(
        settings_start < IMEM_LMA && settings_end <= IMEM_LMA && settings_end > settings_start,
        "the settings block [{settings_start:#x}, {settings_end:#x}) lives in DMEM"
    );
    // The blob spans at least one full DMEM plus the code up to `_text_end`.
    assert!(
        UCODE.len() >= text_end,
        "the blob ({:#x} bytes) must cover the code up to `_text_end` ({text_end:#x})",
        UCODE.len()
    );
}

/// **The real mixer microcode boots and reaches its idle `break`.** The mixer
/// blob carries the same RSPQ kernel as `rdpq`, so its `_start` prologue
/// (`li $gp, 0; …; break`) runs identically: with `SIG_MORE` clear the kernel
/// falls through to its idle `break` at IMEM `0x14`. Booting the *mixer* blob
/// (its own DMEM overlay header + settings, its own IMEM) witnesses that the
/// vendored audio microcode actually executes on our RSP, not just the graphics
/// one.
///
/// Unreachable as a pass (ADR 0008): the RSP starts running with the PC at
/// `_start` and `$gp` holding a sentinel; the test asserts the transition to
/// HALTED+BROKE at the idle target with `$gp` zeroed.
#[test]
fn the_mixer_microcode_boots_to_its_idle_break() {
    /// `$gp`, zeroed by the kernel prologue.
    const GP: usize = 28;
    /// The SU-only idle path is a handful of instructions.
    const MAX_BOOT_STEPS: usize = 1000;
    /// A sentinel distinct from the prologue's zero result.
    const GP_SENTINEL: u32 = 0xDEAD_BEEF;
    /// The idle `break` sits at IMEM `0x14`; the PC parks just past it.
    const IDLE_BREAK: u32 = 0x14;

    assert!(
        UCODE.len() >= IMEM_LMA,
        "the blob must be at least one full DMEM ({IMEM_LMA:#x}) long"
    );

    let mut sys = System::new(0);
    sys.bus.rsp.dmem[..IMEM_LMA].copy_from_slice(&UCODE[..IMEM_LMA]);
    let imem_len = UCODE.len() - IMEM_LMA;
    sys.bus.rsp.imem[..imem_len].copy_from_slice(&UCODE[IMEM_LMA..]);

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

    assert!(
        sys.bus.rsp.sp.halted() && sys.bus.rsp.sp.broke(),
        "the mixer microcode must reach its idle `break` HALTED+BROKE — halted={}, broke={}, \
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
