//! Turn the bytes a terminal sends into key, mouse and paste events, as crossterm does for a
//! local terminal. Serve mode needs this because its terminals are at the far end of SSH.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

/// A parser that keeps unfinished sequences between reads.
#[derive(Debug, Default)]
pub struct Parser {
    buf: Vec<u8>,
    paste: Option<Vec<u8>>,
}

const PASTE_END: &[u8] = b"\x1b[201~";
/// Pastes longer than this are cut, so a client cannot grow memory without end.
const MAX_PASTE: usize = 1 << 20;

fn key(code: KeyCode, mods: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, mods))
}

fn mods_from(param: u16) -> KeyModifiers {
    let m = param.saturating_sub(1);
    let mut k = KeyModifiers::NONE;
    if m & 1 != 0 {
        k |= KeyModifiers::SHIFT;
    }
    if m & 2 != 0 {
        k |= KeyModifiers::ALT;
    }
    if m & 4 != 0 {
        k |= KeyModifiers::CONTROL;
    }
    k
}

/// One plain byte (not ESC) as a key.
fn byte_key(b: u8) -> Option<Event> {
    Some(match b {
        b'\r' | b'\n' => key(KeyCode::Enter, KeyModifiers::NONE),
        b'\t' => key(KeyCode::Tab, KeyModifiers::NONE),
        0x7f => key(KeyCode::Backspace, KeyModifiers::NONE),
        0x00 => key(KeyCode::Char(' '), KeyModifiers::CONTROL),
        0x08 => key(KeyCode::Char('h'), KeyModifiers::CONTROL),
        0x01..=0x1a => key(KeyCode::Char((b - 1 + b'a') as char), KeyModifiers::CONTROL),
        0x1c => key(KeyCode::Char('\\'), KeyModifiers::CONTROL),
        0x1d => key(KeyCode::Char(']'), KeyModifiers::CONTROL),
        0x1e => key(KeyCode::Char('^'), KeyModifiers::CONTROL),
        0x1f => key(KeyCode::Char('_'), KeyModifiers::CONTROL),
        _ => return None,
    })
}

enum Step {
    /// Consumed this many bytes and produced an event (or nothing, for ignored sequences).
    Done(usize, Option<Event>),
    /// Need more bytes.
    More,
}

