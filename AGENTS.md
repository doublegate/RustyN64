<!-- Managed by Master-Claude. Universal rules come from the imported/inlined core.
     Edit only inside the MC-PROJECT block; mc-sync overwrites everything else. -->
<!-- mc-core: 0.1.0 | mode=import | lang=rust -->
# AGENTS.md — RustyN64

@/home/parobek/.claude/master-core/AGENTS.base.md
@/home/parobek/.claude/master-core/lang/rust.md
@/home/parobek/.claude/master-core/modules/10-commits-and-versioning.md
@/home/parobek/.claude/master-core/modules/20-testing-and-accuracy.md
@/home/parobek/.claude/master-core/modules/30-quality-gates.md
@/home/parobek/.claude/master-core/modules/40-docs-and-adrs.md
@/home/parobek/.claude/master-core/modules/50-architecture-patterns.md
@/home/parobek/.claude/master-core/modules/60-security.md
@/home/parobek/.claude/master-core/modules/70-release-ceremony.md
@/home/parobek/.claude/master-core/modules/80-phase-sprint-workflow.md
@/home/parobek/.claude/master-core/modules/90-multi-language-integration.md
@/home/parobek/.claude/master-core/modules/95-named-pattern-library.md

<<< MC-PROJECT-START >>>

## Project: RustyN64

Guidance for agents working in RustyN64. Universal rules come from the shared core above;
only project-specific facts and deliberate overrides live here.

## What this is

RustyN64 is a cycle-accurate Nintendo 64 emulator in Rust at the ares / CEN64 / ParaLLEl bar.
Architecture (the load-bearing facts — read `docs/architecture.md`):

- **One canonical clock: `MASTER_HZ = 187_500_000`** (ADR 0006, supersedes 0001). Every
  emulated domain is an **integer divisor** of it — CPU every 2 ticks (93.75 MHz), RCP every 3
  (62.5 MHz), COP0 `Count` every 4 (half PClock), SI every 12, PIF every 96. **`master_ticks`
  is the only counter that is ever incremented**; `cpu_cycles()`/`rcp_cycles()` are derived
  accessors, and a residue invariant test fails if any position becomes independent. A
  fractional accumulator survives only for VI/AI, which genuinely are not rational multiples.
  "Lockstep" means not-catch-up: an RCP event is visible to the very next CPU step.
- **The CPU is a cycle-accurate 5-stage pipeline** (IC/RF/EX/DC/WB — ADR 0007), four
  inter-stage latches advanced one PClock per step in **reverse order WB→DC→EX→RF→IC**, which
  is what makes the latching implicit. `in_delay_slot` rides in the latch, never global.
- **The Bus owns everything mutable** (`rustyn64-core::Bus`); the CPU borrows `&mut Bus`.
  Chips are stepped via the `core::mem::take` split-borrow trick; each sees only a narrow trait.
- **The crate graph is one-directional**, with exactly ONE permitted chip→chip edge:
  `rustyn64-rdp` → `rustyn64-cart`, solely to borrow the `RdramBus` trait. Any other cross-chip
  dependency breaks the fuzz-in-isolation invariant. Downstream consumers depend on
  `rustyn64-core` (which re-exports the chip types), never on a chip crate directly.
- **LLE, never HLE, in the core.** The RSP and RDP execute the real instruction stream /
  command list. HLE may only ever exist behind an off-by-default flag. Audio falls out free
  (the RSP audio microcode runs on the same LLE core) — there is no per-game audio HLE.
- **Board logic lives in the cart crate** (default-no-op trait hooks). Unlike RustyNES there is
  **no board tiering and no honesty gate** — the N64 has one cart model parameterized by save
  type + CIC + region (ADR 0003).
- **Determinism is a hard contract** (seed+ROM+input ⇒ bit-identical AV; frontend owns rate
  control). Power-on phase alignment comes from a seeded SplitMix64; reset preserves it.
- **Test ROMs are the spec**; pin the failing ROM first, then implement.
- **Additive features are default-off** so shipped/native/no_std/wasm stay byte-identical — with
  one acknowledged exception: `rustyn64-frontend`'s `emu-thread` is default-ON. Note there is no
  wasm build in CI yet, so the wasm half of that claim is aspirational.

## Current state (read `docs/STATUS.md` first)

