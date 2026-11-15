//! Turn key and mouse events into the bytes an xterm would send.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

/// Encode a key. `app_cursor` is DECCKM (arrows send `ESC O A` instead of `ESC [ A`).
pub fn encode(key: &KeyEvent, app_cursor: bool) -> Option<Vec<u8>> {
    let m = key.modifiers;
    let alt = m.contains(KeyModifiers::ALT);
    let ctrl = m.contains(KeyModifiers::CONTROL);
    let shift = m.contains(KeyModifiers::SHIFT);
    // xterm modifier parameter: 1 + shift(1) + alt(2) + ctrl(4).
    let modp = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);

    let csi_letter = |c: char| -> Vec<u8> {
        if modp > 1 {
            format!("\x1b[1;{modp}{c}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{c}").into_bytes()
        } else {
            format!("\x1b[{c}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if modp > 1 { format!("\x1b[{n};{modp}~").into_bytes() } else { format!("\x1b[{n}~").into_bytes() }
    };

    let mut out = match key.code {
        KeyCode::Char(c) => {
            if ctrl {
                let b = match c.to_ascii_lowercase() {
                    c @ 'a'..='z' => (c as u8) - b'a' + 1,
                    ' ' | '@' | '2' => 0,
                    '[' | '3' => 0x1b,
                    '\\' | '4' => 0x1c,
                    ']' | '5' => 0x1d,
                    '^' | '6' => 0x1e,
                    '_' | '-' | '7' => 0x1f,
                    '8' | '?' => 0x7f,
                    _ => return None,
                };
                vec![b]
            } else {
                let mut buf = [0u8; 4];
                c.encode_utf8(&mut buf).as_bytes().to_vec()
            }
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => {
            if ctrl {
                vec![0x08]
            } else {
                vec![0x7f]
            }
        }
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => csi_letter('A'),
        KeyCode::Down => csi_letter('B'),
        KeyCode::Right => csi_letter('C'),
        KeyCode::Left => csi_letter('D'),
        KeyCode::Home => csi_letter('H'),
        KeyCode::End => csi_letter('F'),
        KeyCode::Insert => tilde(2),
        KeyCode::Delete => tilde(3),
        KeyCode::PageUp => tilde(5),
        KeyCode::PageDown => tilde(6),
        KeyCode::F(n @ 1..=4) => {
            let c = b"PQRS"[usize::from(n - 1)] as char;
            if modp > 1 { format!("\x1b[1;{modp}{c}").into_bytes() } else { format!("\x1bO{c}").into_bytes() }
        }
        KeyCode::F(n @ 5..=12) => tilde([15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)]),
        _ => return None,
    };
    // Alt sends ESC first for plain characters and control bytes.
    if alt && matches!(key.code, KeyCode::Char(_) | KeyCode::Enter | KeyCode::Backspace | KeyCode::Tab) {
        out.insert(0, 0x1b);
    }
    Some(out)
}

/// Wrap pasted text, using bracketed paste when the program asked for it.
pub fn paste(text: &str, bracketed: bool) -> Vec<u8> {
    let body = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        // A pasted end marker would let the paste escape the bracket.
        let body = body.replace("\x1b[201~", "");
        format!("\x1b[200~{body}\x1b[201~").into_bytes()
    } else {
        body.into_bytes()
    }
}

/// Encode a mouse event in SGR (1006) form, if the program asked for mouse reports.
/// `col` and `row` are 0-based cells inside the terminal.
pub fn mouse(ev: &MouseEvent, col: u16, row: u16, mode: vt100::MouseProtocolMode, sgr: bool) -> Option<Vec<u8>> {
    use vt100::MouseProtocolMode as M;
    if mode == M::None {
        return None;
    }
    let (mut code, release) = match ev.kind {
        MouseEventKind::Down(b) => (button(b), false),
        MouseEventKind::Up(b) => {
            if mode == M::Press {
                return None;
            }
            (button(b), true)
        }
        MouseEventKind::Drag(b) => {
            if !matches!(mode, M::ButtonMotion | M::AnyMotion) {
                return None;
            }
            (button(b) + 32, false)
        }
        MouseEventKind::Moved => {
            if mode != M::AnyMotion {
                return None;
            }
            (35, false)
        }
        MouseEventKind::ScrollUp => (64, false),
        MouseEventKind::ScrollDown => (65, false),
        _ => return None,
    };
    if ev.modifiers.contains(KeyModifiers::SHIFT) {
        code += 4;
    }
    if ev.modifiers.contains(KeyModifiers::ALT) {
        code += 8;
    }
    if ev.modifiers.contains(KeyModifiers::CONTROL) {
        code += 16;
    }
    if sgr {
        Some(format!("\x1b[<{code};{};{}{}", col + 1, row + 1, if release { 'm' } else { 'M' }).into_bytes())
    } else {
        let code = if release { 3 } else { code };
        let enc = |v: u16| u8::try_from(v + 33).unwrap_or(255);
        Some(vec![0x1b, b'[', b'M', (code + 32) as u8, enc(col), enc(row)])
    }
}

fn button(b: MouseButton) -> u16 {
    match b {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode, m: KeyModifiers) -> Vec<u8> {
        encode(&KeyEvent::new(code, m), false).unwrap()
    }

    #[test]
    fn plain_and_control() {
        assert_eq!(k(KeyCode::Char('a'), KeyModifiers::NONE), b"a");
        assert_eq!(k(KeyCode::Char('c'), KeyModifiers::CONTROL), [3]);
        assert_eq!(k(KeyCode::Char('x'), KeyModifiers::ALT), b"\x1bx");
        assert_eq!(k(KeyCode::Char('é'), KeyModifiers::NONE), "é".as_bytes());
        assert_eq!(k(KeyCode::Enter, KeyModifiers::NONE), b"\r");
        assert_eq!(k(KeyCode::Backspace, KeyModifiers::NONE), [0x7f]);
    }

    #[test]
    fn cursor_keys() {
        assert_eq!(k(KeyCode::Up, KeyModifiers::NONE), b"\x1b[A");
        assert_eq!(encode(&KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), true).unwrap(), b"\x1bOA");
        assert_eq!(k(KeyCode::Right, KeyModifiers::CONTROL), b"\x1b[1;5C");
        assert_eq!(k(KeyCode::Delete, KeyModifiers::NONE), b"\x1b[3~");
        assert_eq!(k(KeyCode::F(1), KeyModifiers::NONE), b"\x1bOP");
        assert_eq!(k(KeyCode::F(5), KeyModifiers::NONE), b"\x1b[15~");
        assert_eq!(k(KeyCode::F(12), KeyModifiers::SHIFT), b"\x1b[24;2~");
    }

    #[test]
    fn pasting() {
        assert_eq!(paste("a\nb", false), b"a\rb");
        assert_eq!(paste("x\x1b[201~y", true), b"\x1b[200~xy\x1b[201~");
    }

    #[test]
    fn mouse_reports() {
        let ev = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(mouse(&ev, 4, 2, vt100::MouseProtocolMode::Press, true).unwrap(), b"\x1b[<0;5;3M");
        assert!(mouse(&ev, 4, 2, vt100::MouseProtocolMode::None, true).is_none());
        assert_eq!(mouse(&ev, 0, 0, vt100::MouseProtocolMode::Press, false).unwrap(), [0x1b, b'[', b'M', 32, 33, 33]);
    }
}
