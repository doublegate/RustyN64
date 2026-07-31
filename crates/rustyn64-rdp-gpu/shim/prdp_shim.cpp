// The C++ half of the flat shim. See `prdp_shim.h` for the contract and its
// provenance (upstream MIT headers only).
//
// Every entry point is `noexcept` in effect: parallel-rdp and Granite can throw,
// and an exception crossing into Rust is undefined behavior, so each one catches
// everything and reports through the return value instead. That is not defensive
// padding — it is the boundary condition that makes the Rust side's `unsafe`
// blocks justifiable.

#include "prdp_shim.h"

#include "rdp_device.hpp"
#include "context.hpp"
#include "device.hpp"

#include <memory>
#include <vector>
#include <cstring>

namespace {

// The scanout memcpy below treats `RGBA` as a 32-bit pixel. That is true of
// upstream's `struct RGBA { uint8_t r, g, b, a; }`, and asserting it here is what
// turns "true when this was written" into "true when this compiles".
static_assert(sizeof(RDP::RGBA) == sizeof(uint32_t),
              "RDP::RGBA is no longer 32 bits; the scanout copy is wrong");
static_assert(alignof(RDP::RGBA) <= alignof(uint32_t),
              "RDP::RGBA over-aligns for a uint32_t copy");

struct Ctx {
    Vulkan::Context context;
    Vulkan::Device device;
    std::unique_ptr<RDP::CommandProcessor> processor;
    // Reused across frames so a per-frame scanout does not reallocate. The
    // buffer belongs to the shim; the Rust side only ever sees a copy.
    std::vector<RDP::RGBA> scratch;
};

} // namespace

struct prdp_ctx : Ctx {};

prdp_ctx *prdp_create(void *rdram, size_t rdram_size, size_t hidden_rdram_size)
{
    try {
        // volk resolves the loader lazily, and *nothing else calls this*.
        // Without it `volkGetInstanceVersion()` reports 0, and Granite refuses
        // with "Vulkan loader does not support required Vulkan version" on a
        // machine with a perfectly good Vulkan 1.4 loader — an error message
        // that names the wrong cause entirely. Passing null asks volk to
        // `dlopen` the system loader itself.
        if (!Vulkan::Context::init_loader(nullptr))
            return nullptr;

        auto ctx = std::make_unique<prdp_ctx>();

        // Headless: no instance or device extensions, because the first cut
        // reads pixels back on the CPU (ADR 0014 §5) and therefore needs no
        // surface, swapchain or windowing integration at all.
        if (!ctx->context.init_instance_and_device(nullptr, 0, nullptr, 0))
            return nullptr;

        ctx->device.set_context(ctx->context);

        RDP::CommandProcessorFlags flags = 0;
        ctx->processor = std::make_unique<RDP::CommandProcessor>(
            ctx->device, rdram, 0, rdram_size, hidden_rdram_size, flags);

        if (!ctx->processor->device_is_supported())
            return nullptr;

        return ctx.release();
    } catch (...) {
        return nullptr;
    }
}

void prdp_destroy(prdp_ctx *ctx)
{
    if (!ctx)
        return;
    // Drain before tearing down. The Rust side's `Drop` states this as the
    // reason no in-flight command can outlive the RDRAM borrow, so it has to
    // actually happen here rather than be inferred from destructor ordering.
    if (ctx->processor) {
        try {
            ctx->processor->idle();
        } catch (...) {
        }
    }
    delete ctx;
}

int prdp_device_is_supported(const prdp_ctx *ctx)
{
    if (!ctx || !ctx->processor)
        return 0;
    try {
        return ctx->processor->device_is_supported() ? 1 : 0;
    } catch (...) {
        return 0;
    }
}

void prdp_enqueue_command(prdp_ctx *ctx, unsigned num_words, const uint32_t *words)
{
    if (!ctx || !ctx->processor || !words || num_words == 0)
        return;
    try {
        ctx->processor->enqueue_command(num_words, words);
    } catch (...) {
        // Swallowed deliberately: there is no channel to report on, and an
        // exception crossing into Rust is undefined behavior. A dropped command
        // shows up as a wrong picture, which the differential comparison against
        // the software rasterizer is there to catch.
    }
}

void prdp_set_vi_register(prdp_ctx *ctx, unsigned reg, uint32_t value)
{
    if (!ctx || !ctx->processor)
        return;
    try {
        ctx->processor->set_vi_register(static_cast<RDP::VIRegister>(reg), value);
    } catch (...) {
    }
}

size_t prdp_scanout_sync(prdp_ctx *ctx, uint32_t *out, size_t out_capacity_pixels,
                         unsigned *width, unsigned *height)
{
    if (!ctx || !ctx->processor || !out || !width || !height)
        return 0;
    try {
        unsigned w = 0, h = 0;
        ctx->scratch.clear();
        ctx->processor->scanout_sync(ctx->scratch, w, h);

        const size_t pixels = static_cast<size_t>(w) * static_cast<size_t>(h);
        if (pixels == 0 || pixels > ctx->scratch.size())
            return 0;
        // Refuse rather than truncate: a partial frame is a plausible-looking
        // wrong picture, and the caller cannot tell it from a real one.
        if (pixels > out_capacity_pixels)
            return 0;

        std::memcpy(out, ctx->scratch.data(), pixels * sizeof(uint32_t));
        *width = w;
        *height = h;
        return pixels;
    } catch (...) {
        return 0;
    }
}

void prdp_flush(prdp_ctx *ctx)
{
    if (!ctx || !ctx->processor)
        return;
    try {
        ctx->processor->flush();
    } catch (...) {
    }
}

void prdp_idle(prdp_ctx *ctx)
{
    if (!ctx || !ctx->processor)
        return;
    try {
        ctx->processor->idle();
    } catch (...) {
    }
}
