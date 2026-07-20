# Accuracy ledger — RustyN64

**References:** ADR 0005 (what defers here), ADR 0006, ADR 0007;
`ref-docs/2026-07-20-vr4300-timing-supplement.md` (the undocumented-constants list);
`docs/testing-strategy.md`; `docs/engineering-lessons.md` §3.3.

## What this file is for

Three things, and nothing else:

1. **Measured constants** — numbers the hardware documentation does not supply, which we fitted
   from test ROMs. Each records *how* it was measured, so it is falsifiable.
2. **Open residuals** — known-wrong behaviour we have chosen to document rather than point-fix.
3. **Ruled-out approaches** — attempts that failed, with the reason, so nobody rediscovers them.

The rule that gives this file its value: **an entry here is honest, a per-quirk patch is not.**
When a ROM fails and the fix would be a special case, the entry goes here instead (ADR 0005).

Equally: **a measured constant is never adjusted to make a specific ROM pass.** The moment a
number is tuned rather than measured, every later timing result built on it becomes
unfalsifiable — the whole suite silently stops being evidence. If a constant looks wrong, measure
it again and say so; do not nudge it.

## Status

**The CPU executes instructions; nothing has been *measured* yet.** Sprint 1 landed the integer
core, so residuals can now be observed in principle — none has been. What has changed is that
three entries (C-2, C-3, S-3) turned out to be **documented all along** and are resolved by
citation rather than measurement; C-7 is new and likewise documented. The remaining constants are
still placeholders with known provenance, recorded so the first implementation does not silently
invent a value and move on.

---

## 1. Measured constants

| # | Constant | Value | How measured | Status |
| --- | --- | --- | --- | --- |
| C-1 | `M` — memory access time (PCycles) | — | — | **not yet measured** |
| C-2 | Exception epilogue cost (PCycles) | **2** | ~~measurement~~ **documented** — UM §4.7 p. 114 | **resolved; not a measured constant** |
| C-3 | CP0I (CP0 bypass interlock) cost | **1** | **documented** — UM §4.6.9 p. 113 | **resolved; not a measured constant** |
| C-7 | ITM (instruction micro-TLB miss) penalty | **3** | **documented** — UM §4.6.2 p. 107 | **resolved; not a measured constant** |
| C-4 | RDRAM row-hit / row-miss / dirty-miss | — | — | **not yet measured** |
| C-5 | `DIV` quotient when divisor bits 63 and 31 differ | *32x35 division* | **guessed** | needs hardware |
| C-6 | Divide-by-zero `HI`/`LO` values | conventional | **guessed** | needs hardware |

### C-1 — `M`, memory access time in PCycles

The single most load-bearing unknown. Both documented cache-miss formulas are parameterised on
it: D-cache fill = **8–9 + M**, I-cache fill = **14–15 + M** (UM Tables 11-1/11-2). No source
gives a value.

Informal hints, all explicitly hedged by their authors and none usable as a number: RDRAM "about
10-20+ clock wait time"; RCP registers "5-6 PClock cycles"; MI registers "about 2"; RSP
DMEM/IMEM "4-5".

For scale, the reference emulators collapse the whole access into one constant and **disagree**:
CEN64 charges 38 PClocks for an uncached word, 44 for a D-cache fill, 48 for an I-cache fill
(under the source comment `// Currently using fixed values....`); ares charges 40 for a D-cache
fill. Neither derived theirs from a spec. Note CEN64's 44 = 8 + 38, which is consistent with the
Table 11-1 sum plus its own word delay — weak corroboration that the formula reading is right.

`M` is almost certainly **not a single number** — it should vary with target region (RDRAM vs
RCP register vs SP memory vs cart) and with RDRAM bank state (C-4). Expect a small table, not a
scalar. Recording it as a scalar first is acceptable; recording it as a scalar *permanently* is
how a fitted constant becomes a fudge factor.

**Owner:** T-11-008.

### C-2 — exception epilogue cost — **RESOLVED, and this entry was wrong**

**2 PCycles, and the manual says so.** UM §4.7 (p. 114), the opening sentence of the section:

> *"When a pipeline exception condition occurs, the pipeline stalls for 2 PCycles and the
> instruction causing the exception as well as all those that follow it in the pipeline are
> aborted."*

This entry previously read *"**Not documented**: no figure appears in UM §4.7 or chapter 6"* —
naming the exact section the figure is in. The mistake was searching §4.7's *tables* and
Chapter 6's exception-processing prose, and never reading §4.7's own first paragraph.

