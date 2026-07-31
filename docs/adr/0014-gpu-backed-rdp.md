# 0014 — A GPU-backed RDP by binding parallel-rdp, not by porting it

Status: **Proposed** — accepted on merge of the PR that introduces this file;
immutable thereafter.
Date: 2026-07-31
Deciders: repo owner
Supersedes: none · Superseded by: none
Relates to: [ADR 0002](0002-lle-coprocessors.md) (LLE coprocessors),
[ADR 0013](0013-fast-execution-mode.md) (the fast execution mode). Neither is
amended: this adds an alternate *rasterizer backend*, not an execution mode, and
the RDP command stream it consumes is the same LLE stream the software rasterizer
consumes today.

## Context

### The throughput case is weak, and that has to be said first

`docs/performance.md`'s `fast-exec` profile puts the **RDP at 6.36%** of a
rendering frame. Eliminating it outright is **1.068x** — against the 3.9x still
needed for 60 FPS. **A GPU RDP is not a throughput answer on the current
workload**, and anyone reaching for it as one is reading the wrong number.

Two honest qualifications to that, in the other direction:

- **The share is small partly because the software RDP is incomplete.** Remaining
  opcodes are recognized-not-dispatched (`crates/rustyn64-rdp/src/lib.rs`
  `TODO(T-31-004)`), per-command timing is deferred, and `docs/residuals/R-18.md`
  documents the end-to-end commercial-video gap. A *finished* software rasterizer
  — full combiner, blender, coverage, LOD, the whole pixel pipeline — costs
  substantially more than 6.36%, and that cost is a future liability this decision
  can avoid rather than incur.
- **It is the only remaining candidate that is not CPU work.** Everything measured
  in this cycle — the fast scheduler, instruction-granular timing, the RSP idle
  steps, the VU hoist, decode caching — competes for the same core. A GPU backend
  moves work off it entirely, which is a different kind of change from any of
  those.

**So the case for this is completeness and accuracy, with throughput as a
secondary and unquantified benefit.** Sizing it as a speedup would repeat the
mistake this project made with the deficit-counter scheduler, which was scoped
against a share that had already moved.

### Porting is not on the table; binding is routine

`ref-proj/parallel-rdp/` is ~10k lines of RDP logic sitting on ~35k lines of
mandatory Granite and volk scaffolding. Porting that to Rust is not a slice, it is
a project. Binding it is ordinary FFI work: the upstream integration surface is a
flat C-style header of POD structs and `uint32_t*`, with no C++ types crossing the
boundary.

**Licensing.** `ref-proj/parallel-rdp/LICENSE` is **MIT** (Themaister, 2020) —
compatible with this project's MIT OR Apache-2.0, and a **separate** attribution
obligation from ares' ISC. It must appear in `NOTICE` in its own right.

**gopher64 is study-only and stays that way.** It has a working binding of the same
library on the same Rust edition, and its existence is evidence the approach works
— but it is **GPLv3**, so its shim is read for *shape* and never copied
(`ref-proj/README.md`). Anything written here is written from the upstream MIT
header and this project's own needs.

## Decision

### 1. Bind `parallel-rdp` behind a default-off feature; do not port it

A new Cargo feature **`gpu-rdp`**, default-off, on the frontend. The name is fixed
here for the reason ADR 0011 and 0013 fixed theirs: this document is immutable on
merge, so deferring the name would make it contradict its own status.

Upstream is vendored as a **git submodule**, not copied into the tree. A copy is a
fork nobody maintains; a submodule keeps the provenance and the update path.

### 2. The software rasterizer remains the oracle, and this changes no accuracy claim

Every accuracy gate — the Angrylion `.rvec` conformance vectors, the RDP golden
frames, `rdp_conformance.rs`, the VI vectors — continues to run against the
**software** rasterizer, unchanged. Nothing in this ADR alters what
`docs/rdp.md` specifies or what `docs/STATUS.md` records.

A result produced by the GPU backend is **not** an accuracy result for this
project. It may be *compared* against the software path, and that comparison is
worth having, but the direction of authority is fixed: the software rasterizer is
what the vectors grade, and a disagreement is a GPU-backend bug until shown
otherwise.

### 3. `unsafe` is confined to one new crate

FFI requires `unsafe`. Every chip crate and `rustyn64-core` keep
`#![forbid(unsafe_code)]` exactly as they carry it today. The binding lives in a
new crate — **`rustyn64-rdp-gpu`** — which is the only place `unsafe` is permitted,
and it depends on `rustyn64-rdp` for the command-stream types rather than the
reverse, so the one-directional crate graph is preserved.

