# 0018 — Asynchronous GPU RDP presentation, one frame late

Status: **Accepted**, as an opt-in that is **off by default**.
Date: 2026-08-02
Deciders: repo owner
Supersedes: none · Superseded by: none
Extends: ADR 0014 (GPU-backed RDP) and ADR 0015 (GPU determinism scope). It
contradicts neither; §*No GPU-to-CPU tracker* below carries ADR 0015's own
reasoning one step further.

## Context

`present` stalls the emulation thread on `scanout_sync`'s fence, waiting for the
GPU to finish rasterizing the frame it just submitted. Measured on
`frame_bench --features gpu-rdp,fast-exec,fast-scheduler`, Super Mario 64:

| | ms/frame |
| --- | --- |
| wait for RDP rasterization | ~1.06 |
| VI pass + read-back + host copies | ~1.39 |

**1.06 ms is what this ADR is about.** It was 1.7% of the 63.2 ms frame it was
measured against. That frame is now ~31.6 ms, so the same absolute cost is
**~3.5%** — the item did not get better, the rest of the emulator got faster
around it (`docs/performance.md` §*The declined backlog, re-derived*).

### Two shapes, and only one is available

**(a) Submit commands as the frame runs.** The GPU would be busy during
emulation and the fence already signaled at present time — the full win, no shim
work. **Unavailable.** `present` stages RDRAM *before* enqueueing any command, so
every command in a frame currently executes against **end-of-frame RDRAM**.
Submitting mid-frame requires staging mid-frame, which changes *which* memory
contents each command reads. That is a change to the presented picture, not a
scheduling change, and it would force the dirty-page map (#245) to clear and
re-accumulate per sub-frame.

**(b) Do not block; present one frame late.** Submit at present time exactly as
now, signal the timeline, and read back the *previous* frame's result. Every
command still sees end-of-frame RDRAM, so the picture is unchanged; the wait
simply moves off the critical path.

### The hazard (b) introduces, which is new and is not the one ADR 0014 names

ADR 0014 §6 calls for a GPU-to-CPU hazard tracker. That does not apply here: this
backend **owns its RDRAM** (ADR 0015), so there is nothing for the CPU to race
against, and no tracker is needed.

But removing the CPU-side wait does not remove ordering *between GPU
submissions*. Under (b), frame N's commands are still executing when frame N+1's
dirty-page stage begins writing the backend's RDRAM — **the same buffer the
in-flight submission reads**. The synchronous path cannot have this, because
`scanout_sync` drains everything before the next `present` stages anything.

## Decision

**Implement shape (b), behind a runtime option that is off by default.**

1. Expose `signal_timeline` and `wait_for_timeline` through the parallel-rdp
   shim. This is new C++ surface and lands under ADR 0014's existing `unsafe`
   quarantine in `rustyn64-rdp-gpu`; no other crate's `forbid(unsafe_code)` moves.
2. `present` submits and signals rather than draining, and reads back frame
   N−1's result.
3. **The GPU-to-GPU hazard is resolved by waiting on frame N's timeline value
   before staging frame N+1**, not by assuming the driver orders it. That wait is
   off the emulation critical path — it happens at the *next* present, by which
   time the GPU has had a full frame — which is the whole point.
4. The option is **off by default**, so the default build and the default run are
   behaviorally unchanged (ADR 0011 §1's discipline, applied to a backend).

### Why off by default, given it is a win

Because its cost is **one frame of presentation latency**, and that is a real
cost to a person playing a game, not an accounting entry. ~16.7 ms at 60 Hz and
more below it. A user should choose that trade explicitly; ~3.5% is not enough to
make it for them.

## Consequences

**Gained:** ~1.06 ms/frame, ~3.5% of today's frame, when enabled.

**Given up:** one frame of latency when enabled. Nothing when not.

**Determinism is unaffected.** The emitted picture is identical — every command
still executes against end-of-frame RDRAM — so ADR 0015's reproducibility gate
applies unchanged and must stay green with the option both on and off.

**A new failure mode exists and must be tested, not argued.** If the timeline
wait in (3) is wrong, frame N+1's stage corrupts frame N's read — and the symptom
is an *occasional* wrong frame under load, which is exactly the kind of defect a
single screenshot comparison passes. The gate is the ADR 0015 reproducibility
run with the option on, repeated, not one frame compared once.

**What this does not authorize.** Shape (a). Submitting mid-frame changes what
the GPU reads and would need its own ADR and its own determinism argument.