**Phase 1 in progress; tagged release still v0.1.0.** The **VR4300 executes instructions**: the
canonical 187.5 MHz clock (ADR 0006), the five-stage pipeline (ADR 0007), the MIPS III integer
set, COP0, the TLB + micro-ITLB, the exception model, interrupts, `CACHE`, COP1 (control,
register file, `ADD`/`SUB`/`MUL`/`DIV`, `ABS`/`MOV`/`NEG`, the compares, the conversions and
enabled FP traps), and **PI DMA** — the last pulled forward from Phase 5 because n64-systemtest
loads its own ELF through it.

FP arithmetic runs on a **soft-float core** (`crates/rustyn64-cpu/src/softfloat.rs`), not on
Rust's `f32`/`f64` operators. That is not gratuitous: the native operators discard the exact
pre-rounding result, so `inexact`/`underflow` cannot be reported and `FCSR.RM` cannot be honoured.
It is verified bit-for-bit against those same operators in round-to-nearest over ~100k cases —
they are the independent oracle, which is why the module implements *IEEE* behaviour and leaves
the VR4300's refusal to produce subnormals as a separate layer.

**The RSP, RDP and AI are still LLE-shaped stubs.** Do not assume any chip *other than the CPU*
executes anything. A green `cargo test` still does not mean a subsystem works — check
`docs/STATUS.md`.

**Phase 1's exit criterion is not met.** The criterion is `Failed: 0` on the **CPU/COP0/TLB
categories** (`to-dos/VERSION-PLAN.md` §v0.2.0) — *not* suite-wide. n64-systemtest currently
reports **42 failing assertions in those categories**, against 455 suite-wide. The RSP's 291 are
explicitly **Phase 2's** criterion (§v0.3.0), and cart/PIF/RDP belong to later phases still.
`docs/STATUS.md` said "CPU/COP0/TLB/RSP" for a while, which conflated the two — VERSION-PLAN is
authoritative for cut criteria.

Do **not** tag v0.2.0 until that number is 0; it is an oracle result, not a self-assessment.
Of the 60: COP1 10 (`CVT` edge cases, `MUL`/`DIV` residue), and ~64 across COP0 registers, the
TLB, privilege/user-mode access, and the odd-index `FR=0` register tests. `BC1F`/`BC1T` decode to
`Cop1Unimplemented` — a silent no-op, so an FP branch never redirects.

## Where things live

- `crates/rustyn64-cpu/` — NEC VR4300 (MIPS R4300i) (cpu)
- `crates/rustyn64-rsp/` — RSP (Reality Signal Processor) (coprocessor)
- `crates/rustyn64-rdp/` — RDP (Reality Display Processor) (video)
- `crates/rustyn64-audio/` — AI + RSP audio microcode (audio)
- `crates/rustyn64-cart/` — PI cart + PIF/CIC + saves (cart)
- `crates/rustyn64-core/` — Bus + scheduler · `crates/rustyn64-frontend/` — egui shell (binary `rustyn64`)
- `crates/rustyn64-test-harness/` — the accuracy oracle
- `crates/rustyn64-netplay/` — rollback netplay orchestration (frontend-side) · empty stub today
- `crates/rustyn64-cheevos/` — RetroAchievements FFI (later, off by default) · empty stub today
- `docs/` — the spec (update in the same PR as code); `docs/STATUS.md` = single source of truth;
  `docs/adr/` — ADRs. `ref-docs/` — immutable research. `ref-proj/` — study clones (gitignored).
  **Read `ref-proj/README.md` before copying anything from a reference emulator.** RustyN64 is
  MIT OR Apache-2.0; only ares, cen64, parallel-rdp, parallel-rsp (MIT arm), n64-systemtest,
  libdragon, and PeterLemon-N64 are permissive enough to vendor. simple64/gopher64 (GPLv3),
  n64-tests (no licence), and angrylion-rdp-plus (**non-commercial MAME licence, despite having
  no `LICENSE` file**) are study-only — compare their outputs, never their source.
- `n64brew_wiki/` — gitignored offline mirror of the N64brew Wiki, the primary hardware
  reference. Search `n64brew_wiki/markdown/`; browse `n64brew_wiki/html/`. Rebuild or update
  with `python3 scripts/mirror_n64brew_wiki.py [--refresh]`. CC BY-SA 4.0 — attribute if quoted.
- **`n64brew_wiki/images/VR4300-Users-Manual.pdf` is the primary CPU timing oracle** — the full
  655-page NEC manual, with every pipeline/cache/FPU/interlock timing table. **Extract with
  `mutool draw -F txt`; `pdftotext` fails on it** and `file` misreports it as 27 pages. Cite as
  `UM §x`. Corrections it forced on the immutable `ref-docs/research-report.md` live in
  `ref-docs/2026-07-20-vr4300-timing-supplement.md`, which wins where they disagree.
