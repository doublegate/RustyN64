//! T-31-005 — the first golden frame.
//!
//! Drives the whole Sprint-1 picture path end to end through the core's public
//! API — an RDP FILL command list rasterised into a framebuffer in RDRAM, then
//! the VI scanned out to RGBA8 — and pins the result against a **committed golden
//! hash**. Any regression in the command decoder, the FILL pipeline, or the VI
//! scan-out changes the frame bytes and so the hash, failing this test.
//!
//! The frame is produced by a synthetic command stream rather than a commercial
//! ROM (Phase 5 lands cartridge boot); the path exercised is identical from the
//! DP FIFO onward.

use rustyn64_core::Bus;
use rustyn64_core::cpu::Bus as CpuBus;
use rustyn64_test_harness::frame::{FrameComparison, compare_to_golden, frame_hash};

/// The golden hash of the rendered 4×2 32-bit frame (`0xAABBCCDD` fill). Pinned:
/// the value is the FNV-1a digest of the expected bytes, and the test also
/// asserts the pipeline reproduces those bytes exactly, so this constant guards
/// the whole FILL → scan-out path against silent drift.
const GOLDEN_FILL_4X2: u64 = 5_830_505_307_848_797_477;

/// RDRAM layout for the test: the framebuffer low, the command list well above.
const FB_ADDR: u32 = 0x1000;
const CMD_ADDR: u32 = 0x4000;

/// Write one 64-bit RDP command word (`hi` = bits 63:32, `lo` = 31:0) big-endian.
fn write_cmd(bus: &mut Bus, addr: u32, hi: u32, lo: u32) {
    let a = addr as usize;
    bus.rdram[a..a + 4].copy_from_slice(&hi.to_be_bytes());
    bus.rdram[a + 4..a + 8].copy_from_slice(&lo.to_be_bytes());
}

/// Fill a known 4×2 32-bit framebuffer via the RDP, scan it out through the VI,
/// and return the RGBA8 frame.
fn render_fill_frame() -> Vec<u8> {
    let mut bus = Bus::new();

    // A four-command FILL list: Set Color Image (32-bit, width 4, base FB_ADDR),
    // Set Fill Color (0xAABBCCDD), Set Scissor (0,0)-(4,2), Fill Rectangle
    // (0,0)-(4,2). Coordinates are u10.2, so a 4-pixel span is 16 and 2 is 8.
    write_cmd(&mut bus, CMD_ADDR, 0x3F18_0003, FB_ADDR); // Set Color Image
    write_cmd(&mut bus, CMD_ADDR + 8, 0x3700_0000, 0xAABB_CCDD); // Set Fill Color
    write_cmd(&mut bus, CMD_ADDR + 16, 0x2D00_0000, 0x0001_0008); // Set Scissor
    write_cmd(&mut bus, CMD_ADDR + 24, 0x3601_0008, 0x0000_0000); // Fill Rectangle

    // Point the DP FIFO at the list and drain it (one command per tick).
    bus.rdp.dpc_write(0, CMD_ADDR); // DPC_START
    bus.rdp.dpc_write(1, CMD_ADDR + 32); // DPC_END (4 words)
    for _ in 0..16 {
        bus.rdp_tick();
    }

    // All four commands must have retired within the tick budget. Asserting the
    // exact retired count makes a timing regression that leaves the FIFO mid-list
    // a hard, deterministic failure here rather than a silently truncated frame.
    assert_eq!(
        bus.rdp.commands_processed, 4,
        "the four-command FILL list drained within the tick budget"
    );

    // Configure the VI for a 32-bit, 4-wide, 2-line scan-out of FB_ADDR, through
    // the CPU register path (VI regs are crate-private to rustyn64-core).
    CpuBus::write_u32(&mut bus, 0x0440_0000, 3); // VI_CTRL: TYPE = 32-bit
    CpuBus::write_u32(&mut bus, 0x0440_0004, FB_ADDR); // VI_ORIGIN
    CpuBus::write_u32(&mut bus, 0x0440_0008, 4); // VI_WIDTH
    CpuBus::write_u32(&mut bus, 0x0440_0028, 4); // VI_V_VIDEO: 4 half-lines -> h=2

    let mut frame = vec![0u8; 4 * 2 * 4];
    let (w, h) = bus.scanout(&mut frame);
    assert_eq!((w, h), (4, 2), "scan-out geometry");
    frame
}

#[test]
fn a_fill_rectangle_renders_the_committed_golden_frame() {
    let frame = render_fill_frame();

    // The rasterised frame is byte-exact: every 32-bit pixel is the fill colour.
    let expected: Vec<u8> = [0xAA, 0xBB, 0xCC, 0xDD].repeat(8);
    assert_eq!(frame, expected, "FILL -> scan-out is byte-exact");

    // ... and its hash matches the committed golden constant directly. Comparing
    // against `GOLDEN_FILL_4X2` (not a value recomputed from `expected`) pins the
    // frame to the committed digest, so the assertion cannot pass by comparing a
    // fresh hash against itself.
    assert_eq!(
        compare_to_golden(&frame, GOLDEN_FILL_4X2),
        FrameComparison::Match,
        "frame hash matches the committed golden"
    );
    // And the committed constant is itself the true digest of the expected bytes,
    // so it cannot be stale or arbitrary: a frame-content change that also updates
    // `expected` must re-derive `GOLDEN_FILL_4X2` here or this fails.
    assert_eq!(
        frame_hash(&expected),
        GOLDEN_FILL_4X2,
        "committed golden hash is the digest of the expected frame"
    );
}
