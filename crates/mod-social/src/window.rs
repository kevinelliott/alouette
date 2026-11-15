//! The Social window: timelines on tabs, posts as cards, a thread beside them.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use alouette_core::config::SocialAccount;
use alouette_core::event::{EventSender, ModulePayload};
use alouette_core::{AppEvent, Notification};
use alouette_net::Net;
use alouette_store::secrets::SecretStore;
use alouette_ui::ask::{Asker, Question};
use alouette_ui::window::{Cx, DrawCx, Effect, Handled, Window};
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::api::{self, Account, Client, Context, Status, Timeline};
use crate::compose::{ComposeWindow, Seed};
use crate::{MODULE, next_id};

#[derive(Clone)]
pub struct Msg {
    pub win: u64,
    pub kind: Kind,
}

#[derive(Clone)]
pub enum Kind {
    SignedIn {
        token: String,
        me: Account,
    },
    Failed(String),
    Note(String),
    /// New items for a timeline. `older` means they go at the end.
    Items {
        tab: Timeline,
        items: Vec<Item>,
        older: bool,
    },
    Thread {
        id: String,
        ctx: Context,
    },
    Updated(Status),
    OpFailed(String),
    Tick,
    Posted(Result<Status, String>),
}

pub fn send(events: &EventSender, win: u64, kind: Kind) {
    let _ = events.send(AppEvent::Module { module: MODULE, payload: ModulePayload::new(Msg { win, kind }) });
}

/// One row of a timeline: a post, or a notification about one.
#[derive(Debug, Clone)]
pub struct Item {
    pub id: String,
    pub status: Option<Status>,
    /// "Ada favourited your post" and the like.
    pub note: Option<String>,
}

impl Item {
    fn from_status(s: Status) -> Self {
        Self { id: s.id.clone(), status: Some(s), note: None }
    }
    fn from_notification(n: api::Notification) -> Self {
        let who = n.account.name().to_string();
        let what = match n.kind.as_str() {
            "favourite" => "favourited your post",
            "reblog" => "boosted your post",
            "mention" => "mentioned you",
            "follow" => "followed you",
            "follow_request" => "asked to follow you",
            "poll" => "a poll you took part in has ended",
            "status" => "posted",
            "update" => "edited a post",
            _ => "did something",
        };
        Self { id: n.id, note: Some(format!("{who} {what}")), status: n.status }
    }
}

fn token_key(name: &str) -> String {
    format!("social/{name}/token")
}
fn app_key(name: &str) -> String {
    format!("social/{name}/app")
}

enum State {
    SigningIn,
    Ready,
    Failed(String),
}

pub struct SocialWindow {
    id: u64,
    account: SocialAccount,
    net: Net,
    token: Option<String>,
    me: Option<Account>,
    state: State,
    asker: Asker,
    typed: String,
    signin: Option<String>,
    tab: Timeline,
    items: HashMap<&'static str, Vec<Item>>,
    sel: HashMap<&'static str, usize>,
    scroll: usize,
    thread: Option<(String, Vec<Status>, u16)>,
    unfolded: std::collections::HashSet<String>,
    fold: bool,
    loading: bool,
    note: Option<String>,
    alive: Arc<AtomicBool>,
    /// Rows of the list: (first line, item index) for clicks.
    hits: Vec<(u16, usize)>,
    areas: [Rect; 3],
    /// Whether this account is new and should be saved once signed in.
    save_account: bool,
}

impl Drop for SocialWindow {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Relaxed);
        self.asker.answer(None);
    }
}

