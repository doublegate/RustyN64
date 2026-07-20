# Sprint 2 — COP0, the TLB, and the exception model

**Phase:** Phase 1 — CPU golden log
**Sprint goal:** the VR4300 has a complete COP0 register file, a 32-entry joint TLB with the
two-entry instruction micro-TLB in front of it, and an exception model exact enough that
**n64-systemtest starts and reports a genuine pass/fail count** — which is the first time this
project has an oracle it did not write itself.
**Estimated duration:** 5 weeks

## Why this sprint is shaped this way

Sprint 1's real gate was `basic.z64`: five hardware-verified cases, all passing. That is a real
result and it is also nearly the whole of what a self-judging ROM can tell us at this level.
n64-systemtest is the step change — hundreds of cases, written by people with hardware — and it
is **blocked on this sprint specifically**, not on the CPU in general.

Confirmed by reading its source rather than assuming: `entrypoint()` in
`ref-proj/n64-systemtest/src/main.rs` calls `set_fcsr(...)` — which is `ctc1::<31>` — as its
**fourth statement**. So COP1 *control*-register access is required before any COP0 test can run,
even though COP1 arithmetic is Sprint 3. Immediately after, `main()`'s first action is
`install_exception_handlers()`, which copies handler code into **all three** vectors at
`0x8000_0000`, `0x8000_0080` and `0x8000_0180`. That single fact sets much of this sprint's
scope: `BEV=0` KSEG0 vectoring, a working store path into KSEG0, and the refill/general vector
split all have to be right before the suite prints anything at all.

**Four things this sprint gets for free**, because a research pass re-opened the manual and found
them documented rather than unknown (see `ref-docs/2026-07-20-vr4300-timing-supplement-corrections.md`):

| Constant | Value | Source |
| --- | --- | --- |
| Exception epilogue stall | **2 PCycles** | UM §4.7 p. 114 |
| CP0I (CP0 bypass interlock) | **1 PCycle** | UM §4.6.9 p. 113 |
| ITM (instruction micro-TLB miss) | **3 PCycles** | UM §4.6.2 p. 107 |
| Vector when `EXL=1` | **`0x180`**, not `0x080` | UM Table 6-4 + §6.4.8 pp. 181, 187–188 |

None of these needs fitting. Three of them were previously recorded as unmeasured, which is its
own lesson (`docs/engineering-lessons.md` §3.3b).

## Tickets

### T-12-001 — The COP0 register file

**Description:** the 32 COP0 registers with correct widths, correct writable-bit masks, and
`MFC0`/`MTC0`/`DMFC0`/`DMTC0` access. This is the foundation every other ticket in the sprint
reads or writes, and it is the one where a wrong *width* is invisible until 64-bit software runs.

**Acceptance criteria:**

- [x] All 32 registers present, with the **eight 64-bit-wide ones** exactly right: `EntryLo0`,
      `EntryLo1`, `Context`, `BadVAddr`, `EntryHi`, `EPC`, `XContext`, `ErrorEPC`. Every other
      register is 32-bit. Pinned by a test per register, because "which ones are 64-bit" is a
      list that cannot be derived from anything.
- [x] Read-only registers reject writes: `Random`, `BadVAddr`, `PRId`, `CacheErr`, and
      `Status.DS.TS`. `Cause` is read-only **except** `IP1:IP0` (UM §6.3.6 p. 171) — a mask, not
      a whole-register rule.
- [x] `Config` returns its **hardwired** bit fields on read: bits 23:16 = `0b00000110` and bits
      14:4 = `0b11001000110`, with only `EP`, `BE`, `CU`, `K0` writable and `EC` read-only from
      the DivMode pins (UM Fig. 5-16 p. 152).
- [x] **Cross-validated against the real IPL boot values**, which decompose exactly:
      `Config = 0x0006E463` reproduces both hardwired constants bit-for-bit with `BE=1`, `K0=3`;
      `Status = 0x34000000` is `CU1|CU0|FR`. A test asserts our read-back of those writes matches.
      This is the cheapest independent check available that the layouts are right.
- [x] `Random` decrements per instruction, floors at `Wired`, wraps at 31, reads 31 after cold
      reset, **and is forced to 31 whenever `Wired` is written** (UM §5.4.2 p. 147).
- [x] `EntryHi.Fill` (bits 61:40) is write-ignored and reads 0.
- [x] Reserved registers 7, 21–25, 31: behaviour is **undocumented** (ledger U-1). Implement one
      explicit choice, comment it as a guess, and make it a ledger entry — do not let it look
      decided.
- [x] `PRId.Imp = 0x0B`; the `Rev` field is undocumented (ledger U-3) and must not be invented
      into a plausible-looking value.

**Dependencies:** none within the sprint
**Reference:** UM §5.4 (pp. 146–158), §6.3 (pp. 164–174), Table 1-2 p. 46
**Estimated complexity:** L

---

### T-12-002 — The exception model and dispatch

**Description:** the full exception epilogue, the vector table, and the priority ordering. The
epilogue is short and every line of it is load-bearing; the priority ordering is where a
cycle-accurate core differs from an interpreter.

**Acceptance criteria:**

- [x] The epilogue, in order (UM Fig. 6-14 p. 201): `Cause.ExcCode`/`CE` set; `BadVAddr` set;
      `EntryHi`/`Context`/`XContext` set **only for TLB Invalid / Modified / Miss**; then
      `EXL ← 1`; then `PC ← vector`.
- [x] **`EPC` and `Cause.BD` are written only when `EXL` was 0.** The `EXL=1?` test precedes the
      EPC write in the flowchart, and this is the entire purpose of `EXL` (UM §6.3.7 p. 174:
      *"to keep the processor from overwriting the address of the exception-causing instruction"*).
      Pinned by a nested-exception test — an implementation that always writes EPC passes every
      single-exception test and corrupts every nested one.
- [x] Delay-slot rule: in a delay slot → `Cause.BD = 1` and **`EPC = PC − 4`** (the branch, not
      the delay-slot instruction). Sprint 1 already carries `in_delay_slot` in the latch, so this
      is a consumer of that work, and the multi-cycle-stall test from T-11-001 must still pass.
- [x] The vector table, all four rows, both `BEV` values — including that a TLB/XTLB refill with
      **`EXL=1` uses `0x180`**, resolving ledger S-3. A test asserts `0x180`, citing that UM
      Fig. 6-15 (p. 203) says `0x080` and is wrong.
