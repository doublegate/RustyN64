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

# A commercial N64 cartridge is 4-64 MB. Outside the allowlist below, nothing in
# this repo is legitimately large, so 2 MB catches every real ROM.
MAX_BYTES=$((2 * 1024 * 1024))

# Committed-tier test ROMs are ALLOWED here, but only under these exact paths and
# only when permissively licensed. This is the counterpart to the .gitignore
# re-include rules; keep the two lists in step.
#
# Adding a path here is a licensing decision, not a convenience: the corpus must
# be redistributable (MIT / BSD / Zlib / CC0 / public domain). Copyleft (GPL) and
# unlicensed corpora stay in the gitignored tests/roms/external/ tier, and
# commercial ROMs never enter the tree at all. See tests/roms/README.md.
ALLOW_RE='^tests/roms/(n64-systemtest|homebrew)/'

# Allowlisted ROMs are exempt from MAX_BYTES (a test ROM legitimately runs to a
# few MB), but capped so a commercial dump cannot hide behind the allowlist.
ALLOW_MAX_BYTES=$((8 * 1024 * 1024))

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

  allowed=0
  if printf '%s' "$f" | grep -qE "$ALLOW_RE"; then
    allowed=1
  fi

  if [ "$size_only" -eq 0 ] && [ "$allowed" -eq 0 ] \
     && printf '%s' "$f" | grep -qiE "$ROM_RE"; then
    echo "ERROR: $f looks like a console ROM (extension)." >&2
    fail=1
    continue
  fi

  # A committed ROM must ship its upstream licence alongside it, so the grant
  # travels with the binary instead of living only in a README someone can
  # forget to update.
  if [ "$allowed" -eq 1 ] && printf '%s' "$f" | grep -qiE "$ROM_RE"; then
    dir=$(dirname "$f")
    if ! compgen -G "$dir/LICENSE*" > /dev/null; then
      echo "ERROR: $f is allowlisted but $dir/ has no LICENSE file." >&2
      fail=1
    fi
  fi

  limit=$MAX_BYTES
  [ "$allowed" -eq 1 ] && limit=$ALLOW_MAX_BYTES
  bytes=$(wc -c < "$f")
  if [ "$bytes" -gt "$limit" ]; then
    echo "ERROR: $f is $((bytes / 1024 / 1024)) MB, over the $((limit / 1024 / 1024)) MB limit." >&2
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
