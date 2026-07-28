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
#include <errno.h>

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

// The VI scan-out output surface, captured from `n64video_update_screen` for the
// `.vivec` oracle. Angrylion's VI writes the resampled/filtered frame into its
// `prescale` buffer and hands us a `frame_buffer` whose `pixels` points at the
// (already overscan-cropped) top-left of the visible region, `pitch` = 640. We
// copy the width x height window out, tightly packed as RGBA8 (rgba.a carries
// coverage 0..7, not opacity — see the VI recipe).
#define VI_OUT_MAX (640u * 625u)
static uint8_t g_vi_out[VI_OUT_MAX * 4];
static uint32_t g_vi_out_w, g_vi_out_h;
static bool g_vi_captured;

void vdac_init(struct n64video_config *config) { (void)config; }
void vdac_write(struct frame_buffer *fb) {
    g_vi_out_w = fb->width;
    g_vi_out_h = fb->height;
    g_vi_captured = true;
    if ((uint64_t)fb->width * fb->height > VI_OUT_MAX) {
        fprintf(stderr, "vdac_write: %ux%u exceeds prescale bound\n", fb->width, fb->height);
        exit(1);
    }
    for (uint32_t y = 0; y < fb->height; y++) {
        for (uint32_t x = 0; x < fb->width; x++) {
            struct rgba p = fb->pixels[y * fb->pitch + x];
            uint8_t *o = &g_vi_out[(y * fb->width + x) * 4];
            o[0] = p.r;
            o[1] = p.g;
            o[2] = p.b;
            o[3] = p.a;
        }
    }
}
void vdac_sync(bool invalid) { (void)invalid; }
void vdac_close(void) {}

static void mi_intr_cb(void) {}

// ---- RDRAM + register backing store ----

#define RDRAM_SIZE 0x800000u // 8 MiB (RDRAM_MAX_SIZE)

// Angrylion's 9th-bit "hidden" RDRAM plane (one byte per 16-bit halfword, low two
// bits meaningful), a non-static global in n64video/rdp/rdram.c. The VI reads it for
// 16-bit RGBA5551 coverage (`rdram_read_pair16`, hidden indexed by halfword with NO
// WORD_ADDR_XOR). `rdram_init` (called from `n64video_init`) memsets it to 3; for the
// coverage vectors we memset it to 0 and set the source region explicitly so the plane
// is fully determined and matches RustyN64's default-0 borders byte-for-byte.
extern uint8_t rdram_hidden[];

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
    // Crop to the visible area so `vdac_write` receives the clean resampled window
    // (no 640-wide prescale borders) — the `.vivec` oracle for RustyN64's scan-out.
    g_config.vi.hide_overscan = true;
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

// Place a logical 16-bit source framebuffer pixel so Angrylion's VI fetch reads it
// back verbatim — the inverse of `read_fb16` (`RREADIDX16` = `rdram16[idx ^ 1]`, the
// WORD_ADDR_XOR). The `.vivec` carries logical pixels; RustyN64's scan-out places the
// same logical pixels big-endian in its own RDRAM, so both renderers see one source.
static void rdram_put_fb16(uint32_t fb_addr, uint32_t x, uint32_t y, uint32_t w, uint16_t px) {
    uint32_t idx16 = ((fb_addr >> 1) + y * w + x) ^ 1;
    if (idx16 >= RDRAM_SIZE / 2) {
        fprintf(stderr, "rdram_put_fb16: pixel out of RDRAM bounds\n");
        exit(1);
    }
    ((uint16_t *)g_rdram)[idx16] = px;
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
// As TEX_BLOCK but with the per-**Y** derivative (`DsDy`/`DtDy`/`DwDy`, the 6th
// 64-bit word) set. Only the LOD reads it — the scanline walk steps by `de` — so it
// is the field that distinguishes a correct LOD from one that reuses `de`.
#define TEX_BLOCK_DY(bs, bt, bw, dxs, dxt, dxw, des, det, dew, dys, dyt, dyw) \
    HALVES(bs, bt), HALVES(bw, 0),          /* int base   S,T | W */          \
        HALVES(dxs, dxt), HALVES(dxw, 0),   /* dx  int */                     \
        0u, 0u,                             /* base frac */                   \
        0u, 0u,                             /* dx  frac */                    \
        HALVES(des, det), HALVES(dew, 0),   /* de  int */                     \
        HALVES(dys, dyt), HALVES(dyw, 0),   /* dy  int */                     \
        0u, 0u,                             /* de  frac */                    \
        0u, 0u                              /* dy  frac */

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
    0x2F0008F0u, 0x00000000u, // Set Other Modes: 1-cycle, bi_lerp0=1, persp off
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

// V22 (R-13 fix probe): tex_tri_16 but with Set Other Modes bi_lerp0 = 1 (bit 11,
// 0x2F0000F0 -> 0x2F0008F0) and a CONSTANT coordinate (dsdx = 0, S = 0 -> texel 0
// = red 0xF801). tex_tri_16 rendered black because bi_lerp0 = 0 selects the RDP's
// YUV colour-convert path (tex.c), which with unset convert coefficients zeroes an
// RGBA texel. With bi_lerp0 = 1 Angrylion does the normal RGBA fetch, so this
// should render red -- a WORKING textured-triangle reference (the missing setup
// that made every textured-triangle vector degenerate).
static const uint32_t V22_TEX_TRI_FIXED_16[] = {
    0x2F0008F0u, 0x00000000u, // Set Other Modes: 1-cycle, bi_lerp0=1, persp off
    0x3C000000u, 0x00000041u, // Set Combine Mode: rgb_d=1 / a_d=1 (texel0 passthrough)
    0x3D100007u, 0x00003000u, // Set Texture Image: 16-bit, width 8, addr 0x3000
    0x35100400u, 0x00000030u, // Set Tile 0: 16-bit, line 2, mask_s=3
    0x32000000u, 0x0001C000u, // Set Tile Size 0: SL=0 TL=0 SH=7 TH=0
    0x34000000u, 0x0001C000u, // Load Tile 0
    0x3F100007u, 0x00001000u, // Set Color Image: 16-bit, width 8
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)
    0x0A800020u, 0x00200000u, // op=0x0A (tex), lft=1, yl=32, ym=32, yh=0, tile 0
    0x00000000u, 0x00000000u, // XL, DxLDy
    0x00020000u, 0x00000000u, // XH = 2.0
    0x00020000u, 0x00010000u, // XM = 2.0, DxMDy = 1.0
    // CONSTANT S/T = 0: every pixel samples texel 0 = red.
    TEX_BLOCK(0, 0, 1, 0, 0, 0, 0, 0, 0),
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
// An 8x8 RGBA5551 CHECKERBOARD (bright red `0xF801` = R31 G0 B0 A1 / dark `0x0001`
// = R0 G0 B0 A1, by `(x+y)&1`; the low bit is the 5551 alpha, set so both are opaque),
// used by the mid-texel vector. A checkerboard is deliberately NON-planar: in every
// 2x2 quad `t0 == t3` and `t1 == t2` are the opposite value, so the 3-point centre
// pick (which ignores `t0`) yields the opposite-parity value while the mid-texel
// four-texel average yields the midpoint — the two differ, making the vector
// non-vacuous where a smooth gradient (planar) would collapse them to the same value.
static uint16_t tex_chk8x8[64];

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

// V19: a 1-cycle 32-bit shaded triangle whose combiner outputs the PRIMITIVE colour
// (rgb_d = a_d = prim, select 3), NOT the shade. The shade block is a DISTINCT
// colour (0x112233) so a combiner that wrongly emitted the shade would show
// 0x112233 instead of the prim 0x224466 — this discriminates the prim mux input
// (Set Prim Color 0x3A) from the shade path. Validates the combiner prim input.
static const uint32_t V19_PRIM_COMBINER_32[] = {
    0x2F0000F0u, 0x00000000u, // Set Other Modes: 1-cycle, dither off
    0x3A000000u, 0x224466FFu, // Set Prim Color: R=0x22 G=0x44 B=0x66 A=0xFF
    0x3C000000u, 0x000000C3u, // Set Combine: cyc1 rgb_d=(lo>>6)&7=3, a_d=lo&7=3 (both prim).
                              // 1-cycle mode evaluates the cyc1 selects (combine() at
                              // rdp/lib.rs applies cyc0 only in 2-cycle); this is the D (add)
                              // input of (A-B)*C+D, so the pixel is the prim colour.
    0x3F180007u, 0x00001000u, // Set Color Image: 32-bit, width 8, addr 0x1000
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)
    0x0C800020u, 0x00200000u, // op=0x0C (shade), lft=1, yl=32, ym=32, yh=0
    0x00000000u, 0x00000000u, // XL, DxLDy
    0x00020000u, 0x00000000u, // XH = 2.0
    0x00020000u, 0x00010000u, // XM = 2.0, DxMDy = 1.0
    SHADE_BLOCK_FLAT(0x11, 0x22, 0x33, 0xFF), // distinct shade (must NOT appear)
};