impl SocialWindow {
    pub fn new(account: SocialAccount, save_account: bool, cx: &mut Cx) -> Self {
        let wake = cx.tasks.events();
        let asker = Asker::new(move || {
            let _ = wake.send(AppEvent::Module { module: MODULE, payload: ModulePayload::new(()) });
        });
        let mut w = Self {
            id: next_id(),
            account,
            net: cx.net.clone(),
            token: None,
            me: None,
            state: State::SigningIn,
            asker,
            typed: String::new(),
            signin: None,
            tab: Timeline::Home,
            items: HashMap::new(),
            sel: HashMap::new(),
            scroll: 0,
            thread: None,
            unfolded: Default::default(),
            fold: cx.config.social.fold_content_warnings,
            loading: false,
            note: None,
            alive: Arc::new(AtomicBool::new(true)),
            hits: vec![],
            areas: [Rect::default(); 3],
            save_account,
        };
        w.sign_in(cx);
        let (alive, id, events) = (w.alive.clone(), w.id, cx.tasks.events());
        let every = std::time::Duration::from_secs(cx.config.social.poll_secs.max(15));
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                if !alive.load(Ordering::Relaxed) {
                    return;
                }
                send(&events, id, Kind::Tick);
            }
        });
        w
    }

    fn client(&self) -> Client {
        Client::new(self.net.clone(), &self.account.instance, self.token.clone())
    }

    fn sign_in(&mut self, cx: &Cx) {
        self.state = State::SigningIn;
        let (id, events, net) = (self.id, cx.tasks.events(), self.net.clone());
        let secrets: Arc<dyn SecretStore> = cx.secrets.clone();
        let (asker, acct) = (self.asker.clone(), self.account.clone());
        tokio::spawn(async move {
            let r = sign_in(net, &acct, secrets, asker, &events, id).await;
            send(
                &events,
                id,
                match r {
                    Ok((token, me)) => Kind::SignedIn { token, me },
                    Err(e) => Kind::Failed(e),
                },
            );
        });
    }

    fn key(t: Timeline) -> &'static str {
        t.label()
    }

    fn list(&self) -> &[Item] {
        self.items.get(Self::key(self.tab)).map(Vec::as_slice).unwrap_or(&[])
    }

    fn selected(&self) -> usize {
        self.sel.get(Self::key(self.tab)).copied().unwrap_or(0).min(self.list().len().saturating_sub(1))
    }

    fn current(&self) -> Option<&Status> {
        self.list().get(self.selected()).and_then(|i| i.status.as_ref())
    }

    fn load(&mut self, tab: Timeline, older: bool, cx: &Cx) {
        if self.token.is_none() {
            return;
        }
        let list = self.items.get(Self::key(tab));
        let since = if older { None } else { list.and_then(|l| l.first()).map(|i| i.id.clone()) };
        let max = if older { list.and_then(|l| l.last()).map(|i| i.id.clone()) } else { None };
        if older && max.is_none() {
            return;
        }
        self.loading = true;
        let (id, events, c) = (self.id, cx.tasks.events(), self.client());
        tokio::spawn(async move {
            let r = match tab {
                Timeline::Notifications => c
                    .notifications(since.as_deref())
                    .await
                    .map(|n| n.into_iter().map(Item::from_notification).collect()),
                t => c
                    .timeline(t, since.as_deref(), max.as_deref())
                    .await
                    .map(|s| s.into_iter().map(Item::from_status).collect()),
            };
            send(
                &events,
                id,
                match r {
                    Ok(items) => Kind::Items { tab, items, older },
                    Err(e) => Kind::OpFailed(e),
                },
            );
        });
    }

    fn switch(&mut self, tab: Timeline, cx: &Cx) {
        self.tab = tab;
        self.scroll = 0;
        self.thread = None;
        if !self.items.contains_key(Self::key(tab)) {
            self.load(tab, false, cx);
        }
    }

    fn open_thread(&mut self, cx: &Cx) {
        let Some(s) = self.current().map(|s| s.shown().clone()) else { return };
        let sid = s.id.clone();
        self.thread = Some((sid.clone(), vec![s], 0));
        let (id, events, c) = (self.id, cx.tasks.events(), self.client());
        tokio::spawn(async move {
            send(
                &events,
                id,
                match c.context(&sid).await {
                    Ok(ctx) => Kind::Thread { id: sid, ctx },
                    Err(e) => Kind::OpFailed(e),
                },
            );
        });
    }

    fn act(&mut self, what: &'static str, cx: &Cx) {
        let Some(s) = self.current().map(|s| s.shown().clone()) else { return };
        let on = match what {
            "favourite" => !s.favourited.unwrap_or(false),
            _ => !s.reblogged.unwrap_or(false),
        };
        // Show it at once; the server's answer corrects it.
        self.update(
            |x| {
                if what == "favourite" {
                    x.favourited = Some(on);
                    x.favourites_count = if on { x.favourites_count + 1 } else { x.favourites_count.saturating_sub(1) };
                } else {
                    x.reblogged = Some(on);
                    x.reblogs_count = if on { x.reblogs_count + 1 } else { x.reblogs_count.saturating_sub(1) };
                }
            },
            &s.id,
        );
        let (id, events, c) = (self.id, cx.tasks.events(), self.client());
        tokio::spawn(async move {
            send(
                &events,
                id,
                match c.act(&s.id, what, on).await {
                    Ok(st) => Kind::Updated(st),
                    Err(e) => Kind::OpFailed(e),
                },
            );
        });
    }

    /// Change every copy of post `id` (including inside boosts).
    fn update(&mut self, f: impl Fn(&mut Status), id: &str) {
        for list in self.items.values_mut() {
            for it in list.iter_mut() {
                if let Some(s) = &mut it.status {
                    if s.id == id {
                        f(s);
                    }
                    if let Some(r) = &mut s.reblog
                        && r.id == id
                    {
                        f(r);
                    }
                }
            }
        }
        if let Some((_, posts, _)) = &mut self.thread {
            for p in posts.iter_mut().filter(|p| p.id == id) {
                f(p);
            }
        }
    }

    fn compose(&mut self, reply: bool, cx: &mut Cx) {
        let Some(token) = self.token.clone() else {
            self.note = Some("Sign in first".into());
            return;
        };
        let mut seed = Seed { visibility: "public".into(), ..Default::default() };
        if reply {
            let Some(s) = self.current().map(|s| s.shown().clone()) else { return };
            let me = self.me.as_ref().map(|m| m.acct.clone()).unwrap_or_default();
            let mut mentions = vec![format!("@{}", s.account.acct)];
            // Others mentioned in the post, from its links.
            for word in s.text().split_whitespace() {
                let w = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '@' && c != '.' && c != '_');
                if w.starts_with('@')
                    && w.len() > 1
                    && !mentions.iter().any(|m| m == w)
                    && w.trim_start_matches('@') != me
                {
                    mentions.push(w.to_string());
                }
            }
            mentions.retain(|m| m.trim_start_matches('@') != me);
            seed.text = format!("{} ", mentions.join(" "));
            seed.reply_to = Some((
                s.id.clone(),
                format!("{}: {}", s.account.name(), s.text().chars().take(60).collect::<String>()),
            ));
            seed.spoiler = s.spoiler_text.clone();
            seed.visibility = if s.visibility.is_empty() { "public".into() } else { s.visibility.clone() };
        }
        let w = ComposeWindow::new(self.net.clone(), self.account.clone(), token, seed, self.id);
        cx.effects.push(Effect::OpenWindow(Box::new(w)));
    }

    /// The lines a post takes in the list, for a given width.
    fn card(&self, it: &Item, width: u16, cx: &DrawCx, selected: bool) -> Vec<Line<'static>> {
        let t = cx.theme;
        let mut out = vec![];
        if let Some(n) = &it.note {
            out.push(Line::from(Span::styled(format!("◆ {n}"), Style::new().fg(t.accent))));
        }
        let Some(st) = &it.status else {
            out.push(Line::default());
            return out;
        };
        if st.reblog.is_some() {
            out.push(Line::from(Span::styled(format!("↻ {} boosted", st.account.name()), t.dim())));
        }
        let s = st.shown();
        let when = s.time().map(crate::ago).unwrap_or_default();
        out.push(Line::from(vec![
            Span::styled(s.account.name().to_string(), Style::new().add_modifier(Modifier::BOLD)),
            Span::styled(format!("  @{}  · {when}", s.account.acct), t.dim()),
        ]));
        let folded = self.fold && !s.spoiler_text.is_empty() && !self.unfolded.contains(&s.id);
        if !s.spoiler_text.is_empty() {
            out.push(Line::from(Span::styled(
                format!("⚠ {}{}", s.spoiler_text, if folded { "  (Enter shows it)" } else { "" }),
                Style::new().fg(t.warning),
            )));
        }
        if !folded {
            let text = s.text();
            let w = width.max(10) as usize;
            for l in text.lines() {
                // Wrap here so heights are known for scrolling.
                for piece in wrap(l, w) {
                    out.push(Line::from(piece));
                }
            }
            for m in &s.media_attachments {
                let d = m.description.clone().filter(|d| !d.is_empty()).unwrap_or_else(|| "no description".into());
                out.push(Line::from(Span::styled(format!("▣ {}: {d}", m.kind), t.dim())));
            }
        }
        let fav = if s.favourited == Some(true) { "★" } else { "☆" };
        let boost = if s.reblogged == Some(true) { "↻" } else { "⟲" };
        let _ = selected;
        out.push(Line::from(Span::styled(
            format!("↩ {}   {boost} {}   {fav} {}", s.replies_count, s.reblogs_count, s.favourites_count),
            t.dim(),
        )));
        out.push(Line::default());
        out
    }

    fn draw_tabs(&self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        let mut spans = vec![Span::raw(" ")];
        for (i, tab) in Timeline::ALL.iter().enumerate() {
            let st = if *tab == self.tab { t.selection() } else { t.dim() };
            spans.push(Span::styled(format!(" {} {} ", i + 1, tab.label()), st));
            spans.push(Span::raw(" "));
        }
        if let Some(me) = &self.me {
            spans.push(Span::styled(format!("  @{}", me.acct), t.dim()));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn draw_list(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        self.areas[1] = area;
        self.hits.clear();
        if let Some(n) = &self.signin {
            let mut lines = vec![Line::from(Span::styled("Sign in", t.accent())), Line::default()];
            lines.extend(n.lines().map(|l| Line::from(l.to_string())));
            let p = Paragraph::new(lines).wrap(Wrap { trim: false });
            frame.render_widget(p, area);
            return;
        }
        let list = self.list().to_vec();
        if list.is_empty() {
            let msg = match &self.state {
                State::SigningIn => "Signing in…".to_string(),
                State::Failed(e) => format!("Could not sign in: {e}\n\nPress g to try again."),
                State::Ready if self.loading => "Loading…".into(),
                State::Ready => "Nothing here yet.".into(),
            };
            frame.render_widget(Paragraph::new(msg).style(t.dim()).wrap(Wrap { trim: false }), area);
            return;
        }
        let sel = self.selected();
        let focused = self.thread.is_none() && cx.focused;
        let cards: Vec<Vec<Line>> =
            list.iter().enumerate().map(|(i, it)| self.card(it, area.width.saturating_sub(2), cx, i == sel)).collect();
        // Scroll so the selected card is in view.
        let tops: Vec<usize> = cards
            .iter()
            .scan(0, |acc, c| {
                let top = *acc;
                *acc += c.len();
                Some(top)
            })
            .collect();
        let h = area.height as usize;
        let (top, len) = (tops[sel], cards[sel].len());
        if top < self.scroll {
            self.scroll = top;
        }
        if top + len.min(h) > self.scroll + h {
            self.scroll = top + len.min(h) - h;
        }
        let mut lines = vec![];
        for (i, c) in cards.into_iter().enumerate() {
            let y = tops[i] as isize - self.scroll as isize;
            if y >= 0 && (y as usize) < h {
                self.hits.push((area.y + y as u16, i));
            }
            let marker = if i == sel { if focused { "▌" } else { "│" } } else { " " };
            for l in c {
                let mut spans = vec![Span::styled(
                    format!("{marker} "),
                    if i == sel { Style::new().fg(t.accent) } else { Style::new() },
                )];
                spans.extend(l.spans);
                lines.push(Line::from(spans));
            }
        }
        let para = Paragraph::new(lines).scroll((self.scroll as u16, 0));
        frame.render_widget(para, area);
    }

    fn draw_thread(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        let block = Block::default()
            .borders(Borders::LEFT)
            .border_style(t.accent())
            .title(Span::styled(" Thread ", t.accent()));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        self.areas[2] = inner;
        let Some((focus_id, posts, scroll)) = &self.thread else { return };
        let mut lines = vec![];
        for p in posts {
            let me = p.id == *focus_id;
            let name = Style::new().add_modifier(Modifier::BOLD);
            lines.push(Line::from(vec![
                Span::styled(if me { "▶ " } else { "  " }, Style::new().fg(t.accent)),
                Span::styled(p.account.name().to_string(), name),
                Span::styled(
                    format!(" @{} · {}", p.account.acct, p.time().map(crate::ago).unwrap_or_default()),
                    t.dim(),
                ),
            ]));
            if !p.spoiler_text.is_empty() {
                lines.push(Line::from(Span::styled(format!("⚠ {}", p.spoiler_text), Style::new().fg(t.warning))));
            }
            for l in p.text().lines() {
                lines.push(Line::from(format!("  {l}")));
            }
            lines.push(Line::default());
        }
        let scroll = *scroll;
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((scroll, 0)), inner);
    }
}

