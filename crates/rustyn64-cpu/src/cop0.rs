//! COP0 — the VR4300 system control coprocessor (T-12-001).
//!
//! The register file only: widths, writable-bit masks, and the four access
//! instructions. The *behaviour* the registers drive — exception dispatch
//! (T-12-002), interrupts (T-12-003), address translation (T-12-004) — reads
//! this module rather than living in it.
//!
//! # Why this is table-driven
//!
//! Almost every accuracy rule here is of the form "register N is 64 bits wide"
//! or "bits 23:16 of `Config` are hardwired to `0b00000110`". Those are *data*.
//! Written as data they can be asserted directly against the manual, one test
//! per table; written as `match` arms they become 32 places to forget one.
//!
//! Two rules in particular cannot be derived and must simply be right:
//!
//! - **Which eight registers are 64 bits wide** ([`WIDE`]). There is no pattern.
//!   Getting one wrong is invisible until 64-bit software runs, and then
//!   presents as a truncated address rather than as a register bug.
//! - **Which bits accept writes** ([`WRITE_MASK`]). Hardware silently discards
//!   the rest; software reads back what it wrote *minus those bits*, and a
//!   too-permissive mask makes an emulator pass tests real hardware fails.
//!
//! # Reset state
//!
//! UM §6.4.4 (p. 183) defines only a handful of fields at cold reset and calls
//! the rest **undefined**: `Index`, `EntryHi`/`EntryLo*`/`PageMask`, `LLAddr`,
//! `TagLo`/`TagHi`, `WatchLo`/`WatchHi`, and most of `Status`. Undefined is not
//! licence to be non-deterministic — ADR 0004 requires a reproducible machine —
//! so undefined fields are a documented zero, not entropy.

use crate::decode::Decoded;

/// COP0 register numbers, by name.
///
/// Named constants rather than an enum: the encoding carries a raw 5-bit field
/// and every value 0..=31 is expressible, so an enum would need a fallible
/// conversion on the hot path for no benefit.
pub mod reg {
    /// TLB entry index for `TLBR`/`TLBWI`.
    pub const INDEX: u8 = 0;
    /// Free-running TLB replacement counter.
    pub const RANDOM: u8 = 1;
    /// Even-page TLB entry.
    pub const ENTRY_LO0: u8 = 2;
    /// Odd-page TLB entry.
    pub const ENTRY_LO1: u8 = 3;
    /// Page-table base plus the faulting VPN2 (32-bit refill).
    pub const CONTEXT: u8 = 4;
    /// TLB page-size mask.
    pub const PAGE_MASK: u8 = 5;
    /// Number of wired (unreplaceable) TLB entries.
    pub const WIRED: u8 = 6;
    /// Faulting virtual address.
    pub const BAD_VADDR: u8 = 8;
    /// Free-running cycle counter, incrementing at half `PClock`.
    pub const COUNT: u8 = 9;
    /// TLB VPN2 + ASID.
    pub const ENTRY_HI: u8 = 10;
    /// Timer-interrupt comparand.
    pub const COMPARE: u8 = 11;
    /// Processor status and mode.
    pub const STATUS: u8 = 12;
    /// Exception cause.
    pub const CAUSE: u8 = 13;
    /// Exception program counter.
    pub const EPC: u8 = 14;
    /// Processor revision identifier.
    pub const PRID: u8 = 15;
    /// Cache and endianness configuration.
    pub const CONFIG: u8 = 16;
    /// Physical address of the last `LL` (diagnostic only).
    pub const LL_ADDR: u8 = 17;
    /// Watchpoint address, low.
    pub const WATCH_LO: u8 = 18;
    /// Watchpoint address, high.
    pub const WATCH_HI: u8 = 19;
    /// Page-table base plus the faulting VPN2 (64-bit refill).
    pub const XCONTEXT: u8 = 20;
    /// Parity error (VR4200 compatibility; unused by VR4300 hardware).
    pub const PERR: u8 = 26;
    /// Cache error (VR4200 compatibility; unused by VR4300 hardware).
    pub const CACHE_ERR: u8 = 27;
    /// Primary cache tag, low.
    pub const TAG_LO: u8 = 28;
    /// Primary cache tag, high.
    pub const TAG_HI: u8 = 29;
    /// Error exception program counter.
    pub const ERROR_EPC: u8 = 30;
}

/// The eight registers that are **64 bits wide**; the other 24 are 32-bit.
///
/// A bitmask indexed by register number. From UM Table 1-2 (p. 46) and the
/// individual register figures in §5.4 and §6.3:
/// `EntryLo0`, `EntryLo1`, `Context`, `BadVAddr`, `EntryHi`, `EPC`, `XContext`,
/// `ErrorEPC`.
///
/// There is no rule generating this list — it is exactly the registers that hold
/// an address or a TLB entry. Pinned by `the_eight_wide_registers_are_exactly_these`.
pub const WIDE: u32 = (1 << reg::ENTRY_LO0)
    | (1 << reg::ENTRY_LO1)
    | (1 << reg::CONTEXT)
    | (1 << reg::BAD_VADDR)
    | (1 << reg::ENTRY_HI)
    | (1 << reg::EPC)
    | (1 << reg::XCONTEXT)
    | (1 << reg::ERROR_EPC);

/// Is register `n` 64 bits wide?
#[must_use]
pub const fn is_wide(n: u8) -> bool {
    WIDE & (1 << (n & 31)) != 0
}

/// `Config` bits 23:16, hardwired to `0b00000110` (UM Fig. 5-16, p. 152).
const CONFIG_HARDWIRED_HI: u64 = 0b0000_0110 << 16;
/// `Config` bits 14:4, hardwired to `0b110_0100_0110` (UM Fig. 5-16, p. 152).
const CONFIG_HARDWIRED_LO: u64 = 0b110_0100_0110 << 4;

