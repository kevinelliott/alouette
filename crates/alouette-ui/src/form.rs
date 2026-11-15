//! A small form: labelled one-line fields in a dialog, for editing a contact, an event
//! or an account.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::chrome::fit;
use crate::input::{InputResult, LineInput};
use crate::theme::Theme;

pub struct Field {
    pub label: String,
    pub input: LineInput,
    /// Shown dimmed after the label while the field is empty.
    pub hint: String,
}

pub struct Form {
    pub title: String,
    pub fields: Vec<Field>,
    pub sel: usize,
    /// A line under the fields, such as why saving failed.
    pub note: Option<String>,
}

pub enum FormResult {
    /// Ctrl+S, or Enter on the last field.
    Submit,
    Cancel,
    Edited,
    Ignored,
}

impl Form {
    pub fn new(title: &str) -> Self {
        Form { title: title.to_string(), fields: vec![], sel: 0, note: None }
    }

    pub fn field(mut self, label: &str, value: &str, hint: &str) -> Self {
        self.fields.push(Field { label: label.to_string(), input: LineInput::new(value), hint: hint.to_string() });
        self
    }

    /// The value of the field with this label, trimmed.
    pub fn get(&self, label: &str) -> String {
        self.fields.iter().find(|f| f.label == label).map(|f| f.input.value.trim().to_string()).unwrap_or_default()
    }

    pub fn on_paste(&mut self, text: &str) {
        if let Some(f) = self.fields.get_mut(self.sel) {
            let v = format!("{}{}", f.input.value, text.replace(['\r', '\n'], " "));
            f.input.set(&v);
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> FormResult {
        let n = self.fields.len();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('s') if ctrl => return FormResult::Submit,
            KeyCode::Esc => return FormResult::Cancel,
            KeyCode::Tab | KeyCode::Down => self.sel = (self.sel + 1) % n.max(1),
            KeyCode::BackTab | KeyCode::Up => self.sel = (self.sel + n.max(1) - 1) % n.max(1),
            KeyCode::Enter => {
                if self.sel + 1 >= n {
                    return FormResult::Submit;
                }
                self.sel += 1;
            }
            _ => {
                let Some(f) = self.fields.get_mut(self.sel) else { return FormResult::Ignored };
                return match f.input.on_key(key) {
                    InputResult::Edited => FormResult::Edited,
                    _ => FormResult::Ignored,
                };
            }
        }
        FormResult::Edited
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect, t: &Theme) {
        let w = area.width.saturating_sub(4).clamp(30, 78).min(area.width);
        let h = (self.fields.len() as u16 + 4 + self.note.is_some() as u16).min(area.height);
        let r = Rect {
            x: area.x + area.width.saturating_sub(w) / 2,
            y: area.y + area.height.saturating_sub(h) / 2,
            width: w,
            height: h,
        };
        frame.render_widget(Clear, r);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} ", self.title))
            .style(Style::new().fg(t.dialog_fg).bg(t.dialog_bg));
        let inner = block.inner(r);
        frame.render_widget(block, r);
        let lw = self.fields.iter().map(|f| f.label.chars().count()).max().unwrap_or(0) + 1;
        let vw = (inner.width as usize).saturating_sub(lw + 1);
        let mut lines = vec![];
        for (i, f) in self.fields.iter().enumerate() {
            let input = Style::new().fg(t.input_fg).bg(t.input_bg);
            let value = if f.input.value.is_empty() && i != self.sel {
                Span::styled(fit(&f.hint, vw), t.dim())
            } else {
                // Keep the cursor in view on long values.
                let col = f.input.cursor_col();
                let skip = col.saturating_sub(vw.saturating_sub(1));
                let shown: String = f.input.value.chars().skip(skip).collect();
                if i == self.sel {
                    let x = inner.x + (lw + 1 + col - skip) as u16;
                    let y = inner.y + i as u16;
                    if x < inner.right() && y < inner.bottom() {
                        frame.set_cursor_position((x, y));
                    }
                }
                Span::styled(fit(&shown, vw), input)
            };
            let label_style = if i == self.sel { t.accent() } else { Style::new() };
            lines.push(Line::from(vec![Span::styled(format!("{:>lw$} ", f.label), label_style), value]));
        }
        lines.push(Line::default());
        if let Some(n) = &self.note {
            lines.push(Line::from(Span::styled(n.clone(), Style::new().fg(t.error))));
        }
        lines.push(Line::from(vec![
            Span::styled("Tab", t.accent()),
            Span::raw(" next field  "),
            Span::styled("Ctrl+S", t.accent()),
            Span::raw(" save  "),
            Span::styled("Esc", t.accent()),
            Span::raw(" cancel"),
        ]));
        frame.render_widget(Paragraph::new(lines), inner);
    }
}
