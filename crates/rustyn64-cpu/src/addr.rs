//! Virtual → physical address translation (T-11-006, T-12-004).
//!
//! Every address the CPU hands to the [`crate::Bus`] is **physical**. This is
//! where that becomes true: the MIPS segment map is applied here, inside the CPU
//! crate, exactly as `docs/cpu.md` specifies.
//!
//! # The segment map
//!
//! The map is **not** a property of the address alone: it depends on the
//! privilege mode and on whether that mode is using 64-bit addressing. A segment
//! that is perfectly ordinary in Kernel mode is an **address error** in User
//! mode, and it is that check — not the TLB — that stops a user program reaching
//! `KSEG0`. See [`Access`].
//!
//! ## Kernel, 32-bit addressing
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
//! ## Supervisor and User, 32-bit addressing
//!
//! Supervisor sees SUSEG (`0x0000_0000`–`0x7FFF_FFFF`) and SSEG
//! (`0xC000_0000`–`0xDFFF_FFFF`), both mapped. User sees USEG
//! (`0x0000_0000`–`0x7FFF_FFFF`) alone. **Everything else is an address error**,
//! including the range that would be `KSEG0`.
//!
//! ## 64-bit addressing
//!
//! With `Status.KX`/`SX`/`UX` set for the current mode, each mapped segment
//! widens to 2^40 and the address space grows holes: an address inside a
//! segment's *region* but past its size is an address error, which is what the
//! `..._gap` cases in n64-systemtest's privilege matrix check.
//!
//! Kernel additionally gains **XKPHYS**, `0x8000_..`–`0xBFFF_..`: eight 2^32
//! direct-mapped windows selected by bits 61:59, differing only in cacheability.
//! Bits 58:32 must be zero or the access is an address error.
//!
//! The compatibility segments `CKSEG0`–`CKSEG3` sit at `0xFFFF_FFFF_8000_0000`
//! upward and behave exactly as their 32-bit namesakes.
//!
//! The mapped segments go through the TLB ([`translate_via`]).
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

/// Which segment a virtual address falls in, and how it translates.
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
    /// Not a valid address in this mode: raises an address error **before** the
    /// TLB is consulted.
    ///
    /// A distinct variant rather than a `TlbFault`, because the two produce
    /// different exceptions from different vectors. Folding an out-of-range
    /// kernel address into a TLB refill would send a user program to the refill
    /// handler, where a well-behaved kernel would map the page and hand it the
    /// access it was never allowed to make.
    Invalid,
}

/// The privilege mode an access is made in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    /// `Status.KSU == 0`, or `EXL`/`ERL` set.
    Kernel,
    /// `Status.KSU == 1`.
    Supervisor,
    /// `Status.KSU == 2`.
    User,
}

/// Everything outside the address itself that decides how it translates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Access {
    /// The privilege mode.
    pub mode: Mode,
    /// `Status.KX`/`SX`/`UX` — for [`Access::mode`], not all three.
    ///
    /// One flag rather than three because only the current mode's bit ever
    /// applies; carrying all three would invite reading the wrong one.
    pub wide: bool,
    /// `Status.ERL` — makes KUSEG unmapped and uncached (UM §5.2.2).
    pub erl: bool,
}

impl Access {
    /// The mode the CPU is in at reset and inside any exception handler.
    #[must_use]
    pub const fn kernel() -> Self {
        Self {
            mode: Mode::Kernel,
            wide: false,
            erl: false,
        }
    }
}

/// Size of a mapped segment under 64-bit addressing: 2^40.
const XSEG_SIZE: u64 = 1 << 40;

