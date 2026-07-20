# RustyN64 ↔ RustyNES / RustySNES Lockstep Checklist

RustyN64 is the third emulator in this line, and it inherits its architecture, frontend shell,
CI shape, and documentation conventions from two siblings that are both still shipping:
RustyNES (`../RustyNES`) and RustySNES (`../RustySNES`) — all three are sibling checkouts on this
machine. Rather than freezing a snapshot of what they looked like at bootstrap, this checklist is
run once at the start of scoping **each** release in `to-dos/VERSION-PLAN.md`'s ladder, to catch
drift before it accumulates.

This is intentionally lightweight — a ~10-minute pass, not a governance process — matching a
solo-maintainer project's actual capacity. It is also written to double as a ready-made prompt:
"run `to-dos/LOCKSTEP-CHECKLIST.md` first" is a complete instruction for whoever (human or agent)
scopes the next release.

## Why two siblings, and what to take from each

They are not interchangeable sources, and conflating them wastes the pass:

- **RustyNES** is the most mature and furthest ahead. Take **process and infrastructure** from
  it: CI shape, release ceremony, docs-site structure, the `plans/` and `archive/` conventions,
  and the frontend/UX surface. Its accuracy techniques are mostly NES-specific and will not port.
- **RustySNES** is the closer structural analogue — a second-generation project that inherited
  from RustyNES and had to adapt rather than copy. Take **planning shape and honesty mechanisms**
  from it: the phase/sprint skeleton, `VERSION-PLAN.md`, `accuracy-ledger.md`, and how it tracks
  residuals it has not closed. Its coprocessor tiering explicitly does **not** apply here
  (ADR 0003: the N64 has one cart model, so there is no board tiering and no honesty gate).

Where the two disagree, prefer RustySNES's shape — it solved the same "inherit without copying"
problem this project has.

## The checklist

1. Read the **Lockstep log** table below for the most recently checked refs and date.
2. Fetch and diff both siblings' changelogs since those refs — no cloning needed, both repos
   already live side by side:

   ```bash
   git -C ../RustyNES  fetch && git -C ../RustyNES  log <last-nes-ref>..origin/main  --oneline -- CHANGELOG.md
   git -C ../RustySNES fetch && git -C ../RustySNES log <last-snes-ref>..origin/main --oneline -- CHANGELOG.md
   ```

3. Skim, since the last check:
   - Each sibling's `CHANGELOG.md` entries.
   - The top status blurb of each `docs/STATUS.md` and `to-dos/ROADMAP.md`.
   - Specifically watch for **new oracle or regression-net *categories*** appearing, not just
     growth of existing ones. RustyNES added Holy Mapperel and a PAL-APU oracle as entirely new
     categories mid-line in its v2.1.x, which is the kind of thing that is easy to miss when
     skimming for bigger numbers.
   - Any CI or release-infrastructure change (new workflow files, new promotion gates, new
     required checks).
   - Any newly logged regressions or known issues — particularly ones whose root cause is
     architectural rather than console-specific, since those are the ones that transfer.
   - Any new ADR, since an ADR in a sibling often signals a decision this project will face.
4. Classify each finding:
   - **Already covered** — RustyN64's roadmap already has equivalent scope planned or shipped.
     Log it, no further action.
   - **Not applicable** — genuinely console-specific (a mapper technique, a coprocessor tier, a
     PPU quirk). Log it with one line saying why, so the same finding is not re-litigated at the
     next pass.
   - **Small catch-up** — fits inside the release currently being scoped without displacing
     already-planned items. Fold it directly into that release's `to-dos/VERSION-PLAN.md` rung.
   - **Large catch-up** — a genuinely new theme, multiple PRs' worth, or would displace planned
     scope. Do **not** silently cram it in. Add it to `to-dos/ROADMAP.md`'s "Milestones beyond
     the phases" and flag it for a maintainer go/no-go before any detail-scoping.
5. Append one row to the Lockstep log below.

**Size threshold, made concrete:** if the sibling change maps onto scope RustyN64's roadmap
already names, fold it in. If it would introduce a category with no existing line item, or would
not fit the release being scoped without bumping already-planned items, give it its own future
rung instead. RustySNES set this precedent — its `v0.9.0 "Threshold"` was not originally planned;
it was added when Phase 7/8 leftovers surfaced during scoping, rather than being force-fit into
an adjacent release. `to-dos/VERSION-PLAN.md` reserves a `v0.9.0 "Threshold"` here for the same
reason.

**A note specific to this project's position.** RustyN64 is at v0.1.0 with no chip implemented,
while the siblings are at v2.2.0 and v1.8.0 with mature feature sets. Most of what they ship in
any given window will classify as **already covered** (it is in a later phase) or **not
applicable**. That is the expected outcome, not a sign the pass was wasted — the value is
catching the small number of process and infrastructure changes that *do* transfer, early enough
to be cheap. Resist the pull to import feature breadth ahead of the phase spine; Phase 8 exists,
and it is last for a reason.

**When to run it:** once, at the start of scoping each new release — never per-PR, never
continuously.

## Lockstep log

| Date | RustyNES ref checked | RustySNES ref checked | Findings since last check | Disposition | Notes |
|---|---|---|---|---|---|
| 2026-07-20 | `v2.2.0` (released) / `main` @ `b49dd1e` | `v1.8.0` (released) / `feat/accuracysnes-phase-a` @ `51829b1` | Initial baseline. Structure adopted from both: the phase/sprint skeleton, `VERSION-PLAN.md`, and `LOCKSTEP-CHECKLIST.md` from RustySNES; the README shape, CI split, docs index, and release ceremony from both. Divergences taken deliberately: no board tiering or honesty gate (ADR 0003, the N64 has one cart model); Phase 8 reach features deferred past v1.0.0 rather than front-loaded as RustyNES did; status markers are plain text, since this project's no-emoji policy applies to docs. | Ladder `v0.1.0`-`v1.0.0` scoped against this baseline in `to-dos/VERSION-PLAN.md`. | First entry; establishes the checklist itself. Note RustySNES's checked ref is a feature branch, not `main` — re-resolve to `main` at the next pass. |
