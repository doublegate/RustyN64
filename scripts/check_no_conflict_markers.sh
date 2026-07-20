#!/usr/bin/env bash
# Fail if any tracked text file carries a merge-conflict marker.
#
# A `|||||||` diff3 marker was once committed into CHANGELOG.md: the resolution
# handled `<<<<<<<`, `=======` and `>>>>>>>` but not the diff3 middle section,
# and nothing downstream noticed because a stray line is valid Markdown. It
# would have shipped into the release notes.
set -euo pipefail

pattern='^(<<<<<<< |\|\|\|\|\|\|\| |>>>>>>> )'
if hits=$(git grep -nE "$pattern" -- ':!scripts/check_no_conflict_markers.sh' 2>/dev/null); then
    echo "error: merge-conflict markers found in tracked files:" >&2
    echo "$hits" >&2
    exit 1
fi
echo "no conflict markers"
