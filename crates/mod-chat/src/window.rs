//! The Chat window: buffers on the left, the conversation in the middle, people on the
//! right, and a line to type in at the bottom.

use alouette_core::AppEvent;
use alouette_core::config::{IrcNetwork, MatrixAccount};
use alouette_ui::input::{InputResult, LineInput};
use alouette_ui::window::{Cx, DrawCx, Handled, Window};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::conn::{self, Ctx};
use crate::hub::{BufKind, ChatLine, Hub, LineKind, Link, Network, Sel, Shared, State, local_time};
use crate::{MODULE, apply_config};

static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

const MATRIX_HELP: &str = "Matrix: type to talk, /me action, /join #room:server, /part, /verify [@user:server], \
/accept, /confirm, /mismatch, /cancel, /recover <recovery key>, /devices, /names, /logout, /quit, /reconnect.";
const CHAT_HELP: &str = "Anywhere: /server host[:port] adds an IRC network, /matrix @you:server [homeserver] \
adds a Matrix account, /connect [network], /users shows or hides people, /clear, /close, /help. \
Alt-Up and Alt-Down (or Ctrl-P and Ctrl-N) change buffer; Tab completes nicks; PgUp and PgDn scroll.";

pub struct ChatWindow {
    id: u64,
    hub: Shared,
    ctx: Ctx,
    sel: Sel,
    input: LineInput,
    history: Vec<String>,
    hist_pos: Option<usize>,
    /// Rows scrolled up from the bottom.
    scroll: usize,
    show_users: bool,
    typed: String,
    /// Sidebar rows: (screen row, selection) for clicks.
    rows: Vec<(u16, Sel)>,
    side: Rect,
    body: Rect,
    /// Tab completion: the word being completed and the last pick.
    completing: Option<(String, usize)>,
}

impl Drop for ChatWindow {
    fn drop(&mut self) {
        if let Ok(mut h) = self.hub.lock() {
            h.viewing.remove(&self.id);
        }
    }
}

/// A colour for a nick, the same every time.
fn nick_color(nick: &str) -> Color {
    const PALETTE: [Color; 10] = [
        Color::Cyan,
        Color::Green,
        Color::Yellow,
        Color::Magenta,
        Color::Blue,
        Color::LightRed,
        Color::LightGreen,
        Color::LightCyan,
        Color::LightMagenta,
        Color::LightBlue,
    ];
    let h = nick.bytes().fold(5381u32, |h, b| h.wrapping_mul(33) ^ b as u32);
    PALETTE[(h % PALETTE.len() as u32) as usize]
}

/// Styled runs for a line's text, from mIRC formatting.
fn styled(text: &str, base: Style, mono: bool) -> Vec<(String, Style)> {
    proto_irc::format::spans(text)
        .into_iter()
        .map(|(t, f)| {
            let mut s = base;
            if f.bold {
                s = s.add_modifier(Modifier::BOLD);
            }
            if f.italic {
                s = s.add_modifier(Modifier::ITALIC);
            }
            if f.underline {
                s = s.add_modifier(Modifier::UNDERLINED);
            }
            if f.strike {
                s = s.add_modifier(Modifier::CROSSED_OUT);
            }
            if f.reverse {
                s = s.add_modifier(Modifier::REVERSED);
            }
            if !mono {
                if let Some((r, g, b)) = f.fg_rgb {
                    s = s.fg(Color::Rgb(r, g, b));
                } else if let Some(c) = f.fg {
                    let (r, g, b) = proto_irc::format::rgb(c);
                    s = s.fg(Color::Rgb(r, g, b));
                }
                if let Some(c) = f.bg {
                    let (r, g, b) = proto_irc::format::rgb(c);
                    s = s.bg(Color::Rgb(r, g, b));
                }
            }
            (t, s)
        })
        .collect()
}

/// Wrap styled runs to `width` columns, breaking at spaces where possible.
pub fn wrap(runs: &[(String, Style)], width: usize) -> Vec<Vec<(String, Style)>> {
    let width = width.max(4);
    let mut rows: Vec<Vec<(String, Style)>> = vec![vec![]];
    let mut col = 0;
    // Words keep their trailing space.
    let mut words: Vec<(String, Style)> = vec![];
    for (t, s) in runs {
        let mut cur = String::new();
        for c in t.chars() {
            cur.push(c);
            if c == ' ' {
                words.push((std::mem::take(&mut cur), *s));
            }
        }
        if !cur.is_empty() {
            words.push((cur, *s));
        }
    }
    let push = |rows: &mut Vec<Vec<(String, Style)>>, t: String, s: Style| {
        let row = rows.last_mut().expect("a row");
        match row.last_mut() {
            Some((lt, ls)) if *ls == s => lt.push_str(&t),
            _ => row.push((t, s)),
        }
    };
    for (w, s) in words {
        let ww = w.trim_end().width();
        if col + ww > width && col > 0 {
            rows.push(vec![]);
            col = 0;
        }
        if ww > width {
            for c in w.chars() {
                let cw = c.width().unwrap_or(0);
                if col + cw > width {
                    rows.push(vec![]);
                    col = 0;
                }
                push(&mut rows, c.to_string(), s);
                col += cw;
            }
            continue;
        }
        col += w.width();
        push(&mut rows, w, s);
    }
    rows
}

