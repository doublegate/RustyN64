# ADR 0006 — One canonical 187.5 MHz master clock; every other position derived

## Status

Accepted (2026-07-20). **Supersedes [ADR 0001](0001-master-clock-lockstep-scheduler.md).**

ADR 0001's goals are unchanged and still correct: one timeline, lockstep rather than
catch-up, seeded power-on phase, no OS threads in the core. What changes is the *unit* the
timeline is counted in and, more importantly, the rule about who is allowed to increment.

## Context

ADR 0001 made the **VR4300 cycle** the master tick and derived the RCP from it with a 2/3
fractional accumulator. Three findings make that the wrong shape.

**1. It is not how the hardware derives its clocks.** `n64brew_wiki/markdown/Clock Timing.md`
gives every rate as an exact fraction, and the VR4300 is the *derived* part:

| Clock | Derivation | Exact | MHz |
|---|---|---|---|
| RCLK | crystal X2 × 17 | 250 | 250 |
| **MClock** | RCLK ÷ 4 | 125/2 | **62.5** |
| **VR4300 PClock** | MClock × 3/2 (PLL, `DivMode = 0b01`) | 375/4 | **93.75** |
| SClock (CPU system interface) | = MClock (normally; reduced in low-power mode) | 125/2 | 62.5 |
| Serial Interface | MClock ÷ 4 | 125/8 | 15.625 |
| Cartridge / PIF | SI ÷ 8 | 125/64 | 1.953125 |

MClock is primary; the CPU is MClock multiplied up by an internal PLL. Making the CPU the
master inverts the hardware's own derivation, which is why the RCP needed a remainder.

**2. A single unit makes every divisor an integer.** 187.5 MHz — the LCM of 93.75 and 62.5 —
divides evenly into every clock domain we emulate:

| Component | Rate (MHz) | Ticks every |
|---|---|---|
| VR4300 PClock | 93.75 | **2** |
| RCP / MClock / SClock | 62.5 | **3** |
| COP0 `Count` (half PClock) | 46.875 | **4** |
| Serial Interface | 15.625 | **12** |
| Cartridge / PIF | 1.953125 | **96** |

No accumulator, no remainder, no drift possible by construction. (The VI video clock — VCLK, 48.6818 MHz NTSC — derives from crystal **X1** where MClock derives
from **X2**, so it is genuinely a different crystal domain, and 187.5 / 48.6818 = 3.8515 does not
divide. It keeps a remainder-carrying accumulator. That is an honest representation of the
hardware, not a workaround — see Consequences.)

**3. This unit is not novel; the ownership rule is what we add.** ares already counts in exactly
this tick: `ref-proj/ares/ares/n64/system/system.hpp` declares `frequency = 93'750'000 * 2` =
187,500,000, and every CPU step is written `step(N * 2)` with the `* 2` left unfolded to keep the
unit visible. So the choice of unit has working prior art in a shipping emulator. What this ADR
adds beyond ares' choice is the counter-ownership rule below and the residue invariant that
enforces it.

**4. The counter-ownership rule matters more than the unit.** A sibling project shipped a
scheduler with five cycle counters (CPU, PPU, APU, bus, master) kept in agreement *by
construction* — every call site incremented them in matching steps — rather than *by
derivation*. Its own ADR describes the result as "a correct but fragile invariant". It took a
full scheduler rewrite, preceded by 17+ failed point-fixes, to collapse them into one. Our
`scheduler.rs` already has the seed of the same disease: `master_ticks`, `Cpu::cycles`, and
`rcp_accum` are three counters that increment independently and agree only because the code
currently happens to keep them in step.

That project measured the outcome, and the honest reading is mixed rather than triumphant. Its
direct A/B — same host, same session — put the one-clock model **6–8% slower in end-to-end frame
time**, buying 94.24% → 100.00% accuracy. Its *isolated CPU loop* got ~35% faster; the cost is
entirely bus-side. Later model-independent optimisation (LTO, lookup tables) brought the
one-clock build below the legacy build's original absolute time, but legacy was never
re-measured with those same optimisations, so the widely-quoted "faster overall" comparison is
apples-to-oranges and is not repeated here.

