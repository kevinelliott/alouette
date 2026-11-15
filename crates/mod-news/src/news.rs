//! The News window: your groups on the left, threads on the right, the article below.

use std::collections::HashMap;
use std::sync::Arc;

use alouette_core::config::NewsServer;
use alouette_core::event::{EventSender, ModulePayload};
use alouette_core::{AppEvent, Notification};
use alouette_mail::message::{Parsed, parse};
use alouette_ui::ask::Asker;
use alouette_ui::input::{InputResult, LineInput};
use alouette_ui::window::{AiContext, Cx, DrawCx, Effect, Handled, Window};
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use proto_nntp::{Group, Overview, Selected};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::conn::Conn;
use crate::post::{PostWindow, Seed};
use crate::readstate::ReadState;
use crate::thread::{Row, thread};
use crate::util::{fit, long_date, short_date, who};
use crate::{NEWS, next_id};

#[derive(Clone)]
pub struct Msg {
    pub win: u64,
    pub kind: Kind,
}

#[derive(Clone)]
pub enum Kind {
    Connected(bool),
    Failed(String),
    Status { group: String, sel: Selected },
    Listed { group: String, sel: Selected, over: Vec<Overview> },
    Opened { group: String, number: u64, raw: Vec<u8> },
    All(Vec<Group>, Vec<(String, String)>),
    OpFailed(String),
    Posted(Result<String, String>),
}

pub fn send(events: &EventSender, win: u64, kind: Kind) {
    let _ = events.send(AppEvent::Module { module: NEWS, payload: ModulePayload::new(Msg { win, kind }) });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Groups,
    List,
    Reader,
}

struct GroupRow {
    name: String,
    sel: Option<Selected>,
}

struct Reader {
    number: u64,
    parsed: Parsed,
    scroll: u16,
    lines: usize,
}

/// The list of every group on the server, to subscribe from.
struct AllGroups {
    groups: Vec<(Group, String)>,
    filter: LineInput,
    sel: usize,
    scroll: usize,
}

impl AllGroups {
    fn visible(&self) -> Vec<usize> {
        let f = self.filter.value.to_lowercase();
        self.groups
            .iter()
            .enumerate()
            .filter(|(_, (g, d))| f.is_empty() || g.name.to_lowercase().contains(&f) || d.to_lowercase().contains(&f))
            .map(|(i, _)| i)
            .collect()
    }
}

pub struct NewsWindow {
    id: u64,
    server: NewsServer,
    conn: Arc<Conn>,
    asker: Asker,
    typed: String,
    connected: Option<Result<bool, String>>,
    groups: Vec<GroupRow>,
    gsel: usize,
    group: Option<String>,
    over: Vec<Overview>,
    rows: Vec<Row>,
    lsel: usize,
    lscroll: usize,
    loading: bool,
    read: HashMap<String, ReadState>,
    reader: Option<Reader>,
    all: Option<AllGroups>,
    focus: Focus,
    note: Option<String>,
    max: u64,
    areas: [Rect; 3],
}

impl Drop for NewsWindow {
    fn drop(&mut self) {
        self.asker.answer(None);
        let c = self.conn.clone();
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move { c.close().await });
        }
    }
}

fn read_key(server: &str, group: &str) -> String {
    format!("news/read/{server}/{group}")
}

impl NewsWindow {
    pub fn new(server: NewsServer, cx: &mut Cx) -> Self {
        let wake = cx.tasks.events();
        let asker = Asker::new(move || {
            let _ = wake.send(AppEvent::Module { module: NEWS, payload: ModulePayload::new(()) });
        });
        let conn = Arc::new(Conn::new(cx.net.clone(), server.clone(), cx.secrets.clone(), asker.clone()));
        let mut w = Self {
            id: next_id(),
            groups: server.groups.iter().map(|g| GroupRow { name: g.clone(), sel: None }).collect(),
            server,
            conn,
            asker,
            typed: String::new(),
            connected: None,
            gsel: 0,
            group: None,
            over: vec![],
            rows: vec![],
            lsel: 0,
            lscroll: 0,
            loading: false,
            read: HashMap::new(),
            reader: None,
            all: None,
            focus: Focus::Groups,
            note: None,
            max: cx.config.news.max_articles.clamp(20, 20_000),
            areas: [Rect::default(); 3],
        };
        w.connect(cx);
        w
    }

