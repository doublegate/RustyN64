//! The VR4300 primary caches (T-11-003).
//!
//! Two direct-mapped, physically-tagged caches sit between the CPU and the bus:
//! a **16 KiB instruction cache** with 32-byte lines and an **8 KiB write-back
//! data cache** with 16-byte lines (UM §11.2, Tables 11-1/11-2).
//!
//! # Why they are modelled at all
//!
//! They were a deliberate no-op until now (accuracy ledger D-5): with no cache
//! contents, invalidate and write-back had nothing to act on, which is
//! observationally sound *only* while nothing can observe staleness. That stops
//! being true the moment a program writes a location through one path and reads
//! it through another — which is precisely what n64-systemtest's `DCACHE:` and
//! `ICACHE:` groups do, and they are the reason this exists.
//!
//! # Indexing is PHYSICAL here, virtual on hardware
//!
//! The real caches are virtually indexed and physically tagged, so two virtual
//! addresses mapping to one physical address can occupy two lines (a cache
//! alias). Indexing by physical address instead makes aliases impossible.
//!
//! This is a **deviation, not a simplification that is strictly safer**. Software
//! that observes aliasing — or that relies on an `Index_*` operation selecting a
//! line by *virtual* index on a TLB-mapped page — sees different behaviour here,
//! because translation preserves only the low 12 bits while the D-cache index
//! reaches bit 12 and the I-cache bit 13. What is bounded is the tested scope:
//! every test that motivated this module operates through KSEG0, where the two
//! indexings coincide. Accuracy ledger **D-6**.
//!
//! # `TagLo`
//!
//! `Index_Load_Tag` and `Index_Store_Tag` move a line's tag through COP0
//! `TagLo`, whose layout differs per cache (UM §5.3, Figures 5-19/5-20):
//!
//! ```text
//!   bits 27..=8   PTagLo — the physical frame number, PA(31:12)
//!   bits  7..=6   PState — I-cache: 2 = Valid, 0 = Invalid
//!                          D-cache: 3 = Valid, 0 = Invalid
//! ```
//!
//! The D-cache's write-back ("dirty") bit is **not** in `TagLo`, so a clean and a
//! dirty valid line are indistinguishable to `Index_Load_Tag`. That is hardware
//! behaviour, not an omission — see [`Dcache::load_tag`].

/// Instruction-cache line size, in bytes (UM §11.2).
pub const ICACHE_LINE: u32 = 32;
/// Data-cache line size, in bytes (UM §11.2).
pub const DCACHE_LINE: u32 = 16;
/// Lines in the 16 KiB instruction cache.
pub const ICACHE_LINES: usize = 512;
/// Lines in the 8 KiB data cache.
pub const DCACHE_LINES: usize = 512;

/// `PState` for a valid I-cache line.
const ICACHE_VALID_STATE: u32 = 2;
/// `PState` for a valid D-cache line.
const DCACHE_VALID_STATE: u32 = 3;

/// One cache line's tag and data.
///
/// `tag` holds the physical frame number, PA(31:12), and survives invalidation:
/// `Index_Invalidate` clears `valid` and leaves `tag` alone, which is directly
/// observable through `Index_Load_Tag` and is asserted by n64-systemtest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Line<const N: usize> {
    /// Physical frame number, PA(31:12).
    tag: u32,
    /// Whether the line holds valid data.
    valid: bool,
    /// Whether the line has been written since it was filled (D-cache only).
    dirty: bool,
    /// The line's bytes, in memory order.
    data: [u8; N],
}

impl<const N: usize> Line<N> {
    const EMPTY: Self = Self {
        tag: 0,
        valid: false,
        dirty: false,
        data: [0; N],
    };
}

/// Pack a tag into the `TagLo` layout.
const fn pack_tag(tag: u32, valid: bool, valid_state: u32) -> u32 {
    let pstate = if valid { valid_state } else { 0 };
    (pstate << 6) | ((tag & 0x000F_FFFF) << 8)
}

