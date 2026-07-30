# Residual records

Long-form records for the accuracy residuals whose entries outgrew a single
Markdown table cell in [`../accuracy-ledger.md`](../accuracy-ledger.md).

The ledger table remains the index: every residual **R-1 … R-23** still has a
row there with its classification and current status. Residuals whose record
passed **4,000 characters** have their full text here, linked from that row.

## Why they were moved

Two reasons, one of them demonstrated rather than theoretical.

1. **The rows had begun corrupting the table.** A Markdown table row cannot
   contain a hard line break, so these records grew as single lines of tens of
   thousands of characters — R-18 reached **47,811**. Unescaped `|` inside
   inline code spans then split the row, and `markdownlint` reported the
   consequence exactly: *"MD056/table-column-count [Expected: 5; Actual: 9; Too
   many cells, **extra data will be missing**]"*. Content at the **end** of the
   row — which is where the newest appends land — was being dropped from the
   rendered table.
2. Three reviewers raised the readability cost independently across four PRs.

## What did NOT change

These files are **append-only**, exactly as the ledger row was. Superseded
claims stay in place and marked (`[HISTORICAL BASELINE — SUPERSEDED …]`,
`RETRACTED`, `at the time of writing`) rather than being edited away — the
provenance is the point, and a record that only shows the conclusion cannot
show which measurement overturned which guess.

The extraction was verified mechanically, not by eye: every one of the 20 cells
across the five moved residuals was checked to appear verbatim in its new file
before the ledger rows were replaced.

## Index

| Residual | Subject |
| --- | --- |
| [R-5](R-5.md) | VI scan-out — scale/resample and the AA / divot / de-dither post-filters |
| [R-10](R-10.md) | Color-combiner exotic inputs — noise, LOD fraction, chroma key, YUV convert |
| [R-13](R-13.md) | Triangle texturing — perspective divide, bilinear sampling, LOD/mip |
| [R-18](R-18.md) | Commercial video — boot, microcode, and the road to a rendered title screen |
| [R-19](R-19.md) | n64-systemtest hang — the branch/vector race |