So CEN64's 2 is **independent corroboration**, not the origin, and its source comment asking
*"do we actually delay an additional two cycles?"* is answered: yes.

**This is not a measured constant and does not belong in this section's spirit** — it is kept
here only so the correction is visible where the wrong claim was. The same error propagated to
`docs/cpu.md` and `ref-docs/2026-07-20-vr4300-timing-supplement.md`; both are corrected (the
latter by a new dated supplement, since `ref-docs/` is immutable).

**The lesson, which is the part worth keeping:** *"undocumented"* is a claim **about** the
manual, and it decays. Once written down it gets copied between files and stops being
re-checked — three files asserted it here. Before recording anything as undocumented, cite the
specific pages checked; before *relying* on such a record, re-check it.

### C-3 — CP0I — **RESOLVED, same cause as C-2**

**1 PCycle.** UM §4.6.9 (p. 113): *"This interlock causes a pipeline stall for one PCycle to
allow the CP0 register to be written in the WB stage before allowing any CP0 register to be read
in the DC stage."* The trigger is equally specific: an instruction that caused an exception
reaches WB while the subsequent instruction in DC requests a read of any CP0 register.

This entry previously said *"no cycle count located in the manual text"* while citing §4.6.9,
which is the paragraph containing it.

Separately, and still true: n64-systemtest's `cop0hazard` set is default-off upstream because
the *hazard* rules are not fully derived by anyone. That is a different question from this
interlock's cost — CP0 hazards are explicitly **not interlocked** (UM Ch. 19), so they are a
software-visible ordering constraint rather than a stall. Sprint 2 decides whether to model them.

### C-7 — ITM, the instruction micro-TLB miss penalty — **documented**

**3 PCycles.** UM §4.6.2 (p. 107): *"A miss penalty of 3 PCycles is incurred when the micro-TLB
is updated from the JTLB."*

Worth stating the structure, because it is easy to conflate: the VR4300 has a **two-entry
instruction micro-TLB (ITLB)** in front of the 32-entry joint TLB. A micro-TLB miss is a
**stall**; a JTLB miss is an **exception**. Modelling only the JTLB loses this cost entirely.
Whether Sprint 2 models the micro-ITLB separately is an open decision recorded in that sprint's
plan.

### C-4 — RDRAM bank state

`RDRAM Interface.md` documents row-open/Ack, row-miss/NAck-close-and-reload, and "takes even
longer if the current row is dirty" — qualitatively, with no cycle counts. The programmable
timing registers (`RasInterval`: `RowPrecharge`/`RowSense`/`RowImpRestore`/`RowExpRestore`;
`Delay`: `AckWinDelay`/`ReadDelay`/`AckDelay`/`WriteDelay`) are documented bitwise but the values
IPL3 programs are not translated into cycles. Interacts with C-1.

### C-5 — `DIV` with mismatched divisor sign bits

The `MULT`/`DIV` sign-extension erratum is documented, but with one hole. When
bits 63 and 31 of the divisor **differ**, the quotient written to `LO` is
described as incorrect and *"it is currently unclear how the outputs of this last
case are arrived at"* — unknown to N64brew, not merely undocumented by NEC.

`alu::div` currently performs the 32x35 division in that case as well. **That is a
guess**, recorded here so it is not mistaken for the documented behaviour. `HI` is
better founded: `remainder = (int32_t)(dividend - quotient * divisor)` computed in
64-bit, which the wiki does state.

**Owner:** T-11-005 (the errata ticket), characterised against hardware or
n64-systemtest.

### C-6 — divide-by-zero `HI`/`LO`

Architecturally *undefined* on MIPS. `alu::div`/`divu`/`ddiv`/`ddivu` use the
conventional emulator interpretation (`LO` = ±1 or all-ones, `HI` = dividend).
Unverified against hardware. What *is* non-negotiable and tested is that it does
not panic — a guest program can divide by zero at will.

---

## 1b. Genuinely undocumented — needs a hardware pin, not a guess

Distinct from section 1: these are not constants to fit, they are *behaviours* the manual
declines to define. Each must be pinned against n64-systemtest or hardware before any
implementation choice here is treated as correct.

