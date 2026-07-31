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
 * "Directly" is conditional on ALIGNMENT, and the failure is silent. The direct
 * path is `VK_EXT_external_memory_host`, which requires the pointer to meet the
 * driver's `minImportedHostPointerAlignment` (4096 on the NVIDIA device this was
 * developed against). An unaligned buffer is not rejected: parallel-rdp logs
 * "Host buffer is not aligned appropriately", falls back to staging every
 * access through a copy, and everything still works — slower, with nothing in
 * the return value to say so. A plain `Vec<u8>` does not meet it. Align the
 * buffer to a page, or accept the copy knowingly.
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

/* The operations below return non-zero on success and 0 on failure.
 *
 * They return a status rather than `void` because the C++ side can fail at
 * runtime — device loss, out of memory, a command buffer that cannot be
 * allocated — and swallowing that in a `void` leaves the caller unable to tell
 * a dropped command from a submitted one. A dropped RDP command surfaces as a
 * wrong picture much later, in a place that gives no hint where it came from. */

/* Submit `num_words` of RDP command stream. */
int prdp_enqueue_command(prdp_ctx *ctx, uint32_t num_words, const uint32_t *words);

/* Set one VI register, indexed as parallel-rdp's `VIRegister` enum. Returns 0
 * for an index outside that enum rather than casting it, since the cast would
 * be a value the C++ side never defined. */
int prdp_set_vi_register(prdp_ctx *ctx, uint32_t reg, uint32_t value);

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
                         uint32_t *width, uint32_t *height);

int prdp_flush(prdp_ctx *ctx);
int prdp_idle(prdp_ctx *ctx);

#ifdef __cplusplus
}
#endif
#endif /* RUSTYN64_PRDP_SHIM_H */
