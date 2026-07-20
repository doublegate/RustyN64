//! `rustyn64_frontend` — the `RustyN64` desktop/web shell library.
//!
//! An always-on `winit + wgpu + cpal + egui` shell wrapped around the
//! `rustyn64-core` [`System`](rustyn64_core::System). egui runs **every frame**
//! (a persistent menu bar + status bar + a stub debugger panel), the emulator
//! runs on a dedicated thread behind an `Arc<Mutex<EmuCore>>` handle + a
//! lock-free [`SharedInput`](input::SharedInput), and a wgpu fullscreen-triangle
//! pass blits the variable-size N64 video framebuffer with aspect correction.
//!
//! # The non-negotiable frontend rules
//!
//! These come from the `RustyNES` `docs/frontend.md` and the rusty-console-forge
//! `frontend_reuse.md`; they are the reason the design holds together:
//!
//! - egui runs every frame — the shell is always on, never a bare window.
//! - **Never hold the emu lock inside the egui closure.** Menu interactions
//!   return a [`MenuAction`](ui_shell::MenuAction) that the app dispatches
//!   *after* the egui pass; the render branch copies the framebuffer under a
//!   brief lock, drops it, then renders.
//! - On native the emulator runs on a dedicated `emu-thread`; the winit thread
//!   only does UI + present.
//! - The frontend owns rate-control + run-ahead (the determinism contract —
//!   never in the core).
//!
//! # v0.1 SKELETON status
//!
//! This is a structural skeleton. The shell, the input map, the audio ring, and
//! the framebuffer plumbing are real and compile; the **emulation itself is
//! stubbed** — `rustyn64-core` advances its scheduler but the LLE RSP/RDP that
//! would produce a real picture are roadmap phases, so the video framebuffer is
//! a frontend-side placeholder until the RDP scanout lands.
//!
//! # v-next: a shared `rusty-frontend-core` crate
//!
//! Most of this module tree is console-agnostic (the egui shell, the emu-thread
//! plumbing, the audio ring, the wgpu blit, the pacing). After a second or third
//! `Rusty<System>` these parts clearly want to factor into a shared
//! `rusty-frontend-core` crate, parameterized over a `Console` trait
//! (framebuffer dims, input map, the debugger-panel set), that each thin
//! `*-frontend` consumes. Recorded here as a v-next TODO (see also `CLAUDE.md`)
//! — do **not** extract it yet; lift-and-adapt first, factor later.

// The N64 VI scanout resolution is variable (common modes 320x240 and
// 640x480); the cast in the blit's aspect math truncates by design.
#![allow(clippy::cast_precision_loss)]
// The skeleton's deterministic test-pattern framebuffer (emu.rs) and the joybus
// analog-stick packing (input.rs) intentionally round-trip narrow integers
// (u32->u8 modulo, i8<->u8 reinterpret); these casts are by design, not bugs.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

#[cfg(not(target_arch = "wasm32"))]
pub mod app;
#[cfg(not(target_arch = "wasm32"))]
pub mod audio;
pub mod config;
pub mod emu;
#[cfg(all(not(target_arch = "wasm32"), feature = "emu-thread"))]
pub mod emu_thread;
#[cfg(not(target_arch = "wasm32"))]
pub mod gfx;
pub mod input;
#[cfg(not(target_arch = "wasm32"))]
pub mod ui_shell;

/// Maximum N64 VI framebuffer width (640x480 hi-res mode). The blit allocates
/// for this and uploads the active sub-rectangle each frame.
pub const FB_MAX_W: u32 = 640;
/// Maximum N64 VI framebuffer height (640x480 hi-res mode).
pub const FB_MAX_H: u32 = 480;
/// Default N64 VI framebuffer width (the common 320x240 mode).
pub const FB_DEFAULT_W: u32 = 320;
/// Default N64 VI framebuffer height (the common 320x240 mode).
pub const FB_DEFAULT_H: u32 = 240;

/// Returns the crate version string.
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
