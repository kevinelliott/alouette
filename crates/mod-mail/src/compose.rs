//! Writing a message: address fields above, the text below.

use std::sync::Arc;

use alouette_core::config::MailAccount;
use alouette_core::{AppEvent, Notification};
use alouette_mail::{Backend, Draft, Flags};
use alouette_ui::ask::Asker;
use alouette_ui::input::{InputResult, LineInput};
use alouette_ui::window::{Cx, DrawCx, Handled, Window};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::bridge::{self, send};
use crate::editor::Editor;
use crate::window::{Kind, Msg, next_id};
use crate::{MODULE, Pending, Shared};

const LABELS: [&str; 4] = ["To", "Cc", "Bcc", "Subject"];
const BODY: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Job {
    Send,
    Draft,
}

pub struct ComposeWindow {
    id: u64,
    account: MailAccount,
    shared: Shared,
    fields: [LineInput; 4],
    editor: Editor,
    focus: usize,
    reply_of: Option<(String, String)>,
    origin: Option<u64>,
    in_reply_to: Option<String>,
    references: Vec<String>,
    busy: Option<Job>,
    dirty: bool,
    confirm_close: bool,
    closing: bool,
    asker: Asker,
    typed: String,
    note: Option<String>,
    body_area: Rect,
    field_top: u16,
}

impl Drop for ComposeWindow {
    fn drop(&mut self) {
        self.asker.answer(None);
    }
}

fn domain(email: &str) -> &str {
    email.rsplit_once('@').map(|(_, d)| d).filter(|d| !d.is_empty()).unwrap_or("localhost")
}

impl ComposeWindow {
    pub fn new(account: MailAccount, shared: Shared, p: Pending, cx: &mut Cx) -> Self {
        let wake = cx.tasks.events();
        let asker = Asker::new(move || {
            let _ =
                wake.send(AppEvent::Module { module: MODULE, payload: alouette_core::event::ModulePayload::new(()) });
        });
        let d = p.draft;
        // Start in the text when the address is known (a reply), else in To.
        let focus = if d.to.is_empty() {
            0
        } else if d.subject.is_empty() {
            3
        } else {
            BODY
        };
        let mut editor = Editor::new(&d.body);
        if focus == BODY {
            // Replies start above the quote; new messages above the signature.
            editor.row = 0;
            editor.col = 0;
        }
        Self {
            id: next_id(),
            account,
            shared,
            fields: [LineInput::new(&d.to), LineInput::new(&d.cc), LineInput::new(&d.bcc), LineInput::new(&d.subject)],
            editor,
            focus,
            reply_of: p.reply_of,
            origin: p.origin,
            in_reply_to: d.in_reply_to,
            references: d.references,
            busy: None,
            dirty: false,
            confirm_close: false,
            closing: false,
            asker,
            typed: String::new(),
            note: None,
            body_area: Rect::default(),
            field_top: 0,
        }
    }

    pub fn draft(&self) -> Draft {
        Draft {
            to: self.fields[0].value.trim().to_string(),
            cc: self.fields[1].value.trim().to_string(),
            bcc: self.fields[2].value.trim().to_string(),
            subject: self.fields[3].value.trim().to_string(),
            body: self.editor.text(),
            in_reply_to: self.in_reply_to.clone(),
            references: self.references.clone(),
        }
    }

    /// Which fields to show: Cc and Bcc only once they have something or have focus.
    fn shown(&self) -> Vec<usize> {
        (0..4).filter(|&i| i == 0 || i == 3 || !self.fields[i].value.is_empty() || self.focus == i).collect()
    }