/// Unpack a `TagLo` value into `(tag, valid)`.
///
/// `valid` requires the cache's **own** `PState` encoding — 2 for the I-cache,
/// 3 for the D-cache — not merely a non-zero field. The two reserved values (1,
/// and 2-vs-3 crossed between the caches) are not "valid": treating any non-zero
/// `PState` as valid would let `Index_Store_Tag` conjure a live line out of an
/// encoding the hardware does not define.
const fn unpack_tag(tag_lo: u32, valid_state: u32) -> (u32, bool) {
    (
        (tag_lo >> 8) & 0x000F_FFFF,
        (tag_lo >> 6) & 3 == valid_state,
    )
}

/// What a cache operation needs the caller to do on its behalf.
///
/// The caches deliberately do **not** hold a bus handle: `rustyn64-cpu` sees the
/// bus only as a `&mut B` borrowed for the duration of a step, and threading it
/// into the cache would widen that borrow across the whole pipeline. Instead a
/// fill or write-back is described here and performed by the caller, which
/// already has the bus.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Writeback<const N: usize> {
    /// Physical address of the line's first byte.
    pub addr: u32,
    /// The line's bytes.
    pub data: [u8; N],
}

/// The 8 KiB write-back data cache.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Dcache {
    lines: [Line<16>; DCACHE_LINES],
}

impl Default for Dcache {
    fn default() -> Self {
        Self::new()
    }
}

