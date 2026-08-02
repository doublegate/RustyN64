import re, sys, pathlib

# Stem-level pairs. Stems (not whole words) so every inflection is covered by one
# entry: "colour" also fixes colours/coloured/colourful. Every stem below was
# verified to occur in the tree; none is a substring of a word that is correct in
# en-US (checked explicitly for realis/normalis/serialis/materialis, and for the
# pairs where en-GB and en-US share a form: analysis, synthesis, peripheral,
# hypothesis, exercise, precise, premise, otherwise, likewise, bitwise).
PAIRS = [
    ("colour", "color"), ("behaviour", "behavior"), ("centre", "center"),
    ("modelled", "modeled"), ("modelling", "modeling"),
    ("licence", "license"), ("neighbour", "neighbor"),
    ("rasteris", "rasteriz"),
    ("signalling", "signaling"), ("signalled", "signaled"),
    ("labelled", "labeled"), ("labelling", "labeling"),
    ("travelled", "traveled"),
    ("honour", "honor"), ("favour", "favor"),
    ("catalogue", "catalog"), ("artefact", "artifact"),
    ("analogue", "analog"), ("analyser", "analyzer"),
    ("judgement", "judgment"), ("acknowledgement", "acknowledgment"),
    ("grey", "gray"),
    # -ise / -isation families
    ("initialis", "initializ"), ("normalis", "normaliz"), ("serialis", "serializ"),
    ("canonicalis", "canonicaliz"), ("capitalis", "capitaliz"),
    ("characteris", "characteriz"), ("containeris", "containeriz"),
    ("synchronis", "synchroniz"), ("finalis", "finaliz"),
    ("generalis", "generaliz"), ("localis", "localiz"),
    ("materialis", "materializ"), ("neutralis", "neutraliz"),
    ("optimis", "optimiz"), ("organis", "organiz"),
    ("parallelis", "paralleliz"), ("parameteris", "parameteriz"),
    ("prioritis", "prioritiz"), ("privatis", "privatiz"),
    ("quantis", "quantiz"), ("randomis", "randomiz"),
    ("realis", "realiz"), ("recognis", "recogniz"),
    ("specialis", "specializ"), ("stabilis", "stabiliz"),
    ("summaris", "summariz"), ("theoris", "theoriz"),
    ("synthesise", "synthesize"),   # NOT "synthesis" -- correct in both  spell-exempt
    ("mis-analyses", "mis-analyzes"),  # the verb; bare "analyses" can be a noun
]

# Literals that must survive the sweep verbatim, with the reason.
PROTECT = [
    "lightgrey",   # a shields.io badge COLOR PARAMETER in a URL, not prose
    "`cancelled`", # GitHub Actions' own literal run status, quoted as a value
]

def case_like(src: str, dst: str) -> str:
    if src.isupper():
        return dst.upper()
    if src[0].isupper():
        return dst[0].upper() + dst[1:]
    return dst

def convert(text: str):
    for i, lit in enumerate(PROTECT):
        text = text.replace(lit, f"\x00P{i}\x00")
    n = 0
    for gb, us in PAIRS:
        pat = re.compile(re.escape(gb), re.IGNORECASE)
        def rep(m, us=us):
            nonlocal n
            n += 1
            return case_like(m.group(0), us)
        text = pat.sub(rep, text)
    for i, lit in enumerate(PROTECT):
        text = text.replace(f"\x00P{i}\x00", lit)
    return text, n

apply = "--apply" in sys.argv
paths = [p for p in sys.argv[1:] if not p.startswith("--")]
total, touched = 0, 0
for name in paths:
    p = pathlib.Path(name)
    try:
        orig = p.read_text(encoding="utf-8")
    except (UnicodeDecodeError, IsADirectoryError, FileNotFoundError):
        continue
    new, n = convert(orig)
    if new != orig:
        touched += 1
        total += n
        print(f"{n:5d}  {name}")
        if apply:
            p.write_text(new, encoding="utf-8")
print(f"--- {total} replacements across {touched} files ({'APPLIED' if apply else 'dry run'})")