impl ChatWindow {
    pub fn new(hub: Shared, ctx: Ctx, cx: &mut Cx) -> Self {
        let mut w = Self {
            id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            hub,
            ctx,
            sel: Sel::default(),
            input: LineInput::default(),
            history: vec![],
            hist_pos: None,
            scroll: 0,
            show_users: true,
            typed: String::new(),
            rows: vec![],
            side: Rect::default(),
            body: Rect::default(),
            completing: None,
        };
        w.follow_focus();
        if w.hub.lock().map(|h| h.nets.is_empty()).unwrap_or(true) {
            w.note(
                LineKind::Info,
                "No networks yet. Try /server irc.libera.chat, or /matrix @you:matrix.org, or open ircs://irc.libera.chat/#alouette",
            );
        }
        let _ = cx;
        w
    }

    /// Jump to a buffer a link asked for, once it exists.
    fn follow_focus(&mut self) {
        let Ok(mut h) = self.hub.lock() else { return };
        let Some((net, key)) = h.focus.clone() else { return };
        let Some(n) = h.net(&net) else {
            h.focus = None;
            return;
        };
        let b = if key.is_empty() {
            Some(0)
        } else {
            h.nets[n].find(&key).or_else(|| h.nets[n].buffers.iter().position(|b| b.name.eq_ignore_ascii_case(&key)))
        };
        if let Some(b) = b {
            self.sel = Sel { net: n, buf: b };
            self.scroll = 0;
            h.focus = None;
        } else if self.sel == Sel::default() {
            self.sel = Sel { net: n, buf: 0 };
        }
    }

    /// Every buffer in sidebar order.
    fn order(h: &Hub) -> Vec<Sel> {
        let mut v = vec![];
        for (ni, n) in h.nets.iter().enumerate() {
            for bi in 0..n.buffers.len() {
                v.push(Sel { net: ni, buf: bi });
            }
        }
        v
    }

    fn select(&mut self, sel: Sel) {
        self.sel = sel;
        self.scroll = 0;
        self.completing = None;
        let Ok(mut h) = self.hub.lock() else { return };
        h.read(sel);
        // A Matrix room: fetch some history and the people the first time it is shown.
        if let Some(n) = h.nets.get_mut(sel.net)
            && let Link::Matrix { tx: Some(tx) } = &n.link
            && let Some(b) = n.buffers.get_mut(sel.buf)
            && b.kind == BufKind::Room
            && !b.backfilled
        {
            b.backfilled = true;
            let _ = tx.send(alouette_matrix::Cmd::History(b.key.clone()));
            let _ = tx.send(alouette_matrix::Cmd::Members(b.key.clone()));
        }
    }

    fn step(&mut self, d: isize) {
        let next = {
            let Ok(h) = self.hub.lock() else { return };
            let order = Self::order(&h);
            if order.is_empty() {
                return;
            }
            let i = order.iter().position(|s| *s == self.sel).unwrap_or(0) as isize;
            order[(i + d).rem_euclid(order.len() as isize) as usize]
        };
        self.select(next);
    }

    /// A line in the current buffer, or the first network's if there is none.
    fn note(&mut self, kind: LineKind, text: impl Into<String>) {
        let Ok(mut h) = self.hub.lock() else { return };
        if h.nets.is_empty() {
            h.nets.push(Network::new("Alouette", Link::Irc { session: None, tx: None }));
        }
        let line = ChatLine {
            time: proto_irc::session::now_ms(),
            kind,
            from: String::new(),
            text: text.into(),
            own: false,
            highlight: false,
        };
        let sel = if self.sel.net < h.nets.len() { self.sel } else { Sel::default() };
        h.push(sel.net, sel.buf, line, false);
    }

    fn current(&self) -> Option<(String, bool, Option<String>, BufKind)> {
        let h = self.hub.lock().ok()?;
        let n = h.nets.get(self.sel.net)?;
        let b = n.buffers.get(self.sel.buf)?;
        Some((n.name.clone(), n.is_matrix(), (b.kind != BufKind::Server).then(|| b.key.clone()), b.kind))
    }

