//! The PIF RAM (64 bytes) + the joybus frame executor.
//!
//! The CPU fills PIF RAM with a joybus *frame* — a sequence of up to five
//! per-channel handshakes `TX RX tt[..] rr[..]` (channels 0–3 = controller
//! ports, channel 4 = the cartridge bus / EEPROM) — sets the command byte
//! (`0x3F`) bit 0, and triggers an SI DMA. On the SI read the PIF *executes* the
//! handshakes and writes the replies back into the `rr[..]` regions
//! (`n64brew_wiki/markdown/PIF-NUS.md`, `Joybus protocol.md`).
//!
//! We model the common commands: `0x00`/`0xFF` info, `0x01` controller state,
//! `0x02`/`0x03` Controller-Pak accessory access, and `0x04`/`0x05` EEPROM.
//! Controller state comes from the Bus's four packed port words; the accessory
//! and EEPROM backing is the cart's [`crate::save::SaveDevice`].
#![allow(
    clippy::doc_markdown,
    reason = "joybus prose names PIF/EEPROM/CRC/DMA/RX/TX constantly"
)]

use alloc::boxed::Box;

use crate::save::SaveDevice;

/// PIF RAM size (bytes). The last byte (`0x3F`) is the command bitmask.
pub const PIF_RAM_LEN: usize = 64;
/// The command byte offset within PIF RAM.
const CMD_BYTE: usize = 0x3F;

/// PIF **boot** ROM size (bytes) — IPL1 + IPL2.
///
/// Memory-mapped at `0x1FC0_0000..0x1FC0_07C0` during boot only (`PIF-NUS.md`
/// §Internal ROMs). The 64-byte PIF RAM (`PIF_RAM_LEN`) is the tail of the same
/// 2 KiB PIF space and is modelled separately. Used only on the real-PIF boot
/// path; `None` under HLE.
pub const PIF_ROM_LEN: usize = 0x7C0;

/// Frame control bytes (`PIF-NUS.md` §Joybus frame).
const TX_END: u8 = 0xFE; // end of the command list
const TX_SKIP: u8 = 0x00; // no command on this channel — advance to the next
const TX_NOP: u8 = 0xFF; // padding byte — skip without advancing a channel

/// The RX "device not present" flag (`PIF-NUS.md` §RX byte special flags): the
/// PIF sets bit 7 of the RX byte when a channel times out (no device).
const RX_NO_DEVICE: u8 = 0x80;

/// Standard N64 controller identifier (`0x00`/`0xFF` info reply, 2 bytes).
const ID_CONTROLLER: [u8; 2] = [0x05, 0x00];
/// EEPROM identifiers (info reply): 4k = `0x0080`, 16k = `0x00C0`.
const ID_EEPROM_4K: [u8; 2] = [0x00, 0x80];
const ID_EEPROM_16K: [u8; 2] = [0x00, 0xC0];

/// The PIF: its 64-byte RAM plus the per-channel accessory-change latch.
#[derive(Clone, Debug)]
pub struct Pif {
    ram: [u8; PIF_RAM_LEN],
    /// Whether each controller port reports a Controller Pak present.
    pak_present: [bool; 4],
    /// The boot ROM (IPL1/IPL2), present only on the real-PIF boot path. `None`
    /// under HLE (the default), where boot skips IPL1/IPL2 entirely — so the
    /// default machine allocates nothing here and the PIF-ROM window reads back 0.
    boot_rom: Option<Box<[u8; PIF_ROM_LEN]>>,
    /// The CIC's 6-byte IPL2 checksum, registered at real-PIF boot. The PIF
    /// compares IPL2's computed checksum against it on the verify command; `None`
    /// under HLE (nothing runs IPL2). See [`Pif::boot_command`].
    boot_checksum: Option<[u8; 6]>,
    /// The checksum IPL2 wrote to PIF RAM `0x32-0x37`, latched by the `0x20`
    /// (acquire) command and compared on the `0x40` (run) command.
    boot_acquired: Option<[u8; 6]>,
    /// Set once IPL2 issues the `0x10` ROM-lockout command; the PIF-ROM window
    /// then reads back 0 (the real PIF removes it from the serial bus).
    rom_locked: bool,
}