// V20: a 4-bit (I4) TEXTURED triangle — the hardware-canonical 4-bit texture path.
// There is NO 4-bit texel LOAD on the N64: a 4-bit texture-image load is invalid
// (Angrylion crashes the pipeline; ledger R-7). Games load 4-bit textures by
// LYING about the format — set an **8-bit** texture image and 8-bit LOAD tile,
// load half as many texels raw, then render with a SEPARATE **4-bit** tile that
// reads the same TMEM and does the nibble extraction at fetch. This vector proves
// that path end to end: eight I4 texels with DESCENDING non-zero intensities
// (0xF,0xD,0xB,0x9,0x7,0x5,0x3,0x1) are packed two-per-byte into four 8-bit bytes
// (0xFD 0xB9 0x75 0x31) at 0x3000, loaded by an 8-bit Load Tile into TMEM word 0,
// then sampled left-to-right by a 4-bit (format I, size 0) render tile whose dx.S
// advances one texel per pixel, so each column is a DISTINCT non-zero intensity.
// Non-zero-and-distinct on purpose: texel 0 = 0 renders black, identical to an
// unloaded (all-zero) TMEM, so a black golden could not tell a correct load from a
// no-op. Angrylion defines the golden; RustyN64's existing 8-bit load + 4-bit fetch
// must match it byte-for-byte.
static const uint16_t TEX_I4_RAMP[2] = {0xFDB9u, 0x7531u}; // 4 bytes = 8 I4 nibbles
static const uint32_t V20_TEX_TRI_I4_16[] = {
    0x2F0008F0u, 0x00000000u, // Set Other Modes: 1-cycle, bi_lerp0=1, persp off
    0x3C000000u, 0x00000041u, // Set Combine Mode: rgb_d=1 / a_d=1 (texel0 passthrough)
    0x3D080003u, 0x00003000u, // Set Texture Image: 8-bit (size=1), width 4, addr 0x3000
    0x35080200u, 0x07000000u, // Set Tile 7 (LOAD): 8-bit, line=1 (one 64-bit word), tmem 0
    0x32000000u, 0x0700C000u, // Set Tile Size 7: SL0 TL0 SH3 TH0 (4 8-bit bytes)
    0x34000000u, 0x0700C000u, // Load Tile 7: SL0 TL0 SH3 TH0
    0x35800200u, 0x00000030u, // Set Tile 0 (RENDER): format I(4), size 0(4bit), line 1, mask_s=3
    0x32000000u, 0x0001C000u, // Set Tile Size 0: SL0 TL0 SH7 TH0 (8 4-bit texels)
    0x3F100007u, 0x00001000u, // Set Color Image: 16-bit, width 8, addr 0x1000
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)
    0x0A800020u, 0x00200000u, // op=0x0A (tex), lft=1, yl=32, ym=32, yh=0, tile 0
    0x00000000u, 0x00000000u, // XL, DxLDy
    0x00020000u, 0x00000000u, // XH = 2.0
    0x00020000u, 0x00010000u, // XM = 2.0, DxMDy = 1.0
    // S base 0, T base 0, W base 1.0; dx.S int = 0x20 (the R-13 s.5 scale advances
    // the texel index one per pixel), so columns sample texels 0,1,2,...; rest 0.
    TEX_BLOCK(0, 0, 1, 0x20, 0, 0, 0, 0, 0),
};

// V21/V22b: the tile coordinate CLAMP and WRAP for a point-sampled textured
// triangle (ledger R-13). A 4-texel 16-bit tile (red, green, blue, white; SH = 3)
// is sampled across a triangle whose S advances one texel per pixel (dx.S = 0x20),
// so the six drawn columns want texels 0,1,2,3,4,5 — two of them PAST the tile.
// What the RDP does past `SH` depends on the tile mode:
//   - CLAMP (`clamp_s = 1`): the over-tile columns clamp to the last texel (3 =
//     white) -> red, green, blue, white, white, white.
//   - WRAP (`mask_s = 2`, so the coordinate wraps mod 4): -> red, green, blue,
//     white, red, green.
// Distinct texels make each non-vacuous: a sampler that ignored the tile transform
// (RustyN64's pre-R-13 behaviour) would read texels 4,5 from unrelated TMEM (or
// clamp-to-black), matching neither golden. Angrylion defines both.
static const uint16_t TEX_CLAMP4[4] = {0xF801u, 0x07C1u, 0x003Fu, 0xFFFFu}; // R,G,B,W
static const uint32_t V21_TEX_TRI_CLAMP_16[] = {
    0x2F0008F0u, 0x00000000u, // Set Other Modes: 1-cycle, bi_lerp0=1, persp off
    0x3C000000u, 0x00000041u, // Set Combine Mode: texel0 passthrough
    0x3D100003u, 0x00003000u, // Set Texture Image: 16-bit, width 4, addr 0x3000
    0x35100200u, 0x00000200u, // Set Tile 0: 16-bit, line 1, tmem 0, clamp_s=1 (bit 9)
    0x32000000u, 0x0000C000u, // Set Tile Size 0: SL0 TL0 SH3 TH0 (4 texels)
    0x34000000u, 0x0000C000u, // Load Tile 0: 4 texels
    0x3F100007u, 0x00001000u, // Set Color Image: 16-bit, width 8, addr 0x1000
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)
    0x0A800020u, 0x00200000u, // op=0x0A (tex), lft=1, yl=32, ym=32, yh=0, tile 0
    0x00000000u, 0x00000000u, // XL, DxLDy
    0x00020000u, 0x00000000u, // XH = 2.0
    0x00020000u, 0x00010000u, // XM = 2.0, DxMDy = 1.0
    TEX_BLOCK(0, 0, 1, 0x20, 0, 0, 0, 0, 0), // dx.S = 0x20 = one texel/pixel (s.5)
};
static const uint32_t V21_TEX_TRI_WRAP_16[] = {
    0x2F0008F0u, 0x00000000u, // Set Other Modes: 1-cycle, bi_lerp0=1, persp off
    0x3C000000u, 0x00000041u, // Set Combine Mode: texel0 passthrough
    0x3D100003u, 0x00003000u, // Set Texture Image: 16-bit, width 4, addr 0x3000
    0x35100200u, 0x00000020u, // Set Tile 0: 16-bit, line 1, tmem 0, mask_s=2 (bits 7:4)
    0x32000000u, 0x0000C000u, // Set Tile Size 0: SL0 TL0 SH3 TH0 (4 texels)
    0x34000000u, 0x0000C000u, // Load Tile 0: 4 texels
    0x3F100007u, 0x00001000u, // Set Color Image: 16-bit, width 8, addr 0x1000
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)
    0x0A800020u, 0x00200000u, // op=0x0A (tex), lft=1, yl=32, ym=32, yh=0, tile 0
    0x00000000u, 0x00000000u, // XL, DxLDy
    0x00020000u, 0x00000000u, // XH = 2.0
    0x00020000u, 0x00010000u, // XM = 2.0, DxMDy = 1.0
    TEX_BLOCK(0, 0, 1, 0x20, 0, 0, 0, 0, 0), // dx.S = 0x20 = one texel/pixel (s.5)
};

// V23: the N64 3-point BILINEAR filter (`sample_type = 1`) on a textured triangle
// (ledger R-13). The 8x8 gradient texture `tex8x8` (R = 4x, G = 4y) is sampled with
// S advancing 0.5 texel/column (dx.S = 0x10) and T 0.5 texel/row (de.T = 0x10), so
// covered pixels land BETWEEN texels — each output is a 3-point blend of the four
// neighbours. Both fractions vary across the triangle, so pixels exercise BOTH the
// lower-left (`sfrac+tfrac < 0x20`) and upper-right (`>= 0x20`) triangle branches.
// The sampled range stays within S,T <= ~3.5 (base+neighbour in-bounds), avoiding
// the deferred mask-wrap seam. Point sampling would show hard texel steps; bilinear
// smooths them — Angrylion defines the golden.
static const uint32_t V23_TEX_TRI_BILINEAR_16[] = {
    0x2F0028F0u, 0x00000000u, // Set Other Modes: 1-cycle, bi_lerp0=1, SAMPLE_TYPE=1 (bit 45), persp off
    0x3C000000u, 0x00000041u, // Set Combine Mode: texel0 passthrough
    0x3D100007u, 0x00003000u, // Set Texture Image: 16-bit, width 8, addr 0x3000
    0x35100400u, 0x0000C030u, // Set Tile 0: 16-bit, line 2, tmem 0, mask_s=3, mask_t=3 (8x8)
    0x32000000u, 0x0001C01Cu, // Set Tile Size 0: SL0 TL0 SH7 TH7 (8x8 texels)
    0x34000000u, 0x0001C01Cu, // Load Tile 0: 8x8 texels
    0x3F100007u, 0x00001000u, // Set Color Image: 16-bit, width 8, addr 0x1000
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)
    0x0A800020u, 0x00200000u, // op=0x0A (tex), lft=1, yl=32, ym=32, yh=0, tile 0
    0x00000000u, 0x00000000u, // XL, DxLDy
    0x00020000u, 0x00000000u, // XH = 2.0
    0x00020000u, 0x00010000u, // XM = 2.0, DxMDy = 1.0 (a widening staircase, 2D coverage)
    // dx.S = 0x10 (0.5 texel/col), de.T = 0x10 (0.5 texel/row); base (0,0,1).
    TEX_BLOCK(0, 0, 1, 0x10, 0, 0, 0, 0x10, 0),
};

// V24: the bilinear MASK-WRAP SEAM (`sdiff`/`tdiff`, ledger R-13). A 2-texel tile
// (`mask_s = 1`, so S wraps mod 2: red, green) is bilinear-sampled with S advancing
// 0.5 texel/pixel. At S = 1.5 the base texel is 1 (the top of the wrap period), so
// the bilinear NEIGHBOUR must wrap back to texel 0 (red) — a mirror-OFF PERIOD-END
// wrap, where `tcmask_coupled` picks `sdiff = -base` (here -1, since base = 1), NOT a
// mirror seam. The pre-fix sampler used a hardcoded `+1`, reading texel 2
// (UNLOADED = black) instead, so the seam column blends green+red here vs green+black
// there — non-vacuous BECAUSE the wrapped texel (red) differs from the unloaded one.
static const uint16_t TEX_SEAM2[2] = {0xF801u, 0x07C1u}; // red, green
static const uint32_t V24_TEX_TRI_BILINEAR_WRAP_16[] = {
    0x2F0028F0u, 0x00000000u, // Set Other Modes: 1-cycle, bi_lerp0=1, SAMPLE_TYPE=1, persp off
    0x3C000000u, 0x00000041u, // Set Combine Mode: texel0 passthrough
    0x3D100001u, 0x00003000u, // Set Texture Image: 16-bit, width 2, addr 0x3000
    0x35100200u, 0x00000010u, // Set Tile 0: 16-bit, line 1, tmem 0, mask_s=1 (wrap mod 2)
    0x32000000u, 0x00004000u, // Set Tile Size 0: SL0 TL0 SH1 TH0 (2 texels)
    0x34000000u, 0x00004000u, // Load Tile 0: 2 texels
    0x3F100007u, 0x00001000u, // Set Color Image: 16-bit, width 8, addr 0x1000
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)
    0x0A800020u, 0x00200000u, // op=0x0A (tex), lft=1, yl=32, ym=32, yh=0, tile 0
    0x00000000u, 0x00000000u, // XL, DxLDy
    0x00020000u, 0x00000000u, // XH = 2.0
    0x00020000u, 0x00010000u, // XM = 2.0, DxMDy = 1.0
    TEX_BLOCK(0, 0, 1, 0x10, 0, 0, 0, 0, 0), // dx.S = 0x10 (0.5 texel/pixel), T flat
};