impl Dcache {
    /// A cache with every line invalid.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            lines: [Line::EMPTY; DCACHE_LINES],
        }
    }

    /// Which line a physical address maps to.
    const fn index(addr: u32) -> usize {
        ((addr / DCACHE_LINE) as usize) % DCACHE_LINES
    }

    /// The physical address of the line currently resident at `index`.
    ///
    /// Reconstructed from the stored tag plus the index bits the tag does not
    /// carry — a write-back must go to the address the line came *from*, not to
    /// whatever address provoked the eviction.
    const fn resident_addr(&self, index: usize) -> u32 {
        let within_page = (index as u32 * DCACHE_LINE) & 0xFFF;
        (self.lines[index].tag << 12) | within_page
    }

    /// Does `addr` hit a valid line?
    #[must_use]
    pub const fn hits(&self, addr: u32) -> bool {
        let i = Self::index(addr);
        self.lines[i].valid && self.lines[i].tag == addr >> 12
    }

    /// Prepare the line covering `addr` for access, reporting what the caller
    /// must do to the bus first.
    ///
    /// Returns the eviction that must be written back (if any). After the
    /// caller has performed it and supplied the fill data through
    /// [`Dcache::install`], the line is resident.
    #[must_use]
    pub const fn miss_plan(&self, addr: u32) -> Option<Option<Writeback<16>>> {
        let i = Self::index(addr);
        if self.lines[i].valid && self.lines[i].tag == addr >> 12 {
            return None;
        }
        if self.lines[i].valid && self.lines[i].dirty {
            Some(Some(Writeback {
                addr: self.resident_addr(i),
                data: self.lines[i].data,
            }))
        } else {
            Some(None)
        }
    }

    /// Install a freshly filled line.
    pub const fn install(&mut self, addr: u32, data: [u8; 16]) {
        let i = Self::index(addr);
        self.lines[i] = Line {
            tag: addr >> 12,
            valid: true,
            dirty: false,
            data,
        };
    }

    /// Read `len` bytes at `addr` from a resident line.
    ///
    /// The caller must have made the line resident first. An access never
    /// straddles two lines: every access reaching the cache is naturally
    /// aligned to its own width, and the widest is 8 bytes into a 16-byte line.
    #[must_use]
    pub fn read(&self, addr: u32, len: usize) -> u64 {
        let i = Self::index(addr);
        let o = (addr % DCACHE_LINE) as usize;
        let mut v = 0u64;
        for k in 0..len {
            v = (v << 8) | u64::from(self.lines[i].data[o + k]);
        }
        v
    }

    /// Write the low `len` bytes of `value` at `addr` into a resident line,
    /// marking it dirty.
    pub fn write(&mut self, addr: u32, len: usize, value: u64) {
        let i = Self::index(addr);
        let o = (addr % DCACHE_LINE) as usize;
        for k in 0..len {
            let shift = 8 * (len - 1 - k);
            self.lines[i].data[o + k] = (value >> shift) as u8;
        }
        self.lines[i].dirty = true;
    }

    /// `Index_Load_Tag`: the tag at the index `addr` selects, in `TagLo` form.
    ///
    /// A dirty line and a clean one both report `PState = 3`. The write-back bit
    /// has no `TagLo` field on this part, so the distinction is genuinely not
    /// visible to software — reporting it would be inventing an encoding.
    #[must_use]
    pub const fn load_tag(&self, addr: u32) -> u32 {
        let i = Self::index(addr);
        pack_tag(self.lines[i].tag, self.lines[i].valid, DCACHE_VALID_STATE)
    }

    /// `Index_Store_Tag`: overwrite the tag at the index `addr` selects.
    pub const fn store_tag(&mut self, addr: u32, tag_lo: u32) {
        let i = Self::index(addr);
        let (tag, valid) = unpack_tag(tag_lo, DCACHE_VALID_STATE);
        self.lines[i].tag = tag;
        self.lines[i].valid = valid;
        self.lines[i].dirty = false;
    }

    /// Clear the valid bit at `index`, keeping the tag.
    const fn invalidate_index(&mut self, i: usize) {
        self.lines[i].valid = false;
        self.lines[i].dirty = false;
    }

    /// Take the line at the index `addr` selects for write-back, if it is dirty.
    ///
    /// `clean` clears the dirty bit (`Hit_Write_Back` leaves the line resident);
    /// `invalidate` additionally clears the valid bit.
    pub const fn flush_index(
        &mut self,
        addr: u32,
        invalidate: bool,
        clean: bool,
    ) -> Option<Writeback<16>> {
        let i = Self::index(addr);
        let out = if self.lines[i].valid && self.lines[i].dirty {
            Some(Writeback {
                addr: self.resident_addr(i),
                data: self.lines[i].data,
            })
        } else {
            None
        };
        if clean {
            self.lines[i].dirty = false;
        }
        if invalidate {
            self.invalidate_index(i);
        }
        out
    }

    /// `Create_Dirty_Exclusive`: claim the line for `addr` without filling it.
    ///
    /// Returns any dirty line evicted in the process. The new line's data is
    /// whatever the old line held — the operation exists precisely so software
    /// can avoid the fill when it is about to overwrite the whole line, so
    /// leaving stale bytes is the point, not an oversight.
    pub const fn create_dirty_exclusive(&mut self, addr: u32) -> Option<Writeback<16>> {
        let i = Self::index(addr);
        let out = if self.lines[i].valid && self.lines[i].dirty && self.lines[i].tag != addr >> 12 {
            Some(Writeback {
                addr: self.resident_addr(i),
                data: self.lines[i].data,
            })
        } else {
            None
        };
        self.lines[i].tag = addr >> 12;
        self.lines[i].valid = true;
        self.lines[i].dirty = true;
        out
    }
}

/// The 16 KiB instruction cache.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Icache {
    lines: [Line<32>; ICACHE_LINES],
}

impl Default for Icache {
    fn default() -> Self {
        Self::new()
    }
}