| # | Question | What the manual says | Owner |
| --- | --- | --- | --- |
| U-1 | `MFC0`/`MTC0` on COP0 registers 7, 21–25, 31 | *"Reserved for future use"* (UM Table 1-2 p. 46) and nothing further — no read value, no write effect | Sprint 2 |
| U-2 | `TLBP` low `Index` bits on a miss (we leave them **zero**) | Only that `Index.P` (bit 31) is set (UM §5.4.11 p. 158); the remaining bits are unstated | Sprint 2 |
| U-3 | The N64's full `PRId` value | `Imp = 0x0B` for the VR4300 series; the `Rev` field is unstated and the manual warns against depending on it (UM §5.4.5 p. 151) | Sprint 2 |
| U-4 | ~~Which `Int[4:0]` line the MI drives~~ | **RESOLVED** — `IP2`. Not in the CPU manual (board-level) nor in the N64brew mirror, but stated by libdragon: `#define C0_INTERRUPT_RCP C0_INTERRUPT_2` (`ref-proj/libdragon/include/cop0.h`), which also gives `IP3` = CART, `IP4` = PRENMI, `IP7` = timer. libdragon is public domain, so this is citable rather than merely observed | **closed** |
| U-5 | 32-bit address calculation that overflows the sign-extended range | *"The address calculated at this time is invalid, and the result is undefined"* (UM §5.2.3 p. 130, §5.2.4 p. 134) — an explicit refusal to define | Sprint 2 |
| U-6 | `Config.EC` on the N64 | `0b111` (1:1.5) is allowed *"with the 100 MHz model only"* (UM Appendix A note 1, p. 628), and the N64's ratio is 1:1.5 — so `0b111` is a strong **inference**, but the manual never names the N64 | Sprint 2 |
| U-7 | The **corrupted output** of the FP multiplication erratum | The *trigger* is documented (`VR4300.md`: a multiply whose preceding multiply had a NaN, zero or infinity operand) and so are the affected steppings (NUS-01/02/03), but **what wrong value is produced has never been characterised** — recorded in `ref-docs/2026-07-20-vr4300-timing-supplement.md` as an undocumented constant. `Stepping::Early` can therefore be *selected* but changes no arithmetic; inventing a plausible wrong value would be the fitted-constant failure this file's preamble forbids | Sprint 3 modelled the switch and the trigger. Needs an affected console, or a hardware capture, before the output can be reproduced |
| U-8 | FPU rounding modes and the `inexact` / `underflow` flags are **partial** | `FCSR.RM` is honoured by the conversions but **not** by `add`/`sub`/`mul`/`div`, which use Rust's operators and are nearest-even only. Likewise `inexact` is set for overflow and conversions but not for ordinary rounding, and `underflow` only for conversions that flush to zero. Both need the *exact* result before rounding, which the hardware float operators do not expose | Needs soft-float arithmetic or per-operation re-rounding. Recorded so a caller does not trust a bit that never sets — the module's own doc table says which flags are complete |

U-6 is the one to watch: it is consistent with ADR 0006's clock derivation, which makes it
tempting to promote to a fact. It is an inference from a part-number restriction, and it stays
labelled as one until something reads the register on hardware.

## 2. Open residuals

| # | Symptom | Suspected mechanism | Classification | Status |
| --- | --- | --- | --- | --- |
| — | none yet | — | — | — |

Every entry must carry a **classification** of the failing measurement as **absolute** or
**differential** before any mechanism is proposed (ADR 0005, `engineering-lessons.md` §1.3). A
differential measurement — the interval between two events on the same clock — is invariant
under uniform re-phasing, so an entire family of plausible fixes is ruled out for free. A sibling
project implemented and rolled back five successive re-phasings before recognising this. One line
here saves that.

---

## 3. Ruled-out approaches

| # | Approach | Applied to | Why it cannot work | Date |
| --- | --- | --- | --- | --- |
| — | none yet | — | — | — |

Record an approach here after **two** rollbacks, not after five (`engineering-lessons.md` §3.3).
An unrecorded dead end gets rediscovered by the next person, or by the same person in six months.

---

## 4. Contradictions in the sources

Not our bugs, but they will look like ours if undocumented.

| # | Contradiction | Sources | Resolution |
| --- | --- | --- | --- |
| S-1 | `SYSCMD` bit 4 polarity: command = 0 or 1? | UM §12.11.1 vs `SysAD Interface.md` cheat sheet | **RESOLVED — not a contradiction at all**; see below |
| S-2 | Pipeline stage names | `ref-docs/research-report.md` §1 says IF/RF/EX/DF/WB; UM §4.1 Fig 4-1 says IC/RF/EX/DC/WB | **resolved** — manual wins; see `ref-docs/2026-07-20-vr4300-timing-supplement.md` §1 |
| S-3 | Exception vector for an exception with `EXL` already set | UM Fig. 6-15 (p. 203) says `0x080`; UM Table 6-4 + §6.4.8 say `0x180`; CEN64 routes to `0x180` | **RESOLVED — `0x180`**; the manual contradicts *itself*, and Fig. 6-15 is the defective source. See below |
| S-4 | D-cache fill cost | CEN64 charges 44 PClocks; ares charges 40 | **unresolved** — neither is spec-derived; supersede both with C-1 |

