//! Optional GPU-backed RDP — a binding over the vendored `parallel-rdp`
//! (ADR 0014).
//!
//! This crate is the **only** place in the tree where `unsafe` is permitted
//! outside the frontend (ADR 0014 §3). Every chip crate and `rustyn64-core`
//! keep `#![forbid(unsafe_code)]` unchanged; the FFI is quarantined here so
//! that claim stays checkable by grep rather than by reading.
//!
//! # What is and is not here
//!
//! This is the **binding**, not the backend. It compiles the vendored C++,
//! creates a headless Vulkan device, submits an RDP command stream, and reads
//! one frame back to the CPU as RGBA8. It is not wired into `Bus`, it does not
//! share RDRAM with a running machine, and the software rasterizer remains the
//! oracle (ADR 0014 §2 — that does not change if this ever ships).
//!
//! # Availability
//!
//! Everything below is behind the default-off `gpu-rdp` feature. With the
//! feature off this crate is [`AVAILABLE`] and nothing else: no C++ compiler is
//! invoked, no submodule is required, and `unsafe` is *forbidden* rather than
//! merely absent. That is the promise ADR 0014 §7 lists as a kill criterion —
//! "the Vulkan dependency cannot be made optional" — kept in a form the compiler
//! enforces.
#![cfg_attr(not(feature = "gpu-rdp"), forbid(unsafe_code))]
#![cfg_attr(feature = "gpu-rdp", allow(unsafe_code))]
#![no_std]

/// Whether this build can construct a `GpuRdp`.
///
/// Deliberately a plain code span, not an intra-doc link: `GpuRdp` only exists
/// under `gpu-rdp`, and the default doc build runs with `-D warnings`.
///
/// `false` unless the `gpu-rdp` feature is on. Exposed unconditionally so a
/// caller can report the reason a GPU backend is unavailable without needing
/// its own `cfg`.
pub const AVAILABLE: bool = cfg!(feature = "gpu-rdp");

#[cfg(feature = "gpu-rdp")]
pub use backend::{GpuRdp, ScanoutFrame, Upscale, ViRegister};

#[cfg(feature = "gpu-rdp")]
mod backend {
    use core::cell::Cell;
    use core::marker::PhantomData;
    use core::ptr::NonNull;

    /// Opaque `prdp_ctx` from `shim/prdp_shim.h`. Never dereferenced here.
    #[repr(C)]
    struct PrdpCtx {
        _private: [u8; 0],
    }

    // Mirrors `shim/prdp_shim.h` exactly, and **nothing checks that** — not the
    // compiler, not the linker, not any test here. Linking resolves symbols; a
    // declaration whose parameter types, order, or return type disagree with the
    // header links exactly as cleanly as a correct one, then corrupts the stack
    // at run time. These are held correct by review, which is why the header is
    // kept short enough to diff by eye and why both are edited together.
    unsafe extern "C" {
        fn prdp_create(
            rdram_size: usize,
            hidden_rdram_size: usize,
            upscale: core::ffi::c_uint,
        ) -> *mut PrdpCtx;
        fn prdp_destroy(ctx: *mut PrdpCtx);
        fn prdp_device_is_supported(ctx: *const PrdpCtx) -> i32;
        fn prdp_enqueue_command(ctx: *mut PrdpCtx, num_words: u32, words: *const u32) -> i32;
        fn prdp_set_vi_register(ctx: *mut PrdpCtx, reg: u32, value: u32) -> i32;
        fn prdp_scanout_sync(
            ctx: *mut PrdpCtx,
            out: *mut u32,
            out_capacity_pixels: usize,
            width: *mut u32,
            height: *mut u32,
        ) -> usize;
        fn prdp_flush(ctx: *mut PrdpCtx) -> i32;
        fn prdp_idle(ctx: *mut PrdpCtx) -> i32;
        fn prdp_rdram_ptr(ctx: *mut PrdpCtx) -> *mut u8;
        fn prdp_end_write_rdram(ctx: *mut PrdpCtx) -> i32;
        fn prdp_rdram_size(ctx: *const PrdpCtx) -> usize;
    }

