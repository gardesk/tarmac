use bitflags::bitflags;

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct Modifiers: u32 {
        const SHIFT   = 1 << 0;
        const CONTROL = 1 << 1;
        const OPTION  = 1 << 2;
        const COMMAND = 1 << 3;
        const FN      = 1 << 4;
    }
}

/// A raw keyboard event from CGEventTap.
#[derive(Debug, Clone, Copy)]
pub struct KeyEvent {
    pub keycode: u16,
    pub modifiers: Modifiers,
}

/// Logical key identifiers mapped from macOS virtual keycodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    Num0,
    Num1,
    Num2,
    Num3,
    Num4,
    Num5,
    Num6,
    Num7,
    Num8,
    Num9,
    Return,
    Space,
    Tab,
    Escape,
    Delete,
    Grave,
    Minus,
    Equal,
    LeftBracket,
    RightBracket,
    Semicolon,
    Quote,
    Comma,
    Period,
    Slash,
    Backslash,
    Left,
    Right,
    Up,
    Down,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
}

/// Map macOS virtual keycode to logical Key.
/// Reference: /System/Library/Frameworks/Carbon.framework/Versions/A/Frameworks/HIToolbox.framework/Headers/Events.h
pub fn keycode_to_key(keycode: u16) -> Option<Key> {
    match keycode {
        0x00 => Some(Key::A),
        0x01 => Some(Key::S),
        0x02 => Some(Key::D),
        0x03 => Some(Key::F),
        0x04 => Some(Key::H),
        0x05 => Some(Key::G),
        0x06 => Some(Key::Z),
        0x07 => Some(Key::X),
        0x08 => Some(Key::C),
        0x09 => Some(Key::V),
        0x0B => Some(Key::B),
        0x0C => Some(Key::Q),
        0x0D => Some(Key::W),
        0x0E => Some(Key::E),
        0x0F => Some(Key::R),
        0x10 => Some(Key::Y),
        0x11 => Some(Key::T),
        0x12 => Some(Key::Num1),
        0x13 => Some(Key::Num2),
        0x14 => Some(Key::Num3),
        0x15 => Some(Key::Num4),
        0x16 => Some(Key::Num6),
        0x17 => Some(Key::Num5),
        0x18 => Some(Key::Equal),
        0x19 => Some(Key::Num9),
        0x1A => Some(Key::Num7),
        0x1B => Some(Key::Minus),
        0x1C => Some(Key::Num8),
        0x1D => Some(Key::Num0),
        0x1E => Some(Key::RightBracket),
        0x1F => Some(Key::O),
        0x20 => Some(Key::U),
        0x21 => Some(Key::LeftBracket),
        0x22 => Some(Key::I),
        0x23 => Some(Key::P),
        0x24 => Some(Key::Return),
        0x25 => Some(Key::L),
        0x26 => Some(Key::J),
        0x27 => Some(Key::Quote),
        0x28 => Some(Key::K),
        0x29 => Some(Key::Semicolon),
        0x2A => Some(Key::Backslash),
        0x2B => Some(Key::Comma),
        0x2C => Some(Key::Slash),
        0x2D => Some(Key::N),
        0x2E => Some(Key::M),
        0x2F => Some(Key::Period),
        0x30 => Some(Key::Tab),
        0x31 => Some(Key::Space),
        0x32 => Some(Key::Grave),
        0x33 => Some(Key::Delete),
        0x35 => Some(Key::Escape),
        0x7A => Some(Key::F1),
        0x78 => Some(Key::F2),
        0x63 => Some(Key::F3),
        0x76 => Some(Key::F4),
        0x60 => Some(Key::F5),
        0x61 => Some(Key::F6),
        0x62 => Some(Key::F7),
        0x64 => Some(Key::F8),
        0x65 => Some(Key::F9),
        0x6D => Some(Key::F10),
        0x67 => Some(Key::F11),
        0x6F => Some(Key::F12),
        0x7B => Some(Key::Left),
        0x7C => Some(Key::Right),
        0x7D => Some(Key::Down),
        0x7E => Some(Key::Up),
        _ => None,
    }
}

/// An action bound to a key combination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    SpawnTerminal,
    CloseWindow,
    // Sprint 4 adds navigation, swap, resize
}

/// A keybinding: modifier+key → action.
pub struct Keybind {
    pub modifiers: Modifiers,
    pub key: Key,
    pub action: Action,
}

/// Manages registered keybindings and dispatches matching events.
pub struct KeybindManager {
    binds: Vec<Keybind>,
}

impl KeybindManager {
    pub fn new() -> Self {
        Self { binds: Vec::new() }
    }

    pub fn add(&mut self, modifiers: Modifiers, key: Key, action: Action) {
        self.binds.push(Keybind {
            modifiers,
            key,
            action,
        });
    }

    /// Check if a key event matches any binding. Returns the action if matched.
    pub fn dispatch(&self, event: &KeyEvent) -> Option<Action> {
        let key = keycode_to_key(event.keycode)?;
        self.binds
            .iter()
            .find(|b| b.key == key && b.modifiers == event.modifiers)
            .map(|b| b.action)
    }

    /// Create default keybindings (Option as mod key).
    pub fn with_defaults() -> Self {
        let mut mgr = Self::new();
        // Option+Return → spawn terminal
        mgr.add(Modifiers::OPTION, Key::Return, Action::SpawnTerminal);
        // Option+Shift+Q → close window
        mgr.add(
            Modifiers::OPTION | Modifiers::SHIFT,
            Key::Q,
            Action::CloseWindow,
        );
        mgr
    }
}

impl Default for KeybindManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keycode_mapping() {
        assert_eq!(keycode_to_key(0x24), Some(Key::Return));
        assert_eq!(keycode_to_key(0x00), Some(Key::A));
        assert_eq!(keycode_to_key(0x7B), Some(Key::Left));
        assert_eq!(keycode_to_key(0x7E), Some(Key::Up));
        assert_eq!(keycode_to_key(0xFF), None);
    }

    #[test]
    fn dispatch_matches() {
        let mgr = KeybindManager::with_defaults();
        let event = KeyEvent {
            keycode: 0x24, // Return
            modifiers: Modifiers::OPTION,
        };
        assert_eq!(mgr.dispatch(&event), Some(Action::SpawnTerminal));
    }

    #[test]
    fn dispatch_no_match() {
        let mgr = KeybindManager::with_defaults();
        let event = KeyEvent {
            keycode: 0x00, // A
            modifiers: Modifiers::empty(),
        };
        assert_eq!(mgr.dispatch(&event), None);
    }

    #[test]
    fn dispatch_wrong_modifier() {
        let mgr = KeybindManager::with_defaults();
        let event = KeyEvent {
            keycode: 0x24,                 // Return
            modifiers: Modifiers::COMMAND, // Wrong mod
        };
        assert_eq!(mgr.dispatch(&event), None);
    }

    #[test]
    fn dispatch_close_window() {
        let mgr = KeybindManager::with_defaults();
        let event = KeyEvent {
            keycode: 0x0C, // Q
            modifiers: Modifiers::OPTION | Modifiers::SHIFT,
        };
        assert_eq!(mgr.dispatch(&event), Some(Action::CloseWindow));
    }
}