/// Per-register writable-bit masks. A `0` bit is hardwired, reserved, or
/// hardware-owned, and a write to it is discarded.
///
/// Every entry is a direct transcription of a register figure. Where a register
/// is entirely read-only the mask is `0`, which makes "read-only" the same
/// mechanism as "reserved bit" rather than a separate code path.
pub const WRITE_MASK: [u64; 32] = {
    let mut m = [0u64; 32];
    // Index: P (31) + Index (5:0). Bit 5 is writable but ignored by a 32-entry
    // TLB -- the manual keeps it, so we keep it (UM Fig. 5-11, p. 146).
    m[reg::INDEX as usize] = 0x8000_003F;
    // Random is READ-ONLY (UM §5.4.2, p. 147).
    m[reg::RANDOM as usize] = 0;
    // EntryLo0/1: PFN (25:6) | C (5:3) | D (2) | V (1) | G (0).
    m[reg::ENTRY_LO0 as usize] = 0x03FF_FFFF;
    m[reg::ENTRY_LO1 as usize] = 0x03FF_FFFF;
    // Context: only PTEBase (63:23) is software-writable; BadVPN2 (22:4) is
    // written by hardware on a TLB exception and 3:0 are always zero.
    m[reg::CONTEXT as usize] = 0xFFFF_FFFF_FF80_0000;
    // PageMask: MASK (24:13).
    m[reg::PAGE_MASK as usize] = 0x01FF_E000;
    m[reg::WIRED as usize] = 0x3F;
    // BadVAddr is READ-ONLY (UM §6.3.2, p. 164).
    m[reg::BAD_VADDR as usize] = 0;
    m[reg::COUNT as usize] = 0xFFFF_FFFF;
    // EntryHi: R (63:62) | VPN2 (39:13) | ASID (7:0). Fill (61:40) is
    // write-ignored and reads zero (UM Fig. 5-10, p. 144).
    m[reg::ENTRY_HI as usize] = 0xC000_00FF_FFFF_E0FF;
    m[reg::COMPARE as usize] = 0xFFFF_FFFF;
    // Status: everything except DS.TS (21), which is read-only, and bits 23 and
    // 19, which are hardwired zero (UM Fig. 6-6, p. 167).
    m[reg::STATUS as usize] = 0xFFFF_FFFF & !(1 << 21) & !(1 << 23) & !(1 << 19);
    // Cause: READ-ONLY except the two software interrupt bits IP1:IP0 (9:8)
    // (UM §6.3.6, p. 171). This is the mask that is most tempting to widen.
    m[reg::CAUSE as usize] = 0x0000_0300;
    m[reg::EPC as usize] = u64::MAX;
    // PRId is READ-ONLY.
    m[reg::PRID as usize] = 0;
    // Config: EP (27:24) | BE (15) | CU (3) | K0 (2:0). EC (30:28) is read-only,
    // sampled from the `DivMode` pins.
    m[reg::CONFIG as usize] = 0x0F00_0000 | (1 << 15) | (1 << 3) | 0b111;
    m[reg::LL_ADDR as usize] = 0xFFFF_FFFF;
    // WatchLo: PAddr0 (31:3) | R (1) | W (0). Bit 2 is zero.
    m[reg::WATCH_LO as usize] = 0xFFFF_FFFB;
    // WatchHi: PAddr1 (3:0). Readable, but "the value in this area is invalid"
    // on the VR4300, whose physical addresses are only 32 bits.
    m[reg::WATCH_HI as usize] = 0xF;
    // XContext: only PTEBase (63:33). R (32:31) and BadVPN2 (30:4) are
    // hardware-written.
    m[reg::XCONTEXT as usize] = 0xFFFF_FFFE_0000_0000;
    // PErr: Diagnostic (7:0). Defined for VR4200 compatibility; the VR4300's
    // hardware never uses it.
    m[reg::PERR as usize] = 0xFF;
    // CacheErr is READ-ONLY and reads as zero -- same compatibility story.
    m[reg::CACHE_ERR as usize] = 0;
    // TagLo: PTagLo (27:8) | PState (7:6).
    m[reg::TAG_LO as usize] = 0x0FFF_FFC0;
    // TagHi is 32 bits of reserved zero.
    m[reg::TAG_HI as usize] = 0;
    m[reg::ERROR_EPC as usize] = u64::MAX;
    // Registers 7, 21..=25 and 31 are "Reserved for future use" (UM Table 1-2,
    // p. 46) and stay 0 -- see `Cop0::write` for why that is a guess, not a fact.
    m
};

