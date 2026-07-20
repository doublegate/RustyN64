# Phase 5 — Cart boot + saves

## Goal

`rustyn64-cart` gains the pieces that make a commercial cartridge actually boot and persist: the
PI bus and its DMA, the PIF/CIC boot handshake, the SI joybus for controllers and accessories,
and all four save backends. This is the phase after which the commercial corpus staged in Phase
0 becomes testable, because until the CIC handshake completes a retail ROM will not run at all.

## Exit criteria

- [ ] The PI registers behave: `PI_DRAM_ADDR`, `PI_CART_ADDR`, `PI_RD_LEN`, `PI_WR_LEN`,
      `PI_STATUS`, and the per-domain `PI_BSD_DOMn_LAT`/`PWD`/`PGS`/`RLS` timing registers.
- [ ] PI DMA is correct for both directions, including the alignment rules and the transfer
      length semantics, with the domain timing affecting duration.
- [ ] Open-bus behaviour on unmapped PI reads matches hardware.
- [ ] The SI registers behave: `SI_DRAM_ADDR`, `SI_PIF_AD_RD64B`, `SI_PIF_AD_WR4B`,
      `SI_PIF_AD_WR64B`, `SI_PIF_AD_RD4B`, and `SI_STATUS`.
- [ ] The joybus protocol is implemented: `0xFF` reset/info, `0x00` info, `0x01` controller
      state, plus the Controller Pak read/write commands, over the 64-byte PIF RAM exchange.
- [ ] The controller status byte reports plugged/changed state and the accessory presence bits
      correctly.
- [ ] The CIC handshake completes for the common variants (6101/6102/6103/6105/6106) with the
      seed reaching the expected PIF RAM location.
- [ ] Boot works via the documented IPL3 stub, with an optional real-IPL mode behind a flag —
      the open question resolved and written down either way.
- [ ] All four save backends round-trip a write and reload: EEPROM 4k, EEPROM 16k, SRAM, and
      FlashRAM, the last including its command/status state machine.
- [ ] The Controller Pak (Memory Pak) round-trips, including its checksum handling.
- [ ] A commercial ROM from each save-type folder boots to its title screen and saves.

## Scope

In-scope:

- The PI bus, its DMA, and the domain timing.
- The SI, PIF RAM, and the joybus command set.
- The CIC variants and the boot seeding.
- The four cart save backends plus the Controller Pak.
- The per-game save-type resolution already staged as data.

Out-of-scope:

- The Transfer Pak and the VRU: both are staged in the corpus (Pokemon Stadium, Hey You Pikachu)
  but are Phase 7 breadth work.
- The 64DD (its own peripheral and disk format).
- Rumble Pak force feedback as a *host* feature — the joybus side lands here, the frontend side
  is Phase 6.

## Sprints

- [Sprint 1 — The PI bus, DMA, and cart addressing](sprint-1-pi-dma.md) —
  the cartridge is readable and the ROM is where the CPU expects it.
- Sprint 2 — SI, PIF, joybus, and the CIC boot handshake.
  **Status:** stub — refine when Sprint 1 is close to complete.
- Sprint 3 — The four save backends and the Controller Pak.
  **Status:** stub — refine when Sprint 2 is close to complete.

## Dependencies

Phase 1 for the CPU that drives every register here. Phases 3 and 4 are not strictly required —
a ROM can boot without a picture — but in practice a title screen is how boot success is judged,
so this phase is far easier to verify after Phase 3.

## Risks

- **Boot is all-or-nothing** — a CIC handshake that is subtly wrong does not degrade, it simply
  fails to boot, and there is no partial credit to debug from. Mitigated by the stub-IPL3 path
  landing first so ROM execution can be verified independently of the handshake.
- **FlashRAM is a state machine, not a buffer** — treating it as memory appears to work until a
  game issues an erase or status sequence. Mitigated by testing against the FlashRAM titles
  already staged (Paper Mario, Majora's Mask, Pokemon Stadium) rather than synthetic writes.
- **Save-type misdetection corrupts saves silently** — writing SRAM semantics into an EEPROM
  game destroys the file without an error. Mitigated by the per-game database resolution already
  used to organise the corpus, with the heuristic fallback logged loudly.
- **PI domain timing looks ignorable** — it affects DMA duration, which affects the games that
  poll rather than wait for the interrupt. Mitigated by implementing the timing registers as
  timing, not as storage.

## Reference docs

- [docs/cart.md](../../docs/cart.md) — the PI, saves, and CIC spec.
- [docs/cartridge-format.md](../../docs/cartridge-format.md) — the ROM header and formats.
- [docs/compatibility.md](../../docs/compatibility.md) — save and CIC per game.
- [docs/adr/0003-no-board-tiering-honesty-gate.md](../../docs/adr/0003-no-board-tiering-honesty-gate.md)
- `n64brew_wiki/markdown/Peripheral Interface.md` — the PI registers, domains, open bus.
- `n64brew_wiki/markdown/Serial Interface.md`, `PIF.md`, `CIC-NUS.md`, `Joybus protocol.md`
- `tests/roms/external/commercial/README.md` — the per-save-type regression corpus.