### S-3 — resolved: the contradiction is *inside* the manual, and `0x180` wins

Recorded as MIPS-docs-vs-CEN64. It is neither: the VR4300 manual disagrees with itself, and the
majority of it says `0x180`.

**For `0x180` — three places, two of them normative tables:**

- **Tables 6-3/6-4 (p. 181)** define the refill offsets *only* for `EXL=0`: the rows are labelled
  `TLB Miss, EXL=0` → `0x000` and `XTLB Miss, EXL=0` → `0x080`. Everything else is `Other` →
  `0x180`. There is no `EXL=1` refill row to select.
- **§6.4.8 (p. 187)**, *Processing*: *"All TLB Miss exceptions use these two special vectors when
  the EXL bit is set to 0 in the Status register, and they use the common exception vector when
  the EXL bit is set to 1 in the Status register."*
- **§6.4.8 (p. 188)**, *Servicing*, describing a nested refill: *"This second exception goes to
  the common exception vector because the EXL bit of the Status register is set."*

**For `0x080` — one flowchart:** Fig. 6-15 (p. 203) has a branch `EXL = 0?` whose **No** arm
leads to a box reading *"General Purpose Exception, Vec. Off. = 0x080"*. That figure is wrong. It
contradicts both tables, the §6.4.8 prose twice, and Fig. 6-14 (p. 201), which is the
general-purpose handler and unconditionally uses `+ 0x180`.

**So CEN64 is right**, and its source comment that `0x080` *"doesn't make any sense"* is a
reaction to exactly this figure. Resolution is by document, not by measurement, so no test ROM is
required — but a pin is still worth having as a regression gate, and n64-systemtest exercises it
directly (it installs handlers at all three of `0x000`, `0x080` and `0x180`).

Kept rather than deleted: Fig. 6-15 is still in the manual, so the next reader will find `0x080`
and have to re-derive this. **Owner:** Sprint 2, with the pin.

### S-1 — resolved: the sources agree on the bits and disagree on the English

Recorded as a contradiction, and it is not one. Reading both carefully:

- **UM §12.11.1**: *"During address cycles \[`SysCmd4` = 0\] … contains a System
  interface command"*; *"During data cycles \[`SysCmd4` = 1\]"*.
- **The wiki cheat sheet**: read/write **requests** carry bit 4 = **0** (its
  column says "Data req"); data-carrying cycles carry bit 4 = **1** (its column
  says "Command").

So both sources put a request at bit 4 clear and a data beat at bit 4 set. They
differ only in that the wiki calls the *data-identifier* cycle "Command". No test
ROM is needed. We follow the manual's naming, since it is the vendor spec and the
rest of the CPU crate cites it.

Worth keeping as an entry rather than deleting: the next reader will hit the same
apparent conflict, and "resolved, and here is why it looked wrong" is more useful
than silence.

---

### C-8 — COP0 CO `funct` 0x20-0x3F retires as a no-op

**Claim.** A COP0 CO-class instruction whose `funct` is in `0x20..=0x3F` retires
with no architectural effect, rather than raising Reserved Instruction.

**Basis: inference, not a manual citation.** The VR4300 manual does not enumerate
this range. The inference is from n64-systemtest's own structure: it probes for
the `emux` emulator by executing `COP0 CO funct 0x20` from `init_allocator`,
inside `entrypoint` — **before** `main` installs any exception handler. An RI
there would derail the suite on every N64 it has ever run on, before it printed a
line. The suite's constant for the probe is named
`XDETECT_CODE_EXTENSIONS_20_3F`, i.e. emux claims exactly this range as extension
space, which only works if hardware leaves it inert.

**Untested.** Whether the target GPR is written (and with what) is unknown; we
leave it untouched, so a probe reads back its prior value and concludes emux is
absent. That is the correct outcome here but is not evidence about hardware.

**Confirm with:** a hardware run of an `XDETECT` word with a known GPR value.