/// Per-register **architecturally-defined** bits — everything a read can ever
/// return non-zero.
///
/// Distinct from [`WRITE_MASK`], and the difference is the point: a read-only
/// register like `BadVAddr` has a write mask of `0` and an arch mask of all-ones,
/// while `Cause` has a two-bit write mask and a wide arch mask. Reserved bits and
/// bits above a 32-bit register's width appear in neither.
///
/// Applied on **both** read and [`Cop0::set_hardware`], so a value that is not
/// architecturally representable cannot enter the file in the first place, let
/// alone leave it. Enforcing only on read would leave the stored state carrying
/// bits no hardware register can hold, which the next reader of `regs` would
/// have to know about.
///
/// `Config` is absent from this table: its readable value is *composed* rather
/// than masked, and [`Cop0::read`] handles it separately.
pub const ARCH_MASK: [u64; 32] = {
    let mut m = [0u64; 32];
    m[reg::INDEX as usize] = 0x8000_003F;
    m[reg::RANDOM as usize] = 0x3F;
    m[reg::ENTRY_LO0 as usize] = 0x03FF_FFFF;
    m[reg::ENTRY_LO1 as usize] = 0x03FF_FFFF;
    // PTEBase (63:23) | BadVPN2 (22:4); bits 3:0 are always zero.
    m[reg::CONTEXT as usize] = 0xFFFF_FFFF_FFFF_FFF0;
    m[reg::PAGE_MASK as usize] = 0x01FF_E000;
    m[reg::WIRED as usize] = 0x3F;
    // A full 64-bit virtual address.
    m[reg::BAD_VADDR as usize] = u64::MAX;
    m[reg::COUNT as usize] = 0xFFFF_FFFF;
    // R (63:62) | VPN2 (39:13) | ASID (7:0). Fill (61:40) reads ZERO, which is
    // why it is absent here rather than merely unwritable.
    m[reg::ENTRY_HI as usize] = 0xC000_00FF_FFFF_E0FF;
    m[reg::COMPARE as usize] = 0xFFFF_FFFF;
    m[reg::STATUS as usize] = 0xFFFF_FFFF & !(1 << 23) & !(1 << 19);
    // BD (31) | CE (29:28) | IP (15:8) | ExcCode (6:2). Far wider than the
    // two-bit WRITE_MASK, because hardware writes most of it.
    m[reg::CAUSE as usize] = 0xB000_FF7C;
    m[reg::EPC as usize] = u64::MAX;
    // Imp (15:8) | Rev (7:0); bits 31:16 read zero.
    m[reg::PRID as usize] = 0xFFFF;
    m[reg::LL_ADDR as usize] = 0xFFFF_FFFF;
    m[reg::WATCH_LO as usize] = 0xFFFF_FFFB;
    m[reg::WATCH_HI as usize] = 0xF;
    // PTEBase (63:33) | R (32:31) | BadVPN2 (30:4); bits 3:0 always zero.
    m[reg::XCONTEXT as usize] = 0xFFFF_FFFF_FFFF_FFF0;
    // Config's READ value is composed rather than masked (see `Cop0::read`), but
    // it still needs an entry: without one, `set_hardware` would mask it to zero.
    // These are its non-hardwired bits: EP (27:24) | BE (15) | CU (3) | K0 (2:0).
    m[reg::CONFIG as usize] = 0x0F00_800F;
    m[reg::PERR as usize] = 0xFF;
    // CacheErr, TagHi and the reserved registers read as zero.
    m[reg::TAG_LO as usize] = 0x0FFF_FFC0;
    m[reg::ERROR_EPC as usize] = u64::MAX;
    m
};

/// The VR4300 system control coprocessor register file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cop0 {
    /// Raw register storage. Prefer [`Cop0::read`] / [`Cop0::write`]: they apply
    /// the width and writable-bit rules that make the values architectural.
    ///
    /// `Config`'s hardwired fields are **not** stored here — they are merged in
    /// on read, so no write path can erase them.
    regs: [u64; 32],
    /// The current `Count` timeline position, supplied by the scheduler.
    ///
    /// ADR 0006: `master_ticks` is the only incremented counter, and every other
    /// cycle position is **derived** from it. `Count` is therefore not stored
    /// and not incremented here — it is computed from this timeline plus an
    /// epoch, so that a `+= 1` on `Count` never exists to drift.
    now: u64,
    /// `Count`'s value at its epoch, set by the last `MTC0 Count`.
    count_epoch_value: u32,
    /// The timeline position at that epoch.
    ///
    /// Together these make `Count` **affine**: guest-writable (so it needs an
    /// offset) while still derived (so it cannot drift from the master clock).
    count_epoch_tick: u64,
    /// `Config.EC`, the PClock:MasterClock ratio, sampled from the `DivMode` pins
    /// at reset and read-only thereafter.
    ///
    /// Held apart from `regs` precisely because it is not writable: keeping it
    /// in the array would need a mask exception that a later edit could relax.
    ec: u8,
}

impl Default for Cop0 {
    fn default() -> Self {
        Self::new()
    }
}

impl Cop0 {
    /// Cold-reset state (UM §6.4.4, p. 183; Fig. 6-16, p. 205).
    ///
    /// Defined by the manual: `Status.ERL` and `Status.BEV` set, `Status.TS`/
    /// `SR`/`RP` clear, `Config.BE` set, `Config.EP` clear, `Random` = 31,
    /// `Wired` = 0. Everything else the manual calls **undefined**, and is a
    /// deterministic zero here (ADR 0004).
    #[must_use]
    pub const fn new() -> Self {
        let mut regs = [0u64; 32];
        // ERL (2) and BEV (22) set; TS/SR/RP already clear.
        regs[reg::STATUS as usize] = (1 << 2) | (1 << 22);
        // "Random is set to 31 on Cold Reset" (UM §5.4.2, p. 147).
        regs[reg::RANDOM as usize] = 31;
        // BE (15) set: the N64 is big-endian. K0 defaults to 0 (uncached); IPL3
        // writes 3 during boot, which is where 0x0006E463 comes from.
        regs[reg::CONFIG as usize] = 1 << 15;
        // Imp = 0x0B for the VR4300 series (UM §5.4.5, p. 151). The Rev field is
        // NOT documented for any specific part, and the manual explicitly warns
        // against depending on it -- so it stays 0 rather than becoming a
        // plausible-looking invention. Accuracy ledger U-3.
        regs[reg::PRID as usize] = 0x0B << 8;
        Self {
            regs,
            now: 0,
            count_epoch_value: 0,
            count_epoch_tick: 0,
            // 0b111 = 1:1.5, which matches the N64's 62.5 MHz : 93.75 MHz and is
            // "allowed with the 100 MHz model only" (UM Appendix A note 1,
            // p. 628). The manual never names the N64, so this is an INFERENCE
            // -- accuracy ledger U-6, not a documented fact.
            ec: 0b111,
        }
    }