/// Break a line at spaces to fit `w` columns.
fn wrap(s: &str, w: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthStr;
    if s.width() <= w {
        return vec![s.to_string()];
    }
    let mut out = vec![];
    let mut cur = String::new();
    for word in s.split(' ') {
        if !cur.is_empty() && cur.width() + 1 + word.width() > w {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        // Very long words (links) are cut.
        let mut word = word.to_string();
        while word.width() > w {
            let cut: String = word.chars().take(w).collect();
            word = word.chars().skip(cut.chars().count()).collect();
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            out.push(cut);
        }
        cur.push_str(&word);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

async fn sign_in(
    net: Net,
    acct: &SocialAccount,
    secrets: Arc<dyn SecretStore>,
    asker: Asker,
    events: &EventSender,
    win: u64,
) -> Result<(String, Account), String> {
    if let Some(t) = secrets.get(&token_key(&acct.name)).ok().flatten() {
        let c = Client::new(net.clone(), &acct.instance, Some(t.clone()));
        match c.me().await {
            Ok(me) => return Ok((t, me)),
            Err(e) if e.contains("token") || e.contains("401") || e.contains("not authorized") => {
                let _ = secrets.delete(&token_key(&acct.name));
            }
            Err(e) => return Err(e),
        }
    }
    let c = Client::new(net.clone(), &acct.instance, None);
    let pasted = asker
        .ask(Question {
            title: format!("Sign in to {}", api::base(&acct.instance)),
            body: vec![
                "Paste an access token from your server's Preferences > Development,".into(),
                "or leave this empty and press Enter to sign in through your browser.".into(),
            ],
            prompt: "Token".into(),
            secret: true,
            yes_no: false,
        })
        .await
        .ok_or("signing in was cancelled")?;
    let token = if !pasted.trim().is_empty() {
        pasted.trim().to_string()
    } else {
        let (cid, secret) = match secrets
            .get(&app_key(&acct.name))
            .ok()
            .flatten()
            .and_then(|v| v.split_once('\n').map(|(a, b)| (a.to_string(), b.to_string())))
        {
            Some(p) => p,
            None => {
                let p = c.register().await?;
                let _ = secrets.set(&app_key(&acct.name), &format!("{}\n{}", p.0, p.1));
                p
            }
        };
        let url = c.authorize_url(&cid);
        send(
            events,
            win,
            Kind::Note(format!(
                "Open this address in your browser, approve Alouette, and paste the code it shows:\n\n{url}"
            )),
        );
        let code = asker
            .ask(Question {
                title: "Authorization code".into(),
                body: vec!["Paste the code from the browser.".into()],
                prompt: "Code".into(),
                secret: false,
                yes_no: false,
            })
            .await
            .ok_or("signing in was cancelled")?;
        c.token(&cid, &secret, &code).await?
    };
    let me = Client::new(net, &acct.instance, Some(token.clone())).me().await?;
    let _ = secrets.set(&token_key(&acct.name), &token);
    Ok((token, me))
}

impl Window for SocialWindow {
    fn title(&self) -> String {
        format!("Social: {} · {}", self.account.name, self.tab.label())
    }
    fn module(&self) -> &'static str {
        MODULE
    }
    fn url(&self) -> Option<String> {
        Some(format!("social:{}", self.account.name))
    }
    fn wants_text(&self) -> bool {
        self.asker.question().is_some()
    }
    fn hints(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("1-4", "Timeline"),
            ("Enter", "Thread"),
            ("c", "Post"),
            ("r", "Reply"),
            ("f", "Favourite"),
            ("b", "Boost"),
            ("o", "Open"),
            ("g", "Refresh"),
        ]
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        if area.height < 5 || area.width < 24 {
            return;
        }
        let t = cx.theme;
        self.draw_tabs(frame, Rect { height: 1, ..area }, cx);
        let foot = Rect { y: area.bottom() - 1, height: 1, ..area };
        let body = Rect { y: area.y + 2, height: area.height.saturating_sub(3), ..area };
        if self.thread.is_some() && body.width >= 60 {
            let lw = body.width / 2;
            self.draw_list(frame, Rect { width: lw, ..body }, cx);
            self.draw_thread(frame, Rect { x: body.x + lw, width: body.width - lw, ..body }, cx);
        } else if self.thread.is_some() {
            self.draw_thread(frame, body, cx);
        } else {
            self.draw_list(frame, body, cx);
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
            self.typed.push_str(text.trim());
            return Handled::Yes;
        }
        Handled::No
    }

    fn on_key(&mut self, key: KeyEvent, cx: &mut Cx) -> Handled {
        if let Some(q) = self.asker.question() {
            alouette_ui::ask::on_key(&self.asker, &q, &mut self.typed, key);
            return Handled::Yes;
        }
        self.note = None;
        let k = Self::key(self.tab);
        let n = self.list().len();
        let mv = |w: &mut Self, d: isize| {
            if let Some((_, _, s)) = &mut w.thread {
                *s = (*s as isize + d).max(0) as u16;
                return;
            }
            let cur = w.selected() as isize;
            let next = (cur + d).clamp(0, n.saturating_sub(1) as isize) as usize;
            w.sel.insert(k, next);
        };
        match key.code {
            KeyCode::Char(c @ '1'..='4') => {
                let t = Timeline::ALL[(c as u8 - b'1') as usize];
                self.switch(t, cx);
            }
            KeyCode::Tab => {
                let i = Timeline::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
                self.switch(Timeline::ALL[(i + 1) % 4], cx);
            }
            KeyCode::BackTab => {
                let i = Timeline::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
                self.switch(Timeline::ALL[(i + 3) % 4], cx);
            }
            KeyCode::Up | KeyCode::Char('k') => mv(self, -1),
            KeyCode::Down | KeyCode::Char('j') => {
                mv(self, 1);
                // Near the end: fetch older posts.
                if self.thread.is_none() && self.selected() + 3 >= n && !self.loading {
                    self.load(self.tab, true, cx);
                }
            }
            KeyCode::PageUp => mv(self, -5),
            KeyCode::PageDown => mv(self, 5),
            KeyCode::Home => mv(self, -100_000),
            KeyCode::End => mv(self, 100_000),
            KeyCode::Enter => {
                if let Some(s) = self.current().map(|s| s.shown().clone())
                    && self.fold
                    && !s.spoiler_text.is_empty()
                    && !self.unfolded.contains(&s.id)
                {
                    self.unfolded.insert(s.id);
                } else {
                    self.open_thread(cx);
                }
            }
            KeyCode::Esc | KeyCode::Left => {
                if self.thread.take().is_none() {
                    return Handled::No;
                }
            }
            KeyCode::Char('c') | KeyCode::Char('n') => self.compose(false, cx),
            KeyCode::Char('r') => self.compose(true, cx),
            KeyCode::Char('f') | KeyCode::Char('s') => self.act("favourite", cx),
            KeyCode::Char('b') => self.act("reblog", cx),
            KeyCode::Char('o') => match self.current().and_then(|s| s.shown().url.clone()) {
                Some(u) => cx.open_url(u),
                None => self.note = Some("This post has no link".into()),
            },
            KeyCode::Char('g') => {
                if matches!(self.state, State::Failed(_)) {
                    self.sign_in(cx);
                } else {
                    self.load(self.tab, false, cx);
                    self.note = Some("Refreshing…".into());
                }
            }
            _ => return Handled::No,
        }
        Handled::Yes
    }

    fn on_mouse(&mut self, ev: MouseEvent, _area: Rect, cx: &mut Cx) -> Handled {
        match ev.kind {
            MouseEventKind::ScrollUp => {
                self.scroll = self.scroll.saturating_sub(3);
            }
            MouseEventKind::ScrollDown => self.scroll += 3,
            MouseEventKind::Down(MouseButton::Left) => {
                if ev.row == self.areas[1].y.saturating_sub(2) {
                    // The tab row.
                    let mut x = 1u16;
                    for tab in Timeline::ALL.iter().enumerate() {
                        let w = tab.1.label().len() as u16 + 4;
                        if ev.column >= x && ev.column < x + w {
                            self.switch(*tab.1, cx);
                            break;
                        }
                        x += w + 1;
                    }
                    return Handled::Yes;
                }
                let r = self.areas[1];
                if ev.column >= r.x
                    && ev.column < r.right()
                    && ev.row >= r.y
                    && ev.row < r.bottom()
                    && let Some(&(_, i)) = self.hits.iter().rev().find(|(y, _)| *y <= ev.row)
                {
                    let again = self.selected() == i;
                    self.sel.insert(Self::key(self.tab), i);
                    if again {
                        self.open_thread(cx);
                    }
                }
            }
            _ => return Handled::No,
        }
        Handled::Yes
    }

    fn on_event(&mut self, ev: &AppEvent, cx: &mut Cx) {
        let AppEvent::Module { module: MODULE, payload } = ev else { return };
        let Some(m) = payload.downcast_ref::<Msg>() else { return };
        if m.win != self.id {
            return;
        }
        match m.kind.clone() {
            Kind::SignedIn { token, me } => {
                self.token = Some(token);
                self.note = Some(format!("Signed in as @{}", me.acct));
                self.me = Some(me);
                self.state = State::Ready;
                self.signin = None;
                if self.save_account {
                    self.save_account = false;
                    let a = self.account.clone();
                    cx.edit_config(move |c| {
                        if !c.social.accounts.iter().any(|x| x.name == a.name) {
                            c.social.accounts.push(a);
                        }
                    });
                }
                self.load(self.tab, false, cx);
            }
            Kind::Failed(e) => {
                self.signin = None;
                self.note = Some(e.clone());
                self.state = State::Failed(e);
            }
            Kind::Note(n) => self.signin = Some(n),
            Kind::Items { tab, items, older } => {
                self.loading = false;
                if self.note.as_deref() == Some("Refreshing…") {
                    self.note = None;
                }
                let k = Self::key(tab);
                let list = self.items.entry(k).or_default();
                let added = items.len();
                if older {
                    list.extend(items);
                } else {
                    let mut new = items;
                    new.append(list);
                    *list = new;
                    // Keep the same post selected.
                    if let Some(s) = self.sel.get_mut(k)
                        && *s > 0
                    {
                        *s += added;
                    }
                }
                list.truncate(1000);
            }
            Kind::Thread { id, ctx } => {
                if let Some((fid, posts, _)) = &mut self.thread
                    && *fid == id
                {
                    let me = posts.first().cloned();
                    let mut all = ctx.ancestors;
                    all.extend(me);
                    all.extend(ctx.descendants);
                    *posts = all;
                }
            }
            Kind::Updated(s) => {
                let id = s.id.clone();
                let s2 = s.clone();
                self.update(
                    move |x| {
                        x.favourited = s2.favourited;
                        x.reblogged = s2.reblogged;
                        x.favourites_count = s2.favourites_count;
                        x.reblogs_count = s2.reblogs_count;
                    },
                    &id,
                );
                if let Some(r) = &s.reblog {
                    let r2 = (**r).clone();
                    self.update(
                        move |x| {
                            x.reblogged = r2.reblogged;
                            x.reblogs_count = r2.reblogs_count;
                        },
                        &r.id,
                    );
                }
            }
            Kind::OpFailed(e) => {
                self.loading = false;
                self.note = Some(e.clone());
                cx.notify(Notification::error(e));
            }
            Kind::Tick => {
                if matches!(self.state, State::Ready) {
                    self.load(self.tab, false, cx);
                }
            }
            Kind::Posted(r) => match r {
                Ok(_) => {
                    self.note = Some("Posted".into());
                    self.load(Timeline::Home, false, cx);
                }
                Err(e) => self.note = Some(e),
            },
        }
    }
}