    fn start(&mut self, job: Job, cx: &mut Cx) {
        if self.busy.is_some() {
            return;
        }
        let d = self.draft();
        let to = d.recipients();
        if job == Job::Send && to.is_empty() {
            self.note = Some("Add someone to send it to".into());
            self.focus = 0;
            return;
        }
        if job == Job::Send
            && d.subject.is_empty()
            && !self.note.as_deref().is_some_and(|n| n.starts_with("No subject"))
        {
            self.note = Some("No subject. Press Ctrl+S again to send anyway.".into());
            return;
        }
        let raw = match d.build(self.account.display_name.as_deref(), &self.account.email, domain(&self.account.email))
        {
            Ok(r) => r,
            Err(e) => {
                self.note = Some(e);
                return;
            }
        };
        self.busy = Some(job);
        self.note = Some(if job == Job::Send { "Sending…".into() } else { "Saving draft…".into() });
        let (id, origin) = (self.id, self.origin);
        let events = cx.tasks.events();
        let net = cx.net.clone();
        let account = self.account.clone();
        let shared = self.shared.clone();
        let secrets: Arc<dyn alouette_mail::alouette_store_like::Secrets> =
            Arc::new(bridge::Secrets(cx.secrets.clone()));
        let prompt: Arc<dyn alouette_mail::Prompt> =
            Arc::new(bridge::Prompt { asker: self.asker.clone(), events: events.clone(), win: id });
        let reply_of = self.reply_of.clone();
        let raw = Arc::new(raw);
        let to = Arc::new(to);
        let who = if to.len() == 1 { to[0].clone() } else { format!("{} people", to.len()) };
        let title = match job {
            Job::Send => format!("Send mail to {who}"),
            Job::Draft => "Save draft".to_string(),
        };
        cx.tasks.spawn(MODULE, title, move |_ctx| {
            let (events, net, account, shared, secrets, prompt) =
                (events.clone(), net.clone(), account.clone(), shared.clone(), secrets.clone(), prompt.clone());
            let (raw, to, reply_of, who) = (raw.clone(), to.clone(), reply_of.clone(), who.clone());
            async move {
                let result = async {
                    let b: Arc<dyn Backend> = match shared.get(&account.name) {
                        Some(b) => b,
                        None => {
                            let b = alouette_mail::connect(net, &account, secrets, prompt).await?;
                            shared.put(&account.name, b.clone());
                            b
                        }
                    };
                    match job {
                        Job::Send => {
                            b.send(&raw, &account.email, &to).await?;
                            if let Some((folder, mid)) = &reply_of {
                                let _ = b
                                    .set_flags(
                                        folder,
                                        std::slice::from_ref(mid),
                                        Flags { answered: true, ..Default::default() },
                                        true,
                                    )
                                    .await;
                            }
                            Ok(format!("Sent to {who}"))
                        }
                        Job::Draft => {
                            b.save_draft(&raw).await?;
                            Ok("Draft saved".to_string())
                        }
                    }
                }
                .await;
                send(&events, id, Kind::Sent(result.clone()));
                if job == Job::Send
                    && let (Some(o), Ok(_)) = (origin, &result)
                {
                    send(&events, o, Kind::Sent(result.clone()));
                }
                result.map(|_| ())
            }
        });
    }

    fn move_focus(&mut self, down: bool) {
        let order: Vec<usize> = self.shown().into_iter().chain([BODY]).collect();
        let at = order.iter().position(|&f| f == self.focus).unwrap_or(0);
        let next = if down { (at + 1).min(order.len() - 1) } else { at.saturating_sub(1) };
        self.focus = order[next];
    }

    fn close(&mut self, cx: &mut Cx) {
        if self.dirty && !self.confirm_close {
            self.confirm_close = true;
            return;
        }
        cx.close();
    }
}