impl Parser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes; returns the events they complete.
    pub fn feed(&mut self, data: &[u8]) -> Vec<Event> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        loop {
            if let Some(p) = &mut self.paste {
                // Inside a bracketed paste: everything up to the end marker is text.
                if let Some(end) = self.buf.windows(PASTE_END.len()).position(|w| w == PASTE_END) {
                    p.extend_from_slice(&self.buf[..end]);
                    self.buf.drain(..end + PASTE_END.len());
                    let text = String::from_utf8_lossy(p).replace("\r\n", "\n").replace('\r', "\n");
                    self.paste = None;
                    out.push(Event::Paste(text));
                    continue;
                }
                // Keep a few bytes back in case the end marker is split across reads.
                let keep = PASTE_END.len().min(self.buf.len());
                let take = self.buf.len() - keep;
                if p.len() < MAX_PASTE {
                    let room = MAX_PASTE - p.len();
                    p.extend_from_slice(&self.buf[..take.min(room)]);
                }
                self.buf.drain(..take);
                break;
            }
            if self.buf.is_empty() {
                break;
            }
            match self.step() {
                Step::Done(n, ev) => {
                    self.buf.drain(..n);
                    out.extend(ev);
                }
                Step::More => break,
            }
        }
        // Nothing should wait forever: a stray unfinished sequence over 64 bytes is dropped.
        if self.paste.is_none() && self.buf.len() > 64 {
            self.buf.clear();
        }
        out
    }

    fn step(&mut self) -> Step {
        let b = &self.buf;
        if b[0] != 0x1b {
            return self.plain(0, KeyModifiers::NONE);
        }
        if b.len() == 1 {
            // A lone ESC at the end of a read is the Esc key.
            return Step::Done(1, Some(key(KeyCode::Esc, KeyModifiers::NONE)));
        }
        match b[1] {
            b'[' => self.csi(),
            b'O' => {
                if b.len() < 3 {
                    return Step::More;
                }
                let code = match b[2] {
                    b'P' => KeyCode::F(1),
                    b'Q' => KeyCode::F(2),
                    b'R' => KeyCode::F(3),
                    b'S' => KeyCode::F(4),
                    b'A' => KeyCode::Up,
                    b'B' => KeyCode::Down,
                    b'C' => KeyCode::Right,
                    b'D' => KeyCode::Left,
                    b'H' => KeyCode::Home,
                    b'F' => KeyCode::End,
                    b'M' => KeyCode::Enter,
                    _ => return Step::Done(3, None),
                };
                Step::Done(3, Some(key(code, KeyModifiers::NONE)))
            }
            0x1b => Step::Done(1, Some(key(KeyCode::Esc, KeyModifiers::NONE))),
            // ESC then a key: that key with Alt.
            _ => self.plain(1, KeyModifiers::ALT),
        }
    }

    /// A plain byte or UTF-8 character at `at`, with `mods`.
    fn plain(&self, at: usize, mods: KeyModifiers) -> Step {
        let b = &self.buf[at..];
        if let Some(Event::Key(mut k)) = byte_key(b[0]) {
            k.modifiers |= mods;
            return Step::Done(at + 1, Some(Event::Key(k)));
        }
        let len = match b[0] {
            0x00..=0x7f => 1,
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf7 => 4,
            _ => return Step::Done(at + 1, None),
        };
        if b.len() < len {
            return Step::More;
        }
        match std::str::from_utf8(&b[..len]).ok().and_then(|s| s.chars().next()) {
            Some(c) => {
                let mut m = mods;
                if c.is_ascii_uppercase() {
                    m |= KeyModifiers::SHIFT;
                }
                Step::Done(at + len, Some(key(KeyCode::Char(c), m)))
            }
            None => Step::Done(at + 1, None),
        }
    }

    fn csi(&mut self) -> Step {
        let b = &self.buf;
        // Find the final byte.
        let Some(end) = b[2..].iter().position(|c| (0x40..=0x7e).contains(c)).map(|p| p + 2) else {
            return if b.len() > 32 { Step::Done(b.len(), None) } else { Step::More };
        };
        let fin = b[end];
        let body = String::from_utf8_lossy(&b[2..end]).into_owned();
        let n = end + 1;
        if let Some(rest) = body.strip_prefix('<') {
            return Step::Done(n, mouse(rest, fin));
        }
        let params: Vec<u16> = body.split(';').map(|p| p.parse().unwrap_or(0)).collect();
        let p0 = params.first().copied().unwrap_or(0);
        let mods = params.get(1).map(|m| mods_from(*m)).unwrap_or(KeyModifiers::NONE);
        let code = match fin {
            b'A' => KeyCode::Up,
            b'B' => KeyCode::Down,
            b'C' => KeyCode::Right,
            b'D' => KeyCode::Left,
            b'H' => KeyCode::Home,
            b'F' => KeyCode::End,
            b'P' => KeyCode::F(1),
            b'Q' => KeyCode::F(2),
            b'R' => KeyCode::F(3),
            b'S' => KeyCode::F(4),
            b'Z' => return Step::Done(n, Some(key(KeyCode::BackTab, KeyModifiers::SHIFT))),
            b'I' => return Step::Done(n, Some(Event::FocusGained)),
            b'O' => return Step::Done(n, Some(Event::FocusLost)),
            b'~' => match p0 {
                1 | 7 => KeyCode::Home,
                2 => KeyCode::Insert,
                3 => KeyCode::Delete,
                4 | 8 => KeyCode::End,
                5 => KeyCode::PageUp,
                6 => KeyCode::PageDown,
                11..=15 => KeyCode::F((p0 - 10) as u8),
                17..=21 => KeyCode::F((p0 - 11) as u8),
                23 | 24 => KeyCode::F((p0 - 12) as u8),
                200 => {
                    self.paste = Some(Vec::new());
                    return Step::Done(n, None);
                }
                _ => return Step::Done(n, None),
            },
            _ => return Step::Done(n, None),
        };
        Step::Done(n, Some(key(code, mods)))
    }
}

