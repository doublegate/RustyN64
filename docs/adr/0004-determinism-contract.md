# ADR 0004 — The determinism contract

## Status

Accepted.

## Context

Save-states, regression tests, TAS replay, and netplay rollback all require reproducibility.

Each of these fails differently without it, and all four fail *silently*:

- **Save-states** — restoring a state must resume the exact machine that was saved. A single
  unreproduced bit of hidden state (a phase accumulator, a DMA countdown) makes the restored
  run diverge from the saved one, usually minutes later and far from the cause.
- **Regression tests** — a golden framebuffer or audio hash is only meaningful if the same
  input yields the same output every run. Non-determinism turns a regression suite into a
  flake generator, and the usual response is to weaken the assertion, which destroys the
  suite's value.
- **TAS replay** — an input sequence must reproduce the same frames on every playback and on
  every machine, or the recording is not a recording.
- **Netplay rollback** — peers re-simulate the same frames from the same inputs. Any
  divergence desyncs the session, and rollback amplifies the problem because the same frames
  are executed repeatedly.

The N64 makes this harder than the 8- and 16-bit consoles RustyNES and RustySNES target:
three asynchronous engines (VR4300, RSP, RDP) share one memory pool, so the *interleaving* is
part of the machine state, not an implementation detail. The power-on phase relationship
between the CPU and the RCP is genuinely indeterminate on real hardware, which is exactly the
kind of thing an emulator is tempted to model with a live RNG.

## Decision

Same seed + ROM + input ⇒ bit-identical framebuffer + audio. Power-on phase alignment is a
SEEDED PRNG; reset preserves it. No system time / thread scheduling / OS RNG in the core.
Rate control + run-ahead live in the frontend (a resampler stage / snapshot-restore
orchestration), never the core synthesis.

Concretely:

- Power-on CPU/RCP phase comes from a **seeded SplitMix64** (`rustyn64-core::scheduler`), so
  hardware's genuine indeterminacy is modelled as a *parameter* rather than as live entropy.
  The seed is part of the machine's identity and of any save-state.
- Reset preserves phase alignment (pinned by the `reset_preserves_phase` unit test).
- The core never reads wall-clock time, OS entropy, or thread scheduling order, and never
  iterates an unordered collection where the order affects output.
- Anything that legitimately needs real time — frame pacing, dynamic rate control,
  run-ahead, netplay rollback orchestration — lives in `rustyn64-frontend`, outside the
  simulated machine.

## Status of enforcement

**Exercised as of 2026-07-20** (T-11-007), not merely specified. Four tests in
`crates/rustyn64-test-harness/tests/determinism.rs`:

- The same seed and ROM produce a **bit-identical machine** — full register file,
  `HI`/`LO`, PC, all three cycle positions, and a content hash of the whole of
  RDRAM. Deliberately the entire machine rather than a summary: a partial hash
  can hide a divergence in a field that only later leaks into the hashed region.
  Repeated eleven times, because a wall-clock or entropy dependency is more
  likely to surface intermittently than on the very next run.
- **Different seeds produce different machines**, so the contract is not vacuous.
  A build that ignored the seed entirely would satisfy the first test.
- **Reset returns to a reproducible state** regardless of what ran before it.
- **A source-level guard** rejects `std::time`, `SystemTime`, `Instant::now`,
  `getrandom`, `thread::spawn`, `HashMap` and `HashSet` anywhere in the core
  crates. This is deliberately *not* behavioural: those dependencies are often
  intermittent, so a run-twice test can pass for months before the first
  divergence. The guard fails on the commit that introduces one.

All are mutation-tested: ignoring the seed fails the second, and naming a banned
construct in any core crate fails the fourth with a precise file and line.

## Consequences

### Positive

- Save-state round-trip, golden-frame regression, TAS replay, and rollback netplay all become
  achievable from one property instead of four separate mechanisms.
- Bugs are reproducible from a seed plus an input log, which is what makes accuracy work
  tractable at all: a divergence can be bisected instead of chased.
- The `emu-thread` frontend feature stays safe. Emulation runs on a dedicated thread, but
  because the core takes no input from the scheduler, thread timing cannot change output.

### Negative / costs

- Optimisations that trade reproducibility for speed are permanently off the table in the
  core: no wall-clock-driven frame skipping, no "catch-up" that varies with host load, no
  parallelising the three engines against each other. ADR 0006's single-timeline lockstep (superseding ADR 0001) is
  partly a consequence of this contract.
- Every new piece of hidden state must be reachable by the save-state serialiser, or it
  becomes a silent divergence source. This is an ongoing tax on every subsystem, not a
  one-time setup.
- The contract is only as good as its test. Today there is **no determinism regression test** —
  no two-run comparison, no AV hashing against a golden. `frame_hash` exists but nothing calls
  it against a stored baseline, so the contract is specified and unexercised
  (`docs/STATUS.md`). It cannot be exercised meaningfully until the CPU executes instructions,
  which makes this a Phase 1 follow-on rather than a Phase 0 gap.

### Risks

- The most likely violation is accidental: an `alloc`-ordering dependency, a `HashMap`
  iteration, or a future backend (wgpu-compute RDP) whose floating-point results differ across
  drivers. The software reference RDP stays the oracle precisely so a nondeterministic
  accelerator cannot become the definition of correct (ADR 0002).
- ADR 0005's future sub-cycle bus-timing refactor is expected to break byte-identity with
  earlier versions. That is an accepted, announced, one-time break — not a relaxation of this
  contract, which continues to hold *within* any given version.
