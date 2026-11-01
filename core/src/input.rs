//! Key and mouse events, as the application state machine reads them. The names are crossterm's;
//! a frontend converts its own events into these (the TUI in `tui/src/keys.rs`).

use std::ops::{BitOr, BitOrAssign};

/// A key, without its modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    Backspace,
    Enter,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Tab,
    BackTab,
    Delete,
    Insert,
    /// A function key, `F(1)` to `F(24)`.
    F(u8),
    Char(char),
    Esc,
    /// Any key that has no variant here (caps lock, media keys): the catch-all arms take it.
    Null,
}

/// The modifier keys held during an event; combine them with `|`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct KeyModifiers(u8);

impl KeyModifiers {
    pub const NONE: Self = Self(0);
    pub const SHIFT: Self = Self(1);
    pub const CONTROL: Self = Self(1 << 1);
    pub const ALT: Self = Self(1 << 2);
    pub const SUPER: Self = Self(1 << 3);
    pub const HYPER: Self = Self(1 << 4);
    pub const META: Self = Self(1 << 5);

    /// Whether every modifier in `other` is held.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl BitOr for KeyModifiers {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for KeyModifiers {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// Whether a key went down, repeats, or came up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyEventKind {
    Press,
    Repeat,
    Release,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
    pub kind: KeyEventKind,
}

impl KeyEvent {
    /// A key press.
    pub const fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self {
            code,
            modifiers,
            kind: KeyEventKind::Press,
        }
    }

    pub const fn new_with_kind(code: KeyCode, modifiers: KeyModifiers, kind: KeyEventKind) -> Self {
        Self {
            code,
            modifiers,
            kind,
        }
    }
}

impl From<KeyCode> for KeyEvent {
    fn from(code: KeyCode) -> Self {
        Self::new(code, KeyModifiers::NONE)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseEventKind {
    Down(MouseButton),
    Up(MouseButton),
    Drag(MouseButton),
    Moved,
    ScrollDown,
    ScrollUp,
    ScrollLeft,
    ScrollRight,
}

/// A mouse event at a terminal cell (`column`, `row`), counted from the top left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MouseEvent {
    pub kind: MouseEventKind,
    pub column: u16,
    pub row: u16,
    pub modifiers: KeyModifiers,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_combine_and_contain() {
        let both = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        assert!(both.contains(KeyModifiers::CONTROL));
        assert!(both.contains(KeyModifiers::SHIFT));
        assert!(!both.contains(KeyModifiers::ALT));
        assert!(both.contains(KeyModifiers::NONE));
        assert!(KeyModifiers::NONE.is_empty());
        assert!(!KeyModifiers::NONE.contains(KeyModifiers::CONTROL));
    }

    #[test]
    fn a_bare_code_is_a_press_without_modifiers() {
        let ev = KeyEvent::from(KeyCode::Char('q'));
        assert_eq!(ev, KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert_eq!(ev.kind, KeyEventKind::Press);
    }
}
