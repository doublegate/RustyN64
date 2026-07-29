//! VI scan-out conformance vs Angrylion (ledger R-5, gap-analysis Stage D).
//!
//! Each `.vivec` was produced by driving Angrylion's real VI pipeline
//! (`vi_process_full` via `n64video_update_screen`) — see
//! `crates/rustyn64-test-harness/vectors-gen/driver.c`. It carries the VI register
//! state, a logical 16-bit source framebuffer, and the golden RGBA8 output. This
//! test replays each through [`Bus::scanout_scaled`] and compares **RGB** (the VI
//! carries coverage in its output alpha, which `RustyN64` renders as opaque `0xFF`
//! for display, so alpha is not part of the comparison).
//!
//! Covered so far: nearest-neighbour scaling + the active-span/overscan geometry +
//! the truncating RGBA5551→8 conversion (slice 1), the 5-bit bilinear lerp (slice 2),
//! the sqrt gamma curve (slice 3), the PAL geometry (slice 4a), 32-bit source
//! resampling (slice 4b), the de-dither restore filter (slice 4c), and the AA edge
//! filter (slice 4d), the divot median (slice 4e), and the 16-bit coverage path — the
//! hidden-bits plane feeding de-dither / AA-edge / divot (slice 4f). The field-rate
//! half of R-6 (PAL 50 Hz cadence / interlace) lands in a later slice.

use rustyn64_test_harness::conformance::{ViMismatch, vi_first_mismatch, vi_vector_bytes};

/// Assert `RustyN64`'s VI scan-out reproduces the Angrylion golden for `name`.
///
/// The vector's bytes come from `conformance::VI_VECTORS`, the single source of
/// truth shared with the accuracy battery — so a vector can never be exercised
/// here while being invisible to the battery, and a name typo fails loudly.
fn assert_matches(name: &str) {
    match vi_first_mismatch(vi_vector_bytes(name)) {
        None => (),
        Some(ViMismatch::Geometry { got, golden }) => panic!(
            "{name}: scan-out geometry differs from the golden: got {got:?}, golden {golden:?}"
        ),
        Some(ViMismatch::Pixel { x, y, got, golden }) => {
            panic!("{name}: RGB differs at pixel ({x},{y}): got {got:02X?}, golden {golden:02X?}")
        }
    }
}

/// **1:1 scan-out — geometry + integer addressing.** No scale (`x_add = y_add =
/// 0x400`), so this pins the active-span/overscan geometry: the NTSC 108-px
/// horizontal overscan makes the first visible column sample source column 8, and
/// the truncating RGBA5551→8 conversion. Non-vacuous: the source pixel encodes its
/// `(x, y)` in separate channels, so any mis-addressed sample lands on a wrong colour.
#[test]
fn vi_scale_1x_16_matches_angrylion() {
    assert_matches("vi_scale_1x_16");
}

/// **2× downscale — the 2.10 accumulator steps two source pixels per output pixel.**
/// `x_add = y_add = 0x800`, so `line_x = x_offs >> 10` advances by two each column;
/// pins the accumulator's integer addressing under a real scale factor.
#[test]
fn vi_scale_down2x_16_matches_angrylion() {
    assert_matches("vi_scale_down2x_16");
}

/// **The 5-bit bilinear lerp (slice 2).** `aa_mode = RESAMP_ONLY` (`VI_STATUS =
/// 0x0202`) enables the bilinear resample with the AA/divot/de-dither filters still
/// off. A 2× upscale (`x_add = y_add = 0x200`) makes `xfrac`/`yfrac` alternate 0 and
/// 0x10, so both the exact-passthrough and the 50 %-blend lerp paths — and both the
/// horizontal and vertical directions — are exercised. Pins `vi_lerp3`'s
/// `a + ((b-a)*frac + 16) >> 5` against Angrylion.
#[test]
fn vi_scale_bilinear_16_matches_angrylion() {
    assert_matches("vi_scale_bilinear_16");
}

/// **The bilinear lerp's `+16 >> 5` rounding.** A non-power-of-two scale (`x_add =
/// y_add = 0x240`) yields `xfrac`/`yfrac` that are not multiples of 4, so the lerp
/// rounding bias changes the result — the `vi_scale_bilinear_16` vector's products
/// are all multiples of 32, hiding it. Dropping the `+16` fails this vector.
#[test]
fn vi_scale_bilinear_odd_16_matches_angrylion() {
    assert_matches("vi_scale_bilinear_odd_16");
}

/// **The gamma curve (slice 3).** `gamma_enable` set, `gamma_dither` clear
/// (`VI_STATUS = 0x030A`), nearest sampling. The sqrt gamma table is applied to the
/// final RGB (`gamma(0x40) = sqrt(0x1000) << 1 = 0x80`), so the output differs from
/// the raw sample — non-vacuous. Pins `vi_gamma`/`vi_integer_sqrt` against Angrylion.
#[test]
fn vi_gamma_1x_16_matches_angrylion() {
    assert_matches("vi_gamma_1x_16");
}

