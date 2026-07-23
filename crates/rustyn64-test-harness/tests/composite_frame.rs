//! A composite "first picture" golden frame (Phase 3, toward T-33-006).
//!
//! Where `golden_frame.rs` pins a single FILL rectangle, this drives a small
//! *multi-primitive* scene end to end — a FILL-mode background rectangle plus a
//! 1-cycle shaded triangle drawn over it — through the RDP and the VI scan-out,
//! and pins the result against a committed golden hash. It exercises the
//! rasteriser features the Sprint-3 slices added (FILL, cycle-type switching, the
//! combiner, sub-pixel-coverage shaded triangles) composed into one frame, and
//! guards the whole compose → scan-out path against silent drift.
//!
//! The frame is a synthetic command stream, not a commercial ROM (cartridge boot
//! is Phase 5); the path exercised is identical from the DP FIFO onward. Because
//! the golden is RustyN64's own output, the test's job is **determinism** (ADR
//! 0004): the committed hash detects any rendering change, and the scene is
//! rendered twice to prove the output is bit-identical across runs.

#![allow(
    clippy::doc_markdown,
    reason = "narrative test docs name RustyN64/RDRAM in prose"
)]

use rustyn64_core::Bus;
use rustyn64_core::cpu::Bus as CpuBus;
use rustyn64_test_harness::frame::{FrameComparison, compare_to_golden, frame_hash};

/// The committed golden hash of the 8×8 32-bit composite frame (blue background
/// with a red shaded triangle). Pinned: a rendering change alters the frame bytes
/// and so this digest, failing the test until the golden is deliberately updated.
const GOLDEN_COMPOSITE_8X8: u64 = 1_768_948_994_989_935_524;

const FB_ADDR: u32 = 0x1000;
const CMD_ADDR: u32 = 0x4000;

/// Write one 64-bit RDP command word (`hi` = bits 63:32, `lo` = 31:0) big-endian.
fn write_cmd(bus: &mut Bus, addr: u32, hi: u32, lo: u32) {
    let a = addr as usize;
    bus.rdram[a..a + 4].copy_from_slice(&hi.to_be_bytes());
    bus.rdram[a + 4..a + 8].copy_from_slice(&lo.to_be_bytes());
}

/// Render the composite scene into an 8×8 32-bit framebuffer and scan it out to
/// RGBA8. The command list: Set Color Image, then a FILL-mode blue background
/// (Set Other Modes FILL, Set Fill Color, Set Scissor, Fill Rectangle), then a
/// 1-cycle red shaded triangle (Set Other Modes 1-cycle, Set Combine passthrough,
/// Fill Shaded Triangle with its 16-word shade block).
fn render_composite_frame() -> Vec<u8> {
    let mut bus = Bus::new();
    let mut a = CMD_ADDR;
    let push = |bus: &mut Bus, hi: u32, lo: u32, a: &mut u32| {
        write_cmd(bus, *a, hi, lo);
        *a += 8;
    };

    // Set Color Image: 32-bit (size 3), width 8, base FB_ADDR.
    push(&mut bus, 0x3F18_0007, FB_ADDR, &mut a);
    // Background: FILL mode, blue (0x0000FFFF), the whole 8×8, then a Fill Rectangle.
    push(&mut bus, 0x2F30_0000, 0x0000_0000, &mut a); // Set Other Modes: FILL
    push(&mut bus, 0x3700_0000, 0x0000_FFFF, &mut a); // Set Fill Color: blue
    push(&mut bus, 0x2D00_0000, 0x0020_0020, &mut a); // Set Scissor (0,0)-(8,8)
    push(&mut bus, 0x3602_0020, 0x0000_0000, &mut a); // Fill Rectangle (0,0)-(8,8)
    // Foreground: a 1-cycle red shaded triangle over the background.
    push(&mut bus, 0x2F00_00F0, 0x0000_0000, &mut a); // Set Other Modes: 1-cycle, dither off
    push(&mut bus, 0x3C00_0000, 0x0000_0104, &mut a); // Set Combine: shade passthrough
    // Fill Shaded Triangle 0x0C: 8-word base + 16-word shade block. A left-major
    // wide triangle (DxMDy = 1.0), flat red shade.
    let tri = a;
    push(&mut bus, 0x0C80_0020, 0x0020_0000, &mut a); // op 0x0C, lft=1, yl=32, ym=32, yh=0
    push(&mut bus, 0x0000_0000, 0x0000_0000, &mut a); // XL, DxLDy
    push(&mut bus, 0x0002_0000, 0x0000_0000, &mut a); // XH = 2.0
    push(&mut bus, 0x0002_0000, 0x0001_0000, &mut a); // XM = 2.0, DxMDy = 1.0
    // Shade block: int base R=0xFF G=0 B=0 A=0xFF, all deltas zero (16 u32).
    write_cmd(&mut bus, a, 0x00FF_0000, 0x0000_00FF);
    a += 8;
    for _ in 0..7 {
        write_cmd(&mut bus, a, 0, 0);
        a += 8;
    }
    let _ = tri;

    // Drain the whole list.
    bus.rdp.dpc_write(0, CMD_ADDR);
    bus.rdp.dpc_write(1, a);
    for _ in 0..((a - CMD_ADDR) / 4 + 16) {
        bus.rdp_tick();
    }

    // Scan out the 8×8 32-bit framebuffer through the VI.
    CpuBus::write_u32(&mut bus, 0x0440_0000, 3); // VI_CTRL: TYPE = 32-bit
    CpuBus::write_u32(&mut bus, 0x0440_0004, FB_ADDR); // VI_ORIGIN
    CpuBus::write_u32(&mut bus, 0x0440_0008, 8); // VI_WIDTH
    CpuBus::write_u32(&mut bus, 0x0440_0028, 16); // VI_V_VIDEO: 16 half-lines -> h=8

    let mut frame = vec![0u8; 8 * 8 * 4];
    let (w, h) = bus.scanout(&mut frame);
    assert_eq!((w, h), (8, 8), "scan-out geometry");
    frame
}

#[test]
fn composite_scene_renders_the_committed_golden_frame() {
    let frame = render_composite_frame();

    // Determinism (ADR 0004): a second render of the same command stream is
    // bit-identical — the core has no wall-clock/RNG in the render path.
    assert_eq!(
        frame,
        render_composite_frame(),
        "the composite scene renders identically across runs (determinism)"
    );

    // The frame is a real scene, not a flat fill: it contains both the blue
    // background and red foreground pixels (proving the triangle drew over the
    // fill), so the golden pins a genuine multi-primitive picture.
    let px = |x: usize, y: usize| -> [u8; 4] {
        let i = (y * 8 + x) * 4;
        [frame[i], frame[i + 1], frame[i + 2], frame[i + 3]]
    };
    // (0,0) is background (the triangle starts at x=2); a lower interior pixel is red.
    assert_eq!(
        px(0, 0),
        [0x00, 0x00, 0xFF, 0xFF],
        "corner is the blue background"
    );
    assert_eq!(
        [px(3, 7)[0], px(3, 7)[1], px(3, 7)[2]],
        [0xFF, 0x00, 0x00],
        "an interior pixel is the red triangle"
    );

    // The committed hash pins the exact frame; a rendering change fails here and
    // forces a deliberate golden update.
    assert_eq!(
        compare_to_golden(&frame, GOLDEN_COMPOSITE_8X8),
        FrameComparison::Match,
        "frame hash matches the committed golden ({})",
        frame_hash(&frame),
    );
}