    /// Internal render scale for the GPU RDP.
    ///
    /// Higher factors are **quality only** — the scan-out is downscaled back to
    /// 1x, so this is supersampling and costs the emulation thread nothing. The
    /// GPU has the headroom: `docs/performance.md` measures the whole present
    /// path at ~4% of a frame, of which the RDP's own rasterization is ~1.7%.
    ///
    /// VRAM scales with the **square** of the factor, so `X8` is ~64x the
    /// buffers. `Native` is the default and the shipped behavior.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub enum Upscale {
        /// 1x — what the hardware does, and the default.
        #[default]
        Native,
        /// 2x internal render.
        X2,
        /// 4x internal render.
        X4,
        /// 8x internal render. ~64x the VRAM; offer it, do not default to it.
        X8,
    }

    impl Upscale {
        /// The factor the shim expects.
        #[must_use]
        pub const fn factor(self) -> core::ffi::c_uint {
            match self {
                Self::Native => 1,
                Self::X2 => 2,
                Self::X4 => 4,
                Self::X8 => 8,
            }
        }
    }

    /// The VI registers `parallel-rdp` accepts, in its own enum order.
    ///
    /// Transcribed from `parallel-rdp/rdp_common.hpp`'s `VIRegister`. The
    /// discriminants are the ABI — reordering these silently misprograms the
    /// video interface, which is why they are written out rather than derived.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[repr(u32)]
    pub enum ViRegister {
        /// `VI_CONTROL` — pixel format, AA mode, filter enables.
        Control = 0,
        /// `VI_ORIGIN` — framebuffer base address in RDRAM.
        Origin = 1,
        /// `VI_WIDTH` — framebuffer width in pixels.
        Width = 2,
        /// `VI_INTR` — the scanline that raises the VI interrupt.
        Intr = 3,
        /// `VI_V_CURRENT` — the current half-line.
        VCurrentLine = 4,
        /// `VI_TIMING` — burst timing.
        Timing = 5,
        /// `VI_V_SYNC` — total half-lines per field.
        VSync = 6,
        /// `VI_H_SYNC` — total pixels per half-line.
        HSync = 7,
        /// `VI_LEAP` — the horizontal sync leap pattern.
        Leap = 8,
        /// `VI_H_START` — the active horizontal window.
        HStart = 9,
        /// `VI_V_START` — the active vertical window.
        VStart = 10,
        /// `VI_V_BURST` — color burst placement.
        VBurst = 11,
        /// `VI_X_SCALE` — horizontal scale and subpixel offset.
        XScale = 12,
        /// `VI_Y_SCALE` — vertical scale and subpixel offset.
        YScale = 13,
    }

    /// The geometry of a frame produced by [`GpuRdp::scanout_sync`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ScanoutFrame {
        /// Width in pixels.
        pub width: u32,
        /// Height in pixels.
        pub height: u32,
        /// Pixels actually written to the caller's buffer.
        pub pixels: usize,
    }

    /// Publishes host writes back to the device when dropped — **including on
    /// the unwind path** — and records whether that succeeded.
    ///
    /// `with_rdram_mut`'s closure can panic (the conformance harness's word swap
    /// asserts on a length mismatch, for one), and a panic caught higher up would
    /// otherwise skip `end_write_rdram` and leave the device rendering from stale
    /// memory with nothing having reported an error.
    ///
    /// The outcome goes into a `Cell` rather than being returned, because `drop`
    /// cannot return anything — and it must be *recorded* rather than discarded:
    /// a failed publish means the host writes never reached the device, which is
    /// exactly the "dropped operation resurfacing as a wrong picture later" that
    /// every other entry point returns a status to prevent.
    struct Publish<'a> {
        ctx: NonNull<PrdpCtx>,
        published: &'a Cell<bool>,
    }

    impl Drop for Publish<'_> {
        fn drop(&mut self) {
            // SAFETY: the guard is created from a live `GpuRdp`'s context and
            // dropped before that `GpuRdp` can be, so the context is valid here.
            self.published
                .set(unsafe { prdp_end_write_rdram(self.ctx.as_ptr()) } != 0);
        }
    }

    /// A `parallel-rdp` command processor and the RDRAM it renders into.
    ///
    /// The RDRAM is **owned by the backend**, not borrowed. That is a deliberate
    /// reversal of the first design: it has to be page-aligned or the direct
    /// host-import path is silently replaced by a staging copy, and a contract a
    /// caller can satisfy by accident is worse than no contract. Owning it means
    /// the alignment is a property of construction rather than of the caller
    /// remembering. Reach it through [`GpuRdp::with_rdram`] and
    /// [`GpuRdp::with_rdram_mut`].
    pub struct GpuRdp {
        ctx: NonNull<PrdpCtx>,
        /// `GpuRdp` is not `Send`/`Sync`, inherited from `NonNull` and left that
        /// way on purpose: `Vulkan::Device` runs a worker thread and
        /// `CommandProcessor` holds a command ring, and nothing upstream
        /// documents that the handle may cross threads. Asserting `Send` on that
        /// basis would be an `unsafe impl` justified by a guess, and the failure
        /// mode is a data race rather than a wrong number.
        _not_send: PhantomData<*const ()>,
    }

    impl GpuRdp {
        /// Create a headless Vulkan device and a command processor with
        /// `rdram_size` bytes of RDRAM, zeroed as at power-on.
        ///
        /// `rdram_size` must be a multiple of 4096; anything else returns `None`
        /// rather than being rounded, since a rounded size would leave caller
        /// and device disagreeing about how much RDRAM exists. The alignment
        /// that requirement exists for is handled inside — parallel-rdp imports
        /// host memory directly only when the pointer meets the driver's
        /// `minImportedHostPointerAlignment`, and an unaligned buffer is not
        /// rejected but **silently** staged through a copy instead.
        ///
        /// Returns `None` when Vulkan is unavailable, the device cannot run
        /// `parallel-rdp`, or initialization fails. There is deliberately no
        /// richer error: the shim catches every C++ exception and reports a null
        /// pointer, because an exception unwinding into Rust is undefined
        /// behavior and a lossy diagnosis beats an unsound one.
        #[must_use]
        pub fn new(rdram_size: usize, hidden_rdram_size: usize) -> Option<Self> {
            Self::with_upscale(rdram_size, hidden_rdram_size, Upscale::Native)
        }

        /// As [`GpuRdp::new`], rendering internally at `upscale`.
        ///
        /// **Supersampling, not a bigger picture.** The scan-out is downscaled
        /// back to 1x, so [`GpuRdp::scanout_sync`] returns identical geometry at
        /// every factor and nothing downstream resizes. `SUPER_SAMPLED_READ_BACK`
        /// is never set, so what lands back in RDRAM is the 1x render and the
        /// emulated machine's state does not vary with this setting.
        ///
        /// Returns `None` on **any** failure — no Vulkan, an unsupported device,
        /// an unsupported factor, an allocation refusal. VRAM is the likeliest
        /// cause specific to *this* constructor, because it scales with the
        /// **square** of the factor, but it is not the only one and a caller
        /// must not report it as such.
        #[must_use]
        pub fn with_upscale(
            rdram_size: usize,
            hidden_rdram_size: usize,
            upscale: Upscale,
        ) -> Option<Self> {
            // SAFETY: `prdp_create` takes two sizes and a factor by value and
            // allocates its own storage, so there is no pointer whose validity
            // this call depends on. It is documented to return null rather than
            // throw on every failure — including a size that is not a multiple
            // of the alignment, and an unsupported factor — so no unwind crosses
            // the boundary. `Upscale` can only produce one of the four it takes.
            let raw = unsafe { prdp_create(rdram_size, hidden_rdram_size, upscale.factor()) };
            Some(Self {
                ctx: NonNull::new(raw)?,
                _not_send: PhantomData,
            })
        }

        /// The RDRAM size this context was created with.
        #[must_use]
        pub fn rdram_size(&self) -> usize {
            // SAFETY: `self.ctx` came from `prdp_create` and is live; the call
            // reads one POD field and cannot fail.
            unsafe { prdp_rdram_size(self.ctx.as_ptr()) }
        }

        /// Read RDRAM.
        ///
        /// Scoped rather than returning a slice because the pointer is only
        /// valid until the next call that touches the device, and a returned
        /// `&[u8]` would have to borrow `self` in a way that made the backend
        /// unusable while it lived.
        ///
        /// Call [`idle`](Self::idle) first if work may be in flight — this does
        /// not synchronize on its own, and reading mid-render returns a
        /// half-drawn frame rather than an error.
        ///
        /// `f` is not called, and `None` is returned, if the mapping fails.
        pub fn with_rdram<R>(&mut self, f: impl FnOnce(&[u8]) -> R) -> Option<R> {
            let len = self.rdram_size();
            // SAFETY: `prdp_rdram_ptr` returns either null or a pointer to
            // `prdp_rdram_size` readable bytes — upstream's `begin_read_rdram`
            // maps whichever buffer is live. No other reference to that region
            // exists here: the C++ side owns it, `f` cannot escape the slice
            // (it is higher-ranked over the borrow), and `&mut self` excludes
            // any concurrent call through this handle.
            unsafe {
                let ptr = prdp_rdram_ptr(self.ctx.as_ptr());
                if ptr.is_null() {
                    return None;
                }
                Some(f(core::slice::from_raw_parts(ptr, len)))
            }
        }

        /// Read and write RDRAM, publishing the writes back to the device.
        ///
        /// The publish (`end_write_rdram`) happens even if `f` writes nothing,
        /// which is correct and cheap: it is what makes host writes visible when
        /// the direct-import path is unavailable and the device is rendering
        /// from a staging buffer.
        ///
        /// Returns `None` without calling `f` if the mapping fails, and `None`
        /// *after* calling `f` if the publish fails — in that case the writes
        /// reached the mapping but not the device, so the result is not usable
        /// and saying so is the whole point of the return value.
        pub fn with_rdram_mut<R>(&mut self, f: impl FnOnce(&mut [u8]) -> R) -> Option<R> {
            let len = self.rdram_size();
            // SAFETY: as `with_rdram`, and the region is writable — upstream
            // pairs `begin_read_rdram` with `end_write_rdram` for exactly this.
            // The `&mut` slice is unique: it is created from a pointer the C++
            // side is not touching (no command is in flight that this handle did
            // not submit), and `&mut self` excludes any other access through it.
            unsafe {
                let ptr = prdp_rdram_ptr(self.ctx.as_ptr());
                if ptr.is_null() {
                    return None;
                }
                let slice = core::slice::from_raw_parts_mut(ptr, len);
                let published = Cell::new(false);
                let out = {
                    let _guard = Publish {
                        ctx: self.ctx,
                        published: &published,
                    };
                    f(slice)
                };
                published.get().then_some(out)
            }
        }

        /// Whether the Vulkan device satisfies `parallel-rdp`'s requirements.
        ///
        /// Distinct from [`new`](Self::new) returning `None`: "no Vulkan at all"
        /// and "a Vulkan that cannot run this" are different things to tell a
        /// user.
        #[must_use]
        pub fn device_is_supported(&self) -> bool {
            // SAFETY: `self.ctx` came from `prdp_create` and has not been
            // destroyed — `Drop` is the only thing that destroys it, and it
            // consumes `self`.
            unsafe { prdp_device_is_supported(self.ctx.as_ptr()) != 0 }
        }

        /// Submit RDP command words. `false` if the command was not accepted.
        ///
        /// Two ways to get `false`, and neither is silent: a list longer than
        /// `u32::MAX` words (refused rather than truncated — truncation would
        /// submit a syntactically valid prefix and render a plausible wrong
        /// picture), and a runtime failure on the C++ side such as device loss.
        ///
        /// `#[must_use]` on purpose. A dropped RDP command surfaces as a wrong
        /// picture many frames later with nothing pointing back at the
        /// submission, so the caller has to say what it wants to happen.
        #[must_use]
        pub fn enqueue_command(&mut self, words: &[u32]) -> bool {
            let Ok(num_words) = u32::try_from(words.len()) else {
                return false;
            };
            // SAFETY: `words.as_ptr()` is valid for `num_words` reads for the
            // duration of the call, which is the whole extent of the C++ side's
            // access — `enqueue_command` copies into its own ring before
            // returning.
            unsafe { prdp_enqueue_command(self.ctx.as_ptr(), num_words, words.as_ptr()) != 0 }
        }

        /// Set one VI register. `false` if it was not accepted.
        #[must_use]
        pub fn set_vi_register(&mut self, reg: ViRegister, value: u32) -> bool {
            // SAFETY: `reg as u32` is one of the enum's transcribed
            // discriminants, all of which are valid `VIRegister` values. The
            // shim range-checks it regardless, because the C entry point is
            // reachable with anything.
            unsafe { prdp_set_vi_register(self.ctx.as_ptr(), reg as u32, value) != 0 }
        }

        /// Rasterize and read one frame back into `out` as RGBA8.
        ///
        /// Returns `None` when the frame does not fit, when the VI produces no
        /// picture, or when read-back fails. A frame larger than `out` writes
        /// nothing — see the header's note on truncation.
        pub fn scanout_sync(&mut self, out: &mut [u32]) -> Option<ScanoutFrame> {
            let mut width = 0u32;
            let mut height = 0u32;
            // SAFETY: `out` is valid for `out.len()` writes and `width`/`height`
            // are live locals for the duration of the call. The shim writes at
            // most `out.len()` pixels and writes both out-parameters before any
            // non-zero return.
            let pixels = unsafe {
                prdp_scanout_sync(
                    self.ctx.as_ptr(),
                    out.as_mut_ptr(),
                    out.len(),
                    &raw mut width,
                    &raw mut height,
                )
            };
            (pixels != 0).then_some(ScanoutFrame {
                width,
                height,
                pixels,
            })
        }

        /// Submit pending work without waiting for it. `false` on failure.
        #[must_use]
        pub fn flush(&mut self) -> bool {
            // SAFETY: as `device_is_supported`.
            unsafe { prdp_flush(self.ctx.as_ptr()) != 0 }
        }

        /// Wait until every submitted command has completed. `false` on failure.
        #[must_use]
        pub fn idle(&mut self) -> bool {
            // SAFETY: as `device_is_supported`.
            unsafe { prdp_idle(self.ctx.as_ptr()) != 0 }
        }
    }

    impl Drop for GpuRdp {
        fn drop(&mut self) {
            // SAFETY: `self.ctx` came from `prdp_create` and is destroyed
            // exactly once, here — `GpuRdp` is not `Clone` and hands out no
            // copy of the pointer. The shim idles the device before tearing it
            // down, so no in-flight command outlives the RDRAM borrow.
            unsafe { prdp_destroy(self.ctx.as_ptr()) }
        }
    }
}

#[cfg(test)]
mod tests {
    /// `AVAILABLE` must track the feature, not a hardcoded answer.
    ///
    /// Mutation-checked: writing `pub const AVAILABLE: bool = false;` fails this
    /// under `--features gpu-rdp`, and `true` fails it by default.
    #[test]
    fn available_tracks_the_feature() {
        assert_eq!(super::AVAILABLE, cfg!(feature = "gpu-rdp"));
    }
}