impl Default for Pif {
    fn default() -> Self {
        Self::new()
    }
}

impl Pif {
    /// Power-on PIF (RAM cleared, no pak, no boot ROM).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            ram: [0; PIF_RAM_LEN],
            pak_present: [false; 4],
            boot_rom: None,
            boot_checksum: None,
            boot_acquired: None,
            rom_locked: false,
        }
    }

    /// Install the PIF boot ROM (IPL1/IPL2) for the real-PIF boot path. Accepts
    /// the raw dump; only the first [`PIF_ROM_LEN`] bytes (the ROM window) are
    /// kept — a longer dump that also carries the 64-byte RAM tail is truncated.
    /// Too-short input leaves the ROM absent (the real-PIF boot then cannot run).
    pub fn load_boot_rom(&mut self, bytes: &[u8]) {
        if bytes.len() < PIF_ROM_LEN {
            return;
        }
        let mut rom = Box::new([0u8; PIF_ROM_LEN]);
        rom.copy_from_slice(&bytes[..PIF_ROM_LEN]);
        self.boot_rom = Some(rom);
    }

    /// Is a real PIF boot ROM installed?
    #[must_use]
    pub const fn has_boot_rom(&self) -> bool {
        self.boot_rom.is_some()
    }

    /// Read a byte of the PIF boot ROM (`0x1FC0_0000 + off`, `off < PIF_ROM_LEN`).
    /// Reads back 0 when no ROM is installed (HLE), once IPL2 has locked the ROM
    /// (`0x10` command), or `off` is out of range — the same value the unmapped
    /// PIF-ROM window returned before this path existed.
    #[must_use]
    pub fn boot_rom_read(&self, off: usize) -> u8 {
        if self.rom_locked {
            return 0;
        }
        self.boot_rom
            .as_ref()
            .and_then(|rom| rom.get(off))
            .copied()
            .unwrap_or(0)
    }

    /// Warm-reset the transient real-PIF boot state so a reset can re-run
    /// IPL1→IPL2 (`PIF-NUS.md` §Console Reset: the PIF **unlocks the PIF ROM** on
    /// the reset NMI). Clears the ROM lockout and the latched checksum; keeps the
    /// installed boot ROM and the registered CIC checksum — the cartridge is still
    /// inserted, so the CIC would re-hand-off the same values.
    pub const fn reset_boot(&mut self) {
        self.rom_locked = false;
        self.boot_acquired = None;
    }

    /// Register the CIC's 6-byte IPL2 checksum for the real-PIF boot verify.
    pub const fn set_boot_checksum(&mut self, checksum: [u8; 6]) {
        self.boot_checksum = Some(checksum);
    }

    /// **Process a reset-mode PIF command-byte write** during the real-PIF boot,
    /// returning `true` if the checksum verify FAILED and the caller must NMI-halt
    /// the CPU (`PIF-NUS.md` §Console startup 8.7). No-op (returns `false`) unless
    /// a boot ROM is installed, so the run-mode joybus path is unaffected.
    ///
    /// The command bits (`PIF-NUS.md` reset-mode table, cross-checked against the
    /// SM5 firmware and the IPL2 stores at `bfc006xx`):
    /// - `0x10` **ROM lockout** — remove the PIF-ROM from the bus (reads 0 after).
    /// - `0x20` **acquire checksum** — latch the 6 bytes IPL2 wrote to `0x32-0x37`,
    ///   zero them in PIF RAM, and set bit `0x80` to acknowledge (IPL2 spins on it).
    /// - `0x40` **run checksum** — compare the latched value against the CIC's; a
    ///   mismatch halts the CPU, a match lets IPL2's unconditional jump reach IPL3.
    ///
    /// Each handled bit is cleared after processing; bit `0x80` is PIF-owned.
    pub fn boot_command(&mut self) -> bool {
        if self.boot_rom.is_none() {
            return false;
        }
        let cmd = self.ram[CMD_BYTE];

        if cmd & 0x10 != 0 {
            self.rom_locked = true;
            self.ram[CMD_BYTE] &= !0x10;
        }

        if cmd & 0x20 != 0 {
            let mut got = [0u8; 6];
            got.copy_from_slice(&self.ram[0x32..0x38]);
            self.boot_acquired = Some(got);
            self.ram[0x32..0x38].fill(0); // PIF zeroes the checksum after reading
            self.ram[CMD_BYTE] = (self.ram[CMD_BYTE] & !0x20) | 0x80; // ack
        }

        if cmd & 0x40 != 0 {
            self.ram[CMD_BYTE] &= !0x40;
            // Mismatch only when both are known and differ; absent data never
            // spuriously halts a genuine boot.
            if let (Some(got), Some(expected)) = (self.boot_acquired, self.boot_checksum) {
                return got != expected;
            }
        }

        false
    }

    /// Report a Controller Pak on port `channel` (0–3) — the info status byte
    /// then advertises it and `0x02`/`0x03` route to the cart save.
    pub const fn set_pak_present(&mut self, channel: usize, present: bool) {
        if channel < 4 {
            self.pak_present[channel] = present;
        }
    }

    /// The 64-byte PIF RAM (for the SI DMA to copy to/from RDRAM).
    #[must_use]
    pub const fn ram(&self) -> &[u8; PIF_RAM_LEN] {
        &self.ram
    }

    /// A byte of PIF RAM (CPU direct read at `0x1FC0_07C0 + off`).
    #[must_use]
    pub fn read(&self, off: usize) -> u8 {
        self.ram.get(off).copied().unwrap_or(0)
    }

    /// Write a byte of PIF RAM (CPU direct write / SI DMA).
    pub const fn write(&mut self, off: usize, val: u8) {
        if off < PIF_RAM_LEN {
            self.ram[off] = val;
        }
    }

    /// Replace the whole PIF RAM (an SI 64-byte DMA write from RDRAM).
    pub fn load(&mut self, bytes: &[u8; PIF_RAM_LEN]) {
        self.ram = *bytes;
    }

    /// **Execute the joybus frame** if the command byte requests it (bit 0),
    /// filling each handshake's reply region. Called on an SI read (the point at
    /// which the PIF runs the handshakes), then the caller DMAs PIF RAM out.
    pub fn execute(&mut self, controllers: &[u32; 4], save: &mut SaveDevice) {
        if self.ram[CMD_BYTE] & 0x01 == 0 {
            return;
        }
        let mut i = 0;
        let mut channel = 0usize;
        while i < CMD_BYTE && channel <= 4 {
            match self.ram[i] {
                TX_END => break,
                TX_NOP => {
                    i += 1;
                }
                TX_SKIP => {
                    channel += 1;
                    i += 1;
                }
                tx => {
                    let tx_len = (tx & 0x3F) as usize;
                    if i + 1 >= CMD_BYTE {
                        break;
                    }
                    let rx_len = (self.ram[i + 1] & 0x3F) as usize;
                    let cmd = i + 2;
                    let resp = cmd + tx_len;
                    if resp + rx_len > PIF_RAM_LEN {
                        break; // malformed frame — do not run past the RAM
                    }
                    self.run_channel(channel, cmd, tx_len, resp, rx_len, controllers, save);
                    i = resp + rx_len;
                    channel += 1;
                }
            }
        }
        // Parsing/execution consumed the request.
        self.ram[CMD_BYTE] &= !0x01;
    }

    /// Run one channel's handshake. `cmd`/`resp` are PIF-RAM byte offsets.
    #[allow(
        clippy::too_many_arguments,
        reason = "a joybus handshake is inherently many small operands"
    )]
    fn run_channel(
        &mut self,
        channel: usize,
        cmd: usize,
        tx_len: usize,
        resp: usize,
        rx_len: usize,
        controllers: &[u32; 4],
        save: &mut SaveDevice,
    ) {
        if tx_len == 0 {
            return;
        }
        let command = self.ram[cmd];
        match (channel, command) {
            // Controllers on channels 0–3.
            (0..=3, 0x00 | 0xFF) => {
                if rx_len >= 3 {
                    self.ram[resp] = ID_CONTROLLER[0];
                    self.ram[resp + 1] = ID_CONTROLLER[1];
                    // Status: 1 = pak present, 2 = pak absent.
                    self.ram[resp + 2] = if self.pak_present[channel] {
                        0x01
                    } else {
                        0x02
                    };
                }
            }
            (0..=3, 0x01) => {
                let state = controllers[channel].to_be_bytes();
                let n = rx_len.min(4);
                self.ram[resp..resp + n].copy_from_slice(&state[..n]);
            }
            // Controller-Pak accessory read/write on channels 0–3.
            (0..=3, 0x02) if tx_len >= 3 => {
                let addr = u16::from_be_bytes([self.ram[cmd + 1], self.ram[cmd + 2]]) & 0xFFE0;
                let mut block = [0u8; 32];
                if self.pak_present[channel] {
                    save.cpak_read(addr, &mut block);
                }
                let n = rx_len.min(32);
                self.ram[resp..resp + n].copy_from_slice(&block[..n]);
                if rx_len >= 33 {
                    // A present pak returns the CRC; an absent one complements it.
                    let crc = data_crc(&block);
                    self.ram[resp + 32] = if self.pak_present[channel] { crc } else { !crc };
                }
            }
            (0..=3, 0x03) if tx_len >= 35 => {
                let addr = u16::from_be_bytes([self.ram[cmd + 1], self.ram[cmd + 2]]) & 0xFFE0;
                let mut block = [0u8; 32];
                block.copy_from_slice(&self.ram[cmd + 3..cmd + 35]);
                if self.pak_present[channel] {
                    save.cpak_write(addr, &block);
                }
                if rx_len >= 1 {
                    let crc = data_crc(&block);
                    self.ram[resp] = if self.pak_present[channel] { crc } else { !crc };
                }
            }
            // Cartridge bus (channel 4): EEPROM.
            (4, 0x00 | 0xFF) => {
                if let SaveDevice::Eeprom(store) = save {
                    if rx_len >= 3 {
                        let id = if store.len() > 512 {
                            ID_EEPROM_16K
                        } else {
                            ID_EEPROM_4K
                        };
                        self.ram[resp] = id[0];
                        self.ram[resp + 1] = id[1];
                        self.ram[resp + 2] = 0x00;
                    }
                } else {
                    self.mark_no_device(resp, rx_len);
                }
            }
            (4, 0x04) if tx_len >= 2 && matches!(save, SaveDevice::Eeprom(_)) => {
                let mut block = [0u8; 8];
                save.eeprom_read_block(self.ram[cmd + 1], &mut block);
                let n = rx_len.min(8);
                self.ram[resp..resp + n].copy_from_slice(&block[..n]);
            }
            (4, 0x05) if tx_len >= 10 && matches!(save, SaveDevice::Eeprom(_)) => {
                let mut data = [0u8; 8];
                data.copy_from_slice(&self.ram[cmd + 2..cmd + 10]);
                save.eeprom_write_block(self.ram[cmd + 1], &data);
                if rx_len >= 1 {
                    self.ram[resp] = 0x00; // status: write OK
                }
            }
            // No device / unsupported command on this channel.
            _ => self.mark_no_device(resp, rx_len),
        }
    }

    /// Flag a channel's reply as "no device" (RX byte bit 7 set).
    fn mark_no_device(&mut self, resp: usize, rx_len: usize) {
        // The RX-length byte sits just before the reply region (`resp - 1`).
        if resp >= 1 && rx_len > 0 {
            self.ram[resp - 1] |= RX_NO_DEVICE;
        }
    }
}

