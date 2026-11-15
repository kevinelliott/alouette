//! The Feeds window: subscriptions on the left, their items on the right, the item below.

use std::collections::{HashMap, HashSet};

use alouette_core::config::FeedSub;
use alouette_core::event::{EventSender, ModulePayload};
use alouette_core::{AppEvent, Notification};
use alouette_feeds::{Feed, Item};
use alouette_net::Net;
use alouette_ui::input::{InputResult, LineInput};
use alouette_ui::window::{Cx, DrawCx, Handled, Window};
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::util::{fit, long_date, short_date};
use crate::{FEEDS, next_id};

const ACCEPT: &str = "application/rss+xml, application/atom+xml, application/feed+json, application/json;q=0.9, application/xml;q=0.9, text/xml;q=0.9, */*;q=0.5";
const MAX_READ: usize = 5000;

#[derive(Clone)]
pub struct Msg {
    pub win: u64,
    pub kind: Kind,
}

#[derive(Clone)]
pub enum Kind {
    /// A feed fetched. `url` is the subscription; `found` is set when the page pointed
    /// at a different feed URL.
    Fetched {
        url: String,
        feed: Feed,
        found: Option<String>,
    },
    Failed {
        url: String,
        error: String,
    },
    /// Time to refresh.
    Tick,
}

fn send(events: &EventSender, win: u64, kind: Kind) {
    let _ = events.send(AppEvent::Module { module: FEEDS, payload: ModulePayload::new(Msg { win, kind }) });
}