- `to-dos/ROADMAP.md` — planning entry point; tickets `T-PS-NNN`.

## Build / test / lint

Overrides the Rust overlay's generic clippy line: this workspace must **NEVER** use
`--all-features` — mutually-exclusive backend features can't resolve. CI uses explicit sets.

```bash
cargo check --workspace && cargo test --workspace
cargo test --workspace --features test-roms
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings   # NEVER --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo build -p rustyn64-core --target thumbv7em-none-eabihf --no-default-features   # no_std gate
```

Workspace lints: clippy `pedantic` + `nursery` at warn, `missing_docs` warn, `unsafe_code` warn.
Edition 2024, toolchain pinned to 1.97. New public items need rustdoc or the doc gate fails.
Feature flags: `test-roms` (committed CC0/homebrew suites) and `commercial-roms` (local dumps;
ROMs gitignored under `tests/roms/external/`, only screenshots/`.snap` committed). **Both gate
zero code today** — no `cfg(feature = ...)` exists for either, so `--features test-roms` runs
exactly the same tests as a bare `cargo test --workspace`. Same for the per-crate `std` features:
every chip crate is unconditionally `#![no_std]`, so `--no-default-features` is currently a no-op.
markdownlint runs via `.pre-commit-config.yaml` (cli pinned **v0.49.1**) — it is NOT in any CI job.
**So any change touching `*.md` must run it locally**, or the only gate it has never runs at all:

```bash
pre-commit run markdownlint --all-files      # uses the pinned rev; a newer local
                                             # binary reports ungated rules
```

**Never pipe a gate into `tail`/`grep`/`head` when its exit status is what decides the next step.**
A pipeline reports the *filter's* status, so a failing gate reads as passing. This has let two
clippy-red commits through and hidden a rustdoc failure inside an `&&` chain that then printed
`ALL-GATES-OK`. Put the whole gate in one conditional with no filters inside it, and silence noise
with `>/dev/null` (which preserves status) rather than a pipe. The interactive shell here is
**fish**, which has no `pipefail`:

```fish
if cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
    git commit -F msg.txt
else
    echo "GATES FAILED"
end
```

**CI is split light/full:** ordinary feature PRs get only fmt/clippy/test/rustdoc/no_std on
ubuntu. The `test-roms` job and the macOS/Windows matrix run ONLY on push-to-main, the merge
queue, `release/*` PRs, dispatch, and a weekly cron. CI runs clippy exactly once — there are no
per-feature clippy jobs, so feature-gated code is currently unlinted.

Linux frontend needs system deps (wgpu/winit/cpal). Arch/CachyOS:
`sudo pacman -S --needed libxkbcommon wayland alsa-lib systemd-libs`

## Shipping: every change goes through a PR

**Never push to `main`.** All work lands via a pull request so the review bots
(Copilot, Gemini Code Assist) can comment. The full ceremony, in order:

1. **Branch** off `main` — `<type>/<short-desc>`. Commit as normal (conventional
   commits, one logical change).
2. **Open the PR.** Body states motivation, the concrete changes, and which gates
   were run locally.
3. **Wait for CI *and* the bots.** Bots comment asynchronously and usually land
   after the first CI leg finishes.
4. **Bot-comment ceremony — every comment gets adjudicated, none are ignored:**
   - Read each comment and decide: **adopt** (it is right), **reject** (it is
     wrong or does not apply here), or **adopt with modification**.
   - Apply the adopted changes as follow-up commits on the branch.
   - **Reply to every comment individually** with the decision and the reasoning.
     A rejection needs a real reason — "this contradicts ADR 0006 because …", not
     "won't fix". A bot being wrong about this codebase is common and worth saying
     why, since the reasoning is what a human reviewer reads later.
   - **Mark each comment resolved** once it has been adjudicated and answered.
5. **Re-run to green.** Pushing fixes re-triggers CI; wait for the final green,
   not the first one.
6. **Squash-merge** into `main` once green and the ceremony is complete —
   subject to the review authority below.
7. **Verify, then delete the branch.** Confirm the squashed commit is on `main`
   and the tree matches before deleting — deletion is only safe after the change
   is provably incorporated. Auto-delete-on-merge is deliberately OFF so this
   check cannot be skipped.

### Who approves

