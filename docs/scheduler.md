# Scheduler — RustyN64

**References:** **ADR 0006** (the canonical master clock — this doc's core),
ADR 0007 (the CPU pipeline it drives), ADR 0002, ADR 0004;
`n64brew_wiki/markdown/Clock Timing.md` (the exact clock fractions);
`n64brew_wiki/images/VR4300-Users-Manual.pdf` §11;
`crates/rustyn64-core/src/scheduler.rs`; `ref-docs/research-report.md`
§Executive summary, §Background, §Architecture options C. ADR 0001 is
**superseded** by ADR 0006 and is retained only as the record of the first design.

## Purpose

The scheduler is the heart of the emulator: it advances the VR4300, the RSP, the
RDP, and the eight RCP interfaces on **one canonical 187.5 MHz master timeline**
in lockstep, so that mid-instruction coprocessor events are visible to subsequent
CPU code without per-quirk patches. It is what makes accuracy a property of the
architecture rather than a pile of special cases.

## The timebase model

### One counter, and only one

**The single load-bearing rule of this scheduler: exactly one counter is ever
incremented, and every other cycle position is *assigned* from it.**

A cycle position that is incremented independently agrees with the master only
because every call site remembers to step it — an invariant maintained by
construction rather than by derivation. A sibling project shipped five such
counters (CPU, PPU, APU, bus, master) and its own ADR describes the result as
"a correct but fragile invariant"; unwinding it took a full scheduler rewrite
preceded by 17+ failed point-fixes. Deriving instead of incrementing makes the
whole class of desync bug unrepresentable.

The derivation is pinned by a **residue invariant test**: the affine offsets
between `master_ticks` and every derived position are sampled at frame
boundaries and asserted never to move. A path that forgets to derive fails it on
the first frame.

### The unit: 187.5 MHz

`n64brew_wiki/markdown/Clock Timing.md` gives every N64 clock as an exact
fraction. The VR4300 is *derived*, not primary — MClock is:

| Clock | Derivation | Exact | MHz |
| --- | --- | --- | --- |
| RCLK | crystal X2 × 17 | 250 | 250 |
| **MClock** | RCLK ÷ 4 | 125/2 | **62.5** |
| **VR4300 PClock** | MClock × 3/2 (PLL, `DivMode = 0b01`) | 375/4 | **93.75** |
| SClock (CPU system interface) | = MClock | 125/2 | 62.5 |
| Serial Interface | MClock ÷ 4 | 125/8 | 15.625 |
| Cartridge / PIF | SI ÷ 8 | 125/64 | 1.953125 |

`MASTER_HZ = 187_500_000` — the LCM of 93.75 and 62.5 — makes **every** emulated
domain an integer divisor:

| Component | Rate (MHz) | Ticks every |
| --- | --- | --- |
| VR4300 PClock | 93.75 | **2** |
| RCP / MClock / SClock | 62.5 | **3** |
| COP0 `Count` | 46.875 (half PClock) | **4** |
| Serial Interface | 15.625 | **12** |
| Cartridge / PIF | 1.953125 | **96** |

No accumulator and no remainder for any of these, so drift is not merely avoided
— it is unrepresentable.

`Count` running at **half** PClock is a hardware fact worth stating loudly: three
reference emulators each encode it differently, it is a classic source of 2x
timing bugs, and n64-systemtest's timing set keys off it.

### The master tick is a time base, not a loop counter

Nothing iterates 187.5M times per second. The CPU lands on every 2nd tick and the
RCP on every 3rd, so the pattern repeats every **6** master ticks (CPU at 0, 2, 4;
RCP at 0, 3) and the scheduler advances **edge to edge**. The cost is one integer
add per component step — the same work CEN64's hard-unrolled 3-CPU/2-RCP loop
does, but with the ratio expressed as data rather than baked into the loop shape.

Because both spans are integers on one counter, "which edge came first" is a
plain comparison rather than a question about accumulator state.

### Where a fractional accumulator still applies

The **VI** dot clock (~48.6818 MHz NTSC) runs off a different crystal and does
*not* divide evenly into 187.5 MHz. It keeps a remainder-carrying accumulator
(integer numerator/denominator, never floats). This is deliberate: forcing VI
into an integer divisor would encode a number that is not true. The same applies
to the AI sample rate (`video_clock / (DACRATE + 1)`), PAL field timing, and
byte-count-driven DMA durations, which are event-scheduled rather than divided.

So the core has two mechanisms, and the split is meaningful: **integer divisors
for domains that genuinely are rational multiples of the master, accumulators
only for domains that genuinely are not.**

## Interfaces

The scheduler's public surface (`crates/rustyn64-core/src/scheduler.rs`):

```rust
pub const MASTER_HZ: u64    = 187_500_000; // the tick unit (ADR 0006)
pub const CPU_HZ: u64       =  93_750_000; // VR4300 PClock
pub const RCP_HZ: u64       =  62_500_000; // MClock / SClock
pub const CPU_DIVIDER: u64  = 2;           // CPU steps every 2nd master tick
pub const RCP_DIVIDER: u64  = 3;           // RCP steps every 3rd
pub const COUNT_DIVIDER: u64 = 4;          // COP0 Count, half PClock

pub struct System {
    pub cpu: Cpu,
    pub bus: Bus,        // owns RSP/RDP/AI/cart/RCP regs/RDRAM
    master_ticks: u64,   // THE counter. Nothing else is incremented.
    phase: u64,          // seeded power-on offset, 0..6
    seed: u64,
}

impl System {
    pub fn new(seed: u64) -> Self;       // seeded power-on phase alignment
    pub fn reset(&mut self);             // warm reset; re-derives the SAME phase
    pub fn run_until(&mut self, tick: u64); // advance edge to edge to `tick`
    pub const fn master_ticks(&self) -> u64;
    pub const fn cpu_cycles(&self) -> u64 { self.master_ticks / CPU_DIVIDER }
    pub const fn rcp_cycles(&self) -> u64 { self.master_ticks / RCP_DIVIDER }
}
```

`cpu_cycles()` and `rcp_cycles()` are **derived accessors, not fields.** That is
the rule in code form: there is no `self.cpu_cycles += 1` anywhere.

## State and the divisor table

| Engine | Advances | Divisor vs. master |
| --- | --- | --- |
| VR4300 pipeline | every 2nd master tick | 2 (integer) |
| RSP (in `step_rcp`) | every 3rd master tick | 3 (integer) |
| RDP (in `step_rcp`) | every 3rd master tick | 3 (integer) |
| COP0 `Count` | every 4th master tick | 4 (integer, half PClock) |
| SI / PIF | every 12th / 96th master tick | 12 / 96 (integer) |
| VI | ~48.68 MHz, separate crystal | fractional accumulator |
| AI | `video_clock / (DACRATE + 1)` | fractional / event-driven |
| Cart (PI/SI DMA) | byte-count-driven completion events | event-driven |

The RCP step order inside `step_rcp` is **RSP → RDP → AI** on the same
`&mut self.bus`, so the RDP sees the RSP's just-emitted commands and the AI sees
the just-mixed samples within the same RCP tick.

## Behavior

### Seeded power-on phase alignment

`System::new(seed)` derives `phase` in `0..6` (the CPU/RCP repeat period) from a
SplitMix64 PRNG seeded with `seed` (never the OS RNG), and `master_ticks` starts
at `phase`. Two power-ons with different seeds therefore begin the CPU/RCP
relationship at a different offset within the 6-tick pattern — modelling the real
hardware's power-on phase indeterminacy while staying **reproducible**.

This is not a modelling convenience; it is vendor-documented. VR4300 User's
Manual Table 11-1 charges "**1 to 2** PCycles: synchronize with SClock and
transfer address to internal SysAD bus" for every data-cache miss — an
indeterminate cost, arising precisely because PClock and SClock are in a 3:2
relationship and the transaction lands on an arbitrary phase of it. The seeded
phase is the deterministic stand-in for that hardware indeterminacy. `reset()` re-derives the *same* phase from the retained seed,
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
- **Never add a counter.** Any `self.something_cycles += 1` in the core
  reintroduces the exact defect ADR 0006 exists to prevent. Derive it from
  `master_ticks` and let the residue test guard it. If a derived value is hot
  enough to cache, cache it — but the residue test still owns correctness.
- **`master_ticks` must be `u64`.** At 187.5 MHz a `u32` wraps in 23 seconds.
- **`master_ticks` starts at `phase`, not zero**, or every power-on is
  phase-aligned and the modelled indeterminacy is lost.
- **"Master clock" is ambiguous across documents.** ADR 0001 used it for the
  93.75 MHz VR4300 cycle; ADR 0006 and this doc use it for the 187.5 MHz tick.
  Always state the unit when citing `MASTER_HZ`.

## Test plan

- **The residue invariant (the important one):** sample the affine offsets
  between `master_ticks` and every derived position at frame boundaries and
  assert they never move after the first boundary. This is what catches a counter
  that has quietly become independently-incremented. Keep it in the default
  `cargo test` path, never behind a feature.
- **Unit:** 3 CPU steps and 2 RCP steps per 6 master ticks; `reset_preserves_phase`.
- **Property:** over N master ticks, CPU steps == `floor((N + phase) / 2)` and RCP
  steps == `floor((N + phase) / 3)` for every seed; `master_ticks()` monotonic.
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
- Whether any commercial title needs resolution finer than one PClock (the
  deferred **ADR 0005** refactor). Note ADR 0007 already models the SysAD
  command/data split at SClock, which is *coarser* than a PClock, so ADR 0005
  remains a genuinely separate and later question.
- **`M`, the memory access time in PCycles** — undocumented, and both cache-miss
  formulas depend on it (`docs/cpu.md` §Cycle costs). Must be fitted against test
  ROMs and recorded as a measured constant.
