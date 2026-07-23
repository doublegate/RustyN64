// Standalone Angrylion RDP driver — generates golden conformance vectors.
//
// Runs the Angrylion N64 RDP software renderer (CPU-only, the accuracy oracle)
// over hand-written RDP command lists and dumps each rendered framebuffer as a
// self-describing `.rvec` vector: header + command-list bytes + golden pixels.
// The RustyN64 test harness replays the same command list through its own RDP
// and asserts a byte-for-byte match.
//
// LICENCE NOTE: Angrylion-rdp-plus is the non-commercial MAME-licensed study
// oracle (ref-proj/, gitignored). This tool and its Angrylion build stay OUT of
// the committed tree; only the *output* vectors (command stream + golden frame,
// both freely committable) are checked in. See ref-proj/README.md.

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdbool.h>
#include <stdarg.h>

#include "n64video.h"
#include "vdac.h"

// ---- Required external stubs (the core links against these) ----

void msg_error(const char *err, ...) {
    // Print and continue — a malformed command must be recorded, not fatal.
    va_list ap;
    va_start(ap, err);
    fprintf(stderr, "[angrylion msg_error] ");
    vfprintf(stderr, err, ap);
    fprintf(stderr, "\n");
    va_end(ap);
}
void msg_warning(const char *err, ...) { (void)err; }
void msg_debug(const char *err, ...) { (void)err; }

void vdac_init(struct n64video_config *config) { (void)config; }
void vdac_write(struct frame_buffer *fb) { (void)fb; }
void vdac_sync(bool invalid) { (void)invalid; }
void vdac_close(void) {}

static void mi_intr_cb(void) {}

// ---- RDRAM + register backing store ----

#define RDRAM_SIZE 0x800000u // 8 MiB (RDRAM_MAX_SIZE)

static uint8_t g_rdram[RDRAM_SIZE];
static uint32_t g_vi_regs[VI_NUM_REG];
static uint32_t g_dp_regs[DP_NUM_REG];
static uint32_t g_irq_reg;
static uint32_t *g_p_vi[VI_NUM_REG];
static uint32_t *g_p_dp[DP_NUM_REG];

static struct n64video_config g_config;

static void engine_init(void) {
    memset(g_rdram, 0, sizeof(g_rdram));
    memset(g_vi_regs, 0, sizeof(g_vi_regs));
    memset(g_dp_regs, 0, sizeof(g_dp_regs));
    g_irq_reg = 0;
    for (unsigned i = 0; i < VI_NUM_REG; i++) g_p_vi[i] = &g_vi_regs[i];
    for (unsigned i = 0; i < DP_NUM_REG; i++) g_p_dp[i] = &g_dp_regs[i];

    memset(&g_config, 0, sizeof(g_config));
    g_config.gfx.rdram = g_rdram;
    g_config.gfx.rdram_size = RDRAM_SIZE;
    g_config.gfx.dmem = NULL;
    g_config.gfx.vi_reg = g_p_vi;
    g_config.gfx.dp_reg = g_p_dp;
    g_config.gfx.mi_intr_reg = &g_irq_reg;
    g_config.gfx.mi_intr_cb = mi_intr_cb;
    g_config.vi.mode = VI_MODE_NORMAL;
    g_config.vi.interp = VI_INTERP_LINEAR;
    g_config.dp.compat = DP_COMPAT_HIGH;
    g_config.parallel = false; // single-threaded => bit-deterministic
    g_config.num_workers = 0;

    n64video_init(&g_config);
}

// Write one 32-bit command word into RDRAM. 32-bit access has no byte-swap XOR,
// so a host u32 stored at word index (addr>>2) is read back verbatim by the RDP.
static void rdram_put_word(uint32_t byte_addr, uint32_t word) {
    // `byte_addr > RDRAM_SIZE - 4`, not `byte_addr + 4 > RDRAM_SIZE`, so a
    // near-UINT32_MAX address cannot wrap past the check.
    if (byte_addr > RDRAM_SIZE - 4) {
        fprintf(stderr, "rdram_put_word: address 0x%X out of bounds\n", byte_addr);
        exit(1);
    }
    ((uint32_t *)g_rdram)[byte_addr >> 2] = word;
}

// fwrite that aborts on a short write (e.g. out of disk space) rather than
// silently producing a truncated/corrupt .rvec.
static void wr(const void *buf, size_t n, FILE *f) {
    if (fwrite(buf, 1, n, f) != n) {
        perror("fwrite");
        exit(1);
    }
}

// Read a 16-bit framebuffer pixel (RGBA5551) as its logical N64 value.
static uint16_t read_fb16(uint32_t fb_addr, uint32_t x, uint32_t y, uint32_t w) {
    uint32_t idx16 = (fb_addr >> 1) + y * w + x;
    if (idx16 >= RDRAM_SIZE / 2) {
        fprintf(stderr, "read_fb16: pixel out of RDRAM bounds\n");
        exit(1);
    }
    return ((const uint16_t *)g_rdram)[idx16 ^ 1]; // WORD_ADDR_XOR
}
// Read a 32-bit framebuffer pixel (RGBA8888) as its logical N64 value (0xRRGGBBCC).
static uint32_t read_fb32(uint32_t fb_addr, uint32_t x, uint32_t y, uint32_t w) {
    uint32_t idx32 = (fb_addr >> 2) + y * w + x;
    if (idx32 >= RDRAM_SIZE / 4) {
        fprintf(stderr, "read_fb32: pixel out of RDRAM bounds\n");
        exit(1);
    }
    return ((const uint32_t *)g_rdram)[idx32]; // no XOR at 32-bit
}

// ---- Vector definition ----