    /// Commands that work in any buffer. Returns true when handled.
    fn global(&mut self, line: &str, cx: &mut Cx) -> bool {
        let (cmd, args) = match line.strip_prefix('/') {
            Some(r) if !r.starts_with('/') => {
                let (c, a) = r.split_once(' ').unwrap_or((r, ""));
                (c.to_ascii_lowercase(), a.trim().to_string())
            }
            _ => return false,
        };
        let words: Vec<String> = args.split_whitespace().map(str::to_string).collect();
        match cmd.as_str() {
            "help" => {
                let matrix = self.current().is_some_and(|c| c.1);
                self.note(LineKind::Info, CHAT_HELP);
                self.note(LineKind::Info, if matrix { MATRIX_HELP } else { proto_irc::command::HELP });
            }
            "users" | "nicklist" => self.show_users = !self.show_users,
            "clear" => self.clear_buffer(),
            "server" => {
                let Some(host) = words.first() else {
                    self.note(LineKind::Error, "/server irc.example.org[:port] [nick]");
                    return true;
                };
                let (name, _, _) = proto_irc::endpoint(host, None);
                let cfg = IrcNetwork {
                    name: name.clone(),
                    host: host.clone(),
                    nick: words.get(1).cloned().unwrap_or_default(),
                    ..Default::default()
                };
                self.add_network(Network::from_irc(&cfg), cx);
                let saved = cfg.clone();
                cx.edit_config(move |c| {
                    if !c.chat.irc.iter().any(|n| n.name == saved.name) {
                        c.chat.irc.push(saved);
                    }
                });
                conn::connect(&self.ctx, &name);
            }
            "matrix" => {
                let Some(user) = words.first().filter(|u| u.starts_with('@') && u.contains(':')) else {
                    self.note(LineKind::Error, "/matrix @you:example.org [homeserver URL]");
                    return true;
                };
                let server = user.split_once(':').map(|(_, s)| s.to_string()).unwrap_or_default();
                let cfg = MatrixAccount {
                    name: server.clone(),
                    homeserver: words.get(1).cloned().unwrap_or(server.clone()),
                    user: user.clone(),
                    autoconnect: true,
                };
                self.add_network(Network::from_matrix(&cfg), cx);
                let saved = cfg.clone();
                cx.edit_config(move |c| {
                    if !c.chat.matrix.iter().any(|n| n.name == saved.name) {
                        c.chat.matrix.push(saved);
                    }
                });
                conn::connect(&self.ctx, &server);
            }
            "connect" if !words.is_empty() || self.current().is_some() => {
                let name = match words.first() {
                    Some(w) => w.clone(),
                    None => self.current().map(|c| c.0).unwrap_or_default(),
                };
                let known = self.hub.lock().ok().and_then(|h| h.net(&name)).is_some();
                if known {
                    conn::connect(&self.ctx, &name);
                } else {
                    return self.global(&format!("/server {name}"), cx);
                }
            }
            _ => return false,
        }
        true
    }

    fn add_network(&mut self, n: Network, cx: &Cx) {
        let Ok(mut h) = self.hub.lock() else { return };
        // The placeholder made for notes before any network existed goes away.
        if h.nets.len() == 1 && h.nets[0].irc.is_none() && h.nets[0].matrix.is_none() {
            h.nets.clear();
        }
        let name = n.name.clone();
        if h.net(&name).is_none() {
            h.nets.push(n);
        }
        apply_config(&mut h, &cx.config.chat, &self.ctx.data_dir);
        let i = h.net(&name).unwrap_or(0);
        drop(h);
        self.select(Sel { net: i, buf: 0 });
    }

    fn submit(&mut self, line: String, cx: &mut Cx) {
        if line.trim().is_empty() {
            return;
        }
        if self.history.last() != Some(&line) {
            self.history.push(line.clone());
        }
        self.hist_pos = None;
        self.scroll = 0;
        if self.global(&line, cx) {
            return;
        }
        let Some((name, matrix, key, kind)) = self.current() else {
            self.note(LineKind::Error, "Add a network first: /server host or /matrix @you:server");
            return;
        };
        if matrix {
            self.matrix_line(&name, key, kind, &line);
        } else {
            self.irc_line(&name, key, &line);
        }
    }

