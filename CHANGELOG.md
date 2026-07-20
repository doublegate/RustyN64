# Changelog

All notable changes to RustyN64 are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/), and this project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

The next rung is `v0.2.0 "Interpreter"` — the VR4300 (see
[`to-dos/VERSION-PLAN.md`](to-dos/VERSION-PLAN.md)).

### Diagnosed — why n64-systemtest reports nothing (T-12-007)

Not waiting; **lost**. Probing for the first divergence found it **191 retired instructions in**:
execution jumps from `0x800A15F4` into a zero-filled region and NOP-slides until it falls off the
top of RDRAM, which is why raising the instruction budget from 6M to 45M never helped.

The cause is in the suite's own `entrypoint()` — `memory_size` and `elf_header_offset` come from
**SP DMEM**, which is a stub returning 0. IPL3 writes the detected RDRAM size there at boot; we do
not, so the suite builds its memory map from zeros and jumps into nothing.

SP DMEM is now readable (`Bus::spmem`) and seeded by `rom::seed_ipl3_handoff` with what IPL3 would
have written. That was **necessary but not sufficient**: the suite runs far longer and still
diverges at the same instruction, because of a second, larger problem underneath.

**n64-systemtest is an ELF and the harness does a flat copy.** Its `PT_LOAD` segments target
`0x8000_0000` onward, while `load_direct` places `ROM[0x1000 + k]` at `entry + k`. Every address in
the image is therefore wrong, which is why the first jump — to a perfectly valid `0x8018368C` —
lands in zeros. The ROM-header entry point is *not* the problem; the load mapping is.

`rom::load_elf` now does that: it parses the ELF at the magic-located offset and loads each
`PT_LOAD` to its `vaddr`, zeroing `memsz - filesz` for BSS. `0x1AD150` bytes load correctly and
execution no longer jumps into zeros.

**The instruction-6 fault was the stack pointer.** Disassembling the entry showed a standard
prologue — `ADDIU $29, $29, -0x50` then `SW $31, 0x4c($29)` — and with `$29` at zero that store
targets `0xFFFF_FFFF_FFFF_FFFC`, a KSEG3 address, TLB-mapped, refill. IPL3 leaves `$sp` at the top
of SP DMEM; the direct-load path set no registers at all. Loading the image correctly is not enough
if the register state it was compiled against is missing.

`seed_ipl3_handoff` now sets `$sp`, which moved the fault to instruction ~25: `ExcCode = 11`
(Coprocessor Unusable) — a COP1 instruction with `CU1` clear. IPL3 leaves `Status` at
`0x3400_0000` (`CU1 | CU0 | FR`), the value the COP0 work had already cross-checked against a real
boot capture. Seeding it also clears `ERL` and `BEV`.

**The suite now runs 108,000,000 instructions with zero exceptions** — and still prints nothing.
That is a materially different failure from every previous round: it is no longer lost, faulting,
or NOP-sledding. The remaining question is genuinely "what is it waiting on", which is what the
first round wrongly assumed. Cheapest next check is `isviewer::detect()`: a failed magic
round-trip would silently route all output to a framebuffer console we cannot read.

**The failure moved rather than disappearing.** Before the `$sp` fix the suite ran six instructions
and vectored to `0xBFC0_0200` — the `BEV=1` TLB refill vector, in PIF ROM we do not emulate. Two separate problems
follow: something at instruction ~6 raises a TLB refill (an emulation question), and `BEV` is
still 1 because nothing has cleared it, so *every* exception vectors into PIF ROM and vanishes
silently. A harness that cannot see PIF ROM should arguably fail loudly there rather than execute
zeros.

Reported as "the failure moved to X" rather than "fixed", because each of the last three fixes did
exactly this — and each new failure was more specific than the last.

Recorded because the *method* mattered more than the finding: three budget increases
(2 → 30k → 3M → 45M) all looked like progress and none were. Probing for the first divergence
found it immediately — the same pattern that found the `ERL` rule and the PI dependency.

### Added — a merge-conflict-marker guard (CI + pre-commit)

A `|||||||` diff3 marker was committed into `CHANGELOG.md` during a rebase: the resolution handled
`<<<<<<<`, `=======` and `>>>>>>>` but not the diff3 middle section. **It would have shipped into
the release notes** — a stray marker line is valid Markdown, and valid inside a comment, so
nothing downstream complained.

`scripts/check_no_conflict_markers.sh` now runs in CI and pre-commit, matching the three-layer
shape the commercial-ROM guard already uses: the local hook can be skipped with `--no-verify`, the
CI job cannot.

### Added — FPU arithmetic (T-13-002)

`ADD`/`SUB`/`MUL`/`DIV` in single and double precision, plus `ABS`/`NEG`, as pure functions that
return a value **and the `FCSR` flags they raised** — rather than mutating `FCSR`, which belongs to
`Cop1Control`. An arithmetic helper that reached into it would have to own it.

Three distinctions that are easy to collapse, each pinned:

- **Signalling vs quiet NaN.** Only a signalling NaN raises Invalid. IEEE puts the quiet bit at the
  top of the mantissa, so `is_nan()` cannot tell them apart, and treating every NaN as signalling
  raises Invalid on ordinary quiet-NaN propagation. The bit sits at a different position for `f64`,
  so the double case is not a free consequence of the single one.
- **`x/0` vs `0/0`.** `DivByZero` and Invalid are different flags and a handler distinguishes them;
  `0/0` is an undefined form, not a division fault. `x/0` is also *not* an overflow, despite the
  infinite result.
- **A NaN from non-NaN inputs** — `inf - inf`, `0 * inf` — is Invalid even though neither operand
  was a NaN.

Flags populate **both** the `Cause` and sticky `Flags` fields, since hardware sets them together;
writing only one leaves software unable to distinguish "raised now" from "raised at some point".

**`C.cond.fmt` derives all sixteen conditions from the bit encoding** rather than enumerating
mnemonics. The 4-bit field is systematic (UM Table 7-11): bit 3 raises Invalid when unordered, and
bits 2/1/0 select less / equal / unordered. So `C.EQ` is 2, `C.OLT` is 4, `C.OLE` is 6, and every
signalling variant is its ordinary form plus 8. Sixteen hand-written cases invites getting one
wrong; deriving them makes all sixteen correct or none.

Two things that fall out of the encoding and are worth stating: **`Greater` matches no bit** —
`fs > ft` is "none of less, equal or unordered", which is why three condition bits suffice — and
**the signalling forms raise Invalid on a merely quiet NaN**, which is the entire difference
between `C.EQ` and `C.SEQ` and means the quiet/signalling test used elsewhere is not sufficient
here on its own.

**Conversions** (`CVT`, and the shared rounding the `ROUND`/`TRUNC`/`CEIL`/`FLOOR` forms use)
carry one VR4300-specific rule worth calling out. UM §7.5.2:

> *"When converting a long integer to a single- or double-precision floating-point number
> (`CVT.[S,D].L`), bits 63:55 of the 64-bit integer must be all zeroes or ones, otherwise the
> VR4300 processor raises a floating-point instruction exception."*

That is a **hardware limitation, not IEEE behaviour** — the value is representable and the
processor simply declines. Converting it anyway produces a *correct* number where hardware traps,
so software's fixup path never runs and the divergence surfaces far downstream from its cause. It
raises `Unimplemented` (`FCSR` bit 17), which is deliberately **not** Invalid: "this processor
cannot do this" is a different thing from "the operation is undefined", and conflating them sends
the handler down the numerical-error path.

Two implementation notes that bit during development:

- Rust's float→int cast **saturates**, so `i32::MAX as f32 as i32` is `i32::MAX` again and a
  round-trip inexactness check silently never fires for exactly the value most likely to be
  inexact. The checks go through `f64` instead.
- `Nearest` is **ties-to-even**, not the round-half-away-from-zero that most `round` functions
  implement and that no MIPS mode selects. This crate is `no_std`, so `trunc`/`floor`/`ceil`/
  `round_ties_even` are implemented here rather than pulling in `libm` for four functions.

**The FP multiplication erratum is modelled, deliberately not reproduced.** `fpu::Stepping` says
which console this is, and `mul_erratum_triggers` says when the erratum would fire — but selecting
the affected stepping changes **no arithmetic**, because *what wrong value the erratum produces
has never been characterised*. The trigger is documented, the affected steppings are documented,
the output is not; it sits in the timing supplement's undocumented-constants list.

Inventing a plausible wrong value would be exactly the fitted-constant failure the ledger's
preamble forbids — every later result built on it would stop being evidence. Recorded as ledger
**U-7**. The switch exists now so that when the output *is* characterised it goes in one place
rather than being threaded through afterwards.

Four of five mutations fail the suite. The fifth — `ABS` written as `f32::abs` rather than an
explicit sign-bit clear — is **equivalent**, including for NaN payloads, and is documented as a
readability choice rather than implied to be load-bearing.

### Added — the `ISViewer` result channel (T-12-007, partial)

n64-systemtest reports results through `ISViewer`, a flashcart/emulator convention at
`0x13FF_0000` in cart space rather than real N64 hardware. The suite **probes for it** — writing
`0x12345678` to the buffer and reading it back — and falls back to a framebuffer console we cannot
read if the round-trip fails. So this window is what turns *"the suite runs"* into *"the suite
reports"*.