typedef struct {
    const char *name;
    uint32_t cmd_addr;
    uint32_t fb_addr;
    uint32_t width;
    uint32_t height;
    uint32_t bpp;       // 2 (RGBA5551) or 4 (RGBA8888)
    uint32_t n_words;   // number of 32-bit command words
    const uint32_t *words;
    // Optional 16-bit texture preload placed in RDRAM before the command list
    // runs (so a Load Tile has something to read). When `n_texels == 0` the vector
    // is emitted in the v1 format (no preload region) and stays byte-identical to
    // the pre-preload generator. `texels` are logical RGBA5551 values.
    uint32_t preload_addr;
    uint32_t n_texels;
    const uint16_t *texels;
} Vector;

// Place a run of logical 16-bit texels into Angrylion RDRAM starting at `byte_addr`.
// Angrylion's Load Tile reads the texture as **32-bit words with no byte-swap**
// (`RREADIDX32` = `rdram32[idx]`, tex.c), exactly like the command list — NOT via
// 16-bit halfword access. So two texels pack big-endian into one word (`hi16 <<
// 16 | lo16`); this is the same byte stream RustyN64 reads big-endian from the
// `.rvec` preload region, so both renderers see identical texels.
static void rdram_put_texels16(uint32_t byte_addr, const uint16_t *texels, uint32_t n) {
    for (uint32_t i = 0; i < n; i += 2) {
        uint32_t hi = texels[i];
        uint32_t lo = (i + 1 < n) ? texels[i + 1] : 0u;
        rdram_put_word(byte_addr + i * 2u, (hi << 16) | lo);
    }
}

// Render one vector through Angrylion and write its `.rvec` file.
static int emit_vector(const Vector *v, const char *out_dir) {
    engine_init();

    // Place the optional texture preload into RDRAM (before the command list, so a
    // Load Tile reads it). Written as 32-bit words, matching Angrylion's Load Tile.
    uint32_t preload_len = v->n_texels * 2u;
    rdram_put_texels16(v->preload_addr, v->texels, v->n_texels);

    // Place the command list into RDRAM.
    for (uint32_t i = 0; i < v->n_words; i++) {
        rdram_put_word(v->cmd_addr + i * 4, v->words[i]);
    }

    uint32_t cmd_len = v->n_words * 4;
    g_dp_regs[DP_STATUS] = 0; // clear XBUS/FREEZE/FLUSH
    g_dp_regs[DP_START] = v->cmd_addr;
    g_dp_regs[DP_CURRENT] = v->cmd_addr;
    g_dp_regs[DP_END] = v->cmd_addr + cmd_len;
    n64video_process_list();

    uint32_t fb_len = v->width * v->height * v->bpp;

    char path[512];
    int need = snprintf(path, sizeof(path), "%s/%s.rvec", out_dir, v->name);
    if (need < 0 || (size_t)need >= sizeof(path)) {
        fprintf(stderr, "path too long for vector %s\n", v->name);
        return 1;
    }
    FILE *f = fopen(path, "wb");
    if (!f) { perror("fopen"); return 1; }

    // Header. A vector with no preload is emitted in the v1 format (9 u32:
    // magic, version=1, fb_addr, width, height, bpp, cmd_addr, cmd_len, fb_len)
    // so the pre-preload vectors stay byte-identical. A vector with a preload uses
    // the v2 format (11 u32: the v1 fields with version=2, plus preload_addr and
    // preload_len), and the preload bytes precede the command list in the body.
    if (preload_len == 0) {
        uint32_t hdr[9] = {0x52564543u, 1u, v->fb_addr, v->width, v->height,
                           v->bpp, v->cmd_addr, cmd_len, fb_len};
        for (int i = 0; i < 9; i++) {
            uint8_t be[4] = {hdr[i] >> 24, hdr[i] >> 16, hdr[i] >> 8, hdr[i]};
            wr(be, 4, f);
        }
    } else {
        uint32_t hdr[11] = {0x52564543u, 2u, v->fb_addr, v->width, v->height,
                            v->bpp, v->cmd_addr, cmd_len, fb_len,
                            v->preload_addr, preload_len};
        for (int i = 0; i < 11; i++) {
            uint8_t be[4] = {hdr[i] >> 24, hdr[i] >> 16, hdr[i] >> 8, hdr[i]};
            wr(be, 4, f);
        }
        // Preload region: the logical texels, big-endian (RustyN64's RDRAM layout).
        for (uint32_t i = 0; i < v->n_texels; i++) {
            uint8_t be[2] = {v->texels[i] >> 8, v->texels[i]};
            wr(be, 2, f);
        }
    }
    // Command list (big-endian words, matching RustyN64's write_cmd layout).
    for (uint32_t i = 0; i < v->n_words; i++) {
        uint32_t w = v->words[i];
        uint8_t be[4] = {w >> 24, w >> 16, w >> 8, w};
        wr(be, 4, f);
    }
    // Golden framebuffer: logical pixel values, row-major, big-endian.
    for (uint32_t y = 0; y < v->height; y++) {
        for (uint32_t x = 0; x < v->width; x++) {
            if (v->bpp == 2) {
                uint16_t p = read_fb16(v->fb_addr, x, y, v->width);
                uint8_t be[2] = {p >> 8, p};
                wr(be, 2, f);
            } else {
                uint32_t p = read_fb32(v->fb_addr, x, y, v->width);
                uint8_t be[4] = {p >> 24, p >> 16, p >> 8, p};
                wr(be, 4, f);
            }
        }
    }
    if (fclose(f) != 0) { perror("fclose"); return 1; }
    n64video_close();
    fprintf(stderr, "wrote %s (%u words cmd, %ux%u %ubpp)\n", path,
            v->n_words, v->width, v->height, v->bpp);
    return 0;
}

// ---- Command-block helpers ----
//
// The shade / texture / z attribute blocks of a Fill Triangle (0x08-0x0F) MUST be
// their full length, or the command is silently shortened, the following blocks
// misalign, and Angrylion renders a BLANK frame (this cost hours once — a shade
// block written 14 words instead of 16). These macros expand to the exact word
// count so a block can never be short by construction.

