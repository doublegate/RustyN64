//! Load/store data shaping (T-11-003).
//!
//! Pure byte-level transforms: no bus, no registers, no addresses beyond the
//! low bits that select alignment. The pipeline's `DC` stage performs the actual
//! access and calls these to shape the result.
//!
//! # The unaligned family is the interesting part
//!
//! MIPS has no unaligned load instruction. Instead `LWL`/`LWR` (and `LDL`/`LDR`
//! for doublewords) are used **as a pair** to assemble an unaligned value from
//! two aligned accesses, each merging part of the addressed word into the
//! destination register while preserving the rest of it.
//!
//! The N64 is **big-endian**, which decides the direction of every shift here.
//! `LWL` takes the bytes from the addressed byte to the end of the containing
//! word and places them at the *top* of the register; `LWR` takes the bytes from
//! the start of the word up to the addressed byte and places them at the
//! *bottom*. Getting the endianness backwards produces plausible-looking values
//! that are wrong only for unaligned addresses, which is a miserable bug to find.

use crate::alu::sext32;

/// Width and signedness of an aligned load.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum LoadKind {
    /// `LB` — signed byte.
    SignedByte,
    /// `LBU` — unsigned byte.
    UnsignedByte,
    /// `LH` — signed halfword.
    SignedHalf,
    /// `LHU` — unsigned halfword.
    UnsignedHalf,
    /// `LW` — signed word (sign-extended into the 64-bit register).
    SignedWord,
    /// `LWU` — unsigned word (zero-extended).
    UnsignedWord,
    /// `LD` — doubleword.
    Double,
}

impl LoadKind {
    /// Bytes accessed, which is also the required alignment.
    #[must_use]
    pub const fn width(self) -> u64 {
        match self {
            Self::SignedByte | Self::UnsignedByte => 1,
            Self::SignedHalf | Self::UnsignedHalf => 2,
            Self::SignedWord | Self::UnsignedWord => 4,
            Self::Double => 8,
        }
    }

    /// Is `addr` correctly aligned for this access?
    ///
    /// An unaligned access raises an address error rather than being fixed up —
    /// that is what the `LWL`/`LWR` family exists for.
    #[must_use]
    pub const fn is_aligned(self, addr: u64) -> bool {
        addr.is_multiple_of(self.width())
    }

    /// Shape raw big-endian bytes (right-justified in a `u64`) into the value the
    /// register receives.
    #[must_use]
    pub const fn shape(self, raw: u64) -> u64 {
        match self {
            Self::SignedByte => raw as u8 as i8 as i64 as u64,
            Self::UnsignedByte => raw as u8 as u64,
            Self::SignedHalf => raw as u16 as i16 as i64 as u64,
            Self::UnsignedHalf => raw as u16 as u64,
            // LW sign-extends into the 64-bit register; LWU does not. Confusing
            // this pair silently breaks any address above 2 GiB.
            Self::SignedWord => sext32(raw as u32),
            Self::UnsignedWord => raw as u32 as u64,
            Self::Double => raw,
        }
    }
}

/// Width of an aligned store.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum StoreKind {
    /// `SB` — byte.
    Byte,
    /// `SH` — halfword.
    Half,
    /// `SW` — word.
    Word,
    /// `SD` — doubleword.
    Double,
}

impl StoreKind {
    /// Bytes written, which is also the required alignment.
    #[must_use]
    pub const fn width(self) -> u64 {
        match self {
            Self::Byte => 1,
            Self::Half => 2,
            Self::Word => 4,
            Self::Double => 8,
        }
    }

    /// Is `addr` correctly aligned for this access?
    #[must_use]
    pub const fn is_aligned(self, addr: u64) -> bool {
        addr.is_multiple_of(self.width())
    }
}

/// `LWL` — merge the bytes from the addressed byte to the end of the containing
/// word into the **top** of `rt`, preserving `rt`'s low bytes.
///
/// `word` is the aligned word containing `addr`; `byte` is `addr & 3`.
/// Big-endian, so byte offset 0 selects the most-significant byte.
#[must_use]
pub const fn lwl(rt: u64, word: u32, byte: u64) -> u64 {
    let shift = (byte as u32) * 8;
    // Bits of `rt` that survive: the low `shift` bits.
    let keep = if shift == 0 { 0 } else { (1u32 << shift) - 1 };
    sext32((word << shift) | ((rt as u32) & keep))
}

/// `LWR` — merge the bytes from the start of the containing word up to the
/// addressed byte into the **bottom** of `rt`, preserving `rt`'s high bytes.
#[must_use]
pub const fn lwr(rt: u64, word: u32, byte: u64) -> u64 {
    let shift = (3 - byte as u32) * 8;
    // Bits of `rt` that survive: everything above the loaded bytes.
    let keep = if shift == 0 { 0 } else { !(u32::MAX >> shift) };
    sext32((word >> shift) | ((rt as u32) & keep))
}