Text is captured on the **length** write, not on the buffer writes, so a whole line is published
at once; capturing per buffer write would interleave partial lines. An oversized length is clamped
rather than panicking — the value comes from guest code.

**Measured progress on the suite** (each figure is instructions retired before it stops
progressing):

| After | Retired | Outcome |
| --- | --- | --- |
| — | 2 | `ExcCode = TLBS`, `BadVAddr = 0` |
| the `ERL` fix | 30,679 | ran past the 1 MiB IPL3 window |
| PI DMA | 3,000,000+ | **no exceptions**; executing its own ELF from high RDRAM |
| `ISViewer` | 6,000,000+ | no exceptions, **no output yet** |

The suite therefore still does not report a count: it runs cleanly but does not reach its
reporting stage, which points at further unimplemented hardware (VI/RSP initialisation) rather
than at the channel. Recorded as a measurement rather than a claim of progress toward `Failed: 0`.

### Added — the PI DMA engine (T-14-001), pulled forward from Phase 5

n64-systemtest loads the rest of its own ELF from cart through PI, so the Phase 1 exit criterion —
and with it the **v0.2.0 cut criterion** — was unreachable without it. Pulling PI forward keeps
that criterion an honest oracle result instead of weakening it or letting the harness diverge from
hardware by staging the whole ROM itself.

`PI_DRAM_ADDR` / `PI_CART_ADDR` / `PI_RD_LEN` / `PI_WR_LEN` / `PI_STATUS`, with the two rules that
bite both pinned:

- **Length is `len + 1` bytes.** Writing 0 transfers **one** byte. An implementation short by one
  corrupts the *last* byte of every block — which presents as memory corruption rather than as a
  DMA bug, and so gets debugged in the wrong place.
- **`RD`/`WR` are named from the cartridge's point of view**, so `PI_WR_LEN` — the one everything
  actually uses — moves data **cart → RDRAM**. Reversed, the first ROM load writes uninitialised
  RDRAM over the ROM image.

**Review found four defects in the first version, two of them serious**, and all four are now
pinned by mutation-checked tests:

- **A guest `sw` to a length register started four DMAs.** The default `write_u32` composes four
  `write_u8` calls, and PI registers were handled byte-wise — so every PI transfer was wrong, with
  four partly-assembled lengths, and the symptom was memory corruption rather than anything that
  looked like DMA. `write_u32` is now overridden so a PI write is one word write.
- **Clearing the PI interrupt never lowered the MI line.** Only completion updated it, and a
  `PI_STATUS` clear starts no transfer — so `IP2` stayed high forever and any interrupt-driven
  loader would hang. The MI line now mirrors the PI's state on *every* write.
- **Byte writes to the trigger and status registers are dropped, not assembled.** `PI_STATUS`'s
  read bits (busy, interrupt) do not correspond to its write bits (reset, clear-interrupt), so
  reading it back to fill in the other three bytes fabricates command strobes out of status flags.
  Only the address registers — which merely latch — can be safely assembled.
- **`PI_DRAM_ADDR` is doubleword-aligned**, not halfword. Masking only bit 0 lets a transfer start
  mid-doubleword and silently shifts every byte of it.

The engine **returns a description of the transfer rather than performing it**, and the Bus
carries it out. The PI does not own RDRAM — having it reach back into its owner is precisely the
cycle this architecture exists to avoid. It also means charging the transfer real time later is a
scheduling change rather than a rewrite, the same reasoning as `SysAD` being a state machine.

Completion raises the PI line into the MI, which the CPU sees as `IP2`.

### Added — the floating-point register file, moves and FP loads/stores (T-13-001)

32 physical 64-bit **FGRs**, with the `Status.FR` view applied on access rather than assumed:

The view applies to **64-bit accesses only**:

| `FR` | 64-bit (double / `DMxC1`) view |
| --- | --- |
| 1 | FPR *n* **is** FGR *n* — 32 independent 64-bit registers |
| 0 | 64-bit values use **even** indices; the value is the FGR **pair** `FGR[n+1]:FGR[n]` |

`FR = 0` does not make half the register file disappear — **single precision is unaffected**, with
all 32 indices valid under both settings. It changes how a *double* is laid out across the file.

Storing 32 `u64`s and indexing directly is right for `FR = 1` and silently wrong for `FR = 0`,
where a double written to FPR 2 must land in FGRs 2 **and** 3. Both modes occur on real N64
software — IPL3 leaves `FR` set, but plenty of games clear it. And because single precision is
identical under both, the bug survives casual testing: every `.S` operation works and only doubles
break. A single-precision write therefore
**preserves** the upper half of its FGR, since with `FR = 0` that half is the other word of a
double the program never touched.