/// Classify a virtual address by segment (UM §5.2.4, Tables 5-3/5-4).
///
/// This is the half of translation that does **not** need the TLB, split out so
/// callers with no TLB (and the many unmapped accesses) do not pay for one.
#[must_use]
pub const fn segment(vaddr: u64, access: Access) -> Segment {
    // "If the ERL bit of the Status register is 1, the user address area is a
    // 2 GB area that cannot be cached without TLB mapping (i.e., the virtual
    // addresses are used as physical addresses as is)" (UM §5.2.2, p. 129).
    //
    // This is not an obscure corner: **cold reset sets ERL** (UM §6.4.4), so it
    // is the state every boot ROM starts in. Without it, the first store to a
    // low address takes a TLB refill before any mapping could possibly exist --
    // which is exactly how n64-systemtest failed, two instructions in.
    // Compared on the FULL 64-bit address, not the truncated low word. The
    // manual says a **2 GB** area, so `0x0000_0001_0000_1000` is outside it —
    // while its low 32 bits are not, and truncating would direct-map it.
    if access.erl && vaddr < 0x8000_0000 {
        return Segment::Direct {
            addr: vaddr as u32,
            cached: Cached::No,
        };
    }
    match access.mode {
        Mode::User => user_segment(vaddr, access.wide),
        Mode::Supervisor => supervisor_segment(vaddr, access.wide),
        Mode::Kernel => kernel_segment(vaddr, access.wide),
    }
}

/// Is `vaddr` a sign-extended 32-bit address?
///
/// Under 32-bit addressing every valid address is one; anything else is an
/// address error rather than a truncation. Truncating instead is the natural
/// shortcut and it silently accepts `0x0000_0001_8000_1000` as `KSEG0`.
const fn is_compat(vaddr: u64) -> bool {
    vaddr as i64 as i32 as i64 as u64 == vaddr
}

/// The 32-bit compatibility segments above `0xFFFF_FFFF_8000_0000`.
///
/// Shared by Kernel's 32- and 64-bit maps, where they are `KSEG0..3` and
/// `CKSEG0..3` respectively — the same four segments under two names.
const fn compat_kernel_segment(v: u32) -> Segment {
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

/// User mode: one segment, and everything else is an address error.
const fn user_segment(vaddr: u64, wide: bool) -> Segment {
    if wide {
        // XUSEG, 2^40. The region runs to 2^62 but the segment does not, and the
        // hole between them faults.
        if vaddr < XSEG_SIZE {
            Segment::Mapped
        } else {
            Segment::Invalid
        }
    } else if is_compat(vaddr) && (vaddr as u32) < 0x8000_0000 {
        Segment::Mapped
    } else {
        Segment::Invalid
    }
}

/// Supervisor mode: SUSEG and SSEG, plus their 64-bit widenings.
const fn supervisor_segment(vaddr: u64, wide: bool) -> Segment {
    if wide {
        // In order: XSUSEG, XSSEG, and CSSEG (the compatibility window, mapped
        // like SSEG). All three are TLB-mapped, so they share one arm.
        match vaddr {
            0..0x0000_0100_0000_0000
            | 0x4000_0000_0000_0000..0x4000_0100_0000_0000
            | 0xFFFF_FFFF_C000_0000..=0xFFFF_FFFF_DFFF_FFFF => Segment::Mapped,
            _ => Segment::Invalid,
        }
    } else if !is_compat(vaddr) {
        Segment::Invalid
    } else {
        // SUSEG and SSEG, both TLB-mapped.
        match vaddr as u32 {
            0x0000_0000..=0x7FFF_FFFF | 0xC000_0000..=0xDFFF_FFFF => Segment::Mapped,
            // Including the range that would be KSEG0/KSEG1 in Kernel mode: the
            // privilege check, not the TLB, is what keeps Supervisor out of it.
            _ => Segment::Invalid,
        }
    }
}

/// Kernel mode: the whole map, and under 64-bit addressing also XKPHYS.
const fn kernel_segment(vaddr: u64, wide: bool) -> Segment {
    if !wide {
        return if is_compat(vaddr) {
            compat_kernel_segment(vaddr as u32)
        } else {
            Segment::Invalid
        };
    }
    match vaddr {
        // XKUSEG, XKSSEG and XKSEG: 2^40 each (XKSEG a little less), each with a
        // hole above it, and all TLB-mapped -- so one arm.
        0..0x0000_0100_0000_0000
        | 0x4000_0000_0000_0000..0x4000_0100_0000_0000
        | 0xC000_0000_0000_0000..0xC000_00FF_8000_0000 => Segment::Mapped,
        // XKPHYS: eight direct windows chosen by bits 61:59, each 2^32 wide and
        // differing only in cacheability. Bits 58:32 must be zero.
        0x8000_0000_0000_0000..0xC000_0000_0000_0000 => xkphys(vaddr),
        // CKSEG0..3 — the same four segments as the 32-bit map.
        0xFFFF_FFFF_8000_0000..=0xFFFF_FFFF_FFFF_FFFF => compat_kernel_segment(vaddr as u32),
        _ => Segment::Invalid,
    }
}

/// One of the eight XKPHYS windows.
///
/// The cacheability comes from the `C` encoding in bits 61:59, the same three-bit
/// field a TLB entry carries — and by the same rule, **only `C == 2` is
/// uncached** (UM Table 5-6).
const fn xkphys(vaddr: u64) -> Segment {
    if vaddr & 0x07FF_FFFF_0000_0000 != 0 {
        return Segment::Invalid;
    }
    let c = (vaddr >> 59) & 0b111;
    Segment::Direct {
        addr: vaddr as u32,
        cached: if c == 2 { Cached::No } else { Cached::Yes },
    }
}

/// Why a translation failed.
///
/// Two variants rather than one because they raise **different exceptions from
/// different vectors**: an address error goes to the general handler, a TLB
/// fault to the refill vector. Collapsing them would send a user program that
/// touched a kernel address to the refill handler, where a well-behaved kernel
/// maps the page and grants the access it was never allowed to make.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TranslateError {
    /// The address is not valid in this privilege mode: `AdEL`/`AdES`.
    Address,
    /// The address is valid but the TLB could not resolve it.
    Tlb(TlbFault),
}

