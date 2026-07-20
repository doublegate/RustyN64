//! Exception dispatch — the epilogue and the vector table (T-12-002).
//!
//! Short, and every line of it is load-bearing. The hardware sequence is UM
//! Fig. 6-14 (p. 201) for the general case and Fig. 6-15 (p. 203) for TLB
//! refills.
//!
//! # The `EXL` gate is the whole point
//!
//! `EPC` and `Cause.BD` are written **only when `EXL` was already 0**. The
//! flowchart's `EXL = 1?` test precedes the `EPC` write, commented *"Check for
//! multiple exception"*, and UM §6.3.7 (p. 174) states the reason directly:
//!
//! > *"The EXL bit ... is set to 1 to keep the processor from overwriting the
//! > address of the exception-causing instruction contained in the EPC
//! > register in the event of another exception."*
//!
//! An implementation that always writes `EPC` passes **every** single-exception
//! test and corrupts **every** nested one — and nesting is not exotic: UM §6.4.8
//! (p. 188) describes a TLB refill handler taking a second TLB miss as the
//! normal path.
//!
//! # The `0x080` trap
//!
//! UM Fig. 6-15 (p. 203) routes a refill with `EXL = 1` to offset `0x080`. That
//! figure is **wrong**, and the manual contradicts it three times — see
//! [`vector`] and accuracy-ledger S-3.

use crate::cop0::{Cop0, reg};
use crate::pipeline::Exception;

/// `Cause.ExcCode` values (UM Table 6-2, p. 172).
///
/// The full architectural list, including the codes the N64 cannot reach, so
/// that a value read out of `Cause` can always be named.
pub mod exc_code {
    /// Interrupt.
    pub const INT: u64 = 0;
    /// TLB modification.
    pub const MOD: u64 = 1;
    /// TLB miss on a load or instruction fetch.
    pub const TLBL: u64 = 2;
    /// TLB miss on a store.
    pub const TLBS: u64 = 3;
    /// Address error on a load or instruction fetch.
    pub const ADEL: u64 = 4;
    /// Address error on a store.
    pub const ADES: u64 = 5;
    /// Bus error on an instruction fetch.
    pub const IBE: u64 = 6;
    /// Bus error on a data reference.
    pub const DBE: u64 = 7;
    /// `SYSCALL`.
    pub const SYS: u64 = 8;
    /// `BREAK`.
    pub const BP: u64 = 9;
    /// Reserved instruction.
    pub const RI: u64 = 10;
    /// Coprocessor unusable.
    pub const CPU: u64 = 11;
    /// Arithmetic overflow.
    pub const OV: u64 = 12;
    /// Trap.
    pub const TR: u64 = 13;
    /// Floating-point exception.
    pub const FPE: u64 = 15;
    /// Watchpoint.
    pub const WATCH: u64 = 23;
}

/// Which vector an exception uses.
///
/// Only three kinds exist, because the vector table has only three rows. Every
/// exception that is not a TLB refill takes the general vector, including a TLB
/// refill that arrives with `EXL` already set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VectorKind {
    /// The common vector, offset `0x180`.
    General,
    /// 32-bit TLB refill, offset `0x000` — **only** with `EXL = 0`.
    TlbRefill,
    /// 64-bit TLB refill, offset `0x080` — **only** with `EXL = 0`.
    XtlbRefill,
    /// Cold reset, soft reset and NMI, which ignore `BEV` and go to
    /// `0xBFC0_0000`.
    Reset,
}

/// `Status.EXL`.
const STATUS_EXL: u64 = 1 << 1;
/// `Status.ERL`.
const STATUS_ERL: u64 = 1 << 2;
/// `Status.DS.BEV`.
const STATUS_BEV: u64 = 1 << 22;