/// `LDL` — the doubleword form of [`lwl`]. `byte` is `addr & 7`.
#[must_use]
pub const fn ldl(rt: u64, dword: u64, byte: u64) -> u64 {
    let shift = (byte as u32) * 8;
    let keep = if shift == 0 { 0 } else { (1u64 << shift) - 1 };
    (dword << shift) | (rt & keep)
}

/// `LDR` — the doubleword form of [`lwr`].
#[must_use]
pub const fn ldr(rt: u64, dword: u64, byte: u64) -> u64 {
    let shift = (7 - byte as u32) * 8;
    let keep = if shift == 0 { 0 } else { !(u64::MAX >> shift) };
    (dword >> shift) | (rt & keep)
}

/// `SWL` — merge the **top** bytes of `rt` into the addressed word.
///
/// Returns the new value of the aligned word containing `addr`.
#[must_use]
pub const fn swl(rt: u64, word: u32, byte: u64) -> u32 {
    let shift = (byte as u32) * 8;
    let keep = if shift == 0 { 0 } else { !(u32::MAX >> shift) };
    (word & keep) | ((rt as u32) >> shift)
}

/// `SWR` — merge the **bottom** bytes of `rt` into the addressed word.
#[must_use]
pub const fn swr(rt: u64, word: u32, byte: u64) -> u32 {
    let shift = (3 - byte as u32) * 8;
    let keep = if shift == 0 { 0 } else { (1u32 << shift) - 1 };
    (word & keep) | ((rt as u32) << shift)
}

/// `SDL` — the doubleword form of [`swl`].
#[must_use]
pub const fn sdl(rt: u64, dword: u64, byte: u64) -> u64 {
    let shift = (byte as u32) * 8;
    let keep = if shift == 0 { 0 } else { !(u64::MAX >> shift) };
    (dword & keep) | (rt >> shift)
}