    fn spawn<F>(&self, cx: &Cx, f: impl FnOnce(Arc<Conn>) -> F)
    where
        F: std::future::Future<Output = Kind> + Send + 'static,
    {
        let (id, events) = (self.id, cx.tasks.events());
        let fut = f(self.conn.clone());
        tokio::spawn(async move { send(&events, id, fut.await) });
    }

    fn connect(&mut self, cx: &Cx) {
        self.connected = None;
        self.spawn(cx, |c| async move {
            match c.connect().await {
                Ok(p) => Kind::Connected(p),
                Err(e) => Kind::Failed(e),
            }
        });
    }

    fn refresh_counts(&self, cx: &Cx) {
        for g in &self.groups {
            let name = g.name.clone();
            self.spawn(cx, |c| async move {
                match c.status(&name).await {
                    Ok(sel) => Kind::Status { group: name, sel },
                    Err(e) => Kind::OpFailed(format!("{name}: {e}")),
                }
            });
        }
    }

    fn state(&mut self, group: &str, cx: &Cx) -> &mut ReadState {
        let key = read_key(&self.server.name, group);
        self.read.entry(group.to_string()).or_insert_with(|| {
            cx.store
                .and_then(|s| s.kv_get(&key).ok().flatten())
                .and_then(|v| serde_json::from_str(&v).ok())
                .unwrap_or_default()
        })
    }

    fn save(&self, group: &str, cx: &Cx) {
        if let (Some(store), Some(st)) = (cx.store, self.read.get(group))
            && let Ok(v) = serde_json::to_string(st)
        {
            let _ = store.kv_set(&read_key(&self.server.name, group), &v);
        }
    }

    fn unread(&mut self, i: usize, cx: &Cx) -> Option<u64> {
        let (name, sel) = (self.groups[i].name.clone(), self.groups[i].sel?);
        Some(self.state(&name, cx).unread(sel.low, sel.high))
    }

    fn open_group(&mut self, i: usize, cx: &Cx) {
        let Some(g) = self.groups.get(i) else { return };
        let name = g.name.clone();
        self.gsel = i;
        self.group = Some(name.clone());
        self.over.clear();
        self.rows.clear();
        self.reader = None;
        self.lsel = 0;
        self.lscroll = 0;
        self.loading = true;
        self.focus = Focus::List;
        let max = self.max;
        self.spawn(cx, |c| async move {
            match c.overview(&name, max).await {
                Ok((sel, over)) => Kind::Listed { group: name, sel, over },
                Err(e) => Kind::OpFailed(e),
            }
        });
    }

    fn current(&self) -> Option<&Overview> {
        self.rows.get(self.lsel).map(|r| &self.over[r.idx])
    }

    fn open_article(&mut self, cx: &Cx) {
        let (Some(group), Some(o)) = (self.group.clone(), self.current()) else { return };
        let number = o.number;
        self.note = Some("Loading…".into());
        self.spawn(cx, |c| async move {
            match c.article(&group, &number.to_string()).await {
                Ok(raw) => Kind::Opened { group, number, raw },
                Err(e) => Kind::OpFailed(e),
            }
        });
    }

    fn is_read(&self, n: u64) -> bool {
        self.group.as_ref().and_then(|g| self.read.get(g)).is_some_and(|r| r.is_read(n))
    }

    /// Move to the next unread article after the selection, and open it.
    fn next_unread(&mut self, cx: &Cx) -> bool {
        let start = self.lsel + usize::from(self.reader.is_some());
        let found = (start..self.rows.len()).find(|&i| !self.is_read(self.over[self.rows[i].idx].number));
        match found {
            Some(i) => {
                self.lsel = i;
                self.open_article(cx);
                true
            }
            None => {
                self.note = Some("No more unread articles here. c catches up; Tab goes to the groups.".into());
                false
            }
        }
    }

    fn mark(&mut self, number: u64, read: bool, cx: &Cx) {
        let Some(g) = self.group.clone() else { return };
        self.state(&g, cx).mark(number, read);
        self.save(&g, cx);
    }