// V25: 2-cycle mode with a SECOND texel from tile+1 (ledger R-13). Two 1-texel
// tiles are loaded — tile 0 = red (TMEM word 0), tile 1 = green (TMEM word 1) — and
// a 2-cycle combine outputs TEXEL0 in BOTH cycles (cyc0 D=texel0, cyc1 D=texel0).
// The hardware swaps texel0/texel1 before cycle 1, so cycle 1's TEXEL0 reads tile 1
// (green); the pixel is GREEN. Distinguishes the two-tile 2-cycle path + swap: a
// missing texel1 sample (or no swap) would leave cycle 1 reading tile 0 = RED.
// Constant texture coordinate (both tiles sample their single texel). Point sampled.
static const uint16_t TEX_2CYC[2] = {0xF801u, 0x07C1u}; // red @0x3000, green @0x3002
static const uint32_t V25_TEX_TRI_2CYCLE_16[] = {
    0x2F100CF0u, 0x00000000u, // Set Other Modes: 2-CYCLE (cycle_type=1), bi_lerp0/1, point, dither off
    0x3C000000u, 0x00008241u, // Set Combine: cyc0 D=texel0 (rgb+a), cyc1 D=texel0 (rgb+a)
    0x3D100000u, 0x00003000u, // Set Texture Image: 16-bit, width 1, addr 0x3000 (red)
    0x35100200u, 0x00000000u, // Set Tile 0: 16-bit, line 1, tmem word 0
    0x32000000u, 0x00000000u, // Set Tile Size 0: SL0 TL0 SH0 TH0 (1 texel)
    0x34000000u, 0x00000000u, // Load Tile 0: red -> TMEM word 0
    0x3D100000u, 0x00003002u, // Set Texture Image: addr 0x3002 (green)
    0x35100201u, 0x01000000u, // Set Tile 1: 16-bit, line 1, tmem word 1
    0x32000000u, 0x01000000u, // Set Tile Size 1: SL0 TL0 SH0 TH0
    0x34000000u, 0x01000000u, // Load Tile 1: green -> TMEM word 1
    0x3F100007u, 0x00001000u, // Set Color Image: 16-bit, width 8, addr 0x1000
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)
    0x0A800020u, 0x00200000u, // op=0x0A (tex), lft=1, yl=32, ym=32, yh=0, tile 0
    0x00000000u, 0x00000000u, // XL, DxLDy
    0x00020000u, 0x00000000u, // XH = 2.0
    0x00020000u, 0x00010000u, // XM = 2.0, DxMDy = 1.0
    TEX_BLOCK(0, 0, 1, 0, 0, 0, 0, 0, 0), // constant coord: both tiles sample texel 0
};

// V26: the PRIM_LOD_FRAC combiner input (ledger R-10). Set Prim Color carries
// prim_lod_frac = 0x80 in its word-0 low byte; the combiner computes ONE * prim_lod_frac
// (rgb_a=One, rgb_c=PrimLODFrac select 14), so the pixel is a mid-gray ~0x80. Without
// the input wired it reads 0 and the pixel is black — non-vacuous. A flat shade block
// routes the triangle through the combiner (the combine ignores the shade).
static const uint32_t V26_TEX_TRI_PRIMLODFRAC_16[] = {
    0x2F0000F0u, 0x00000000u, // Set Other Modes: 1-cycle, dither off
    0x3A000080u, 0x00000000u, // Set Prim Color: prim_lod_frac = 0x80 (word-0 low byte)
    0x3C0000CEu, 0x081C01C6u, // Set Combine: rgb = (One - Zero) * PrimLODFrac + Zero; alpha = One
    0x3F100007u, 0x00001000u, // Set Color Image: 16-bit, width 8, addr 0x1000
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)
    0x0C800020u, 0x00200000u, // op=0x0C (shade), lft=1, yl=32, ym=32, yh=0
    0x00000000u, 0x00000000u, // XL, DxLDy
    0x00020000u, 0x00000000u, // XH = 2.0
    0x00020000u, 0x00010000u, // XM = 2.0, DxMDy = 1.0
    SHADE_BLOCK_FLAT(0x40, 0x40, 0x40, 0xFF), // flat shade (ignored by the combine)
};

// V27: the CONVERT K4 (RGB sub-B, select 7) and K5 (RGB mul, select 15) combiner
// inputs from Set Convert (0x2C) — ledger R-10. K4 = 0x040, K5 = 0x0C0; the combiner
// computes (One - K4) * K5 = (0xFF - 0x40) * 0xC0 >> 8. Without the inputs wired both
// read 0 and the result is (0xFF - 0) * 0 = black — non-vacuous. Flat shade routes the
// triangle through the combiner.
static const uint32_t V27_TEX_TRI_CONVERT_K45_16[] = {
    0x2F0000F0u, 0x00000000u, // Set Other Modes: 1-cycle, dither off
    0x2C000000u, 0x000080C0u, // Set Convert: K4 = 0x040 (lo[17:9]), K5 = 0x0C0 (lo[8:0])
    0x3C0000CFu, 0x071C01C6u, // Set Combine: rgb = (One - K4) * K5 + Zero; alpha = One
    0x3F100007u, 0x00001000u, // Set Color Image: 16-bit, width 8, addr 0x1000
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)
    0x0C800020u, 0x00200000u, // op=0x0C (shade), lft=1, yl=32, ym=32, yh=0
    0x00000000u, 0x00000000u, // XL, DxLDy
    0x00020000u, 0x00000000u, // XH = 2.0
    0x00020000u, 0x00010000u, // XM = 2.0, DxMDy = 1.0
    SHADE_BLOCK_FLAT(0x40, 0x40, 0x40, 0xFF), // flat shade (ignored by the combine)
};

// V28: a NEGATIVE Convert K4 (bit 8 set) — ledger R-10, sign-extension proof.
// K4 = 0x1C0 (raw 9-bit; = -64 after the RDP's special-9bit expand), K5 = 0x040
// (= 64). The combine is `(One - K4) * K5 = (256 - (-64)) * 64 >> 8 = 80` (a gray),
// which is only correct if K4 is sign-expanded through the `special_9bit_exttable`
// (Angrylion combiner.c:481) rather than read as the raw positive 448 — a raw read
// gives `(256 - 448) * 64 < 0` → clamped black. Non-vacuous (gray vs black) and it
// exercises the bit-8-set path the positive `tex_tri_convert_k45_16` vector cannot.
static const uint32_t V28_TEX_TRI_CONVERT_KNEG_16[] = {
    0x2F0000F0u, 0x00000000u, // Set Other Modes: 1-cycle, dither off
    0x2C000000u, 0x00038040u, // Set Convert: K4 = 0x1C0 (=-64), K5 = 0x040 (=64)
    0x3C0000CFu, 0x071C01C6u, // Set Combine: rgb = (One - K4) * K5 + Zero; alpha = One
    0x3F100007u, 0x00001000u, // Set Color Image: 16-bit, width 8, addr 0x1000
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)
    0x0C800020u, 0x00200000u, // op=0x0C (shade), lft=1, yl=32, ym=32, yh=0
    0x00000000u, 0x00000000u, // XL, DxLDy
    0x00020000u, 0x00000000u, // XH = 2.0
    0x00020000u, 0x00010000u, // XM = 2.0, DxMDy = 1.0
    SHADE_BLOCK_FLAT(0x40, 0x40, 0x40, 0xFF), // flat shade (ignored by the combine)
};

// V29: the chroma-key KeyCentre (RGB sub-B select 6) and KeyScale (RGB mul select 6)
// combiner inputs from Set Key GB (0x2A) / Set Key R (0x2B) — ledger R-10. Per channel
// centre [0x20,0x40,0x60], scale [0x40,0x80,0xC0], so the combine `(One - centre) *
// scale >> 8` is [56,96,120] (a distinct RGB, quantised to RGBA5551). Without the inputs
// wired both read 0 and the result is `(One - 0) * 0 = 0` (black) — non-vacuous. The Set
// Combine word only differs from V27 in rgb_b (7->6, KeyCentre) and rgb_c (15->6,
// KeyScale); the alpha/cyc0 fields are identical (alpha = One).
static const uint32_t V29_TEX_TRI_CHROMAKEY_16[] = {
    0x2F0000F0u, 0x00000000u, // Set Other Modes: 1-cycle, dither off
    0x2A000000u, 0x408060C0u, // Set Key GB: cg=0x40 sg=0x80 cb=0x60 sb=0xC0
    0x2B000000u, 0x00002040u, // Set Key R:  wr=0 cr=0x20 sr=0x40
    0x3C0000C6u, 0x061C01C6u, // Set Combine: rgb = (One - KeyCentre) * KeyScale + Zero; a=One
    0x3F100007u, 0x00001000u, // Set Color Image: 16-bit, width 8, addr 0x1000
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)
    0x0C800020u, 0x00200000u, // op=0x0C (shade), lft=1, yl=32, ym=32, yh=0
    0x00000000u, 0x00000000u, // XL, DxLDy
    0x00020000u, 0x00000000u, // XH = 2.0
    0x00020000u, 0x00010000u, // XM = 2.0, DxMDy = 1.0
    SHADE_BLOCK_FLAT(0x40, 0x40, 0x40, 0xFF), // flat shade (ignored by the combine)
};

// V30: the chroma-key ALPHA compare (key_en, Set Other Modes bit 40) — ledger R-10.
// The key alpha (`chroma_key_min`) must be OBSERVABLE, so alpha-compare is enabled
// (bit 0) with a Set Blend Color threshold of 0x80: the pixel is written only if the
// key alpha >= 0x80. With rgb_a=rgb_b=Shade the 17-bit combined colour is 0x80 (A−B=0
// → 0x80 constant), so per channel `SIGN(0x80)=128` folds to `-128`, then `+width<<4`:
// width_r=0x10 → 256−128 = 128 = 0x80 (the min across r/g/b), which meets the 0x80
// threshold, so the Shade chromabypass triangle IS drawn. A broken `chroma_key_min`
// (wrong sign/fold/width/min) shifts the key alpha below 0x80 → the triangle vanishes;
// clearing key_en outputs the *combined* colour (black, A−B=0) instead of Shade. Both
// make this non-vacuous — the golden's drawn Shade pixels observe the key alpha.
static const uint32_t V30_TEX_TRI_CHROMAKEY_ALPHA_16[] = {
    0x2F0001F0u, 0x00000001u, // Set Other Modes: 1-cycle, KEY_EN (bit 40), ALPHA_COMPARE (bit 0)
    0x39000000u, 0x00000080u, // Set Blend Color: alpha threshold = 0x80
    0x2A014018u, 0x00000000u, // Set Key GB: wg=0x14 wb=0x18 (centre/scale unused here)
    0x2B000000u, 0x00100000u, // Set Key R:  wr=0x10 (centre/scale unused)
    0x3C000080u, 0x041C01C6u, // Set Combine: rgb_a=Shade rgb_b=Shade rgb_c=Combined rgb_d=Zero; a=One
    0x3F100007u, 0x00001000u, // Set Color Image: 16-bit, width 8, addr 0x1000
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)
    0x0C800020u, 0x00200000u, // op=0x0C (shade), lft=1, yl=32, ym=32, yh=0
    0x00000000u, 0x00000000u, // XL, DxLDy
    0x00020000u, 0x00000000u, // XH = 2.0
    0x00020000u, 0x00010000u, // XM = 2.0, DxMDy = 1.0
    SHADE_BLOCK_FLAT(0x40, 0x60, 0x80, 0xFF), // flat shade -> chromabypass + col17 source
};