/// An SGR mouse report: `b;x;y` then `M` (press, move) or `m` (release).
fn mouse(body: &str, fin: u8) -> Option<Event> {
    let mut it = body.split(';').map(|p| p.parse::<u16>().ok());
    let (b, x, y) = (it.next()??, it.next()??, it.next()??);
    let mut modifiers = KeyModifiers::NONE;
    if b & 4 != 0 {
        modifiers |= KeyModifiers::SHIFT;
    }
    if b & 8 != 0 {
        modifiers |= KeyModifiers::ALT;
    }
    if b & 16 != 0 {
        modifiers |= KeyModifiers::CONTROL;
    }
    let button = match b & 3 {
        0 => MouseButton::Left,
        1 => MouseButton::Middle,
        _ => MouseButton::Right,
    };
    let kind = if b & 64 != 0 {
        match b & 3 {
            0 => MouseEventKind::ScrollUp,
            1 => MouseEventKind::ScrollDown,
            2 => MouseEventKind::ScrollLeft,
            _ => MouseEventKind::ScrollRight,
        }
    } else if b & 32 != 0 {
        if b & 3 == 3 { MouseEventKind::Moved } else { MouseEventKind::Drag(button) }
    } else if fin == b'm' {
        MouseEventKind::Up(button)
    } else {
        MouseEventKind::Down(button)
    };
    Some(Event::Mouse(MouseEvent { kind, column: x.saturating_sub(1), row: y.saturating_sub(1), modifiers }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode, m: KeyModifiers) -> Event {
        key(code, m)
    }

    #[test]
    fn keys() {
        let mut p = Parser::new();
        let n = KeyModifiers::NONE;
        assert_eq!(
            p.feed(b"a\r\x7f\t"),
            vec![k(KeyCode::Char('a'), n), k(KeyCode::Enter, n), k(KeyCode::Backspace, n), k(KeyCode::Tab, n)]
        );
        assert_eq!(
            p.feed(b"\x0b\x1d\x00"),
            vec![
                k(KeyCode::Char('k'), KeyModifiers::CONTROL),
                k(KeyCode::Char(']'), KeyModifiers::CONTROL),
                k(KeyCode::Char(' '), KeyModifiers::CONTROL)
            ]
        );
        assert_eq!(
            p.feed(b"\x1b[A\x1bOQ\x1b[1;5C\x1b[5~\x1b[24~\x1b[3;2~"),
            vec![
                k(KeyCode::Up, n),
                k(KeyCode::F(2), n),
                k(KeyCode::Right, KeyModifiers::CONTROL),
                k(KeyCode::PageUp, n),
                k(KeyCode::F(12), n),
                k(KeyCode::Delete, KeyModifiers::SHIFT)
            ]
        );
        assert_eq!(p.feed(b"\x1bx\x1b"), vec![k(KeyCode::Char('x'), KeyModifiers::ALT), k(KeyCode::Esc, n)]);
        assert_eq!(p.feed("é€".as_bytes()), vec![k(KeyCode::Char('é'), n), k(KeyCode::Char('€'), n)]);
        assert_eq!(p.feed(b"Q"), vec![k(KeyCode::Char('Q'), KeyModifiers::SHIFT)]);
    }

    #[test]
    fn split_reads() {
        let mut p = Parser::new();
        assert!(p.feed(b"\x1b[1;").is_empty());
        assert_eq!(p.feed(b"2A"), vec![k(KeyCode::Up, KeyModifiers::SHIFT)]);
        let e = "é".as_bytes();
        assert!(p.feed(&e[..1]).is_empty());
        assert_eq!(p.feed(&e[1..]), vec![k(KeyCode::Char('é'), KeyModifiers::NONE)]);
    }

    #[test]
    fn mouse_and_paste() {
        let mut p = Parser::new();
        let ev = p.feed(b"\x1b[<0;10;5M\x1b[<64;1;1M\x1b[<0;10;5m");
        assert_eq!(ev.len(), 3);
        assert!(matches!(
            ev[0],
            Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 9, row: 4, .. })
        ));
        assert!(matches!(ev[1], Event::Mouse(MouseEvent { kind: MouseEventKind::ScrollUp, .. })));
        assert!(matches!(ev[2], Event::Mouse(MouseEvent { kind: MouseEventKind::Up(_), .. })));
        assert!(p.feed(b"\x1b[200~hello\r\nwor").is_empty());
        assert_eq!(p.feed(b"ld\x1b[20"), vec![]);
        assert_eq!(
            p.feed(b"1~x"),
            vec![Event::Paste("hello\nworld".into()), k(KeyCode::Char('x'), KeyModifiers::NONE)]
        );
    }
}
