//! The command palette: one box for every command, window, module, theme and URL.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Matcher, Utf32Str};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};
use unicode_width::UnicodeWidthStr;

use crate::chrome;
use crate::keymap::Action;
use crate::theme::Theme;
use crate::wm::WinId;

/// What choosing a palette entry does.
#[derive(Debug, Clone, PartialEq)]
pub enum PaletteCmd {
    Action(Action),
    Open(String),
    Focus(WinId),
    Theme(String),
    Keymap(alouette_core::config::KeymapPreset),
    Layout(alouette_core::config::LayoutKind),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PaletteItem {
    /// What the entry is, such as "Command", "Window" or "Theme".
    pub kind: &'static str,
    pub label: String,
    pub detail: String,
    pub cmd: PaletteCmd,
}

impl PaletteItem {
    pub fn new(kind: &'static str, label: impl Into<String>, detail: impl Into<String>, cmd: PaletteCmd) -> Self {
        Self { kind, label: label.into(), detail: detail.into(), cmd }
    }
    fn haystack(&self) -> String {
        format!("{} {} {}", self.kind, self.label, self.detail)
    }
}

pub struct Palette {
    pub query: String,
    cursor: usize,
    items: Vec<PaletteItem>,
    shown: Vec<usize>,
    /// A URL item built from the query, shown first.
    url_item: Option<PaletteItem>,
    state: ListState,
    matcher: Matcher,
}

/// What the palette wants after a key.
pub enum PaletteOutcome {
    Continue,
    Close,
    Run(PaletteCmd),
}

impl Palette {
    pub fn new(items: Vec<PaletteItem>, query: &str) -> Self {
        let mut p = Self {
            query: query.to_string(),
            cursor: query.chars().count(),
            items,
            shown: vec![],
            url_item: None,
            state: ListState::default(),
            matcher: Matcher::new(nucleo_matcher::Config::DEFAULT),
        };
        p.refilter();
        p
    }

    fn refilter(&mut self) {
        let q = self.query.trim();
        self.url_item = alouette_core::router::normalize(q)
            .filter(|_| q.contains(':') || q.contains('.') || q.contains('@'))
            .map(|u| PaletteItem::new("Open", u.to_string(), "Open this URL", PaletteCmd::Open(u.to_string())));
        if q.is_empty() {
            self.shown = (0..self.items.len()).collect();
        } else {
            let pattern = Pattern::parse(q, CaseMatching::Ignore, Normalization::Smart);
            let mut buf = Vec::new();
            let mut scored: Vec<(u32, usize)> = self
                .items
                .iter()
                .enumerate()
                .filter_map(|(i, it)| {
                    let hay = it.haystack();
                    pattern.score(Utf32Str::new(&hay, &mut buf), &mut self.matcher).map(|s| (s, i))
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            self.shown = scored.into_iter().map(|(_, i)| i).collect();
        }
        let total = self.len();
        self.state.select(if total == 0 { None } else { Some(0) });
    }

    fn len(&self) -> usize {
        self.shown.len() + usize::from(self.url_item.is_some())
    }

    fn item_at(&self, i: usize) -> Option<&PaletteItem> {
        match &self.url_item {
            Some(u) if i == 0 => Some(u),
            Some(_) => self.shown.get(i - 1).map(|j| &self.items[*j]),
            None => self.shown.get(i).map(|j| &self.items[*j]),
        }
    }

    /// The labels currently listed, for tests.
    pub fn visible_labels(&self) -> Vec<String> {
        (0..self.len()).filter_map(|i| self.item_at(i)).map(|it| it.label.clone()).collect()
    }

    pub fn on_key(&mut self, key: KeyEvent) -> PaletteOutcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return PaletteOutcome::Close,
            KeyCode::Char('g') if ctrl => return PaletteOutcome::Close,
            KeyCode::Enter => {
                return match self.state.selected().and_then(|i| self.item_at(i)) {
                    Some(it) => PaletteOutcome::Run(it.cmd.clone()),
                    None => PaletteOutcome::Continue,
                };
            }
            KeyCode::Up => self.select(-1),
            KeyCode::Char('p') if ctrl => self.select(-1),
            KeyCode::Down | KeyCode::Tab => self.select(1),
            KeyCode::Char('n') if ctrl => self.select(1),
            KeyCode::PageUp => self.select(-8),
            KeyCode::PageDown => self.select(8),
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.query.chars().count()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::End => self.cursor = self.query.chars().count(),
            KeyCode::Char('e') if ctrl => self.cursor = self.query.chars().count(),
            KeyCode::Char('u') if ctrl => {
                self.query.clear();
                self.cursor = 0;
                self.refilter();
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    let idx = byte_index(&self.query, self.cursor - 1);
                    self.query.remove(idx);
                    self.cursor -= 1;
                    self.refilter();
                }
            }
            KeyCode::Delete => {
                if self.cursor < self.query.chars().count() {
                    let idx = byte_index(&self.query, self.cursor);
                    self.query.remove(idx);
                    self.refilter();
                }
            }
            KeyCode::Char(c) if !ctrl => {
                let idx = byte_index(&self.query, self.cursor);
                self.query.insert(idx, c);
                self.cursor += 1;
                self.refilter();
            }
            _ => {}
        }
        PaletteOutcome::Continue
    }

