//! `rustyn64-rdp` — RDP (Reality Display Processor), the RCP rasterizer.
//!
//! The RDP consumes a command stream (from the RSP or the CPU via the DP FIFO)
//! and rasterizes triangles/rectangles into a framebuffer in RDRAM, running the
//! color-combiner + blender + Z/coverage pipeline. The Video Interface (VI)
//! then scans that framebuffer out. The accuracy bar is **LLE** — a faithful
//! per-pixel pipeline (the ParaLLEl-RDP / angrylion reference), not a
//! triangle-list HLE.
//!
//! [`Rdp::tick`] decodes the DP FIFO — recognising every command `0x00`–`0x3F`
//! and consuming each one's full length (via [`command`]) so the stream stays
//! aligned — and dispatches the sync commands and the **FILL pipeline** (Set
//! Color Image, Set Fill Color, Set Scissor, Fill Rectangle), which writes solid
//! rectangles into the framebuffer. The rest of the rasterizer (edge-walked
//! triangles, the texture engine with TMEM, the combiner/blender, dithering,
//! coverage AA) is the remainder of this roadmap phase.
//!
//! Part of the one-directional chip-crate graph (see `docs/architecture.md`):
//! this crate depends on **exactly one** chip crate, `rustyn64-cart`, purely for
//! its [`RdramBus`] memory-bus trait — the RDP reads texture and framebuffer
//! reaches its tile storage through `rustynes-mappers`. `#![no_std]` + `alloc`.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::cast_possible_truncation, clippy::cast_lossless)]
// Skeleton `tick` is deliberately non-`const` (it will drain the DP FIFO).
#![allow(clippy::missing_const_for_fn)]

extern crate alloc;

pub mod command;

pub use rustyn64_cart::RdramBus;

/// The narrow bus the RDP sees.
///
/// RDRAM access (for the framebuffer + texture fetches) plus the
/// DP-interrupt-raise hook. Extends [`RdramBus`] (`RustyNES`'s `PpuBus` analog)
/// with the IRQ notify the rasterizer needs on `SYNC_FULL` / DP-done.
pub trait VideoBus: RdramBus {
    /// Raise the DP (RDP-done) interrupt on the MI. Default no-op for ad-hoc
    /// test buses; `rustyn64-core` sets the live `MI_INTR.dp` line.
    fn raise_dp_interrupt(&mut self) {}
}

/// One RGBA8888 output pixel (post-VI-filter); the framebuffer the frontend
/// presents is a slice of these.
pub type Pixel = u32;

/// `DPC_STATUS.XBUS` — the DP reads commands from DMEM rather than RDRAM.
pub const DP_STATUS_XBUS: u32 = 0x1;
/// `DPC_STATUS.FREEZE` — the DP is halted; registers can be read/written freely
/// without the command FIFO advancing.
pub const DP_STATUS_FREEZE: u32 = 0x2;
/// `DPC_STATUS.END_VALID` (the wiki's `END_PENDING`, read bit 9) — an end
/// address is latched behind an in-flight transfer.
///
/// Defined for the read-back layout but **not yet driven**: setting it requires
/// tracking a transfer *in progress*, which only exists once the rasterizer
/// runs (`tick` is a stub). It therefore always reads 0 today, which is exactly
/// what n64-systemtest's frozen `start-valid` case expects; the set/clear
/// transition lands with the FIFO drain.
pub const DP_STATUS_END_VALID: u32 = 0x200;
/// `DPC_STATUS.START_VALID` — a start address is latched and pending; further
/// writes to `DPC_START` are ignored until it is consumed by a `DPC_END` write.
pub const DP_STATUS_START_VALID: u32 = 0x400;

/// The `DPC_START`/`DPC_END` register mask: a 24-bit, 8-byte-aligned RDRAM
/// address (n64-systemtest's `RDP START & END REG (masking)`).
pub const DPC_ADDR_MASK: u32 = 0x00FF_FFF8;

/// `Sync Load` (0x26) pipeline stall, in GCLK cycles.
///
/// Fixed and unconditional — the RDP always stalls this long, whether or not a
/// load is in flight (N64brew *Reality Display Processor/Commands* §0x26). One
/// `tick` is one GCLK.
pub const SYNC_LOAD_GCLK: u32 = 25;
/// `Sync Pipe` (0x27) pipeline stall, in GCLK cycles.
///
/// Fixed and unconditional (N64brew *…/Commands* §0x27).
pub const SYNC_PIPE_GCLK: u32 = 50;
/// `Sync Tile` (0x28) pipeline stall, in GCLK cycles.
///
/// Fixed and unconditional (N64brew *…/Commands* §0x28).
pub const SYNC_TILE_GCLK: u32 = 33;

// RDP command opcodes handled by the dispatcher (bits 61:56 of a command word).
const OP_SYNC_LOAD: u8 = 0x26;
const OP_SYNC_PIPE: u8 = 0x27;
const OP_SYNC_TILE: u8 = 0x28;
const OP_SYNC_FULL: u8 = 0x29;
const OP_TEXTURE_RECTANGLE: u8 = 0x24;
const OP_TEXTURE_RECTANGLE_FLIP: u8 = 0x25;
const OP_SET_SCISSOR: u8 = 0x2D;
const OP_LOAD_TLUT: u8 = 0x30;
const OP_SET_TILE_SIZE: u8 = 0x32;
const OP_LOAD_BLOCK: u8 = 0x33;
const OP_LOAD_TILE: u8 = 0x34;
const OP_SET_TILE: u8 = 0x35;

/// TMEM byte offset of the high (palette / split-high) half.
const TMEM_HIGH: u32 = 0x800;

/// Sign-extend the low 16 bits of `v` to `i32` (for the `s10.5` `S`/`T` and the
/// `s5.10` `DsDx`/`DtDy`). `v as i16` already keeps only the low 16 bits.
const fn sext16(v: u32) -> i32 {
    v as i16 as i32
}

/// Sign-extend the low `bits` bits of `v` to `i32` (for the triangle edge fields:
/// `yh/ym/yl` are 14-bit `s11.2`, `xh/xm/xl` 28-bit `s11.16`, the slopes 30-bit).
#[allow(clippy::cast_possible_wrap)] // the reinterpret-as-signed IS the sign extension
const fn sext(v: u32, bits: u32) -> i32 {
    debug_assert!(bits >= 1 && bits <= 32, "sext width out of range");
    let shift = 32 - bits;
    ((v << shift) as i32) >> shift
}

/// Sign-extend the low 9 bits of `x` (the combiner's `bitfieldExtract(x, 0, 9)`).
///
/// Only bits 0–8 are used: the `<< 23` shifts bit 8 to the sign position and the
/// arithmetic `>> 23` fills from it, so any higher bits of `x` are discarded.
const fn sext9(x: i32) -> i32 {
    (x << 23) >> 23
}

/// Position of the most significant set bit of `x`, or `-1` for `x <= 0` — GLSL
/// `findMSB` semantics for the non-negative inputs this codec uses (`z_encode.h`).
/// All callers here pass non-negative magnitudes; the `-1` result flows through
/// `max(findMSB(dz), 0)` in [`dz_compress`], and [`combine_dz`] guards on it so a
/// non-positive input can never reach the negative shift (GLSL's signed `findMSB`
/// differs for genuinely-negative inputs, which do not occur in this domain).
#[allow(
    clippy::cast_possible_wrap,
    reason = "leading_zeros() is 0..=31, always in range for i32"
)]
const fn find_msb(x: i32) -> i32 {
    if x <= 0 {
        -1
    } else {
        31 - (x.leading_zeros() as i32)
    }
}

/// Decompress a 14-bit stored depth to an 18-bit UNORM (`0..=0x3_FFFF`). The N64
/// Z buffer uses an inverted floating-point encoding with more precision near 1
/// (ParaLLEl-RDP `z_encode.h`): `exponent` in bits 13:11, `mantissa` in 10:0.
fn z_decompress(z: u16) -> i32 {
    let z = i32::from(z);
    let exponent = z >> 11;
    let mantissa = z & 0x7ff;
    let shift = (6 - exponent).max(0);
    let base = 0x4_0000 - (0x4_0000 >> exponent);
    (mantissa << shift) + base
}

/// Compress an 18-bit UNORM depth back to the 14-bit stored form (`z_encode.h`).
/// Inverse of [`z_decompress`]; `exponent` is derived from the leading zeros of
/// the inverted depth so precision concentrates near the far plane.
///
/// Verified by the [`z_decompress`] round-trip test and exercised by
/// [`Rdp::zbuffer_write`], which compresses the computed 18-bit depth before it is
/// packed into the Z buffer.
#[allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    reason = "the packed result is 0..=0x3FFF, always a valid non-negative u16"
)]
fn z_compress(z: i32) -> u16 {
    // Clamp to the 18-bit UNORM domain so the `0x3FFFF - z` subtraction and the
    // `z >> shift` mantissa are well-defined for any computed depth (parallel-rdp
    // clamps `z` this way before writeback, `clamping.h`).
    let z = z.clamp(0, 0x3_FFFF);
    let inv_z = (0x3_FFFF - z).max(1);
    let exponent = (17 - find_msb(inv_z)).clamp(0, 7);
    let shift = (6 - exponent).max(0);
    let mantissa = (z >> shift) & 0x7ff;
    ((exponent << 11) + mantissa) as u16
}

/// Decompress a 4-bit stored `dz` to its linear delta `1 << dz` (`z_encode.h`).
const fn dz_decompress(dz: i32) -> i32 {
    1 << dz
}

/// Compress a linear `dz` delta to its 4-bit `log2` form (`z_encode.h`). The RDP
/// uses this cheap integer `log2`, correct only for powers of two (hence the
/// "dz should be a power of 2" hazard); `dz == 0` yields 0 via `find_msb`'s `-1`.
const fn dz_compress(dz: i32) -> i32 {
    let m = find_msb(dz);
    if m < 0 { 0 } else { m }
}

/// The largest power of two `<= dz` (ParaLLEl-RDP `depth_test.h` `combine_dz`).
/// Guards on `find_msb(dz) >= 0` rather than `dz != 0` so a non-positive `dz`
/// returns 0 instead of shifting `1 << -1` (the codec only feeds it non-negative
/// magnitudes; the guard makes it panic-free for any `i32` regardless).
const fn combine_dz(dz: i32) -> i32 {
    let m = find_msb(dz);
    if m >= 0 { 1 << m } else { 0 }
}

/// Interpolate the per-pixel depth (18-bit UNORM, `0..=0x3_FFFF`) for pixel
/// `(x, y)` from the triangle's z-coefficients — a faithful port of ParaLLEl-RDP's
/// `interpolate_z` (`interpolation.h`) for the full-coverage, `do_offset == false`
/// case (sub-pixel coverage snapping is the deferred R-9 residual).
///
/// `z_base`/`dzdx`/`dzde` are the `s15.16` z-coefficient and its per-x / per-major-
/// edge deltas; `major_x` is the `s15.16` major-edge x at this scanline (its
/// integer part is the interpolation origin, its `xfrac` aligns to the edge);
/// `y_base` is the top scanline. 32-bit wrapping matches the reference's `int`
/// arithmetic; the final `clamp_z` bounds the result to the depth range.
#[allow(
    clippy::cast_possible_truncation,
    reason = "major_x fields and the clamped result are within range for the casts"
)]
#[allow(
    clippy::similar_names,
    reason = "dzdx / dzde are the N64 RDP's own z-coefficient names"
)]
fn interpolate_z(
    z_base: i32,
    dzdx: i32,
    dzde: i32,
    major_x: i64,
    y: i32,
    y_base: i32,
    x: i32,
) -> i32 {
    let base_x = (major_x >> 16) as i32;
    let xfrac = ((major_x >> 8) & 0xff) as i32;
    let mut z = z_base.wrapping_add(dzde.wrapping_mul(y.wrapping_sub(y_base)));
    z = ((z & !0x1ff).wrapping_sub(xfrac.wrapping_mul((dzdx >> 8) & !1))) & !0x3ff;
    z = z.wrapping_add(dzdx.wrapping_mul(x.wrapping_sub(base_x)));
    // Snap (full coverage: the first-subpixel xoff/yoff terms are 0).
    let snapped = ((z >> 10) << 2) >> 5;
    snapped.clamp(0, 0x3_FFFF)
}

/// Interpolate the per-pixel shade colour for pixel `(x, y)` from a triangle's
/// shade coefficients — a port of ParaLLEl-RDP's `interpolate_rgba`
/// (`interpolation.h`) for the full-coverage case. `base`/`dx`/`de` are the `s15.16`
/// RGBA base and its per-x / per-major-edge deltas; `base_x` is the major-edge x
/// integer at this scanline. Each channel: walk the scanline base by `de`, add the
/// per-x `dx` term (masked `& ~0x1f`), snap `>> 14 << 2 >> 4`, and clamp to a byte.
#[allow(
    clippy::cast_possible_truncation,
    reason = "base_x derives from the s15.16 major-edge x, in range for the cast"
)]
fn interpolate_shade(
    base: &[i32; 4],
    dx: &[i32; 4],
    de: &[i32; 4],
    major_x: i64,
    y: i32,
    y_base: i32,
    x: i32,
) -> [u8; 4] {
    let base_x = (major_x >> 16) as i32;
    let mut out = [0u8; 4];
    for (c, o) in out.iter_mut().enumerate() {
        let scan = base[c].wrapping_add(de[c].wrapping_mul(y.wrapping_sub(y_base)));
        let v = scan.wrapping_add((dx[c] & !0x1f).wrapping_mul(x.wrapping_sub(base_x)));
        // The snap is done in i16, matching the oracle's `i16x4(rgba >> 14)` cast
        // (`interpolation.h`) — the truncation to 16 bits is part of the hardware
        // result, and it keeps the `<< 2` off the i32 sign bit.
        let snapped = (((v >> 14) as i16).wrapping_shl(2) >> 4).into();
        *o = clamp_9bit(snapped);
    }
    out
}

/// Interpolate the per-pixel texture coordinate `(s, t)` for pixel `(x, y)` from a
/// triangle's texture coefficients — the **non-perspective** path
/// (`no_perspective_divide(stw >> 16)`, `interpolation.h`/`perspective.h`): walk
/// the `s16.16` `S`/`T` by `de` down the scanline and `dx` across, then take the
/// integer part (`>> 16`) as the texel coordinate. The `W` perspective divide is
/// the deferred perspective slice; the returned coords wrap into `fetch_texel`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "base_x is in range for the cast; the texel coord wraps into fetch_texel"
)]
fn interpolate_st(
    tex: &TexSetup,
    persp: bool,
    major_x: i64,
    y: i32,
    y_base: i32,
    x: i32,
) -> [u32; 2] {
    let base_x = (major_x >> 16) as i32;
    let mut stw = [0i32; 3];
    for (c, o) in stw.iter_mut().enumerate() {
        let scan = tex.base[c].wrapping_add(tex.de[c].wrapping_mul(y.wrapping_sub(y_base)));
        let v = scan.wrapping_add((tex.dx[c] & !0x1f).wrapping_mul(x.wrapping_sub(base_x)));
        *o = v >> 16; // the integer coordinate fed to the (non-)perspective divide
    }
    // `no_perspective_divide` is just `(s, t)`; the perspective path divides by W.
    let (s, t) = if persp {
        perspective_divide(stw[0], stw[1], stw[2])
    } else {
        (stw[0], stw[1])
    };
    [s as u32, t as u32]
}

/// The RDP's perspective-divide reciprocal LUT (ParaLLEl-RDP `perspective.h`,
/// transcribed in its `(base, slope · 4)` source form). Indexed by the top 6 bits
/// of the normalised `W`; `(base, slope)` give `rcp = ((slope · wnorm) >> 10) + base`.
#[rustfmt::skip]
const PERSPECTIVE_TABLE: [(i16, i16); 64] = [
    (0x4000, -252 * 4), (0x3f04, -244 * 4), (0x3e10, -238 * 4), (0x3d22, -230 * 4),
    (0x3c3c, -223 * 4), (0x3b5d, -218 * 4), (0x3a83, -210 * 4), (0x39b1, -205 * 4),
    (0x38e4, -200 * 4), (0x381c, -194 * 4), (0x375a, -189 * 4), (0x369d, -184 * 4),
    (0x35e5, -179 * 4), (0x3532, -175 * 4), (0x3483, -170 * 4), (0x33d9, -166 * 4),
    (0x3333, -162 * 4), (0x3291, -157 * 4), (0x31f4, -155 * 4), (0x3159, -150 * 4),
    (0x30c3, -147 * 4), (0x3030, -143 * 4), (0x2fa1, -140 * 4), (0x2f15, -137 * 4),
    (0x2e8c, -134 * 4), (0x2e06, -131 * 4), (0x2d83, -128 * 4), (0x2d03, -125 * 4),
    (0x2c86, -123 * 4), (0x2c0b, -120 * 4), (0x2b93, -117 * 4), (0x2b1e, -115 * 4),
    (0x2aab, -113 * 4), (0x2a3a, -110 * 4), (0x29cc, -108 * 4), (0x2960, -106 * 4),
    (0x28f6, -104 * 4), (0x288e, -102 * 4), (0x2828, -100 * 4), (0x27c4,  -98 * 4),
    (0x2762,  -96 * 4), (0x2702,  -94 * 4), (0x26a4,  -92 * 4), (0x2648,  -91 * 4),
    (0x25ed,  -89 * 4), (0x2594,  -87 * 4), (0x253d,  -86 * 4), (0x24e7,  -85 * 4),
    (0x2492,  -83 * 4), (0x243f,  -81 * 4), (0x23ee,  -80 * 4), (0x239e,  -79 * 4),
    (0x234f,  -77 * 4), (0x2302,  -76 * 4), (0x22b6,  -74 * 4), (0x226c,  -74 * 4),
    (0x2222,  -72 * 4), (0x21da,  -71 * 4), (0x2193,  -70 * 4), (0x214d,  -69 * 4),
    (0x2108,  -67 * 4), (0x20c5,  -67 * 4), (0x2082,  -65 * 4), (0x2041,  -65 * 4),
];

/// The reciprocal and shift for a normalised `W` (ParaLLEl-RDP `perspective_get_lut`).
#[allow(
    clippy::cast_sign_loss,
    reason = "normout & 0x3fff >> 8 is 0..=63, a valid table index"
)]
fn perspective_get_lut(w: i32) -> (i32, i32) {
    let shift = (14 - find_msb(w)).min(14);
    let normout = (w << shift) & 0x3fff;
    let wnorm = normout & 0xff;
    let (base, slope) = PERSPECTIVE_TABLE[(normout >> 8) as usize];
    let rcp = ((i32::from(slope) * wnorm) >> 10) + i32::from(base);
    (rcp, shift)
}

/// Perspective-divide a texture coordinate `(s, t)` by `w` — a faithful port of
/// ParaLLEl-RDP's `perspective_divide` (`perspective.h`): the LUT reciprocal, the
/// shift, the `temp_mask` out-of-bounds saturation, the `w <= 0` carry, and the
/// final 17-bit clamp. Returns the divided `(s, t)`.
#[allow(
    clippy::similar_names,
    reason = "s / t / w are the RDP's own texture-coordinate names"
)]
fn perspective_divide(s: i32, t: i32, w: i32) -> (i32, i32) {
    let w_carry = w <= 0;
    let w = w & 0x7fff;
    let (rcp, shift) = perspective_get_lut(w);
    let prod0 = [s.wrapping_mul(rcp), t.wrapping_mul(rcp)];
    let temp_mask = ((1 << 30) - 1) & -((1 << 29) >> shift);
    let out_of_bounds = [prod0[0] & temp_mask, prod0[1] & temp_mask];
    let (mut temp, prod) = if shift == 14 {
        ([prod0[0] << 1, prod0[1] << 1], prod0)
    } else {
        let p = [prod0[0] >> (13 - shift), prod0[1] >> (13 - shift)];
        (p, p)
    };
    if out_of_bounds != [0, 0] {
        for c in 0..2 {
            if out_of_bounds[c] != temp_mask && out_of_bounds[c] != 0 {
                temp[c] = if prod[c] & (1 << 29) == 0 {
                    0x7fff
                } else {
                    -0x8000
                };
            }
        }
    }
    if w_carry {
        temp = [0x7fff, 0x7fff];
    }
    (
        temp[0].clamp(-0x10000, 0xffff),
        temp[1].clamp(-0x10000, 0xffff),
    )
}

/// The RDP's asymmetric 9-bit expand for the combiner's A/B/D inputs: subtract
/// the 0x80 bias, sign-extend to 9 bits, add the bias back (ParaLLEl-RDP
/// `special_expand`, `combiner.h`). The multiplier C uses a plain [`sext9`].
const fn special_expand(v: i32) -> i32 {
    sext9(v - 0x80) + 0x80
}

/// Evaluate one combiner channel `(A − B) * C + D` with the RDP's fixed-point
/// rules: A/B/D through [`special_expand`], C a plain 9-bit value, a `+0x80`
/// rounding bias applied before the `>> 8`, and D added afterwards, unscaled
/// (ParaLLEl-RDP `combiner_equation`). No clamp here — that is per cycle.
const fn combine_channel(a: i32, b: i32, c: i32, d: i32) -> i32 {
    let color = (special_expand(a) - special_expand(b)) * sext9(c) + 0x80;
    (color >> 8) + special_expand(d)
}

/// Clamp a combiner result to `[0, 255]` with the RDP's 9-bit fold — the
/// `-0x80 / sext9 / +0x80` before the clamp is what makes 256–383 saturate and
/// 384–511 wrap toward 0 (ParaLLEl-RDP `clamp_9bit_notrunc`, `clamping.h`).
#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
fn clamp_9bit(color: i32) -> u8 {
    special_expand(color).clamp(0, 0xFF) as u8
}

/// The combiner's "1" input — `0x100` in the internal `.8` representation, not
/// `0xFF` (N64brew *…/Commands* §0x3C).
const COMBINER_ONE: i16 = 0x100;

/// The RGB `A` (muladd) input for channel `ch`. Table: 0 Combined, 1 Texel0,
/// 2 Texel1, 3 Prim, 4 Shade, 5 Env, 6 One, 7 Noise (R-10 → 0), 8+ Zero.
fn rgb_input_a(sel: u8, inp: &CombinerInputs, ch: usize) -> i16 {
    match sel {
        0 => i16::from(inp.combined[ch]),
        1 => i16::from(inp.texel0[ch]),
        2 => i16::from(inp.texel1[ch]),
        3 => i16::from(inp.prim[ch]),
        4 => i16::from(inp.shade[ch]),
        5 => i16::from(inp.env[ch]),
        6 => COMBINER_ONE,
        _ => 0, // 7 Noise + 8+ Zero — R-10
    }
}

/// The RGB `B` (mulsub) input. Same as `A` except 6/7 are KeyCenter/ConvertK4
/// (both R-10 → 0), so 6+ reads as zero.
fn rgb_input_b(sel: u8, inp: &CombinerInputs, ch: usize) -> i16 {
    match sel {
        0 => i16::from(inp.combined[ch]),
        1 => i16::from(inp.texel0[ch]),
        2 => i16::from(inp.texel1[ch]),
        3 => i16::from(inp.prim[ch]),
        4 => i16::from(inp.shade[ch]),
        5 => i16::from(inp.env[ch]),
        _ => 0, // 6 KeyCenter, 7 ConvertK4 — R-10; 8+ Zero
    }
}

/// The RGB `C` (mul) input (5-bit table). Alpha-channel inputs (7–12) tap the
/// alpha of the corresponding signal; the key/LOD/convert inputs are R-10 → 0.
fn rgb_input_c(sel: u8, inp: &CombinerInputs, ch: usize) -> i16 {
    match sel {
        0 => i16::from(inp.combined[ch]),
        1 => i16::from(inp.texel0[ch]),
        2 => i16::from(inp.texel1[ch]),
        3 => i16::from(inp.prim[ch]),
        4 => i16::from(inp.shade[ch]),
        5 => i16::from(inp.env[ch]),
        7 => i16::from(inp.combined[3]),
        8 => i16::from(inp.texel0[3]),
        9 => i16::from(inp.texel1[3]),
        10 => i16::from(inp.prim[3]),
        11 => i16::from(inp.shade[3]),
        12 => i16::from(inp.env[3]),
        _ => 0, // 6 KeyScale, 13 LODFrac, 14 PrimLODFrac, 15 ConvertK5 — R-10; 16+ Zero
    }
}

/// The RGB `D` (add) input (3-bit table): 0 Combined … 5 Env, 6 One, 7 Zero.
fn rgb_input_d(sel: u8, inp: &CombinerInputs, ch: usize) -> i16 {
    match sel {
        0 => i16::from(inp.combined[ch]),
        1 => i16::from(inp.texel0[ch]),
        2 => i16::from(inp.texel1[ch]),
        3 => i16::from(inp.prim[ch]),
        4 => i16::from(inp.shade[ch]),
        5 => i16::from(inp.env[ch]),
        6 => COMBINER_ONE,
        _ => 0, // 7 Zero
    }
}

/// The alpha `A`/`B`/`D` input (3-bit table): 0 combined-alpha … 5 env-alpha,
/// 6 one, 7 zero.
fn alpha_input_abd(sel: u8, inp: &CombinerInputs) -> i16 {
    match sel {
        0 => i16::from(inp.combined[3]),
        1 => i16::from(inp.texel0[3]),
        2 => i16::from(inp.texel1[3]),
        3 => i16::from(inp.prim[3]),
        4 => i16::from(inp.shade[3]),
        5 => i16::from(inp.env[3]),
        6 => COMBINER_ONE,
        _ => 0, // 7 Zero
    }
}

/// The alpha `C` (mul) input (3-bit table): 0 lod-frac (R-10 → 0), 1 texel0-alpha,
/// … 5 env-alpha, 6 prim-lod-frac (R-10 → 0), 7 zero.
fn alpha_input_c(sel: u8, inp: &CombinerInputs) -> i16 {
    match sel {
        1 => i16::from(inp.texel0[3]),
        2 => i16::from(inp.texel1[3]),
        3 => i16::from(inp.prim[3]),
        4 => i16::from(inp.shade[3]),
        5 => i16::from(inp.env[3]),
        _ => 0, // 0 LODFrac, 6 PrimLODFrac — R-10; 7 Zero
    }
}

/// The blender's `P`/`M` colour select: which RGB triple feeds one blend term
/// (N64brew *…/Blender*). 0 = pixel (combiner output), 1 = memory (framebuffer),
/// 2 = blend-colour register, 3 = fog-colour register.
fn blend_rgb_input(sel: u8, inp: &BlendInputs) -> [u8; 3] {
    let [r, g, b, _] = match sel & 0x3 {
        1 => inp.memory,
        2 => inp.blend_color,
        3 => inp.fog,
        _ => inp.pixel,
    };
    [r, g, b]
}

/// The blender's `A` (1b) alpha weight: 0 = pixel alpha, 1 = fog alpha,
/// 2 = shade alpha, 3 = zero (N64brew *…/Blender*).
fn blend_a_input(sel: u8, inp: &BlendInputs) -> u8 {
    match sel & 0x3 {
        1 => inp.fog[3],
        2 => inp.shade_alpha,
        3 => 0,
        _ => inp.pixel[3],
    }
}

/// The blender's `B` (2b) alpha weight: 0 = `1 − A`, 1 = memory alpha (framebuffer
/// coverage), 2 = one (0xFF), 3 = zero (N64brew *…/Blender*).
///
/// The `1 − A` case is the one's complement of the **already-selected `A` weight**,
/// not of pixel alpha specifically — the ParaLLEl-RDP constant is named
/// `INV_PIXEL_ALPHA` but computes `~a0` (`blender.h:106`), so `A` selecting fog or
/// shade alpha makes `B` their complement too. `a0_full` is that resolved `A` weight
/// **before** the `>> 3` (the complement is taken on the full 8-bit value, then both
/// weights are shifted — `blender.h:106` vs `:112`).
fn blend_b_input(sel: u8, inp: &BlendInputs, a0_full: u8) -> u8 {
    match sel & 0x3 {
        1 => inp.memory[3],
        2 => 0xFF,
        3 => 0,
        _ => !a0_full,
    }
}

/// Wrap one raw texture coordinate (`s10.5` fixed point) into a tile-relative
/// integer texel, applying the tile's shift, tile-origin subtraction, mirror, and
/// mask — the COPY-mode order (no clamp). Matches the ParaLLEl-RDP reference
/// (`texture.h`): clamp to `i16`, then shift (codes 1–10 shift right, 11–15 shift
/// left by `16−code`), subtract `SL<<3`, take the integer part (`>>5`), then
/// mirror-on-alternate-spans and mask to `mask` bits (`mask == 0` = no wrap).
fn wrap_coord(coord: i32, shift: u8, mask: u8, mirror: bool, lo: u16) -> i32 {
    let shift = shift.min(15); // the hardware shift field is 4 bits (0..15)
    let c = coord.clamp(-0x8000, 0x7FFF);
    let shifted = if shift <= 10 {
        c >> shift
    } else {
        // Left shift by (16 − shift), truncated to 16 bits (sign-preserving).
        i32::from((c as i16).wrapping_shl(u32::from(16 - shift)))
    };
    let mut s = (shifted - (i32::from(lo) << 3)) >> 5;
    let mask = mask.min(10); // hardware caps the mask width at 10
    if mask != 0 {
        let m = 1i32 << mask;
        if mirror && s & m != 0 {
            s ^= m - 1; // reflect on odd mask-sized spans
        }
        s &= m - 1;
    }
    s
}

/// Widen a 5-bit channel to 8 bits by bit-replication (`v<<3 | v>>2`).
const fn widen5(v: u32) -> u8 {
    ((v << 3) | (v >> 2)) as u8
}

/// Widen a 4-bit channel to 8 bits by bit-replication (`v<<4 | v`).
const fn widen4(v: u32) -> u8 {
    ((v << 4) | v) as u8
}

/// Widen a 3-bit channel to 8 bits by bit-replication.
const fn widen3(v: u32) -> u8 {
    ((v << 5) | (v << 2) | (v >> 1)) as u8
}

/// Decode a 16-bit RGBA5551 word to `[R, G, B, A]` (5→8 replication; 1-bit alpha).
const fn decode_rgba16(w: u32) -> [u8; 4] {
    [
        widen5((w >> 11) & 0x1F),
        widen5((w >> 6) & 0x1F),
        widen5((w >> 1) & 0x1F),
        if w & 1 != 0 { 0xFF } else { 0 },
    ]
}

/// Maximum texels a single `Load Block` may transfer (N64brew *…/Commands*
/// §Load Block); a load over this writes nothing into TMEM.
pub const LOAD_BLOCK_MAX_TEXELS: u32 = 2048;

/// Bytes per texel for a texel-size code, or `None` for 4-bit (`size` 0), which
/// is sub-byte and needs nibble addressing: 8bpp=1, 16bpp=2, 32bpp=4.
const fn bytes_per_texel(size: u8) -> Option<u32> {
    match size {
        1 => Some(1),
        2 => Some(2),
        3 => Some(4),
        _ => None,
    }
}
const OP_SET_PRIM_DEPTH: u8 = 0x2E;
const OP_SET_OTHER_MODES: u8 = 0x2F;
const OP_FILL_RECTANGLE: u8 = 0x36;
const OP_SET_FILL_COLOR: u8 = 0x37;
const OP_SET_FOG_COLOR: u8 = 0x38;
const OP_SET_BLEND_COLOR: u8 = 0x39;
const OP_SET_PRIM_COLOR: u8 = 0x3A;
const OP_SET_ENV_COLOR: u8 = 0x3B;
const OP_SET_COMBINE_MODE: u8 = 0x3C;
const OP_SET_TEXTURE_IMAGE: u8 = 0x3D;
const OP_SET_DEPTH_IMAGE: u8 = 0x3E;
const OP_SET_COLOR_IMAGE: u8 = 0x3F;

/// One cycle of the blender: the `P`/`M` colour selects and the `A`/`B` alpha
/// selects for `P * A + M * (B + 1)` (`Set Other Modes`, 0x2F). Each is 2-bit.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BlendCycle {
    /// `P` (1a) colour select: 0 pixel, 1 memory, 2 blend, 3 fog.
    pub p: u8,
    /// `A` (1b) alpha select: 0 pixel-alpha, 1 fog-alpha, 2 shade-alpha, 3 zero.
    pub a: u8,
    /// `M` (2a) colour select (same table as `P`).
    pub m: u8,
    /// `B` (2b) alpha select: 0 `1−A`, 1 memory-alpha, 2 one, 3 zero.
    pub b: u8,
}

