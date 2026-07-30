#!/usr/bin/env bash
# Fail if en-GB spellings reach the tracked tree. RustyN64 is written in en-US.
#
# This exists because a one-off spelling sweep decays: nothing fails when a
# single "colour" comes back, so it survives review and the next one has
# precedent. The same reasoning as scripts/check_no_roms.sh — the convention is
# only worth as much as its gate. (markdownlint in this repo is pre-commit only,
# and a fence-language violation sat unnoticed on main as a result; this check
# runs in CI so it cannot be skipped with --no-verify.)
#
# EXCLUDED TREES, and why:
#   ref-docs/        immutable research corpus — corrections land as new dated
#                    supplemental files, never as in-place rewrites (module 40)
#   n64brew_wiki/    offline mirror of a CC BY-SA source; quoting it verbatim is
#                    the point, and it is gitignored anyway
#   ref-proj/        study clones of other emulators (gitignored)
#   third_party/     vendored upstream source (libdragon); not our prose to edit
#
# PER-LINE OPT-OUT: append the marker `spell-exempt` to a line that must keep an
# en-GB form — a quoted external value, or prose that names the spelling itself.
# Prefer rephrasing over the marker; every use is a small permanent exception.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

# Stems, not whole words, so every inflection is covered by one entry: "colour"
# also catches colours/coloured/colouring/colourful.
#
# Deliberately ABSENT because the form is correct in en-US too, and a stem here
# would corrupt it: analysis, analyses (as a noun), synthesis, hypothesis,
# peripheral, exercise, precise, imprecise, premise, promise, otherwise,
# likewise, bitwise, advertise, revise, praise, controlled, installed, stalled.
STEMS=(
  colour behaviour centre modelled modelling licence neighbour
  rasteris signalling signalled labelled labelling travelled
  honour favour catalogue artefact analogue analyser
  judgement acknowledgement grey
  initialis normalis serialis canonicalis capitalis characteris
  containeris synchronis finalis generalis localis materialis
  neutralis optimis organis parallelis parameteris prioritis
  privatis quantis randomis realis recognis specialis stabilis
  summaris theoris synthesise
)

# Literals that are NOT prose and must survive verbatim. These are STRIPPED from
# the line before matching rather than exempting the whole line — a line-level
# skip would also shield any other en-GB word that happened to sit beside them.
#   lightgrey    a shields.io badge color PARAMETER inside a URL
#   `cancelled`  GitHub Actions' own literal run status, quoted as a value
export ALLOW_LITERALS='lightgrey|`cancelled`'

# The only line-level escape hatch, because exempting the line IS its purpose.
LINE_MARKER='spell-exempt'

export PATTERN="$(IFS='|'; echo "${STEMS[*]}")"

# `git ls-files` so untracked scratch files and ignored trees never fail the
# gate; -I skips binaries (the .rvec/.snap/.png fixtures).
mapfile -d '' -t files < <(
  git ls-files -z |
    grep -zvE '^(ref-docs|n64brew_wiki|ref-proj|third_party)/' |
    grep -zvE '^scripts/check_en_us\.sh$'
)

hits="$(
  printf '%s\0' "${files[@]}" |
    xargs -0 grep -nIiE "$PATTERN" 2>/dev/null |
    grep -vF "$LINE_MARKER" |
    # Re-test each candidate with the allowed literals removed, so a permitted
    # value cannot shield a genuine hit sharing its line.
    perl -ne '($f,$l,$c) = /^(.*?):(\d+):(.*)$/s or next;
              $c =~ s/$ENV{ALLOW_LITERALS}//g;
              print if $c =~ /$ENV{PATTERN}/i' ||
    true
)"

if [ -n "$hits" ]; then
  count="$(printf '%s\n' "$hits" | grep -c '' || true)"
  echo "en-US check FAILED: $count line(s) carry an en-GB spelling." >&2
  echo >&2
  printf '%s\n' "$hits" | cut -c1-160 >&2
  echo >&2
  echo "Fix the spelling. If a line must keep the en-GB form because it quotes" >&2
  echo "an external value verbatim, append the marker: spell-exempt" >&2
  exit 1
fi

echo "en-US check passed: ${#files[@]} tracked files, no en-GB spellings."
