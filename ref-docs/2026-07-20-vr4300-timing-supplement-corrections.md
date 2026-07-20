# VR4300 timing supplement — corrections (2026-07-20)

**Supersedes items 2 and 3 of §"Undocumented constants" in
[`2026-07-20-vr4300-timing-supplement.md`](2026-07-20-vr4300-timing-supplement.md).**

`ref-docs/` is an immutable corpus: corrections land as new dated files, never as in-place
rewrites (`docs/adr/`-adjacent rule, stated in the project's documentation discipline). This file
is that correction. The original supplement is left exactly as written, including its errors, so
the record of what was believed when remains intact.

## What was wrong

The original supplement listed three costs as undocumented. **Two of them are documented**, in
the very sections the supplement cited as lacking them.

### Item 2 — the exception epilogue cost

The supplement said:

> **The exception epilogue cost.** Commonly quoted as 2 PCycles, but no figure appears in UM
> §4.7 or chapter 6.

**It appears in UM §4.7, as that section's opening sentence** (p. 114):

> *"When a pipeline exception condition occurs, the pipeline stalls for 2 PCycles and the
> instruction causing the exception as well as all those that follow it in the pipeline are
> aborted. Accordingly, any stall conditions and any later exception conditions from any aborted
> instruction are inhibited; there is no benefit in servicing stalls for an aborted
> instruction."*

So the value is **2 PCycles**, vendor-documented. CEN64's 2 is independent corroboration rather
than the origin, and the CEN64 source comment the supplement quoted — *"Is the cycle count just
the killing of IC/RF, or do we actually delay an additional two cycles?"* — is answered by the
same sentence: the stall is real and it is in addition to the abort.

### Item 3 — CP0I

The supplement said:

> **CP0I (CP0 Bypass Interlock) cost.** Named in UM Table 4-3 and §4.6.9; no cycle count found.

**§4.6.9 (p. 113) gives it**, and also gives the trigger precisely:

> *"A pipeline stall due to a CP0 Bypass Interlock occurs when an instruction which caused an
> exception reaches the WB stage and the subsequent instruction in the DC stage requests a read
> of any CP0 register. This interlock causes a pipeline stall for one PCycle to allow the CP0
> register to be written in the WB stage before allowing any CP0 register to be read in the DC
> stage."*

So the value is **1 PCycle**.

### Additionally documented, and missing from the original list

**ITM — the instruction micro-TLB miss penalty is 3 PCycles** (UM §4.6.2, p. 107):

> *"A miss penalty of 3 PCycles is incurred when the micro-TLB is updated from the JTLB."*

The structure matters as much as the number. The VR4300 has a **two-entry instruction micro-TLB
(ITLB)** in front of the 32-entry joint TLB (UM §1.5.1). A micro-TLB miss is a **stall** serviced
by a JTLB lookup; a JTLB miss is an **exception**. An implementation that models only the JTLB
does not merely approximate this cost — it loses the distinction entirely, because there is no
structure left for the stall to occur in.

## Items that remain correct

Items 1 (`M`), 4 (RDRAM row state) and 5 (SClock resync determinism) of the original list stand
unchanged. `M` in particular is still the load-bearing unknown, and the two most accurate
reference emulators still disagree on the equivalent constant without either deriving theirs from
a spec.

## Why this happened, and the rule it produces

The error was not a misreading — it was a **search-shape** failure. The exception cost was looked
for in §4.7's *tables* and in Chapter 6's exception-processing prose. It is in §4.7's first
paragraph. Similarly, CP0I's cost was looked for in Table 4-3, which names the interlocks; the
count is in the §4.6.9 body.

Once written down, *"undocumented"* propagated. The same claim reached three files —
`docs/accuracy-ledger.md` (C-2, C-3), `docs/cpu.md`, and the original supplement — and each
subsequent file cited the previous one's confidence rather than the manual. No individual copy
was unreasonable; the aggregate was a fact that nobody had checked and everybody believed.

**The rule:** *"undocumented"* is a claim **about a document**, and claims about documents decay
into folklore faster than claims about behaviour, because nothing ever fails when they are wrong.

- When recording something as undocumented, cite **which pages were actually read**. "No figure
  appears in UM §4.7" is falsifiable and was duly falsified; "not documented" is not.
- When *relying* on such a record — especially to justify a fitted constant — re-open the manual.
  A fitted constant standing in for a documented one is strictly worse than no constant: it looks
  like evidence.

This is the mirror image of the ledger's existing rule that a measured constant must never be
tuned to make a ROM pass. Both failures produce a number that cannot be falsified; this one just
arrives by a politer route.

## Downstream changes

- `docs/accuracy-ledger.md`: C-2 and C-3 reclassified from *not yet measured* to
  **documented-with-citation**; C-7 added for ITM.
- `docs/cpu.md`: the "NOT documented" paragraph replaced with the citations above.

## Citations

All page numbers are printed pages in *VR4300, VR4305, VR4310 User's Manual*, NEC document
U10504EJ7V0UM00 (`n64brew_wiki/images/VR4300-Users-Manual.pdf`; PDF pages map 1:1 to printed
pages). Extract text with `mutool draw -F txt` — `pdftotext` fails on this file.

| Fact | Section | Page |
| --- | --- | --- |
| Exception epilogue = 2 PCycles | §4.7 | 114 |
| CP0I = 1 PCycle | §4.6.9 | 113 |
| ITM = 3 PCycles | §4.6.2 | 107 |
| Two-entry instruction micro-TLB in front of the JTLB | §1.5.1 | — |