/// The `Set Other Modes` (0x2F) render-mode state the blender and cycle control
/// need.
///
/// Coverage/alpha-compare/dither and the full Z model are decoded here but the
/// blend equation only uses the cycle type, the two blend cycles, and
/// `force_blend` so far (**open residual R-11**).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "these are independent hardware mode bits from Set Other Modes, not a state machine"
)]
pub struct OtherModes {
    /// Cycle type: 0 = 1-cycle, 1 = 2-cycle, 2 = copy, 3 = fill.
    pub cycle_type: u8,
    /// The two blender cycles (cycle 0 chains into cycle 1 in 2-cycle mode).
    pub blend: [BlendCycle; 2],
    /// Force the no-divide blend form even on the final cycle.
    pub force_blend: bool,
    /// Read the framebuffer (memory) colour into the blend.
    pub image_read_en: bool,
    /// Coverage write-back mode: 0 clamp, 1 wrap, 2 full, 3 save.
    pub cvg_dest: u8,
    /// Z test / update enables and the Z mode (T-33-004 consumes these).
    pub z_compare_en: bool,
    /// Z-buffer update enable.
    pub z_update_en: bool,
    /// Z mode: 0 opaque, 1 interpenetrating, 2 transparent, 3 decal.
    pub z_mode: u8,
    /// Alpha-compare enable (gates the pixel write; R-11).
    pub alpha_compare_en: bool,
    /// Perspective-correct texturing (bit 51): divide the interpolated `S`/`T` by `W`.
    pub persp_tex_en: bool,
    /// Anti-aliasing enable (bit 3): sub-pixel edge coverage governs which edge
    /// pixels draw and enables the edge blend (N64brew *…/Commands* §0x2F bit 3).
    pub aa_enable: bool,
}

/// The resolved per-pixel blender input colours (each RGBA8888).
#[derive(Debug, Default, Clone, Copy)]
pub struct BlendInputs {
    /// The combiner's output ("pixel colour").
    pub pixel: [u8; 4],
    /// The framebuffer's current colour (memory), with coverage in its alpha.
    pub memory: [u8; 4],
    /// The blend-colour register (`Set Blend Color`, 0x39).
    pub blend_color: [u8; 4],
    /// The fog-colour register (`Set Fog Color`, 0x38).
    pub fog: [u8; 4],
    /// The interpolated shade alpha.
    pub shade_alpha: u8,
}

/// The Z-buffer read and render-mode flags [`Rdp::depth_test`] needs beyond the
/// incoming pixel's own `z`/`dz` (grouped so the signature stays legible).
#[derive(Debug, Default, Clone, Copy)]
pub struct DepthInputs {
    /// The 14-bit compressed depth already stored for this pixel.
    pub current_depth: u16,
    /// The 4-bit `dz` already stored for this pixel.
    pub current_dz: u8,
    /// The coverage already accumulated in the framebuffer pixel.
    pub current_coverage: i32,
    /// `Set Other Modes` `z_compare_en`: run the depth comparison at all.
    pub z_compare: bool,
    /// `Set Other Modes` `z_mode`: 0 opaque, 1 interpenetrating, 2 transparent, 3 decal.
    pub z_mode: u8,
    /// `Set Other Modes` `force_blend`.
    pub force_blend: bool,
    /// Anti-aliasing enable (`Set Other Modes` `antialias_en`).
    pub aa_enable: bool,
}

/// The outcome of a per-pixel depth test: whether the pixel is written, and the
/// blend/coverage state the blender consumes.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DepthResult {
    /// The pixel passes the depth test and is written.
    pub depth_pass: bool,
    /// Enable the blender for this pixel (vs. an opaque overwrite).
    pub blend_en: bool,
    /// Coverage overflowed (`>= 8`): the surface differs, wrap the coverage.
    pub coverage_wrap: bool,
    /// The two blender coverage shifts (`[dz→mem, mem→dz]`, each clamped `0..=4`).
    pub blend_shift: [u8; 2],
    /// The (possibly interpenetrating-reduced) coverage count carried forward.
    pub coverage_count: i32,
}

/// The decoded per-triangle depth setup for the per-pixel path: the `s15.16`
/// z-coefficient and its per-x / per-major-edge deltas, plus the primitive `dz`
/// (linear) and `dz_compressed` (4-bit stored form) for the test and writeback.
#[derive(Debug, Default, Clone, Copy)]
#[allow(
    clippy::similar_names,
    reason = "dzdx / dzde / dz are the N64 RDP's own z-coefficient names"
)]
struct ZTriSetup {
    z_base: i32,
    dzdx: i32,
    dzde: i32,
    dz: i32,
    dz_compressed: i32,
}

/// The decoded per-triangle shade setup for the per-pixel path: the `s15.16` RGBA
/// base colour (at the top vertex) and its per-x (`dx`) and per-major-edge (`de`)
/// deltas. The per-scanline `dy` term (sub-pixel snap) is part 2c.
#[derive(Debug, Default, Clone, Copy)]
struct ShadeSetup {
    base: [i32; 4],
    dx: [i32; 4],
    de: [i32; 4],
}

/// The decoded per-triangle texture setup: `S`/`T`/`W` (`W` for the perspective
/// divide). `base`/`dx`/`de` are the `s16.16` base at the top vertex and its per-x
/// and per-major-edge deltas.
#[derive(Debug, Default, Clone, Copy)]
struct TexSetup {
    base: [i32; 3],
    dx: [i32; 3],
    de: [i32; 3],
}

/// Unpack an RGBA8888 register word into `[r, g, b, a]` bytes.
const fn unpack_rgba(w: u32) -> [u8; 4] {
    w.to_be_bytes()
}

/// Pack an RGBA8888 colour into a 16-bit RGBA5551 framebuffer pixel
/// (`R[15:11] G[10:6] B[5:1] A[0]`).
const fn pack_rgba5551(rgba: [u8; 4]) -> u16 {
    ((rgba[0] as u16 >> 3) << 11)
        | ((rgba[1] as u16 >> 3) << 6)
        | ((rgba[2] as u16 >> 3) << 1)
        | (rgba[3] as u16 >> 7)
}

/// Unpack a 16-bit RGBA5551 framebuffer pixel to RGBA8888, widening each 5-bit
/// channel by high-bit replication (`v << 3 | v >> 2`) and the 1-bit alpha to
/// `0x00`/`0xFF`.
const fn unpack_rgba5551(p: u16) -> [u8; 4] {
    let r = (p >> 11) & 0x1F;
    let g = (p >> 6) & 0x1F;
    let b = (p >> 1) & 0x1F;
    [
        ((r << 3) | (r >> 2)) as u8,
        ((g << 3) | (g >> 2)) as u8,
        ((b << 3) | (b >> 2)) as u8,
        if p & 1 != 0 { 0xFF } else { 0 },
    ]
}

/// The number of Y-subpixels the RDP samples per scanline for coverage
/// (parallel-rdp `coverage.h` `SUBPIXELS`).
pub const COVERAGE_SUBPIXELS: usize = 4;

/// `log2` of the 4 sub-scanlines per pixel row: the triangle edge slopes are dx
/// per **pixel** row but the edge-walk steps per **quarter**-pixel, so each slope
/// is pre-shifted `>> SUB_SCANLINE_SHIFT` at decode (ledger R-14).
const SUB_SCANLINE_SHIFT: i32 = 2;

/// Left-edge poison for a sub-scanline outside the triangle: larger than any real
/// `s.3` edge, so every X-sample tests as clipped and `min4` ignores it.
const SPAN_X_POISON_LEFT: i32 = i32::MAX;
/// Right-edge poison (mirror of [`SPAN_X_POISON_LEFT`]).
const SPAN_X_POISON_RIGHT: i32 = i32::MIN;

/// The minimum of the four per-Y-subpixel left edges (parallel-rdp `span_setup.comp` `min4`).
fn min4(v: &[i32; COVERAGE_SUBPIXELS]) -> i32 {
    v.iter().copied().min().unwrap_or(0)
}
/// The maximum of the four per-Y-subpixel right edges (`span_setup.comp` `max4`).
fn max4(v: &[i32; COVERAGE_SUBPIXELS]) -> i32 {
    v.iter().copied().max().unwrap_or(0)
}

/// Quantise a signed edge X to the 3-fraction-bit sub-pixel domain used by
/// [`compute_coverage`], with the RDP sticky bit.
///
/// Any discarded fraction bit forces the low output bit set, so a
/// truncated-but-nonzero coordinate never snaps exactly onto a sub-pixel
/// boundary — this is what makes the half-open `<` / `>=` edge tests in
/// [`compute_coverage`] bit-exact (parallel-rdp `span_setup.comp:60-66`).
///
/// **Fixed-point domain.** parallel-rdp's `setup.xh` is `s.15` (its `base_x =
/// xh >> 15`), so it quantises with `>> 12` to reach the `s.3` coverage domain.
/// Our edge values are the raw command `s.16` (`major >> 16` is the pixel), one
/// fraction bit wider, so we shift `>> 13` and take the sticky bit over the low
/// 13 discarded bits — the same 3-fraction-bit result.
///
/// This and [`compute_coverage`] are the sub-pixel coverage primitives (T-33-004
/// slice 2c); the rasteriser's edge-walk still unions the four sub-scanlines into
/// a whole-pixel bounding span (**open residual R-9**) until the coverage
/// integration lands, so these have no runtime caller yet.
#[must_use]
pub const fn quantize_x(x: i32) -> i32 {
    let sticky = (x & 0x1FFF != 0) as i32;
    (x >> 13) | sticky
}

/// The 8-bit sub-pixel coverage mask for integer pixel column `x`, given the
/// per-Y-subpixel left/right span edges (each `s.3`, from [`quantize_x`]).
///
/// The RDP samples 4 Y-subpixels × 2 X-samples = 8 sub-positions. The two
/// X-samples sit at sub-pixel fractions that alternate by Y-subpixel — `{0, 4}`
/// for Y-subpixels 0/2 and `{2, 6}` for 1/3 — the RDP's diamond sample pattern
/// (`coverage.h:31-44` `u16x4(0, 4, 2, 6)`). A sample is covered when it lies in
/// the half-open span `[xleft, xright)` of its Y-subpixel.
///
/// **Bit layout.** The oracle's `clip_x0 * (1,2,4,8) + clip_x1 * (16,32,64,128)`
/// packs the two X-samples of each Y-subpixel into adjacent bits — bit
/// `2·Ysub + Xsample` — so the mask reads (LSB→MSB) `Y0X0 Y0X1 Y1X0 Y1X1 Y2X0
/// Y2X1 Y3X0 Y3X1`, *not* all-X0 then all-X1. Bit 0 is therefore the top-left
/// sample (Y-subpixel 0, X-sample 0), which is the one the AA-off path tests.
/// The popcount ([`u8::count_ones`]) is the coverage count (0–8) regardless of order.
///
/// See [`quantize_x`] for the domain of `xleft`/`xright` and why this has no
/// runtime caller yet.
#[must_use]
pub fn compute_coverage(
    xleft: [i32; COVERAGE_SUBPIXELS],
    xright: [i32; COVERAGE_SUBPIXELS],
    x: i32,
) -> u8 {
    // The four lanes carry X-sample offsets {0, 4, 2, 6}; `xshift = (x << 3) + offset`.
    let base = x << 3;
    let xshift = [base, base + 4, base + 2, base + 6];
    // `.xxyy` / `.zzww`: lanes 0-3 test Y-subpixels {0,0,1,1} then {2,2,3,3}.
    let ysub_lo = [0usize, 0, 1, 1];
    let ysub_hi = [2usize, 2, 3, 3];
    let mut clip = 0u8;
    let mut lane = 0;
    while lane < COVERAGE_SUBPIXELS {
        let lo = ysub_lo[lane];
        if xshift[lane] < xleft[lo] || xshift[lane] >= xright[lo] {
            clip |= 1 << lane;
        }
        let hi = ysub_hi[lane];
        if xshift[lane] < xleft[hi] || xshift[lane] >= xright[hi] {
            clip |= 1 << (lane + 4);
        }
        lane += 1;
    }
    !clip
}

/// One cycle of the colour combiner.
///
/// The four RGB input selects and the four alpha input selects for
/// `(A − B) * C + D` (`Set Combine Mode`, 0x3C). A/B/D RGB are 4-bit (D 3-bit),
/// C RGB is 5-bit; the alpha selects are all 3-bit.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CombineCycle {
    /// RGB `A` (muladd) select.
    pub rgb_a: u8,
    /// RGB `B` (mulsub) select.
    pub rgb_b: u8,
    /// RGB `C` (mul) select.
    pub rgb_c: u8,
    /// RGB `D` (add) select.
    pub rgb_d: u8,
    /// Alpha `A` select.
    pub a_a: u8,
    /// Alpha `B` select.
    pub a_b: u8,
    /// Alpha `C` select.
    pub a_c: u8,
    /// Alpha `D` select.
    pub a_d: u8,
}

/// The two-cycle colour-combiner configuration (`Set Combine Mode`, 0x3C).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CombineMode {
    /// Cycle-0 selects (the first stage in 2-cycle mode).
    pub cyc0: CombineCycle,
    /// Cycle-1 selects (the only stage used in 1-cycle mode).
    pub cyc1: CombineCycle,
}

/// The resolved per-pixel combiner input signals (each RGBA8888).
///
/// The combiner muxes these by the [`CombineCycle`] selects. Exotic inputs
/// (noise, LOD frac, the key/convert constants) are not modelled yet (**open
/// residual R-10**) and read as zero.
#[derive(Debug, Default, Clone, Copy)]
pub struct CombinerInputs {
    /// The previous cycle's output (cycle 0's result feeds cycle 1's `Combined`).
    pub combined: [u8; 4],
    /// Texel from tile 0.
    pub texel0: [u8; 4],
    /// Texel from tile 1.
    pub texel1: [u8; 4],
    /// The primitive colour (`Set Prim Color`, 0x3A).
    pub prim: [u8; 4],
    /// The interpolated shade colour.
    pub shade: [u8; 4],
    /// The environment colour (`Set Env Color`, 0x3B).
    pub env: [u8; 4],
}

/// TMEM size in bytes — 4 KiB of on-chip texture memory.
///
/// Addressed as 512 64-bit words. The upper half (byte >= 0x800 / word >= 0x100)
/// holds TLUTs and the high halves of 32-bit / YUV textures (N64brew
/// *…/Commands* §Set Tile).
pub const TMEM_SIZE: usize = 4096;

/// One of the RDP's eight tile descriptors.
///
/// The format/size/addressing state that binds a region of TMEM to a texture,
/// set by `Set Tile` (0x35) and sized by `Set Tile Size` (0x32) / the load
/// commands. All fields are decoded straight from the command word (N64brew
/// *…/Commands* §0x35/§0x32).
// The four bools are the hardware's four independent clamp/mirror bit-flags (one
// clamp + one mirror per S/T axis); they are decoded straight from command bits
// 8/9/18/19, so an enum would misrepresent the register rather than clarify it.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TileDescriptor {
    /// Texel format: RGBA=0, YUV=1, CI=2, IA=3, I=4 (`Set Tile` bits 55:53).
    pub format: u8,
    /// Texel size code: 4bpp=0, 8bpp=1, 16bpp=2, 32bpp=3 (bits 52:51).
    pub size: u8,
    /// Row stride in 64-bit TMEM words (bits 49:41).
    pub line: u16,
    /// Base TMEM address in 64-bit words (bits 40:32); word 0x100 = byte 0x800.
    pub tmem_addr: u16,
    /// Palette index, the high half of the TLUT address for CI4 tiles only
    /// (bits 23:20).
    pub palette: u8,
    // T-axis fields precede S-axis, matching the command word's MSB→LSB order
    // (T in bits 19:10, S in bits 9:0) and the `set_tile` decoder.
    /// Clamp T when sampling outside the tile (bit 19).
    pub clamp_t: bool,
    /// Mirror T on every other wrap (bit 18).
    pub mirror_t: bool,
    /// Number of T integer-coordinate bits used for wrap; 0 = all (bits 17:14).
    pub mask_t: u8,
    /// T coordinate shift code, per the shift table (bits 13:10).
    pub shift_t: u8,
    /// Clamp S when sampling outside the tile (bit 9).
    pub clamp_s: bool,
    /// Mirror S on every other wrap (bit 8).
    pub mirror_s: bool,
    /// Number of S integer-coordinate bits used for wrap; 0 = all (bits 7:4).
    pub mask_s: u8,
    /// S coordinate shift code, per the shift table (bits 3:0).
    pub shift_s: u8,
    /// Tile-size upper-left S (`u10.2`), from `Set Tile Size` / the loaders.
    pub sl: u16,
    /// Tile-size upper-left T (`u10.2`).
    pub tl: u16,
    /// Tile-size lower-right S (`u10.2`).
    pub sh: u16,
    /// Tile-size lower-right T (`u10.2`).
    pub th: u16,
}

/// RDP state (skeleton).
///
/// Holds the command-FIFO pointers, the current render mode (other-modes),
/// scissor rectangle, the color-image / Z-image RDRAM addresses, the
/// texture-image source registers, the eight tile descriptors, and TMEM. The
/// texel loads and the sampler/combiner that consume this state land in the rest
/// of the sprint. `#[non_exhaustive]`: this render state grows every sprint
/// (other-modes and the combiner latches are still to come), so adding a field
/// must not be a breaking change. Construct via [`Rdp::new`]; the workspace
/// never uses a struct literal.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct Rdp {
    /// DP command FIFO start (`DPC_START`).
    pub cmd_start: u32,
    /// DP command FIFO end (`DPC_END`).
    pub cmd_end: u32,
    /// DP command FIFO current (`DPC_CURRENT`).
    pub cmd_current: u32,
    /// DP command FIFO status (`DPC_STATUS`): FREEZE, START/END-valid, XBUS,
    /// and (later) the busy/counter bits.
    pub status: u32,
    /// Color-image (framebuffer) base in RDRAM (`Set Color Image`, 0x3F).
    pub color_image: u32,
    /// Color-image pixel size code (`Set Color Image` size\[1:0\]): 0 = 4-bit,
    /// 1 = 8-bit, 2 = 16-bit, 3 = 32-bit. Bytes-per-pixel derive from it.
    pub color_image_size: u8,
    /// Color-image pixel format code (`Set Color Image` format\[2:0\]); the same
    /// format enumeration as textures. Stored for later pipeline stages — the
    /// FILL path writes the raw fill value and does not consult it.
    pub color_image_format: u8,
    /// Color-image width in pixels (`Set Color Image` width\[9:0\] + 1). The row
    /// stride is `width * bytes_per_pixel`.
    pub color_image_width: u16,
    /// Z-image base in RDRAM (`Set Depth Image`, 0x3E).
    pub z_image: u32,
    /// Primitive depth `z` (`Set Primitive Depth`, 0x2E): the s15.3 depth used when
    /// `Set Other Modes` `z_source_sel` selects it (and the only depth source for
    /// rectangle commands). Stored as the raw 16-bit field.
    pub prim_z: u16,
    /// Primitive `dz` (`Set Primitive Depth`, 0x2E), the raw 16-bit field.
    pub prim_dz: u16,
    /// FILL-mode colour register (`Set Fill Color`, 0x37): a 32-bit value written
    /// verbatim to the color image. Its interpretation depends on the pixel size
    /// — one RGBA32, two RGBA16 (even pixel = upper half, odd = lower), or four
    /// 8-bit values repeating every four pixels.
    pub fill_color: u32,
    /// Scissor rectangle (`Set Scissor`, 0x2D), the four `u10.2` screen
    /// coordinates that bound every primitive: upper-left (x, y) and lower-right
    /// (x, y). Pixels outside it are neither processed nor written.
    pub scissor_ulx: u16,
    /// Scissor upper-left y (`u10.2`). See [`Rdp::scissor_ulx`].
    pub scissor_uly: u16,
    /// Scissor lower-right x (`u10.2`). See [`Rdp::scissor_ulx`].
    pub scissor_lrx: u16,
    /// Scissor lower-right y (`u10.2`). See [`Rdp::scissor_ulx`].
    pub scissor_lry: u16,
    /// Count of commands the FIFO decoder has retired. A **retired-work tally**,
    /// not a cycle position: nothing schedules against it (the residue
    /// invariant governs only `master_ticks`), it is derived from the command
    /// stream, and it exists so tests can witness that the decoder consumed the
    /// number of commands it should. Wraps rather than panicking.
    pub commands_processed: u64,
    /// GCLK cycles the pipeline is currently stalled, counted **down** one per
    /// `tick`; while non-zero the FIFO does not advance. Set by the sync
    /// commands to their documented fixed stalls ([`SYNC_LOAD_GCLK`] etc.). This
    /// is a stall countdown, not a cycle position — it is decremented, nothing
    /// derives a clock from it, and it does not touch the derive-don't-increment
    /// rule (only `master_ticks` is ever incremented; ADR 0006).
    pub stall: u32,
    /// Texture-image (`Set Texture Image`, 0x3D) format code — the RDRAM source
    /// for texture loads. The wiki notes this format has no effect on any
    /// operation (only the tile format matters); stored for completeness.
    pub tex_image_format: u8,
    /// Texture-image texel size code (0x3D size\[1:0\]): 4/8/16/32bpp = 0/1/2/3.
    /// Drives the per-texel address stride during a load.
    pub tex_image_size: u8,
    /// Texture-image width in pixels (0x3D width\[9:0\] + 1); the row stride of
    /// the RDRAM source during a load.
    pub tex_image_width: u16,
    /// Texture-image base address in RDRAM (0x3D dramAddress\[23:0\]).
    pub tex_image_addr: u32,
    /// The eight tile descriptors (`Set Tile` / `Set Tile Size` / the loaders).
    pub tiles: [TileDescriptor; 8],
    /// The colour-combiner configuration (`Set Combine Mode`, 0x3C).
    pub combine: CombineMode,
    /// The render-mode / blender configuration (`Set Other Modes`, 0x2F).
    pub other_modes: OtherModes,
    /// The primitive colour, RGBA8888 (`Set Prim Color`, 0x3A).
    pub prim_color: u32,
    /// The environment colour, RGBA8888 (`Set Env Color`, 0x3B).
    pub env_color: u32,
    /// The blend colour, RGBA8888 (`Set Blend Color`, 0x39).
    pub blend_color: u32,
    /// The fog colour, RGBA8888 (`Set Fog Color`, 0x38).
    pub fog_color: u32,
    /// On-chip texture memory (4 KiB). **Lazily allocated**: `None` until the
    /// first byte is written, and read as all-zero while `None`. This keeps
    /// [`Rdp`]'s `Default` cheap, which matters because `Bus::rdp_tick` does a
    /// `core::mem::take` every RCP step — a `None` placeholder is swapped in with
    /// no 4 KiB allocation or copy, while the real TMEM box moves by pointer.
    tmem: Option<alloc::boxed::Box<[u8; TMEM_SIZE]>>,
}

impl Rdp {
    /// Construct at power-on.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Read a DP command register by word offset within the `0x0410_0000`
    /// block: 0 `DPC_START`, 1 `DPC_END`, 2 `DPC_CURRENT`, 3 `DPC_STATUS`. The
    /// clock/busy/counter registers (4..=7) are not modelled and read zero.
    #[must_use]
    pub const fn dpc_read(&self, offset: u32) -> u32 {
        match offset & 7 {
            0 => self.cmd_start,
            1 => self.cmd_end,
            2 => self.cmd_current,
            3 => self.status,
            _ => 0,
        }
    }

    /// Write a DP command register (word offsets as in [`Rdp::dpc_read`]).
    ///
    /// The FIFO uses a double-latch pinned by n64-systemtest's `RSP STATUS:
    /// start-valid` and documented in the N64brew wiki (*Reality Display
    /// Processor Interface*, the `DPC_END` section):
    ///
    /// - Writing `DPC_START` latches the (masked) address and sets `START_VALID`
    ///   **only if it was clear** — a second write while valid is ignored.
    /// - Writing `DPC_END` latches the end address, then branches on
    ///   `START_VALID` (the wiki's `START_PENDING`): if **set**, this is a fresh
    ///   transfer — copy the pending start into `DPC_CURRENT` and clear
    ///   `START_VALID`. If **clear**, it is an *incremental* transfer that
    ///   continues from the current position, so `DPC_CURRENT` is left alone
    ///   (rewinding it would reprocess already-consumed commands). On unfrozen
    ///   hardware the transfer also runs; while frozen only the latch happens.
    pub const fn dpc_write(&mut self, offset: u32, value: u32) {
        match offset & 7 {
            0 => {
                if self.status & DP_STATUS_START_VALID == 0 {
                    self.cmd_start = value & DPC_ADDR_MASK;
                    self.status |= DP_STATUS_START_VALID;
                }
            }
            1 => {
                self.cmd_end = value & DPC_ADDR_MASK;
                if self.status & DP_STATUS_START_VALID != 0 {
                    self.cmd_current = self.cmd_start;
                    self.status &= !DP_STATUS_START_VALID;
                }
            }
            3 => self.dpc_write_status(value),
            _ => {}
        }
    }

    /// Apply a `DPC_STATUS` write, whose bits are set/clear *commands* rather
    /// than the status layout read back. Only XBUS and FREEZE are modelled; the
    /// FLUSH/TMEM/PIPE/CMD/CLOCK-counter commands come with the FIFO drain.
    // TODO(T-RDP-01): when `SET_FLUSH` (pipeline flush) lands here, it must also
    // clear `self.stall` — a flush discards in-flight pipeline work, so a
    // leftover sync-stall countdown must not persist across it. Subsystem-scoped
    // (pre-ticket) rather than T-31-003, which is the fill pipeline, not flush.
    const fn dpc_write_status(&mut self, value: u32) {
        const CLEAR_XBUS: u32 = 0x1;
        const SET_XBUS: u32 = 0x2;
        const CLEAR_FREEZE: u32 = 0x4;
        const SET_FREEZE: u32 = 0x8;
        if value & CLEAR_XBUS != 0 {
            self.status &= !DP_STATUS_XBUS;
        }
        if value & SET_XBUS != 0 {
            self.status |= DP_STATUS_XBUS;
        }
        if value & CLEAR_FREEZE != 0 {
            self.status &= !DP_STATUS_FREEZE;
        }
        if value & SET_FREEZE != 0 {
            self.status |= DP_STATUS_FREEZE;
        }
    }

    /// Advance the RDP by one rasterization step: decode the command at
    /// `DPC_CURRENT` and consume its whole length, so the FIFO drains one
    /// command per scheduler tick rather than in a burst.
    ///
    /// Hot path: keep allocation-free. No-op while the FIFO is empty
    /// (`DPC_CURRENT >= DPC_END`) or the DP is frozen (`DPC_STATUS.FREEZE`).
    ///
    /// The command length comes from [`command::command_len_words`], which
    /// recognises every opcode `0x00`–`0x3F`; consuming the exact length is what
    /// keeps a multi-word primitive from desyncing the pointer. Today the
    /// decoder only advances and counts — no primitive is rasterized yet.
    ///
    /// Commands are read from RDRAM (the `XBUS` bit clear). The `XBUS`/DMEM
    /// command source is not yet wired: the `rdpq` microcode that drives us DMAs
    /// its list to RDRAM, so the RDRAM path is the one exercised. With `XBUS`
    /// set the decoder **stalls** rather than mis-reading RDRAM as the command
    /// stream — decoding DMEM commands out of RDRAM would treat parameter data
    /// as opcodes and desync.
    ///
    /// Dispatch so far (`dispatch`) covers the four sync commands and the FILL
    /// pipeline (Set Color Image, Set Fill Color, Set Scissor, Fill Rectangle).
    /// Everything else is still recognised-and-consumed only.
    pub fn tick<B: VideoBus>(&mut self, bus: &mut B) {
        // Frozen or DMEM-sourced (XBUS, not yet wired): the pipeline counter is
        // halted, so do not even burn a stall cycle.
        if self.status & (DP_STATUS_FREEZE | DP_STATUS_XBUS) != 0 {
            return;
        }
        // A prior sync is still stalling the pipeline — burn one GCLK and hold
        // the FIFO until the stall expires.
        if self.stall > 0 {
            self.stall -= 1;
            return;
        }
        if self.cmd_current >= self.cmd_end {
            return;
        }
        let word0_hi = bus.rdram_read_u32(self.cmd_current);
        let opcode = command::opcode_of(word0_hi);
        let len_bytes = command::command_len_words(opcode) * 8;
        // Consume a command only once it is present in full. The `rdpq`
        // microcode advances `DPC_END` incrementally as it fills the buffer, so
        // `DPC_END` can land mid-command; consuming a partially-written
        // multi-word primitive would decode against unwritten RDRAM. The guard
        // above guarantees `cmd_current < cmd_end`, so the subtraction cannot
        // underflow.
        if self.cmd_end - self.cmd_current < len_bytes {
            return;
        }
        // The low half of the first command word. Multi-word commands (e.g.
        // Texture Rectangle, 2 words) read their later words through `cmd_base`,
        // the command's RDRAM address captured *before* the pointer advances.
        let word0_lo = bus.rdram_read_u32(self.cmd_current.wrapping_add(4));
        let cmd_base = self.cmd_current;
        self.cmd_current = self.cmd_current.wrapping_add(len_bytes);
        self.commands_processed = self.commands_processed.wrapping_add(1);
        self.dispatch(opcode, word0_hi, word0_lo, cmd_base, bus);
    }

    /// Act on a just-consumed command. Only the sync commands are handled so
    /// far; every other opcode is a recognised no-op until its handler lands.
    ///
    /// - `Sync Load`/`Pipe`/`Tile` (0x26/0x27/0x28) each stall the pipeline for
    ///   a fixed, unconditional number of GCLK cycles (25/50/33) — the RDP waits
    ///   the full time whether or not the sync was needed, which is why the
    ///   stall is a constant and not a wait on an internal signal.
    /// - `Sync Full` (0x29) **raises the DP interrupt** (`raise_dp_interrupt`) —
    ///   the only part of the command implemented. On hardware it first waits for
    ///   all staged pipeline/memory work and halts the pipeline counter; neither
    ///   is modelled (there is no asynchronous pipeline work yet, and no pipeline
    ///   counter), so the interrupt is raised as soon as the command is
    ///   dispatched. A *preceding* sync stall still delays this dispatch via the
    ///   `stall` gate above (checked before a command is dispatched), so a queued
    ///   stall drains before the interrupt fires.
    ///
    /// On stall resolution: per-command *execution* cost is not modelled yet —
    /// every command is consumed in a single placeholder `tick` — so the `stall`
    /// set here is the documented pipeline stall *layered on top of* that one
    /// consume tick, not a claim about total command latency (the next command
    /// resumes after `1 + N` ticks). The stall itself is exactly the documented
    /// N GCLK; exact per-command base timing is deferred to the command-timing
    /// model.
    ///
    /// The FILL-pipeline arms take the command's two 32-bit halves (`hi` =
    /// RDRAM bits 63:32, `lo` = 31:0). `Fill Rectangle` writes the fill colour
    /// into the color image, clipped to the scissor — the FILL-mode path (the
    /// cycle-type gate arrives with `Set Other Modes`, so `Fill Rectangle` is a
    /// solid FILL fill for now; 1-/2-cycle rectangles route through the blender,
    /// not this code).
    fn dispatch<B: VideoBus>(&mut self, opcode: u8, hi: u32, lo: u32, cmd_base: u32, bus: &mut B) {
        match opcode {
            OP_SYNC_LOAD => self.stall = SYNC_LOAD_GCLK,
            OP_SYNC_PIPE => self.stall = SYNC_PIPE_GCLK,
            OP_SYNC_TILE => self.stall = SYNC_TILE_GCLK,
            OP_SYNC_FULL => bus.raise_dp_interrupt(),
            OP_SET_COLOR_IMAGE => {
                // format[2:0] = hi 23:21, size[1:0] = hi 20:19, width[9:0] = hi
                // 9:0 (minus one), dramAddress[23:0] = lo 23:0.
                self.color_image_format = ((hi >> 21) & 0x7) as u8;
                self.color_image_size = ((hi >> 19) & 0x3) as u8;
                self.color_image_width = ((hi & 0x3FF) as u16).wrapping_add(1);
                self.color_image = lo & 0x00FF_FFFF;
            }
            OP_SET_FILL_COLOR => self.fill_color = lo,
            OP_SET_PRIM_COLOR => self.prim_color = lo,
            OP_SET_ENV_COLOR => self.env_color = lo,
            OP_SET_BLEND_COLOR => self.blend_color = lo,
            OP_SET_FOG_COLOR => self.fog_color = lo,
            OP_SET_DEPTH_IMAGE => self.z_image = lo & 0x00FF_FFFF,
            OP_SET_PRIM_DEPTH => {
                // z[15:0] = lo 31:16 (s15.3, integer part), dz[15:0] = lo 15:0.
                self.prim_z = (lo >> 16) as u16;
                self.prim_dz = lo as u16;
            }
            OP_SET_OTHER_MODES => self.set_other_modes(hi, lo),
            OP_SET_COMBINE_MODE => self.set_combine_mode(hi, lo),
            OP_SET_SCISSOR => {
                // upper-left x/y = hi 23:12 / 11:0, lower-right x/y = lo 23:12 /
                // 11:0 (all u10.2). The field/odd interlace bits (lo 25/24) are
                // not modelled yet.
                self.scissor_ulx = ((hi >> 12) & 0xFFF) as u16;
                self.scissor_uly = (hi & 0xFFF) as u16;
                self.scissor_lrx = ((lo >> 12) & 0xFFF) as u16;
                self.scissor_lry = (lo & 0xFFF) as u16;
            }
            OP_FILL_RECTANGLE => self.fill_rectangle(hi, lo, bus),
            OP_SET_TEXTURE_IMAGE => {
                // Same field layout as Set Color Image: format[55:53] = hi
                // 23:21, size[52:51] = hi 20:19, width[41:32] = hi 9:0 (minus
                // one), dramAddress[23:0] = lo 23:0.
                self.tex_image_format = ((hi >> 21) & 0x7) as u8;
                self.tex_image_size = ((hi >> 19) & 0x3) as u8;
                self.tex_image_width = ((hi & 0x3FF) as u16).wrapping_add(1);
                self.tex_image_addr = lo & 0x00FF_FFFF;
            }
            OP_SET_TILE => self.set_tile(hi, lo),
            OP_SET_TILE_SIZE => self.set_tile_size(hi, lo),
            OP_LOAD_TILE => self.load_tile(hi, lo, bus),
            OP_LOAD_BLOCK => self.load_block(hi, lo, bus),
            OP_LOAD_TLUT => self.load_tlut(hi, lo, bus),
            OP_TEXTURE_RECTANGLE => self.texture_rectangle(hi, lo, cmd_base, bus, false),
            OP_TEXTURE_RECTANGLE_FLIP => self.texture_rectangle(hi, lo, cmd_base, bus, true),
            // Fill Triangle and its shade/texture/Z variants (0x08–0x0F): flat-fill
            // the triangle for now (the shade/texture/Z coefficient blocks and the
            // combiner/blender come later in Sprint 3).
            0x08..=0x0F => self.triangle_fill(hi, lo, cmd_base, bus),
            // TODO(T-31-004): remaining opcodes are recognised and
            // length-consumed by `tick`, but not yet dispatched — an
            // intentional, documented no-op at this stage, not a silent discard.
            // Handlers arrive per ticket (VI scan-out, then texture / combiner /
            // blender), and `docs/rdp.md` is the authoritative list of what is
            // dispatched versus recognised-only, so a later missing arm is caught
            // against that spec rather than passing silently here.
            _ => {}
        }
    }

