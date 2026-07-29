//! Retail cartridge boot — the console boot a real N64 performs, moved here from
//! the test harness so the frontend can boot a game too (both consume it).
//!
//! Two paths, both modelling real hardware (not a test shortcut):
//!
//! - [`hle_boot`] — the copyright-clean default. It seeds the state IPL3 expects,
//!   copies the cart's real IPL3 into RSP DMEM, and jumps to it, skipping only the
//!   PIF ROM (IPL1/IPL2) and the CIC challenge — which the seed injection stands
//!   in for. Deterministic; the seeds are cited constants (`docs/accuracy-ledger.md`
//!   C-32).
//! - [`real_pif_boot`] — the faithful path (off by default, local-only, never
//!   CI-gated: it needs the copyrighted PIF ROM). It runs the console's real
//!   IPL1/IPL2 from the PIF ROM at the reset vector and CIC-verifies the IPL2
//!   checksum (ledger C-33).
//!
//! The ELF direct-load (`seed_ipl3_handoff`, for the n64-systemtest ELF payload)
//! is a genuine *test* facility and stays in `rustyn64-test-harness` — the core
//! must not acquire a test load-path dependency.

use crate::System;

/// Why a retail boot could not start.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootError {
    /// The image is shorter than the `0x1000` boot header + IPL3, so there is
    /// nothing to boot. (Also covers a PIF ROM shorter than its window.)
    TooSmall,
    /// The cartridge image could not be parsed (unrecognised byte order or a
    /// truncated header) — the underlying [`crate::cart::CartError`].
    Cart(crate::cart::CartError),
}

/// The CIC seed word IPL2 leaves in PIF RAM `0x24` (from which the HLE boot reads
/// the `s3`–`s7` GPRs). Values from cen64 `si/cic.c`.
#[must_use]
pub const fn cic_seed(cic: crate::cart::Cic) -> u32 {
    use crate::cart::Cic;
    match cic {
        Cic::Cic6101 => 0x0004_3F3F,
        Cic::Cic6102 => 0x0000_3F3F,
        Cic::Cic6103 => 0x0000_783F,
        Cic::Cic6105 => 0x0000_913F,
        Cic::Cic6106 => 0x0000_853F,
    }
}

/// Select the AI region from the inserted cartridge's **destination code** (ROM
/// header byte `0x3E`, the last character of the 4-byte game code).
///
/// The AI's video clock — and so its sample rate for a given `AI_DACRATE` —
/// differs between NTSC and PAL consoles. The machinery existed but nothing drove
/// it, so a PAL cartridge played at the NTSC rate; this is the selector
/// (`T-71-005`). An unrecognised code maps to NTSC, so the default is
/// behaviour-preserving.
///
/// The classification and its provenance limits live on
/// [`rustyn64_audio::Region::from_destination_code`].
///
/// `const` is not decorative: the workspace runs clippy's `missing_const_for_fn`
/// under `-D warnings`, which rejects this function without it.
const fn apply_cartridge_region(system: &mut System) {
    let code = system.bus.cart.header().game_code[3];
    let region = rustyn64_audio::Region::from_destination_code(code);
    system.bus.audio.set_region(region);
}

/// The MIPS general-purpose register index of the stack pointer (`$29`/`$sp`).
/// Named so the seed below reads as intent rather than as a magic index, matching
/// how the COP0 writes use `reg::STATUS` / `reg::CONFIG`.
const GPR_SP: u8 = 29;

/// `$at` — the first of the ROM-independent IPL2-exit registers `hle_boot`
/// seeds (ledger R-23). Named, like [`GPR_SP`], so the seeds below read as
/// intent rather than as a column of bare indices.
const GPR_AT: u8 = 1;
/// `$a2` — see [`GPR_AT`].
const GPR_A2: u8 = 6;
/// `$a3` — see [`GPR_AT`].
const GPR_A3: u8 = 7;
/// `$t0` — see [`GPR_AT`].
const GPR_T0: u8 = 8;
/// `$t2` — see [`GPR_AT`].
const GPR_T2: u8 = 10;
/// `$t3` — the RSP DMEM base. Called out separately because it is the one
/// register that decides whether CIC-6105 titles boot at all: their IPL3
/// self-descrambles by reading `0x44(t3)`, its own image in DMEM.
const GPR_T3: u8 = 11;
/// `$s4` — see [`GPR_AT`].
const GPR_S4: u8 = 20;
/// `$ra` — see [`GPR_AT`].
const GPR_RA: u8 = 31;

