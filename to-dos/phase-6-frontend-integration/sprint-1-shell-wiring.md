# Sprint 1 — Wiring scan-out, audio, and input

**Phase:** Phase 6 — Frontend integration
**Sprint goal:** the shell stops presenting a test pattern and starts presenting the machine —
real frames, real audio, real controller input — without giving the core any way to learn what
time it is.
**Estimated duration:** 2 weeks

## Tickets

### T-61-001 — Present the real VI scan-out

**Description:** replace the test-pattern framebuffer with the VI scan-out, blitted through the
existing wgpu fullscreen-triangle pass with aspect correction and overscan handling.

**Acceptance criteria:**

- [ ] The presented frame is the VI output, at the correct geometry for the current mode.
- [ ] Aspect correction and optional overscan cropping both work.
- [ ] Resolution changes mid-run are handled without a stall or a leak.
- [ ] The presented image is the *pre-shader* core framebuffer, so any later filter cannot affect
      the golden corpus.

**Dependencies:** T-31-004
**Reference:** `docs/frontend.md`; `docs/rdp.md` §VI
**Estimated complexity:** M

---

### T-61-002 — Drain real audio to the host ring

**Description:** feed the AI's drained samples into the lock-free ring and out to the host device,
with dynamic rate control absorbing host clock drift.

**Acceptance criteria:**

- [ ] Audio is the real AI stream, not silence or a tone.
- [ ] Dynamic rate control keeps the ring fed without pitch artefacts.
- [ ] Underrun is surfaced in the status bar rather than silently concealed.
- [ ] All pacing lives in the frontend; the core emits on the emulated timeline only.

**Dependencies:** T-41-004
**Reference:** `docs/frontend.md` §audio; `docs/adr/0004-determinism-contract.md`
**Estimated complexity:** M

---

### T-61-003 — Route controller input through the SI joybus

**Description:** map keyboard and gamepad input into the joybus controller state the SI serves,
so input reaches the game the way hardware delivers it rather than through a side channel.

**Acceptance criteria:**

- [ ] The existing keymap and gamepad input reach the game through the SI, not a shortcut.
- [ ] The analog stick range and deadzone match a real controller closely enough that games
      calibrate correctly.
- [ ] Controller hot-plug is reflected in the status byte.
- [ ] Input is latched at a fixed point in the frame so a replay reproduces it exactly.

**Dependencies:** T-51-004, and Phase 5 Sprint 2 for the joybus itself
**Reference:** `docs/frontend.md` §input; `n64brew_wiki/markdown/Joybus protocol.md`
**Estimated complexity:** M

---

### T-61-004 — The emulation thread under load

**Description:** confirm the default-on `emu-thread` path holds up once the emulator is doing
real work, with the shell never holding the emulator lock inside the egui closure.

**Acceptance criteria:**

- [ ] Emulation runs off the winit thread; UI stalls do not disturb emulation cadence.
- [ ] Menu interactions still return a `MenuAction` dispatched after the egui pass.
- [ ] Output is byte-identical with the feature on and off, proving thread timing cannot leak
      into the core.

**Dependencies:** T-61-002
**Reference:** `docs/architecture.md` fact 7; `docs/frontend.md`
**Estimated complexity:** M

---

## Sprint review checklist

- [ ] All tickets checked off or explicitly deferred (with reason).
- [ ] A real ROM is playable: picture, sound, and control.
- [ ] CHANGELOG.md updated.
- [ ] `docs/frontend.md` updated in the same change as the code it describes.
