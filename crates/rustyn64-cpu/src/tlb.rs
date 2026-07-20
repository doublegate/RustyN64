//! The joint TLB and the instruction micro-TLB (T-12-004).
//!
//! 32 fully-associative joint-TLB (JTLB) entries, each mapping an **even/odd
//! page pair**, plus a two-entry instruction micro-TLB (ITLB) in front of it.
//!
//! # The distinction that is easy to lose
//!
//! A **micro-TLB miss is a stall** (3 `PCycles`, UM §4.6.2 p. 107); a **JTLB miss
//! is an exception**. An implementation with only the JTLB does not approximate
//! the micro-TLB's cost — it deletes the structure the cost occurs in, so there
//! is nowhere left to charge it.
//!
//! # The matching rule, and the trap in it
//!
//! An entry matches when `VPN2` matches **and** (`G` is set **or** the `ASID`
//! matches). The **`V` bit does not participate** (UM §5.4.9, p. 155):
//!
//! > *"While the V bit of the entry must be set for a valid translation to take
//! > place, it is not involved in the determination of a matching TLB entry."*
//!
//! So an invalid entry still *matches* — it just raises TLB Invalid instead of
//! translating. Checking `V` during matching looks like an optimisation, passes
//! ordinary tests, and breaks two things: an invalid entry would fall through to
//! a **refill** (wrong vector, wrong handler), and TLB shutdown would stop
//! firing on duplicates involving an invalid entry, which UM Fig. 6-6 (p. 167)
//! explicitly says it must.

use crate::cop0::{Cop0, reg};

/// JTLB entries. Fully associative (UM §5.1, p. 122).
pub const JTLB_ENTRIES: usize = 32;

/// Instruction micro-TLB entries (UM §1.5.1).
pub const ITLB_ENTRIES: usize = 2;

/// The micro-TLB reload penalty, in `PCycle`s (UM §4.6.2, p. 107).
///
/// *"A miss penalty of 3 `PCycles` is incurred when the micro-TLB is updated from
/// the JTLB."* Documented, not fitted — accuracy-ledger C-7.
pub const ITLB_MISS_PCYCLES: u32 = 3;

/// One JTLB entry: a `VPN2` tag plus the even and odd page it maps.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Entry {
    /// `PageMask`, selecting the page size.
    pub page_mask: u32,
    /// Virtual page number / 2 — the pair's tag.
    pub vpn2: u64,
    /// Address-space identifier.
    pub asid: u8,
    /// Global: ignore `ASID` when matching.
    ///
    /// Derived on write as `EntryLo0.G AND EntryLo1.G` (UM Fig. 5-10, p. 145),
    /// which is why it lives on the entry rather than per-page.
    pub global: bool,
    /// The `R` field (bits 63:62 of `EntryHi`) — the 64-bit address region.
    pub region: u8,
    /// Even page: `EntryLo0`.
    pub lo0: PageEntry,
    /// Odd page: `EntryLo1`.
    pub lo1: PageEntry,
}

/// One half of an entry — the even or odd page.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PageEntry {
    /// Page frame number.
    pub pfn: u32,
    /// Cache coherency attribute. **Only `2` means uncached** (UM Table 5-6,
    /// p. 145); 0, 1, 3, 4, 5, 6 and 7 are all cached, because the VR4300 has no
    /// coherency protocol and the VR4400's finer encodings collapse.
    pub c: u8,
    /// Dirty — meaning **writable**, not "has been written". A store to a page
    /// with `D` clear raises TLB Modified.
    pub dirty: bool,
    /// Valid. Does **not** participate in matching; see the module docs.
    pub valid: bool,
}

impl PageEntry {
    /// Is this page uncached?
    #[must_use]
    pub const fn uncached(self) -> bool {
        self.c == 2
    }
}

/// Why a translation failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TlbFault {
    /// No entry matched — TLB refill (`TLBL`/`TLBS`, refill vector).
    Refill,
    /// An entry matched but its `V` bit is clear — TLB Invalid (`TLBL`/`TLBS`,
    /// **general** vector).
    Invalid,
    /// A store to a matching, valid page whose `D` bit is clear — TLB Modified.
    Modified,
}