// V31: PRIMITIVE BASE-TILE THREADING (R-13) — the triangle command's tile field
// (bits 50:48 = `(ewdata[0] >> 16) & 7`, Angrylion rasterizer.c:1887) selects which
// of the 8 tile descriptors the sampler reads. RustyN64 hardwired tiles[0]/tiles[1];
// this vector proves the thread. The 8-colour ramp (TEX8_RAMP, as V11) is loaded into
// **tile 3** at a NON-ZERO TMEM word (0x40 -> byte 0x200), and the triangle names
// **tile 3** (op 0x0A830020). Tile 0 is set as RGBA16 over the UNLOADED low TMEM, so a
// renderer that wrongly samples tile 0 reads zeros -> black. The golden is therefore the
// V11 ramp (identical picture, just via tile 3), and a `tiles[0]` regression collapses
// it to black -> fails. Non-vacuous: the colourful ramp is produced ONLY by the correct
// tile-3 selection of the tile-3 load.
static const uint32_t V31_TEX_TRI_BASE_TILE_16[] = {
    0x2F0008F0u, 0x00000000u, // Set Other Modes: 1-cycle, bi_lerp0=1, persp off
    0x3C000000u, 0x00000041u, // Set Combine: rgb_d=1 / a_d=1 (texel0 passthrough)
    0x3D100007u, 0x00003000u, // Set Texture Image: 16-bit, width 8, addr 0x3000
    0x35100400u, 0x00000030u, // Set Tile 0: RGBA16, line 2, tmem 0, mask_s=3 (UNLOADED -> black)
    0x35100440u, 0x03000030u, // Set Tile 3: RGBA16, line 2, tmem 0x40 (byte 0x200), mask_s=3
    0x32000000u, 0x0301C000u, // Set Tile Size 3: SL0 TL0 SH7 TH0
    0x34000000u, 0x0301C000u, // Load Tile 3: SL0 TL0 SH7 TH0 -> tile 3 TMEM
    0x3F100007u, 0x00001000u, // Set Color Image: 16-bit, width 8, addr 0x1000
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)
    0x0A830020u, 0x00200000u, // op=0x0A (tex), lft=1, yl=32, ym=32, yh=0, TILE 3 (bits 50:48)
    0x00000000u, 0x00000000u, // XL, DxLDy
    0x00020000u, 0x00000000u, // XH = 2.0
    0x00020000u, 0x00010000u, // XM = 2.0, DxMDy = 1.0
    // S base 0, T base 0, W base 1.0; dx.S = 1.0 (one texel per pixel); rest 0.
    TEX_BLOCK(0, 0, 1, 1, 0, 0, 0, 0, 0),
};

// V32: the MID-TEXEL filter (Set Other Modes bit 44, ledger R-13). Same bilinear setup
// as V23 (0.5 texel/column and /row, so odd-column/odd-row covered pixels sample the
// exact texel centre `sfrac == tfrac == 0x10`) but over the NON-planar CHECKERBOARD
// texture `tex_chk8x8` and with `mid_texel` set. At the centre the RDP replaces the
// 3-point triangle pick (which ignores `t0`) with a four-texel average (Angrylion
// `tex.c` `center` case) — on a checkerboard the 3-point yields the opposite-parity
// value while the average yields the midpoint, so bit 44 visibly changes those pixels.
// The checkerboard is essential: a smooth gradient is planar, so its centre average
// equals its 3-point pick and bit 44 would be invisible (a vacuous golden).
static const uint32_t V32_TEX_TRI_MID_TEXEL_16[] = {
    0x2F0038F0u, 0x00000000u, // Set Other Modes: 1-cycle, bi_lerp0=1, SAMPLE_TYPE=1 (bit 45), MID_TEXEL=1 (bit 44)
    0x3C000000u, 0x00000041u, // Set Combine Mode: texel0 passthrough
    0x3D100007u, 0x00003000u, // Set Texture Image: 16-bit, width 8, addr 0x3000
    0x35100400u, 0x0000C030u, // Set Tile 0: 16-bit, line 2, tmem 0, mask_s=3, mask_t=3 (8x8)
    0x32000000u, 0x0001C01Cu, // Set Tile Size 0: SL0 TL0 SH7 TH7 (8x8 texels)
    0x34000000u, 0x0001C01Cu, // Load Tile 0: 8x8 texels
    0x3F100007u, 0x00001000u, // Set Color Image: 16-bit, width 8, addr 0x1000
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)
    0x0A800020u, 0x00200000u, // op=0x0A (tex), lft=1, yl=32, ym=32, yh=0, tile 0
    0x00000000u, 0x00000000u, // XL, DxLDy
    0x00020000u, 0x00000000u, // XH = 2.0
    0x00020000u, 0x00010000u, // XM = 2.0, DxMDy = 1.0 (a widening staircase, 2D coverage)
    // dx.S = 0x10 (0.5 texel/col), de.T = 0x10 (0.5 texel/row); base (0,0,1).
    TEX_BLOCK(0, 0, 1, 0x10, 0, 0, 0, 0x10, 0),
};

// V33: the derivative-computed LOD FRACTION combiner input (ledger R-13/R-10) —
// the `LODFrac` mul select, distinct from V26's PRIM_LOD_FRAC register input.
//
// A **2-cycle** textured triangle (the 2-cycle LOD form is the one that needs no
// span-edge signals). The LOD is the LARGER of the per-x and per-y coordinate
// deltas, and this vector makes those differ so the golden pins BOTH taps:
//   dx.S =  48 -> x delta  48
//   dy.S = 112 -> y delta 112   (`de` stays 0, so `de` cannot stand in for `dy`)
// so the LOD settles at 112. With `level = 2` (`max_level`, bits 53:51) it is not
// "distant" (`l_tile = log2(112>>5) = 1 < 2`), so the fraction is the real
// interpolation weight `((112 << 3) >> 1) & 0xff = 0xc0` rather than the saturated
// 0xff. Cycle 1 computes `(One - Zero) * LODFrac + Zero`, so the pixel IS the LOD
// fraction. Two ways non-vacuous: unwiring the input renders black, and an
// implementation that reads `de` (0) instead of `dy` for the y tap gets LOD 48 ->
// 0x80 instead of 0xc0.
static const uint32_t V33_TEX_TRI_LODFRAC_16[] = {
    0x2F1008F0u, 0x00000000u, // Set Other Modes: 2-CYCLE (bits 21:20=01), bi_lerp0=1, dither off
    0x3C0000CDu, 0x081C01C6u, // Set Combine: cyc1 rgb = (One - Zero) * LODFrac(13) + Zero; alpha One
    0x3D100007u, 0x00003000u, // Set Texture Image: 16-bit, width 8, addr 0x3000
    0x35100400u, 0x00000030u, // Set Tile 0: 16-bit, line 2, tmem 0, mask_s=3
    0x32000000u, 0x0001C000u, // Set Tile Size 0: SL0 TL0 SH7 TH0
    0x34000000u, 0x0001C000u, // Load Tile 0: 8 texels
    0x35100400u, 0x01000030u, // Set Tile 1: 16-bit, line 2, tmem 0, mask_s=3 (2-cycle texel1)
    0x32000000u, 0x0101C000u, // Set Tile Size 1: SL0 TL0 SH7 TH0
    0x3F100007u, 0x00001000u, // Set Color Image: 16-bit, width 8, addr 0x1000
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)
    0x0A900020u, 0x00200000u, // op=0x0A (tex), lft=1, LEVEL=2 (bits 53:51), tile 0, yl=32 ym=32 yh=0
    0x00000000u, 0x00000000u, // XL, DxLDy
    0x00020000u, 0x00000000u, // XH = 2.0
    0x00020000u, 0x00010000u, // XM = 2.0, DxMDy = 1.0
    // dx.S = 48 (x delta), de = 0 (flat scanline walk), dy.S = 112 (y delta, LOD only).
    // The LOD is max(48, 112) = 112 -> fraction 0xc0.
    TEX_BLOCK_DY(0, 0, 1, 48, 0, 0, 0, 0, 0, 112, 0, 0),
};

