//! The `SysAD` bus transaction model (T-11-008).
//!
//! The CPU reaches everything outside its caches through `SysAD`, a **packet
//! protocol** — not a simple addressed read. A transaction is an *address cycle*
//! carrying a command, then a *data cycle* carrying the payload, handshaked by
//! `EOK` / `Pvalid` / `Evalid`, with an **unbounded** wait between them. That wait
//! is where real RDRAM latency lives.
//!
//! # Why model it at all
//!
//! Neither reference emulator does. CEN64 completes the whole access atomically
//! in zero emulated time and charges a flat constant (its own source says
//! `// Currently using fixed values....`); ares charges different constants. They
//! disagree on the value and neither derived it from a spec. Modelling the split
//! is where this project can be better rather than equal — and it is what makes
//! the bus access a point the scheduler can interleave the RCP around, which is
//! the whole reason ADR 0007 models a pipeline.
//!
//! # Clock domain
//!
//! `SysAD` runs at `SClock` = `MClock` = **62.5 MHz**, so one bus cycle is 1.5
//! `PCycle`s — 3 master ticks against the CPU's 2 (ADR 0006). A transaction is
//! therefore *not* a whole number of CPU cycles, which is exactly the
//! "**1 to 2** `PCycle`s: synchronize with `SClock`" indeterminacy the manual
//! charges in Table 11-1.

/// Which half of a transaction is on the wire.
///
/// # The polarity, and a contradiction that turns out not to be one
///
/// `docs/accuracy-ledger.md` recorded S-1: the User's Manual says command =
/// `SysCmd4` **0** while the wiki's cheat sheet says **1**. Reading both
/// carefully, they **agree on every bit value** and disagree only on English.
///
/// - UM §12.11.1: *"During address cycles \[`SysCmd4` = 0\] … contains a System
///   interface command"*, and *"During data cycles \[`SysCmd4` = 1\]"*.
/// - The wiki table gives read/write **requests** bit 4 = 0 (labelled "Data
///   req") and data-carrying cycles bit 4 = 1 (labelled "Command").
///
/// So a request always has bit 4 clear and a data beat always has it set, in both
/// sources. The wiki simply uses "Command" for the cycle the manual calls a data
/// identifier. We follow the **manual's** naming, since it is the vendor spec and
/// the rest of this crate cites it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Phase {
    /// The address cycle: `SysCmd4 = 0`, carrying a command in `SysCmd(3:0)`.
    Address,
    /// The data cycle: `SysCmd4 = 1`, carrying a data identifier.
    Data,
}

impl Phase {
    /// `SysCmd4` for this phase.
    #[must_use]
    pub const fn syscmd4(self) -> bool {
        matches!(self, Self::Data)
    }
}

/// Transfer size, encoded in `SysCmd(1:0)` of a request.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Width {
    /// 32-bit single transfer. All 8/16/24/32-bit reads are issued as this; the
    /// CPU does the shifting internally.
    Single32,
    /// 64-bit.
    Single64,
    /// 128-bit block — a D-cache line.
    Block128,
    /// 256-bit block — an I-cache line.
    Block256,
}

impl Width {
    /// Bytes moved.
    #[must_use]
    pub const fn bytes(self) -> u32 {
        match self {
            Self::Single32 => 4,
            Self::Single64 => 8,
            Self::Block128 => 16,
            Self::Block256 => 32,
        }
    }

    /// 32-bit beats on the bus, which is what the data phase costs.
    #[must_use]
    pub const fn beats(self) -> u32 {
        self.bytes() / 4
    }
}

/// The order a block transfer's 64-bit halves arrive in.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum BlockOrder {
    /// Lowest address first.
    Sequential,
    /// The **requested** 64 bits first, then the 64 bits *below* it.
    ///
    /// Not "the ones after it" — this is the trap. A D-cache 128-bit read whose
    /// address bit 4 is set returns the addressed half and then the *preceding*
    /// half (`n64brew_wiki/markdown/SysAD Interface.md`, citing UM p.339).
    SubBlock,
}