    /// Advance the `Count` timeline to `now` (the scheduler's `count_ticks`).
    ///
    /// Called once per CPU step. Note this **sets** rather than increments: the
    /// position is derived, so a dropped or repeated call cannot desynchronise
    /// it from the master clock the way an increment would.
    pub const fn set_now(&mut self, now: u64) {
        self.now = now;
    }

    /// The current `Count` timeline position.
    #[must_use]
    pub const fn count_now(&self) -> u64 {
        self.now
    }

    /// `Count`, computed rather than stored.
    #[must_use]
    pub const fn count(&self) -> u32 {
        self.count_epoch_value
            .wrapping_add((self.now.wrapping_sub(self.count_epoch_tick)) as u32)
    }

    /// Has the timer fired — `Count == Compare`?
    ///
    /// UM §6.3.4 (p. 165). The comparison is on the *computed* `Count`, so it
    /// stays true regardless of how the timeline was reached.
    #[must_use]
    pub const fn timer_matches(&self) -> bool {
        self.count() == self.regs[reg::COMPARE as usize] as u32
    }

    /// Set or clear a `Cause.IP` bit.
    ///
    /// `bit` is 0..=7. `IP1:IP0` are software interrupts and are written through
    /// `MTC0` instead; this is the hardware path for `IP2` (RCP) and `IP7`
    /// (timer).
    pub const fn set_ip(&mut self, bit: u8, on: bool) {
        let m = 1u64 << (8 + (bit & 7));
        let cause = self.regs[reg::CAUSE as usize];
        self.regs[reg::CAUSE as usize] = if on { cause | m } else { cause & !m };
    }

    /// Is an interrupt currently *recognised*?
    ///
    /// All four conditions, and each one matters (UM §6.1 p. 160, §6.3.5 p. 168,
    /// Fig. 14-4 p. 357):
    ///
    /// - `Status.IE` — the global enable.
    /// - **`Status.EXL` clear** — a handler is not interrupted by the thing it
    ///   is handling. This is why `EXL` implies interrupts-off without `IE`
    ///   being touched.
    /// - **`Status.ERL` clear** — likewise for the error path.
    /// - `Cause.IP & Status.IM` — at least one pending *and* unmasked.
    ///
    /// Dropping the `EXL`/`ERL` terms is the classic version of this bug: it
    /// works until the first interrupt arrives inside a handler, and then
    /// re-enters it forever.
    #[must_use]
    pub const fn interrupt_pending(&self) -> bool {
        let status = self.regs[reg::STATUS as usize];
        if status & 1 == 0 {
            return false; // IE clear
        }
        if status & (1 << 1) != 0 || status & (1 << 2) != 0 {
            return false; // EXL or ERL set
        }
        let ip = (self.regs[reg::CAUSE as usize] >> 8) & 0xFF;
        let im = (status >> 8) & 0xFF;
        ip & im != 0
    }

    /// Read a register's architectural value.
    ///
    /// [`ARCH_MASK`] is applied here as well as on the way in, so a 32-bit
    /// register can never return non-zero upper bits and `EntryHi.Fill` always
    /// reads zero — regardless of how the stored value arrived. That matters
    /// because [`Cop0::set_hardware`] exists precisely to bypass the *write*
    /// masks, and exception dispatch will feed it raw faulting addresses.
    #[must_use]
    pub const fn read(&self, n: u8) -> u64 {
        let n = n & 31;
        let raw = self.regs[n as usize];
        if n == reg::COUNT {
            // Derived, never stored -- see `count`.
            return self.count() as u64;
        }
        if n == reg::CONFIG {
            // Composed, not masked: the hardwired fields are merged on READ
            // rather than seeded at construction, because seeded they could be
            // erased by a wide-enough write mask and every later read would be
            // wrong. Merged, that is structurally impossible.
            //
            // The writable bits are EP (27:24) | BE (15) | CU (3) | K0 (2:0).
            return (raw & ARCH_MASK[reg::CONFIG as usize])
                | ((self.ec as u64) << 28)
                | CONFIG_HARDWIRED_HI
                | CONFIG_HARDWIRED_LO;
        }
        raw & ARCH_MASK[n as usize]
    }

    /// Write a register, applying its writable-bit mask.
    ///
    /// Bits outside [`WRITE_MASK`] keep their previous value, which is what
    /// hardware does and is *not* the same as writing zero to them.
    ///
    /// Registers 7, 21..=25 and 31 are *"Reserved for future use"* (UM Table 1-2,
    /// p. 46). The manual never says what reading them returns or what writing
    /// them does, so this implementation **discards writes and reads zero** —
    /// a deliberate, arbitrary choice recorded as accuracy-ledger **U-1**. It is
    /// a guess; it must not be cited as behaviour.
    pub const fn write(&mut self, n: u8, value: u64) {
        let n = n & 31;
        let mask = WRITE_MASK[n as usize];
        self.regs[n as usize] = (self.regs[n as usize] & !mask) | (value & mask);
        // "If the timer interrupt request is generated, either clear the IP7 bit
        // of the Cause register or change the contents of the Compare register,
        // to clear this interrupt" (UM §6.4.18, p. 200).
        //
        // IP7 is LATCHED, not a level. The existence of a documented clear is
        // itself the evidence: a level tied to `Count == Compare` would
        // self-clear on the next tick and would need no clearing mechanism at
        // all. It would also LOSE a timer interrupt raised while `EXL` was set,
        // because the handler would never see the one-tick pulse.
        //
        // Note the manual's first option -- writing `Cause.IP7` -- is not
        // actually available on this part: `Cause` is read-only to software
        // except `IP1:IP0`. Writing `Compare` is the usable path, and the one
        // libdragon takes.
        if n == reg::COMPARE {
            self.set_ip(7, false);
        }
        // Writing Count re-bases the affine mapping rather than storing a value,
        // so the register stays derived from the master clock (ADR 0006).
        if n == reg::COUNT {
            self.count_epoch_value = value as u32;
            self.count_epoch_tick = self.now;
        }
        // "Random is set to 31 whenever the Wired register is written"
        // (UM §5.4.2, p. 147) -- a side effect, not a rule about Random itself,
        // and easy to lose because it belongs to neither register alone.
        if n == reg::WIRED {
            self.regs[reg::RANDOM as usize] = 31;
        }
    }

