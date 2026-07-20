//! Virtual → physical address translation (T-11-006).
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
//! The two mapped segments are the TLB's job (Phase 1 Sprint 2). Until then they
//! are passed through with their low 29 bits, which is right for the identity
//! mappings early boot code uses and wrong for anything else — see
//! [`translate`].

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

/// Translate a virtual address to physical.
///
/// KSEG0/KSEG1 are unmapped and become a subtraction. KUSEG and KSEG3 are
/// TLB-mapped; until the TLB lands (Sprint 2) they are masked to their low 29
/// bits, which happens to be correct for the identity mappings boot code uses
/// and is **wrong in general** — a TLB miss must raise a refill exception rather
/// than silently aliasing. `TODO(T-12)`.
#[must_use]
pub const fn translate(vaddr: u64) -> Physical {
    // 32-bit mode sign-extends, so a 64-bit register holding a KSEG0 address
    // reads as 0xFFFF_FFFF_8000_xxxx. Take the low 32 bits before deciding.
    let v = vaddr as u32;
    match v {
        0x8000_0000..=0x9FFF_FFFF => Physical {
            addr: v - 0x8000_0000,
            cached: Cached::Yes,
        },
        0xA000_0000..=0xBFFF_FFFF => Physical {
            addr: v - 0xA000_0000,
            cached: Cached::No,
        },
        _ => Physical {
            addr: v & 0x1FFF_FFFF,
            cached: Cached::Yes,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// KSEG0 and KSEG1 address the *same* physical memory and differ only in
    /// cacheability. Emitting different physical addresses for them is a classic
    /// bug that makes uncached register writes land in RDRAM.
    #[test]
    fn kseg0_and_kseg1_alias_the_same_physical_memory() {
        for phys in [0u32, 0x1000, 0x0080_0000, 0x0400_0000, 0x1000_0000] {
            let k0 = translate(u64::from(0x8000_0000 + phys));
            let k1 = translate(u64::from(0xA000_0000 + phys));
            assert_eq!(k0.addr, phys, "KSEG0 + {phys:#X}");
            assert_eq!(k1.addr, phys, "KSEG1 + {phys:#X}");
            assert_eq!(k0.cached, Cached::Yes);
            assert_eq!(k1.cached, Cached::No, "KSEG1 is uncached");
        }
    }

    /// The addresses that actually matter for bring-up.
    #[test]
    fn the_boot_and_register_addresses_translate_correctly() {
        // basic.z64's entry point.
        assert_eq!(translate(0x8000_1000).addr, 0x1000);
        // n64-systemtest's entry point.
        assert_eq!(translate(0x800A_15E8).addr, 0x000A_15E8);
        // The PIF RAM word basic.z64 writes before its first test.
        assert_eq!(translate(0xBFC0_07FC).addr, 0x1FC0_07FC);
        // Cart domain 1, where a ROM is memory-mapped.
        assert_eq!(translate(0xB000_0000).addr, 0x1000_0000);
        // The RSP DMEM base.
        assert_eq!(translate(0xA400_0000).addr, 0x0400_0000);
    }

    /// A 64-bit register holding a 32-bit address is sign-extended, so KSEG0
    /// arrives as `0xFFFF_FFFF_8xxx_xxxx`. Translating that as a 64-bit value
    /// rather than its low half sends it to the wrong segment.
    #[test]
    fn a_sign_extended_32_bit_address_still_translates() {
        assert_eq!(translate(0xFFFF_FFFF_8000_1000).addr, 0x1000);
        assert_eq!(
            translate(0xFFFF_FFFF_A400_0000).cached,
            Cached::No,
            "sign extension must not lose the segment"
        );
    }
}