**Treat the throughput cost as real and bounded at single-digit percent, not as refuted.** That
is still a cost worth paying for the accuracy, and it is small enough that it does not by itself
put a full-speed cycle-accurate core out of reach — but the project's performance goal has to be
defended by measurement, not by this precedent.

## Decision

**The master tick is 1/187,500,000 s. One `u64` counts it, and it is the only counter in the
core that is ever incremented.**

```rust
pub const MASTER_HZ: u64     = 187_500_000; // the tick unit
pub const CPU_DIVIDER: u64   = 2;           // VR4300 PClock   93.75    MHz
pub const RCP_DIVIDER: u64   = 3;           // MClock / SClock 62.5     MHz
pub const COUNT_DIVIDER: u64 = 4;           // COP0 Count      46.875   MHz
pub const SI_DIVIDER: u64    = 12;          // Serial Interface 15.625  MHz
pub const PIF_DIVIDER: u64   = 96;          // Cartridge / PIF  1.953125 MHz
```

Every other cycle position is **derived** from `master_ticks`, never incremented:

- `cpu_cycles()` and `rcp_cycles()` are **accessors, not fields** — there is no
  `self.cpu_cycles += 1` anywhere in the tree.
- COP0 `Count` runs at **half PClock** (46.875 MHz — every 4th master tick), per UM §6.3.3
  Figure 6-3: *"Count : latest count value (incremented at frequency half PClock)"*. All three
  reference emulators implement it by storing at PClock and shifting right on read; forgetting
  that shift is a common source of 2x timing bugs.

  **`Count` is architecturally writable** (`MTC0`), so it cannot be a pure function of
  `master_ticks`. It is **affine**: `count = count_epoch_value + (master_ticks -
  count_epoch_tick) / 4`, re-based on every write. That is still derived-never-incremented — the
  rule forbids a counter that advances on its own, not a re-basable origin.

### Per-domain phase offsets, so the seeded power-on phase survives

A component is stepped when `(master_ticks + phase[domain]) % divider == 0`.

The per-domain offset is **not optional bookkeeping** — without it the seeded power-on phase
that ADR 0004 requires would be destroyed by this ADR. If both domains keyed off the same
absolute counter (CPU on even ticks, RCP on multiples of 3), their interleaving would be
byte-identical for every seed from tick 6 onward, and a different seed would only truncate the
first partial period. ADR 0004's "hardware indeterminacy modelled as a *parameter*" would become
vacuous while `reset_preserves_phase` still passed, testing nothing.

So each domain carries an immutable phase constant, seeded at power-on from the SplitMix64 PRNG
and re-derived identically on reset. Phases are **constants, not counters** — nothing increments
them — so the ownership rule is intact. This gives a persistent relative CPU↔RCP alignment, which
is what the hardware's PLL lock actually produces.

The CPU:RCP pattern repeats every **6** master ticks, so the scheduler advances **edge to edge**
and never iterates the master tick itself. The unit is a time base, not a loop counter.

### Positions are not counts

`master_ticks / divider` is a **position on the timeline**, and with a nonzero phase it is
nonzero before the component has done any work. That is correct and expected — the residue
invariant test exists precisely to pin those constant offsets (the sibling's shipped residues
were `(12, 0, 0)`, not all zero).

A **count of work retired** is a different thing and is legitimately its own field: `Cpu::retired`
for the golden-log differ, incremented per retired instruction, which nothing schedules against.
The ownership rule forbids a second counter that *time is derived from*, not a tally of work done.
Do not conflate the two — reading a position as a count is how an off-by-phase bug enters.

**The derivation is pinned by a residue invariant test.** The affine offsets between the
canonical counter and every derived position are sampled at frame boundaries and asserted
never to move. An independently-incremented counter fails this test on the first frame where
any path forgets to step it — which is precisely the failure mode that was unpatchable in the
sibling's five-counter model.

## Consequences

### Positive

- Drift is impossible by construction rather than by discipline. There is no remainder to
  carry for the CPU/RCP relationship and no call site that can forget to increment.
