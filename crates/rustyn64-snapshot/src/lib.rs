//! serde helpers for save-state serialisation (Phase 6, ADR 0004).
//!
//! `serde` derives cover almost every field of the emulator's state, and
//! `serde-big-array` covers plain fixed arrays larger than 32 (`[Line; 512]`,
//! `[u8; 64]`, …). What it does **not** cover is the emulator's `Box<[u8; N]>`
//! backing stores — RSP DMEM/IMEM, RDP TMEM, the PIF boot ROM — which are boxed
//! *arrays* (not slices), too large for serde's built-in array impls. These
//! `#[serde(with = "…")]` helpers serialise those as byte sequences and rebuild
//! the boxed array on the way back.
//!
//! Keeping the field types (`Box<[u8; N]>`) unchanged means the hot-path
//! indexing code is untouched; only the serialisation attribute is added.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

/// serde `with` module for a `Box<[u8; N]>`.
///
/// `#[serde(with = "rustyn64_snapshot::boxed_bytes")]` on a `Box<[u8; N]>` field.
/// (De)serialisation delegates to [`serde_big_array::BigArray`], which reads
/// **exactly `N`** elements as a fixed tuple — no length-prefixed `Vec` that a
/// malformed / hostile save-state could use to trigger an unbounded allocation
/// before the length check (CWE-400). The `[u8; N]` lands on the stack and is
/// then boxed.
pub mod boxed_bytes {
    use alloc::boxed::Box;

    use serde::{Deserializer, Serializer};
    use serde_big_array::BigArray;

    /// Serialise the boxed array as a fixed-length array (bounded, no length
    /// prefix an attacker could inflate).
    ///
    /// # Errors
    /// Propagates the serializer's error.
    #[allow(
        clippy::borrowed_box,
        reason = "serde's `with` serialize signature must take a reference to the exact field type, `Box<[u8; N]>`"
    )]
    pub fn serialize<S: Serializer, const N: usize>(
        value: &Box<[u8; N]>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        BigArray::serialize(value.as_ref(), serializer)
    }

    /// Deserialise exactly `N` bytes back into a `Box<[u8; N]>` (bounded).
    ///
    /// # Errors
    /// A [`serde::de::Error`] if the input is not exactly `N` elements.
    pub fn deserialize<'de, D: Deserializer<'de>, const N: usize>(
        deserializer: D,
    ) -> Result<Box<[u8; N]>, D::Error> {
        let arr: [u8; N] = BigArray::deserialize(deserializer)?;
        Ok(Box::new(arr))
    }
}

/// serde `with` module for an `Option<Box<[u8; N]>>`: `None` stays `None`, `Some`
/// (de)serialises through the bounded fixed-array path of [`boxed_bytes`].
///
/// `#[serde(with = "rustyn64_snapshot::opt_boxed_bytes")]` on the field.
pub mod opt_boxed_bytes {
    use alloc::boxed::Box;
    use core::marker::PhantomData;

    use serde::de::Deserialize;
    use serde::{Deserializer, Serialize, Serializer};
    use serde_big_array::BigArray;

    /// A borrowed `[u8; N]` that serialises via [`BigArray`] (for `Some`).
    struct BorrowBig<'a, const N: usize>(&'a [u8; N]);
    impl<const N: usize> Serialize for BorrowBig<'_, N> {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            BigArray::serialize(self.0, s)
        }
    }

    /// An owned `Box<[u8; N]>` that deserialises via [`BigArray`] (bounded).
    struct OwnBig<const N: usize>(Box<[u8; N]>, PhantomData<()>);
    impl<'de, const N: usize> Deserialize<'de> for OwnBig<N> {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            let arr: [u8; N] = BigArray::deserialize(d)?;
            Ok(Self(Box::new(arr), PhantomData))
        }
    }

    /// Serialise `Option<Box<[u8; N]>>`, preserving absence.
    ///
    /// # Errors
    /// Propagates the serializer's error.
    pub fn serialize<S: Serializer, const N: usize>(
        value: &Option<Box<[u8; N]>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value
            .as_ref()
            .map(|b| BorrowBig(b.as_ref()))
            .serialize(serializer)
    }

    /// Deserialise `Option<Box<[u8; N]>>` (bounded fixed-array when present).
    ///
    /// # Errors
    /// A [`serde::de::Error`] if a present value is not exactly `N` elements.
    pub fn deserialize<'de, D: Deserializer<'de>, const N: usize>(
        deserializer: D,
    ) -> Result<Option<Box<[u8; N]>>, D::Error> {
        Ok(Option::<OwnBig<N>>::deserialize(deserializer)?.map(|w| w.0))
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use alloc::boxed::Box;

    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Holder {
        #[serde(with = "crate::boxed_bytes")]
        mem: Box<[u8; 4096]>,
        #[serde(with = "crate::opt_boxed_bytes")]
        maybe: Option<Box<[u8; 64]>>,
    }

    #[test]
    fn boxed_arrays_round_trip_through_json() {
        let mut mem = Box::new([0u8; 4096]);
        mem[0] = 1;
        mem[4095] = 0xAB;
        let h = Holder {
            mem,
            maybe: Some(Box::new([7u8; 64])),
        };
        let json = serde_json::to_string(&h).unwrap();
        let back: Holder = serde_json::from_str(&json).unwrap();
        assert_eq!(h, back);

        let none = Holder {
            mem: Box::new([0u8; 4096]),
            maybe: None,
        };
        let j = serde_json::to_string(&none).unwrap();
        assert_eq!(none, serde_json::from_str::<Holder>(&j).unwrap());
    }
}
