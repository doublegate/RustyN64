# Frontend — RustyN64

**References:** `ref-docs/research-report.md` §External dependencies (cpal / winit
/ egui); `crates/rustyn64-frontend/src/main.rs`; ADR 0004; `docs/architecture.md`;
the RustyNES `docs/frontend.md` (the shell this ports from).

This is a SPEC for the frontend agent's code, not the code itself. The binary is
`rustyn64` (in `rustyn64-frontend`).

## Purpose

The frontend is an always-on `winit + wgpu + cpal + egui` shell — NOT a bare
window. egui runs **every frame**, drawing a persistent menu bar + status bar +
tabbed Settings with toggleable debugger panels layered on top. It hosts the
emulator (`rustyn64-core::System`), presents the framebuffer, plays audio, and
owns everything non-deterministic (rate control, run-ahead, host resample) so the
core stays a pure deterministic timeline.

## The shell rules (ported from RustyNES)

- **egui runs every frame.** The shell draws menu bar (File / Emulation / Tools /
  View / Debug / Help) + status bar + tabbed Settings, with the toggleable
  CPU/RSP/RDP/memory debugger panels on top.
- **Never hold the emu lock inside the egui closure.** Menu interactions return a
  `MenuAction` value; `App::dispatch_menu_action` runs it *after* the egui pass.
  The hidden render branch copies the framebuffer under a brief lock, drops it,
  and renders/presents with the core unlocked.
- **The emulator runs on a dedicated thread** (`emu-thread`) on native,
  communicating via an `Arc<Mutex<System>>` handle + a lock-free shared-input
  channel; the winit thread only does UI + present. This thread is a *frontend*
  construct — the core itself is single-timeline (ADR 0004).
- **The frontend owns rate control + run-ahead.** Dynamic rate control (a
  resampler stage feeding the lock-free audio ring) and run-ahead
  (snapshot-restore orchestration) live here, NEVER in the core synthesis — that
  is what keeps the determinism contract intact (ADR 0004, `docs/audio.md`).

## N64-specific bits (what differs from the NES shell)

### Booting a ROM

`EmuCore::load_rom` does not just insert the cartridge — it **boots** it via the
core's retail boot (`rustyn64_core::boot::hle_boot`, ADR 0010): the HLE boot seeds
the state IPL3 expects, copies the cart's real IPL3 into RSP DMEM, and jumps in, so
the game actually runs. Without the boot the CPU sits at the PIF reset vector
`0xBFC0_0000` fetching zeros. The retail boot lives in the core precisely so the
frontend can share it with the harness; only the ELF direct-load (an n64-systemtest
test shortcut) stays in the harness. A commercial ROM boots and executes to game
code in RDRAM; reaching a rendered frame additionally needs the OS-boot runtime a
game programs (VI/RI/F3DEX), tracked as accuracy-ledger **R-18**.

### Framebuffer

The VI scans out **320×240 or 640×480** (NTSC), up to 32-bit color
(`docs/rdp.md`, `ref-docs/research-report.md` §4). The present path uploads the
post-VI-filter RGBA8888 frame to a wgpu texture; honor the VI's selected
resolution and AA per frame. PAL field timing differs (`docs/compatibility.md`).

### Controller map

The N64 controller (default P1 mapping; configurable):

| N64 input | Suggested default |
| --- | --- |
| Analog stick | left thumbstick (gamepad) / WASD or arrows (keyboard) |
| D-pad | gamepad d-pad / arrow keys |
| A / B | South / West (gamepad) / Z / X (keyboard) |
| C-buttons (C-up/down/left/right) | right thumbstick / I-J-K-L |
| Z (trigger) | left trigger / Space |
| L / R (shoulders) | bumpers / Q / E |
| Start | Start / Enter |

USB gamepads auto-bind to P1; up to four ports map to the Bus `controllers`
latch (`docs/cart.md` §SI). The analog stick is two signed axes; the C-buttons are
four discrete buttons (not a second stick on hardware).

### Debugger panels

CPU (VR4300 GPR/COP0/PC), RSP (SU/VU regs + DMEM/IMEM), RDP (command FIFO + TMEM),
and a memory viewer over RDRAM — the N64 analogs of the RustyNES debugger panels.

## Save-states / rewind / run-ahead

**Implemented** (frontend-side, ADR 0004). The whole `System` is serde-serializable
(the core just needs to be (de)serializable + deterministic); the frontend picks
the wire format (bincode) and owns all orchestration:

- **Save-states** — `EmuCore::snapshot`/`restore` (`bincode` over `System`). The
  cart ROM is `#[serde(skip)]`'d and re-attached on restore, so a blob is small
  (RDRAM-dominated) and valid alongside the same ROM.
- **Rewind** — `savestate::RewindRing`, a bounded ring that captures a snapshot
  every N frames (config `RewindConfig`, capacity-bounded); `Backspace` rewinds
  one step.
- **Run-ahead** — `savestate::RunAhead` runs the emulation a few frames ahead on
  the latest input, presents the speculative video frame, then restores to the
  committed point (speculative audio discarded so it is never heard twice).

The emu thread drives a `SaveStateCoordinator` (rewind capture + run-ahead +
save/load) each frame; with rewind off and run-ahead 0 (the defaults) it is a
plain `run_frame`, so output stays byte-identical. Hotkeys: **F2** save, **F4**
load, **Backspace** rewind.

## WebAssembly

A wasm browser entry point ships in `src/wasm.rs` (`#[wasm_bindgen(start)]`),
built with `trunk` from `web/index.html`. It is a **2D-canvas demo**, not the
full shell: it boots a committed license-clean homebrew ROM
(`render_fill.z64`), runs one emulated frame per `requestAnimationFrame`, and
blits the VI scan-out to a `<canvas>` through `web-sys`'s `ImageData`
(`EmuCore::frame_rgba`). This proves the LLE core runs and renders in a browser.

The full winit/wgpu/egui shell on wasm needs async wgpu init and the browser
audio/input/file-picker APIs; it is the roadmap for the in-browser shell (the
native host deps — `cpal`/`gilrs`/`rfd`/`directories` — are already gated to
`cfg(not(target_arch = "wasm32"))`, and `wgpu` carries the `webgl` feature for
that future). Host-specific non-determinism (resample, rate control) stays
frontend-side there too.

Build (`crates/rustyn64-frontend/web/`): `trunk build --release`. The
`wasm-bindgen` **library** version in `Cargo.lock` and the `wasm_bindgen` **CLI**
pin in `web/Trunk.toml` must be byte-identical (both `0.2.126`) — a mismatch
fails `trunk build` and the `wasm-bindgen-pin` CI job while wasm clippy still
passes.

## Edge cases and gotchas

- **The analog stick is not a d-pad.** Feed the real signed axis values; many N64
  games read the magnitude.
- **Don't run emulation inside the egui closure** — the lock discipline above is
  load-bearing for not stalling present.
- **Resolution changes mid-game.** The VI can switch 240p↔480i; the present path
  must handle a resolution change without a panic.
- **Audio resample is non-deterministic** — frontend only (ADR 0004).

## Open questions

- Default C-button mapping ergonomics (right stick vs face cluster) — confirm with
  user testing.
- 480i interlace handling in the present path (deinterlace vs bob).