- The hardware's own derivation is preserved: MClock primary, PClock a 3/2 multiple.
- Adding a domain is adding a divisor. SI and PIF already divide evenly and cost nothing.
- Sub-cycle event ordering becomes a plain integer comparison. A CPU cycle spans master ticks
  0–1 and an RCP cycle spans 0–2; they coincide only every 6, and "which edge came first" is
  answerable without consulting accumulator state.
- Free right now. `scheduler.rs` and the `Bus` trait are both still stubs, and no save-state
  format exists. The sibling paid for this change with a save-state/movie format epoch break
  because it arrived after shipping; we pay nothing.

### Negative / costs

- `master_ticks` counts 187.5M/s, so a `u64` is mandatory (it wraps after ~3,100 years — fine;
  a `u32` would wrap in 23 seconds).
- Two divisions per step to derive positions. Both are by compile-time constants, so they
  compile to multiply-shift; if they ever show in a profile, cache the derived values and let
  the residue test guard them.
- The VI keeps a fractional accumulator, so the core has two mechanisms rather than one. This
  is deliberate: VI genuinely runs off a different crystal, and forcing it into an integer
  divisor would encode a number that is not true.

### Risks

- **"Master clock" now has three colliding meanings, and the primary sources own one of them.**
  The hardware documentation uses **MasterClock / MClock for 62.5 MHz** (`Clock Timing.md`, UM
  Figure 4-1) — that is the meaning a reader arrives with. ADR 0001 used it for the 93.75 MHz
  VR4300 cycle. This ADR uses it for the 187.5 MHz tick. Worse, `Clock Timing.md`'s DivMode table
  lists `0b11 → PClock 187.5 MHz`, so the number 187.5 *also* appears in the sources as an
  overclocked PClock the N64 does not use. Never write "master clock" without its rate;
  `docs/scheduler.md` names all three.
- **Derived-position code can silently drift back to incremented.** A future contributor adding
  `self.foo_cycles += 1` reintroduces the exact defect. The residue test is the guard; keep it
  in the default `cargo test` path, not behind a feature.

## Alternatives considered

- **Keep the VR4300 cycle as master with a 2/3 accumulator** (ADR 0001, status quo). Works and
  cannot drift *if* the remainder is always carried and never floated — it is isomorphic to a
  6-tick integer period. Rejected because it inverts the hardware derivation, because the
  accumulator state makes cross-domain event ordering a state question rather than an integer
  comparison, and because it does not extend to SI/PIF without more remainders.
- **Barrier-synchronised threads with a hard-coded per-iteration ratio** (CEN64,
  `ref-proj/cen64/device/device.c`): the VR4300 and the RSP/VI run on **two OS threads** that
  rendezvous every 6250 RCP cycles, and within the CPU thread 3 `vr4300_cycle` calls alternate
  with 2 AI/PI cycles — so the 3:2 emerges from 9375:6250 across the barrier, not from an
  unrolled loop. Cannot drift, but the ratio is baked into the loop shape, ordering across the
  barrier is not expressible at all, and threads in the core are flatly incompatible with the
  determinism contract (ADR 0004). CEN64's own multithreaded mode is not bit-reproducible.
- **A 750 MHz master** so the raw 250 MHz RCLK also divides evenly. Rejected: RCLK is a clock
  source, not a component we emulate, and the factor of 4 buys nothing else.

## References

- `n64brew_wiki/markdown/Clock Timing.md` — the exact clock fractions and the `DivMode` table.
- `n64brew_wiki/markdown/SysAD Interface.md` — "all access between the RCP and CPU is at the
  masterclock rate (62.5mhz)".
- `n64brew_wiki/images/VR4300-Users-Manual.pdf` §11 Table 11-1 — the "1 to 2 PCycles:
  synchronize with SClock" line, a vendor-documented phase indeterminacy at the CPU↔bus
  boundary, and the hardware basis for the seeded power-on phase (ADR 0004).
- [ADR 0007](0007-cycle-accurate-vr4300-pipeline.md) — the CPU microarchitecture this clock drives.
- [ADR 0005](0005-sub-cycle-bus-timing-refactor.md) — still deferred; it concerns resolution
  *finer* than one PClock, which this ADR does not provide.
