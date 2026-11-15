//! A scrolling view of lookup sections, with selectable links. Inquire and NetTools both
//! show their results with it.

use alouette_lookup::{Out, Section, Style as OStyle};
use alouette_ui::theme::Theme;
use alouette_ui::window::{Cx, Handled};
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// A slot is a section that may still be loading.
#[derive(Debug, Clone)]
pub enum Slot {
    Waiting { source: String, title: String },
    Done(Section),
}

#[derive(Debug, Clone, PartialEq)]
enum RowKind {
    Header,
    Waiting,
    Line(OStyle),
    Blank,
}

#[derive(Debug, Clone)]
struct Row {
    kind: RowKind,
    text: String,
    link: Option<String>,
    /// Rows from the same wrapped line share this id, for link selection.
    id: usize,
}

#[derive(Default)]
pub struct Report {
    pub slots: Vec<Slot>,
    /// Lines shown before the slots, such as a ping log.
    pub log: Vec<Out>,
    rows: Vec<Row>,
    width: usize,
    dirty: bool,
    scroll: usize,
    view_h: usize,
    sel: Option<usize>,
    /// Keep the view at the bottom as lines arrive (for logs).
    pub follow: bool,
}

impl Report {
    pub fn clear(&mut self) {
        *self = Self { follow: self.follow, ..Self::default() };
        self.dirty = true;
    }

    pub fn set_slots(&mut self, slots: Vec<Slot>) {
        self.slots = slots;
        self.dirty = true;
    }

    pub fn fill(&mut self, index: usize, section: Section) {
        if let Some(s) = self.slots.get_mut(index) {
            *s = Slot::Done(section);
            self.dirty = true;
        }
    }

    pub fn push_log(&mut self, o: Out) {
        self.log.push(o);
        self.dirty = true;
    }

    pub fn waiting(&self) -> usize {
        self.slots.iter().filter(|s| matches!(s, Slot::Waiting { .. })).count()
    }

    fn rebuild(&mut self, width: usize) {
        let mut rows = vec![];
        let mut id = 0;
        let push_out = |rows: &mut Vec<Row>, o: &Out, id: &mut usize| {
            *id += 1;
            let indent = if o.style == OStyle::Field { 15 } else { 0 };
            for (i, piece) in wrap(&o.text, width.max(10), indent).into_iter().enumerate() {
                rows.push(Row {
                    kind: RowKind::Line(o.style),
                    text: if i == 0 { piece } else { format!("{}{piece}", " ".repeat(indent)) },
                    link: o.link.clone(),
                    id: *id,
                });
            }
        };
        for o in &self.log {
            push_out(&mut rows, o, &mut id);
        }
        for slot in &self.slots {
            if !rows.is_empty() {
                rows.push(Row { kind: RowKind::Blank, text: String::new(), link: None, id: 0 });
            }
            match slot {
                Slot::Waiting { source, title } => {
                    rows.push(Row { kind: RowKind::Header, text: format!("{title}  · {source}"), link: None, id: 0 });
                    rows.push(Row { kind: RowKind::Waiting, text: format!("asking {source}…"), link: None, id: 0 });
                }
                Slot::Done(s) => {
                    rows.push(Row {
                        kind: RowKind::Header,
                        text: format!("{}  · {}", s.title, s.source),
                        link: None,
                        id: 0,
                    });
                    for o in &s.lines {
                        push_out(&mut rows, o, &mut id);
                    }
                }
            }
        }
        self.rows = rows;
        self.width = width;
        self.dirty = false;
        if self.sel.is_some_and(|s| !self.rows.iter().any(|r| r.id == s && r.link.is_some())) {
            self.sel = None;
        }
    }

