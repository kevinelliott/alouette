//! Writing a news article: groups and subject above, the text below.

use std::sync::Arc;

use alouette_core::{AppEvent, Notification};
use alouette_ui::editor::Editor;
use alouette_ui::input::{InputResult, LineInput};
use alouette_ui::window::{Cx, DrawCx, Handled, Window};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::conn::Conn;
use crate::news::{Kind, Msg, send};
use crate::{NEWS, next_id};

/// What a new article starts with.
#[derive(Debug, Clone, Default)]
pub struct Seed {
    pub groups: String,
    pub subject: String,
    pub body: String,
    /// With angle brackets.
    pub references: Vec<String>,
}

/// Build the article text.
pub fn article(seed: &Seed, name: Option<&str>, email: &str) -> Result<Vec<u8>, String> {
    use mail_builder::MessageBuilder;
    use mail_builder::headers::address::Address;
    use mail_builder::headers::raw::Raw;
    let groups: Vec<&str> = seed.groups.split([',', ' ']).map(str::trim).filter(|g| !g.is_empty()).collect();
    if groups.is_empty() {
        return Err("Name at least one group".into());
    }
    if seed.subject.trim().is_empty() {
        return Err("Add a subject".into());
    }
    if !email.contains('@') {
        return Err("Set your address under [news] email in Settings".into());
    }
    let domain = email.rsplit('@').next().unwrap_or("localhost");
    let id = format!("{}.{}@{domain}", time::OffsetDateTime::now_utc().unix_timestamp_nanos(), std::process::id());
    let mut m = MessageBuilder::new()
        .from(Address::new_address(name.map(str::to_string), email.to_string()))
        .subject(seed.subject.trim().to_string())
        .message_id(id)
        .header("Newsgroups", Raw::new(groups.join(",")))
        .header("User-Agent", Raw::new(format!("Alouette/{}", env!("CARGO_PKG_VERSION"))))
        .text_body(seed.body.replace("\r\n", "\n"));
    if !seed.references.is_empty() {
        let refs: Vec<String> = seed.references.iter().map(|r| r.trim_matches(['<', '>']).to_string()).collect();
        if let Some(last) = refs.last() {
            m = m.in_reply_to(last.clone());
        }
        m = m.references(refs);
    }
    m.write_to_vec().map_err(|e| e.to_string())
}

pub struct PostWindow {
    id: u64,
    conn: Arc<Conn>,
    groups: LineInput,
    subject: LineInput,
    editor: Editor,
    references: Vec<String>,
    name: Option<String>,
    email: Option<String>,
    origin: Option<u64>,
    focus: usize,
    busy: bool,
    dirty: bool,
    confirm_close: bool,
    note: Option<String>,
    body_area: Rect,
}

impl PostWindow {
    pub fn new(
        conn: Arc<Conn>,
        seed: Seed,
        name: Option<String>,
        email: Option<String>,
        origin: Option<u64>,
        _cx: &mut Cx,
    ) -> Self {
        let focus = if seed.subject.is_empty() { 1 } else { 2 };
        Self {
            id: next_id(),
            conn,
            groups: LineInput::new(&seed.groups),
            subject: LineInput::new(&seed.subject),
            editor: Editor::new(&seed.body),
            references: seed.references,
            note: email.is_none().then(|| "Set your name and address under [news] in Settings before posting.".into()),
            name,
            email,
            origin,
            focus,
            busy: false,
            dirty: false,
            confirm_close: false,
            body_area: Rect::default(),
        }
    }

    fn seed(&self) -> Seed {
        Seed {
            groups: self.groups.value.clone(),
            subject: self.subject.value.clone(),
            body: self.editor.text(),
            references: self.references.clone(),
        }
    }

    fn post(&mut self, cx: &mut Cx) {
        if self.busy {
            return;
        }
        let raw = match article(&self.seed(), self.name.as_deref(), self.email.as_deref().unwrap_or("")) {
            Ok(r) => r,
            Err(e) => {
                self.note = Some(e);
                return;
            }
        };
        self.busy = true;
        self.note = Some("Posting…".into());
        let (id, origin, events, conn) = (self.id, self.origin, cx.tasks.events(), self.conn.clone());
        let groups = self.groups.value.clone();
        tokio::spawn(async move {
            let r = conn.post(&raw).await.map(|()| format!("Posted to {groups}"));
            send(&events, id, Kind::Posted(r.clone()));
            if let (Some(o), Ok(_)) = (origin, &r) {
                send(&events, o, Kind::Posted(r));
            }
        });
    }
}

