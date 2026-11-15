//! Modal dialogs: a one-line input and a yes/no question.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::chrome;
use crate::theme::Theme;

/// What a dialog is for, so the app knows what to do with the answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Purpose {
    OpenUrl,
    ConfirmQuit,
}

pub enum DialogKind {
    Input { value: String, cursor: usize, placeholder: String },
    Confirm { yes: bool },
}

pub struct Dialog {
    pub purpose: Purpose,
    pub title: String,
    pub message: String,
    pub kind: DialogKind,
}

pub enum DialogOutcome {
    Continue,
    Cancel,
    Submit(String),
    Yes,
    No,
}

impl Dialog {
    pub fn input(purpose: Purpose, title: &str, message: &str, initial: &str, placeholder: &str) -> Self {
        Self {
            purpose,
            title: title.into(),
            message: message.into(),
            kind: DialogKind::Input {
                value: initial.into(),
                cursor: initial.chars().count(),
                placeholder: placeholder.into(),
            },
        }
    }

    pub fn confirm(purpose: Purpose, title: &str, message: &str) -> Self {
        Self { purpose, title: title.into(), message: message.into(), kind: DialogKind::Confirm { yes: false } }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> DialogOutcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match &mut self.kind {
            DialogKind::Input { value, cursor, .. } => match key.code {
                KeyCode::Esc => DialogOutcome::Cancel,
                KeyCode::Enter => DialogOutcome::Submit(value.clone()),
                KeyCode::Left => {
                    *cursor = cursor.saturating_sub(1);
                    DialogOutcome::Continue
                }
                KeyCode::Right => {
                    *cursor = (*cursor + 1).min(value.chars().count());
                    DialogOutcome::Continue
                }
                KeyCode::Home => {
                    *cursor = 0;
                    DialogOutcome::Continue
                }
                KeyCode::End => {
                    *cursor = value.chars().count();
                    DialogOutcome::Continue
                }
                KeyCode::Backspace => {
                    if *cursor > 0 {
                        let i = value.char_indices().nth(*cursor - 1).map(|(i, _)| i).unwrap_or(0);
                        value.remove(i);
                        *cursor -= 1;
                    }
                    DialogOutcome::Continue
                }
                KeyCode::Delete => {
                    if let Some((i, _)) = value.char_indices().nth(*cursor) {
                        value.remove(i);
                    }
                    DialogOutcome::Continue
                }
                KeyCode::Char('u') if ctrl => {
                    value.clear();
                    *cursor = 0;
                    DialogOutcome::Continue
                }
                KeyCode::Char(c) if !ctrl => {
                    let i = value.char_indices().nth(*cursor).map(|(i, _)| i).unwrap_or(value.len());
                    value.insert(i, c);
                    *cursor += 1;
                    DialogOutcome::Continue
                }
                _ => DialogOutcome::Continue,
            },
            DialogKind::Confirm { yes } => match key.code {
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => DialogOutcome::No,
                KeyCode::Char('y') | KeyCode::Char('Y') => DialogOutcome::Yes,
                KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                    *yes = !*yes;
                    DialogOutcome::Continue
                }
                KeyCode::Enter => {
                    if *yes {
                        DialogOutcome::Yes
                    } else {
                        DialogOutcome::No
                    }
                }
                _ => DialogOutcome::Continue,
            },
        }
    }

    pub fn draw(&self, frame: &mut Frame, screen: Rect, theme: &Theme, shadows: bool) {
        let width = 60.min(screen.width.saturating_sub(4)).max(24.min(screen.width));
        let msg_lines = (self.message.width() as u16 / width.saturating_sub(4).max(1)) + 1;
        let height = (msg_lines + 5).min(screen.height);
        let rect = chrome::centered(screen, width, height);
        let style = Style::new().fg(theme.dialog_fg).bg(theme.dialog_bg);
        frame.render_widget(Clear, rect);
        let blk = Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Double)
            .border_style(style)
            .title(format!(" {} ", self.title))
            .title_alignment(ratatui::layout::Alignment::Center)
            .style(style);
        let inner = blk.inner(rect);
        frame.render_widget(blk, rect);
        if shadows {
            chrome::shadow(frame.buffer_mut(), rect, screen, theme);
        }
        if inner.height < 3 {
            return;
        }
        let msg_area = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), height: inner.height - 2, ..inner };
        frame.render_widget(
            Paragraph::new(Line::from(self.message.clone())).wrap(Wrap { trim: true }).style(style),
            msg_area,
        );
        let row = inner.bottom() - 1;
        match &self.kind {
            DialogKind::Input { value, cursor, placeholder } => {
                let input = Rect { x: inner.x + 1, y: row, width: inner.width.saturating_sub(2), height: 1 };
                let istyle = Style::new().fg(theme.input_fg).bg(theme.input_bg);
                frame.buffer_mut().set_style(input, istyle);
                if value.is_empty() {
                    frame.buffer_mut().set_string(input.x, row, placeholder, istyle.add_modifier(Modifier::DIM));
                } else {
                    frame.buffer_mut().set_string(input.x, row, chrome::truncate(value, input.width as usize), istyle);
                }
                let cx = input.x + value.chars().take(*cursor).collect::<String>().width() as u16;
                frame.set_cursor_position((cx.min(input.right().saturating_sub(1)), row));
            }
            DialogKind::Confirm { yes } => {
                let yes_label = " Yes ";
                let no_label = " No ";
                let total = (yes_label.len() + no_label.len() + 4) as u16;
                let x = inner.x + inner.width.saturating_sub(total) / 2;
                let sel = theme.menu_selected().add_modifier(Modifier::BOLD);
                frame.buffer_mut().set_string(x, row, format!("[{yes_label}]"), if *yes { sel } else { style });
                frame.buffer_mut().set_string(
                    x + yes_label.len() as u16 + 3,
                    row,
                    format!("[{no_label}]"),
                    if *yes { style } else { sel },
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_edits_and_submits() {
        let mut d = Dialog::input(Purpose::OpenUrl, "Open", "URL", "", "");
        for c in "about:tXks".chars() {
            d.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        d.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        d.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        d.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        d.on_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        d.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        d.on_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        d.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        d.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        match d.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
            DialogOutcome::Submit(v) => assert_eq!(v, "about:tasks"),
            _ => panic!("expected submit"),
        }
    }

    #[test]
    fn confirm_defaults_to_no() {
        let mut d = Dialog::confirm(Purpose::ConfirmQuit, "Exit", "Tasks are running.");
        assert!(matches!(d.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), DialogOutcome::No));
        d.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert!(matches!(d.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), DialogOutcome::Yes));
    }
}
