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
| SClock (CPU system interface) | = MClock | 125/2 | 62.5 |
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
| Serial Interface | 15.625 | **12** |
| Cartridge / PIF | 1.953125 | **96** |

No accumulator, no remainder, no drift possible by construction. (The VI dot clock at
~48.6818 MHz is genuinely a *different crystal domain* and does not divide evenly; it keeps a
remainder-carrying accumulator. That is an honest representation of the hardware, not a
workaround — see Consequences.)

**3. The counter-ownership rule matters more than the unit.** A sibling project shipped a
scheduler with five cycle counters (CPU, PPU, APU, bus, master) kept in agreement *by
construction* — every call site incremented them in matching steps — rather than *by
derivation*. Its own ADR describes the result as "a correct but fragile invariant". It took a
full scheduler rewrite, preceded by 17+ failed point-fixes, to collapse them into one. Our
`scheduler.rs` already has the seed of the same disease: `master_ticks`, `Cpu::cycles`, and
`rcp_accum` are three counters that increment independently and agree only because the code
currently happens to keep them in step.

That project measured the outcome of the rewrite: after ordinary optimisation the one-clock
model ran **~9% faster than the design it replaced**, at 100% versus 94.24% accuracy. The
throughput objection to a canonical master clock does not survive contact with that data.

## Decision

**The master tick is 1/187,500,000 s. One `u64` counts it, and it is the only counter in the
core that is ever incremented.**

```rust
pub const MASTER_HZ: u64 = 187_500_000;   // the tick unit
pub const CPU_DIVIDER: u64 = 2;           // VR4300 PClock  93.75 MHz
pub const RCP_DIVIDER: u64 = 3;           // MClock / SClock 62.5  MHz
```

Every other cycle position is **assigned** from `master_ticks`, never incremented:

- `Cpu::cycles` is `master_ticks / CPU_DIVIDER`, written at the start of each CPU step.
- The RCP's position is `master_ticks / RCP_DIVIDER`.
- COP0 `Count` runs at **half PClock** (46.875 MHz — `master_ticks / 4`), which is a hardware
  fact three reference emulators each encode differently and a common source of 2x timing bugs.

A component is stepped when `master_ticks % divider == 0`. The CPU:RCP pattern repeats every
**6** master ticks (CPU at 0, 2, 4; RCP at 0, 3), so the scheduler advances **edge to edge**
and never iterates the master tick itself. The unit is a time base, not a loop counter.

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

- **"Master clock" is now ambiguous across documents.** ADR 0001 used it for the 93.75 MHz
  VR4300 cycle; this ADR uses it for the 187.5 MHz tick. `docs/scheduler.md` states both and
  names which is which. Any document citing `MASTER_HZ` must say the unit.
- **Derived-position code can silently drift back to incremented.** A future contributor adding
  `self.foo_cycles += 1` reintroduces the exact defect. The residue test is the guard; keep it
  in the default `cargo test` path, not behind a feature.

## Alternatives considered

- **Keep the VR4300 cycle as master with a 2/3 accumulator** (ADR 0001, status quo). Works and
  cannot drift *if* the remainder is always carried and never floated — it is isomorphic to a
  6-tick integer period. Rejected because it inverts the hardware derivation, because the
  accumulator state makes cross-domain event ordering a state question rather than an integer
  comparison, and because it does not extend to SI/PIF without more remainders.
- **Structural unrolling** (CEN64: a hard-coded loop of 3 CPU cycles and 2 RCP cycles per
  iteration, `ref-proj/cen64/device/device.c`). Cannot drift and costs nothing, but the ratio
  is baked into the loop shape, adding a domain means rewriting the loop, and sub-cycle
  ordering within the group is not expressible.
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
