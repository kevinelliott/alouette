//! Writing a post: an optional content warning, the text, and who can see it.

use alouette_core::config::SocialAccount;
use alouette_core::{AppEvent, Notification};
use alouette_net::Net;
use alouette_ui::editor::Editor;
use alouette_ui::input::{InputResult, LineInput};
use alouette_ui::window::{Cx, DrawCx, Handled, Window};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::api::{Client, VISIBILITIES};
use crate::window::{Kind, Msg, send};
use crate::{MODULE, next_id};

/// The usual limit; servers may allow more.
const LIMIT: usize = 500;

#[derive(Debug, Clone, Default)]
pub struct Seed {
    pub text: String,
    pub spoiler: String,
    pub visibility: String,
    /// The post being answered: (id, a short quote for the header).
    pub reply_to: Option<(String, String)>,
}

pub struct ComposeWindow {
    id: u64,
    net: Net,
    account: SocialAccount,
    token: String,
    origin: u64,
    cw: LineInput,
    editor: Editor,
    visibility: usize,
    reply_to: Option<(String, String)>,
    focus: usize,
    busy: bool,
    confirm_close: bool,
    note: Option<String>,
    body_area: Rect,
}

impl ComposeWindow {
    pub fn new(net: Net, account: SocialAccount, token: String, seed: Seed, origin: u64) -> Self {
        let mut editor = Editor::new(&seed.text);
        editor.row = editor.lines.len() - 1;
        editor.col = editor.lines.last().map(|l| l.chars().count()).unwrap_or(0);
        Self {
            id: next_id(),
            net,
            account,
            token,
            origin,
            cw: LineInput::new(&seed.spoiler),
            editor,
            visibility: VISIBILITIES.iter().position(|v| *v == seed.visibility).unwrap_or(0),
            reply_to: seed.reply_to,
            focus: 1,
            busy: false,
            confirm_close: false,
            note: None,
            body_area: Rect::default(),
        }
    }

    fn post(&mut self, cx: &mut Cx) {
        let text = self.editor.text().trim().to_string();
        if text.is_empty() {
            self.note = Some("Write something first".into());
            return;
        }
        if self.busy {
            return;
        }
        self.busy = true;
        self.note = Some("Posting…".into());
        let c = Client::new(self.net.clone(), &self.account.instance, Some(self.token.clone()));
        let (id, origin, events) = (self.id, self.origin, cx.tasks.events());
        let (cw, vis, reply) = (
            self.cw.value.trim().to_string(),
            VISIBILITIES[self.visibility],
            self.reply_to.as_ref().map(|r| r.0.clone()),
        );
        tokio::spawn(async move {
            let r = c.post(&text, reply.as_deref(), &cw, vis).await;
            send(&events, id, Kind::Posted(r.clone()));
            if r.is_ok() {
                send(&events, origin, Kind::Posted(r));
            }
        });
    }
}