- [x] `BadVAddr` is **not** written on a Bus Error (UM §6.3.2 p. 164 Caution) — it is not an
      address error.
- [x] The exception epilogue costs **2 PCycles** (UM §4.7 p. 114). Not a fitted constant.
- [x] `ERET`: `ERL=1` → `PC ← ErrorEPC`, clear `ERL`; else `PC ← EPC`, clear `EXL`. **Always
      clears `LLbit`** — this completes the Sprint 1 `LL`/`SC` work, which currently has nothing
      that clears the link. Pinned by a test that `LL`; `ERET`; `SC` fails.
- [ ] **Stage-based priority**, not just the Table 6-5 list: WB > DC > EX > RF, with the
      per-stage orderings of UM §4.7.4–4.7.8 (pp. 116–118), and the rule that an exception from
      any stage beats a stall from the same or an earlier stage. ADR 0007's reverse cascade maps
      onto this directly — that is a reason to encode the stage order explicitly rather than
      re-deriving it per exception.
- [x] The `ExcCode` values of UM Table 6-2 p. 172, including `23 = WATCH`.

**Dependencies:** T-12-001
**Reference:** UM §6.4 (pp. 180–205), Figs. 6-14/6-15/6-16, §4.7 (pp. 114–118)
**Estimated complexity:** XL

---

### T-12-003 — Interrupts, `Count`/`Compare`, and the MI line

**Description:** wire the interrupt path end to end, from the MI's aggregate line through
`Cause.IP` to dispatch.

**Acceptance criteria:**

- [x] An interrupt is taken iff `IE=1` **and** `EXL=0` **and** `ERL=0` **and** the matching `IM`
      bit is set. Exactly one recognition predicate, reusing Sprint 1's DC-stage sampling — the
      T-11-001 criterion that there be only one such predicate in the tree still holds.
- [x] `Count` increments at **half PClock** (every 4th master tick), which ADR 0006's
      `COUNT_DIVIDER = 4` already provides — this ticket asserts it end to end rather than
      re-implementing it.
- [x] `Count == Compare` sets `Cause.IP7`; it is cleared by clearing `IP7` **or by writing
      `Compare`** (UM §6.3.4 p. 165). The write-side effect is the one that gets missed.
- [x] `IP1:0` are software-only — settable and clearable by software, with no hardware path.
- [ ] NMI bypasses `IE`/`EXL`/`ERL` entirely and vectors to `0xBFC0_0000`.
- [x] Which `Int[4:0]` line the MI drives is **board-level and not in the CPU manual**
      (ledger U-4). Resolve it from the N64brew wiki before wiring, and record the source.
- [ ] A `Count`/`Compare` interrupt fires at the right cycle under a multi-cycle stall, not
      merely at the right instruction.

**Dependencies:** T-12-002
**Reference:** UM §6.3.3–6.3.5, §6.4.18 p. 199, Ch. 14 pp. 351–357
**Estimated complexity:** M

---

### T-12-004 — The TLB: JTLB, micro-ITLB, and address translation

**Description:** the 32-entry fully-associative joint TLB, the two-entry instruction micro-TLB in
front of it, and the full 32-/64-bit segment map.

**Acceptance criteria:**

- [x] 32 entries, each mapping an even/odd page pair; page sizes 4K…16M via the seven legal
      `PageMask` encodings of UM Table 5-7 p. 149. An illegal mask is **undefined** per the
      manual — pick a behaviour, comment it, ledger it.
- [x] Match rule: `VPN2` match **and** (`G` **or** `ASID` match). **The `V` bit does not
      participate in matching** (UM §5.4.9 p. 155) — a V-checking matcher passes ordinary tests
      and gets TLB-shutdown wrong.
- [x] `G` on write is `EntryLo0.G AND EntryLo1.G` (UM Fig. 5-10 p. 145).
- [x] `TLBP` sets `Index.P` (bit 31) on a miss. What the **low** bits hold on a miss is
      undocumented (ledger U-2) — pin it with n64-systemtest, do not guess it into the spec.
- [x] `TLBWI` uses `Index`, `TLBWR` uses `Random`; `TLBWR` cannot touch wired entries but
      **`TLBWI` can** (UM §5.4.4 p. 150).
- [x] **TLB shutdown** on multiple matching entries: `Status.TS ← 1`, TLB unusable, reset
      required. It *"can occur even when a matching entry is invalid"* (UM Fig. 6-6 p. 167),
      which is the same fact as the `V`-not-matching rule seen from the other side.
- [x] The 32-bit segment map: `kuseg`/`kseg0`/`kseg1`/`ksseg`/`kseg3` with correct
      mapped/unmapped and cached/uncached attributes; `kseg0`/`kseg1` PA = VA − base. This
      replaces Sprint 1's `addr.rs` segment stripping with the real thing.
- [ ] The 64-bit map including `xkphys`, where **only `C = 2` / window 2 is uncached** and all
      seven other encodings are cached (UM Tables 5-5 p. 140, 5-6 p. 145) — the VR4300 has no
      coherency protocol, so the VR4400's finer encodings collapse.
- [ ] `Status.KX`/`SX`/`UX` gate 64-bit addressing and select the **XTLB** refill vector for the
      mode the faulting address belongs to.
- [x] An address outside any valid region raises Address Error (UM §6.4.7 p. 186).
- [x] The **micro-ITLB is modelled separately** from the JTLB, with its 3-PCycle refill stall
      (UM §4.6.2 p. 107). A micro-TLB miss is a *stall*; a JTLB miss is an *exception*. Collapsing
      the two does not approximate the cost — it deletes the structure the cost occurs in.
      **If this is descoped, it must be descoped explicitly here**, not by omission.
- [ ] 32-bit address calculation that overflows the sign-extended range is **explicitly
      undefined** (UM §5.2.3 p. 130) — ledger U-5, pin with n64-systemtest.

**Dependencies:** T-12-001, T-12-002
**Reference:** UM §5.1–5.4 (pp. 122–158), §4.6.2 p. 107
**Estimated complexity:** XL

---

### T-12-005 — `CACHE`, and the cache model to observable depth

**Description:** the `CACHE` instruction currently decodes to `Reserved` and therefore **raises**
— and both IPL3 and libdragon use it. This is a hard blocker for anything past a bare test ROM.

