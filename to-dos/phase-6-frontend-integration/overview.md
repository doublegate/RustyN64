# Phase 6 — Frontend integration

## Goal

Wire the always-on egui shell to the real machine: the VI scan-out becomes the presented frame,
the AI drain becomes audible audio, and the SI joybus becomes controller input. Then add the
frontend-side conveniences that the determinism contract explicitly assigns to the frontend —
save-states, rewind, run-ahead — and give the wasm build a browser entry point so it is a demo
rather than a compilation target.

## Exit criteria

- [ ] The presented frame is the real VI scan-out, not a test pattern, at the correct aspect
      ratio with overscan handling.
- [ ] Audio is the real AI drain through the host ring, with dynamic rate control keeping it fed
      and no underrun during normal play.
- [ ] Controller input reaches the game through the SI joybus, with keyboard and gamepad both
      mapped and hot-plug handled.
- [ ] Save-states round-trip: every piece of machine state, including the scheduler's phase
      accumulator and any in-flight DMA, restores to a run that continues bit-identically.
- [ ] Rewind works off a bounded ring of snapshots, and run-ahead hides input lag, both
      config-driven and off by default.
- [ ] The wasm build has a browser entry point: a `wasm-bindgen` dependency, a
      `#[wasm_bindgen(start)]` entry, an `index.html`, and a `trunk build` that produces a
      working demo. (Today the crate compiles for `wasm32` but none of that exists.)
- [ ] The `Trunk.toml` wasm-bindgen pin still matches `Cargo.lock` — the `wasm-bindgen-pin` CI
      job already guards this.
- [ ] The determinism contract holds with all of the above on: rate control, pacing, and
      run-ahead live in the frontend and cannot influence core output (ADR 0004).

## Scope

In-scope:

- Wiring the shell to the real scan-out, audio drain, and input.
- Save-states, rewind, run-ahead, and the snapshot envelope they share.
- Audio pacing and dynamic rate control.
- The wasm browser entry point and the hosted demo.
- Input configuration and gamepad support.

Out-of-scope:

- Netplay, RetroAchievements, TAS tooling, Lua, and shaders — all Phase 8, all additive and
  off by default.
- Any change to the core to make the frontend easier. If the frontend needs something, it takes
  it from the existing snapshot and presentation surfaces.

## Sprints

- [Sprint 1 — Wiring scan-out, audio, and input](sprint-1-shell-wiring.md) —
  the shell stops showing a test pattern and starts showing the machine.
- Sprint 2 — Save-states, rewind, and run-ahead.
  **Status:** stub — refine when Sprint 1 is close to complete.
- Sprint 3 — The wasm browser entry point and the hosted demo.
  **Status:** stub — refine when Sprint 2 is close to complete.

## Dependencies

Phases 3, 4, and 5: a frame to present, samples to play, and input to deliver. The shell,
the `MenuAction` dispatch, and the wgpu blit already exist from Phase 0.

## Risks

- **Save-states are where hidden state surfaces** — anything not in the snapshot is a silent
  divergence, and it shows up minutes later, far from the cause. Mitigated by ADR 0004's rule
  that new hidden state must be reachable by the serialiser, and by testing restore with a
  two-run trace comparison rather than by eye.
- **The frontend is the natural place to break determinism** — pacing, rate control, and
  run-ahead all want to consult the clock. Mitigated by the hard split: the core never learns
  what time it is.
- **The wasm gap is larger than it looks** — "it compiles for wasm32" is not "it runs in a
  browser", and the missing pieces are dependency, entry point, and host page. Mitigated by
  treating them as an explicit sprint rather than a build-flag change.
- **egui can stall emulation** — holding the emulator lock inside the egui closure would couple
  UI latency to emulation cadence. Mitigated by the existing rule that menu interactions return
  a `MenuAction` dispatched after the egui pass.

## Reference docs

- [docs/frontend.md](../../docs/frontend.md) — the shell, the ring, and pacing.
- [docs/adr/0004-determinism-contract.md](../../docs/adr/0004-determinism-contract.md)
- [docs/architecture.md](../../docs/architecture.md) — fact 7, the always-on shell.
- `n64brew_wiki/markdown/Video Interface.md` — what scan-out actually presents.