/// Translate a virtual address, consulting the TLB for the mapped segments.
///
/// # Errors
///
/// [`TranslateError::Address`] when the address is not valid in `access`'s mode,
/// and [`TranslateError::Tlb`] when a mapped access misses, hits an invalid
/// entry, or stores to a non-writable page. Unmapped segments cannot fail.
pub fn translate_via(
    tlb: &mut Tlb,
    vaddr: u64,
    asid: u8,
    store: bool,
    access: Access,
) -> Result<Physical, TranslateError> {
    match segment(vaddr, access) {
        Segment::Direct { addr, cached } => Ok(Physical { addr, cached }),
        Segment::Mapped => {
            let t = tlb
                .lookup(vaddr, asid, store)
                .map_err(TranslateError::Tlb)?;
            Ok(Physical {
                addr: t.addr,
                // Cacheability of a mapped page comes from its entry's `C`
                // field, not from the segment -- which is why `Cached` has to be
                // returned from here rather than derived from the address later.
                cached: if t.uncached { Cached::No } else { Cached::Yes },
            })
        }
        Segment::Invalid => Err(TranslateError::Address),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resolve an unmapped address, panicking if it is mapped — a test helper,
    /// so the assertions below stay about addresses rather than about `match`.
    fn direct(vaddr: u64) -> Physical {
        match segment(vaddr, Access::kernel()) {
            Segment::Direct { addr, cached } => Physical { addr, cached },
            Segment::Mapped => panic!("{vaddr:#X} is TLB-mapped, not direct"),
            Segment::Invalid => panic!("{vaddr:#X} is not valid in kernel mode"),
        }
    }

    /// KSEG0 and KSEG1 address the *same* physical memory and differ only in
    /// cacheability. Emitting different physical addresses for them is a classic
    /// bug that makes uncached register writes land in RDRAM.
    #[test]
    fn kseg0_and_kseg1_alias_the_same_physical_memory() {
        for phys in [0u32, 0x1000, 0x0080_0000, 0x0400_0000, 0x1000_0000] {
            let k0 = direct(0xFFFF_FFFF_8000_0000 + u64::from(phys));
            let k1 = direct(0xFFFF_FFFF_A000_0000 + u64::from(phys));
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
        assert_eq!(direct(0xFFFF_FFFF_8000_1000).addr, 0x1000);
        // n64-systemtest's entry point.
        assert_eq!(direct(0xFFFF_FFFF_800A_15E8).addr, 0x000A_15E8);
        // The PIF RAM word basic.z64 writes before its first test.
        assert_eq!(direct(0xFFFF_FFFF_BFC0_07FC).addr, 0x1FC0_07FC);
        // Cart domain 1, where a ROM is memory-mapped.
        assert_eq!(direct(0xFFFF_FFFF_B000_0000).addr, 0x1000_0000);
        // The RSP DMEM base.
        assert_eq!(direct(0xFFFF_FFFF_A400_0000).addr, 0x0400_0000);
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
            0xFFFF_FFFF_C000_0000, // KSSEG
            0xFFFF_FFFF_E000_0000, // KSEG3
            0xFFFF_FFFF_FFFF_FFFF,
        ] {
            assert_eq!(
                segment(v, Access::kernel()),
                Segment::Mapped,
                "{v:#X} must go through the TLB"
            );
        }
    }

    /// **`ERL = 1` makes KUSEG unmapped and uncached** (UM §5.2.2, p. 129):
    /// *"the user address area is a 2 GB area that cannot be cached without TLB
    /// mapping (i.e., the virtual addresses are used as physical addresses as
    /// is)."*
    ///
    /// This is not a corner case — **cold reset sets `ERL`** (UM §6.4.4), so it
    /// is the state every boot ROM starts in. Without it the first store to a low
    /// address takes a TLB refill before any mapping could exist, which is
    /// exactly how n64-systemtest failed: two instructions in, `ExcCode = TLBS`,
    /// `BadVAddr = 0`.
    #[test]
    fn erl_makes_kuseg_unmapped_and_uncached() {
        for v in [0x0000_0000u64, 0x1000, 0x0040_0000, 0x7FFF_FFFF] {
            assert_eq!(
                segment(v, Access::kernel()),
                Segment::Mapped,
                "{v:#X} is TLB-mapped with ERL clear"
            );
            assert_eq!(
                segment(
                    v,
                    Access {
                        erl: true,
                        ..Access::kernel()
                    }
                ),
                Segment::Direct {
                    addr: v as u32,
                    cached: Cached::No
                },
                "{v:#X} is identity-mapped and uncached with ERL set"
            );
        }
    }

    /// The `ERL` area is **2 GB**, so the check is on the full 64-bit address.
    /// Comparing the truncated low half would direct-map a 64-bit address whose
    /// low 32 bits happen to fall below `0xFFFF_FFFF_8000_0000`.
    #[test]
    fn the_erl_user_area_is_two_gigabytes_not_a_truncated_comparison() {
        // Genuinely inside: a 32-bit KUSEG address is zero-extended.
        assert_eq!(
            segment(
                0x0000_1000,
                Access {
                    erl: true,
                    ..Access::kernel()
                }
            ),
            Segment::Direct {
                addr: 0x1000,
                cached: Cached::No
            }
        );
        // Outside the 2 GB area, but with low 32 bits that look inside.
        for v in [0x0000_0001_0000_1000u64, 0x0000_00FF_0000_1000] {
            assert_ne!(
                segment(
                    v,
                    Access {
                        erl: true,
                        ..Access::kernel()
                    }
                ),
                Segment::Direct {
                    addr: 0x1000,
                    cached: Cached::No
                },
                "{v:#X} is beyond the 2 GB user area and must not be direct-mapped"
            );
        }
    }

    /// A helper for the matrix below: `Access` for a mode at a given width.
    fn acc(mode: Mode, wide: bool) -> Access {
        Access {
            mode,
            wide,
            erl: false,
        }
    }

    /// The privilege/segment matrix, mirroring n64-systemtest's own
    /// `Privilege: memory accesses` cases.
    ///
    /// The rows that matter most are the ones where the SAME address resolves
    /// differently by mode: `0xFFFF_FFFF_8000_1000` is KSEG0 for the kernel and
    /// an address error for everyone else. A map that ignores the mode passes
    /// every kernel row and fails exactly these.
    const SEGMENT_MATRIX: &[(&str, u64, Mode, bool, Segment)] = {
        use Mode::{Kernel, Supervisor, User};
        &[
            // Kernel, 32-bit.
            (
                "k32 kuseg",
                0x0000_0000_0000_1000,
                Kernel,
                false,
                Segment::Mapped,
            ),
            (
                "k32 kseg0",
                0xFFFF_FFFF_8000_1000,
                Kernel,
                false,
                Segment::Direct {
                    addr: 0x1000,
                    cached: Cached::Yes,
                },
            ),
            (
                "k32 kseg1",
                0xFFFF_FFFF_A000_1000,
                Kernel,
                false,
                Segment::Direct {
                    addr: 0x1000,
                    cached: Cached::No,
                },
            ),
            (
                "k32 ksseg",
                0xFFFF_FFFF_C000_1000,
                Kernel,
                false,
                Segment::Mapped,
            ),
            (
                "k32 kseg3",
                0xFFFF_FFFF_E000_1000,
                Kernel,
                false,
                Segment::Mapped,
            ),
            // The same KSEG0 address, one privilege level down.
            (
                "s32 low unused",
                0xFFFF_FFFF_9000_1000,
                Supervisor,
                false,
                Segment::Invalid,
            ),
            (
                "s32 suseg",
                0x0000_0000_0000_1000,
                Supervisor,
                false,
                Segment::Mapped,
            ),
            (
                "s32 sseg",
                0xFFFF_FFFF_C000_1000,
                Supervisor,
                false,
                Segment::Mapped,
            ),
            (
                "s32 top unused",
                0xFFFF_FFFF_E000_1000,
                Supervisor,
                false,
                Segment::Invalid,
            ),
            (
                "u32 useg",
                0x0000_0000_0000_1000,
                User,
                false,
                Segment::Mapped,
            ),
            (
                "u32 unused",
                0xFFFF_FFFF_9000_1000,
                User,
                false,
                Segment::Invalid,
            ),
            // 64-bit addressing: each mapped segment widens to 2^40, and the
            // hole above it faults.
            (
                "k64 xkuseg",
                0x0000_0000_0000_1000,
                Kernel,
                true,
                Segment::Mapped,
            ),
            (
                "k64 xkuseg gap",
                0x0000_0100_0000_0000,
                Kernel,
                true,
                Segment::Invalid,
            ),
            (
                "k64 xksseg",
                0x4000_0000_0000_1000,
                Kernel,
                true,
                Segment::Mapped,
            ),
            (
                "k64 xksseg gap",
                0x4000_0100_0000_0000,
                Kernel,
                true,
                Segment::Invalid,
            ),
            (
                "k64 xkseg",
                0xC000_0000_0000_1000,
                Kernel,
                true,
                Segment::Mapped,
            ),
            (
                "k64 xkseg gap",
                0xC000_0100_0000_0000,
                Kernel,
                true,
                Segment::Invalid,
            ),
            (
                "k64 ckseg0",
                0xFFFF_FFFF_8000_1000,
                Kernel,
                true,
                Segment::Direct {
                    addr: 0x1000,
                    cached: Cached::Yes,
                },
            ),
            (
                "k64 ckseg1",
                0xFFFF_FFFF_A000_1000,
                Kernel,
                true,
                Segment::Direct {
                    addr: 0x1000,
                    cached: Cached::No,
                },
            ),
            (
                "s64 xsuseg",
                0x0000_0000_0000_1000,
                Supervisor,
                true,
                Segment::Mapped,
            ),
            (
                "s64 xsuseg gap",
                0x0000_0100_0000_0000,
                Supervisor,
                true,
                Segment::Invalid,
            ),
            (
                "s64 xsseg",
                0x4000_0000_0000_1000,
                Supervisor,
                true,
                Segment::Mapped,
            ),
            (
                "s64 xsseg gap",
                0x4000_0100_0000_0000,
                Supervisor,
                true,
                Segment::Invalid,
            ),
            (
                "s64 csseg",
                0xFFFF_FFFF_C000_1000,
                Supervisor,
                true,
                Segment::Mapped,
            ),
            (
                "s64 top unused",
                0xFFFF_FFFF_E000_1000,
                Supervisor,
                true,
                Segment::Invalid,
            ),
            (
                "u64 xuseg",
                0x0000_0000_0000_1000,
                User,
                true,
                Segment::Mapped,
            ),
            (
                "u64 xuseg gap",
                0x0000_0100_0000_0000,
                User,
                true,
                Segment::Invalid,
            ),
        ]
    };

    #[test]
    fn the_segment_map_depends_on_the_privilege_mode() {
        for (name, vaddr, mode, wide, want) in SEGMENT_MATRIX {
            assert_eq!(segment(*vaddr, acc(*mode, *wide)), *want, "{name}");
        }
    }

    /// All eight XKPHYS windows are direct, and only `C == 2` is uncached — the
    /// same rule a TLB entry's `C` field follows (UM Table 5-6).
    #[test]
    fn xkphys_is_eight_direct_windows_differing_only_in_cacheability() {
        for c in 0u64..8 {
            let v = (0b10 << 62) | (c << 59) | 0x1000;
            assert_eq!(
                segment(v, acc(Mode::Kernel, true)),
                Segment::Direct {
                    addr: 0x1000,
                    cached: if c == 2 { Cached::No } else { Cached::Yes },
                },
                "XKPHYS window {c}"
            );
            // Bits 58:32 must be zero: a window is 2^32 wide, not 2^59.
            assert_eq!(
                segment(v | (1 << 32), acc(Mode::Kernel, true)),
                Segment::Invalid,
                "XKPHYS window {c} past 2^32"
            );
        }
    }

    /// Under 32-bit addressing an address must be the sign extension of its low
    /// word. `0x0000_0000_8000_1000` is **not** a shorthand for KSEG0.
    ///
    /// n64-systemtest asserts this directly ("LW with address not sign
    /// extended"), and truncating to `u32` instead — the natural shortcut, and
    /// what this module did until T-11-003 — accepts it silently.
    #[test]
    fn a_non_sign_extended_address_is_invalid_in_32_bit_mode() {
        assert_eq!(
            segment(0x0000_0000_8000_1000, acc(Mode::Kernel, false)),
            Segment::Invalid
        );
        assert_eq!(
            segment(0xFFFF_FFFF_8000_1000, acc(Mode::Kernel, false)),
            Segment::Direct {
                addr: 0x1000,
                cached: Cached::Yes
            },
            "the sign-extended form is the same address, and is valid"
        );
    }

    /// `ERL` affects **only** the user area. The kernel segments keep their own
    /// rules, and the mapped kernel segments stay mapped — a blanket
    /// `if erl { Direct }` would silently unmap KSSEG and KSEG3 too.
    #[test]
    fn erl_does_not_change_the_kernel_segments() {
        // Unmapped kernel segments are unaffected in both directions.
        assert_eq!(
            segment(
                0xFFFF_FFFF_8000_1000,
                Access {
                    erl: true,
                    ..Access::kernel()
                }
            ),
            segment(0xFFFF_FFFF_8000_1000, Access::kernel()),
            "KSEG0"
        );
        assert_eq!(
            segment(
                0xFFFF_FFFF_A400_0000,
                Access {
                    erl: true,
                    ..Access::kernel()
                }
            ),
            segment(0xFFFF_FFFF_A400_0000, Access::kernel()),
            "KSEG1"
        );
        // Mapped kernel segments stay MAPPED even with ERL set.
        for v in [0xFFFF_FFFF_C000_0000u64, 0xFFFF_FFFF_E000_0000] {
            assert_eq!(
                segment(
                    v,
                    Access {
                        erl: true,
                        ..Access::kernel()
                    }
                ),
                Segment::Mapped,
                "{v:#X} must stay TLB-mapped -- ERL covers the user area only"
            );
        }
    }
}