    /// Force a value past the writable-bit mask, for hardware-owned fields.
    ///
    /// Exception dispatch writes `Cause.ExcCode`, `EPC` and `BadVAddr`, all of
    /// which are read-only or partly read-only *to software*. Routing those
    /// through [`Cop0::write`] would require widening the masks, which would
    /// also let `MTC0` write them — the exact bug the masks exist to prevent.
    pub const fn set_hardware(&mut self, n: u8, value: u64) {
        let n = n & 31;
        // Bypasses WRITE_MASK, NOT ARCH_MASK. Hardware may write bits software
        // cannot; it cannot write bits the register does not have. Without this,
        // dispatch storing a raw 64-bit faulting address into `EntryHi` would
        // put non-zero bits in `Fill`, which architecturally reads zero.
        self.regs[n as usize] = value & ARCH_MASK[n as usize];
    }

    /// `MFC0 rt, rd` — read the low 32 bits, sign-extended into the 64-bit GPR.
    ///
    /// Sign-extension applies even to a register that is architecturally 64 bits
    /// wide: `MFC0` is defined as a 32-bit move, so `MFC0` of an `EPC` whose bit
    /// 31 is set yields a sign-extended value, not a truncated one.
    #[must_use]
    pub const fn mfc0(&self, n: u8) -> u64 {
        self.read(n) as u32 as i32 as i64 as u64
    }

    /// `DMFC0 rt, rd` — read the full 64 bits.
    ///
    /// On a 32-bit-wide register this is the same as [`Cop0::mfc0`] except for
    /// sign-extension: the upper half is zero rather than a copy of bit 31.
    #[must_use]
    pub const fn dmfc0(&self, n: u8) -> u64 {
        self.read(n)
    }

    /// `MTC0 rd, rt` — write the low 32 bits.
    ///
    /// For a 64-bit register the upper half is **cleared**, not preserved: the
    /// value written is the sign-extended 32-bit operand, which is how software
    /// legitimately writes a KSEG0 address into a 64-bit register.
    pub const fn mtc0(&mut self, n: u8, value: u64) {
        let v = if is_wide(n) {
            value as u32 as i32 as i64 as u64
        } else {
            value & 0xFFFF_FFFF
        };
        self.write(n, v);
    }

    /// `DMTC0 rd, rt` — write the full 64 bits.
    pub const fn dmtc0(&mut self, n: u8, value: u64) {
        self.write(n, value);
    }

    /// Advance `Random` by one instruction (UM §5.4.2, p. 147).
    ///
    /// *"decrements as each instruction executes"*, wrapping to 31 at the
    /// `Wired` floor. When `Wired` is 31 the range is a single value and the
    /// register is effectively constant — that is the documented behaviour, not
    /// a degenerate case to guard against.
    pub const fn tick_random(&mut self) {
        let wired = (self.regs[reg::WIRED as usize] & 0x3F) as u32;
        let cur = (self.regs[reg::RANDOM as usize] & 0x3F) as u32;
        self.regs[reg::RANDOM as usize] = if cur <= wired || cur == 0 {
            31
        } else {
            (cur - 1) as u64
        };
    }
}

/// The COP0 access forms, decoded from the `rs` field of a `COP0` instruction.
///
/// Split out so the decoder names them rather than the executor re-deriving them
/// from raw bits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Cop0Op {
    /// `MFC0 rt, rd` — 32-bit read, sign-extended.
    Mfc0,
    /// `DMFC0 rt, rd` — 64-bit read.
    Dmfc0,
    /// `MTC0 rt, rd` — 32-bit write.
    Mtc0,
    /// `DMTC0 rt, rd` — 64-bit write.
    Dmtc0,
}

impl Cop0Op {
    /// Decode the `rs` field of a `COP0` instruction, if it names an access form.
    ///
    /// Returns `None` for the TLB and `ERET` encodings (`rs` bit 4 set), which
    /// are separate instructions handled by T-12-002 and T-12-004.
    #[must_use]
    pub const fn from_rs(rs: u8) -> Option<Self> {
        match rs {
            0o00 => Some(Self::Mfc0),
            0o01 => Some(Self::Dmfc0),
            0o04 => Some(Self::Mtc0),
            0o05 => Some(Self::Dmtc0),
            _ => None,
        }
    }
}