// Pack two signed 16-bit coefficient halves into one command word (hi | lo).
#define HALVES(hi16, lo16) \
    ((((uint32_t)(hi16) & 0xFFFFu) << 16) | ((uint32_t)(lo16) & 0xFFFFu))

// A full 16-word (8-u64) shade block: integer RGBA base, per-x (dx) integer
// deltas, and per-major-edge (de) integer deltas; all fractional words are zero.
// Word order matches RustyN64's decode: base_int, dx_int, base_frac, dx_frac,
// de_int, dy_int, de_frac, dy_frac (two words each).
#define SHADE_BLOCK(br, bg, bb, ba, dxr, dxg, dxb, dxa, der, deg, deb, dea) \
    HALVES(br, bg), HALVES(bb, ba),   /* int base   R,G | B,A */             \
        HALVES(dxr, dxg), HALVES(dxb, dxa), /* dx  int */                    \
        0u, 0u,                             /* base frac */                  \
        0u, 0u,                             /* dx  frac */                   \
        HALVES(der, deg), HALVES(deb, dea), /* de  int */                    \
        0u, 0u,                             /* dy  int */                    \
        0u, 0u,                             /* de  frac */                   \
        0u, 0u                              /* dy  frac */

// A flat (constant-colour) shade block: base only, no deltas.
#define SHADE_BLOCK_FLAT(r, g, b, a) SHADE_BLOCK(r, g, b, a, 0, 0, 0, 0, 0, 0, 0, 0)

// A full 4-word z-suffix: z (s15.16, high word), then dzdx, dzde, dzdy (all 0).
#define Z_SUFFIX(z) (uint32_t)(z), 0u, 0u, 0u

// A full 16-word texture-coordinate block: S/T/W integer base, per-x (dx) and
// per-major-edge (de) integer deltas; fractional words zero. Each 64-bit word
// packs S in bits 63:48, T in 47:32, W in 31:16 (S,T in the hi u32, W in the lo).
#define TEX_BLOCK(bs, bt, bw, dxs, dxt, dxw, des, det, dew)  \
    HALVES(bs, bt), HALVES(bw, 0),      /* int base   S,T | W */ \
        HALVES(dxs, dxt), HALVES(dxw, 0), /* dx  int */           \
        0u, 0u,                           /* base frac */         \
        0u, 0u,                           /* dx  frac */          \
        HALVES(des, det), HALVES(dew, 0), /* de  int */           \
        0u, 0u,                           /* dy  int */           \
        0u, 0u,                           /* de  frac */          \
        0u, 0u                            /* dy  frac */

// ---- Vectors ----
// V1: a FILL-mode Fill Rectangle over an 8x8 RGBA5551 image (green 0x07C1).
// Both renderers write the fill verbatim, so this proves the harness plumbing
// and byte order with a guaranteed 0-diff.
static const uint32_t V1_FILL_RECT_16[] = {
    0x2F300000u, 0x00000000u, // Set Other Modes: cycle_type = FILL (bits 21:20 = 3)
    0x3F100007u, 0x00001000u, // Set Color Image: size=2(16b), width-1=7, addr=0x1000
    0x37000000u, 0x07C107C1u, // Set Fill Color: green 0x07C1 in both halves
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)  [XL=32, YL=32]
    0x36020020u, 0x00000000u, // Fill Rectangle: (0,0)-(8,8)
};

// V2: a FILL-mode Fill Triangle (0x08) — a left-major triangle with a vertical
// left edge at x=2 and a hypotenuse of slope DxMDy=0.25 px per *pixel* row over
// rows 0-3, filled green. Angrylion renders a near-vertical line (right edge only
// reaches x~2.75); this vector caught RustyN64's 4x edge-slope bug (ledger R-14),
// which draws a staircase instead. The 4-u64-word (8-u32) edge-coefficient block
// matches RustyN64's flat-fill test geometry: yl=ym=16, yh=0, flip=1, xh=xm=2.0.
static const uint32_t V2_FILL_TRI_16[] = {
    0x2F300000u, 0x00000000u, // Set Other Modes: cycle_type = FILL
    0x3F100007u, 0x00001000u, // Set Color Image: 16-bit, width 8, addr 0x1000
    0x37000000u, 0x07C107C1u, // Set Fill Color: green 0x07C1
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)
    // Fill Triangle 0x08 (8 u32 words): word0/1 = flags+YL/YM/YH; then
    // XL,DxLDy,XH,DxHDy,XM,DxMDy.
    0x08800010u, 0x00100000u, // op=0x08, lft=1, yl=16, ym=16, yh=0
    0x00000000u, 0x00000000u, // XL = 0, DxLDy = 0
    0x00020000u, 0x00000000u, // XH = 2.0, DxHDy = 0
    0x00020000u, 0x00004000u, // XM = 2.0, DxMDy = 0.25
};

// V3: a WIDER FILL-mode Fill Triangle — same left-major shape but DxMDy = 1.0
// px/pixel-row, so the hypotenuse spans several columns over the 8 rows (a real
// multi-pixel staircase), exercising the edge-walk across columns.
static const uint32_t V3_FILL_TRI_WIDE_16[] = {
    0x2F300000u, 0x00000000u, // Set Other Modes: cycle_type = FILL
    0x3F100007u, 0x00001000u, // Set Color Image: 16-bit, width 8, addr 0x1000
    0x37000000u, 0x07C107C1u, // Set Fill Color: green
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)
    0x08800020u, 0x00200000u, // op=0x08, lft=1, yl=32, ym=32, yh=0  (8 rows)
    0x00000000u, 0x00000000u, // XL = 0, DxLDy = 0
    0x00020000u, 0x00000000u, // XH = 2.0, DxHDy = 0
    0x00020000u, 0x00010000u, // XM = 2.0, DxMDy = 1.0
};