impl Icache {
    /// A cache with every line invalid.
    #[must_use]
    #[expect(
        clippy::large_stack_arrays,
        reason = "a cache is a fixed hardware structure that lives inside System \
                  for the whole run, not a stack temporary; boxing it would cost \
                  Pipeline::new its const-ness for no benefit"
    )]
    pub const fn new() -> Self {
        Self {
            lines: [Line::EMPTY; ICACHE_LINES],
        }
    }

    /// Which line a physical address maps to.
    const fn index(addr: u32) -> usize {
        ((addr / ICACHE_LINE) as usize) % ICACHE_LINES
    }

    /// The physical address of the line currently resident at `index`.
    const fn resident_addr(&self, index: usize) -> u32 {
        let within_page = (index as u32 * ICACHE_LINE) & 0xFFF;
        (self.lines[index].tag << 12) | within_page
    }

    /// Does `addr` hit a valid line?
    #[must_use]
    pub const fn hits(&self, addr: u32) -> bool {
        let i = Self::index(addr);
        self.lines[i].valid && self.lines[i].tag == addr >> 12
    }

    /// The instruction word at `addr` from a resident line.
    #[must_use]
    pub const fn read_word(&self, addr: u32) -> u32 {
        let i = Self::index(addr);
        let o = (addr % ICACHE_LINE) as usize;
        u32::from_be_bytes([
            self.lines[i].data[o],
            self.lines[i].data[o + 1],
            self.lines[i].data[o + 2],
            self.lines[i].data[o + 3],
        ])
    }

    /// Install a freshly filled line.
    ///
    /// The I-cache is read-only to the CPU, so a fill never evicts anything that
    /// needs writing back — only `Hit_Write_Back` can move data outward, and it
    /// is explicitly requested.
    pub const fn install(&mut self, addr: u32, data: [u8; 32]) {
        let i = Self::index(addr);
        self.lines[i] = Line {
            tag: addr >> 12,
            valid: true,
            dirty: false,
            data,
        };
    }

    /// `Index_Load_Tag`, in `TagLo` form.
    #[must_use]
    pub const fn load_tag(&self, addr: u32) -> u32 {
        let i = Self::index(addr);
        pack_tag(self.lines[i].tag, self.lines[i].valid, ICACHE_VALID_STATE)
    }

    /// `Index_Store_Tag`: overwrite the tag at the index `addr` selects.
    pub const fn store_tag(&mut self, addr: u32, tag_lo: u32) {
        let i = Self::index(addr);
        let (tag, valid) = unpack_tag(tag_lo, ICACHE_VALID_STATE);
        self.lines[i].tag = tag;
        self.lines[i].valid = valid;
    }

    /// Clear the valid bit at the index `addr` selects, keeping the tag.
    pub const fn invalidate_index(&mut self, addr: u32) {
        let i = Self::index(addr);
        self.lines[i].valid = false;
    }

    /// Invalidate the line covering `addr`, but only if it is resident.
    pub const fn hit_invalidate(&mut self, addr: u32) {
        if self.hits(addr) {
            self.invalidate_index(addr);
        }
    }

    /// `Hit_Write_Back`: push a resident line's contents out to memory.
    ///
    /// The I-cache has no dirty bit and the CPU never writes it, so on hardware
    /// this can only ever rewrite memory with what it already held — unless
    /// memory was changed underneath it, which is exactly the case
    /// n64-systemtest constructs.
    #[must_use]
    pub const fn flush_hit(&self, addr: u32) -> Option<Writeback<32>> {
        if !self.hits(addr) {
            return None;
        }
        let i = Self::index(addr);
        Some(Writeback {
            addr: self.resident_addr(i),
            data: self.lines[i].data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fill_then_read_returns_the_filled_bytes() {
        let mut d = Dcache::new();
        let mut data = [0u8; 16];
        data[4] = 0xDE;
        data[5] = 0xAD;
        data[6] = 0xBE;
        data[7] = 0xEF;
        d.install(0x1000, data);
        assert_eq!(d.read(0x1004, 4), 0xDEAD_BEEF);
    }

    #[test]
    fn invalidating_keeps_the_tag_so_load_tag_still_reports_the_pfn() {
        // The distinction the ROM asserts: `Index_Invalidate` clears PState and
        // leaves PTagLo alone. Clearing both would report tag 0 and pass any
        // test that only looked at the valid bit.
        let mut d = Dcache::new();
        d.install(0x2_5000, [0; 16]);
        assert_eq!(d.load_tag(0x2_5000), (3 << 6) | (0x25 << 8));
        d.flush_index(0x2_5000, true, false);
        assert_eq!(d.load_tag(0x2_5000), 0x25 << 8, "PFN survives invalidation");
    }

    #[test]
    fn a_dirty_line_is_written_back_to_the_address_it_came_from() {
        // Not to the address that evicted it. The two differ in exactly the bits
        // the index does not carry, so a cache small enough to alias is the only
        // place the mistake shows up -- which is every real eviction.
        let mut d = Dcache::new();
        d.install(0x1_0000, [0xAA; 16]);
        d.write(0x1_0000, 4, 0x1122_3344);
        let plan = d.miss_plan(0x2_0000).expect("different tag misses");
        assert_eq!(plan.expect("dirty line evicts").addr, 0x1_0000);
    }

    #[test]
    fn a_clean_eviction_needs_no_writeback() {
        let mut d = Dcache::new();
        d.install(0x1_0000, [0xAA; 16]);
        assert_eq!(d.miss_plan(0x2_0000), Some(None));
    }

    #[test]
    fn a_hit_needs_no_plan_at_all() {
        let mut d = Dcache::new();
        d.install(0x1_0000, [0xAA; 16]);
        assert_eq!(d.miss_plan(0x1_0008), None, "same line");
    }

    /// `Index_Store_Tag` requires the cache's OWN valid encoding. Accepting any
    /// non-zero `PState` conjures a live line from an undefined one — and note
    /// the two caches disagree, so the D-cache's 3 must not validate an I-cache
    /// line either.
    #[test]
    fn store_tag_rejects_a_pstate_this_cache_does_not_define() {
        let mut d = Dcache::new();
        let mut i = Icache::new();
        for bad in [1u32, 2] {
            d.store_tag(0x1000, (bad << 6) | (0x1 << 8));
            assert!(!d.hits(0x1000), "D-cache PState {bad} is not Valid");
        }
        for bad in [1u32, 3] {
            i.store_tag(0x1000, (bad << 6) | (0x1 << 8));
            assert!(!i.hits(0x1000), "I-cache PState {bad} is not Valid");
        }
        d.store_tag(0x1000, (3 << 6) | (0x1 << 8));
        assert!(d.hits(0x1000), "3 is the D-cache's Valid");
        i.store_tag(0x1000, (2 << 6) | (0x1 << 8));
        assert!(i.hits(0x1000), "2 is the I-cache's Valid");
    }

    #[test]
    fn store_tag_round_trips_through_load_tag() {
        let mut d = Dcache::new();
        let want = (3 << 6) | (0x1AC << 8);
        d.store_tag(0x1AC_000, want);
        assert_eq!(d.load_tag(0x1AC_000), want);
    }

    #[test]
    fn the_two_caches_use_different_valid_states() {
        // I-cache Valid is 2, D-cache Valid is 3 (UM Figures 5-19/5-20). Sharing
        // one constant would pass every test that only checked "non-zero".
        let mut i = Icache::new();
        let mut d = Dcache::new();
        i.install(0x1000, [0; 32]);
        d.install(0x1000, [0; 16]);
        assert_eq!((i.load_tag(0x1000) >> 6) & 3, 2);
        assert_eq!((d.load_tag(0x1000) >> 6) & 3, 3);
    }

    #[test]
    fn hit_writeback_only_reports_a_resident_line() {
        let mut i = Icache::new();
        assert_eq!(i.flush_hit(0x1000), None);
        i.install(0x1000, [0x5A; 32]);
        assert_eq!(i.flush_hit(0x1000).expect("resident").addr, 0x1000);
        assert_eq!(i.flush_hit(0x2000), None, "different tag, same index");
    }

    #[test]
    fn create_dirty_exclusive_claims_the_line_without_a_fill() {
        let mut d = Dcache::new();
        assert_eq!(d.create_dirty_exclusive(0x3000), None);
        assert!(d.hits(0x3000));
        assert_eq!((d.load_tag(0x3000) >> 6) & 3, 3);
    }
}
