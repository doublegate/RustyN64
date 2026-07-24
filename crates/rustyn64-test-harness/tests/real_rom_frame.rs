//! T-33-006 — the first *real ROM* renders a frame.
//!
//! Boots the committed, license-clean `render_fill.z64` (our own MIPS assembly —
//! see `tests/roms/homebrew/render_fill.s`) through the harness direct-load path,
//! runs the **real VR4300** until the ROM's CPU-side framebuffer fill completes,
//! then scans the framebuffer out through the **real VI** and pins the result.
//!
//! Unlike `golden_frame.rs` / `composite_frame.rs` (which poke a synthetic RDP
//! command stream), nothing here is synthesised: the CPU executes the ROM's own
//! instructions, which program the VI registers and write every pixel. It is the
//! CPU → RDRAM → VI scan-out path end to end. No RDP/RSP/texture is involved, so
//! this is orthogonal to the still-open texture work (ledger R-13).
#![allow(
    clippy::doc_markdown,
    reason = "narrative test docs name VR4300/RDRAM/VI in prose"
)]

use rustyn64_core::System;
use rustyn64_test_harness::rom;

/// The committed render ROM, relative to this crate's manifest dir.
const RENDER_FILL_Z64: &[u8] = include_bytes!("../../../tests/roms/homebrew/render_fill.z64");

/// Framebuffer geometry the ROM programs (32×24, 16-bit RGBA5551).
const W: u32 = 32;
const H: u32 = 24;

/// Physical RDRAM address of the framebuffer the ROM programs (`VI_ORIGIN`).
const FB_PHYS: usize = 0x0020_0000;

/// Generous master-tick budget: the fill is ~770 loop iterations (~6k retired
/// instructions), well under this.
const BUDGET_TICKS: u64 = 2_000_000;

/// Retired-instruction floor proving the fill loop actually ran: ~12 setup
/// instructions + 768 iterations × ~8 loop-body instructions ≈ 6156, so a run
/// that completes below this slipped past the loop rather than executing it.
const MIN_FILL_RETIRED: u64 = 6000;

/// Expand a 5-bit channel to 8 bits the way the VI DAC does (`v<<3 | v>>2`). The
/// input is masked to 5 bits, matching the hardware field width, so an unmasked
/// caller cannot corrupt the top bits.
const fn expand5(v: u8) -> u8 {
    let v = v & 0x1F;
    (v << 3) | (v >> 2)
}

/// Boot the ROM, run to the spin loop, and return the scanned-out RGBA8 frame.
fn render() -> Vec<u8> {
    let entry = rom::entry_point(RENDER_FILL_Z64).expect("readable header");
    assert_eq!(
        entry, 0xFFFF_FFFF_8000_1000,
        "render_fill.z64's entry point, sign-extended"
    );

    let mut sys = System::new(0);
    rom::load_direct(&mut sys, RENDER_FILL_Z64, entry).expect("loadable");

    // Run the real CPU until the fill has written the LAST pixel, or the budget
    // is spent. The completion signal is the framebuffer itself — pixel 767 at
    // `FB_PHYS + 767*2` becomes non-zero (`0xF801`) only after the loop's final
    // store. (The PC alone is unreliable here: the in-order pipeline speculatively
    // fetches past the loop's `bne` before it resolves, so the fetch PC briefly
    // visits the halt loop on the very first iteration.)
    let last_px = FB_PHYS + (W * H - 1) as usize * 2;
    let mut filled = false;
    let deadline = sys.master_ticks() + BUDGET_TICKS;
    while sys.master_ticks() < deadline {
        sys.step_to_next_edge();
        // The whole 16-bit pixel, not one byte: pixel 767 is `0xF801`, so any
        // format/value change still trips this as long as the pixel is non-zero.
        if sys.bus.rdram[last_px] != 0 || sys.bus.rdram[last_px + 1] != 0 {
            filled = true;
            break;
        }
    }
    assert!(
        filled,
        "ROM never finished the fill (retired = {}, last pixel still zero) — any \
         frame would be a vacuous pass",
        sys.cpu.retired
    );
    assert!(
        sys.cpu.retired > MIN_FILL_RETIRED,
        "only {} instructions retired; the fill loop cannot have run",
        sys.cpu.retired
    );

    let mut frame = vec![0u8; (W * H * 4) as usize];
    let (w, h) = sys.bus.scanout(&mut frame);
    assert_eq!((w, h), (W, H), "VI scanned out the programmed geometry");
    frame
}

/// **The ROM boots and its frame is the CPU-computed gradient.** Each column `x`
/// holds red = `expand5(x)`, green/blue 0, alpha 255 — because the ROM writes
/// pixel `i` as `((i & 0x1F) << 11) | 1` and `i & 0x1F == x` for a width-32 row.
/// Verifying the exact per-pixel value (not just "non-blank") proves the VR4300
/// executed the fill arithmetic, and that the VI scanned the right framebuffer.
#[test]
fn render_fill_rom_boots_and_scans_out_its_gradient() {
    let frame = render();
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            let px = &frame[i..i + 4];
            let red = expand5(u8::try_from(x).expect("x < 32 fits u8"));
            assert_eq!(
                px,
                [red, 0, 0, 255],
                "pixel ({x},{y}) is not the expected gradient value"
            );
        }
    }
    // Not a flat frame: column 0 is black-ish (red 0) and column 31 is full red.
    assert_ne!(
        &frame[0..4],
        &frame[(31 * 4)..(31 * 4 + 4)],
        "the gradient must vary across columns"
    );
}

/// **Determinism (ADR 0004):** the same ROM on the same seed renders a
/// bit-identical frame across two independent boots.
#[test]
fn render_fill_rom_is_deterministic() {
    assert_eq!(
        render(),
        render(),
        "two boots must produce identical frames"
    );
}
