# RustyN64 — Roadmap

Entry point for planning. Each phase links its overview; phases contain sprints; sprints
contain tickets with stable IDs `T-PS-NNN`. Reference them in commit messages.

- **Phase 0** — foundation: workspace + crate skeletons compiling; CI green on stubs  → `to-dos/phase-0-foundation/overview.md`
- **Phase 1** — cpu golden log: the VR4300 (MIPS R4300i) interpreter to 0-diff against the golden log  → `to-dos/phase-1-cpu-golden-log/overview.md`
- **Phase 2** — rsp lle: the LLE RSP (the MIPS+SIMD vector coprocessor running game microcode) under master-clock lockstep  → `to-dos/phase-2-rsp-lle/overview.md`
- **Phase 3** — rdp lle + vi: the LLE RDP rasterizer + the VI scanout to a stable rendered frame  → `to-dos/phase-3-rdp-lle-vi/overview.md`
- **Phase 4** — ai audio: the AI (Audio Interface) fed by RSP audio microcode + the band-limited mixer  → `to-dos/phase-4-ai-audio/overview.md`
- **Phase 5** — cart boot + saves: the PI cart + PIF/CIC boot handshake + save backends (EEPROM/SRAM/FlashRAM)  → `to-dos/phase-5-cart-boot-saves/overview.md`
- **Phase 6** — frontend integration: wire the egui shell to the real scanout + audio + SI joybus input; wasm + save-states/rewind/run-ahead  → `to-dos/phase-6-frontend-integration/overview.md`
- **Phase 7** — accuracy breadth: drive the accuracy battery to target across the game corpus; region timing as data; defer hard residuals  → `to-dos/phase-7-accuracy-breadth/overview.md`
- **Phase 8** — reach: netplay / RA / TAS / Lua / shaders — additive, off-by-default  → `to-dos/phase-8-reach/overview.md`

- **v1.0.0** — production cut (all of the above; README/CHANGELOG/docs/STATUS in sync;
  release matrix + Pages green).
- **Beyond v1.0** — the fractional-timebase refactor (ADR 0002), *only if* hard residuals
  warrant it. The one release expected to break byte-identity.