`CONTRIBUTING.md` §Code review requires one reviewer minimum, and two for changes
to `docs/architecture.md` or cross-subsystem refactors. That rule still stands and
this section does not override it. Reconciling the two:

- The **repo owner is the reviewer of record.** This is a single-maintainer
  repository; there is no reviewer pool, and pretending otherwise would make the
  requirement ceremonial. (Public visibility changes who can *read* it, not who
  reviews.)
- Authorization for an agent to run the ceremony and squash-merge is **standing,
  not per-PR** — granted once, and it covers routine work: implementing a decided
  ticket, docs, dependency bumps, test additions.
- **Stop and ask instead of merging** when any of these is true. These are the
  cases where standing authorization does not reach:
  - CI is red for a reason not understood, or a fix would mean weakening a gate.
  - A bot comment cannot be confidently adjudicated.
  - The change touches `docs/architecture.md`, or is a cross-subsystem refactor
    (`CONTRIBUTING.md` asks for two reviewers).
  - It would break byte-identity, save-state, or determinism guarantees
    (ADR 0004 / ADR 0005) — those are announced-in-advance MAJOR events.
  - It supersedes an ADR, or contradicts one without superseding it.
  - It would require a force-push, a history rewrite, or deleting anything not
    created by this same change.

Branch protection on `main` is deliberately absent: the convention is the gate,
and a rule that is written down and followed is worth more here than one enforced
against a single maintainer who can bypass it anyway. If this ever becomes a
multi-maintainer repo, protect the branch and delete this paragraph.

### Bot suggestions are inputs, not instructions

Several will conflict with decisions recorded in the ADRs (the reverse-order
pipeline cascade, the derive-don't-increment rule, deliberately reproduced
hardware errata). Reject those **with the citation** — and check the bot's
premise first: a suggestion that looks generic may rest on a real inconsistency
in this repo, which is worth fixing even when the suggested wording is not.

## Conventions

- A chip change touches the chip code AND its `docs/<chip>.md` in the same commit.
- **A comment stating a rule is not an implementation of it.** Four times in Sprints 2–3 (`CACHE`
  Index/Hit, `DMFC1` "ignores FR", the PI byte-write RMW, `DIV` on `inf/0`) the comment was right
  and the code disagreed — and no test failed, because a wrong comment breaks nothing. If a rule is
  worth a comment, write the test that fails without it (`docs/engineering-lessons.md` §3.3c).
- **"Undocumented" is a claim about a document, and it decays.** Three files asserted the exception
  epilogue cost was undocumented while it sits in the opening sentence of the section they cited.
  Cite the pages actually read; re-open the manual before relying on such a record (§3.3b).
- **Never invent a value the documentation does not give — and that includes *behaviour*.** The FP
  multiplication erratum's trigger is documented and its *output* is not, so `Stepping::Early`
  changes no arithmetic (ledger U-7). A fitted constant makes every later result built on it stop
  being evidence. Invented **side effects** are worse, because a constant at least lands in the
  ledger where it can be argued with: `MOV`/`ABS`/`NEG` were written to clear `FCSR.Cause` on no
  authority, which erased the previous operation's result before software could read it — 112
  n64-systemtest assertions, and it made a separately-correct feature measure as zero improvement.
  Before writing any incidental state change (clearing a field, zeroing a half-register), ask what
  documents it; if nothing does, do nothing and say so. Doing nothing is falsifiable.
- **An instruction that decodes to a silent no-op is invisible to every "does not raise" test.**
  `MOV.fmt` (COP1 funct 6) fell outside the decode arm and no-op'd; compilers emit it for every FP
  argument and return value, so operands were stale and results never left their callee. It cost
  ~100 oracle assertions and nine rounds aimed at the wrong subsystem, while the existing test for
  that path asserted only "does not raise when `CU1` is set" — which a no-op satisfies. Assert the
  **effect**: seed the destination so it differs from the expected result in every byte. When
  adding a decode arm, enumerate the neighbouring funct/opcode space rather than only the encoding
  that prompted the change.
- **Capture the instruction stream, not the state, and correlate the capture.** When a value looks
  wrong or stale, dump `(pc, word)` around the site before theorising about the unit that produced
  it — four register-watching probes failed where one code dump succeeded. Arm the capture on the
  suite's own `Running <test>...` marker so the instruction is provably the failing case's;
  uncorrelated captures produced two confident conclusions here that had to be retracted. Three
  signatures worth knowing: a value **identical across every case regardless of input** means the
  instruction never wrote it; a value that is some **earlier test's fill pattern** is stale
  leftover, not this test's sentinel; a **sticky flag present while its per-operation twin is
  clear** means a later instruction overwrote it. And read the oracle's own source in `ref-proj/`
  first — it is cheaper than any probe.
