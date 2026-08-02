//! The always-on egui shell: menu bar + status bar + a stub debugger panel.
//!
//! egui runs **every frame**, whether or not a ROM is loaded — this is the
//! persistent shell, not a bare window. The critical rule: the egui closure
//! NEVER holds the emulator lock. Menu interactions return a [`MenuAction`] that
//! the app (`crate::app`) dispatches *after* the egui pass; any core state the shell
//! needs to display is copied into a [`ShellState`] snapshot under a brief lock
//! before the pass runs.

/// An action a menu / panel interaction requests, dispatched after the egui pass
/// (so the dispatch — which may take the emu lock — never runs inside the egui
/// closure).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuAction {
    /// File -> Open ROM (the app pops a file dialog).
    OpenRom,
    /// File -> Close ROM.
    CloseRom,
    /// Emulation -> Pause / Resume toggle.
    TogglePause,
    /// Emulation -> Reset (warm).
    Reset,
    /// View -> toggle the debugger panel.
    ToggleDebugger,
    /// File -> Quit.
    Quit,
}

/// A read-only snapshot of core state for the shell to display, copied under a
/// brief lock BEFORE the egui pass (never read from inside the egui closure).
#[derive(Clone, Copy, Debug, Default)]
pub struct ShellState {
    /// A ROM is loaded.
    pub rom_loaded: bool,
    /// The core is paused.
    pub paused: bool,
    /// Produced frame count.
    pub frames: u64,
    /// Elapsed master (VR4300) ticks.
    pub master_ticks: u64,
    /// Active framebuffer width.
    pub fb_w: u32,
    /// Active framebuffer height.
    pub fb_h: u32,
}

/// The shell's own (non-core) UI state: which panels are open, and the frame-rate
/// estimator behind the status bar.
#[derive(Clone, Copy, Debug, Default)]
pub struct Shell {
    /// The debugger panel is visible.
    pub debugger_open: bool,
    /// Wall-clock of the last frame-rate sample, in egui's monotonic seconds.
    ///
    /// `ctx.input(|i| i.time)` rather than [`std::time::Instant`] because it is
    /// already in hand at the call site and keeps this struct `Copy` — an
    /// `Instant` would work equally well here, since this whole module is
    /// `cfg(not(target_arch = "wasm32"))` and the wasm shell is a separate one.
    /// Stated because the tempting justification — "`Instant` panics on wasm" —
    /// is true of the crate and **not** of this module, and would have been a
    /// reason that quietly stopped applying.
    fps_sampled_at: f64,
    /// The produced-frame count at that sample.
    fps_sampled_frames: u64,
    /// The rate shown in the status bar, or `None` before the first interval has
    /// elapsed — displayed as `--` rather than as a confident `0.0`.
    fps: Option<f32>,
}

