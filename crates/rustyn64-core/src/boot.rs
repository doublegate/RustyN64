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