// V4: a right-major Fill Triangle with a NEGATIVE minor-edge slope — the left
// edge leans left as y increases (DxMDy = -1.0), the right edge vertical at x=5.
// Exercises sign-extension + the arithmetic `>> 2` (rounds toward -inf) on a
// negative slope against Angrylion. The 30-bit slope field for -1.0 is
// 0x3FFF0000 (two's complement of 0x10000 in bits 29:0).
static const uint32_t V4_FILL_TRI_NEG_16[] = {
    0x2F300000u, 0x00000000u, // Set Other Modes: cycle_type = FILL
    0x3F100007u, 0x00001000u, // Set Color Image: 16-bit, width 8, addr 0x1000
    0x37000000u, 0x07C107C1u, // Set Fill Color: green
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)
    0x08000020u, 0x00200000u, // op=0x08, lft=0 (right-major), yl=32, ym=32, yh=0
    0x00000000u, 0x00000000u, // XL = 0, DxLDy = 0
    0x00050000u, 0x00000000u, // XH = 5.0 (right edge), DxHDy = 0
    0x00050000u, 0x3FFF0000u, // XM = 5.0 (left edge), DxMDy = -1.0
};

// V5: a FRACTIONAL-edge triangle — a left-major "rectangle" whose left edge sits
// at x=2.5 and right edge at x=6.5, both vertical, full height. With AA off the
// RDP draws a pixel only when its top-left sub-sample (at integer x) lies in the
// half-open span [2.5, 6.5): pixel 2's sample (2.0) is outside, so column 2 is NOT
// drawn (columns 3-6 are). RustyN64's whole-pixel union approximation wrongly
// includes column 2 — this vector drives the 2c sub-pixel coverage rewrite.
static const uint32_t V5_FILL_TRI_FRAC_16[] = {
    0x2F300000u, 0x00000000u, // Set Other Modes: cycle_type = FILL
    0x3F100007u, 0x00001000u, // Set Color Image: 16-bit, width 8, addr 0x1000
    0x37000000u, 0x07C107C1u, // Set Fill Color: green
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)
    0x08800020u, 0x00200000u, // op=0x08, lft=1, yl=32, ym=32, yh=0
    0x00000000u, 0x00000000u, // XL = 0, DxLDy = 0
    0x00028000u, 0x00000000u, // XH = 2.5 (left edge), DxHDy = 0
    0x00068000u, 0x00000000u, // XM = 6.5 (right edge), DxMDy = 0
};

// V6: a 1-CYCLE-mode shaded triangle with a fractional left edge (x=2.5). Unlike
// FILL mode, 1-cycle mode renders with sub-pixel accuracy: with AA off, a pixel
// draws only when its top-left sub-sample (integer x) is inside the span, so
// column 2 (sample at 2.0 < 2.5) is EXCLUDED — columns 3-6 draw. The colour is the
// combiner output (a flat red shade), not the fill register. This drives the 2c
// sub-pixel coverage rewrite (RustyN64's whole-pixel union wrongly includes col 2).
static const uint32_t V6_SHADE_TRI_FRAC_16[] = {
    0x2F000000u, 0x00000000u, // Set Other Modes: cycle_type=0 (1-cycle), AA off
    0x3C000000u, 0x00000104u, // Set Combine Mode: cyc1 rgb_d=4/a_d=4 (shade passthrough)
    0x3F100007u, 0x00001000u, // Set Color Image: 16-bit, width 8, addr 0x1000
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)
    // Fill Shaded Triangle 0x0C: 4-word base + 8-word shade block.
    0x0C800020u, 0x00200000u, // op=0x0C (shade), lft=1, yl=32, ym=32, yh=0
    0x00000000u, 0x00000000u, // XL = 0, DxLDy = 0
    0x00028000u, 0x00000000u, // XH = 2.5 (left edge), DxHDy = 0
    0x00068000u, 0x00000000u, // XM = 6.5 (right edge), DxMDy = 0
    SHADE_BLOCK_FLAT(0xFF, 0x00, 0x00, 0xFF), // flat red shade (16 words)
};

// V7: a 1-cycle shaded triangle with a z-suffix (z_update on, z_compare off) and
// the same fractional edges as V6 (2.5/6.5). Angrylion applies the identical
// sub-pixel coverage on the depth path — column 2 excluded, column 6 partial — so
// the colour output equals V6's. The z (0x0800_0000, near) writes the z buffer at
// 0x1800; the depth test passes (z_compare off). Drives the depth-path coverage.
static const uint32_t V7_SHADE_DEPTH_TRI_FRAC_16[] = {
    0x2F000000u, 0x00000020u, // Set Other Modes: 1-cycle, AA off, z_update_en (bit 5)
    0x3C000000u, 0x00000104u, // Set Combine Mode: shade passthrough
    0x3F100007u, 0x00001000u, // Set Color Image: 16-bit, width 8, addr 0x1000
    0x3E000000u, 0x00001800u, // Set Depth Image: z buffer at 0x1800
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)
    0x0D800020u, 0x00200000u, // op=0x0D (shade+z), lft=1, yl=32, ym=32, yh=0
    0x00000000u, 0x00000000u, // XL, DxLDy
    0x00028000u, 0x00000000u, // XH = 2.5, DxHDy = 0
    0x00068000u, 0x00000000u, // XM = 6.5, DxMDy = 0
    SHADE_BLOCK_FLAT(0xFF, 0x00, 0x00, 0xFF), // flat red shade (16 words)
    Z_SUFFIX(0x08000000),                     // z = 0x0800_0000 (near), dz* = 0
};

