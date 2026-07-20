//! Virtual → physical address translation (T-11-006, T-12-004).
//!
//! Every address the CPU hands to the [`crate::Bus`] is **physical**. This is
//! where that becomes true: the MIPS segment map is applied here, inside the CPU
//! crate, exactly as `docs/cpu.md` specifies.
//!
//! # The segment map
//!
//! | Segment | Range | Mapping |
//! |---|---|---|
//! | KUSEG | `0x0000_0000`–`0x7FFF_FFFF` | TLB-mapped |
//! | KSEG0 | `0x8000_0000`–`0x9FFF_FFFF` | direct, **cached** |
//! | KSEG1 | `0xA000_0000`–`0xBFFF_FFFF` | direct, **uncached** |
//! | KSSEG/KSEG3 | `0xC000_0000`–`0xFFFF_FFFF` | TLB-mapped |
//!
//! KSEG0 and KSEG1 are *unmapped*: they address the same physical memory and
//! differ only in cacheability, so translation is a subtraction. That is why a
//! ROM entry point of `0x8000_1000` and a hardware register at `0xA430_0000`
//! both work without any TLB.
//!
//! The two mapped segments go through the TLB ([`translate_via`]).
//!
//! An earlier `translate` masked mapped addresses to their low 29 bits — right
//! for the identity mappings early boot code uses, wrong for anything else. It
//! was **deleted** rather than kept "for unmapped-only paths" once nothing
//! called it: an unused function that quietly gets translation wrong is exactly
//! the inert-API hazard `docs/engineering-lessons.md` §3.2 describes.

use crate::tlb::{Tlb, TlbFault};

/// Whether an access goes through the caches.
///
/// Not yet consumed: the I- and D-caches are Sprint 2 work. It is returned now
/// because the *segment* determines it, and recomputing that later from a
/// physical address is impossible — the information is gone once KSEG0 and KSEG1
/// have both become the same number.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Cached {
    /// Through the cache (KSEG0, and mapped segments per their TLB entry).
    Yes,
    /// Bypassing it (KSEG1).
    No,
}

/// A translated address.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct Physical {
    /// The physical address.
    pub addr: u32,
    /// Whether the access is cached.
    pub cached: Cached,
}

/// Which segment a 32-bit virtual address falls in, and how it translates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Segment {
    /// Unmapped: physical address is the virtual one minus the segment base.
    Direct {
        /// The physical address.
        addr: u32,
        /// Whether the access is cached.
        cached: Cached,
    },
    /// TLB-mapped: KUSEG, KSSEG or KSEG3. The TLB decides the physical address
    /// *and* the cacheability, from the matching entry's `C` field.
    Mapped,
}

/// Classify a virtual address by segment (UM §5.2.4, Tables 5-3/5-4).
///
/// This is the half of translation that does **not** need the TLB, split out so
/// callers with no TLB (and the many unmapped accesses) do not pay for one.
#[must_use]
pub const fn segment(vaddr: u64) -> Segment {
    let v = vaddr as u32;
    match v {
        0x8000_0000..=0x9FFF_FFFF => Segment::Direct {
            addr: v - 0x8000_0000,
            cached: Cached::Yes,
        },
        0xA000_0000..=0xBFFF_FFFF => Segment::Direct {
            addr: v - 0xA000_0000,
            cached: Cached::No,
        },
        // KUSEG, KSSEG and KSEG3 are all TLB-mapped.
        _ => Segment::Mapped,
    }
}