// V34: LOD-DRIVEN MIP TILE SELECTION (`tex_lod_en`, Set Other Modes bit 48) —
// ledger R-13. Same 2-cycle LOD setup as V33 (dx.S = 48, dy.S = 112, de = 0 -> LOD
// 112; `level = 2` so it is not "distant"), so `l_tile = log2(112>>5) = 1`. With
// `tex_lod_en` the sampler must therefore read tile `base + l_tile` = **tile 1**,
// not the base tile 0 — and the second cycle reads tile 2.
//
// Three 1-texel tiles are loaded with DISTINCT colours (tile 0 red, tile 1 green,
// tile 2 blue) and the combiner passes TEXEL0 through, so the pixel names the tile
// that was actually sampled: GREEN when the LOD selects tile 1, RED if the selection
// is ignored and the base tile is sampled. That is what makes it non-vacuous.
static const uint16_t TEX_MIP3[3] = {0xF801u, 0x07C1u, 0x003Fu}; // red, green, blue
static const uint32_t V34_TEX_TRI_MIP_TILE_16[] = {
    // NOTE both bi_lerp0 (bit 11) AND bi_lerp1 (bit 10) must be set: cycle 1 runs
    // `texture_pipeline_cycle(.., 1)`, whose filter is selected by bi_lerp1, and a
    // clear bi_lerp1 sends cycle 1 down the YUV colour-convert path (which, with
    // unset convert coefficients, renders white here rather than the sampled texel).
    0x2F110CF0u, 0x00000000u, // Set Other Modes: 2-CYCLE, TEX_LOD_EN (bit 48), bi_lerp0/1, dither off
    0x3C000000u, 0x00008241u, // Set Combine: cyc0 D=texel0, cyc1 D=texel0 (passthrough)
    0x3D100000u, 0x00003000u, // Set Texture Image: 16-bit, width 1, addr 0x3000 (red)
    0x35100200u, 0x00000000u, // Set Tile 0: 16-bit, line 1, tmem word 0
    0x32000000u, 0x00000000u, // Set Tile Size 0: 1 texel
    0x34000000u, 0x00000000u, // Load Tile 0: red -> TMEM word 0
    0x3D100000u, 0x00003002u, // Set Texture Image: addr 0x3002 (green)
    0x35100201u, 0x01000000u, // Set Tile 1: 16-bit, line 1, tmem word 1
    0x32000000u, 0x01000000u, // Set Tile Size 1: 1 texel
    0x34000000u, 0x01000000u, // Load Tile 1: green -> TMEM word 1
    0x3D100000u, 0x00003004u, // Set Texture Image: addr 0x3004 (blue)
    0x35100202u, 0x02000000u, // Set Tile 2: 16-bit, line 1, tmem word 2
    0x32000000u, 0x02000000u, // Set Tile Size 2: 1 texel
    0x34000000u, 0x02000000u, // Load Tile 2: blue -> TMEM word 2
    0x3F100007u, 0x00001000u, // Set Color Image: 16-bit, width 8, addr 0x1000
    0x2D000000u, 0x00020020u, // Set Scissor: (0,0)-(8,8)
    0x0A900020u, 0x00200000u, // op=0x0A (tex), lft=1, LEVEL=2, tile 0, yl=32 ym=32 yh=0
    0x00000000u, 0x00000000u, // XL, DxLDy
    0x00020000u, 0x00000000u, // XH = 2.0
    0x00020000u, 0x00010000u, // XM = 2.0, DxMDy = 1.0
    // dx.S = 48, de = 0, dy.S = 112 -> LOD 112 -> l_tile 1. S/T stay at texel 0 of
    // whichever tile is chosen (each tile holds a single texel), so the colour is
    // purely a report of WHICH tile the LOD selected.
    TEX_BLOCK_DY(0, 0, 1, 48, 0, 0, 0, 0, 0, 112, 0, 0),
};

// ---- Seeded fuzz generator (SplitMix64) ----
//
// A reproducible pseudo-random corpus: the seed and this generator's source fully
// determine every emitted vector, so the committed goldens can be regenerated
// bit-for-bit (`./driver --fuzz <dir> <seed> <count>`). Only families whose RDP
// paths RustyN64 already models byte-exactly are generated here; the Rust side
// *curates* (replays each candidate and keeps only oracle-matching ones), so a
// generated config that trips an unmodelled corner is dropped, never committed.
// The generator stays deliberately conservative: correctness of the corpus is the
// invariant, breadth is grown family by family.
static uint64_t g_rng;
static uint64_t sm64(void) {
    g_rng += 0x9E3779B97F4A7C15ull;
    uint64_t z = g_rng;
    z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9ull;
    z = (z ^ (z >> 27)) * 0x94D049BB133111EBull;
    return z ^ (z >> 31);
}
// Uniform in [lo, hi] inclusive.
static uint32_t rng_range(uint32_t lo, uint32_t hi) {
    return lo + (uint32_t)(sm64() % (uint64_t)(hi - lo + 1u));
}

// FILL-mode Fill Rectangle family (16-bit): a random fill colour, image size, and
// aligned rectangle. FILL writes the colour verbatim to covered pixels and leaves
// the rest zero, which RustyN64 reproduces byte-exactly (ledger R-3), so every
// member matches the oracle. Coordinates are integer (`* 4` into the .2 fixed
// point) — no sub-pixel edge, which is the one FILL corner not yet modelled.
static int fuzz_fill_rect(int idx, const char *out_dir) {
    uint32_t w = rng_range(4, 12), h = rng_range(4, 12);
    uint32_t rx0 = rng_range(0, w - 1), rx1 = rng_range(rx0 + 1, w);
    uint32_t ry0 = rng_range(0, h - 1), ry1 = rng_range(ry0 + 1, h);
    uint16_t c = (uint16_t)rng_range(0, 0xFFFF);
    uint32_t fill_lo = ((uint32_t)c << 16) | c;
    // The scissor is the full image so the rectangle's own edges determine the fill
    // extent (the `fillrect` family). The `scissor` family below varies the scissor.
    uint32_t words[] = {
        0x2F300000u, 0x00000000u,                  // Set Other Modes: FILL
        0x3F100000u | (w - 1u), 0x00001000u,       // Set Color Image: 16-bit
        0x37000000u, fill_lo,                      // Set Fill Color
        0x2D000000u, ((w * 4u) << 12) | (h * 4u),  // Set Scissor (0,0)-(w,h)
        0x36000000u | ((rx1 * 4u) << 12) | (ry1 * 4u), // Fill Rectangle: lower-right
        ((rx0 * 4u) << 12) | (ry0 * 4u),           //   upper-left
    };
    char name[64];
    snprintf(name, sizeof(name), "fz_fill_%03d", idx);
    Vector v = {name, 0x2000, 0x1000, w, h, 2, sizeof(words) / 4, words, 0, 0, NULL};
    return emit_vector(&v, out_dir);
}

// FILL-mode Fill Rectangle with an INDEPENDENT scissor sub-rect that often clips
// the rectangle. This exercises the asymmetric scissor lower-right (ledger R-15):
// the X bound is inclusive of its boundary pixel, the Y bound is exclusive. The
// scissor upper-left is in the first half of the image and its lower-right past it,
// so the clip lands inside the drawn region for most members.
static int fuzz_scissor(int idx, const char *out_dir) {
    uint32_t w = rng_range(4, 12), h = rng_range(4, 12);
    uint32_t rx0 = rng_range(0, w - 1), rx1 = rng_range(rx0 + 1, w);
    uint32_t ry0 = rng_range(0, h - 1), ry1 = rng_range(ry0 + 1, h);
    uint32_t sx0 = rng_range(0, w / 2), sy0 = rng_range(0, h / 2);
    uint32_t sx1 = rng_range(sx0 + 1, w), sy1 = rng_range(sy0 + 1, h);
    uint16_t c = (uint16_t)rng_range(0, 0xFFFF);
    uint32_t fill_lo = ((uint32_t)c << 16) | c;
    uint32_t words[] = {
        0x2F300000u, 0x00000000u,                  // Set Other Modes: FILL
        0x3F100000u | (w - 1u), 0x00001000u,       // Set Color Image: 16-bit
        0x37000000u, fill_lo,                      // Set Fill Color
        0x2D000000u | ((sx0 * 4u) << 12) | (sy0 * 4u), // Set Scissor: upper-left
        ((sx1 * 4u) << 12) | (sy1 * 4u),           //   lower-right
        0x36000000u | ((rx1 * 4u) << 12) | (ry1 * 4u), // Fill Rectangle: lower-right
        ((rx0 * 4u) << 12) | (ry0 * 4u),           //   upper-left
    };
    char name[64];
    snprintf(name, sizeof(name), "fz_scis_%03d", idx);
    Vector v = {name, 0x2000, 0x1000, w, h, 2, sizeof(words) / 4, words, 0, 0, NULL};
    return emit_vector(&v, out_dir);
}

// FILL-mode Fill Triangle (0x08) with INTEGER vertices and slopes. One edge is the
// vertical major edge (an apex column), the other a minor edge widening by an
// integer px/row slope; `flip` picks which side is major. FILL mode renders whole
// pixels (no sub-pixel coverage), and RustyN64's union-approximation edge-walk
// matches Angrylion for integer-aligned triangles, so members reproduce the oracle.
// `ym == yl` uses only the major + one minor edge (the simplest two-edge triangle).
static int fuzz_fill_tri(int idx, const char *out_dir) {
    uint32_t w = rng_range(6, 12), h = rng_range(5, 10);
    uint16_t c = (uint16_t)rng_range(0, 0xFFFF);
    uint32_t fill_lo = ((uint32_t)c << 16) | c;
    uint32_t flip = rng_range(0, 1);
    uint32_t yl = h * 4u;              // full height, integer .2 (yh = 0, ym = yl)
    uint32_t slope = rng_range(1, 2);  // 1 or 2 px per row
    uint32_t apex, dxmdy;
    if (flip) { // left-major: vertical left edge, minor widens right (+slope)
        apex = rng_range(0, w / 3);
        dxmdy = slope << 16;
    } else {    // right-major: vertical right edge, minor widens left (-slope)
        apex = rng_range((2u * w) / 3u, w - 1u);
        dxmdy = (uint32_t)(-(int32_t)(slope << 16)) & 0x3FFFFFFFu;
    }
    uint32_t xedge = apex << 16;      // XH == XM == apex.0 (s.16)
    uint32_t op = 0x08000000u | (flip << 23) | (yl & 0x3FFFu);
    uint32_t words[] = {
        0x2F300000u, 0x00000000u,                  // Set Other Modes: FILL
        0x3F100000u | (w - 1u), 0x00001000u,       // Set Color Image: 16-bit
        0x37000000u, fill_lo,                      // Set Fill Color
        0x2D000000u, ((w * 4u) << 12) | (h * 4u),  // Set Scissor (0,0)-(w,h)
        op, (yl << 16),                            // Fill Triangle: yl / ym=yl / yh=0
        0x00000000u, 0x00000000u,                  // XL, DxLDy (unused, ym == yl)
        xedge, 0x00000000u,                        // XH (major), DxHDy = 0 (vertical)
        xedge, dxmdy,                              // XM (minor apex), DxMDy
    };
    char name[64];
    snprintf(name, sizeof(name), "fz_tri_%03d", idx);
    Vector v = {name, 0x2000, 0x1000, w, h, 2, sizeof(words) / 4, words, 0, 0, NULL};
    return emit_vector(&v, out_dir);
}

// Emit a reproducible fuzz corpus of `count` candidates from `seed` in `family`.
static int emit_fuzz(const char *out_dir, uint64_t seed, int count, const char *family) {
    g_rng = seed;
    int (*gen)(int, const char *) = fuzz_fill_rect;
    if (strcmp(family, "scissor") == 0) {
        gen = fuzz_scissor;
    } else if (strcmp(family, "filltri") == 0) {
        gen = fuzz_fill_tri;
    } else if (strcmp(family, "fillrect") != 0) {
        fprintf(stderr, "fuzz: unknown family '%s' (want fillrect|scissor|filltri)\n", family);
        return 1;
    }
    for (int i = 0; i < count; i++) {
        if (gen(i, out_dir)) return 1;
    }
    fprintf(stderr, "fuzz: emitted %d '%s' candidate(s) from seed %llu\n", count, family,
            (unsigned long long)seed);
    return 0;
}

