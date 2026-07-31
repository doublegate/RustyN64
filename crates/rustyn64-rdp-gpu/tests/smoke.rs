//! End-to-end smoke test for the `parallel-rdp` binding (ADR 0014).
//!
//! This test has to be honest about an environment it cannot control: a machine
//! with no Vulkan ICD, or a headless CI runner, legitimately cannot create a
//! device. So it is written so that **both outcomes are informative and neither
//! is a false pass**:
//!
//! - No device: the test prints why and returns. It asserts nothing about
//!   rendering, and says so out loud rather than passing silently.
//! - Device present: it renders a real frame and asserts the picture, so the
//!   "it works" claim is witnessed rather than assumed.
//!
//! Run with `--nocapture` to see which branch was taken. That output is the
//! whole point — a green result alone does not tell you the backend ran.

#![cfg(feature = "gpu-rdp")]

use rustyn64_rdp_gpu::{GpuRdp, ScanoutFrame, ViRegister};

/// 4 MiB is enough RDRAM for a small framebuffer and matches the base N64.
const RDRAM_SIZE: usize = 4 * 1024 * 1024;

/// The alignment parallel-rdp needs to import host memory directly.
///
/// `VK_EXT_external_memory_host` maps the caller's buffer into the GPU's
/// address space instead of staging every access through a copy — which is the
/// whole reason the shim hands RDRAM over rather than copying it. The driver's
/// `minImportedHostPointerAlignment` is 4096 on the NVIDIA device this was
/// developed against, and a plain `Vec<u8>` does not meet it: the first version
/// of this test produced `Host buffer is not aligned appropriately` /
/// `falling back to a slower path` in parallel-rdp's own log while otherwise
/// passing. 4096 is a page and is the common value, but it is a *property of
/// the driver*, not a constant of the API — a device requiring more would fall
/// back again, silently, exactly as this did.
const PAGE: usize = 4096;

/// parallel-rdp's hidden RDRAM is one byte per RDRAM half-word.
const HIDDEN_RDRAM_SIZE: usize = RDRAM_SIZE / 2;

const FB_ADDR: u32 = 0x0010_0000;
const FB_WIDTH: u32 = 64;
const FB_HEIGHT: u32 = 32;

/// `VI_CONTROL` for 32-bit RGBA, no AA, no filtering — the plainest scan-out
/// there is, chosen so a wrong picture is unambiguous rather than arguable.
const VI_CONTROL_32BPP: u32 = 0x0000_0003;

/// Opaque red as the RDP writes it and the scan-out returns it: R, G, B, A in
/// ascending byte order, so `0xFF0000FF` as a little-endian `u32`. Observed,
/// not assumed — a probe run produced exactly two values in the whole frame,
/// `0x00000000` and this.
const FILL_RGBA: u32 = 0xFF00_00FF;

/// Black, with or without alpha.
///
/// The scan-out uses `0x00000000` outside the VI's active window, and
/// `0xFF000000` for a cleared framebuffer inside it — the latter observed by
/// removing the Fill Rectangle command and reading what came back. Both are
/// "nothing was drawn here", and conflating them with a genuinely unexpected
/// color is what made the first version of this test's assertions collapse
/// into one.
const fn is_background(px: u32) -> bool {
    px == 0x0000_0000 || px == 0xFF00_0000
}

/// Assert a submission was accepted.
///
/// `enqueue_command` and `set_vi_register` are `#[must_use]` precisely so a
/// dropped submission cannot pass unnoticed, and this is the test honoring that
/// rather than routing around it with a `let _ =`. A refused command would
/// otherwise show up only as a wrong picture, several assertions later.
#[track_caller]
fn submit(accepted: bool) {
    assert!(accepted, "the backend refused a submission");
}