/// A successful translation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Translated {
    /// The physical address.
    pub addr: u32,
    /// Whether the access bypasses the caches.
    pub uncached: bool,
}

/// The joint TLB plus its instruction micro-TLB.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tlb {
    /// The 32 joint entries.
    entries: [Entry; JTLB_ENTRIES],
    /// Micro-ITLB: indices into `entries`, plus an LRU bit.
    ///
    /// Holds *indices* rather than copies so a `TLBWI` cannot leave the ITLB
    /// serving a stale mapping — a real hazard on hardware that software must
    /// handle, but not one worth reproducing by accident.
    itlb: [Option<usize>; ITLB_ENTRIES],
    /// Which ITLB way is least recently used (UM §1.5.1 specifies LRU).
    itlb_lru: usize,
    /// TLB shutdown: two or more entries matched, and the TLB is now unusable
    /// until reset (UM §5.1, p. 122).
    shutdown: bool,
}

impl Default for Tlb {
    fn default() -> Self {
        Self::new()
    }
}

impl Tlb {
    /// A TLB in its power-on state.
    ///
    /// The manual calls the reset contents **undefined** (UM §6.4.4, p. 183) and
    /// ADR 0004 requires reproducibility, so a fixed state must be chosen — but
    /// **all-zero is not a usable choice**, and that is not obvious.
    ///
    /// Zeroing gives all 32 entries `VPN2 = 0` and `ASID = 0`. Since `V` does
    /// not participate in matching, *any* access to virtual page-pair 0 then
    /// matches all 32 entries at once, which is the TLB-shutdown condition — so
    /// the very first KUSEG access to low memory would brick the TLB.
    ///
    /// Each entry therefore gets a **distinct** `VPN2` near the top of the
    /// field, so no two coincide and none sits where software is likely to look.
    /// Recorded as accuracy-ledger **D-4**: a deliberate deviation, chosen
    /// because real hardware powers up with *arbitrary* contents that do not
    /// coincide, and zero is the one arbitrary value that does.
    #[must_use]
    pub const fn new() -> Self {
        let mut t = Self {
            entries: [Entry {
                page_mask: 0,
                vpn2: 0,
                asid: 0,
                global: false,
                region: 0,
                lo0: PageEntry {
                    pfn: 0,
                    c: 0,
                    dirty: false,
                    valid: false,
                },
                lo1: PageEntry {
                    pfn: 0,
                    c: 0,
                    dirty: false,
                    valid: false,
                },
            }; JTLB_ENTRIES],
            itlb: [None; ITLB_ENTRIES],
            itlb_lru: 0,
            shutdown: false,
        };
        // Distinct, non-coinciding tags. `VPN2` is 27 bits, so counting down
        // from its maximum keeps them clear of anything a program maps.
        let mut i = 0;
        while i < JTLB_ENTRIES {
            t.entries[i].vpn2 = (0x7FF_FFFF - i) as u64;
            i += 1;
        }
        t
    }

    /// Has the TLB shut down? *"the processor must be reset to restart"*
    /// (UM Fig. 6-6, p. 167).
    #[must_use]
    pub const fn is_shutdown(&self) -> bool {
        self.shutdown
    }

    /// Read an entry (for `TLBR` and for tests).
    #[must_use]
    pub const fn entry(&self, i: usize) -> Entry {
        self.entries[i & (JTLB_ENTRIES - 1)]
    }

