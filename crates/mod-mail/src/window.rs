//! The Mail window: folders on the left, messages on the right, and the open message
//! below them.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use alouette_core::config::MailAccount;
use alouette_core::event::EventSender;
use alouette_core::{AppEvent, Notification};
use alouette_mail::message::{Parsed, parse};
use alouette_mail::{Backend, Draft, Flags, Folder, Role, Summary};
use alouette_ui::ask::Asker;
use alouette_ui::input::{InputResult, LineInput};
use alouette_ui::window::{AiContext, Cx, DrawCx, Handled, Window};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::bridge::{self, send};
use crate::cache::Cache;
use crate::{MODULE, Shared};

static NEXT: AtomicU64 = AtomicU64::new(1);

pub fn next_id() -> u64 {
    NEXT.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone)]
pub struct Msg {
    pub win: u64,
    pub kind: Kind,
}

#[derive(Clone)]
pub enum Kind {
    Connected(Arc<dyn Backend>),
    Failed(String),
    Folders(Vec<Folder>),
    Listed {
        folder: String,
        list: Vec<Summary>,
        offline: bool,
    },
    Opened {
        folder: String,
        id: String,
        raw: Vec<u8>,
    },
    /// Something changed on the server; reload.
    Changed {
        folder: String,
    },
    Done(String),
    OpFailed(String),
    /// A note from sign-in, such as a code to type at a web page.
    Note(String),
    /// From a compose window.
    Sent(Result<String, String>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Folders,
    List,
    Reader,
}

enum State {
    Connecting,
    Ready,
    Failed(String),
}

enum Ask {
    MoveTo,
    Search,
}

struct Reader {
    folder: String,
    id: String,
    parsed: Parsed,
    scroll: u16,
    invite: Option<crate::invite::Invite>,
    /// The sender's name in the address book, if they are in it.
    known: Option<String>,
}

pub struct MailWindow {
    id: u64,
    data_dir: std::path::PathBuf,
    account: MailAccount,
    shared: Shared,
    cache: Option<Cache>,
    backend: Option<Arc<dyn Backend>>,
    state: State,
    folders: Vec<Folder>,
    fsel: usize,
    folder: Option<String>,
    list: Vec<Summary>,
    filter: Option<String>,
    lsel: usize,
    lscroll: usize,
    loading: bool,
    offline: bool,
    reader: Option<Reader>,
    focus: Focus,
    asker: Asker,
    typed: String,
    note: Option<String>,
    signin: Option<String>,
    confirm: Option<(String, Vec<String>)>,
    prompt: Option<(Ask, LineInput)>,
    alive: Arc<AtomicBool>,
    watch: Arc<AtomicU64>,
    poll: Duration,
    areas: [Rect; 3],
}

impl Drop for MailWindow {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Relaxed);
        self.asker.answer(None);
    }
}

fn short_date(t: Option<i64>, offset: time::UtcOffset) -> String {
    let Some(d) = t.and_then(|t| time::OffsetDateTime::from_unix_timestamp(t).ok()) else { return String::new() };
    let d = d.to_offset(offset);
    let now = time::OffsetDateTime::now_utc().to_offset(offset);
    if d.date() == now.date() {
        format!("{:02}:{:02}", d.hour(), d.minute())
    } else if d.year() == now.year() {
        format!("{} {:>2}", &format!("{:?}", d.month())[..3], d.day())
    } else {
        format!("{:04}-{:02}", d.year(), d.month() as u8)
    }
}

fn long_date(t: Option<i64>, offset: time::UtcOffset) -> String {
    let Some(d) = t.and_then(|t| time::OffsetDateTime::from_unix_timestamp(t).ok()) else { return String::new() };
    let d = d.to_offset(offset);
    format!("{:?}, {} {:?} {} {:02}:{:02}", d.weekday(), d.day(), d.month(), d.year(), d.hour(), d.minute())
}

use alouette_ui::chrome::fit;

impl MailWindow {
    pub fn new(
        account: MailAccount,
        shared: Shared,
        cache_dir: Option<std::path::PathBuf>,
        data_dir: std::path::PathBuf,
        cx: &mut Cx,
    ) -> Self {
        let id = next_id();
        let events = cx.tasks.events();
        let wake = events.clone();
        let asker = Asker::new(move || {
            let _ =
                wake.send(AppEvent::Module { module: MODULE, payload: alouette_core::event::ModulePayload::new(()) });
        });
        let cache = cache_dir.filter(|_| cx.config.mail.offline_cache).map(|d| Cache::new(d, &account.name));
        let mut w = Self {
            id,
            data_dir,
            account,
            shared,
            cache,
            backend: None,
            state: State::Connecting,
            folders: vec![],
            fsel: 0,
            folder: None,
            list: vec![],
            filter: None,
            lsel: 0,
            lscroll: 0,
            loading: false,
            offline: false,
            reader: None,
            focus: Focus::List,
            asker,
            typed: String::new(),
            note: None,
            signin: None,
            confirm: None,
            prompt: None,
            alive: Arc::new(AtomicBool::new(true)),
            watch: Arc::new(AtomicU64::new(0)),
            poll: Duration::from_secs(cx.config.mail.poll_secs.max(30)),
            areas: [Rect::default(); 3],
        };
        w.connect(cx);
        w
    }