/// The `rd` field of a `COP0` instruction is the register number.
#[must_use]
pub const fn cop0_reg(d: Decoded) -> u8 {
    d.rd
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The list of 64-bit registers has no generating rule, so it is asserted
    /// element by element against UM Table 1-2 (p. 46). Getting one wrong is
    /// invisible until 64-bit software runs.
    #[test]
    fn the_eight_wide_registers_are_exactly_these() {
        let wide = [
            reg::ENTRY_LO0,
            reg::ENTRY_LO1,
            reg::CONTEXT,
            reg::BAD_VADDR,
            reg::ENTRY_HI,
            reg::EPC,
            reg::XCONTEXT,
            reg::ERROR_EPC,
        ];
        for n in 0..32u8 {
            assert_eq!(is_wide(n), wide.contains(&n), "register {n} width is wrong");
        }
        assert_eq!(wide.len(), 8, "exactly eight, not seven or nine");
    }

    /// Read-only registers must reject `MTC0`. Modelled as an all-zero write
    /// mask so "read-only" and "reserved bit" share one mechanism.
    #[test]
    fn read_only_registers_reject_writes() {
        for n in [reg::RANDOM, reg::BAD_VADDR, reg::PRID, reg::CACHE_ERR] {
            let mut c = Cop0::new();
            let before = c.read(n);
            c.dmtc0(n, 0xDEAD_BEEF_DEAD_BEEF);
            assert_eq!(c.read(n), before, "register {n} is read-only");
        }
    }

    /// `Cause` is read-only *except* `IP1:IP0` — a mask, not a whole-register
    /// rule, and the one most likely to be widened by mistake.
    #[test]
    fn cause_accepts_only_the_two_software_interrupt_bits() {
        let mut c = Cop0::new();
        c.dmtc0(reg::CAUSE, u64::MAX);
        assert_eq!(
            c.read(reg::CAUSE),
            0x0000_0300,
            "only IP1:IP0 (bits 9:8) are software-writable"
        );
    }

    /// `Status.DS.TS` (bit 21) is read-only; the rest of `DS` is writable.
    #[test]
    fn status_ts_is_read_only_but_the_rest_of_ds_is_not() {
        let mut c = Cop0::new();
        c.dmtc0(reg::STATUS, u64::MAX);
        let s = c.read(reg::STATUS);
        assert_eq!(s & (1 << 21), 0, "TS must not be software-settable");
        assert_ne!(s & (1 << 22), 0, "BEV in the same field must be settable");
        assert_eq!(s & (1 << 23), 0, "bit 23 is hardwired zero");
        assert_eq!(s & (1 << 19), 0, "bit 19 is hardwired zero");
    }

    /// `Config`'s hardwired fields survive any write, because they are merged on
    /// read rather than stored. Seeding them instead would let a wide mask erase
    /// them permanently.
    #[test]
    fn config_hardwired_fields_survive_a_hostile_write() {
        let mut c = Cop0::new();
        c.dmtc0(reg::CONFIG, 0);
        let v = c.read(reg::CONFIG);
        assert_eq!(
            (v >> 16) & 0xFF,
            0b0000_0110,
            "bits 23:16 are hardwired (UM Fig. 5-16)"
        );
        assert_eq!(
            (v >> 4) & 0x7FF,
            0b110_0100_0110,
            "bits 14:4 are hardwired (UM Fig. 5-16)"
        );
        assert_eq!(
            (v >> 28) & 0b111,
            0b111,
            "EC is read-only from `DivMode` pins"
        );
    }

    /// Independent cross-check: the real N64 IPL boot values must decompose
    /// exactly against these layouts. This is the cheapest evidence available
    /// that the field positions are right, and it comes from outside the manual.
    #[test]
    fn the_real_ipl_boot_values_round_trip() {
        let mut c = Cop0::new();
        // IPL3 leaves Config = 0x0006E463: BE=1, K0=3, EP=0, CU=0, and both
        // hardwired fields. Our EC is 0b111 rather than the 0b000 in that
        // capture, because EC is read-only and the captured value is what IPL
        // *wrote*, not what it read back -- so compare with EC masked out.
        c.mtc0(reg::CONFIG, 0x0006_E463);
        let got = c.read(reg::CONFIG) & !0x7000_0000;
        assert_eq!(
            got, 0x0006_E463,
            "Config must round-trip the real IPL value outside EC"
        );

        // Status = 0x34000000 is CU1|CU0|FR.
        c.mtc0(reg::STATUS, 0x3400_0000);
        assert_eq!(c.read(reg::STATUS), 0x3400_0000);
    }

    /// Writing `Wired` forces `Random` to 31 — a side effect belonging to
    /// neither register alone, and easy to lose for exactly that reason.
    #[test]
    fn writing_wired_reloads_random() {
        let mut c = Cop0::new();
        c.tick_random();
        c.tick_random();
        assert_ne!(c.read(reg::RANDOM), 31, "Random moved off its reset value");
        c.mtc0(reg::WIRED, 4);
        assert_eq!(c.read(reg::RANDOM), 31, "writing Wired reloads Random");
    }

    /// `Random` decrements per instruction and wraps at the `Wired` floor.
    #[test]
    fn random_decrements_and_floors_at_wired() {
        let mut c = Cop0::new();
        c.mtc0(reg::WIRED, 28);
        assert_eq!(c.read(reg::RANDOM), 31);
        // Wired itself IS a legal value -- it is the first non-wired entry, and
        // TLBWR must be able to select it. The range is [Wired, 31] inclusive,
        // so the wrap happens after 28, not after 29.
        for expect in [30, 29, 28, 31, 30, 29] {
            c.tick_random();
            assert_eq!(c.read(reg::RANDOM), expect, "range is [Wired, 31]");
        }
    }

    /// `EntryHi.Fill` (61:40) is write-ignored and reads zero.
    #[test]
    fn entryhi_fill_is_write_ignored() {
        let mut c = Cop0::new();
        c.dmtc0(reg::ENTRY_HI, u64::MAX);
        let v = c.read(reg::ENTRY_HI);
        assert_eq!((v >> 40) & 0x3F_FFFF, 0, "Fill reads zero");
        assert_eq!((v >> 62) & 0b11, 0b11, "R is writable");
        assert_eq!(v & 0xFF, 0xFF, "ASID is writable");
        assert_eq!((v >> 13) & 0x7FF_FFFF, 0x7FF_FFFF, "VPN2 is writable");
        assert_eq!((v >> 8) & 0x1F, 0, "bits 12:8 are hardwired zero");
    }

    /// `MFC0` sign-extends; `DMFC0` does not. The difference is visible on any
    /// register holding a KSEG0 address, which is most of the interesting ones.
    #[test]
    fn mfc0_sign_extends_where_dmfc0_does_not() {
        let mut c = Cop0::new();
        c.dmtc0(reg::EPC, 0x8000_0180);
        assert_eq!(c.mfc0(reg::EPC), 0xFFFF_FFFF_8000_0180);
        assert_eq!(c.dmfc0(reg::EPC), 0x8000_0180);
    }

    /// `MTC0` to a 64-bit register sign-extends rather than preserving the upper
    /// half — that is how software writes a KSEG0 address with one instruction.
    #[test]
    fn mtc0_to_a_wide_register_sign_extends() {
        let mut c = Cop0::new();
        c.dmtc0(reg::EPC, 0x1234_5678_9ABC_DEF0);
        c.mtc0(reg::EPC, 0x8000_0180);
        assert_eq!(
            c.read(reg::EPC),
            0xFFFF_FFFF_8000_0180,
            "the old upper half must not survive"
        );
    }

    /// `set_hardware` bypasses the masks, because exception dispatch writes
    /// fields that are read-only to software. If this went through `write`, the
    /// masks would have to be widened and `MTC0` could reach them too.
    #[test]
    fn hardware_writes_bypass_the_software_masks() {
        let mut c = Cop0::new();
        c.dmtc0(reg::BAD_VADDR, 0x1234);
        assert_eq!(c.read(reg::BAD_VADDR), 0, "software cannot write BadVAddr");
        c.set_hardware(reg::BAD_VADDR, 0x1234);
        assert_eq!(c.read(reg::BAD_VADDR), 0x1234, "hardware can");
    }

    /// Cold-reset state, as far as the manual defines it (UM §6.4.4, p. 183).
    #[test]
    fn cold_reset_matches_the_documented_fields() {
        let c = Cop0::new();
        let s = c.read(reg::STATUS);
        assert_ne!(s & (1 << 2), 0, "ERL set");
        assert_ne!(s & (1 << 22), 0, "BEV set");
        assert_eq!(s & (1 << 21), 0, "TS clear");
        assert_eq!(s & (1 << 20), 0, "SR clear");
        assert_eq!(s & (1 << 27), 0, "RP clear");
        assert_eq!(c.read(reg::RANDOM), 31, "Random = 31");
        assert_eq!(c.read(reg::WIRED), 0, "Wired = 0");
        assert_ne!(c.read(reg::CONFIG) & (1 << 15), 0, "Config.BE set");
        assert_eq!((c.read(reg::PRID) >> 8) & 0xFF, 0x0B, "PRId.Imp = 0x0B");
    }

    /// Reserved registers: a documented *absence*, so the behaviour here is an
    /// arbitrary choice (ledger U-1) and this test pins the choice, not the
    /// hardware. It exists so a future change to it is deliberate.
    #[test]
    fn reserved_registers_read_zero_and_discard_writes_by_choice_not_by_evidence() {
        for n in [7u8, 21, 22, 23, 24, 25, 31] {
            let mut c = Cop0::new();
            c.dmtc0(n, u64::MAX);
            assert_eq!(c.read(n), 0, "reserved register {n} -- see ledger U-1");
        }
    }

    /// The access-form decode, including that TLB/`ERET` encodings are not
    /// access forms and must not be silently treated as one.
    #[test]
    fn cop0_access_forms_decode_and_tlb_forms_do_not() {
        assert_eq!(Cop0Op::from_rs(0o00), Some(Cop0Op::Mfc0));
        assert_eq!(Cop0Op::from_rs(0o01), Some(Cop0Op::Dmfc0));
        assert_eq!(Cop0Op::from_rs(0o04), Some(Cop0Op::Mtc0));
        assert_eq!(Cop0Op::from_rs(0o05), Some(Cop0Op::Dmtc0));
        // rs bit 4 set: TLBR/TLBWI/TLBWR/TLBP/ERET -- T-12-002 and T-12-004.
        assert_eq!(Cop0Op::from_rs(0o20), None);
        assert_eq!(
            Cop0Op::from_rs(0o02),
            None,
            "unassigned rs is not an access"
        );
    }

    /// Writes must preserve unmasked bits rather than zeroing them -- hardware
    /// discards the write to those bits, which is not the same thing.
    #[test]
    fn masked_off_bits_keep_their_previous_value() {
        let mut c = Cop0::new();
        // set_hardware itself applies ARCH_MASK, so this stores Cause's
        // architectural bits (BD | CE | IP | ExcCode), not all 32.
        c.set_hardware(reg::CAUSE, 0xFFFF_FFFF);
        assert_eq!(c.read(reg::CAUSE), ARCH_MASK[reg::CAUSE as usize]);
        c.dmtc0(reg::CAUSE, 0);
        assert_eq!(
            c.read(reg::CAUSE),
            ARCH_MASK[reg::CAUSE as usize] & !0x300,
            "only IP1:IP0 were cleared; every other architectural bit survived"
        );
    }

    /// `set_hardware` must mask on the way **in**, so the stored state never
    /// holds bits no hardware register has.
    ///
    /// Asserted against `regs` directly rather than through `read`, because
    /// `read` masks too — a read-back test passes with either enforcement point
    /// alone and so pins neither. Found by mutation testing: removing either one
    /// individually was invisible until these two tests existed.
    #[test]
    fn set_hardware_masks_on_the_way_in_not_only_on_the_way_out() {
        let mut c = Cop0::new();
        c.set_hardware(reg::CAUSE, u64::MAX);
        assert_eq!(
            c.regs[reg::CAUSE as usize],
            ARCH_MASK[reg::CAUSE as usize],
            "stored state must already be architectural"
        );
        c.set_hardware(reg::ENTRY_HI, u64::MAX);
        assert_eq!(
            (c.regs[reg::ENTRY_HI as usize] >> 40) & 0x3F_FFFF,
            0,
            "EntryHi.Fill is not even stored"
        );
    }

    /// `read` must mask on the way **out**, independently of how the value was
    /// stored. Poked straight into `regs` for the same reason as above: going
    /// through `set_hardware` would let that mask do the work instead.
    #[test]
    fn read_masks_on_the_way_out_even_if_storage_is_corrupt() {
        let mut c = Cop0::new();
        // Not `Count`: that one is derived rather than stored, so poking its
        // backing slot proves nothing.
        c.regs[reg::COMPARE as usize] = u64::MAX;
        assert_eq!(c.read(reg::COMPARE), 0xFFFF_FFFF, "32 bits, not 64");
        c.regs[reg::ENTRY_HI as usize] = u64::MAX;
        assert_eq!(
            (c.read(reg::ENTRY_HI) >> 40) & 0x3F_FFFF,
            0,
            "Fill reads zero even from corrupt storage"
        );
    }

    /// `set_hardware` on `Config` must not erase it. `Config`'s read value is
    /// composed rather than masked, so it is the one register whose `ARCH_MASK`
    /// entry is easy to leave at zero — which would silently wipe it.
    #[test]
    fn set_hardware_does_not_wipe_config() {
        let mut c = Cop0::new();
        c.set_hardware(reg::CONFIG, 0x0006_E463);
        let v = c.read(reg::CONFIG);
        assert_ne!(v & (1 << 15), 0, "BE survived");
        assert_eq!(v & 0b111, 3, "K0 survived");
        assert_eq!((v >> 16) & 0xFF, 0b0000_0110, "hardwired field still there");
    }

    /// A 32-bit register must never return non-zero upper bits, no matter how
    /// the value got in. `set_hardware` deliberately bypasses the *write* masks,
    /// so without an architectural mask it would be a hole straight into the
    /// stored state — and exception dispatch (T-12-002) feeds it raw addresses.
    #[test]
    fn a_32_bit_register_cannot_hold_upper_bits_even_via_set_hardware() {
        for n in [
            reg::COUNT,
            reg::COMPARE,
            reg::STATUS,
            reg::CAUSE,
            reg::LL_ADDR,
        ] {
            let mut c = Cop0::new();
            c.set_hardware(n, u64::MAX);
            assert_eq!(
                c.read(n) >> 32,
                0,
                "register {n} is 32 bits wide; its upper half must read zero"
            );
            assert_eq!(c.dmfc0(n) >> 32, 0, "and DMFC0 must not expose them either");
        }
    }

    /// `EntryHi.Fill` reads zero architecturally, not merely "is unwritable by
    /// MTC0". Dispatch storing a raw 64-bit faulting address is exactly the path
    /// that would otherwise put bits there.
    #[test]
    fn entryhi_fill_reads_zero_even_when_hardware_writes_a_raw_address() {
        let mut c = Cop0::new();
        c.set_hardware(reg::ENTRY_HI, 0xFFFF_FFFF_FFFF_FFFF);
        assert_eq!(
            (c.read(reg::ENTRY_HI) >> 40) & 0x3F_FFFF,
            0,
            "Fill (61:40) reads zero regardless of the writer"
        );
        assert_eq!((c.read(reg::ENTRY_HI) >> 62) & 0b11, 0b11, "R survives");
    }

    /// The two masks are different things and the difference is load-bearing:
    /// a writable bit that is not architecturally present would be storable and
    /// unreadable, and a read-only register needs a wide arch mask with a zero
    /// write mask.
    #[test]
    fn every_writable_bit_is_also_an_architectural_bit() {
        for n in 0..32u8 {
            let w = WRITE_MASK[n as usize];
            let a = ARCH_MASK[n as usize];
            assert_eq!(
                w & !a,
                0,
                "register {n} has writable bits that are not architectural"
            );
        }
        // And the converse must NOT hold, or the two tables would be redundant.
        assert_ne!(
            ARCH_MASK[reg::CAUSE as usize],
            WRITE_MASK[reg::CAUSE as usize],
            "Cause is mostly hardware-written"
        );
        assert_eq!(WRITE_MASK[reg::BAD_VADDR as usize], 0);
        assert_ne!(ARCH_MASK[reg::BAD_VADDR as usize], 0);
    }

    /// `Count` is **derived**, not stored: ADR 0006 permits exactly one
    /// incremented counter in the core, and it is `master_ticks`.
    #[test]
    fn count_is_derived_from_the_timeline_not_incremented() {
        let mut c = Cop0::new();
        c.set_now(0);
        assert_eq!(c.read(reg::COUNT), 0);
        c.set_now(100);
        assert_eq!(c.read(reg::COUNT), 100, "follows the timeline with no += 1");
        // Skipping the timeline forward must not lose anything -- an increment
        // would, which is the whole reason this is derived.
        c.set_now(1_000_000);
        assert_eq!(c.read(reg::COUNT), 1_000_000);
    }

    /// ...and still guest-writable, which is what makes it *affine* rather than
    /// simply derived. A write re-bases the epoch; it does not store a value
    /// that then drifts.
    #[test]
    fn writing_count_rebases_the_epoch_rather_than_storing() {
        let mut c = Cop0::new();
        c.set_now(500);
        c.mtc0(reg::COUNT, 42);
        assert_eq!(c.read(reg::COUNT), 42, "reads back what was written");
        c.set_now(510);
        assert_eq!(
            c.read(reg::COUNT),
            52,
            "and then advances with the timeline from there"
        );
    }

    /// The timer fires on `Count == Compare` (UM §6.3.4, p. 165).
    #[test]
    fn the_timer_matches_when_count_reaches_compare() {
        let mut c = Cop0::new();
        c.set_now(0);
        c.mtc0(reg::COMPARE, 5);
        assert!(!c.timer_matches());
        c.set_now(5);
        assert!(c.timer_matches());
        c.set_now(6);
        assert!(!c.timer_matches(), "it is an equality, not a threshold");
    }
}
