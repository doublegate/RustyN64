//! The native app: the winit event loop tying the shell, gfx, emu thread, and
//! input together.
//!
//! The winit thread does UI + present ONLY. The emulator runs on the dedicated
//! [`crate::emu_thread::EmuThread`] behind an `Arc<Mutex<EmuCore>>`; this thread
//! reads the staged framebuffer under a brief lock, runs the egui pass (never
//! holding the lock), dispatches the resulting [`MenuAction`]s, then presents.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::Key;
use winit::window::{Window, WindowId};

use crate::config::Config;
use crate::emu::EmuCore;
use crate::gfx::{Gfx, PresentError};
use crate::input::{N64Buttons, SharedInput, keymap};
use crate::ui_shell::{MenuAction, Shell, ShellState};

#[cfg(feature = "emu-thread")]
use crate::emu_thread::EmuThread;

/// Errors from launching the app.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// The winit event loop failed to build or run.
    #[error("event loop: {0}")]
    EventLoop(String),
    /// Graphics init failed.
    #[error(transparent)]
    Gfx(#[from] crate::gfx::GfxError),
    /// Reading the ROM file failed.
    #[error("read ROM: {0}")]
    Rom(String),
}

/// Run the frontend, optionally loading `rom` at start.
///
/// # Errors
/// [`AppError`] on event-loop / graphics / ROM-load failure.
pub fn run(rom: Option<&Path>) -> Result<(), AppError> {
    let event_loop = EventLoop::new().map_err(|e| AppError::EventLoop(e.to_string()))?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::new(rom.map(Path::to_path_buf));
    event_loop
        .run_app(&mut app)
        .map_err(|e| AppError::EventLoop(e.to_string()))?;
    Ok(())
}

/// The winit application state.
struct App {
    config: Config,
    pending_rom: Option<PathBuf>,
    window: Option<Arc<Window>>,
    gfx: Option<Gfx>,
    egui_ctx: egui::Context,
    egui_state: Option<egui_winit::State>,
    shell: Shell,
    emu: Arc<Mutex<EmuCore>>,
    input: Arc<SharedInput>,
    /// Pressed logical keys this instant (the input collector reads these).
    keys_down: HashSet<Key>,
    #[cfg(feature = "emu-thread")]
    emu_thread: Option<EmuThread>,
    #[cfg(not(feature = "emu-thread"))]
    last_frame: web_time::Instant,
}

impl App {
    fn new(pending_rom: Option<PathBuf>) -> Self {
        let config = Config::default();
        let emu = Arc::new(Mutex::new(EmuCore::new(config.seed)));
        let mut shell = Shell::new();
        shell.debugger_open = config.debugger_open;
        Self {
            config,
            pending_rom,
            window: None,
            gfx: None,
            egui_ctx: egui::Context::default(),
            egui_state: None,
            shell,
            emu,
            input: Arc::new(SharedInput::new()),
            keys_down: HashSet::new(),
            #[cfg(feature = "emu-thread")]
            emu_thread: None,
            #[cfg(not(feature = "emu-thread"))]
            last_frame: web_time::Instant::now(),
        }
    }

    /// Read a ROM file and hand it to the core.
    fn load_rom(&self, path: &Path) -> Result<(), AppError> {
        let raw = std::fs::read(path).map_err(|e| AppError::Rom(e.to_string()))?;
        if let Ok(mut core) = self.emu.lock() {
            core.load_rom(&raw)
                .map_err(|e| AppError::Rom(format!("{e:?}")))?;
        }
        Ok(())
    }

    /// Pack the currently-pressed keys into P1's controller word and publish it
    /// to the lock-free [`SharedInput`] (the emu thread latches it).
    fn publish_input(&self) {
        let mut buttons = N64Buttons::default();
        for bind in keymap::p1_digital() {
            buttons.set(bind.mask, self.keys_down.contains(&bind.key));
        }
        for sb in keymap::p1_stick() {
            if self.keys_down.contains(&sb.key) {
                match sb.dir {
                    keymap::StickDir::Up => buttons.stick_y = keymap::STICK_DEFLECTION,
                    keymap::StickDir::Down => buttons.stick_y = -keymap::STICK_DEFLECTION,
                    keymap::StickDir::Left => buttons.stick_x = -keymap::STICK_DEFLECTION,
                    keymap::StickDir::Right => buttons.stick_x = keymap::STICK_DEFLECTION,
                }
            }
        }
        self.input.store(0, buttons);
    }

    /// Copy the core state the shell displays, under a brief lock.
    fn snapshot(&self) -> (ShellState, Vec<u8>, u32, u32) {
        self.emu.lock().map_or_else(
            |_| (ShellState::default(), Vec::new(), 0, 0),
            |core| {
                let frame = core.frame();
                let state = ShellState {
                    rom_loaded: core.is_loaded(),
                    paused: core.is_paused(),
                    frames: core.frame_count(),
                    master_ticks: core.master_ticks(),
                    fb_w: frame.w,
                    fb_h: frame.h,
                };
                (state, frame.rgba.clone(), frame.w, frame.h)
            },
        )
    }