    fn events(cx: &Cx) -> EventSender {
        cx.tasks.events()
    }

    fn connect(&mut self, cx: &mut Cx) {
        self.state = State::Connecting;
        if let Some(b) = self.shared.get(&self.account.name) {
            send(&Self::events(cx), self.id, Kind::Connected(b));
            return;
        }
        let (id, events) = (self.id, Self::events(cx));
        let net = cx.net.clone();
        let account = self.account.clone();
        let secrets: Arc<dyn alouette_mail::alouette_store_like::Secrets> =
            Arc::new(bridge::Secrets(cx.secrets.clone()));
        let prompt = Arc::new(bridge::Prompt { asker: self.asker.clone(), events: events.clone(), win: id });
        // Offline: show what the cache has while connecting.
        if let Some(c) = &self.cache
            && let Some(f) = c.folders()
        {
            send(&events, id, Kind::Folders(f));
        }
        tokio::spawn(async move {
            match alouette_mail::connect(net, &account, secrets, prompt).await {
                Ok(b) => send(&events, id, Kind::Connected(b)),
                Err(e) => send(&events, id, Kind::Failed(e)),
            }
        });
    }

    fn load_folders(&self, cx: &Cx) {
        let Some(b) = self.backend.clone() else { return };
        let (id, events) = (self.id, Self::events(cx));
        tokio::spawn(async move {
            match b.folders().await {
                Ok(f) => send(&events, id, Kind::Folders(f)),
                Err(e) => send(&events, id, Kind::OpFailed(e)),
            }
        });
    }

    fn load_list(&mut self, folder: String, cx: &Cx) {
        self.loading = true;
        let (id, events) = (self.id, Self::events(cx));
        let Some(b) = self.backend.clone() else {
            if let Some(l) = self.cache.as_ref().and_then(|c| c.list(&folder)) {
                send(&events, id, Kind::Listed { folder, list: l, offline: true });
            }
            return;
        };
        let cache = self.cache.clone();
        tokio::spawn(async move {
            match b.list(&folder, 300).await {
                Ok(l) => {
                    if let Some(c) = &cache {
                        c.put_list(&folder, &l);
                    }
                    send(&events, id, Kind::Listed { folder, list: l, offline: false });
                }
                Err(e) => match cache.and_then(|c| c.list(&folder)) {
                    Some(l) => send(&events, id, Kind::Listed { folder, list: l, offline: true }),
                    None => send(&events, id, Kind::OpFailed(e)),
                },
            }
        });
    }

    /// Watch the open folder for news (IMAP IDLE, or a poll).
    fn watch(&self, folder: String, cx: &Cx) {
        let Some(b) = self.backend.clone() else { return };
        let gen_ = self.watch.fetch_add(1, Ordering::Relaxed) + 1;
        let (watch, alive, poll) = (self.watch.clone(), self.alive.clone(), self.poll);
        let (id, events) = (self.id, Self::events(cx));
        tokio::spawn(async move {
            while alive.load(Ordering::Relaxed) && watch.load(Ordering::Relaxed) == gen_ {
                match b.wait(&folder, Duration::from_secs(25 * 60)).await {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(_) => tokio::time::sleep(poll).await,
                }
                if alive.load(Ordering::Relaxed) && watch.load(Ordering::Relaxed) == gen_ {
                    send(&events, id, Kind::Changed { folder: folder.clone() });
                }
            }
        });
    }

    fn open_folder(&mut self, i: usize, cx: &Cx) {
        let Some(f) = self.folders.get(i) else { return };
        let fid = f.id.clone();
        self.fsel = i;
        if self.folder.as_deref() != Some(&fid) {
            self.list.clear();
            self.lsel = 0;
            self.lscroll = 0;
            self.reader = None;
            self.filter = None;
        }
        self.folder = Some(fid.clone());
        self.load_list(fid.clone(), cx);
        self.watch(fid, cx);
    }