/// **HLE-boot a retail ROM.**
///
/// Seed the state IPL3 expects, copy the cart's *real* IPL3 (ROM `0x40..0x1000`)
/// into RSP DMEM, and jump to it at `0xA400_0040`. IPL3 then copies the game to
/// RDRAM and jumps to the header entry — running the cart's own bootcode rather
/// than a reimplementation of it. This skips only IPL1/IPL2 (the PIF ROM) and the
/// CIC challenge, which the seed injection stands in for.
///
/// This is the pure retail path: an ELF-payload ROM (n64-systemtest) is handled
/// separately by the harness's `seed_ipl3_handoff`, not here.
///
/// # Errors
/// [`BootError::TooSmall`] if the image is shorter than a `0x1000` boot header.
pub fn hle_boot(system: &mut System, rom: &[u8]) -> Result<(), BootError> {
    use crate::cpu::cop0::reg;

    if rom.len() < 0x1000 {
        return Err(BootError::TooSmall);
    }

    // Insert the cartridge; PI reads and IPL3's DMA see the ROM through it.
    let cart = crate::cart::Cart::load(rom).map_err(BootError::Cart)?;
    let cic = cart.header().cic;
    system.bus.cart = cart;
    apply_cartridge_region(system);

    // Inject the CIC seed into PIF RAM 0x24..0x28 (the boot reads it for s3–s7).
    let seed = cic_seed(cic);
    for (i, b) in seed.to_be_bytes().into_iter().enumerate() {
        system.bus.cart.pif_write(0x24 + i, b);
    }

    // COP0: Status = CU1|CU0|FR, Config = the IPL3-left value (K0=3 cached).
    system
        .cpu
        .pipeline
        .cop0
        .set_hardware(reg::STATUS, 0x3400_0000);
    system
        .cpu
        .pipeline
        .cop0
        .set_hardware(reg::CONFIG, 0x7006_E463);

    // **The stack pointer IPL3 inherits.** IPL1 sets it before handing off —
    // N64brew *IPL2* §IPL1 listing, `0xBFC000D0`:
    // `ORI sp, sp, 0x1FF0  # sp = 0xA4001FF0 (this prepares sp for use in IPL2)`
    // — and IPL2 leaves it alone, so IPL3 runs on it. It points at the top of RSP
    // IMEM, which is where IPL3's stack lives while IPL3 itself executes from DMEM.
    //
    // Skipping this is NOT harmless. `sp` would be 0, IPL3's opening
    // `ADDIU sp, sp, -24` / `SW s3, 0(sp)` prologue would store to `0xFFFF_FFE8`
    // (KSEG3 — TLB-mapped, no entries), and the resulting TLB-refill exception
    // vectors to `0x8000_0000` in empty RDRAM. Every retail title then executed a
    // NOP sled to the end of memory instead of booting (ledger R-18).
    system.cpu.regs.write(GPR_SP, 0xFFFF_FFFF_A400_1FF0);

    // **The rest of the IPL2 exit state IPL3 inherits — MEASURED, not invented.**
    //
    // Captured at IPL3's entry (`0xA400_0040`) by running the console's real
    // IPL1/IPL2 out of a PIF ROM dump via [`real_pif_boot`], then keeping only the
    // registers that are **identical across ROMs of different CIC variants**
    // (compared Banjo-Tooie / CIC-6105 against Super Mario 64 / CIC-6102).
    //
    // The excluded registers are excluded on purpose: `v0`, `v1`, `a0`, `a1` and
    // `t4`-`t9` differ per ROM because they carry IPL2's running checksum of that
    // cartridge's IPL3, and `s6` is the CIC seed, already set per-CIC above.
    // Freezing any of those would be fabricating a value the boot computes.
    //
    // `t3 = 0xA400_0000` is the one that matters most: **CIC-6105's IPL3 is a
    // different program** that opens with a self-descrambling XOR loop reading
    // `0x44(t3)` — DMEM + 0x40, its own image. With `t3 = 0` that read goes to
    // low RDRAM and the descramble produces garbage (ledger R-23).
    system.cpu.regs.write(GPR_AT, 1);
    system.cpu.regs.write(GPR_A2, 0xFFFF_FFFF_A400_1F0C);
    system.cpu.regs.write(GPR_A3, 0xFFFF_FFFF_A400_1F08);
    system.cpu.regs.write(GPR_T0, 0xC0);
    system.cpu.regs.write(GPR_T2, 0x40);
    system.cpu.regs.write(GPR_T3, 0xFFFF_FFFF_A400_0000);
    system.cpu.regs.write(GPR_S4, 1);
    system.cpu.regs.write(GPR_RA, 0xFFFF_FFFF_A400_1550);

    // s3–s7 the OS/IPL3 rely on: rom_type=0 (cart), tv_type=1 (NTSC),
    // reset_type=0 (cold), s6 = the CIC seed byte, s7 = 0.
    system.cpu.regs.write(19, 0);
    system.cpu.regs.write(20, 1);
    system.cpu.regs.write(21, 0);
    system.cpu.regs.write(22, u64::from((seed >> 8) & 0xFF));
    system.cpu.regs.write(23, 0);

    // PI DOM1 bus timing from the ROM header's first word (as IPL2 does).
    let cfg = u32::from_be_bytes([rom[0], rom[1], rom[2], rom[3]]);
    system
        .bus
        .pi
        .write(crate::cart::pi::PI_BSD_DOM1_LAT, cfg & 0xFF);
    system
        .bus
        .pi
        .write(crate::cart::pi::PI_BSD_DOM1_PWD, (cfg >> 8) & 0xFF);
    system
        .bus
        .pi
        .write(crate::cart::pi::PI_BSD_DOM1_PGS, (cfg >> 16) & 0x0F);
    system
        .bus
        .pi
        .write(crate::cart::pi::PI_BSD_DOM1_RLS, (cfg >> 20) & 0x03);

    // Copy the real IPL3 into DMEM (`0x40..0x1000`) and jump to it.
    let ipl3 = &rom[0x40..0x1000];
    system.bus.rsp.dmem[0x40..0x40 + ipl3.len()].copy_from_slice(ipl3);
    system.cpu.set_pc(0xFFFF_FFFF_A400_0040);
    Ok(())
}