// V8: a 1-cycle 32-bit (RGBA8888) shaded triangle — the wide DxMDy=1.0 staircase,
// flat shade R=0x11 G=0x22 B=0x33. Exercises the 32-bit colour path; the stored
// alpha byte holds sub-pixel coverage (cov<<5), so a fully-covered interior pixel
// is 0x112233E0. (The full 16-u32 shade block is required — a short block would
// misalign nothing here but shorten the command; see shade_depth_tri_frac_16.)
static const uint32_t V8_SHADE_TRI_32[] = {
    0x2F0000F0u, 0x00000000u, // Set Other Modes: 1-cycle, AA off, RGB+alpha dither OFF (hi 7:4=1111)
    0x3C000000u, 0x00000104u, // Set Combine Mode: shade passthrough
    0x3F180007u, 0x00001000u, // Set Color Image: 32-bit (size=3), width 8, addr 0x1000
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)
    0x0C800020u, 0x00200000u, // op=0x0C (shade), lft=1, yl=32, ym=32, yh=0
    0x00000000u, 0x00000000u, // XL, DxLDy
    0x00020000u, 0x00000000u, // XH = 2.0
    0x00020000u, 0x00010000u, // XM = 2.0, DxMDy = 1.0
    SHADE_BLOCK_FLAT(0x11, 0x22, 0x33, 0xFF), // flat shade R11 G22 B33 (16 words)
};

// V9: a 32-bit shaded triangle with MAGIC dither ON (the default, hi bits 7:4=0)
// and a flat shade R=0x11 G=0x22 B=0x33. Each channel is dithered per-pixel by the
// 4x4 magic matrix, so interior pixels vary (0x33->0x38 where the matrix cell <
// the channel's low 3 bits). Drives the dither implementation.
static const uint32_t V9_DITHER_TRI_32[] = {
    0x2F000000u, 0x00000000u, // Set Other Modes: 1-cycle, AA off, magic RGB+alpha dither (default)
    0x3C000000u, 0x00000104u, // Set Combine Mode: shade passthrough
    0x3F180007u, 0x00001000u, // Set Color Image: 32-bit, width 8, addr 0x1000
    0x2D000000u, 0x00020020u, // Set Scissor
    0x0C800020u, 0x00200000u, // op=0x0C (shade), lft=1, yl=32, ym=32, yh=0
    0x00000000u, 0x00000000u, // XL, DxLDy
    0x00020000u, 0x00000000u, // XH = 2.0
    0x00020000u, 0x00010000u, // XM = 2.0, DxMDy = 1.0
    SHADE_BLOCK_FLAT(0x11, 0x22, 0x33, 0xFF), // flat shade R11 G22 B33 (16 words)
};

// V10: a 32-bit Gouraud-gradient shaded triangle (dither off) — the FIRST vector
// to exercise shade *interpolation* rather than a flat colour. R has a per-x
// gradient (dx.R = -0x10 per pixel, from base 0xF0) and G a per-major-edge
// gradient (de.G = +0x08 per scanline, from base 0x40); B is flat 0x80. Angrylion
// defines the golden, so this validates RustyN64's `interpolate_shade` (base + dx
// + de, i16 snap) end-to-end against the oracle. Dither is off (hi 7:4 = 1111) so
// the gradient is isolated from the dither round-up.
static const uint32_t V10_SHADE_GRAD_TRI_32[] = {
    0x2F0000F0u, 0x00000000u, // Set Other Modes: 1-cycle, AA off, dither OFF
    0x3C000000u, 0x00000104u, // Set Combine Mode: shade passthrough
    0x3F180007u, 0x00001000u, // Set Color Image: 32-bit, width 8, addr 0x1000
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)
    0x0C800020u, 0x00200000u, // op=0x0C (shade), lft=1, yl=32, ym=32, yh=0
    0x00000000u, 0x00000000u, // XL, DxLDy
    0x00020000u, 0x00000000u, // XH = 2.0
    0x00020000u, 0x00010000u, // XM = 2.0, DxMDy = 1.0
    // base R=0xF0 G=0x40 B=0x80 A=0xFF; dx.R=-0x10; de.G=+0x08.
    SHADE_BLOCK(0xF0, 0x40, 0x80, 0xFF, -0x10, 0, 0, 0, 0, 0x08, 0, 0),
};

// V11: a 1-cycle TEXTURED triangle (16-bit RGBA5551), the first vector to validate
// texture sampling against Angrylion (every prior texture check was an internal
// round-trip). An 8x1 texture of eight distinct texels is preloaded into RDRAM at
// 0x3000, loaded into TMEM by Load Tile, and sampled across the triangle by a per-x
// S gradient (dx.S = 1.0/pixel, T flat) so each column reads a different texel. The
// combiner passes texel0 straight through (rgb_d = a_d = texel0). Perspective off.
// This exercises Set Texture Image / Set Tile / Set Tile Size / Load Tile /
// interpolate_st / fetch_texel end-to-end; Angrylion defines the golden.
static const uint16_t TEX8_RAMP[8] = {
    0xF801u, 0x07C1u, 0x003Fu, 0xFFFFu, // red, green, blue, white
    0xF83Fu, 0x07FFu, 0xFFC1u, 0x8421u, // magenta, cyan, yellow, grey
};
static const uint32_t V11_TEX_TRI_16[] = {
    0x2F0000F0u, 0x00000000u, // Set Other Modes: 1-cycle, AA off, dither off, persp off
    0x3C000000u, 0x00000041u, // Set Combine Mode: rgb_d=1 / a_d=1 (texel0 passthrough)
    0x3D100007u, 0x00003000u, // Set Texture Image: 16-bit, width 8, addr 0x3000
    0x35100400u, 0x00000030u, // Set Tile 0: 16-bit, line=2 (64-bit words), tmem=0, mask_s=3
    0x32000000u, 0x0001C000u, // Set Tile Size 0: SL=0 TL=0 SH=7 TH=0 (u10.2)
    0x34000000u, 0x0001C000u, // Load Tile 0: SL=0 TL=0 SH=7 TH=0
    0x3F100007u, 0x00001000u, // Set Color Image: 16-bit, width 8, addr 0x1000
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)
    0x0A800020u, 0x00200000u, // op=0x0A (tex), lft=1, yl=32, ym=32, yh=0, tile 0
    0x00000000u, 0x00000000u, // XL, DxLDy
    0x00020000u, 0x00000000u, // XH = 2.0
    0x00020000u, 0x00010000u, // XM = 2.0, DxMDy = 1.0
    // S base 0, T base 0, W base 1.0; dx.S = 1.0 (one texel per pixel); rest 0.
    TEX_BLOCK(0, 0, 1, 1, 0, 0, 0, 0, 0),
};