/// Fetch and parse a feed. A web page is searched for the feed it announces.
pub async fn fetch(net: &Net, url: &str) -> Result<(Feed, Option<String>), String> {
    let mut url = url.to_string();
    let mut found = None;
    for _ in 0..2 {
        let r = net.get(&url, &[("Accept", ACCEPT)], 16 * 1024 * 1024).await.map_err(|e| e.to_string())?;
        if !r.ok() {
            return Err(format!("{url}: the server said {}", r.status));
        }
        let base = url::Url::parse(&r.url).ok();
        match alouette_feeds::parse(&r.body, base.as_ref()) {
            Ok(f) => return Ok((f, found)),
            Err(e) => {
                let html = r.header("content-type").is_some_and(|c| c.contains("html"))
                    || r.text().trim_start().to_lowercase().starts_with("<!doctype html");
                let next = base
                    .as_ref()
                    .filter(|_| html && found.is_none())
                    .and_then(|b| alouette_feeds::discover(&r.text(), b).into_iter().next());
                match next {
                    Some((u, _)) => {
                        found = Some(u.clone());
                        url = u;
                    }
                    None => return Err(format!("{url}: {e}")),
                }
            }
        }
    }
    Err(format!("{url}: no feed found"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Feeds,
    List,
    Reader,
}

struct Sub {
    sub: FeedSub,
    feed: Option<Feed>,
    error: Option<String>,
    loading: bool,
    /// Not in the config: opened with feed:URL.
    temporary: bool,
}

impl Sub {
    fn title(&self) -> String {
        self.sub
            .title
            .clone()
            .or_else(|| self.feed.as_ref().map(|f| f.title.clone()).filter(|t| !t.is_empty()))
            .unwrap_or_else(|| self.sub.url.clone())
    }
}

pub struct FeedsWindow {
    id: u64,
    subs: Vec<Sub>,
    fsel: usize,
    lsel: usize,
    lscroll: usize,
    read: HashMap<String, HashSet<String>>,
    reader: Option<(Item, u16, usize)>,
    focus: Focus,
    add: Option<LineInput>,
    confirm_remove: bool,
    note: Option<String>,
    alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
    areas: [Rect; 3],
}

impl Drop for FeedsWindow {
    fn drop(&mut self) {
        self.alive.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

fn cache_key(url: &str) -> String {
    format!("feeds/cache/{url}")
}
fn read_key(url: &str) -> String {
    format!("feeds/read/{url}")
}

impl FeedsWindow {
    /// `extra` is a feed to show that you may not be subscribed to.
    pub fn new(extra: Option<String>, cx: &mut Cx) -> Self {
        let mut subs: Vec<Sub> = cx
            .config
            .feeds
            .subscriptions
            .iter()
            .map(|s| Sub { sub: s.clone(), feed: None, error: None, loading: false, temporary: false })
            .collect();
        let mut fsel = 0;
        if let Some(u) = extra {
            match subs.iter().position(|s| s.sub.url == u) {
                Some(i) => fsel = i,
                None => {
                    subs.push(Sub {
                        sub: FeedSub { url: u, title: None, folder: None },
                        feed: None,
                        error: None,
                        loading: false,
                        temporary: true,
                    });
                    fsel = subs.len() - 1;
                }
            }
        }
        let mut w = Self {
            id: next_id(),
            subs,
            fsel,
            lsel: 0,
            lscroll: 0,
            read: HashMap::new(),
            reader: None,
            focus: if fsel > 0 { Focus::List } else { Focus::Feeds },
            add: None,
            confirm_remove: false,
            note: None,
            alive: Default::default(),
            areas: [Rect::default(); 3],
        };
        // Show what was saved last time at once, then fetch.
        if let Some(store) = cx.store {
            for s in &mut w.subs {
                s.feed =
                    store.kv_get(&cache_key(&s.sub.url)).ok().flatten().and_then(|v| serde_json::from_str(&v).ok());
                let r: Vec<String> = store
                    .kv_get(&read_key(&s.sub.url))
                    .ok()
                    .flatten()
                    .and_then(|v| serde_json::from_str(&v).ok())
                    .unwrap_or_default();
                w.read.insert(s.sub.url.clone(), r.into_iter().collect());
            }
        }
        if w.subs.is_empty() {
            w.note = Some("No feeds yet. Press a and type a feed or site address.".into());
        }
        w.fetch_all(cx);
        w.alive.store(true, std::sync::atomic::Ordering::Relaxed);
        let (alive, id, events) = (w.alive.clone(), w.id, cx.tasks.events());
        let every = std::time::Duration::from_secs(cx.config.feeds.refresh_mins.max(5) * 60);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                if !alive.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                send(&events, id, Kind::Tick);
            }
        });
        w
    }

    fn fetch(&mut self, i: usize, cx: &Cx) {
        let Some(s) = self.subs.get_mut(i) else { return };
        s.loading = true;
        let url = s.sub.url.clone();
        let (id, events, net) = (self.id, cx.tasks.events(), cx.net.clone());
        tokio::spawn(async move {
            match fetch(&net, &url).await {
                Ok((feed, found)) => send(&events, id, Kind::Fetched { url, feed, found }),
                Err(error) => send(&events, id, Kind::Failed { url, error }),
            }
        });
    }

    fn fetch_all(&mut self, cx: &Cx) {
        for i in 0..self.subs.len() {
            self.fetch(i, cx);
        }
    }

    fn items(&self) -> Vec<&Item> {
        let Some(f) = self.subs.get(self.fsel).and_then(|s| s.feed.as_ref()) else { return vec![] };
        let mut v: Vec<&Item> = f.items.iter().collect();
        // Newest first; undated items keep the feed's order at the end.
        v.sort_by_key(|i| std::cmp::Reverse(i.date.unwrap_or(i64::MIN)));
        v
    }

    fn is_read(&self, url: &str, id: &str) -> bool {
        self.read.get(url).is_some_and(|r| r.contains(id))
    }

    fn unread(&self, s: &Sub) -> usize {
        s.feed.as_ref().map(|f| f.items.iter().filter(|i| !self.is_read(&s.sub.url, &i.id)).count()).unwrap_or(0)
    }

    fn save_read(&mut self, url: &str, cx: &Cx) {
        // Forget ids no longer in the feed once the list grows, so it stays small.
        let live: HashSet<String> = self
            .subs
            .iter()
            .find(|s| s.sub.url == url)
            .and_then(|s| s.feed.as_ref())
            .map(|f| f.items.iter().map(|i| i.id.clone()).collect())
            .unwrap_or_default();
        if let Some(r) = self.read.get_mut(url) {
            if r.len() > MAX_READ {
                r.retain(|id| live.contains(id));
            }
            if let (Some(store), Ok(v)) = (cx.store, serde_json::to_string(&r.iter().collect::<Vec<_>>())) {
                let _ = store.kv_set(&read_key(url), &v);
            }
        }
    }

    fn mark(&mut self, id: &str, read: bool, cx: &Cx) {
        let Some(url) = self.subs.get(self.fsel).map(|s| s.sub.url.clone()) else { return };
        let set = self.read.entry(url.clone()).or_default();
        if read {
            set.insert(id.to_string());
        } else {
            set.remove(id);
        }
        self.save_read(&url, cx);
    }

    fn open_item(&mut self, cx: &Cx) {
        let Some(item) = self.items().get(self.lsel).map(|i| (*i).clone()) else { return };
        self.mark(&item.id, true, cx);
        self.reader = Some((item, 0, 0));
    }

    fn mark_all(&mut self, cx: &Cx) {
        let Some(s) = self.subs.get(self.fsel) else { return };
        let url = s.sub.url.clone();
        let ids: Vec<String> =
            s.feed.as_ref().map(|f| f.items.iter().map(|i| i.id.clone()).collect()).unwrap_or_default();
        self.read.entry(url.clone()).or_default().extend(ids);
        self.save_read(&url, cx);
        self.note = Some("Marked everything in this feed as read".into());
    }

    fn subscribe(&mut self, url: String, cx: &mut Cx) {
        let url = if url.contains("://") { url } else { format!("https://{url}") };
        if let Some(i) = self.subs.iter().position(|s| s.sub.url == url) {
            if self.subs[i].temporary {
                self.subs[i].temporary = false;
                let sub = self.subs[i].sub.clone();
                cx.edit_config(move |c| c.feeds.subscriptions.push(sub));
                self.note = Some("Subscribed".into());
            } else {
                self.note = Some("You already follow that feed".into());
            }
            self.fsel = i;
            return;
        }
        let sub = FeedSub { url: url.clone(), title: None, folder: None };
        self.subs.push(Sub { sub: sub.clone(), feed: None, error: None, loading: false, temporary: false });
        cx.edit_config(move |c| c.feeds.subscriptions.push(sub));
        self.fsel = self.subs.len() - 1;
        self.lsel = 0;
        self.reader = None;
        self.note = Some(format!("Added {url}"));
        self.fetch(self.fsel, cx);
    }

    fn remove(&mut self, cx: &mut Cx) {
        let Some(s) = self.subs.get(self.fsel) else { return };
        let url = s.sub.url.clone();
        let temporary = s.temporary;
        self.subs.remove(self.fsel);
        self.fsel = self.fsel.min(self.subs.len().saturating_sub(1));
        self.reader = None;
        self.lsel = 0;
        if !temporary {
            let u = url.clone();
            cx.edit_config(move |c| c.feeds.subscriptions.retain(|s| s.url != u));
        }
        if let Some(store) = cx.store {
            let _ = store.kv_set(&cache_key(&url), "");
        }
        self.note = Some(format!("Removed {url}"));
    }

    fn move_sel(&mut self, d: isize) {
        let step = |v: usize, n: usize| if n == 0 { 0 } else { (v as isize + d).clamp(0, n as isize - 1) as usize };
        match self.focus {
            Focus::Feeds => {
                let n = step(self.fsel, self.subs.len());
                if n != self.fsel {
                    self.fsel = n;
                    self.lsel = 0;
                    self.lscroll = 0;
                    self.reader = None;
                }
            }
            Focus::List => self.lsel = step(self.lsel, self.items().len()),
            Focus::Reader => {
                if let Some((_, s, n)) = &mut self.reader {
                    *s = (*s as isize + d).clamp(0, n.saturating_sub(1) as isize) as u16;
                }
            }
        }
    }

    fn draw_feeds(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        let block = Block::default().borders(Borders::RIGHT).border_style(t.dim());
        let inner = block.inner(area);
        frame.render_widget(block, area);
        self.areas[0] = inner;
        let w = inner.width as usize;
        let focused = self.focus == Focus::Feeds && cx.focused;
        let lines: Vec<Line> = self
            .subs
            .iter()
            .enumerate()
            .skip(self.fsel.saturating_sub(inner.height.saturating_sub(1) as usize))
            .take(inner.height as usize)
            .map(|(i, s)| {
                let n = self.unread(s);
                let count = if s.loading && s.feed.is_none() {
                    " …".to_string()
                } else if s.error.is_some() {
                    " !".into()
                } else if n > 0 {
                    format!(" {n}")
                } else {
                    String::new()
                };
                let mark = if i == self.fsel { "▸" } else { " " };
                let title = match &s.sub.folder {
                    Some(f) => format!("{f}/{}", s.title()),
                    None => s.title(),
                };
                let text = format!("{mark} {}{count}", fit(&title, w.saturating_sub(count.width() + 2)));
                let mut st = if n > 0 { Style::new().add_modifier(Modifier::BOLD) } else { Style::new() };
                if s.error.is_some() {
                    st = st.fg(t.error);
                }
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
        let Some(s) = self.subs.get(self.fsel) else {
            frame.render_widget(
                Paragraph::new(
                    "Press a to add a feed, such as https://blog.rust-lang.org/feed.xml, or just a site's address.",
                )
                .style(t.dim())
                .wrap(Wrap { trim: false }),
                area,
            );
            return;
        };
        let url = s.sub.url.clone();
        let err = s.error.clone();
        let loading = s.loading;
        let items: Vec<Item> = self.items().into_iter().cloned().collect();
        if items.is_empty() {
            let msg = match (err, loading) {
                (Some(e), _) => format!("Could not read the feed: {e}\n\nPress g to try again."),
                (None, true) => "Loading…".into(),
                _ => "This feed is empty.".into(),
            };
            frame.render_widget(Paragraph::new(msg).style(t.dim()).wrap(Wrap { trim: false }), area);
            return;
        }
        let h = area.height as usize;
        if self.lsel < self.lscroll {
            self.lscroll = self.lsel;
        }
        if h > 0 && self.lsel >= self.lscroll + h {
            self.lscroll = self.lsel + 1 - h;
        }
        let focused = self.focus == Focus::List && cx.focused;
        let w = area.width as usize;
        let lines: Vec<Line> = items
            .iter()
            .enumerate()
            .skip(self.lscroll)
            .take(h)
            .map(|(row, it)| {
                let read = self.is_read(&url, &it.id);
                let pod = if it.enclosures.is_empty() { " " } else { "♪" };
                let text = format!(
                    "{}{pod}{:>6}  {}",
                    if read { " " } else { "●" },
                    short_date(it.date, cx.local_offset),
                    it.title
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
        let Some((it, scroll, count)) = &mut self.reader else { return };
        let mut lines = vec![Line::from(Span::styled(it.title.clone(), Style::new().add_modifier(Modifier::BOLD)))];
        let mut meta = long_date(it.date, cx.local_offset);
        if let Some(a) = &it.author {
            meta = if meta.is_empty() { a.clone() } else { format!("{meta} · {a}") };
        }
        if !meta.is_empty() {
            lines.push(Line::from(Span::styled(meta, t.dim())));
        }
        if let Some(l) = &it.link {
            lines.push(Line::from(Span::styled(l.clone(), Style::new().fg(t.link))));
        }
        for (u, mime, size) in &it.enclosures {
            let sz = if *size > 0 { format!(" ({} MB)", size / 1_048_576) } else { String::new() };
            lines.push(Line::from(Span::styled(format!("♪ {mime}{sz}: {u}"), t.dim())));
        }
        lines.push(Line::default());
        let text = if it.body.contains('<') { alouette_mail::message::html_to_text(&it.body) } else { it.body.clone() };
        lines.extend(text.lines().map(|l| Line::from(l.to_string())));
        let para = Paragraph::new(lines).wrap(Wrap { trim: false });
        *count = para.line_count(inner.width);
        *scroll = (*scroll).min(count.saturating_sub(1) as u16);
        frame.render_widget(para.scroll((*scroll, 0)), inner);
    }
}

impl Window for FeedsWindow {
    fn title(&self) -> String {
        match self.subs.get(self.fsel) {
            Some(s) if self.focus != Focus::Feeds || self.reader.is_some() => format!("Feeds: {}", s.title()),
            _ => "Feeds".into(),
        }
    }
    fn module(&self) -> &'static str {
        FEEDS
    }
    fn url(&self) -> Option<String> {
        Some("feeds:".into())
    }
    fn wants_text(&self) -> bool {
        self.add.is_some()
    }
    fn hints(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("Enter", "Read"),
            ("Space", "Next"),
            ("o", "Open link"),
            ("a", "Add feed"),
            ("m", "All read"),
            ("g", "Refresh"),
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
        let fw = (body.width / 4).clamp(14, 32);
        let left = Rect { width: fw, ..body };
        let right = Rect { x: body.x + fw + 1, width: body.width.saturating_sub(fw + 1), ..body };
        self.draw_feeds(frame, left, cx);
        if self.reader.is_some() {
            let lh = (right.height * 2 / 5).max(3);
            self.draw_list(frame, Rect { height: lh, ..right }, cx);
            self.draw_reader(frame, Rect { y: right.y + lh, height: right.height - lh, ..right }, cx);
        } else {
            self.draw_list(frame, right, cx);
        }
        if let Some(input) = &self.add {
            let label = " Feed or site address: ";
            let field = Style::new().fg(t.input_fg).bg(t.input_bg);
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(label, t.accent()),
                    Span::styled(input.value.clone(), field),
                ])),
                foot,
            );
            frame.set_cursor_position((foot.x + label.width() as u16 + input.cursor_col() as u16, foot.y));
        } else if self.confirm_remove {
            let name = self.subs.get(self.fsel).map(|s| s.title()).unwrap_or_default();
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(format!(" Stop following {name}? "), Style::new().fg(t.error)),
                    Span::styled("y", t.accent()),
                    Span::raw(" yes   "),
                    Span::styled("n", t.accent()),
                    Span::raw(" no"),
                ])),
                foot,
            );
        } else {
            let mut s = self.note.clone().unwrap_or_default();
            if s.is_empty()
                && let Some(sub) = self.subs.get(self.fsel)
            {
                if sub.temporary {
                    s = "You do not follow this feed. Press s to subscribe.".into();
                } else if let Some(e) = &sub.error {
                    s = e.clone();
                }
            }
            frame.render_widget(Paragraph::new(Span::styled(format!(" {s}"), t.dim())), foot);
        }
    }

    fn on_paste(&mut self, text: &str, _cx: &mut Cx) -> Handled {
        if let Some(i) = &mut self.add {
            let v = format!("{}{}", i.value, text.trim());
            i.set(&v);
            return Handled::Yes;
        }
        Handled::No
    }

    fn on_key(&mut self, key: KeyEvent, cx: &mut Cx) -> Handled {
        if let Some(input) = &mut self.add {
            match input.on_key(key) {
                InputResult::Submit => {
                    let v = input.value.trim().to_string();
                    self.add = None;
                    if !v.is_empty() {
                        self.subscribe(v, cx);
                    }
                }
                InputResult::Cancel => self.add = None,
                _ => {}
            }
            return Handled::Yes;
        }
        if self.confirm_remove {
            self.confirm_remove = false;
            if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                self.remove(cx);
            }
            return Handled::Yes;
        }
        self.note = None;
        let page = |w: &Self, i: usize| w.areas[i].height.saturating_sub(1).max(1) as isize;
        match key.code {
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Feeds => Focus::List,
                    Focus::List if self.reader.is_some() => Focus::Reader,
                    _ => Focus::Feeds,
                }
            }
            KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Reader => Focus::List,
                    Focus::List => Focus::Feeds,
                    Focus::Feeds => Focus::List,
                }
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
            KeyCode::PageUp => self.move_sel(-page(self, if self.focus == Focus::Reader { 2 } else { 1 })),
            KeyCode::PageDown => self.move_sel(page(self, if self.focus == Focus::Reader { 2 } else { 1 })),
            KeyCode::Home => self.move_sel(-1_000_000),
            KeyCode::End => self.move_sel(1_000_000),
            KeyCode::Enter | KeyCode::Right => match self.focus {
                Focus::Feeds => self.focus = Focus::List,
                Focus::List => self.open_item(cx),
                Focus::Reader => self.move_sel(1),
            },
            KeyCode::Char(' ') => {
                let more =
                    self.reader.as_ref().is_some_and(|(_, s, n)| (*s as usize + self.areas[2].height as usize) < *n);
                if more {
                    let h = self.areas[2].height.saturating_sub(1).max(1);
                    if let Some((_, s, _)) = &mut self.reader {
                        *s += h;
                    }
                } else {
                    let url = self.subs.get(self.fsel).map(|s| s.sub.url.clone()).unwrap_or_default();
                    let items = self.items();
                    let start = self.lsel + usize::from(self.reader.is_some());
                    match (start..items.len()).find(|&i| !self.is_read(&url, &items[i].id)) {
                        Some(i) => {
                            self.lsel = i;
                            self.focus = Focus::List;
                            self.open_item(cx);
                        }
                        None => self.note = Some("No more unread items in this feed".into()),
                    }
                }
            }
            KeyCode::Esc | KeyCode::Left => {
                if self.reader.take().is_some() {
                    self.focus = Focus::List;
                } else if self.focus == Focus::List {
                    self.focus = Focus::Feeds;
                } else {
                    return Handled::No;
                }
            }
            KeyCode::Char('o') => {
                let link = self
                    .reader
                    .as_ref()
                    .map(|(i, _, _)| i.link.clone())
                    .unwrap_or_else(|| self.items().get(self.lsel).and_then(|i| i.link.clone()));
                match link {
                    Some(l) => cx.open_url(l),
                    None => self.note = Some("This item has no link".into()),
                }
            }
            KeyCode::Char('u') => {
                if let Some(id) = self.items().get(self.lsel).map(|i| i.id.clone()) {
                    let url = self.subs.get(self.fsel).map(|s| s.sub.url.clone()).unwrap_or_default();
                    let r = self.is_read(&url, &id);
                    self.mark(&id, !r, cx);
                }
            }
            KeyCode::Char('m') | KeyCode::Char('c') => self.mark_all(cx),
            KeyCode::Char('a') => self.add = Some(LineInput::default()),
            KeyCode::Char('s') => {
                if let Some(u) = self.subs.get(self.fsel).filter(|s| s.temporary).map(|s| s.sub.url.clone()) {
                    self.subscribe(u, cx);
                }
            }
            KeyCode::Char('D') | KeyCode::Delete if self.focus == Focus::Feeds && !self.subs.is_empty() => {
                self.confirm_remove = true
            }
            KeyCode::Char('g') => {
                if self.focus == Focus::Feeds {
                    self.fetch_all(cx);
                } else {
                    self.fetch(self.fsel, cx);
                }
                self.note = Some("Refreshing…".into());
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
            MouseEventKind::Down(MouseButton::Left) => {
                if inside(self.areas[0]) {
                    let skip = self.fsel.saturating_sub(self.areas[0].height.saturating_sub(1) as usize);
                    let i = skip + (ev.row - self.areas[0].y) as usize;
                    if i < self.subs.len() {
                        self.focus = Focus::Feeds;
                        if i != self.fsel {
                            self.fsel = i;
                            self.lsel = 0;
                            self.reader = None;
                        }
                    }
                } else if inside(self.areas[1]) {
                    let i = self.lscroll + (ev.row - self.areas[1].y) as usize;
                    if i < self.items().len() {
                        self.focus = Focus::List;
                        self.lsel = i;
                        self.open_item(cx);
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
        let AppEvent::Module { module: FEEDS, payload } = ev else { return };
        let Some(m) = payload.downcast_ref::<Msg>() else { return };
        if m.win != self.id {
            return;
        }
        match m.kind.clone() {
            Kind::Fetched { url, feed, found } => {
                if self.note.as_deref() == Some("Refreshing…") {
                    self.note = None;
                }
                let Some(i) = self.subs.iter().position(|s| s.sub.url == url) else { return };
                let keep = self.items().get(self.lsel).map(|i| i.id.clone());
                if let Some(store) = cx.store
                    && !self.subs[i].temporary
                    && let Ok(v) = serde_json::to_string(&feed)
                {
                    let _ = store.kv_set(&cache_key(&url), &v);
                }
                let s = &mut self.subs[i];
                s.loading = false;
                s.error = None;
                s.feed = Some(feed);
                // The address was a web page: follow the feed it named from now on.
                if let Some(real) = found {
                    s.sub.url = real.clone();
                    if !s.temporary {
                        let old = url.clone();
                        cx.edit_config(move |c| {
                            if let Some(x) = c.feeds.subscriptions.iter_mut().find(|x| x.url == old) {
                                x.url = real;
                            }
                        });
                    }
                }
                if i == self.fsel
                    && let Some(k) = keep
                    && let Some(p) = self.items().iter().position(|it| it.id == k)
                {
                    self.lsel = p;
                }
            }
            Kind::Tick => self.fetch_all(cx),
            Kind::Failed { url, error } => {
                if let Some(s) = self.subs.iter_mut().find(|s| s.sub.url == url) {
                    s.loading = false;
                    s.error = Some(error.clone());
                }
                if self.subs.get(self.fsel).is_some_and(|s| s.sub.url == url) {
                    cx.notify(Notification::error(error));
                }
            }
        }
    }
}
