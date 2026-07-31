/* Flat C surface over parallel-rdp's `CommandProcessor` (ADR 0014).
 *
 * Written against `parallel-rdp/rdp_device.hpp` and `vulkan/context.hpp` from
 * the vendored upstream (MIT, Themaister 2020), which is the only source this
 * was derived from. gopher64 has a binding of the same library and is GPLv3;
 * per `ref-proj/README.md` it is study-only, so nothing here is copied from it.
 *
 * EVERY type crossing this boundary is POD or a pointer. No C++ type, no
 * exception, no allocation whose ownership is ambiguous. That is what lets the
 * Rust side state its safety invariants in terms it can actually check.
 */
#ifndef RUSTYN64_PRDP_SHIM_H
#define RUSTYN64_PRDP_SHIM_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Opaque handle. The Rust side never dereferences it. */
typedef struct prdp_ctx prdp_ctx;

/* Create a headless Vulkan device and a CommandProcessor over `rdram`.
 *
 * `rdram` must stay valid, and must not move, until `prdp_destroy`. parallel-rdp
 * reads and writes it directly — that is the whole point of handing it over
 * rather than copying.
 *
 * Returns NULL if Vulkan is unavailable, the device is unsupported, or the
 * CommandProcessor declines to initialize. A NULL return is the ONLY failure
 * signal: nothing here throws across the boundary. */
prdp_ctx *prdp_create(void *rdram, size_t rdram_size, size_t hidden_rdram_size);

void prdp_destroy(prdp_ctx *ctx);

/* Non-zero when the device satisfies parallel-rdp's requirements. Separate from
 * a NULL `prdp_create` because "no Vulkan at all" and "a Vulkan that cannot run
 * this" are different diagnoses for a user. */
int prdp_device_is_supported(const prdp_ctx *ctx);

/* Submit `num_words` of RDP command stream. */
void prdp_enqueue_command(prdp_ctx *ctx, unsigned num_words, const uint32_t *words);

/* Set one VI register, indexed as parallel-rdp's `VIRegister` enum. */
void prdp_set_vi_register(prdp_ctx *ctx, unsigned reg, uint32_t value);

/* Rasterize and read back one frame into `out` as RGBA8.
 *
 * `out_capacity_pixels` bounds the write; on success `*width`/`*height` carry
 * the produced geometry and the return value is the pixel count written. A
 * frame larger than the buffer writes NOTHING and returns 0 rather than
 * truncating, because a truncated frame is a plausible-looking wrong picture.
 *
 * This is the synchronous path ADR 0014 §5 names: it hands back a CPU-side
 * buffer, so the first working version needs no Vulkan in the presenter. */
size_t prdp_scanout_sync(prdp_ctx *ctx, uint32_t *out, size_t out_capacity_pixels,
                         unsigned *width, unsigned *height);

void prdp_flush(prdp_ctx *ctx);
void prdp_idle(prdp_ctx *ctx);

#ifdef __cplusplus
}
#endif
#endif /* RUSTYN64_PRDP_SHIM_H */
