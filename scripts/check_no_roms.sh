#!/usr/bin/env bash
# Block commercial ROMs (and anything ROM-sized) from entering the repository.
#
# Layer 2 of the three-layer guard:
#   1. .gitignore          -- ignores *.z64/*.n64/*.v64/*.ndd and tests/roms/external/
#   2. this script         -- pre-commit hook over the STAGED file list
#   3. no-commercial-roms  -- CI job over the whole tracked tree (.github/workflows/ci.yml)
#
# Layer 1 alone is not a guarantee: `git add -f` bypasses .gitignore silently,
# and a ROM saved under an unexpected name is not matched by extension at all.
# This script closes both gaps by inspecting what is actually staged.
#
# Usage:
#   scripts/check_no_roms.sh [--size-only] [files...]
#
# With no file arguments it checks every tracked file, which is how the CI job
# invokes it. Exits non-zero on the first violation.

set -euo pipefail

# A commercial N64 cartridge is 4-64 MB. The largest legitimately committed file
# in this repo is ref-docs/research-report.md at ~51 KB, so 2 MB is a generous
# ceiling that still catches every real ROM.
MAX_BYTES=$((2 * 1024 * 1024))

ROM_RE='\.(z64|n64|v64|ndd)$'

size_only=0
files=()
for arg in "$@"; do
  case "$arg" in
    --size-only) size_only=1 ;;
    *) files+=("$arg") ;;
  esac
done

# No explicit list -> check everything git tracks (the CI mode).
if [ ${#files[@]} -eq 0 ]; then
  mapfile -t files < <(git ls-files)
fi

fail=0

for f in "${files[@]}"; do
  [ -f "$f" ] || continue

  if [ "$size_only" -eq 0 ] && printf '%s' "$f" | grep -qiE "$ROM_RE"; then
    echo "ERROR: $f looks like a console ROM (extension)." >&2
    fail=1
    continue
  fi

  bytes=$(wc -c < "$f")
  if [ "$bytes" -gt "$MAX_BYTES" ]; then
    echo "ERROR: $f is $((bytes / 1024 / 1024)) MB, over the $((MAX_BYTES / 1024 / 1024)) MB limit." >&2
    fail=1
  fi
done

if [ "$fail" -ne 0 ]; then
  cat >&2 <<'MSG'

Commercial ROMs must never be committed. They are personal cartridge dumps and
local-only test fixtures.

  - Stage them under tests/roms/external/ (gitignored), not in the tree.
  - Commit only the screenshots and .snap baselines they produce.
  - See tests/roms/external/commercial/README.md, CONTRIBUTING.md, and
    docs/testing-strategy.md.

If you are adding a legitimately redistributable CC0/homebrew ROM, add an
explicit .gitignore negation for that one path and re-run. Do not relax the
global rules, and do not bypass this hook with --no-verify.
MSG
  exit 1
fi

echo "no ROMs or oversized files staged"
