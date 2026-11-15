//! Every network and buffer, shared by the connections that fill it and the windows that
//! show it.

use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use alouette_core::event::EventSender;
use alouette_core::{AppEvent, Notification};
use proto_irc::client::Command as IrcCmd;
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Message,
    Action,
    Notice,
    /// Joins, parts, quits and nick changes.
    Presence,
    Topic,
    Info,
    Error,
    /// Emoji, prompts and results of a Matrix verification.
    Verify,
    Undecryptable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatLine {
    pub time: i64,
    pub kind: LineKind,
    pub from: String,
    pub text: String,
    pub own: bool,
    pub highlight: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufKind {
    Server,
    Channel,
    Query,
    Room,
}

#[derive(Debug, Clone)]
pub struct Buffer {
    /// What to show: a channel, a nick, a room's name.
    pub name: String,
    /// What the protocol calls it: a channel, a nick, a room ID.
    pub key: String,
    pub kind: BufKind,
    pub lines: VecDeque<ChatLine>,
    pub unread: usize,
    pub mentioned: bool,
    pub topic: String,
    pub users: Vec<(String, String)>,
    /// Still in the channel or room.
    pub joined: bool,
    pub encrypted: bool,
    /// Older messages were asked for once.
    pub backfilled: bool,
}

impl Buffer {
    pub fn new(name: &str, key: &str, kind: BufKind) -> Self {
        Self {
            name: name.to_string(),
            key: key.to_string(),
            kind,
            lines: VecDeque::new(),
            unread: 0,
            mentioned: false,
            topic: String::new(),
            users: vec![],
            joined: kind != BufKind::Query,
            encrypted: false,
            backfilled: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Off,
    Connecting,
    Online,
    Failed(String),
}

pub enum Link {
    Irc { session: Option<Arc<Mutex<proto_irc::Session>>>, tx: Option<UnboundedSender<IrcCmd>> },
    Matrix { tx: Option<UnboundedSender<alouette_matrix::Cmd>> },
}

pub struct Network {
    /// The name from the config, also the name of its log folder.
    pub name: String,
    pub link: Link,
    pub state: State,
    /// Buffer 0 is the network's own.
    pub buffers: Vec<Buffer>,
    /// Our nick or Matrix user ID.
    pub me: String,
    /// Set when the person disconnects on purpose, so nothing reconnects.
    pub quitting: bool,
    /// Bumped on each connect so an old connection's task can tell it was replaced.
    pub generation: u64,
    pub irc: Option<alouette_core::config::IrcNetwork>,
    pub matrix: Option<alouette_core::config::MatrixAccount>,
    /// Rooms to join once signed in.
    pub pending: Vec<String>,
}

impl Network {
    pub fn new(name: &str, link: Link) -> Self {
        Self {
            name: name.to_string(),
            link,
            state: State::Off,
            buffers: vec![Buffer::new(name, "", BufKind::Server)],
            me: String::new(),
            quitting: false,
            generation: 0,
            irc: None,
            matrix: None,
            pending: vec![],
        }
    }

    pub fn from_irc(cfg: &alouette_core::config::IrcNetwork) -> Self {
        let mut n = Self::new(&cfg.name, Link::Irc { session: None, tx: None });
        n.irc = Some(cfg.clone());
        n
    }

    pub fn from_matrix(cfg: &alouette_core::config::MatrixAccount) -> Self {
        let mut n = Self::new(&cfg.name, Link::Matrix { tx: None });
        n.matrix = Some(cfg.clone());
        n
    }

    pub fn is_matrix(&self) -> bool {
        matches!(self.link, Link::Matrix { .. })
    }

    /// The buffer for `key`, compared without case for IRC.
    pub fn find(&self, key: &str) -> Option<usize> {
        let matrix = self.is_matrix();
        self.buffers.iter().position(|b| {
            b.kind != BufKind::Server && if matrix { b.key == key } else { b.key.eq_ignore_ascii_case(key) }
        })
    }

    /// The buffer for `key`, made if missing.
    pub fn buffer(&mut self, key: &str, name: &str, kind: BufKind) -> usize {
        if let Some(i) = self.find(key) {
            return i;
        }
        self.buffers.push(Buffer::new(name, key, kind));
        self.buffers.len() - 1
    }
}

/// Which buffer a window shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sel {
    pub net: usize,
    pub buf: usize,
}

pub struct Hub {
    pub nets: Vec<Network>,
    /// Buffers someone is looking at now: no unread counts or toasts for these.
    pub viewing: std::collections::HashMap<u64, Sel>,
    pub events: Option<EventSender>,
    pub logs: Option<PathBuf>,
    pub notify: String,
    pub show_joins: bool,
    pub scrollback: usize,
    /// A buffer to jump to, set by URLs such as `irc://host/#chan`.
    pub focus: Option<(String, String)>,
}

pub type Shared = Arc<Mutex<Hub>>;

impl Default for Hub {
    fn default() -> Self {
        Self {
            nets: vec![],
            viewing: Default::default(),
            events: None,
            logs: None,
            notify: "highlights".into(),
            show_joins: true,
            scrollback: 2000,
            focus: None,
        }
    }
}

/// A file name that is safe on every system.
pub fn safe_name(s: &str) -> String {
    let s: String = s
        .chars()
        .map(|c| if c.is_alphanumeric() || matches!(c, '#' | '-' | '_' | '.' | '@' | '!') { c } else { '_' })
        .collect();
    let s = s.trim_start_matches('.').to_string();
    if s.is_empty() { "_".into() } else { s }
}

/// The text of a log line.
pub fn log_text(l: &ChatLine) -> String {
    match l.kind {
        LineKind::Message => format!("<{}> {}", l.from, l.text),
        LineKind::Action => format!("* {} {}", l.from, l.text),
        LineKind::Notice => format!("-{}- {}", l.from, l.text),
        _ => format!("-- {}", l.text),
    }
}

pub fn local_time(ms: i64, fmt: &[time::format_description::FormatItem<'_>]) -> String {
    let offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
    time::OffsetDateTime::from_unix_timestamp(ms / 1000)
        .map(|t| t.to_offset(offset))
        .ok()
        .and_then(|t| t.format(fmt).ok())
        .unwrap_or_default()
}

impl Hub {
    pub fn wake(&self) {
        if let Some(e) = &self.events {
            let _ = e.send(AppEvent::Module {
                module: crate::MODULE,
                payload: alouette_core::event::ModulePayload::new(()),
            });
        }
    }

    pub fn net(&self, name: &str) -> Option<usize> {
        self.nets.iter().position(|n| n.name.eq_ignore_ascii_case(name))
    }

    /// Add a line to a buffer: count it, log it, and maybe raise a toast.
    pub fn push(&mut self, net: usize, buf: usize, line: ChatLine, history: bool) {
        let Some(n) = self.nets.get_mut(net) else { return };
        let netname = n.name.clone();
        let Some(b) = n.buffers.get_mut(buf) else { return };
        if !self.show_joins && line.kind == LineKind::Presence && !line.own {
            return;
        }
        // History can repeat what is already shown (a reconnect, a second fetch).
        if history && b.lines.iter().any(|l| l.time == line.time && l.text == line.text && l.from == line.from) {
            return;
        }
        let seen = self.viewing.values().any(|s| *s == Sel { net, buf });
        let talk = matches!(line.kind, LineKind::Message | LineKind::Action | LineKind::Notice);
        if !seen && !line.own && !history && (talk || b.kind == BufKind::Server) {
            b.unread += 1;
            b.mentioned |= line.highlight;
        }
        if let Some(dir) = &self.logs
            && !history
        {
            let dir = dir.join(safe_name(&netname)).join(safe_name(if b.kind == BufKind::Server {
                "server"
            } else {
                &b.name
            }));
            let day = local_time(line.time, time::macros::format_description!("[year]-[month]-[day]"));
            let at = local_time(line.time, time::macros::format_description!("[hour]:[minute]:[second]"));
            if std::fs::create_dir_all(&dir).is_ok()
                && let Ok(mut f) =
                    std::fs::OpenOptions::new().create(true).append(true).open(dir.join(format!("{day}.log")))
            {
                let _ = writeln!(f, "{at} {}", log_text(&line));
            }
        }
        let toast = !seen
            && !line.own
            && !history
            && talk
            && match self.notify.as_str() {
                "all" => true,
                "off" => false,
                _ => line.highlight,
            };
        if toast && let Some(e) = &self.events {
            let place = if b.kind == BufKind::Query { netname.clone() } else { format!("{} on {netname}", b.name) };
            let text = format!("{} in {place}: {}", line.from, proto_irc::format::strip(&line.text));
            let _ = e.send(AppEvent::Notify(Notification::info(text)));
        }
        // Keep history in time order: a fetched old line goes before newer ones.
        if history {
            let at = b.lines.iter().position(|l| l.time > line.time).unwrap_or(b.lines.len());
            b.lines.insert(at, line);
        } else {
            b.lines.push_back(line);
        }
        while b.lines.len() > self.scrollback.max(100) {
            b.lines.pop_front();
        }
    }

    /// A line for a network's own buffer.
    pub fn status(&mut self, net: usize, kind: LineKind, text: impl Into<String>) {
        let line = ChatLine {
            time: proto_irc::session::now_ms(),
            kind,
            from: String::new(),
            text: text.into(),
            own: false,
            highlight: kind == LineKind::Verify,
        };
        self.push(net, 0, line, false);
    }

    /// Mark a buffer as read.
    pub fn read(&mut self, sel: Sel) {
        if let Some(b) = self.nets.get_mut(sel.net).and_then(|n| n.buffers.get_mut(sel.buf)) {
            b.unread = 0;
            b.mentioned = false;
        }
    }

    /// Apply an IRC event to network `net`.
    pub fn irc_event(&mut self, net: usize, ev: proto_irc::Event) {
        use proto_irc::Event as E;
        let Some(n) = self.nets.get_mut(net) else { return };
        match ev {
            E::Registered { nick } => {
                n.me = nick;
                n.state = State::Online;
            }
            E::Nick(nick) => n.me = nick,
            E::Network(name) => {
                n.buffers[0].topic = name;
            }
            E::Joined(chan) => {
                let i = n.buffer(&chan, &chan, BufKind::Channel);
                n.buffers[i].joined = true;
                n.buffers[i].name = chan;
            }
            E::Left(chan) => {
                if let Some(i) = n.find(&chan) {
                    n.buffers[i].joined = false;
                    n.buffers[i].users.clear();
                }
            }
            E::Users { channel, users } => {
                let i = n.buffer(&channel, &channel, BufKind::Channel);
                n.buffers[i].users = users;
            }
            E::Topic { channel, topic } => {
                let i = n.buffer(&channel, &channel, BufKind::Channel);
                n.buffers[i].topic = topic;
            }
            E::Renamed { old, new } => {
                if let Some(i) = n.find(&old)
                    && n.buffers[i].kind == BufKind::Query
                    && n.find(&new).is_none()
                {
                    n.buffers[i].name = new.clone();
                    n.buffers[i].key = new;
                }
            }
            E::SaslDone { .. } | E::SaslFailed(_) | E::Invited { .. } => {}
            E::Line { buffer, line } => {
                let i = match &buffer {
                    None => 0,
                    Some(b) => {
                        let channel = n.link_is_channel(b);
                        n.buffer(b, b, if channel { BufKind::Channel } else { BufKind::Query })
                    }
                };
                let history = line.history;
                let kind = match line.kind {
                    proto_irc::Kind::Message => LineKind::Message,
                    proto_irc::Kind::Action => LineKind::Action,
                    proto_irc::Kind::Notice => LineKind::Notice,
                    proto_irc::Kind::Join | proto_irc::Kind::Part | proto_irc::Kind::Quit | proto_irc::Kind::Nick => {
                        LineKind::Presence
                    }
                    proto_irc::Kind::Kick | proto_irc::Kind::Mode | proto_irc::Kind::Info => LineKind::Info,
                    proto_irc::Kind::Topic => LineKind::Topic,
                    proto_irc::Kind::Error => LineKind::Error,
                };
                let own = line.own;
                let cl = ChatLine {
                    time: line.time,
                    kind,
                    from: line.from,
                    text: line.text,
                    own,
                    highlight: line.highlight,
                };
                self.push(net, i, cl, history);
            }
        }
    }

    /// Apply a Matrix update to network `net`.
    pub fn matrix_update(&mut self, net: usize, u: alouette_matrix::Update) -> Option<String> {
        use alouette_matrix::Update as U;
        let n = self.nets.get_mut(net)?;
        match u {
            U::Status(s) => self.status(net, LineKind::Info, s),
            U::Error(s) => self.status(net, LineKind::Error, s),
            U::Verify(s) => self.status(net, LineKind::Verify, s),
            U::Ready { user_id, device_id, session } => {
                n.me = user_id.clone();
                n.state = State::Online;
                if let Link::Matrix { tx: Some(tx) } = &n.link {
                    for room in n.pending.drain(..) {
                        let _ = tx.send(alouette_matrix::Cmd::Join(room));
                    }
                }
                self.status(net, LineKind::Info, format!("Signed in as {user_id} (session {device_id})"));
                return session;
            }
            U::Rooms(rooms) => {
                for b in n.buffers.iter_mut().filter(|b| b.kind == BufKind::Room) {
                    b.joined = false;
                }
                for r in rooms {
                    let i = n.buffer(&r.id, &r.name, BufKind::Room);
                    let b = &mut n.buffers[i];
                    b.name = r.name;
                    b.topic = r.topic;
                    b.encrypted = r.encrypted;
                    b.joined = true;
                }
            }
            U::Members { room, users } => {
                if let Some(i) = n.find(&room) {
                    n.buffers[i].users = users;
                }
            }
            U::Message { room, msg } => {
                let i = n.buffer(&room, &room, BufKind::Room);
                use alouette_matrix::Kind as K;
                let kind = match msg.kind {
                    K::Text => LineKind::Message,
                    K::Emote => LineKind::Action,
                    K::Notice => LineKind::Notice,
                    K::Undecryptable => LineKind::Undecryptable,
                    K::Join | K::Leave => LineKind::Presence,
                    K::Topic => LineKind::Topic,
                    K::Info => LineKind::Info,
                };
                let history = msg.history;
                let cl = ChatLine {
                    time: msg.time,
                    kind,
                    from: msg.from,
                    text: msg.text,
                    own: msg.own,
                    highlight: msg.highlight,
                };
                self.push(net, i, cl, history);
            }
        }
        None
    }
}

impl Network {
    fn link_is_channel(&self, name: &str) -> bool {
        match &self.link {
            Link::Irc { session: Some(s), .. } => s.lock().map(|s| s.support.is_channel(name)).unwrap_or(false),
            _ => name.starts_with('#') || name.starts_with('&'),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, highlight: bool) -> ChatLine {
        ChatLine { time: 1, kind: LineKind::Message, from: "bob".into(), text: text.into(), own: false, highlight }
    }

    #[test]
    fn counts_and_logs() {
        let dir = std::env::temp_dir().join(format!("alouette-chat-hub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut hub = Hub { logs: Some(dir.clone()), ..Default::default() };
        hub.nets.push(Network::new("Test/Net", Link::Irc { session: None, tx: None }));
        let i = hub.nets[0].buffer("#rust", "#rust", BufKind::Channel);
        hub.push(0, i, line("hello", false), false);
        hub.push(0, i, line("hey ada", true), false);
        // The same history line twice shows once, and isn't counted or logged.
        let old = ChatLine { time: 0, ..line("old", false) };
        hub.push(0, i, old.clone(), true);
        hub.push(0, i, old, true);
        let b = &hub.nets[0].buffers[i];
        assert_eq!((b.unread, b.mentioned, b.lines.len()), (2, true, 3));
        assert_eq!(b.lines[0].text, "old", "history goes in time order");
        hub.viewing.insert(1, Sel { net: 0, buf: i });
        hub.read(Sel { net: 0, buf: i });
        hub.push(0, i, line("seen", false), false);
        assert_eq!(hub.nets[0].buffers[i].unread, 0);
        let logs: Vec<_> = std::fs::read_dir(dir.join("Test_Net").join("#rust")).unwrap().collect();
        assert_eq!(logs.len(), 1);
        let text = std::fs::read_to_string(logs[0].as_ref().unwrap().path()).unwrap();
        assert_eq!(text.lines().count(), 3);
        assert!(text.contains("<bob> hey ada"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn irc_events_make_buffers() {
        let mut hub = Hub::default();
        hub.nets.push(Network::new("t", Link::Irc { session: None, tx: None }));
        hub.irc_event(0, proto_irc::Event::Joined("#a".into()));
        let l = proto_irc::Line {
            time: 5,
            kind: proto_irc::Kind::Message,
            from: "bob".into(),
            text: "psst".into(),
            highlight: true,
            own: false,
            history: false,
        };
        hub.irc_event(0, proto_irc::Event::Line { buffer: Some("bob".into()), line: l });
        assert_eq!(hub.nets[0].buffers.len(), 3);
        assert_eq!(hub.nets[0].buffers[2].kind, BufKind::Query);
        hub.irc_event(0, proto_irc::Event::Renamed { old: "bob".into(), new: "rob".into() });
        assert_eq!(hub.nets[0].buffers[2].name, "rob");
        assert_eq!(safe_name("../etc"), "_etc");
    }
}