impl Window for ComposeWindow {
    fn title(&self) -> String {
        let s = self.fields[3].value.trim();
        if s.is_empty() { format!("New message · {}", self.account.name) } else { format!("Write: {s}") }
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
        vec![
            ("Ctrl+S", "Send"),
            ("Ctrl+D", "Save draft"),
            ("Tab", "Next field"),
            ("Ctrl+B", "Cc/Bcc"),
            ("Esc", "Close"),
        ]
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        if area.height < 4 || area.width < 16 {
            return;
        }
        let t = cx.theme;
        let lw = 9u16;
        let mut y = area.y;
        let from = match &self.account.display_name {
            Some(n) => format!("{n} <{}>", self.account.email),
            None => self.account.email.clone(),
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("{:>7}: ", "From"), t.dim()),
                Span::styled(from, t.dim()),
            ])),
            Rect { y, height: 1, ..area },
        );
        y += 1;
        self.field_top = y;
        for i in self.shown() {
            let row = Rect { y, height: 1, ..area };
            let focused = self.focus == i && cx.focused;
            let label = Span::styled(format!("{:>7}: ", LABELS[i]), if focused { t.accent() } else { t.dim() });
            let style = if focused { Style::new().fg(t.input_fg).bg(t.input_bg) } else { Style::new() };
            let w = area.width.saturating_sub(lw) as usize;
            let v = &self.fields[i].value;
            let pad = w.saturating_sub(unicode_width::UnicodeWidthStr::width(v.as_str()));
            frame.render_widget(
                Paragraph::new(Line::from(vec![label, Span::styled(format!("{v}{}", " ".repeat(pad)), style)])),
                row,
            );
            if focused && self.asker.question().is_none() {
                frame.set_cursor_position((area.x + lw + self.fields[i].cursor_col() as u16, y));
            }
            y += 1;
        }
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
        if self.focus == BODY && cx.focused && self.asker.question().is_none() && body.height > 0 {
            frame.set_cursor_position((body.x + cc as u16, body.y + cr as u16));
        }
        let foot_line = if self.confirm_close {
            Line::from(vec![
                Span::styled(" Close without sending? ", Style::new().fg(t.error).add_modifier(Modifier::BOLD)),
                Span::styled("y", t.accent()),
                Span::raw(" discard   "),
                Span::styled("d", t.accent()),
                Span::raw(" save draft   "),
                Span::styled("n", t.accent()),
                Span::raw(" keep writing"),
            ])
        } else {
            Line::from(Span::styled(
                format!(
                    " {}",
                    self.note.clone().unwrap_or_else(|| "Ctrl+S sends · Ctrl+D saves a draft · Esc closes".into())
                ),
                t.dim(),
            ))
        };
        frame.render_widget(Paragraph::new(foot_line), foot);
        if let Some(q) = self.asker.question() {
            alouette_ui::ask::draw(frame, area, t, &q, &self.typed);
        }
    }

    fn on_paste(&mut self, text: &str, _cx: &mut Cx) -> Handled {
        if self.asker.question().is_some() {
            self.typed.push_str(text.trim_end_matches(['\r', '\n']));
        } else if self.focus == BODY {
            self.editor.insert(&text.replace("\r\n", "\n").replace('\r', "\n"));
            self.dirty = true;
        } else {
            let line = text.lines().collect::<Vec<_>>().join(" ");
            let f = &mut self.fields[self.focus];
            let v = format!("{}{line}", f.value);
            f.set(&v);
            self.dirty = true;
        }
        Handled::Yes
    }

    fn on_key(&mut self, key: KeyEvent, cx: &mut Cx) -> Handled {
        if let Some(q) = self.asker.question() {
            alouette_ui::ask::on_key(&self.asker, &q, &mut self.typed, key);
            return Handled::Yes;
        }
        if self.confirm_close {
            self.confirm_close = false;
            match key.code {
                KeyCode::Char('y' | 'Y') => cx.close(),
                KeyCode::Char('d' | 'D') => {
                    // Close once it is saved; see on_event.
                    self.closing = true;
                    self.start(Job::Draft, cx);
                    self.closing = self.busy.is_some();
                }
                _ => {}
            }
            return Handled::Yes;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char('s') if ctrl => {
                self.start(Job::Send, cx);
                return Handled::Yes;
            }
            KeyCode::Char('d') if ctrl => {
                self.start(Job::Draft, cx);
                return Handled::Yes;
            }
            KeyCode::Char('b') if ctrl => {
                self.focus = if self.fields[1].value.is_empty() && self.focus != 1 { 1 } else { 2 };
                return Handled::Yes;
            }
            KeyCode::Esc => {
                self.close(cx);
                return Handled::Yes;
            }
            KeyCode::Tab => {
                self.move_focus(true);
                return Handled::Yes;
            }
            KeyCode::BackTab => {
                self.move_focus(false);
                return Handled::Yes;
            }
            _ => {}
        }
        if self.note.as_deref().is_some_and(|n| n.starts_with("No subject")) {
            self.note = None;
        }
        if self.focus == BODY {
            if (ctrl || alt)
                && !matches!(
                    key.code,
                    KeyCode::Char('a' | 'e' | 'k' | 'u')
                        | KeyCode::Home
                        | KeyCode::End
                        | KeyCode::Left
                        | KeyCode::Right
                )
            {
                return Handled::No;
            }
            if matches!(key.code, KeyCode::F(_)) {
                return Handled::No;
            }
            if key.code == KeyCode::Up && self.editor.row == 0 {
                self.move_focus(false);
                return Handled::Yes;
            }
            let used = self.editor.on_key(key, self.body_area.width.max(1) as usize);
            if used
                && !matches!(
                    key.code,
                    KeyCode::Up
                        | KeyCode::Down
                        | KeyCode::Left
                        | KeyCode::Right
                        | KeyCode::Home
                        | KeyCode::End
                        | KeyCode::PageUp
                        | KeyCode::PageDown
                )
            {
                self.dirty = true;
            }
            return used.into();
        }
        match key.code {
            KeyCode::Enter | KeyCode::Down => {
                self.move_focus(true);
                Handled::Yes
            }
            KeyCode::Up => {
                self.move_focus(false);
                Handled::Yes
            }
            _ => match self.fields[self.focus].on_key(key) {
                InputResult::Edited => {
                    self.dirty = true;
                    Handled::Yes
                }
                InputResult::Ignored => Handled::No,
                _ => Handled::Yes,
            },
        }
    }

    fn on_mouse(&mut self, ev: MouseEvent, area: Rect, _cx: &mut Cx) -> Handled {
        if ev.kind != MouseEventKind::Down(MouseButton::Left) {
            return Handled::No;
        }
        let _ = area;
        if ev.row >= self.body_area.y && ev.row < self.body_area.bottom() {
            self.focus = BODY;
            return Handled::Yes;
        }
        let shown = self.shown();
        if ev.row >= self.field_top
            && let Some(&i) = shown.get((ev.row - self.field_top) as usize)
        {
            self.focus = i;
            return Handled::Yes;
        }
        Handled::No
    }

    fn on_event(&mut self, ev: &AppEvent, cx: &mut Cx) {
        let AppEvent::Module { module: MODULE, payload } = ev else { return };
        let Some(m) = payload.downcast_ref::<Msg>() else { return };
        if m.win != self.id {
            return;
        }
        match &m.kind {
            Kind::Note(n) => self.note = Some(n.clone()),
            Kind::Sent(r) => {
                let job = self.busy.take();
                match r {
                    Ok(n) => {
                        cx.notify(Notification::info(n.clone()));
                        self.dirty = false;
                        if job == Some(Job::Send) || self.closing {
                            cx.close();
                        } else {
                            self.note = Some(n.clone());
                        }
                    }
                    Err(e) => {
                        self.closing = false;
                        self.note = Some(e.clone());
                        cx.notify(Notification::error(e.clone()));
                    }
                }
            }
            _ => {}
        }
    }
}