/// The exception vector address (UM Tables 6-3/6-4, p. 181).
///
/// # The `EXL` rule, which is where implementations diverge
///
/// A TLB or XTLB refill uses its special vector **only when `EXL` is 0**. With
/// `EXL` already set it takes the general vector at `0x180`. The manual says so
/// three times:
///
/// - Tables 6-3/6-4 label the refill rows `TLB Miss, EXL=0` and
///   `XTLB Miss, EXL=0`. There is no `EXL=1` refill row to select.
/// - §6.4.8 (p. 187): *"All TLB Miss exceptions use these two special vectors
///   when the EXL bit is set to 0 ... and they use the common exception vector
///   when the EXL bit is set to 1."*
/// - §6.4.8 (p. 188): *"This second exception goes to the common exception
///   vector because the EXL bit of the Status register is set."*
///
/// **UM Fig. 6-15 (p. 203) disagrees and is wrong.** Its `EXL = 0? → No` arm
/// leads to a box reading *"General Purpose Exception, Vec. Off. = 0x080"*,
/// contradicting both tables, the prose twice, and Fig. 6-14 — which is the
/// general-purpose handler and unconditionally uses `+ 0x180`. CEN64 routes to
/// `0x180` with a source comment that `0x080` *"doesn't make any sense"*; it is
/// right, and this is accuracy-ledger **S-3**.
///
/// Note also that UM p. 181's prose gives the `BEV = 1` general vector as
/// `0x8000_0180`, which is a typo: per Table 6-4 the `BEV = 1` base is
/// `0xBFC0_0200`, making the vector `0xBFC0_0380`. The 64-bit value in the same
/// sentence is correct and proves it.
#[must_use]
pub const fn vector(status: u64, kind: VectorKind) -> u64 {
    if matches!(kind, VectorKind::Reset) {
        return 0xFFFF_FFFF_BFC0_0000;
    }
    let bev = status & STATUS_BEV != 0;
    let exl = status & STATUS_EXL != 0;
    let base: u64 = if bev {
        0xFFFF_FFFF_BFC0_0200
    } else {
        0xFFFF_FFFF_8000_0000
    };
    // A refill with EXL already set is NOT a refill for vectoring purposes.
    let offset: u64 = match kind {
        VectorKind::TlbRefill if !exl => 0x000,
        VectorKind::XtlbRefill if !exl => 0x080,
        _ => 0x180,
    };
    base.wrapping_add(offset)
}

/// The `ExcCode` an exception reports.
#[must_use]
pub const fn exc_code_of(exc: Exception) -> u64 {
    match exc {
        Exception::Interrupt => exc_code::INT,
        // The direction matters: AdEL and AdES are different codes, and a
        // handler distinguishes them.
        Exception::AddressError { store: false } => exc_code::ADEL,
        Exception::AddressError { store: true } => exc_code::ADES,
        Exception::Overflow => exc_code::OV,
        Exception::Syscall => exc_code::SYS,
        Exception::Breakpoint => exc_code::BP,
        Exception::Trap => exc_code::TR,
        Exception::ReservedInstruction => exc_code::RI,
        // Refill and Invalid share an ExcCode -- the handler tells them apart by
        // which vector it was entered through, not by Cause.
        Exception::TlbRefill { store: false } | Exception::TlbInvalid { store: false } => {
            exc_code::TLBL
        }
        Exception::TlbRefill { store: true } | Exception::TlbInvalid { store: true } => {
            exc_code::TLBS
        }
        Exception::TlbModified => exc_code::MOD,
        Exception::CoprocessorUnusable { .. } => exc_code::CPU,
    }
}

/// Which vector kind an exception takes.
///
/// Every exception this crate can currently raise takes the general vector; the
/// refill kinds arrive with the TLB in T-12-004. Written as a function rather
/// than assumed, so that adding a TLB exception forces a decision here.
#[must_use]
pub const fn vector_kind_of(exc: Exception) -> VectorKind {
    match exc {
        Exception::Interrupt
        | Exception::AddressError { .. }
        | Exception::Overflow
        | Exception::Syscall
        | Exception::Breakpoint
        | Exception::Trap
        | Exception::ReservedInstruction
        // Invalid and Modified take the GENERAL vector: an entry was found, so
        // there is nothing for a refill handler to refill.
        | Exception::TlbInvalid { .. }
        | Exception::CoprocessorUnusable { .. }
        | Exception::TlbModified => VectorKind::General,
        // Only a genuine miss takes the refill vector, and only with EXL clear.
        Exception::TlbRefill { .. } => VectorKind::TlbRefill,
    }
}

/// Does this exception write `BadVAddr`?
///
/// **Only address errors and TLB exceptions.** UM §6.3.2 (p. 164) carries an
/// explicit Caution that a Bus Error does *not* write it, because a bus error is
/// not an address error — the address was fine, the transaction failed.
#[must_use]
pub const fn writes_bad_vaddr(exc: Exception) -> bool {
    matches!(
        exc,
        Exception::AddressError { .. }
            | Exception::TlbRefill { .. }
            | Exception::TlbInvalid { .. }
            | Exception::TlbModified
    )
}

