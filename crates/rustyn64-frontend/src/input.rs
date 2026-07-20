//! The N64 controller input map + the lock-free `SharedInput` handoff.
//!
//! The N64 controller is a single 32-bit joybus state word per port: a
//! 16-bit digital-button field plus a signed 8-bit analog-stick X and Y. The
//! frontend reads the host keyboard (and, on native, a gamepad via gilrs) and
//! packs it into that word, which `rustyn64-core`'s `Bus::controllers[port]`
//! consumes when the SI joybus latches.
//!
//! # The default keymap (player 1)
//!
//! | N64 control          | Keyboard            |
//! |----------------------|---------------------|
//! | Analog stick         | Arrow keys          |
//! | D-pad                | I / J / K / L       |
//! | A button             | X                   |
//! | B button             | Z                   |
//! | C-up / C-down        | T / G               |
//! | C-left / C-right      | F / H               |
//! | L shoulder           | Q                   |
//! | R shoulder           | E                   |
//! | Z trigger            | Space               |
//! | Start                | Enter               |
//!
//! A USB gamepad auto-binds to P1 (Xbox-style): the left stick drives the analog
//! stick, the right stick / face cluster the C-buttons, South=A, West=B,
//! LB/RB=L/R, the left trigger=Z, Start=Start, the D-pad=D-pad.

use core::sync::atomic::{AtomicU32, Ordering};

/// N64 joybus button bit positions within the 16-bit digital field.
///
/// These match the real controller's joybus reply word (high byte first). The
/// analog X/Y bytes live in the low 16 bits of the packed [`N64Buttons::pack`]
/// word; the digital flags live in the high 16 bits.
pub mod bit {
    /// A button.
    pub const A: u16 = 1 << 15;
    /// B button.
    pub const B: u16 = 1 << 14;
    /// Z trigger.
    pub const Z: u16 = 1 << 13;
    /// Start.
    pub const START: u16 = 1 << 12;
    /// D-pad up.
    pub const D_UP: u16 = 1 << 11;
    /// D-pad down.
    pub const D_DOWN: u16 = 1 << 10;
    /// D-pad left.
    pub const D_LEFT: u16 = 1 << 9;
    /// D-pad right.
    pub const D_RIGHT: u16 = 1 << 8;
    /// L shoulder.
    pub const L: u16 = 1 << 5;
    /// R shoulder.
    pub const R: u16 = 1 << 4;
    /// C-up.
    pub const C_UP: u16 = 1 << 3;
    /// C-down.
    pub const C_DOWN: u16 = 1 << 2;
    /// C-left.
    pub const C_LEFT: u16 = 1 << 1;
    /// C-right.
    pub const C_RIGHT: u16 = 1 << 0;
}

/// A decoded N64 controller state: the 16-bit digital field + signed analog X/Y.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct N64Buttons {
    /// The digital-button bitfield (see [`bit`]).
    pub digital: u16,
    /// Analog stick X, signed (`-128..=127`; left negative).
    pub stick_x: i8,
    /// Analog stick Y, signed (`-128..=127`; down negative).
    pub stick_y: i8,
}

impl N64Buttons {
    /// Set or clear a digital button by its [`bit`] mask.
    pub const fn set(&mut self, mask: u16, pressed: bool) {
        if pressed {
            self.digital |= mask;
        } else {
            self.digital &= !mask;
        }
    }

    /// Pack into the 32-bit joybus word the core's `controllers[port]` consumes:
    /// the digital field in the high 16 bits, then the analog X and Y bytes.
    #[must_use]
    pub fn pack(self) -> u32 {
        let x = u32::from(self.stick_x as u8);
        let y = u32::from(self.stick_y as u8);
        (u32::from(self.digital) << 16) | (x << 8) | y
    }

    /// Inverse of [`pack`](Self::pack), for the (future) replay / netplay path.
    #[must_use]
    pub const fn unpack(word: u32) -> Self {
        Self {
            digital: (word >> 16) as u16,
            stick_x: ((word >> 8) & 0xFF) as u8 as i8,
            stick_y: (word & 0xFF) as u8 as i8,
        }
    }
}

/// The lock-free input handoff from the winit (UI) thread to the emu thread.
///
/// Each port is a single `AtomicU32` packed by [`N64Buttons::pack`]. The UI
/// thread stores the latest state every frame; the emu thread loads it when the
/// SI joybus latches. A relaxed atomic per port is sufficient — the value is a
/// snapshot, never a multi-word structure that could tear.
#[derive(Debug, Default)]
pub struct SharedInput {
    ports: [AtomicU32; 4],
}