/// The joybus accessory data CRC8 (seed 0x00, polynomial 0x85 —
/// `Joybus protocol.md` §Data CRC).
#[must_use]
pub fn data_crc(data: &[u8; 32]) -> u8 {
    let mut crc = 0u8;
    for &byte in data {
        crc ^= byte;
        for _ in 0..8 {
            let carry = crc & 0x80 != 0;
            crc <<= 1;
            if carry {
                crc ^= 0x85;
            }
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SaveType;

    /// Build a single-channel frame in PIF RAM and set the run bit.
    fn frame(pif: &mut Pif, channel_skips: usize, tx: &[u8], rx_len: u8) -> usize {
        let mut i = 0;
        for _ in 0..channel_skips {
            pif.ram[i] = TX_SKIP;
            i += 1;
        }
        pif.ram[i] = tx.len() as u8;
        pif.ram[i + 1] = rx_len;
        pif.ram[i + 2..i + 2 + tx.len()].copy_from_slice(tx);
        let resp = i + 2 + tx.len();
        pif.ram[CMD_BYTE] = 0x01;
        resp
    }

    #[test]
    fn controller_state_is_returned_from_the_port_word() {
        let mut pif = Pif::new();
        let mut save = SaveDevice::None;
        let resp = frame(&mut pif, 0, &[0x01], 4);
        let controllers = [0x8000_1234, 0, 0, 0]; // A pressed, stick (0x12, 0x34)
        pif.execute(&controllers, &mut save);
        assert_eq!(&pif.ram[resp..resp + 4], &[0x80, 0x00, 0x12, 0x34]);
        assert_eq!(pif.ram[CMD_BYTE] & 1, 0, "the run bit is cleared");
    }

    #[test]
    fn info_reports_a_controller_and_pak_status() {
        let mut pif = Pif::new();
        pif.set_pak_present(0, true);
        let mut save = SaveDevice::None;
        let resp = frame(&mut pif, 0, &[0x00], 3);
        pif.execute(&[0; 4], &mut save);
        assert_eq!(
            &pif.ram[resp..resp + 3],
            &[0x05, 0x00, 0x01],
            "controller + pak present"
        );
    }

    #[test]
    fn eeprom_write_then_read_round_trips_over_joybus() {
        let mut save = SaveDevice::new(SaveType::Eeprom4k);
        // Write block 2 = [1..8] on channel 4 (0x05 cmd, block, 8 data, rx 1).
        let mut pif = Pif::new();
        let tx = [0x05, 0x02, 1, 2, 3, 4, 5, 6, 7, 8];
        frame(&mut pif, 4, &tx, 1);
        pif.execute(&[0; 4], &mut save);
        // Read it back (0x04 cmd, block, rx 8).
        let mut pif = Pif::new();
        let resp = frame(&mut pif, 4, &[0x04, 0x02], 8);
        pif.execute(&[0; 4], &mut save);
        assert_eq!(&pif.ram[resp..resp + 8], &[1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn controller_pak_write_then_read_round_trips_with_crc() {
        let mut save = SaveDevice::new(SaveType::ControllerPak);
        let mut pif = Pif::new();
        pif.set_pak_present(0, true);
        // Write 32 bytes at address 0x0100 (aligned).
        let mut tx = [0u8; 35];
        tx[0] = 0x03;
        tx[1] = 0x01;
        tx[2] = 0x00;
        for (k, b) in tx[3..].iter_mut().enumerate() {
            *b = k as u8;
        }
        frame(&mut pif, 0, &tx, 1);
        pif.execute(&[0; 4], &mut save);
        // Read it back with the data CRC appended.
        let mut pif = Pif::new();
        pif.set_pak_present(0, true);
        let resp = frame(&mut pif, 0, &[0x02, 0x01, 0x00], 33);
        pif.execute(&[0; 4], &mut save);
        let mut expected = [0u8; 32];
        for (k, b) in expected.iter_mut().enumerate() {
            *b = k as u8;
        }
        assert_eq!(&pif.ram[resp..resp + 32], &expected);
        assert_eq!(
            pif.ram[resp + 32],
            data_crc(&expected),
            "present pak returns the CRC"
        );
    }

    #[test]
    fn boot_rom_is_absent_by_default_and_reads_zero() {
        let pif = Pif::new();
        assert!(!pif.has_boot_rom(), "HLE default installs no boot ROM");
        assert_eq!(pif.boot_rom_read(0), 0, "absent ROM reads 0");
        assert_eq!(pif.boot_rom_read(PIF_ROM_LEN - 1), 0);
    }

    #[test]
    fn boot_rom_loads_and_reads_back_the_window() {
        let mut pif = Pif::new();
        // A dump longer than the ROM window (carrying the 64-byte RAM tail) is
        // truncated to the window; a byte pattern makes the mapping observable.
        let mut dump = alloc::vec![0u8; PIF_ROM_LEN + PIF_RAM_LEN];
        for (i, b) in dump.iter_mut().enumerate() {
            *b = (i & 0xFF) as u8;
        }
        pif.load_boot_rom(&dump);
        assert!(pif.has_boot_rom());
        assert_eq!(pif.boot_rom_read(0), 0x00);
        assert_eq!(pif.boot_rom_read(1), 0x01);
        assert_eq!(pif.boot_rom_read(0x100), 0x00);
        assert_eq!(
            pif.boot_rom_read(PIF_ROM_LEN - 1),
            ((PIF_ROM_LEN - 1) & 0xFF) as u8
        );
        // Out of range still reads 0, not the truncated tail.
        assert_eq!(pif.boot_rom_read(PIF_ROM_LEN), 0);
    }

    #[test]
    fn too_short_a_dump_leaves_the_boot_rom_absent() {
        let mut pif = Pif::new();
        pif.load_boot_rom(&[0xAB; 16]);
        assert!(
            !pif.has_boot_rom(),
            "a short dump must not install a partial ROM"
        );
    }

    /// The CIC 6102 IPL2 checksum (`PIF-NUS.md` §checksum table).
    const SUM_6102: [u8; 6] = [0xA5, 0x36, 0xC0, 0xF1, 0xD8, 0x59];

    /// Put the PIF in real-PIF boot mode with a registered CIC checksum.
    fn booting_pif(checksum: [u8; 6]) -> Pif {
        let mut pif = Pif::new();
        pif.load_boot_rom(&[0u8; PIF_ROM_LEN]); // presence enables the boot path
        pif.set_boot_checksum(checksum);
        pif
    }

    #[test]
    fn boot_acquire_latches_zeroes_and_acks_the_checksum() {
        let mut pif = booting_pif(SUM_6102);
        // IPL2 writes its computed checksum to 0x32-0x37, then sets 0x20.
        pif.ram[0x32..0x38].copy_from_slice(&SUM_6102);
        pif.ram[CMD_BYTE] = 0x20;
        assert!(!pif.boot_command(), "acquire never halts");
        assert_eq!(&pif.ram[0x32..0x38], &[0u8; 6], "PIF zeroes the checksum");
        assert_eq!(
            pif.ram[CMD_BYTE] & 0x80,
            0x80,
            "ack bit set (IPL2 spins on it)"
        );
        assert_eq!(pif.ram[CMD_BYTE] & 0x20, 0, "acquire bit cleared");
    }

    #[test]
    fn boot_run_boots_on_a_matching_checksum() {
        let mut pif = booting_pif(SUM_6102);
        pif.ram[0x32..0x38].copy_from_slice(&SUM_6102);
        pif.ram[CMD_BYTE] = 0x20;
        assert!(!pif.boot_command());
        pif.ram[CMD_BYTE] |= 0x40;
        assert!(
            !pif.boot_command(),
            "a matching checksum lets IPL2 reach IPL3"
        );
        assert_eq!(pif.ram[CMD_BYTE] & 0x40, 0, "run bit cleared");
    }

    #[test]
    fn boot_run_halts_on_a_wrong_checksum() {
        // Mutation check: a checksum that differs from the CIC's must NMI-halt —
        // if this passed with a matching one the guard would be vacuous.
        let mut pif = booting_pif(SUM_6102);
        pif.ram[0x32..0x38].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01]);
        pif.ram[CMD_BYTE] = 0x20;
        assert!(!pif.boot_command());
        pif.ram[CMD_BYTE] |= 0x40;
        assert!(
            pif.boot_command(),
            "a wrong checksum freezes the CPU via NMI"
        );
    }

    #[test]
    fn boot_command_is_inert_without_a_boot_rom() {
        // Under HLE (no boot ROM) the reset-mode path must not touch the command
        // byte, so the run-mode joybus protocol is unaffected.
        let mut pif = Pif::new();
        pif.set_boot_checksum(SUM_6102);
        pif.ram[CMD_BYTE] = 0x20 | 0x40;
        assert!(!pif.boot_command(), "no boot ROM => inert");
        assert_eq!(
            pif.ram[CMD_BYTE],
            0x20 | 0x40,
            "command byte untouched under HLE"
        );
    }

    #[test]
    fn rom_lockout_makes_the_boot_rom_read_zero() {
        let mut pif = Pif::new();
        let mut dump = alloc::vec![0u8; PIF_ROM_LEN];
        dump[0] = 0x3C; // a non-zero byte 0 so the change is observable
        pif.load_boot_rom(&dump);
        pif.set_boot_checksum(SUM_6102);
        assert_eq!(pif.boot_rom_read(0), 0x3C, "readable before lockout");
        pif.ram[CMD_BYTE] = 0x10; // IPL2's ROM-lockout command
        assert!(!pif.boot_command());
        assert_eq!(pif.ram[CMD_BYTE] & 0x10, 0, "lockout bit cleared");
        assert_eq!(
            pif.boot_rom_read(0),
            0,
            "the PIF ROM is off the bus after lockout"
        );
    }

    #[test]
    fn a_warm_reset_unlocks_the_rom_so_ipl1_can_re_run() {
        let mut pif = Pif::new();
        let mut dump = alloc::vec![0u8; PIF_ROM_LEN];
        dump[0] = 0x3C;
        pif.load_boot_rom(&dump);
        pif.set_boot_checksum(SUM_6102);
        // Boot once: lock the ROM and latch a checksum.
        pif.ram[0x32..0x38].copy_from_slice(&SUM_6102);
        pif.ram[CMD_BYTE] = 0x10 | 0x20;
        assert!(!pif.boot_command());
        assert_eq!(pif.boot_rom_read(0), 0, "ROM locked after the first boot");
        // Warm reset: the PIF unlocks the ROM and drops the latch, so IPL1 reads
        // again — but keeps the installed ROM and the registered CIC checksum.
        pif.reset_boot();
        assert_eq!(pif.boot_rom_read(0), 0x3C, "ROM readable again after reset");
        assert!(
            pif.has_boot_rom(),
            "the installed boot ROM survives a warm reset"
        );
        // The latch is gone, so a fresh acquire compares the newly-written value.
        pif.ram[0x32..0x38].copy_from_slice(&SUM_6102);
        pif.ram[CMD_BYTE] = 0x20;
        assert!(!pif.boot_command());
        pif.ram[CMD_BYTE] |= 0x40;
        assert!(
            !pif.boot_command(),
            "the retained CIC checksum still matches"
        );
    }
}