/// **The PAL active-span geometry (R-6, partial).** `v_sync = 625` (> 550) selects
/// the PAL branch: the horizontal overscan is 128 px, not NTSC's 108. With
/// `h_start = 115`, PAL's `-128` clamps to `-13` (so output column 0 samples source
/// column 13), while a mis-applied NTSC `-108` would sample column 8 — the golden's
/// first pixel is `src(13,0)`, so this distinguishes the PAL geometry. The field-rate
/// / interlace half of R-6 is still deferred.
#[test]
fn vi_pal_geometry_16_matches_angrylion() {
    assert_matches("vi_pal_geometry_16");
}

/// **32-bit RGBA8888 source with the bilinear resample (slice 4b).** `type = 3`,
/// `aa_mode = RESAMP_ONLY` (`VI_STATUS = 0x0203`), 2× upscale. Exercises the 32-bit
/// fetch (`vi_fetch32`, the big-endian R/G/B bytes) through the same `vi_lerp3` path
/// as the 16-bit bilinear — the source alpha carries coverage, not shown.
#[test]
fn vi_scale_bilinear_32_matches_angrylion() {
    assert_matches("vi_scale_bilinear_32");
}

/// **The de-dither restore filter (slice 4c).** `aa_mode = 0` (reads real coverage),
/// `dither_filter_enable` (`VI_STATUS = 0x00010003`), 32-bit source, every pixel
/// fully covered (alpha `0xFF` → `cvg = 7`), 1:1 scale. So `restore_filter32` runs
/// everywhere: over the 3×3-minus-centre 8 taps, each channel is nudged ±1 toward the
/// neighbour's top-5-bit value. Non-vacuous — the output differs from the raw sample
/// (e.g. output col 0 = `0x1b`, not the raw `0x20`, because the row-0 top taps read 0).
#[test]
fn vi_dedither_32_matches_angrylion() {
    assert_matches("vi_dedither_32");
}

/// **The AA edge filter (slice 4d).** `aa_mode = 0`, no dither/divot/gamma
/// (`VI_STATUS = 0x00000003`), 32-bit source where every 4th column is partial
/// (`cvg = 0`). A partial pixel takes `video_filter32`: it gathers the fully-covered
/// pixels among its 6 diagonal/two-away taps, takes the penultimate min/max per
/// channel (`vi_video_max`), and pulls the centre toward their midpoint by
/// `(7 - cvg)/8`. The partial pixels are a fixed dark colour (not the gradient's local
/// midpoint), so the filter changes them measurably at **interior** pixels too, not
/// just the top boundary — a raw-fetch mutation fails everywhere. 1:1 scale so no lerp.
#[test]
fn vi_aa_edge_32_matches_angrylion() {
    assert_matches("vi_aa_edge_32");
}

/// **The divot median filter (slice 4e).** `divot_enable` (bit 4), `aa_mode = 0`
/// (`VI_STATUS = 0x00000013`), the same 32-bit every-4th-column-partial source. A
/// pixel whose 3 horizontal neighbours are not all fully covered (the partial columns
/// and their immediate neighbours) takes the per-channel median of the post-AA values;
/// the rest pass through. Non-vacuous — the output differs from the non-divot AA-edge
/// result at those pixels (the median ≠ the AA blend). 1:1 scale so no lerp.
#[test]
fn vi_divot_32_matches_angrylion() {
    assert_matches("vi_divot_32");
}

/// **The 16-bit de-dither restore filter (slice 4f).** `type = 2` (RGBA5551),
/// `aa_mode = 0`, `dither_filter_enable` (`VI_STATUS = 0x00010002`), every pixel fully
/// covered — here coverage comes from the **hidden-bits plane** (`((px & 1) << 2) |
/// hidden`, bit 0 = 1 and hidden = `0b11` ⇒ `cvg = 7`), not a 32-bit alpha byte. The
/// same `restore_filter` runs, comparing the stored 5-bit channels (`rgb8 >> 3`). This
/// pins the 16-bit coverage read and the 5-bit→8-bit unpack against Angrylion.
#[test]
fn vi_dedither_16_matches_angrylion() {
    assert_matches("vi_dedither_16");
}

/// **The 16-bit AA edge filter (slice 4f).** `type = 2`, `aa_mode = 0` (`VI_STATUS =
/// 0x00000002`), every 4th column partial via the hidden plane (bit 0 = 0, hidden = 0
/// ⇒ `cvg = 0`). A partial pixel takes `video_filter` over its fully-covered taps,
/// unpacking each 5-bit channel to 8-bit — the same blend arithmetic as the 32-bit
/// path. Confirms coverage is read from the hidden plane for the AA-edge branch.
#[test]
fn vi_aa_edge_16_matches_angrylion() {
    assert_matches("vi_aa_edge_16");
}

/// **The 16-bit divot median filter (slice 4f).** `divot_enable` (bit 4), `type = 2`
/// (`VI_STATUS = 0x00000012`), the every-4th-column-partial hidden-plane pattern plus a
/// non-monotonic fully-covered probe triplet (source columns 17/18/19, row 10) so the
/// all-fully-covered early-return is observable: its centre lands on an output pixel
/// with the median differing from the centre. Confirms the divot path over 16-bit
/// coverage. Deleting the early-return computes the median and fails the vector.
#[test]
fn vi_divot_16_matches_angrylion() {
    assert_matches("vi_divot_16");
}