// V12: a COPY-mode Texture Rectangle (16-bit) — the first texture path validated
// against Angrylion that does NOT go through the combiner or the 1-cycle texel
// pipeline (copy mode blits texels straight from TMEM to the colour image). A 4x2
// texture of 8 byte-asymmetric texels is preloaded at 0x3000, loaded into TMEM by
// Load Tile, and blitted 1:1 (DsDx = 4.0, DtDy = 1.0) into a 4x2 colour image, so
// the framebuffer should equal the source texels verbatim. RustyN64 passes this as
// an internal round-trip; this checks it against the oracle.
static const uint16_t TEX4X2_RAMP[8] = {
    0x0102u, 0x0304u, 0x0506u, 0x0708u, // row 0
    0x090Au, 0x0B0Cu, 0x0D0Eu, 0x0F10u, // row 1
};
static const uint32_t V12_TEX_RECT_COPY_16[] = {
    0x2F2000F0u, 0x00000000u, // Set Other Modes: COPY (cycle 21:20=10), dither off
    0x3D100003u, 0x00003000u, // Set Texture Image: 16-bit, width 4, addr 0x3000
    0x35100200u, 0x00000000u, // Set Tile 0: 16-bit, line 1 (64-bit words), tmem 0
    0x32000000u, 0x0000C004u, // Set Tile Size: SL0 TL0 SH3 TH1 (u10.2 = 0xC,4)
    0x34000000u, 0x0000C004u, // Load Tile: SL0 TL0 SH3 TH1 -> 4x2 texels
    0x3F100003u, 0x00001000u, // Set Color Image: 16-bit, width 4, addr 0x1000
    0x2D000000u, 0x00010008u, // Set Scissor: (0,0)-(4,2)
    0x2400C004u, 0x00000000u, // Texture Rectangle: XL=3 YL=1 tile0 XH0 YH0
    0x00000000u, 0x10000400u, // S=0 T=0 | DsDx=4.0 (0x1000) DtDy=1.0 (0x400)
};

// V13: a COPY-mode Texture Rectangle blitted to an OFFSET position — the same 4x2
// texture as V12, but 1:1-blitted into the (2,2)..(5,3) sub-rectangle of an 8x8
// colour image (the rest stays background 0). Exercises the destination positioning
// (XH/YH != 0) and the scissor interaction that the origin blit did not. Still 1:1
// (DsDx = 4.0, DtDy = 1.0) 16-bit, so RustyN64's supported copy path applies.
static const uint32_t V13_TEX_RECT_OFFSET_16[] = {
    0x2F2000F0u, 0x00000000u, // Set Other Modes: COPY, dither off
    0x3D100003u, 0x00003000u, // Set Texture Image: 16-bit, width 4, addr 0x3000
    0x35100200u, 0x00000000u, // Set Tile 0: 16-bit, line 1, tmem 0
    0x32000000u, 0x0000C004u, // Set Tile Size: SL0 TL0 SH3 TH1 (4x2)
    0x34000000u, 0x0000C004u, // Load Tile: SL0 TL0 SH3 TH1 -> 4x2 texels
    0x3F100007u, 0x00001000u, // Set Color Image: 16-bit, width 8, addr 0x1000
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)
    0x2401400Cu, 0x00008008u, // Texture Rectangle: XL=5 YL=3 tile0 XH=2 YH=2
    0x00000000u, 0x10000400u, // S=0 T=0 | DsDx=4.0 DtDy=1.0
};

// V14: a COPY-mode Texture Rectangle of a full **8x8** texture (64 texels, tile
// line = 2 64-bit words per row) 1:1-blitted into an 8x8 colour image. Exercises the
// load + blit at a larger scale than the small vectors — the full tile stride, all
// eight rows, and the odd-row TMEM swap across a bigger surface. The texture is an
// RGBA5551 gradient (R = 4x, G = 4y) filled at run time into `tex8x8` (below).
static const uint32_t V14_TEX_RECT_8X8_16[] = {
    0x2F2000F0u, 0x00000000u, // Set Other Modes: COPY, dither off
    0x3D100007u, 0x00003000u, // Set Texture Image: 16-bit, width 8, addr 0x3000
    0x35100400u, 0x00000000u, // Set Tile 0: 16-bit, line 2 (64-bit words), tmem 0
    0x32000000u, 0x0001C01Cu, // Set Tile Size: SL0 TL0 SH7 TH7 (8x8, u10.2 = 0x1C)
    0x34000000u, 0x0001C01Cu, // Load Tile: SL0 TL0 SH7 TH7 -> 8x8 texels
    0x3F100007u, 0x00001000u, // Set Color Image: 16-bit, width 8, addr 0x1000
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)
    0x2401C01Cu, 0x00000000u, // Texture Rectangle: XL=7 YL=7 tile0 XH0 YH0
    0x00000000u, 0x10000400u, // S=0 T=0 | DsDx=4.0 DtDy=1.0
};
static uint16_t tex8x8[64];

