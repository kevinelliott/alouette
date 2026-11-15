use alouette_store::HistoryEntry;
use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};

use crate::window::{Cx, DrawCx, Handled, Window};

/// Minuet kept "a log of recently accessed URLs". This is that log.
pub struct HistoryWindow {
    entries: Vec<HistoryEntry>,
    state: ListState,
}

impl HistoryWindow {
    pub fn new(cx: &Cx) -> Self {
        let mut w = Self { entries: vec![], state: ListState::default() };
        w.reload(cx);
        w
    }

    fn reload(&mut self, cx: &Cx) {
        self.entries = cx.store.and_then(|s| s.recent_history(500).ok()).unwrap_or_default();
        self.state.select(if self.entries.is_empty() { None } else { Some(0) });
    }
}

impl Window for HistoryWindow {
    fn title(&self) -> String {
        "History".into()
    }
    fn module(&self) -> &'static str {
        "core"
    }
    fn url(&self) -> Option<String> {
        Some("about:history".into())
    }
    fn draw(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        if self.entries.is_empty() {
            frame.render_widget(
                Paragraph::new(vec![
                    Line::default(),
                    Line::from("  Nothing here yet. URLs you open are listed here, newest first."),
                ])
                .style(t.window()),
                area,
            );
            return;
        }
        let items: Vec<ListItem> = self
            .entries
            .iter()
            .map(|e| {
                let when = e
                    .visited_at
                    .format(time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]"))
                    .unwrap_or_default();
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{when}  "), t.dim()),
                    Span::styled(
                        format!("{:<28}", crate::chrome::truncate(&e.title, 28)),
                        ratatui::style::Style::new(),
                    ),
                    Span::styled(e.url.clone(), t.link()),
                ]))
            })
            .collect();
        let list = List::new(items).style(t.window()).highlight_style(if cx.focused {
            t.selection()
        } else {
            Default::default()
        });
        frame.render_stateful_widget(list, area, &mut self.state);
    }
    fn on_key(&mut self, key: KeyEvent, cx: &mut Cx) -> Handled {
        let n = self.entries.len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.state.select(Some(self.state.selected().unwrap_or(0).saturating_sub(1)))
            }
            KeyCode::Down | KeyCode::Char('j') if n > 0 => {
                self.state.select(Some((self.state.selected().unwrap_or(0) + 1).min(n - 1)))
            }
            KeyCode::Enter => {
                if let Some(e) = self.state.selected().and_then(|i| self.entries.get(i)) {
                    cx.open_url(e.url.clone());
                }
            }
            KeyCode::Char('r') => self.reload(cx),
            KeyCode::Char('C') => {
                if let Some(s) = cx.store {
                    let _ = s.clear_history();
                }
                self.reload(cx);
            }
            _ => return Handled::No,
        }
        Handled::Yes
    }
    fn on_mouse(&mut self, ev: MouseEvent, area: Rect, cx: &mut Cx) -> Handled {
        if let MouseEventKind::Down(_) = ev.kind {
            let i = (ev.row - area.y) as usize + self.state.offset();
            if i < self.entries.len() {
                if self.state.selected() == Some(i) {
                    cx.open_url(self.entries[i].url.clone());
                }
                self.state.select(Some(i));
            }
            return Handled::Yes;
        }
        Handled::No
    }
    fn hints(&self) -> Vec<(&'static str, &'static str)> {
        vec![("Enter", "Open"), ("r", "Reload"), ("C", "Clear all")]
    }
}