Every `unsafe` block carries a `// SAFETY:` comment naming the invariant and **who
guarantees it**, per the workspace rule. For an FFI shim that is not decoration:
the invariants are pointer validity, buffer length, and lifetime across a call into
C++, and all three are guaranteed by the Rust side.

### 4. Native-only, and the wasm claim is not weakened because it never existed here

The frontend depends on `wgpu 29` with the **`webgl`** feature, and WebGL has no
compute shaders. `parallel-rdp` is a Vulkan compute implementation. So `gpu-rdp` is
**native-only**, and any wasm build simply does not enable it.

This costs nothing that is currently owed: `CLAUDE.md` already records that there
is no wasm build in CI, so the wasm half of the byte-identity claim is aspirational
rather than tested.

### 5. First cut takes the CPU-side scanout; no Vulkan reaches the presenter

Upstream's `scanout_sync()` returns a plain CPU-side RGBA8 buffer. The first
working version hands that to the existing presenter, so **no Vulkan surface,
swapchain, or wgpu interop is needed to get a picture**. Zero-copy GPU-to-GPU
presentation is a later, separable optimization and is explicitly not part of this
decision.

### 6. Synchronization is a dirty-region tracker, not a per-frame stall

A CPU read of RDRAM that intersects a range the GPU has pending must block; one
that does not, must not. A per-frame full sync is simpler and would surrender the
throughput benefit that motivates the GPU in the first place.

**This is the part most likely to be got wrong**, because a missing sync is a
*race*: it produces a wrong pixel occasionally, on some machines, and no
deterministic gate catches it. ADR 0004's determinism contract is the binding
constraint — seed + ROM + input must still give bit-identical AV — and a GPU
backend that cannot honor that is not shippable **whatever its frame rate**.

### 7. This is scoped as an experiment with a stated kill criterion

Merged behind a default-off feature, with the software path untouched, this is
reversible. It is **abandoned** if any of the following holds after the first
working picture:

- determinism (ADR 0004) cannot be honored under the dirty-region policy;
- the measured frame is not better than the software path on a real title;
- the Vulkan dependency cannot be made optional for people who do not enable the
  feature.

Writing the kill criteria down before starting is the point. A vendored submodule,
an FFI layer and a Vulkan dependency are exactly the kind of investment that
acquires momentum, and this project has just spent a cycle learning to abandon
things on measurement.

## Consequences

### Good

- The only remaining candidate that moves work **off the CPU** rather than around it.
- Avoids the future cost of completing the software pixel pipeline, which the
  6.36% figure does not include.
- `parallel-rdp` is the reference-grade implementation; agreement with it is
  evidence, and disagreement is a lead.
- Default-off and reversible, with the software path untouched.

### Bad, and accepted

- **A native, non-Rust dependency** — Vulkan, a C++ compiler, a submodule — in a
  project that currently builds with `cargo build` and nothing else. This is the
  single largest cost and it is paid by everyone who builds with the feature on.
- **`unsafe` enters the tree**, for the first time outside the frontend.
- **Two rasterizers to keep in step**, and the classic hazard that the unattended
  one rots.
- **No wasm.**
- CI grows a Vulkan-capable job, or the feature goes untested on CI — and an
  untested feature is the "gate that never runs" this project has already been
  bitten by.

### Explicitly rejected alternatives

- *Port `parallel-rdp` to Rust.* ~45k lines including mandatory scaffolding. Not a
  slice.
- *Write a new compute rasterizer against `wgpu`.* Attractive — no FFI, no
  submodule, no `unsafe`, and the frontend already has `wgpu`. Rejected **for the
  first cut only**: it forfeits parallel-rdp's decade of accuracy work, and the
  point of a GPU backend here is completeness rather than novelty. Worth revisiting
  if the FFI cost proves worse than expected.
- *Copy gopher64's binding.* GPLv3. Not available, and reading it for shape is the
  limit (`ref-proj/README.md`).
- *Do it as a throughput play.* Rejected on the measurement: 6.36% of a frame,
  1.068x if perfect. That is not why this is worth doing.

## Follow-up work this ADR does not decide

- The sync policy's exact granularity, and how determinism is *demonstrated* under
  it rather than assumed.
- Whether the presenter eventually takes a GPU texture instead of the CPU buffer.
- Whether CI gains a Vulkan runner, or the feature is validated only locally — and
  if the latter, how that is recorded so it is not mistaken for tested.
- Whether the software rasterizer's remaining opcodes (`T-31-004`) are still
  finished once a GPU backend exists. They are the oracle, so the answer is
  probably yes, but it is a real question and this ADR does not settle it.
