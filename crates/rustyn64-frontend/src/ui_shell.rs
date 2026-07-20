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

/// The shell's own (non-core) UI state: which panels are open.
#[derive(Clone, Copy, Debug, Default)]
pub struct Shell {
    /// The debugger panel is visible.
    pub debugger_open: bool,
}

impl Shell {
    /// Construct the shell.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            debugger_open: false,
        }
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
        Self::status_bar(root_ui, state);
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
        egui::Panel::top("menu_bar").show_inside(root_ui, |ui| {
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

    fn status_bar(root_ui: &mut egui::Ui, state: &ShellState) {
        egui::Panel::bottom("status_bar").show_inside(root_ui, |ui| {
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
                ui.label(format!("master {} ticks", state.master_ticks));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(format!("RustyN64 v{}", crate::version()));
                });
            });
        });
    }

    /// The stub VR4300 debugger window. The real per-chip register views (VR4300
    /// COP0/COP1, the RSP scalar/vector regs, the RDP command stream) are
    /// roadmap; v0.1 shows a placeholder + the elapsed timebase. Rendered as a
    /// floating [`egui::Window`] (egui 0.34 has no nested `SidePanel` over a
    /// `run_ui` root).
    fn debugger_panel(ctx: &egui::Context, state: &ShellState) {
        egui::Window::new("Debugger (stub)")
            .default_width(260.0)
            .show(ctx, |ui| {
                ui.label("VR4300 register view: roadmap.");
                ui.label("RSP / RDP panels: roadmap (LLE not yet implemented).");
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
