//! COP1 **control** registers (T-12-006).
//!
//! `CTC1` / `CFC1` on `FCR31` (the FCSR) and `FCR0` (the revision register), and
//! nothing else. **FPU arithmetic is Sprint 3** — this module exists for one
//! reason, stated plainly so it does not quietly grow:
//!
//! n64-systemtest's `entrypoint()` calls `set_fcsr(...)` — which is
//! `ctc1::<31>` — as its **fourth statement**
//! (`ref-proj/n64-systemtest/src/main.rs`). Without COP1 control the suite dies
//! three statements after entry and reports nothing at all, so every COP0 and
//! TLB test in Sprint 2 is unreachable behind it.
//!
//! # Scope discipline
//!
//! FCSR needs *storage* with correct bit semantics, not *behaviour*: nothing
//! acts on the rounding mode or the enable bits until COP1 arithmetic lands.
//! Adding an arithmetic path here would make this ticket Sprint 3 by stealth.

/// `FCR0` — the FPU implementation/revision register.
///
/// Read-only. `Imp` (bits 15:8) is **`0x0A`**, and the revision half is `0x00`.
///
/// # `Imp` is NOT the same as `PRId`'s
///
/// This was `0x0B00` on the reasoning that the FPU's implementation number
/// matches the CPU's — and the N64brew Wiki says so outright: *"All VR4300
/// units will report 0x0B (11) for the implementation number"*
/// (`n64brew_wiki/markdown/VR4300.md`). That is **wrong**, and it is wrong in
/// this project's designated primary hardware reference, which is worth knowing
/// before trusting the wiki on a single-value claim.
///
/// Two independent sources give `0x0A00`:
///
/// - n64-systemtest asserts it directly, and it runs on real hardware.
/// - cen64 hardcodes `0xa00` with the comment *"fpu version of both 0xb22 and
///   0xb10 N64s"* — i.e. checked against two console revisions.
///
/// `PRId.Imp` really is `0x0B`; the two registers identify different units, and
/// the near-identical values are what makes the conflation easy. Accuracy
/// ledger **S-4**.
pub const FCR0_REVISION: u32 = 0x0A00;

/// The writable bits of `FCR31` (FCSR).
///
/// Bits 25 and 22..=18 are unused on the VR4300 and read zero. Everything else
/// is software-writable, including the `Cause` bits — software clears them by
/// writing, which is how an FP exception handler acknowledges.
///
/// | Bits | Field |
/// | --- | --- |
/// | 24 | `FS` — flush denormals to zero |
/// | 23 | `C` — condition |
/// | 17..=12 | `Cause` (unimplemented, invalid, div0, overflow, underflow, inexact) |
/// | 11..=7 | `Enable` |
/// | 6..=2 | `Flags` |
/// | 1..=0 | `RM` — rounding mode |
pub const FCSR_MASK: u32 = 0x0183_FFFF;

/// COP1 control-register state.
///
/// Deliberately **only** the control registers: the 32 floating-point data
/// registers arrive with the arithmetic in Sprint 3, and putting them here now
/// would be state nothing reads.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Cop1Control {
    /// `FCR31`, the Floating-point Control/Status Register.
    fcsr: u32,
}

impl Cop1Control {
    /// Power-on state: all zero.
    ///
    /// The manual does not define a reset value for `FCSR`; ADR 0004 requires
    /// reproducibility, so it is a documented zero. Software sets it up — which
    /// is exactly what n64-systemtest does on its fourth instruction.
    #[must_use]
    pub const fn new() -> Self {
        Self { fcsr: 0 }
    }

    /// `CFC1 rt, fs` — read a control register.
    ///
    /// Only `FCR0` and `FCR31` exist. Every other `fs` reads **zero**, which is
    /// a choice rather than a documented fact — the manual does not say — and is
    /// recorded as such.
    #[must_use]
    pub const fn cfc1(&self, fs: u8) -> u32 {
        match fs {
            0 => FCR0_REVISION,
            31 => self.fcsr,
            _ => 0,
        }
    }