/// The ordering a read of `width` at `addr` uses.
///
/// D-cache 128-bit reads are sub-block ordered when address bit 4 is set;
/// everything else, including all I-cache 256-bit reads, is sequential. Getting
/// this wrong corrupts every other cache line fill and nothing else.
#[must_use]
pub const fn block_order(width: Width, addr: u32) -> BlockOrder {
    match width {
        Width::Block128 if addr & 0x10 != 0 => BlockOrder::SubBlock,
        // I-cache reads are always 256-bit aligned and always sequential:
        // "there is no smarts in the 4300i CPU to know if all of the cache entry
        // is full", so the whole line is fetched in order.
        _ => BlockOrder::Sequential,
    }
}

/// A bus transaction in progress.
///
/// Modelled as a small state machine rather than an atomic operation, so the
/// scheduler can advance the RCP *between* the address and data phases — the
/// property that makes a device able to observe the bus mid-transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Transaction {
    /// Physical address.
    pub addr: u32,
    /// Transfer size.
    pub width: Width,
    /// Is this a write? Writes are throttled by `EOK` rather than waiting for
    /// `Evalid`.
    pub write: bool,
    /// Which phase is on the wire.
    pub phase: Phase,
    /// `SClock` cycles remaining in the current phase.
    pub remaining: u32,
}

impl Transaction {
    /// Begin a transaction. It starts in the address phase, which occupies one
    /// `SClock` cycle.
    #[must_use]
    pub const fn begin(addr: u32, width: Width, write: bool) -> Self {
        Self {
            addr,
            width,
            write,
            phase: Phase::Address,
            remaining: 1,
        }
    }

    /// Advance one `SClock` cycle. Returns `true` once the transaction completes.
    ///
    /// `service_cycles` is the target's latency — the unbounded wait between the
    /// phases, and where `M` lives (`docs/accuracy-ledger.md` C-1). It is a
    /// parameter rather than a constant precisely so it cannot be quietly tuned:
    /// the caller must supply a value it can justify.
    pub const fn step(&mut self, service_cycles: u32) -> bool {
        if self.remaining > 0 {
            self.remaining -= 1;
        }
        if self.remaining > 0 {
            return false;
        }
        match self.phase {
            Phase::Address => {
                self.phase = Phase::Data;
                // The target's latency, then one bus cycle per 32-bit beat.
                self.remaining = service_cycles + self.width.beats();
                false
            }
            Phase::Data => true,
        }
    }

