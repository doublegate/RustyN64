# RustyN64 agent reference: shipping

Moved verbatim from the AGENTS.md project block: the PR ceremony, review authority, and bot-suggestion policy.

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