impl Window for PostWindow {
    fn title(&self) -> String {
        let s = self.subject.value.trim();
        if s.is_empty() { "New article".into() } else { format!("Post: {s}") }
    }
    fn module(&self) -> &'static str {
        NEWS
    }
    fn captures_keys(&self) -> bool {
        true
    }
    fn wants_text(&self) -> bool {
        true
    }
    fn hints(&self) -> Vec<(&'static str, &'static str)> {
        vec![("Ctrl+S", "Post"), ("Tab", "Next field"), ("Esc", "Close")]
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        if area.height < 5 {
            return;
        }
        let t = cx.theme;
        let field = Style::new().fg(t.input_fg).bg(t.input_bg);
        let from = match (&self.name, &self.email) {
            (Some(n), Some(e)) => format!("{n} <{e}>"),
            (None, Some(e)) => e.clone(),
            _ => "(no address set)".into(),
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![Span::styled("      From: ", t.dim()), Span::styled(from, t.dim())])),
            Rect { height: 1, ..area },
        );
        for (i, (label, input)) in [("Newsgroups", &self.groups), ("Subject", &self.subject)].into_iter().enumerate() {
            let y = area.y + 1 + i as u16;
            let focused = self.focus == i && cx.focused;
            let lab = format!("{label:>10}: ");
            let pad =
                (area.width as usize).saturating_sub(12 + unicode_width::UnicodeWidthStr::width(input.value.as_str()));
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(lab, if focused { t.accent() } else { t.dim() }),
                    Span::styled(
                        format!("{}{}", input.value, " ".repeat(pad)),
                        if focused { field } else { Style::new() },
                    ),
                ])),
                Rect { y, height: 1, ..area },
            );
            if focused {
                frame.set_cursor_position((area.x + 12 + input.cursor_col() as u16, y));
            }
        }
        frame.render_widget(
            Paragraph::new(Span::styled("─".repeat(area.width as usize), t.dim())),
            Rect { y: area.y + 3, height: 1, ..area },
        );
        let foot = Rect { y: area.bottom() - 1, height: 1, ..area };
        let body = Rect { y: area.y + 4, height: foot.y.saturating_sub(area.y + 4), ..area };
        self.body_area = body;
        let (rows, (cr, cc)) = self.editor.view(body.width as usize, body.height as usize);
        frame.render_widget(Paragraph::new(rows.into_iter().map(Line::from).collect::<Vec<_>>()), body);
        if self.focus == 2 && cx.focused && body.height > 0 {
            frame.set_cursor_position((body.x + cc as u16, body.y + cr as u16));
        }
        let line = if self.confirm_close {
            Line::from(vec![
                Span::styled(" Close without posting? ", Style::new().fg(t.error).add_modifier(Modifier::BOLD)),
                Span::styled("y", t.accent()),
                Span::raw(" yes   "),
                Span::styled("n", t.accent()),
                Span::raw(" no"),
            ])
        } else {
            Line::from(Span::styled(
                format!(" {}", self.note.clone().unwrap_or_else(|| "Ctrl+S posts · Esc closes".into())),
                t.dim(),
            ))
        };
        frame.render_widget(Paragraph::new(line), foot);
    }

    fn on_paste(&mut self, text: &str, _cx: &mut Cx) -> Handled {
        self.dirty = true;
        match self.focus {
            2 => self.editor.insert(&text.replace("\r\n", "\n")),
            i => {
                let f = if i == 0 { &mut self.groups } else { &mut self.subject };
                let v = format!("{}{}", f.value, text.lines().collect::<Vec<_>>().join(" "));
                f.set(&v);
            }
        }
        Handled::Yes
    }

    fn on_key(&mut self, key: KeyEvent, cx: &mut Cx) -> Handled {
        if self.confirm_close {
            self.confirm_close = false;
            if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                cx.close();
            }
            return Handled::Yes;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('s') if ctrl => self.post(cx),
            KeyCode::Esc => {
                if self.dirty {
                    self.confirm_close = true;
                } else {
                    cx.close();
                }
            }
            KeyCode::Tab => self.focus = (self.focus + 1) % 3,
            KeyCode::BackTab => self.focus = (self.focus + 2) % 3,
            _ if self.focus == 2 => {
                if ctrl && !matches!(key.code, KeyCode::Char('a' | 'e' | 'k')) || matches!(key.code, KeyCode::F(_)) {
                    return Handled::No;
                }
                if key.code == KeyCode::Up && self.editor.row == 0 {
                    self.focus = 1;
                    return Handled::Yes;
                }
                let used = self.editor.on_key(key, self.body_area.width.max(1) as usize);
                if used
                    && matches!(
                        key.code,
                        KeyCode::Char(_) | KeyCode::Enter | KeyCode::Backspace | KeyCode::Delete | KeyCode::Tab
                    )
                {
                    self.dirty = true;
                }
                return used.into();
            }
            KeyCode::Enter | KeyCode::Down => self.focus += 1,
            KeyCode::Up => self.focus = self.focus.saturating_sub(1),
            _ => {
                let f = if self.focus == 0 { &mut self.groups } else { &mut self.subject };
                match f.on_key(key) {
                    InputResult::Edited => self.dirty = true,
                    InputResult::Ignored => return Handled::No,
                    _ => {}
                }
            }
        }
        Handled::Yes
    }

    fn on_event(&mut self, ev: &AppEvent, cx: &mut Cx) {
        let AppEvent::Module { module: NEWS, payload } = ev else { return };
        let Some(m) = payload.downcast_ref::<Msg>() else { return };
        if m.win != self.id {
            return;
        }
        if let Kind::Posted(r) = &m.kind {
            self.busy = false;
            match r {
                Ok(n) => {
                    cx.notify(Notification::info(n.clone()));
                    cx.close();
                }
                Err(e) => {
                    self.note = Some(e.clone());
                    cx.notify(Notification::error(e.clone()));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_articles() {
        let seed = Seed {
            groups: "comp.lang.rust, alt.test".into(),
            subject: "Re: Hi".into(),
            body: "Text\n".into(),
            references: vec!["<a@x>".into()],
        };
        let raw = String::from_utf8(article(&seed, Some("Me"), "me@example.org").unwrap()).unwrap();
        assert!(raw.contains("Newsgroups: comp.lang.rust,alt.test\r\n"), "{raw}");
        assert!(raw.contains("References: <a@x>"), "{raw}");
        assert!(raw.contains("From: \"Me\" <me@example.org>") || raw.contains("From: Me <me@example.org>"), "{raw}");
        assert!(article(&Seed { groups: String::new(), ..seed.clone() }, None, "me@x").is_err());
        assert!(article(&seed, None, "nobody").is_err());
    }
}