    fn irc_line(&mut self, name: &str, key: Option<String>, line: &str) {
        let (session, tx) = match self.hub.lock().ok().and_then(|h| {
            h.net(name).map(|i| match &h.nets[i].link {
                Link::Irc { session, tx } => (session.clone(), tx.clone()),
                _ => (None, None),
            })
        }) {
            Some(v) => v,
            None => return,
        };
        let typed = match &session {
            Some(s) => {
                s.lock().map_err(|e| e.to_string()).and_then(|s| proto_irc::command::typed(&s, key.as_deref(), line))
            }
            None => {
                let s = proto_irc::Session::new(proto_irc::Config::default());
                let t = proto_irc::command::typed(&s, key.as_deref(), line);
                match &t {
                    Ok(t) if t.out.send.is_empty() => t
                        .local
                        .clone()
                        .map(|l| proto_irc::command::Typed { out: Default::default(), local: Some(l) })
                        .ok_or_else(String::new),
                    _ => Err(format!("Not connected to {name}. /connect tries again.")),
                }
            }
        };
        let t = match typed {
            Ok(t) => t,
            Err(e) if e.is_empty() => return,
            Err(e) => {
                self.note(LineKind::Error, e);
                return;
            }
        };
        // Show a channel we asked to join as soon as we are in it.
        if let Some(j) = t.out.send.iter().find(|m| m.command == "JOIN")
            && let Ok(mut h) = self.hub.lock()
        {
            let first = j.param(0).split(',').next().unwrap_or("").to_string();
            h.focus = Some((name.to_string(), first));
        }
        if let Some(tx) = &tx
            && !t.out.send.is_empty()
        {
            let _ = tx.send(proto_irc::client::Command::Send(t.out.send));
        }
        if let Ok(mut h) = self.hub.lock()
            && let Some(i) = h.net(name)
        {
            for e in t.out.events {
                h.irc_event(i, e);
            }
        }
        use proto_irc::command::Local;
        match t.local {
            Some(Local::Query { nick, .. }) => {
                let sel = self.hub.lock().ok().and_then(|mut h| {
                    let i = h.net(name)?;
                    let b = h.nets[i].buffer(&nick, &nick, BufKind::Query);
                    Some(Sel { net: i, buf: b })
                });
                if let Some(s) = sel {
                    self.select(s);
                }
            }
            Some(Local::Close) => self.close_buffer(),
            Some(Local::Clear) => self.clear_buffer(),
            Some(Local::Help) => {}
            Some(Local::Quit(why)) => conn::disconnect(&self.hub, name, &why),
            Some(Local::Reconnect) => {
                conn::disconnect(&self.hub, name, "Reconnecting");
                conn::connect(&self.ctx, name);
            }
            None => {}
        }
    }

    fn matrix_line(&mut self, name: &str, key: Option<String>, kind: BufKind, line: &str) {
        use alouette_matrix::Cmd;
        let tx = self.hub.lock().ok().and_then(|h| {
            h.net(name).and_then(|i| match &h.nets[i].link {
                Link::Matrix { tx } => tx.clone(),
                _ => None,
            })
        });
        let (cmd, args) = match line.strip_prefix('/') {
            Some(r) if !r.starts_with('/') => {
                let (c, a) = r.split_once(' ').unwrap_or((r, ""));
                (c.to_ascii_lowercase(), a.trim().to_string())
            }
            _ => (String::new(), line.strip_prefix('/').unwrap_or(line).to_string()),
        };
        let room = key.filter(|_| kind == BufKind::Room);
        let need_room = || room.clone().ok_or("Switch to a room first".to_string());
        let c: Result<Option<Cmd>, String> = match cmd.as_str() {
            "" => need_room().map(|r| Some(Cmd::Send { room: r, text: args, emote: false })),
            "me" => need_room().map(|r| Some(Cmd::Send { room: r, text: args, emote: true })),
            "msg" | "say" => need_room().map(|r| Some(Cmd::Send { room: r, text: args, emote: false })),
            "join" | "j" if !args.is_empty() => Ok(Some(Cmd::Join(args))),
            "part" | "leave" => need_room().map(|r| Some(Cmd::Leave(r))),
            "close" => {
                self.close_buffer();
                Ok(None)
            }
            "names" | "who" => need_room().map(|r| Some(Cmd::Members(r))),
            "history" => need_room().map(|r| Some(Cmd::History(r))),
            "verify" => Ok(Some(Cmd::Verify((!args.is_empty()).then_some(args)))),
            "accept" => Ok(Some(Cmd::Accept)),
            "confirm" => Ok(Some(Cmd::Confirm)),
            "mismatch" => Ok(Some(Cmd::Mismatch)),
            "cancel" => Ok(Some(Cmd::Cancel)),
            "recover" if !args.is_empty() => Ok(Some(Cmd::Recover(args))),
            "devices" | "sessions" => Ok(Some(Cmd::Devices)),
            "logout" => Ok(Some(Cmd::Logout)),
            "quit" | "disconnect" => {
                conn::disconnect(&self.hub, name, "");
                Ok(None)
            }
            "reconnect" => {
                conn::disconnect(&self.hub, name, "");
                conn::connect(&self.ctx, name);
                Ok(None)
            }
            other => Err(format!("Unknown command /{other}; /help lists them")),
        };
        match c {
            Ok(Some(c)) => match tx {
                Some(tx) => {
                    let _ = tx.send(c);
                }
                None => self.note(LineKind::Error, format!("Not connected to {name}. /connect signs in.")),
            },
            Ok(None) => {}
            Err(e) => self.note(LineKind::Error, e),
        }
    }

