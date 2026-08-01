# 0015 — Amend 0014: the GPU backend's determinism claim, and why dirty-region sync is not what unlocks it

Status: **Proposed** — accepted on merge of the PR that introduces this file;
immutable thereafter.
Date: 2026-08-01
Deciders: repo owner
Supersedes: none · Superseded by: none
Amends: [ADR 0014](0014-gpu-backed-rdp.md) (GPU-backed RDP) — one premise
corrected and one claim added. 0014's decision stands in full.

## Context

ADR 0014 §5 named synchronization as the dangerous part of a GPU RDP, and said
so in terms that were right about the general problem:

> A missing dirty-region sync is a *race*: a wrong pixel occasionally, on some
> machines, invisible to every deterministic gate.

It then made a determinism claim conditional on that work: no ADR 0004 claim
until dirty-region synchronization exists. `docs/rdp.md` and PRs #241–#243
repeated it.

**That conditional is wrong for the architecture that was actually built**, and
the error is worth recording because it was inherited unexamined through three
PRs. Dirty-region tracking synchronizes *asynchronous GPU writes into memory the
CPU also reads*. The integration in #243 has none:

- **The backend owns its RDRAM.** `GpuRdp` allocates its own buffer (#242); the
  Bus's RDRAM is **snapshotted into it** each frame and never shared. The GPU
  cannot write anything the machine reads.
- **The path is fully synchronous.** `CommandProcessor::scanout_sync` calls
  `scanout.fence->wait()` before its read-back (`rdp_device.cpp`), so no GPU work
  outlives the `present` that submitted it.
- **The software rasterizer still produces the machine's framebuffer.** The GPU
  output goes to the screen and nowhere else.

So there is nothing for a dirty-region tracker to synchronize. It would remain a
legitimate *upload* optimization — sending only the RDRAM pages that changed
rather than all 8 MiB — but that is a throughput change, and the throughput is
already at parity with the software scan-out (0.72–0.93 ms against 0.75 ms).

What was genuinely unestablished was much simpler and had never been measured:
**is parallel-rdp's output reproducible at all?**

## Decision

State the claim at the scope that is verified, and no wider.

### Verified

**On a single device and driver, the GPU path is bit-reproducible.**
`crates/rustyn64-test-harness/tests/gpu_determinism.rs` asserts two properties
that fail for different reasons:

1. **Independent devices agree.** All 43 `.rvec` vectors, replayed through three
   independently created contexts, hash identically. A failure would mean device
   creation leaks into the output.
2. **A stateful frame sequence agrees.** One backend rendering 60 successive
   frames — reusing the device, because TMEM, tile descriptors and combiner state
   legitimately persist between frames on hardware — reproduces exactly, across
   two runs, with 36 distinct frames in the sequence. This is where
   order-dependent or host-timing-dependent state would show; a per-frame-fresh
   context would test a machine that does not exist.

Both are mutation-checked: perturbing the digest by a per-run counter fails both.

Corroborating, and the reason the result is unsurprising rather than lucky:
parallel-rdp's dither/combiner noise is seeded from `(x, y, primitive_offset)`
(`shaders/noise.h`), with no clock, frame counter, or entropy — the same
positional scheme that lets it reproduce Angrylion byte-for-byte at all.

### Not claimed

- **Cross-vendor or cross-driver bit-exactness.** Every measurement here is on
  one NVIDIA GPU. parallel-rdp is fixed-point integer compute, so there is reason
  to expect portability, but reason-to-expect is not evidence and this project
  does not have the hardware to produce it.
- **That the GPU and software paths present the same picture.** They do not —
  parallel-rdp scans out the whole VI raster, `Bus::scanout_scaled` crops to the
  active span. The **backend is part of the output's identity**, exactly as the
  mode is for `fast-scheduler` (ADR 0011) and `fast-exec` (ADR 0013).
- **Reproducibility across a runtime fallback.** A host with no usable device
  presents the software picture; a mid-session backend failure discards the
  device and, if it cannot be rebuilt, switches to software for the rest of the
  run. Both are host-dependent, and neither is silent —
  `EmuCore::gpu_frames()` fails to advance on any frame the GPU did not produce,
  which is the observable that distinguishes them.

### ADR 0004 is untouched

0004's contract is *same seed + ROM + input ⇒ bit-identical framebuffer +
audio*, and it binds the **core**. The core is unchanged: the software
rasterizer still writes the machine's framebuffer, the AI still produces the
audio, and a save-state still captures both. This ADR adds a claim about what is
**presented**, which 0004 never covered, and weakens nothing it does.

## Consequences

### Positive

- The determinism question is settled by measurement instead of deferred behind
  work that would not have answered it.
- The gate is reusable: a future backend change that introduces a per-run seed,
  a wall-clock dependency, or cross-frame state contamination fails it.
- The scope statement is falsifiable. "Reproducible on one driver" can be
  disproved by anyone with a second GPU, which "deterministic" could not.

### Negative / costs

- The strongest form of the claim — bit-identical output on any conforming
  Vulkan device — remains unverified, and this ADR makes that visible rather
  than resolving it.
- The runtime fallback means a user's presented output depends on their
  hardware. That is the price of not failing outright on a machine without
  Vulkan, and it is a deliberate trade rather than an oversight.

### When dirty-region sync becomes required

If the GPU backend is ever promoted from a display path to the machine's
rasterizer — writing back into the Bus's RDRAM so the CPU can read what it drew —
then 0014 §5's warning applies in full and this ADR's reasoning expires with it.
That promotion is currently prevented by the crate graph: `rustyn64-core` is
`#![no_std]` and `#![forbid(unsafe_code)]`, so the Bus cannot own a Vulkan
device. Anyone changing that should re-read 0014 §5 before touching
synchronization.
