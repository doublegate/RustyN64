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
relationship at a different offset within the 6-tick pattern — modeling the real
hardware's power-on phase indeterminacy while staying **reproducible**.

This is not a modeling convenience; it is vendor-documented. VR4300 User's
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
  phase-aligned and the modeled indeterminacy is lost.
- **"Master clock" is ambiguous across documents.** ADR 0001 used it for the
  93.75 MHz VR4300 cycle; ADR 0006 and this doc use it for the 187.5 MHz tick.
  Always state the unit when citing `MASTER_HZ`.

## The fast-path seam (`fast-scheduler`, default-off)

ADR 0011 / ADR 0012 accept an optional block-based scheduler. Its seam exists now
as `System::run_until_fast`, behind the default-off `fast-scheduler` feature.

**The block is one period of the edge schedule.** Which domains are due is a
function of `tick mod lcm(CPU_DIVIDER, RCP_DIVIDER)` and the power-on phases, so
the pattern repeats every 6 ticks and never changes mid-run — while the accurate
loop re-derives it on every edge. The fast path computes that shape once and
replays it: the same edges, in the same order, at the same `master_ticks`, so it is
a different *enumeration* rather than a different schedule. The partial-period tail
and the `master_ticks = target` landing still go through the accurate loop, which
is where ADR 0011 §6's fallback now lives. Measured **1.0563x** on the run loop and
**1.0544x** on a real frame through the frontend (`docs/performance.md`).

**It is reachable but still off by default.** `rustyn64-frontend` forwards the
feature (`--features fast-scheduler`), so `run_frame` calls `run_until_fast` when it
is compiled in and `run_until` otherwise — ADR 0011 §1 holds because the default
build contains neither the branch nor the function. It is deliberately **not** in
the frontend's `full` feature set: `full` is what the release aliases build, and
promoting an alternate execution mode into a shipped artifact is an ADR 0011
decision rather than a build-configuration one.

The differential gate now also drives a **real commercial title** end to end
(`the_fast_path_agrees_while_running_a_real_rom`, `#[ignore]`d and env-gated since
ROMs are never committed): Super Mario 64, 20 compared chunks of 2M ticks,
**13,951,609 instructions retired**, machine state byte-identical at every chunk
boundary. That is the evidence ADR 0011's promotion criteria ask for — the synthetic
reset-vector runs exercise the scheduler but not a real instruction mix, memory
traffic, or RSP/RDP activity.

Three structural choices, made so the later work cannot quietly violate the ADR:

- **A separate entry point, not a branch inside `run_until`.** With the feature
  off the function does not exist, so ADR 0011 §1's "default builds are
  byte-identical" is true by construction rather than by inspection, and the
  accurate path carries no test that exists only for its sibling.
- **No mode field on `System`.** The caller picks the entry point per call, so the
  save-state layout is identical either way and ADR 0011 §4's header marker is not
  yet owed. It falls due the moment the fast path stores state of its own.
- **The differential gate exists and passes before any block executes**
  (`crates/rustyn64-core/tests/fast_scheduler_differential.rs`). A gate written
  after the thing it grades is a gate that has never once been observed to fail
  for the right reason. It compares the **whole serialized machine** rather than a
  list of fields, so a field added to any chip later is covered automatically —
  and it carries its own falsifiability test, which perturbs one run by a single
  CPU edge and requires the comparison to notice.

`master_ticks` equality is asserted separately from the state bytes even though
the byte comparison subsumes it: right state at the wrong tick is the
*correct-but-late* failure ADR 0011 §6 singles out, and it deserves an error
message that says so rather than a byte offset.

The gate runs in CI on the light leg (`cargo test -p rustyn64-core --features
fast-scheduler`), because feature-gated code is invisible to every other job —
CI runs clippy exactly once and not with this feature.

### The hand-off enumeration and the completion witness (ADR 0012 §2)

**A gate that hangs mid-suite is indistinguishable from one that passed**, because
both produce no failure. ADR 0012 §2 closes that, and it makes one demand of the
production code: *"bailing out must not be expressible any other way"*.

`crates/rustyn64-core/src/fastpath.rs` is that enumeration.

- **`BailOut` and `BailOut::ALL` are generated from one list** by a `bail_reasons!`
  macro, so the number of reasons is not stateable independently of the enum. A
  literal expected count would drift the moment the suite grew, which converts the
  witness into decoration. ADR 0012 leaves the *technique* open and asks only for
  that property.
- **The enum is deliberately not `#[non_exhaustive]`.** The gate is an integration
  test and therefore a downstream crate; `#[non_exhaustive]` would force it to carry
  a wildcard arm, which is the exact coverage hole being closed. A new reason with no
  fixture must be a **build failure**.
- **`run_until_fast` returns `FastRunReport { blocks, bailed }`.** A bare `return;`
  in a function with that return type does not compile, so a new exit has to say
  which exit it is. It is a *return value*, not a field and not a hook: a field would
  make ADR 0011 §4's save-state mode marker fall due, and a test-only branch compiled
  into a release build is what ADR 0011 §6 forbids.
- **Today there is exactly one reason**, `PartialPeriodTail`, and it is recorded on
  the tail being non-empty rather than on `run_until` being called — that call
  happens on every path, so reporting it unconditionally would make the witness true
  of every call, which is the same as it being true of none. `fixture_whole_periods_only`
  is the mutation guard for precisely that, and it fails on the unconditional version.

