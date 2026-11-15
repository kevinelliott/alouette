//! A small multi-line editor with soft wrapping, for writing mail.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthChar;

#[derive(Debug, Clone, Default)]
pub struct Editor {
    /// Logical lines (no newlines inside).
    pub lines: Vec<String>,
    /// Cursor: line, and char index within it.
    pub row: usize,
    pub col: usize,
    /// First visual row shown.
    pub scroll: usize,
}

/// One visual row: (logical line, start char, end char).
type Seg = (usize, usize, usize);

impl Editor {
    pub fn new(text: &str) -> Self {
        let mut lines: Vec<String> = text.split('\n').map(|l| l.trim_end_matches('\r').to_string()).collect();
        if lines.is_empty() {
            lines.push(String::new());
        }
        Self { lines, ..Default::default() }
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    fn line_len(&self, r: usize) -> usize {
        self.lines[r].chars().count()
    }

    fn byte(&self, r: usize, c: usize) -> usize {
        self.lines[r].char_indices().nth(c).map(|(i, _)| i).unwrap_or(self.lines[r].len())
    }

    pub fn insert(&mut self, s: &str) {
        for (i, part) in s.split('\n').enumerate() {
            if i > 0 {
                self.newline();
            }
            let part = part.trim_end_matches('\r').replace('\t', "    ");
            let at = self.byte(self.row, self.col);
            self.lines[self.row].insert_str(at, &part);
            self.col += part.chars().count();
        }
    }

    fn newline(&mut self) {
        let at = self.byte(self.row, self.col);
        let rest = self.lines[self.row].split_off(at);
        self.lines.insert(self.row + 1, rest);
        self.row += 1;
        self.col = 0;
    }

    /// Handle a key. Returns false for keys it does not use.
    pub fn on_key(&mut self, k: KeyEvent, width: usize) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char(c) if !ctrl && !k.modifiers.contains(KeyModifiers::ALT) => self.insert(&c.to_string()),
            KeyCode::Enter => self.newline(),
            KeyCode::Tab => self.insert("    "),
            KeyCode::Backspace => {
                if self.col > 0 {
                    let a = self.byte(self.row, self.col - 1);
                    let b = self.byte(self.row, self.col);
                    self.lines[self.row].replace_range(a..b, "");
                    self.col -= 1;
                } else if self.row > 0 {
                    let line = self.lines.remove(self.row);
                    self.row -= 1;
                    self.col = self.line_len(self.row);
                    self.lines[self.row].push_str(&line);
                }
            }
            KeyCode::Delete => {
                if self.col < self.line_len(self.row) {
                    let a = self.byte(self.row, self.col);
                    let b = self.byte(self.row, self.col + 1);
                    self.lines[self.row].replace_range(a..b, "");
                } else if self.row + 1 < self.lines.len() {
                    let next = self.lines.remove(self.row + 1);
                    self.lines[self.row].push_str(&next);
                }
            }
            KeyCode::Left => {
                if self.col > 0 {
                    self.col -= 1;
                } else if self.row > 0 {
                    self.row -= 1;
                    self.col = self.line_len(self.row);
                }
            }
            KeyCode::Right => {
                if self.col < self.line_len(self.row) {
                    self.col += 1;
                } else if self.row + 1 < self.lines.len() {
                    self.row += 1;
                    self.col = 0;
                }
            }
            KeyCode::Up => self.vertical(-1, width),
            KeyCode::Down => self.vertical(1, width),
            KeyCode::PageUp => self.vertical(-10, width),
            KeyCode::PageDown => self.vertical(10, width),
            KeyCode::Home if ctrl => (self.row, self.col) = (0, 0),
            KeyCode::End if ctrl => {
                self.row = self.lines.len() - 1;
                self.col = self.line_len(self.row);
            }
            KeyCode::Home => self.col = 0,
            KeyCode::End => self.col = self.line_len(self.row),
            KeyCode::Char('a') if ctrl => self.col = 0,
            KeyCode::Char('e') if ctrl => self.col = self.line_len(self.row),
            KeyCode::Char('k') if ctrl => {
                let at = self.byte(self.row, self.col);
                self.lines[self.row].truncate(at);
            }
            _ => return false,
        }
        true
    }

    /// Visual rows for a width, breaking at spaces where possible.
    fn segments(&self, width: usize) -> Vec<Seg> {
        let width = width.max(4);
        let mut out = vec![];
        for (r, line) in self.lines.iter().enumerate() {
            let chars: Vec<char> = line.chars().collect();
            if chars.is_empty() {
                out.push((r, 0, 0));
                continue;
            }
            let mut start = 0;
            while start < chars.len() {
                let mut w = 0;
                let mut end = start;
                while end < chars.len() {
                    let cw = chars[end].width().unwrap_or(0);
                    if w + cw > width {
                        break;
                    }
                    w += cw;
                    end += 1;
                }
                if end < chars.len()
                    && let Some(sp) = chars[start..end].iter().rposition(|c| *c == ' ')
                    && sp > 0
                {
                    end = start + sp + 1;
                }
                out.push((r, start, end));
                start = end;
            }
            // A cursor at the very end of a full row goes on a new visual row.
            if r == self.row
                && self.col == chars.len()
                && let Some(&(_, s, e)) = out.last()
                && e - s >= width
                && e == chars.len()
            {
                out.push((r, e, e));
            }
        }
        out
    }

    /// The visual row and column of the cursor.
    fn cursor_visual(&self, segs: &[Seg]) -> (usize, usize) {
        for (i, &(r, s, e)) in segs.iter().enumerate() {
            let last_of_line = segs.get(i + 1).is_none_or(|n| n.0 != r);
            if r == self.row && self.col >= s && (self.col < e || last_of_line) {
                let col: usize = self.lines[r].chars().skip(s).take(self.col - s).map(|c| c.width().unwrap_or(0)).sum();
                return (i, col);
            }
        }
        (segs.len().saturating_sub(1), 0)
    }

    fn vertical(&mut self, delta: isize, width: usize) {
        let segs = self.segments(width);
        let (vr, vc) = self.cursor_visual(&segs);
        let target = (vr as isize + delta).clamp(0, segs.len() as isize - 1) as usize;
        let (r, s, e) = segs[target];
        self.row = r;
        let mut col = s;
        let mut w = 0;
        for ch in self.lines[r].chars().skip(s).take(e - s) {
            let cw = ch.width().unwrap_or(0);
            if w + cw > vc {
                break;
            }
            w += cw;
            col += 1;
        }
        self.col = col.min(self.line_len(r));
    }

    /// The rows to draw and the cursor position within them, for a box of this size.
    pub fn view(&mut self, width: usize, height: usize) -> (Vec<String>, (usize, usize)) {
        let segs = self.segments(width);
        let (vr, vc) = self.cursor_visual(&segs);
        if vr < self.scroll {
            self.scroll = vr;
        }
        if height > 0 && vr >= self.scroll + height {
            self.scroll = vr + 1 - height;
        }
        let rows = segs
            .iter()
            .skip(self.scroll)
            .take(height)
            .map(|&(r, s, e)| self.lines[r].chars().skip(s).take(e - s).collect())
            .collect();
        (rows, (vr - self.scroll, vc))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    #[test]
    fn typing_and_wrapping() {
        let mut e = Editor::new("");
        e.insert("hello world, this wraps");
        e.on_key(key(KeyCode::Enter), 10);
        e.insert("é");
        assert_eq!(e.text(), "hello world, this wraps\né");
        let (rows, cur) = e.view(10, 10);
        assert_eq!(rows, ["hello ", "world, ", "this wraps", "é"]);
        assert_eq!(cur, (3, 1));
        e.on_key(key(KeyCode::Up), 10);
        assert_eq!((e.row, e.col), (0, 14));
        e.on_key(key(KeyCode::Backspace), 10);
        assert!(e.text().starts_with("hello world, his wraps"));
        // End moves to the end of the line.
        e.on_key(key(KeyCode::End), 10);
        let (rows, cur) = e.view(10, 10);
        assert_eq!(rows, ["hello ", "world, ", "his wraps", "é"]);
        assert_eq!(cur, (2, 9));
    }

    #[test]
    fn joins_and_pastes() {
        let mut e = Editor::new("a\nb");
        e.row = 1;
        e.on_key(key(KeyCode::Backspace), 20);
        assert_eq!(e.text(), "ab");
        e.insert("1\r\n2\n3");
        assert_eq!(e.text(), "a1\n2\n3b");
        assert_eq!((e.row, e.col), (2, 1));
        e.on_key(key(KeyCode::Delete), 20);
        assert_eq!(e.text(), "a1\n2\n3");
    }
}