    fn clear_buffer(&mut self) {
        if let Ok(mut h) = self.hub.lock()
            && let Some(b) = h.nets.get_mut(self.sel.net).and_then(|n| n.buffers.get_mut(self.sel.buf))
        {
            b.lines.clear();
        }
    }

    fn close_buffer(&mut self) {
        let Ok(mut h) = self.hub.lock() else { return };
        let Some(n) = h.nets.get_mut(self.sel.net) else { return };
        if self.sel.buf == 0 {
            return;
        }
        let b = n.buffers.remove(self.sel.buf);
        if b.kind == BufKind::Channel
            && b.joined
            && let Link::Irc { tx: Some(tx), .. } = &n.link
        {
            let _ = tx.send(proto_irc::client::Command::Send(vec![proto_irc::Message::new("PART", &[&b.key])]));
        }
        self.sel.buf -= 1;
    }

    fn complete(&mut self) {
        let users: Vec<String> = self
            .hub
            .lock()
            .ok()
            .and_then(|h| {
                h.nets
                    .get(self.sel.net)
                    .and_then(|n| n.buffers.get(self.sel.buf))
                    .map(|b| b.users.iter().map(|u| u.1.clone()).collect())
            })
            .unwrap_or_default();
        let value = self.input.value.clone();
        let start = value.rfind(' ').map(|i| i + 1).unwrap_or(0);
        let (prefix, skip) = match &self.completing {
            Some((p, n)) => (p.clone(), n + 1),
            None => (value[start..].to_string(), 0),
        };
        if prefix.is_empty() {
            return;
        }
        let matches: Vec<&String> =
            users.iter().filter(|u| u.to_lowercase().starts_with(&prefix.to_lowercase())).collect();
        if matches.is_empty() {
            return;
        }
        let pick = matches[skip % matches.len()];
        let suffix = if start == 0 { ": " } else { " " };
        // Replace the last word (or the last completion) with the pick.
        let base = match &self.completing {
            Some(_) => {
                let trimmed = value.trim_end_matches(' ').trim_end_matches(':');
                trimmed.rfind(' ').map(|i| &value[..=i]).unwrap_or("").to_string()
            }
            None => value[..start].to_string(),
        };
        self.input.set(&format!("{base}{pick}{suffix}"));
        self.completing = Some((prefix, skip));
    }

    fn draw_side(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        self.side = area;
        self.rows.clear();
        let Ok(h) = self.hub.lock() else { return };
        let mut lines: Vec<Line> = vec![];
        for (ni, n) in h.nets.iter().enumerate() {
            for (bi, b) in n.buffers.iter().enumerate() {
                if lines.len() as u16 >= area.height {
                    break;
                }
                let sel = Sel { net: ni, buf: bi };
                self.rows.push((area.y + lines.len() as u16, sel));
                let mut style = if sel == self.sel { t.selection() } else { Style::default() };
                if !b.joined && b.kind != BufKind::Server && b.kind != BufKind::Query {
                    style = style.patch(t.dim());
                }
                let (lead, name) = if bi == 0 {
                    let mark = match &n.state {
                        State::Online => "●",
                        State::Connecting => "…",
                        State::Failed(_) => "!",
                        State::Off => "○",
                    };
                    (format!("{mark} "), n.name.clone())
                } else {
                    (if b.encrypted { " 🔒".to_string() } else { "   ".to_string() }, b.name.clone())
                };
                let count = if b.unread > 0 { format!(" {}", b.unread) } else { String::new() };
                let room = (area.width as usize).saturating_sub(lead.width() + count.width());
                let name: String = name
                    .chars()
                    .scan(0, |w, c| {
                        *w += c.width().unwrap_or(0);
                        (*w <= room).then_some(c)
                    })
                    .collect();
                let pad = room.saturating_sub(name.width());
                let count_style = if b.mentioned { t.accent().add_modifier(Modifier::BOLD) } else { t.dim() };
                let name_style = if bi == 0 {
                    style.add_modifier(Modifier::BOLD)
                } else if b.mentioned {
                    style.patch(t.accent())
                } else {
                    style
                };
                lines.push(Line::from(vec![
                    Span::styled(lead, style),
                    Span::styled(name, name_style),
                    Span::styled(" ".repeat(pad), style),
                    Span::styled(
                        count,
                        count_style.patch(if sel == self.sel { t.selection() } else { Style::default() }),
                    ),
                ]));
            }
        }
        frame.render_widget(Paragraph::new(lines), area);
    }

