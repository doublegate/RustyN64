//! `rustyn64` — the `RustyN64` frontend binary (native).
//!
//! A thin shim over `lib.rs`, which owns the module tree: an always-on
//! `winit` + `wgpu` + `cpal` + `egui` shell wrapped around the `rustyn64-core`
//! `System`. The shell runs every frame (menu bar + status bar + a stub
//! debugger panel); the emulator runs on a dedicated thread behind an
//! `Arc<Mutex<EmuCore>>` handle; a `wgpu` fullscreen-triangle pass blits the
//! N64 video framebuffer with 4:3 aspect correction.
//!
//! v0.1 is a SKELETON: the shell + input map + framebuffer plumbing are real,
//! but the emulation is stubbed (the LLE RSP/RDP are roadmap phases). See
//! `README.md` and `docs/STATUS.md` for the honest current state.
//!
//! Usage: `rustyn64 [ROM.z64]` — pass a ROM to load at start, or launch bare and
//! use File -> Open ROM. The default keymap is documented in `input.rs`.

// On the wasm32 target this `[[bin]]` is not the entry point (the browser
// frontend is the `cdylib`); compile an empty `main` there.
#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;
#[cfg(not(target_arch = "wasm32"))]
use std::process::ExitCode;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> ExitCode {
    // Minimal argv handling for v0.1: an optional ROM path, plus `--help` /
    // `--version`. The clap-4 styled CLI + the ratatui help TUI are a roadmap
    // item (they land with the rest of the native CLI UX).
    let mut rom: Option<PathBuf> = None;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-h" | "--help" => {
                println!(
                    "RustyN64 v{} — a cycle-accurate N64 emulator (v0.1 skeleton).\n\n\
                     Usage: rustyn64 [ROM.z64|.n64|.v64]\n\n\
                     Launch bare and use File -> Open ROM, or pass a ROM path.\n\
                     P1 keys: arrows = analog stick, IJKL = D-pad, X = A, Z = B,\n\
                     Space = Z, Enter = Start, TFGH = C-buttons, Q/E = L/R.",
                    rustyn64_frontend::version()
                );
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("rustyn64 {}", rustyn64_frontend::version());
                return ExitCode::SUCCESS;
            }
            other if other.starts_with('-') => {
                eprintln!("rustyn64: unknown option: {other}");
                return ExitCode::from(2);
            }
            path => rom = Some(PathBuf::from(path)),
        }
    }

    if let Some(p) = rom.as_ref()
        && !p.exists()
    {
        eprintln!("rustyn64: ROM file not found: {}", p.display());
        return ExitCode::from(1);
    }

    match rustyn64_frontend::app::run(rom.as_deref()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rustyn64: {e}");
            ExitCode::from(1)
        }
    }
}