`the_gate_witnesses_its_own_completion` drives crafted boundary fixtures under a
per-fixture and a suite timeout, and asserts three suite-wide conditions that would
otherwise each look like success: every reason in `BailOut::ALL` was reached, the
fast path engaged at all, and some boundary was reached. An **abnormal termination
is a gate failure, never an uncounted exit** — a panic in a fixture arrives as a
disconnected channel and a hang as a timeout, and the two are reported differently
because they need different investigations. Setting `RUSTYN64_GATE_FIXTURE` narrows
the run and **prints that the suite-wide witness did not apply**; the unfiltered run
in CI is what gates.

The boundary fixtures are deliberately cheap and separate from the equivalence
sweeps above: re-running 96 machine pairs to collect coverage would double the suite
for information it already has but does not aggregate. Each fixture still asserts
equivalence for its own crafted state, so coverage is never bought with a run that
grades nothing.

### `fast-exec` is a different feature, and a different predicate

[ADR 0013](adr/0013-fast-execution-mode.md) authorizes a **second** default-off
mode, `fast-exec`, in which the CPU charges documented instruction-granular issue
costs instead of advancing the pipeline per cycle. It is **not** `fast-scheduler`
with more in it, and the two features are independent:

| | `fast-scheduler` | `fast-exec` |
| --- | --- | --- |
| relation to the accurate run | **tick-identical** — same edges, same order, same `master_ticks` | timing deliberately diverges |
| gate predicate | whole serialized state, every tick | architectural state at retirement boundaries, **minus the timing-derived carve-out**; timing divergence measured and bounded |
| `master_ticks` equality | asserted | **not** asserted — it is the quantity being relaxed |
| ADR 0006 | unchanged | **unchanged as implemented.** ADR 0013 *authorizes* per-domain deficit counters for this mode; the implementation has none, and `master_ticks` is still the only incremented counter. The authorization falls due with the deficit-counter scheduler, not before. |

Where both features are enabled, `fast-exec`'s scheduler is the one that runs
(ADR 0013 §1). The whole-state tests above keep their stricter predicate: they grade
a tick-identical path and would be weakened for nothing by relaxing them.

**The carve-out is a rule, not a list, and that is the part that is easy to get
wrong.** A retirement boundary is an *instruction index*, and the two modes reach
instruction N at **different `master_ticks`** — so any state driven by the **clock**
rather than by the **instruction stream** may differ there. `Count` is the visible
instance; the `Count == Compare` timer interrupt and the `Cause.IP` state derived
from it follow, and so does every interrupt whose delivery the scheduler times
(PI, SI, AI, VI, DP, SP through MI). `Compare` is software-written and stays in the
predicate; what leaves it is *when the comparison fires*.

What that does **not** license: deadlines stay in `master_ticks` and devices still
raise at the same tick, so once both modes have passed a deadline its *effects* —
the DMA'd bytes, the register writes, the eventual interrupt — must agree. The
predicate is therefore anchored in **time as well as instructions**. ADR 0013 §4
is authoritative.

`System::run_until_exec` is the entry point, and it inverts who sets the pace: the
**CPU executes one instruction**, reports what it cost in `PCycles`,
`master_ticks` advances by that many CPU periods, and the RCP runs every one of its
edges in the span that just elapsed. The CPU therefore no longer lands on a derived
edge — which is the relaxation, stated plainly.

**ADR 0006 still holds.** `master_ticks` is still the only incremented counter and
every other position is still derived from it; what changed is how far it moves per
step, not who owns it.

Three consequences worth knowing before reading the code:

- **It can land past `target`**, by at most one instruction's cost, because a cost
  is only known after the instruction has run. Nothing drifts — the next call's
  target is absolute — and this is the timing divergence ADR 0013 §4 requires to be
  measured rather than eliminated. Measured: **+1.04%** over 120 frames
  (`docs/accuracy-ledger.md` C-16).
- **A halted CPU advances on RCP edges.** A failed real-PIF boot checksum freezes
  the CPU while the RCP keeps running; with no instruction to time the advance
  with, the loop steps to the next RCP edge. Deliberately *not* a bail-out:
  ADR 0011 §6 sanctions a test-only seam only where a boundary genuinely cannot be
  reached, and this one can simply be handled.
- **`fast-exec` therefore adds no new `BailOut` variant.** Saying so is better than
  inventing an exit to justify the machinery; the enumeration exists so a *real*
  one cannot be added silently.

Measured **1.53x** on a real frame (`docs/performance.md`).

This is **`fast-exec` policy, not a hardware claim.** The VR4300 has one timeline,
so the hardware has no opinion about how two emulated modes should be compared;
there is no manual section to cite, and citing one would make a policy read as a
measurement. What *is* owed to `docs/accuracy-ledger.md` is the measured divergence
bound, and it falls due when there is something to measure.

Because the timer can interrupt at a different instruction, **the two modes'
instruction streams may legitimately diverge**. The `fast-exec` gate therefore has
three outcomes rather than two — agreement (pass), **stream divergence** (reported,
comparison ends for that fixture, counts toward nothing), and disagreement
(failure) — and it must never absorb the middle one, which would leave it comparing
two unrelated runs while reporting agreement. ADR 0013 §4 is authoritative.

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
  observes the halt clear at the modeled cycle.
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