    /// `CTC1 rt, fs` — write a control register.
    ///
    /// `FCR0` is read-only, so a write to it is discarded rather than stored.
    pub const fn ctc1(&mut self, fs: u8, value: u32) {
        if fs == 31 {
            self.fcsr = value & FCSR_MASK;
        }
    }

    /// The raw `FCSR` value, for the arithmetic path in Sprint 3.
    #[must_use]
    pub const fn fcsr(&self) -> u32 {
        self.fcsr
    }

    /// `FS` — flush denormals to zero (bit 24).
    ///
    /// Read by nothing yet. Exposed because n64-systemtest **sets** it during
    /// startup, so an implementation that silently dropped the bit would look
    /// fine until the first denormal.
    #[must_use]
    pub const fn flush_denorm_to_zero(&self) -> bool {
        self.fcsr & (1 << 24) != 0
    }

    /// `RM` — the rounding mode (bits 1..=0).
    #[must_use]
    pub const fn rounding_mode(&self) -> u8 {
        (self.fcsr & 0b11) as u8
    }

    /// The `Enable` field (bits 11..=7).
    #[must_use]
    pub const fn enables(&self) -> u32 {
        (self.fcsr >> 7) & 0x1F
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact sequence n64-systemtest performs on its fourth statement:
    /// `FCSR::new().with_flush_denorm_to_zero(true).with_enable_invalid_operation(true)`.
    ///
    /// If this does not round-trip, the suite cannot start and Sprint 2 has no
    /// oracle at all.
    #[test]
    fn the_n64_systemtest_startup_fcsr_round_trips() {
        let mut c = Cop1Control::new();
        // bit 24 = flush_denorm_to_zero, bit 11 = enable_invalid_operation.
        let want = (1 << 24) | (1 << 11);
        c.ctc1(31, want);
        assert_eq!(c.cfc1(31), want, "CTC1 then CFC1 must round-trip");
        assert!(c.flush_denorm_to_zero());
        assert_eq!(c.enables(), 1 << 4, "enable_invalid_operation");
    }

    /// `FCR0` is read-only and reports the VR4300 implementation number.
    #[test]
    fn fcr0_is_read_only_and_reports_the_implementation() {
        let mut c = Cop1Control::new();
        assert_eq!(c.cfc1(0), FCR0_REVISION);
        assert_eq!(
            (c.cfc1(0) >> 8) & 0xFF,
            0x0A,
            "FCR0.Imp is 0x0A -- NOT PRId's 0x0B, and not the 0x0B the wiki claims"
        );
        c.ctc1(0, 0xFFFF_FFFF);
        assert_eq!(c.cfc1(0), FCR0_REVISION, "writes are discarded");
    }

    /// The unused bits read zero rather than storing what was written.
    #[test]
    fn the_unused_fcsr_bits_read_zero() {
        let mut c = Cop1Control::new();
        c.ctc1(31, 0xFFFF_FFFF);
        let v = c.cfc1(31);
        assert_eq!(v & (1 << 25), 0, "bit 25 is unused");
        assert_eq!(v & 0x007C_0000, 0, "bits 22..=18 are unused");
        assert_eq!(v, FCSR_MASK, "everything else is writable");
    }

    /// The `Cause` bits are software-writable — that is how a handler
    /// acknowledges an FP exception. Making them read-only looks defensive and
    /// leaves the handler unable to clear them.
    #[test]
    fn the_cause_bits_are_software_writable() {
        let mut c = Cop1Control::new();
        c.ctc1(31, 0x0003_F000);
        assert_eq!(c.cfc1(31), 0x0003_F000, "all six Cause bits took");
        c.ctc1(31, 0);
        assert_eq!(c.cfc1(31), 0, "and can be cleared again");
    }

    /// Rounding mode is stored, though nothing acts on it until Sprint 3.
    #[test]
    fn the_rounding_mode_is_stored_even_though_nothing_reads_it_yet() {
        let mut c = Cop1Control::new();
        for rm in 0..4u32 {
            c.ctc1(31, rm);
            assert_eq!(u32::from(c.rounding_mode()), rm);
        }
    }
}
