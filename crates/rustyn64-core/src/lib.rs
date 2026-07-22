//! `rustyn64-core` — the Bus + the fractional master-clock lockstep scheduler.
//!
//! The single crate that knows about every chip; it owns the [`Bus`] (all
//! mutable state) and the [`System`] run loop, implements each chip's narrow
//! bus trait, and re-exports the chip crates' public types so downstream
//! consumers (`rustyn64-frontend`, `rustyn64-test-harness`) depend on
//! `rustyn64-core`, not the chip crates directly. See `docs/architecture.md`.
//!
//! `#![no_std]` + `alloc`; the determinism contract (no system time / OS RNG /
//! thread scheduling) is enforced here — the scheduler is one timeline.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

pub mod bus;
pub mod scheduler;
pub mod vi;

// Re-export the chip crates (the public surface). Downstream consumers reach
// the chip types through these aliases (e.g. `rustyn64_core::rustyn64_cpu::Cpu`).
pub use rustyn64_audio as rustyn64_audio_crate;
pub use rustyn64_cart as rustyn64_cart_crate;
pub use rustyn64_cpu as rustyn64_cpu_crate;
pub use rustyn64_rdp as rustyn64_rdp_crate;
pub use rustyn64_rsp as rustyn64_rsp_crate;

// Friendly short aliases for the chip crates.
pub use rustyn64_audio as audio;
pub use rustyn64_cart as cart;
pub use rustyn64_cpu as cpu;
pub use rustyn64_rdp as rdp;
pub use rustyn64_rsp as rsp;

pub use bus::{Bus, MiInterrupt, RDRAM_SIZE, RcpRegs};
pub use scheduler::{
    COUNT_DIVIDER, CPU_DIVIDER, CPU_HZ, MASTER_HZ, PHASE_PERIOD, PIF_DIVIDER, RCP_DIVIDER, RCP_HZ,
    SI_DIVIDER, System,
};
pub use vi::Vi;

/// Returns the crate version string.
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