    fn draw_body(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        let t = cx.theme;
        self.body = area;
        let Ok(h) = self.hub.lock() else { return };
        let Some(n) = h.nets.get(self.sel.net) else {
            return;
        };
        let Some(b) = n.buffers.get(self.sel.buf) else { return };
        // The header: name, encryption, topic.
        let mut head = vec![Span::styled(format!(" {} ", b.name), t.accent().add_modifier(Modifier::BOLD))];
        if b.encrypted {
            head.push(Span::styled("encrypted ", t.dim()));
        }
        let topic = if b.kind == BufKind::Server {
            match &n.state {
                State::Online => format!("connected as {}", n.me),
                State::Connecting => "connecting…".into(),
                State::Failed(e) => format!("disconnected: {e}"),
                State::Off => "not connected; /connect".into(),
            }
        } else {
            proto_irc::format::strip(&b.topic)
        };
        head.push(Span::styled(topic, t.dim()));
        frame.render_widget(Paragraph::new(Line::from(head)), Rect { height: 1, ..area });
        let msgs = Rect { y: area.y + 1, height: area.height.saturating_sub(1), ..area };
        if msgs.height == 0 {
            return;
        }
        let nick_w = b.lines.iter().rev().take(200).map(|l| l.from.width()).max().unwrap_or(4).clamp(4, 14);
        let fmt = time::macros::format_description!("[hour]:[minute]");
        let text_w = (msgs.width as usize).saturating_sub(6 + nick_w + 3).max(10);
        let mono = t.is_mono();
        // Build rows from the bottom up, only as many as needed.
        let want = msgs.height as usize + self.scroll;
        let mut rows: Vec<Line> = vec![];
        for l in b.lines.iter().rev() {
            if rows.len() >= want {
                break;
            }
            let when = local_time(l.time, fmt);
            let (who, who_style, base) = match l.kind {
                LineKind::Message => {
                    let st = if mono { Style::default() } else { Style::default().fg(nick_color(&l.from)) };
                    (l.from.clone(), if l.own { st.add_modifier(Modifier::BOLD) } else { st }, Style::default())
                }
                LineKind::Action => ("*".into(), t.accent(), Style::default().add_modifier(Modifier::ITALIC)),
                LineKind::Notice => (format!("-{}-", l.from), t.accent(), Style::default()),
                LineKind::Error => ("!!".into(), Style::default().fg(Color::Red), Style::default().fg(Color::Red)),
                LineKind::Verify => ("🔐".into(), t.accent(), t.accent().add_modifier(Modifier::BOLD)),
                LineKind::Undecryptable => (l.from.clone(), t.dim(), t.dim().add_modifier(Modifier::ITALIC)),
                _ => ("--".into(), t.dim(), t.dim()),
            };
            let text = if l.kind == LineKind::Action { format!("{} {}", l.from, l.text) } else { l.text.clone() };
            let base = if l.highlight { base.patch(t.accent()).add_modifier(Modifier::BOLD) } else { base };
            let mut wrapped = vec![];
            for (pi, para) in text.split('\n').enumerate() {
                for (ri, row) in wrap(&styled(para, base, mono), text_w).into_iter().enumerate() {
                    let first = pi == 0 && ri == 0;
                    let who_cell: String = if first {
                        let w: String = who.chars().take(nick_w).collect();
                        format!("{}{w}", " ".repeat(nick_w.saturating_sub(w.width())))
                    } else {
                        " ".repeat(nick_w)
                    };
                    let mut spans = vec![
                        Span::styled(if first { format!("{when:>5} ") } else { "      ".into() }, t.dim()),
                        Span::styled(who_cell, who_style),
                        Span::styled(" │ ", t.dim()),
                    ];
                    spans.extend(row.into_iter().map(|(s, st)| Span::styled(s, st)));
                    wrapped.push(Line::from(spans));
                }
            }
            for r in wrapped.into_iter().rev() {
                rows.push(r);
            }
        }
        let max_scroll = rows.len().saturating_sub(msgs.height as usize);
        let scroll = self.scroll.min(max_scroll);
        let shown: Vec<Line> =
            rows.into_iter().skip(scroll).take(msgs.height as usize).collect::<Vec<_>>().into_iter().rev().collect();
        let pad = (msgs.height as usize).saturating_sub(shown.len()) as u16;
        frame.render_widget(Paragraph::new(shown), Rect { y: msgs.y + pad, height: msgs.height - pad, ..msgs });
        if scroll > 0 {
            let tag = format!(" ↑ {scroll} more below ");
            let w = tag.width() as u16;
            frame.render_widget(
                Paragraph::new(Span::styled(tag, t.selection())),
                Rect { x: msgs.right().saturating_sub(w), y: msgs.bottom() - 1, width: w.min(msgs.width), height: 1 },
            );
        }
    }

