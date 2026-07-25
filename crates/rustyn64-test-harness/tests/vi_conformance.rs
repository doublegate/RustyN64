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
//! Slice 1 covers nearest-neighbour scaling (`aa_mode = REPLICATE`): the 2.10
//! accumulator, the active-span/overscan geometry, and the truncating RGBA5551→8
//! conversion. Bilinear + the AA/divot/de-dither filters land in later slices.

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
    src_w: u32,
    src_h: u32,
    out_w: u32,
    out_h: u32,
    /// Logical source pixels (RGBA5551), row-major `src_w * src_h`.
    src: Vec<u16>,
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
    assert_eq!(src_bpp, 2, "slice 1 only handles 16-bit source");
    let (out_w, out_h) = (h(13), h(14));
    let mut off = 15 * 4;
    let mut src = Vec::with_capacity((src_w * src_h) as usize);
    for _ in 0..src_w * src_h {
        src.push(u16::from_be_bytes([bytes[off], bytes[off + 1]]));
        off += 2;
    }
    let golden = bytes[off..off + (out_w * out_h * 4) as usize].to_vec();
    ViVec {
        ctrl: h(2),
        origin: h(3),
        width: h(4),
        x_scale: h(5),
        y_scale: h(6),
        h_video: h(7),
        v_video: h(8),
        v_sync: h(9),
        src_w,
        src_h,
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

    // Place the logical source pixels big-endian (RustyN64's RDRAM order) — the same
    // logical framebuffer Angrylion read via its WORD_ADDR_XOR placement.
    for y in 0..v.src_h {
        for x in 0..v.src_w {
            let px = v.src[(y * v.src_w + x) as usize];
            let b = (v.origin + (y * v.src_w + x) * 2) as usize;
            bus.rdram[b..b + 2].copy_from_slice(&px.to_be_bytes());
        }
    }

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
    let (w, h) = bus.scanout_scaled(&mut frame);
    assert_eq!(
        (w, h),
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
