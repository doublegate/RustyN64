# Scheduler — RustyN64

**References:** `ref-docs/research-report.md` §Executive summary, §Background,
§Architecture options C; `crates/rustyn64-core/src/scheduler.rs`; ADR 0001;
ADR 0002; ADR 0004.

## Purpose

The scheduler is the heart of the emulator: it advances the VR4300, the RSP, the
RDP, and the eight RCP interfaces on **one fractional master timeline** in
lockstep, so that mid-instruction coprocessor events are visible to subsequent
CPU code without per-quirk patches. It is what makes accuracy a property of the
architecture rather than a pile of special cases.

## The timebase model

Per `ref-docs/research-report.md` §1 and §Background, the whole machine derives
from a single 14.31818 MHz NTSC colour-burst crystal via PLLs. The two numbers
the scheduler cares about:

| Clock | Rate | Source |
|---|---|---|
| VR4300 pipeline (PClock) | **93.75 MHz** | R4300i `DivMode` = 1.5:1 off MasterClock |
| RCP / system (MasterClock) | **62.5 MHz** | RSP + RDP + the interfaces |

The CPU runs at exactly **1.5×** the RCP, i.e. a clean **3:2** ratio. This is the
N64's analogue of the NES "PPU is the master clock, CPU every third dot"
relationship — except the roles invert (the *CPU* is the finer clock here).

### Why the VR4300 is the master tick unit

We pick the **finer** clock (the VR4300 cycle) as the master tick so the RCP
advances on an integer-fraction accumulator rather than the CPU advancing on a
fractional one. `MASTER_HZ = 93_750_000`. The RCP advances **2 ticks per 3
master ticks** via a numerator/denominator accumulator:

```rust
pub const RCP_NUM: u32 = 2; // RCP ticks numerator
pub const RCP_DEN: u32 = 3; // master-ticks denominator

// in tick_one_unit(), after stepping the CPU:
self.rcp_accum += RCP_NUM;          // add 2 each master tick
while self.rcp_accum >= RCP_DEN {   // fire one RCP step per 3-wrap
    self.rcp_accum -= RCP_DEN;
    self.step_rcp();                // RSP, then RDP, then AI/interfaces
}
```

Over any 3 master ticks the RCP advances exactly 2 times — the
`fractional_divisor_holds_3_to_2` unit test pins this.

### Why fractional, not integer lockstep

The clean 3:2 *could* be expressed as integer lockstep. We still choose a
**fractional, event-aware** scheduler (per `ref-docs/research-report.md`
§Architecture options C) because the surrounding clocks are not integer multiples
of either core clock:

- The **AI** sample rate is `video_clock / (DACRATE + 1)` — an arbitrary divisor.
- The **VI** vertical/horizontal counters and the **PAL** field timing
  (50 Hz, ~312.5 lines) do not divide evenly into the 93.75/62.5 MHz pair.
- **DMA durations** (PI/SI/SP) are byte-count-driven, not cycle-aligned.

A fractional accumulator (plus an event queue for timed completions, below)
absorbs all of these without per-quirk fudging. This is the same direction
RustyNES v2.0.0 "Timebase" is heading, designed in from day one here. The
*future* refinement to sub-cycle bus-transaction resolution (if a hard-tier ROM needs it)
is ADR 0005 — a separate, later milestone, not the v0.1 scheduler.

## Interfaces

The scheduler's public surface (`crates/rustyn64-core/src/scheduler.rs`):

```rust
pub const MASTER_HZ: u64 = 93_750_000;
pub const RCP_HZ: u64    = 62_500_000;
pub const RCP_NUM: u32   = 2;
pub const RCP_DEN: u32   = 3;

pub struct System {
    pub cpu: Cpu,
    pub bus: Bus,        // owns RSP/RDP/AI/cart/RCP regs/RDRAM
    phase: u32,          // seeded power-on phase, 0..RCP_DEN
    rcp_accum: u32,      // fractional RCP divisor accumulator
    master_ticks: u64,
    seed: u64,
}

impl System {
    pub fn new(seed: u64) -> Self;        // seeded power-on phase alignment
    pub fn reset(&mut self);              // warm reset; re-derives the SAME phase
    pub fn tick_one_unit(&mut self);      // advance one master tick (CPU + RCP)
    pub const fn master_ticks(&self) -> u64;
}
```

## State and the divisor table