    /// Bytes per pixel for the current color-image size, or `None` for the
    /// 4-bit mode, which cannot be a FILL-mode render target (it would crash the
    /// real RDP — N64brew *…/Commands* §Set Color Image hazards).
    const fn color_image_bpp(&self) -> Option<u32> {
        match self.color_image_size {
            1 => Some(1), // 8-bit
            2 => Some(2), // 16-bit
            3 => Some(4), // 32-bit
            _ => None,    // 4-bit: crash on the real RDP
        }
    }

    /// Render a `Fill Rectangle` in FILL mode: write the 32-bit fill colour into
    /// the color image over the rectangle, clipped to the scissor.
    ///
    /// FILL mode "repeats the 32-bit value verbatim out to memory", which
    /// resolves per pixel by size (N64brew *…/Commands* §Set Fill Color):
    /// 32-bit writes the whole colour; 16-bit takes the upper half for even
    /// pixels and the lower half for odd; 8-bit takes byte `x & 3`. Coordinates
    /// are `u10.2`; FILL mode floors the upper-left and rounds the lower-right up
    /// (a half-open pixel span — N64brew *…/Commands* §Fill Rectangle). The exact
    /// sub-pixel edge rules, and the scissor's inclusive-right/exclusive-lower
    /// FILL rule, are an **open residual** (`docs/accuracy-ledger.md` R-3):
    /// byte-exact for aligned rectangles, validated bit-for-bit against Angrylion
    /// via the ParaLLEl-RDP fuzz suite (Sprint 3) and superseded there if it
    /// diverges.
    fn fill_rectangle<B: VideoBus>(&self, hi: u32, lo: u32, bus: &mut B) {
        let Some(bpp) = self.color_image_bpp() else {
            return; // 4-bit target: the real RDP crashes; we skip.
        };
        // No color image configured yet (width is field+1, so a real Set Color
        // Image never yields 0). Rendering before it is a documented hazard —
        // the real RDP writes to an unspecified location — so we write nothing
        // rather than smear every row onto offset 0 with a zero stride.
        if self.color_image_width == 0 {
            return;
        }
        // Rectangle: lower-right x/y = hi 23:12 / 11:0, upper-left x/y = lo 23:12
        // / 11:0 (all u10.2). Floor the upper-left, round the lower-right up.
        let rx0 = ((lo >> 12) & 0xFFF) >> 2;
        let ry0 = (lo & 0xFFF) >> 2;
        let rx1 = (((hi >> 12) & 0xFFF) + 3) >> 2;
        let ry1 = ((hi & 0xFFF) + 3) >> 2;
        // Scissor: floor upper-left, round lower-right up.
        let sx0 = u32::from(self.scissor_ulx) >> 2;
        let sy0 = u32::from(self.scissor_uly) >> 2;
        let sx1 = (u32::from(self.scissor_lrx) + 3) >> 2;
        let sy1 = (u32::from(self.scissor_lry) + 3) >> 2;
        // Intersection of rectangle and scissor (half-open).
        let x0 = rx0.max(sx0);
        let y0 = ry0.max(sy0);
        let x1 = rx1.min(sx1);
        let y1 = ry1.min(sy1);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let stride = u32::from(self.color_image_width) * bpp;
        for y in y0..y1 {
            let row = self.color_image.wrapping_add(y * stride);
            for x in x0..x1 {
                self.fill_pixel(row, x, bpp, bus);
            }
        }
    }