/// **Real-PIF boot a retail ROM** — the faithful path (off by default, local).
///
/// Where [`hle_boot`] *seeds* the post-IPL3 state and jumps straight into the
/// cartridge's IPL3, this runs the console's **real IPL1 and IPL2** from the
/// supplied PIF boot ROM: it installs that ROM at `0x1FC0_0000`, models the
/// PIF-SM5's power-on hand-off (writes the CIC seed word the CIC would have
/// relayed into PIF RAM `0x24`, and registers the CIC's IPL2 checksum so the PIF
/// can adjudicate IPL2's verify command), and leaves the CPU at its reset vector
/// `0xBFC0_0000`. The CPU then fetches IPL1 → copies IPL2 to IMEM → runs IPL2 →
/// verifies the checksum → jumps into the cart's own IPL3, exactly as hardware
/// does (`n64brew_wiki/markdown/PIF-NUS.md` §Console startup).
///
/// The PIF boot ROM is copyrighted and is **never committed**; a caller supplies
/// a local dump. On a genuine cartridge the checksum matches and boot proceeds;
/// a mismatch makes the PIF freeze the CPU via NMI ([`crate::Bus::boot_nmi_halt`]).
///
/// # Errors
/// [`BootError::TooSmall`] if `rom` is shorter than a `0x1000` boot header or
/// `pif_rom` is shorter than the PIF-ROM window.
pub fn real_pif_boot(system: &mut System, rom: &[u8], pif_rom: &[u8]) -> Result<(), BootError> {
    if rom.len() < 0x1000 || pif_rom.len() < crate::cart::pif::PIF_ROM_LEN {
        return Err(BootError::TooSmall);
    }

    let cart = crate::cart::Cart::load(rom).map_err(BootError::Cart)?;
    let cic = cart.header().cic;
    system.bus.cart = cart;
    apply_cartridge_region(system);

    // Install the real IPL1/IPL2 so the CPU fetches them from the reset vector.
    system.bus.cart.pif_load_boot_rom(pif_rom);

    // Model the PIF-SM5 power-on hand-off: after the CIC exchange, the PIF writes
    // the boot-info word to PIF RAM 0x24-0x27, which IPL2 reads — byte 0x27 = the
    // IPL2 seed (IPL2 feeds it to its checksum), byte 0x26 = the IPL3 seed. These
    // come from `CicBootSecrets`, NOT the legacy `cic_seed` word: cen64's seed
    // packs 0x3F into the IPL2-seed byte for every CIC (harmless when the checksum
    // is HLE'd, wrong when the real IPL2 consumes it), which N64brew corrects. The
    // upper bits (region / reset-type / 64DD) stay 0 for a cold NTSC cart boot.
    let secrets = cic.boot_secrets();
    system.bus.cart.pif_write(0x24, 0x00);
    system.bus.cart.pif_write(0x25, 0x00);
    system.bus.cart.pif_write(0x26, secrets.ipl3_seed);
    system.bus.cart.pif_write(0x27, secrets.ipl2_seed);
    // Command byte 0x3F bit 0x80 is the PIF's "busy" gate IPL2 spins on; a zeroed
    // PIF RAM already has it clear, so IPL2's startup sync passes immediately.

    // Register the CIC's IPL2 checksum so the PIF adjudicates IPL2's verify.
    system.bus.cart.pif_set_boot_checksum(secrets.ipl2_checksum);

    // The CPU is already at 0xBFC0_0000 (System::new's reset vector); do NOT set
    // the PC — that is the whole point of running the real boot ROM.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cart::Cic;

    /// **`hle_boot` seeds the stack pointer IPL3 inherits** (`0xA400_1FF0`,
    /// sign-extended). IPL1 sets it before handing off — N64brew *IPL2* §IPL1
    /// listing, `0xBFC000D0` — and `hle_boot` skips IPL1/IPL2, so it must stand in.
    ///
    /// This is asserted on its own because leaving `sp` at 0 does **not** crash or
    /// panic: IPL3's opening `ADDIU sp, sp, -24` / `SW s3, 0(sp)` prologue faults to
    /// `0xFFFF_FFE8` (KSEG3, unmapped), takes a TLB-refill exception to
    /// `0x8000_0000`, and executes a NOP sled through empty RDRAM to the end of
    /// memory. Every retail title did exactly that, quietly, while still retiring
    /// hundreds of millions of instructions — so an instruction-count or
    /// does-not-panic check cannot detect it. Ledger R-18.
    #[test]
    fn hle_boot_seeds_the_stack_pointer_ipl3_inherits() {
        let mut rom = [0u8; 0x1000];
        rom[0..4].copy_from_slice(&[0x80, 0x37, 0x12, 0x40]); // .z64 magic
        let mut sys = System::new(0);
        hle_boot(&mut sys, &rom).expect("boot");
        assert_eq!(
            sys.cpu.regs.read(29),
            0xFFFF_FFFF_A400_1FF0,
            "sp must be the top of RSP IMEM, sign-extended"
        );
        // And it must be a *valid* address to store through: the whole failure was
        // that `sp - 24` landed outside any mapped segment.
        let prologue = sys.cpu.regs.read(29).wrapping_sub(24) & 0xFFFF_FFFF;
        assert!(
            (0xA400_0000..0xA400_2000).contains(&prologue),
            "sp-24 must stay inside SP DMEM/IMEM, got {prologue:#010x}"
        );
    }

    /// **`hle_boot` seeds the ROM-independent part of the IPL2 exit state**
    /// (ledger R-23), measured by running the real IPL1/IPL2 from a PIF ROM dump.
    ///
    /// `t3` is asserted with its own message because it is the one that decides
    /// whether CIC-6105 titles boot at all: their IPL3 self-descrambles by reading
    /// `0x44(t3)` = DMEM + 0x40, its own image. Removing just this register sends
    /// Banjo-Tooie back to a NOP sled with RDRAM left entirely empty.
    ///
    /// The checksum-derived registers are asserted **absent**: `v0`/`v1`/`a0`/`a1`
    /// and `t4`-`t9` differ per ROM because they carry IPL2's running checksum of
    /// that cartridge's IPL3, so seeding them would freeze a computed value.
    #[test]
    fn hle_boot_seeds_the_rom_independent_ipl2_exit_state() {
        let mut rom = [0u8; 0x1000];
        rom[0..4].copy_from_slice(&[0x80, 0x37, 0x12, 0x40]);
        let mut sys = System::new(0);
        hle_boot(&mut sys, &rom).expect("boot");

        assert_eq!(
            sys.cpu.regs.read(11),
            0xFFFF_FFFF_A400_0000,
            "t3 must be the DMEM base — CIC-6105's IPL3 descrambles through it"
        );
        for (reg, want, name) in [
            (1u8, 1u64, "at"),
            (6, 0xFFFF_FFFF_A400_1F0C, "a2"),
            (7, 0xFFFF_FFFF_A400_1F08, "a3"),
            (8, 0xC0, "t0"),
            (10, 0x40, "t2"),
            (20, 1, "s4"),
            (31, 0xFFFF_FFFF_A400_1550, "ra"),
        ] {
            assert_eq!(sys.cpu.regs.read(reg), want, "{name} (r{reg})");
        }
        // Checksum-derived registers must stay unset — **every** documented
        // exclusion, not a sample. `t4`-`t9` is r12-r15 plus r24-r25; listing only
        // the endpoints would let a seed slip into the middle of the range.
        for (reg, name) in [
            (2u8, "v0"),
            (3, "v1"),
            (4, "a0"),
            (5, "a1"),
            (12, "t4"),
            (13, "t5"),
            (14, "t6"),
            (15, "t7"),
            (24, "t8"),
            (25, "t9"),
        ] {
            assert_eq!(
                sys.cpu.regs.read(reg),
                0,
                "{name} (r{reg}) is IPL2's per-ROM checksum state and must NOT be seeded"
            );
        }
    }

    #[test]
    fn too_small_a_rom_is_rejected_before_any_slice() {
        let mut sys = System::new(0);
        // Shorter than the 0x1000 header + IPL3 — must error, not panic on a slice.
        assert_eq!(hle_boot(&mut sys, &[0u8; 0x40]), Err(BootError::TooSmall));
        assert_eq!(hle_boot(&mut sys, &[]), Err(BootError::TooSmall));
    }

    #[test]
    fn real_pif_boot_rejects_a_short_pif_rom() {
        let mut sys = System::new(0);
        let rom = [0u8; 0x1000];
        assert_eq!(
            real_pif_boot(&mut sys, &rom, &[0u8; 16]),
            Err(BootError::TooSmall),
            "a PIF ROM shorter than its window is rejected"
        );
    }

    /// **Booting a PAL cartridge retunes the AI (T-71-005).** The region selector
    /// must actually run during boot, not merely exist: this boots two ROMs that
    /// differ ONLY in the destination code (header byte 0x3E) and asserts the AI
    /// sample rate differs. Removing the `apply_cartridge_region` call makes both
    /// rates identical and fails here.
    #[test]
    fn a_pal_destination_code_retunes_the_ai_at_boot() {
        // AI_DACRATE must be programmed for a rate to exist at all; the boot path
        // does not set it, so drive it directly after booting.
        let rate_for = |dest: u8| {
            let mut rom = [0u8; 0x1000];
            rom[0..4].copy_from_slice(&[0x80, 0x37, 0x12, 0x40]); // .z64 big-endian magic
            rom[0x3E] = dest;
            let mut sys = System::new(0);
            hle_boot(&mut sys, &rom).expect("boot");
            sys.bus.audio.write_reg(4, 1103); // AI_DACRATE
            sys.bus.audio.sample_rate()
        };
        let ntsc = rate_for(b'E'); // North America
        let pal = rate_for(b'P'); // Europe
        assert_ne!(
            ntsc, pal,
            "a PAL cartridge must boot with a different AI rate than an NTSC one"
        );
        // And an unknown code keeps the NTSC default (behaviour-preserving).
        assert_eq!(
            rate_for(b'?'),
            ntsc,
            "an unknown destination code stays NTSC"
        );
    }

    /// **The real-PIF boot path selects the region too.** `apply_cartridge_region`
    /// has two call sites; the HLE test above covers only one, so a regression in
    /// the `real_pif_boot` call would go undetected. Same assertion, other path.
    #[test]
    fn the_real_pif_boot_path_also_selects_the_region() {
        let rate_for = |dest: u8| {
            let mut rom = [0u8; 0x1000];
            rom[0..4].copy_from_slice(&[0x80, 0x37, 0x12, 0x40]); // .z64 magic
            rom[0x3E] = dest;
            // A blank PIF ROM of the right size: this test asserts the region
            // selection, not the IPL1/IPL2 execution a real ROM would drive.
            let pif = [0u8; crate::cart::pif::PIF_ROM_LEN];
            let mut sys = System::new(0);
            real_pif_boot(&mut sys, &rom, &pif).expect("boot");
            sys.bus.audio.write_reg(4, 1103); // AI_DACRATE
            sys.bus.audio.sample_rate()
        };
        assert_ne!(
            rate_for(b'E'),
            rate_for(b'P'),
            "real_pif_boot must select the region as hle_boot does"
        );
    }

    #[test]
    fn cic_seed_covers_every_variant() {
        // The low byte is the IPL2 seed the boot leaves in PIF RAM; bits 8-15 the
        // IPL3 seed (cen64 si/cic.c). Pin all five arms so a typo fails here.
        assert_eq!(cic_seed(Cic::Cic6101), 0x0004_3F3F);
        assert_eq!(cic_seed(Cic::Cic6102), 0x0000_3F3F);
        assert_eq!(cic_seed(Cic::Cic6103), 0x0000_783F);
        assert_eq!(cic_seed(Cic::Cic6105), 0x0000_913F);
        assert_eq!(cic_seed(Cic::Cic6106), 0x0000_853F);
    }
}