    fn catch_up(&mut self, cx: &Cx) {
        let i = if self.focus == Focus::Groups {
            self.gsel
        } else {
            self.groups.iter().position(|g| Some(&g.name) == self.group.as_ref()).unwrap_or(self.gsel)
        };
        let Some(g) = self.groups.get(i) else { return };
        let (name, high) = (
            g.name.clone(),
            g.sel.map(|s| s.high).unwrap_or_else(|| self.over.iter().map(|o| o.number).max().unwrap_or(0)),
        );
        self.state(&name, cx).catch_up(high);
        self.save(&name, cx);
        self.note = Some(format!("Marked everything in {name} as read"));
    }

    fn me(cx: &Cx) -> (Option<String>, Option<String>) {
        let n = &cx.config.news;
        let acct = cx.config.mail.accounts.first();
        (
            n.name.clone().or_else(|| acct.and_then(|a| a.display_name.clone())),
            n.email.clone().or_else(|| acct.map(|a| a.email.clone())),
        )
    }

    fn post(&mut self, followup: bool, cx: &mut Cx) {
        if self.connected == Some(Ok(false)) {
            self.note = Some("This server does not take posts".into());
            return;
        }
        let Some(group) = self.group.clone().or_else(|| self.groups.get(self.gsel).map(|g| g.name.clone())) else {
            self.note = Some("Choose a group first".into());
            return;
        };
        let (name, email) = Self::me(cx);
        let mut seed = Seed { groups: group, subject: String::new(), body: String::new(), references: vec![] };
        if followup {
            let Some(r) = &self.reader else {
                self.note = Some("Open an article first".into());
                return;
            };
            let p = &r.parsed;
            seed.subject = if p.subject.to_lowercase().starts_with("re:") {
                p.subject.clone()
            } else {
                format!("Re: {}", p.subject)
            };
            seed.references = p.references.iter().map(|r| format!("<{}>", r.trim_matches(['<', '>']))).collect();
            if let Some(id) = &p.message_id {
                seed.references.push(format!("<{}>", id.trim_matches(['<', '>'])));
            }
            let quoted: String = p
                .text
                .lines()
                .map(|l| if l.starts_with('>') { format!(">{l}\n") } else { format!("> {l}\n") })
                .collect();
            seed.body = format!("\n\n{} wrote:\n{quoted}", if p.from.is_empty() { &p.from_addr } else { &p.from });
        }
        let w = PostWindow::new(self.conn.clone(), seed, name, email, Some(self.id), cx);
        cx.effects.push(Effect::OpenWindow(Box::new(w)));
    }

    fn reply_by_mail(&mut self, cx: &mut Cx) {
        let Some(r) = &self.reader else {
            self.note = Some("Open an article first".into());
            return;
        };
        let p = &r.parsed;
        let enc = |s: &str| url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>().replace('+', "%20");
        let subject =
            if p.subject.to_lowercase().starts_with("re:") { p.subject.clone() } else { format!("Re: {}", p.subject) };
        let quoted: String = p.text.lines().map(|l| format!("> {l}\n")).collect();
        let mut url = format!(
            "mailto:{}?subject={}&body={}",
            enc(&p.from_addr),
            enc(&subject),
            enc(&format!("\n\n{} wrote:\n{quoted}", p.from))
        );
        if let Some(id) = &p.message_id {
            url.push_str(&format!("&in-reply-to={}", enc(&format!("<{id}>"))));
        }
        cx.open_url(url);
    }

    fn show_all(&mut self, cx: &Cx) {
        self.all = Some(AllGroups { groups: vec![], filter: LineInput::default(), sel: 0, scroll: 0 });
        self.note = Some("Loading the list of groups…".into());
        self.spawn(cx, |c| async move {
            match c.groups().await {
                Ok(g) => Kind::All(g, c.descriptions().await.unwrap_or_default()),
                Err(e) => Kind::OpFailed(e),
            }
        });
    }

    fn subscribe(&mut self, name: String, cx: &mut Cx) {
        if self.groups.iter().any(|g| g.name == name) {
            self.note = Some(format!("You already read {name}"));
            return;
        }
        self.groups.push(GroupRow { name: name.clone(), sel: None });
        let mut saved = self.server.clone();
        saved.groups = self.groups.iter().map(|g| g.name.clone()).collect();
        let n = name.clone();
        cx.edit_config(move |c| match c.news.servers.iter_mut().find(|s| s.name == saved.name) {
            Some(s) => {
                if !s.groups.contains(&n) {
                    s.groups.push(n);
                }
            }
            // A server opened by URL is saved once you pick a group there.
            None => c.news.servers.push(saved),
        });
        self.note = Some(format!("Added {name}"));
        let i = self.groups.len() - 1;
        let nm = name;
        self.spawn(cx, |c| async move {
            match c.status(&nm).await {
                Ok(sel) => Kind::Status { group: nm, sel },
                Err(e) => Kind::OpFailed(e),
            }
        });
        self.gsel = i;
    }

