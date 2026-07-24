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
// Loop indices (< 32) and small constants provably fit the target widths.
#![allow(clippy::cast_possible_truncation)]

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

/// One output sample-pair per `MAX_SAMPLES_PER_LOOP` tick.
const NSAMPLES: usize = 32;

/// Drive the mixer overlay's `command_exec` (command 0) once with a one-channel
/// settings block feeding a known 16-bit mono ramp at full volume, and return
/// the mixed 16-bit stereo output the ucode DMAs back to RDRAM.
///
/// This exercises the mixer's real resampling / volume-filter / mixing DSP on
/// our RSP: the kernel DMAs the settings, `UpdateAndFetch` DMAs the waveform and
/// point-resamples it, `SetupMixer`+the mix loop apply the (global × channel)
/// volume through the vector unit, and the result is DMA'd to the output buffer.
fn drive_mixer() -> Vec<(i16, i16)> {
    use rustyn64_core::rsp::sp;

    // Overlay id 0xC (as rdpq uses): command_base = id<<5 = 0x180, and
    // command 0's byte is id<<4 = 0xC0.
    const OVL_ID: u8 = 0xC;
    const COMMAND_BASE: u16 = 0x180;
    const CMD_BYTE: u32 = 0xC0;
    const OVL_HEADER_CMDBASE: usize = 14;

    const CLR_HALT: u32 = 1 << 0;
    const SET_SIG_MORE: u32 = 1 << 24;
    const MAX_STEPS: usize = 200_000;

    // RDRAM layout (all 8-aligned, well inside RDRAM).
    const QUEUE: u32 = 0x2000;
    const OUTPUT: u32 = 0x4000;
    const SETTINGS: u32 = 0x5000;
    const BANK: u32 = 0x6000;
    const OUT_SENTINEL: u8 = 0x5A;

    let mut sys = System::new(0);
    sys.bus.rsp.dmem[..IMEM_LMA].copy_from_slice(&UCODE[..IMEM_LMA]);
    let imem_len = UCODE.len() - IMEM_LMA;
    sys.bus.rsp.imem[..imem_len].copy_from_slice(&UCODE[IMEM_LMA..]);

    let wr32 = |ram: &mut [u8], addr: u32, v: u32| {
        let a = addr as usize;
        ram[a..a + 4].copy_from_slice(&v.to_be_bytes());
    };
    let wr16 = |ram: &mut [u8], addr: u32, v: u16| {
        let a = addr as usize;
        ram[a..a + 2].copy_from_slice(&v.to_be_bytes());
    };

    // --- Register the resident mixer overlay (three DMEM patches, per rdpq). ---
    let cur_ovl = sym("RSPQ_CURRENT_OVL");
    sys.bus.rsp.dmem[cur_ovl..cur_ovl + 2].copy_from_slice(&u16::from(OVL_ID).to_be_bytes());
    let idmap = sym("RSPQ_OVERLAY_IDMAP");
    for hi in OVL_ID..=0xF {
        sys.bus.rsp.dmem[idmap + usize::from(hi)] = OVL_ID;
    }
    let cmd_base_off = sym("_ovl_data_start") + OVL_HEADER_CMDBASE;
    sys.bus.rsp.dmem[cmd_base_off..cmd_base_off + 2].copy_from_slice(&COMMAND_BASE.to_be_bytes());

    // --- The sample bank: 32 16-bit mono samples, a rising ramp. ---
    for j in 0..NSAMPLES {
        wr16(
            &mut sys.bus.rdram,
            BANK + (j as u32) * 2,
            ((j + 1) * 400) as u16,
        );
    }

    // --- The 896-byte settings block: channel 0 only, full volume. ---
    // Layout: L-vols(0x00..0x40) | R-vols(0x40..0x80) | channels(0x80..0x380).
    wr16(&mut sys.bus.rdram, SETTINGS, 0x7FFF); // CHANNEL_VOLUMES_L[0]
    wr16(&mut sys.bus.rdram, SETTINGS + 0x40, 0x7FFF); // CHANNEL_VOLUMES_R[0]
    let ch0 = SETTINGS + 0x80; // WAVEFORM_SETTINGS[0]
    wr32(&mut sys.bus.rdram, ch0, 0); // pos
    wr32(&mut sys.bus.rdram, ch0 + 4, 2 << 12); // step: 2 bytes/sample, 12 frac bits
    wr32(&mut sys.bus.rdram, ch0 + 8, (NSAMPLES as u32 * 2) << 12); // len (bytes, 12 frac)
    wr32(&mut sys.bus.rdram, ch0 + 12, 0); // loop_len
    wr32(&mut sys.bus.rdram, ch0 + 16, BANK); // ptr
    wr32(&mut sys.bus.rdram, ch0 + 20, 0x4); // flags: CH_FLAGS_16BIT

    // --- The command queue: command_exec (16 bytes) + terminator. ---
    wr32(&mut sys.bus.rdram, QUEUE, (CMD_BYTE << 24) | 0x7FFF); // opcode | global vol
    wr32(&mut sys.bus.rdram, QUEUE + 4, ((NSAMPLES as u32) << 16) | 1); // samples | channels
    wr32(&mut sys.bus.rdram, QUEUE + 8, OUTPUT); // output RDRAM
    wr32(&mut sys.bus.rdram, QUEUE + 12, SETTINGS); // settings RDRAM
    wr32(&mut sys.bus.rdram, QUEUE + 16, 0); // WaitNewInput terminator
    let rdram_ptr = sym("RSPQ_RDRAM_PTR");
    sys.bus.rsp.dmem[rdram_ptr..rdram_ptr + 4].copy_from_slice(&QUEUE.to_be_bytes());

    // Pre-seed the output buffer so mixed samples are provably new.
    let ob = OUTPUT as usize;
    sys.bus.rdram[ob..ob + NSAMPLES * 4].fill(OUT_SENTINEL);

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
        sys.bus.rsp.sp.halted() && sys.bus.rsp.sp.broke(),
        "the mixer must run to its idle break — halted={}, broke={}, steps={steps}, pc={:#x}",
        sys.bus.rsp.sp.halted(),
        sys.bus.rsp.sp.broke(),
        sys.bus.rsp.sp.pc()
    );

    let mut out = Vec::with_capacity(NSAMPLES);
    for j in 0..NSAMPLES {
        let a = ob + j * 4;
        let l = i16::from_be_bytes([sys.bus.rdram[a], sys.bus.rdram[a + 1]]);
        let r = i16::from_be_bytes([sys.bus.rdram[a + 2], sys.bus.rdram[a + 3]]);
        out.push((l, r));
    }
    out
}

