# RustyN64 agent reference: project state

Moved verbatim from the AGENTS.md project block. `docs/STATUS.md` remains authoritative for current numbers.

## Current state (read `docs/STATUS.md` first)

**Phases 1 and 2 are COMPLETE (v0.2.0 released; v0.3.0 being cut).** All four exit criteria
(two per phase) are met, each an oracle result with a committed runner:

- **Phase 1 (v0.2.0):** `Failed: 0` on the **CPU/COP0/TLB/COP1 categories**
  (`to-dos/VERSION-PLAN.md` §v0.2.0) — *not* suite-wide — and the CPU golden-log 0-diff against
  ares. `docs/STATUS.md` once said "CPU/COP0/TLB/RSP", conflating the two — VERSION-PLAN is
  authoritative.
- **Phase 2 (v0.3.0):** `Failed: 0` on the **RSP category**, and a real graphics microcode —
  libdragon's `rdpq` — boots on the RSP and **emits a plausible RDP command list** through the
  DPC seam (witnessed byte-for-byte in `tests/microcode.rs`).

The ~93 suite-wide failures that remain are cart/PIF (Phase 5) and the RDP rasterizer (Phase 3).
`docs/STATUS.md` is authoritative for the current numbers. The next rung is **v0.4.0
"Rasterizer"** (Phase 3 — the LLE RDP and VI, the first picture). Tagging is the phase-close
ceremony in VERSION-PLAN (pre-release gate, annotated tag, notes) and belongs on `main` after the
work lands, not on a feature branch.

The **VR4300 executes instructions**: the canonical 187.5 MHz clock (ADR 0006), the five-stage
pipeline (ADR 0007), the MIPS III integer set, COP0, the TLB + micro-ITLB, the exception model,
interrupts, the **primary I- and D-caches**, the privilege-aware segment map, `Status.RE`, COP1
(control, register file, arithmetic, compares, conversions, enabled traps, `BC1`), and **PI DMA** —
the last pulled forward from Phase 5 because n64-systemtest loads its own ELF through it.

The **RSP executes microcode** (Phase 2): the scalar unit, the full 8-lane vector unit (multiplies,
the accumulator, clip compares, reciprocals, the whole vector load/store family, the reserved VZERO
opcodes), SP DMA, and the halt/break/interrupt handshake. Its COP0 `c8`–`c15` route to the RDP's
DPC command registers via `su::StepResult::dp_write` → `Bus::rsp_tick` → `Rdp::dpc_write` (the RSP
may not name `rustyn64-rdp`), which is how `mtc0 DP_END` submits an RDP command list.

FP arithmetic runs on a **soft-float core** (`crates/rustyn64-cpu/src/softfloat.rs`), not on
Rust's `f32`/`f64` operators. That is not gratuitous: the native operators discard the exact
pre-rounding result, so `inexact`/`underflow` cannot be reported and `FCSR.RM` cannot be honored.
It is verified bit-for-bit against those same operators in round-to-nearest over ~100k cases —
they are the independent oracle, which is why the module implements *IEEE* behavior and leaves
the VR4300's refusal to produce subnormals as a separate layer.

**The RDP and AI are still LLE-shaped stubs; the RSP now executes.** The RDP's DPC command
registers receive command lists, but nothing downstream rasterizes them yet, and the AI does not
play audio. Do not assume any chip *other than the CPU and RSP* executes. A green `cargo test`
still does not mean a subsystem works — check `docs/STATUS.md`.

**The n64-systemtest runner is committed** at
`crates/rustyn64-test-harness/tests/systemtest.rs`, `#[ignore]`d because a full pass takes ~2
minutes in `--release`:

```bash
cargo test -p rustyn64-test-harness --release --test systemtest -- --ignored --nocapture
```

It asserts Phase 1's criterion directly and witnesses execution before trusting a zero (an empty
run produces zero failures just as convincingly as a passing one). Every `Failed: 0` claim in
`docs/STATUS.md` is this test, not a self-assessment — which is the whole point of committing it.