### C-9 — PI direct-I/O write latch duration is fitted, not measured

**Claim.** A PI direct-I/O write latches its value and shadows every PI-bus read
for `Bus::PI_WRITE_CYCLES` (100) RCP cycles.

**Documented part.** The *behaviour* is from N64brew *Memory map* (PI external
bus): writes are asynchronous, the PI latches the value and releases the CPU
immediately, `PI_STATUS.IOBUSY` reports the in-flight write, further writes are
ignored, and reads from **any** address return the value being written. The PI
does not know a device is read-only, so ROM writes follow the same path and are
dropped by the ROM.

**Undocumented part — the duration.** Hardware finalisation depends on the PI
domain timing registers (`LAT`/`PWD`/`PGS`/`RLS`), which we do not model.
n64-systemtest bounds the latch only relatively: visible after 0 decay-loop
iterations, gone after 110. The constant was chosen by trying values against the
suite and keeping the best.

**Known-wrong, deliberately.** `cart-writing: Write32, Read32 (same location)`
still fails on its **second** read, where hardware has finalised and we have not.
No single constant closes that, because the real duration is not constant. This
is recorded as a fitted approximation rather than presented as accurate.

**Confirm with:** modelling the PI domain timing registers and deriving the
finalisation time, then deleting the constant.

### C-10 — FP arithmetic is correct only in round-to-nearest-even

**Claim.** `fpu::{add,sub,mul,div}_{s,d}` compute with Rust's native `+`/`-`/
`*`/`/`, which round to nearest-even unconditionally.

**Why that is wrong.** The VR4300's `FCSR.RM` selects one of four rounding modes
(nearest, toward zero, toward +inf, toward -inf), and `FCSR.FS` flushes denormal
results to zero. Neither is consulted. Every operation whose exact result is not
representable therefore has a wrong last bit under any mode except `RM = 0`.

**Evidence.** n64-systemtest sweeps rounding modes and reports 63 `Result after
MUL.S`, 54 `Result after DIV.S`, 39 `Result after ADD.S` failures (and the `.D`
equivalents) — on operations that *are* wired and do execute.

**Not fixable by wiring.** The arithmetic core itself is mode-blind. `no_std`
Rust has no `fesetround`, so directed rounding has to be produced explicitly:
compute exactly in wider precision and round per `RM`, or use a soft-float
implementation. Both need their own golden vectors.

**Note the asymmetry.** `to_i32`/`to_i64`/`round_f64` already take a `Rounding`
argument, so the conversions were written mode-aware and the arithmetic was not.
The gap has existed since Sprint 3 and was invisible because nothing decoded to
the arithmetic until COP1 was wired.

**A fix was attempted and reverted.** Routing `ADD.S`/`SUB.S`/`MUL.S` through an
exact `f64` computation rounded per `RM` changed **nothing** the oracle measures
(2,897 before and after) and made `ADD.S` marginally worse, 39 failures to 40.
Two lessons, both recorded rather than discarded:

1. The exactness argument (53 significand bits ≥ 2×24+2) holds only in the
   **normal** range. An `f64` value that is subnormal as an `f32` has already
   lost bits to the narrower exponent range, so converting it double-rounds. A
   correct implementation must never leave the target format — i.e. soft-float.
2. **The rounding mode is not what these tests are failing on.** The hypothesis
   was plausible and measurably wrong, so the cause of the ~250 `Result after
   <op>` failures is still unidentified. Do not assume `RM` next time.

The helper (`fpu::round_f64_to_f32`, `next_up_f32`, `next_down_f32`) is retained
with its tests: it is correct in the normal range and will be needed.

**Measured, and it is not an arithmetic problem at all.** The verbatim failure:

```text
'COP1: ADD.S' with '(false, Nearest, 0.0, 2e0, Ok((, 2e0)))' failed:
  a=1.2795344e-28 b=2e0 (0x11223344 vs 0x40000000)
```

`0x11223344` is the test's **sentinel**, unchanged — the destination is never
written. And the mode is `Nearest`, so `RM` was never implicated. Both earlier
hypotheses (unwired operations, exception behaviour) and the rounding hypothesis
are now all excluded by measurement.

A neighbouring case is more informative still: `Upper bits of 32 bit operation
(half mode)` reports `0x1111_40C0_0000` against an expected `0x40C0_0000`. There
the low word **is** correct (`0x40C00000` = 6.0) and the *upper* half of the FGR
retains its sentinel. So the arithmetic works and the write-back width or path is
wrong — a 32-bit FP result apparently must not leave the upper half intact.