#[test]
fn a_filled_rectangle_reaches_the_scanout() {
    // Over-allocate and slice to the next page boundary. Deliberately NOT a
    // `repr(align)` type reinterpreted as bytes: that needs `unsafe`, and this
    // needs none — an aligned buffer is a property of the address, and the
    // address is something safe code can inspect.
    let mut backing = vec![0u8; RDRAM_SIZE + PAGE];
    let offset = backing.as_ptr().align_offset(PAGE);
    let rdram = &mut backing[offset..offset + RDRAM_SIZE];
    assert_eq!(
        rdram.as_ptr() as usize % PAGE,
        0,
        "the RDRAM backing is not page-aligned, so the direct-import path would \
         silently fall back to staging"
    );
    let Some(mut gpu) = GpuRdp::new(rdram, HIDDEN_RDRAM_SIZE) else {
        println!(
            "SKIPPED: prdp_create returned null — no usable Vulkan device here. \
             Nothing about rendering was verified."
        );
        return;
    };

    assert!(
        gpu.device_is_supported(),
        "a context was created over a device parallel-rdp reports as unsupported; \
         prdp_create should have refused"
    );
    println!("device is supported; rendering a real frame");

    // Set Color Image (0x3F). In the 64-bit command: format at [55:53], size at
    // [52:51], width-1 at [41:32], address in the low word — so in this high
    // word, format at [23:21] (0 = RGBA), size at [20:19] (3 = 32bpp), width-1
    // at [9:0]. Bits [18:10] are unused and are left clear: a review caught an
    // earlier `(0b11 << 16)` here, which set two of them for no reason, and a
    // backend that ignores undefined bits would have passed the test regardless.
    submit(gpu.enqueue_command(&[0x3F00_0000 | (0b11 << 19) | (FB_WIDTH - 1), FB_ADDR]));
    // Set Scissor (0x2D) over the whole framebuffer, in 10.2 fixed point.
    submit(gpu.enqueue_command(&[0x2D00_0000, ((FB_WIDTH << 2) << 12) | (FB_HEIGHT << 2)]));
    // Set Other Modes (0x2F) with cycle type = FILL (0b11 at bit 52).
    submit(gpu.enqueue_command(&[0x2F00_0000 | (0b11 << 20), 0x0000_0000]));
    // Set Fill Color (0x37) — in 32bpp FILL mode this word is one whole pixel.
    submit(gpu.enqueue_command(&[0x3700_0000, FILL_RGBA]));
    // Fill Rectangle (0x36): xh/yh in the low word, xl/yl in the high word,
    // all 10.2 fixed point.
    submit(gpu.enqueue_command(&[
        0x3600_0000 | (((FB_WIDTH - 1) << 2) << 12) | ((FB_HEIGHT - 1) << 2),
        0,
    ]));
    // Sync Full (0x29) so the rasterizer is drained before scan-out.
    submit(gpu.enqueue_command(&[0x2900_0000, 0]));

    submit(gpu.set_vi_register(ViRegister::Control, VI_CONTROL_32BPP));
    submit(gpu.set_vi_register(ViRegister::Origin, FB_ADDR));
    submit(gpu.set_vi_register(ViRegister::Width, FB_WIDTH));
    submit(gpu.set_vi_register(ViRegister::XScale, 0x0000_0400)); // 1:1
    submit(gpu.set_vi_register(ViRegister::YScale, 0x0000_0400)); // 1:1
    submit(gpu.set_vi_register(ViRegister::HStart, (108 << 16) | (108 + FB_WIDTH)));
    submit(gpu.set_vi_register(ViRegister::VStart, (34 << 16) | (34 + FB_HEIGHT * 2)));

    let mut out = vec![0u32; 1024 * 1024];
    let Some(ScanoutFrame {
        width,
        height,
        pixels,
    }) = gpu.scanout_sync(&mut out)
    else {
        panic!(
            "scanout_sync produced no frame on a supported device; the VI was \
             programmed for a {FB_WIDTH}x{FB_HEIGHT} window"
        );
    };

    println!("scanout: {width}x{height}, {pixels} pixels");
    assert_eq!(
        pixels,
        width as usize * height as usize,
        "pixel count disagrees with the reported geometry"
    );
    assert!(width > 0 && height > 0, "scan-out reported empty geometry");

    // Three separate claims, because "a buffer came back" is not "it rendered"
    // and no single one of these distinguishes them:
    //
    // 1. every non-black pixel is EXACTLY the fill color — noise, an
    //    uninitialized read-back, or a filtered/blended result all fail this;
    // 2. there is a rectangle's worth of it — a fill of 64x32 is ~2k pixels, so
    //    a handful would mean something other than this command drew;
    // 3. it does not cover the frame — a whole-frame wash would satisfy (1) and
    //    (2) while meaning the rasterizer ignored the scissor and the rectangle
    //    alike.
    //
    // Mutation results, stated as measured rather than as intended — the first
    // version of this comment got the mapping wrong twice:
    //
    // - removing Fill Rectangle    -> (2), at zero
    // - shrinking it to 1x1        -> (2), at zero
    // - removing Set Fill Color    -> (2), at zero (the fill becomes black)
    // - enlarging it past the frame-> nothing; the scissor clips it back to
    //                                 64x32, so (3) is not reachable from here
    //
    // So (2) is the assertion carrying the weight, and (1) and (3) are guards
    // against a backend regression that this test's own mutations cannot
    // provoke. Worth keeping and worth not overclaiming.
    //
    // An earlier draft counted only `0x00000000` as background, and then EVERY
    // mutation tripped (1) while (2) never ran at all — a cleared framebuffer
    // inside the VI's active window scans out as opaque black, which read as a
    // "color that was never drawn". [`is_background`] exists for that reason.
    let mut fill = 0usize;
    let mut foreign = None;
    for &px in &out[..pixels] {
        if px == FILL_RGBA {
            fill += 1;
        } else if !is_background(px) && foreign.is_none() {
            foreign = Some(px);
        }
    }
    assert!(
        foreign.is_none(),
        "the scan-out contains a color that was never drawn: {:08X}",
        foreign.unwrap_or_default()
    );
    assert!(
        fill >= 1000,
        "the filled rectangle is not in the scan-out: only {fill} of {pixels} \
         pixels carry the fill color"
    );
    assert!(
        fill * 4 < pixels,
        "the fill covers {fill} of {pixels} pixels — the scissor and the \
         rectangle's own extents were both ignored"
    );
    println!("fill color occupies {fill} of {pixels} pixels");
}
