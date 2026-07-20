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

The first four suites are cloned for study under `ref-proj/` (gitignored; see
`ref-proj/README.md` for per-repo licence terms before copying anything — the
Angrylion reference is non-commercial MAME-licensed despite shipping no
`LICENSE` file, so it is compare-outputs-only). The commercial corpus is staged
locally at `tests/roms/external/commercial/` — see **Layer 5** below.

### Corpus tiers and what is actually staged

`tests/roms/` is split into a **committed tier** (permissively licensed, in the
git tree) and an **external tier** (gitignored, local only). Promotion to the
committed tier is a licensing decision, not a convenience — full rules and
provenance in `tests/roms/README.md`.

| Corpus | Licence (verified) | Tier | Staged |
|---|---|---|---|
| `n64-systemtest/` | MIT | **committed** | 1 ROM, 2.7 MB — built from source |
| `external/krom/` | Unlicense (public domain) | external (size) | 196 ROMs, 182 MB |
| `external/dillon-n64-tests/` | **none** | external (no grant) | 26 ROMs, 38 MB |
| `external/commercial/` | copyrighted | external (never) | 66 ROMs, 1.5 GB |
| `external/240p/` | GPL-2.0 | external (copyleft) | not yet fetched |

Absence of a licence is **not** public domain — it means no grant, so
`dillon-n64-tests` is run-only and never redistributed. `krom` is public domain
and *could* be committed; it stays external purely on footprint.

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

#### The commercial corpus (`tests/roms/external/commercial/`)

A curated local oracle set, staged per-developer from personally-owned
cartridge dumps. **Never committed** — see the guards below. The full per-ROM
manifest, with the rationale for every title, is in that directory's
`README.md`.

Organised by **save type**, one folder per cart backend, because ADR 0003
defines the N64 cart as "one cart model parameterized by save type + CIC +
region" — so save type is what `rustyn64-cart` actually implements, and it is
mutually exclusive (a ROM has exactly one save backend):

| Folder | Backend | Exercises |
|---|---|---|
| `eeprom-4k/` | EEPROM 512 B | the most common backend |
| `eeprom-16k/` | EEPROM 2 KB | same protocol, larger address space (off-by-one trap) |
| `sram/` | SRAM 32 KB | battery-backed, PI-mapped |
| `flashram/` | FlashRAM 128 KB | command/status state machine — the most complex backend |
| `controller-pak/` | none on cart | joybus/SI Controller Pak path |

Non-exclusive traits are recorded in the manifest rather than duplicating
32–64 MB files across folders. The corpus is selected to cover, beyond the save
backends:

- **Custom RSP microcode** — the payoff for choosing LLE over HLE (ADR 0002).
  Both canonical HLE-breaking families are included: Factor 5 (Rogue Squadron,
  Battle for Naboo, Indiana Jones) and Boss Game Studios (World Driver
  Championship, Top Gear Rally 2). If those render, the LLE RSP is working.
- **Expansion Pak** (8 MB RDRAM) — Donkey Kong 64, Perfect Dark, Majora's Mask,
  Turok 2.
- **Exotic joybus peripherals** — the VRU (Hey You, Pikachu!) and the Transfer
  Pak (both Pokemon Stadium titles).
- **Commercial popularity** — an emulator that passes hardware tests but breaks
  the games people actually play is not much use, and regressions surface in
  popular titles first. Every N64 million-seller available locally is staged.
- **ROM-size spread**, 4 MB to 64 MB, to exercise PI DMA and the cart address
  space at both ends.

Save types are **not guessed**. Each ROM is hashed and matched by MD5 against
the mupen64plus catalogue
(`ref-proj/simple64/mupen64plus-core/data/mupen64plus.ini`), and re-hashed after
extraction to confirm the staged copy is byte-identical.

ROMs are **copied**, never moved, out of the developer's own library, matching
how RustyNES and RustySNES stage their corpora.

## Feature flags

- `test-roms` — committed CC0/public-domain suites + the integration tests.
- `commercial-roms` — the local-dump oracle described under Layer 5
  (`tests/roms/external/commercial/`); ROMs gitignored, only screenshots/`.snap`
  committed. Tests behind this flag must **skip, not fail**, when the corpus is
  absent: no contributor other than the ROM owner can have it.

Both flags currently gate zero code — no `cfg(feature = ...)` exists for either
yet, so `--features test-roms` runs exactly the same tests as a bare
`cargo test --workspace` (`docs/STATUS.md`).

### Never commit commercial ROMs

Project policy (`docs/architecture.md`, `CONTRIBUTING.md`, `NOTICE`), enforced by
three independent guards rather than by convention:

1. **`.gitignore`** — ignores `*.z64` / `*.n64` / `*.v64` / `*.ndd` *everywhere*,
   plus all of `/tests/roms/external/`. Fail-closed: when redistributable
   CC0/homebrew ROMs land, add a negation for that one path rather than relaxing
   the global rule.
2. **`scripts/check_no_roms.sh`** — a pre-commit hook over the staged file list,
   closing the two gaps `.gitignore` cannot: `git add -f` bypasses ignore rules
   silently, and a ROM saved under an unexpected name is never matched by
   extension (a 2 MB size ceiling catches those).
3. **the `no-commercial-roms` CI job** — server-side and unskippable, scanning
   the whole tracked tree, so it also catches anything committed earlier. A
   contributor can skip the local hook with `--no-verify`; they cannot skip this.

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
- `bash scripts/check_no_roms.sh` (the `no-commercial-roms` job — no ROM and no
  file over 2 MB is tracked)
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
