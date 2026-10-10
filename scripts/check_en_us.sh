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
#   salvaged/        development artifacts rescued verbatim from a volatile /tmp.
#                    Same reason as the trees above: they are a RECORD, not prose
#                    this project edits. Two of them are en-US conversion tools
#                    whose substitution TABLES are en-GB words as data (26 lines
#                    in one file), and one is a diff that must stay byte-exact to
#                    be worth keeping at all. Per-line markers would litter a data
#                    table and corrupt a patch. Nothing here is built, run in CI,
#                    or read as documentation.
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
# would flag it: analysis, analyses (as a noun), synthesis, hypothesis,
# peripheral, exercise, precise, imprecise, premise, promise, otherwise,
# likewise, bitwise, advertise, revise, praise, controlled, installed, stalled.
#
# `centring` and `cancelling` are listed separately from `centre`/`cancelled`
# because neither CONTAINS its sibling stem — a stem list silently under-matches
# wherever the en-GB inflection drops a letter the stem depends on.
PLAIN_STEMS=(
  colour behaviour centre centring modelled modelling licence neighbour
  signalling signalled labelled labelling travelled cancelled cancelling
  honour favour catalogue artefact analogue analyser
  judgement acknowledgement grey
)

# The -ise/-isation family, matched ONLY when followed by an en-GB verb/noun
# ending. A BARE stem here is wrong, and wrong in both directions: `optimis`
# matches inside `optimistic`, `realis` inside `realistically`, `characteris`
# inside `characteristics` — all three already correct in en-US. Bare stems both
# corrupted those words in the first sweep AND made this gate report them as
# violations. Requiring the ending fixes both: `optimis+tic` no longer matches,
# `optimis+ation` still does.
ISE_STEMS=(
  rasteris initialis normalis serialis canonicalis capitalis characteris
  containeris synchronis finalis generalis localis materialis
  neutralis optimis organis parallelis parameteris prioritis
  privatis quantis randomis realis recognis specialis stabilis
  summaris theoris synthesis
)
ISE_ENDINGS='(e|es|ed|er|ers|ing|ation|ations|able|ably)'

# Literals that are NOT prose and must survive verbatim. These are STRIPPED from
# the line before matching rather than exempting the whole line — a line-level
# skip would also shield any other en-GB word that happened to sit beside them.
#   lightgrey    a shields.io badge color PARAMETER inside a URL
#   `cancelled`  GitHub Actions' own literal run status, quoted as a value
export ALLOW_LITERALS='lightgrey|`cancelled`'

# NON-WORDS a stem substitution produces when the stem matches inside a word that
# is ALREADY correct in en-US. This list exists because the first sweep shipped
# four of them and this gate PASSED on the corrupted tree -- the outputs are not
# en-GB either, so an en-GB pattern cannot see them:
#
#   characteris -> characteriz   corrupted  characteristics -> characteriztics
#   optimis     -> optimiz       corrupted  optimistic      -> optimiztic
#   realis      -> realiz        corrupted  realistically   -> realiztically
#   centre      -> center        corrupted  centred         -> centerd
#
# A reviewer caught one of the four; a before/after word-pair audit caught all
# four. If you run another stem sweep, diff the distinct word pairs it produced
# rather than trusting this list to be complete -- it is a backstop for the forms
# already seen, not a substitute for auditing the new ones.
MALFORMED='characteriztic|optimiztic|realiztical|optimizm|realizm|centerd|catalogd|analogd|licensd|synthesiz(is|es-)'

# The only line-level escape hatch, because exempting the line IS its purpose.
LINE_MARKER='spell-exempt'

plain="$(IFS='|'; echo "${PLAIN_STEMS[*]}")"
ise="$(IFS='|'; echo "${ISE_STEMS[*]}")"
export PATTERN="${plain}|(${ise})${ISE_ENDINGS}"

# Tracked files PLUS untracked-but-not-ignored ones. `-I` skips binaries (the
# .rvec/.snap/.png fixtures) and `--exclude-standard` honors .gitignore, so
# scratch files under an ignored path still never fail the gate.
#
# `--others` is load-bearing and was MISSING in the first version, which made the
# gate give a **false PASS** on the very commit that introduced a violation: a
# brand-new file is untracked until it is staged, `git ls-files` alone lists only
# TRACKED files, so a new file was invisible to the check for exactly as long as
# it took to `git add` it. `present_buffer.rs` shipped a `licence` that way, and
# the pre-commit hook would not have caught it until the NEXT commit. A gate that
# cannot see new files is worst precisely where new violations arrive.
# The listing is built through a FILE, not a process substitution, for one
# reason: `mapfile -d '' -t files < <(producer)` discards the producer's exit
# status entirely. `set -euo pipefail` cannot see inside a process
# substitution, so a failing `git ls-files` yielded an empty `files` array and
# the gate then reported "no en-GB spellings" over zero files -- PASS.
#
# That is the SAME false-PASS class this script was already fixed for once (the
# missing `--others`, below). A gate whose failure mode is a silent pass is
# worse than no gate, so both the producer's status and a suspiciously empty
# result are now checked explicitly.
list="$(mktemp)"
raw="$(mktemp)"
trap 'rm -f "$list" "$raw"' EXIT

# Each `git ls-files` is a simple command with its status checked by `set -e`.
git ls-files -z >"$raw"
git ls-files -z --others --exclude-standard >>"$raw"

# `grep -v` exits 1 when it selects nothing, which is not an error here, so the
# filters are deliberately status-tolerant -- the emptiness check below is what
# catches a broken listing.
grep -zvE '^(ref-docs|n64brew_wiki|ref-proj|third_party|salvaged)/' <"$raw" |
  grep -zvE '^scripts/check_en_us\.sh$' >"$list" || true

mapfile -d '' -t files <"$list"

if [ "${#files[@]}" -eq 0 ]; then
  echo "en-US check FAILED: the file listing is empty." >&2
  echo "That is a broken gate, not a clean tree -- git ls-files produced" >&2
  echo "nothing, or every path was filtered out. Refusing to report a pass." >&2
  exit 1
fi

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

bad="$(
  printf '%s\0' "${files[@]}" |
    xargs -0 grep -nIiE "$MALFORMED" 2>/dev/null |
    grep -vF "$LINE_MARKER" ||
    true
)"

status=0
if [ -n "$hits" ]; then
  count="$(printf '%s\n' "$hits" | grep -c '' || true)"
  echo "en-US check FAILED: $count line(s) carry an en-GB spelling." >&2
  echo >&2
  printf '%s\n' "$hits" | cut -c1-160 >&2
  echo >&2
  echo "Fix the spelling. If a line must keep the en-GB form because it quotes" >&2
  echo "an external value verbatim, append the marker: spell-exempt" >&2
  status=1
fi

if [ -n "$bad" ]; then
  count="$(printf '%s\n' "$bad" | grep -c '' || true)"
  echo "en-US check FAILED: $count line(s) carry a MALFORMED word -- the" >&2
  echo "signature of a stem substitution that matched inside a word already" >&2
  echo "correct in en-US. See the MALFORMED list in this script." >&2
  echo >&2
  printf '%s\n' "$bad" | cut -c1-160 >&2
  status=1
fi

if [ "$status" -ne 0 ]; then
  exit 1
fi

echo "en-US check passed: ${#files[@]} files (tracked + untracked), no en-GB or malformed spellings."