/// Does this exception write `EntryHi` / `Context` / `XContext`?
///
/// **TLB exceptions only** (UM Fig. 6-14, p. 201, step 2) — the flowchart is
/// explicit that it *"is not set by bus error exceptions"*, and an address error
/// leaves them **undefined** (UM §6.4.7, p. 186) rather than merely unchanged.
#[must_use]
pub const fn writes_tlb_context(exc: Exception) -> bool {
    matches!(
        exc,
        Exception::TlbRefill { .. } | Exception::TlbInvalid { .. } | Exception::TlbModified
    )
}

/// The result of dispatching an exception.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Dispatch {
    /// Where to fetch next.
    pub vector: u64,
    /// `PCycle`s the pipeline stalls.
    ///
    /// **2**, and documented rather than fitted: *"When a pipeline exception
    /// condition occurs, the pipeline stalls for 2 `PCycles` and the instruction
    /// causing the exception as well as all those that follow it in the pipeline
    /// are aborted"* (UM §4.7, p. 114). Three files in this project once
    /// recorded this as undocumented; see `docs/engineering-lessons.md` §3.3b.
    pub stall_cycles: u32,
}

/// The documented stall on taking an exception (UM §4.7, p. 114).
pub const EPILOGUE_STALL: u32 = 2;

/// Perform the exception epilogue and return where to vector.
///
/// The order is the flowchart's (UM Fig. 6-14, p. 201), and it matters:
///
/// 1. `Cause.ExcCode` and `Cause.CE`.
/// 2. `BadVAddr` — address errors and TLB exceptions only.
/// 3. `EntryHi` / `Context` / `XContext` — TLB exceptions only (T-12-004).
/// 4. **If `EXL` was 0**: `Cause.BD` and `EPC`. Otherwise both are left alone.
/// 5. `EXL ← 1`.
/// 6. `PC ← vector`.
///
/// `pc` is the faulting instruction's address and `in_delay_slot` says whether
/// it sits in a branch delay slot; when it does, `EPC` gets **`pc - 4`** — the
/// branch, not the delay-slot instruction — because that is where a handler must
/// resume for the branch to be re-evaluated.
pub fn dispatch(
    cop0: &mut Cop0,
    exc: Exception,
    pc: u64,
    in_delay_slot: bool,
    bad_vaddr: u64,
) -> Dispatch {
    let status = cop0.read(reg::STATUS);
    let exl_was_set = status & STATUS_EXL != 0;

    // 1. Cause.ExcCode. Written through `set_hardware` because ExcCode is
    //    read-only to software -- routing it through `write` would need the
    //    Cause write mask widened, which would also let MTC0 forge it.
    let cause = cop0.read(reg::CAUSE);
    let mut new_cause = (cause & !0x7C) | (exc_code_of(exc) << 2);
    // `Cause.CE` (29:28) names the coprocessor for a Coprocessor Unusable
    // exception, and is meaningless otherwise -- so it is written only here
    // rather than cleared unconditionally, which would erase a previous value
    // the handler has not read yet.
    if let Exception::CoprocessorUnusable { unit } = exc {
        new_cause = (new_cause & !0x3000_0000) | ((u64::from(unit) & 0b11) << 28);
    }

    // 4. EPC and Cause.BD, ONLY if EXL was clear. This is the gate; see the
    //    module docs. Note it also governs BD, not just EPC -- a stale BD with a
    //    fresh ExcCode would misreport the *first* exception's delay-slot state.
    if exl_was_set {
        // Deliberately nothing. The first exception's EPC is what the handler
        // will eventually return to, and it must survive.
    } else {
        new_cause = if in_delay_slot {
            new_cause | (1 << 31)
        } else {
            new_cause & !(1 << 31)
        };
        let epc = if in_delay_slot {
            pc.wrapping_sub(4)
        } else {
            pc
        };
        cop0.set_hardware(reg::EPC, epc);
    }
    cop0.set_hardware(reg::CAUSE, new_cause);

    // 2. BadVAddr, for the exceptions that define it.
    if writes_bad_vaddr(exc) {
        cop0.set_hardware(reg::BAD_VADDR, bad_vaddr);
    }

    // 3. EntryHi / Context / XContext -- TLB exceptions only. The refill handler
    //    reads `Context` as a ready-made page-table pointer, which is the whole
    //    reason the hardware assembles it here rather than leaving it to
    //    software.
    if writes_tlb_context(exc) {
        let vpn2 = bad_vaddr & crate::tlb::VPN2_MASK;
        let hi = cop0.read(reg::ENTRY_HI);
        // The `R` field (63:62) comes from the faulting address too, not just
        // `VPN2`. Leaving it zero puts every sign-extended kernel fault in
        // region 0, so the handler's `TLBWR` would install an entry that can
        // never match the address that faulted.
        let region = bad_vaddr & 0xC000_0000_0000_0000;
        // ASID is preserved; VPN2 and R are replaced.
        cop0.set_hardware(reg::ENTRY_HI, (hi & crate::tlb::ASID_MASK) | vpn2 | region);
        // Context: PTEBase (63:23) kept, BadVPN2 (22:4) = VA(31:13).
        let ctx = cop0.read(reg::CONTEXT);
        cop0.set_hardware(
            reg::CONTEXT,
            (ctx & 0xFFFF_FFFF_FF80_0000) | ((bad_vaddr >> 13) & 0x7_FFFF) << 4,
        );
        // XContext: PTEBase (63:33) kept, R (32:31) = VA(63:62),
        // BadVPN2 (30:4) = VA(39:13).
        let xctx = cop0.read(reg::XCONTEXT);
        cop0.set_hardware(
            reg::XCONTEXT,
            (xctx & 0xFFFF_FFFE_0000_0000)
                | (((bad_vaddr >> 62) & 0b11) << 31)
                | (((bad_vaddr >> 13) & 0x7FF_FFFF) << 4),
        );
    }

    // 5. EXL. Setting it puts the CPU in Kernel mode with interrupts disabled,
    //    which is what makes the handler's own first instructions safe.
    cop0.set_hardware(reg::STATUS, status | STATUS_EXL);

    Dispatch {
        vector: vector(status, vector_kind_of(exc)),
        stall_cycles: EPILOGUE_STALL,
    }
}