    /// Dispatch the actions the egui pass requested (runs AFTER the pass, so
    /// taking the emu lock here never collides with the egui closure).
    fn dispatch(&self, actions: Vec<MenuAction>, event_loop: &ActiveEventLoop) {
        for action in actions {
            match action {
                MenuAction::OpenRom => {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("N64 ROM", &["z64", "n64", "v64"])
                        .pick_file()
                        && let Err(e) = self.load_rom(&path)
                    {
                        eprintln!("rustyn64: {e}");
                    }
                }
                MenuAction::CloseRom => {
                    if let Ok(mut core) = self.emu.lock() {
                        *core = EmuCore::new(self.config.seed);
                    }
                }
                MenuAction::TogglePause => {
                    if let Ok(mut core) = self.emu.lock() {
                        let p = core.is_paused();
                        core.set_paused(!p);
                    }
                }
                MenuAction::Reset => {
                    if let Ok(mut core) = self.emu.lock() {
                        core.reset();
                    }
                }
                MenuAction::ToggleDebugger => { /* the checkbox already flipped Shell state */ }
                MenuAction::Quit => event_loop.exit(),
            }
        }
    }

    /// One UI frame: input -> snapshot -> egui pass -> dispatch -> blit + present.
    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        self.publish_input();

        // Without the emu thread, drive the core synchronously here.
        #[cfg(not(feature = "emu-thread"))]
        {
            let now = web_time::Instant::now();
            if now.duration_since(self.last_frame).as_secs_f64()
                >= 1.0 / self.config.region.target_fps()
            {
                self.last_frame = now;
                if let Ok(mut core) = self.emu.lock() {
                    core.set_controllers(self.input.load_all());
                    core.run_frame();
                    let _ = core.drain_audio();
                }
            }
        }

        let (state, rgba, fb_w, fb_h) = self.snapshot();

        // Hold an owned `Arc<Window>` clone so the immutable window borrow does
        // not span the `&mut self` dispatch below (which takes the emu lock).
        let Some(window) = self.window.clone() else {
            return;
        };
        if self.gfx.is_none() || self.egui_state.is_none() {
            return;
        }

        // Run the egui pass via the 0.34 `run_ui` API: the shell receives the
        // root `Ui` and nests its panels with `show_inside`. The shell NEVER
        // touches the emu lock — it reads the pre-copied `state` snapshot only.
        let raw_input = self.egui_state.as_mut().unwrap().take_egui_input(&window);
        let shell = &mut self.shell;
        let mut actions = Vec::new();
        let full_output = self.egui_ctx.run_ui(raw_input, |ui| {
            actions = shell.ui(ui, &state);
        });
        self.egui_state
            .as_mut()
            .unwrap()
            .handle_platform_output(&window, full_output.platform_output);

        // Dispatch AFTER the egui pass (may take the emu lock — safe here, the
        // window borrow is an owned Arc clone, not a borrow of `self`).
        self.dispatch(actions, event_loop);

        // Blit + present (winit thread only).
        let Some(gfx) = self.gfx.as_mut() else { return };
        if !rgba.is_empty() {
            gfx.upload_framebuffer(&rgba, fb_w, fb_h);
        }
        let prims = self
            .egui_ctx
            .tessellate(full_output.shapes, full_output.pixels_per_point);
        let size = window.inner_size();
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [size.width.max(1), size.height.max(1)],
            pixels_per_point: full_output.pixels_per_point,
        };
        match gfx.render(
            fb_w.max(1),
            fb_h.max(1),
            &prims,
            &full_output.textures_delta,
            &screen,
        ) {
            Ok(()) => {}
            Err(PresentError::Reconfigure) => gfx.resize(size.width, size.height),
            Err(PresentError::Other(label)) => eprintln!("rustyn64: present skipped: {label}"),
        }

        window.request_redraw();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("RustyN64")
            .with_inner_size(winit::dpi::LogicalSize::new(640.0, 480.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("rustyn64: create window: {e}");
                event_loop.exit();
                return;
            }
        };
        let gfx = match Gfx::new(Arc::clone(&window)) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("rustyn64: {e}");
                event_loop.exit();
                return;
            }
        };
        let egui_state = egui_winit::State::new(
            self.egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            None,
            None,
            None,
        );

        // Load the ROM requested on the command line, if any.
        if let Some(path) = self.pending_rom.take()
            && let Err(e) = self.load_rom(&path)
        {
            eprintln!("rustyn64: {e}");
        }

        // Spawn the dedicated emulation thread (native, default-on).
        #[cfg(feature = "emu-thread")]
        {
            self.emu_thread = Some(EmuThread::spawn(
                Arc::clone(&self.emu),
                Arc::clone(&self.input),
                None, // audio wiring lands when AudioOutput::open is plumbed in
                self.config.region,
            ));
        }

        self.window = Some(window);
        self.gfx = Some(gfx);
        self.egui_state = Some(egui_state);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        // Feed egui first; it tells us whether it consumed the event.
        if let (Some(window), Some(egui_state)) = (self.window.as_ref(), self.egui_state.as_mut()) {
            let _ = egui_state.on_window_event(window, &event);
        }

        match event {
            WindowEvent::CloseRequested => {
                #[cfg(feature = "emu-thread")]
                if let Some(t) = self.emu_thread.as_ref() {
                    t.stop();
                }
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if let Some(gfx) = self.gfx.as_mut() {
                    gfx.resize(size.width, size.height);
                }
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } => {
                let key = key_event.logical_key;
                if key_event.state.is_pressed() {
                    self.keys_down.insert(key);
                } else {
                    self.keys_down.remove(&key);
                }
            }
            WindowEvent::RedrawRequested => self.redraw(event_loop),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
}