**Next:** determine why `fd` is unwritten in the main path while the "upper bits"
case does write. Candidates:

- ~~the result never leaves the FPR because `SWC1` does not store, or the
  operands are never loaded by `LWC1`~~ — **eliminated**: `LWC1`/`LDC1`/`SWC1`/
  `SDC1` are all decoded *and* executed (`Pipeline`, the FP load/store arm), so
  the transfer path exists.
- the `Cop1Access::Arith` request is dropped between EX and WB, so `fp_arith`
  never runs for these cases;
- or it runs and writes, but the test reads the register through a path whose
  view disagrees — note the failing tuple begins `(false, …)`, and the
  neighbouring failure is explicitly labelled **"half mode"**, which is what
  `Status.FR = 0` is called. Under `FR = 0` a 32-bit result and a 64-bit read
  disagree about which FGR half holds it, and `0x1111_40C0_0000` — correct low
  word, sentinel upper half — is exactly that shape.

The second and third are distinguishable in one run: dump the FPR immediately
after an `ADD.S` retires and compare against what the test reads back. **Do not
assume the third is right because it is the tidiest** — that reasoning has now
failed nine times in this ticket.

**Run done. FPR writes do occur**, so the `Arith` request is not being dropped
wholesale — candidate two is weakened. (Watching all 32 raw FGRs, values change
during the COP1 phase; pairs appearing to change together are an artefact of
`step_to_next_edge` advancing several cycles per observation, not aliasing.)

That leaves the `FR = 0` view as the live candidate, but it is **not confirmed**:
"writes happen somewhere" is much weaker than "the write for *this* `ADD.S` lands
where the test reads". The next probe must be *targeted*, not global — break on
the specific `ADD.S`, then read back `fd` through both `read_s` and `read_d` and
compare with the `0x11223344` the test sees. A global FGR watch cannot answer it,
which is worth stating because this run looked informative and was not.

## 5. Deliberate deviations from hardware

Behaviour we model differently *on purpose*, so it is never mistaken for a bug.

| # | Deviation | Why | Bounded by |
| --- | --- | --- | --- |
| D-1 | Power-on CPU/RCP phase comes from a seeded PRNG, not from real indeterminacy | The determinism contract requires reproducibility; the hardware's own indeterminacy is documented (UM Table 11-1's "1 to 2 PCycles: synchronize with SClock") and is modelled as a *parameter* rather than eliminated | ADR 0004, ADR 0006 |
| D-5 | `CACHE` is an **address-translating no-op**: it decodes, translates (so it can raise a TLB fault) and does nothing else | The cache *contents* are not modelled, so invalidate and write-back have nothing to act on. This is observationally sound **only** because no cache state exists to become stale — the depth decision the Phase 1 open question asked for. What matters now is that `CACHE` does not *raise*: IPL3 and libdragon both issue it, so a `Reserved` decode blocks every real ROM | **Stops being sound at Phase 5**, when cart/RSP DMA writes land in RDRAM behind a cache that games explicitly flush. Revisit with the cache model and `M` (C-1) |
| D-4 | TLB entries reset to **distinct** `VPN2` tags, not to zero | All-zero is not a usable state: with 32 entries at `VPN2 = 0` and `V` not participating in matching, the first access to virtual page-pair 0 matches all 32 and triggers **TLB shutdown**. Reset contents are undefined (UM §6.4.4) and ADR 0004 forbids entropy, so a fixed non-coinciding set is chosen — which is what real hardware's arbitrary power-on contents almost always are | Pinned by `a_fresh_tlb_does_not_shut_down_on_the_first_low_access`; revisit if n64-systemtest probes uninitialised entries |
| D-3 | `Count` and `Compare` both reset to a deterministic **0**, so the timer matches at power-on and latches `IP7` | Both reset values are **undefined** (UM §6.4.4, p. 183) and ADR 0004 forbids entropy, so *some* fixed pair must be chosen; 0/0 is the least surprising. The consequence is a timer interrupt pending before software writes `Compare` — masked in practice, since cold reset also leaves `IE` clear and `ERL` set | ADR 0004; IPL3 writes `Compare` during boot, so no real ROM observes it. Revisit if n64-systemtest's startup set disagrees |
| D-2 | The VR4300 errata are **reproduced**, not fixed | They are observable behaviour that software depends on; `sra`/`srav` in particular affects every console | ADR 0007; pinned by named tests that fail if "corrected" |