// ---- VI scan-out vectors (`.vivec`) ----
//
// A VI vector drives Angrylion's real scan-out pipeline (`n64video_update_screen`
// → `vi_process_full`): it places a 16-bit source framebuffer in RDRAM, sets the VI
// registers, captures the resampled/cropped output via `vdac_write`, and writes a
// `.vivec` = 15 big-endian u32 header + the logical source pixels (BE 16-bit) + the
// golden RGBA8 output. RustyN64 replays it through `Bus::scanout_scaled` (R-5/R-6).
typedef struct {
    const char *name;
    uint32_t ctrl;    // VI_STATUS/VI_CTRL (type[1:0], aa_mode[9:8], ...)
    uint32_t origin;  // VI_ORIGIN (source framebuffer RDRAM byte address)
    uint32_t width;   // VI_WIDTH (source stride, pixels)
    uint32_t x_scale; // VI_X_SCALE (2.10: add[11:0], start[27:16])
    uint32_t y_scale; // VI_Y_SCALE
    uint32_t h_video; // VI_H_START/VI_H_VIDEO (h_start[25:16], h_end[9:0])
    uint32_t v_video; // VI_V_START/VI_V_VIDEO
    uint32_t v_sync;  // VI_V_SYNC (V_TOTAL; > 550 selects PAL)
    uint32_t src_w, src_h; // source framebuffer dimensions
    uint32_t bpp;     // source bytes/pixel: 0 or 2 = RGBA5551, 4 = RGBA8888
    uint32_t aa;      // 0 = every pixel fully covered; 1 = every 4th column partial (cvg 0);
                      // 2 = as 1, plus a non-monotonic fully-covered probe triplet (divot bypass)
} ViVector;

// Non-monotonic fully-covered probe triplet for the divot vector (aa == 2). The
// three columns x = PROBE_X-1 / PROBE_X / PROBE_X+1 are all fully covered (none is
// an x%4==0 partial column), so the divot filter's all-fully-covered early-return
// fires and the centre must PASS THROUGH UNCHANGED. Because the values are
// non-monotonic (low / high / mid per channel), the per-channel median of the
// three does NOT equal the centre — so deleting the early-return and computing the
// median instead would change the centre pixel and fail the vector. This is what
// makes the bypass observable (CodeRabbit #155). PROBE_X = 18 -> {17,18,19} are
// all fully covered (none is an x%4==0 partial column), and all three lie inside
// the scanned-out source window (columns 8..32 for this vector's 1:1 h-scale, so
// the x=1..3 range is never sampled). x=18 maps to output column 10 with zero
// x-fraction (nearest sample), so that output pixel IS the divot centre. PROBE_Y
// = 10 is an interior sampled row (source rows 0..15 are scanned out).
#define VI_DIVOT_PROBE_X 18u
#define VI_DIVOT_PROBE_Y 10u

// A deterministic distinct source pixel (RGBA5551): encodes x and y into separate
// channels so any mis-addressed sample lands on a visibly wrong colour.
static uint16_t vi_src_pixel(uint32_t x, uint32_t y) {
    return (uint16_t)(((x & 0x1f) << 11) | ((y & 0x1f) << 6) | (((x + y) & 0x1f) << 1) | 1u);
}

// The 32-bit (RGBA8888) counterpart: distinct per (x, y). Alpha = 0xFF so its
// coverage field (bits 7:5) reads 7 (fully covered) under aa_mode 0/1.
static uint32_t vi_src_pixel32(uint32_t x, uint32_t y, uint32_t aa) {
    uint32_t r = (x * 4u) & 0xFFu, g = (y * 4u) & 0xFFu, b = ((x + y) * 4u) & 0xFFu;
    // aa: every 4th column is partial (cvg 0, alpha 0x00), so a partial pixel's 6 taps
    // (all at x±1/x±2 columns) are fully covered and the AA edge filter has neighbours.
    // A partial pixel is given a fixed DARK colour (0x08) — deliberately NOT its
    // neighbours' midpoint, unlike the smooth gradient — so `video_filter32` pulls it
    // measurably brighter at INTERIOR pixels too, not just at the top boundary. With
    // the gradient value the neighbour penultimate min/max are symmetric about the
    // centre and the filter would output the raw colour, hiding a raw-fetch mutation.
    if (aa && (x % 4u == 0u)) {
        return (0x08u << 24) | (0x08u << 16) | (0x08u << 8) | 0x00u;
    }
    // aa == 2: a non-monotonic fully-covered probe triplet (see VI_DIVOT_PROBE_*).
    // All three pixels keep alpha 0xFF (cvg 7), so the divot bypass fires; the
    // centre's median across the triplet differs from the centre in every channel.
    if (aa == 2u && y == VI_DIVOT_PROBE_Y) {
        if (x == VI_DIVOT_PROBE_X - 1u) {
            return (0x10u << 24) | (0x20u << 16) | (0x30u << 8) | 0xFFu; // left  (low)
        }
        if (x == VI_DIVOT_PROBE_X) {
            return (0xF0u << 24) | (0xE0u << 16) | (0xD0u << 8) | 0xFFu; // centre (high)
        }
        if (x == VI_DIVOT_PROBE_X + 1u) {
            return (0x80u << 24) | (0x90u << 16) | (0xA0u << 8) | 0xFFu; // right (mid)
        }
    }
    return (r << 24) | (g << 16) | (b << 8) | 0xFFu;
}

// The 16-bit (RGBA5551) coverage-path source pixel: distinct 5-bit channels per
// (x, y), with the pixel's bit 0 as the coverage MSB and the two hidden bits (written
// via `*hid`) as the low bits, so `cvg = ((px & 1) << 2) | hidden`. `aa` mirrors the
// 32-bit generator: 1 = every 4th column partial (cvg 0), 2 = 1 plus the non-monotonic
// fully-covered divot probe triplet. A fully-covered pixel is bit0=1, hidden=3 (cvg 7).
static uint16_t vi_src_pixel16_cov(uint32_t x, uint32_t y, uint32_t aa, uint8_t *hid) {
    uint16_t base = (uint16_t)(((x & 0x1f) << 11) | ((y & 0x1f) << 6) | (((x + y) & 0x1f) << 1));
    // Partial column (cvg 0): bit0 = 0, hidden = 0.
    if (aa && (x % 4u == 0u)) {
        *hid = 0;
        return base;
    }
    // Divot probe (aa == 2): a non-monotonic fully-covered triplet at the probe row so
    // the divot all-fully-covered early-return is observable (its per-channel median
    // differs from the centre). All three keep bit0=1, hidden=3 (cvg 7).
    if (aa == 2u && y == VI_DIVOT_PROBE_Y) {
        *hid = 3;
        if (x == VI_DIVOT_PROBE_X - 1u) {
            return (2u << 11) | (4u << 6) | (6u << 1) | 1u; // left  (low 5-bit channels)
        }
        if (x == VI_DIVOT_PROBE_X) {
            return (30u << 11) | (28u << 6) | (26u << 1) | 1u; // centre (high)
        }
        if (x == VI_DIVOT_PROBE_X + 1u) {
            return (16u << 11) | (18u << 6) | (20u << 1) | 1u; // right (mid)
        }
    }
    // Fully covered (cvg 7): bit0 = 1, hidden = 3.
    *hid = 3;
    return base | 1u;
}

// Set the hidden byte for the source halfword at (x, y): `rdram_hidden` is indexed by
// halfword with NO WORD_ADDR_XOR (unlike the colour plane), matching the VI's read.
static void rdram_put_hidden16(uint32_t fb_addr, uint32_t x, uint32_t y, uint32_t w,
                               uint8_t hid) {
    uint32_t idx16 = (fb_addr >> 1) + y * w + x;
    if (idx16 >= RDRAM_SIZE / 2) {
        fprintf(stderr, "rdram_put_hidden16: pixel out of RDRAM bounds\n");
        exit(1);
    }
    rdram_hidden[idx16] = hid & 0x3;
}

// Place a logical 32-bit source pixel so Angrylion's VI fetch reads it verbatim —
// 32-bit access has no WORD_ADDR_XOR (`RREADIDX32` = `rdram32[idx]`), so a host u32
// at word index is read back as-is (the inverse of `read_fb32`).
static void rdram_put_fb32(uint32_t fb_addr, uint32_t x, uint32_t y, uint32_t w, uint32_t px) {
    uint32_t idx32 = (fb_addr >> 2) + y * w + x;
    if (idx32 >= RDRAM_SIZE / 4) {
        fprintf(stderr, "rdram_put_fb32: pixel out of RDRAM bounds\n");
        exit(1);
    }
    ((uint32_t *)g_rdram)[idx32] = px;
}