| Engine | Advances | Divisor vs. master |
|---|---|---|
| VR4300 (master) | every master tick | 1 (the tick unit) |
| RSP (in `step_rcp`) | every RCP tick | 2 per 3 master ticks |
| RDP (in `step_rcp`) | every RCP tick | 2 per 3 master ticks |
| AI / interfaces | sub-divided off the RCP/VI clock | derived (event-driven) |
| Cart (PI/SI DMA) | byte-count-driven completion events | event-driven |

The RCP step order inside `step_rcp` is **RSP → RDP → AI** on the same
`&mut self.bus`, so the RDP sees the RSP's just-emitted commands and the AI sees
the just-mixed samples within the same RCP tick.

## Behavior

### Seeded power-on phase alignment

`System::new(seed)` derives `phase` in `0..RCP_DEN` from a SplitMix64 PRNG seeded
with `seed` (never the OS RNG). `rcp_accum` starts at `phase`, so two power-ons
with different seeds begin the CPU/RCP relationship at a different sub-tick
offset — modelling the real hardware's power-on phase indeterminacy while staying
**reproducible**. `reset()` re-derives the *same* phase from the retained seed,
so a mid-run reset preserves alignment (the `reset_preserves_phase` test pins
this). This is the determinism contract's foundation (ADR 0004).

### Lockstep, not catch-up

Each `tick_one_unit` steps the CPU first, then drains the RCP accumulator. There
is never a burst where the RSP "runs to completion" at a frame boundary — that
would break the framebuffer-readback and mid-display-list synchronization games
rely on (`ref-docs/research-report.md` §3, challenge 3). One timeline, in order.

### Timed completions (the event model)

DMA completions (PI/SI/SP/AI) and the VI scanline interrupt are **future events**:
when the CPU starts a DMA, the scheduler computes its duration from the byte count
and bus rate and schedules the completion interrupt at the correct future master
tick — never instantaneously. Instantaneous DMA desyncs audio and breaks busy-wait
loops (`ref-docs/research-report.md` §challenge 5). The v0.1 skeleton steps DMA
progress per RCP tick; the event-queue refinement is a Phase-1/2 ticket.

## Edge cases and gotchas

- **Don't advance the RCP before the CPU within a tick.** The order is CPU then
  RCP-drain; reversing it changes which engine sees whose write first and breaks
  determinism.
- **No OS threads in the core.** The dedicated emulation thread lives in the
  *frontend* and owns a `System`; the core itself is single-timeline
  (`ref-docs/research-report.md` §Background; ADR 0004).
- **PAL changes the VI/AI divisors, not the core clocks.** The 93.75/62.5 MHz
  pair is region-independent; only the VI counters and AI video-clock divisor
  differ (`ref-docs/research-report.md` §9). Carry these as a region data table,
  not a build fork. See `docs/compatibility.md`.
- **`rcp_accum` must be seeded from `phase`, not zeroed**, or every power-on
  starts phase-aligned and loses the modelled indeterminacy.
- **The fractional wrap is a `while`, not an `if`.** With `RCP_NUM < RCP_DEN` it
  fires at most once per master tick today, but keeping the loop is correct if a
  future sub-divided clock ever has a numerator ≥ denominator.

## Test plan

- **Unit:** `fractional_divisor_holds_3_to_2` (2 RCP per 3 master),
  `reset_preserves_phase` — both already present.
- **Property:** over N master ticks, RCP ticks == `floor((N + phase) * 2 / 3)`
  for every seed; `master_ticks()` monotonic.
- **Integration (Phase 1+):** a DMA scheduled at tick T raises its completion
  interrupt at exactly T + duration; a busy-wait loop polling `SP_STATUS`
  observes the halt clear at the modelled cycle.
- **Determinism:** two `System`s with the same seed + input produce identical
  `master_ticks` and bit-identical RDRAM after a fixed run (the determinism gate,
  ADR 0004).

## Open questions

- The exact bus-arbitration cost model (CPU vs RSP vs RDP vs DMA on the shared
  RDRAM) — how deep before commercial-game correctness is reached vs CEN64's RTL
  depth (`ref-docs/research-report.md` §Open questions 1). Needs a prototype.
- Whether any commercial title needs sub-cycle bus-transaction resolution (the ADR
  0002 refactor) or whether whole-master-tick lockstep suffices through v1.0.