    fn select(&mut self, delta: isize) {
        let n = self.len();
        if n == 0 {
            return;
        }
        let cur = self.state.selected().unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, n as isize - 1);
        self.state.select(Some(next as usize));
    }

    pub fn draw(&mut self, frame: &mut Frame, screen: Rect, theme: &Theme, shadows: bool) {
        let width = 72.min(screen.width.saturating_sub(4)).max(20.min(screen.width));
        let height = 18.min(screen.height.saturating_sub(4)).max(5.min(screen.height));
        let rect =
            Rect { x: screen.x + (screen.width - width) / 2, y: screen.y + 2.min(screen.height / 6), width, height };
        frame.render_widget(Clear, rect);
        let style = Style::new().fg(theme.dialog_fg).bg(theme.dialog_bg);
        let blk = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(theme.border_active_fg).bg(theme.dialog_bg))
            .title(" Command palette ")
            .title_style(theme.accent())
            .style(style);
        let inner = blk.inner(rect);
        frame.render_widget(blk, rect);
        if shadows {
            chrome::shadow(frame.buffer_mut(), rect, screen, theme);
        }
        if inner.height < 2 {
            return;
        }
        // Input line.
        let input = Rect { height: 1, ..inner };
        let input_style = Style::new().fg(theme.input_fg).bg(theme.input_bg);
        frame.buffer_mut().set_style(input, input_style);
        let prompt = "> ";
        frame.buffer_mut().set_string(input.x, input.y, prompt, input_style.add_modifier(Modifier::BOLD));
        let shown_query = chrome::truncate(&self.query, input.width.saturating_sub(3) as usize);
        frame.buffer_mut().set_string(input.x + 2, input.y, &shown_query, input_style);
        if self.query.is_empty() {
            frame.buffer_mut().set_string(
                input.x + 2,
                input.y,
                "Type a command, a window, a theme or a URL",
                input_style.add_modifier(Modifier::DIM),
            );
        }
        let cursor_x = input.x + 2 + self.query.chars().take(self.cursor).collect::<String>().width() as u16;
        frame.set_cursor_position((cursor_x.min(input.right().saturating_sub(1)), input.y));

        let list_area = Rect { y: inner.y + 1, height: inner.height - 1, ..inner };
        let label_w = list_area.width.saturating_sub(12) as usize;
        let items: Vec<ListItem> = (0..self.len())
            .filter_map(|i| self.item_at(i))
            .map(|it| {
                let detail_room = label_w.saturating_sub(it.label.width() + 2);
                let mut spans = vec![
                    Span::styled(format!("{:<8} ", it.kind), theme.dim()),
                    Span::raw(chrome::truncate(&it.label, label_w)),
                ];
                if detail_room > 4 && !it.detail.is_empty() {
                    spans.push(Span::styled(format!("  {}", chrome::truncate(&it.detail, detail_room)), theme.dim()));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();
        let empty = items.is_empty();
        let list = List::new(items).style(style).highlight_style(theme.menu_selected());
        frame.render_stateful_widget(list, list_area, &mut self.state);
        if empty {
            frame.buffer_mut().set_string(list_area.x + 1, list_area.y, "No matches", theme.dim());
        }
    }
}

fn byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map(|(i, _)| i).unwrap_or(s.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<PaletteItem> {
        vec![
            PaletteItem::new("Command", "Tasks", "Ctrl-T", PaletteCmd::Action(Action::Tasks)),
            PaletteItem::new("Theme", "amber", "Amber phosphor", PaletteCmd::Theme("amber".into())),
            PaletteItem::new("Theme", "modern-dark", "", PaletteCmd::Theme("modern-dark".into())),
            PaletteItem::new("Command", "Exit Alouette", "Alt-X", PaletteCmd::Action(Action::Quit)),
        ]
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn fuzzy_filters_and_runs() {
        let mut p = Palette::new(items(), "");
        assert_eq!(p.visible_labels().len(), 4);
        for c in "amb".chars() {
            p.on_key(key(c));
        }
        assert_eq!(p.visible_labels()[0], "amber");
        match p.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
            PaletteOutcome::Run(PaletteCmd::Theme(t)) => assert_eq!(t, "amber"),
            _ => panic!("expected theme command"),
        }
    }

    #[test]
    fn prefilled_query_and_urls() {
        let p = Palette::new(items(), "theme ");
        assert!(p.visible_labels().iter().all(|l| l == "amber" || l == "modern-dark"));
        let p = Palette::new(items(), "gemini://geminiprotocol.net/");
        assert_eq!(p.visible_labels()[0], "gemini://geminiprotocol.net/");
    }

    #[test]
    fn editing_keys() {
        let mut p = Palette::new(items(), "tasx");
        p.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        p.on_key(key('k'));
        assert_eq!(p.query, "task");
        p.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(p.query, "");
        assert!(matches!(p.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)), PaletteOutcome::Close));
    }
}