static int emit_vi_vector(const ViVector *v, const char *out_dir) {
    engine_init();
    uint32_t src_bpp = (v->bpp == 4u) ? 4u : 2u;
    // A 16-bit vector under aa_mode 0/1 exercises the coverage path (de-dither /
    // AA-edge / divot), which reads the hidden-bits plane. Those vectors carry an
    // explicit hidden plane (format version 2); everything else is version 1.
    int cov16 = (src_bpp == 2u) && (((v->ctrl >> 8) & 3u) <= 1u);
    if (cov16) {
        // Override rdram_init's default-3 with 0 so the plane is fully determined by
        // the source region below and matches RustyN64's default-0 borders exactly.
        memset(rdram_hidden, 0, RDRAM_SIZE / 2);
    }
    for (uint32_t y = 0; y < v->src_h; y++) {
        for (uint32_t x = 0; x < v->src_w; x++) {
            if (src_bpp == 4u) {
                rdram_put_fb32(v->origin, x, y, v->src_w, vi_src_pixel32(x, y, v->aa));
            } else if (cov16) {
                uint8_t hid;
                uint16_t px = vi_src_pixel16_cov(x, y, v->aa, &hid);
                rdram_put_fb16(v->origin, x, y, v->src_w, px);
                rdram_put_hidden16(v->origin, x, y, v->src_w, hid);
            } else {
                rdram_put_fb16(v->origin, x, y, v->src_w, vi_src_pixel(x, y));
            }
        }
    }
    g_vi_regs[VI_STATUS] = v->ctrl;
    g_vi_regs[VI_ORIGIN] = v->origin;
    g_vi_regs[VI_WIDTH] = v->width;
    g_vi_regs[VI_X_SCALE] = v->x_scale;
    g_vi_regs[VI_Y_SCALE] = v->y_scale;
    g_vi_regs[VI_H_START] = v->h_video;
    g_vi_regs[VI_V_START] = v->v_video;
    g_vi_regs[VI_V_SYNC] = v->v_sync;

    g_vi_captured = false;
    n64video_update_screen();
    if (!g_vi_captured) {
        fprintf(stderr, "%s: VI produced no output (blanked?)\n", v->name);
        return 1;
    }

    char path[512];
    int need = snprintf(path, sizeof(path), "%s/%s.vivec", out_dir, v->name);
    if (need < 0 || (size_t)need >= sizeof(path)) {
        fprintf(stderr, "path too long for VI vector %s\n", v->name);
        return 1;
    }
    FILE *f = fopen(path, "wb");
    if (!f) {
        perror("fopen");
        return 1;
    }
    // Format version 2 signals a trailing hidden-bits plane (coverage vectors only);
    // version 1 is unchanged and carries none.
    uint32_t version = cov16 ? 2u : 1u;
    uint32_t hdr[15] = {0x56495643u, version,     v->ctrl,     v->origin,   v->width,
                        v->x_scale,  v->y_scale,   v->h_video,  v->v_video,  v->v_sync,
                        v->src_w,    v->src_h,     src_bpp,     g_vi_out_w,  g_vi_out_h};
    for (int i = 0; i < 15; i++) {
        uint8_t be[4] = {hdr[i] >> 24, hdr[i] >> 16, hdr[i] >> 8, hdr[i]};
        wr(be, 4, f);
    }
    for (uint32_t y = 0; y < v->src_h; y++) {
        for (uint32_t x = 0; x < v->src_w; x++) {
            if (src_bpp == 4u) {
                uint32_t px = vi_src_pixel32(x, y, v->aa);
                uint8_t be[4] = {px >> 24, px >> 16, px >> 8, px};
                wr(be, 4, f);
            } else if (cov16) {
                uint8_t hid;
                uint16_t px = vi_src_pixel16_cov(x, y, v->aa, &hid);
                uint8_t be[2] = {px >> 8, px};
                wr(be, 2, f);
            } else {
                uint16_t px = vi_src_pixel(x, y);
                uint8_t be[2] = {px >> 8, px};
                wr(be, 2, f);
            }
        }
    }
    // Version 2: the hidden-bits plane, one byte per source pixel (low 2 bits), in the
    // same (x, y) order as the source above.
    if (cov16) {
        for (uint32_t y = 0; y < v->src_h; y++) {
            for (uint32_t x = 0; x < v->src_w; x++) {
                uint8_t hid;
                (void)vi_src_pixel16_cov(x, y, v->aa, &hid);
                wr(&hid, 1, f);
            }
        }
    }
    wr(g_vi_out, (size_t)g_vi_out_w * g_vi_out_h * 4, f);
    if (fclose(f) != 0) {
        perror("fclose");
        return 1;
    }
    fprintf(stderr, "wrote %s (%ux%u src -> %ux%u out)\n", path, v->src_w, v->src_h, g_vi_out_w,
            g_vi_out_h);
    return 0;
}

// Emit the fixed VI scan-out vectors. Slice 1 (ledger R-5): nearest-neighbour X/Y
// scale (aa_mode = REPLICATE, so no bilinear lerp and no AA/divot/de-dither/gamma),
// exercising the 2.10 accumulator + the active-span/overscan geometry.
static int emit_vi_vectors(const char *out_dir) {
    // 1:1 scale: validates the geometry + integer addressing (the 108-px NTSC
    // h-overscan makes the first visible column sample source column 8).
    ViVector v1x = {"vi_scale_1x_16",   0x00000302u, 0x1000u, 80u,
                    0x00000400u,        0x00000400u, 0x006C0094u, 0x00220042u,
                    525u,               80u,         48u};
    if (emit_vi_vector(&v1x, out_dir)) return 1;

    // 2x downscale (x_add = y_add = 0x800): every output pixel steps two source
    // pixels, so the accumulator's integer >>10 addressing is exercised.
    ViVector vdown = {"vi_scale_down2x_16", 0x00000302u, 0x1000u, 80u,
                      0x00000800u,          0x00000800u, 0x006C0094u, 0x00220042u,
                      525u,                 80u,         48u};
    if (emit_vi_vector(&vdown, out_dir)) return 1;

    // Slice 2 (ledger R-5): the 5-bit bilinear lerp. aa_mode = RESAMP_ONLY (VI_STATUS
    // bit 9 set, 0x0202) keeps the AA edge / divot / de-dither filters off but enables
    // the bilinear resample. 2x upscale (x_add = y_add = 0x200) makes xfrac/yfrac
    // alternate 0 and 0x10, so both the exact-passthrough and the 50%-blend lerp paths
    // (and both triangles of horizontal x vertical) are exercised.
    ViVector vbilin = {"vi_scale_bilinear_16", 0x00000202u, 0x1000u, 80u,
                       0x00000200u,            0x00000200u, 0x006C0094u, 0x00220042u,
                       525u,                   80u,         48u};
    if (emit_vi_vector(&vbilin, out_dir)) return 1;

    // A non-power-of-two scale (x_add = y_add = 0x240) makes xfrac/yfrac take values
    // that are not multiples of 4, so the lerp's `+16 >> 5` rounding bias actually
    // changes the result (unlike the 0x200 vector, where every product is a multiple
    // of 32 and the rounding is invisible) — this pins the rounding.
    ViVector vbilodd = {"vi_scale_bilinear_odd_16", 0x00000202u, 0x1000u, 80u,
                        0x00000240u,               0x00000240u, 0x006C0094u, 0x00220042u,
                        525u,                      80u,         48u};
    if (emit_vi_vector(&vbilodd, out_dir)) return 1;

    // Slice 3 (ledger R-5): the gamma curve. VI_STATUS bit 3 (gamma_enable) set, bit 2
    // (gamma_dither) clear, aa_mode = REPLICATE (0x030A) so nearest sampling with the
    // sqrt gamma table applied to the final RGB. 1:1 scale isolates the curve.
    ViVector vgamma = {"vi_gamma_1x_16", 0x0000030Au, 0x1000u, 80u,
                       0x00000400u,      0x00000400u, 0x006C0094u, 0x00220042u,
                       525u,             80u,         48u};
    if (emit_vi_vector(&vgamma, out_dir)) return 1;

    // Slice 4a / R-6 (partial): the PAL active-span geometry. v_sync = 625 (> 550)
    // selects the PAL branch — the horizontal overscan is 128 px (not NTSC's 108). With
    // h_start = 115, PAL's -128 drives it to -13 (left-clamp: minhpass = 0, x_start
    // folds +13 columns → first sample is source column 13), while a mis-applied NTSC
    // -108 would leave h_start = +7 (no clamp, minhpass = 8 → source column 8). The
    // output therefore distinguishes the PAL geometry. Nearest, 1:1; only the
    // field-rate/interlace half of R-6 stays deferred.
    ViVector vpal = {"vi_pal_geometry_16", 0x00000302u, 0x1000u, 80u,
                     0x00000400u,          0x00000400u, 0x007300A3u, 0x002C004Cu,
                     625u,                 80u,         48u};
    if (emit_vi_vector(&vpal, out_dir)) return 1;

    // Slice 4b (ledger R-5): 32-bit RGBA8888 source with the bilinear resample.
    // aa_mode = RESAMP_ONLY, type = 3 (0x0203), 2x upscale. Exercises the 32-bit fetch
    // (`(px>>24)&0xff` ...) through the same `vi_lerp3` path as the 16-bit bilinear.
    // The trailing `4u` selects a 32-bit source framebuffer.
    ViVector vbil32 = {"vi_scale_bilinear_32", 0x00000203u, 0x1000u, 80u,
                       0x00000200u,            0x00000200u, 0x006C0094u, 0x00220042u,
                       525u,                   80u,         48u,        4u};
    if (emit_vi_vector(&vbil32, out_dir)) return 1;

    // Slice 4c (ledger R-5): the de-dither restore filter. aa_mode = 0 (reads real
    // coverage), dither_filter_enable (bit 16), type = 3 (0x00010003). Every 32-bit
    // source pixel has alpha 0xFF (coverage bits 7:5 = 7, fully covered), so the
    // cur_cvg==7 + dither_filter path (restore_filter32, an 8-tap ±1 nudge toward
    // neighbours) runs everywhere; the AA-edge path is never hit. 1:1 scale (xfrac =
    // yfrac = 0) so no bilinear lerp post-blends the filter output.
    ViVector vdedith = {"vi_dedither_32", 0x00010003u, 0x1000u, 80u,
                        0x00000400u,      0x00000400u, 0x006C0094u, 0x00220042u,
                        525u,             80u,         48u,        4u};
    if (emit_vi_vector(&vdedith, out_dir)) return 1;

    // Slice 4d (ledger R-5): the AA edge filter. aa_mode = 0 (reads coverage), no
    // dither/divot/gamma (0x00000003), 32-bit source with every 4th column partial
    // (cvg 0, the trailing `1` = coverage-varying pattern). A partial pixel takes
    // video_filter32: it gathers the fully-covered pixels among its 6 diagonal/far
    // taps, takes the penultimate min/max per channel, and pulls the centre toward
    // their midpoint by (7 - cvg)/8. 1:1 scale so no lerp post-blends it.
    ViVector vaa = {"vi_aa_edge_32", 0x00000003u, 0x1000u, 80u,
                    0x00000400u,     0x00000400u, 0x006C0094u, 0x00220042u,
                    525u,            80u,         48u,        4u, 1u};
    if (emit_vi_vector(&vaa, out_dir)) return 1;

    // Slice 4e (ledger R-5): the divot median filter. divot_enable (VI_CTRL bit 4),
    // aa_mode = 0, type = 3 (0x00000013), 32-bit with the same every-4th-column partial
    // pattern. A pixel whose 3 horizontal neighbours are not all fully covered (the
    // partial columns and their immediate neighbours) takes the per-channel median of
    // the post-AA values; the rest pass through. 1:1 scale so no lerp. aa = 2 adds a
    // non-monotonic fully-covered probe triplet (VI_DIVOT_PROBE_*) so the divot
    // all-fully-covered EARLY-RETURN is observable: its centre must pass through
    // unchanged even though the triplet's per-channel median differs from it.
    ViVector vdivot = {"vi_divot_32", 0x00000013u, 0x1000u, 80u,
                       0x00000400u,   0x00000400u, 0x006C0094u, 0x00220042u,
                       525u,          80u,         48u,        4u, 2u};
    if (emit_vi_vector(&vdivot, out_dir)) return 1;

    // Slice 4f (ledger R-5): the 16-bit RGBA5551 coverage path. Coverage is read from
    // the hidden-bits plane (`((px & 1) << 2) | hidden`) instead of a 32-bit alpha
    // byte, then the same de-dither / AA-edge / divot filters run on the 5-bit→8-bit
    // channels. These vectors carry an explicit hidden plane (format version 2). Same
    // geometry as the 32-bit coverage vectors; type = 2 (RGBA5551).

    // De-dither: aa = 0 → every pixel fully covered (bit0=1, hidden=3, cvg 7), so the
    // restore filter runs everywhere. dither_filter (bit 16), aa_mode 0 → 0x00010002.
    ViVector vdd16 = {"vi_dedither_16", 0x00010002u, 0x1000u, 80u,
                      0x00000400u,      0x00000400u, 0x006C0094u, 0x00220042u,
                      525u,             80u,         48u,        2u, 0u};
    if (emit_vi_vector(&vdd16, out_dir)) return 1;

    // AA-edge: aa = 1 → every 4th column partial (cvg 0), so a partial pixel takes the
    // video filter over its fully-covered taps. aa_mode 0, no dither → 0x00000002.
    ViVector vaa16 = {"vi_aa_edge_16", 0x00000002u, 0x1000u, 80u,
                      0x00000400u,     0x00000400u, 0x006C0094u, 0x00220042u,
                      525u,            80u,         48u,        2u, 1u};
    if (emit_vi_vector(&vaa16, out_dir)) return 1;

    // Divot: aa = 2 → the partial pattern plus the non-monotonic fully-covered probe
    // triplet, so the all-fully-covered early-return is observable. divot_enable (bit
    // 4), aa_mode 0 → 0x00000012.
    ViVector vdivot16 = {"vi_divot_16", 0x00000012u, 0x1000u, 80u,
                         0x00000400u,   0x00000400u, 0x006C0094u, 0x00220042u,
                         525u,          80u,         48u,        2u, 2u};
    if (emit_vi_vector(&vdivot16, out_dir)) return 1;

    return 0;
}

