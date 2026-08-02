# 0019 — The GPU as the machine's rasterizer, gated on parity rather than on frame time

Status: **Accepted as an opt-in that is off by default, and BLOCKED from being
built until the accuracy gate below is met.** The gate is not ceremony: with it
unmet, this change makes the emulator *less* accurate.
Date: 2026-08-02
Deciders: repo owner
Supersedes: none · Superseded by: none
Extends: ADR 0014 (GPU-backed RDP), ADR 0015 (GPU determinism scope). Brings
**ADR 0004** (determinism) into scope for the GPU path, exactly as ADR 0015
predicted would happen if this were ever attempted.

## Context

Today the GPU backend is a **display** backend: it renders what the machine has
already produced, and the machine's own RDRAM comes from the software
rasterizer. A4 proposes the GPU write the framebuffer back into RDRAM instead,
retiring the software rasterizer from the render path.

### What it is worth

Measured in the configuration A4 would actually change
(`frame_bench --features fast-exec,fast-scheduler,gpu-rdp`, Super Mario 64), by
stubbing the three rasterizing dispatch arms to no-ops:

| basis | difference |
| --- | --- |
| conservative (worst B vs best A) | **1.23%**, 0.753 ms |
| clean-leg means | 3.16%, 1.946 ms |

Against today's ~31.6 ms frame the same absolute cost is **~2.5%**. And this is
an **upper bound A4 cannot reach**: deleting rasterization is strictly cheaper
than replacing it, because the GPU must still write its result back into RDRAM
every frame and the probe got that for free.

### The cost, which is not primarily a performance cost

1. **It is an accuracy regression today.** The 42/43 census grades parallel-rdp
   against *Angrylion*, not against RustyN64's software path, and the one known
   gap — `key_en` chroma-key alpha compare (#160) — is a case where **the GPU is
   less complete than the rasterizer it would replace**. Shipping this now trades
   correctness for ~2.5%, which is the trade this project exists to refuse.
2. **ADR 0004 comes into scope.** The determinism contract binds the core, and
   the core's framebuffer would begin arriving from a GPU.
3. **Timing changes.** The software RDP executes commands as the machine runs;
   the GPU renders at frame end. A game that reads its framebuffer mid-frame sees
   a different picture — a behavior change, not a rendering one.
4. **It pushes GPU-written memory into `rustyn64-core`**, a crate that is
   `#![no_std]` and `#![forbid(unsafe_code)]` by design.

## Decision

**Accept A4 as an opt-in, off by default — and gate building it on accuracy, not
on frame time.**

The gate, all of which must hold before the work starts:

1. **The GPU/Angrylion census reaches 43/43**, closing `key_en` (#160). Until
   then this change makes the emulator worse at its primary job, and the ~2.5% is
   not an argument against that, it is the thing being refused.
2. **A software-vs-GPU framebuffer differential** exists and passes on the
   committed test-ROM corpus — parity against *Angrylion* is not parity against
   the path being replaced, and only the latter is the relevant question here.
3. **ADR 0004's determinism contract is re-derived for the GPU path** and stated,
   not assumed: seed + ROM + input must still give bit-identical AV with the
   option enabled.
4. **The mid-frame read behavior is characterized** — which titles read their own
   framebuffer, and what they would see — rather than assumed absent.

**The justification is inverted on purpose.** This ADR does *not* accept A4 for
performance; ~2.5% does not pay for an ADR, a determinism re-derivation and a
known regression. It accepts it as an **accuracy** change — the software
rasterizer is itself incomplete, and a fully-parity GPU path is a better
rasterizer — with the frame time as a side effect. That reframing is what makes
gate (1) load-bearing instead of decorative.

## Consequences

**Gained, when the gates are met and the option is on:** under 2.5% of a frame,
and a rasterizer that is more complete than the software one — which is the
larger prize and the reason to do it at all.

**Given up:** nothing while off. When on: the software rasterizer stops being the
thing under test on that path, so CI must keep grading it separately or the
project loses its own oracle.

**The honest position, recorded so it is not re-litigated as a performance
item:** if someone revisits this looking for FPS, the answer is that it was
measured at under 1.23% in the configuration it changes, and the answer is no.
The only version of A4 worth building is the accuracy one.
