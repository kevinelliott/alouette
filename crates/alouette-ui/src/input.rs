//! A one-line text field.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Default)]
pub struct LineInput {
    pub value: String,
    cursor: usize,
}

pub enum InputResult {
    Submit,
    Cancel,
    Edited,
    Ignored,
}

impl LineInput {
    pub fn new(value: &str) -> Self {
        Self { value: value.to_string(), cursor: value.chars().count() }
    }

    pub fn set(&mut self, value: &str) {
        *self = Self::new(value);
    }

    fn byte(&self, ch: usize) -> usize {
        self.value.char_indices().nth(ch).map(|(i, _)| i).unwrap_or(self.value.len())
    }

    pub fn on_key(&mut self, key: KeyEvent) -> InputResult {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let len = self.value.chars().count();
        match key.code {
            KeyCode::Enter => return InputResult::Submit,
            KeyCode::Esc => return InputResult::Cancel,
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(len),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = len,
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = len,
            KeyCode::Char('u') if ctrl => {
                let b = self.byte(self.cursor);
                self.value.replace_range(..b, "");
                self.cursor = 0;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                let b = self.byte(self.cursor - 1);
                self.value.remove(b);
                self.cursor -= 1;
            }
            KeyCode::Delete if self.cursor < len => {
                let b = self.byte(self.cursor);
                self.value.remove(b);
            }
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                let b = self.byte(self.cursor);
                self.value.insert(b, c);
                self.cursor += 1;
            }
            KeyCode::Backspace | KeyCode::Delete => {}
            _ => return InputResult::Ignored,
        }
        InputResult::Edited
    }

    /// Display column of the cursor.
    pub fn cursor_col(&self) -> usize {
        self.value.chars().take(self.cursor).collect::<String>().width()
    }
}