// V15: a MAGNIFIED COPY-mode Texture Rectangle — an 8-texel texture (fully loaded,
// so no unloaded-TMEM read) blitted into an 8x1 image with DsDx = 2.0 (0.5
// texel/pixel). The RDP copies 4 pixels per cycle (reading 4 consecutive texels
// from a 64-bit TMEM word, the base advancing DsDx*4 texels/cycle), so a 2x magnify
// reads texels 0,1,2,3,2,3,4,5 — NOT the naive per-pixel 0,0,1,1,2,2,3,3. RustyN64
// now models this (ledger R-8) and matches the golden below byte-for-byte.
static const uint16_t TEX8X1_RAMP[8] = {
    0xF801u, 0x07C1u, 0x003Fu, 0xFFFFu, 0xF83Fu, 0x07FFu, 0xFFC1u, 0x8421u,
};
static const uint32_t V15_TEX_RECT_MAG_16[] = {
    0x2F2000F0u, 0x00000000u, // Set Other Modes: COPY, dither off
    0x3D100007u, 0x00003000u, // Set Texture Image: 16-bit, width 8, addr 0x3000
    0x35100400u, 0x00000000u, // Set Tile 0: 16-bit, line 2, tmem 0
    0x32000000u, 0x0001C000u, // Set Tile Size: SL0 TL0 SH7 TH0 (8x1)
    0x34000000u, 0x0001C000u, // Load Tile: SL0 TL0 SH7 TH0 -> 8x1 texels
    0x3F100007u, 0x00001000u, // Set Color Image: 16-bit, width 8, addr 0x1000
    0x2D000000u, 0x00020004u, // Set Scissor: (0,0)-(8,1)
    0x2401C000u, 0x00000000u, // Texture Rectangle: XL=7 YL=0 tile0 XH0 YH0
    0x00000000u, 0x08000400u, // S=0 T=0 | DsDx=2.0 (0x800) DtDy=1.0
};

// V16: an ALPHA-COMPARE probe — a 1-cycle shaded triangle (flat red RGB) whose
// combiner alpha (= the interpolated shade alpha) ramps across X via dx.A = 0x30,
// with alpha-compare enabled (Set Other Modes bit 0) against a Set Blend Color
// alpha threshold of 0x80. Columns whose alpha fails the compare are killed (stay
// background 0); the rest draw red. RustyN64 currently ignores alpha_compare_en
// (draws every column). Angrylion defines which columns survive.
static const uint32_t V16_ALPHA_COMPARE_16[] = {
    0x2F0000F0u, 0x00000001u, // Set Other Modes: 1-cycle, dither off, alpha_compare_en (bit 0)
    0x3C000000u, 0x00000104u, // Set Combine Mode: rgb_d=4/a_d=4 (shade passthrough)
    0x39000000u, 0x00000080u, // Set Blend Color: alpha threshold = 0x80
    0x3F100007u, 0x00001000u, // Set Color Image: 16-bit, width 8, addr 0x1000
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)
    0x0C800020u, 0x00200000u, // op=0x0C (shade), lft=1, yl=32, ym=32, yh=0
    0x00000000u, 0x00000000u, // XL, DxLDy
    0x00020000u, 0x00000000u, // XH = 2.0
    0x00020000u, 0x00010000u, // XM = 2.0, DxMDy = 1.0
    // base R=0xFF G=0 B=0 A=0; dx.A = 0x30 (alpha ramps 0,0x30,0x60,0x90,... across X).
    SHADE_BLOCK(0xFF, 0x00, 0x00, 0x00, 0, 0, 0, 0x30, 0, 0, 0, 0),
};

// V17: alpha-compare on the DEPTH path — the same shade-alpha ramp + 0x80 threshold
// as V16, but on a z-suffixed triangle (op 0x0D) with z_update on and z_compare off
// (so every pixel passes the depth test and alpha-compare is the only gate). The
// colour output equals V16's: low-alpha columns are killed. Guards the alpha-compare
// gate on `depth_span` (it must skip both the colour write and the z-write).
static const uint32_t V17_ALPHA_COMPARE_Z_16[] = {
    0x2F0000F0u, 0x00000021u, // Set Other Modes: 1-cycle, dither off, alpha_compare (bit0) + z_update (bit5)
    0x3C000000u, 0x00000104u, // Set Combine Mode: shade passthrough
    0x39000000u, 0x00000080u, // Set Blend Color: alpha threshold = 0x80
    0x3F100007u, 0x00001000u, // Set Color Image: 16-bit, width 8, addr 0x1000
    0x3E000000u, 0x00001800u, // Set Depth Image: z buffer at 0x1800
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)
    0x0D800020u, 0x00200000u, // op=0x0D (shade+z), lft=1, yl=32, ym=32, yh=0
    0x00000000u, 0x00000000u, // XL, DxLDy
    0x00020000u, 0x00000000u, // XH = 2.0
    0x00020000u, 0x00010000u, // XM = 2.0, DxMDy = 1.0
    SHADE_BLOCK(0xFF, 0x00, 0x00, 0x00, 0, 0, 0, 0x30, 0, 0, 0, 0), // red, alpha ramp dx.A=0x30
    Z_SUFFIX(0x08000000),                                          // z = near
};

// V18: a fractional-edge shaded triangle with cvg_dest = FULL (Set Other Modes bits
// 9:8 = 10). Identical to shade_tri_frac_16 except the coverage write-back stores
// FULL coverage (7) instead of the clamp `(count-1)&7`, so the partially-covered
// right-edge column stores alpha bit 1 (`0xf801`) instead of `0xf800`. Probes the
// cvg_dest = full mode (the clamp default is the other vectors).
static const uint32_t V18_CVG_DEST_FULL_16[] = {
    0x2F000000u, 0x00000200u, // Set Other Modes: 1-cycle, AA off, cvg_dest = full (bits 9:8 = 10)
    0x3C000000u, 0x00000104u, // Set Combine Mode: shade passthrough
    0x3F100007u, 0x00001000u, // Set Color Image: 16-bit, width 8, addr 0x1000
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)
    0x0C800020u, 0x00200000u, // op=0x0C (shade), lft=1, yl=32, ym=32, yh=0
    0x00000000u, 0x00000000u, // XL, DxLDy
    0x00028000u, 0x00000000u, // XH = 2.5, DxHDy = 0
    0x00068000u, 0x00000000u, // XM = 6.5, DxMDy = 0
    SHADE_BLOCK_FLAT(0xFF, 0x00, 0x00, 0xFF), // flat red shade (16 words)
};