- **Never increment a cycle counter except `master_ticks`.** Every other cycle position is a
  derived accessor (ADR 0006). A new `cycles`/`ticks` field on a chip struct needs justification
  in review; the one legitimate use is a *retired-work* tally that nothing schedules against. The
  residue invariant test exists to catch this and must stay in the default `cargo test` path.
- **In the reverse cascade, check which latch actually holds what you mean.** Stages run
  WB → DC → EX → RF → IC, so by the time a stage executes, every *downstream* stage has already
  moved its latch on. Reading the "obvious" latch has now silently produced a no-op twice — the
  load interlock read `rf_ex` when the load was in `ex_dc`, and the delay-slot flag read `ic_rf`
  after `rf_stage` had vacated it. Both compiled, both passed every existing test, and both did
  nothing. When a pipeline check mysteriously never fires, suspect this first.
- **Trace, don't reason, about pipeline timing.** Dumping the actual fetch/PC sequence found two
  such bugs in one run after several minutes of reasoning had produced two wrong answers.
- **A test whose success and failure paths converge proves nothing.** A control-flow test whose
  branch target equalled the sequential path passed with the redirect code entirely absent.
  Choose targets that are unreachable if the feature is broken.
- **Mutation-check every guard before keeping it**: revert the fix, confirm the test goes red,
  restore. A test asserting "a trapped FP operation does not retire" compared *total* retired
  counts between a trapping and non-trapping run and **passed with the fix removed**, because the
  trap flushes the pipeline and the totals differ over a fixed cycle budget either way. Put the
  reason in the test's doc comment or the confounded version returns as a "simplification".
- **A timing constant the hardware docs do not supply is MEASURED, never tuned.** It goes in
  `docs/accuracy-ledger.md` with its provenance. Adjusting one until a ROM passes makes every
  later timing result unfalsifiable. Currently unmeasured: `M` (memory access time), the
  exception-epilogue cost, CP0I, RDRAM bank-state costs.
- **NaN classification on the VR4300 is INVERTED from IEEE-754:2008**: significand MSB **set**
  means *signalling*, so `f32::NAN` (`0x7FC0_0000`) raises Invalid here. This looks like a bug on
  every reading and is not — it is the legacy MIPS convention, corroborated by the processor's own
  default NaN result (`0x7FBF_FFFF`, MSB clear) being quiet only under it. Never "correct" it back
  to IEEE; see ledger **C-12** and the test that asserts `is_snan_f32(f32::NAN)` on purpose.
- **The VR4300 has no subnormal datapath.** A subnormal operand *or result* raises the unmaskable
  unimplemented-operation cause (bit 17), not a number — `FCSR.FS` flushing is the only exception,
  and even that only when underflow and inexact are both disabled. Ledger **C-13**. Two traps
  here: `MOV` is a pure bit move while `ABS`/`NEG` classify their operand and replace `Cause`; and
  **compares are exempt** — `C.cond.fmt` treats a subnormal as an ordinary number, so applying the
  rule there regresses all sixteen compare tests.
- Say "master clock" only with its rate. The sources use **MasterClock = 62.5 MHz**; this
  project's master tick is **187.5 MHz**; ADR 0001 used it for 93.75 MHz. See `docs/glossary.md`.
- `unsafe` is allowed only in the frontend and FFI. Enforced: every chip crate and `-core` carry
  `#![forbid(unsafe_code)]`. There is zero `unsafe` in the tree today — keep it that way.
- Stubs are `TODO(T-XXX-NN)` comments in no-op bodies that still compile, NOT `todo!()`. So a
  green `cargo test` does not mean a subsystem works — check `docs/STATUS.md`.
- Ticket IDs are `T-PS-NNN`, where **P is the phase digit and S the sprint digit** — so
  phase 1 sprint 1 mints `T-11-001`, phase 3 sprint 2 mints `T-32-004`. CONTRIBUTING/ROADMAP
  state the template; the phase overviews instantiate it. Code TODOs use a separate
  subsystem-scoped form (`T-CPU-01`, `T-HARNESS-02`) for pre-ticket scaffolding.
- Never commit commercial ROMs.
- Versioning starts clean at v0.1.0 — RustyNES "v2.0 / engine-lineage" anchors are NOT this
  project's releases.

<<< MC-PROJECT-END >>>
