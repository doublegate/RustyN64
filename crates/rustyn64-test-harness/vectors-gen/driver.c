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
} Vector;

// Render one vector through Angrylion and write its `.rvec` file.
static int emit_vector(const Vector *v, const char *out_dir) {
    engine_init();

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

    // Header (9 big-endian u32): magic, version, fb_addr, width, height, bpp,
    // cmd_addr, cmd_len, fb_len.
    uint32_t hdr[9] = {0x52564543u, 1u, v->fb_addr, v->width, v->height,
                       v->bpp, v->cmd_addr, cmd_len, fb_len};
    for (int i = 0; i < 9; i++) {
        uint8_t be[4] = {hdr[i] >> 24, hdr[i] >> 16, hdr[i] >> 8, hdr[i]};
        wr(be, 4, f);
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
    0x00FF0000u, 0x000000FFu, // shade int base: R=0xFF, G=0, B=0, A=0xFF (flat red)
    0x00000000u, 0x00000000u, // dx int
    0x00000000u, 0x00000000u, // frac base
    0x00000000u, 0x00000000u, // dx frac
    0x00000000u, 0x00000000u, // de int
    0x00000000u, 0x00000000u, // dy int
    0x00000000u, 0x00000000u, // de frac
    0x00000000u, 0x00000000u, // dy frac
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
    0x00FF0000u, 0x000000FFu, // shade int base: red (8 u64 shade words)
    0x00000000u, 0x00000000u,
    0x00000000u, 0x00000000u,
    0x00000000u, 0x00000000u,
    0x00000000u, 0x00000000u,
    0x00000000u, 0x00000000u,
    0x00000000u, 0x00000000u,
    0x00000000u, 0x00000000u,
    0x08000000u, 0x00000000u, // z suffix: z = 0x0800_0000 (near), dzdx = 0
    0x00000000u, 0x00000000u, // dzde, dzdy
};

int main(int argc, char **argv) {
    const char *out_dir = (argc > 1) ? argv[1] : ".";

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

    return 0;
}
