# Performance — RustyN64

**References:** `ref-docs/research-report.md` §3, §4, §challenge 7,
§Architecture options B; `docs/scheduler.md`; `docs/rsp.md`; `docs/rdp.md`.

## The bottleneck (where the time goes)

Per `ref-docs/research-report.md` §4 and §challenge 7, once the RDP is on the GPU
(ParaLLEl-RDP: ~0.2 ms/frame, 2000–5000 VI/s on mid-range GPUs), the RDP is **not**
the bottleneck — the single-threaded **RSP + CPU** are. RustyN64 inherits this
shape: the LLE RSP interpreter (scalar + vector, one issue per cycle) and the
VR4300 interpreter dominate the frame budget. This drives the whole performance
plan.

## Strategy: correctness first, then accelerate as validated layers

1. **Interpreter-only, everywhere, first.** The VR4300 and RSP ship as
   interpreters — they are the **determinism oracle** (ADR 0004) and the thing the
   accuracy suites pin. Do not optimize before the suites pass.
2. **Software reference RDP first.** A pure-Rust software LLE RDP (the angrylion
   analog) is the always-correct renderer and the RDP fuzz-suite gate
   (`docs/rdp.md`, `ref-docs/research-report.md` §4, §Architecture options B).
3. **Then accelerate, each validated against the interpreter/reference:**
   - a **wgpu-compute RDP** backend (the ParaLLEl-RDP analog) — validated against
     the software RDP, never replacing it as the oracle;
   - an **RSP dynarec** (the ParaLLEl-RSP / dgb-n64 approach) — the interpreter
     stays the deterministic fallback (`ref-docs/research-report.md` §3,
     §challenge 7);
   - a **CPU dynarec** if the interpreter can't hit frame rate.

Acceleration is additive and off-by-default-equivalent: the interpreter path
remains byte-identical and is what the determinism contract is defined against.

## Hot-path discipline

- **No allocations** in `Cpu::tick`, `Rsp::tick`, `Rdp::tick`, `Audio::tick`, or
  the per-pixel RDP inner loop. Prefer fixed arrays; the Bus uses
  `core::mem::take` split-borrow (no heap) to step owned chips
  (`docs/architecture.md` fact 2).
- The chip stack is `#![no_std]` + `alloc`; only the frontend carries `std`.
- **Profile before abstracting.** `cargo bench` (criterion) per chip crate +
  `perf record` on a headless run. Each chip crate has a `benches/` harness
  (`crates/rustyn64-*/benches/`).

## Targets (provisional — refine after the interpreter lands)

| Metric | Target |
| --- | --- |
| Headless emulated frame | ≤ 16.67 ms (60 fps NTSC) on a modern desktop core |
| RSP interpreter | the watch item — measure first, dynarec if it misses |
| Software RDP at native res | interactive; quantify vs the compute backend need |
| wgpu-compute RDP | sub-ms/frame (the ParaLLEl-RDP reference point) |

These are interpreter-era goals; the dynarec/compute backends exist to close any
gap that remains.

## Measurement plan

- Per-chip criterion benches (`cargo bench -p rustyn64-cpu` / `-rsp` / `-rdp` /
  `-audio`).
- A headless "run N frames of ROM X" harness for end-to-end ms/frame.
- `perf record` / flamegraph on the headless run to find the top hot functions
  before any optimization (the RustyNES measure-first discipline).
- A perf-capture regression gate (≥X% Criterion regression fails CI) once the
  interpreter is stable.

## Open questions

- **RDP backend ordering** — can the software RDP hit interactive native-res
  speed, or must the wgpu-compute backend come sooner
  (`ref-docs/research-report.md` §Open questions 3; `docs/rdp.md`)?
- **Bus-arbitration cost** — how much CPU/RSP/RDP/DMA contention modelling is
  needed before it becomes a measurable cost (`docs/scheduler.md` open question).
- **Dynarec backend** — `cranelift` vs hand-rolled x86_64/aarch64 for the RSP/CPU
  recompilers (`ref-docs/research-report.md` §External dependencies).