    fn draw_users(&self, frame: &mut Frame, area: Rect, cx: &DrawCx, users: &[(String, String)]) {
        let t = cx.theme;
        let mut lines = vec![Line::from(Span::styled(
            format!(" {} {}", users.len(), if users.len() == 1 { "person" } else { "people" }),
            t.dim(),
        ))];
        for (pre, nick) in users.iter().take(area.height.saturating_sub(1) as usize) {
            let st = if t.is_mono() { Style::default() } else { Style::default().fg(nick_color(nick)) };
            lines.push(Line::from(vec![
                Span::styled(format!(" {:>1}", pre.chars().next().unwrap_or(' ')), t.accent()),
                Span::styled(nick.clone(), st),
            ]));
        }
        frame.render_widget(Paragraph::new(lines), area);
    }
}

impl Window for ChatWindow {
    fn title(&self) -> String {
        match self.current() {
            Some((net, _, Some(_), _)) => {
                let name = self
                    .hub
                    .lock()
                    .ok()
                    .and_then(|h| {
                        h.nets.get(self.sel.net).and_then(|n| n.buffers.get(self.sel.buf)).map(|b| b.name.clone())
                    })
                    .unwrap_or_default();
                format!("Chat: {name} · {net}")
            }
            Some((net, ..)) => format!("Chat: {net}"),
            None => "Chat".into(),
        }
    }
    fn module(&self) -> &'static str {
        MODULE
    }
    fn url(&self) -> Option<String> {
        Some("chat:".into())
    }
    fn wants_text(&self) -> bool {
        true
    }
    fn hints(&self) -> Vec<(&'static str, &'static str)> {
        vec![("Enter", "Send"), ("Alt-↑↓", "Buffer"), ("Tab", "Complete"), ("PgUp", "Scroll"), ("/help", "Commands")]
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, cx: &DrawCx) {
        if area.height < 4 || area.width < 30 {
            return;
        }
        self.follow_focus();
        let t = cx.theme;
        if let Ok(mut h) = self.hub.lock() {
            if self.sel.net >= h.nets.len() || h.nets.get(self.sel.net).is_some_and(|n| self.sel.buf >= n.buffers.len())
            {
                self.sel = Sel::default();
            }
            if cx.focused {
                h.viewing.insert(self.id, self.sel);
                h.read(self.sel);
            } else {
                h.viewing.remove(&self.id);
            }
        }
        let side_w = if area.width >= 60 { 20.min(area.width / 4) } else { 0 };
        let users = self
            .hub
            .lock()
            .ok()
            .and_then(|h| h.nets.get(self.sel.net).and_then(|n| n.buffers.get(self.sel.buf)).map(|b| b.users.clone()))
            .unwrap_or_default();
        let users_w = if self.show_users && !users.is_empty() && area.width >= 90 { 18 } else { 0 };
        let main_h = area.height - 1;
        if side_w > 0 {
            self.draw_side(frame, Rect { width: side_w - 1, height: main_h, ..area }, cx);
            let bar: Vec<Line> = (0..main_h).map(|_| Line::from(Span::styled("│", t.dim()))).collect();
            frame.render_widget(Paragraph::new(bar), Rect { x: area.x + side_w - 1, width: 1, height: main_h, ..area });
        }
        let body = Rect { x: area.x + side_w, width: area.width - side_w - users_w, height: main_h, ..area };
        self.draw_body(frame, body, cx);
        if users_w > 0 {
            let ux = area.right() - users_w;
            let bar: Vec<Line> = (0..main_h).map(|_| Line::from(Span::styled("│", t.dim()))).collect();
            frame.render_widget(Paragraph::new(bar), Rect { x: ux, width: 1, height: main_h, ..area });
            self.draw_users(frame, Rect { x: ux + 1, width: users_w - 1, height: main_h, ..area }, cx, &users);
        }
        // The input line, with our nick in front.
        let me = self.hub.lock().ok().and_then(|h| h.nets.get(self.sel.net).map(|n| n.me.clone())).unwrap_or_default();
        let lead = if me.is_empty() { "> ".to_string() } else { format!("[{me}] ") };
        let row = Rect { y: area.bottom() - 1, height: 1, ..area };
        let room = (row.width as usize).saturating_sub(lead.width() + 1);
        let col = self.input.cursor_col();
        let skip = col.saturating_sub(room);
        let shown: String = self
            .input
            .value
            .chars()
            .scan(0usize, |w, c| {
                let start = *w;
                *w += c.width().unwrap_or(0);
                Some((start, c))
            })
            .filter(|(s, _)| *s >= skip)
            .map(|(_, c)| c)
            .collect();
        frame.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(lead.clone(), t.accent()), Span::raw(shown)])),
            row,
        );
        if cx.focused {
            frame.set_cursor_position(Position { x: row.x + (lead.width() + col - skip) as u16, y: row.y });
        }
        if let Some(q) = self.ctx.asker.question() {
            alouette_ui::ask::draw(frame, area, t, &q, &self.typed);
        }
    }

    fn on_paste(&mut self, text: &str, _cx: &mut Cx) -> Handled {
        if self.ctx.asker.question().is_some() {
            self.typed.push_str(text.trim());
            return Handled::Yes;
        }
        // A pasted block goes in line by line as separate messages only on Enter; join
        // it here so nothing is sent by accident.
        let one = text.replace("\r\n", "\n").trim_end_matches('\n').replace('\n', " ");
        let v = format!("{}{}", self.input.value, one);
        self.input.set(&v);
        Handled::Yes
    }

    fn on_key(&mut self, key: KeyEvent, cx: &mut Cx) -> Handled {
        if let Some(q) = self.ctx.asker.question() {
            alouette_ui::ask::on_key(&self.ctx.asker, &q, &mut self.typed, key);
            return Handled::Yes;
        }
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if key.code != KeyCode::Tab {
            self.completing = None;
        }
        let page = self.body.height.saturating_sub(2).max(1) as usize;
        match key.code {
            KeyCode::Up if alt => self.step(-1),
            KeyCode::Down if alt => self.step(1),
            KeyCode::Char('p') if ctrl => self.step(-1),
            KeyCode::Char('n') if ctrl => self.step(1),
            KeyCode::PageUp => self.scroll += page,
            KeyCode::PageDown => self.scroll = self.scroll.saturating_sub(page),
            KeyCode::Tab => self.complete(),
            KeyCode::Up => {
                if !self.history.is_empty() {
                    let i = match self.hist_pos {
                        None => self.history.len() - 1,
                        Some(i) => i.saturating_sub(1),
                    };
                    self.hist_pos = Some(i);
                    let v = self.history[i].clone();
                    self.input.set(&v);
                }
            }
            KeyCode::Down => match self.hist_pos {
                Some(i) if i + 1 < self.history.len() => {
                    self.hist_pos = Some(i + 1);
                    let v = self.history[i + 1].clone();
                    self.input.set(&v);
                }
                Some(_) => {
                    self.hist_pos = None;
                    self.input.set("");
                }
                None => {}
            },
            KeyCode::Esc if self.scroll > 0 => self.scroll = 0,
            KeyCode::Esc if !self.input.value.is_empty() => self.input.set(""),
            KeyCode::Esc => return Handled::No,
            _ => match self.input.on_key(key) {
                InputResult::Submit => {
                    let line = std::mem::take(&mut self.input.value);
                    self.input.set("");
                    self.submit(line, cx);
                }
                InputResult::Cancel => return Handled::No,
                InputResult::Edited => {}
                InputResult::Ignored => return Handled::No,
            },
        }
        Handled::Yes
    }

    fn on_mouse(&mut self, ev: MouseEvent, _area: Rect, _cx: &mut Cx) -> Handled {
        let inside = |r: Rect| ev.column >= r.x && ev.column < r.right() && ev.row >= r.y && ev.row < r.bottom();
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) if inside(self.side) => {
                if let Some((_, s)) = self.rows.iter().find(|(y, _)| *y == ev.row).copied() {
                    self.select(s);
                }
                Handled::Yes
            }
            MouseEventKind::ScrollUp if inside(self.body) => {
                self.scroll += 3;
                Handled::Yes
            }
            MouseEventKind::ScrollDown if inside(self.body) => {
                self.scroll = self.scroll.saturating_sub(3);
                Handled::Yes
            }
            MouseEventKind::ScrollUp if inside(self.side) => {
                self.step(-1);
                Handled::Yes
            }
            MouseEventKind::ScrollDown if inside(self.side) => {
                self.step(1);
                Handled::Yes
            }
            _ => Handled::No,
        }
    }

    fn on_event(&mut self, ev: &AppEvent, _cx: &mut Cx) {
        if matches!(ev, AppEvent::Module { module: MODULE, .. }) {
            self.follow_focus();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_at_spaces() {
        let runs = vec![
            ("hello there ".to_string(), Style::default()),
            ("world".to_string(), Style::default().add_modifier(Modifier::BOLD)),
        ];
        let rows = wrap(&runs, 11);
        let text: Vec<String> = rows.iter().map(|r| r.iter().map(|(t, _)| t.as_str()).collect()).collect();
        assert_eq!(text, ["hello there ", "world"]);
        let rows = wrap(&runs, 8);
        assert_eq!(rows.len(), 3);
        let rows = wrap(&[("abcdefghij".to_string(), Style::default())], 4);
        assert_eq!(rows.len(), 3);
        assert_ne!(nick_color("ada"), nick_color("bob"));
    }
}
