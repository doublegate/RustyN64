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
//! filter (slice 4d). The divot median, the 16-bit coverage path, and the field-rate
//! half of R-6 (PAL 50 Hz cadence / interlace) land in later slices.

use rustyn64_core::Bus;
use rustyn64_core::cpu::Bus as CpuBus;

/// The `.vivec` header (15 big-endian `u32`).
struct ViVec {
    ctrl: u32,
    origin: u32,
    width: u32,
    x_scale: u32,
    y_scale: u32,
    h_video: u32,
    v_video: u32,
    v_sync: u32,
    out_w: u32,
    out_h: u32,
    /// The raw source framebuffer bytes (logical pixels, big-endian, `src_w * src_h *
    /// src_bpp`). Big-endian matches `RustyN64`'s `RDRAM` order, so they are placed
    /// directly at `origin` — the same logical framebuffer Angrylion read.
    src: Vec<u8>,
    /// Golden output, `out_w * out_h` RGBA8 pixels.
    golden: Vec<u8>,
}

fn be_u32(b: &[u8], i: usize) -> u32 {
    u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn parse(bytes: &[u8]) -> ViVec {
    assert_eq!(be_u32(bytes, 0), 0x5649_5643, "bad .vivec magic");
    assert_eq!(be_u32(bytes, 4), 1, "unsupported .vivec version");
    let h = |i: usize| be_u32(bytes, i * 4);
    let (src_w, src_h) = (h(10), h(11));
    let src_bpp = h(12);
    assert!(src_bpp == 2 || src_bpp == 4, "src_bpp must be 2 or 4");
    let (out_w, out_h) = (h(13), h(14));
    let off = 15 * 4;
    let src_len = (src_w * src_h * src_bpp) as usize;
    let src = bytes[off..off + src_len].to_vec();
    let golden = bytes[off + src_len..off + src_len + (out_w * out_h * 4) as usize].to_vec();
    ViVec {
        ctrl: h(2),
        origin: h(3),
        width: h(4),
        x_scale: h(5),
        y_scale: h(6),
        h_video: h(7),
        v_video: h(8),
        v_sync: h(9),
        out_w,
        out_h,
        src,
        golden,
    }
}

/// Build a [`Bus`], place the source framebuffer big-endian at `origin`, program the
/// VI registers, and scan out through [`Bus::scanout_scaled`], asserting the RGB
/// matches the Angrylion golden.
fn assert_matches(name: &str, bytes: &[u8]) {
    const VI: u32 = 0x0440_0000; // VI register block base
    let v = parse(bytes);
    let mut bus = Bus::new();

    // The source bytes are big-endian logical pixels (16- or 32-bit), which is
    // RustyN64's RDRAM order, so they drop in directly at `origin` — the same logical
    // framebuffer Angrylion read (via its per-format access pattern).
    let base = v.origin as usize;
    bus.rdram[base..base + v.src.len()].copy_from_slice(&v.src);

    // Program the VI registers through the CPU MMIO path (VI regs are crate-private).
    CpuBus::write_u32(&mut bus, VI, v.ctrl); // VI_CTRL   (+0x00)
    CpuBus::write_u32(&mut bus, VI + 0x04, v.origin); // VI_ORIGIN (+0x04)
    CpuBus::write_u32(&mut bus, VI + 0x08, v.width); // VI_WIDTH  (+0x08)
    CpuBus::write_u32(&mut bus, VI + 0x18, v.v_sync); // VI_V_TOTAL(+0x18)
    CpuBus::write_u32(&mut bus, VI + 0x24, v.h_video); // VI_H_VIDEO(+0x24)
    CpuBus::write_u32(&mut bus, VI + 0x28, v.v_video); // VI_V_VIDEO(+0x28)
    CpuBus::write_u32(&mut bus, VI + 0x30, v.x_scale); // VI_X_SCALE(+0x30)
    CpuBus::write_u32(&mut bus, VI + 0x34, v.y_scale); // VI_Y_SCALE(+0x34)

    let mut frame = vec![0u8; (v.out_w * v.out_h * 4) as usize];
    let got_dims = bus.scanout_scaled(&mut frame);
    assert_eq!(
        got_dims,
        (v.out_w, v.out_h),
        "{name}: scan-out geometry differs from the golden"
    );

    for y in 0..v.out_h {
        for x in 0..v.out_w {
            let i = ((y * v.out_w + x) * 4) as usize;
            let got = &frame[i..i + 3];
            let want = &v.golden[i..i + 3];
            assert!(
                got == want,
                "{name}: RGB differs at pixel ({x},{y}): got {got:02X?}, golden {want:02X?}"
            );
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
    assert_matches(
        "vi_scale_1x_16",
        include_bytes!("vectors/vi_scale_1x_16.vivec"),
    );
}

/// **2× downscale — the 2.10 accumulator steps two source pixels per output pixel.**
/// `x_add = y_add = 0x800`, so `line_x = x_offs >> 10` advances by two each column;
/// pins the accumulator's integer addressing under a real scale factor.
#[test]
fn vi_scale_down2x_16_matches_angrylion() {
    assert_matches(
        "vi_scale_down2x_16",
        include_bytes!("vectors/vi_scale_down2x_16.vivec"),
    );
}

/// **The 5-bit bilinear lerp (slice 2).** `aa_mode = RESAMP_ONLY` (`VI_STATUS =
/// 0x0202`) enables the bilinear resample with the AA/divot/de-dither filters still
/// off. A 2× upscale (`x_add = y_add = 0x200`) makes `xfrac`/`yfrac` alternate 0 and
/// 0x10, so both the exact-passthrough and the 50 %-blend lerp paths — and both the
/// horizontal and vertical directions — are exercised. Pins `vi_lerp3`'s
/// `a + ((b-a)*frac + 16) >> 5` against Angrylion.
#[test]
fn vi_scale_bilinear_16_matches_angrylion() {
    assert_matches(
        "vi_scale_bilinear_16",
        include_bytes!("vectors/vi_scale_bilinear_16.vivec"),
    );
}

/// **The bilinear lerp's `+16 >> 5` rounding.** A non-power-of-two scale (`x_add =
/// y_add = 0x240`) yields `xfrac`/`yfrac` that are not multiples of 4, so the lerp
/// rounding bias changes the result — the `vi_scale_bilinear_16` vector's products
/// are all multiples of 32, hiding it. Dropping the `+16` fails this vector.
#[test]
fn vi_scale_bilinear_odd_16_matches_angrylion() {
    assert_matches(
        "vi_scale_bilinear_odd_16",
        include_bytes!("vectors/vi_scale_bilinear_odd_16.vivec"),
    );
}

/// **The gamma curve (slice 3).** `gamma_enable` set, `gamma_dither` clear
/// (`VI_STATUS = 0x030A`), nearest sampling. The sqrt gamma table is applied to the
/// final RGB (`gamma(0x40) = sqrt(0x1000) << 1 = 0x80`), so the output differs from
/// the raw sample — non-vacuous. Pins `vi_gamma`/`vi_integer_sqrt` against Angrylion.
#[test]
fn vi_gamma_1x_16_matches_angrylion() {
    assert_matches(
        "vi_gamma_1x_16",
        include_bytes!("vectors/vi_gamma_1x_16.vivec"),
    );
}

/// **The PAL active-span geometry (R-6, partial).** `v_sync = 625` (> 550) selects
/// the PAL branch: the horizontal overscan is 128 px, not NTSC's 108. With
/// `h_start = 115`, PAL's `-128` clamps to `-13` (so output column 0 samples source
/// column 13), while a mis-applied NTSC `-108` would sample column 8 — the golden's
/// first pixel is `src(13,0)`, so this distinguishes the PAL geometry. The field-rate
/// / interlace half of R-6 is still deferred.
#[test]
fn vi_pal_geometry_16_matches_angrylion() {
    assert_matches(
        "vi_pal_geometry_16",
        include_bytes!("vectors/vi_pal_geometry_16.vivec"),
    );
}

/// **32-bit RGBA8888 source with the bilinear resample (slice 4b).** `type = 3`,
/// `aa_mode = RESAMP_ONLY` (`VI_STATUS = 0x0203`), 2× upscale. Exercises the 32-bit
/// fetch (`vi_fetch32`, the big-endian R/G/B bytes) through the same `vi_lerp3` path
/// as the 16-bit bilinear — the source alpha carries coverage, not shown.
#[test]
fn vi_scale_bilinear_32_matches_angrylion() {
    assert_matches(
        "vi_scale_bilinear_32",
        include_bytes!("vectors/vi_scale_bilinear_32.vivec"),
    );
}

/// **The de-dither restore filter (slice 4c).** `aa_mode = 0` (reads real coverage),
/// `dither_filter_enable` (`VI_STATUS = 0x00010003`), 32-bit source, every pixel
/// fully covered (alpha `0xFF` → `cvg = 7`), 1:1 scale. So `restore_filter32` runs
/// everywhere: over the 3×3-minus-centre 8 taps, each channel is nudged ±1 toward the
/// neighbour's top-5-bit value. Non-vacuous — the output differs from the raw sample
/// (e.g. output col 0 = `0x1b`, not the raw `0x20`, because the row-0 top taps read 0).
#[test]
fn vi_dedither_32_matches_angrylion() {
    assert_matches(
        "vi_dedither_32",
        include_bytes!("vectors/vi_dedither_32.vivec"),
    );
}

/// **The AA edge filter (slice 4d).** `aa_mode = 0`, no dither/divot/gamma
/// (`VI_STATUS = 0x00000003`), 32-bit source where every 4th column is partial
/// (`cvg = 0`). A partial pixel takes `video_filter32`: it gathers the fully-covered
/// pixels among its 6 diagonal/two-away taps, takes the penultimate min/max per
/// channel (`vi_video_max`), and pulls the centre toward their midpoint by
/// `(7 - cvg)/8`. Non-vacuous — the partial columns differ from the raw sample, the
/// fully-covered columns are raw. 1:1 scale so no lerp.
#[test]
fn vi_aa_edge_32_matches_angrylion() {
    assert_matches(
        "vi_aa_edge_32",
        include_bytes!("vectors/vi_aa_edge_32.vivec"),
    );
}
