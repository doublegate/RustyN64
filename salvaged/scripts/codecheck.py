import re, sys, pathlib
src = pathlib.Path(sys.argv[1]).read_text()
PAIRS = re.findall(r'\("([a-z-]+)", "([a-z-]+)"\)', src)
PROTECT = ["lightgrey", "`cancelled`"]
pat = re.compile("|".join(re.escape(g) for g, _ in PAIRS), re.IGNORECASE)
prot = re.compile("|".join(re.escape(p) for p in PROTECT))
print(f"[{len(PAIRS)} pairs loaded]")
for name in sys.argv[2:]:
    p = pathlib.Path(name)
    if p.suffix not in (".rs", ".c", ".h", ".py", ".sh", ".toml", ".yml", ".yaml", ".s"):
        continue
    try:
        lines = p.read_text(encoding="utf-8").splitlines()
    except (UnicodeDecodeError, IsADirectoryError, FileNotFoundError):
        continue
    for i, line in enumerate(lines, 1):
        if not pat.search(line) or prot.search(line):
            continue
        s = line.strip()
        if s.startswith(("//", "///", "//!", "#", "*", "/*")):
            continue
        print(f"{name}:{i}: {s[:120]}")