    /// Apply a `Texture Rectangle` (0x24) / `Flip` (0x25) in **COPY mode**: blit a
    /// tile into the colour image. Word 0 carries the screen rectangle (`u10.2`)
    /// and the tile; word 1 (read from `cmd_base`) carries the texture start
    /// (`S`/`T`, `s10.5`) and the per-pixel increments (`DsDx`/`DtDy`, `s5.10`).
    ///
    /// COPY mode is a raw texel blit — no combiner or blender. The lower-right
    /// screen bound is inclusive. For a 16-bit colour image the texel bits are
    /// copied verbatim (a direct 16-bit copy). `S` steps across X and `T` down Y
    /// (`Flip` swaps them); the horizontal step is scaled by the 4-pixels-per-cycle
    /// factor (`>> (5 + dx_shift)`) so a 1:1 blit's `DsDx = 4.0` advances one texel
    /// per pixel.
    ///
    /// Scope (**open residual R-8**): wired for a **16-bit tile → 16-bit colour
    /// image** (the first-picture path). `Flip`, the 8/32-bit and TLUT copy paths,
    /// non-1:1 sub-texel selection, and the copy alpha-compare are deferred to the
    /// ParaLLEl-RDP fuzz validation (Sprint 3); an unsupported configuration draws
    /// nothing.
    // The coordinate/address arithmetic casts here (screen/texel coords to `i32`
    // and back to `u32` offsets) wrap deliberately: a degenerate coordinate wraps
    // into the framebuffer/TMEM space rather than trapping. `bus` IS used mutably
    // (`rdram_write` in the inner loop); `needless_pass_by_ref_mut` mis-analyses the
    // mutable trait call nested past the early-return guard (a known false positive).
    #[allow(
        clippy::cast_sign_loss,
        clippy::cast_possible_wrap,
        clippy::needless_pass_by_ref_mut
    )]
    fn texture_rectangle<B: VideoBus>(
        &mut self,
        hi: u32,
        lo: u32,
        cmd_base: u32,
        bus: &mut B,
        flip: bool,
    ) {
        // Word 0: screen rectangle (u10.2) + tile index.
        let xl = (hi >> 12) & 0xFFF;
        let yl = hi & 0xFFF;
        let tile_idx = ((lo >> 24) & 0x7) as usize;
        let xh = (lo >> 12) & 0xFFF;
        let yh = lo & 0xFFF;
        // Word 1: texture start (s10.5) + increments (s5.10).
        let w1_hi = bus.rdram_read_u32(cmd_base.wrapping_add(8));
        let w1_lo = bus.rdram_read_u32(cmd_base.wrapping_add(12));
        // S/T are signed s10.5 (a scrolled/wrapped tile can start negative), so
        // sign-extend like DsDx/DtDy — a plain mask would read bit 15 as +32768.
        let s_start = sext16(w1_hi >> 16);
        let t_start = sext16(w1_hi);
        let dsdx = sext16(w1_lo >> 16);
        let dtdy = sext16(w1_lo);

        let tile = self.tiles[tile_idx];
        // Only the 16-bit -> 16-bit copy is wired (R-8); Flip too.
        if flip || tile.size != 2 || self.color_image_size != 2 || self.color_image_width == 0 {
            return;
        }
        let dx_shift = 2u32; // 4 pixels per 64-bit cycle for a 16-bit image.
        // Integer pixel bounds; COPY mode's lower-right is inclusive.
        let px0 = xh >> 2;
        let py0 = yh >> 2;
        // Clip to the scissor (floor upper-left, and the rect's inclusive lower-right).
        let x_lo = px0.max(u32::from(self.scissor_ulx) >> 2);
        let y_lo = py0.max(u32::from(self.scissor_uly) >> 2);
        let x_hi =
            (xl >> 2).min((u32::from(self.scissor_lrx).wrapping_add(3) >> 2).saturating_sub(1));
        let y_hi =
            (yl >> 2).min((u32::from(self.scissor_lry).wrapping_add(3) >> 2).saturating_sub(1));
        let stride = u32::from(self.color_image_width) * 2;
        for py in y_lo..=y_hi {
            let row = (py - py0) as i32;
            let t105 = t_start + ((dtdy * row) >> 5);
            let t_tex = wrap_coord(t105, tile.shift_t, tile.mask_t, tile.mirror_t, tile.tl);
            let swap = ((t_tex & 1) << 2) as u32;
            let t_row = u32::from(tile.line)
                .wrapping_mul(8)
                .wrapping_mul(t_tex as u32);
            let row_addr = self.color_image.wrapping_add(py.wrapping_mul(stride));
            for px in x_lo..=x_hi {
                let col = (px - px0) as i32;
                // Horizontal step scaled for the 4-pixels-per-cycle copy.
                let s105 = s_start + ((dsdx * col) >> (5 + dx_shift));
                let s_tex = wrap_coord(s105, tile.shift_s, tile.mask_s, tile.mirror_s, tile.sl);
                // Raw 16-bit texel fetch (RGBA16 addressing, no decode).
                let boff = (u32::from(tile.tmem_addr).wrapping_mul(8))
                    .wrapping_add(t_row)
                    .wrapping_add((s_tex as u32).wrapping_mul(2))
                    ^ swap;
                let texel = self.tmem_u16(boff);
                let addr = row_addr.wrapping_add(px.wrapping_mul(2));
                bus.rdram_write(addr, (texel >> 8) as u8);
                bus.rdram_write(addr.wrapping_add(1), (texel & 0xFF) as u8);
            }
        }
    }

    /// Write the FILL-mode colour to one pixel of the colour image (shared by the
    /// fill rectangle and the flat-fill triangle). `bpp` is 1/2/4; the 16-bit case
    /// takes the upper half of the fill register for even `x` and the lower for
    /// odd, and the 8-bit case cycles the four bytes — as `Set Fill Color` defines.
    fn fill_pixel<B: VideoBus>(&self, row_addr: u32, x: u32, bpp: u32, bus: &mut B) {
        let addr = row_addr.wrapping_add(x.wrapping_mul(bpp));
        let color = self.fill_color.to_be_bytes();
        match bpp {
            4 => {
                for (i, b) in color.iter().enumerate() {
                    bus.rdram_write(addr.wrapping_add(i as u32), *b);
                }
            }
            2 => {
                let half = if x & 1 == 0 { 0 } else { 2 };
                bus.rdram_write(addr, color[half]);
                bus.rdram_write(addr.wrapping_add(1), color[half + 1]);
            }
            1 => bus.rdram_write(addr, color[(x & 3) as usize]),
            _ => {}
        }
    }

    /// Flat-fill a `Fill Triangle` (0x08) or one of its shade/texture/Z variants
    /// (0x09–0x0F). Decode the three edges (major `H` yh→yl, minor `M` yh→ym, minor
    /// `L` ym→yl), walk each scanline's span between the major edge and the active
    /// minor edge, and write the FILL-mode colour into the span — the FILL-cycle
    /// path (a 1-/2-cycle triangle is coloured by the combiner/blender, which is
    /// later in Sprint 3, so the shade/texture/Z coefficient words are ignored
    /// here, only length-consumed).
    ///
    /// Y is `s11.2` (four sub-scanlines per pixel); X and the slopes are `s11.16` /
    /// `s13.16`. Per sub-scanline the edge X is `x0 + (y − yh_base) * slope`,
    /// reduced to a whole pixel (`>> 16`); `lmajor`/`flip` (bit 55) selects which
    /// edge is the left bound. Matched to the ParaLLEl-RDP `interpolate_x` walk
    /// (native scaling — no upscale sub-pixel bit); the bit-exact sub-pixel
    /// coverage (`quantize_x` sticky bit) and attribute interpolation are deferred
    /// to the fuzz-validated pipeline (**open residual R-9**).
    // `bus` IS used mutably (`fill_pixel` → `rdram_write`); the lint mis-analyses the
    // call nested past the early returns. `xl`/`xh`/`xm` etc. are the hardware edge names.
    #[allow(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        clippy::needless_pass_by_ref_mut,
        clippy::similar_names
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "the rasteriser's edge decode, span walk, and the flat/shaded/depth render paths are one tightly-coupled unit; splitting further would fragment the shared setup"
    )]
    fn triangle_fill<B: VideoBus>(&mut self, hi: u32, lo: u32, cmd_base: u32, bus: &mut B) {
        let Some(bpp) = self.color_image_bpp() else {
            return;
        };
        if self.color_image_width == 0 {
            return;
        }
        let flip = hi >> 23 & 1 != 0;
        let yl = sext(hi & 0x3FFF, 14);
        let mut ym = sext(lo >> 16 & 0x3FFF, 14);
        let yh = sext(lo & 0x3FFF, 14);
        if yl <= yh {
            return; // degenerate
        }
        // The triangle setup guarantees yh <= ym <= yl (sorted vertices); clamp
        // malformed input into range so the M/L edge split stays well-defined.
        ym = ym.clamp(yh, yl);
        // Edge coefficients: words 1 (L), 2 (H major), 3 (M).
        //
        // The slopes `dx?dy` are `s13.16` **dx per pixel-row** (N64brew *…/Commands*
        // §Edge Coefficients: "change in x per change in y", with `yh/ym/yl` in
        // `s11.2` *screen* pixels). The edge-walk below advances the edge per
        // Y-subpixel — `y = line*4 + sub` is in **quarter-pixel** units — so each
        // slope is pre-shifted `>> SUB_SCANLINE_SHIFT` to a per-quarter-pixel step
        // (parallel-rdp `span_setup.comp:167`, where `setup.dxhdy = raw >> 2`).
        // Omitting this advanced every edge 4× too fast (ledger R-14, caught by the
        // T-33-005 conformance gate against Angrylion). The arithmetic shift rounds
        // a negative slope toward −∞, matching the hardware (`fill_tri_neg_16`).
        let xl = sext(
            bus.rdram_read_u32(cmd_base.wrapping_add(8)) & 0x0FFF_FFFF,
            28,
        );
        let dxldy = sext(
            bus.rdram_read_u32(cmd_base.wrapping_add(12)) & 0x3FFF_FFFF,
            30,
        ) >> SUB_SCANLINE_SHIFT;
        let xh = sext(
            bus.rdram_read_u32(cmd_base.wrapping_add(16)) & 0x0FFF_FFFF,
            28,
        );
        let dxhdy = sext(
            bus.rdram_read_u32(cmd_base.wrapping_add(20)) & 0x3FFF_FFFF,
            30,
        ) >> SUB_SCANLINE_SHIFT;
        let xm = sext(
            bus.rdram_read_u32(cmd_base.wrapping_add(24)) & 0x0FFF_FFFF,
            28,
        );
        let dxmdy = sext(
            bus.rdram_read_u32(cmd_base.wrapping_add(28)) & 0x3FFF_FFFF,
            30,
        ) >> SUB_SCANLINE_SHIFT;

        // Scissor in integer pixels (u10.2 -> pixel).
        let sx0 = i32::from(self.scissor_ulx) >> 2;
        let sx1 = i32::from(self.scissor_lrx) >> 2;
        let sy0 = i32::from(self.scissor_uly) >> 2;
        let sy1 = i32::from(self.scissor_lry) >> 2;
        let start_line = (yh >> 2).max(sy0);
        let end_line = ((yl - 1) >> 2).min(sy1 - 1);
        let yh_base = yh & !3;
        let width = i32::from(self.color_image_width);
        let stride = (width as u32).wrapping_mul(bpp);

        // Per-pixel depth path (T-33-004 PR-B part 2a): active when the command
        // carries z-coefficients (bit 56, the opcode's low bit) and `Set Other
        // Modes` enables the depth test or update. The z-suffix follows the 4-word
        // base plus the shade (bit 58) and texture (bit 57) blocks, in that order.
        let z_setup = if self.other_modes.z_compare_en || self.other_modes.z_update_en {
            Self::decode_triangle_z(hi, cmd_base, bus)
        } else {
            None
        };
        // Shade block (bit 58): when present, the pixel colour comes from the
        // combiner fed the interpolated shade, not the FILL register (T-33-004
        // PR-B 2b — the first shaded triangle).
        let shade_setup = Self::decode_shade(hi, cmd_base, bus);
        // Texture block (bit 57): the combiner samples tile 0 at the interpolated
        // (non-perspective) coordinate. The perspective divide is a later slice.
        let tex_setup = Self::decode_texture(hi, cmd_base, bus);
        let has_color = shade_setup.is_some() || tex_setup.is_some();
        let y_base = yh >> 2;

        // 1-/2-cycle mode rasterises with sub-pixel coverage; FILL/COPY mode
        // (`cycle_type >= 2`) rounds to whole pixels (N64brew *…/Commands*: FILL is
        // "without subpixel accuracy"), which the union span already models exactly.
        let subpixel = self.other_modes.cycle_type < 2;
        // Sub-pixel scissor bounds (`s.3`): the raw `s10.2` scissor is one fraction
        // bit narrower, so `<< 1` lifts it (parallel-rdp `span_setup.comp:196`).
        let sc_lo = i32::from(self.scissor_ulx) << 1;
        let sc_hi = i32::from(self.scissor_lrx) << 1;
        for line in start_line..=end_line {
            let mut span_l = i32::MAX;
            let mut span_r = i32::MIN;
            // Per-Y-subpixel `s.3` edges for the sub-pixel coverage path.
            let mut xleft = [SPAN_X_POISON_LEFT; COVERAGE_SUBPIXELS];
            let mut xright = [SPAN_X_POISON_RIGHT; COVERAGE_SUBPIXELS];
            for sub in 0..4 {
                let y = line * 4 + sub;
                if y < yh || y >= yl {
                    continue;
                }
                let major = i64::from(xh) + i64::from(y - yh_base) * i64::from(dxhdy);
                let minor = if y < ym {
                    i64::from(xm) + i64::from(y - yh_base) * i64::from(dxmdy)
                } else {
                    i64::from(xl) + i64::from(y - ym) * i64::from(dxldy)
                };
                let major_x = (major >> 16) as i32;
                let minor_x = (minor >> 16) as i32;
                let (xl_i, xr_i) = if flip {
                    (major_x, minor_x)
                } else {
                    (minor_x, major_x)
                };
                if xl_i > xr_i {
                    continue;
                }
                // The union bounding span (FILL / COPY / depth paths).
                span_l = span_l.min(xl_i);
                span_r = span_r.max(xr_i);
                // Sub-pixel edges (`s.3`, sticky-bit snapped) for the coverage path:
                // quantise the `s.16` major/minor and clamp to the scissor.
                let (raw_l, raw_r) = if flip { (major, minor) } else { (minor, major) };
                #[allow(clippy::cast_possible_truncation)]
                let el = quantize_x(sext(raw_l as u32, 27)).clamp(sc_lo, sc_hi);
                #[allow(clippy::cast_possible_truncation)]
                let er = quantize_x(sext(raw_r as u32, 27)).clamp(sc_lo, sc_hi);
                if (el >> 1) <= (er >> 1) {
                    xleft[sub as usize] = el;
                    xright[sub as usize] = er;
                }
            }
            let (x0, x1) = if subpixel {
                (
                    (min4(&xleft) >> 3).max(0),
                    (max4(&xright) >> 3).min(width - 1),
                )
            } else {
                (span_l.max(sx0).max(0), span_r.min(sx1).min(width - 1))
            };
            if x0 > x1 {
                continue;
            }
            let row_addr = self
                .color_image
                .wrapping_add((line as u32).wrapping_mul(stride));
            // The major-edge x at this scanline (s15.16), the interpolation origin
            // shared by the depth and shade interpolators.
            let major_x = i64::from(xh) + i64::from(line * 4 - yh_base) * i64::from(dxhdy);
            if let Some(z) = z_setup {
                self.depth_span(
                    row_addr,
                    x0,
                    x1,
                    line,
                    bpp,
                    major_x,
                    y_base,
                    &z,
                    shade_setup.as_ref(),
                    tex_setup.as_ref(),
                    bus,
                );
            } else if has_color {
                // The no-Z path (a triangle with no z-suffix): the combiner colour.
                // In 1-/2-cycle mode each pixel's sub-pixel coverage gates the write
                // and is stored in the pixel alpha (the AA/`cvg_dest` write-back); the
                // memory-read blender still lives only on the depth path (R-9/R-11).
                #[allow(clippy::cast_sign_loss, reason = "x >= 0 within a clipped span")]
                for x in x0..=x1 {
                    let (mut color, _shade_alpha) = self.combined_color(
                        shade_setup.as_ref(),
                        tex_setup.as_ref(),
                        major_x,
                        line,
                        y_base,
                        x,
                    );
                    if subpixel {
                        match self.pixel_coverage(&xleft, &xright, x) {
                            Some(cov) => color[3] = cov << 5,
                            None => continue,
                        }
                    }
                    Self::write_pixel(row_addr, x as u32, bpp, color, bus);
                }
            } else {
                #[allow(clippy::cast_sign_loss, reason = "x >= 0 within a clipped span")]
                for x in x0..=x1 {
                    self.fill_pixel(row_addr, x as u32, bpp, bus);
                }
            }
        }
    }

    /// Decode the z-coefficient suffix of a `Fill Triangle` — present when bit 56
    /// (the opcode's low bit) is set. The suffix follows the 4-word base plus the
    /// shade (bit 58, +8 words) and texture (bit 57, +8 words) blocks, in that
    /// order; each field is `s15.16`. Returns `None` when there is no z-suffix.
    #[allow(
        clippy::cast_possible_wrap,
        reason = "the s15.16 z-coefficients are the raw command bits reinterpreted as signed"
    )]
    #[allow(
        clippy::similar_names,
        reason = "dzdx / dzde are the N64 RDP's own z-coefficient names"
    )]
    fn decode_triangle_z<B: VideoBus>(hi: u32, cmd_base: u32, bus: &B) -> Option<ZTriSetup> {
        if (hi >> 24) & 1 == 0 {
            return None;
        }
        let has_shade = (hi >> 26) & 1 != 0;
        let has_tex = (hi >> 25) & 1 != 0;
        let zoff = (4 + 8 * u32::from(has_shade) + 8 * u32::from(has_tex)) * 8;
        let za = cmd_base.wrapping_add(zoff);
        let z_base = bus.rdram_read_u32(za) as i32;
        let dzdx = bus.rdram_read_u32(za.wrapping_add(4)) as i32;
        let dzde = bus.rdram_read_u32(za.wrapping_add(8)) as i32;
        // `dzdy` (za + 12) is the 4th z-suffix word; it feeds only the sub-pixel
        // snap (part 2c), so the per-scanline path here uses `dzde` and leaves it.
        //
        // Primitive dz for the stored value and the test tolerance: the integer
        // depth gradient (first cut — the exact setup derivation is R-9/R-12).
        // `saturating_abs` avoids the `i32::MIN.abs()` overflow panic on the
        // unvalidated RDRAM coefficients.
        let dz = dzdx.saturating_abs().max(dzde.saturating_abs()) >> 16;
        Some(ZTriSetup {
            z_base,
            dzdx,
            dzde,
            dz,
            dz_compressed: dz_compress(dz).min(0xf),
        })
    }

    /// Decode the 8-word shade coefficient block of a `Fill Triangle` (present when
    /// bit 58 is set) into [`ShadeSetup`]. It follows the 4-word base immediately
    /// (before the texture/z blocks). Per channel the value is `s15.16`: the base's
    /// int part is 9-bit signed, the deltas' int parts 16-bit (N64brew *…/Commands*
    /// §Fill Shaded Triangle). Returns `None` when there is no shade block.
    #[allow(
        clippy::cast_possible_wrap,
        reason = "the s15.16 delta is the raw (int << 16 | frac) bits reinterpreted as signed"
    )]
    fn decode_shade<B: VideoBus>(hi: u32, cmd_base: u32, bus: &B) -> Option<ShadeSetup> {
        if (hi >> 26) & 1 == 0 {
            return None;
        }
        let sa = cmd_base.wrapping_add(4 * 8);
        let w = |word: u32| bus.rdram_read_u32(sa.wrapping_add(word * 4));
        // Words: 0 int-base, 1 dx-int, 2 frac-base, 3 dx-frac, 4 de-int, 5 dy-int,
        // 6 de-frac, 7 dy-frac. Each 64-bit word packs R/G/B/A; read as two u32
        // halves — channels 0/1 (R/G) in the hi u32, 2/3 (B/A) in the lo. Within a
        // u32, the even channel (R/B) is the high 16 bits, the odd (G/A) the low.
        let u32_index = |base_word: u32, ch: usize| base_word * 2 + (ch >> 1) as u32;
        let field = |base_word: u32, ch: usize, mask: u32| {
            let shift = 16 * (1 - (ch & 1) as u32);
            (w(u32_index(base_word, ch)) >> shift) & mask
        };
        let mut shade = ShadeSetup::default();
        for ch in 0..4 {
            // Base: 9-bit int (word 0) + 16-bit frac (word 2) -> a 25-bit s(9).16.
            let i9 = field(0, ch, 0x1FF);
            let bf = field(2, ch, 0xFFFF);
            shade.base[ch] = sext((i9 << 16) | bf, 25);
            // Deltas: 16-bit int + 16-bit frac -> a full s15.16, reinterpreted.
            let assemble =
                |iw: u32, fw: u32| ((field(iw, ch, 0xFFFF) << 16) | field(fw, ch, 0xFFFF)) as i32;
            shade.dx[ch] = assemble(1, 3);
            shade.de[ch] = assemble(4, 6);
        }
        Some(shade)
    }

    /// Write one RGBA8888 pixel to the colour image at `(row_addr, x)`: direct for a
    /// 32-bit image, packed to RGBA5551 for a 16-bit one (matching `fill_pixel`'s
    /// addressing). Other sizes are unsupported and write nothing.
    fn write_pixel<B: VideoBus>(row_addr: u32, x: u32, bpp: u32, rgba: [u8; 4], bus: &mut B) {
        let addr = row_addr.wrapping_add(x.wrapping_mul(bpp));
        match bpp {
            4 => {
                for (i, b) in rgba.iter().enumerate() {
                    bus.rdram_write(addr.wrapping_add(i as u32), *b);
                }
            }
            2 => {
                let p = pack_rgba5551(rgba).to_be_bytes();
                bus.rdram_write(addr, p[0]);
                bus.rdram_write(addr.wrapping_add(1), p[1]);
            }
            _ => {}
        }
    }

    /// Read the current colour-image pixel at `(row_addr, x)` as RGBA8888 — the
    /// blender's `memory_color`. The inverse of [`Self::write_pixel`]: direct for a
    /// 32-bit image, RGBA5551 widened (5→8 bits) for a 16-bit one.
    ///
    /// An 8-bit colour image (`bpp == 1`, a legal but unsupported render target —
    /// [`Self::color_image_bpp`] returns `Some(1)` for it) reads as transparent
    /// black, mirroring [`Self::write_pixel`]'s silent no-op for the same size (the
    /// blended result is discarded there anyway, so no draw happens either way). It
    /// stays a graceful default rather than a panic because `bpp` derives from a
    /// `Set Color Image` field under ROM control (module 60: never panic on external
    /// input, and a `debug_assert` would fire on that legal configuration).
    fn read_pixel<B: VideoBus>(row_addr: u32, x: u32, bpp: u32, bus: &B) -> [u8; 4] {
        let addr = row_addr.wrapping_add(x.wrapping_mul(bpp));
        match bpp {
            4 => [
                bus.rdram_read(addr),
                bus.rdram_read(addr.wrapping_add(1)),
                bus.rdram_read(addr.wrapping_add(2)),
                bus.rdram_read(addr.wrapping_add(3)),
            ],
            2 => {
                let p = u16::from_be_bytes([
                    bus.rdram_read(addr),
                    bus.rdram_read(addr.wrapping_add(1)),
                ]);
                unpack_rgba5551(p)
            }
            // 8-bit (bpp 1) and any other size: transparent black, as documented above.
            _ => [0; 4],
        }
    }

    /// Decode the 8-word texture coefficient block of a `Fill Triangle` (present
    /// when bit 57 is set) into [`TexSetup`]. It follows the 4-word base plus the
    /// shade block (if bit 58 set), before the z block. Each word packs `S`/`T`/`W`
    /// into bits 63:48/47:32/31:16 (`s16.16`); this slice keeps only `S`/`T` (both
    /// in the hi u32) — the `W` perspective term is the deferred perspective slice.
    #[allow(
        clippy::cast_possible_wrap,
        reason = "the s16.16 coordinate is the raw (int << 16 | frac) bits reinterpreted"
    )]
    fn decode_texture<B: VideoBus>(hi: u32, cmd_base: u32, bus: &B) -> Option<TexSetup> {
        if (hi >> 25) & 1 == 0 {
            return None;
        }
        let has_shade = (hi >> 26) & 1;
        let ta = cmd_base.wrapping_add((4 + 8 * has_shade) * 8);
        let w = |word: u32| bus.rdram_read_u32(ta.wrapping_add(word * 4));
        // Each 64-bit word packs S (bits 63:48), T (47:32), W (31:16): S and T are in
        // the hi u32 (shift 16 / 0), W in the lo u32 (shift 16). `c` = 0 S, 1 T, 2 W.
        let field = |base_word: u32, c: usize| {
            let u32_off = u32::from(c == 2);
            let shift = if c == 1 { 0 } else { 16 };
            (w(base_word * 2 + u32_off) >> shift) & 0xFFFF
        };
        let assemble = |iw: u32, fw: u32, c: usize| ((field(iw, c) << 16) | field(fw, c)) as i32;
        let mut tex = TexSetup::default();
        for c in 0..3 {
            tex.base[c] = assemble(0, 2, c); // int word 0, frac word 2
            tex.dx[c] = assemble(1, 3, c); // per-x
            tex.de[c] = assemble(4, 6, c); // per-major-edge
        }
        Some(tex)
    }

    /// Compute a pixel's colour through the combiner from the interpolated shade
    /// and/or sampled texel (plus the prim/env registers). `shade`/`tex` are the
    /// decoded setups, `None` when that attribute is absent. The two-cycle mode
    /// comes from `Set Other Modes`. Texture uses the non-perspective coordinate
    /// (`interpolate_st`) sampled from tile 0.
    ///
    /// Returns the combiner output **and** the interpolated shade alpha, which the
    /// blender selects separately (`A`-select 2) from the combiner output alpha
    /// (`A`-select 0) — the two differ whenever the alpha combiner transforms the
    /// shade alpha. Shade alpha is `0` when the triangle carries no shade block.
    fn combined_color(
        &self,
        shade: Option<&ShadeSetup>,
        tex: Option<&TexSetup>,
        major_x: i64,
        line: i32,
        y_base: i32,
        x: i32,
    ) -> ([u8; 4], u8) {
        let mut inp = CombinerInputs {
            prim: unpack_rgba(self.prim_color),
            env: unpack_rgba(self.env_color),
            ..CombinerInputs::default()
        };
        if let Some(shade) = shade {
            inp.shade =
                interpolate_shade(&shade.base, &shade.dx, &shade.de, major_x, line, y_base, x);
        }
        if let Some(tex) = tex {
            let [s, t] =
                interpolate_st(tex, self.other_modes.persp_tex_en, major_x, line, y_base, x);
            inp.texel0 = self.fetch_texel(&self.tiles[0], s, t);
        }
        let shade_alpha = inp.shade[3];
        (
            self.combine(inp, self.other_modes.cycle_type == 1),
            shade_alpha,
        )
    }

    /// The stored coverage value (`0..=7`) for pixel column `x` under sub-pixel
    /// coverage, or `None` when the pixel is not drawn.
    ///
    /// A zero mask kills the pixel. With anti-aliasing off, only the first
    /// sub-sample (mask bit 0 — the top-left) matters, so a pixel whose top-left
    /// sample is outside the span is dropped (parallel-rdp `shading.h:171-178`). The
    /// stored coverage is the `COVERAGE_CLAMP` no-blend write-back `(count - 1) & 7`
    /// (`coverage.h`), which packs into the pixel's alpha/coverage bits — full
    /// coverage (count 8) stores 7, so the RGBA5551 alpha bit (`cov >> 2`) is set.
    /// The coverage-weighted AA blend and the other `cvg_dest` modes are slice 2c-2.
    fn pixel_coverage(
        &self,
        xleft: &[i32; COVERAGE_SUBPIXELS],
        xright: &[i32; COVERAGE_SUBPIXELS],
        x: i32,
    ) -> Option<u8> {
        let mask = compute_coverage(*xleft, *xright, x);
        if mask == 0 || (!self.other_modes.aa_enable && (mask & 1) == 0) {
            return None;
        }
        #[allow(clippy::cast_possible_truncation)] // count_ones() is 1..=8 here
        Some(((mask.count_ones() - 1) & 7) as u8)
    }

    /// Render one scanline's span with the per-pixel depth test (T-33-004 PR-B 2a):
    /// for each pixel, interpolate the depth, test it against the Z buffer, and —
    /// only if it passes — write the colour and (when `z_update`) the depth. Coverage
    /// is full (`8`) here; sub-pixel edge coverage is the deferred R-9 residual.
    #[allow(
        clippy::too_many_arguments,
        reason = "an internal rasteriser span helper; a struct would only relocate the parameters"
    )]
    #[allow(
        clippy::cast_sign_loss,
        reason = "x and line are >= 0 within a clipped span"
    )]
    #[allow(
        clippy::similar_names,
        reason = "mem_z / mem_dz mirror the memory depth/dz pair they hold"
    )]
    fn depth_span<B: VideoBus>(
        &self,
        row_addr: u32,
        x0: i32,
        x1: i32,
        line: i32,
        bpp: u32,
        major_x: i64,
        y_base: i32,
        z: &ZTriSetup,
        shade: Option<&ShadeSetup>,
        tex: Option<&TexSetup>,
        bus: &mut B,
    ) {
        let yu = line as u32;
        for x in x0..=x1 {
            let z_px = interpolate_z(z.z_base, z.dzdx, z.dzde, major_x, line, y_base, x);
            let xu = x as u32;
            let (mem_z, mem_dz) = self.zbuffer_read(xu, yu, bus);
            let dinp = DepthInputs {
                current_depth: mem_z,
                current_dz: mem_dz,
                current_coverage: 0,
                z_compare: self.other_modes.z_compare_en,
                z_mode: self.other_modes.z_mode,
                force_blend: self.other_modes.force_blend,
                // AA-edge coverage (the `aa_enable && farther` blend path) is slice 2c;
                // until per-pixel coverage exists, only `force_blend` drives the blender.
                aa_enable: false,
            };
            let dr = Self::depth_test(z_px, z.dz, z.dz_compressed, 8, &dinp);
            if dr.depth_pass {
                // Shaded/textured triangles take the combiner colour; else the FILL register.
                if shade.is_some() || tex.is_some() {
                    let (mut color, shade_alpha) =
                        self.combined_color(shade, tex, major_x, line, y_base, x);
                    // The blender runs only when the depth test enabled it (translucent /
                    // AA-edge pixels); an opaque pixel keeps the combiner colour, matching
                    // the reference's `!blend_en` fast-path. The full opaque-alpha fast path
                    // and coverage-driven alpha are R-11.
                    if dr.blend_en {
                        let memory = Self::read_pixel(row_addr, xu, bpp, bus);
                        let rgb = self.blend(BlendInputs {
                            pixel: color,
                            memory,
                            blend_color: unpack_rgba(self.blend_color),
                            fog: unpack_rgba(self.fog_color),
                            // The blender's shade-alpha mux input (`A`-select 2) is the
                            // interpolated shade alpha, independent of the combiner output
                            // alpha (`A`-select 0, carried in `pixel[3]`).
                            shade_alpha,
                        });
                        // The blender produces RGB only; the alpha byte keeps the combiner
                        // output alpha. The destination alpha / coverage write-back
                        // (`cvg_dest`) is deliberately deferred to slice 2c, not omitted.
                        color = [rgb[0], rgb[1], rgb[2], color[3]];
                    }
                    Self::write_pixel(row_addr, xu, bpp, color, bus);
                } else {
                    self.fill_pixel(row_addr, xu, bpp, bus);
                }
                if self.other_modes.z_update_en {
                    #[allow(
                        clippy::cast_sign_loss,
                        clippy::cast_possible_truncation,
                        reason = "dz_compressed is clamped to 0..=0xf in decode_triangle_z"
                    )]
                    self.zbuffer_write(xu, yu, z_px, z.dz_compressed as u8, bus);
                }
            }
        }
    }

    /// Apply a `Set Combine Mode` (0x3C): decode the 16 mux input selects for both
    /// cycles into [`CombineMode`] (N64brew *…/Commands* §0x3C, matched to the
    /// ParaLLEl-RDP field layout).
    fn set_combine_mode(&mut self, hi: u32, lo: u32) {
        self.combine.cyc0 = CombineCycle {
            rgb_a: ((hi >> 20) & 0xF) as u8,
            rgb_c: ((hi >> 15) & 0x1F) as u8,
            rgb_b: ((lo >> 28) & 0xF) as u8,
            rgb_d: ((lo >> 15) & 0x7) as u8,
            a_a: ((hi >> 12) & 0x7) as u8,
            a_c: ((hi >> 9) & 0x7) as u8,
            a_b: ((lo >> 12) & 0x7) as u8,
            a_d: ((lo >> 9) & 0x7) as u8,
        };
        self.combine.cyc1 = CombineCycle {
            rgb_a: ((hi >> 5) & 0xF) as u8,
            rgb_c: (hi & 0x1F) as u8,
            rgb_b: ((lo >> 24) & 0xF) as u8,
            rgb_d: ((lo >> 6) & 0x7) as u8,
            a_a: ((lo >> 21) & 0x7) as u8,
            a_c: ((lo >> 18) & 0x7) as u8,
            a_b: ((lo >> 3) & 0x7) as u8,
            a_d: (lo & 0x7) as u8,
        };
    }

    /// Evaluate the colour combiner for one cycle, returning the RGBA8888 output.
    ///
    /// Muxes the [`CombinerInputs`] by the cycle's selects into `(A − B) * C + D`
    /// per channel (`combine_channel`), clamps to `[0, 255]` (`clamp_9bit`), and
    /// does the same for alpha. The RGB and alpha combiners use different
    /// input tables (N64brew *…/Commands* §0x3C). Exotic inputs (noise, LOD frac,
    /// key/convert constants) are **open residual R-10** and read as zero.
    #[must_use]
    pub fn combine_cycle(cfg: CombineCycle, inp: &CombinerInputs) -> [u8; 4] {
        // RGB: A/B share the muladd/mulsub table, C the wide mul table, D the add.
        let mut out = [0u8; 4];
        for (ch, o) in out.iter_mut().enumerate().take(3) {
            let a = i32::from(rgb_input_a(cfg.rgb_a, inp, ch));
            let b = i32::from(rgb_input_b(cfg.rgb_b, inp, ch));
            let c = i32::from(rgb_input_c(cfg.rgb_c, inp, ch));
            let d = i32::from(rgb_input_d(cfg.rgb_d, inp, ch));
            *o = clamp_9bit(combine_channel(a, b, c, d));
        }
        // Alpha: A/B/D share one 3-bit table; C its own.
        let a = i32::from(alpha_input_abd(cfg.a_a, inp));
        let b = i32::from(alpha_input_abd(cfg.a_b, inp));
        let c = i32::from(alpha_input_c(cfg.a_c, inp));
        let d = i32::from(alpha_input_abd(cfg.a_d, inp));
        out[3] = clamp_9bit(combine_channel(a, b, c, d));
        out
    }

    /// Evaluate the whole combiner for a pixel: cycle 1 alone in 1-cycle mode, or
    /// cycle 0 feeding cycle 1's `Combined` input in 2-cycle mode. `two_cycle`
    /// comes from `Set Other Modes` (T-33-003).
    #[must_use]
    pub fn combine(&self, mut inp: CombinerInputs, two_cycle: bool) -> [u8; 4] {
        if two_cycle {
            inp.combined = Self::combine_cycle(self.combine.cyc0, &inp);
        }
        Self::combine_cycle(self.combine.cyc1, &inp)
    }

    /// Decode `Set Other Modes` (0x2F) into [`OtherModes`]. The blend selects and
    /// the cycle-control / Z / coverage flags all live in this one word; only the
    /// subset the blender and cycle control consume today is used, but the full
    /// layout is decoded so nothing silently reads as its `Default` (N64brew
    /// *…/Commands* §0x2F, cross-checked against parallel-rdp `rdp_common.hpp`).
    ///
    /// `hi` is command bits 63:32 and `lo` bits 31:0. The blend selects pack two
    /// cycles into `lo` bits 31:16 (`P0 P1 A0 A1 M0 M1 B0 B1`, MSB-first, 2 bits
    /// each); the cycle type is `hi` bits 53:52.
    fn set_other_modes(&mut self, hi: u32, lo: u32) {
        self.other_modes = OtherModes {
            cycle_type: ((hi >> 20) & 0x3) as u8,
            blend: [
                BlendCycle {
                    p: ((lo >> 30) & 0x3) as u8,
                    a: ((lo >> 26) & 0x3) as u8,
                    m: ((lo >> 22) & 0x3) as u8,
                    b: ((lo >> 18) & 0x3) as u8,
                },
                BlendCycle {
                    p: ((lo >> 28) & 0x3) as u8,
                    a: ((lo >> 24) & 0x3) as u8,
                    m: ((lo >> 20) & 0x3) as u8,
                    b: ((lo >> 16) & 0x3) as u8,
                },
            ],
            force_blend: (lo >> 14) & 1 != 0,
            image_read_en: (lo >> 6) & 1 != 0,
            cvg_dest: ((lo >> 8) & 0x3) as u8,
            z_compare_en: (lo >> 4) & 1 != 0,
            z_update_en: (lo >> 5) & 1 != 0,
            z_mode: ((lo >> 10) & 0x3) as u8,
            alpha_compare_en: lo & 1 != 0,
            persp_tex_en: (hi >> 19) & 1 != 0, // command bit 51
            aa_enable: (lo >> 3) & 1 != 0,     // command bit 3
        };
    }

    /// Evaluate one blender cycle: `(P * a0 + M * (a1 + 1)) >> 5`, the divide-free
    /// form the hardware uses whenever the result is not an anti-aliased edge
    /// (N64brew *…/Blender*; parallel-rdp `shaders/blender.h`). `a0 = A >> 3` and
    /// `a1 = B >> 3` map the 8-bit alpha selects to the 5-bit blend weights, and
    /// the `+ 1` on the `M` term is real hardware, not a rounding fudge.
    ///
    /// The colour selects (`P`, `M`) pick an RGB triple; the alpha selects (`A`,
    /// `B`) pick a scalar weight — `B`'s `1 − A` case complements the resolved `A`
    /// weight, so `A` is computed first and handed to `blend_b_input`. The result
    /// is masked to 8 bits, **not** clamped: the reference casts through `u8` and
    /// re-masks (`blender.h:142`), so an over-range blend wraps exactly as hardware
    /// does — software is expected to keep `a0 + a1 + 1 ≈ 32`.
    ///
    /// The final-cycle early-return fast paths (opaque passthrough, `color_on_cvg`),
    /// the anti-aliased divider path, alpha-compare, dither, and Z are **open
    /// residual R-11**; `blend_cycle` always takes the no-divide branch for now.
    #[must_use]
    pub fn blend_cycle(cycle: BlendCycle, inp: &BlendInputs) -> [u8; 3] {
        let p = blend_rgb_input(cycle.p, inp);
        let m = blend_rgb_input(cycle.m, inp);
        let a0_full = blend_a_input(cycle.a, inp);
        let a0 = u32::from(a0_full >> 3);
        let a1 = u32::from(blend_b_input(cycle.b, inp, a0_full) >> 3);
        let mut out = [0u8; 3];
        for (ch, o) in out.iter_mut().enumerate() {
            let blended = u32::from(p[ch]) * a0 + u32::from(m[ch]) * (a1 + 1);
            *o = ((blended >> 5) & 0xFF) as u8;
        }
        out
    }

    /// Evaluate the whole blender for a pixel: blend cycle 0 alone in 1-cycle
    /// mode, or cycle 0's RGB fed back as the pixel colour into cycle 1 in
    /// 2-cycle mode (N64brew *…/Blender*).
    ///
    /// Only `pixel.rgb` chains between cycles — **`pixel.a` is deliberately left
    /// unchanged**, so both cycles' `A`/`B` alpha selects see the original combiner
    /// alpha. This matches the reference, which reassigns `pixel_color.rgb` only
    /// before the second `blender()` call (parallel-rdp `memory_interfacing.h:536`);
    /// the blender produces no alpha of its own (`blender.h` returns `u8x3`).
    ///
    /// **Precondition: only valid for cycle types 0 (1-cycle) and 1 (2-cycle).** Copy
    /// (2) and Fill (3) bypass the blender on hardware — the pixel comes straight from
    /// the texel copy / fill register — so the pixel pipeline (T-33-004) must gate on
    /// `cycle_type` and not route those modes through here. This method is not given a
    /// fabricated Copy/Fill result, because the honest contract is "not called", not
    /// "called and returns something"; for cycle type 0 it correctly runs cycle 0 once.
    #[must_use]
    pub fn blend(&self, mut inp: BlendInputs) -> [u8; 3] {
        if self.other_modes.cycle_type == 1 {
            let rgb0 = Self::blend_cycle(self.other_modes.blend[0], &inp);
            inp.pixel[0] = rgb0[0];
            inp.pixel[1] = rgb0[1];
            inp.pixel[2] = rgb0[2];
            return Self::blend_cycle(self.other_modes.blend[1], &inp);
        }
        Self::blend_cycle(self.other_modes.blend[0], &inp)
    }

    /// The per-pixel depth test and coverage/blend derivation — a faithful port of
    /// ParaLLEl-RDP's `depth_test.h` (the Angrylion-parity reference).
    ///
    /// `z`/`dz` are this pixel's decompressed 18-bit depth and its raw delta;
    /// `dz_compressed` is `dz`'s 4-bit `log2`; `coverage_count` is this pixel's
    /// span coverage. [`DepthInputs`] carries the Z-buffer read and the render-mode
    /// flags. When `z_compare` is off the pixel always passes; otherwise the four Z
    /// modes (opaque/interpenetrating/transparent/decal) apply, with the
    /// coplanar/precision-factor handling of the stored `dz`. Interpenetrating mode
    /// can *reduce* the returned `coverage_count`. No buffer is touched here — the
    /// caller (the pixel pipeline, PR-B) reads/writes the Z buffer and applies the
    /// result; today this has no runtime caller, so the oracle is unchanged.
    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "dz_compressed is clamped to 0..=0xf up front, so both blend_shift \
                  branches (clamp(0,4) and min(0xf - dz_compressed, 4)) yield 0..=4 \
                  before the u8 cast — the DepthResult invariant holds"
    )]
    #[allow(
        clippy::similar_names,
        reason = "memory_z / memory_dz / dz are the oracle's own names (depth_test.h)"
    )]
    pub fn depth_test(
        z: i32,
        dz: i32,
        dz_compressed: i32,
        mut coverage_count: i32,
        inp: &DepthInputs,
    ) -> DepthResult {
        let depth_pass;
        let blend_en;
        let coverage_wrap;
        let mut blend_shift = [0u8; 2];
        // Sanitize every input to its hardware domain up front, so the shifts and
        // sums below are bounded for any `i32`/`u8` a caller passes (`depth_test` is
        // public and the pipeline that will call it clamps identically — the oracle
        // clamps `z` to the 18-bit range in `clamping.h`, and `current_dz`/
        // `current_depth`/`dz_compressed` are 4-/14-/4-bit storage fields).
        let z = z.clamp(0, 0x3_FFFF); // 18-bit UNORM depth
        let dz = dz.clamp(0, 0x3_FFFF); // depth-range delta (non-negative)
        let dz_compressed = dz_compressed.clamp(0, 0xf); // 4-bit log2, as `0xf - …` assumes
        let current_depth = inp.current_depth & 0x3FFF; // 14-bit stored z
        let current_dz = i32::from(inp.current_dz & 0xf); // 4-bit stored dz

        if inp.z_compare {
            let memory_z = z_decompress(current_depth);
            let mut memory_dz = dz_decompress(current_dz);
            let precision_factor = (i32::from(current_depth) >> 11) & 0xf;
            let mut coplanar = false;

            blend_shift[0] = (dz_compressed - current_dz).clamp(0, 4) as u8;
            blend_shift[1] = (current_dz - dz_compressed).clamp(0, 4) as u8;

            if precision_factor < 3 {
                if memory_dz == 0x8000 {
                    coplanar = true;
                    memory_dz = 0xffff;
                } else {
                    memory_dz = (memory_dz << 1).max(16 >> precision_factor);
                }
            }

            let mut combined_dz = combine_dz(dz | memory_dz);
            let combined_dz_interpenetrate = combined_dz;
            combined_dz <<= 3;

            let farther = coplanar || (z + combined_dz) >= memory_z;
            let overflow = (coverage_count + inp.current_coverage) >= 8;

            blend_en = inp.force_blend || (!overflow && inp.aa_enable && farther);
            coverage_wrap = overflow;

            let max_z = memory_z == 0x3_FFFF;
            let front = z < memory_z;
            let nearer = coplanar || (z - combined_dz) <= memory_z;
            let opaque_pass = max_z || if overflow { front } else { nearer };

            depth_pass = match inp.z_mode {
                // Interpenetrating: a decal-like intersect modifies coverage; else
                // it falls back to the opaque less-than test.
                1 if front && farther && overflow => {
                    let ip = dz_compress(combined_dz_interpenetrate & 0xffff);
                    let cvg_coeff = ((memory_z >> ip) - (z >> ip)) & 0xf;
                    coverage_count = ((cvg_coeff * coverage_count) >> 3).min(8);
                    true
                }
                0 | 1 => opaque_pass,
                2 => front || max_z,              // transparent
                _ => farther && nearer && !max_z, // decal (3)
            };
        } else {
            blend_shift[1] = (0xf - dz_compressed).min(4) as u8;
            let overflow = (coverage_count + inp.current_coverage) >= 8;
            blend_en = inp.force_blend || (!overflow && inp.aa_enable);
            coverage_wrap = overflow;
            depth_pass = true;
        }

        DepthResult {
            depth_pass,
            blend_en,
            coverage_wrap,
            blend_shift,
            coverage_count,
        }
    }

    /// Byte address of the 16-bit Z-buffer entry for pixel `(x, y)`: entries are
    /// 16-bit, based at `z_image`. The row stride reuses `color_image_width` — the
    /// N64 RDP addresses the depth buffer with the *colour* buffer's width (there is
    /// no separate depth-image width register), so the two buffers share a geometry.
    fn zbuffer_addr(&self, x: u32, y: u32) -> u32 {
        let index = y
            .wrapping_mul(u32::from(self.color_image_width))
            .wrapping_add(x);
        self.z_image.wrapping_add(index.wrapping_mul(2))
    }

    /// Read the Z-buffer entry at `(x, y)` as `(compressed_z, dz)`.
    ///
    /// The 16-bit halfword holds the 14-bit compressed `z` in bits 15:2 and the
    /// **high** two bits of the 4-bit `dz` in bits 1:0; the **low** two bits of
    /// `dz` come from the RDRAM hidden bits — matching ParaLLEl-RDP's
    /// `load_vram_depth`. `dz` is returned as the 4-bit value `0..=15`.
    #[must_use]
    pub fn zbuffer_read<B: VideoBus>(&self, x: u32, y: u32, bus: &B) -> (u16, u8) {
        let addr = self.zbuffer_addr(x, y);
        let word = (u16::from(bus.rdram_read(addr)) << 8)
            | u16::from(bus.rdram_read(addr.wrapping_add(1)));
        let hidden = bus.rdram_read_hidden(addr) & 0x3;
        let compressed_z = word >> 2;
        let dz = (((word & 0x3) as u8) << 2) | hidden;
        (compressed_z, dz)
    }

    /// Write the Z-buffer entry at `(x, y)`: compress the 18-bit `z`, pack it with
    /// `dz`'s high two bits into the halfword, and store `dz`'s low two bits in the
    /// RDRAM hidden bits — matching ParaLLEl-RDP's `store_vram_depth`. `dz` is the
    /// 4-bit compressed delta (`0..=15`); `z` is the 18-bit UNORM depth.
    pub fn zbuffer_write<B: VideoBus>(&self, x: u32, y: u32, z: i32, dz: u8, bus: &mut B) {
        let addr = self.zbuffer_addr(x, y);
        let dz = dz & 0xf;
        let word = (z_compress(z) << 2) | u16::from(dz >> 2);
        let bytes = word.to_be_bytes();
        bus.rdram_write(addr, bytes[0]);
        bus.rdram_write(addr.wrapping_add(1), bytes[1]);
        bus.rdram_write_hidden(addr, dz & 0x3);
    }

    /// Apply a `Set Tile` (0x35): decode the descriptor at `index` (bits 26:24)
    /// from the command word. Pure state — no texel is moved (that is the load
    /// commands). Field layout: format 55:53, size 52:51, line 49:41, TMEM
    /// address 40:32; then per-axis clamp/mirror/mask/shift with T in bits 19:10
    /// and S in bits 9:0 (N64brew *…/Commands* §0x35).
    fn set_tile(&mut self, hi: u32, lo: u32) {
        let index = ((lo >> 24) & 0x7) as usize;
        self.tiles[index] = TileDescriptor {
            format: ((hi >> 21) & 0x7) as u8,
            size: ((hi >> 19) & 0x3) as u8,
            line: ((hi >> 9) & 0x1FF) as u16,
            tmem_addr: (hi & 0x1FF) as u16,
            palette: ((lo >> 20) & 0xF) as u8,
            clamp_t: (lo >> 19) & 1 != 0,
            mirror_t: (lo >> 18) & 1 != 0,
            mask_t: ((lo >> 14) & 0xF) as u8,
            shift_t: ((lo >> 10) & 0xF) as u8,
            clamp_s: (lo >> 9) & 1 != 0,
            mirror_s: (lo >> 8) & 1 != 0,
            mask_s: ((lo >> 4) & 0xF) as u8,
            shift_s: (lo & 0xF) as u8,
            // Set Tile does not touch the tile-size coords; preserve them.
            ..self.tiles[index]
        };
    }

    /// Apply a `Set Tile Size` (0x32): the clamp/mask/mirror extents for the
    /// descriptor at `index` (bits 26:24). Upper-left `SL`/`TL` in bits 55:44 /
    /// 43:32, lower-right `SH`/`TH` in bits 23:12 / 11:0, all `u10.2` (N64brew
    /// *…/Commands* §0x32).
    fn set_tile_size(&mut self, hi: u32, lo: u32) {
        let index = ((lo >> 24) & 0x7) as usize;
        let tile = &mut self.tiles[index];
        tile.sl = ((hi >> 12) & 0xFFF) as u16;
        tile.tl = (hi & 0xFFF) as u16;
        tile.sh = ((lo >> 12) & 0xFFF) as u16;
        tile.th = (lo & 0xFFF) as u16;
    }

    /// Write one byte into TMEM, allocating the 4 KiB backing box on first use.
    ///
    /// `offset` is a byte address, masked into the 4 KiB space (loads past the end
    /// wrap to the start — N64brew *…/Commands* §Load Tile). This is the single
    /// allocation seam the load commands share, so `get_or_insert_with` is not
    /// repeated per command.
    fn tmem_write(&mut self, offset: usize, byte: u8) {
        let tmem = self
            .tmem
            .get_or_insert_with(|| alloc::boxed::Box::new([0u8; TMEM_SIZE]));
        tmem[offset & (TMEM_SIZE - 1)] = byte;
    }

    /// Apply a `Load Tile` (0x34): copy a rectangle of texels from the current
    /// texture image in RDRAM into the tile's TMEM region, then update the tile
    /// size for rendering. `SL`/`TL`/`SH`/`TH` (bits 55:44 / 43:32 / 23:12 / 11:0)
    /// are `u10.2`; the `.2` fraction is floored and the span is **inclusive**
    /// (`SH − SL + 1` texels per row). Rows advance by the tile's `line` stride.
    ///
    /// The TMEM byte placement mirrors the sampler exactly: an **odd-row 32-bit
    /// word swap** (`dst ^= (t & 1) << 2`) applies to every size, and **32-bit
    /// RGBA is split** — R,G into the low half of TMEM, B,A into the high half,
    /// stepping two bytes per texel and masking to `0x7FF`. Both are the read-side
    /// layout in the ParaLLEl-RDP reference (MIT); see `docs/rdp.md` §TMEM loads.
    ///
    /// Scope: 8/16/32-bit texels (`size` 1/2/3). 4-bit (`size` 0) loading needs
    /// nibble addressing and lands with the CI4/I4 decoders (T-32-003); an
    /// unsupported size writes nothing (**open residual R-7**).
    fn load_tile<B: VideoBus>(&mut self, hi: u32, lo: u32, bus: &B) {
        let index = ((lo >> 24) & 0x7) as usize;
        let sl = ((hi >> 12) & 0xFFF) >> 2;
        let tl = (hi & 0xFFF) >> 2;
        let sh = ((lo >> 12) & 0xFFF) >> 2;
        let th = (lo & 0xFFF) >> 2;
        // Reject a degenerate/inverted range the same way as every other unsupported
        // path (write nothing) rather than letting the `& 0xFFF` wrap produce a large
        // bogus width/height: `sh < sl` would otherwise iterate ~4095 texels of
        // garbage. A well-formed load has SL <= SH and TL <= TH.
        if th < tl || sh < sl {
            return;
        }
        let width = (sh - sl + 1) & 0xFFF;
        let height = th - tl + 1;
        if width == 0 {
            return;
        }
        let tile = self.tiles[index];
        let Some(dst_bpt) = bytes_per_texel(tile.size) else {
            return; // 4-bit (or unmapped) not loaded here — R-7.
        };
        // The texture image should match the tile size (documented hazard). A 4-bit
        // (or unmapped) source size has no byte stride, so bail rather than fall back
        // to the tile stride and read out of bounds — R-7.
        let Some(src_bpt) = bytes_per_texel(self.tex_image_size) else {
            return;
        };
        let split = tile.size == 3; // 32-bit RGBA uses the split TMEM layout.
        let tmem_base = u32::from(tile.tmem_addr) * 8;
        let stride = u32::from(tile.line) * 8;
        let tex_w = u32::from(self.tex_image_width);
        for t in 0..height {
            let swap = (t & 1) << 2;
            for s in 0..width {
                let src_pixel = (tl + t) * tex_w + (sl + s);
                let src = self.tex_image_addr.wrapping_add(src_pixel * src_bpt);
                if split {
                    // R,G -> low half; B,A -> high half (offset by 0x800). The swap is
                    // applied per final byte so it stays correct regardless of alignment.
                    let bo = (tmem_base + stride * t + s * 2) & 0x7FF;
                    self.tmem_write((bo ^ swap) as usize, bus.rdram_read(src));
                    self.tmem_write(
                        ((bo + 1) ^ swap) as usize,
                        bus.rdram_read(src.wrapping_add(1)),
                    );
                    self.tmem_write(
                        ((bo + 0x800) ^ swap) as usize,
                        bus.rdram_read(src.wrapping_add(2)),
                    );
                    self.tmem_write(
                        ((bo + 0x801) ^ swap) as usize,
                        bus.rdram_read(src.wrapping_add(3)),
                    );
                } else {
                    let base = tmem_base + stride * t + s * dst_bpt;
                    for i in 0..dst_bpt {
                        // XOR the swap into each final byte address, not the base.
                        self.tmem_write(
                            ((base + i) ^ swap) as usize,
                            bus.rdram_read(src.wrapping_add(i)),
                        );
                    }
                }
            }
        }
        // Load Tile updates the descriptor's tile size for rendering.
        let tile = &mut self.tiles[index];
        tile.sl = ((hi >> 12) & 0xFFF) as u16;
        tile.tl = (hi & 0xFFF) as u16;
        tile.sh = ((lo >> 12) & 0xFFF) as u16;
        tile.th = (lo & 0xFFF) as u16;
    }

    /// Apply a `Load Block` (0x33): stream a linear run of texels from the current
    /// texture image into the tile's TMEM region. `SL`/`SH` (bits 55:44 / 23:12)
    /// are `u12.0` integer texels; `SH − SL + 1` is the count (**inclusive**), and
    /// a count over [`LOAD_BLOCK_MAX_TEXELS`] writes nothing. The low field
    /// (bits 11:0) is **`dxt`** (`u1.11`): a running counter `T = (word * dxt) >>
    /// 11` over each 64-bit TMEM word decides line parity, and an odd line swaps
    /// that word's two 32-bit halves (`dst ^= 4`).
    ///
    /// Scope: 8/16-bit texels (`size` 1/2). The 32-bit split path and 4-bit are
    /// deferred (**open residual R-7**); an unsupported size writes nothing.
    fn load_block<B: VideoBus>(&mut self, hi: u32, lo: u32, bus: &B) {
        let index = ((lo >> 24) & 0x7) as usize;
        let slo = (hi >> 12) & 0xFFF;
        let tlo = hi & 0xFFF;
        let shi = (lo >> 12) & 0xFFF;
        let dxt = lo & 0xFFF;
        // An inverted range writes nothing (as in `load_tile`); without this an
        // extreme `shi < slo` (slo >= 2049) wraps into a valid-looking count.
        if shi < slo {
            return;
        }
        let count = (shi - slo + 1) & 0xFFF;
        if count == 0 || count > LOAD_BLOCK_MAX_TEXELS {
            return; // over the limit: nothing written (§Load Block).
        }
        let tile = self.tiles[index];
        let Some(bpt) = bytes_per_texel(tile.size) else {
            return;
        };
        if tile.size == 3 {
            return; // 32-bit block load (split) deferred — R-7.
        }
        // A 4-bit (or unmapped) source size has no byte stride — bail rather than
        // fall back to the tile stride and read out of bounds (R-7).
        let Some(src_bpt) = bytes_per_texel(self.tex_image_size) else {
            return;
        };
        let tex_w = u32::from(self.tex_image_width);
        let src_base = self
            .tex_image_addr
            .wrapping_add((tex_w * tlo + slo) * src_bpt);
        let tmem_base = u32::from(tile.tmem_addr) * 8;
        for s in 0..count {
            let src = src_base.wrapping_add(s * src_bpt);
            let byte_off = s * bpt;
            // Line parity from the dxt counter over 64-bit TMEM words.
            let word = byte_off / 8;
            let line = (word * dxt) >> 11;
            let swap = (line & 1) << 2;
            let base = tmem_base + byte_off;
            for i in 0..bpt {
                // XOR the swap into each final byte address, not the base.
                self.tmem_write(
                    ((base + i) ^ swap) as usize,
                    bus.rdram_read(src.wrapping_add(i)),
                );
            }
        }
    }

    /// Apply a `Load TLUT` (0x30): load a palette into TMEM. Each 16-bit entry
    /// from the (16-bit) texture image is **quadrupled** — written to four
    /// adjacent `u16` slots — so entry `i` occupies 8 bytes at TMEM byte
    /// `tmem_addr*8 + i*8`. The count is inclusive (`(SH>>2) − (SL>>2) + 1`).
    ///
    /// The destination is wherever the tile's `tmem_addr` points; a correct
    /// program sets it into the upper 2 KiB (byte >= 0x800), aligned to 128 bytes
    /// (N64brew *…/Commands* §Load TLUT). That is a **programmer requirement, not
    /// a hardware rejection** — the ParaLLEl-RDP reference writes to the addressed
    /// location and the sampler reads the palette from the upper half, so a
    /// misplaced TLUT is simply not found rather than refused. Enforcing a
    /// rejection here would invent behaviour the hardware does not have.
    fn load_tlut<B: VideoBus>(&mut self, hi: u32, lo: u32, bus: &B) {
        let sl = ((hi >> 12) & 0xFFF) >> 2;
        let sh = ((lo >> 12) & 0xFFF) >> 2;
        let index = ((lo >> 24) & 0x7) as usize;
        if sh < sl {
            return;
        }
        let count = sh - sl + 1;
        let tmem_base = u32::from(self.tiles[index].tmem_addr) * 8;
        let src_base = self.tex_image_addr.wrapping_add(sl * 2); // 16-bit source
        for i in 0..count {
            let src = src_base.wrapping_add(i * 2);
            let hi_b = bus.rdram_read(src);
            let lo_b = bus.rdram_read(src.wrapping_add(1));
            let dst = tmem_base + i * 8;
            for k in 0..4u32 {
                let slot = (dst + k * 2) as usize;
                self.tmem_write(slot, hi_b);
                self.tmem_write(slot + 1, lo_b);
            }
        }
        // Load TLUT also updates the tile size (like the other loads).
        let tile = &mut self.tiles[index];
        tile.sl = ((hi >> 12) & 0xFFF) as u16;
        tile.tl = (hi & 0xFFF) as u16;
        tile.sh = ((lo >> 12) & 0xFFF) as u16;
        tile.th = (lo & 0xFFF) as u16;
    }

    /// Sample one texel from `tile` at tile-relative integer coords `(s, t)`,
    /// returning RGBA8888. The fetch half of the texture pipeline; the
    /// clamp/mirror/mask/shift wrapper and the filter/combiner are T-32-004 /
    /// Sprint 3. Decodes every listed texel format (RGBA16/32, IA16/8/4, I8/4,
    /// CI8/4 via the TLUT), matched to the ParaLLEl-RDP read layout.
    ///
    /// TMEM is read as a natural big-endian byte array with the odd-row
    /// 32-bit-word swap `^= (t & 1) << 2` — the same convention the loads use, so
    /// the endian twiddles ParaLLEl-RDP applies to its host-word storage are
    /// intentionally absent here too. An unsupported format/size is transparent
    /// black.
    #[must_use]
    pub fn fetch_texel(&self, tile: &TileDescriptor, s: u32, t: u32) -> [u8; 4] {
        let swap = (t & 1) << 2;
        // Wrapping arithmetic end to end: an oversized/unclipped `s` or `t` must
        // not debug-panic on overflow before the TMEM mask applies — a panic would
        // break the determinism contract (ADR 0004). Every offset below is masked
        // into the 4 KiB space by `tmem_byte`. The per-column byte offsets for the
        // three texel widths (16/32-bit, 8-bit, 4-bit-nibble):
        let base = u32::from(tile.tmem_addr)
            .wrapping_mul(8)
            .wrapping_add(u32::from(tile.line).wrapping_mul(8).wrapping_mul(t));
        let off16 = base.wrapping_add(s.wrapping_mul(2));
        let off8 = base.wrapping_add(s);
        let off4 = base.wrapping_add(s >> 1);
        match (tile.format, tile.size) {
            (0, 2) => decode_rgba16(self.tmem_u16(off16 ^ swap)), // RGBA16
            (0, 3) => {
                // RGBA32 split: R,G low half; B,A high half.
                let bo = (off16 & 0x7FF) ^ swap;
                [
                    self.tmem_byte(bo as usize),
                    self.tmem_byte(bo.wrapping_add(1) as usize),
                    self.tmem_byte(bo.wrapping_add(TMEM_HIGH) as usize),
                    self.tmem_byte(bo.wrapping_add(TMEM_HIGH + 1) as usize),
                ]
            }
            (3, 2) => {
                // IA16: I high byte, A low byte.
                let w = self.tmem_u16(off16 ^ swap);
                let i = (w >> 8) as u8;
                [i, i, i, (w & 0xFF) as u8]
            }
            (3, 1) => {
                // IA8: I high nibble, A low nibble (each 4->8).
                let byte = self.tmem_byte((off8 ^ swap) as usize);
                let i = widen4(u32::from(byte) >> 4);
                [i, i, i, widen4(u32::from(byte) & 0xF)]
            }
            (3, 0) => {
                // IA4: I top 3 bits (3->8), A bottom bit.
                let nib = self.nibble_at((off4 ^ swap) as usize, s);
                let i = widen3(u32::from(nib) >> 1);
                [i, i, i, if nib & 1 != 0 { 0xFF } else { 0 }]
            }
            (4, 1) => {
                // I8: intensity in all channels, alpha = intensity.
                let v = self.tmem_byte((off8 ^ swap) as usize);
                [v, v, v, v]
            }
            (4, 0) => {
                // I4: 4-bit intensity (4->8), alpha = intensity.
                let v = widen4(u32::from(self.nibble_at((off4 ^ swap) as usize, s)));
                [v, v, v, v]
            }
            (2, 1) => {
                // CI8: 8-bit index into the TLUT.
                let ci = self.tmem_byte(((off8 & 0x7FF) ^ swap) as usize);
                self.tlut_lookup(u32::from(ci))
            }
            (2, 0) => {
                // CI4: 4-bit index + tile.palette as the high nibble.
                let nib = self.nibble_at(((off4 & 0x7FF) ^ swap) as usize, s);
                // `palette` is already 4-bit from `set_tile` decode; mask defensively
                // so a directly-constructed descriptor cannot push `ci` out of range.
                let ci = u32::from(nib) | (u32::from(tile.palette & 0xF) << 4);
                self.tlut_lookup(ci)
            }
            _ => [0, 0, 0, 0],
        }
    }

    /// Read a big-endian `u16` from TMEM at byte offset `b` (both bytes masked
    /// into the 4 KiB space; `b + 1` wraps rather than overflowing).
    fn tmem_u16(&self, b: u32) -> u32 {
        (u32::from(self.tmem_byte(b as usize)) << 8)
            | u32::from(self.tmem_byte(b.wrapping_add(1) as usize))
    }

    /// Select the 4-bit nibble of the TMEM byte at `byte_off` for texel column
    /// `s`: the high nibble for even `s`, the low nibble for odd `s`.
    fn nibble_at(&self, byte_off: usize, s: u32) -> u8 {
        let byte = self.tmem_byte(byte_off);
        (byte >> ((!s & 1) * 4)) & 0xF
    }

    /// Look up a TLUT entry by 8-bit index `ci` and decode it as RGBA5551.
    ///
    /// Entry `ci` is the quadrupled 16-bit word at TMEM byte `0x800 + ci*8` (the
    /// four copies are identical after `Load TLUT`, so the first is read). The
    /// `IA16` TLUT type (Other Modes `tlut_type = 1`) is deferred; RGBA16 is
    /// assumed.
    fn tlut_lookup(&self, ci: u32) -> [u8; 4] {
        decode_rgba16(self.tmem_u16(TMEM_HIGH.wrapping_add(ci.wrapping_mul(8))))
    }

    /// Read one byte of TMEM.
    ///
    /// `offset` is a **byte** address (0..[`TMEM_SIZE`]), masked into the 4 KiB
    /// space — *not* the 64-bit-word address that `Set Tile`'s `tmem_addr` /
    /// `line` use; a word address must be multiplied by 8 first (word 0x100 =
    /// byte 0x800). An unwritten (lazily-unallocated) TMEM reads as zero.
    #[must_use]
    pub fn tmem_byte(&self, offset: usize) -> u8 {
        self.tmem
            .as_ref()
            .map_or(0, |t| t[offset & (TMEM_SIZE - 1)])
    }
}