**Acceptance criteria:**

- [x] `CACHE` (opcode `0o57`) decodes and executes rather than raising.
- [x] The I- and D-caches are modelled to the depth the test ROMs actually observe, and **the
      chosen depth is written down with its justification** — the answer is **zero depth**: cache
      contents are not modelled at all, so `CACHE` is a translating no-op. Sound only because no
      cache state exists to become stale. Recorded as ledger **D-5**, with the point it stops
      being sound (Phase 5 DMA coherency) stated rather than left to be discovered.
- [ ] **Deferred with the cache model.** Cache-miss costs need a cache to miss in; with zero
      modelled depth there is no miss to charge. The formulas and `M` (ledger C-1) stay recorded
      and unimplemented rather than being applied to a cache that does not exist.
- [x] **`M` was not measured, and stays absent.** No real measurement became available, so no
      value was invented. This criterion is met by *not* producing a number.
- [ ] The deferred T-11-008 criterion — **stepping the RCP between SysAD phases** — lands here,
      since it needs the scheduler to own the transaction rather than `DC` completing it inline.
      That is the `Bus`-trait change T-11-008 named and deferred to this sprint.

**Dependencies:** T-12-004
**Reference:** UM Ch. 11, Tables 11-1/11-2; `docs/accuracy-ledger.md` C-1;
`to-dos/phase-1-cpu-golden-log/sprint-1-integer-core.md` T-11-008
**Estimated complexity:** L

---

### T-12-006 — COP1 control access, purely to unblock the oracle

**Description:** `CTC1`/`CFC1` on `FCR31` and `FCR0`, and the coprocessor-unusable path. **Not**
FPU arithmetic — that is Sprint 3. This ticket exists solely because n64-systemtest dies on
`CTC1 $31` three statements after entry.

**Acceptance criteria:**

- [x] `CTC1`/`CFC1` on `FCR31` (FCSR) and `FCR0` (revision) work. FCSR needs real bit semantics
      for the flush-denorm-to-zero and enable-invalid-operation bits the suite sets, but nothing
      needs to *act* on them until Sprint 3.
- [x] `Coprocessor Unusable` is raised correctly per `Status.CU`, with `Cause.CE` set to the
      offending unit.
- [x] **Scope is explicitly control-only.** A criterion here that starts requiring FPU arithmetic
      means the ticket has grown into Sprint 3 and should be split, not stretched.

**Dependencies:** T-12-001
**Reference:** `ref-proj/n64-systemtest/src/cop1.rs`; UM Ch. 7
**Estimated complexity:** M

---

### T-12-007 — n64-systemtest reports a genuine number (was T-11-009)

**Description:** the sprint's actual goal, and the criterion Sprint 1 had to re-scope three times
rather than quietly drop. Get the suite to start, run, and report a real pass/fail count.

**Acceptance criteria:**

- [ ] **Blocked on PI/cart, which is Phase 5.** Measured rather than assumed: with the `ERL`
      rule implemented the suite now retires **30,679 instructions** with no exception (up from
      **2**), and then runs past the end of the 1 MiB IPL3 window at `entry + 0x100000`. It reads
      the rest of its own ELF from cart **via PI DMA**, which does not exist yet — so the
      remaining criteria below cannot be met inside Phase 1 without pulling PI forward.
      → re-scoped; see the note at the end of this ticket.
- [ ] The suite gets past `entrypoint()` — SP/DMEM reads, `CTC1` FCR31, MI mask writes, KSEG0
      cached and KSEG1 uncached translation, and a working store path into KSEG0.
- [ ] `install_exception_handlers()` succeeds: handler code is copied to all three of
      `0x8000_0000`, `0x8000_0080`, `0x8000_0180` and is subsequently *executed* from there.
- [ ] The harness reads the result count. **Which channel to read is not yet determined** — the
      suite has `isviewer.rs`, `sc64.rs` and a `FramebufferConsole`, and more than one may be
      live. Establish which before building the reader; do not assume.
- [ ] `docs/STATUS.md`'s accuracy table records the genuine number, whatever it is. **A low
      number is a result, not a failure** — the honest count is the point, and tuning anything to
      raise it violates the ledger's central rule.
- [ ] The ledger's `U-1`…`U-5` undocumented entries are pinned against the suite where it
      exercises them, and remain open where it does not.

**Diagnosed (2026-07-20): the suite is not waiting, it is lost — and the cause is SP DMEM.**

With PI and `ISViewer` in place the suite still printed nothing, so the failure was traced rather
than guessed at. Probing for the first divergence found it **191 retired instructions in**:
execution jumps from `0x800A15F4` into a zero-filled region and NOP-slides until it falls off the
top of RDRAM at `0x8080_0000`, which is why longer budgets never helped.

The cause is in the suite's own `entrypoint()`:

```rust
let memory_size = SPMEM::read(0) as usize;
let elf_header_offset = ((SPMEM::read(12) >> 16) << 8) as usize;
MemoryMap::init(memory_size, elf_header_offset);
```

`SPMEM` is **RSP DMEM**, which is a stub returning 0. **IPL3 writes the detected RDRAM size into
SP DMEM at boot**; we do not, so the suite builds its memory map from zeros and jumps into
nothing. Everything after that is noise.

**SP DMEM was necessary but not sufficient.** Seeding it (word 0 = RDRAM size, word 12 = the
packed ELF offset) is now implemented and let the suite run far longer — but it still diverges at
the same instruction, because there is a second, larger problem underneath.

**The real blocker: n64-systemtest is an ELF and the harness does a flat copy.** Its `PT_LOAD`
segments are:

| Segment | ROM offset | vaddr | `filesz` | `memsz` |
| --- | --- | --- | --- | --- |
| 0 | `0x1900` | `0x8000_0000` | `0x400` | `0x400` |
| 1 | `0x1D00` | `0x8000_0400` | `0x18F560` | `0x18F560` |
| 2 | `0x191260` | `0x8018_F960` | `0x1D340` | `0x1D340` |
| 3 | `0x1AE5A0` | `0x801A_CCA0` | `0x60` | `0x4B0` (BSS) |