impl Shell {
    /// Construct the shell.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            debugger_open: false,
            fps_sampled_at: 0.0,
            fps_sampled_frames: 0,
            fps: None,
        }
    }

    /// Re-estimate the frame rate, at most once per [`Self::FPS_INTERVAL`].
    ///
    /// Counts **produced** frames against wall time, which is what a user means
    /// by FPS — not the UI's repaint rate, which egui drives independently and
    /// which would read as 60 while the machine crawled.
    ///
    /// The interval exists so the number is readable: sampling every repaint
    /// makes it flicker through a range instead of showing a value. A frame
    /// counter that goes backwards (a reset, a state load) yields `None` rather
    /// than a negative rate.
    fn sample_fps(&mut self, now: f64, frames: u64) {
        if self.fps_sampled_at <= 0.0 {
            self.fps_sampled_at = now;
            self.fps_sampled_frames = frames;
            return;
        }
        let elapsed = now - self.fps_sampled_at;
        if elapsed < Self::FPS_INTERVAL {
            return;
        }
        self.fps = frames
            .checked_sub(self.fps_sampled_frames)
            .filter(|_| elapsed > 0.0)
            .map(|d| (d as f64 / elapsed) as f32);
        self.fps_sampled_at = now;
        self.fps_sampled_frames = frames;
    }

    /// Draw the whole shell for one frame and collect the requested actions.
    ///
    /// `root_ui` is the root [`egui::Ui`] from [`egui::Context::run_ui`], into
    /// which the top/bottom panels are nested with `show_inside` (the egui 0.34
    /// panel API). `state` is the pre-copied core snapshot; the returned actions
    /// are dispatched by the app after this returns (outside the egui closure).
    pub fn ui(&mut self, root_ui: &mut egui::Ui, state: &ShellState) -> Vec<MenuAction> {
        let mut actions = Vec::new();
        let ctx = root_ui.ctx().clone();
        self.menu_bar(root_ui, state, &mut actions);
        self.sample_fps(ctx.input(|i| i.time), state.frames);
        self.status_bar(root_ui, state);
        if self.debugger_open {
            Self::debugger_panel(&ctx, state);
        }
        actions
    }

    fn menu_bar(
        &mut self,
        root_ui: &mut egui::Ui,
        state: &ShellState,
        actions: &mut Vec<MenuAction>,
    ) {
        egui::Panel::top("menu_bar").show(root_ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open ROM...").clicked() {
                        actions.push(MenuAction::OpenRom);
                        ui.close();
                    }
                    if ui
                        .add_enabled(state.rom_loaded, egui::Button::new("Close ROM"))
                        .clicked()
                    {
                        actions.push(MenuAction::CloseRom);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Quit").clicked() {
                        actions.push(MenuAction::Quit);
                        ui.close();
                    }
                });
                ui.menu_button("Emulation", |ui| {
                    let pause_label = if state.paused { "Resume" } else { "Pause" };
                    if ui
                        .add_enabled(state.rom_loaded, egui::Button::new(pause_label))
                        .clicked()
                    {
                        actions.push(MenuAction::TogglePause);
                        ui.close();
                    }
                    if ui
                        .add_enabled(state.rom_loaded, egui::Button::new("Reset"))
                        .clicked()
                    {
                        actions.push(MenuAction::Reset);
                        ui.close();
                    }
                });
                ui.menu_button("View", |ui| {
                    if ui.checkbox(&mut self.debugger_open, "Debugger").changed() {
                        // The checkbox toggled our own state directly; no action
                        // needed, but emit one so the app can mirror it if it
                        // tracks panel visibility elsewhere.
                        actions.push(MenuAction::ToggleDebugger);
                        ui.close();
                    }
                });
                ui.menu_button("Help", |ui| {
                    if ui.button("About RustyN64").clicked() {
                        ui.close();
                    }
                });
            });
        });
    }

    /// How long a frame-rate sample covers. Long enough that the reading is
    /// steady, short enough that it tracks a change the user just made.
    const FPS_INTERVAL: f64 = 0.5;

    fn status_bar(&self, root_ui: &mut egui::Ui, state: &ShellState) {
        egui::Panel::bottom("status_bar").show(root_ui, |ui| {
            ui.horizontal(|ui| {
                let status = if state.rom_loaded {
                    if state.paused {
                        "Paused".to_string()
                    } else {
                        format!("Running  frame {}", state.frames)
                    }
                } else {
                    "No ROM".to_string()
                };
                ui.label(status);
                ui.separator();
                ui.label(format!("{}x{}", state.fb_w, state.fb_h));
                ui.separator();
                // The frame rate, not `master_ticks`. The tick count is a
                // monotonically climbing 64-bit number that tells a user nothing
                // about whether the emulator is keeping up; it stays in the
                // debugger panel, where a raw timebase belongs.
                ui.label(match self.fps {
                    Some(fps) if state.rom_loaded && !state.paused => format!("{fps:.1} FPS"),
                    _ => "-- FPS".to_string(),
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(format!("RustyN64 v{}", crate::version()));
                });
            });
        });
    }

    /// The stub debugger window. The core's chips are implemented (VR4300, LLE
    /// RSP + RDP), but the per-chip register *views* (COP0/COP1, the RSP
    /// scalar/vector regs, the RDP command stream) are still roadmap; this shows a
    /// placeholder + the elapsed timebase. Rendered as a floating [`egui::Window`]
    /// (egui 0.34 has no nested `SidePanel` over a `run_ui` root).
    fn debugger_panel(ctx: &egui::Context, state: &ShellState) {
        egui::Window::new("Debugger (stub)")
            .default_width(260.0)
            .show(ctx, |ui| {
                ui.label("VR4300 register view: roadmap.");
                ui.label("RSP / RDP register panels: roadmap (the chips are implemented).");
                ui.separator();
                egui::Grid::new("dbg_grid").striped(true).show(ui, |ui| {
                    ui.label("master ticks");
                    ui.label(state.master_ticks.to_string());
                    ui.end_row();
                    ui.label("frames");
                    ui.label(state.frames.to_string());
                    ui.end_row();
                    ui.label("paused");
                    ui.label(state.paused.to_string());
                    ui.end_row();
                });
            });
    }
}

#[cfg(test)]
mod tests {

    /// The estimator counts produced frames against wall time, and says nothing
    /// until it has an interval to divide by.
    #[test]
    fn fps_is_frames_over_wall_time_and_unknown_before_the_first_interval() {
        let mut shell = Shell::new();
        shell.sample_fps(10.0, 1_000);
        assert_eq!(shell.fps, None, "no rate can exist from a single sample");

        // Half the interval: too soon, still nothing.
        shell.sample_fps(10.2, 1_006);
        assert_eq!(
            shell.fps, None,
            "reported a rate before an interval elapsed"
        );

        // 30 frames in exactly one second.
        shell.sample_fps(11.0, 1_030);
        assert_eq!(shell.fps, Some(30.0));

        // Half a second, 15 frames -> still 30 FPS, so the interval is divided
        // out rather than assumed.
        shell.sample_fps(11.5, 1_045);
        assert_eq!(shell.fps, Some(30.0));
    }

    /// A frame counter that goes backwards — a reset, a state load — must not
    /// produce a negative rate.
    #[test]
    fn a_frame_counter_going_backwards_yields_no_rate() {
        let mut shell = Shell::new();
        shell.sample_fps(1.0, 5_000);
        shell.sample_fps(2.0, 5_060);
        assert_eq!(shell.fps, Some(60.0));
        shell.sample_fps(3.0, 12);
        assert_eq!(
            shell.fps, None,
            "a reset produced a rate instead of clearing it"
        );
    }
    use super::*;

    #[test]
    fn menu_actions_are_distinct() {
        assert_ne!(MenuAction::OpenRom, MenuAction::CloseRom);
        assert_ne!(MenuAction::TogglePause, MenuAction::Reset);
    }

    #[test]
    fn shell_starts_with_debugger_closed() {
        assert!(!Shell::new().debugger_open);
    }
}