    fn max_scroll(&self) -> usize {
        self.rows.len().saturating_sub(self.view_h)
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect, theme: &Theme, tick: u128) {
        let width = area.width.saturating_sub(1) as usize;
        if self.dirty || width != self.width {
            self.rebuild(width);
        }
        self.view_h = area.height as usize;
        if self.follow {
            self.scroll = self.max_scroll();
        }
        self.scroll = self.scroll.min(self.max_scroll());
        const SPIN: [&str; 4] = ["◐", "◓", "◑", "◒"];
        let lines: Vec<Line> = self
            .rows
            .iter()
            .skip(self.scroll)
            .take(self.view_h)
            .map(|r| {
                let selected = r.link.is_some() && self.sel == Some(r.id);
                let style = match &r.kind {
                    _ if selected => theme.selection(),
                    RowKind::Header => theme.accent().add_modifier(Modifier::UNDERLINED),
                    RowKind::Waiting => theme.dim(),
                    RowKind::Blank => theme.window(),
                    RowKind::Line(_) if r.link.is_some() => theme.link(),
                    RowKind::Line(OStyle::Good) => Style::new().fg(theme.success),
                    RowKind::Line(OStyle::Bad) => Style::new().fg(theme.error),
                    RowKind::Line(OStyle::Dim) => theme.dim(),
                    RowKind::Line(OStyle::Heading) => theme.accent(),
                    RowKind::Line(_) => theme.window(),
                };
                let text = if r.kind == RowKind::Waiting {
                    format!("{} {}", SPIN[(tick / 200) as usize % 4], r.text)
                } else {
                    r.text.clone()
                };
                if let RowKind::Line(OStyle::Field) = r.kind
                    && !selected
                    && text.len() > 15
                    && text.is_char_boundary(15)
                {
                    let (k, v) = text.split_at(15);
                    return Line::from(vec![
                        Span::styled(k.to_string(), theme.dim()),
                        Span::styled(v.to_string(), style),
                    ]);
                }
                Line::from(Span::styled(text, style))
            })
            .collect();
        frame.render_widget(Paragraph::new(lines).style(theme.window()), area);
        if self.rows.len() > self.view_h {
            let mut st = ScrollbarState::new(self.max_scroll()).position(self.scroll);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight).style(theme.dim()),
                area,
                &mut st,
            );
        }
    }

    fn link_ids(&self) -> Vec<usize> {
        let mut ids: Vec<usize> = self.rows.iter().filter(|r| r.link.is_some()).map(|r| r.id).collect();
        ids.dedup();
        ids
    }

    fn select(&mut self, forward: bool) {
        let ids = self.link_ids();
        let next = match self.sel {
            Some(s) if forward => ids.iter().copied().find(|&i| i > s).or(ids.first().copied()),
            Some(s) => ids.iter().rev().copied().find(|&i| i < s).or(ids.last().copied()),
            None => {
                let first_visible = self.rows.iter().skip(self.scroll).find(|r| r.link.is_some()).map(|r| r.id);
                first_visible.or(ids.first().copied())
            }
        };
        self.sel = next;
        if let Some(row) = self.sel.and_then(|s| self.rows.iter().position(|r| r.id == s && r.link.is_some())) {
            if row < self.scroll {
                self.scroll = row;
            } else if row >= self.scroll + self.view_h {
                self.scroll = row + 1 - self.view_h;
            }
        }
        self.follow = false;
    }

    pub fn selected_link(&self) -> Option<String> {
        let s = self.sel?;
        self.rows.iter().find(|r| r.id == s).and_then(|r| r.link.clone())
    }

    fn scroll_by(&mut self, n: isize) {
        self.follow = false;
        self.scroll = (self.scroll as isize + n).clamp(0, self.max_scroll() as isize) as usize;
    }

    pub fn on_key(&mut self, key: KeyEvent, cx: &mut Cx) -> Handled {
        let page = self.view_h.saturating_sub(1).max(1) as isize;
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => self.scroll_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.scroll_by(-1),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll_by(page),
            KeyCode::PageUp => self.scroll_by(-page),
            KeyCode::Home | KeyCode::Char('g') => self.scroll_by(-(self.rows.len() as isize)),
            KeyCode::End | KeyCode::Char('G') => self.scroll_by(self.rows.len() as isize),
            KeyCode::Tab => self.select(true),
            KeyCode::BackTab => self.select(false),
            KeyCode::Enter => match self.selected_link() {
                Some(u) => cx.open_url(u),
                None => return Handled::No,
            },
            _ => return Handled::No,
        }
        Handled::Yes
    }

    pub fn on_mouse(&mut self, ev: MouseEvent, area: Rect, cx: &mut Cx) -> Handled {
        match ev.kind {
            MouseEventKind::ScrollDown => self.scroll_by(3),
            MouseEventKind::ScrollUp => self.scroll_by(-3),
            MouseEventKind::Down(MouseButton::Left) => {
                let Some(r) =
                    (ev.row as usize).checked_sub(area.y as usize).and_then(|r| self.rows.get(r + self.scroll))
                else {
                    return Handled::No;
                };
                if let Some(u) = r.link.clone() {
                    self.sel = Some(r.id);
                    cx.open_url(u);
                }
            }
            _ => return Handled::No,
        }
        Handled::Yes
    }

    /// Plain text of everything shown, for tests and copying.
    pub fn text(&mut self, width: usize) -> String {
        self.rebuild(width);
        self.rows.iter().map(|r| r.text.as_str()).collect::<Vec<_>>().join("\n")
    }
}

/// Word wrap with a hanging indent for continuation lines.
fn wrap(text: &str, width: usize, indent: usize) -> Vec<String> {
    let mut out = vec![];
    let mut line = String::new();
    let mut w = 0;
    let limit = |first: bool| if first { width } else { width.saturating_sub(indent).max(4) };
    for word in text.split(' ') {
        let ww = word.width();
        let lim = limit(out.is_empty());
        if w > 0 && w + 1 + ww > lim {
            out.push(std::mem::take(&mut line));
            w = 0;
        }
        let lim = limit(out.is_empty());
        if ww > lim {
            for c in word.chars() {
                let cw = c.width().unwrap_or(0);
                if w + cw > limit(out.is_empty()) {
                    out.push(std::mem::take(&mut line));
                    w = 0;
                }
                line.push(c);
                w += cw;
            }
            continue;
        }
        if w > 0 {
            line.push(' ');
            w += 1;
        }
        line.push_str(word);
        w += ww;
    }
    out.push(line);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_with_hanging_indent() {
        let w = wrap("Nameservers    a.example, b.example, c.example", 30, 15);
        assert_eq!(w[0], "Nameservers    a.example,");
        assert!(w.len() >= 2);
    }

    #[test]
    fn slots_fill_in_order() {
        let mut r = Report::default();
        r.set_slots(vec![
            Slot::Waiting { source: "DNS".into(), title: "DNS records".into() },
            Slot::Waiting { source: "RDAP".into(), title: "RDAP".into() },
        ]);
        assert_eq!(r.waiting(), 2);
        let mut s = Section::new("RDAP", "RDAP example.org");
        s.field("Name", "EXAMPLE.ORG");
        r.fill(1, s);
        let text = r.text(60);
        assert!(text.contains("asking DNS"));
        assert!(text.contains("Name           EXAMPLE.ORG"));
        assert_eq!(r.waiting(), 1);
    }
}
