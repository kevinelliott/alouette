//! The Connect window: type a destination or pick a host from `~/.ssh/config` and your
//! saved hosts.

use alouette_ui::input::{InputResult, LineInput};
use alouette_ui::window::{Cx, DrawCx, Handled, Window};
use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::MODULE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Ssh,
    Telnet,
    Mosh,
}

impl Kind {
    fn scheme(self) -> &'static str {
        match self {
            Kind::Ssh => "ssh",
            Kind::Telnet => "telnet",
            Kind::Mosh => "mosh",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Kind::Ssh => "SSH",
            Kind::Telnet => "Telnet",
            Kind::Mosh => "Mosh",
        }
    }
}

/// One row in the host list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub detail: String,
    pub source: &'static str,
}

pub struct HostsWindow {
    kind: Kind,
    input: LineInput,
    hosts: Vec<Entry>,
    selected: usize,
    scroll: usize,
    list_area: Rect,
}

impl HostsWindow {
    pub fn new(kind: Kind, hosts: Vec<Entry>) -> Self {
        Self { kind, input: LineInput::default(), hosts, selected: 0, scroll: 0, list_area: Rect::default() }
    }

    fn filtered(&self) -> Vec<&Entry> {
        let q = self.input.value.trim().to_lowercase();
        self.hosts
            .iter()
            .filter(|h| q.is_empty() || h.name.to_lowercase().contains(&q) || h.detail.to_lowercase().contains(&q))
            .collect()
    }

    fn connect(&mut self, cx: &mut Cx) {
        let typed = self.input.value.trim().to_string();
        let list = self.filtered();
        let dest = match list.get(self.selected) {
            // A typed name that matches a listed host picks it; anything else is a destination.
            Some(h) if typed.is_empty() || list.len() == 1 || h.name.eq_ignore_ascii_case(&typed) => h.name.clone(),
            _ if !typed.is_empty() => typed,
            _ => return,
        };
        let dest = dest.trim_start_matches(&format!("{}://", self.kind.scheme())).to_string();
        cx.open_url(format!("{}://{dest}", self.kind.scheme()));
    }
}

impl Window for HostsWindow {
    fn title(&self) -> String {
        format!("{} connect", self.kind.label())
    }
    fn module(&self) -> &'static str {
        MODULE
    }
    fn url(&self) -> Option<String> {
        Some(format!("{}:", self.kind.scheme()))
    }
    fn wants_text(&self) -> bool {
        true
    }
    fn hints(&self) -> Vec<(&'static str, &'static str)> {
        vec![("Enter", "Connect"), ("↑↓", "Pick a host")]
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        if area.height < 4 {
            return;
        }
        let label = format!(" {}> ", self.kind.scheme());
        let field = Style::new().fg(t.input_fg).bg(t.input_bg);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(label.clone(), t.accent()),
                Span::raw(self.input.value.clone()),
            ]))
            .style(field),
            Rect { height: 1, ..area },
        );
        if cx.focused {
            let x = area.x + label.len() as u16 + self.input.cursor_col() as u16;
            frame.set_cursor_position((x.min(area.right().saturating_sub(1)), area.y));
        }
        let example = match self.kind {
            Kind::Ssh => "Type user@host:port or a host name, or pick one below.",
            Kind::Telnet => "Type host or host:port. Try telehack.com or towel.blinkenlights.nl.",
            Kind::Mosh => "Type user@host. Mosh starts over SSH and then keeps the session through roaming.",
        };
        frame.render_widget(Paragraph::new(Span::styled(example, t.dim())), Rect { y: area.y + 1, height: 1, ..area });

        let list: Vec<Entry> = self.filtered().into_iter().cloned().collect();
        self.selected = self.selected.min(list.len().saturating_sub(1));
        let body = Rect { y: area.y + 3, height: area.height - 3, ..area };
        self.list_area = body;
        if list.is_empty() {
            let msg = if self.hosts.is_empty() && self.kind != Kind::Telnet {
                "No hosts yet. Add Host entries to ~/.ssh/config or [[ssh.hosts]] to alouette.toml."
            } else if self.hosts.is_empty() {
                ""
            } else {
                "No host matches; Enter connects to what you typed."
            };
            frame.render_widget(Paragraph::new(Span::styled(msg, t.dim())), body);
            return;
        }
        let rows = body.height as usize;
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + rows {
            self.scroll = self.selected + 1 - rows;
        }
        let w = list.iter().map(|h| h.name.chars().count()).max().unwrap_or(0).clamp(8, 28);
        let lines: Vec<Line> = list
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(rows)
            .map(|(i, h)| {
                let style = if i == self.selected { t.selection() } else { Style::new() };
                Line::from(vec![
                    Span::styled(format!(" {:<w$}  ", h.name), style.patch(Style::new().fg(t.link))),
                    Span::styled(format!("{:<40}", h.detail), style),
                    Span::styled(format!(" {}", h.source), if i == self.selected { style } else { t.dim() }),
                ])
            })
            .collect();
        frame.render_widget(Paragraph::new(lines), body);
    }

    fn on_key(&mut self, key: KeyEvent, cx: &mut Cx) -> Handled {
        let n = self.filtered().len();
        match key.code {
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected = (self.selected + 1).min(n.saturating_sub(1)),
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(10),
            KeyCode::PageDown => self.selected = (self.selected + 10).min(n.saturating_sub(1)),
            _ => {
                return match self.input.on_key(key) {
                    InputResult::Submit => {
                        self.connect(cx);
                        Handled::Yes
                    }
                    InputResult::Edited => {
                        self.selected = 0;
                        Handled::Yes
                    }
                    InputResult::Cancel => {
                        self.input.set("");
                        Handled::Yes
                    }
                    InputResult::Ignored => Handled::No,
                };
            }
        }
        Handled::Yes
    }

    fn on_mouse(&mut self, ev: MouseEvent, _area: Rect, cx: &mut Cx) -> Handled {
        let a = self.list_area;
        match ev.kind {
            MouseEventKind::Down(_) if ev.row >= a.y && ev.row < a.bottom() => {
                let i = self.scroll + usize::from(ev.row - a.y);
                if i < self.filtered().len() {
                    if i == self.selected {
                        self.input.set("");
                        self.connect(cx);
                    }
                    self.selected = i;
                }
                Handled::Yes
            }
            MouseEventKind::ScrollUp => {
                self.selected = self.selected.saturating_sub(1);
                Handled::Yes
            }
            MouseEventKind::ScrollDown => {
                self.selected = (self.selected + 1).min(self.filtered().len().saturating_sub(1));
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}
