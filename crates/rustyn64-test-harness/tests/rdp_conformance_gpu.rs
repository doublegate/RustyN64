//! The GPU-backend parity gate (ADR 0014).
//!
//! Runs the whole registered `.rvec` corpus through **both** RDP paths and
//! grades each against Angrylion's golden framebuffer — the independent oracle,
//! not one implementation against the other.
//!
//! Two properties this test is built around:
//!
//! 1. **It cannot pass vacuously.** A machine with no Vulkan device reports that
//!    it verified nothing, by name, rather than passing quietly. A green result
//!    with no output is not achievable here.
//! 2. **It asserts the recorded census, not a threshold.** The expected split
//!    below is a committed measurement. A vector moving in *either* direction —
//!    including the GPU starting to pass something it used to fail — fails this
//!    test, because an unexplained improvement is as much a signal as a
//!    regression.
#![cfg(feature = "gpu-rdp")]
#![allow(
    clippy::doc_markdown,
    reason = "narrative test docs name RustyN64/RDRAM/Angrylion in prose"
)]

use rustyn64_test_harness::conformance_gpu::{Unavailable, census};

// The one known divergence, and it is NOT a plumbing defect on this side.
//
// `tex_tri_chromakey_alpha_16` exercises the `key_en` chroma-key alpha
// compare. parallel-rdp **does not implement it**, and that is checkable
// rather than inferred from the pixels:
//
// - `op_set_other_modes` (`rdp_device.cpp`) decodes bits 9..19 of `words[0]`
//   and never bit 8, which is `key_en`. There is no `1 << 8` in the whole
//   function.
// - `op_set_key_r`/`op_set_key_gb` do store the key, but `Renderer::
//   set_color_key` puts `key_center`/`key_scale` into the combiner's mux
//   inputs (the KEY_CENTER / KEY_SCALE *sources*, a different feature), and
//   `key_width` — the comparison width the alpha compare needs — is written
//   and **never read**.
// - No shader under `parallel-rdp/shaders/` mentions a key at all.
//
// So this is a software-rasterizer feature the GPU backend lacks, which is
// the reverse of the expected direction and worth stating plainly. RustyN64
// implements it (PR #160) and matches Angrylion; parallel-rdp renders the
// shade color as black because the keyed combiner path is absent.
const KNOWN_GPU_GAPS: &[&str] = &["tex_tri_chromakey_alpha_16"];

#[test]
fn gpu_matches_angrylion_across_the_corpus() {
    let c = match census() {
        Ok(c) => c,
        Err(Unavailable::NoDevice) => {
            println!(
                "SKIPPED: no usable Vulkan device — the GPU parity of the .rvec \
                 corpus was NOT verified on this machine."
            );
            return;
        }
        Err(Unavailable::RdramUnmappable) => {
            panic!("a Vulkan device exists but its RDRAM could not be mapped for host access")
        }
    };

    println!("graded {} vectors", c.total());
    println!("  both paths match the golden : {}", c.both.len());
    println!("  software only               : {:?}", c.software_only);
    println!("  GPU only                    : {:?}", c.gpu_only);
    println!("  neither                     : {:?}", c.neither);

    assert!(
        c.total() >= 35,
        "the corpus shrank to {} vectors; the census would be grading almost nothing",
        c.total()
    );

    assert_eq!(
        c.software_only, KNOWN_GPU_GAPS,
        "the set of vectors only the software rasterizer reproduces changed. \
         A vector LEAVING this set is as much a signal as one joining it — it \
         means the GPU backend's behavior moved for a reason nobody recorded."
    );
    assert!(
        c.gpu_only.is_empty(),
        "the GPU reproduces vectors the software rasterizer does not: {:?}. \
         That is a software-rasterizer gap worth a ledger entry, not a failure \
         of this backend — but it must not appear silently.",
        c.gpu_only
    );
    assert!(
        c.neither.is_empty(),
        "neither path reproduces: {:?}",
        c.neither
    );
}
