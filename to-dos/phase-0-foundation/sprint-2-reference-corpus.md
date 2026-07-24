# Sprint 2 — Reference corpus and repository guards

**Phase:** Phase 0 — Foundation
**Sprint goal:** the hardware reference, the study clones, and the test-ROM corpora are acquired,
licence-classified, and organised, with commercial ROMs structurally unable to enter the
repository.
**Estimated duration:** 1 week

## Tickets

### T-02-001 — Offline N64brew Wiki mirror

**Description:** a script that mirrors the N64brew Wiki to a gitignored `n64brew_wiki/` from the
MediaWiki API, producing parallel HTML, Markdown, and wikitext trees plus original-resolution
media, with revision-based incremental refresh.

**Acceptance criteria:**

- [x] Main, File, Category, and Help namespaces mirrored; Template and Module excluded because
      `action=parse` returns expanded HTML. *(324 pages, 96 media.)*
- [x] Links and image sources rewritten to local relative paths at the correct depth.
- [x] `--verify` audits every local reference. *(1,094 checked, 0 unresolved, 0 remote images.)*
- [x] `--refresh` re-fetches only changed revisions.
- [x] Broken upstream redirects are kept as stubs rather than dropped. *(3 pages.)*
- [x] CC BY-SA 4.0 recorded, with attribution required if quoted.

**Dependencies:** none
**Reference:** `n64brew_wiki/README.md`
**Estimated complexity:** M

---

### T-02-002 — Reference emulators cloned and licence-classified

**Description:** shallow-clone the reference emulators and test suites into the gitignored
`ref-proj/`, verify each licence by reading the actual file, and record a vendor-ok or
study-only decision per repo.

**Acceptance criteria:**

- [x] Eleven clones with commit hashes recorded. *(ares, cen64, gopher64, simple64, parallel-rdp,
      parallel-rsp, angrylion, n64-systemtest, n64-tests, libdragon, PeterLemon.)*
- [x] Every licence verified against the upstream file, not a badge.
- [x] Each classified vendor-ok or study-only, with the reasoning written down.
- [x] The non-obvious traps are called out: angrylion ships **no** `LICENSE` but is
      non-commercial MAME-licensed; n64-tests has no licence at all, which grants nothing.
- [x] `AGENTS.md` carries the summary so it is read before anything is copied.

**Dependencies:** none
**Reference:** `ref-proj/README.md`
**Estimated complexity:** M

---

### T-02-003 — Test-ROM corpora, tiered by licence

**Description:** stage the test-ROM corpora in two tiers — a committed tier of permissively
licensed suites and a gitignored external tier for everything else — and build the committed
suite from source where no prebuilt exists.

**Acceptance criteria:**

- [x] `n64-systemtest` (MIT) built from source and committed with its upstream `LICENSE`.
      *(sha256 recorded; upstream publishes no prebuilt ROM.)*
- [x] External tier staged: krom (196 ROMs), dillon-n64-tests (26), 240p (built from source),
      commercial (66).
- [x] Every corpus has a verified licence and a recorded reason for its tier.
- [x] The 240p build recipe is documented, including that dependency install and build must
      happen in one container because `/opt/libdragon` does not survive `--rm`.

**Dependencies:** T-02-002
**Reference:** `tests/roms/README.md`; `docs/testing-strategy.md`
**Estimated complexity:** L

---

### T-02-004 — The commercial-ROM guard, three layers deep

**Description:** make it structurally impossible to commit a commercial ROM, using guards that
each cover a different bypass.

**Acceptance criteria:**

- [x] `.gitignore` excludes the ROM extensions everywhere, re-includes only the committed tier,
      then hard-excludes `tests/roms/external/` last so no negation can leak a dump back in.
- [x] A pre-commit hook checks the staged list, closing the `git add -f` gap that `.gitignore`
      cannot.
- [x] A size ceiling catches a ROM renamed to hide its extension.
- [x] An allowlisted ROM must ship a `LICENSE` beside it, enforced rather than documented.
- [x] A CI job re-runs the scan server-side over the whole tracked tree.
- [x] All of it verified against a real bypass attempt, not assumed. *(`git add -f` on an 8 MB
      commercial ROM stages past `.gitignore`; the hook then rejects it.)*

**Dependencies:** T-02-003
**Reference:** `scripts/check_no_roms.sh`; `tests/roms/README.md`
**Estimated complexity:** M

---

### T-02-005 — Golden VR4300 trace capture

**Description:** script a cen64 or ares run that emits a per-instruction trace in the differ's
`TraceRecord { pc, gpr, cycle }` format, and commit it to `tests/golden/`.

**Acceptance criteria:**

- [ ] A reference run produces a trace in the differ's format.
- [ ] The trace is committed and `load_golden_stub` is replaced by a real loader.
- [ ] The differ reports the first divergence by index and PC, not a bare failure.

**Dependencies:** T-02-002
**Reference:** `docs/testing-strategy.md` Layer 2; `crates/rustyn64-test-harness/src/golden.rs`
**Estimated complexity:** M

---

## Sprint review checklist

- [x] All tickets checked off or explicitly deferred (with reason).
- [x] `git status` clean with the corpora present; no ROM is stageable.
- [x] CHANGELOG.md updated.
- [x] The golden trace exists (T-02-005 landed in Phase 1: `tests/golden/n64-systemtest.log`,
      a 50,027-record ares capture, gated by `golden_log.rs`).
