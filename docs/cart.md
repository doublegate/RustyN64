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
- **Saves** — the backing store sized by `SaveType`; FlashRAM additionally needs
  a small command state machine (erase/write/status).

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

RustyN64 **stubs the boot** by default (load ROM, set the RDRAM/CPU state IPL3
would, apply the per-CIC entry-point adjustment, seed the PIF-RAM CIC-result byte
the game polls), with an optional real-IPL path later
(`ref-docs/research-report.md` §6, §Open questions 2).

### PIF + CIC lockout

Every cart carries a CIC-NUS chip; the PIF and CIC run a continuous seed/checksum
handshake and the PIF can **halt the CPU** if the check fails
(`ref-docs/research-report.md` §6). Variants and their effect:

| CIC (NTSC / PAL) | Notes |
|---|---|
| 6101 | early NTSC (Star Fox 64) |
| 6102 / 7101 | the common variant (~88% of games) |
| 6103 / 7103 | RAM entry point **+ `0x100000`** |
| 6105 / 7105 | different challenge protocol (X105 ramp) |
| 6106 / 7106 | RAM entry point **+ `0x200000`** |

The per-CIC entry-point offset must be applied when stubbing the boot.

### SI / controllers / PIF RAM

Controller polling and accessory I/O go through the 64-byte PIF RAM
(`ref-docs/research-report.md` §6): the CPU fills it with a per-port command block
(read controller, read/write Controller Pak, read/write EEPROM), triggers an SI
DMA (`SI_PIF_AD_RD64B`/`WR64B`), the PIF runs the joybus transactions, writes
results back, and the **SI interrupt** signals completion. The skeleton models the
four controller ports as `[u32; 4]` latched state on the Bus.

### Save backends

Four cart save technologies, **detected per-game** (the header has no reliable
save-type field — resolve via the per-game DB by serial/CRC). Access path matters
(`ref-docs/research-report.md` §6):

| Type | Size | Path |
|---|---|---|
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
