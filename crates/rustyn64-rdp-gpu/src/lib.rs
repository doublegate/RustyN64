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
pub use backend::{GpuRdp, ScanoutFrame, ViRegister};

#[cfg(feature = "gpu-rdp")]
mod backend {
    use core::ffi::c_void;
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
            rdram: *mut c_void,
            rdram_size: usize,
            hidden_rdram_size: usize,
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

    /// A `parallel-rdp` command processor over a borrowed RDRAM.
    ///
    /// The borrow is the safety mechanism, not a convenience: `parallel-rdp`
    /// reads and writes the buffer directly for as long as the context lives,
    /// so the `&'r mut [u8]` makes "the memory outlives the context and nobody
    /// else touches it" a compile-time fact rather than a comment.
    pub struct GpuRdp<'r> {
        ctx: NonNull<PrdpCtx>,
        rdram: PhantomData<&'r mut [u8]>,
    }

    impl<'r> GpuRdp<'r> {
        /// Create a headless Vulkan device and a command processor over `rdram`.
        ///
        /// **Align `rdram` to a page.** parallel-rdp maps the buffer into the
        /// GPU's address space via `VK_EXT_external_memory_host` when the
        /// pointer meets the driver's `minImportedHostPointerAlignment` (4096
        /// on the device this was developed against), and otherwise stages every
        /// access through a copy. The fallback is **silent** — it logs and keeps
        /// working, and nothing in the return value distinguishes the two. A
        /// plain `Vec<u8>` does not meet it; `tests/smoke.rs` shows one way to
        /// get an aligned buffer.
        ///
        /// Returns `None` when Vulkan is unavailable, the device cannot run
        /// `parallel-rdp`, or initialization fails. There is deliberately no
        /// richer error: the shim catches every C++ exception and reports a null
        /// pointer, because an exception unwinding into Rust is undefined
        /// behavior and a lossy diagnosis beats an unsound one.
        #[must_use]
        pub fn new(rdram: &'r mut [u8], hidden_rdram_size: usize) -> Option<Self> {
            let len = rdram.len();
            let ptr = rdram.as_mut_ptr().cast::<c_void>();
            // SAFETY: `ptr`/`len` describe the live `&mut` borrow held for `'r`,
            // which the returned value's lifetime ties the context to — so the
            // pointer stays valid and unaliased for as long as the C++ side may
            // use it. `prdp_create` is documented to return null rather than
            // throw on any failure, so no unwind crosses the boundary.
            let raw = unsafe { prdp_create(ptr, len, hidden_rdram_size) };
            Some(Self {
                ctx: NonNull::new(raw)?,
                rdram: PhantomData,
            })
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

    impl Drop for GpuRdp<'_> {
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