    /// Total `SClock` cycles this transaction occupies, given a target latency.
    ///
    /// One address cycle, the target's service time, then one cycle per beat.
    #[must_use]
    pub const fn total_cycles(width: Width, service_cycles: u32) -> u32 {
        1 + service_cycles + width.beats()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The polarity, settled. Both sources agree on the bit; they disagree only
    /// on which cycle they call "command".
    #[test]
    fn syscmd4_is_clear_for_a_request_and_set_for_data() {
        assert!(!Phase::Address.syscmd4(), "UM 12.11.1: address cycle = 0");
        assert!(Phase::Data.syscmd4(), "UM 12.11.1: data cycle = 1");
    }

    #[test]
    fn widths_are_the_documented_transfer_sizes() {
        assert_eq!(Width::Single32.bytes(), 4);
        assert_eq!(Width::Single64.bytes(), 8);
        assert_eq!(Width::Block128.bytes(), 16, "a D-cache line");
        assert_eq!(Width::Block256.bytes(), 32, "an I-cache line");
        // The bus is 32 bits wide, so beats are what the data phase costs.
        assert_eq!(Width::Block256.beats(), 8);
    }

    /// **The sub-block ordering quirk.** A D-cache 128-bit read whose address
    /// bit 4 is set returns the addressed 64 bits first, then the 64 bits
    /// *below* — not the ones after. Getting this wrong corrupts every other
    /// cache line fill and nothing else, which is a miserable way to find it.
    #[test]
    fn dcache_block_reads_use_sub_block_ordering_when_address_bit_4_is_set() {
        // Bit 4 is the 0x10 bit, so it alternates every 16 bytes -- NOT every
        // 8. My first version of this test asserted 0x...70 and 0x...78 were
        // sequential; both have bit 4 set. Values below are computed, not
        // eyeballed.
        for addr in [0x0000u32, 0x0020, 0x0040, 0x1234_5660] {
            assert_eq!(
                block_order(Width::Block128, addr),
                BlockOrder::Sequential,
                "{addr:#X} has bit 4 clear"
            );
        }
        for addr in [0x0010u32, 0x0030, 0x1234_5670, 0x1234_5678] {
            assert_eq!(
                block_order(Width::Block128, addr),
                BlockOrder::SubBlock,
                "{addr:#X} has bit 4 set"
            );
        }

        // I-cache reads are ALWAYS sequential, whatever bit 4 says -- the CPU has
        // no way to use a partial line, so it always fetches the whole thing in
        // order.
        for addr in [0x0000u32, 0x0010, 0x0020, 0x0030] {
            assert_eq!(
                block_order(Width::Block256, addr),
                BlockOrder::Sequential,
                "I-cache read at {addr:#X}"
            );
        }
        // Single transfers have no ordering to get wrong.
        assert_eq!(block_order(Width::Single32, 0x10), BlockOrder::Sequential);
    }

    /// A transaction is a state machine, not an atomic operation. It must pass
    /// through the address phase before the data phase, so the scheduler has a
    /// point at which to step the RCP.
    #[test]
    fn a_transaction_passes_through_both_phases() {
        let mut t = Transaction::begin(0x1000, Width::Single32, false);
        assert_eq!(t.phase, Phase::Address);

        // The address cycle completes and hands over to the data phase.
        assert!(!t.step(0), "one address cycle is not a whole transaction");
        assert_eq!(t.phase, Phase::Data, "must reach the data phase");

        // One beat for a 32-bit transfer.
        assert!(
            t.step(0),
            "a zero-latency 32-bit transfer ends after its beat"
        );
    }

    /// The inter-phase wait is unbounded, and it is where `M` lives. A slow
    /// target must not shorten the transaction or complete it early.
    #[test]
    fn target_latency_extends_the_data_phase_and_nothing_else() {
        for latency in [0u32, 1, 5, 20, 100] {
            let mut t = Transaction::begin(0x1000, Width::Single32, false);
            let mut cycles = 0;
            while !t.step(latency) {
                cycles += 1;
                assert!(cycles < 1000, "transaction never completed");
            }
            cycles += 1;
            assert_eq!(
                cycles,
                Transaction::total_cycles(Width::Single32, latency),
                "latency {latency}"
            );
        }
    }

    /// Block transfers cost one bus cycle per 32-bit beat, so an I-cache line
    /// costs meaningfully more than a D-cache line at the same latency.
    #[test]
    fn block_transfers_cost_one_cycle_per_beat() {
        let m = 10;
        assert_eq!(Transaction::total_cycles(Width::Single32, m), 1 + m + 1);
        assert_eq!(Transaction::total_cycles(Width::Block128, m), 1 + m + 4);
        assert_eq!(Transaction::total_cycles(Width::Block256, m), 1 + m + 8);
        assert!(
            Transaction::total_cycles(Width::Block256, m)
                > Transaction::total_cycles(Width::Block128, m),
            "an I-cache line is twice a D-cache line on the wire"
        );
    }

    /// The transaction never completes during the address phase, whatever the
    /// latency — a device must always get the chance to observe it mid-flight.
    #[test]
    fn a_transaction_can_never_complete_in_its_address_phase() {
        for width in [
            Width::Single32,
            Width::Single64,
            Width::Block128,
            Width::Block256,
        ] {
            for latency in [0u32, 3, 50] {
                let mut t = Transaction::begin(0x2000, width, false);
                assert!(
                    !t.step(latency),
                    "{width:?} at latency {latency} completed atomically -- the \
                     whole point is that it cannot"
                );
            }
        }
    }
}