`load_direct` places `ROM[0x1000 + k]` at `entry + k`, i.e. `0x800A15E8 + k`. The segments want to
land at `0x8000_0000` onward. So **every address in the image is wrong**, which is why the first
jump — to a perfectly valid `0x8018368C` — lands in zeros.

**ELF loading is now implemented** (`rom::load_elf`), and it moved the failure. The image loads
correctly — `0x1AD150` bytes across the four `PT_LOAD` segments, BSS zeroed — and execution no
longer jumps into RDRAM zeros.

**The failure is now an exception, six instructions in.** Execution runs
`0x800A15E8 … 0x800A15F8` and then vectors to `0xBFC0_0200` — the **`BEV=1` TLB refill vector**,
which lives in PIF ROM that we do not emulate, so it lands in zeros and slides.

Two things follow, and they are different problems:

1. ~~**Something at instruction ~6 raises a TLB refill.**~~ **Identified: the stack pointer.**
   Disassembling the entry showed a standard prologue — `ADDIU $29, $29, -0x50` then
   `SW $31, 0x4c($29)`. With `$29` at zero that store targets `0xFFFF_FFFF_FFFF_FFFC`, a KSEG3
   address, TLB-mapped, refill. IPL3 leaves `$sp` at the top of SP DMEM (`0xA400_1FF0`); the
   direct-load path set no registers at all.

   **Loading the image correctly is not sufficient if the register state it was compiled against
   is missing.** `seed_ipl3_handoff` now sets `$sp`, and the fault moves from instruction ~6 to
   **instruction ~25** — so there is at least one more such expectation to find. The same
   disassemble-at-the-faulting-PC method will find it; it took one look to find this one.
2. ~~**`BEV` is still 1**~~ **Resolved with the same fix.** The next fault after `$sp` was
   `ExcCode = 11` (Coprocessor Unusable) at `EPC = 0x800A_1650` — a COP1 instruction with `CU1`
   clear. IPL3 leaves `Status` at `0x3400_0000` (`CU1 | CU0 | FR`), the value the COP0 work had
   already cross-checked against a real boot capture, and seeding it also clears `ERL` and `BEV`.

**Current state: the suite runs 108,000,000 instructions with ZERO exceptions and still prints
nothing.** That is a materially different failure from every previous round — it is no longer
lost, faulting, or sledding. The remaining question is genuinely *"what is it waiting on"*, which
is what the first round wrongly assumed.

**Both were checked, and the answer is the encouraging one.**

`isviewer::detect()` **never runs** — its magic word is never written to the buffer across 108M
instructions. And a PC histogram over a 2M-instruction window shows the hottest address by a wide
margin is **`0x8000_0180`, the general exception vector**, with the suite's own installed handler
underneath it (33,900 hits, ~1.7× the next).

I first read that as *"the suite is executing its tests"* — n64-systemtest raises exceptions by
the thousand on purpose. **That reading was wrong**, and a second probe disproved it: over a
2M-instruction window there is exactly **one distinct `EPC`** (`0x8018_32E8`, 2,000,000 hits) and
exactly **one `ExcCode` (2 = `TLBL`)**.

**It is an infinite exception loop.** A single load faults, the handler runs, returns, and the load
faults again. A hot exception vector looks identical to a busy test suite from a PC histogram
alone; the thing that distinguishes them is whether `EPC` *moves*, and it never does.

Recording the mistake because it is instructive: the histogram was real evidence and I drew an
optimistic conclusion from it that one more cheap measurement would have refuted. "Which
instruction" was the question the whole session's method was built on, and I stopped one step
short of asking it.

**The real remaining problem is a TLB refill that never resolves.** Candidates, in order:

1. ~~The suite's refill handler writes a TLB entry that our TLB does not match.~~
   **Investigated. A real bug was found here and it was *not* the cause.**

   `Cop0::tick_random` was implemented and **never called from the pipeline** — only from a unit
   test. `Random` therefore sat at 31 forever, so every `TLBWR` overwrote the same entry
   (UM §5.4.2: *"decrements as each instruction executes"*). That is a genuine emulation defect,
   and precisely the shape this candidate predicted: a stuck counter is invisible to any test that
   calls `tick_random` itself, which is what the COP0 unit tests do.

   **Fixed** — advanced at retirement, pinned by a test driven through `advance` rather than by
   calling the method directly — **and the loop persists unchanged**: still one distinct `EPC`,
   still `TLBL`. So the refill failure is candidate 2 or 3.