/// **The real mixer microcode mixes a known waveform into a verified PCM buffer
/// — the full command-driven audio gate.**
///
/// The output is pinned as a golden: the input ramp (`(j+1)·400`) scaled by the
/// mixer's `VOLUME_FILTER` envelope, which ramps the actual volume up from 0
/// (its persisted `XVOL` power-on state) toward the programmed full volume — so
/// the first samples are silent and the level then rises, tracking the input.
/// Each pair is **stereo-symmetric** (a mono channel at equal L/R volume). The
/// golden is not hand-derived from the filter arithmetic (that would reimplement
/// the ucode); its correctness rests on it being the *real* libdragon mixer's
/// output for a *known* input, recognisably the ramp under a rising envelope,
/// and byte-identical across runs.
#[test]
fn the_mixer_mixes_a_waveform_into_pcm() {
    /// The mixed left-channel output (right is identical); captured from the real
    /// mixer microcode. A change here is an intentional, reviewed behaviour change.
    const GOLDEN_L: [i16; NSAMPLES] = [
        0, 0, 0, 0, 0, 0, 0, 0, 222, 246, 271, 296, 320, 345, 370, 394, 786, 832, 878, 925, 971,
        1017, 1063, 1109, 1629, 1694, 1759, 1825, 1890, 1955, 2020, 2085,
    ];

    let out = drive_mixer();

    // Non-vacuity: not the sentinel, actually produced sound, stereo-symmetric,
    // and it tracks the rising input ramp (tail louder than the first voiced
    // sample).
    assert!(
        out.iter().any(|&(l, _)| l != 0),
        "the mixed output is all zero — the mixer produced nothing: {out:?}"
    );
    for (j, &(l, r)) in out.iter().enumerate() {
        assert_eq!(
            l, r,
            "sample {j} is not stereo-symmetric for a mono channel: {out:?}"
        );
    }
    assert!(
        out[NSAMPLES - 1].0 > out[8].0 && out[8].0 > 0,
        "the output does not track the rising input ramp: {out:?}"
    );

    // The exact mix, pinned.
    let got_l: Vec<i16> = out.iter().map(|&(l, _)| l).collect();
    assert_eq!(
        got_l, GOLDEN_L,
        "the mixed PCM differs from the committed golden"
    );
}

/// Determinism (ADR 0004): the real mixer microcode produces a byte-identical
/// PCM stream across two runs from the same input.
#[test]
fn the_mixer_output_is_deterministic() {
    assert_eq!(
        drive_mixer(),
        drive_mixer(),
        "same input ⇒ bit-identical mix"
    );
}