    /// The page size an entry maps, in bytes, from its `PageMask`.
    ///
    /// `PageMask` bits 24:13 select 4K…16M (UM Table 5-7, p. 149). *"When the
    /// Mask field is not one of the values shown in Table 5-7, the operation of
    /// the TLB is undefined"* — we take the mask at face value, which is a
    /// documented-undefined choice rather than a hardware fact.
    #[must_use]
    pub const fn page_size(mask: u32) -> u64 {
        // `(mask | 0x1FFF) + 1` is the size of the PAIR; one page is half that.
        // Mask 0 gives a 0x2000 pair and a 0x1000 page, which is the 4 KiB row
        // of Table 5-7. Returning the pair size here is an easy off-by-one-bit
        // that makes every entry cover twice its range and match twice as often.
        Self::pair_size(mask) >> 1
    }

    /// The size of the even/odd **pair** an entry covers — twice
    /// [`Tlb::page_size`], and the granularity the `VPN2` tag is compared at.
    #[must_use]
    pub const fn pair_size(mask: u32) -> u64 {
        ((mask as u64) | 0x1FFF) + 1
    }

    /// Look up a virtual address.
    ///
    /// `store` selects the `D`-bit check; `asid` is the current `EntryHi.ASID`.
    ///
    /// # Errors
    ///
    /// [`TlbFault`] describing which of the three TLB exceptions to raise.
    pub fn lookup(&mut self, vaddr: u64, asid: u8, store: bool) -> Result<Translated, TlbFault> {
        let mut found: Option<usize> = None;
        for (i, e) in self.entries.iter().enumerate() {
            if !Self::matches(e, vaddr, asid) {
                continue;
            }
            if found.is_some() {
                // "If there are two or more TLB entries that coincide, the TLB
                // operation is not correctly executed. In this case, the
                // TLB-Shutdown (TS) bit of the status register is set to 1, and
                // then the TLB cannot be used" (UM §5.1, p. 122).
                //
                // Reachable with an INVALID duplicate, because V is not part of
                // matching -- UM Fig. 6-6 says so explicitly.
                self.shutdown = true;
                return Err(TlbFault::Refill);
            }
            found = Some(i);
        }
        let i = found.ok_or(TlbFault::Refill)?;
        let e = &self.entries[i];

        // Which half of the pair? The bit just above the page-size field.
        let size = Self::page_size(e.page_mask);
        let page = if (vaddr & size) == 0 { e.lo0 } else { e.lo1 };

        if !page.valid {
            // Matched but invalid: TLB Invalid, which takes the GENERAL vector,
            // not the refill vector. Treating it as a miss would send the
            // handler to the wrong place.
            return Err(TlbFault::Invalid);
        }
        if store && !page.dirty {
            // "Dirty" means WRITABLE here, not "has been written".
            return Err(TlbFault::Modified);
        }
        // PFN is always in **4 KiB units**, whatever the page size — so a large
        // page's frame number has low bits that must be masked off rather than
        // scaled. Multiplying by `size` instead would place a 16 KiB page four
        // times too high in physical memory.
        let offset = vaddr & (size - 1);
        let base = ((page.pfn as u64) << 12) & !(size - 1);
        Ok(Translated {
            addr: (base | offset) as u32,
            uncached: page.uncached(),
        })
    }

    /// Does this entry match?
    ///
    /// `VPN2` **and** (`G` **or** `ASID`). `V` is deliberately absent — see the
    /// module docs for why including it breaks two separate behaviours.
    fn matches(e: &Entry, vaddr: u64, asid: u8) -> bool {
        // The tag is compared at PAIR granularity, not page granularity.
        if (vaddr / Self::pair_size(e.page_mask)) != e.vpn2 {
            return false;
        }
        e.global || e.asid == asid
    }

