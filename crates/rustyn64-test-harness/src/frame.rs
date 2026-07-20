//! The `.snap` / screenshot frame-hash comparator.
//!
//! Hashes a rendered RGBA framebuffer with a stable FNV-1a 64-bit digest and
//! compares it against a committed golden hash. The hash is real and
//! deterministic; the golden-corpus loader (reading `tests/golden/*.snap`) is a
//! frontend-side job and is left as a TODO.
//!
//! See `docs/testing-strategy.md` §visual golden corpus.

/// Stable FNV-1a 64-bit hash over a framebuffer's bytes.
///
/// Deterministic and allocation-free — the same framebuffer always hashes to
/// the same value, which is what makes it usable as a regression sentinel.
#[must_use]
pub fn frame_hash(framebuffer: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    for &b in framebuffer {
        h ^= u64::from(b);
        h = h.wrapping_mul(PRIME);
    }
    h
}

/// The result of comparing a rendered frame's hash to a golden hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameComparison {
    /// The frame hash matched the golden.
    Match,
    /// Mismatch: golden vs. actual hash.
    Mismatch {
        /// Committed golden hash.
        expected: u64,
        /// Hash of the frame under test.
        actual: u64,
    },
}

/// Compare a framebuffer against a committed golden hash.
#[must_use]
pub fn compare_to_golden(framebuffer: &[u8], golden: u64) -> FrameComparison {
    let actual = frame_hash(framebuffer);
    if actual == golden {
        FrameComparison::Match
    } else {
        FrameComparison::Mismatch {
            expected: golden,
            actual,
        }
    }
    // TODO(T-HARNESS-04): a `load_golden_snap(name)` that reads
    // `tests/golden/<name>.snap` so suites can reference goldens by name.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_deterministic() {
        assert_eq!(frame_hash(b"abc"), frame_hash(b"abc"));
        assert_ne!(frame_hash(b"abc"), frame_hash(b"abd"));
    }

    #[test]
    fn matches_self() {
        let fb = [1u8, 2, 3, 4];
        let golden = frame_hash(&fb);
        assert_eq!(compare_to_golden(&fb, golden), FrameComparison::Match);
    }
}
