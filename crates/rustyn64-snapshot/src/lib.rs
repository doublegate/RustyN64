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

/// serde `with` module for a `Box<[u8; N]>`: (de)serialise as a byte sequence.
///
/// `#[serde(with = "rustyn64_snapshot::boxed_bytes")]` on a `Box<[u8; N]>` field.
pub mod boxed_bytes {
    use alloc::boxed::Box;
    use alloc::vec::Vec;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// Serialise the boxed array as a borrowed byte slice.
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
        // `[u8]` serialises compactly (a bytes seq) in every format.
        value.as_slice().serialize(serializer)
    }

    /// Deserialise a byte sequence back into a `Box<[u8; N]>`, erroring on a
    /// wrong length (a corrupt / mismatched-version save-state).
    ///
    /// # Errors
    /// A [`serde::de::Error`] if the sequence is not exactly `N` bytes.
    pub fn deserialize<'de, D: Deserializer<'de>, const N: usize>(
        deserializer: D,
    ) -> Result<Box<[u8; N]>, D::Error> {
        let bytes = Vec::<u8>::deserialize(deserializer)?;
        let boxed: Box<[u8]> = bytes.into_boxed_slice();
        boxed
            .try_into()
            .map_err(|_| serde::de::Error::custom("boxed_bytes: wrong length for [u8; N]"))
    }
}

/// serde `with` module for an `Option<Box<[u8; N]>>`: `None` stays `None`, `Some`
/// (de)serialises through [`boxed_bytes`].
///
/// `#[serde(with = "rustyn64_snapshot::opt_boxed_bytes")]` on the field.
pub mod opt_boxed_bytes {
    use alloc::boxed::Box;
    use alloc::vec::Vec;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// Serialise `Option<Box<[u8; N]>>` as an optional byte sequence.
    ///
    /// # Errors
    /// Propagates the serializer's error.
    pub fn serialize<S: Serializer, const N: usize>(
        value: &Option<Box<[u8; N]>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        // Serialise as Option<&[u8]> so absence is preserved.
        value.as_ref().map(|b| b.as_slice()).serialize(serializer)
    }

    /// Deserialise an optional byte sequence back into `Option<Box<[u8; N]>>`.
    ///
    /// # Errors
    /// A [`serde::de::Error`] if a present sequence is not exactly `N` bytes.
    pub fn deserialize<'de, D: Deserializer<'de>, const N: usize>(
        deserializer: D,
    ) -> Result<Option<Box<[u8; N]>>, D::Error> {
        Option::<Vec<u8>>::deserialize(deserializer)?
            .map(|bytes| {
                let boxed: Box<[u8]> = bytes.into_boxed_slice();
                boxed
                    .try_into()
                    .map_err(|_| serde::de::Error::custom("opt_boxed_bytes: wrong length"))
            })
            .transpose()
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
