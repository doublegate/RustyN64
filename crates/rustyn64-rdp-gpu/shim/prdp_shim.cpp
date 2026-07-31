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
#include <cstdlib>

// The alignment `VK_EXT_external_memory_host` needs to import host memory
// directly. See the note on `prdp_create` in the header: getting this wrong is
// not an error, it is a silent fallback to staging every access through a copy.
// 4096 is the value on the device this was developed against; a driver asking
// for more would fall back again, and would do so quietly.
#define PRDP_RDRAM_ALIGN 4096u

namespace {

// The scanout memcpy below treats `RGBA` as a 32-bit pixel. That is true of
// upstream's `struct RGBA { uint8_t r, g, b, a; }`, and asserting it here is what
// turns "true when this was written" into "true when this compiles".
static_assert(sizeof(RDP::RGBA) == sizeof(uint32_t),
              "RDP::RGBA is no longer 32 bits; the scanout copy is wrong");
static_assert(alignof(RDP::RGBA) <= alignof(uint32_t),
              "RDP::RGBA over-aligns for a uint32_t copy");

// `free`-on-destruction wrapper for the aligned RDRAM allocation, so an early
// return anywhere in `prdp_create` cannot leak it.
struct AlignedBuffer {
    void *ptr = nullptr;
    size_t size = 0;

    ~AlignedBuffer() { std::free(ptr); }
    AlignedBuffer() = default;
    AlignedBuffer(const AlignedBuffer &) = delete;
    AlignedBuffer &operator=(const AlignedBuffer &) = delete;
};

struct Ctx {
    Vulkan::Context context;
    Vulkan::Device device;
    // Declared BEFORE `processor` so it is destroyed AFTER it: the
    // CommandProcessor holds this pointer for its whole life, and members are
    // destroyed in reverse declaration order.
    AlignedBuffer rdram;
    std::unique_ptr<RDP::CommandProcessor> processor;
    // Reused across frames so a per-frame scanout does not reallocate. The
    // buffer belongs to the shim; the Rust side only ever sees a copy.
    std::vector<RDP::RGBA> scratch;
};

} // namespace

struct prdp_ctx : Ctx {};

prdp_ctx *prdp_create(size_t rdram_size, size_t hidden_rdram_size)
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

        if (rdram_size == 0 || rdram_size % PRDP_RDRAM_ALIGN != 0)
            return nullptr;

        auto ctx = std::make_unique<prdp_ctx>();

        // `aligned_alloc` requires the size to be a multiple of the alignment,
        // which the check above enforces rather than rounding up silently -- a
        // rounded size would leave the caller and the device disagreeing about
        // how much RDRAM exists.
        ctx->rdram.ptr = std::aligned_alloc(PRDP_RDRAM_ALIGN, rdram_size);
        if (!ctx->rdram.ptr)
            return nullptr;
        ctx->rdram.size = rdram_size;
        // Power-on RDRAM is zero, and an uninitialized read here would make a
        // frame depend on whatever the allocator handed back.
        std::memset(ctx->rdram.ptr, 0, rdram_size);

        // Headless: no instance or device extensions, because the first cut
        // reads pixels back on the CPU (ADR 0014 §5) and therefore needs no
        // surface, swapchain or windowing integration at all.
        if (!ctx->context.init_instance_and_device(nullptr, 0, nullptr, 0))
            return nullptr;

        ctx->device.set_context(ctx->context);

        RDP::CommandProcessorFlags flags = 0;
        ctx->processor = std::make_unique<RDP::CommandProcessor>(
            ctx->device, ctx->rdram.ptr, 0, rdram_size, hidden_rdram_size, flags);

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

int prdp_enqueue_command(prdp_ctx *ctx, uint32_t num_words, const uint32_t *words)
{
    if (!ctx || !ctx->processor || !words || num_words == 0)
        return 0;
    try {
        ctx->processor->enqueue_command(num_words, words);
        return 1;
    } catch (...) {
        // The exception still cannot cross into Rust — that is undefined
        // behavior — but it no longer vanishes: the caller gets 0 and can say
        // so at the point of failure rather than discovering a wrong picture
        // several frames later with nothing pointing back here.
        return 0;
    }
}

int prdp_set_vi_register(prdp_ctx *ctx, uint32_t reg, uint32_t value)
{
    if (!ctx || !ctx->processor)
        return 0;
    // Refuse an index outside the enum rather than casting it. `VIRegister` has
    // no fixed underlying type, so a cast of an out-of-range value is not a
    // number the C++ side ever defined — and this entry point is reachable from
    // C with anything, not only from the Rust enum.
    if (reg >= static_cast<uint32_t>(RDP::VIRegister::Count))
        return 0;
    try {
        ctx->processor->set_vi_register(static_cast<RDP::VIRegister>(reg), value);
        return 1;
    } catch (...) {
        return 0;
    }
}

size_t prdp_scanout_sync(prdp_ctx *ctx, uint32_t *out, size_t out_capacity_pixels,
                         uint32_t *width, uint32_t *height)
{
    if (!ctx || !ctx->processor || !out || !width || !height)
        return 0;
    try {
        // `unsigned`, not `uint32_t`: upstream takes `unsigned &`, and a
        // reference will not bind through a typedef that is a different type.
        // They are the same type on every target here, and relying on that
        // silently is exactly the assumption the header stopped making.
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
        *width = static_cast<uint32_t>(w);
        *height = static_cast<uint32_t>(h);
        return pixels;
    } catch (...) {
        return 0;
    }
}

int prdp_flush(prdp_ctx *ctx)
{
    if (!ctx || !ctx->processor)
        return 0;
    try {
        ctx->processor->flush();
        return 1;
    } catch (...) {
        return 0;
    }
}

int prdp_idle(prdp_ctx *ctx)
{
    if (!ctx || !ctx->processor)
        return 0;
    try {
        ctx->processor->idle();
        return 1;
    } catch (...) {
        return 0;
    }
}

void *prdp_rdram_ptr(prdp_ctx *ctx)
{
    if (!ctx || !ctx->processor)
        return nullptr;
    try {
        return ctx->processor->begin_read_rdram();
    } catch (...) {
        return nullptr;
    }
}

int prdp_end_write_rdram(prdp_ctx *ctx)
{
    if (!ctx || !ctx->processor)
        return 0;
    try {
        ctx->processor->end_write_rdram();
        return 1;
    } catch (...) {
        return 0;
    }
}

size_t prdp_rdram_size(const prdp_ctx *ctx)
{
    return ctx ? ctx->rdram.size : 0;
}