/// Translate a virtual address, consulting the TLB for the mapped segments.
///
/// # Errors
///
/// [`TlbFault`] when a mapped access misses, hits an invalid entry, or stores to
/// a non-writable page. Unmapped segments cannot fail.
pub fn translate_via(
    tlb: &mut Tlb,
    vaddr: u64,
    asid: u8,
    store: bool,
) -> Result<Physical, TlbFault> {
    match segment(vaddr) {
        Segment::Direct { addr, cached } => Ok(Physical { addr, cached }),
        Segment::Mapped => {
            let t = tlb.lookup(vaddr, asid, store)?;
            Ok(Physical {
                addr: t.addr,
                // Cacheability of a mapped page comes from its entry's `C`
                // field, not from the segment -- which is why `Cached` has to be
                // returned from here rather than derived from the address later.
                cached: if t.uncached { Cached::No } else { Cached::Yes },
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resolve an unmapped address, panicking if it is mapped — a test helper,
    /// so the assertions below stay about addresses rather than about `match`.
    fn direct(vaddr: u64) -> Physical {
        match segment(vaddr) {
            Segment::Direct { addr, cached } => Physical { addr, cached },
            Segment::Mapped => panic!("{vaddr:#X} is TLB-mapped, not direct"),
        }
    }

    /// KSEG0 and KSEG1 address the *same* physical memory and differ only in
    /// cacheability. Emitting different physical addresses for them is a classic
    /// bug that makes uncached register writes land in RDRAM.
    #[test]
    fn kseg0_and_kseg1_alias_the_same_physical_memory() {
        for phys in [0u32, 0x1000, 0x0080_0000, 0x0400_0000, 0x1000_0000] {
            let k0 = direct(u64::from(0x8000_0000 + phys));
            let k1 = direct(u64::from(0xA000_0000 + phys));
            assert_eq!(k0.addr, phys, "KSEG0 + {phys:#X}");
            assert_eq!(k1.addr, phys, "KSEG1 + {phys:#X}");
            assert_eq!(k0.cached, Cached::Yes);
            assert_eq!(k1.cached, Cached::No, "KSEG1 is uncached");
        }
    }

    /// The addresses that actually matter for bring-up. All unmapped, which is
    /// why early boot works before any TLB entry exists.
    #[test]
    fn the_boot_and_register_addresses_translate_correctly() {
        // basic.z64's entry point.
        assert_eq!(direct(0x8000_1000).addr, 0x1000);
        // n64-systemtest's entry point.
        assert_eq!(direct(0x800A_15E8).addr, 0x000A_15E8);
        // The PIF RAM word basic.z64 writes before its first test.
        assert_eq!(direct(0xBFC0_07FC).addr, 0x1FC0_07FC);
        // Cart domain 1, where a ROM is memory-mapped.
        assert_eq!(direct(0xB000_0000).addr, 0x1000_0000);
        // The RSP DMEM base.
        assert_eq!(direct(0xA400_0000).addr, 0x0400_0000);
    }

    /// A 64-bit register holding a 32-bit address is sign-extended, so KSEG0
    /// arrives as `0xFFFF_FFFF_8xxx_xxxx`. Classifying that as a 64-bit value
    /// rather than its low half sends it to the wrong segment.
    #[test]
    fn a_sign_extended_32_bit_address_still_translates() {
        assert_eq!(direct(0xFFFF_FFFF_8000_1000).addr, 0x1000);
        assert_eq!(
            direct(0xFFFF_FFFF_A400_0000).cached,
            Cached::No,
            "sign extension must not lose the segment"
        );
    }

    /// KUSEG, KSSEG and KSEG3 are **mapped** — the TLB decides, and a miss
    /// raises. Before T-12-004 they were silently masked to their low 29 bits,
    /// which aliased every unmapped access onto real memory instead of faulting.
    #[test]
    fn the_mapped_segments_are_reported_as_mapped_not_masked() {
        for v in [
            0x0000_0000u64, // KUSEG
            0x0000_1000,
            0x7FFF_FFFF,
            0xC000_0000, // KSSEG
            0xE000_0000, // KSEG3
            0xFFFF_FFFF,
        ] {
            assert_eq!(
                segment(v),
                Segment::Mapped,
                "{v:#X} must go through the TLB"
            );
        }
    }
}