    /// `TLBWI` / `TLBWR` — write `EntryHi`/`EntryLo0`/`EntryLo1`/`PageMask` into
    /// entry `index`.
    pub fn write_entry(&mut self, index: usize, cop0: &Cop0) {
        let hi = cop0.read(reg::ENTRY_HI);
        let lo0 = cop0.read(reg::ENTRY_LO0);
        let lo1 = cop0.read(reg::ENTRY_LO1);
        let mask = cop0.read(reg::PAGE_MASK) as u32;

        self.entries[index & (JTLB_ENTRIES - 1)] = Entry {
            page_mask: mask,
            vpn2: (hi & 0x0000_00FF_FFFF_E000) / Self::pair_size(mask),
            asid: (hi & 0xFF) as u8,
            // "If this bit is set in BOTH EntryLo0 and EntryLo1, then the
            // processor ignores the ASID during TLB lookup" (UM Fig. 5-10,
            // p. 145). An OR here would make far too many entries global.
            global: (lo0 & 1) != 0 && (lo1 & 1) != 0,
            region: ((hi >> 62) & 0b11) as u8,
            lo0: Self::page_from(lo0),
            lo1: Self::page_from(lo1),
        };
        // A write invalidates the micro-TLB, which caches indices into this
        // array. Cheaper and safer than tracking which way held `index`.
        self.itlb = [None; ITLB_ENTRIES];
    }

    /// Decode an `EntryLo` into a page.
    const fn page_from(lo: u64) -> PageEntry {
        PageEntry {
            pfn: ((lo >> 6) & 0x000F_FFFF) as u32,
            c: ((lo >> 3) & 0b111) as u8,
            dirty: (lo >> 2) & 1 != 0,
            valid: (lo >> 1) & 1 != 0,
        }
    }

    /// `TLBR` — read entry `index` back into the COP0 registers.
    pub fn read_entry(&self, index: usize, cop0: &mut Cop0) {
        let e = self.entries[index & (JTLB_ENTRIES - 1)];
        cop0.set_hardware(reg::PAGE_MASK, u64::from(e.page_mask));
        cop0.set_hardware(
            reg::ENTRY_HI,
            ((e.region as u64) << 62) | (e.vpn2 * Self::pair_size(e.page_mask)) | u64::from(e.asid),
        );
        // `EntryHi` has no G field, so the entry's G is written back into BOTH
        // EntryLo halves -- which is the inverse of the AND applied on write.
        cop0.set_hardware(reg::ENTRY_LO0, Self::lo_from(e.lo0, e.global));
        cop0.set_hardware(reg::ENTRY_LO1, Self::lo_from(e.lo1, e.global));
    }

    /// Encode a page back into an `EntryLo` value.
    const fn lo_from(p: PageEntry, global: bool) -> u64 {
        ((p.pfn as u64) << 6)
            | ((p.c as u64) << 3)
            | ((p.dirty as u64) << 2)
            | ((p.valid as u64) << 1)
            | global as u64
    }

    /// `TLBP` — probe for an entry matching the current `EntryHi`.
    ///
    /// Sets `Index` to the matching index, or sets `Index.P` (bit 31) on a miss:
    /// *"Set to 1 when the previous `TLBProbe` (`TLBP`) instruction was
    /// unsuccessful"* (UM §5.4.1, p. 146).
    ///
    /// **What the low bits hold on a miss is undocumented** (accuracy-ledger
    /// U-2). This implementation leaves them zero — a guess, not a fact.
    pub fn probe(&self, cop0: &mut Cop0) {
        let hi = cop0.read(reg::ENTRY_HI);
        let asid = (hi & 0xFF) as u8;
        for (i, e) in self.entries.iter().enumerate() {
            if Self::matches(e, hi & 0x0000_00FF_FFFF_E000, asid) {
                cop0.set_hardware(reg::INDEX, i as u64);
                return;
            }
        }
        cop0.set_hardware(reg::INDEX, 1 << 31);
    }

    /// Probe the instruction micro-TLB, reporting whether it hit.
    ///
    /// A miss costs [`ITLB_MISS_PCYCLES`] and is a **stall**, not an exception —
    /// the JTLB is then consulted, and only a JTLB miss raises.
    pub fn itlb_probe(&mut self, vaddr: u64, asid: u8) -> bool {
        for slot in 0..ITLB_ENTRIES {
            if let Some(i) = self.itlb[slot]
                && Self::matches(&self.entries[i], vaddr, asid)
            {
                self.itlb_lru = 1 - slot;
                return true;
            }
        }
        false
    }