int main(int argc, char **argv) {
    // Fuzz mode: `./driver --fuzz <out_dir> <seed> <count>` emits a reproducible
    // candidate corpus (curated on the Rust side) instead of the fixed vectors.
    // Both numeric arguments are validated: a malformed or out-of-range value must
    // fail loudly, never emit nothing and return success (the accuracy oracle must
    // not report success without actually generating vectors).
    if (argc >= 5 && strcmp(argv[1], "--fuzz") == 0) {
        char *endp = NULL;
        errno = 0;
        unsigned long long seed = strtoull(argv[3], &endp, 0);
        if (errno != 0 || endp == argv[3] || *endp != '\0') {
            fprintf(stderr, "--fuzz: invalid seed '%s'\n", argv[3]);
            return 1;
        }
        errno = 0;
        long count = strtol(argv[4], &endp, 0);
        if (errno != 0 || endp == argv[4] || *endp != '\0' || count <= 0 || count > 100000) {
            fprintf(stderr, "--fuzz: invalid count '%s' (want 1..100000)\n", argv[4]);
            return 1;
        }
        const char *family = (argc >= 6) ? argv[5] : "fillrect";
        return emit_fuzz(argv[2], seed, (int)count, family);
    }
    const char *out_dir = (argc > 1) ? argv[1] : ".";
    // Fill the 8x8 gradient texture (RGBA5551: R = 4x, G = 4y, B = 0, alpha 1).
    for (uint32_t y = 0; y < 8; y++)
        for (uint32_t x = 0; x < 8; x++)
            tex8x8[y * 8 + x] =
                (uint16_t)(((x * 4u) << 11) | ((y * 4u) << 6) | 1u);
    // Checkerboard for the mid-texel vector: bright red / dark by (x+y) parity.
    for (uint32_t y = 0; y < 8; y++)
        for (uint32_t x = 0; x < 8; x++)
            tex_chk8x8[y * 8 + x] = ((x + y) & 1u) ? 0x0001u : 0xF801u;

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

    Vector v22 = {"tex_tri_fixed_16", 0x2000, 0x1000, 8, 8, 2,
                  sizeof(V22_TEX_TRI_FIXED_16) / 4, V22_TEX_TRI_FIXED_16,
                  0x3000, 8, TEX8_RAMP};
    if (emit_vector(&v22, out_dir)) return 1;

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

    Vector v19 = {"prim_combiner_32", 0x2000, 0x1000, 8, 8, 4,
                  sizeof(V19_PRIM_COMBINER_32) / 4, V19_PRIM_COMBINER_32};
    if (emit_vector(&v19, out_dir)) return 1;

    Vector v20 = {"tex_tri_i4_16", 0x2000, 0x1000, 8, 8, 2,
                  sizeof(V20_TEX_TRI_I4_16) / 4, V20_TEX_TRI_I4_16,
                  0x3000, 2, TEX_I4_RAMP};
    if (emit_vector(&v20, out_dir)) return 1;

    Vector v21 = {"tex_tri_clamp_16", 0x2000, 0x1000, 8, 8, 2,
                  sizeof(V21_TEX_TRI_CLAMP_16) / 4, V21_TEX_TRI_CLAMP_16,
                  0x3000, 4, TEX_CLAMP4};
    if (emit_vector(&v21, out_dir)) return 1;

    Vector v21b = {"tex_tri_wrap_16", 0x2000, 0x1000, 8, 8, 2,
                   sizeof(V21_TEX_TRI_WRAP_16) / 4, V21_TEX_TRI_WRAP_16,
                   0x3000, 4, TEX_CLAMP4};
    if (emit_vector(&v21b, out_dir)) return 1;

    Vector v23 = {"tex_tri_bilinear_16", 0x2000, 0x1000, 8, 8, 2,
                  sizeof(V23_TEX_TRI_BILINEAR_16) / 4, V23_TEX_TRI_BILINEAR_16,
                  0x3000, 64, tex8x8};
    if (emit_vector(&v23, out_dir)) return 1;

    Vector v24 = {"tex_tri_bilinear_wrap_16", 0x2000, 0x1000, 8, 8, 2,
                  sizeof(V24_TEX_TRI_BILINEAR_WRAP_16) / 4, V24_TEX_TRI_BILINEAR_WRAP_16,
                  0x3000, 2, TEX_SEAM2};
    if (emit_vector(&v24, out_dir)) return 1;

    Vector v25 = {"tex_tri_2cycle_16", 0x2000, 0x1000, 8, 8, 2,
                  sizeof(V25_TEX_TRI_2CYCLE_16) / 4, V25_TEX_TRI_2CYCLE_16,
                  0x3000, 2, TEX_2CYC};
    if (emit_vector(&v25, out_dir)) return 1;

    Vector v26 = {"tex_tri_primlodfrac_16", 0x2000, 0x1000, 8, 8, 2,
                  sizeof(V26_TEX_TRI_PRIMLODFRAC_16) / 4, V26_TEX_TRI_PRIMLODFRAC_16,
                  0, 0, NULL};
    if (emit_vector(&v26, out_dir)) return 1;

    Vector v27 = {"tex_tri_convert_k45_16", 0x2000, 0x1000, 8, 8, 2,
                  sizeof(V27_TEX_TRI_CONVERT_K45_16) / 4, V27_TEX_TRI_CONVERT_K45_16,
                  0, 0, NULL};
    if (emit_vector(&v27, out_dir)) return 1;

    Vector v28 = {"tex_tri_convert_kneg_16", 0x2000, 0x1000, 8, 8, 2,
                  sizeof(V28_TEX_TRI_CONVERT_KNEG_16) / 4, V28_TEX_TRI_CONVERT_KNEG_16,
                  0, 0, NULL};
    if (emit_vector(&v28, out_dir)) return 1;

    Vector v29 = {"tex_tri_chromakey_16", 0x2000, 0x1000, 8, 8, 2,
                  sizeof(V29_TEX_TRI_CHROMAKEY_16) / 4, V29_TEX_TRI_CHROMAKEY_16,
                  0, 0, NULL};
    if (emit_vector(&v29, out_dir)) return 1;

    Vector v30 = {"tex_tri_chromakey_alpha_16", 0x2000, 0x1000, 8, 8, 2,
                  sizeof(V30_TEX_TRI_CHROMAKEY_ALPHA_16) / 4, V30_TEX_TRI_CHROMAKEY_ALPHA_16,
                  0, 0, NULL};
    if (emit_vector(&v30, out_dir)) return 1;

    Vector v31 = {"tex_tri_base_tile_16", 0x2000, 0x1000, 8, 8, 2,
                  sizeof(V31_TEX_TRI_BASE_TILE_16) / 4, V31_TEX_TRI_BASE_TILE_16,
                  0x3000, 8, TEX8_RAMP};
    if (emit_vector(&v31, out_dir)) return 1;

    Vector v32 = {"tex_tri_mid_texel_16", 0x2000, 0x1000, 8, 8, 2,
                  sizeof(V32_TEX_TRI_MID_TEXEL_16) / 4, V32_TEX_TRI_MID_TEXEL_16,
                  0x3000, 64, tex_chk8x8};
    if (emit_vector(&v32, out_dir)) return 1;

    Vector v33 = {"tex_tri_lodfrac_16", 0x2000, 0x1000, 8, 8, 2,
                  sizeof(V33_TEX_TRI_LODFRAC_16) / 4, V33_TEX_TRI_LODFRAC_16,
                  0x3000, 8, TEX8_RAMP};
    if (emit_vector(&v33, out_dir)) return 1;

    Vector v34 = {"tex_tri_mip_tile_16", 0x2000, 0x1000, 8, 8, 2,
                  sizeof(V34_TEX_TRI_MIP_TILE_16) / 4, V34_TEX_TRI_MIP_TILE_16,
                  0x3000, 3, TEX_MIP3};
    if (emit_vector(&v34, out_dir)) return 1;

    if (emit_vi_vectors(out_dir)) return 1;

    return 0;
}
