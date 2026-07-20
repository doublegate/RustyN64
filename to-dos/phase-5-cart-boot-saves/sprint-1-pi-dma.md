# Sprint 1 — The PI bus, DMA, and cart addressing

**Phase:** Phase 5 — Cart boot + saves
**Sprint goal:** the cartridge is readable at the right addresses with the right timing, so ROM
code reaches the CPU the way hardware delivers it.
**Estimated duration:** 2 weeks

## Tickets

### T-51-001 — The PI register set

**Description:** implement `PI_DRAM_ADDR`, `PI_CART_ADDR`, `PI_RD_LEN`, `PI_WR_LEN`, and
`PI_STATUS`, including the status bits that report and clear DMA state.

**Acceptance criteria:**

- [ ] Every register reads and writes with hardware semantics.
- [ ] `PI_STATUS` reports busy and IO-busy, and its write-side reset and interrupt-clear bits
      work.
- [ ] Writing a length register starts a transfer, as hardware does.

**Dependencies:** T-11-003
**Reference:** `n64brew_wiki/markdown/Peripheral Interface.md` §Registers
**Estimated complexity:** M

---

### T-51-002 — PI DMA in both directions

**Description:** implement cart-to-RDRAM and RDRAM-to-cart transfers, honouring the length
semantics and the alignment rules, and raising the PI interrupt on completion.

**Acceptance criteria:**

- [ ] Both directions transfer the correct bytes for every legal length.
- [ ] The length-plus-one and alignment rules match hardware, including the misaligned cases.
- [ ] The PI interrupt fires on completion and drives the MI.
- [ ] Transfers occupy realistic time, so code that polls rather than waits behaves correctly.

**Dependencies:** T-51-001
**Reference:** `n64brew_wiki/markdown/Peripheral Interface.md` §The PI Bus
**Estimated complexity:** L

---

### T-51-003 — Domain timing and open-bus behaviour

**Description:** implement the per-domain `PI_BSD_DOMn_LAT`, `PWD`, `PGS`, and `RLS` timing
registers so they affect transfer duration, and reproduce open-bus behaviour on unmapped reads.

**Acceptance criteria:**

- [ ] The timing registers are stored *and* affect DMA duration, not merely readable.
- [ ] Domain 1 and domain 2 are addressed separately with their own timing.
- [ ] Reads outside mapped cart space return the open-bus value hardware returns, not zero.
- [ ] n64-systemtest's ROM access category passes.

**Dependencies:** T-51-002
**Reference:** `n64brew_wiki/markdown/Peripheral Interface.md` §Domains, §Open bus behavior
**Estimated complexity:** M

---

### T-51-004 — ROM mapping and the stub boot path

**Description:** map the cartridge into the PI address space and implement the documented IPL3
stub boot so a ROM can begin executing without the full CIC handshake, which lands in Sprint 2.

**Acceptance criteria:**

- [ ] The ROM appears at the correct physical address, with the header where code expects it.
- [ ] The stub boot seeds the state IPL3 would leave and jumps to the ROM entry point.
- [ ] The stub path is documented as a deliberate approximation, with the real-IPL option
      recorded as the alternative.
- [ ] A homebrew ROM runs from a cold start through this path.

**Dependencies:** T-51-003
**Reference:** `docs/cart.md`; `n64brew_wiki/markdown/Bootcode.md`
**Estimated complexity:** L

---

## Sprint review checklist

- [ ] All tickets checked off or explicitly deferred (with reason).
- [ ] A homebrew ROM boots and runs through the real PI path.
- [ ] CHANGELOG.md updated.
- [ ] `docs/cart.md` updated in the same change as the code it describes.
