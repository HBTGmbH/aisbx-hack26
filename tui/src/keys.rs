//! Translates crossterm's events into the core's [`crate::input`] types, one function per type.

use ratatui::crossterm::event as ct;

use crate::geom;
use crate::input::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

pub fn key(e: ct::KeyEvent) -> KeyEvent {
    KeyEvent::new_with_kind(code(e.code), modifiers(e.modifiers), kind(e.kind))
}

pub fn mouse(e: ct::MouseEvent) -> MouseEvent {
    MouseEvent {
        kind: mouse_kind(e.kind),
        column: e.column,
        row: e.row,
        modifiers: modifiers(e.modifiers),
    }
}

/// A click box as the renderer laid it out.
pub fn rect(r: ratatui::layout::Rect) -> geom::Rect {
    geom::Rect::new(r.x, r.y, r.width, r.height)
}

fn code(c: ct::KeyCode) -> KeyCode {
    match c {
        ct::KeyCode::Backspace => KeyCode::Backspace,
        ct::KeyCode::Enter => KeyCode::Enter,
        ct::KeyCode::Left => KeyCode::Left,
        ct::KeyCode::Right => KeyCode::Right,
        ct::KeyCode::Up => KeyCode::Up,
        ct::KeyCode::Down => KeyCode::Down,
        ct::KeyCode::Home => KeyCode::Home,
        ct::KeyCode::End => KeyCode::End,
        ct::KeyCode::PageUp => KeyCode::PageUp,
        ct::KeyCode::PageDown => KeyCode::PageDown,
        ct::KeyCode::Tab => KeyCode::Tab,
        ct::KeyCode::BackTab => KeyCode::BackTab,
        ct::KeyCode::Delete => KeyCode::Delete,
        ct::KeyCode::Insert => KeyCode::Insert,
        ct::KeyCode::F(n) => KeyCode::F(n),
        ct::KeyCode::Char(c) => KeyCode::Char(c),
        ct::KeyCode::Esc => KeyCode::Esc,
        ct::KeyCode::Null
        | ct::KeyCode::CapsLock
        | ct::KeyCode::ScrollLock
        | ct::KeyCode::NumLock
        | ct::KeyCode::PrintScreen
        | ct::KeyCode::Pause
        | ct::KeyCode::Menu
        | ct::KeyCode::KeypadBegin
        | ct::KeyCode::Media(_)
        | ct::KeyCode::Modifier(_) => KeyCode::Null,
    }
}

fn modifiers(m: ct::KeyModifiers) -> KeyModifiers {
    [
        (ct::KeyModifiers::SHIFT, KeyModifiers::SHIFT),
        (ct::KeyModifiers::CONTROL, KeyModifiers::CONTROL),
        (ct::KeyModifiers::ALT, KeyModifiers::ALT),
        (ct::KeyModifiers::SUPER, KeyModifiers::SUPER),
        (ct::KeyModifiers::HYPER, KeyModifiers::HYPER),
        (ct::KeyModifiers::META, KeyModifiers::META),
    ]
    .into_iter()
    .filter(|(from, _)| m.contains(*from))
    .fold(KeyModifiers::NONE, |acc, (_, to)| acc | to)
}

fn kind(k: ct::KeyEventKind) -> KeyEventKind {
    match k {
        ct::KeyEventKind::Press => KeyEventKind::Press,
        ct::KeyEventKind::Repeat => KeyEventKind::Repeat,
        ct::KeyEventKind::Release => KeyEventKind::Release,
    }
}

fn button(b: ct::MouseButton) -> MouseButton {
    match b {
        ct::MouseButton::Left => MouseButton::Left,
        ct::MouseButton::Right => MouseButton::Right,
        ct::MouseButton::Middle => MouseButton::Middle,
    }
}

fn mouse_kind(k: ct::MouseEventKind) -> MouseEventKind {
    match k {
        ct::MouseEventKind::Down(b) => MouseEventKind::Down(button(b)),
        ct::MouseEventKind::Up(b) => MouseEventKind::Up(button(b)),
        ct::MouseEventKind::Drag(b) => MouseEventKind::Drag(button(b)),
        ct::MouseEventKind::Moved => MouseEventKind::Moved,
        ct::MouseEventKind::ScrollDown => MouseEventKind::ScrollDown,
        ct::MouseEventKind::ScrollUp => MouseEventKind::ScrollUp,
        ct::MouseEventKind::ScrollLeft => MouseEventKind::ScrollLeft,
        ct::MouseEventKind::ScrollRight => MouseEventKind::ScrollRight,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_keep_their_code_modifiers_and_kind() {
        let e = ct::KeyEvent::new_with_kind(
            ct::KeyCode::Char('r'),
            ct::KeyModifiers::CONTROL,
            ct::KeyEventKind::Release,
        );
        assert_eq!(
            key(e),
            KeyEvent::new_with_kind(
                KeyCode::Char('r'),
                KeyModifiers::CONTROL,
                KeyEventKind::Release
            )
        );
        assert_eq!(
            key(ct::KeyEvent::from(ct::KeyCode::F(5))),
            KeyEvent::from(KeyCode::F(5))
        );
        for (from, to) in [
            (ct::KeyCode::Enter, KeyCode::Enter),
            (ct::KeyCode::Esc, KeyCode::Esc),
            (ct::KeyCode::BackTab, KeyCode::BackTab),
            (ct::KeyCode::Delete, KeyCode::Delete),
            (ct::KeyCode::PageDown, KeyCode::PageDown),
            (ct::KeyCode::Home, KeyCode::Home),
            (ct::KeyCode::CapsLock, KeyCode::Null),
        ] {
            assert_eq!(key(ct::KeyEvent::from(from)).code, to, "{from:?}");
        }
    }

    #[test]
    fn every_modifier_crosses_over() {
        let held = ct::KeyModifiers::SHIFT | ct::KeyModifiers::ALT;
        let m = modifiers(held);
        assert!(m.contains(KeyModifiers::SHIFT) && m.contains(KeyModifiers::ALT));
        assert!(!m.contains(KeyModifiers::CONTROL));
        assert_eq!(modifiers(ct::KeyModifiers::NONE), KeyModifiers::NONE);
        assert_eq!(modifiers(ct::KeyModifiers::CONTROL), KeyModifiers::CONTROL);
        assert_eq!(modifiers(ct::KeyModifiers::SUPER), KeyModifiers::SUPER);
    }

    #[test]
    fn mouse_events_keep_their_cell_and_button() {
        let e = ct::MouseEvent {
            kind: ct::MouseEventKind::Down(ct::MouseButton::Left),
            column: 7,
            row: 3,
            modifiers: ct::KeyModifiers::NONE,
        };
        assert_eq!(
            mouse(e),
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 7,
                row: 3,
                modifiers: KeyModifiers::NONE,
            }
        );
        assert_eq!(
            mouse_kind(ct::MouseEventKind::ScrollUp),
            MouseEventKind::ScrollUp
        );
        assert_eq!(
            mouse_kind(ct::MouseEventKind::Drag(ct::MouseButton::Right)),
            MouseEventKind::Drag(MouseButton::Right)
        );
    }

    #[test]
    fn rects_keep_their_cells() {
        let r = rect(ratatui::layout::Rect::new(1, 2, 3, 4));
        assert_eq!(r, geom::Rect::new(1, 2, 3, 4));
    }
}