    fn visible(&self) -> Vec<usize> {
        let f = self.filter.as_ref().map(|f| f.to_lowercase());
        self.list
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                f.as_ref().is_none_or(|f| {
                    s.subject.to_lowercase().contains(f)
                        || s.from.to_lowercase().contains(f)
                        || s.from_addr.to_lowercase().contains(f)
                })
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn current(&self) -> Option<&Summary> {
        let v = self.visible();
        v.get(self.lsel).and_then(|&i| self.list.get(i))
    }

    fn open_message(&mut self, cx: &Cx) {
        let (Some(folder), Some(s)) = (self.folder.clone(), self.current().cloned()) else { return };
        let (id, events) = (self.id, Self::events(cx));
        if let Some(raw) = self.cache.as_ref().and_then(|c| c.message(&folder, &s.id))
            && self.backend.is_none()
        {
            send(&events, id, Kind::Opened { folder, id: s.id, raw });
            return;
        }
        let Some(b) = self.backend.clone() else { return };
        let cache = self.cache.clone();
        self.note = Some("Opening…".into());
        tokio::spawn(async move {
            match b.fetch(&folder, &s.id).await {
                Ok(raw) => {
                    if let Some(c) = &cache {
                        c.put_message(&folder, &s.id, &raw);
                    }
                    send(&events, id, Kind::Opened { folder, id: s.id, raw });
                }
                Err(e) => send(&events, id, Kind::OpFailed(e)),
            }
        });
    }

    /// Run an operation on the server in the background, then reload.
    fn op(
        &mut self,
        cx: &mut Cx,
        title: String,
        f: impl FnOnce(Arc<dyn Backend>) -> alouette_mail::BoxFut<'static, String> + Send + 'static,
    ) {
        let Some(b) = self.backend.clone() else {
            self.note = Some("Not connected".into());
            return;
        };
        let (id, events) = (self.id, Self::events(cx));
        let once = std::sync::Mutex::new(Some(f));
        cx.tasks.spawn(MODULE, title, move |_| {
            let f = once.lock().unwrap_or_else(|e| e.into_inner()).take();
            let (b, events) = (b.clone(), events.clone());
            async move {
                let Some(f) = f else { return Err("already done".to_string()) };
                match f(b).await {
                    Ok(n) => {
                        send(&events, id, Kind::Done(n));
                        Ok(())
                    }
                    Err(e) => {
                        send(&events, id, Kind::OpFailed(e.clone()));
                        Err(e)
                    }
                }
            }
        });
    }

    fn role_folder(&self, r: Role) -> Option<String> {
        self.folders.iter().find(|f| f.role == r).map(|f| f.id.clone())
    }

    fn folder_role(&self) -> Role {
        self.folders.iter().find(|f| Some(&f.id) == self.folder.as_ref()).map(|f| f.role).unwrap_or(Role::Other)
    }

    /// The selected message's id, as a list for operations.
    fn chosen(&self) -> Vec<String> {
        self.current().map(|s| vec![s.id.clone()]).unwrap_or_default()
    }

    fn delete(&mut self, cx: &mut Cx) {
        let ids = self.chosen();
        if ids.is_empty() {
            return;
        }
        let Some(folder) = self.folder.clone() else { return };
        match self.role_folder(Role::Trash) {
            Some(trash) if self.folder_role() != Role::Trash => {
                self.remove_local(&ids);
                self.op(cx, "Move to Trash".into(), move |b| {
                    Box::pin(async move {
                        b.move_to(&folder, &ids, &trash).await?;
                        Ok("Moved to Trash".into())
                    })
                })
            }
            _ => {
                let subject = self.current().map(|s| s.subject.clone()).unwrap_or_default();
                self.confirm = Some((format!("Delete \"{subject}\" for good?"), ids));
            }
        }
    }

    fn remove_local(&mut self, ids: &[String]) {
        self.list.retain(|s| !ids.contains(&s.id));
        if self.reader.as_ref().is_some_and(|r| ids.contains(&r.id)) {
            self.reader = None;
        }
        self.lsel = self.lsel.min(self.visible().len().saturating_sub(1));
    }

    fn move_selected(&mut self, to: String, label: &str, cx: &mut Cx) {
        let ids = self.chosen();
        let Some(folder) = self.folder.clone() else { return };
        if ids.is_empty() || to == folder {
            return;
        }
        self.remove_local(&ids);
        let note = format!("Moved to {label}");
        self.op(cx, format!("Move to {label}"), move |b| {
            Box::pin(async move {
                b.move_to(&folder, &ids, &to).await?;
                Ok(note)
            })
        });
    }

    fn toggle(&mut self, which: Flags, cx: &mut Cx) {
        let Some(folder) = self.folder.clone() else { return };
        let v = self.visible();
        let Some(&i) = v.get(self.lsel) else { return };
        let s = &mut self.list[i];
        let on = if which.seen { !s.flags.seen } else { !s.flags.flagged };
        if which.seen {
            s.flags.seen = on;
        } else {
            s.flags.flagged = on;
        }
        let ids = vec![s.id.clone()];
        let Some(b) = self.backend.clone() else { return };
        let (id, events) = (self.id, Self::events(cx));
        tokio::spawn(async move {
            if let Err(e) = b.set_flags(&folder, &ids, which, on).await {
                send(&events, id, Kind::OpFailed(e));
            }
        });
    }

    fn compose(&mut self, draft: Draft, reply_of: Option<(String, String)>, cx: &mut Cx) {
        let token = self.shared.stash(draft, reply_of, Some(self.id));
        cx.open_url(format!(
            "mail:compose/{}/{token}",
            url::form_urlencoded::byte_serialize(self.account.name.as_bytes()).collect::<String>()
        ));
    }

    fn reply(&mut self, all: bool, forward: bool, cx: &mut Cx) {
        let Some(r) = &self.reader else {
            self.note = Some("Open a message first".into());
            return;
        };
        let d = if forward { Draft::forward(&r.parsed) } else { Draft::reply(&r.parsed, all, &self.account.email) };
        let of = (!forward).then(|| (r.folder.clone(), r.id.clone()));
        self.compose(d, of, cx);
    }

    fn answer_invite(&mut self, partstat: &str, cx: &mut Cx) {
        let Some(r) = &self.reader else { return };
        let Some(inv) = r.invite.clone() else { return };
        let message_id = r.parsed.message_id.clone();
        let res = crate::invite::answer(
            &inv,
            partstat,
            &self.account.email,
            self.account.display_name.as_deref(),
            &self.data_dir,
            cx.config,
        );
        match res {
            Ok(a) => {
                calendar_changed(&cx.tasks.events());
                if let Some(r) = &mut self.reader
                    && let Some(i) = &mut r.invite
                {
                    i.answered = (partstat != "DECLINED" && i.method != alouette_pim::itip::Method::Cancel)
                        .then(|| partstat.to_string());
                }
                if let Some((to, subject, body, ics)) = a.reply {
                    let d = Draft {
                        to,
                        subject,
                        body,
                        in_reply_to: message_id.clone(),
                        references: message_id.into_iter().collect(),
                        calendar: Some(("REPLY".into(), ics)),
                        ..Default::default()
                    };
                    self.compose(d, None, cx);
                }
                self.note = Some(a.note.clone());
                cx.notify(Notification::success(a.note));
            }
            Err(e) => self.note = Some(e),
        }
    }

    fn submit_prompt(&mut self, cx: &mut Cx) {
        let Some((ask, input)) = self.prompt.take() else { return };
        let v = input.value.trim().to_string();
        match ask {
            Ask::Search => {
                self.filter = (!v.is_empty()).then_some(v);
                self.lsel = 0;
                self.lscroll = 0;
            }
            Ask::MoveTo => {
                let lower = v.to_lowercase();
                let hits: Vec<&Folder> =
                    self.folders.iter().filter(|f| f.name.to_lowercase().contains(&lower)).collect();
                let exact = self.folders.iter().find(|f| f.name.to_lowercase() == lower);
                match exact.or(if hits.len() == 1 { hits.first().copied() } else { None }) {
                    Some(f) => {
                        let (fid, name) = (f.id.clone(), f.name.clone());
                        self.move_selected(fid, &name, cx);
                    }
                    None if hits.is_empty() => self.note = Some(format!("No folder called {v}")),
                    None => {
                        self.note = Some(format!(
                            "{v} matches {}",
                            hits.iter().map(|f| f.name.as_str()).collect::<Vec<_>>().join(", ")
                        ))
                    }
                }
            }
        }
    }

    fn move_sel(&mut self, d: isize) {
        match self.focus {
            Focus::Folders => {
                if !self.folders.is_empty() {
                    self.fsel = (self.fsel as isize + d).clamp(0, self.folders.len() as isize - 1) as usize;
                }
            }
            Focus::List => {
                let n = self.visible().len();
                if n > 0 {
                    self.lsel = (self.lsel as isize + d).clamp(0, n as isize - 1) as usize;
                }
            }
            Focus::Reader => {
                if let Some(r) = &mut self.reader {
                    r.scroll = (r.scroll as isize + d).max(0) as u16;
                }
            }
        }
    }

    fn draw_folders(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        let focused = self.focus == Focus::Folders && cx.focused;
        let block = Block::default().borders(Borders::RIGHT).border_style(t.dim());
        let inner = block.inner(area);
        frame.render_widget(block, area);
        self.areas[0] = inner;
        let w = inner.width as usize;
        let lines: Vec<Line> = self
            .folders
            .iter()
            .enumerate()
            .skip(self.fsel.saturating_sub(inner.height.saturating_sub(1) as usize))
            .take(inner.height as usize)
            .map(|(i, f)| {
                let depth = f.name.matches('/').count();
                let leaf = f.name.rsplit('/').next().unwrap_or(&f.name);
                let label = if f.role == Role::Inbox { "Inbox" } else { leaf };
                let count = if f.unread > 0 { format!(" {}", f.unread) } else { String::new() };
                let name_w = w.saturating_sub(count.width() + depth * 2 + 2);
                let text = format!(
                    "{}{} {}{}",
                    "  ".repeat(depth),
                    if Some(&f.id) == self.folder.as_ref() { "▸" } else { " " },
                    fit(label, name_w),
                    count
                );
                let mut st = if f.unread > 0 { Style::new().add_modifier(Modifier::BOLD) } else { Style::new() };
                if i == self.fsel && focused {
                    st = t.selection();
                }
                Line::from(Span::styled(fit(&text, w), st))
            })
            .collect();
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn draw_list(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        self.areas[1] = area;
        let focused = self.focus == Focus::List && cx.focused;
        let h = area.height as usize;
        let v = self.visible();
        if v.is_empty() {
            let msg = match (&self.state, self.loading, &self.filter) {
                (State::Connecting, _, _) => "Connecting…".to_string(),
                (State::Failed(e), _, _) => format!("Could not connect: {e}\n\nPress g to try again."),
                (_, true, _) => "Loading…".into(),
                (_, _, Some(f)) => format!("Nothing matches \"{f}\". Press / and Enter to clear."),
                _ if self.folder.is_none() => "Choose a folder.".into(),
                _ => "No messages here.".into(),
            };
            frame.render_widget(Paragraph::new(msg).style(t.dim()).wrap(Wrap { trim: false }), area);
            return;
        }
        if self.lsel < self.lscroll {
            self.lscroll = self.lsel;
        }
        if h > 0 && self.lsel >= self.lscroll + h {
            self.lscroll = self.lsel + 1 - h;
        }
        let w = area.width as usize;
        let from_w = (w / 4).clamp(8, 24);
        let lines: Vec<Line> = v
            .iter()
            .enumerate()
            .skip(self.lscroll)
            .take(h)
            .map(|(row, &i)| {
                let s = &self.list[i];
                let mark = if !s.flags.seen {
                    "●"
                } else if s.flags.answered {
                    "↩"
                } else {
                    " "
                };
                let star = if s.flags.flagged { "★" } else { " " };
                let date = short_date(s.date, cx.local_offset);
                let subj_w = w.saturating_sub(2 + 1 + 6 + 2 + from_w + 2);
                let text = format!("{mark}{star} {:>6}  {}  {}", date, fit(&s.from, from_w), fit(&s.subject, subj_w));
                let mut st = if !s.flags.seen { Style::new().add_modifier(Modifier::BOLD) } else { Style::new() };
                if row == self.lsel {
                    st = if focused { t.selection() } else { st.add_modifier(Modifier::REVERSED) };
                }
                Line::from(Span::styled(fit(&text, w), st))
            })
            .collect();
        frame.render_widget(Paragraph::new(lines), area);
    }

    fn draw_reader(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        let block = Block::default().borders(Borders::TOP).border_style(if self.focus == Focus::Reader {
            t.accent()
        } else {
            t.dim()
        });
        let inner = block.inner(area);
        frame.render_widget(block, area);
        self.areas[2] = inner;
        if let Some(note) = &self.signin {
            let p = Paragraph::new(vec![
                Line::from(Span::styled("Sign in", t.accent())),
                Line::default(),
                Line::from(note.clone()),
                Line::default(),
                Line::from(Span::styled("Alouette carries on by itself once you have signed in.", t.dim())),
            ])
            .wrap(Wrap { trim: false });
            frame.render_widget(p, inner);
            return;
        }
        let Some(r) = &mut self.reader else { return };
        let p = &r.parsed;
        let head =
            |k: &str, v: &str| Line::from(vec![Span::styled(format!("{k:<9}"), t.accent()), Span::raw(v.to_string())]);
        let from = match &r.known {
            Some(n) => Line::from(vec![
                Span::styled(format!("{:<9}", "From"), t.accent()),
                Span::raw(format!("{} <{}>", p.from, p.from_addr)),
                Span::styled(format!("  · {n} in your contacts"), t.dim()),
            ]),
            None => head("From", &format!("{} <{}>", p.from, p.from_addr)),
        };
        let mut lines = vec![from, head("To", &p.to)];
        if !p.cc.is_empty() {
            lines.push(head("Cc", &p.cc));
        }
        lines.push(head("Date", &long_date(p.date, cx.local_offset)));
        lines.push(Line::from(vec![
            Span::styled(format!("{:<9}", "Subject"), t.accent()),
            Span::styled(p.subject.clone(), Style::new().add_modifier(Modifier::BOLD)),
        ]));
        if !p.attachments.is_empty() {
            let a: Vec<String> =
                p.attachments.iter().map(|a| format!("{} ({})", a.name, alouette_ui_size(a.size))).collect();
            lines.push(head("Attached", &a.join(", ")));
        }
        if p.from_html {
            lines.push(Line::from(Span::styled("Shown as text from HTML; remote images were not loaded.", t.dim())));
        }
        if let Some(inv) = &r.invite {
            lines.push(Line::default());
            for (i, l) in inv.lines.iter().enumerate() {
                let st = if i == 0 { t.accent().add_modifier(Modifier::BOLD) } else { Style::new() };
                lines.push(Line::from(vec![Span::styled("▌ ", t.accent()), Span::styled(l.clone(), st)]));
            }
            let h = crate::invite::hint(inv);
            if !h.is_empty() {
                lines.push(Line::from(vec![Span::styled("▌ ", t.accent()), Span::styled(h, t.dim())]));
            }
        }
        lines.push(Line::default());
        for l in p.text.lines() {
            let st = if l.starts_with('>') { t.dim() } else { Style::new() };
            lines.push(Line::from(Span::styled(l.to_string(), st)));
        }
        let para = Paragraph::new(lines).wrap(Wrap { trim: false });
        let total = para.line_count(inner.width) as u16;
        r.scroll = r.scroll.min(total.saturating_sub(inner.height.max(1)));
        frame.render_widget(para.scroll((r.scroll, 0)), inner);
    }
}

/// Tell open Calendar windows that an invitation changed a calendar.
fn calendar_changed(events: &EventSender) {
    let _ = events.send(AppEvent::Module {
        module: "calendar",
        payload: alouette_core::event::ModulePayload::new(alouette_pim::Changed),
    });
}

fn alouette_ui_size(n: usize) -> String {
    match n {
        0..1024 => format!("{n} B"),
        1024..1_048_576 => format!("{} KB", n / 1024),
        _ => format!("{:.1} MB", n as f64 / 1_048_576.0),
    }
}

impl Window for MailWindow {
    fn title(&self) -> String {
        let f = self
            .folders
            .iter()
            .find(|f| Some(&f.id) == self.folder.as_ref())
            .map(|f| if f.role == Role::Inbox { "Inbox".to_string() } else { f.name.clone() });
        match f {
            Some(f) => format!("Mail: {} · {f}", self.account.name),
            None => format!("Mail: {}", self.account.name),
        }
    }
    fn module(&self) -> &'static str {
        MODULE
    }
    fn url(&self) -> Option<String> {
        Some(format!("mail:{}", self.account.name))
    }
    fn ai_context(&self) -> Option<AiContext> {
        if let Some(r) = &self.reader {
            let p = &r.parsed;
            let d = Draft::reply(p, false, &self.account.email);
            let text =
                format!("From: {} <{}>\nTo: {}\nSubject: {}\n\n{}", p.from, p.from_addr, p.to, p.subject, p.text);
            return Some(AiContext {
                module: MODULE.into(),
                kind: "email".into(),
                title: p.subject.clone(),
                text,
                list: false,
                reply_url: Some(crate::draft_url(&d)),
            });
        }
        if self.list.is_empty() {
            return None;
        }
        let off = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
        let text = self
            .list
            .iter()
            .take(200)
            .map(|m| {
                format!(
                    "{}{} | {} | {}",
                    if m.flags.seen { "" } else { "[unread] " },
                    short_date(m.date, off),
                    if m.from.is_empty() { &m.from_addr } else { &m.from },
                    m.subject
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        Some(AiContext {
            module: MODULE.into(),
            kind: "mailbox".into(),
            title: self.title(),
            text,
            list: true,
            reply_url: None,
        })
    }
    fn badge(&self) -> Option<String> {
        if self.offline {
            return Some("offline · cached".into());
        }
        self.backend.as_ref().map(|b| b.badge())
    }
    fn wants_text(&self) -> bool {
        self.prompt.is_some() || self.asker.question().is_some()
    }
    fn hints(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("Enter", "Open"),
            ("c", "Write"),
            ("r/a/f", "Reply/All/Fwd"),
            ("d", "Delete"),
            ("s", "Star"),
            ("m", "Move"),
            ("/", "Find"),
            ("+", "Add sender"),
            ("Tab", "Pane"),
        ]
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        if area.height < 4 || area.width < 20 {
            return;
        }
        let foot = Rect { y: area.bottom() - 1, height: 1, ..area };
        let body = Rect { height: area.height - 1, ..area };
        let fw = (body.width / 4).clamp(12, 26);
        let left = Rect { width: fw, ..body };
        let right = Rect { x: body.x + fw + 1, width: body.width.saturating_sub(fw + 1), ..body };
        self.draw_folders(frame, left, cx);
        let show_reader = self.reader.is_some() || self.signin.is_some();
        if show_reader {
            let lh = (right.height * 2 / 5).max(3);
            self.draw_list(frame, Rect { height: lh, ..right }, cx);
            self.draw_reader(frame, Rect { y: right.y + lh, height: right.height - lh, ..right }, cx);
        } else {
            self.draw_list(frame, right, cx);
        }
        let t = cx.theme;
        if let Some((ask, input)) = &self.prompt {
            let label = match ask {
                Ask::MoveTo => " Move to folder: ",
                Ask::Search => " Find (subject or sender): ",
            };
            let field = Style::new().fg(t.input_fg).bg(t.input_bg);
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(label, t.accent()),
                    Span::styled(input.value.clone(), field),
                ])),
                foot,
            );
            frame.set_cursor_position((foot.x + label.width() as u16 + input.cursor_col() as u16, foot.y));
        } else if let Some((q, _)) = &self.confirm {
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(format!(" {q} "), Style::new().fg(t.error).add_modifier(Modifier::BOLD)),
                    Span::styled("y", t.accent()),
                    Span::raw(" yes   "),
                    Span::styled("n", t.accent()),
                    Span::raw(" no"),
                ])),
                foot,
            );
        } else {
            let mut s = self.note.clone().unwrap_or_default();
            if let Some(f) = &self.filter {
                s = format!("Showing \"{f}\" ({} of {}). {s}", self.visible().len(), self.list.len());
            }
            frame.render_widget(Paragraph::new(Span::styled(format!(" {s}"), t.dim())), foot);
        }
        if let Some(q) = self.asker.question() {
            alouette_ui::ask::draw(frame, area, t, &q, &self.typed);
        }
    }

    fn on_paste(&mut self, text: &str, _cx: &mut Cx) -> Handled {
        if self.asker.question().is_some() {
            self.typed.push_str(text.trim_end_matches(['\r', '\n']));
            return Handled::Yes;
        }
        Handled::No
    }

    fn on_key(&mut self, key: KeyEvent, cx: &mut Cx) -> Handled {
        if let Some(q) = self.asker.question() {
            alouette_ui::ask::on_key(&self.asker, &q, &mut self.typed, key);
            return Handled::Yes;
        }
        if let Some((_, input)) = &mut self.prompt {
            match input.on_key(key) {
                InputResult::Submit => self.submit_prompt(cx),
                InputResult::Cancel => self.prompt = None,
                _ => {}
            }
            return Handled::Yes;
        }
        if let Some((_, ids)) = self.confirm.take() {
            if matches!(key.code, KeyCode::Char('y' | 'Y'))
                && let Some(folder) = self.folder.clone()
            {
                self.remove_local(&ids);
                self.op(cx, "Delete".into(), move |b| {
                    Box::pin(async move {
                        b.expunge(&folder, &ids).await?;
                        Ok("Deleted".into())
                    })
                });
            }
            return Handled::Yes;
        }
        if key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
            return Handled::No;
        }
        self.note = None;
        match key.code {
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Folders => Focus::List,
                    Focus::List if self.reader.is_some() => Focus::Reader,
                    _ => Focus::Folders,
                }
            }
            KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Reader => Focus::List,
                    Focus::List => Focus::Folders,
                    Focus::Folders if self.reader.is_some() => Focus::Reader,
                    Focus::Folders => Focus::List,
                }
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
            KeyCode::PageUp => self.move_sel(-10),
            KeyCode::PageDown => self.move_sel(10),
            KeyCode::Home => self.move_sel(-100_000),
            KeyCode::End => self.move_sel(100_000),
            KeyCode::Char(' ') => {
                // Space pages the open message, then moves on to the next one, as in pine.
                if let Some(r) = &mut self.reader {
                    r.scroll += 10;
                } else {
                    self.open_message(cx);
                }
            }
            KeyCode::Enter | KeyCode::Right => match self.focus {
                Focus::Folders => {
                    self.open_folder(self.fsel, cx);
                    self.focus = Focus::List;
                }
                Focus::List => self.open_message(cx),
                Focus::Reader => {}
            },
            KeyCode::Esc | KeyCode::Left => {
                if self.reader.take().is_some() || self.signin.take().is_some() {
                    self.focus = Focus::List;
                } else if self.focus == Focus::List && key.code == KeyCode::Left {
                    self.focus = Focus::Folders;
                } else if self.filter.take().is_none() {
                    return Handled::No;
                }
            }
            KeyCode::Char('c') | KeyCode::Char('w') => {
                let sig = self.account.signature.clone().map(|s| format!("\n\n-- \n{s}")).unwrap_or_default();
                self.compose(Draft { body: sig, ..Default::default() }, None, cx);
            }
            KeyCode::Char('Y') if self.reader.as_ref().is_some_and(|r| r.invite.is_some()) => {
                self.answer_invite("ACCEPTED", cx)
            }
            KeyCode::Char('N') if self.reader.as_ref().is_some_and(|r| r.invite.is_some()) => {
                self.answer_invite("DECLINED", cx)
            }
            KeyCode::Char('T') if self.reader.as_ref().is_some_and(|r| r.invite.is_some()) => {
                self.answer_invite("TENTATIVE", cx)
            }
            KeyCode::Char('+') => match &self.reader {
                Some(r) => cx.open_url(mod_contacts::people::add_url(&r.parsed.from, &r.parsed.from_addr)),
                None => self.note = Some("Open a message to add its sender to your contacts".into()),
            },
            KeyCode::Char('r') => self.reply(false, false, cx),
            KeyCode::Char('a') | KeyCode::Char('R') => self.reply(true, false, cx),
            KeyCode::Char('f') => self.reply(false, true, cx),
            KeyCode::Char('d') | KeyCode::Delete => self.delete(cx),
            KeyCode::Char('s') | KeyCode::Char('*') => self.toggle(Flags { flagged: true, ..Default::default() }, cx),
            KeyCode::Char('u') => self.toggle(Flags { seen: true, ..Default::default() }, cx),
            KeyCode::Char('A') | KeyCode::Char('e') => {
                match self.role_folder(Role::Archive).or_else(|| self.role_folder(Role::All)) {
                    Some(a) if self.folder_role() == Role::Inbox => self.move_selected(a, "Archive", cx),
                    Some(_) => self.note = Some("Archive moves messages out of the Inbox".into()),
                    None => self.note = Some("This account has no Archive folder; press m to move".into()),
                }
            }
            KeyCode::Char('m') => {
                if !self.chosen().is_empty() {
                    self.prompt = Some((Ask::MoveTo, LineInput::default()));
                }
            }
            KeyCode::Char('/') => {
                self.prompt = Some((Ask::Search, LineInput::new(self.filter.as_deref().unwrap_or(""))))
            }
            KeyCode::Char('g') => {
                if matches!(self.state, State::Failed(_)) {
                    self.connect(cx);
                } else {
                    self.load_folders(cx);
                    if let Some(f) = self.folder.clone() {
                        self.load_list(f, cx);
                    }
                }
            }
            _ => return Handled::No,
        }
        Handled::Yes
    }

    fn on_mouse(&mut self, ev: MouseEvent, _area: Rect, cx: &mut Cx) -> Handled {
        let inside = |r: Rect| ev.column >= r.x && ev.column < r.right() && ev.row >= r.y && ev.row < r.bottom();
        match ev.kind {
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let d = if ev.kind == MouseEventKind::ScrollDown { 3 } else { -3 };
                let was = self.focus;
                if inside(self.areas[2]) && self.reader.is_some() {
                    self.focus = Focus::Reader;
                } else if inside(self.areas[0]) {
                    self.focus = Focus::Folders;
                } else {
                    self.focus = Focus::List;
                }
                self.move_sel(d);
                self.focus = was;
                Handled::Yes
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if inside(self.areas[0]) {
                    let skip = self.fsel.saturating_sub(self.areas[0].height.saturating_sub(1) as usize);
                    let i = skip + (ev.row - self.areas[0].y) as usize;
                    if i < self.folders.len() {
                        self.focus = Focus::Folders;
                        self.open_folder(i, cx);
                    }
                } else if inside(self.areas[1]) {
                    let i = self.lscroll + (ev.row - self.areas[1].y) as usize;
                    if i < self.visible().len() {
                        let again = self.lsel == i && self.focus == Focus::List;
                        self.focus = Focus::List;
                        self.lsel = i;
                        if again || self.reader.is_some() {
                            self.open_message(cx);
                        }
                    }
                } else if inside(self.areas[2]) {
                    self.focus = Focus::Reader;
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }

    fn on_event(&mut self, ev: &AppEvent, cx: &mut Cx) {
        let AppEvent::Module { module: MODULE, payload } = ev else { return };
        let Some(m) = payload.downcast_ref::<Msg>() else { return };
        if m.win != self.id {
            return;
        }
        match m.kind.clone() {
            Kind::Connected(b) => {
                self.shared.put(&self.account.name, b.clone());
                self.backend = Some(b);
                self.state = State::Ready;
                self.signin = None;
                self.offline = false;
                self.load_folders(cx);
            }
            Kind::Failed(e) => {
                self.offline = self.cache.as_ref().is_some_and(|c| c.folders().is_some());
                self.state = State::Failed(e.clone());
                self.signin = None;
                self.note =
                    Some(if self.offline { format!("Offline ({e}); showing saved mail. g retries.") } else { e });
            }
            Kind::Folders(f) => {
                if let Some(c) = &self.cache
                    && self.backend.is_some()
                {
                    c.put_folders(&f);
                }
                let first = self.folders.is_empty();
                self.folders = f;
                self.fsel = self.fsel.min(self.folders.len().saturating_sub(1));
                if first || self.folder.is_none() {
                    let inbox = self.folders.iter().position(|f| f.role == Role::Inbox).unwrap_or(0);
                    if !self.folders.is_empty() {
                        self.open_folder(inbox, cx);
                    }
                }
            }
            Kind::Listed { folder, list, offline } => {
                if self.folder.as_deref() != Some(&folder) {
                    return;
                }
                let keep = self.current().map(|s| s.id.clone());
                self.loading = false;
                self.offline = offline;
                self.list = list;
                let v = self.visible();
                self.lsel = keep
                    .and_then(|k| v.iter().position(|&i| self.list[i].id == k))
                    .unwrap_or(0)
                    .min(v.len().saturating_sub(1));
                // Unread counts may have changed.
                let unread = self.list.iter().filter(|s| !s.flags.seen).count() as u32;
                if let Some(f) = self.folders.iter_mut().find(|f| f.id == folder)
                    && self.list.len() as u32 >= f.total
                {
                    f.unread = unread;
                }
            }
            Kind::Opened { folder, id, raw } => {
                self.note = None;
                if let Some(s) = self.list.iter_mut().find(|s| s.id == id) {
                    if !s.flags.seen
                        && let Some(f) = self.folders.iter_mut().find(|f| f.id == folder)
                    {
                        f.unread = f.unread.saturating_sub(1);
                    }
                    s.flags.seen = true;
                }
                let parsed = parse(&raw);
                let invite = parsed.calendar.as_deref().and_then(|c| crate::invite::read(c, &self.data_dir, cx.config));
                if let Some(inv) = invite.as_ref().filter(|i| i.method == alouette_pim::itip::Method::Reply)
                    && let Some(n) = crate::invite::apply_reply(inv, &self.data_dir)
                {
                    self.note = Some(n);
                    calendar_changed(&cx.tasks.events());
                }
                let known = mod_contacts::people::by_email(&self.data_dir, &parsed.from_addr).map(|c| c.name);
                self.reader = Some(Reader { folder, id, parsed, scroll: 0, invite, known });
            }
            Kind::Changed { folder } => {
                if self.folder.as_deref() == Some(&folder) {
                    self.load_list(folder, cx);
                    self.load_folders(cx);
                }
            }
            Kind::Done(n) => {
                self.note = Some(n);
                self.load_folders(cx);
                if let Some(f) = self.folder.clone() {
                    self.load_list(f, cx);
                }
            }
            Kind::OpFailed(e) => {
                self.loading = false;
                self.note = Some(e.clone());
                cx.notify(Notification::error(e));
            }
            Kind::Note(n) => self.signin = Some(n),
            Kind::Sent(r) => match r {
                Ok(n) => {
                    self.note = Some(n);
                    if let Some(f) = self.folder.clone() {
                        self.load_list(f, cx);
                    }
                }
                Err(e) => self.note = Some(e),
            },
        }
    }
}