/// Returns the crate version string.
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    struct NullBus;
    impl RdramBus for NullBus {
        fn rdram_read(&self, _addr: u32) -> u8 {
            0
        }
        fn rdram_write(&mut self, _addr: u32, _val: u8) {}
    }
    impl VideoBus for NullBus {}

    #[test]
    fn empty_fifo_tick_is_noop() {
        let mut rdp = Rdp::new();
        let mut bus = NullBus;
        rdp.tick(&mut bus);
        assert_eq!(rdp.cmd_current, 0);
    }

    #[test]
    fn version_is_non_empty() {
        assert!(!version().is_empty());
    }

    /// **`DPC_STATUS` writes are set/clear commands.** `SET_FREEZE` (0x8) raises
    /// FREEZE; `CLEAR_FREEZE` (0x4) drops it. n64-systemtest's `RDP START & END
    /// REG` freezes the DP precisely so it can poke the registers.
    #[test]
    fn status_write_sets_and_clears_freeze() {
        let mut rdp = Rdp::new();
        rdp.dpc_write(3, 0x8); // SET_FREEZE
        assert_ne!(rdp.dpc_read(3) & DP_STATUS_FREEZE, 0, "freeze set");
        rdp.dpc_write(3, 0x4); // CLEAR_FREEZE
        assert_eq!(rdp.dpc_read(3) & DP_STATUS_FREEZE, 0, "freeze cleared");
    }

    /// **`DPC_START`/`END` mask to a 24-bit, 8-aligned address**, and writing
    /// `END` copies the latched start into `CURRENT`.
    #[test]
    fn start_end_mask_and_current_follows_start() {
        let mut rdp = Rdp::new();
        rdp.dpc_write(3, 0x8); // freeze
        rdp.dpc_write(0, 0x12FF_FFFF); // START
        rdp.dpc_write(1, 0x12FF_FFFF); // END
        assert_eq!(rdp.dpc_read(0), 0x00FF_FFF8, "START masked");
        assert_eq!(rdp.dpc_read(1), 0x00FF_FFF8, "END masked");
        assert_eq!(rdp.dpc_read(2), 0x00FF_FFF8, "CURRENT = START after END");
    }

    /// **The `START_VALID` double-latch.** Writing START sets `START_VALID`; a
    /// second write while valid is *ignored*; writing END consumes it (clears
    /// `START_VALID`, leaves `END_VALID` clear while frozen). This is the exact
    /// sequence `RSP STATUS: start-valid` walks.
    #[test]
    fn start_valid_latch_ignores_a_second_start_write() {
        let mut rdp = Rdp::new();
        rdp.dpc_write(3, 0x8); // freeze
        assert_eq!(rdp.dpc_read(3) & DP_STATUS_START_VALID, 0, "clear at entry");

        rdp.dpc_write(0, 0x1238); // START
        assert_ne!(
            rdp.dpc_read(3) & DP_STATUS_START_VALID,
            0,
            "set after write"
        );
        assert_eq!(rdp.dpc_read(0), 0x1238);

        rdp.dpc_write(0, 0x12_3450); // ignored while valid
        assert_eq!(rdp.dpc_read(0), 0x1238, "second START write ignored");

        rdp.dpc_write(1, 0x1238); // END consumes the latch
        assert_eq!(rdp.dpc_read(3) & DP_STATUS_START_VALID, 0, "cleared by END");
        assert_eq!(
            rdp.dpc_read(3) & DP_STATUS_END_VALID,
            0,
            "END_VALID clear while frozen"
        );
        assert_eq!(rdp.dpc_read(2), 0x1238, "CURRENT = START");
    }

    /// **An END-only write is an incremental transfer: `CURRENT` is not
    /// rewound.** With `START_VALID` clear (the first transfer already
    /// consumed), writing a new END extends the buffer from where the DMA
    /// stopped — reloading `CURRENT` from `START` would reprocess commands
    /// already transferred (N64brew *Interface*, `DPC_END`: "If `START_PENDING`
    /// is 0, the write is considered an incremental transfer").
    #[test]
    fn an_end_only_write_extends_without_rewinding_current() {
        let mut rdp = Rdp::new();
        rdp.dpc_write(3, 0x8); // freeze
        rdp.dpc_write(0, 0x1000); // START
        rdp.dpc_write(1, 0x1000); // END consumes START -> START_VALID clear
        rdp.cmd_current = 0x1000; // pretend the transfer reached the end

        rdp.dpc_write(1, 0x1040); // incremental END, no new START
        assert_eq!(rdp.dpc_read(1), 0x1040, "END extended");
        assert_eq!(rdp.dpc_read(2), 0x1000, "CURRENT not rewound to START");
    }

    /// **A frozen DP does not advance the FIFO**, so registers stay put even
    /// with `cmd_current < cmd_end`.
    #[test]
    fn a_frozen_dp_does_not_tick() {
        let mut rdp = Rdp::new();
        rdp.status = DP_STATUS_FREEZE;
        rdp.cmd_current = 0x10;
        rdp.cmd_end = 0x40;
        let mut bus = NullBus;
        rdp.tick(&mut bus);
        assert_eq!(rdp.cmd_current, 0x10, "frozen: CURRENT unchanged");
    }

    /// A bus backed by a byte buffer, so the decoder can walk a real command
    /// list out of "RDRAM" and we can assert the pointer lands exactly on
    /// `DPC_END`.
    struct SliceBus {
        mem: Vec<u8>,
        dp_raised: bool,
    }
    impl RdramBus for SliceBus {
        fn rdram_read(&self, addr: u32) -> u8 {
            self.mem.get(addr as usize).copied().unwrap_or(0)
        }
        fn rdram_write(&mut self, addr: u32, val: u8) {
            if let Some(b) = self.mem.get_mut(addr as usize) {
                *b = val;
            }
        }
    }
    impl VideoBus for SliceBus {
        fn raise_dp_interrupt(&mut self) {
            self.dp_raised = true;
        }
    }

    /// Append a command: its opcode in bits 61:56 of the first word, then
    /// `words` total 64-bit words with the remainder zero-filled. The word count
    /// is supplied **explicitly by the caller**, independent of the production
    /// decoder, so a walk over the buffer is a genuine check of
    /// `command_len_words` rather than a tautology built from it.
    fn push_cmd(buf: &mut Vec<u8>, opcode: u8, words: u32) {
        buf.extend_from_slice(&(u32::from(opcode) << 24).to_be_bytes());
        for _ in 4..words * 8 {
            buf.push(0);
        }
    }

    /// **The decoder consumes every command whole and never desyncs.** A mixed
    /// list exercising all three length classes — a 1-word set-state, a 22-word
    /// shade+texture+z triangle, a no-op, a 2-word texture rectangle, and
    /// `Sync Full` — drains one command per tick and lands `DPC_CURRENT` exactly
    /// on `DPC_END`. The expected lengths are stated here from the N64brew
    /// command map, so a wrong decoder length overshoots or stops short.
    #[test]
    fn decoder_consumes_each_command_whole_without_desync() {
        // (opcode, documented 64-bit-word length) — independent of the decoder.
        let fixtures = [
            (0x3F_u8, 1), // Set Color Image
            (0x0F, 22),   // Fill Triangle (STZ) = shade + texture + z
            (0x00, 1),    // No Operation
            (0x24, 2),    // Texture Rectangle
            (0x29, 1),    // Sync Full
        ];
        let mut mem = Vec::new();
        for &(op, words) in &fixtures {
            push_cmd(&mut mem, op, words);
        }
        let total = u32::try_from(mem.len()).unwrap();
        let mut bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.cmd_end = total;

        let mut ticks = 0u32;
        while rdp.cmd_current < rdp.cmd_end && ticks < 1000 {
            rdp.tick(&mut bus);
            ticks += 1;
        }
        assert_eq!(rdp.cmd_current, total, "consumed exactly to DPC_END");
        assert_eq!(ticks, 5, "one command retired per scheduler tick");
        assert_eq!(rdp.commands_processed, 5, "every command counted");
    }

    /// **A multi-word primitive is consumed in a single tick**, by its full
    /// decoded length — an unimplemented command advances the FIFO past all its
    /// words rather than treating each word as a fresh command.
    #[test]
    fn a_multiword_command_is_consumed_in_one_tick() {
        let mut mem = Vec::new();
        push_cmd(&mut mem, 0x0E, 20); // Fill Triangle (ST) = shade + texture: 20 words
        let mut bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.cmd_end = 20 * 8;
        rdp.tick(&mut bus);
        assert_eq!(rdp.cmd_current, 20 * 8, "whole 20-word triangle at once");
        assert_eq!(rdp.commands_processed, 1);
    }

    /// **A partially-written command is not consumed until it is complete.** If
    /// `DPC_END` lands mid-command — as it does while the `rdpq` microcode fills
    /// the buffer and advances `DPC_END` incrementally — the decoder stalls
    /// rather than executing against unwritten RDRAM, then consumes the command
    /// whole once the rest of its words arrive.
    #[test]
    fn a_partial_command_is_not_consumed_until_complete() {
        let mut mem = Vec::new();
        push_cmd(&mut mem, 0x0F, 22); // 22-word triangle
        let mut bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.cmd_end = 10 * 8; // DPC_END only reached word 10 of 22
        rdp.tick(&mut bus);
        assert_eq!(rdp.cmd_current, 0, "stalled: partial command not consumed");
        assert_eq!(rdp.commands_processed, 0);

        rdp.cmd_end = 22 * 8; // the rest of the command arrives
        rdp.tick(&mut bus);
        assert_eq!(rdp.cmd_current, 22 * 8, "consumed whole once complete");
        assert_eq!(rdp.commands_processed, 1);
    }

    /// **XBUS mode reads commands from DMEM, which is not yet wired**, so the
    /// decoder must not mis-read RDRAM as the command stream. With `XBUS` set it
    /// stalls, leaving `DPC_CURRENT` and the counter untouched.
    #[test]
    fn xbus_mode_does_not_decode_rdram() {
        let mut mem = Vec::new();
        push_cmd(&mut mem, 0x3F, 1);
        let mut bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.status = DP_STATUS_XBUS;
        rdp.cmd_end = 8;
        rdp.tick(&mut bus);
        assert_eq!(rdp.cmd_current, 0, "XBUS: RDRAM not decoded");
        assert_eq!(rdp.commands_processed, 0);
    }

    /// Drive a single command through `tick` and return the resulting state.
    fn run_one(opcode: u8) -> (Rdp, SliceBus) {
        let mut mem = Vec::new();
        push_cmd(&mut mem, opcode, 1);
        let mut bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.cmd_end = 8;
        rdp.tick(&mut bus);
        (rdp, bus)
    }

    /// **`Sync Full` (0x29) raises the DP interrupt.** The dispatcher calls
    /// `raise_dp_interrupt` on the bus, which the live `Bus` turns into
    /// `MI_INTR.dp`; here the test bus records the raise.
    #[test]
    fn sync_full_raises_the_dp_interrupt() {
        let (rdp, bus) = run_one(OP_SYNC_FULL);
        assert!(bus.dp_raised, "Sync Full raised the DP interrupt");
        assert_eq!(rdp.commands_processed, 1);
        assert_eq!(rdp.stall, 0, "Sync Full does not stall the pipeline itself");
    }

    /// **The other sync commands do not raise an interrupt** — only `Sync Full`
    /// does. They each set the documented fixed pipeline stall instead.
    #[test]
    fn sync_load_pipe_tile_set_the_documented_stall() {
        for (opcode, expected) in [
            (OP_SYNC_LOAD, SYNC_LOAD_GCLK),
            (OP_SYNC_PIPE, SYNC_PIPE_GCLK),
            (OP_SYNC_TILE, SYNC_TILE_GCLK),
        ] {
            let (rdp, bus) = run_one(opcode);
            assert!(!bus.dp_raised, "opcode {opcode:#04x} raised no interrupt");
            assert_eq!(rdp.stall, expected, "opcode {opcode:#04x} stall cycles");
        }
    }

    /// **A sync stall holds the FIFO for exactly its GCLK count.** After a
    /// `Sync Pipe` (50 GCLK) the next command is not consumed until 50 further
    /// ticks have elapsed — the pipeline is unavailable for exactly that long,
    /// as the command is an unconditional fixed-length stall.
    #[test]
    fn a_sync_pipe_stall_holds_the_fifo_for_50_gclk() {
        let mut mem = Vec::new();
        push_cmd(&mut mem, OP_SYNC_PIPE, 1); // sets stall = 50
        push_cmd(&mut mem, 0x00, 1); // a following no-op
        let mut bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.cmd_end = 16;

        rdp.tick(&mut bus); // consumes Sync Pipe, sets stall = 50
        assert_eq!(rdp.commands_processed, 1);
        assert_eq!(rdp.stall, SYNC_PIPE_GCLK);

        // The next 50 ticks burn the stall and do not advance the FIFO.
        for i in 0..SYNC_PIPE_GCLK {
            rdp.tick(&mut bus);
            assert_eq!(rdp.commands_processed, 1, "still stalled at tick {i}");
            assert_eq!(rdp.stall, SYNC_PIPE_GCLK - 1 - i);
        }
        // Stall expired: the following command is consumed on the next tick.
        rdp.tick(&mut bus);
        assert_eq!(rdp.commands_processed, 2, "FIFO resumes after the stall");
    }

    /// **A frozen DP does not burn stall cycles.** The freeze guard is checked
    /// before the stall countdown, so a non-zero `stall` is held — not
    /// decremented — while frozen, and resumes counting down only once the DP is
    /// unfrozen. The plain `a_frozen_dp_does_not_tick` test leaves `stall` at
    /// zero and so cannot catch a regression that decremented it under freeze.
    #[test]
    fn a_frozen_dp_holds_its_stall_countdown() {
        let mut rdp = Rdp::new();
        let mut bus = NullBus;
        rdp.stall = 10;
        rdp.status = DP_STATUS_FREEZE;
        rdp.tick(&mut bus);
        assert_eq!(rdp.stall, 10, "frozen: stall countdown held, not burned");

        rdp.status = 0; // unfreeze
        rdp.tick(&mut bus);
        assert_eq!(rdp.stall, 9, "unfrozen: countdown resumes");
    }

    /// **A preceding stall delays the `Sync Full` interrupt.** With `Sync Pipe`
    /// (50 GCLK) queued before `Sync Full`, the DP interrupt stays low for all
    /// 50 stall ticks and rises only once the stall drains and `Sync Full` is
    /// dispatched — the stall-before-interrupt ordering the dispatch doc claims.
    /// (Were the stall gate absent, `Sync Full` would dispatch on the very next
    /// tick and the interrupt would rise during the loop.)
    #[test]
    fn a_preceding_stall_delays_the_sync_full_interrupt() {
        let mut mem = Vec::new();
        push_cmd(&mut mem, OP_SYNC_PIPE, 1);
        push_cmd(&mut mem, OP_SYNC_FULL, 1);
        let mut bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.cmd_end = 16;

        rdp.tick(&mut bus); // consume Sync Pipe -> stall = 50
        assert_eq!(rdp.stall, SYNC_PIPE_GCLK);
        assert!(!bus.dp_raised, "no interrupt while the stall is set");

        for i in 0..SYNC_PIPE_GCLK {
            rdp.tick(&mut bus);
            assert!(!bus.dp_raised, "interrupt still low during stall tick {i}");
        }
        // Stall drained: the next tick dispatches Sync Full and raises.
        rdp.tick(&mut bus);
        assert!(
            bus.dp_raised,
            "interrupt raised only after the stall drains"
        );
        assert_eq!(rdp.commands_processed, 2);
    }

    // --- The FILL pipeline (T-31-003) ---

    // The command list lives here; the color image is based at RDRAM 0, well
    // below it, so the two never overlap in the shared test buffer.
    const CMD_BASE: u32 = 0x4000;

    fn push_word(buf: &mut Vec<u8>, hi: u32, lo: u32) {
        buf.extend_from_slice(&hi.to_be_bytes());
        buf.extend_from_slice(&lo.to_be_bytes());
    }

    // Command builders. Screen coordinates are given in whole pixels; the wire
    // format is u10.2, so each is shifted left by two.
    fn set_color_image(format: u32, size: u32, width: u32, addr: u32) -> (u32, u32) {
        let hi =
            (u32::from(OP_SET_COLOR_IMAGE) << 24) | (format << 21) | (size << 19) | (width - 1);
        (hi, addr)
    }
    fn set_fill_color(color: u32) -> (u32, u32) {
        (u32::from(OP_SET_FILL_COLOR) << 24, color)
    }
    fn set_scissor(ulx: u32, uly: u32, lrx: u32, lry: u32) -> (u32, u32) {
        let hi = (u32::from(OP_SET_SCISSOR) << 24) | (ulx << 14) | (uly << 2);
        let lo = (lrx << 14) | (lry << 2);
        (hi, lo)
    }
    fn fill_rect(ulx: u32, uly: u32, lrx: u32, lry: u32) -> (u32, u32) {
        let hi = (u32::from(OP_FILL_RECTANGLE) << 24) | (lrx << 14) | (lry << 2);
        let lo = (ulx << 14) | (uly << 2);
        (hi, lo)
    }

    /// Run a command list through the FIFO (color image at RDRAM 0, commands at
    /// `CMD_BASE`) and return the RDP plus the memory the fill wrote into.
    fn run_commands(words: &[(u32, u32)]) -> (Rdp, SliceBus) {
        let mut mem = alloc::vec![0u8; CMD_BASE as usize + words.len() * 8];
        let mut list = Vec::new();
        for &(hi, lo) in words {
            push_word(&mut list, hi, lo);
        }
        mem[CMD_BASE as usize..CMD_BASE as usize + list.len()].copy_from_slice(&list);
        let mut bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.cmd_current = CMD_BASE;
        rdp.cmd_end = CMD_BASE + u32::try_from(list.len()).unwrap();
        let mut guard = 0;
        while rdp.cmd_current < rdp.cmd_end && guard < 10_000 {
            rdp.tick(&mut bus);
            guard += 1;
        }
        (rdp, bus)
    }

    /// **`Set Color Image` parses format, size, width, and address.** Width is
    /// the encoded field plus one; the address is masked to 24 bits.
    #[test]
    fn set_color_image_parses_its_fields() {
        let (rdp, _) = run_commands(&[set_color_image(0, 3, 320, 0x0010_0000)]);
        assert_eq!(rdp.color_image_format, 0);
        assert_eq!(rdp.color_image_size, 3);
        assert_eq!(rdp.color_image_width, 320);
        assert_eq!(rdp.color_image, 0x0010_0000);
    }

    /// **`Set Fill Color` and `Set Scissor` store their values.**
    #[test]
    fn set_fill_color_and_scissor_store_state() {
        let (rdp, _) = run_commands(&[set_fill_color(0xDEAD_BEEF), set_scissor(2, 3, 6, 7)]);
        assert_eq!(rdp.fill_color, 0xDEAD_BEEF);
        assert_eq!(rdp.scissor_ulx, 2 << 2);
        assert_eq!(rdp.scissor_uly, 3 << 2);
        assert_eq!(rdp.scissor_lrx, 6 << 2);
        assert_eq!(rdp.scissor_lry, 7 << 2);
    }

    /// **A 32-bit FILL writes the whole colour to every pixel**, four bytes
    /// each, big-endian — the memory is the fill value repeated verbatim.
    #[test]
    fn fill_rectangle_32bpp_writes_the_colour_verbatim() {
        let (_, bus) = run_commands(&[
            set_color_image(0, 3, 4, 0), // 32-bit, width 4, base 0
            set_fill_color(0xAABB_CCDD),
            set_scissor(0, 0, 4, 2),
            fill_rect(0, 0, 4, 2),
        ]);
        // 4 px * 2 rows * 4 bytes = 32 bytes, all AA BB CC DD.
        for chunk in bus.mem[0..32].chunks_exact(4) {
            assert_eq!(chunk, [0xAA, 0xBB, 0xCC, 0xDD]);
        }
        assert_eq!(bus.mem[32], 0, "nothing written past the rectangle");
    }

    /// **A 16-bit FILL alternates the colour's halves per pixel** — even pixels
    /// take the upper 16 bits, odd pixels the lower — so memory is still the
    /// 32-bit value repeated.
    #[test]
    fn fill_rectangle_16bpp_alternates_halves() {
        let (_, bus) = run_commands(&[
            set_color_image(0, 2, 4, 0), // 16-bit, width 4
            set_fill_color(0xAABB_CCDD),
            set_scissor(0, 0, 4, 1),
            fill_rect(0, 0, 4, 1),
        ]);
        // px0 even -> AABB, px1 odd -> CCDD, px2 -> AABB, px3 -> CCDD.
        assert_eq!(
            bus.mem[0..8],
            [0xAA, 0xBB, 0xCC, 0xDD, 0xAA, 0xBB, 0xCC, 0xDD]
        );
    }

    /// **An 8-bit FILL writes one of the four colour bytes per pixel**, cycling
    /// every four pixels.
    #[test]
    fn fill_rectangle_8bpp_cycles_four_bytes() {
        let (_, bus) = run_commands(&[
            set_color_image(4, 1, 4, 0), // 8-bit (I8), width 4
            set_fill_color(0xAABB_CCDD),
            set_scissor(0, 0, 4, 1),
            fill_rect(0, 0, 4, 1),
        ]);
        assert_eq!(bus.mem[0..4], [0xAA, 0xBB, 0xCC, 0xDD]);
    }

    /// **The scissor clips the fill on all four edges.** A rectangle larger than
    /// the scissor only writes the scissored region; the right and lower edges
    /// are exclusive, so the boundary pixels just outside stay clear.
    #[test]
    fn fill_rectangle_is_clipped_to_the_scissor() {
        // 32-bit, width 8. Scissor keeps x in [2,6), y in [1,3).
        let (_, bus) = run_commands(&[
            set_color_image(0, 3, 8, 0),
            set_fill_color(0x1122_3344),
            set_scissor(2, 1, 6, 3),
            fill_rect(0, 0, 8, 4), // larger than the scissor on every side
        ]);
        let px = |x: u32, y: u32| {
            let a = (y * 8 + x) as usize * 4;
            &bus.mem[a..a + 4]
        };
        // Inside the scissor: written.
        assert_eq!(px(2, 1), [0x11, 0x22, 0x33, 0x44], "inside top-left");
        assert_eq!(px(5, 2), [0x11, 0x22, 0x33, 0x44], "inside bottom-right");
        // Outside each edge: clear.
        assert_eq!(px(1, 1), [0, 0, 0, 0], "left of scissor");
        assert_eq!(px(6, 1), [0, 0, 0, 0], "right edge exclusive");
        assert_eq!(px(2, 0), [0, 0, 0, 0], "above scissor");
        assert_eq!(px(2, 3), [0, 0, 0, 0], "lower edge exclusive");
    }

    /// **A 4-bit color image is not a FILL target** — the real RDP crashes, so
    /// the fill is skipped and no memory is written.
    #[test]
    fn fill_rectangle_4bit_target_writes_nothing() {
        let (_, bus) = run_commands(&[
            set_color_image(0, 0, 4, 0), // 4-bit
            set_fill_color(0xFFFF_FFFF),
            set_scissor(0, 0, 4, 2),
            fill_rect(0, 0, 4, 2),
        ]);
        assert!(bus.mem[0..16].iter().all(|&b| b == 0), "no fill at 4-bit");
    }

    /// **Degenerate and empty rectangles write nothing.** An inverted rectangle
    /// (`ulx > lrx`), an inverted scissor (`ul* > lr*`), and a rectangle disjoint
    /// from the scissor each yield an empty pixel span, so no pixel is written.
    /// The span emptiness — not the early-return, which is a redundant fast path
    /// over Rust's empty `for` ranges — is what these assert.
    #[test]
    fn fill_rectangle_degenerate_bounds_write_nothing() {
        // Inverted rectangle: upper-left past lower-right.
        let (_, bus) = run_commands(&[
            set_color_image(0, 3, 8, 0),
            set_fill_color(0xFFFF_FFFF),
            set_scissor(0, 0, 8, 4),
            fill_rect(6, 3, 2, 1), // ulx>lrx, uly>lry
        ]);
        assert!(bus.mem[0..128].iter().all(|&b| b == 0), "inverted rect");

        // Inverted scissor: upper-left past lower-right.
        let (_, bus) = run_commands(&[
            set_color_image(0, 3, 8, 0),
            set_fill_color(0xFFFF_FFFF),
            set_scissor(6, 3, 2, 1), // sulx>slrx, suly>slry
            fill_rect(0, 0, 8, 4),
        ]);
        assert!(bus.mem[0..128].iter().all(|&b| b == 0), "inverted scissor");

        // Rectangle entirely to the left of the scissor: empty intersection.
        let (_, bus) = run_commands(&[
            set_color_image(0, 3, 8, 0),
            set_fill_color(0xFFFF_FFFF),
            set_scissor(4, 0, 8, 4),
            fill_rect(0, 0, 3, 4), // rx1 = 3 <= scissor sx0 = 4
        ]);
        assert!(bus.mem[0..128].iter().all(|&b| b == 0), "disjoint rect");
    }

    /// **A fill with no configured width writes nothing.** `color_image_width`
    /// is 0 only before `Set Color Image` (which always yields field + 1 ≥ 1);
    /// with a valid pixel size but zero width the guard skips the fill rather
    /// than smearing every row onto offset 0 with a zero stride. Reached here by
    /// setting the state directly, since the command stream cannot produce it.
    #[test]
    fn fill_rectangle_without_a_valid_width_writes_nothing() {
        let mut mem = alloc::vec![0u8; CMD_BASE as usize + 8];
        let (hi, lo) = fill_rect(0, 0, 4, 4);
        mem[CMD_BASE as usize..CMD_BASE as usize + 4].copy_from_slice(&hi.to_be_bytes());
        mem[CMD_BASE as usize + 4..CMD_BASE as usize + 8].copy_from_slice(&lo.to_be_bytes());
        let mut bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.color_image_size = 3; // valid 32-bit size, but width left at 0
        rdp.fill_color = 0xFFFF_FFFF;
        rdp.scissor_lrx = 4 << 2;
        rdp.scissor_lry = 4 << 2;
        rdp.cmd_current = CMD_BASE;
        rdp.cmd_end = CMD_BASE + 8;
        rdp.tick(&mut bus);
        assert!(bus.mem[0..64].iter().all(|&b| b == 0), "width 0: no write");
    }

    // ---- T-32-001: texture state (TMEM, tile descriptors, state commands) ----

    /// **TMEM is zero at power-on and unallocated.** The lazy `Option<Box<..>>`
    /// reads as all-zero while `None`, so a fresh RDP sees a blank TMEM without
    /// having paid a 4 KiB allocation.
    #[test]
    fn tmem_is_zero_and_unallocated_at_power_on() {
        let rdp = Rdp::new();
        assert!(rdp.tmem.is_none(), "no TMEM box allocated at power-on");
        assert_eq!(rdp.tmem_byte(0), 0);
        assert_eq!(rdp.tmem_byte(0x800), 0, "high half zero too");
        assert_eq!(rdp.tmem_byte(TMEM_SIZE - 1), 0, "last byte zero");
    }

    /// **`Set Tile` (0x35) decodes every field into the addressed descriptor.**
    /// Each field is seeded with a distinct value so a swapped bit range shows
    /// up as a wrong field, not a coincidental match. Word built here from the
    /// N64brew field table, independent of the decoder.
    #[test]
    fn set_tile_decodes_all_fields() {
        // format=3 size=2 line=0x1F addr=0x100 | index=5 palette=0xA
        // clamp_t=1 mirror_t=0 mask_t=7 shift_t=3 | clamp_s=0 mirror_s=1
        // mask_s=5 shift_s=9
        let mut rdp = Rdp::new();
        rdp.set_tile(0x3570_3F00, 0x05A9_CD59);
        let expected = TileDescriptor {
            format: 3,
            size: 2,
            line: 0x1F,
            tmem_addr: 0x100,
            palette: 0xA,
            clamp_t: true,
            mirror_t: false,
            mask_t: 7,
            shift_t: 3,
            clamp_s: false,
            mirror_s: true,
            mask_s: 5,
            shift_s: 9,
            sl: 0,
            tl: 0,
            sh: 0,
            th: 0,
        };
        assert_eq!(rdp.tiles[5], expected, "descriptor 5 fully decoded");
        // Only the addressed descriptor is touched.
        for (i, t) in rdp.tiles.iter().enumerate() {
            if i != 5 {
                assert_eq!(*t, TileDescriptor::default(), "tile {i} untouched");
            }
        }
    }

    /// **`Set Tile Size` (0x32) decodes SL/TL/SH/TH for the addressed
    /// descriptor** and leaves the format/addressing fields alone.
    #[test]
    fn set_tile_size_decodes_coords() {
        let mut rdp = Rdp::new();
        // Seed descriptor 2's addressing fields to non-zero first, so the
        // preservation check is real: if Set Tile Size wrongly cleared them this
        // catches it, whereas asserting `== 0` on a fresh descriptor could not.
        rdp.set_tile(0x3570_3F00, 0x02A9_CD59); // index 2: format=3 size=2 line=0x1F addr=0x100
        // SL=0x123 TL=0x045 | index=2 SH=0x678 TH=0x0AB
        rdp.set_tile_size(0x3212_3045, 0x0267_80AB);
        let t = rdp.tiles[2];
        assert_eq!(
            (t.sl, t.tl, t.sh, t.th),
            (0x123, 0x045, 0x678, 0x0AB),
            "coords updated"
        );
        assert_eq!(t.format, 3, "Set Tile Size preserves format");
        assert_eq!(t.size, 2, "preserves size");
        assert_eq!(t.line, 0x1F, "preserves line");
        assert_eq!(t.tmem_addr, 0x100, "preserves tmem_addr");
    }

    /// **`Set Tile` preserves the tile-size coords** — the two commands write
    /// disjoint parts of the same descriptor, so a `Set Tile` after a `Set Tile
    /// Size` must not clear SL/TL/SH/TH (the `..self.tiles[index]` spread).
    #[test]
    fn set_tile_preserves_tile_size_coords() {
        let mut rdp = Rdp::new();
        rdp.set_tile_size(0x3212_3045, 0x0267_80AB); // seeds tiles[2] coords
        rdp.set_tile(0x3570_3F00, 0x02A9_CD59); // index bits -> descriptor 2
        let t = rdp.tiles[2];
        assert_eq!(t.format, 3, "Set Tile applied");
        assert_eq!(
            (t.sl, t.tl, t.sh, t.th),
            (0x123, 0x045, 0x678, 0x0AB),
            "tile-size coords survive a later Set Tile"
        );
    }

    /// **`Set Texture Image` (0x3D) decodes format/size/width/addr.** Same field
    /// layout as Set Color Image; width is a field+1 pixel count.
    #[test]
    fn set_texture_image_decodes_fields() {
        // format=4 size=1 width_field=0x13F (-> 0x140) addr=0x654321
        let mut rdp = Rdp::new();
        let mut bus = NullBus;
        rdp.dispatch(OP_SET_TEXTURE_IMAGE, 0x3D88_013F, 0x0065_4321, 0, &mut bus);
        assert_eq!(rdp.tex_image_format, 4);
        assert_eq!(rdp.tex_image_size, 1);
        assert_eq!(rdp.tex_image_width, 0x140, "width is field + 1");
        assert_eq!(rdp.tex_image_addr, 0x0065_4321);
    }

    /// **The dispatcher routes 0x35 and 0x32 to the right handlers.** The
    /// field-level tests call `set_tile` / `set_tile_size` directly; this drives
    /// the actual `dispatch` entry point, so a mis-wired opcode arm (0x35 sent to
    /// the size handler, or 0x32 to the tile handler) is caught, not only a
    /// decode bug. The inputs distinguish the two: `Set Tile` sets `format = 3`
    /// which `Set Tile Size` must leave alone, and `Set Tile Size` sets coords
    /// that `Set Tile` does not.
    #[test]
    fn dispatch_routes_set_tile_and_set_tile_size() {
        let mut rdp = Rdp::new();
        let mut bus = NullBus;
        rdp.dispatch(OP_SET_TILE, 0x3570_3F00, 0x02A9_CD59, 0, &mut bus); // index 2
        assert_eq!(rdp.tiles[2].format, 3, "0x35 routed to set_tile");
        assert_eq!(rdp.tiles[2].line, 0x1F, "and decoded its fields");
        rdp.dispatch(OP_SET_TILE_SIZE, 0x3212_3045, 0x0267_80AB, 0, &mut bus); // index 2
        assert_eq!(
            (rdp.tiles[2].sl, rdp.tiles[2].th),
            (0x123, 0x0AB),
            "0x32 routed to set_tile_size"
        );
        assert_eq!(
            rdp.tiles[2].format, 3,
            "set_tile_size did not clobber the tile format (distinct routing)"
        );
    }

    // ---- T-32-002: TMEM loads (Load Tile 0x34, Load Block 0x33) ----

    /// **`Load Tile` copies a 16-bit row and latches the tile size.** Row 0 is
    /// even, so no odd-row swap applies; the four texels land verbatim, and the
    /// descriptor's `SL/TL/SH/TH` are updated for rendering.
    #[test]
    fn load_tile_16bit_copies_a_row_and_sets_size() {
        let mut mem = alloc::vec![0u8; 0x200];
        let src = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
        mem[0x100..0x108].copy_from_slice(&src);
        let bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.tiles[0].size = 2; // 16-bit
        rdp.tiles[0].line = 1; // 8 bytes/row
        rdp.tex_image_size = 2;
        rdp.tex_image_width = 4;
        rdp.tex_image_addr = 0x100;
        // SL=0 TL=0 index=0 SH=3 (field 0xC) TH=0.
        rdp.load_tile(0x0000_0000, 0x0000_C000, &bus);
        for (i, &b) in src.iter().enumerate() {
            assert_eq!(rdp.tmem_byte(i), b, "tmem[{i}]");
        }
        assert_eq!(
            (rdp.tiles[0].sl, rdp.tiles[0].sh),
            (0, 0xC),
            "Load Tile latches the tile size"
        );
    }

    /// **`Load Tile` applies the odd-row 32-bit-word swap.** Row 1 is odd, so each
    /// texel's TMEM byte address gains bit 2 (a `^ 4`), swapping the two 32-bit
    /// halves of the 64-bit word, while row 0 is unswapped.
    #[test]
    fn load_tile_swaps_odd_rows() {
        let mut mem = alloc::vec![0u8; 0x200];
        let row0 = [0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7];
        let row1 = [0xB0, 0xB1, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7];
        mem[0x100..0x108].copy_from_slice(&row0);
        mem[0x108..0x110].copy_from_slice(&row1);
        let bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.tiles[0].size = 2;
        rdp.tiles[0].line = 1;
        rdp.tex_image_size = 2;
        rdp.tex_image_width = 4;
        rdp.tex_image_addr = 0x100;
        // SL=0 TL=0 SH=3 TH=1 (field 4) -> 4x2.
        rdp.load_tile(0x0000_0000, 0x0000_C004, &bus);
        for (i, &b) in row0.iter().enumerate() {
            assert_eq!(rdp.tmem_byte(i), b, "row 0 verbatim [{i}]");
        }
        // Row 1 base = line*8 = 8; texel s at (8 + s*2) ^ 4.
        assert_eq!((rdp.tmem_byte(0xC), rdp.tmem_byte(0xD)), (0xB0, 0xB1));
        assert_eq!((rdp.tmem_byte(0xE), rdp.tmem_byte(0xF)), (0xB2, 0xB3));
        assert_eq!((rdp.tmem_byte(0x8), rdp.tmem_byte(0x9)), (0xB4, 0xB5));
        assert_eq!((rdp.tmem_byte(0xA), rdp.tmem_byte(0xB)), (0xB6, 0xB7));
    }

    /// **`Load Tile` splits a 32-bit texel across TMEM.** R,G go to the low half
    /// and B,A to the high half (offset 0x800) — the documented 32-bit layout.
    #[test]
    fn load_tile_32bit_splits_rg_low_ba_high() {
        let mut mem = alloc::vec![0u8; 0x200];
        mem[0x100..0x104].copy_from_slice(&[0x11, 0x22, 0x33, 0x44]); // R G B A
        let bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.tiles[0].size = 3; // 32-bit RGBA
        rdp.tiles[0].line = 1;
        rdp.tex_image_size = 3;
        rdp.tex_image_width = 1;
        rdp.tex_image_addr = 0x100;
        rdp.load_tile(0x0000_0000, 0x0000_0000, &bus); // 1x1
        assert_eq!(rdp.tmem_byte(0), 0x11, "R low half");
        assert_eq!(rdp.tmem_byte(1), 0x22, "G low half");
        assert_eq!(rdp.tmem_byte(0x800), 0x33, "B high half");
        assert_eq!(rdp.tmem_byte(0x801), 0x44, "A high half");
    }

    /// **`Load Block` streams texels and pins both sides of the 2048 limit.** A
    /// small 8-bit block copies verbatim (dxt 0 → one line, no swap); a 2049-texel
    /// load writes nothing, while exactly 2048 loads.
    #[test]
    fn load_block_streams_and_enforces_the_limit() {
        let mut mem = alloc::vec![0u8; 0x200];
        mem[0x100..0x104].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        let bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.tiles[0].size = 1; // 8-bit
        rdp.tex_image_size = 1;
        rdp.tex_image_width = 4;
        rdp.tex_image_addr = 0x100;
        // SL=0 index=0 SH=3 dxt=0.
        rdp.load_block(0x0000_0000, 0x0000_3000, &bus);
        for (i, &b) in [0xDE, 0xAD, 0xBE, 0xEF].iter().enumerate() {
            assert_eq!(rdp.tmem_byte(i), b, "block[{i}]");
        }

        // Over the limit (count = 2049): nothing written / allocated.
        let mut over = Rdp::new();
        over.tiles[0].size = 1;
        over.tex_image_size = 1;
        over.tex_image_width = 4096;
        over.tex_image_addr = 0x100;
        over.load_block(0x0000_0000, 0x0080_0000, &bus); // SH field 2048
        assert!(
            over.tmem.is_none(),
            "a 2049-texel load writes nothing (over the 2048 limit)"
        );

        // Exactly at the limit (count = 2048): loads.
        let mut edge = Rdp::new();
        edge.tiles[0].size = 1;
        edge.tex_image_size = 1;
        edge.tex_image_width = 4096;
        edge.tex_image_addr = 0x100;
        edge.load_block(0x0000_0000, 0x007F_F000, &bus); // SH field 2047 -> 2048 texels
        assert!(edge.tmem.is_some(), "exactly 2048 texels loads");
        assert_eq!(edge.tmem_byte(0), 0xDE, "and writes the first texel");
    }

    /// **`Load Block` uses dxt to swap odd lines.** With `dxt = 0x800` the line
    /// index `(word * dxt) >> 11` is odd for odd 64-bit words, so the second group
    /// of four 16-bit texels (word 1) is swapped while the first (word 0) is not.
    #[test]
    fn load_block_dxt_swaps_odd_lines() {
        let mut mem = alloc::vec![0u8; 0x200];
        // 8 x 16-bit texels = 16 bytes: word 0 = texels 0..4, word 1 = texels 4..8.
        let data: [u8; 16] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x40, 0x41, 0x42, 0x43, 0x44, 0x45,
            0x46, 0x47,
        ];
        mem[0x100..0x110].copy_from_slice(&data);
        let bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.tiles[0].size = 2; // 16-bit
        rdp.tex_image_size = 2;
        rdp.tex_image_width = 8;
        rdp.tex_image_addr = 0x100;
        // SL=0 index=0 SH=7 (field 7<<... u12.0 so field=7) dxt=0x800.
        rdp.load_block(0x0000_0000, 0x0000_7800, &bus);
        // Word 0 (texels 0..4) unswapped at bytes 0..8.
        for (i, &b) in data[..8].iter().enumerate() {
            assert_eq!(rdp.tmem_byte(i), b, "word0[{i}] unswapped");
        }
        // Word 1 (texels 4..8) swapped: byte_off (8..16) ^ 4.
        for (i, &b) in data[8..].iter().enumerate() {
            assert_eq!(rdp.tmem_byte((8 + i) ^ 4), b, "word1 byte {i} swapped");
        }
    }

    /// **An unsupported (4-bit) *tile* size writes nothing.** Isolates the
    /// destination-size guard: the source is a supported image, so only the
    /// tile-size (`dst_bpt`) guard stops the load. A silent no-op is invisible to a
    /// "does not panic" test, so this asserts the *effect*: TMEM is never allocated.
    #[test]
    fn load_with_unsupported_tile_size_writes_nothing() {
        let mut mem = alloc::vec![0u8; 0x200];
        mem[0x100..0x108].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let bus = SliceBus {
            mem,
            dp_raised: false,
        };
        // 4-bit tile (size 0), supported source: Load Tile writes nothing.
        let mut a = Rdp::new();
        a.tiles[0].size = 0;
        a.tex_image_size = 2;
        a.tex_image_width = 4;
        a.tex_image_addr = 0x100;
        a.load_tile(0x0000_0000, 0x0000_C000, &bus);
        assert!(a.tmem.is_none(), "4-bit tile: Load Tile writes nothing");
        // 4-bit tile: Load Block writes nothing.
        let mut b = Rdp::new();
        b.tiles[0].size = 0;
        b.tex_image_size = 1;
        b.tex_image_width = 4;
        b.tex_image_addr = 0x100;
        b.load_block(0x0000_0000, 0x0000_3000, &bus);
        assert!(b.tmem.is_none(), "4-bit tile: Load Block writes nothing");
    }

    /// **An unsupported (4-bit) *source* size writes nothing — independently.** The
    /// tile size is supported, so only the source-size (`src_bpt`) guard stops the
    /// load; deleting that guard would read RDRAM at the wrong stride and allocate
    /// TMEM, failing this test.
    #[test]
    fn load_with_unsupported_source_size_writes_nothing() {
        let mut mem = alloc::vec![0u8; 0x200];
        mem[0x100..0x108].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut a = Rdp::new();
        a.tiles[0].size = 2; // supported destination
        a.tiles[0].line = 1;
        a.tex_image_size = 0; // 4-bit source (unsupported)
        a.tex_image_width = 4;
        a.tex_image_addr = 0x100;
        a.load_tile(0x0000_0000, 0x0000_C000, &bus);
        assert!(a.tmem.is_none(), "4-bit source: Load Tile writes nothing");
        let mut b = Rdp::new();
        b.tiles[0].size = 1; // supported destination
        b.tex_image_size = 0; // 4-bit source
        b.tex_image_width = 4;
        b.tex_image_addr = 0x100;
        b.load_block(0x0000_0000, 0x0000_3000, &bus);
        assert!(b.tmem.is_none(), "4-bit source: Load Block writes nothing");
    }

    /// **`Load Block` rejects an inverted `SH < SL` range** (like `Load Tile`),
    /// with valid sizes so only the inverted-range guard prevents the load.
    #[test]
    fn load_block_rejects_inverted_range() {
        let mut mem = alloc::vec![0u8; 0x200];
        mem[0x100..0x110].copy_from_slice(&[0xFF; 16]);
        let bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.tiles[0].size = 1;
        rdp.tex_image_size = 1;
        rdp.tex_image_width = 8;
        rdp.tex_image_addr = 0x100;
        // SL=10 (field 0xA000) SH=2 (field 0x2000), u12.0: shi < slo.
        rdp.load_block(0x0000_A000, 0x0000_2000, &bus);
        assert!(rdp.tmem.is_none(), "shi < slo writes nothing");
    }

    /// **The dispatcher routes 0x34 and 0x33 to the load handlers.** The other
    /// load tests call `load_tile` / `load_block` directly; this drives the actual
    /// `dispatch` entry so a removed or mis-wired opcode arm is caught by an
    /// observable TMEM write, not only a decode bug.
    #[test]
    fn dispatch_routes_load_tile_and_load_block() {
        let mut mem = alloc::vec![0u8; 0x200];
        mem[0x100..0x104].copy_from_slice(&[0x11, 0x22, 0x33, 0x44]);
        let mut bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut a = Rdp::new();
        a.tiles[0].size = 2;
        a.tiles[0].line = 1;
        a.tex_image_size = 2;
        a.tex_image_width = 2;
        a.tex_image_addr = 0x100;
        a.dispatch(OP_LOAD_TILE, 0x0000_0000, 0x0000_4000, 0, &mut bus); // SH=1 -> 2 texels
        assert_eq!(
            (a.tmem_byte(0), a.tmem_byte(1)),
            (0x11, 0x22),
            "0x34 routed to load_tile"
        );
        let mut b = Rdp::new();
        b.tiles[0].size = 1;
        b.tex_image_size = 1;
        b.tex_image_width = 4;
        b.tex_image_addr = 0x100;
        b.dispatch(OP_LOAD_BLOCK, 0x0000_0000, 0x0000_3000, 0, &mut bus); // SH=3 -> 4 texels
        assert_eq!(b.tmem_byte(0), 0x11, "0x33 routed to load_block");
    }

    /// **An inverted range writes nothing.** `SH < SL` (like `TH < TL`) is a
    /// degenerate command; without the guard the `& 0xFFF` wrap would iterate a
    /// large bogus width. Asserts the effect: TMEM stays unallocated.
    #[test]
    fn load_tile_rejects_inverted_range() {
        let mut mem = alloc::vec![0u8; 0x200];
        mem[0x100..0x110].copy_from_slice(&[0xFF; 16]);
        let bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.tiles[0].size = 2;
        rdp.tiles[0].line = 1;
        rdp.tex_image_size = 2;
        rdp.tex_image_width = 8;
        rdp.tex_image_addr = 0x100;
        // SL=10 (field 10<<2=0x28) SH=2 (field 2<<2=8): sh < sl.
        rdp.load_tile(0x0002_8000, 0x0000_8000, &bus);
        assert!(
            rdp.tmem.is_none(),
            "sh < sl writes nothing rather than a wrapped-width rectangle"
        );
    }

    // ---- T-32-003: Load TLUT (0x30) and the texel-format decoders ----

    /// Seed a tile descriptor's format/size (and optional palette) for a fetch test.
    fn tile_fmt(format: u8, size: u8) -> TileDescriptor {
        TileDescriptor {
            format,
            size,
            line: 1,
            ..TileDescriptor::default()
        }
    }

    /// **`fetch_texel` decodes every supported format.** Each case seeds TMEM
    /// directly (the load path is tested separately) and asserts the RGBA8888.
    #[test]
    fn fetch_texel_decodes_each_format() {
        let mut rdp = Rdp::new();
        // RGBA16 (5551): 0xF801 = R=31, G=0, B=0, A=1 -> opaque red.
        rdp.tmem_write(0, 0xF8);
        rdp.tmem_write(1, 0x01);
        assert_eq!(
            rdp.fetch_texel(&tile_fmt(0, 2), 0, 0),
            [0xFF, 0, 0, 0xFF],
            "RGBA16"
        );
        // IA16: I=0x80 A=0xFF.
        assert_eq!(
            rdp.fetch_texel(&tile_fmt(3, 2), 0, 0),
            [0xF8, 0xF8, 0xF8, 0x01],
            "IA16 reads the same two bytes as I/A"
        );

        // IA8: byte 0x5A -> I=widen4(5)=0x55, A=widen4(0xA)=0xAA.
        let mut r = Rdp::new();
        r.tmem_write(0, 0x5A);
        assert_eq!(
            r.fetch_texel(&tile_fmt(3, 1), 0, 0),
            [0x55, 0x55, 0x55, 0xAA],
            "IA8"
        );
        // I8: 0x5A in all channels.
        assert_eq!(
            r.fetch_texel(&tile_fmt(4, 1), 0, 0),
            [0x5A, 0x5A, 0x5A, 0x5A],
            "I8 alpha = intensity"
        );

        // IA4: high nibble 0xE (i3=7 -> 0xFF, A = bit0 = 0).
        let mut n = Rdp::new();
        n.tmem_write(0, 0xE5);
        assert_eq!(
            n.fetch_texel(&tile_fmt(3, 0), 0, 0),
            [0xFF, 0xFF, 0xFF, 0],
            "IA4 (high nibble, even s)"
        );
        // I4: high nibble 0xE -> widen4(0xE)=0xEE in all channels.
        assert_eq!(
            n.fetch_texel(&tile_fmt(4, 0), 0, 0),
            [0xEE, 0xEE, 0xEE, 0xEE],
            "I4 alpha = intensity"
        );
        // Odd s selects the low nibble (0x5): I4 -> widen4(5)=0x55.
        assert_eq!(
            n.fetch_texel(&tile_fmt(4, 0), 1, 0),
            [0x55, 0x55, 0x55, 0x55],
            "I4 odd s -> low nibble"
        );
    }

    /// **`fetch_texel` decodes RGBA32 from the split TMEM.** R,G come from the low
    /// half and B,A from the high half (0x800).
    #[test]
    fn fetch_texel_rgba32_reads_the_split() {
        let mut rdp = Rdp::new();
        rdp.tmem_write(0, 0x11); // R
        rdp.tmem_write(1, 0x22); // G
        rdp.tmem_write(0x800, 0x33); // B
        rdp.tmem_write(0x801, 0x44); // A
        assert_eq!(
            rdp.fetch_texel(&tile_fmt(0, 3), 0, 0),
            [0x11, 0x22, 0x33, 0x44]
        );
    }

    /// **`fetch_texel` resolves CI8 and CI4 through the TLUT.** A CI index selects
    /// a quadrupled RGBA16 entry in the high TMEM half; CI4 folds in the tile
    /// palette as the high nibble.
    #[test]
    fn fetch_texel_ci_through_the_tlut() {
        // CI8: index 5 -> TLUT entry at 0x800 + 5*8 = 0x828 = 0xF801 (red).
        let mut rdp = Rdp::new();
        rdp.tmem_write(0, 5); // the index texel
        rdp.tmem_write(0x828, 0xF8);
        rdp.tmem_write(0x829, 0x01);
        assert_eq!(
            rdp.fetch_texel(&tile_fmt(2, 1), 0, 0),
            [0xFF, 0, 0, 0xFF],
            "CI8 -> TLUT red"
        );

        // CI4: nibble 5 (high, even s) + palette 3 -> index 0x35 -> entry at
        // 0x800 + 0x35*8 = 0x9A8 = 0x07C1 (green).
        let mut c = Rdp::new();
        c.tmem_write(0, 0x50); // high nibble 5
        c.tmem_write(0x9A8, 0x07);
        c.tmem_write(0x9A9, 0xC1);
        let mut tile = tile_fmt(2, 0);
        tile.palette = 3;
        assert_eq!(
            c.fetch_texel(&tile, 0, 0),
            [0, 0xFF, 0, 0xFF],
            "CI4 index = nibble | palette<<4"
        );
    }

    /// **`fetch_texel` applies the odd-row swap.** Row 1 reads through the
    /// `^= (t & 1) << 2` twiddle, so the same TMEM contents sample differently on
    /// even vs odd rows — matching how the loads wrote them.
    #[test]
    fn fetch_texel_odd_row_swap() {
        let mut rdp = Rdp::new();
        // I8 tile, line 1 (8 bytes/row). Row 1 texel 0 reads byte (8) ^ 4 = 0xC.
        rdp.tmem_write(0xC, 0x99);
        assert_eq!(
            rdp.fetch_texel(&tile_fmt(4, 1), 0, 1),
            [0x99, 0x99, 0x99, 0x99],
            "odd row samples the swapped byte"
        );
    }

    /// **`Load TLUT` quadruples each entry into the addressed TMEM region** and
    /// updates the tile size. Two 16-bit entries land as four adjacent copies each.
    #[test]
    fn load_tlut_quadruples_entries() {
        let mut mem = alloc::vec![0u8; 0x200];
        mem[0x100..0x104].copy_from_slice(&[0xF8, 0x01, 0x07, 0xC1]); // entry0, entry1
        let bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.tiles[0].tmem_addr = 0x100; // -> byte 0x800 (upper half)
        rdp.tex_image_addr = 0x100;
        // SL=0 SH=1 (field 1<<2=4) -> 2 entries.
        rdp.load_tlut(0x0000_0000, 0x0000_4000, &bus);
        // Entry 0 quadrupled at 0x800..0x808.
        for k in 0..4 {
            assert_eq!(rdp.tmem_byte(0x800 + k * 2), 0xF8, "entry0 copy {k} hi");
            assert_eq!(rdp.tmem_byte(0x801 + k * 2), 0x01, "entry0 copy {k} lo");
        }
        // Entry 1 quadrupled at 0x808..0x810.
        assert_eq!(rdp.tmem_byte(0x808), 0x07);
        assert_eq!(rdp.tmem_byte(0x809), 0xC1);
        assert_eq!(rdp.tmem_byte(0x80E), 0x07, "entry1 4th copy");
        assert_eq!(rdp.tiles[0].sh, 4, "Load TLUT latches the tile size");
    }

    /// **`fetch_texel` never debug-panics on oversized coordinates.** An
    /// unclipped `s`/`t` must wrap into the 4 KiB TMEM space rather than
    /// overflowing (ADR 0004 determinism). In a debug build, non-wrapping
    /// arithmetic here would panic; this exercises every format at `u32::MAX`
    /// coordinates and asserts a deterministic (repeatable) result.
    #[test]
    fn fetch_texel_oversized_coords_wrap_deterministically() {
        let rdp = Rdp::new();
        for &(fmt, size) in &[
            (0, 2),
            (0, 3),
            (3, 2),
            (3, 1),
            (3, 0),
            (4, 1),
            (4, 0),
            (2, 1),
            (2, 0),
        ] {
            let mut tile = tile_fmt(fmt, size);
            tile.line = 0x1FF;
            tile.tmem_addr = 0x1FF;
            tile.palette = 0xF;
            // Must not panic, and must be deterministic for identical inputs.
            let a = rdp.fetch_texel(&tile, u32::MAX, u32::MAX);
            let b = rdp.fetch_texel(&tile, u32::MAX, u32::MAX);
            assert_eq!(a, b, "fmt {fmt} size {size} is deterministic");
        }
    }

    /// **The dispatcher routes 0x30 to `load_tlut`.** Drives the FIFO dispatch
    /// entry so a removed/misrouted arm is caught by an observable TMEM write.
    #[test]
    fn dispatch_routes_load_tlut() {
        let mut mem = alloc::vec![0u8; 0x200];
        mem[0x100..0x102].copy_from_slice(&[0xAB, 0xCD]);
        let mut bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.tiles[0].tmem_addr = 0x100;
        rdp.tex_image_addr = 0x100;
        rdp.dispatch(OP_LOAD_TLUT, 0x0000_0000, 0x0000_0000, 0, &mut bus); // 1 entry
        assert_eq!(rdp.tmem_byte(0x800), 0xAB, "0x30 routed to load_tlut");
        assert_eq!(rdp.tmem_byte(0x801), 0xCD);
    }

    // ---- T-32-004: coordinate wrap + copy-mode Texture Rectangle ----

    /// **`wrap_coord` applies shift, tile-origin subtraction, mirror, and mask.**
    /// The coordinate is `s10.5`; a texel index of `n` is `n << 5`.
    #[test]
    fn wrap_coord_shift_subtract_mirror_mask() {
        // Plain: texel 3 (= 3<<5 = 96), no shift/mask/mirror, SL 0 -> 3.
        assert_eq!(wrap_coord(96, 0, 0, false, 0), 3);
        // Subtract SL: SL=1 (u10.2) -> shifts the origin left by one texel.
        assert_eq!(wrap_coord(96, 0, 0, false, 4), 2, "SL=1 texel subtracted");
        // Mask to 2 bits (mask_s=2 -> wrap every 4 texels): texel 5 -> 1.
        assert_eq!(wrap_coord(5 << 5, 0, 2, false, 0), 1, "masked to 2 bits");
        // Mirror with mask 2: texel 5 is in the odd span [4,7] -> reflects to 2.
        assert_eq!(wrap_coord(5 << 5, 0, 2, true, 0), 2, "mirrored");
        // Right shift (code 1) halves the coordinate before the texel divide.
        assert_eq!(wrap_coord(4 << 5, 1, 0, false, 0), 2, "shift code 1 = >>1");
        // Left shift (code 12 = left by 4): coord 0x10 -> 0x100 -> texel 8.
        assert_eq!(wrap_coord(0x10, 12, 0, false, 0), 8, "shift code 12 = <<4");
        // A negative coordinate stays negative through the left shift.
        assert_eq!(
            wrap_coord(-0x10, 12, 0, false, 0),
            -8,
            "left shift preserves sign"
        );
    }

    /// **A copy-mode Texture Rectangle round-trips a texture.** `Load Tile` loads a
    /// 4×2 16-bit texture into TMEM; a 1:1 `Texture Rectangle` (copy mode) blits it
    /// into a 16-bit colour image. Because the load and the copy fetch share the
    /// odd-row swap, the framebuffer must equal the source texture exactly — the
    /// first textured picture, end to end.
    #[test]
    fn texture_rectangle_copy_round_trips_a_texture() {
        let mut mem = alloc::vec![0u8; 0x400];
        // Source 4x2 16-bit texture at 0x100 (8 distinct texels).
        let tex: [u16; 8] = [
            0x0102, 0x0304, 0x0506, 0x0708, 0x090A, 0x0B0C, 0x0D0E, 0x0F10,
        ];
        for (i, &v) in tex.iter().enumerate() {
            mem[0x100 + i * 2] = (v >> 8) as u8;
            mem[0x100 + i * 2 + 1] = (v & 0xFF) as u8;
        }
        // Texture Rectangle command word 1 at 0x308 (word 0 supplied to dispatch):
        // S=0 T=0 | DsDx=4.0 (0x1000) DtDy=1.0 (0x400) -> a 1:1 blit.
        mem[0x308..0x30C].copy_from_slice(&0u32.to_be_bytes()); // S=0, T=0
        mem[0x30C..0x310].copy_from_slice(&0x1000_0400u32.to_be_bytes());
        let mut bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        // Load the texture into TMEM (tile 0: 16-bit, line 1).
        rdp.tex_image_size = 2;
        rdp.tex_image_width = 4;
        rdp.tex_image_addr = 0x100;
        rdp.tiles[0].size = 2;
        rdp.tiles[0].line = 1;
        rdp.load_tile(0x0000_0000, 0x0000_C004, &bus); // SL0 TL0 SH3 TH1 -> 4x2
        // Colour image: 16-bit, width 4, at 0x200.
        rdp.color_image_size = 2;
        rdp.color_image_width = 4;
        rdp.color_image = 0x200;
        // Scissor covering the 4x2 rect (a real command list always sets it).
        rdp.scissor_lrx = 4 << 2;
        rdp.scissor_lry = 2 << 2;
        // Texture Rectangle: word0 XL=3<<2 (0xC), YL=1<<2 (4), tile 0, XH=0, YH=0.
        rdp.dispatch(
            OP_TEXTURE_RECTANGLE,
            0x0000_C004,
            0x0000_0000,
            0x300,
            &mut bus,
        );
        // The colour image equals the source texture, texel for texel.
        for (i, &v) in tex.iter().enumerate() {
            let hi = bus.mem[0x200 + i * 2];
            let lo = bus.mem[0x200 + i * 2 + 1];
            assert_eq!(
                u16::from_be_bytes([hi, lo]),
                v,
                "framebuffer pixel {i} matches the source texel"
            );
        }
    }

    /// **`Texture Rectangle Flip` and unsupported sizes draw nothing** (R-8): the
    /// copy path is wired only for a 16-bit tile into a 16-bit colour image.
    #[test]
    fn texture_rectangle_unsupported_configs_draw_nothing() {
        let mut mem = alloc::vec![0u8; 0x400];
        mem[0x308..0x310].copy_from_slice(&[0u8; 8]);
        let mut bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.tiles[0].size = 2;
        rdp.tiles[0].line = 1;
        rdp.color_image_size = 2;
        rdp.color_image_width = 4;
        rdp.color_image = 0x200;
        // Flip is deferred -> draws nothing.
        rdp.dispatch(
            OP_TEXTURE_RECTANGLE_FLIP,
            0x0000_C004,
            0x0000_0000,
            0x300,
            &mut bus,
        );
        assert!(
            bus.mem[0x200..0x210].iter().all(|&b| b == 0),
            "Flip draws nothing"
        );
        // 8-bit colour image (unsupported) -> draws nothing.
        rdp.color_image_size = 1;
        rdp.dispatch(
            OP_TEXTURE_RECTANGLE,
            0x0000_C004,
            0x0000_0000,
            0x300,
            &mut bus,
        );
        assert!(
            bus.mem[0x200..0x210].iter().all(|&b| b == 0),
            "8-bit target draws nothing"
        );
    }

    // ---- T-33-001: flat-fill triangle rasteriser ----

    /// **`Fill Triangle` (0x08) flat-fills a right triangle.** A left-major triangle
    /// with a vertical left edge at x=2 and a hypotenuse widening 1 pixel per row
    /// fills the staircase {row0:x2, row1:x2-3, row2:x2-4, row3:x2-5} — verified
    /// pixel-for-pixel against a 32-bit colour image, which pins the edge-walk and
    /// the s11.2/s11.16 fixed-point decode. This exact staircase is oracle-confirmed:
    /// `fill_tri_wide_16` (T-33-005) renders the same geometry byte-for-byte in Angrylion.
    #[test]
    fn fill_triangle_flat_fills_a_right_triangle() {
        let mut mem = alloc::vec![0u8; 0x400];
        // Edge words at cmd_base 0x300: word1 (L, unused) = 0; word2 (H major):
        // xh = 2.0 (0x2_0000), dxhdy = 0; word3 (M): xm = 2.0, dxmdy = 1.0 (0x1_0000).
        // The slope is dx per *pixel* row (R-14): 1.0 widens the span one pixel per row.
        mem[0x310..0x314].copy_from_slice(&0x0002_0000u32.to_be_bytes()); // xh
        mem[0x318..0x31C].copy_from_slice(&0x0002_0000u32.to_be_bytes()); // xm
        mem[0x31C..0x320].copy_from_slice(&0x0001_0000u32.to_be_bytes()); // dxmdy = 1.0
        let mut bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.color_image_size = 3; // 32-bit
        rdp.color_image_width = 8;
        rdp.color_image = 0x200;
        rdp.fill_color = 0xAABB_CCDD;
        rdp.scissor_lrx = 8 << 2;
        rdp.scissor_lry = 8 << 2;
        // word0: opcode 0x08, flip/lmajor (bit 55), yl=16, ym=16, yh=0.
        rdp.dispatch(0x08, 0x0880_0010, 0x0010_0000, 0x300, &mut bus);

        let filled = |bus: &SliceBus, x: usize, row: usize| -> bool {
            let a = 0x200 + row * 32 + x * 4;
            u32::from_be_bytes([bus.mem[a], bus.mem[a + 1], bus.mem[a + 2], bus.mem[a + 3]])
                == 0xAABB_CCDD
        };
        let expected = [
            (2, 0),
            (2, 1),
            (3, 1),
            (2, 2),
            (3, 2),
            (4, 2),
            (2, 3),
            (3, 3),
            (4, 3),
            (5, 3),
        ];
        for row in 0..4 {
            for x in 0..8 {
                let want = expected.contains(&(x, row));
                assert_eq!(filled(&bus, x, row), want, "pixel ({x},{row})");
            }
        }
    }

    /// **The triangle span is clipped to the scissor.** The same right triangle
    /// with the scissor's right edge at x=3 must lose the pixels the hypotenuse
    /// would otherwise reach (x=4 on row 2, x=5 on row 3) — exercising the X
    /// scissor boundary with an independent expectation.
    #[test]
    fn fill_triangle_is_clipped_to_the_scissor() {
        let mut mem = alloc::vec![0u8; 0x400];
        mem[0x310..0x314].copy_from_slice(&0x0002_0000u32.to_be_bytes()); // xh = 2.0
        mem[0x318..0x31C].copy_from_slice(&0x0002_0000u32.to_be_bytes()); // xm = 2.0
        mem[0x31C..0x320].copy_from_slice(&0x0001_0000u32.to_be_bytes()); // dxmdy = 1.0 (R-14)
        let mut bus = SliceBus {
            mem,
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.color_image_size = 3;
        rdp.color_image_width = 8;
        rdp.color_image = 0x200;
        rdp.fill_color = 0xAABB_CCDD;
        rdp.scissor_lrx = 3 << 2; // right edge at x=3 -> clips x>=4
        rdp.scissor_lry = 8 << 2; // all rows kept
        rdp.dispatch(0x08, 0x0880_0010, 0x0010_0000, 0x300, &mut bus);

        let filled = |bus: &SliceBus, x: usize, row: usize| -> bool {
            let a = 0x200 + row * 32 + x * 4;
            u32::from_be_bytes([bus.mem[a], bus.mem[a + 1], bus.mem[a + 2], bus.mem[a + 3]])
                == 0xAABB_CCDD
        };
        // The hypotenuse pixels past x=3 are clipped away.
        assert!(!filled(&bus, 4, 2), "x=4 row 2 clipped by scissor");
        assert!(!filled(&bus, 5, 3), "x=5 row 3 clipped by scissor");
        // The in-scissor part of every row is still drawn.
        assert!(filled(&bus, 2, 0));
        assert!(filled(&bus, 3, 1));
        assert!(filled(&bus, 3, 3), "x=3 row 3 kept (at the scissor edge)");
    }

    // ---- T-33-002: the colour combiner ----

    /// **`(A − B) * C + D` matches the hand-computed RDP arithmetic.** The `+0x80`
    /// rounding before `>> 8`, C's plain 9-bit sign, and D added unscaled.
    #[test]
    fn combiner_equation_matches_hand_computed() {
        // Passthrough C: A=One (0x100), B=0, D=0 -> the result is C.
        assert_eq!(combine_channel(0x100, 0, 128, 0), 128, "One*C passthrough");
        assert_eq!(combine_channel(0x100, 0, 255, 0), 255);
        // Lerp: (200-100)*128/256 + 100 = 50 + 100 = 150.
        assert_eq!(combine_channel(200, 100, 128, 100), 150);
        // clamp_9bit folds an over-range result: 300 -> 300-0x80=0x94 -> sext9 stays
        // positive (0x94<0x100) -> +0x80 = 0x114 -> clamp to 0xFF.
        assert_eq!(clamp_9bit(300), 0xFF, "over-range saturates");
        assert_eq!(clamp_9bit(-10), 0, "negative clamps to 0");
    }

    /// **`Set Combine Mode` (0x3C) decodes all 16 selects.** Each field is seeded
    /// distinctly so a swapped bit range surfaces as a wrong select.
    #[test]
    fn set_combine_mode_decodes_selects() {
        // Each field gets a distinct value in its own (non-overlapping) bit range.
        let mut rdp = Rdp::new();
        let hi = (0xA << 20) | (0x15 << 15) | (1 << 12) | (2 << 9) | (0xB << 5) | 0x1A;
        let lo = (3 << 28)
            | (0xC << 24)
            | (3 << 21)
            | (4 << 18)
            | (5 << 15)
            | (6 << 12)
            | (7 << 9)
            | (1 << 6)
            | (2 << 3)
            | 3;
        rdp.set_combine_mode(hi, lo);
        assert_eq!(rdp.combine.cyc0.rgb_a, 0xA);
        assert_eq!(rdp.combine.cyc0.rgb_c, 0x15);
        assert_eq!(rdp.combine.cyc0.rgb_b, 3);
        assert_eq!(rdp.combine.cyc0.rgb_d, 5);
        assert_eq!(rdp.combine.cyc0.a_a, 1);
        assert_eq!(rdp.combine.cyc0.a_c, 2);
        assert_eq!(rdp.combine.cyc0.a_b, 6);
        assert_eq!(rdp.combine.cyc0.a_d, 7);
        assert_eq!(rdp.combine.cyc1.rgb_a, 0xB);
        assert_eq!(rdp.combine.cyc1.rgb_c, 0x1A);
        assert_eq!(rdp.combine.cyc1.rgb_b, 0xC);
        assert_eq!(rdp.combine.cyc1.rgb_d, 1);
        assert_eq!(rdp.combine.cyc1.a_a, 3);
        assert_eq!(rdp.combine.cyc1.a_c, 4);
        assert_eq!(rdp.combine.cyc1.a_b, 2);
        assert_eq!(rdp.combine.cyc1.a_d, 3);
    }

    /// **A one-cycle combiner passes texel0 through.** `A = One`, `B = Zero`,
    /// `C = Texel0`, `D = Zero` for both RGB and alpha, so the output equals the
    /// texel — an observable evaluation, seeded so a no-op differs.
    #[test]
    fn combine_cycle_passes_texel0_through() {
        let cfg = CombineCycle {
            rgb_a: 6, // One
            rgb_b: 7, // -> Zero
            rgb_c: 1, // Texel0
            rgb_d: 7, // Zero
            a_a: 6,   // One
            a_b: 7,   // Zero
            a_c: 1,   // Texel0 alpha
            a_d: 7,   // Zero
        };
        let inp = CombinerInputs {
            texel0: [10, 20, 30, 40],
            ..CombinerInputs::default()
        };
        assert_eq!(Rdp::combine_cycle(cfg, &inp), [10, 20, 30, 40]);
    }

    /// **Two-cycle mode chains cycle 0 into cycle 1's `Combined` input.** Cycle 0
    /// passes texel0 through; cycle 1 selects `Combined` for A (with C=One-ish),
    /// so the final output reflects cycle 0's result — not cycle 1 reading a stale
    /// combined value.
    #[test]
    fn combine_two_cycle_chains() {
        let passthrough_texel0 = CombineCycle {
            rgb_a: 6,
            rgb_b: 7,
            rgb_c: 1,
            rgb_d: 7,
            a_a: 6,
            a_b: 7,
            a_c: 1,
            a_d: 7,
        };
        // Cycle 1: pass the Combined input through (A=One, B=Zero, C=Combined, D=Zero).
        let passthrough_combined = CombineCycle {
            rgb_a: 6,
            rgb_b: 7,
            rgb_c: 0, // Combined
            rgb_d: 7,
            a_a: 6,
            a_b: 7,
            a_c: 0, // combined alpha via C? alpha C 0 = lod-frac (0); use D instead
            a_d: 0, // combined alpha
        };
        let mut rdp = Rdp::new();
        rdp.combine.cyc0 = passthrough_texel0;
        rdp.combine.cyc1 = passthrough_combined;
        let inp = CombinerInputs {
            texel0: [11, 22, 33, 44],
            ..CombinerInputs::default()
        };
        let out = rdp.combine(inp, true);
        // RGB is cycle0's texel0 passed through cycle1's Combined.
        assert_eq!(&out[0..3], &[11, 22, 33], "2-cycle chains RGB");
        // Alpha also chains: cycle0 passes texel0's alpha (44) to Combined, and
        // cycle1's D = combined-alpha (C = lod-frac = 0), so the output is 44.
        assert_eq!(out[3], 44, "2-cycle chains alpha");
    }

    // ---- T-33-003: the blender ----

    /// **`Set Other Modes` (0x2F) decodes every field the blender uses.** Each is
    /// seeded distinctly in its own bit range so a swapped range surfaces as a
    /// wrong select — the two blend cycles interleave `P0 P1 A0 A1 M0 M1 B0 B1`.
    #[test]
    fn set_other_modes_decodes_fields() {
        let mut rdp = Rdp::new();
        let hi = 1 << 20; // cycle_type = 1 (2-cycle)
        let lo = (2 << 30) // P0
            | (3 << 28)    // P1
            | (1 << 26)    // A0
            | (2 << 24)    // A1
            | (3 << 22)    // M0
            | (1 << 20)    // M1
            | (2 << 18)    // B0
            | (3 << 16)    // B1
            | (1 << 14)    // force_blend
            | (2 << 10)    // z_mode
            | (1 << 8)     // cvg_dest
            | (1 << 6)     // image_read_en
            | (1 << 5)     // z_update_en
            | (1 << 4)     // z_compare_en
            | (1 << 3)     // aa_enable
            | 1; // alpha_compare_en
        rdp.set_other_modes(hi, lo);
        let om = rdp.other_modes;
        assert_eq!(om.cycle_type, 1);
        assert_eq!(
            om.blend[0],
            BlendCycle {
                p: 2,
                a: 1,
                m: 3,
                b: 2
            }
        );
        assert_eq!(
            om.blend[1],
            BlendCycle {
                p: 3,
                a: 2,
                m: 1,
                b: 3
            }
        );
        assert!(om.force_blend);
        assert_eq!(om.z_mode, 2);
        assert_eq!(om.cvg_dest, 1);
        assert!(om.image_read_en);
        assert!(om.z_update_en);
        assert!(om.z_compare_en);
        assert!(om.aa_enable);
        assert!(om.alpha_compare_en);
    }

    /// **`Set Blend Color` / `Set Fog Color` latch their RGBA8888 registers.**
    #[test]
    fn set_blend_and_fog_color_latch() {
        let mut bus = SliceBus {
            mem: alloc::vec![0u8; 0x100],
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.dispatch(OP_SET_BLEND_COLOR, 0, 0x1122_3344, 0, &mut bus);
        rdp.dispatch(OP_SET_FOG_COLOR, 0, 0x5566_7788, 0, &mut bus);
        assert_eq!(rdp.blend_color, 0x1122_3344);
        assert_eq!(rdp.fog_color, 0x5566_7788);
    }

    /// **`(P * a0 + M * (a1 + 1)) >> 5` matches the hand-computed no-divide blend.**
    /// `P = pixel`, `M = memory`, `A = pixel-alpha` (128 → a0 = 16), `B = one`
    /// (0xFF → a1 = 31): ch0 = 100·16 + 10·32 = 1920 → 60; ch1 → 95; ch2 → 130.
    #[test]
    fn blend_cycle_matches_hand_computed() {
        let cycle = BlendCycle {
            p: 0,
            a: 0,
            m: 1,
            b: 2,
        };
        let inp = BlendInputs {
            pixel: [100, 150, 200, 128],
            memory: [10, 20, 30, 40],
            ..BlendInputs::default()
        };
        assert_eq!(Rdp::blend_cycle(cycle, &inp), [60, 95, 130]);
    }

    /// **Two-cycle mode chains cycle 0's RGB into cycle 1's pixel input.** Cycle 0
    /// blends pixel⊕fog → [93,93,93]; cycle 1 re-blends that against fog with
    /// `B = zero` → [90,90,90], which differs from cycle 0 alone — proving the
    /// chain feeds forward rather than re-reading the original pixel.
    #[test]
    fn blend_two_cycle_chains() {
        let mut rdp = Rdp::new();
        rdp.other_modes.cycle_type = 1;
        rdp.other_modes.blend[0] = BlendCycle {
            p: 0,
            a: 0,
            m: 3,
            b: 2,
        };
        rdp.other_modes.blend[1] = BlendCycle {
            p: 0,
            a: 0,
            m: 3,
            b: 3,
        };
        let inp = BlendInputs {
            pixel: [80, 80, 80, 0xFF],
            fog: [16, 16, 16, 0xFF],
            ..BlendInputs::default()
        };
        // Cycle 0 alone would give [93,93,93]; the chained result is [90,90,90].
        assert_eq!(
            Rdp::blend_cycle(rdp.other_modes.blend[0], &inp),
            [93, 93, 93]
        );
        assert_eq!(rdp.blend(inp), [90, 90, 90], "2-cycle chains forward");
    }

    /// **The `B = 1 − A` select complements the *resolved* `A`, not pixel alpha.**
    /// `A` selects fog alpha (0xFF → a0 = 31) and `B` selects `1 − A`, so
    /// `a1 = (~0xFF) >> 3 = 0` and `a1 + 1 = 1`: `100·31 + 10·1 = 3110 → 97`.
    /// Pixel alpha is 0x00 here, so the old `!pixel_alpha` bug would use
    /// `a1 = (~0x00) >> 3 = 31` → `100·31 + 10·32 = 3420 → 106`; asserting 97
    /// fails against that regression (ParaLLEl-RDP `blender.h:106`, `~a0`).
    #[test]
    fn blend_inv_alpha_complements_selected_a_not_pixel() {
        let cycle = BlendCycle {
            p: 0,
            a: 1,
            m: 1,
            b: 0,
        };
        let inp = BlendInputs {
            pixel: [100, 100, 100, 0x00],
            memory: [10, 10, 10, 40],
            fog: [0, 0, 0, 0xFF],
            ..BlendInputs::default()
        };
        assert_eq!(Rdp::blend_cycle(cycle, &inp), [97, 97, 97]);
    }

    // ---- T-33-004: the Z-buffer machinery ----

    /// **The Z codec matches the ParaLLEl-RDP `z_encode.h` arithmetic.** Boundary
    /// values are hand-computed; `z_compress ∘ z_decompress` round-trips canonical
    /// stored values; `dz` is `1 << n` with an integer-`log2` inverse.
    #[test]
    fn z_codec_matches_hand_computed() {
        // decompress: 0 → 0; max 14-bit (0x3FFF) → max 18-bit (0x3FFFF); a mid value.
        assert_eq!(z_decompress(0), 0);
        assert_eq!(z_decompress(0x3FFF), 0x3_FFFF);
        assert_eq!(z_decompress(0x3000), 0x3_F000); // exp 6, man 0
        // round-trip canonical stored values.
        for stored in [0u16, 0x2000, 0x3FFF] {
            assert_eq!(
                z_compress(z_decompress(stored)),
                stored,
                "round-trip {stored:#x}"
            );
        }
        // dz: 1<<n and its integer-log2 inverse (0 maps to 0 via find_msb == -1).
        assert_eq!(dz_decompress(15), 0x8000);
        assert_eq!(dz_decompress(0), 1);
        assert_eq!(dz_compress(0x8000), 15);
        assert_eq!(dz_compress(1), 0);
        assert_eq!(dz_compress(0), 0);
        assert_eq!(combine_dz(0x180), 0x100, "largest POT <= 384 is 256");
        assert_eq!(combine_dz(0), 0);
    }

    /// **`Set Primitive Depth` (0x2E) and `Set Depth Image` (0x3E) latch.** z in
    /// `lo[31:16]`, dz in `lo[15:0]`; the depth-image base masks to 24 bits.
    #[test]
    fn set_prim_depth_and_depth_image_latch() {
        let mut bus = SliceBus {
            mem: alloc::vec![0u8; 0x100],
            dp_raised: false,
        };
        let mut rdp = Rdp::new();
        rdp.dispatch(OP_SET_PRIM_DEPTH, 0, (0x1234 << 16) | 0x0080, 0, &mut bus);
        assert_eq!(rdp.prim_z, 0x1234);
        assert_eq!(rdp.prim_dz, 0x0080);
        rdp.dispatch(OP_SET_DEPTH_IMAGE, 0, 0xAB00_1240, 0, &mut bus);
        assert_eq!(rdp.z_image, 0x0000_1240, "24-bit masked base");
    }

    /// Build [`DepthInputs`] for a memory pixel at stored depth `0x3000`
    /// (`memory_z == 0x3F000`), `dz == 0`, precision-factor 6 (so no `dz`
    /// adjustment), with the given mode. `aa`/`force_blend`/`coverage` are off so
    /// the depth decision is pure less-than.
    fn depth_inputs(z_mode: u8) -> DepthInputs {
        DepthInputs {
            current_depth: 0x3000,
            current_dz: 0,
            current_coverage: 0,
            z_compare: true,
            z_mode,
            force_blend: false,
            aa_enable: false,
        }
    }

    /// **Opaque Z mode: the nearer pixel passes, the farther pixel is rejected.**
    /// The observable occluding-vs-occluded pair against a memory depth of
    /// `0x3F000`: an in-front pixel (`z = 0x30000`) writes, a behind one
    /// (`z = 0x3FF00`) does not.
    #[test]
    fn depth_test_opaque_occludes() {
        let inp = depth_inputs(0);
        assert!(
            Rdp::depth_test(0x30000, 0, 0, 1, &inp).depth_pass,
            "nearer pixel passes"
        );
        assert!(
            !Rdp::depth_test(0x3FF00, 0, 0, 1, &inp).depth_pass,
            "farther pixel rejected"
        );
    }

    /// **Transparent Z mode passes strictly-in-front pixels only.** Same pair:
    /// front passes, behind fails (no coverage/decal subtlety).
    #[test]
    fn depth_test_transparent_passes_front() {
        let inp = depth_inputs(2);
        assert!(Rdp::depth_test(0x30000, 0, 0, 1, &inp).depth_pass);
        assert!(!Rdp::depth_test(0x3FF00, 0, 0, 1, &inp).depth_pass);
    }

    /// **Decal Z mode passes only coplanar pixels.** A pixel at the memory depth
    /// (`z = 0x3F000`) passes; an in-front pixel (`z = 0x30000`) — which opaque
    /// mode would accept — is rejected, distinguishing decal from opaque.
    #[test]
    fn depth_test_decal_passes_coplanar_only() {
        let inp = depth_inputs(3);
        assert!(
            Rdp::depth_test(0x3_F000, 0, 0, 1, &inp).depth_pass,
            "coplanar passes"
        );
        assert!(
            !Rdp::depth_test(0x30000, 0, 0, 1, &inp).depth_pass,
            "in-front (non-coplanar) rejected"
        );
    }

    /// **`z_compare` off: every pixel passes**, regardless of stored depth — the
    /// depth test is bypassed and only coverage/blend state is derived.
    #[test]
    fn depth_test_disabled_always_passes() {
        let mut inp = depth_inputs(0);
        inp.z_compare = false;
        assert!(Rdp::depth_test(0x3FF00, 0, 0, 1, &inp).depth_pass);
        assert!(Rdp::depth_test(0x00000, 0, 0, 1, &inp).depth_pass);
    }

    /// **Interpenetrating Z mode reduces coverage at an intersect.** With the
    /// `front && farther && overflow` intersect condition met (a near pixel just
    /// short of `memory_z = 0x3F000`, coverage overflowing), the pixel passes and
    /// its coverage is scaled: `cvg_coeff = (0x3F000 − 0x3EFFC) & 0xf = 4`, so
    /// `coverage_count = min((4·4) >> 3, 8) = 2` — hand-computed from `depth_test.h`.
    #[test]
    fn depth_test_interpenetrating_reduces_coverage() {
        let inp = DepthInputs {
            current_depth: 0x3000, // memory_z = 0x3F000, precision-factor 6
            current_dz: 0,
            current_coverage: 4, // + coverage_count 4 => overflow (>= 8)
            z_compare: true,
            z_mode: 1,
            force_blend: false,
            aa_enable: false,
        };
        let r = Rdp::depth_test(0x3_EFFC, 0, 0, 4, &inp);
        assert!(r.depth_pass, "intersect passes");
        assert_eq!(r.coverage_count, 2, "coverage scaled down at the intersect");
    }

    /// **The `precision_factor < 3` coplanar path forces a pass.** A memory pixel
    /// with a low exponent (`precision-factor 2`) and `current_dz == 15`
    /// (`memory_dz == 0x8000`) is treated as coplanar, so even a pixel *behind*
    /// `memory_z` passes opaque mode — exercising the stored-`dz` adjustment that
    /// the plain occluding pairs (precision-factor 6) deliberately avoid.
    #[test]
    fn depth_test_precision_factor_coplanar_forces_pass() {
        let inp = DepthInputs {
            current_depth: 0x1000, // memory_z = 0x30000, precision-factor 2 (< 3)
            current_dz: 15,        // memory_dz = 0x8000 -> coplanar branch
            current_coverage: 0,
            z_compare: true,
            z_mode: 0,
            force_blend: false,
            aa_enable: false,
        };
        // 0x3FF00 is behind memory_z (0x30000); without the coplanar path it would
        // fail opaque mode, but coplanar makes `nearer` unconditionally true.
        assert!(Rdp::depth_test(0x3_FF00, 0, 0, 1, &inp).depth_pass);
    }

    /// **Out-of-domain inputs are sanitized, not panicked on.** Every argument here
    /// is outside its hardware domain — a `current_depth`/`current_dz` with junk in
    /// the upper bits, a negative `z`, a huge `dz`, an out-of-4-bit `dz_compressed`.
    /// The boundary clamps/masks bound them all, so the shifts (`1 << dz`,
    /// `combine_dz`, `combined_dz << 3`) and the `z ± combined_dz` sums stay in
    /// range. Without the sanitization this panics in a debug build.
    #[test]
    fn depth_test_out_of_domain_inputs_do_not_panic() {
        let inp = DepthInputs {
            current_depth: 0xFFFF, // masked to 14 bits
            current_dz: 200,       // masked to 4 bits (→ 8)
            current_coverage: 0,
            z_compare: true,
            z_mode: 0,
            force_blend: false,
            aa_enable: false,
        };
        // Negative z (clamped to 0), a large dz (clamped to the 18-bit range, so
        // combine_dz << 3 stays bounded), and dz_compressed 20 (clamped to 0xf).
        let r = Rdp::depth_test(-5, 0x7FFF_FFFF, 20, 1, &inp);
        assert!(
            r.blend_shift[0] <= 4 && r.blend_shift[1] <= 4,
            "invariant holds"
        );
    }

    /// A test bus that models the RDRAM hidden bits (one 2-bit value per 16-bit
    /// halfword), so the full 4-bit `dz` round-trip can be exercised.
    struct ZBufBus {
        mem: Vec<u8>,
        hidden: Vec<u8>,
    }
    impl RdramBus for ZBufBus {
        fn rdram_read(&self, addr: u32) -> u8 {
            self.mem.get(addr as usize).copied().unwrap_or(0)
        }
        fn rdram_write(&mut self, addr: u32, val: u8) {
            if let Some(b) = self.mem.get_mut(addr as usize) {
                *b = val;
            }
        }
        fn rdram_read_hidden(&self, addr: u32) -> u8 {
            self.hidden.get((addr >> 1) as usize).copied().unwrap_or(0) & 0x3
        }
        fn rdram_write_hidden(&mut self, addr: u32, val: u8) {
            if let Some(b) = self.hidden.get_mut((addr >> 1) as usize) {
                *b = val & 0x3;
            }
        }
    }
    impl VideoBus for ZBufBus {
        fn raise_dp_interrupt(&mut self) {}
    }

    /// **The Z buffer round-trips the compressed z and the full 4-bit dz.** The dz
    /// splits across the halfword's low 2 bits and the hidden bits, so a value like
    /// `0xB` (`0b1011`) only survives if the hidden path carries the low 2 bits —
    /// without it, `read` would return `0b1000` (`8`). `0x30000` is a canonical
    /// depth that `z_compress`/`z_decompress` reproduce exactly.
    #[test]
    fn zbuffer_round_trips_z_and_dz() {
        let mut bus = ZBufBus {
            mem: alloc::vec![0u8; 0x1000],
            hidden: alloc::vec![0u8; 0x800],
        };
        let mut rdp = Rdp::new();
        rdp.z_image = 0x200;
        rdp.color_image_width = 8;
        rdp.zbuffer_write(3, 2, 0x30000, 0xB, &mut bus);
        let (cz, dz) = rdp.zbuffer_read(3, 2, &bus);
        assert_eq!(cz, z_compress(0x30000), "compressed z stored and loaded");
        assert_eq!(
            z_decompress(cz),
            0x30000,
            "canonical depth decompresses exactly"
        );
        assert_eq!(dz, 0xB, "full 4-bit dz survives via halfword + hidden bits");
        // A different pixel is untouched (independent entry).
        assert_eq!(rdp.zbuffer_read(4, 2, &bus), (0, 0));
    }

    /// **`interpolate_z` matches the hand-computed ParaLLEl-RDP snap.** A flat depth
    /// (`z_base = 0x0800_0000`, no gradient) snaps to `0x4000`; a pure horizontal
    /// gradient (`dzdx = 1.0`) advances `4 · 0x1_0000` over four pixels, which the
    /// `>> 10 << 2 >> 5` snap folds to `0x20`.
    #[test]
    fn interpolate_z_matches_hand_computed() {
        assert_eq!(interpolate_z(0x0800_0000, 0, 0, 0, 0, 0, 0), 0x4000);
        assert_eq!(interpolate_z(0, 0x0001_0000, 0, 0, 0, 0, 4), 0x20);
        // A negative (below-near-plane) depth clamps to 0.
        assert_eq!(interpolate_z(-0x0001_0000, 0, 0, 0, 0, 0, 0), 0);
    }

    /// **A Z-buffered triangle occludes a farther one and yields to a nearer one.**
    /// The first-ever depth-tested rendering: three overlapping right triangles into
    /// a Z buffer pre-cleared to the far plane, with flat depths `z_px` 0x4000 (near),
    /// 0x8000 (far), 0x2000 (nearer). The near draws (vs the cleared buffer), the far
    /// is **rejected** (stays the near colour), and the nearer **overwrites** — so the
    /// test discriminates both the accept and reject paths of `depth_test`.
    #[test]
    fn depth_tested_triangle_occludes_farther_and_yields_to_nearer() {
        let mut bus = ZBufBus {
            mem: alloc::vec![0u8; 0x1000],
            hidden: alloc::vec![0u8; 0x800],
        };
        // Pre-clear the Z buffer to the far plane (compressed 0x3FFF → z 0x3FFFF).
        for b in &mut bus.mem[0x400..0x500] {
            *b = 0xFF;
        }
        let mut rdp = Rdp::new();
        rdp.color_image = 0x200;
        rdp.color_image_size = 3; // 32-bit
        rdp.color_image_width = 8;
        rdp.z_image = 0x400;
        rdp.scissor_lrx = 8 << 2;
        rdp.scissor_lry = 8 << 2;
        rdp.other_modes.z_compare_en = true;
        rdp.other_modes.z_update_en = true;
        rdp.other_modes.z_mode = 0; // opaque

        // Draw the same staircase triangle (as the flat-fill test) at each cmd_base,
        // now as the Z-buffered variant (opcode 0x09), with a flat z-suffix (z_base
        // only; dzdx/dzde/dzdy = 0). The command word matches the flat-fill test with
        // the z-flag bit (56) set → hi 0x0980_0010.
        let draw = |bus: &mut ZBufBus, rdp: &mut Rdp, base: usize, z_base: u32, color: u32| {
            bus.mem[base + 0x10..base + 0x14].copy_from_slice(&0x0002_0000u32.to_be_bytes()); // xh
            bus.mem[base + 0x18..base + 0x1C].copy_from_slice(&0x0002_0000u32.to_be_bytes()); // xm
            bus.mem[base + 0x1C..base + 0x20].copy_from_slice(&0x0000_4000u32.to_be_bytes()); // dxmdy
            bus.mem[base + 0x20..base + 0x24].copy_from_slice(&z_base.to_be_bytes()); // z_base
            rdp.fill_color = color;
            rdp.dispatch(0x09, 0x0980_0010, 0x0010_0000, base as u32, bus);
        };
        let px = |bus: &ZBufBus, x: usize, row: usize| -> u32 {
            let a = 0x200 + row * 32 + x * 4;
            u32::from_be_bytes([bus.mem[a], bus.mem[a + 1], bus.mem[a + 2], bus.mem[a + 3]])
        };

        draw(&mut bus, &mut rdp, 0x600, 0x0800_0000, 0x1111_1111); // near  (z_px 0x4000)
        assert_eq!(px(&bus, 2, 0), 0x1111_1111, "near draws vs cleared buffer");
        draw(&mut bus, &mut rdp, 0x700, 0x1000_0000, 0x2222_2222); // far   (z_px 0x8000)
        assert_eq!(
            px(&bus, 2, 0),
            0x1111_1111,
            "far is occluded (depth rejects)"
        );
        assert_eq!(px(&bus, 2, 3), 0x1111_1111);
        draw(&mut bus, &mut rdp, 0x800, 0x0400_0000, 0x3333_3333); // nearer(z_px 0x2000)
        assert_eq!(
            px(&bus, 2, 0),
            0x3333_3333,
            "nearer overwrites (depth accepts)"
        );
    }

    /// **`decode_triangle_z` does not panic on an `i32::MIN` gradient.** The z-suffix
    /// is unvalidated RDRAM; a `dzdx`/`dzde` of `0x8000_0000` would overflow `.abs()`.
    /// `saturating_abs` keeps it total.
    #[test]
    fn decode_triangle_z_survives_i32_min_gradient() {
        let mut bus = ZBufBus {
            mem: alloc::vec![0u8; 0x100],
            hidden: alloc::vec![0u8; 0x80],
        };
        // z-flag set (bit 24), no shade/tex -> z-suffix at cmd_base + 0x20; put
        // i32::MIN at dzdx (za + 4 = 0x24).
        bus.mem[0x24..0x28].copy_from_slice(&0x8000_0000u32.to_be_bytes());
        assert!(Rdp::decode_triangle_z(1 << 24, 0, &bus).is_some());
    }

    // ---- T-33-004 PR-B 2b: shade interpolation ----

    /// **`decode_shade` assembles the RGBA base and `interpolate_shade` yields the
    /// byte colours.** The shade block's int-base word packs `R.i`/`G.i` in the hi
    /// u32 (bits 56:48 / 40:32) and `B.i`/`A.i` in the lo u32; a flat base (no
    /// deltas) of `(100, 150, 200, 255)` interpolates to exactly those bytes.
    #[test]
    fn decode_shade_assembles_base_and_interpolates() {
        let mut bus = ZBufBus {
            mem: alloc::vec![0u8; 0x100],
            hidden: alloc::vec![0u8; 0x80],
        };
        // Shade block at cmd_base(0) + 4 words = 0x20. Word0 int-base:
        // hi = (R.i << 16) | G.i, lo = (B.i << 16) | A.i. Frac/deltas left 0.
        bus.mem[0x20..0x24].copy_from_slice(&((0x64u32 << 16) | 0x96).to_be_bytes()); // R=100 G=150
        bus.mem[0x24..0x28].copy_from_slice(&((0xC8u32 << 16) | 0xFF).to_be_bytes()); // B=200 A=255
        let shade = Rdp::decode_shade(1 << 26, 0, &bus).expect("shade block present");
        assert_eq!(shade.base, [0x64_0000, 0x96_0000, 0xC8_0000, 0xFF_0000]);
        assert_eq!(shade.dx, [0; 4]);
        assert_eq!(shade.de, [0; 4]);
        assert_eq!(
            interpolate_shade(&shade.base, &shade.dx, &shade.de, 0, 0, 0, 0),
            [100, 150, 200, 255],
            "flat base interpolates to the byte colour"
        );
    }

    /// **A shaded triangle renders the interpolated colour through the combiner.**
    /// A flat-shaded triangle (base `(0x11, 0x22, 0x33, 0xFF)`) with a shade-
    /// passthrough combiner (`D = shade`, `A = B` so `(A−B)·C = 0`) writes that
    /// colour — not the FILL register — into the 32-bit colour image, proving the
    /// decode → interpolate → combine → write path.
    #[test]
    fn shaded_triangle_renders_combined_shade() {
        let mut bus = ZBufBus {
            mem: alloc::vec![0u8; 0x1000],
            hidden: alloc::vec![0u8; 0x800],
        };
        let mut rdp = Rdp::new();
        rdp.color_image = 0x200;
        rdp.color_image_size = 3; // 32-bit
        rdp.color_image_width = 8;
        rdp.scissor_lrx = 8 << 2;
        rdp.scissor_lry = 8 << 2;
        rdp.fill_color = 0xDEAD_BEEF; // must NOT appear
        // Shade-passthrough combiner: cyc1 D = shade (4), A = B (cancel).
        rdp.combine.cyc1 = CombineCycle {
            rgb_a: 0,
            rgb_b: 0,
            rgb_c: 0,
            rgb_d: 4,
            a_a: 0,
            a_b: 0,
            a_c: 0,
            a_d: 4,
        };
        // Staircase triangle (as the flat-fill test) with the shade flag (bit 58 ->
        // hi bit 26), flat base colour (0x11, 0x22, 0x33, 0xFF).
        let base = 0x600usize;
        bus.mem[base + 0x10..base + 0x14].copy_from_slice(&0x0002_0000u32.to_be_bytes()); // xh
        bus.mem[base + 0x18..base + 0x1C].copy_from_slice(&0x0002_0000u32.to_be_bytes()); // xm
        bus.mem[base + 0x1C..base + 0x20].copy_from_slice(&0x0001_0000u32.to_be_bytes()); // dxmdy = 1.0 (R-14)
        // Shade int-base at base + 0x20 / 0x24.
        bus.mem[base + 0x20..base + 0x24].copy_from_slice(&((0x11u32 << 16) | 0x22).to_be_bytes());
        bus.mem[base + 0x24..base + 0x28].copy_from_slice(&((0x33u32 << 16) | 0xFF).to_be_bytes());
        // opcode 0x0C = Fill Shaded Triangle (bit 58 set); hi = 0x0880_0010 | (1<<26).
        rdp.dispatch(0x0C, 0x0C80_0010, 0x0010_0000, base as u32, &mut bus);

        let px = |bus: &ZBufBus, x: usize, row: usize| -> u32 {
            let a = 0x200 + row * 32 + x * 4;
            u32::from_be_bytes([bus.mem[a], bus.mem[a + 1], bus.mem[a + 2], bus.mem[a + 3]])
        };
        // 1-cycle mode stores sub-pixel coverage in the alpha byte (validated against
        // Angrylion by `shade_tri_frac_16`), so check the combiner RGB at a fully-
        // covered interior pixel — full coverage stores `7 << 5 = 0xE0`. The top
        // vertex (2,0) is a degenerate single point, excluded by the AA-off top-left
        // sample rule.
        assert_eq!(
            px(&bus, 2, 3),
            0x1122_33E0,
            "shaded RGB + full-coverage alpha"
        );
        assert_eq!(px(&bus, 0, 0), 0, "outside the triangle stays clear");
    }

    /// **A combined shaded + depth-tested triangle (0x0D) decodes both blocks at the
    /// right offsets.** With the shade block at `+0x20` and the z block at `+0x60`
    /// (past the 8-word shade block), the pixel must be the shade colour *and* the
    /// stored depth must be the z-block's value — if `decode_triangle_z` misread the
    /// shade block as z, the stored `compressed_z` would differ.
    #[test]
    fn shaded_and_depth_tested_triangle_reads_both_blocks() {
        let mut bus = ZBufBus {
            mem: alloc::vec![0u8; 0x1000],
            hidden: alloc::vec![0u8; 0x800],
        };
        for b in &mut bus.mem[0x400..0x500] {
            *b = 0xFF; // Z buffer pre-cleared to the far plane
        }
        let mut rdp = Rdp::new();
        rdp.color_image = 0x200;
        rdp.color_image_size = 3;
        rdp.color_image_width = 8;
        rdp.z_image = 0x400;
        rdp.scissor_lrx = 8 << 2;
        rdp.scissor_lry = 8 << 2;
        rdp.other_modes.z_compare_en = true;
        rdp.other_modes.z_update_en = true;
        rdp.combine.cyc1 = CombineCycle {
            rgb_a: 0,
            rgb_b: 0,
            rgb_c: 0,
            rgb_d: 4,
            a_a: 0,
            a_b: 0,
            a_c: 0,
            a_d: 4,
        };
        let base = 0x600usize;
        bus.mem[base + 0x10..base + 0x14].copy_from_slice(&0x0002_0000u32.to_be_bytes()); // xh
        bus.mem[base + 0x18..base + 0x1C].copy_from_slice(&0x0002_0000u32.to_be_bytes()); // xm
        bus.mem[base + 0x1C..base + 0x20].copy_from_slice(&0x0000_4000u32.to_be_bytes()); // dxmdy
        // Shade block int-base at +0x20 (colour 0x1122_33FF).
        bus.mem[base + 0x20..base + 0x24].copy_from_slice(&((0x11u32 << 16) | 0x22).to_be_bytes());
        bus.mem[base + 0x24..base + 0x28].copy_from_slice(&((0x33u32 << 16) | 0xFF).to_be_bytes());
        // Z block at +0x60 (past the 8-word shade block): z_base -> z_px 0x4000.
        bus.mem[base + 0x60..base + 0x64].copy_from_slice(&0x0800_0000u32.to_be_bytes());
        // opcode 0x0D = shade (bit 58) + z (bit 56); hi = 0x0880_0010 | (1<<26) | (1<<24).
        rdp.dispatch(0x0D, 0x0D80_0010, 0x0010_0000, base as u32, &mut bus);

        let a = 0x200 + 2 * 4; // pixel (2, 0)
        let color =
            u32::from_be_bytes([bus.mem[a], bus.mem[a + 1], bus.mem[a + 2], bus.mem[a + 3]]);
        assert_eq!(color, 0x1122_33FF, "shade block decoded (colour)");
        let (cz, _) = rdp.zbuffer_read(2, 0, &bus);
        assert_eq!(
            cz,
            z_compress(0x4000),
            "z block decoded at +0x60, not the shade block"
        );
    }

    /// **A textured triangle samples the tile through the combiner.** A flat texture
    /// coordinate (`s = t = 0`, scale-independent of the perspective divide) samples
    /// texel `(0, 0)` — an opaque red RGBA16 — and a texel-passthrough combiner
    /// (`D = texel0`) writes `0xFF0000FF`, not the FILL register, proving the
    /// `decode_texture` → `interpolate_st` → `fetch_texel` → combine → write path.
    #[test]
    fn textured_triangle_samples_the_texel() {
        let mut bus = ZBufBus {
            mem: alloc::vec![0u8; 0x1000],
            hidden: alloc::vec![0u8; 0x800],
        };
        let mut rdp = Rdp::new();
        rdp.color_image = 0x200;
        rdp.color_image_size = 3;
        rdp.color_image_width = 8;
        rdp.scissor_lrx = 8 << 2;
        rdp.scissor_lry = 8 << 2;
        rdp.fill_color = 0xDEAD_BEEF; // must NOT appear
        // Tile 0: RGBA16 at TMEM 0; texel (0,0) = 0xF801 (opaque red).
        rdp.tiles[0].format = 0;
        rdp.tiles[0].size = 2;
        rdp.tiles[0].tmem_addr = 0;
        rdp.tiles[0].line = 0;
        rdp.tmem_write(0, 0xF8);
        rdp.tmem_write(1, 0x01);
        // Texel-passthrough combiner: cyc1 D = texel0 (1), A = B (cancel).
        rdp.combine.cyc1 = CombineCycle {
            rgb_a: 0,
            rgb_b: 0,
            rgb_c: 0,
            rgb_d: 1,
            a_a: 0,
            a_b: 0,
            a_c: 0,
            a_d: 1,
        };
        let base = 0x600usize;
        bus.mem[base + 0x10..base + 0x14].copy_from_slice(&0x0002_0000u32.to_be_bytes()); // xh
        bus.mem[base + 0x18..base + 0x1C].copy_from_slice(&0x0002_0000u32.to_be_bytes()); // xm
        bus.mem[base + 0x1C..base + 0x20].copy_from_slice(&0x0001_0000u32.to_be_bytes()); // dxmdy = 1.0 (R-14)
        // Texture block at base + 0x20 (no shade): all-zero -> s = t = 0.
        // opcode 0x0A = texture (bit 57); hi = 0x0880_0010 | (1 << 25).
        rdp.dispatch(0x0A, 0x0A80_0010, 0x0010_0000, base as u32, &mut bus);

        let px = |bus: &ZBufBus, x: usize, row: usize| -> u32 {
            let a = 0x200 + row * 32 + x * 4;
            u32::from_be_bytes([bus.mem[a], bus.mem[a + 1], bus.mem[a + 2], bus.mem[a + 3]])
        };
        // 1-cycle sub-pixel coverage stores `7 << 5 = 0xE0` in the alpha at a
        // fully-covered interior pixel; the degenerate top vertex (2,0) is excluded.
        assert_eq!(
            px(&bus, 2, 3),
            0xFF00_00E0,
            "textured red + full-coverage alpha, not the FILL colour"
        );
    }

    /// **`decode_texture` pairs the interleaved int/frac words per the wiki.** The
    /// block is `word0 = s.i/t.i`, `word2 = s.f/t.f`, `word4 = dsde.i` — so the base
    /// assembles from words 0 (int) and **2** (frac). Distinct frac bytes absent
    /// from word 4 make a mispaired decode (e.g. int + de) surface as a wrong base.
    #[test]
    fn decode_texture_pairs_interleaved_int_frac() {
        let mut bus = ZBufBus {
            mem: alloc::vec![0u8; 0x100],
            hidden: alloc::vec![0u8; 0x80],
        };
        // Word0 int-base hi = (s.i << 16) | t.i; word2 frac-base hi = (s.f << 16) | t.f.
        // Word4 (de int) left 0, so a base that read word4 as its frac would be wrong.
        bus.mem[0x20..0x24].copy_from_slice(&((0x0005u32 << 16) | 0x0007).to_be_bytes());
        bus.mem[0x30..0x34].copy_from_slice(&((0x8000u32 << 16) | 0x4000).to_be_bytes());
        let tex = Rdp::decode_texture(1 << 25, 0, &bus).expect("texture block present");
        assert_eq!(
            [tex.base[0], tex.base[1]],
            [0x0005_8000, 0x0007_4000],
            "base = word0 (int) + word2 (frac), not word4"
        );
        assert_eq!(tex.de, [0, 0, 0], "de reads word4/word6 (all zero here)");
    }

    /// **A shaded + textured triangle (0x0E) reads the texture block past the 8-word
    /// shade block.** The shade block is 8 words, so the texture block sits at
    /// `+0x60`. The texture coordinate there selects texel column 1 (green); if the
    /// texture offset were wrong (e.g. a 16-word shade assumption → `+0xA0`), it would
    /// read zeros and sample column 0 (red) instead. The green result pins `+0x60`.
    #[test]
    fn shaded_and_textured_triangle_reads_texture_past_shade() {
        let mut bus = ZBufBus {
            mem: alloc::vec![0u8; 0x1000],
            hidden: alloc::vec![0u8; 0x800],
        };
        let mut rdp = Rdp::new();
        rdp.color_image = 0x200;
        rdp.color_image_size = 3;
        rdp.color_image_width = 8;
        rdp.scissor_lrx = 8 << 2;
        rdp.scissor_lry = 8 << 2;
        rdp.tiles[0].format = 0;
        rdp.tiles[0].size = 2;
        // texel (0,0) = red 0xF801, texel (1,0) = green 0x07C1.
        rdp.tmem_write(0, 0xF8);
        rdp.tmem_write(1, 0x01);
        rdp.tmem_write(2, 0x07);
        rdp.tmem_write(3, 0xC1);
        rdp.combine.cyc1 = CombineCycle {
            rgb_a: 0,
            rgb_b: 0,
            rgb_c: 0,
            rgb_d: 1, // texel0
            a_a: 0,
            a_b: 0,
            a_c: 0,
            a_d: 1,
        };
        let base = 0x600usize;
        bus.mem[base + 0x10..base + 0x14].copy_from_slice(&0x0002_0000u32.to_be_bytes()); // xh
        bus.mem[base + 0x18..base + 0x1C].copy_from_slice(&0x0002_0000u32.to_be_bytes()); // xm
        bus.mem[base + 0x1C..base + 0x20].copy_from_slice(&0x0000_4000u32.to_be_bytes()); // dxmdy
        // Shade block at +0x20 (8 words). Texture block at +0x60: s.i = 1 -> column 1.
        bus.mem[base + 0x60..base + 0x64].copy_from_slice(&(0x0001u32 << 16).to_be_bytes());
        // opcode 0x0E = shade (bit 58) + texture (bit 57); hi = 0x0880_0010 | (1<<26) | (1<<25).
        rdp.dispatch(0x0E, 0x0E80_0010, 0x0010_0000, base as u32, &mut bus);

        // This near-vertical triangle (DxMDy 0.25) covers column 2 only partially, so
        // check the combiner RGB (the point of this test — the texture is decoded at
        // +0x60, past the shade block); the alpha holds sub-pixel coverage, exercised
        // separately by `shade_tri_frac_16`. Pixel (2,1) is drawn (the top vertex is a
        // degenerate point).
        let a = 0x200 + 32 + 2 * 4; // pixel (2, 1)
        let color =
            u32::from_be_bytes([bus.mem[a], bus.mem[a + 1], bus.mem[a + 2], bus.mem[a + 3]]);
        assert_eq!(
            color & 0xFFFF_FF00,
            0x00FF_0000,
            "texel column 1 (green) — texture read at +0x60"
        );
        assert_ne!(
            color & 0xFF,
            0,
            "the covered pixel stores non-zero coverage"
        );
    }

    /// **`perspective_divide` matches the hand-computed ParaLLEl-RDP arithmetic.** For
    /// `w = 0x4000` the LUT gives `rcp = 0x4000`, `shift = 0`, so `s → (s·0x4000) >> 13`
    /// (`0x10 → 0x20`); `w = 0x2000` gives `shift = 1` so `>> 12` (`0x100 → 0x400`); and
    /// `w <= 0` sets the carry to `0x7FFF`.
    #[test]
    fn perspective_divide_matches_hand_computed() {
        assert_eq!(perspective_divide(0x10, 0x20, 0x4000), (0x20, 0x40));
        assert_eq!(perspective_divide(0x100, 0, 0x2000), (0x400, 0));
        assert_eq!(
            perspective_divide(0x10, 0x20, -1),
            (0x7FFF, 0x7FFF),
            "w<=0 carry"
        );
        // The LUT's first/last entries pin the transcription boundaries.
        assert_eq!(PERSPECTIVE_TABLE[0], (0x4000, -1008));
        assert_eq!(PERSPECTIVE_TABLE[63], (0x2041, -260));
    }

    /// **A translucent triangle blends with the framebuffer.** A shaded triangle
    /// (combiner → red, alpha `0x80`) over a green background, with `force_blend`
    /// and blend modes `P = pixel`, `A = pixel-alpha`, `M = memory`, `B = 1−A`, blends
    /// 50/50: `a0 = 0x80>>3 = 16`, `a1+1 = (~0x80>>3)+1 = 16`, so each channel is
    /// `(pixel + memory)/2` → `0x7F7F00`. Without the memory read (or with blend off)
    /// it would be plain red — the green contribution proves the blender ran.
    #[test]
    fn translucent_triangle_blends_with_framebuffer() {
        let mut bus = ZBufBus {
            mem: alloc::vec![0u8; 0x1000],
            hidden: alloc::vec![0u8; 0x800],
        };
        // Pre-fill the colour image with green (0x00FF00FF) and the Z buffer far.
        for row in 0..8 {
            for x in 0..8 {
                let a = 0x200 + row * 32 + x * 4;
                bus.mem[a..a + 4].copy_from_slice(&0x00FF_00FFu32.to_be_bytes());
            }
        }
        for b in &mut bus.mem[0x400..0x500] {
            *b = 0xFF;
        }
        let mut rdp = Rdp::new();
        rdp.color_image = 0x200;
        rdp.color_image_size = 3;
        rdp.color_image_width = 8;
        rdp.z_image = 0x400;
        rdp.scissor_lrx = 8 << 2;
        rdp.scissor_lry = 8 << 2;
        rdp.other_modes.z_compare_en = true;
        rdp.other_modes.z_update_en = true;
        rdp.other_modes.force_blend = true; // -> depth_test sets blend_en
        rdp.other_modes.blend[0] = BlendCycle {
            p: 0,
            a: 0,
            m: 1,
            b: 0,
        }; // pixel, pixel-a, memory, 1-A
        rdp.combine.cyc1 = CombineCycle {
            rgb_a: 0,
            rgb_b: 0,
            rgb_c: 0,
            rgb_d: 4, // shade rgb
            a_a: 0,
            a_b: 0,
            a_c: 0,
            a_d: 4, // shade alpha
        };
        let base = 0x600usize;
        bus.mem[base + 0x10..base + 0x14].copy_from_slice(&0x0002_0000u32.to_be_bytes()); // xh
        bus.mem[base + 0x18..base + 0x1C].copy_from_slice(&0x0002_0000u32.to_be_bytes()); // xm
        bus.mem[base + 0x1C..base + 0x20].copy_from_slice(&0x0000_4000u32.to_be_bytes()); // dxmdy
        // Shade int-base: R = 0xFF, G = B = 0, A = 0x80.
        bus.mem[base + 0x20..base + 0x24].copy_from_slice(&(0xFFu32 << 16).to_be_bytes());
        bus.mem[base + 0x24..base + 0x28].copy_from_slice(&0x0000_0080u32.to_be_bytes());
        // opcode 0x0D = shade (bit 58) + z (bit 56); z block at +0x60 (near).
        bus.mem[base + 0x60..base + 0x64].copy_from_slice(&0x0800_0000u32.to_be_bytes()); // z_px 0x4000
        rdp.dispatch(0x0D, 0x0D80_0010, 0x0010_0000, base as u32, &mut bus);

        let a = 0x200 + 2 * 4; // pixel (2, 0)
        let color =
            u32::from_be_bytes([bus.mem[a], bus.mem[a + 1], bus.mem[a + 2], bus.mem[a + 3]]);
        assert_eq!(
            color, 0x7F7F_0080,
            "50/50 blend of red over green (not plain red)"
        );
    }

    /// **`unpack_rgba5551` widens each 5-bit channel by high-bit replication** and
    /// maps the 1-bit alpha to `0x00`/`0xFF` — the exact inverse of `pack_rgba5551`
    /// on the packable values. `0x1F → 0xFF`, `0x00 → 0x00`, `0x10 → 0x84`
    /// (`0b10000 << 3 | 0b10000 >> 2 = 0x80 | 0x04`).
    #[test]
    fn unpack_rgba5551_widens_by_high_bit_replication() {
        assert_eq!(unpack_rgba5551(0xFFFF), [0xFF, 0xFF, 0xFF, 0xFF]);
        assert_eq!(unpack_rgba5551(0x0000), [0x00, 0x00, 0x00, 0x00]);
        // R = 0x10 (bits 15:11), everything else zero, alpha bit set.
        assert_eq!(unpack_rgba5551(0x8001), [0x84, 0x00, 0x00, 0xFF]);
        // Pure green (G = 0x1F) with alpha — the 16-bit background the blend test uses.
        assert_eq!(unpack_rgba5551(0x07C1), [0x00, 0xFF, 0x00, 0xFF]);
        // Round-trip every packable RGBA8888 whose low bits are already truncated.
        for &v in &[0x00u8, 0x08, 0x84, 0xF8, 0xFF] {
            let packed = pack_rgba5551([v & 0xF8, 0, v & 0xF8, 0x80]);
            let un = unpack_rgba5551(packed);
            assert_eq!(pack_rgba5551(un), packed, "round-trip stable for {v:#04x}");
        }
    }

    /// **The blender also runs against a 16-bit RGBA5551 framebuffer.** The same
    /// red-over-green 50/50 blend as [`translucent_triangle_blends_with_framebuffer`]
    /// but with a 16-bit colour image, exercising `read_pixel`'s RGBA5551 decode and
    /// `write_pixel`'s repack. Memory green `0x07C1` unpacks to `0x00FF00`; the blend
    /// `0x7F7F00` repacks to `0x7BC1` (`R,G = 0x7F>>3 = 0x0F`, alpha bit `0x80>>7 = 1`).
    #[test]
    fn translucent_triangle_blends_16bit_framebuffer() {
        let mut bus = ZBufBus {
            mem: alloc::vec![0u8; 0x1000],
            hidden: alloc::vec![0u8; 0x800],
        };
        // Pre-fill the 16-bit colour image with green (RGBA5551 0x07C1); Z buffer far.
        for row in 0..8 {
            for x in 0..8 {
                let a = 0x200 + row * 16 + x * 2;
                bus.mem[a..a + 2].copy_from_slice(&0x07C1u16.to_be_bytes());
            }
        }
        for b in &mut bus.mem[0x400..0x500] {
            *b = 0xFF;
        }
        let mut rdp = Rdp::new();
        rdp.color_image = 0x200;
        rdp.color_image_size = 2; // 16-bit RGBA5551
        rdp.color_image_width = 8;
        rdp.z_image = 0x400;
        rdp.scissor_lrx = 8 << 2;
        rdp.scissor_lry = 8 << 2;
        rdp.other_modes.z_compare_en = true;
        rdp.other_modes.z_update_en = true;
        rdp.other_modes.force_blend = true;
        rdp.other_modes.blend[0] = BlendCycle {
            p: 0,
            a: 0,
            m: 1,
            b: 0,
        };
        rdp.combine.cyc1 = CombineCycle {
            rgb_a: 0,
            rgb_b: 0,
            rgb_c: 0,
            rgb_d: 4,
            a_a: 0,
            a_b: 0,
            a_c: 0,
            a_d: 4,
        };
        let base = 0x600usize;
        bus.mem[base + 0x10..base + 0x14].copy_from_slice(&0x0002_0000u32.to_be_bytes()); // xh
        bus.mem[base + 0x18..base + 0x1C].copy_from_slice(&0x0002_0000u32.to_be_bytes()); // xm
        bus.mem[base + 0x1C..base + 0x20].copy_from_slice(&0x0000_4000u32.to_be_bytes()); // dxmdy
        bus.mem[base + 0x20..base + 0x24].copy_from_slice(&(0xFFu32 << 16).to_be_bytes()); // R=0xFF
        bus.mem[base + 0x24..base + 0x28].copy_from_slice(&0x0000_0080u32.to_be_bytes()); // A=0x80
        bus.mem[base + 0x60..base + 0x64].copy_from_slice(&0x0800_0000u32.to_be_bytes()); // z_px
        rdp.dispatch(0x0D, 0x0D80_0010, 0x0010_0000, base as u32, &mut bus);

        let a = 0x200 + 2 * 2; // pixel (2, 0), bpp = 2
        let color = u16::from_be_bytes([bus.mem[a], bus.mem[a + 1]]);
        assert_eq!(
            color, 0x7BC1,
            "50/50 blend of red over green, repacked to RGBA5551"
        );
    }

    /// **The blender's `A`-select 2 takes the interpolated shade alpha, not the
    /// combiner output alpha.** These are independent inputs; this test forces them
    /// apart so a regression that fed `color[3]` back in would be caught. The alpha
    /// combiner outputs the env alpha (`0xF0`) while the interpolated shade alpha is
    /// `0x80`; the blender selects `A = 2` (shade alpha). With shade alpha `0x80`,
    /// `a0 = 16` / `a1 + 1 = 16` gives the 50/50 red-over-green `0x7F7F00`. Had the
    /// combiner alpha `0xF0` leaked in, `a0 = 30` / `a1 + 1 = 2` would give
    /// `0xEF0F00` — a distinct value, so the test mutation-checks the fix.
    #[test]
    fn blender_shade_alpha_is_interpolated_not_combiner_output() {
        let mut bus = ZBufBus {
            mem: alloc::vec![0u8; 0x1000],
            hidden: alloc::vec![0u8; 0x800],
        };
        for row in 0..8 {
            for x in 0..8 {
                let a = 0x200 + row * 32 + x * 4;
                bus.mem[a..a + 4].copy_from_slice(&0x00FF_00FFu32.to_be_bytes()); // green
            }
        }
        for b in &mut bus.mem[0x400..0x500] {
            *b = 0xFF;
        }
        let mut rdp = Rdp::new();
        rdp.color_image = 0x200;
        rdp.color_image_size = 3;
        rdp.color_image_width = 8;
        rdp.z_image = 0x400;
        rdp.env_color = 0x0000_00F0; // env alpha = 0xF0 (the combiner output alpha)
        rdp.scissor_lrx = 8 << 2;
        rdp.scissor_lry = 8 << 2;
        rdp.other_modes.z_compare_en = true;
        rdp.other_modes.z_update_en = true;
        rdp.other_modes.force_blend = true;
        rdp.other_modes.blend[0] = BlendCycle {
            p: 0,
            a: 2, // shade alpha -- the input under test
            m: 1,
            b: 0,
        };
        rdp.combine.cyc1 = CombineCycle {
            rgb_a: 0,
            rgb_b: 0,
            rgb_c: 0,
            rgb_d: 4, // shade rgb -> red
            a_a: 7,
            a_b: 7,
            a_c: 7,
            a_d: 5, // env alpha -> combiner output alpha = 0xF0 (!= shade alpha 0x80)
        };
        let base = 0x600usize;
        bus.mem[base + 0x10..base + 0x14].copy_from_slice(&0x0002_0000u32.to_be_bytes()); // xh
        bus.mem[base + 0x18..base + 0x1C].copy_from_slice(&0x0002_0000u32.to_be_bytes()); // xm
        bus.mem[base + 0x1C..base + 0x20].copy_from_slice(&0x0000_4000u32.to_be_bytes()); // dxmdy
        bus.mem[base + 0x20..base + 0x24].copy_from_slice(&(0xFFu32 << 16).to_be_bytes()); // R=0xFF
        bus.mem[base + 0x24..base + 0x28].copy_from_slice(&0x0000_0080u32.to_be_bytes()); // shade A=0x80
        bus.mem[base + 0x60..base + 0x64].copy_from_slice(&0x0800_0000u32.to_be_bytes()); // z_px
        rdp.dispatch(0x0D, 0x0D80_0010, 0x0010_0000, base as u32, &mut bus);

        let a = 0x200 + 2 * 4; // pixel (2, 0)
        let color =
            u32::from_be_bytes([bus.mem[a], bus.mem[a + 1], bus.mem[a + 2], bus.mem[a + 3]]);
        assert_eq!(
            color, 0x7F7F_00F0,
            "shade alpha 0x80 drives a 50/50 blend; combiner alpha 0xF0 only rides in the output byte"
        );
    }

    /// **`quantize_x` maps `s.16` edge X to the `s.3` coverage domain with the
    /// sticky bit.** An integer pixel `p` maps to `p << 3` (`p·8`); any discarded
    /// fraction bit forces the low bit set so the coordinate stays strictly inside
    /// the half-open span. Negative coordinates arithmetic-shift toward −∞.
    #[test]
    fn quantize_x_maps_to_subpixel_domain_with_sticky() {
        assert_eq!(quantize_x(5 << 16), 40, "pixel 5 -> 5*8");
        // Pixel 5 + a small fraction (0x1000, below sub-pixel resolution): 40 | sticky.
        assert_eq!(
            quantize_x((5 << 16) | 0x1000),
            41,
            "sticky bit forces the LSB"
        );
        // Exactly half a pixel: sub-pixel offset 4 within pixel 5, no discarded bits.
        assert_eq!(quantize_x((5 << 16) | (1 << 15)), 44, "pixel 5.5 -> 44");
        assert_eq!(
            quantize_x(-(3 << 16)),
            -24,
            "pixel -3 -> -24 (arithmetic shift)"
        );
    }

    /// **`compute_coverage` yields a full mask for an interior pixel and a partial
    /// mask at an edge.** With the left edge quantised to sub-pixel `43` (between
    /// X-samples 2 and 4), pixel 5's samples at offsets `{0, 2}` fall outside and
    /// `{4, 6}` inside, so the four Y-subpixels each cover their high sample only:
    /// mask `0xAA`, count 4. A fully-enclosed pixel is `0xFF` (8); a fully-excluded
    /// one is `0` (hand-computed against `coverage.h:31-44`).
    #[test]
    fn compute_coverage_full_partial_and_empty() {
        // Fully inside: left edge at 0, right edge far away.
        assert_eq!(compute_coverage([0; 4], [800; 4], 5), 0xFF);
        assert_eq!(compute_coverage([0; 4], [800; 4], 5).count_ones(), 8);
        // Fully outside: right edge behind the pixel.
        assert_eq!(compute_coverage([800; 4], [0; 4], 5), 0x00);
        // Left edge at sub-pixel 43: each Y-subpixel's low X-sample (offset 0 or 2)
        // is outside and its high sample (4 or 6) inside, so every Y-subpixel keeps
        // its odd bit only -> 0xAA.
        let mask = compute_coverage([43; 4], [800; 4], 5);
        assert_eq!(mask, 0xAA, "high sample of each Y-subpixel covered");
        assert_eq!(mask.count_ones(), 4);
    }

    /// **The mask bit layout is `2·Ysub + Xsample`, and the X-sample offsets
    /// alternate by Y-subpixel.** Covering only Y-subpixel 0 sets bits `{0, 1}`
    /// (`0x03`), proving the two X-samples of a Y-subpixel occupy adjacent bits.
    /// A uniform left edge at sub-pixel `41` then discriminates the diamond
    /// offsets: pixel 5's samples are at `{40, 44}` for Y-subpixels 0/2 and
    /// `{42, 46}` for 1/3, so 0/2 lose their offset-0 sample (`40 < 41`) while 1/3
    /// keep both — mask `0xEE`. (Hand-computed against `coverage.h:31-44`.)
    #[test]
    fn compute_coverage_bit_layout_and_diamond_offsets() {
        // Only Y-subpixel 0's span is valid; the others are poisoned (inverted).
        let only_y0 = compute_coverage([0, 800, 800, 800], [800, 0, 0, 0], 5);
        assert_eq!(only_y0, 0x03, "Y-subpixel 0 occupies bits 0 and 1");
        // Uniform left edge at 41: 0/2 (offsets 0,4) drop offset 0; 1/3 (offsets 2,6) keep both.
        let mask = compute_coverage([41; 4], [800; 4], 5);
        assert_eq!(
            mask, 0xEE,
            "Y0/Y2 lose their offset-0 sample; Y1/Y3 keep both"
        );
        assert_eq!(mask.count_ones(), 6);
    }
}
