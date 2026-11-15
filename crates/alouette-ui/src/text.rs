//! A scrollable text view, shared by help, about and other read-only windows.

use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Text};
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap};

use crate::theme::Theme;
use crate::window::Handled;

/// Text that scrolls with the arrow keys, Page Up and Down, Home, End and the mouse wheel.
#[derive(Debug, Clone, Default)]
pub struct ScrollText {
    pub lines: Vec<Line<'static>>,
    pub scroll: u16,
    viewport: u16,
    content_height: u16,
}

impl ScrollText {
    pub fn new(lines: Vec<Line<'static>>) -> Self {
        Self { lines, ..Default::default() }
    }

    pub fn set_lines(&mut self, lines: Vec<Line<'static>>) {
        self.lines = lines;
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let para = Paragraph::new(Text::from(self.lines.clone())).wrap(Wrap { trim: false }).style(theme.window());
        self.content_height = para.line_count(area.width.saturating_sub(1)) as u16;
        self.viewport = area.height;
        self.scroll = self.scroll.min(self.max_scroll());
        let text_area = Rect { width: area.width.saturating_sub(1), ..area };
        frame.render_widget(para.scroll((self.scroll, 0)), text_area);
        if self.content_height > self.viewport {
            let mut state = ScrollbarState::new(self.max_scroll() as usize).position(self.scroll as usize);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight).style(theme.dim()),
                area,
                &mut state,
            );
        }
    }

    fn max_scroll(&self) -> u16 {
        self.content_height.saturating_sub(self.viewport)
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Handled {
        let page = self.viewport.saturating_sub(1).max(1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll = (self.scroll + 1).min(self.max_scroll()),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(page),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll = (self.scroll + page).min(self.max_scroll()),
            KeyCode::Home | KeyCode::Char('g') => self.scroll = 0,
            KeyCode::End | KeyCode::Char('G') => self.scroll = self.max_scroll(),
            _ => return Handled::No,
        }
        Handled::Yes
    }

    pub fn on_mouse(&mut self, ev: MouseEvent) -> Handled {
        match ev.kind {
            MouseEventKind::ScrollUp => self.scroll = self.scroll.saturating_sub(3),
            MouseEventKind::ScrollDown => self.scroll = (self.scroll + 3).min(self.max_scroll()),
            _ => return Handled::No,
        }
        Handled::Yes
    }
}
