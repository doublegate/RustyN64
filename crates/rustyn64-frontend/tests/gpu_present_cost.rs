//! What one GPU-presented frame costs, against the software scan-out it replaces.
//!
//! `#[ignore]`d: it needs a Vulkan device, and it is a measurement rather than an
//! assertion — there is no threshold here that would mean anything on an unknown
//! runner's GPU.
//!
//! ```text
//! cargo test -p rustyn64-frontend --release --features gpu-rdp \
//!     --test gpu_present_cost -- --ignored --nocapture
//! ```
//!
//! **Release, always.** A debug build measures a different program.
//!
//! # A measurement mistake worth not repeating
//!
//! An earlier version of this file also timed the RDRAM word swap *standalone*,
//! in its own loop over its own buffers, and reported **0.24 ms**. In situ the
//! same swap cost **~2 ms**. The standalone loop ran 60 times over the same two
//! buffers with nothing else touching the cache, so after the first iteration it
//! was measuring L3 bandwidth; the real path has the GPU upload evicting
//! everything between frames.
//!
//! It was not a small error, and it pointed the wrong way: it said the snapshot
//! was 6% of the cost when moving RDRAM was in fact ~90% of it. The isolated
//! number is deleted rather than kept with a caveat, because a number in a test
//! gets quoted.

#![cfg(all(feature = "gpu-rdp", not(target_arch = "wasm32")))]

use rustyn64_core::Bus;
use rustyn64_core::cpu::Bus as CpuBus;
use std::time::Instant;

/// Frames per timed leg. Enough to average out scheduler noise, short enough
/// that the whole run is a second.
const FRAMES: u32 = 60;

#[test]
#[ignore = "needs a Vulkan device; a measurement, not an assertion"]
fn gpu_present_versus_software_scanout() {
    const VI: u32 = 0x0440_0000;
    let mut bus = Bus::new();
    // A plausible 320x240 NTSC program, so the scan-out produces a real picture
    // rather than the VI's off-state early return.
    CpuBus::write_u32(&mut bus, VI, 0x0000_0302); // VI_CTRL: 16-bit, REPLICATE
    CpuBus::write_u32(&mut bus, VI + 0x04, 0x1000); // VI_ORIGIN
    CpuBus::write_u32(&mut bus, VI + 0x08, 320); // VI_WIDTH
    CpuBus::write_u32(&mut bus, VI + 0x18, 525); // VI_V_TOTAL
    CpuBus::write_u32(&mut bus, VI + 0x24, 0x006C_02EC); // VI_H_VIDEO
    CpuBus::write_u32(&mut bus, VI + 0x28, 0x0025_01FF); // VI_V_VIDEO
    CpuBus::write_u32(&mut bus, VI + 0x30, 0x0000_0200); // VI_X_SCALE
    CpuBus::write_u32(&mut bus, VI + 0x34, 0x0000_0400); // VI_Y_SCALE

    let mut out = vec![0u8; 640 * 480 * 4];

    // Warm both paths before timing either: the first GPU frame pays for shader
    // compilation, which is not a per-frame cost and would swamp the number.
    for _ in 0..5 {
        rustyn64_frontend::gpu_rdp::present(&mut bus, &mut out, 640, 480);
    }
    if rustyn64_frontend::gpu_rdp::frames_produced() == 0 {
        println!("SKIPPED: no usable Vulkan device — nothing was measured.");
        return;
    }
    for _ in 0..5 {
        let _ = bus.scanout_scaled(&mut out);
    }

    let t = Instant::now();
    for _ in 0..FRAMES {
        rustyn64_frontend::gpu_rdp::present(&mut bus, &mut out, 640, 480);
    }
    let gpu = t.elapsed().as_secs_f64() / f64::from(FRAMES);

    let t = Instant::now();
    for _ in 0..FRAMES {
        let _ = bus.scanout_scaled(&mut out);
    }
    let software = t.elapsed().as_secs_f64() / f64::from(FRAMES);

    println!(
        "gpu_present={:.3} ms   software_scanout={:.3} ms   ratio={:.2}x",
        gpu * 1e3,
        software * 1e3,
        gpu / software
    );
}
