# Release notes

One file per released tag, named exactly `<tag>.md` (for example `v0.1.0.md`).

The release workflow reads `docs/release-notes/<tag>.md` and uses it as the GitHub Release body.
Precedence, most specific first:

1. `docs/release-notes/<tag>.md` — the long-form notes.
2. The annotated tag body — for a patch cut that does not warrant its own file.
3. `--generate-notes` — last resort, an auto-generated commit list.

## Why the notes live here rather than in the tag

The tag annotation stays short and points at the release. Long-form notes live in the repository
so they can be **reviewed in a PR like any other document**, corrected after the fact without
rewriting a published tag, linted by the same markdownlint gate as everything else, and read by
anyone who has the source without going to GitHub.

Rewriting a tag that has already been pushed is exactly the kind of history rewrite the release
ceremony warns against; a file is editable.

## What a release note should contain

Follow the shape of `v0.1.0.md`:

- What the release actually is, stated plainly — including what it is *not*, when that is the
  more useful fact.
- The load-bearing technical changes, with the concrete mechanism: the exact ratio, the exact
  register, the exact opcode. Not "improved timing".
- Infrastructure and tooling changes that affect contributors.
- **Known limitations**, recorded rather than hidden. A release note that lists only additions is
  a marketing document.
- Verification: the gates that ran, and the accuracy suites with their results — or an explicit
  statement that none are active yet.
- Installation and checksum-verification steps.

Keep it emoji-free, per project policy.
