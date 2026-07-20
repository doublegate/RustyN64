//! Frontend configuration (skeleton).
//!
//! The v0.1 config is a small in-memory struct of defaults. Persistence (a TOML
//! file under the platform config dir, per-setting auto-save, input rebinding
//! tables) is a roadmap feature; the shape is kept minimal here so the shell and
//! the pacer have a single place to read tunables from.

/// The display-sync pacing strategy.
///
/// The native pacer is wall-clock authoritative; the present mode only governs
/// how the swapchain hands frames to the compositor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PresentMode {
    /// vsync (the safe default; every wgpu backend supports it).
    #[default]
    Fifo,
    /// Triple-buffered, no tearing, no vsync gate (avoids the double-pacing beat).
    Mailbox,
    /// Uncapped, may tear.
    Immediate,
}

impl PresentMode {
    /// The lowercase config string for this mode.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fifo => "fifo",
            Self::Mailbox => "mailbox",
            Self::Immediate => "immediate",
        }
    }

    /// Parse from a (case-insensitive) config string, defaulting to `Fifo`.
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "mailbox" => Self::Mailbox,
            "immediate" => Self::Immediate,
            _ => Self::Fifo,
        }
    }
}

/// The emulated video region (sets the target frame rate the pacer holds to).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Region {
    /// NTSC (~60 Hz), the common region.
    #[default]
    Ntsc,
    /// PAL (~50 Hz).
    Pal,
}

impl Region {
    /// The target frame rate for this region (Hz).
    #[must_use]
    pub const fn target_fps(self) -> f64 {
        match self {
            Self::Ntsc => 60.0,
            Self::Pal => 50.0,
        }
    }
}

/// The frontend's runtime configuration.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Swapchain present mode.
    pub present_mode: PresentMode,
    /// Emulated region (target frame rate).
    pub region: Region,
    /// Master audio volume (`0.0..=1.0`).
    pub volume: f32,
    /// Determinism seed for the power-on phase alignment.
    pub seed: u64,
    /// Whether the debugger panel starts visible.
    pub debugger_open: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            present_mode: PresentMode::Fifo,
            region: Region::Ntsc,
            volume: 1.0,
            seed: 0,
            debugger_open: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn present_mode_round_trips() {
        for m in [
            PresentMode::Fifo,
            PresentMode::Mailbox,
            PresentMode::Immediate,
        ] {
            assert_eq!(PresentMode::parse(m.as_str()), m);
        }
        assert_eq!(PresentMode::parse("NONSENSE"), PresentMode::Fifo);
    }

    #[test]
    fn region_fps() {
        assert!((Region::Ntsc.target_fps() - 60.0).abs() < f64::EPSILON);
        assert!((Region::Pal.target_fps() - 50.0).abs() < f64::EPSILON);
    }

    #[test]
    fn default_config() {
        let c = Config::default();
        assert_eq!(c.present_mode, PresentMode::Fifo);
        assert!((c.volume - 1.0).abs() < f32::EPSILON);
    }
}
