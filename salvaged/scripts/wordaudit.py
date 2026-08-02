"""Every distinct word the sweep produced, so a malformed output cannot hide.

Reconstructs before/after per changed line and reports the set of word pairs
that actually differ. A stem substitution can produce a NON-word (centred ->  spell-exempt
centerd) which no en-GB gate can catch, because the output is not en-GB either.  spell-exempt
"""
import re, subprocess, collections

out = subprocess.run(["git", "diff", "HEAD~1", "--unified=0", "-U0"],
                     capture_output=True, text=True).stdout.splitlines()
minus, plus = [], []
pairs = collections.Counter()

def flush():
    for a, b in zip(minus, plus):
        wa = re.findall(r"[A-Za-z][A-Za-z'-]*", a)
        wb = re.findall(r"[A-Za-z][A-Za-z'-]*", b)
        if len(wa) != len(wb):
            continue
        for x, y in zip(wa, wb):
            if x != y:
                pairs[(x, y)] += 1
    minus.clear(); plus.clear()

for line in out:
    if line.startswith("@@") or line.startswith("+++") or line.startswith("---"):
        flush(); continue
    if line.startswith("-"): minus.append(line[1:])
    elif line.startswith("+"): plus.append(line[1:])
flush()

print(f"{len(pairs)} distinct word substitutions:\n")
for (a, b), n in sorted(pairs.items(), key=lambda kv: (-kv[1], kv[0])):
    print(f"  {n:5d}  {a:28s} -> {b}")