Plus `MFC1`/`DMFC1`/`MTC1`/`DMTC1` — which **apply the `FR` view rather than moving the physical
register**, per UM Ch. 17's pseudocode: with `FR = 0` and an even `fs`, `data <- FGR[fs+1] ||
FGR[fs]`, the pair, exactly like `LDC1`. Only an *odd* `fs` with `FR = 0` is undefined, and it is
undefined rather than a Reserved Instruction exception. A raw-move implementation round-trips
through `DMTC1`/`DMFC1` correctly and disagrees with `SDC1`, so the test asserts across both
paths — and
`LWC1`/`LDC1`/`SWC1`/`SDC1`, which obey the same alignment, translation and `CU1` rules as the
integer forms. The move-to and FP load/store forms write no general register — `rt` names an FPR,
so giving them a GPR destination would corrupt an integer register.

FP *arithmetic* is still not implemented; this is the file it will operate on.

### Fixed — `Status.ERL = 1` makes KUSEG unmapped (found by running n64-systemtest)

> *"If the ERL bit of the Status register is 1, the user address area is a 2 GB area that cannot
> be cached without TLB mapping (i.e., the virtual addresses are used as physical addresses as
> is)."* — UM §5.2.2, p. 129

**Cold reset sets `ERL`**, so this is the state every boot ROM starts in. Without the rule, the
first store to a low address takes a TLB refill before any mapping could possibly exist.

Found by *running the oracle*, not by reading: n64-systemtest died **two instructions in** with
`ExcCode = TLBS`, `BadVAddr = 0`. With the rule implemented it retires **30,679** instructions
with no exception. No amount of re-reading the TLB code would have surfaced this — the TLB was
behaving exactly as written; the missing piece was upstream of it.

The rule covers the **user area only**: mapped kernel segments stay mapped, so a blanket
"`ERL` ⇒ direct" would silently unmap KSSEG and KSEG3 too. Both directions are pinned and both
mutations fail the suite.

### Changed — README and `.gitignore` brought up to date

`README.md` described the superseded fractional scheduler, claimed Phase 1 had not started, and
carried a "no chip emulates anything" banner that stopped being true several tickets ago. It now
describes the canonical 187.5 MHz clock, the five-stage pipeline, the integer set, COP0, the TLB
and the exception model — and is equally explicit that **everything else is still a stub** and
that a green test run does not mean a subsystem works. The accuracy table reports the one gate
that produces a real number (`basic.z64`, 5/5) instead of asserting that none do, and points at
the accuracy ledger. Release status now names the v0.1.0 tag and the v0.2.0 cut criterion rather
than saying no release has been cut.

Kept deliberately out of the README: per-release detail. That belongs here.

`.gitignore` gained `*.preedit` / `*.pre` (snapshots taken before instrumenting a file, so cleanup
restores a known copy rather than discarding unrelated uncommitted work) and `um.txt` (the
extracted VR4300 manual text — a large derived artifact whose extraction is *not* reproducible by
the obvious command, since `pdftotext` scrambles that PDF and only `mutool` works, which is
precisely why someone would be tempted to commit it).

### Added — `CACHE` executes instead of raising (T-12-005, partial)

`CACHE` decoded to `Reserved` and therefore **raised**. IPL3 and libdragon both issue it, so that
blocked every real ROM — the last hard blocker of its kind in Sprint 2.

It now decodes, resolves its effective address — so it can still raise a TLB fault, like any other
memory instruction — and performs no data transfer. Its `rt` slot is the **operation selector**,
not a destination; decoding it as a load clobbers whichever GPR the cache-op encoding names, so
the register destroyed would depend on which operation was requested.

**Only the address-addressed operations translate.** `op4..2` 0–2 are `Index_*`, defined *"at the
index specified"*, so they never consult the TLB; 3 and 4–6 are defined in terms of *"the specified
address"* and do. Translating unconditionally raised spurious TLB refills on exactly the code that
matters — cache-init walks every index with an arbitrary base, before any mapping exists to satisfy
it. Caught in review, where the first version's *comment* described the distinction while the code
ignored it and the test asserted the wrong side of it.

**The cache depth question is answered explicitly, and the answer is zero.** Phase 1 listed
"how exact must the cache model be" as an open question. Cache *contents* are not modelled at all,
so invalidate and write-back have nothing to act on — which is observationally sound **only**
because no cache state exists to become stale. Recorded as ledger **D-5**, together with the point
it stops being sound: Phase 5, when cart/RSP DMA writes land in RDRAM behind a cache games
explicitly flush.

Cache-miss costs are **deferred with the model** rather than applied to a cache that does not
exist, and **`M` remains unmeasured with no value** (ledger C-1). That criterion is met by *not*
producing a number: a fitted `M` would make every later timing result unfalsifiable.

### Added — COP1 control registers and coprocessor usability (T-12-006)

**Control registers only** — `CTC1`/`CFC1` on `FCR31` and `FCR0`. FPU arithmetic is Sprint 3, and
the module says so in its own docs so the ticket cannot grow into Sprint 3 by stealth.

It exists for one reason: n64-systemtest calls `set_fcsr(...)` — `ctc1::<31>` — as the **fourth
statement** of `entrypoint()`, so without it the suite dies three statements in and every COP0 and
TLB test in Sprint 2 is unreachable behind it.

**Coprocessor Unusable** is checked in `EX` with `Cause.CE` naming the unit. Two rules pinned in
both directions: COP0 is usable from kernel mode regardless of `CU0` — otherwise a handler could
not run before `Status` was set up — but that exemption is *not* a blanket bypass, and user mode
without `CU0` still raises.

A valid-but-unimplemented COP1 encoding decodes to `Cop1Unimplemented`, **not** `Reserved`, and
does not raise when `CU1` is set. That makes Sprint 3's arithmetic an addition rather than a
behaviour change; raising here would look correct until the FPU landed.

All seven mutations were confirmed to fail the suite.

### Added — the TLB and the instruction micro-TLB (T-12-004)

32 fully-associative joint-TLB entries mapping even/odd page pairs, plus the **two-entry
instruction micro-TLB** in front of it. A micro-TLB miss is a **stall** (3 PCycles); a JTLB miss
is an **exception**. Modelling only the JTLB would not approximate that cost — it deletes the
structure the cost occurs in.

`KUSEG`, `KSSEG` and `KSEG3` are now genuinely TLB-mapped. Previously they were masked to their
low 29 bits, which silently aliased every access onto real memory instead of faulting.

**The rules that pass ordinary tests while being wrong**, each pinned:

- **`V` does not participate in matching** (UM §5.4.9). An invalid entry still *matches*, so it
  raises TLB Invalid — which shares an `ExcCode` with a refill and differs **only in vector**.
  Checking `V` while matching sends a protection fault to the refill handler, and also stops TLB
  shutdown firing on duplicates involving an invalid entry, which UM Fig. 6-6 requires.
- **`G` is the AND of both halves**, not the OR. An OR makes far too many entries global, and a
  global entry matches every `ASID` — so it presents as address-space leakage rather than a
  missing translation.
- **`PFN` is always in 4 KiB units** whatever the page size, so a large page's frame number has
  low bits masked off rather than scaled.
- **Only `C == 2` is uncached**; the VR4400's other coherency encodings collapse on a part with
  no coherency protocol.
- **`TLBWI` can overwrite a wired entry; `TLBWR` cannot** — and the protection is structural
  (`Random` never goes below `Wired`), not a check. Guarding both makes wired entries unwritable.

**Ledger D-4 added: TLB entries reset to distinct tags, not zero.** All-zero is not a usable state
— 32 entries at `VPN2 = 0`, with `V` out of the matching rule, means the first access to virtual
page-pair 0 matches all 32 and triggers TLB shutdown. Found by a test, not by reasoning.

`addr::translate` was **deleted** rather than kept "for unmapped-only paths" once nothing called
it: an unused function that quietly gets translation wrong is the inert-API hazard
`docs/engineering-lessons.md` §3.2 describes.

**Review caught a bug the entire test suite was structurally blind to.** TLB matching compared the
raw 64-bit address, so a sign-extended kernel address — KSEG3 is `0xFFFF_FFFF_E000_0000`, not
`0xE000_0000` — pitted its sign extension against a `VPN2` holding only VA(39:13). **No mapped
kernel address could ever match**, however correct the entry. Every TLB test used KUSEG, which has
no sign extension, so nothing could see it. Matching now compares the architectural `R` and
`VPN2` fields, and there are regressions for both translating and probing a KSEG3 address.

Three more from the same review: `EntryHi.R` was left zero on a fault (which makes the handler
install an entry that can never match, so an infinite refill loop rather than a wrong value);
`TLBP` masked the region away before probing; and **TLB shutdown set a flag nobody could
observe** — the ticket's own criterion said `Status.TS ← 1` and *TLB unusable*, and neither half
was propagated.

All ten original TLB mutations plus six of the seven fix mutations fail the suite. One initially survived — nothing
checked that TLB Invalid takes the *general* vector — and there is now a test for the one
distinction `Cause` cannot express.

### Added — interrupts, `Count`/`Compare`, and the MI line (T-12-003)

Assertion and recognition are kept **separate**, because they are different things: `Cause.IP`
tracks what hardware asserts regardless of masks — software polls it — while recognition applies
`IE`, `EXL`, `ERL` and `IM`. Folding them together makes a masked interrupt invisible to
`MFC0 Cause`.

Dropping the `EXL`/`ERL` terms is the classic version of this bug: it works until an interrupt
arrives inside a handler, and then re-enters it forever. All four terms are pinned individually.

**`Count` is derived, never incremented.** ADR 0006 permits exactly one incremented counter and it
is `master_ticks`; the scheduler supplies the `Count` timeline (half PClock) and COP0 adds an
epoch that `MTC0 Count` re-bases. So the register is guest-writable *without* being incremented,
and cannot drift from the master clock. `Cpu::tick_at` is the seam. The scheduler's `count_ticks`
doc comment predicted this exact split and is now correct rather than aspirational.

**`IP7` is latched**, not a level: `Count == Compare` sets it and only a `Compare` write clears it
(UM §6.4.18, p. 200). The first version of this modelled it as a level tied to the equality, which
looked tidier and was wrong — the existence of a *documented* clearing mechanism is itself the
evidence that it latches, since a level would self-clear and need none. Worse, a level silently
**drops** any timer interrupt raised while `EXL` is set, because the equality holds for one tick
and the handler never sees it. Caught in review; now pinned by a test that fails against the level
implementation.

**Ledger U-4 closed:** the MI drives **`IP2`**. Neither the CPU manual (board-level) nor the
N64brew mirror states it; libdragon does — `C0_INTERRUPT_RCP = C0_INTERRUPT_2` — and is public
domain, so it is citable rather than merely observed. It also gives `IP3` = CART, `IP4` = PRENMI,
`IP7` = timer.

All seven mutations of the interrupt path were confirmed to fail the suite.

### Added — exception dispatch and `ERET` (T-12-002)

Exceptions were previously stamped into pipeline latches and never *dispatched* — nothing wrote
`EPC`/`Cause` or redirected the PC. Now the full epilogue runs (UM Fig. 6-14, p. 201) and the
pipeline stalls the documented 2 PCycles.

**The `EXL` gate is the part worth calling out.** `EPC` and `Cause.BD` are written **only when
`EXL` was already 0**; with it set, both are left untouched. That is the entire purpose of `EXL`
(UM §6.3.7, p. 174), and an implementation that always writes `EPC` passes every single-exception
test while corrupting every nested one — where nesting is the *normal* path for TLB refill
handlers, not an edge case. Pinned both directly and through the pipeline.

`AddressError` now carries its direction, because `AdEL` (4) and `AdES` (5) are different
`ExcCode` values and a handler distinguishes them. An instruction fetch is a load.

The vector table implements ledger **S-3**: a refill arriving with `EXL` set takes the **general**
vector, not `0x080`. UM Fig. 6-15 says `0x080` and is contradicted by two tables, by §6.4.8 twice,
and by Fig. 6-14. A second manual typo is pinned too — p. 181's prose gives the `BEV=1` general
vector as `0x8000_0180` where Table 6-4 makes it `0xBFC0_0380`.

`ERET` **completes the Sprint 1 `LL`/`SC` contract**: it is the only thing besides cache
invalidation that clears `LLbit` (UM §3.1), so until now `LL`; `ERET`; `SC` wrongly succeeded. It
also has no delay slot, alone among the control transfers, so the already-fetched next instruction
is squashed.

All ten mutations of the epilogue were confirmed to fail the suite, including removing the `EXL`
gate, restoring the `0x080` vector, `ERET` clearing `EXL` instead of `ERL`, and `ERET` gaining a
delay slot.

### Added — the COP0 register file (T-12-001)

Table-driven, because nearly every accuracy rule in COP0 is data rather than logic — "register N
is 64 bits wide", "bits 23:16 of `Config` are hardwired". Written as data each rule is asserted
directly against the manual; written as `match` arms they become 32 places to forget one.

- **Exactly eight 64-bit-wide registers**, pinned element by element: there is no generating rule,
  and a wrong entry is invisible until 64-bit software runs.
- **Per-register writable-bit masks.** A write to a masked-off bit preserves the old value rather
  than zeroing it — hardware discards the write, which is not the same thing. `Cause` is read-only
  except `IP1:IP0`; `Status.DS.TS` is read-only while the rest of `DS` is not.
- **`Config` hardwired fields are merged on read**, so no write path can erase them. Seeding them
  at construction would let a too-wide mask destroy them permanently.
- **Cross-checked against the real N64 IPL boot values**, which decompose exactly: `0x0006E463`
  reproduces both hardwired constants bit-for-bit with `BE=1`, `K0=3`. That is the cheapest
  evidence available that the field positions are right, and it comes from outside the manual.
- **`LLAddr` now has one storage location.** Sprint 1 put it on `Pipeline` because COP0 did not
  exist; it is COP0 register 17, and two copies of one architectural value would have let
  `MFC0 $rt, $17` disagree with the CPU's own link address.

A second mask table, `ARCH_MASK`, records which bits each register can ever *return*. It is
applied on read **and** on `set_hardware`, so a value that is not architecturally representable
cannot enter the file, let alone leave it. This closes a latent bug for T-12-002: exception
dispatch bypasses the write masks by design and will feed `set_hardware` raw faulting addresses,
which would otherwise have put non-zero bits in `EntryHi.Fill` — a field that architecturally
reads zero — and non-zero upper halves into 32-bit registers.

`MFC0`/`DMFC0` read in **DC** and `MTC0`/`DMTC0` write in **WB**, because UM §4.6.9 defines the
CP0 bypass interlock in terms of a write reaching WB while the next instruction reads in DC.
Doing both in EX would make that interlock unexpressible — the same mistake ADR 0007 exists to
prevent one level up.

Undocumented behaviour is marked as such rather than invented: reserved registers 7/21–25/31
(ledger U-1) and `PRId.Rev` (U-3) are explicit guesses with tests pinning the *choice*, not the
hardware. All nine mutations of the mask and width tables were confirmed to fail the suite.

### Added — `LL` / `SC` / `LLD` / `SCD`, closing a Sprint 1 gap (T-11-003)

The synchronisation pair was listed in T-11-003's acceptance criteria and in the Phase 1 exit
criteria, and had **not** been implemented — the four opcodes decoded to `Reserved`. Found while
verifying Sprint 1's criteria against the code rather than against the ticket's own checkboxes.

- `LL`/`LLD` arm the link bit and record `PA(31:4)` in `LLAddr` (COP0 reg 17, diagnostic-only per
  UM §5.4.7). `SC`/`SCD` test it, store only if set, and write 1/0 to `rt` **either way**.
- A misaligned `SC` **raises** rather than reporting failure — *"If this instruction both fails
  and causes an exception, the exception takes precedence"* (UM §16 p. 487). A misaligned `LL`
  leaves the link disarmed.

### Fixed — three "undocumented" timing constants were documented all along

A research pass for Sprint 2 re-opened the VR4300 manual and found that three constants recorded
across `docs/accuracy-ledger.md`, `docs/cpu.md` and the timing supplement as *undocumented* are
stated plainly in the sections those notes cited as lacking them:

| Constant | Value | Where it actually is |
| --- | --- | --- |
| Exception epilogue stall | **2 PCycles** | UM §4.7 p. 114 — the section's opening sentence |
| CP0I (CP0 bypass interlock) | **1 PCycle** | UM §4.6.9 p. 113 |
| ITM (instruction micro-TLB miss) | **3 PCycles** | UM §4.6.2 p. 107 — never looked for |

The ledger said *"no figure appears in UM §4.7 or chapter 6"* while the figure is §4.7's first
paragraph. The cause was search shape, not misreading: numbers were looked for in tables, and
these are in prose. CEN64's 2 is therefore corroboration rather than the origin.

Ledger entries C-2 and C-3 are reclassified from *not yet measured* to documented-with-citation,
C-7 is added for ITM, and the general lesson is recorded as `docs/engineering-lessons.md` §3.3b:
*"undocumented" is a claim about a document, and unlike a claim about behaviour, nothing ever
fails when it is wrong* — so it spreads between files and licenses fitted constants. The
`ref-docs/` correction lands as a new dated supplement, since that corpus is immutable.

### Fixed — ledger S-3 resolved: the `EXL=1` vector is `0x180`

Recorded as MIPS-docs-versus-CEN64. It is neither — **the manual contradicts itself**. Tables
6-3/6-4 (p. 181) define the refill offsets only for `EXL=0`, and §6.4.8 says twice (pp. 187, 188)
that with `EXL=1` you take the common vector. One flowchart, Fig. 6-15 (p. 203), says `0x080` and
is wrong. CEN64 is right, and its source comment that `0x080` *"doesn't make any sense"* is a
reaction to that figure. Kept as a ledger entry rather than deleted, because Fig. 6-15 is still
in the manual and the next reader will find it.

### Added — Sprint 2 planned (T-12-001 … T-12-007)

`to-dos/phase-1-cpu-golden-log/sprint-2-cop0-tlb-exceptions.md`: COP0, the TLB (including the
two-entry instruction micro-TLB in front of the JTLB), the exception model, `CACHE`, and COP1
*control* access. The goal is n64-systemtest reporting a genuine number — the first oracle this
project did not write itself. Its blockers were established by reading the suite's source rather
than assuming: `entrypoint()` calls `CTC1 $31` as its fourth statement, and `main()` immediately
installs handlers at all three exception vectors.

### Added — `SYNC` retires as a NOP instead of raising

Found by the same audit: `SYNC` (SPECIAL funct `0o17`) decoded to `Reserved`, so it would have
raised a reserved-instruction exception on code that runs fine on hardware — and compilers emit
it. *"all load/store instructions in this processor are executed in program order since the SYNC
instruction is handled as a NOP"* (UM §3.1).

The audit's other findings were all legitimately out of Sprint 1 scope — COP0 (`0o20`), COP1
(`0o21`), `CACHE` (`0o57`) and the coprocessor load/store forms belong to Sprints 2 and 3 — or
genuinely unassigned encodings that *should* raise.

### Fixed — `docs/cpu.md` had the link-bit clearing rule wrong

The spec said *"Any intervening store (or ERET) clears the link"*. The manual's list is
exhaustive and does not include stores: *"set by the LL instruction, cleared by an ERET, and
tested by the SC instruction"* (UM §3.1). `SC` is a **tester, not a clearer** — clearing it there
looks right, matches other architectures, and makes the second iteration of an `LL`/`SC` retry
loop fail forever. Now pinned by `sc_does_not_clear_the_link_bit`, and all five mutations of the
new behaviour were checked to fail the suite before the tests were kept.

`ERET` is the one thing that does clear it, and it arrives with the exception model in Sprint 2;
until then nothing clears the bit, which `docs/cpu.md` now states rather than leaving implied.

### Added — the `SysAD` transaction model (T-11-008, partial)

`sysad.rs` models the CPU↔RCP bus as a **packet protocol** — an address cycle carrying a
command, then a data cycle carrying the payload, with an unbounded wait between them where real
RDRAM latency lives. Neither reference emulator models this: both complete the access atomically
and charge a flat constant, and they disagree on the value.

- A `Transaction` is a **state machine** that is structurally incapable of completing in its
  address phase — pinned by `a_transaction_can_never_complete_in_its_address_phase`, since the
  whole point is that a device can observe the bus mid-transaction.
- The inter-phase wait is a **caller-supplied parameter**, not a constant, so it cannot be
  quietly tuned. The caller must justify what it passes.
- Block transfer orderings, including the **sub-block quirk**: a D-cache 128-bit read whose
  address bit 4 is set returns the addressed 64 bits and then the 64 bits *below* it — not the
  ones after. I-cache reads are always sequential regardless.

**`SYSCMD` bit-4 polarity resolved, and it was never a contradiction.** Ledger entry S-1 recorded
the manual and the wiki as disagreeing. Reading both carefully, they **agree on every bit**: a
request has bit 4 clear, a data beat has it set. They differ only in that the wiki calls the
*data-identifier* cycle "Command". No test ROM was needed. Kept as a ledger entry rather than
deleted, because the next reader will hit the same apparent conflict.

**Two criteria are deferred and say so.** The RCP is not yet stepped *between* the phases — the
model supports it, but driving it needs the scheduler to own the transaction rather than `DC`
completing inline, which is a `Bus`-trait change belonging with the cache model in Sprint 2. And
`M` is still unmeasured, exactly as this ticket's own note predicted: `basic.z64` is too short to
constrain it. `M` stays an explicit ledger entry with **no value** rather than a fitted-looking
number without provenance.

### Added — the determinism contract is exercised (T-11-007)

ADR 0004 said "same seed + ROM ⇒ bit-identical output" and nothing checked it. Four tests now do.

- **Bit-identical across runs** — the *whole* machine, not a summary: registers, `HI`/`LO`, PC,
  all three cycle positions, and a content hash of all of RDRAM. A partial hash can hide a
  divergence in a field that only later leaks into the hashed region. Repeated eleven times,
  because an entropy dependency surfaces intermittently rather than on the very next run.
- **Different seeds produce different machines**, added beyond the stated criteria — without it,
  a build that ignored the seed entirely would pass the first test.
- **Reset is reproducible** regardless of what ran before it.
- **A source-level guard** rejects `std::time`, `SystemTime`, `Instant::now`, `getrandom`,
  `thread::spawn`, `HashMap` and `HashSet` anywhere in the core crates. Deliberately *not*
  behavioural: such dependencies are intermittent, so a run-twice test can pass for months
  before the first divergence. This fails on the commit that introduces one, naming file and line.

The content hash is FNV-1a rather than `DefaultHasher`, whose output is randomised per process
— using it would have made the determinism test itself nondeterministic.

Mutation-tested: ignoring the seed fails the second test, and naming a banned construct in any
core crate fails the fourth.

### Added — the documented errata are recorded and pinned (T-11-005)

`docs/cpu.md` gains a **"reproduced, not corrected"** section stating each VR4300 erratum as
intended behaviour, with the manual-vs-hardware divergence spelled out and the pinning test
named. The manual documents none of these; the wiki is the only source.

- `SRA`/`SRAV` leak the upper 32 bits — **all consoles**, never known to be fixed.
- `MULT` is 64-bit × **35-bit**; `DIV` is 32-bit ÷ **35-bit** (divisor sign-extended on bit 34).
- Two new tests: `div_reproduces_the_35_bit_divisor_sign_extension_erratum` and
  `srav_shares_the_sra_erratum` — the variable shift form shares the erratum, and implementing
  one correctly and the other "properly" is an easy inconsistency.

All four errata guards are **mutation-tested**: "correcting" `sra` per the manual fires both the
`SRA` and `SRAV` guards, and reducing `div` to a plain 32-bit division fires its own.

**The FP multiplication bug is deferred to Sprint 3** and recorded rather than silently dropped.
It needs COP1, and it is the only erratum that is *not* universal — NUS-01/02/03 only — so it
also needs the console revision as a machine parameter. Its exact corrupted output is
undocumented and will have to be characterised against hardware.

### Added — the first ROM actually runs (T-11-006)

**`basic.z64` executes end to end and passes all five of its hardware-verified cases** — delay-slot
semantics, `J` in a delay slot, `BEQL` nullification, `BNEL` nullification, and `LWU`+`DADDI`.
59 instructions retired, 129 master ticks. `docs/STATUS.md` gains its first real accuracy number.

This is the first time the emulator has executed a ROM at all, and it validates the delay-slot
and branch-likely work against something other than our own expectations.

- `addr.rs` — virtual→physical translation, the prerequisite the plan had never written down.
  KSEG0/KSEG1 are unmapped and become a subtraction; the mapped segments are masked with a
  `TODO` until the TLB lands. `Cached` is returned now because the *segment* determines it and
  that information is unrecoverable once KSEG0 and KSEG1 have become the same number.
- `rom.rs` — direct loading, doing what IPL3 does: copy `0x10_0000` bytes from ROM offset
  `0x1000`, clamped to both the ROM's length and RDRAM's capacity. A commercial ROM is up to
  64 MiB against 8 MiB of RDRAM, so an unclamped copy panics on exactly the corpus it is for.
- `run_until_complete` implements Dillon's `r30` protocol for real. The pass sentinel is `-1` as
  a **64-bit** value; matching only the low 32 bits would also match `0xFFFF_FFFF`, which is a
  *failure* index.

**The test witnesses execution rather than trusting the sentinel.** `basic.z64` ends with
`BNE $30, $0, TestsFailed` / `ADDI $30, $0, -1`, so if `r30` is still 0 the branch falls through
and the pass sentinel is written **anyway** — "ran and passed" and "never ran" are identical at
the sentinel. The test therefore also asserts the PC entered the test subroutines and that a
plausible number of instructions retired. Mutation-tested: pointing the subroutine address
somewhere unreachable fails it with "this is a vacuous pass".

The test **skips rather than fails** when the ROM is absent — Dillon's suite has no licence, so
it is external-tier and CI has no copy. A gate that is red by default stops being read.

### Added — branches, jumps, and the trap family (T-11-004)

The full control-flow set: `J`/`JAL`/`JR`/`JALR`, every conditional branch including the eight
**branch-likely** forms, the twelve conditional traps, `SYSCALL` and `BREAK`.

**The delay slot now does real work**, and the reverse cascade turns out to make it fall out
cleanly. By the time `EX` resolves a branch, `IC` has already fetched the delay slot — that *is*
the architectural delay slot, not a modelling artefact. Because `EX` runs before `IC` in the same
cycle, writing the target to `next_pc` in `EX` makes the very next fetch land on it with exactly
one delay slot in between. No wrong-path fetch needs squashing.

**Branch-likely nullifies its delay slot when not taken**; an ordinary branch does not. Getting
that backwards silently runs or skips one instruction per untaken branch — invisible until a
loop's trip count is wrong. Mutation-tested.

Two bugs found by a fetch trace rather than by reasoning:

- The redirect was never applied at all — an earlier edit had failed to write, and everything
  still compiled and passed, because the test program's branch target coincided with the
  sequential path. The trace showed `next_pc` marching straight through.
- `in_delay_slot` read `ic_rf`, which `rf_stage` has already vacated by the time `IC` runs in the
  reverse cascade. The flag was silently always false. It now reads `rf_ex`.

**`in_delay_slot` is pinned by its own test**, because mutation-testing showed it was *not yet
load-bearing*: forcing it to `false` broke nothing, since its only consumer is `Cause.BD`/`EPC`
at exception time and COP0 arrives in Sprint 2. Rather than delete it — it is genuinely needed
and must ride in the latch — it is now verified from the moment it is written.

The reserved-instruction tests are repointed at encodings the VR4300 leaves **architecturally**
unassigned (primary opcodes `0o35`/`0o36`, SPECIAL funct `0o01`) rather than at instructions this
project merely hasn't implemented. They had to be moved three times — `LW`, then `BEQ` — because
they were tracking implementation progress instead of the architecture.

### Fixed — a stale test-ROM sentinel, and T-11-006 re-scoped

Investigating whether Sprint 1 could actually close turned up a documentation error that would
have cost real debugging time, and a dependency the plan had wrong.

- **The n64-systemtest completion string in our docs was from 2021.** `docs/testing-strategy.md`
  and `tests/roms/README.md` both quoted `Done! Tests: 262. Failed: 0`. That string does **not**
  appear in the committed v2.1.0 ROM — verified with `strings`, zero occurrences. The real
  summary has the form `Finished in <T>s. Base: Failed <F> of <N> tests (<P>% success rate)`,
  whose counts vary per run — so the sentinel must match the stable pattern
  `Failed (\d+) of (\d+) tests` rather than any literal line. Anything written against the old
  string would have matched nothing, silently. Both documents corrected, and the three text
  sinks the ROM actually uses (emux COP0 hooks, ISViewer, SC64) are now documented — it writes
  to **no fixed RDRAM address**, and with no sink implemented `text_out` is a silent no-op.
- **T-11-006 re-scoped.** n64-systemtest cannot report anything in Sprint 1: it dies at
  `src/main.rs:68` on `CTC1 $31`, the third statement after entry, and needs COP1 control,
  COP0, MI, PI, VI, a heap and exception vectors first — a large fraction of its tests fault
  deliberately and would hang rather than fail. No flag avoids this; category selection is
  compile-time. Retargeted at **`basic.z64`**, which is uniquely suitable: it is the only
  Dillon ROM that does not PI-DMA itself at startup, its result protocol is a single GPR
  (`r30`: 0 running, `-1` pass, `1..=5` failing index), and it needs only the integer core plus
  the T-11-004 branch/jump family. The n64-systemtest goal moves to T-11-009 in Sprint 2.
- Two prerequisites nobody had written down are now acceptance criteria: **KSEG0/KSEG1 segment
  stripping** (nothing does it today, so no ROM can execute) and a **direct-load path**. Also
  recorded that Dillon's suite has no licence, so its test must **skip** rather than fail when
  the ROM is absent.
- T-11-008 notes that fitting `M` needs a ROM that runs long enough to measure — realistically
  n64-systemtest's default-off `timing` set, which is Sprint 2. The transaction model is Sprint
  1 work; the measurement may not be.

### Added — loads, stores, and the unaligned family (T-11-003)

- `mem.rs` — load/store data shaping as pure byte-level transforms. Width and signedness rules
  (`LW` sign-extends into the 64-bit register, `LWU` does not — confusing that pair silently
  breaks any address above 2 GiB), alignment requirements, and the `LWL`/`LWR`/`LDL`/`LDR` and
  `SWL`/`SWR`/`SDL`/`SDR` merges.
- The unaligned family is **big-endian-directional**, and getting a shift backwards produces
  plausible values that are wrong only at unaligned addresses. It is tested as a *pair* — the
  way it is actually used — plus a store-then-load round-trip across every byte offset, which is
  the strongest statement that the shift directions agree with each other.
- `EX` resolves the effective address (base + **sign**-extended offset) and `DC` performs the
  access. That split is the point of the pipeline: `DC` is the cycle the scheduler interleaves
  the RCP around.
- An unaligned *aligned-form* access raises `AddressError`; the `LWL`/`LWR` family is exempt by
  construction, since being usable at any byte offset is the reason it exists.

**The load-delay interlock is live** (UM §4.6.5) — it finally has something to interlock against.
It reproduces the hardware's documented imprecision, and it compares against the `ex_dc` latch
rather than `rf_ex`: in the reverse cascade `EX` runs before `RF`, so by the time `RF` executes,
the instruction that was in `EX` has already moved on. Checking the wrong latch made it silently
never fire, which is what the integration test caught. Mutation-tested.

### Added — decode and `EX`-stage wiring: the CPU executes (T-11-002, second part)

The pipeline stops moving empty latches and starts running programs.

- `decode.rs` — MIPS III decode for the integer subset. **Total**: every 32-bit pattern decodes
  to something, with anything unrecognised becoming `Op::Reserved` rather than panicking. A guest
  can execute arbitrary bytes. Unimplemented opcodes decode to `Reserved` (which raises a
  reserved-instruction exception) rather than to a `NOP`, so a missing opcode is loud instead of
  silently producing wrong results.
- `regs.rs` — the register file, split out so `$zero`'s hardwiring lives in exactly one place.
  A scattered `if rd != 0` is how a write to `$zero` eventually slips through.
- `exec.rs` — `EX` execution as a pure function of `(op, operands, HI/LO)`, bridging decode to
  the ALU. Carries the immediate sign/zero-extension asymmetry: arithmetic forms sign-extend,
  logical forms zero-extend, so `ORI $t0, $0, 0xFFFF` yields `0xFFFF` and not all-ones.
- The pipeline stages now do their jobs: `IC` fetches and decodes, `RF` reads, `EX` executes and
  raises the multiply/divide stall, `WB` commits.

**The operand bypass network (UM §4.6) — found by the end-to-end test, missed by every unit
test.** Without it, back-to-back dependent instructions read stale registers, and `LUI`+`ORI` —
the standard way to build a 32-bit constant — produced `0xFFFF` instead of `0x7FFFFFFF`. Every
one of the 46 unit tests passed while the CPU could not run a six-instruction program. The
bypass is mutation-tested: removing it fails the end-to-end test.

Three integration tests that exercise the whole machine rather than a piece of it: a real
program computing through `ADDIU`/`MULT`/`MFLO`/`ADDU`/`SLL` into the register file; `$zero`
surviving an instruction that targets it; and an overflowing `ADD` aborting without committing.

### Added — the integer ALU (T-11-002, first part)

`crates/rustyn64-cpu/src/alu.rs`: the arithmetic, logical, shift and multiply/divide families as
**pure functions**, deliberately free of pipeline and register-file state so every rule can be
tested without constructing a machine. Decode and the `EX`-stage wiring are the remainder of
T-11-002 and follow separately.

- 32-bit arithmetic (`ADD`/`ADDU`/`SUB`/`SUBU`) trapping on signed overflow where specified, and
  the 64-bit `D*` forms trapping at 64-bit boundaries.
- The logical family (`AND`/`OR`/`XOR`/`NOR`/`LUI`) and all shifts including the `D*` forms.
- `MULT`/`MULTU`/`DIV`/`DIVU` and the `D*` forms writing `HI`/`LO`, with the documented
  full-pipeline stalls (5 / 37 / 8 / 69 `PCycle`s, UM Table 3-12) — these are not background
  operations.
- **Every 32-bit result is sign-extended into the 64-bit register**, the rule that dominates
  MIPS III and the most common source of emulator bugs in it, since it stays invisible until
  software inspects the upper half.
- The `MFHI`/`MFLO` hazard is recorded as a **non-interlocked** two-instruction window producing
  hardware's wrong result — modelling it as a stall would add timing hardware does not have *and*
  hide the value software can observe.

**Two errata reproduced rather than corrected**, each pinned by a test that fails if it is
"fixed":

- `SRA`/`SRAV` leak the upper 32 bits instead of sign-extending bit 31. Present on every console
  and never known to be fixed, so software can depend on it.
- `MULT` acts as a 64-bit by **35-bit** signed multiply (second operand sign-extended on bit 34).
  Invisible for well-formed inputs, which is why ordinary compiler output never trips it.

Two behaviours are **guesses and are recorded as such** in `docs/accuracy-ledger.md` (C-5, C-6)
rather than left looking authoritative: the `DIV` quotient when divisor bits 63 and 31 differ
(N64brew calls this "currently unclear"), and the architecturally-undefined divide-by-zero
values. What is tested and non-negotiable is that neither panics — a guest can do both at will.

### Fixed — rustdoc gets its own CI job

`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` was the **last step of the
`test` job**, after the slow test run. Two consequences, both observed rather than theorised:

- It was the most likely thing to be lost to `cancel-in-progress`. A commit with a broken
  intra-doc link (public docs linking to a private item) went through with its run marked
  **`cancelled`, not failed** — rustdoc never executed. The gate did not fail; it did not run.
- A rustdoc failure reported as `test (ubuntu-latest) failed`, pointing at the wrong subsystem.

Now a dedicated `rustdoc (-D warnings)` job with no `needs:`, so it starts immediately, runs in
parallel with the tests, and finishes in well under a minute — fast enough to be useful feedback
and small enough to rarely be mid-flight when a supersede happens. Verified by reintroducing the
exact defect that slipped through: the job's command rejects it, and `cargo test` is blind to it.

### Added — the ADR 0007 five-stage pipeline (T-11-001, second half)

`crates/rustyn64-cpu/src/pipeline.rs`. **Structure, not instructions** — the stages move latches
and account for time; decode and execute are T-11-002 onward. What is real here is the shape,
which is the part that cannot be retrofitted without rewriting every consumer.

- Four inter-stage `Latch`es (`ic_rf`, `rf_ex`, `ex_dc`, `dc_wb`), each carrying `pc`, `word`,
  `occupied`, `in_delay_slot`, and `abort`. Five stages have four boundaries; the state lives on
  the boundaries.
- `Pipeline::advance` runs **WB → DC → EX → RF → IC**. Each stage reads its input latch before
  any upstream stage writes it, so no value moves two stages in one cycle and no double buffering
  is needed — the reverse order *is* the latching.
- `Stall { cycles, cause }` with `Interlock` naming all eight documented interlocks (LDI, DCB,
  DCM, ICB, ITM, MCI, **COp**, CP0I — UM Table 4-3) so a stall is always attributable in a trace.
  ADR 0007's `resume_stage` is deliberately **absent** until it can be load-bearing: `advance`
  always runs the full cascade today, so a stored `resume` would be read by nothing — the same
  hazard `poll_irq_at_phase` was removed for (`engineering-lessons.md` §3.2).
- An abort raises a pending flush, so the instruction fetched later in the *same* cycle is a
  bubble rather than a live wrong-path fetch that would escape the flush entirely.
- `stall_for(0)` is ignored rather than recorded — a zero-cycle stall would still consume a cycle
  and mark it not-a-run-cycle, silently inserting a bubble *and* suppressing interrupt acceptance
  on the following cycle.
- `Exception` is deliberately **not** named `Fault`: UM §4.5 defines a fault as interlocks ∪
  exceptions, and only the aborting subset rides in a latch.
- `abort_from` stamps an exception into its own latch and every latch **upstream** — the
  kill-younger-instructions step. Older instructions are untouched.
- Interrupts are sampled once per `PClock` in **DC** (UM Figure 4-12, §4.7.6) and accepted only
  if the previous `PCycle` was a run cycle (§4.7.1). Exactly one recognition predicate exists.
- `load_interlocks` reproduces the hardware's documented **imprecision** — matching on the `rs`
  *or* `rt` encoded field whether or not it is used as a source, exempting `$zero`, and not
  crossing the GPR/FPR boundary. Emulating precise behaviour here would be the bug.

Seven pipeline tests, two of which are the structural guards and both **mutation-tested**:

- `a_value_advances_exactly_one_stage_per_cycle` — reversing the cascade to run forwards fails it.
- `delay_slot_flag_survives_a_multi_cycle_stall` — the Phase 1 exit criterion. Dropping the flag
  in transit fails it. A global `in_delay_slot` bool passes a naive test and fails this one.
- `an_abort_survives_the_cascade` — removing the flush fails it.
- Plus stall-freezes-the-pipeline, the interrupt run-cycle gate, abort-kills-younger-only,
  aborted-instructions-do-not-retire, zero-cycle-stall-is-not-a-stall, and the load-interlock
  imprecision cases.

Two existing tests had premises invalidated by this change and were corrected rather than
patched around: `Cpu::tick` no longer retires an instruction per call (it takes 5 `PCycle`s to
fill the pipeline), and the scheduler's step count now derives from `cpu_cycles()` rather than
`Cpu::retired`, since retirement lags stepping by the pipeline depth and the two are no longer
interchangeable. The residue invariant's third term likewise moved to an inter-domain
(CPU vs RCP) comparison, keeping it a property of the clock rather than of the CPU.

### Changed — the ADR 0006 scheduler rework (T-11-001, first half)

The canonical master clock is now **implemented**, not just decided.

- `MASTER_HZ` is **187,500,000**. `master_ticks: u64` is the only counter in the core that is
  ever incremented; `cpu_cycles()`, `rcp_cycles()` and `count_ticks()` are derived accessors,
  not fields. The 3:2 fractional accumulator (`rcp_accum`, `RCP_NUM`, `RCP_DEN`) is deleted.
- Every domain is an integer divisor, exported and asserted exact: `CPU_DIVIDER` 2,
  `RCP_DIVIDER` 3, `COUNT_DIVIDER` 4 (half PClock), `SI_DIVIDER` 12, `PIF_DIVIDER` 96.
- **Per-domain seeded phase offsets.** `Phases { cpu, rcp }` are constants derived from the
  SplitMix64 seed — not counters — so the ownership rule holds while the seeded power-on phase
  stays meaningful. A single shared offset would have made every seed produce an identical
  interleaving from tick 6 onward.
- `tick_one_unit()` is replaced by `step_to_next_edge()` and `run_until(target)`, which advance
  **edge to edge**. The master tick is a time base and is never iterated; `run_until` steps every
  edge in `(now, target]` and deliberately does not overshoot by one.
- `Cpu::cycles` is renamed **`Cpu::retired`** — a retired-work tally, which is the one kind of
  counter still allowed to be incremented because nothing schedules against it. `Cpu::tick` is
  documented as advancing **one PClock, not one instruction**.
- `Bus::poll_irq_at_phase(BusPhase)` is replaced by `Bus::poll_irq()`. `BusPhase` survives to
  describe the bus protocol, with its doc comment stating plainly that it carries no interrupt
  semantics and why.

Seven new tests, two of which are the guards that make the rest trustworthy:

- **`residue_invariants_never_move`** — samples the affine offsets between `master_ticks` and
  every derived position across 64 periods and asserts they never move.
- **`seeds_produce_distinct_interleavings`** — fails if the per-domain phases ever collapse.
- Plus `every_divider_is_exact`, `three_cpu_and_two_rcp_steps_per_six_ticks` (all seeds),
  `same_seed_same_timeline`, `edges_are_never_skipped`, `reset_preserves_phase`.

Both guards were **mutation-tested**: introducing an independently-incremented counter fails the
residue test, and collapsing the phases fails the interleaving test. A guard that cannot fail is
not a guard.

### Added — reference corpus and the accuracy ledger

- **[`docs/accuracy-ledger.md`](docs/accuracy-ledger.md)** — referenced from five documents and
  never created until now. Records measured constants with their provenance, open residuals,
  ruled-out approaches, and contradictions between primary sources. Seeded with the four timing
  constants the hardware documentation does not supply (`M`, the exception epilogue, CP0I, RDRAM
  bank state) and the four source contradictions found during the ADR review. The governing rule:
  a constant is measured, never tuned until a ROM passes — a tuned constant makes every later
  timing result unfalsifiable.
- **[`ref-docs/2026-07-20-vr4300-timing-supplement.md`](ref-docs/2026-07-20-vr4300-timing-supplement.md)**
  — `ref-docs/` is an immutable corpus, so corrections to `research-report.md` land as a dated
  supplement rather than an in-place edit. Records the IC/RF/EX/DC/WB stage-name correction, the
  MClock-is-primary clock derivation, the documented cycle-cost tables, the errata with exact
  behaviour, the SysAD block-ordering rules, and an explicit list of what is *not* documented.
- `docs/glossary.md` gains a **Clocks** section, because "master clock" is overloaded three ways
  and the primary sources own one of them (MasterClock = 62.5 MHz).
- `docs/performance.md` states the project goal plainly — sustained fully cycle-accurate
  emulation at full speed — with the budget (~156M component-steps/s, ~32 host cycles each) and
  an honest account of why it is unproven rather than impossible.

### Changed — the timebase and CPU microarchitecture are settled (Phase 1 design)

Two ADRs land ahead of any Phase 1 code, because both decisions are of the kind that cannot be
retrofitted without rewriting the scheduler and every chip's step contract at once.

- **[ADR 0006](docs/adr/0006-one-canonical-master-clock.md) — one canonical 187.5 MHz master
  clock. Supersedes [ADR 0001](docs/adr/0001-master-clock-lockstep-scheduler.md)**, which is
  retained unmodified as the record of the first design. 187.5 MHz is the LCM of the 93.75 MHz
  CPU and 62.5 MHz RCP clocks, which makes *every* emulated domain an integer divisor: CPU
  every 2 ticks, RCP every 3, COP0 `Count` every 4 (half PClock), SI every 12, PIF every 96.
  The 3:2 fractional accumulator is gone; a fractional path survives only for VI and AI, which
  genuinely run off a different crystal. The load-bearing rule is not the unit but the
  ownership: **`master_ticks` is the only counter ever incremented**, every other cycle position
  is a derived accessor, and a residue invariant test fails if any position becomes independent.
- **[ADR 0007](docs/adr/0007-cycle-accurate-vr4300-pipeline.md) — the VR4300 is a cycle-accurate
  five-stage pipeline** (IC/RF/EX/DC/WB) of four inter-stage latches advanced one PClock per
  step in **reverse stage order** (WB → DC → EX → RF → IC), which is what makes the latching
  implicit and removes any need for double-buffered state. `in_delay_slot` travels with the
  instruction in the latch chain rather than as global CPU state, so `Cause.BD`/`EPC` survive a
  multi-cycle stall between a branch and its delay slot. Interlocks are `(cycles, resume_stage)`.

Three factual corrections came out of the hardware research, all in `docs/cpu.md`:

- The pipeline stages are **IC/RF/EX/DC/WB**, not the "IF/RF/EX/DF/WB" previously documented.
  The manual's entire interlock and exception taxonomy is named stage-relative, so this matters.
- "The CPU advances one issued instruction per master tick" was not a timing model. `DDIV`
  stalls the whole pipeline for 69 PCycles, and at minimum 5 PCycles are needed per instruction.
  The documented cycle-cost tables are now transcribed into `docs/cpu.md` §Cycle costs.
- **`Bus::poll_irq_at_phase(BusPhase)` is removed, not completed.** It was shaped on the
  assumption that interrupt sampling is tied to the SysAD command/data phase; no such coupling
  is documented anywhere. The real rule is per-PCycle, gated on the previous PCycle having been
  a run cycle. Wiring the parameter up to *something* would have encoded a fiction that looked
  correct. `BusPhase` is retained for the bus protocol, with no interrupt semantics attached.

The full 655-page NEC VR4300 User's Manual turned out to be in the local wiki mirror already
(`n64brew_wiki/images/VR4300-Users-Manual.pdf`) with an intact text layer — it is now cited as
the primary timing oracle. Extract with `mutool draw -F txt`; `pdftotext` fails on it.

`docs/scheduler.md`, `docs/cpu.md`, `docs/architecture.md`, `docs/engineering-lessons.md`,
`README.md`, `AGENTS.md`, and the Phase 1 plan are updated to match. Sprint 1 gains T-11-008
(the SysAD transaction model and the measurement of `M`, the undocumented memory access time)
and its estimate rises from 3 to 5 weeks.

### Changed — dependency and toolchain refresh

Everything moved to the newest mutually-compatible versions available.

- Rust toolchain 1.96 → **1.97.1** (`rust-toolchain.toml`, workspace `rust-version`, and the
  `dtolnay/rust-toolchain` pin in all three workflows).
- egui / egui-wgpu / egui-winit 0.34 → **0.35** (MSRV 1.92, satisfied). `Panel::show_inside` is
  deprecated in favour of `Panel::show`; both call sites in `ui_shell` updated.
- `directories` 5 → **6**, `pollster` 0.4 → **1.0**, plus patch/minor moves across `winit`,
  `cpal`, `gilrs`, `rfd`, `bytemuck`, `thiserror`, `bitflags`, and `insta` via `cargo update`.
- GitHub Actions: `checkout` v4 → **v7**, `upload-artifact` v4 → **v7**, `download-artifact`
  v4 → **v8**, `configure-pages` v5 → **v6**, `upload-pages-artifact` v3 → **v5**,
  `deploy-pages` v4 → **v5**. `Swatinem/rust-cache` stays on the floating `v2` (currently 2.9.1).
- `markdownlint-cli` v0.39.0 → **v0.49.1**, which adds MD060 (table-column-style). Delimiter
  rows are normalised to padded form across 15 documents, and MD060 is pinned to
  `style: "padded"` rather than the default per-table inference — inference classified two
  single-row tables with 300-character cells as "aligned" and demanded header padding that
  cannot be met.

**`wgpu` is deliberately held at 29.** wgpu 30 is released, but no published `egui-wgpu`
accepts it — 0.35.0 requires `wgpu = "^29.0"`, and egui's unreleased `main` still pins 29.0.
Bumping wgpu alone would not fail resolution; cargo would link both wgpu 29 (for egui-wgpu) and
wgpu 30 (for the frontend), making the two `wgpu::Device` types distinct and breaking the build
with "expected `wgpu::Device`, found `wgpu::Device`". The rationale is recorded at the
dependency itself, with `cargo tree -p wgpu -d` given as the check.

### Added

- [`docs/engineering-lessons.md`](docs/engineering-lessons.md) — failure patterns carried over
  from two prior cycle-accurate emulators, generalised to this machine rather than copied.
  Ordered by when each lesson must be acted on: structural decisions that are cheap now and
  expensive-to-impossible later, then what "green" is permitted to mean, then debugging
  discipline. Phase-specific entries are also filed as risks in the relevant phase overview.
- Explicit `timeout-minutes` on every job in all three workflows (CI 30, Pages 20, release 45).
  Jobs previously inherited GitHub's 6-hour default, where a hang holds a concurrency slot and
  presents as "CI is slow" rather than as a hang.
- Phase 1 exit criterion requiring an instruction's memory access to be a point the scheduler can
  interleave around, with `Bus::poll_irq_at_phase` reaching a genuinely per-phase branch pinned by
  a test. Retrofitting that structure later means rewriting the scheduler and every chip's step
  contract at once.
- `docs/testing-strategy.md`: re-blessing a visual golden now requires justification against an
  external reference (ParaLLEl-RDP, Angrylion, hardware docs), never against our own previous
  output, with the reference named in the commit message.

### Changed

- ADR 0005 now requires a residual to be classified as an absolute or differential measurement
  before it can be attributed to sub-cycle bus timing. Differential measurements are invariant
  under uniform re-phasing and cannot be evidence for the refactor.
- Phase 5 records the per-game database as a bounded-authority data file: it may describe
  cartridge identity only, the core never consults it, and a frontend-only reproduction is
  treated as a load-path problem until proven otherwise.

### Fixed

- `Bus::poll_irq_at_phase` documented as not yet phase-sensitive. It ignores its `BusPhase`
  argument in both the trait default and the `rustyn64-core` implementation, so the signature
  implied per-phase interrupt sampling that does not exist yet.

## [0.1.0] "Foundation" — 2026-07-20

The architectural skeleton. The workspace compiles, CI is green across three platforms, the
reference corpus is acquired and licence-classified — and **no chip executes an instruction yet**.
This tag exists so the foundation is a fixed, citable point rather than an ever-growing
`[Unreleased]` section.

### Added

- Initial workspace scaffold (cycle-accurate emulator architecture, ported from RustyNES).
- Always-on egui shell (menu bar + status bar + stub debugger window) wired to the
  `winit + wgpu + cpal + egui` frontend, presenting a cleared/test-pattern frame at the
  N64 VI dimensions with the N64 controller input map (digital + analog stick).
- Release automation: a `v*` tag now publishes a GitHub Release with per-target archives
  (Linux x86_64, macOS aarch64, Windows x86_64) containing the `rustyn64` binary, both
  licences, `NOTICE`, `README`, and `CHANGELOG`, plus a `SHA256SUMS` manifest. The tag is
  checked against the workspace version before anything is published.
- Documentation site: pushes to `main` publish the rustdoc API reference to GitHub Pages
  under `/api/`, with `/` reserved for the wasm demo that lands in Phase 6.
- `scripts/mirror_n64brew_wiki.py` builds a gitignored offline mirror of the N64brew Wiki
  at `n64brew_wiki/` (see its `README.md`). 324 pages and 96 media files, with parallel
  HTML / Markdown / wikitext trees and `--refresh` for revision-based incremental updates.
- Test-ROM corpora under `tests/roms/`, split into a committed tier (permissively licensed,
  each ROM shipping its upstream `LICENSE`) and a gitignored external tier:
  - **committed** — `n64-systemtest` (MIT), the self-judging CPU/COP0/TLB/RSP gate, built
    from source since upstream publishes no prebuilt ROM;
  - **external** — PeterLemon/`krom` (196 ROMs), Dillon's n64-tests (26), the 240p Test
    Suite (built from source in a container), and a commercial regression corpus organised
    by save type.
- `scripts/check_no_roms.sh` plus a `no-commercial-roms` CI job: commercial ROMs are blocked
  by three independent guards (`.gitignore`, a pre-commit hook over the staged file list,
  and a server-side CI scan). The hook also enforces that any allowlisted ROM ships a
  `LICENSE` beside it.
- Full phase and sprint planning in `to-dos/`: the ROADMAP gains a status section, a phase spine
  with per-phase goal/exit criteria, cross-phase dependencies, and the open questions that gate
  deeper planning. All nine phase overviews adopt the seven-section skeleton (Goal, Exit criteria,
  Scope, Sprints, Dependencies, Risks, Reference docs), and ten sprint files mint **49 tickets**
  (`T-PS-NNN`, P = phase, S = sprint) with acceptance criteria, dependencies, spec references, and
  complexity. Phase 0's tickets are checked off against what actually shipped; the rest are
  forward plans grounded in the register-level detail from the wiki mirror.
- `docs/DOCUMENTATION_INDEX.md`, the docs map both sibling projects carry — subsystem specs,
  cross-cutting references, subdirectories, and the material outside `docs/`.
- Reference emulators and test suites cloned for study under the gitignored `ref-proj/`
  (ares, cen64, gopher64, simple64, parallel-rdp, parallel-rsp, angrylion-rdp-plus,
  n64-systemtest, n64-tests, libdragon, PeterLemon/N64). Per-repo licences are verified and
  recorded in `ref-proj/README.md`, which classifies each as vendor-ok or study-only.

### Changed

- Modernized the frontend against egui 0.34 / wgpu 29 / cpal 0.18 (`Panel::*::show_inside`,
  `MenuBar::new().ui`, `ui.close`, `Context::run_ui`, `CurrentSurfaceTexture`, the
  `experimental_features` / `multiview_mask` / `immediate_size` descriptor fields).
- Aligned the `to-dos/` phase overviews + ROADMAP to the N64 phase set (RSP LLE, RDP LLE+VI,
  AI audio, cart boot + saves, frontend integration, accuracy breadth).
- Filled out `CONTRIBUTING.md` (Rust-only) and fixed the markdownlint pre-commit hook to
  pass `--config .markdownlint.json`.
- `docs/STATUS.md` gained a project-infrastructure table and a test-ROM corpus table, and
  its accuracy table gained an "oracle available?" column. Several gates now have their ROM
  staged locally while remaining not-started, and collapsing those two states into one
  would misrepresent progress.
- `docs/testing-strategy.md` documents the corpus tiers, the commercial-ROM guards, and the
  per-corpus licensing that decides which tier a corpus lands in.
- `README.md` rebuilt to the structure RustyNES and RustySNES share — centred title block with a
  three-row badge set, Overview, Why RustyN64, Highlights, Features, Quick Start, Default
  Controls, Architecture with crate and layout tables, Compatibility and Accuracy, Performance,
  Platform Support, Documentation, Current Release, Roadmap, Contributing, License,
  Acknowledgments, Citation, and the shared footer. Adapted honestly for a pre-alpha: the
  accuracy badges read "not started" rather than borrowing the siblings' 0-diff claims, the
  Highlights table separates working from stubbed, and the Performance section states that no
  measurements exist rather than inventing any.
- `.gitignore` covers output our own workflows produce when run locally (`_site/` from pages.yml,
  `dist/` and the release archives from release.yml) plus `site/` for a future docs handbook.
- `docs/adr/0004-determinism-contract.md` gained the `Consequences` section the Nygard format and
  the project's own docs rules require — it was the only ADR without one. Records the costs the
  contract imposes and the fact that it is currently specified but unexercised.
- Root `Cargo.toml` excludes `ref-proj/` and `n64brew_wiki/` from the workspace. Cargo's
  upward workspace discovery otherwise makes any nested project there — `n64-systemtest`,
  `gopher64` — resolve *this* workspace as its root and fail to parse our members.

### Fixed

- CI, release, and Pages jobs now install the Linux system headers the frontend needs
  (`libasound2-dev`, `libudev-dev`, `libxkbcommon-dev`, `libwayland-dev`). `cpal` pulls
  `alsa-sys` and `gilrs` pulls `libudev-sys`, whose build scripts call `pkg-config`; those
  headers are absent from the GitHub runner images, so every Linux job that compiled the
  workspace would have failed in a build script.
- `release.yml` pins the toolchain to 1.96 instead of `@stable`. `rust-toolchain.toml`
  takes precedence over the action's default, so the previous config installed one
  toolchain and built with another.
- The nine phase overviews cited "the skill's references/roadmap_template.md" for their exit
  criteria — a dangling reference to the generator skill, which is not part of this repository, so
  no phase had a stated exit bar. Every phase now carries real, checkable criteria.
- `AGENTS.md` claimed the repository used three incompatible ticket-ID schemes. It does not:
  `T-PS-NNN` is a template where P is the phase digit and S the sprint digit, and the overviews
  instantiate it correctly as `T-01-NNN` through `T-81-NNN`. Only the pre-ticket code TODOs use a
  separate subsystem-scoped form.
- `README.md` cited the "Mesen2 / ares / higan" accuracy bar, which belongs to the NES/SNES
  projects. The N64 reference set is ares / CEN64 / Gopher64 / ParaLLEl, per
  `docs/architecture.md`. It also listed only 8 of the 10 workspace crates, and its
  quick-start implied the binary plays games — it currently opens a shell and presents a
  test pattern.
- `crates/rustyn64-frontend/web/Trunk.toml` pinned the wasm-bindgen CLI to 0.2.100 while
  `Cargo.lock` resolved the library to 0.2.126. Trunk requires these to be equal, so the
  wasm build would have failed at bindgen time. A new `wasm-bindgen-pin` CI job now
  compares the two and fails on drift, which is otherwise invisible until someone runs
  `trunk build`.

[Unreleased]: https://github.com/doublegate/RustyN64/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/doublegate/RustyN64/releases/tag/v0.1.0
