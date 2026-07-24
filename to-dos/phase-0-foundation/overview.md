# Phase 0 — Foundation

## Goal

Stand up the Cargo workspace with every crate skeleton compiling and CI green, and acquire and
organise the material the accuracy work will be graded against: the hardware reference, the
reference emulators, and the test-ROM corpora. By the end of this phase the repository builds,
lints clean, publishes documentation, and has an oracle staged — even though no chip executes a
single instruction.

**Status: COMPLETE.** All exit criteria met; the workspace builds, lints, and publishes docs, and
the oracle corpus is staged with licence tiers enforced. One criterion (the ADR 0001 3:2 clock)
was later superseded by ADR 0006 in Phase 1 — noted inline below.

## Exit criteria

- [x] `cargo build --workspace` and `cargo test --workspace` succeed on Linux, macOS, and
      Windows. *(42 tests across 21 suites; the CI matrix is green on all three.)*
- [x] `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings`
      clean under `pedantic` + `nursery`. *(clippy reports no issues.)*
- [x] `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` clean, and the API reference
      publishes. *(live at <https://doublegate.github.io/RustyN64/>.)*
- [x] The chip stack cross-compiles to `thumbv7em-none-eabihf` with `--no-default-features`.
      *(the `no_std` CI job is green.)*
- [x] The Bus owns all mutable state; power-on phase is seeded. *(Met at Phase 0 with the
      ADR 0001 3:2 fractional master clock; **that timebase was superseded in Phase 1 by ADR
      0006's canonical integer-divisor 187.5 MHz clock** — the 3:2 accumulator and its
      `fractional_divisor_holds_3_to_2` test no longer exist, only VI/AI keep a fractional
      accumulator now. `reset_preserves_phase` still passes.)*
- [x] ROM-format detection and byte-order normalisation for `.z64`/`.n64`/`.v64`.
      *(`rustyn64-cart`: header parse plus the `SaveType`/`Cic`/`RomFormat` enums.)*
- [x] The hardware reference is available offline. *(`n64brew_wiki/` — 324 pages, 96 media,
      rebuilt by `scripts/mirror_n64brew_wiki.py`.)*
- [x] Reference emulators cloned, each with a verified licence and a vendor-vs-study decision.
      *(11 clones in `ref-proj/`; see its `README.md`.)*
- [x] Test-ROM corpora staged in licence tiers, with commercial ROMs unable to enter the tree.
      *(committed: n64-systemtest. External: krom, dillon-n64-tests, 240p, commercial. Three
      independent guards, each verified against a real `git add -f` bypass.)*
- [x] A golden VR4300 instruction trace captured from cen64 or ares and committed to
      `tests/golden/`. **RESOLVED (T-02-005):** `tests/golden/n64-systemtest.log` is a real
      50,027-record ares capture, and `crates/rustyn64-test-harness/tests/golden_log.rs` is the
      committed 0-diff gate (Phase 1). Originally deferred here because the trace is only
      meaningful once a CPU exists to diff against.

## Scope

In-scope:

- The Cargo workspace: ten crates, `[workspace.lints]`, the pinned 1.96 toolchain, the release
  profile.
- CI: fmt, clippy, test, rustdoc, the `no_std` cross-build, the `test-roms` battery, and the two
  repository guards, split light/full to control cost.
- The `Bus` and the fractional scheduler — the load-bearing architecture later phases fill in.
- ROM-format detection and header parsing (not PI DMA, which is Phase 5).
- The offline hardware reference, the study clones, and the test-ROM corpora.
- Release and Pages automation.

Out-of-scope (deferred to later phases):

- Any instruction decode or execute: the CPU, RSP, and RDP `tick` methods stay LLE-shaped
  no-ops.
- PI/SI DMA, the CIC handshake, and the save backends (Phase 5).
- Anything that actually runs a test ROM: the harness scaffolding exists, but
  `run_until_complete` always returns `Timeout` until Phase 1.

## Sprints

- [Sprint 1 — Workspace, CI, and the architecture skeleton](sprint-1-workspace-ci.md) —
  the workspace, the quality gates, the Bus, and the scheduler.
- [Sprint 2 — Reference corpus and repository guards](sprint-2-reference-corpus.md) —
  the wiki mirror, the study clones, the test-ROM tiers, and the never-commit guards.

## Dependencies

None. This is the first phase.

## Risks

- **A green CI that means nothing** — stubs are no-op `TODO(...)` bodies rather than `todo!()`
  panics, so nothing fails loudly and "CI green" can be misread as "it works". Mitigated by
  `docs/STATUS.md` stating the distinction explicitly and by its accuracy table carrying an
  "oracle available?" column separate from status.
- **Licence contamination from the reference clones** — several are copyleft or worse
  (angrylion is non-commercial MAME-licensed despite shipping no `LICENSE` file). Mitigated by
  `ref-proj/README.md` classifying every clone vendor-ok or study-only, and by the standing rule
  that the permissive set is consulted first.
- **A commercial ROM reaching the repository** — 1.5 GB of copyrighted dumps sit in the working
  tree. Mitigated by three independent guards; `.gitignore` alone is insufficient because
  `git add -f` bypasses it silently.
- **Reference drift** — the wiki mirror and the study clones are snapshots that will age.
  Mitigated by recording every upstream commit hash and by `--refresh` doing revision-based
  incremental updates.

## Reference docs

- [docs/architecture.md](../../docs/architecture.md) — the eight load-bearing facts.
- [docs/scheduler.md](../../docs/scheduler.md) — the fractional master clock.
- [docs/testing-strategy.md](../../docs/testing-strategy.md) — the oracle and the five layers.
- [docs/STATUS.md](../../docs/STATUS.md) — the authoritative current state.
- [docs/adr/0001-master-clock-lockstep-scheduler.md](../../docs/adr/0001-master-clock-lockstep-scheduler.md)
- [docs/adr/0004-determinism-contract.md](../../docs/adr/0004-determinism-contract.md)