    fn unsubscribe(&mut self, cx: &mut Cx) {
        let Some(g) = self.groups.get(self.gsel) else { return };
        let name = g.name.clone();
        self.groups.remove(self.gsel);
        self.gsel = self.gsel.min(self.groups.len().saturating_sub(1));
        let server = self.server.name.clone();
        let n = name.clone();
        cx.edit_config(move |c| {
            if let Some(s) = c.news.servers.iter_mut().find(|s| s.name == server) {
                s.groups.retain(|g| *g != n);
            }
        });
        if self.group.as_deref() == Some(&name) {
            self.group = None;
            self.over.clear();
            self.rows.clear();
            self.reader = None;
        }
        self.note = Some(format!("Removed {name}. L lists every group."));
    }

    fn move_sel(&mut self, d: isize) {
        let step = |v: usize, n: usize| if n == 0 { 0 } else { (v as isize + d).clamp(0, n as isize - 1) as usize };
        match self.focus {
            Focus::Groups => self.gsel = step(self.gsel, self.groups.len()),
            Focus::List => self.lsel = step(self.lsel, self.rows.len()),
            Focus::Reader => {
                if let Some(r) = &mut self.reader {
                    r.scroll = (r.scroll as isize + d).clamp(0, r.lines.saturating_sub(1) as isize) as u16;
                }
            }
        }
    }

    fn draw_groups(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        let block = Block::default().borders(Borders::RIGHT).border_style(t.dim());
        let inner = block.inner(area);
        frame.render_widget(block, area);
        self.areas[0] = inner;
        let w = inner.width as usize;
        if self.groups.is_empty() {
            frame.render_widget(
                Paragraph::new("No groups yet.\nPress L to list the server's groups.")
                    .style(t.dim())
                    .wrap(Wrap { trim: false }),
                inner,
            );
            return;
        }
        let focused = self.focus == Focus::Groups && cx.focused;
        let counts: Vec<Option<u64>> = (0..self.groups.len()).map(|i| self.unread_cached(i)).collect();
        let lines: Vec<Line> = self
            .groups
            .iter()
            .enumerate()
            .skip(self.gsel.saturating_sub(inner.height.saturating_sub(1) as usize))
            .take(inner.height as usize)
            .map(|(i, g)| {
                let count = counts[i].filter(|&c| c > 0).map(|c| format!(" {c}")).unwrap_or_default();
                let mark = if Some(&g.name) == self.group.as_ref() { "▸" } else { " " };
                let name_w = w.saturating_sub(count.width() + 2);
                let text = format!("{mark} {}{count}", fit(&g.name, name_w));
                let mut st =
                    if counts[i].unwrap_or(0) > 0 { Style::new().add_modifier(Modifier::BOLD) } else { Style::new() };
                if i == self.gsel && focused {
                    st = t.selection();
                }
                Line::from(Span::styled(fit(&text, w), st))
            })
            .collect();
        frame.render_widget(Paragraph::new(lines), inner);
    }

    /// Unread count without loading state from the store (draw has no store).
    fn unread_cached(&self, i: usize) -> Option<u64> {
        let g = &self.groups[i];
        let sel = g.sel?;
        Some(self.read.get(&g.name).map(|r| r.unread(sel.low, sel.high)).unwrap_or(sel.count))
    }