/// `ERET` — return from an exception (UM Ch. 16, p. 434).
///
/// Returns where to resume. Three rules, all easy to half-implement:
///
/// - If `Status.ERL` is set, resume at `ErrorEPC` and clear **`ERL`**; otherwise
///   resume at `EPC` and clear **`EXL`**. Clearing the wrong one leaves the CPU
///   stuck in kernel mode or returns to the wrong address.
/// - **`LLbit` is always cleared**, which is the *only* thing besides cache
///   invalidation that clears it (UM §3.1) — the other half of the `LL`/`SC`
///   contract implemented in Sprint 1.
/// - `ERET` has **no delay slot** and must not itself be placed in one.
///
/// The caller clears the link bit; this function reports that it must.
#[must_use]
pub fn eret(cop0: &mut Cop0) -> u64 {
    let status = cop0.read(reg::STATUS);
    if status & STATUS_ERL != 0 {
        cop0.set_hardware(reg::STATUS, status & !STATUS_ERL);
        cop0.read(reg::ERROR_EPC)
    } else {
        cop0.set_hardware(reg::STATUS, status & !STATUS_EXL);
        cop0.read(reg::EPC)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Baseline vectoring, both `BEV` values.
    #[test]
    fn the_vector_table_matches_the_manual() {
        let bev0 = 0;
        let bev1 = STATUS_BEV;
        assert_eq!(vector(bev0, VectorKind::General), 0xFFFF_FFFF_8000_0180);
        assert_eq!(vector(bev0, VectorKind::TlbRefill), 0xFFFF_FFFF_8000_0000);
        assert_eq!(vector(bev0, VectorKind::XtlbRefill), 0xFFFF_FFFF_8000_0080);
        assert_eq!(
            vector(bev1, VectorKind::General),
            0xFFFF_FFFF_BFC0_0380,
            "UM p.181's prose says 0x8000_0180 here and is a typo; Table 6-4 wins"
        );
        assert_eq!(vector(bev1, VectorKind::TlbRefill), 0xFFFF_FFFF_BFC0_0200);
        assert_eq!(vector(bev1, VectorKind::XtlbRefill), 0xFFFF_FFFF_BFC0_0280);
        assert_eq!(
            vector(bev1, VectorKind::Reset),
            0xFFFF_FFFF_BFC0_0000,
            "reset ignores BEV"
        );
    }

    /// Accuracy-ledger **S-3**. A refill arriving with `EXL` already set takes
    /// the **general** vector, not `0x080`.
    ///
    /// UM Fig. 6-15 (p. 203) says `0x080` and is wrong; Tables 6-3/6-4 and
    /// §6.4.8 (twice) say otherwise, and Fig. 6-14 unconditionally uses `0x180`.
    /// This test exists so nobody "fixes" it back to the figure.
    #[test]
    fn a_refill_with_exl_already_set_uses_the_general_vector() {
        let exl = STATUS_EXL;
        assert_eq!(
            vector(exl, VectorKind::TlbRefill),
            0xFFFF_FFFF_8000_0180,
            "ledger S-3: NOT 0x8000_0000"
        );
        assert_eq!(
            vector(exl, VectorKind::XtlbRefill),
            0xFFFF_FFFF_8000_0180,
            "ledger S-3: NOT 0x8000_0080 -- this is the value UM Fig. 6-15 gives"
        );
        // And with BEV too, since the two rules compose.
        assert_eq!(
            vector(exl | STATUS_BEV, VectorKind::XtlbRefill),
            0xFFFF_FFFF_BFC0_0380
        );
    }

    /// The ordinary epilogue: `EPC` gets the faulting PC, `BD` is clear, `EXL`
    /// is set, and the `ExcCode` is right.
    #[test]
    fn a_first_exception_records_epc_and_sets_exl() {
        let mut c = Cop0::new();
        c.set_hardware(reg::STATUS, 0);
        let d = dispatch(&mut c, Exception::Overflow, 0x8000_1000, false, 0);
        assert_eq!(c.read(reg::EPC), 0x8000_1000);
        assert_eq!((c.read(reg::CAUSE) >> 2) & 0x1F, exc_code::OV);
        assert_eq!(c.read(reg::CAUSE) & (1 << 31), 0, "BD clear");
        assert_ne!(c.read(reg::STATUS) & STATUS_EXL, 0, "EXL set");
        assert_eq!(d.vector, 0xFFFF_FFFF_8000_0180);
        assert_eq!(d.stall_cycles, 2, "UM §4.7 p.114");
    }

    /// In a delay slot, `EPC` points at the **branch** and `BD` is set.
    #[test]
    fn a_delay_slot_exception_reports_the_branch_address() {
        let mut c = Cop0::new();
        c.set_hardware(reg::STATUS, 0);
        dispatch(&mut c, Exception::Overflow, 0x8000_1004, true, 0);
        assert_eq!(
            c.read(reg::EPC),
            0x8000_1000,
            "EPC = pc - 4, the branch itself"
        );
        assert_ne!(c.read(reg::CAUSE) & (1 << 31), 0, "BD set");
    }

    /// **The nested case, and the one that separates a correct epilogue from one
    /// that merely passes.** With `EXL` already set, `EPC` and `BD` must not be
    /// touched — otherwise the first exception's return address is destroyed.
    ///
    /// An implementation that always writes `EPC` passes every other test here.
    #[test]
    fn a_nested_exception_does_not_overwrite_epc_or_bd() {
        let mut c = Cop0::new();
        c.set_hardware(reg::STATUS, 0);

        // First exception, in a delay slot: EPC = branch, BD = 1.
        dispatch(&mut c, Exception::Overflow, 0x8000_1004, true, 0);
        assert_eq!(c.read(reg::EPC), 0x8000_1000);
        assert_ne!(c.read(reg::CAUSE) & (1 << 31), 0);

        // Second exception while EXL is still set, NOT in a delay slot and at a
        // completely different address. Both EPC and BD must survive.
        dispatch(
            &mut c,
            Exception::AddressError { store: true },
            0x8000_9999,
            false,
            0xDEAD,
        );
        assert_eq!(
            c.read(reg::EPC),
            0x8000_1000,
            "the FIRST exception's EPC must survive (UM §6.3.7)"
        );
        assert_ne!(
            c.read(reg::CAUSE) & (1 << 31),
            0,
            "and so must its BD -- a stale ExcCode with a fresh BD misreports it"
        );
        // ExcCode and BadVAddr, by contrast, DO update: they describe the
        // current exception, not the return path.
        assert_eq!((c.read(reg::CAUSE) >> 2) & 0x1F, exc_code::ADES);
        assert_eq!(c.read(reg::BAD_VADDR), 0xDEAD);
    }

    /// **Refill and Invalid share an `ExcCode` and differ only in vector.** The
    /// handler tells them apart by which entry point it was reached through, so
    /// getting the vector wrong sends a page-protection fault to the refill
    /// handler — which would refill a mapping that already exists.
    ///
    /// This is the one distinction `Cause` cannot express, so nothing but the
    /// vector check can catch it.
    #[test]
    fn refill_and_invalid_share_an_exccode_but_not_a_vector() {
        for store in [false, true] {
            let refill = Exception::TlbRefill { store };
            let invalid = Exception::TlbInvalid { store };
            assert_eq!(
                exc_code_of(refill),
                exc_code_of(invalid),
                "the ExcCode cannot distinguish them"
            );

            assert_eq!(vector_kind_of(refill), VectorKind::TlbRefill);
            assert_eq!(
                vector_kind_of(invalid),
                VectorKind::General,
                "an entry WAS found, so there is nothing to refill"
            );
            assert_eq!(vector(0, refill_kind(store)), 0xFFFF_FFFF_8000_0000);
            assert_eq!(
                vector(0, vector_kind_of(invalid)),
                0xFFFF_FFFF_8000_0180,
                "Invalid must NOT reach the refill vector"
            );
        }
        assert_eq!(
            vector_kind_of(Exception::TlbModified),
            VectorKind::General,
            "Modified likewise -- the mapping exists, it is just not writable"
        );
    }

    /// Helper for the test above: the refill kind, independent of direction.
    const fn refill_kind(store: bool) -> VectorKind {
        vector_kind_of(Exception::TlbRefill { store })
    }

    /// `AdEL` and `AdES` are different codes; conflating them loses information
    /// a handler uses.
    #[test]
    fn address_error_reports_its_direction() {
        assert_eq!(
            exc_code_of(Exception::AddressError { store: false }),
            exc_code::ADEL
        );
        assert_eq!(
            exc_code_of(Exception::AddressError { store: true }),
            exc_code::ADES
        );
        assert_ne!(exc_code::ADEL, exc_code::ADES);
    }

    /// `BadVAddr` is written for address errors and **not** for anything that is
    /// not an addressing failure (UM §6.3.2 p. 164 Caution).
    #[test]
    fn bad_vaddr_is_written_only_for_addressing_failures() {
        let mut c = Cop0::new();
        c.set_hardware(reg::STATUS, 0);
        dispatch(&mut c, Exception::Syscall, 0x8000_1000, false, 0x1234);
        assert_eq!(
            c.read(reg::BAD_VADDR),
            0,
            "SYSCALL is not an addressing failure"
        );

        let mut c = Cop0::new();
        c.set_hardware(reg::STATUS, 0);
        dispatch(
            &mut c,
            Exception::AddressError { store: false },
            0x8000_1000,
            false,
            0x1234,
        );
        assert_eq!(c.read(reg::BAD_VADDR), 0x1234);
    }

    /// `ERET` clears `EXL` and returns to `EPC` in the ordinary case.
    #[test]
    fn eret_returns_to_epc_and_clears_exl() {
        let mut c = Cop0::new();
        c.set_hardware(reg::STATUS, STATUS_EXL);
        c.set_hardware(reg::EPC, 0x8000_2000);
        assert_eq!(eret(&mut c), 0x8000_2000);
        assert_eq!(c.read(reg::STATUS) & STATUS_EXL, 0, "EXL cleared");
    }

    /// With `ERL` set, `ERET` uses `ErrorEPC` and clears **`ERL`**, leaving
    /// `EXL` alone. Clearing the wrong bit is the classic version of this bug.
    #[test]
    fn eret_prefers_error_epc_when_erl_is_set() {
        let mut c = Cop0::new();
        c.set_hardware(reg::STATUS, STATUS_ERL | STATUS_EXL);
        c.set_hardware(reg::EPC, 0x8000_2000);
        c.set_hardware(reg::ERROR_EPC, 0x8000_3000);
        assert_eq!(eret(&mut c), 0x8000_3000, "ErrorEPC wins when ERL is set");
        let s = c.read(reg::STATUS);
        assert_eq!(s & STATUS_ERL, 0, "ERL cleared");
        assert_ne!(s & STATUS_EXL, 0, "EXL untouched -- clearing it is the bug");
    }

    /// Cold reset leaves `ERL` set (UM §6.4.4), so an `ERET` immediately after
    /// reset takes the `ErrorEPC` path. Worth pinning because it is the state
    /// the machine actually boots in.
    #[test]
    fn a_freshly_reset_cop0_erets_through_error_epc() {
        let mut c = Cop0::new();
        assert_ne!(c.read(reg::STATUS) & STATUS_ERL, 0, "reset sets ERL");
        c.set_hardware(reg::ERROR_EPC, 0xBFC0_0000);
        assert_eq!(eret(&mut c), 0xBFC0_0000);
    }
}