    /// Fill a micro-TLB way from the JTLB after a miss.
    pub fn itlb_fill(&mut self, vaddr: u64, asid: u8) {
        for (i, e) in self.entries.iter().enumerate() {
            if Self::matches(e, vaddr, asid) {
                let way = self.itlb_lru;
                self.itlb[way] = Some(i);
                self.itlb_lru = 1 - way;
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a COP0 primed to write one entry, then write it.
    fn install(tlb: &mut Tlb, index: usize, vpn2_addr: u64, asid: u8, lo0: u64, lo1: u64) {
        let mut c = Cop0::new();
        c.set_hardware(reg::PAGE_MASK, 0);
        c.set_hardware(reg::ENTRY_HI, vpn2_addr | u64::from(asid));
        c.set_hardware(reg::ENTRY_LO0, lo0);
        c.set_hardware(reg::ENTRY_LO1, lo1);
        tlb.write_entry(index, &c);
    }

    /// `V | D | C=3`, i.e. a valid writable cached page at `pfn`.
    const fn lo(pfn: u64) -> u64 {
        (pfn << 6) | (3 << 3) | 0b110
    }

    #[test]
    fn a_matching_valid_entry_translates() {
        let mut t = Tlb::new();
        install(&mut t, 0, 0x0000_2000, 0, lo(0x100), lo(0x101));
        // Even page of the pair.
        let r = t.lookup(0x0000_2000, 0, false).expect("hit");
        assert_eq!(r.addr, 0x100 * 0x1000);
        // Odd page.
        let r = t.lookup(0x0000_3000, 0, false).expect("hit");
        assert_eq!(r.addr, 0x101 * 0x1000);
        // Offset within the page is preserved.
        let r = t.lookup(0x0000_2ABC, 0, false).expect("hit");
        assert_eq!(r.addr, 0x100 * 0x1000 + 0xABC);
    }

    #[test]
    fn no_matching_entry_is_a_refill() {
        let mut t = Tlb::new();
        install(&mut t, 0, 0x0000_2000, 0, lo(0x100), lo(0x101));
        assert_eq!(t.lookup(0x0040_0000, 0, false), Err(TlbFault::Refill));
    }

    /// **The `V`-bit rule.** An invalid entry still *matches*, so it raises TLB
    /// Invalid (general vector) rather than falling through to a refill (refill
    /// vector). Checking `V` while matching sends the handler to the wrong
    /// place, and passes any test that only checks "it failed".
    #[test]
    fn an_invalid_entry_matches_and_raises_invalid_not_refill() {
        let mut t = Tlb::new();
        // V clear on the even page.
        install(
            &mut t,
            0,
            0x0000_2000,
            0,
            (0x100 << 6) | (3 << 3) | 0b100,
            lo(0x101),
        );
        assert_eq!(
            t.lookup(0x0000_2000, 0, false),
            Err(TlbFault::Invalid),
            "matched-but-invalid is NOT a refill"
        );
        // The odd page of the same pair is still fine.
        assert!(t.lookup(0x0000_3000, 0, false).is_ok());
    }

    /// `D` means **writable**. A store to a clean page raises TLB Modified; a
    /// load from it does not.
    #[test]
    fn a_store_to_a_non_dirty_page_raises_modified() {
        let mut t = Tlb::new();
        install(
            &mut t,
            0,
            0x0000_2000,
            0,
            (0x100 << 6) | (3 << 3) | 0b010,
            lo(0x101),
        );
        assert!(t.lookup(0x0000_2000, 0, false).is_ok(), "loads are fine");
        assert_eq!(
            t.lookup(0x0000_2000, 0, true),
            Err(TlbFault::Modified),
            "stores are not"
        );
    }

    /// `ASID` gates matching unless `G` is set.
    #[test]
    fn asid_gates_matching_unless_global() {
        let mut t = Tlb::new();
        install(&mut t, 0, 0x0000_2000, 7, lo(0x100), lo(0x101));
        assert!(t.lookup(0x0000_2000, 7, false).is_ok(), "matching ASID");
        assert_eq!(
            t.lookup(0x0000_2000, 8, false),
            Err(TlbFault::Refill),
            "different ASID does not match"
        );

        // Now global: G set in BOTH halves.
        let mut t = Tlb::new();
        install(&mut t, 0, 0x0000_2000, 7, lo(0x100) | 1, lo(0x101) | 1);
        assert!(
            t.lookup(0x0000_2000, 99, false).is_ok(),
            "G ignores the ASID"
        );
    }

    /// `G` is the **AND** of the two halves (UM Fig. 5-10). An OR would make far
    /// too many entries global, and global entries match every ASID — so the bug
    /// shows up as address-space leakage, not as a missing translation.
    #[test]
    fn global_is_the_and_of_both_halves_not_the_or() {
        let mut t = Tlb::new();
        install(&mut t, 0, 0x0000_2000, 7, lo(0x100) | 1, lo(0x101));
        assert!(!t.entry(0).global, "one half set is not global");
        assert_eq!(
            t.lookup(0x0000_2000, 99, false),
            Err(TlbFault::Refill),
            "and so a foreign ASID must not match"
        );
    }

    /// Two coinciding entries shut the TLB down (UM §5.1, p. 122), **including
    /// when one is invalid** (UM Fig. 6-6, p. 167) — which is the same fact as
    /// the `V`-not-in-matching rule, seen from the other side.
    #[test]
    fn duplicate_entries_trigger_tlb_shutdown_even_when_one_is_invalid() {
        let mut t = Tlb::new();
        install(&mut t, 0, 0x0000_2000, 0, lo(0x100), lo(0x101));
        // Same VPN2, and deliberately INVALID.
        install(&mut t, 5, 0x0000_2000, 0, (0x200 << 6) | (3 << 3), 0);
        assert!(!t.is_shutdown(), "not until a lookup notices");
        let _ = t.lookup(0x0000_2000, 0, false);
        assert!(
            t.is_shutdown(),
            "an invalid duplicate must still cause shutdown"
        );
    }

    /// `TLBP` reports the index, and sets `Index.P` on a miss.
    #[test]
    fn tlbp_reports_the_index_or_sets_the_probe_failure_bit() {
        let mut t = Tlb::new();
        install(&mut t, 9, 0x0000_2000, 0, lo(0x100), lo(0x101));

        let mut c = Cop0::new();
        c.set_hardware(reg::ENTRY_HI, 0x0000_2000);
        t.probe(&mut c);
        assert_eq!(c.read(reg::INDEX), 9);

        c.set_hardware(reg::ENTRY_HI, 0x0080_0000);
        t.probe(&mut c);
        assert_ne!(
            c.read(reg::INDEX) & (1 << 31),
            0,
            "Index.P set on a failed probe"
        );
    }

    /// `TLBR` round-trips an entry, and puts the entry's `G` back into **both**
    /// `EntryLo` halves — `EntryHi` has no `G` field to hold it.
    #[test]
    fn tlbr_round_trips_and_restores_g_to_both_halves() {
        let mut t = Tlb::new();
        install(&mut t, 3, 0x0000_2000, 7, lo(0x100) | 1, lo(0x101) | 1);

        let mut c = Cop0::new();
        t.read_entry(3, &mut c);
        assert_eq!(c.read(reg::ENTRY_HI) & 0xFF, 7, "ASID");
        assert_eq!(c.read(reg::ENTRY_HI) & 0xFFFF_E000, 0x0000_2000, "VPN2");
        assert_eq!(c.read(reg::ENTRY_LO0) & 1, 1, "G restored to lo0");
        assert_eq!(c.read(reg::ENTRY_LO1) & 1, 1, "and to lo1");
        assert_eq!((c.read(reg::ENTRY_LO0) >> 6) & 0xF_FFFF, 0x100, "PFN");
    }

    /// Larger page sizes map larger regions, and the even/odd split moves with
    /// the size rather than staying at 4K.
    #[test]
    fn page_mask_selects_the_page_size_and_moves_the_even_odd_split() {
        let mut t = Tlb::new();
        let mut c = Cop0::new();
        // 16K pages: PageMask bits 24:13 = 0b000000000011.
        c.set_hardware(reg::PAGE_MASK, 0b11 << 13);
        c.set_hardware(reg::ENTRY_HI, 0x0001_0000);
        c.set_hardware(reg::ENTRY_LO0, lo(0x100));
        c.set_hardware(reg::ENTRY_LO1, lo(0x200));
        t.write_entry(0, &c);

        assert_eq!(Tlb::page_size(0b11 << 13), 0x4000, "16 KiB page");
        assert_eq!(Tlb::pair_size(0b11 << 13), 0x8000, "32 KiB pair");
        // PFN is in 4 KiB units regardless of page size, and its low bits are
        // masked off rather than scaled.
        let r = t.lookup(0x0001_0000, 0, false).expect("even page");
        assert_eq!(r.addr, 0x100 << 12);
        // The split is at 16K now, not 4K.
        let r = t.lookup(0x0001_4000, 0, false).expect("odd page");
        assert_eq!(r.addr, (0x200 << 12) & !0x3FFF);
    }

    /// Only `C == 2` is uncached; the VR4400's other coherency encodings all
    /// collapse to "cached" on a part with no coherency protocol.
    #[test]
    fn only_cache_attribute_two_is_uncached() {
        for c in 0..8u8 {
            let p = PageEntry {
                pfn: 0,
                c,
                dirty: true,
                valid: true,
            };
            assert_eq!(p.uncached(), c == 2, "C = {c}");
        }
    }

    /// A micro-TLB miss is a **stall**, not an exception: it consults the JTLB
    /// and fills. Only a JTLB miss raises.
    #[test]
    fn the_micro_itlb_misses_then_fills_without_raising() {
        let mut t = Tlb::new();
        install(&mut t, 0, 0x0000_2000, 0, lo(0x100), lo(0x101));
        assert!(!t.itlb_probe(0x0000_2000, 0), "cold: a miss");
        t.itlb_fill(0x0000_2000, 0);
        assert!(t.itlb_probe(0x0000_2000, 0), "warm: a hit");
        assert_eq!(ITLB_MISS_PCYCLES, 3, "UM §4.6.2 p.107");
    }

    /// Writing an entry must invalidate the micro-TLB, which caches indices.
    #[test]
    fn writing_an_entry_invalidates_the_micro_itlb() {
        let mut t = Tlb::new();
        install(&mut t, 0, 0x0000_2000, 0, lo(0x100), lo(0x101));
        t.itlb_fill(0x0000_2000, 0);
        assert!(t.itlb_probe(0x0000_2000, 0));
        install(&mut t, 1, 0x0000_8000, 0, lo(0x300), lo(0x301));
        assert!(
            !t.itlb_probe(0x0000_2000, 0),
            "a TLB write must not leave the ITLB serving a stale mapping"
        );
    }

    /// **All-zero is not a usable reset state**, and the reason is subtle: with
    /// every entry at `VPN2 = 0` and `V` not participating in matching, the
    /// first access to page-pair 0 matches all 32 entries and shuts the TLB
    /// down. Entries must therefore start distinct.
    #[test]
    fn a_fresh_tlb_does_not_shut_down_on_the_first_low_access() {
        let mut t = Tlb::new();
        assert_eq!(t.lookup(0x0000_0000, 0, false), Err(TlbFault::Refill));
        assert!(
            !t.is_shutdown(),
            "a power-on TLB must not self-destruct on its first lookup"
        );
        // And the tags really are distinct, which is what guarantees it.
        for i in 0..JTLB_ENTRIES {
            for j in (i + 1)..JTLB_ENTRIES {
                assert_ne!(
                    t.entry(i).vpn2,
                    t.entry(j).vpn2,
                    "entries {i} and {j} coincide at reset"
                );
            }
        }
    }
}