/// `SDR` — the doubleword form of [`swr`].
#[must_use]
pub const fn sdr(rt: u64, dword: u64, byte: u64) -> u64 {
    let shift = (7 - byte as u32) * 8;
    let keep = if shift == 0 { 0 } else { (1u64 << shift) - 1 };
    (dword & keep) | (rt << shift)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_shaping_extends_by_width_and_signedness() {
        assert_eq!(
            LoadKind::SignedByte.shape(0xFF),
            u64::MAX,
            "LB sign-extends"
        );
        assert_eq!(LoadKind::UnsignedByte.shape(0xFF), 0xFF, "LBU does not");
        assert_eq!(LoadKind::SignedHalf.shape(0x8000), 0xFFFF_FFFF_FFFF_8000);
        assert_eq!(LoadKind::UnsignedHalf.shape(0x8000), 0x8000);
        // The LW/LWU distinction: LW sign-extends into the 64-bit register.
        assert_eq!(
            LoadKind::SignedWord.shape(0x8000_0000),
            0xFFFF_FFFF_8000_0000
        );
        assert_eq!(
            LoadKind::UnsignedWord.shape(0x8000_0000),
            0x0000_0000_8000_0000
        );
        assert_eq!(LoadKind::Double.shape(u64::MAX), u64::MAX);
    }

    #[test]
    fn alignment_requirements_match_access_width() {
        assert!(
            LoadKind::SignedByte.is_aligned(1),
            "bytes are always aligned"
        );
        assert!(!LoadKind::SignedHalf.is_aligned(1));
        assert!(LoadKind::SignedHalf.is_aligned(2));
        assert!(!LoadKind::SignedWord.is_aligned(2));
        assert!(LoadKind::SignedWord.is_aligned(4));
        assert!(!LoadKind::Double.is_aligned(4));
        assert!(LoadKind::Double.is_aligned(8));
        assert!(!StoreKind::Word.is_aligned(3));
        assert!(StoreKind::Double.is_aligned(16));
    }

    /// The canonical use: `LWL` then `LWR` assembles an unaligned word from two
    /// aligned accesses. This is the whole reason the family exists, so it is
    /// tested as the *pair* rather than as two independent transforms.
    ///
    /// Memory (big-endian): `00 11 22 33 | 44 55 66 77`
    /// An unaligned load at address 1 must yield `0x11223344`.
    #[test]
    fn lwl_plus_lwr_assembles_an_unaligned_word() {
        let word0: u32 = 0x0011_2233;
        let word1: u32 = 0x4455_6677;
        let rt = 0xDEAD_BEEF_DEAD_BEEF;

        // LWL rt, 1(base): addr 1 -> word0, byte 1.
        let high_half = lwl(rt, word0, 1);
        // LWR rt, 4(base): the pair's second access covers the next word.
        let assembled = lwr(high_half, word1, 0);
        assert_eq!(assembled, sext32(0x1122_3344));
    }

    /// At byte 0, `LWL` loads the whole word; at byte 3, `LWR` does. Those are
    /// the degenerate ends of the family and the easiest places to be off by a
    /// byte.
    #[test]
    fn the_unaligned_family_degenerates_to_a_full_word_at_the_ends() {
        let w = 0x0011_2233u32;
        assert_eq!(
            lwl(0xFFFF_FFFF_FFFF_FFFF, w, 0),
            sext32(w),
            "LWL @0 = whole word"
        );
        assert_eq!(
            lwr(0xFFFF_FFFF_FFFF_FFFF, w, 3),
            sext32(w),
            "LWR @3 = whole word"
        );
        // ...and at the other end each touches exactly one byte.
        assert_eq!(
            lwl(0, w, 3) as u32,
            0x3300_0000,
            "LWL @3 = one byte at the top"
        );
        assert_eq!(
            lwr(0, w, 0) as u32,
            0x0000_0000,
            "LWR @0 = one byte at the bottom"
        );
    }

    /// `LWL` preserves the low bytes of `rt` and `LWR` the high bytes — that
    /// preservation is what makes the pair composable.
    #[test]
    fn the_unaligned_loads_preserve_the_untouched_half_of_rt() {
        let rt = 0x0000_0000_AABB_CCDD;
        // LWL at byte 2 loads 2 bytes into the top, keeping rt's low 2 bytes.
        assert_eq!(lwl(rt, 0x1122_3344, 2) as u32 & 0xFFFF, 0xCCDD);
        // LWR at byte 1 loads 2 bytes into the bottom, keeping rt's top 2 bytes.
        assert_eq!(lwr(rt, 0x1122_3344, 1) as u32 >> 16, 0xAABB);
    }

    #[test]
    fn the_doubleword_unaligned_family_mirrors_the_word_one() {
        let d0 = 0x0011_2233_4455_6677u64;
        let d1 = 0x8899_AABB_CCDD_EEFFu64;
        // An unaligned doubleword load at byte 1 of d0.
        let r = ldr(ldl(0, d0, 1), d1, 0);
        assert_eq!(r, 0x1122_3344_5566_7788);
        // Degenerate ends.
        assert_eq!(ldl(u64::MAX, d0, 0), d0);
        assert_eq!(ldr(u64::MAX, d0, 7), d0);
    }

    /// Stores are the inverse: `SWL`/`SWR` merge parts of `rt` into memory while
    /// preserving the bytes outside the access.
    #[test]
    fn the_unaligned_stores_merge_into_memory_preserving_the_rest() {
        let mem = 0xAABB_CCDDu32;
        let rt = 0x1122_3344u64;
        // SWL at byte 1 writes rt's top 3 bytes into mem's low 3 bytes.
        assert_eq!(swl(rt, mem, 1), 0xAA11_2233);
        // SWR at byte 2: rt's low-order bytes fill BACKWARD from the addressed
        // byte to the word start -- mem[2]=0x44, mem[1]=0x33, mem[0]=0x22, and
        // mem[3] is untouched. (My first assertion here had the direction
        // backwards; `unaligned_store_then_load_round_trips` is what proves the
        // shift directions actually agree with each other.)
        assert_eq!(swr(rt, mem, 2), 0x2233_44DD);
        // Degenerate ends write the whole word.
        assert_eq!(swl(rt, mem, 0), rt as u32);
        assert_eq!(swr(rt, mem, 3), rt as u32);
        // Doubleword forms mirror it.
        assert_eq!(sdl(0x0123_4567_89AB_CDEF, 0, 0), 0x0123_4567_89AB_CDEF);
        assert_eq!(sdr(0x0123_4567_89AB_CDEF, 0, 7), 0x0123_4567_89AB_CDEF);
    }

    /// A store followed by the matching load pair must round-trip — the strongest
    /// statement that the shift directions agree with each other.
    #[test]
    fn unaligned_store_then_load_round_trips() {
        for byte in 0..4u64 {
            let mut w0 = 0u32;
            let mut w1 = 0u32;
            let value = 0x1122_3344u64;
            // SWL/SWR pair at `byte`.
            w0 = swl(value, w0, byte);
            if byte > 0 {
                w1 = swr(value, w1, byte - 1);
            }
            // LWL/LWR pair reads it back.
            let mut rt = 0u64;
            rt = lwl(rt, w0, byte);
            if byte > 0 {
                rt = lwr(rt, w1, byte - 1);
            }
            assert_eq!(rt, sext32(value as u32), "round trip failed at byte {byte}");
        }
    }
}
