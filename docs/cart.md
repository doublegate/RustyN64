# PI cart + PIF/CIC boot + SI + saves — RustyN64

**References:** `ref-docs/research-report.md` §6 (boot/PIF/CIC/SI/saves), §2 (PI/SI
interfaces); `crates/rustyn64-cart/src/lib.rs`; `docs/cartridge-format.md`;
`docs/architecture.md`; `docs/compatibility.md`.

This doc is the SPEC, not history — update it in the same PR as the code.

## Purpose

This subsystem is the cartridge and I/O boundary: the **Peripheral Interface (PI)**
DMA path to the cart ROM and PI-bus saves; the **PIF + CIC** boot/lockout
handshake; the **Serial Interface (SI)** joybus path to controllers, Controller
Paks, and serial EEPROM. Board behaviour lives behind the `Cartridge` trait, not
in the CPU (`docs/architecture.md` fact 5).

## Interfaces

```rust
pub trait Cartridge {
    fn pi_read(&mut self, addr: u32) -> u8;                       // $1000_0000..
    fn pi_write(&mut self, addr: u32, val: u8);                   // saves/regs
    fn si_exchange(&mut self, channel: u8, tx: &[u8], rx: &mut [u8]); // joybus
    fn notify_cpu_cycle(&mut self);                               // counter HW
    fn save_type(&self) -> SaveType;
}

pub enum SaveType { None, Eeprom4k, Eeprom16k, Sram, FlashRam, ControllerPak }
pub enum Cic { Cic6101, Cic6102, Cic6103, Cic6105, Cic6106 }     // + PAL 71xx

pub struct Cart { /* rom, header, save backing */ }
impl Cart {
    pub fn load(raw: &[u8]) -> Result<Self, CartError>; // any byte order
    pub fn header(&self) -> &RomHeader;
    pub fn save(&self) -> &[u8];
    pub fn tick(&mut self); // step in-flight PI/SI DMA
}
```

The shared-RDRAM `RdramBus` trait (defined in this crate, used by the DMA paths
and the RDP) is documented in `docs/architecture.md` fact 2/3.

## State

- **PI** — `PI_DRAM_ADDR`, `PI_CART_ADDR`, `PI_RD_LEN`, `PI_WR_LEN`, `PI_STATUS`,
  the four DOM1/DOM2 bus-timing registers; an in-flight DMA's progress.
- **PIF RAM** — 64 bytes at the top of the PIF block; the command block the CPU
  fills and the SI DMA executes.
- **CIC** — the lockout variant + the seed/checksum handshake state.
- **Saves** — **implemented** (`crates/rustyn64-cart/src/save.rs`): the
  `SaveDevice` enum backs SRAM (flat 32 KiB, PI DOM2), FlashRAM (the real
  erase/program/status command machine over its CIR at `0x0801_0000`), EEPROM
  4k/16k (flat, joybus 8-byte blocks), and the Controller Pak (flat 32 KiB,
  joybus 32-byte blocks). SRAM/FlashRAM route through `Cart::pi_write`/`pi_read`
  (the Bus's direct-I/O + DMA paths); the joybus backends are driven by the SI/PIF
  module. Each round-trips byte-for-byte (unit tests).

## Behavior

### Boot (IPL stages)

Power-on runs a three-stage Initial Program Loader
(`ref-docs/research-report.md` §6):

1. **IPL1** — in the PIF-NUS internal boot ROM (`0x1FC0_0000`): brings up the CPU,
   the PI, and the RCP.
2. **IPL2** — runs in RSP memory; participates in validating the cart vs the CIC.
3. **IPL3** — the cart's own bootcode at ROM offset **`0x40`, length 4032 bytes**;
   it initializes RDRAM, checksums the first 1 MB, and jumps to the game entry
   point (executing from `0xA4000040`). The standard 6102/7101 bootcode covers
   ~88% of games.

RustyN64 has **two boot paths** (ADR 0009), both implemented:

- **HLE boot** (`rom::hle_boot`) — the default. Skips IPL1/IPL2, copies the cart's
  real IPL3 into DMEM, seeds the post-IPL3 CPU/COP0/PI-DOM1 state and the CIC seed
  word into PIF RAM `0x24`, and jumps to IPL3 at `0xA4000040`. Copyright-clean and
  CI-able; the seeds are cited constants (accuracy-ledger **C-32**).
- **Real-PIF boot** (`rom::real_pif_boot`) — faithful, **off by default, local-only,
  never CI-gated** (it needs the copyrighted PIF ROM, never committed). Installs the
  real IPL1/IPL2 at `0x1FC0_0000` and runs the CPU from the reset vector
  `0xBFC0_0000`: IPL1 → IPL2 (checksum-verified against the CIC) → the cart's IPL3.
  The PIF-SM5's boot behaviours (seed hand-off, ROM lockout, checksum acquire/run)
  are modelled behaviourally from `PIF-NUS.md`; the SM5 firmware is not run
  (accuracy-ledger **C-33**). Validated locally across 6102/6103/6105 CICs.

### PIF + CIC lockout

Every cart carries a CIC-NUS chip; the PIF and CIC run a continuous seed/checksum
handshake and the PIF can **halt the CPU** if the check fails
(`ref-docs/research-report.md` §6). The **CIC is identified from the cartridge
IPL3's CRC-32** (`Cic::from_ipl3`, cen64's fingerprint table) — the core reads only
the ROM's own boot code, never a per-game DB (ADR 0003/0004). Variants:

| CIC (NTSC / PAL) | IPL2 seed | Notes |
| --- | --- | --- |
| 6101 | `0x3F` | early NTSC (Star Fox 64); shares its seed with 7102 / iQue |
| 6102 / 7101 | `0x3F` | the common variant (~88% of games); the table fallback |
| 6103 / 7103 | `0x78` | RAM entry point **+ `0x100000`** |
| 6105 / 7105 | `0x91` | different **running** challenge (X105); boot checksum is standard |
| 6106 / 7106 | `0x85` | RAM entry point **+ `0x200000`** |

On the real-PIF path the PIF holds the CIC's 6-byte IPL2 checksum
(`Cic::boot_secrets`, N64brew *PIF-NUS* table) and adjudicates IPL2's verify command
(`Pif::boot_command`): a genuine match lets IPL2 reach IPL3; a mismatch freezes the
CPU via NMI (`Bus::boot_nmi_halt`). The seeds use the **corrected** per-CIC IPL2-seed
byte (not cen64's legacy all-`0x3F`), because the real IPL2 consumes it.

### SI / controllers / PIF RAM

Controller polling and accessory I/O go through the 64-byte PIF RAM
(`ref-docs/research-report.md` §6): the CPU fills it with a per-port command block
(read controller, read/write Controller Pak, read/write EEPROM), triggers an SI
DMA (`SI_PIF_AD_RD64B`/`WR64B`), the PIF runs the joybus transactions, writes
results back, and the **SI interrupt** signals completion. **Implemented**
(`crates/rustyn64-cart/src/pif.rs` + the Bus SI block at `0x0480_0000`): the joybus
frame parser runs `0x00`/`0xFF` info, `0x01` controller state (from the Bus's four
packed `[u32; 4]` port words), `0x02`/`0x03` Controller-Pak accessory access (with
the data CRC8), and `0x04`/`0x05` EEPROM blocks. `SI_PIF_AD_WR64B` DMAs the frame
in; `SI_PIF_AD_RD64B` executes it and DMAs the replies out, raising `MI_INTR.si`.

### Save backends

Four cart save technologies, **detected per-game** (the header has no reliable
save-type field — resolve via the per-game DB by serial/CRC). Access path matters
(`ref-docs/research-report.md` §6):

| Type | Size | Path |
| --- | --- | --- |
| EEPROM 4kbit | 512 B | joybus via PIF/SI |
| EEPROM 16kbit | 2 KiB | joybus via PIF/SI |
| SRAM | 32 KiB (some 96 KiB) | PI bus (DOM2), needs battery |
| FlashRAM | 128 KiB | PI bus (DOM2), command-driven |
| Controller Pak | 32 KiB | joybus via PIF/SI, external card |

EEPROM may coexist with SRAM **or** FlashRAM, but SRAM and FlashRAM cannot
coexist. EEPROM/Controller-Pak are joybus (SI/PIF) devices; SRAM/FlashRAM are
PI-bus DOM2 devices; FlashRAM models its erase/write/status command machine.

## Edge cases and gotchas

- **Save type is DB-resolved, not header-read.** Unlike the iNES mapper byte,
  there is no in-header save field — key off the cart serial / CRC
  (`crates/rustyn64-cart/src/lib.rs`, `ref-docs/research-report.md` §6).
- **Apply the CIC entry-point offset** (6103 +`0x100000`, 6106 +`0x200000`) when
  HLE-booting, or the game jumps to the wrong PC.
- **DMA is not instantaneous.** PI/SI DMA completion timing is what game code
  busy-waits on; schedule the completion interrupt at a future cycle derived from
  the byte count (`docs/scheduler.md` event model; `ref-docs/research-report.md`
  §challenge 5).
- **FlashRAM is a state machine.** It is command-driven (erase/write/status), not
  a flat memory; a flat backing store mis-saves.
- **SRAM batteries die.** Not an emulation concern, but the save file is the
  battery — persist it on the host.
- **Byte order is sniffed, not extension-trusted.** See `docs/cartridge-format.md`.

## Test plan

- **ROM-format round-trip** — `.z64`/`.n64`/`.v64` detect + normalize to
  big-endian (already unit-tested in the crate).
- **Header parse** — title / game code extraction; short-header error.
- **Save round-trip oracle** — write a save via the game path, reload, assert
  byte-identical (the RustyNES battery-save oracle analog), per `SaveType`.
- **SaveTest-N64 + n64-systemtest PIF/SI categories** — joybus + multi-width PIF
  access (`ref-docs/research-report.md` §6, §7).
- **Boot stub** — a stubbed IPL3 reaches the game entry PC for each CIC variant.

## Open questions

- **Stub vs real PIF/IPL** — which (if any) commercial titles depend on real-PIF
  timing (`ref-docs/research-report.md` §Open questions 2).
- **Per-game DB source** — micro-64's CIC + save lists are the ground truth; how
  to vendor/refresh them (`ref-docs/research-report.md` §6 sources).