impl SharedInput {
    /// Construct with all ports neutral.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            ports: [
                AtomicU32::new(0),
                AtomicU32::new(0),
                AtomicU32::new(0),
                AtomicU32::new(0),
            ],
        }
    }

    /// Store a port's packed controller state (UI thread).
    pub fn store(&self, port: usize, buttons: N64Buttons) {
        if let Some(slot) = self.ports.get(port) {
            slot.store(buttons.pack(), Ordering::Relaxed);
        }
    }

    /// Load a port's packed controller word (emu thread).
    #[must_use]
    pub fn load(&self, port: usize) -> u32 {
        self.ports
            .get(port)
            .map_or(0, |s| s.load(Ordering::Relaxed))
    }

    /// Load every port as a packed array, ready to copy into the core's
    /// `Bus::controllers`.
    #[must_use]
    pub fn load_all(&self) -> [u32; 4] {
        [self.load(0), self.load(1), self.load(2), self.load(3)]
    }
}

/// The keyboard binding for player 1's controls, as winit logical keys.
///
/// Kept as a plain map (not config-driven yet) for the v0.1 skeleton; rebinding
/// is a roadmap feature (it lands in `config.rs`'s `[input.*]` tables).
#[cfg(not(target_arch = "wasm32"))]
pub mod keymap {
    use super::bit;
    use winit::keyboard::{Key, NamedKey};

    /// A keyboard-to-digital-button binding entry.
    pub struct Bind {
        /// The winit logical key.
        pub key: Key,
        /// The N64 digital-button mask it sets.
        pub mask: u16,
    }

    /// One of the four analog-stick directions a key can drive.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum StickDir {
        /// Push the stick up (+Y).
        Up,
        /// Push the stick down (-Y).
        Down,
        /// Push the stick left (-X).
        Left,
        /// Push the stick right (+X).
        Right,
    }

    /// An analog-stick key binding.
    pub struct StickBind {
        /// The winit logical key.
        pub key: Key,
        /// The direction it pushes.
        pub dir: StickDir,
    }

    /// Player 1's default digital-button bindings (see the module-level table).
    #[must_use]
    pub fn p1_digital() -> [Bind; 14] {
        [
            Bind {
                key: Key::Character("x".into()),
                mask: bit::A,
            },
            Bind {
                key: Key::Character("z".into()),
                mask: bit::B,
            },
            Bind {
                key: Key::Named(NamedKey::Space),
                mask: bit::Z,
            },
            Bind {
                key: Key::Named(NamedKey::Enter),
                mask: bit::START,
            },
            Bind {
                key: Key::Character("i".into()),
                mask: bit::D_UP,
            },
            Bind {
                key: Key::Character("k".into()),
                mask: bit::D_DOWN,
            },
            Bind {
                key: Key::Character("j".into()),
                mask: bit::D_LEFT,
            },
            Bind {
                key: Key::Character("l".into()),
                mask: bit::D_RIGHT,
            },
            Bind {
                key: Key::Character("t".into()),
                mask: bit::C_UP,
            },
            Bind {
                key: Key::Character("g".into()),
                mask: bit::C_DOWN,
            },
            Bind {
                key: Key::Character("f".into()),
                mask: bit::C_LEFT,
            },
            Bind {
                key: Key::Character("h".into()),
                mask: bit::C_RIGHT,
            },
            Bind {
                key: Key::Character("q".into()),
                mask: bit::L,
            },
            Bind {
                key: Key::Character("e".into()),
                mask: bit::R,
            },
        ]
    }

    /// Player 1's default analog-stick (arrow-key) bindings.
    #[must_use]
    pub const fn p1_stick() -> [StickBind; 4] {
        [
            StickBind {
                key: Key::Named(NamedKey::ArrowUp),
                dir: StickDir::Up,
            },
            StickBind {
                key: Key::Named(NamedKey::ArrowDown),
                dir: StickDir::Down,
            },
            StickBind {
                key: Key::Named(NamedKey::ArrowLeft),
                dir: StickDir::Left,
            },
            StickBind {
                key: Key::Named(NamedKey::ArrowRight),
                dir: StickDir::Right,
            },
        ]
    }

    /// The full-deflection analog magnitude a digital arrow press maps to.
    /// The real stick range is roughly +/-80 at the gate; we use that.
    pub const STICK_DEFLECTION: i8 = 80;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_round_trips() {
        let mut b = N64Buttons {
            stick_x: -40,
            stick_y: 80,
            ..Default::default()
        };
        b.set(bit::A, true);
        b.set(bit::START, true);
        let word = b.pack();
        assert_eq!(N64Buttons::unpack(word), b);
    }

    #[test]
    fn set_and_clear() {
        let mut b = N64Buttons::default();
        b.set(bit::Z, true);
        assert_eq!(b.digital & bit::Z, bit::Z);
        b.set(bit::Z, false);
        assert_eq!(b.digital & bit::Z, 0);
    }

    #[test]
    fn shared_input_round_trips() {
        let si = SharedInput::new();
        let mut b = N64Buttons::default();
        b.set(bit::B, true);
        si.store(1, b);
        assert_eq!(si.load(1), b.pack());
        assert_eq!(si.load_all()[1], b.pack());
    }
}
