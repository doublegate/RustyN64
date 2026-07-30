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
- **The present path never takes the emu lock at all.** The producer publishes the
  frame and the status readings into `present_buffer::PresentBuffer` (a
  triple-buffer SPSC handoff with its own small mutex, held only for one RGBA8
  memcpy); the winit thread reads that. The only emu-lock takers on the UI thread
  are the menu actions that genuinely mutate the core (open / close / pause /
  reset), which happen on a click, not per frame. See *Present handoff* below for
  why this is not merely an optimization.
- **The emulator runs on a dedicated thread** (`emu-thread`) on native,
  communicating via an `Arc<Mutex<System>>` handle + a lock-free shared-input
  channel; the winit thread only does UI + present. This thread is a *frontend*
  construct — the core itself is single-timeline (ADR 0004).
- **The frontend owns rate control + run-ahead.** Dynamic rate control (a
  resampler stage feeding the lock-free audio ring) and run-ahead
  (snapshot-restore orchestration) live here, NEVER in the core synthesis — that
  is what keeps the determinism contract intact (ADR 0004, `docs/audio.md`).

## Present handoff and pacing

Both ported from `RustyNES`/`RustySNES` (same author, so the port is
license-clean). They existed there and were **missing here**: RustyN64 shipped the
`emu-thread` feature without the handoff that feature exists to enable.

### The defect

`App::snapshot` took the emu mutex every UI frame merely to clone the framebuffer,
while the emu thread held that same mutex across an entire emulated frame. Worse,
the pacer's fell-behind branch was `next = now` with **no sleep and no yield** —
and this core is ~6.5x slower than real time, so that branch was taken on every
iteration, leaving the emu thread holding the mutex ~100% of the time. Against an
unfair mutex the UI starved for many frames at a stretch: menu clicks took 15-45
seconds and roughly one frame was presented per 30-60 seconds.

### Measured

`emu_thread::tests::measure_ui_read_latency_through_the_handoff_versus_the_emu_mutex`
(an `#[ignore]`d stopwatch, not a gate) times a competing UI thread's read both
ways against a live emu thread. On the development machine, `--release`, 120
samples:

| UI-side read | p50 | p99 | max |
| --- | --- | --- | --- |
| via the handoff | **894 ns** | 163 µs | 163 µs |
| via the emu mutex (the old path) | **97.8 ms** | 113.6 ms | 118 ms |

The same run reported **60 snap-forwards over 60 produced frames** — the core is
behind on *every* iteration, which is the direct confirmation that the old
no-yield branch was the one always taken.

### The pacer

A bounded catch-up burst (`MAX_CATCHUP_FRAMES = 3`) then a **snap forward**
(re-base the schedule on `now` rather than replay the missed window), then
`block_until_native`: sleep in `SLEEP_CHUNK` (2 ms) capped naps until within
`SPIN_MARGIN` (2 ms), then `spin_loop` to the exact instant. The nap cap is
load-bearing — with one long sleep, a single OS oversleep blows past the target and
the precise spin never engages.

Video frames coalesce (the handoff keeps only the newest); **audio is never
dropped**, since every produced frame's samples are pushed to the ring.

**Known cost, not yet optimized.** Because the core is behind every iteration, the
snap imposes a full frame period of wait after each frame, so the effective rate is
`1 / (frame_cost + period)` rather than `1 / frame_cost` — about 8.1 FPS against a
~9.3 FPS core ceiling, ~13%. That is the ported behavior and it is what buys the UI
its window; whether a shorter yield is better is a question for the perf work, with
`perf.rs` data, rather than a constant to tune by feel.

Thread-priority elevation (`SCHED_RR` at a low priority, below the audio callback)
is deliberately **not** ported yet: it is the only `unsafe` in RustyNES's frontend,
and it should wait until measurement shows it is needed.

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
