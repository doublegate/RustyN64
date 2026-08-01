//! Is the GPU RDP path **reproducible**? (ADR 0004, ADR 0014)
//!
//! ADR 0004's contract is *same seed + ROM + input ⇒ bit-identical framebuffer +
//! audio*. The **core** already satisfies it and this backend does not touch the
//! core — the software rasterizer still produces the machine's framebuffer. What
//! is open is whether the *presented* picture, which comes from parallel-rdp, is
//! reproducible at all.
//!
//! That is an empirical question, so it is measured rather than reasoned about.
//! **Two** tests, covering properties that fail for different reasons:
//!
//! 1. **Fresh contexts agree, repeatedly.** The corpus is replayed `RUNS` times,
//!    and `conformance_gpu::replay` builds a **new device per vector** — so a
//!    run is 43 independently created contexts and the test compares 3 × 43 =
//!    129 of them. A failure means device creation carries something into the
//!    output. Repeating the whole corpus rather than one vector also spreads the
//!    comparison over time, where a wall-clock or entropy dependency is likelier
//!    to show than inside one tight loop.
//! 2. **A frame sequence agrees.** One backend rendering N successive frames,
//!    run twice, produces the same sequence. RDP state (TMEM, tiles, combiner)
//!    legitimately persists between frames, so this is where order-dependent or
//!    host-timing-dependent state would show — and a per-frame-fresh context
//!    would test a machine that does not exist.
//!
//! # What this cannot establish
//!
//! **Cross-vendor bit-exactness.** Every run here is on one GPU with one driver.
//! parallel-rdp is fixed-point integer compute — which is why it reproduces
//! Angrylion byte-for-byte at all — so there is reason to expect it to hold
//! across devices, but reason-to-expect is not evidence and this file does not
//! provide it. `docs/rdp.md` states the claim at the scope actually verified.

#![cfg(feature = "gpu-rdp")]
#![allow(
    clippy::doc_markdown,
    reason = "narrative test docs name RustyN64/RDRAM/Angrylion in prose"
)]

use rustyn64_test_harness::RDP_VECTORS;
use rustyn64_test_harness::conformance_gpu::{Unavailable, corpus_digest, repeated_frame_digest};

/// Runs of the whole corpus. Three is enough to catch a per-run seed while
/// keeping the test near ten seconds.
const RUNS: usize = 3;

/// Frames in the sequence test. Longer than the corpus so it wraps and revisits
/// vectors with different state behind them.
const SEQUENCE_FRAMES: usize = 60;

/// Distinct frame digests the sequence must produce.
///
/// **Asserted, not merely printed.** Three documents quote this number, and a
/// number quoted in prose while only being observed in a `println!` is the
/// gap this project keeps writing lessons about. It moves by hand, with those
/// documents, when the corpus changes.
const EXPECTED_DISTINCT: usize = 36;

/// Report a skip in the one form CI greps for, and say what went unverified.
fn skipped(what: &str) {
    println!("SKIPPED: no usable Vulkan device — {what} was NOT verified.");
}

#[test]
fn independent_devices_render_identically() {
    let first = match corpus_digest() {
        Ok(d) => d,
        Err(Unavailable::NoDevice) => return skipped("GPU reproducibility"),
        Err(e) => panic!("the GPU backend is unusable: {e:?}"),
    };
    // EVERY registered vector, not "enough of them". Compared against
    // `RDP_VECTORS.len()` rather than a literal so the two cannot drift: a
    // threshold would let a run that digested half the corpus pass while the
    // docs still claimed 43.
    assert_eq!(
        first.len(),
        RDP_VECTORS.len(),
        "the corpus digest covered {} of {} registered vectors",
        first.len(),
        RDP_VECTORS.len()
    );

    for run in 1..RUNS {
        let again = corpus_digest().expect("the device worked a moment ago");
        assert_eq!(
            again.len(),
            first.len(),
            "run {run} digested a different number of vectors"
        );
        for ((name, a), (_, b)) in first.iter().zip(&again) {
            assert_eq!(
                a, b,
                "run {run} rendered `{name}` differently from run 0 — the GPU \
                 path is not reproducible, so no determinism claim can rest on it"
            );
        }
    }
    println!(
        "{} vectors identical across {RUNS} independent runs",
        first.len()
    );
}

#[test]
fn a_frame_sequence_on_one_device_is_reproducible() {
    let first = match repeated_frame_digest(SEQUENCE_FRAMES) {
        Ok(d) => d,
        Err(Unavailable::NoDevice) => return skipped("GPU frame-sequence reproducibility"),
        Err(e) => panic!("the GPU backend is unusable: {e:?}"),
    };
    assert_eq!(first.len(), SEQUENCE_FRAMES, "the sequence ran short");

    // A sequence whose frames are all identical would satisfy the comparison
    // below while proving nothing about ordering. The corpus renders different
    // pictures, so this must hold — and if it ever does not, the test above is
    // measuring one repeated frame rather than a sequence.
    let distinct = first
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    assert_eq!(
        distinct, EXPECTED_DISTINCT,
        "the sequence produced {distinct} distinct frames, not {EXPECTED_DISTINCT}. \
         Update EXPECTED_DISTINCT together with the figure quoted in ADR 0015, \
         docs/rdp.md and the CHANGELOG"
    );

    let again = repeated_frame_digest(SEQUENCE_FRAMES).expect("the device worked a moment ago");
    // Before the `zip`: it stops at the shorter side, so a second run that
    // produced fewer frames would compare only the prefix and pass.
    assert_eq!(
        again.len(),
        first.len(),
        "the second run produced {} frames against the first run's {}",
        again.len(),
        first.len()
    );
    for (i, (a, b)) in first.iter().zip(&again).enumerate() {
        assert_eq!(
            a, b,
            "frame {i} of the sequence differed between two runs on fresh \
             devices — state carried across frames is not reproducible"
        );
    }
    println!("{SEQUENCE_FRAMES}-frame sequence reproduced exactly ({distinct} distinct frames)");
}