impl Window for ComposeWindow {
    fn title(&self) -> String {
        if self.reply_to.is_some() { "Reply".into() } else { format!("New post · {}", self.account.name) }
    }
    fn module(&self) -> &'static str {
        MODULE
    }
    fn captures_keys(&self) -> bool {
        true
    }
    fn wants_text(&self) -> bool {
        true
    }
    fn hints(&self) -> Vec<(&'static str, &'static str)> {
        vec![("Ctrl+S", "Post"), ("Ctrl+V", "Who sees it"), ("Tab", "Warning/Text"), ("Esc", "Close")]
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        if area.height < 5 {
            return;
        }
        let t = cx.theme;
        let mut y = area.y;
        if let Some((_, q)) = &self.reply_to {
            frame
                .render_widget(Paragraph::new(Span::styled(format!(" ↩ {q}"), t.dim())), Rect { y, height: 1, ..area });
            y += 1;
        }
        let focused = self.focus == 0 && cx.focused;
        let field = Style::new().fg(t.input_fg).bg(t.input_bg);
        let label = " Content warning: ";
        let pad = (area.width as usize)
            .saturating_sub(label.len() + unicode_width::UnicodeWidthStr::width(self.cw.value.as_str()));
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(label, if focused { t.accent() } else { t.dim() }),
                Span::styled(
                    format!("{}{}", self.cw.value, " ".repeat(pad)),
                    if focused { field } else { Style::new() },
                ),
            ])),
            Rect { y, height: 1, ..area },
        );
        if focused {
            frame.set_cursor_position((area.x + label.len() as u16 + self.cw.cursor_col() as u16, y));
        }
        y += 1;
        frame.render_widget(
            Paragraph::new(Span::styled("─".repeat(area.width as usize), t.dim())),
            Rect { y, height: 1, ..area },
        );
        y += 1;
        let foot = Rect { y: area.bottom() - 1, height: 1, ..area };
        let body = Rect { y, height: foot.y.saturating_sub(y), ..area };
        self.body_area = body;
        let (rows, (cr, cc)) = self.editor.view(body.width as usize, body.height as usize);
        frame.render_widget(Paragraph::new(rows.into_iter().map(Line::from).collect::<Vec<_>>()), body);
        if self.focus == 1 && cx.focused && body.height > 0 {
            frame.set_cursor_position((body.x + cc as u16, body.y + cr as u16));
        }
        let used = self.editor.text().chars().count() + self.cw.value.chars().count();
        let left = LIMIT as isize - used as isize;
        let line = if self.confirm_close {
            Line::from(vec![
                Span::styled(" Throw this post away? ", Style::new().fg(t.error).add_modifier(Modifier::BOLD)),
                Span::styled("y", t.accent()),
                Span::raw(" yes   "),
                Span::styled("n", t.accent()),
                Span::raw(" no"),
            ])
        } else {
            Line::from(vec![
                Span::styled(format!(" {left} left "), if left < 0 { Style::new().fg(t.error) } else { t.dim() }),
                Span::styled(format!("· {} ", VISIBILITIES[self.visibility]), t.accent()),
                Span::styled(format!("· {}", self.note.clone().unwrap_or_else(|| "Ctrl+S posts".into())), t.dim()),
            ])
        };
        frame.render_widget(Paragraph::new(line), foot);
    }

    fn on_paste(&mut self, text: &str, _cx: &mut Cx) -> Handled {
        if self.focus == 1 {
            self.editor.insert(&text.replace("\r\n", "\n"));
        } else {
            let v = format!("{}{}", self.cw.value, text.lines().collect::<Vec<_>>().join(" "));
            self.cw.set(&v);
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
            KeyCode::Char('v') if ctrl => self.visibility = (self.visibility + 1) % VISIBILITIES.len(),
            KeyCode::Esc => {
                if self.editor.text().trim().is_empty()
                    || self.reply_to.is_some()
                        && self.editor.text().split_whitespace().all(|w| w.starts_with('@'))
                {
                    cx.close();
                } else {
                    self.confirm_close = true;
                }
            }
            KeyCode::Tab | KeyCode::BackTab => self.focus = 1 - self.focus,
            _ if self.focus == 1 => {
                if ctrl && !matches!(key.code, KeyCode::Char('a' | 'e' | 'k')) || matches!(key.code, KeyCode::F(_)) {
                    return Handled::No;
                }
                if key.code == KeyCode::Up && self.editor.row == 0 {
                    self.focus = 0;
                    return Handled::Yes;
                }
                return self.editor.on_key(key, self.body_area.width.max(1) as usize).into();
            }
            KeyCode::Enter | KeyCode::Down => self.focus = 1,
            _ => {
                if let InputResult::Ignored = self.cw.on_key(key) {
                    return Handled::No;
                }
            }
        }
        Handled::Yes
    }

    fn on_event(&mut self, ev: &AppEvent, cx: &mut Cx) {
        let AppEvent::Module { module: MODULE, payload } = ev else { return };
        let Some(m) = payload.downcast_ref::<Msg>() else { return };
        if m.win != self.id {
            return;
        }
        if let Kind::Posted(r) = &m.kind {
            self.busy = false;
            match r {
                Ok(_) => {
                    cx.notify(Notification::info("Posted"));
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