    fn draw_list(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        self.areas[1] = area;
        let h = area.height as usize;
        if self.rows.is_empty() {
            let msg = match (&self.connected, self.loading) {
                (None, _) => "Connecting…".to_string(),
                (Some(Err(e)), _) => format!("Could not connect: {e}\n\nPress g to try again."),
                (_, true) => "Loading…".into(),
                _ if self.group.is_none() => "Choose a group and press Enter.".into(),
                _ => "No articles here.".into(),
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
        let focused = self.focus == Focus::List && cx.focused;
        let w = area.width as usize;
        let from_w = (w / 4).clamp(8, 22);
        let lines: Vec<Line> = self
            .rows
            .iter()
            .enumerate()
            .skip(self.lscroll)
            .take(h)
            .map(|(row, r)| {
                let o = &self.over[r.idx];
                let read = self.is_read(o.number);
                let indent = "  ".repeat(r.depth.min(8));
                let subject =
                    if r.depth > 0 { format!("{indent}└ {}", strip_re(&o.subject)) } else { o.subject.clone() };
                let date = short_date(alouette_feeds::parse_date(&o.date), cx.local_offset);
                let subj_w = w.saturating_sub(2 + 6 + 2 + from_w + 2);
                let text = format!(
                    "{} {:>6}  {}  {}",
                    if read { " " } else { "●" },
                    date,
                    fit(&who(&o.from), from_w),
                    fit(&subject, subj_w)
                );
                let mut st = if read { Style::new() } else { Style::new().add_modifier(Modifier::BOLD) };
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
        let Some(r) = &mut self.reader else { return };
        let p = &r.parsed;
        let head = |k: &str, v: String| Line::from(vec![Span::styled(format!("{k:<8}"), t.accent()), Span::raw(v)]);
        let mut lines = vec![
            head("From", if p.from.is_empty() { p.from_addr.clone() } else { format!("{} <{}>", p.from, p.from_addr) }),
            head("Date", long_date(p.date, cx.local_offset)),
            Line::from(vec![
                Span::styled(format!("{:<8}", "Subject"), t.accent()),
                Span::styled(p.subject.clone(), Style::new().add_modifier(Modifier::BOLD)),
            ]),
            Line::default(),
        ];
        let mut sig = false;
        for l in p.text.lines() {
            if l == "-- " {
                sig = true;
            }
            let st = if sig || l.starts_with(">>") {
                t.dim()
            } else if l.starts_with('>') {
                Style::new().fg(t.link)
            } else {
                Style::new()
            };
            lines.push(Line::from(Span::styled(l.to_string(), st)));
        }
        let para = Paragraph::new(lines).wrap(Wrap { trim: false });
        r.lines = para.line_count(inner.width);
        r.scroll = r.scroll.min(r.lines.saturating_sub(1) as u16);
        frame.render_widget(para.scroll((r.scroll, 0)), inner);
    }

    fn draw_all(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        let Some(a) = &mut self.all else { return };
        let label = " Find a group: ";
        let field = Style::new().fg(t.input_fg).bg(t.input_bg);
        let pad = (area.width as usize).saturating_sub(label.width() + a.filter.value.width());
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(label, t.accent()),
                Span::styled(format!("{}{}", a.filter.value, " ".repeat(pad)), field),
            ])),
            Rect { height: 1, ..area },
        );
        if cx.focused {
            frame.set_cursor_position((area.x + label.width() as u16 + a.filter.cursor_col() as u16, area.y));
        }
        let list = Rect { y: area.y + 2, height: area.height.saturating_sub(3), ..area };
        let v = a.visible();
        a.sel = a.sel.min(v.len().saturating_sub(1));
        let h = list.height as usize;
        if a.sel < a.scroll {
            a.scroll = a.sel;
        }
        if h > 0 && a.sel >= a.scroll + h {
            a.scroll = a.sel + 1 - h;
        }
        if a.groups.is_empty() {
            frame.render_widget(Paragraph::new("Loading…").style(t.dim()), list);
        }
        let w = list.width as usize;
        let name_w = (w / 3).clamp(16, 40);
        let lines: Vec<Line> = v
            .iter()
            .enumerate()
            .skip(a.scroll)
            .take(h)
            .map(|(row, &i)| {
                let (g, d) = &a.groups[i];
                let n = g.high.saturating_sub(g.low.saturating_sub(1)).min(g.high);
                let text = format!(" {}  {:>7}  {}", fit(&g.name, name_w), n, d);
                let st = if row == a.sel { t.selection() } else { Style::new() };
                Line::from(Span::styled(fit(&text, w), st))
            })
            .collect();
        frame.render_widget(Paragraph::new(lines), list);
        let foot = Rect { y: area.bottom().saturating_sub(1), height: 1, ..area };
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!(" {} of {} groups. Enter adds a group; Esc goes back.", v.len(), a.groups.len()),
                t.dim(),
            )),
            foot,
        );
    }
}

