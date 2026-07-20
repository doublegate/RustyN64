# Testing strategy — RustyN64

**References:** `ref-docs/research-report.md` §7 (the test-ROM oracle), §4
(ParaLLEl-RDP fuzz); `crates/rustyn64-test-harness/`; ADR 0003; `docs/STATUS.md`.

## The principle

**Test ROMs are the spec.** When the docs and a passing test ROM disagree, the
ROM wins — the docs get updated. For any accuracy work: pin the failing ROM
expectation FIRST, then implement until it passes. The N64 has a mature,
self-validating oracle analogous to blargg/kevtris/AccuracyCoin for the NES
(`ref-docs/research-report.md` §7).

## The oracle (what "accurate" means)

| Suite | Lang | Role | Gate |
|---|---|---|---|
| **n64-systemtest** | Rust | CPU / COP0 / TLB / RSP, hardware-verified, self-judging | "Failed: 0" (strict) |
| **ParaLLEl-RDP fuzz** | — | RDP bit-exactness vs Angrylion (~150 tests) | exact match (strict) |
| **Dillonb n64-tests** | C/asm | targeted CPU/RSP, hardware-verified | result-code pass |
| **PeterLemon/N64** | asm | bare-metal CPU/RSP/RDP/audio demos | visual/behavioural regression |
| **Commercial dumps** | — | custom-microcode + game regression | screenshot/`.snap` (gitignored ROMs) |

n64-systemtest is **self-judging** — it decides pass/fail itself and finishes with
a line like "Done! Tests: 262. Failed: 0", so no image comparison is needed for
the CPU/COP0/TLB/RSP gate (`ref-docs/research-report.md` §7).

## The five layers

The harness (`crates/rustyn64-test-harness/`) reuses the RustyNES harness SHAPE,
retargeted to the N64:

### Layer 1 — unit (per crate)

Each chip crate has in-crate tests against a tiny null bus; target >90% on the
chip crates. The chip crates are fuzzable in isolation because of the
one-directional graph (`docs/architecture.md` fact 3).

### Layer 2 — CPU/RSP golden-log differ

`golden::GoldenLogDiffer` captures a `TraceRecord { pc, gpr, cycle }` per retired
instruction and diffs the stream against a golden log by PC, record-for-record;
the first divergence (`GoldenDiff::Diverged { index, expected_pc, actual_pc }`)
fails. The golden source is **stubbed** (`load_golden_stub`) until a reference
VR4300 trace of n64-systemtest (captured on cen64/ares) is committed to
`tests/golden/` (`docs/cpu.md`, `docs/rsp.md`).

### Layer 3 — test-ROM corpus

`runner::run_until_complete` steps a `System` until a completion sentinel (the
n64-systemtest result protocol) and asserts the result code. Drives
n64-systemtest, Dillonb, and PeterLemon ROMs. Behind the `test-roms` feature
(committed CC0 / homebrew ROMs).

### Layer 4 — accuracy battery

`accuracy::AccuracyScorer` runs a battery of named probes and reports a pass-rate
(`AccuracyReport`) — the AccuracyCoin-equivalent. The battery itself is **stubbed**
until the probes are authored. Gate: ≥90% by v1.0, 100% the goal; hard residuals
deferred to the ADR 0002 refactor, documented not point-fixed.

### Layer 5 — visual golden + screenshots

`frame::frame_hash` + `FrameComparison` compare a rendered frame against a
committed golden (`tests/golden/`, `screenshots/`). Commercial ROMs live in
`tests/roms/external/` and are **gitignored**; only the screenshots/`.snap`
baselines are committed. The RDP/VI bit-exactness is graded here against an
Angrylion reference scan-out.

## Feature flags

- `test-roms` — committed CC0/public-domain suites + the integration tests.
- `commercial-roms` — the local-dump oracle (custom-microcode + 60-game-style
  regression); ROMs gitignored, only screenshots/`.snap` committed.

**Never commit commercial Nintendo ROMs** (`docs/architecture.md`; project
policy).

## No honesty gate (ADR 0003)

N64 boards are **NOT tiered** (`boards_tiered = false`): there is one cart model
parameterized by save type + CIC, not hundreds of mappers. So — unlike RustyNES's
`mapper_tier_honesty` test — there is no Core/Curated/BestEffort tiering and no
honesty-gate test here. The accuracy oracle is simply n64-systemtest pass/fail +
the RDP fuzz suite (ADR 0003).

## CI gates (target)

All must be green:

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `cargo test --workspace --features test-roms` (the committed suites)
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`
- `cargo build -p rustyn64-core --target thumbv7em-none-eabihf --no-default-features`
  (the chip stack stays `no_std` + `alloc`)
- markdownlint (pinned cli v0.39.0; see project config)

## Test plan summary by subsystem

- **CPU** — golden-log + n64-systemtest CPU/COP0/TLB/exception categories
  (`docs/cpu.md`).
- **RSP** — n64-systemtest RSP category + Dillonb RSP tests + per-op vectors
  (`docs/rsp.md`).
- **RDP/VI** — the ParaLLEl-RDP fuzz suite + PeterLemon demos + VI golden frames
  (`docs/rdp.md`).
- **Audio** — AI timing units + RSP audio-microcode integration + PeterLemon audio
  (`docs/audio.md`).
- **Cart** — ROM-format round-trip + save oracle + SaveTest-N64 + PIF/SI
  categories (`docs/cart.md`).

## Open questions

- When a reference VR4300 golden trace gets captured (cen64/ares) and committed to
  unblock Layer 2 (`crates/rustyn64-test-harness/src/golden.rs` TODO).
- The exact accuracy-battery probe set (Layer 4) — author alongside Phase 1.