int main(int argc, char **argv) {
    const char *out_dir = (argc > 1) ? argv[1] : ".";
    // Fill the 8x8 gradient texture (RGBA5551: R = 4x, G = 4y, B = 0, alpha 1).
    for (uint32_t y = 0; y < 8; y++)
        for (uint32_t x = 0; x < 8; x++)
            tex8x8[y * 8 + x] =
                (uint16_t)(((x * 4u) << 11) | ((y * 4u) << 6) | 1u);

    Vector v1 = {"fill_rect_16", 0x2000, 0x1000, 8, 8, 2,
                 sizeof(V1_FILL_RECT_16) / 4, V1_FILL_RECT_16};
    if (emit_vector(&v1, out_dir)) return 1;

    Vector v2 = {"fill_tri_16", 0x2000, 0x1000, 8, 8, 2,
                 sizeof(V2_FILL_TRI_16) / 4, V2_FILL_TRI_16};
    if (emit_vector(&v2, out_dir)) return 1;

    Vector v3 = {"fill_tri_wide_16", 0x2000, 0x1000, 8, 8, 2,
                 sizeof(V3_FILL_TRI_WIDE_16) / 4, V3_FILL_TRI_WIDE_16};
    if (emit_vector(&v3, out_dir)) return 1;

    Vector v4 = {"fill_tri_neg_16", 0x2000, 0x1000, 8, 8, 2,
                 sizeof(V4_FILL_TRI_NEG_16) / 4, V4_FILL_TRI_NEG_16};
    if (emit_vector(&v4, out_dir)) return 1;

    Vector v5 = {"fill_tri_frac_16", 0x2000, 0x1000, 8, 8, 2,
                 sizeof(V5_FILL_TRI_FRAC_16) / 4, V5_FILL_TRI_FRAC_16};
    if (emit_vector(&v5, out_dir)) return 1;

    Vector v6 = {"shade_tri_frac_16", 0x2000, 0x1000, 8, 8, 2,
                 sizeof(V6_SHADE_TRI_FRAC_16) / 4, V6_SHADE_TRI_FRAC_16};
    if (emit_vector(&v6, out_dir)) return 1;

    Vector v7 = {"shade_depth_tri_frac_16", 0x2000, 0x1000, 8, 8, 2,
                 sizeof(V7_SHADE_DEPTH_TRI_FRAC_16) / 4, V7_SHADE_DEPTH_TRI_FRAC_16};
    if (emit_vector(&v7, out_dir)) return 1;

    Vector v8 = {"shade_tri_32", 0x2000, 0x1000, 8, 8, 4,
                 sizeof(V8_SHADE_TRI_32) / 4, V8_SHADE_TRI_32};
    if (emit_vector(&v8, out_dir)) return 1;

    Vector v9 = {"dither_tri_32", 0x2000, 0x1000, 8, 8, 4,
                 sizeof(V9_DITHER_TRI_32) / 4, V9_DITHER_TRI_32};
    if (emit_vector(&v9, out_dir)) return 1;

    Vector v10 = {"shade_grad_tri_32", 0x2000, 0x1000, 8, 8, 4,
                  sizeof(V10_SHADE_GRAD_TRI_32) / 4, V10_SHADE_GRAD_TRI_32};
    if (emit_vector(&v10, out_dir)) return 1;

    Vector v11 = {"tex_tri_16", 0x2000, 0x1000, 8, 8, 2,
                  sizeof(V11_TEX_TRI_16) / 4, V11_TEX_TRI_16,
                  0x3000, 8, TEX8_RAMP};
    if (emit_vector(&v11, out_dir)) return 1;

    Vector v12 = {"tex_rect_copy_16", 0x2000, 0x1000, 4, 2, 2,
                  sizeof(V12_TEX_RECT_COPY_16) / 4, V12_TEX_RECT_COPY_16,
                  0x3000, 8, TEX4X2_RAMP};
    if (emit_vector(&v12, out_dir)) return 1;

    Vector v13 = {"tex_rect_offset_16", 0x2000, 0x1000, 8, 8, 2,
                  sizeof(V13_TEX_RECT_OFFSET_16) / 4, V13_TEX_RECT_OFFSET_16,
                  0x3000, 8, TEX4X2_RAMP};
    if (emit_vector(&v13, out_dir)) return 1;

    Vector v14 = {"tex_rect_8x8_16", 0x2000, 0x1000, 8, 8, 2,
                  sizeof(V14_TEX_RECT_8X8_16) / 4, V14_TEX_RECT_8X8_16,
                  0x3000, 64, tex8x8};
    if (emit_vector(&v14, out_dir)) return 1;

    Vector v15 = {"tex_rect_mag_16", 0x2000, 0x1000, 8, 1, 2,
                  sizeof(V15_TEX_RECT_MAG_16) / 4, V15_TEX_RECT_MAG_16,
                  0x3000, 8, TEX8X1_RAMP};
    if (emit_vector(&v15, out_dir)) return 1;

    Vector v16 = {"alpha_compare_16", 0x2000, 0x1000, 8, 8, 2,
                  sizeof(V16_ALPHA_COMPARE_16) / 4, V16_ALPHA_COMPARE_16};
    if (emit_vector(&v16, out_dir)) return 1;

    Vector v17 = {"alpha_compare_z_16", 0x2000, 0x1000, 8, 8, 2,
                  sizeof(V17_ALPHA_COMPARE_Z_16) / 4, V17_ALPHA_COMPARE_Z_16};
    if (emit_vector(&v17, out_dir)) return 1;

    Vector v18 = {"cvg_dest_full_16", 0x2000, 0x1000, 8, 8, 2,
                  sizeof(V18_CVG_DEST_FULL_16) / 4, V18_CVG_DEST_FULL_16};
    if (emit_vector(&v18, out_dir)) return 1;

    return 0;
}