fn strip_re(s: &str) -> &str {
    let mut s = s.trim();
    while s.len() >= 3 && s[..3].eq_ignore_ascii_case("re:") {
        s = s[3..].trim_start();
    }
    s
}

impl Window for NewsWindow {
    fn title(&self) -> String {
        match &self.group {
            Some(g) => format!("News: {g}"),
            None => format!("News: {}", self.server.name),
        }
    }
    fn module(&self) -> &'static str {
        NEWS
    }
    fn url(&self) -> Option<String> {
        Some(format!("news:{}", self.server.name))
    }
    fn ai_context(&self) -> Option<AiContext> {
        if let Some(r) = &self.reader {
            let p = &r.parsed;
            let enc =
                |s: &str| url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>().replace('+', "%20");
            let subject = if p.subject.to_lowercase().starts_with("re:") {
                p.subject.clone()
            } else {
                format!("Re: {}", p.subject)
            };
            let mut reply = format!("mailto:{}?subject={}", enc(&p.from_addr), enc(&subject));
            if let Some(id) = &p.message_id {
                reply.push_str(&format!("&in-reply-to={}", enc(&format!("<{id}>"))));
            }
            return Some(AiContext {
                module: NEWS.into(),
                kind: "news article".into(),
                title: p.subject.clone(),
                text: format!(
                    "From: {}\nNewsgroups: {}\nSubject: {}\n\n{}",
                    p.from,
                    self.group.clone().unwrap_or_default(),
                    p.subject,
                    p.text
                ),
                list: false,
                reply_url: Some(reply),
            });
        }
        let g = self.group.clone()?;
        if self.over.is_empty() {
            return None;
        }
        let text = self
            .over
            .iter()
            .rev()
            .take(200)
            .map(|o| {
                let unread = !self.is_read(o.number);
                format!("{}{} | {} | {}", if unread { "[unread] " } else { "" }, o.date, o.from, o.subject)
            })
            .collect::<Vec<_>>()
            .join("\n");
        Some(AiContext { module: NEWS.into(), kind: "newsgroup".into(), title: g, text, list: true, reply_url: None })
    }
    fn badge(&self) -> Option<String> {
        Some(self.conn.badge()).filter(|b| !b.is_empty())
    }
    fn wants_text(&self) -> bool {
        self.all.is_some() || self.asker.question().is_some()
    }
    fn hints(&self) -> Vec<(&'static str, &'static str)> {
        if self.all.is_some() {
            return vec![("Enter", "Add group"), ("Esc", "Back")];
        }
        vec![
            ("Enter", "Open"),
            ("Space", "Next"),
            ("p", "Post"),
            ("f", "Follow up"),
            ("r", "Reply"),
            ("c", "Catch up"),
            ("L", "All groups"),
            ("Tab", "Pane"),
        ]
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        if area.height < 4 || area.width < 20 {
            return;
        }
        let t = cx.theme;
        let foot = Rect { y: area.bottom() - 1, height: 1, ..area };
        let body = Rect { height: area.height - 1, ..area };
        if self.all.is_some() {
            self.draw_all(frame, body, cx);
        } else {
            let gw = (body.width / 4).clamp(14, 30);
            let left = Rect { width: gw, ..body };
            let right = Rect { x: body.x + gw + 1, width: body.width.saturating_sub(gw + 1), ..body };
            self.draw_groups(frame, left, cx);
            if self.reader.is_some() {
                let lh = (right.height * 2 / 5).max(3);
                self.draw_list(frame, Rect { height: lh, ..right }, cx);
                self.draw_reader(frame, Rect { y: right.y + lh, height: right.height - lh, ..right }, cx);
            } else {
                self.draw_list(frame, right, cx);
            }
        }
        frame.render_widget(
            Paragraph::new(Span::styled(format!(" {}", self.note.clone().unwrap_or_default()), t.dim())),
            foot,
        );
        if let Some(q) = self.asker.question() {
            alouette_ui::ask::draw(frame, area, t, &q, &self.typed);
        }
    }

    fn on_paste(&mut self, text: &str, _cx: &mut Cx) -> Handled {
        if self.asker.question().is_some() {
            self.typed.push_str(text.trim_end_matches(['\r', '\n']));
            return Handled::Yes;
        }
        if let Some(a) = &mut self.all {
            let v = format!("{}{}", a.filter.value, text.trim());
            a.filter.set(&v);
            return Handled::Yes;
        }
        Handled::No
    }

    fn on_key(&mut self, key: KeyEvent, cx: &mut Cx) -> Handled {
        if let Some(q) = self.asker.question() {
            alouette_ui::ask::on_key(&self.asker, &q, &mut self.typed, key);
            return Handled::Yes;
        }
        if let Some(a) = &mut self.all {
            let v = a.visible();
            match key.code {
                KeyCode::Up => a.sel = a.sel.saturating_sub(1),
                KeyCode::Down => a.sel = (a.sel + 1).min(v.len().saturating_sub(1)),
                KeyCode::PageUp => a.sel = a.sel.saturating_sub(10),
                KeyCode::PageDown => a.sel = (a.sel + 10).min(v.len().saturating_sub(1)),
                KeyCode::Enter => {
                    if let Some(&i) = v.get(a.sel) {
                        let name = a.groups[i].0.name.clone();
                        self.subscribe(name, cx);
                    }
                }
                KeyCode::Esc => {
                    self.all = None;
                    self.note = None;
                }
                _ => match a.filter.on_key(key) {
                    InputResult::Edited => a.sel = 0,
                    InputResult::Ignored => return Handled::No,
                    _ => {}
                },
            }
            return Handled::Yes;
        }
        let page = |w: &Self| match w.focus {
            Focus::Reader => w.areas[2].height.saturating_sub(1).max(1) as isize,
            Focus::List => w.areas[1].height.saturating_sub(1).max(1) as isize,
            Focus::Groups => w.areas[0].height.saturating_sub(1).max(1) as isize,
        };
        match key.code {
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Groups => Focus::List,
                    Focus::List if self.reader.is_some() => Focus::Reader,
                    _ => Focus::Groups,
                }
            }
            KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Reader => Focus::List,
                    Focus::List => Focus::Groups,
                    Focus::Groups if self.reader.is_some() => Focus::Reader,
                    Focus::Groups => Focus::List,
                }
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
            KeyCode::PageUp => self.move_sel(-page(self)),
            KeyCode::PageDown => self.move_sel(page(self)),
            KeyCode::Home => self.move_sel(-1_000_000),
            KeyCode::End => self.move_sel(1_000_000),
            KeyCode::Enter | KeyCode::Right => match self.focus {
                Focus::Groups => self.open_group(self.gsel, cx),
                Focus::List => self.open_article(cx),
                Focus::Reader => self.move_sel(1),
            },
            KeyCode::Char(' ') => {
                // Page through the article, then on to the next unread one.
                let more =
                    self.reader.as_ref().is_some_and(|r| (r.scroll as usize + self.areas[2].height as usize) < r.lines);
                if self.focus == Focus::Groups {
                    self.open_group(self.gsel, cx);
                } else if more {
                    let h = self.areas[2].height.saturating_sub(1).max(1);
                    if let Some(r) = &mut self.reader {
                        r.scroll += h;
                    }
                } else {
                    self.next_unread(cx);
                }
            }
            KeyCode::Char('n') => {
                self.next_unread(cx);
            }
            KeyCode::Esc | KeyCode::Left => {
                if self.reader.take().is_some() {
                    self.focus = Focus::List;
                } else if self.focus == Focus::List {
                    self.focus = Focus::Groups;
                } else {
                    return Handled::No;
                }
            }
            KeyCode::Char('u') => {
                if let Some(n) = self.current().map(|o| o.number) {
                    let r = self.is_read(n);
                    self.mark(n, !r, cx);
                }
            }
            KeyCode::Char('c') => self.catch_up(cx),
            KeyCode::Char('p') | KeyCode::Char('w') => self.post(false, cx),
            KeyCode::Char('f') => self.post(true, cx),
            KeyCode::Char('r') => self.reply_by_mail(cx),
            KeyCode::Char('L') | KeyCode::Char('l') => self.show_all(cx),
            KeyCode::Char('D') if self.focus == Focus::Groups => self.unsubscribe(cx),
            KeyCode::Char('g') => {
                if matches!(self.connected, Some(Err(_))) {
                    self.connect(cx);
                } else {
                    self.refresh_counts(cx);
                    if let Some(i) = self.groups.iter().position(|g| Some(&g.name) == self.group.as_ref()) {
                        let keep = self.lsel;
                        self.open_group(i, cx);
                        self.lsel = keep;
                        self.focus = Focus::List;
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
            MouseEventKind::ScrollUp => self.move_sel(-3),
            MouseEventKind::ScrollDown => self.move_sel(3),
            MouseEventKind::Down(MouseButton::Left) if self.all.is_none() => {
                if inside(self.areas[0]) {
                    let skip = self.gsel.saturating_sub(self.areas[0].height.saturating_sub(1) as usize);
                    let i = skip + (ev.row - self.areas[0].y) as usize;
                    if i < self.groups.len() {
                        self.focus = Focus::Groups;
                        self.open_group(i, cx);
                    }
                } else if inside(self.areas[1]) {
                    let i = self.lscroll + (ev.row - self.areas[1].y) as usize;
                    if i < self.rows.len() {
                        self.focus = Focus::List;
                        self.lsel = i;
                        self.open_article(cx);
                    }
                } else if inside(self.areas[2]) {
                    self.focus = Focus::Reader;
                }
            }
            _ => return Handled::No,
        }
        Handled::Yes
    }

    fn on_event(&mut self, ev: &AppEvent, cx: &mut Cx) {
        let AppEvent::Module { module: NEWS, payload } = ev else { return };
        let Some(m) = payload.downcast_ref::<Msg>() else { return };
        if m.win != self.id {
            return;
        }
        match m.kind.clone() {
            Kind::Connected(posting) => {
                self.connected = Some(Ok(posting));
                self.note =
                    if self.groups.is_empty() { Some("Press L to choose groups to read.".into()) } else { None };
                self.refresh_counts(cx);
            }
            Kind::Failed(e) => {
                self.note = Some(e.clone());
                self.connected = Some(Err(e));
            }
            Kind::Status { group, sel } => {
                self.state(&group, cx).expire(sel.low);
                if let Some(g) = self.groups.iter_mut().find(|g| g.name == group) {
                    g.sel = Some(sel);
                }
                if let Some(i) = self.groups.iter().position(|g| g.name == group) {
                    let _ = self.unread(i, cx);
                }
            }
            Kind::Listed { group, sel, over } => {
                if self.group.as_deref() != Some(&group) {
                    return;
                }
                self.loading = false;
                self.state(&group, cx).expire(sel.low);
                if let Some(g) = self.groups.iter_mut().find(|g| g.name == group) {
                    g.sel = Some(sel);
                }
                self.rows = thread(&over);
                self.over = over;
                self.lsel = self.lsel.min(self.rows.len().saturating_sub(1));
                // Start at the first unread article.
                if let Some(i) = (0..self.rows.len()).find(|&i| !self.is_read(self.over[self.rows[i].idx].number)) {
                    self.lsel = i;
                }
            }
            Kind::Opened { group, number, raw } => {
                if self.group.as_deref() != Some(&group) {
                    return;
                }
                self.note = None;
                self.mark(number, true, cx);
                self.reader = Some(Reader { number, parsed: parse(&raw), scroll: 0, lines: 0 });
                if let Some(i) = self.rows.iter().position(|r| self.over[r.idx].number == number) {
                    self.lsel = i;
                }
            }
            Kind::All(groups, descs) => {
                let d: HashMap<String, String> = descs.into_iter().collect();
                if let Some(a) = &mut self.all {
                    a.groups = groups
                        .into_iter()
                        .map(|g| {
                            let desc = d.get(&g.name).cloned().unwrap_or_default();
                            (g, desc)
                        })
                        .collect();
                    a.groups.sort_by(|x, y| x.0.name.cmp(&y.0.name));
                }
                self.note = None;
            }
            Kind::OpFailed(e) => {
                self.loading = false;
                self.note = Some(e.clone());
                cx.notify(Notification::error(e));
            }
            Kind::Posted(r) => match r {
                Ok(n) => {
                    self.note = Some(n);
                    if let Some(i) = self.groups.iter().position(|g| Some(&g.name) == self.group.as_ref()) {
                        self.open_group(i, cx);
                    }
                }
                Err(e) => self.note = Some(e),
            },
        }
    }
}

impl NewsWindow {
    /// The article number shown in the reader, for tests.
    pub fn reading(&self) -> Option<u64> {
        self.reader.as_ref().map(|r| r.number)
    }
}