2. **CONFIRMED — the handler itself faults.** The instruction at `0x8018_32E8` is `0x42800060`:
   COP0 CO-class, funct **`0x20`**, which is n64-systemtest's `XDETECT_FUNCT` — the **emux probe
   opcode** (`ref-proj/n64-systemtest/src/emux.rs`). The suite executes it deliberately to ask
   *"am I running under emux?"*, expecting a Reserved Instruction exception on anything else.

   Our decoder correctly leaves it `Reserved`, so the RI fires and `EPC` is set to `0x8018_32E8`.
   But the reported `ExcCode` is **2 (`TLBL`)**, not 10 (`RI`) — meaning a **second** fault happens
   *inside the handler*, and `EPC` survives it exactly as the `EXL` gate requires. That also
   explains the histogram: `0x180` (general, `EXL=1`) hotter than `0x000` (refill, `EXL=0`).

   **The `$gp` hypothesis was wrong, and checking it cost one command.** `readelf -Ws` reports
   `_gp` as `NOTYPE LOCAL HIDDEN ABS 0x00000000` — the linker never assigns it, because Rust's
   MIPS backend does not use `$gp`-relative addressing here. `$gp = 0` is *correct*. Seeding it
   would have invented a value and buried the real fault under a plausible-looking fix.

   **Root cause found: the suite's exception vectors are never installed.** At the instant of the
   Reserved Instruction, RDRAM holds

   | Address | Contents | Expected |
   | --- | --- | --- |
   | `0x8000_0000` | `0x7F454C46` (`\x7fELF`) | `la $26, 0x80000000` |
   | `0x8000_0180` | `0x0000_0000` | `la $26, 0x80000180` |

   `main()` calls `install_exception_handlers` **before** the emux probe, so those three-word stubs
   (`la $26, <vector>; j generic; nop`) should be in place. They are not — **the installer's stores
   are being lost**, and `0x8000_0000` still holds the ELF header we loaded there.

   That explains the whole signature end to end, with nothing left over:

   1. The RI fires correctly and dispatches to `0x8000_0180`.
   2. `0x180` is zeros; `0x0000_0000` decodes as `SLL $0, $0, 0` — a `NOP`.
   3. The CPU therefore NOPs from `0x180` all the way to `0x8000_0400`, which is exactly the PC
      trail we recorded (`…0x3F8, 0x3FC, 0x400, 0x404…`).
   4. `0x8000_0400` is the start of `.text` — arbitrary code entered mid-function with no
      register context, which faults on a load through a zero base: `BadVAddr = 0xC`.
   5. `EXL` is set by then, so `EPC` keeps the *first* exception's value — which is why the
      reported `ExcCode` (2, `TLBL`) and `EPC` (`0x8018_32E8`, the RI site) disagreed and made
      this look like a TLB bug for three rounds.

   **A note on the ELF header at `0x8000_0000`.** The image's first `PT_LOAD` is
   `off=0 vaddr=0x8000_0000 filesz=0x400` — the ELF header and program headers themselves, which
   the linker maps over the vector region. Loading it is *correct* ELF behaviour and is not the
   bug; hardware never populates that range because IPL3 copies from ROM `0x1000` to
   `0x8000_0400`. It is benign precisely because the suite overwrites the vectors — which is the
   step that is failing.

   **Resolved. The bug was ours, in the decoder.**

   `init_allocator`'s print was not a panic — the function legitimately ends with
   `println!("Heap range: …")`, and its own comment explains printing must wait for the allocator.
   So the suite prints from `entrypoint`, *before* `main` installs handlers, by design.

   That reframed the question: if the emux probe at `0x8018_32E8` raised Reserved Instruction on a
   real VR4300, n64-systemtest would derail on every N64 it has ever run on, before printing a
   line. It does not. **COP0 CO `funct` 0x20-0x3F must retire inertly**, and our decoder was
   raising RI. The range is not a guess — the suite's probe constant is named
   `XDETECT_CODE_EXTENSIONS_20_3F`, i.e. emux claims exactly that space because hardware leaves it
   inert. Added as `Op::Cop0Extension`, recorded as ledger **C-8** (an inference, not a manual
   citation; the target-GPR writeback is untested), with a decode test whose range boundary was
   mutation-checked.

   **The suite now boots and runs its tests:**

   ```text
   Heap range: 801ad150 to 80300000
   Running StartupTest...
   Test 'StartupTest' failed: a=0x7006e460 b=0x7006e463. Initial COP0 Config
   Running Unaligned LW exception...
   Test 'Unaligned LW exception' failed: a=0x8010 b=0x10. Cause during AdEL exception
   ```

   Two genuine defects are now visible and exactly quantified, where before there was only
   silence — COP0 `Config` reset low bits (`K0`), and a spurious `IP7` in `Cause` during AdEL
   (`0x8010` vs `0x10`). Neither was reachable while the decoder swallowed the probe.

   **Method note.** Four hypotheses, three wrong, and the wrong ones were all *mechanism* guesses
   (`$gp` unset, lost stores, an early panic) that would each have taken real work to "fix". The
   three cheap questions that actually moved it were: what is in memory at the faulting moment,
   which symbol owns this address, and was this function ever executed. `readelf` and a
   fourteen-entry control-transfer trace did more than 100M extra instructions ever did.

   **T-12-007 status after the unblock.** The suite boots, runs its full corpus, and reports.
   Five defects fixed, all oracle-surfaced:

   | # | Defect | Oracle signature |
   | --- | --- | --- |
   | 1 | COP0 CO `funct` 0x20-0x3F raised RI | suite never printed a line |
   | 2 | `IP7` latched at power-on (equality, not edge) | `Cause = 0x8010` vs `0x10` |
   | 3 | `Context`/`XContext` gated on TLB exceptions | `Context = 0x0` vs `0x0052_0000` |
   | 4 | `SP_STATUS` unmodelled, read as 0 | RSP STATUS `0x0` vs `0x1` |
   | 5 | Cartridge never inserted, only its RDRAM image | every cart read `0x0` |

   `StartupTest` and the whole unaligned-access group pass. **`Failed: 0` is not met** — the run
   still ends in "Exception storm detected. Aborting."

   **Remaining work, in yield order.** These are enumerable now, which they were not before:

   1. **PI/cart bus semantics** (the largest cluster). Sub-word read latching — a 16-bit read at
      `cart[1]` gives `0x4567` where hardware gives `0x89AB`; cart-address auto-increment after
      DMA; `PI_STATUS.IOBUSY` during a write; and the documented **write-latch decay**, where a
      value written to cart ROM stays briefly visible then fades. We drop cart writes entirely, so
      that test now fails in the *opposite* direction, which is progress worth noting.
   2. **32-bit address sign-extension.** `LW/SW with address not sign extended` expect an
      `AdEL` that we do not raise: `segment()` truncates with `vaddr as u32`, so
      `0x0000_0000_8000_0000` is treated as KSEG0. The check must be **gated on 64-bit addressing
      mode** (`Status.KX`/`SX`/`UX`), which is not modelled yet — landing it ungated would fault
      legitimate sign-extended kernel addresses, so `KX` comes first.
   3. **The exception storm.** Whatever drives the abort likely masks further tests; worth
      diagnosing before grinding through the cart list, since the abort may be truncating the run.

   **Item 3 done — it was COP2.** `MFC2`/`MTC2`/`DMFC2`/`DMTC2` are valid VR4300 encodings that must
   raise **Coprocessor Unusable** (`ExcCode 11`) with `Status.CU2` clear, not Reserved Instruction
   (`10`) — the same distinction already drawn for COP1. Five in a row tripped the suite's recovery
   limit and aborted the whole run. Removing the abort raised the reported failure count from 2,551
   to **2,909** while the emulator strictly improved: the old number was a floor, as predicted.

   **Next blocker: the allocator returns a KSEG1 pointer.** The run now stops with a panic inside
   the suite's own `MemoryMap::uncached_mut` during `spmem: DMA RDRAM -> DMEM (all aligned)`:

   ```text
   assertion failed: `(left == right)`  left: 0xA0000000, right: 0x80000000
   src/memory_map.rs:50
   ```

   The test does `MemoryMap::uncached_mut(source_data.as_mut_ptr())` on an ordinary heap `Vec`.
   The assert checks bits 31:29, so the **`Vec`'s own pointer was in KSEG1** — yet the heap is
   `0x801A_D150..0x8030_0000`, squarely KSEG0 (the suite prints this at startup, and it is
   correct). So the allocator handed back a pointer outside its own arena.

   That points at **heap corruption from an earlier test**, not at the DMA test. The investigation
   is therefore a bisect, not a lookup: find the earliest test after which the allocator's
   free-list is inconsistent. A cheap first probe is to watch for the first store that lands
   outside `0x801A_D150..0x8030_0000` but was meant for the heap — most likely a DMA whose length
   or destination we compute wrongly, given how much of the PI/SI surface is still stubbed.

   Do **not** guess a mechanism here. The three wrong hypotheses earlier in this ticket were all
   mechanism guesses, and the cheap state questions were what actually resolved it.

   **Update — that blocker is fixed, and it was the stack.** The suite's SP-DMA tests build their
   source data in a **stack array**, so `MemoryMap::uncached_mut(source_data.as_mut_ptr())` is
   asserting on `$sp`. We seeded `$sp` into SP DMEM (`0xA400_1FF0`, KSEG1) and the assert demands
   KSEG0. Not heap corruption at all — the bisect was never needed. `$sp` now sits in KSEG0 above
   `MemoryMap::HEAP_END`, and SP DMA is implemented (`SP_RD_LEN`/`SP_WR_LEN`, with the packed
   length word's row/count/skip fields).

   Note the shape of the earlier note: it correctly ruled out the DMA test itself and correctly
   said "do not guess a mechanism", but its own suggested framing — *heap corruption, so bisect* —
   was itself a mechanism guess, and wrong. The answer came from reading what the failing line
   actually operates on.

   **Current blocker:** `TLB: Execute mapped branch which has a non-mapped delay slot`, reporting
   `Cause during TLB exception` where the suite expects raw `0x8` (`ExcCode 2`, `TLBL`, nothing
   else set).

   **Update — the TLB `Cause` mismatch was a stale `Cause.CE`.** `0x2000_0008` vs `0x8` differs in
   bit 29 alone: `CE = 2`, left behind by the COP2 usability check. `CE` was written only for
   Coprocessor Unusable, on the documented reasoning that it is meaningless otherwise. It is now
   written on every dispatch. Failure count 2,932 to **2,901**.

   **Next area: PI direct-I/O sub-word reads.** `cart: Read16` at byte offset 2 gives `0x4567`
   where hardware gives `0x89AB`; `cart: Read8` at offset 2 gives `0x45` where hardware gives
   `0x89`. Both hardware values come from byte offset **4**, after a preceding 32-bit read at
   offset 0.

   The N64brew *Peripheral Interface* page supplies the governing fact: **"direct I/O accesses can
   only be done as 32-bit words (concatenating two consecutive 16-bit reads)"**, and the mapped
   ranges are `0x0500_0000-0x1FBF_FFFF` and `0x1FD0_0000-0x7FFF_FFFF`. So a CPU byte or halfword
   load from cart is not a narrow bus access at all — the PI performs a full 32-bit read.

   **The rule, read rather than inferred.** I guessed "an address latch advanced past the previous
   read". **Wrong**, and the wiki says so plainly (`Memory map`, PI external bus):

   > The external PI bus is 16-bit. Given that the RCP only knows of 32-bit accesses (as access
   > size is ignored), this means that each read or write performed by the VR4300 will cause
   > exactly two reads or two writes on the PI bus: first the MSB at the address specified by the
   > CPU (ignoring bit 0), then the LSB at address+2. [...] effectively a 16-bit read at
   > `0x1000'0002` returns the 16-bit word at `0x1000'0004`.

   So: **access size is ignored**; the RCP always returns the 32-bit word starting at
   `addr & !1`, while the CPU selects its byte lane assuming a word at `addr & !3`. The
   off-by-two between those two bases *is* the bug, and it is a hardware bug we must reproduce.

   Checking it against every data point we have:

   | Access | RCP word base (`addr & !1`) | CPU lane (`addr & 3`) | Result | Expected |
   | --- | --- | --- | --- | --- |
   | `LW` @0 | 0 → bytes 0-3 | 0 | `0x01234567` | `0x01234567` ✓ |
   | `LH` @2 | 2 → bytes 2-5 | 2 (low half) | `0x89AB` | `0x89AB` ✓ |
   | `LB` @2 | 2 → bytes 2-5 | 2 | byte 4 = `0x89` | `0x89` ✓ |

   **Implementation note that matters.** This cannot be done in `read_u8`: the lane depends on the
   *original access address and width*, which a byte-granular decomposition throws away — a word
   read at 0 and a halfword read at 2 both touch byte 2, and they must resolve differently.
   And the change is **larger than a `Bus` override**, which I initially assumed. `CpuBus` today
   declares `read_u8` (required), `read_u32` (defaulted from `read_u8`), and `write_u32` — there is
   **no `read_u16`**. So an `LH` is issued as two `read_u8` calls and the bus is never told a
   halfword was requested. Overriding on `Bus` alone cannot work, because the information does not
   reach it.

   The actual work is three steps, in order:

   1. Add `fn read_u16(&mut self, addr: u32) -> u16` to `CpuBus`, defaulted from `read_u8` so no
      existing implementor changes behaviour.
   2. Route the CPU's `LH`/`LHU` path through it, so the access width reaches the bus.
   3. Override `read_u16`/`read_u32` on `Bus` for the PI window with the `addr & !1` base and
      `addr & 3` lane.

   Step 2 touches the shared memory path, so it is the one to be careful with: every load must
   keep its current behaviour for RDRAM, SPMEM, and the register windows. That is why this was
   not attempted at the tail of a long session.

   Confirm against the `cart: Read32` cases that currently **pass**, so this does not regress them.

   **Done** — and smaller than the three steps above predicted. No `CpuBus::read_u16` was needed:
   a halfword load is already issued as two byte reads, and both land correctly under the byte
   rule `byte = (addr & !1) + (addr & 3)`. Only `read_u32` needed an override, to read the PI
   window raw instead of applying the shift per byte. One real trap: **ISViewer lives inside the
   PI range** and must keep claiming its window first, or the debug channel's read-back handshake
   breaks — the existing round-trip test caught it immediately. Failure count 2,901 to **2,899**.

   **Measured failure breakdown — and a correction.** I asserted several times that the remaining
   gap was dominated by the LLE RSP. **That was wrong, and I had never measured it.** Grouping the
   2,850 attributable failures by test prefix:

   | Failures | Group |
   | ---: | --- |
   | **2,413** | **COP1** |
   | 291 | RSP |
   | 15 | SP |
   | 14 | cart-writing |
   | 14 | cart_memory |
   | 13 | spmem |
   | 9 | pifram |
   | ~80 | everything else (CACHE, MI, RDP, RE, …) |

   **COP1 is 85% of the remaining gap.** The RSP is 10%. Every PI/cart item I have been working
   through is a rounding error against the FPU — I was optimising the tail because I picked targets
   by reading failure text rather than by counting it.

   The good news is that this is *plumbing*, not new modelling. `crates/rustyn64-cpu/src/fpu.rs`
   already implements the arithmetic, comparisons, conversions, rounding modes, and the `MUL`
   erratum; `fpr.rs` implements the `FR` register views. What is missing is the **decode and
   execute wiring**: FP arithmetic still decodes to `Op::Cop1Unimplemented`, which executes as a
   no-op, so the whole implemented FPU is unreachable from an instruction stream.

   That makes COP1 the next work item, ahead of everything else in this ticket, and it is
   `T-13-00x` Sprint 3 scope that was deferred rather than an unstarted subsystem.

   **Lesson worth keeping:** "which failure is most common" is a *state question*, answerable in
   one probe, and I spent several rounds on mechanism-adjacent target selection without asking it.
   The same mistake as guessing mechanisms, one level up.

   **Design for the COP1 wiring, derived from reading the existing paths.** FP arithmetic cannot
   go through `exec::execute`: that function has no access to the FPR file, and an FP op reads two
   FPRs and writes a third. The existing COP1 moves already solve this shape — `exec` returns a
   `Cop0Access::Cop1(Cop1Access::…)` request and the **pipeline** performs it against `self.fpr`
   (see the `ReadFpr` / `WriteFpr` handling). Follow that, do not invent a second mechanism:

   1. **Decode.** In `OP_COP1`, the `rs` field is the format (`16`=S, `17`=D, `20`=W, `21`=L) and
      `funct` is the operation. `Decoded` already carries what is needed: `rs`=fmt, `rt`=ft,
      `rd`=fs, `sa`=fd. Add one `Op::FpArith` rather than ~60 variants.
   2. **Request.** Add a `Cop1Access::Arith { fmt, funct, ft, fs, fd }` variant.
   3. **Execute in the pipeline.** Read the operands via `fpr.read_s`/`read_d` (which apply the
      `FR` view — do **not** use `read_raw`, that was ledger U-7's bug), dispatch into the existing
      `fpu::add_s` / `mul_d` / `compare_s` / `cvt_*` / `to_i32` functions, merge the returned
      `Outcome::flags` into `FCSR`, and write the result back through `write_s`/`write_d`.
   4. **Exceptions.** `Outcome` already reports the IEEE flags; an enabled trap must raise
      `Exception::FloatingPoint`, and `cvt_s_l`/`cvt_d_l` return `Err` for the bits-63:55 case that
      must become `Unimplemented` (bit 17), **not** Invalid.

   The FPU semantics are already written and tested in `fpu.rs`; this is wiring, and the risk is
   in the flag/exception merge rather than the arithmetic. Do it format-by-format with the suite
   re-run between formats, since COP1 is 2,413 failures and a regression here would be invisible
   against that background.

   **First slice landed: S/D `ADD`/`SUB`/`MUL`/`DIV`. It moved the failure count by zero.**
   2,897 before, 2,897 after. The wiring is correct and unit-tested — decode reaches `Op::FpArith`,
   the pipeline reads through the `FR` view, dispatches into `fpu`, and merges `Cause`/`Flags` into
   `FCSR` — but eight operations out of roughly sixty did not move the needle.

   **Do not read that as "COP1 is hopeless"; read it as "the breakdown is still too coarse."**
   Grouping by the `COP1` prefix told me where the mass is, not which operations or which
   *assertions* fail. Two candidate explanations, both unverified:

   - the failures are concentrated in the operations still unwired (`SQRT`, `ABS`, `NEG`, `MOV`,
     the conversions, the `C.cond.fmt` compares and the `BC1*` branches), so a wired `ADD.S` is a
     small share; or
   - the arithmetic executes correctly but each test *also* asserts exception behaviour, and the
     **enabled-trap path is deliberately absent** (see `Pipeline::fp_arith`), so every one still
     fails on the trap even when the value is right.

   **Next probe, before writing more code:** group the COP1 failures by *test name* rather than by
   prefix, and check whether an `ADD.S` case now fails on its value or on its exception. That
   distinguishes the two explanations in one run and decides whether the next work is more
   operations or the trap path. Writing the remaining fifty operations first would be the same
   mistake as picking targets by reading failure text — one level finer, but the same shape.

   **Deferred area: PI direct-I/O writes are asynchronous, with a decaying latch.**

   The `cart-writing:` group. The same N64brew *Memory map* section documents it:

   > All writes are performed **asynchronously** by the PI. Making a write in this area will in
   > fact just cause the PI to latch the value internally, and release the VR4300 immediately. The
   > write will then happen in background. The status of the ongoing write will be reflected by the
   > PI "I/O busy" status bit, which will be set to 1 until the write is finalized. While a write
   > is ongoing, further writes are ignored, and reads (from any address) return the 32-bit value
   > that is being written.

   So a cart write must: latch the 32-bit value; set `PI_STATUS.IOBUSY`; make **every** PI-bus read
   return the latched value while busy; and ignore further writes until it finalises. Note the PI
   does not know a device is read-only, so ROM-area writes follow the same path and are simply
   dropped by the ROM.

   That much is documented and directly testable — it covers `Write32/Read32`, the `Write8/16/64`
   variants, and `check PI.STATUS IOBUSY`.

   **The decay duration is NOT documented here, and must not be invented.** The
   `cart-writing: Temp value decay` tests bound it only relatively: still visible after 0 loop
   iterations, gone after 110. Turning that into a cycle count requires knowing the loop's cost,
   and the honest options are to derive the finalisation time from the PI domain timing registers
   (`PGS`/`RLS`/`PWD`/`LAT`, which we do not model) or to leave those four tests failing and say
   so. Implementing the latch **without** a decay would flip them from failing-absent to
   failing-stuck, which is not progress — the previous note in this ticket made exactly that
   mistake in the opposite direction and it was worth recording, so do not repeat it.

   **One hypothesis already refuted, so nobody re-spends it:** a spurious `IP2` from the PI
   interrupt, which the Bus now mirrors on every PI write and which software must clear
   explicitly. **Ruled out by inspection** — `Bus::poll_irq` ANDs each MI line against
   `mi_mask`, and the reset mask is all-false, so `IP2` cannot assert until software enables it.
   Refuted by reading the code, at no runtime cost.

Disassembling `0x8018_32E8` separated these in one step. Note what made it possible: the
**instruction word**, not the address. `0x42800060` is meaningless as an address and unambiguous
as an encoding.

Output routing and budget are **secondary** and were previously mis-ranked as the remaining work:

- `main()` renders the framebuffer console *after* `tests::run()` returns, so with `ISViewer`
  unselected there is nothing to read until the whole suite finishes.
- 108M instructions is plainly not enough for hundreds of hardware tests that each fault
  repeatedly. The budget was chosen to prove liveness, not to reach completion.

**Next steps, in order:** find why `detect()` is not reached (it is called from the print path, so
the suite may simply not print until the end); failing that, read the **framebuffer console**
instead, which needs no VI emulation — the text is rendered into a buffer in RDRAM and can be read
directly. Then run to completion with a budget sized for the whole suite rather than for liveness.

A harness that cannot see PIF ROM should still **fail loudly** on a `BEV=1` vector rather than
executing zeros; that is now a latent trap rather than an active one, but it will bite the next
person who introduces a fault.

The pattern from the last three findings holds: each fix moved the failure rather than resolving
it, and each new failure was more specific than the last. That is progress, but it should be
reported as *"the failure moved to X"*, not as *"fixed"*.

Note the entry point in the **ROM header** (`0x800A15E8`) is inside segment 1 and remains correct;
what was wrong was the load *mapping*, not the entry.

Worth recording *how* this was found: three successive budget increases (2 → 30k → 3M → 45M
instructions) all looked like progress and none were. Probing for the **first divergence** found
it immediately, and would have on the first attempt. Same pattern that found the `ERL` rule and
the PI dependency (`docs/engineering-lessons.md` §3.2).

**Re-scope note (measured, 2026-07-20).** Running the ROM was itself the most valuable thing
this ticket did, and it found a real bug nothing else would have: `Status.ERL = 1` makes KUSEG
unmapped (UM §5.2.2), cold reset **sets** `ERL`, and without that rule the suite died on its
second instruction with `ExcCode = TLBS`, `BadVAddr = 0`. Fixed and pinned.

What it then hits is a **dependency, not a defect**: n64-systemtest loads the rest of its own ELF
from cart through PI DMA. PI is Phase 5. So `Failed: 0` on the CPU/COP0/TLB categories — which is
also the **v0.2.0 cut criterion** — is not reachable without either pulling PI DMA forward into
Phase 1 or building a harness-side loader that stages the whole ROM image rather than IPL3's
1 MiB. That is a scoping decision, not something to quietly work around; it is recorded here so
it is made deliberately.

**Decided (2026-07-20): pull PI/cart DMA forward into Phase 1.** The alternatives were a
harness-side loader that stages the whole ROM image — which would have made the harness diverge
from what hardware does, exactly the kind of shortcut later mistaken for accuracy — or weakening
the v0.2.0 cut criterion. Pulling PI forward keeps the criterion an honest oracle result. Phase 1
therefore absorbs the PI/cart work that Phase 5 had scoped; **`to-dos/ROADMAP.md` and
`to-dos/VERSION-PLAN.md` must be updated to reflect that**, so the phase spine stays truthful
rather than the change living only in this note.

**Dependencies:** T-12-002, T-12-003, T-12-004, T-12-006, **and PI/cart DMA (Phase 5)**
**Reference:** `ref-proj/n64-systemtest/src/main.rs`, `src/exception_handler.rs`,
`src/tests/testlist.rs`
**Estimated complexity:** L

---

## Risks

- **The oracle changes what "done" means, and it will be unkind.** Every prior gate was one we
  wrote or a five-case ROM. A suite with hundreds of hardware-verified cases will report a number
  that is lower than it feels like it should be. The risk is not the number — it is the pressure
  to special-case toward it. ADR 0005 and the ledger exist for exactly this moment: a failure
  whose fix would be a per-quirk patch becomes a ledger entry instead.
- **Nested exceptions are where an epilogue that passes every test is still wrong.** The `EXL`
  gate on the `EPC` write is invisible to single-exception testing, and TLB refill handlers take
  nested exceptions *by design* (UM §6.4.8 p. 188 describes it as the normal path). Test nesting
  first, not last.
- **The `V`-bit-not-in-matching rule looks like a bug in the manual.** It is stated twice, from
  two directions, and TLB shutdown depends on it. Expect to want to "fix" it.
- **`M` remains unmeasured and is now more tempting**, because T-12-005 puts cache costs in front
  of a suite that reports a number. Fitting `M` until the number improves would make every later
  timing result unfalsifiable. The ledger's rule is not negotiable here.
- **The micro-ITLB is easy to skip and hard to retrofit** — it sits in front of the JTLB and
  changes what a "TLB miss" even means structurally. Decide explicitly in T-12-004.
- **Scope creep from T-12-006 into Sprint 3.** COP1 control is a small ticket adjacent to a very
  large one. The criterion above is the tripwire.

## Sprint review checklist

- [ ] All tickets checked off or explicitly deferred **with a reason recorded in the ticket**.
- [ ] n64-systemtest reports a genuine number, and `docs/STATUS.md` records it honestly.
- [ ] Ledger entries C-2, C-3, C-7 and S-3 are closed as documented-by-citation; U-1…U-6 are
      either pinned or still explicitly open.
- [ ] No constant was adjusted to make a ROM pass. (`M` in particular either has a real
      measurement or has no value.)
- [ ] Still exactly one interrupt-recognition predicate in the tree.
- [ ] `docs/cpu.md` updated in the same change as the code it describes.
- [ ] CHANGELOG.md updated.
