//! The client side of an IRC connection as a state machine: feed it messages from the
//! server, and it says what to send back and what happened. No I/O happens here, so every
//! rule is testable.

use std::collections::{BTreeMap, HashMap, HashSet};

use base64::Engine;

use crate::message::Message;
use crate::support::Support;

/// How to sign in to services during registration.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Sasl {
    #[default]
    None,
    /// An account name and password.
    Plain { user: String, pass: String },
    /// The client certificate presented over TLS.
    External,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub nick: String,
    /// Tried in turn when the nick is taken; then `_` is added.
    pub alt_nicks: Vec<String>,
    pub user: String,
    pub realname: String,
    /// The server password (`PASS`), rarely needed; bouncers use it.
    pub password: Option<String>,
    pub sasl: Sasl,
    /// Words besides your nick that mark a line as a highlight.
    pub highlights: Vec<String>,
    /// The reply to CTCP VERSION.
    pub version: String,
    /// Fetch recent history on join where the server offers it.
    pub history: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            nick: "alouette".into(),
            alt_nicks: vec![],
            user: "alouette".into(),
            realname: "Alouette user".into(),
            password: None,
            sasl: Sasl::None,
            highlights: vec![],
            version: "Alouette".into(),
            history: 50,
        }
    }
}

/// Capabilities we use when the server offers them.
pub const WANTED: &[&str] = &[
    "sasl",
    "server-time",
    "message-tags",
    "echo-message",
    "multi-prefix",
    "away-notify",
    "account-notify",
    "extended-join",
    "chghost",
    "cap-notify",
    "invite-notify",
    "setname",
    "userhost-in-names",
    "batch",
    "draft/chathistory",
    "chathistory",
    "draft/event-playback",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Message,
    Action,
    Notice,
    Join,
    Part,
    Quit,
    Nick,
    Topic,
    Mode,
    Kick,
    Info,
    Error,
}

/// A line to show in a buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// Unix time in milliseconds.
    pub time: i64,
    pub kind: Kind,
    /// Who said it; empty for server text.
    pub from: String,
    pub text: String,
    pub highlight: bool,
    /// Sent by us.
    pub own: bool,
    /// Fetched from history rather than live.
    pub history: bool,
}

/// Something that happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Registration finished; this is our nick.
    Registered {
        nick: String,
    },
    /// A line for a buffer: `None` is the server buffer, otherwise a channel or a nick.
    Line {
        buffer: Option<String>,
        line: Line,
    },
    /// We joined a channel.
    Joined(String),
    /// We left a channel, or were kicked.
    Left(String),
    /// The people in a channel changed: (prefixes, nick) sorted by rank then name.
    Users {
        channel: String,
        users: Vec<(String, String)>,
    },
    Topic {
        channel: String,
        topic: String,
    },
    /// Our nick changed.
    Nick(String),
    /// Someone else changed nick: rename any query buffer.
    Renamed {
        old: String,
        new: String,
    },
    SaslDone {
        account: String,
    },
    SaslFailed(String),
    Invited {
        by: String,
        channel: String,
    },
    /// The ISUPPORT network name.
    Network(String),
}

/// What to do after a message.
#[derive(Debug, Default)]
pub struct Output {
    pub send: Vec<Message>,
    pub events: Vec<Event>,
}

impl Output {
    fn line(&mut self, buffer: Option<String>, line: Line) {
        self.events.push(Event::Line { buffer, line });
    }
}

#[derive(Debug, Clone, Default)]
pub struct Channel {
    pub name: String,
    pub topic: String,
    /// Folded nick to (prefixes, nick).
    pub users: BTreeMap<String, (String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SaslState {
    Idle,
    Started,
    Done,
}

pub struct Session {
    pub cfg: Config,
    pub nick: String,
    pub support: Support,
    pub registered: bool,
    offered: HashMap<String, String>,
    pub caps: HashSet<String>,
    ls_done: bool,
    sasl: SaslState,
    pending_req: usize,
    tries: usize,
    pub channels: HashMap<String, Channel>,
    /// Open batches: reference to type.
    batches: HashMap<String, String>,
    pub account: Option<String>,
    motd: Vec<String>,
}

pub fn now_ms() -> i64 {
    (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

fn parse_time(s: &str) -> Option<i64> {
    let t = time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()?;
    Some((t.unix_timestamp_nanos() / 1_000_000) as i64)
}

impl Session {
    pub fn new(cfg: Config) -> Self {
        Self {
            nick: cfg.nick.clone(),
            cfg,
            support: Support::default(),
            registered: false,
            offered: HashMap::new(),
            caps: HashSet::new(),
            ls_done: false,
            sasl: SaslState::Idle,
            pending_req: 0,
            tries: 0,
            channels: HashMap::new(),
            batches: HashMap::new(),
            account: None,
            motd: vec![],
        }
    }

    /// The first messages to send after connecting.
    pub fn start(&mut self) -> Vec<Message> {
        let mut out = vec![Message::new("CAP", &["LS", "302"])];
        if let Some(p) = &self.cfg.password {
            out.push(Message::new("PASS", &[p]));
        }
        out.push(Message::new("NICK", &[&self.nick]));
        out.push(Message::new("USER", &[&self.cfg.user, "0", "*", &self.cfg.realname]));
        out
    }

    pub fn is_me(&self, nick: &str) -> bool {
        self.support.eq(nick, &self.nick)
    }

    pub fn channel(&self, name: &str) -> Option<&Channel> {
        self.channels.get(&self.support.fold(name))
    }

    fn info(&self, out: &mut Output, buffer: Option<String>, kind: Kind, from: &str, text: String, time: i64) {
        out.line(
            buffer,
            Line { time, kind, from: from.to_string(), text, highlight: false, own: false, history: false },
        );
    }

    /// Whether text mentions us.
    pub fn highlights(&self, text: &str) -> bool {
        let plain = crate::format::strip(text).to_lowercase();
        std::iter::once(self.nick.to_lowercase())
            .chain(self.cfg.highlights.iter().map(|h| h.to_lowercase()))
            .filter(|w| !w.is_empty())
            .any(|w| {
                plain.match_indices(&w).any(|(i, _)| {
                    let before = plain[..i].chars().next_back();
                    let after = plain[i + w.len()..].chars().next();
                    let edge = |c: Option<char>| c.is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '-'));
                    edge(before) && edge(after)
                })
            })
    }

    fn users_event(&self, out: &mut Output, chan: &str) {
        if let Some(c) = self.channel(chan) {
            let mut users: Vec<(String, String)> = c.users.values().cloned().collect();
            users.sort_by(|a, b| {
                let ra = a.0.chars().next().map(|s| self.support.rank(s)).unwrap_or(usize::MAX);
                let rb = b.0.chars().next().map(|s| self.support.rank(s)).unwrap_or(usize::MAX);
                ra.cmp(&rb).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
            });
            out.events.push(Event::Users { channel: c.name.clone(), users });
        }
    }

    fn end_cap(&mut self, out: &mut Output) {
        if !self.registered {
            out.send.push(Message::new("CAP", &["END"]));
        }
    }

    fn request_caps(&mut self, names: Vec<String>, out: &mut Output) -> bool {
        let want: Vec<String> = names
            .into_iter()
            .filter(|c| WANTED.contains(&c.as_str()) && !self.caps.contains(c))
            .filter(|c| c != "sasl" || self.cfg.sasl != Sasl::None)
            .collect();
        if want.is_empty() {
            return false;
        }
        // Keep each request well under the line limit.
        let mut line = String::new();
        for c in want {
            if line.len() + c.len() > 400 {
                out.send.push(Message::new("CAP", &["REQ", &line]));
                self.pending_req += 1;
                line.clear();
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(&c);
        }
        out.send.push(Message::new("CAP", &["REQ", &line]));
        self.pending_req += 1;
        true
    }

    fn sasl_payload(&self) -> Vec<String> {
        let raw = match &self.cfg.sasl {
            Sasl::Plain { user, pass } => format!("{user}\0{user}\0{pass}"),
            _ => return vec!["+".into()],
        };
        let b = base64::engine::general_purpose::STANDARD.encode(raw.as_bytes());
        let mut out: Vec<String> = b.as_bytes().chunks(400).map(|c| String::from_utf8_lossy(c).into_owned()).collect();
        if b.len() % 400 == 0 {
            out.push("+".into());
        }
        out
    }

    /// The buffer a PRIVMSG or NOTICE belongs in.
    fn target_buffer(&self, m: &Message) -> Option<String> {
        let target = m.param(0);
        if self.support.is_channel(target) {
            let t = target.trim_start_matches(|c| self.support.statusmsg.contains(c));
            return Some(self.channel(t).map(|c| c.name.clone()).unwrap_or_else(|| t.to_string()));
        }
        let from = m.nick().unwrap_or("");
        let server = m.source.as_deref().is_none_or(|s| !s.contains('!')) && from.contains('.');
        if server || !self.registered || target == "*" {
            return None;
        }
        if self.is_me(from) { Some(target.to_string()) } else { Some(from.to_string()) }
    }

    /// Handle one message from the server.
    pub fn handle(&mut self, m: &Message) -> Output {
        let mut out = Output::default();
        let time = m.tag("time").and_then(parse_time).unwrap_or_else(now_ms);
        let history = m.tag("batch").and_then(|b| self.batches.get(b)).is_some_and(|t| t.contains("chathistory"));
        let from = m.nick().unwrap_or("").to_string();
        let rest = || m.params.iter().skip(1).cloned().collect::<Vec<_>>().join(" ");
        match m.command.as_str() {
            "PING" => out.send.push(Message::new("PONG", &[m.param(0)])),
            "PONG" => {}
            "CAP" => self.on_cap(m, &mut out),
            "AUTHENTICATE" if m.param(0) == "+" && self.sasl == SaslState::Started => {
                for p in self.sasl_payload() {
                    out.send.push(Message::new("AUTHENTICATE", &[&p]));
                }
            }
            "900" => {
                self.account = Some(m.param(2).to_string());
                self.info(&mut out, None, Kind::Info, "", m.trailing().to_string(), time);
            }
            "903" => {
                self.sasl = SaslState::Done;
                out.events.push(Event::SaslDone { account: self.account.clone().unwrap_or_default() });
                self.end_cap(&mut out);
            }
            "902" | "904" | "905" | "906" | "908" => {
                if self.sasl == SaslState::Started {
                    self.sasl = SaslState::Done;
                    let why = m.trailing().to_string();
                    self.info(&mut out, None, Kind::Error, "", format!("Signing in to services failed: {why}"), time);
                    out.events.push(Event::SaslFailed(why));
                    self.end_cap(&mut out);
                }
            }
            "001" => {
                self.registered = true;
                self.nick = m.param(0).to_string();
                out.events.push(Event::Registered { nick: self.nick.clone() });
                self.info(&mut out, None, Kind::Info, "", m.trailing().to_string(), time);
            }
            "005" => {
                let n = m.params.len();
                if n > 2 {
                    let before = self.support.network.clone();
                    self.support.apply(&m.params[1..n - 1]);
                    if self.support.network != before
                        && let Some(net) = &self.support.network
                    {
                        out.events.push(Event::Network(net.clone()));
                    }
                }
            }
            "375" => self.motd.clear(),
            "372" => self.motd.push(m.trailing().trim_start_matches("- ").to_string()),
            "376" | "422" => {
                for l in std::mem::take(&mut self.motd) {
                    self.info(&mut out, None, Kind::Info, "", l, time);
                }
                if m.command == "422" {
                    self.info(&mut out, None, Kind::Info, "", m.trailing().to_string(), time);
                }
            }
            "432" | "433" | "437" if !self.registered => {
                let next = self.cfg.alt_nicks.get(self.tries).cloned().unwrap_or_else(|| format!("{}_", self.nick));
                self.tries += 1;
                self.info(
                    &mut out,
                    None,
                    Kind::Info,
                    "",
                    format!("The nick {} is taken; trying {next}", m.param(1)),
                    time,
                );
                self.nick = next.clone();
                out.send.push(Message::new("NICK", &[&next]));
            }
            "PRIVMSG" | "NOTICE" => self.on_message(m, time, history, &mut out),
            "JOIN" => {
                let chan = m.param(0).to_string();
                let key = self.support.fold(&chan);
                if self.is_me(&from) {
                    self.channels.insert(key.clone(), Channel { name: chan.clone(), ..Default::default() });
                    out.events.push(Event::Joined(chan.clone()));
                    if self.cfg.history > 0
                        && (self.caps.contains("draft/chathistory") || self.caps.contains("chathistory"))
                    {
                        let n = self.cfg.history.to_string();
                        out.send.push(Message::new("CHATHISTORY", &["LATEST", &chan, "*", &n]));
                    }
                }
                if let Some(c) = self.channels.get_mut(&key) {
                    c.users.insert(self.support.fold(&from), (String::new(), from.clone()));
                }
                let who = m.userhost().map(|uh| format!(" ({uh})")).unwrap_or_default();
                let text = format!("{from}{who} joined");
                self.info(&mut out, Some(chan.clone()), Kind::Join, &from, text, time);
                if !history && !self.is_me(&from) {
                    self.users_event(&mut out, &chan);
                }
            }
            "PART" => {
                let chan = m.param(0).to_string();
                let key = self.support.fold(&chan);
                let why = m.params.get(1).map(|r| format!(" ({r})")).unwrap_or_default();
                self.info(&mut out, Some(chan.clone()), Kind::Part, &from, format!("{from} left{why}"), time);
                if self.is_me(&from) {
                    self.channels.remove(&key);
                    out.events.push(Event::Left(chan));
                } else if let Some(c) = self.channels.get_mut(&key) {
                    c.users.remove(&self.support.fold(&from));
                    self.users_event(&mut out, &chan);
                }
            }
            "KICK" => {
                let (chan, who) = (m.param(0).to_string(), m.param(1).to_string());
                let why = m.params.get(2).map(|r| format!(" ({r})")).unwrap_or_default();
                let text = format!("{from} removed {who}{why}");
                let mut l =
                    Line { time, kind: Kind::Kick, from: from.clone(), text, highlight: false, own: false, history };
                if self.is_me(&who) {
                    l.highlight = true;
                    self.channels.remove(&self.support.fold(&chan));
                    out.line(Some(chan.clone()), l);
                    out.events.push(Event::Left(chan));
                } else {
                    out.line(Some(chan.clone()), l);
                    if let Some(c) = self.channels.get_mut(&self.support.fold(&chan)) {
                        c.users.remove(&self.support.fold(&who));
                    }
                    self.users_event(&mut out, &chan);
                }
            }
            "QUIT" => {
                let why = m.params.first().map(|r| format!(" ({r})")).unwrap_or_default();
                let key = self.support.fold(&from);
                let mut touched = vec![];
                for c in self.channels.values_mut() {
                    if c.users.remove(&key).is_some() {
                        touched.push(c.name.clone());
                    }
                }
                for chan in touched {
                    self.info(&mut out, Some(chan.clone()), Kind::Quit, &from, format!("{from} quit{why}"), time);
                    self.users_event(&mut out, &chan);
                }
            }
            "NICK" => {
                let new = m.param(0).to_string();
                let (old, new_key) = (self.support.fold(&from), self.support.fold(&new));
                let me = self.is_me(&from);
                if me {
                    self.nick = new.clone();
                    out.events.push(Event::Nick(new.clone()));
                    self.info(&mut out, None, Kind::Nick, &from, format!("You are now known as {new}"), time);
                } else {
                    out.events.push(Event::Renamed { old: from.clone(), new: new.clone() });
                }
                let mut touched = vec![];
                for c in self.channels.values_mut() {
                    if let Some((pre, _)) = c.users.remove(&old) {
                        c.users.insert(new_key.clone(), (pre, new.clone()));
                        touched.push(c.name.clone());
                    }
                }
                for chan in touched {
                    self.info(&mut out, Some(chan.clone()), Kind::Nick, &from, format!("{from} is now {new}"), time);
                    self.users_event(&mut out, &chan);
                }
            }
            "TOPIC" => {
                let (chan, topic) = (m.param(0).to_string(), m.param(1).to_string());
                if let Some(c) = self.channels.get_mut(&self.support.fold(&chan)) {
                    c.topic = topic.clone();
                }
                let text = format!("{from} set the topic: {topic}");
                self.info(&mut out, Some(chan.clone()), Kind::Topic, &from, text, time);
                out.events.push(Event::Topic { channel: chan, topic });
            }
            "332" => {
                let (chan, topic) = (m.param(1).to_string(), m.param(2).to_string());
                if let Some(c) = self.channels.get_mut(&self.support.fold(&chan)) {
                    c.topic = topic.clone();
                }
                self.info(&mut out, Some(chan.clone()), Kind::Topic, "", format!("Topic: {topic}"), time);
                out.events.push(Event::Topic { channel: chan, topic });
            }
            "333" | "366" | "329" => {
                if m.command == "366" {
                    self.users_event(&mut out, m.param(1));
                }
            }
            "353" => {
                let chan = m.param(2).to_string();
                let support = self.support.clone();
                if let Some(c) = self.channels.get_mut(&support.fold(&chan)) {
                    for entry in m.trailing().split(' ').filter(|e| !e.is_empty()) {
                        let (pre, nick) = support.split_prefixes(entry);
                        c.users.insert(support.fold(nick), (pre, nick.to_string()));
                    }
                } else {
                    self.info(&mut out, None, Kind::Info, "", format!("{chan}: {}", m.trailing()), time);
                }
            }
            "MODE" => self.on_mode(m, time, &mut out),
            "INVITE" => {
                let chan = m.param(1).to_string();
                if self.is_me(m.param(0)) {
                    out.events.push(Event::Invited { by: from.clone(), channel: chan.clone() });
                    let text = format!("{from} invited you to {chan}; /join {chan} to go");
                    let l =
                        Line { time, kind: Kind::Info, from: from.clone(), text, highlight: true, own: false, history };
                    out.line(None, l);
                } else {
                    let text = format!("{from} invited {} to {chan}", m.param(0));
                    self.info(&mut out, Some(chan), Kind::Info, &from, text, time);
                }
            }
            "BATCH" => {
                let r = m.param(0);
                if let Some(id) = r.strip_prefix('+') {
                    self.batches.insert(id.to_string(), m.param(1).to_string());
                } else if let Some(id) = r.strip_prefix('-') {
                    self.batches.remove(id);
                }
            }
            "ERROR" => self.info(&mut out, None, Kind::Error, "", m.trailing().to_string(), time),
            "AWAY" | "ACCOUNT" | "CHGHOST" | "SETNAME" | "FAIL" | "WARN" | "NOTE" => {
                if matches!(m.command.as_str(), "FAIL" | "WARN") {
                    let kind = if m.command == "FAIL" { Kind::Error } else { Kind::Info };
                    self.info(&mut out, None, kind, "", m.trailing().to_string(), time);
                }
            }
            c if c.len() == 3 && c.chars().all(|c| c.is_ascii_digit()) => {
                let kind = if c.starts_with('4') || c.starts_with('5') { Kind::Error } else { Kind::Info };
                // Errors about a channel go to that channel's buffer.
                let buffer = m.params.get(1).filter(|p| self.channel(p).is_some()).cloned();
                let text = match c {
                    "311" => format!("{} is {}@{} ({})", m.param(1), m.param(2), m.param(3), m.trailing()),
                    "312" => format!("{} is on {} ({})", m.param(1), m.param(2), m.trailing()),
                    "317" => format!("{} has been idle {} seconds", m.param(1), m.param(2)),
                    "319" => format!("{} is in {}", m.param(1), m.trailing()),
                    "330" => format!("{} is signed in as {}", m.param(1), m.param(2)),
                    "301" => format!("{} is away: {}", m.param(1), m.trailing()),
                    _ => rest(),
                };
                self.info(&mut out, buffer, kind, "", text, time);
            }
            _ => {}
        }
        out
    }

    fn on_cap(&mut self, m: &Message, out: &mut Output) {
        let sub = m.param(1).to_ascii_uppercase();
        let list = m.trailing();
        let more = m.params.len() > 3 && m.param(2) == "*";
        match sub.as_str() {
            "LS" | "NEW" => {
                let mut names = vec![];
                for c in list.split(' ').filter(|c| !c.is_empty()) {
                    let (k, v) = c.split_once('=').unwrap_or((c, ""));
                    self.offered.insert(k.to_string(), v.to_string());
                    names.push(k.to_string());
                }
                if sub == "LS" && more {
                    return;
                }
                if sub == "LS" {
                    self.ls_done = true;
                    let all: Vec<String> = self.offered.keys().cloned().collect();
                    // EXTERNAL or PLAIN must be on offer when the server lists mechanisms.
                    let mech_ok = |me: &Self| match (&me.cfg.sasl, me.offered.get("sasl").map(String::as_str)) {
                        (_, None | Some("")) => true,
                        (Sasl::Plain { .. }, Some(v)) => v.split(',').any(|x| x == "PLAIN"),
                        (Sasl::External, Some(v)) => v.split(',').any(|x| x == "EXTERNAL"),
                        (Sasl::None, _) => false,
                    };
                    let mut all = all;
                    if !mech_ok(self) {
                        all.retain(|c| c != "sasl");
                    }
                    if !self.request_caps(all, out) {
                        self.end_cap(out);
                    }
                } else {
                    self.request_caps(names, out);
                }
            }
            "ACK" => {
                for c in list.split(' ').filter(|c| !c.is_empty()) {
                    match c.strip_prefix('-') {
                        Some(c) => {
                            self.caps.remove(c);
                        }
                        None => {
                            self.caps.insert(c.to_string());
                        }
                    }
                }
                self.pending_req = self.pending_req.saturating_sub(1);
                if self.caps.contains("sasl") && self.sasl == SaslState::Idle && !self.registered {
                    let mech = match self.cfg.sasl {
                        Sasl::External => "EXTERNAL",
                        _ => "PLAIN",
                    };
                    self.sasl = SaslState::Started;
                    out.send.push(Message::new("AUTHENTICATE", &[mech]));
                } else if self.pending_req == 0 && self.sasl != SaslState::Started {
                    self.end_cap(out);
                }
            }
            "NAK" => {
                self.pending_req = self.pending_req.saturating_sub(1);
                if self.pending_req == 0 && self.sasl != SaslState::Started {
                    self.end_cap(out);
                }
            }
            "DEL" => {
                for c in list.split(' ') {
                    self.caps.remove(c);
                    self.offered.remove(c);
                }
            }
            _ => {}
        }
    }

    fn on_message(&mut self, m: &Message, time: i64, history: bool, out: &mut Output) {
        let notice = m.command == "NOTICE";
        let from = m.nick().unwrap_or("").to_string();
        let mut text = m.param(1).to_string();
        let buffer = self.target_buffer(m);
        let own = self.is_me(&from);
        let mut kind = if notice { Kind::Notice } else { Kind::Message };
        if let Some(inner) = text.strip_prefix('\x01') {
            let inner = inner.trim_end_matches('\x01').to_string();
            let (cmd, arg) = inner.split_once(' ').unwrap_or((&inner, ""));
            let cmd = cmd.to_ascii_uppercase();
            if cmd == "ACTION" {
                kind = Kind::Action;
                text = arg.to_string();
            } else {
                if notice {
                    let t = format!("CTCP {cmd} reply from {from}: {arg}");
                    self.info(out, None, Kind::Info, &from, t, time);
                } else if !own && !history {
                    let reply = match cmd.as_str() {
                        "VERSION" => Some(self.cfg.version.clone()),
                        "PING" => Some(arg.to_string()),
                        "TIME" => {
                            time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc2822).ok()
                        }
                        "CLIENTINFO" => Some("ACTION CLIENTINFO PING TIME VERSION".into()),
                        _ => None,
                    };
                    if let Some(r) = reply {
                        let body = if r.is_empty() { format!("\x01{cmd}\x01") } else { format!("\x01{cmd} {r}\x01") };
                        out.send.push(Message::new("NOTICE", &[&from, &body]));
                    }
                    self.info(out, None, Kind::Info, &from, format!("{from} asked for CTCP {cmd}"), time);
                }
                return;
            }
        }
        let private = buffer.as_deref().is_some_and(|b| !self.support.is_channel(b));
        let highlight = !own && !history && (self.highlights(&text) || (private && !notice));
        out.line(buffer, Line { time, kind, from, text, highlight, own, history });
    }

    fn on_mode(&mut self, m: &Message, time: i64, out: &mut Output) {
        let from = m.nick().unwrap_or("").to_string();
        let target = m.param(0).to_string();
        let modes = m.params.iter().skip(1).cloned().collect::<Vec<_>>().join(" ");
        if !self.support.is_channel(&target) {
            self.info(out, None, Kind::Mode, &from, format!("Your modes: {modes}"), time);
            return;
        }
        let support = self.support.clone();
        let mut args = m.params.iter().skip(2);
        let mut set = true;
        let mut changed = false;
        for c in m.param(1).chars() {
            match c {
                '+' => set = true,
                '-' => set = false,
                c => {
                    let arg = if support.takes_param(c, set) { args.next() } else { None };
                    if let (Some(sym), Some(nick)) = (support.symbol(c), arg)
                        && let Some(ch) = self.channels.get_mut(&support.fold(&target))
                        && let Some((pre, _)) = ch.users.get_mut(&support.fold(nick))
                    {
                        if set && !pre.contains(sym) {
                            pre.push(sym);
                            let mut v: Vec<char> = pre.chars().collect();
                            v.sort_by_key(|s| support.rank(*s));
                            *pre = v.into_iter().collect();
                        } else if !set {
                            pre.retain(|s| s != sym);
                        }
                        changed = true;
                    }
                }
            }
        }
        self.info(out, Some(target.clone()), Kind::Mode, &from, format!("{from} set mode {modes}"), time);
        if changed {
            self.users_event(out, &target);
        }
    }

    /// Messages to say something, split to fit the line limit. Adds our own lines when
    /// the server won't echo them.
    pub fn say(&self, target: &str, text: &str, kind: Kind) -> Output {
        let mut out = Output::default();
        // 512 bytes less the prefix the server adds (nick!user@host up to ~100) and the
        // command, target and separators.
        let room = 512usize.saturating_sub(120 + target.len() + 20).max(64);
        let room = if kind == Kind::Action { room - 9 } else { room };
        for line in text.split('\n') {
            for part in split_text(line, room) {
                let (cmd, body) = match kind {
                    Kind::Action => ("PRIVMSG", format!("\x01ACTION {part}\x01")),
                    Kind::Notice => ("NOTICE", part.clone()),
                    _ => ("PRIVMSG", part.clone()),
                };
                out.send.push(Message::new(cmd, &[target, &body]));
                if !self.caps.contains("echo-message") {
                    let buffer = Some(target.to_string());
                    let l = Line {
                        time: now_ms(),
                        kind,
                        from: self.nick.clone(),
                        text: part,
                        highlight: false,
                        own: true,
                        history: false,
                    };
                    out.line(buffer, l);
                }
            }
        }
        out
    }
}

/// Split text into pieces of at most `max` bytes, at spaces where possible.
pub fn split_text(s: &str, max: usize) -> Vec<String> {
    let mut out = vec![];
    let mut rest = s;
    while rest.len() > max {
        let mut cut = max;
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        if let Some(sp) = rest[..cut].rfind(' ').filter(|&i| i > max / 2) {
            cut = sp;
        }
        out.push(rest[..cut].to_string());
        rest = rest[cut..].trim_start_matches(' ');
    }
    if !rest.is_empty() || out.is_empty() {
        out.push(rest.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(s: &mut Session, line: &str) -> Output {
        s.handle(&Message::parse(line).unwrap())
    }

    fn sent(o: &Output) -> Vec<String> {
        o.send.iter().map(|m| m.to_line()).collect()
    }

    #[test]
    fn negotiates_caps_and_sasl_plain() {
        let cfg = Config {
            nick: "ada".into(),
            sasl: Sasl::Plain { user: "ada".into(), pass: "secret".into() },
            ..Default::default()
        };
        let mut s = Session::new(cfg);
        let start: Vec<String> = s.start().iter().map(|m| m.to_line()).collect();
        assert_eq!(start, ["CAP LS 302", "NICK ada", "USER alouette 0 * :Alouette user"]);
        assert!(feed(&mut s, ":srv CAP * LS * :multi-prefix sasl=PLAIN,EXTERNAL").send.is_empty());
        let o = feed(&mut s, ":srv CAP * LS :server-time echo-message unknown-cap");
        let req = sent(&o);
        assert_eq!(req.len(), 1);
        for c in ["multi-prefix", "sasl", "server-time", "echo-message"] {
            assert!(req[0].contains(c), "{req:?}");
        }
        assert!(!req[0].contains("unknown-cap"));
        let o = feed(&mut s, ":srv CAP ada ACK :multi-prefix sasl server-time echo-message");
        assert_eq!(sent(&o), ["AUTHENTICATE PLAIN"]);
        let o = feed(&mut s, "AUTHENTICATE +");
        assert_eq!(sent(&o), ["AUTHENTICATE YWRhAGFkYQBzZWNyZXQ="]);
        assert!(feed(&mut s, ":srv 900 ada ada!a@h ada :You are now logged in as ada").events.len() == 1);
        let o = feed(&mut s, ":srv 903 ada :SASL authentication successful");
        assert_eq!(sent(&o), ["CAP END"]);
        assert_eq!(o.events[0], Event::SaslDone { account: "ada".into() });
        let o = feed(&mut s, ":srv 001 ada :Welcome");
        assert!(matches!(&o.events[0], Event::Registered { nick } if nick == "ada"));
    }

    #[test]
    fn sasl_failure_still_finishes_registration() {
        let cfg = Config { sasl: Sasl::External, ..Default::default() };
        let mut s = Session::new(cfg);
        s.start();
        // A server that lists only PLAIN: don't ask for sasl at all.
        let o = feed(&mut s, ":srv CAP * LS :sasl=PLAIN");
        assert_eq!(sent(&o), ["CAP END"]);
        let mut s = Session::new(Config { sasl: Sasl::External, ..Default::default() });
        s.start();
        feed(&mut s, ":srv CAP * LS :sasl");
        assert_eq!(sent(&feed(&mut s, ":srv CAP * ACK :sasl")), ["AUTHENTICATE EXTERNAL"]);
        assert_eq!(sent(&feed(&mut s, "AUTHENTICATE +")), ["AUTHENTICATE +"]);
        let o = feed(&mut s, ":srv 904 * :SASL authentication failed");
        assert_eq!(sent(&o), ["CAP END"]);
        assert!(o.events.iter().any(|e| matches!(e, Event::SaslFailed(_))));
    }

    #[test]
    fn no_caps_ends_at_once_and_nick_in_use_retries() {
        let mut s = Session::new(Config { nick: "ada".into(), alt_nicks: vec!["ada2".into()], ..Default::default() });
        s.start();
        assert_eq!(sent(&feed(&mut s, ":srv CAP * LS :")), ["CAP END"]);
        assert_eq!(sent(&feed(&mut s, ":srv 433 * ada :Nickname is already in use")), ["NICK ada2"]);
        assert_eq!(sent(&feed(&mut s, ":srv 433 * ada2 :Nickname is already in use")), ["NICK ada2_"]);
        feed(&mut s, ":srv 001 ada2_ :Welcome");
        assert_eq!(s.nick, "ada2_");
        assert_eq!(sent(&feed(&mut s, "PING :abc")), ["PONG abc"]);
    }

    fn joined() -> Session {
        let mut s =
            Session::new(Config { nick: "ada".into(), highlights: vec!["rustacean".into()], ..Default::default() });
        s.start();
        feed(&mut s, ":srv 001 ada :Welcome");
        feed(&mut s, ":srv 005 ada PREFIX=(ov)@+ CHANTYPES=# NETWORK=Test :are supported");
        feed(&mut s, ":ada!a@h JOIN #rust");
        feed(&mut s, ":srv 353 ada = #rust :@bob +carol ada");
        feed(&mut s, ":srv 366 ada #rust :End of /NAMES list.");
        s
    }

    #[test]
    fn tracks_channel_people() {
        let mut s = joined();
        let users = |s: &Session| {
            let mut v: Vec<(String, String)> = s.channel("#RUST").unwrap().users.values().cloned().collect();
            v.sort();
            v
        };
        assert_eq!(users(&s).len(), 3);
        let o = feed(&mut s, ":dan!d@h JOIN #rust");
        assert!(o.events.iter().any(|e| matches!(e, Event::Users { users, .. } if users.len() == 4)));
        let o = feed(&mut s, ":bob!b@h MODE #rust +v-o dan bob");
        let Some(Event::Users { users, .. }) = o.events.iter().find(|e| matches!(e, Event::Users { .. })) else {
            panic!("{:?}", o.events)
        };
        // Ranked: voiced first, then plain, by name.
        assert_eq!(users[0], ("+".into(), "carol".into()));
        assert_eq!(users[1], ("+".into(), "dan".into()));
        feed(&mut s, ":dan!d@h NICK dana");
        assert!(s.channel("#rust").unwrap().users.contains_key("dana"));
        let o = feed(&mut s, ":dana!d@h QUIT :bye");
        assert!(
            matches!(&o.events[0], Event::Line { buffer: Some(b), line } if b == "#rust" && line.kind == Kind::Quit)
        );
        let o = feed(&mut s, ":bob!b@h KICK #rust ada :out");
        assert!(o.events.contains(&Event::Left("#rust".into())));
        assert!(s.channel("#rust").is_none());
    }

    #[test]
    fn messages_go_to_the_right_buffer() {
        let mut s = joined();
        let line = |o: &Output| match &o.events[0] {
            Event::Line { buffer, line } => (buffer.clone(), line.clone()),
            e => panic!("{e:?}"),
        };
        let (b, l) = line(&feed(&mut s, "@time=2026-03-01T10:00:00.000Z :bob!b@h PRIVMSG #rust :hi ada!"));
        assert_eq!(b.as_deref(), Some("#rust"));
        assert!(l.highlight);
        assert_eq!(l.time, 1_772_359_200_000);
        let (_, l) = line(&feed(&mut s, ":bob!b@h PRIVMSG #rust :adamant is not a mention"));
        assert!(!l.highlight);
        let (_, l) = line(&feed(&mut s, ":bob!b@h PRIVMSG #rust :hello Rustacean"));
        assert!(l.highlight);
        let (b, l) = line(&feed(&mut s, ":bob!b@h PRIVMSG ada :psst"));
        assert_eq!(b.as_deref(), Some("bob"));
        assert!(l.highlight);
        // Our own echo goes to the target's buffer.
        let (b, l) = line(&feed(&mut s, ":ada!a@h PRIVMSG bob :hi back"));
        assert_eq!(b.as_deref(), Some("bob"));
        assert!(l.own && !l.highlight);
        let (_, l) = line(&feed(&mut s, ":bob!b@h PRIVMSG #rust :\x01ACTION waves\x01"));
        assert_eq!((l.kind, l.text.as_str()), (Kind::Action, "waves"));
        let o = feed(&mut s, ":bob!b@h PRIVMSG ada :\x01VERSION\x01");
        assert_eq!(sent(&o), ["NOTICE bob :\x01VERSION Alouette\x01"]);
        let (b, _) = line(&feed(&mut s, ":irc.example.org NOTICE ada :*** server notice"));
        assert_eq!(b, None);
        // History inside a chathistory batch is marked and never highlights.
        feed(&mut s, ":srv BATCH +h1 chathistory #rust");
        let (_, l) = line(&feed(&mut s, "@batch=h1 :bob!b@h PRIVMSG #rust :old news, ada"));
        assert!(l.history && !l.highlight);
        feed(&mut s, ":srv BATCH -h1");
        let (b, l) = line(&feed(&mut s, ":srv 404 ada #rust :Cannot send to channel"));
        assert_eq!((b.as_deref(), l.kind), (Some("#rust"), Kind::Error));
    }

    #[test]
    fn saying_things() {
        let mut s = joined();
        let o = s.say("#rust", "hello", Kind::Message);
        assert_eq!(sent(&o), ["PRIVMSG #rust hello"]);
        assert_eq!(o.events.len(), 1, "no echo-message: we add our own line");
        s.caps.insert("echo-message".into());
        let o = s.say("#rust", "waves", Kind::Action);
        assert_eq!(sent(&o), ["PRIVMSG #rust :\x01ACTION waves\x01"]);
        assert!(o.events.is_empty());
        let long = "word ".repeat(200);
        let o = s.say("#rust", &long, Kind::Message);
        assert!(o.send.len() >= 3);
        assert!(o.send.iter().all(|m| m.to_line().len() < 400));
        assert_eq!(split_text("ééé", 3), ["é", "é", "é"]);
    }

    #[test]
    fn chathistory_on_join() {
        let mut s = Session::new(Config { nick: "ada".into(), ..Default::default() });
        s.start();
        feed(&mut s, ":srv CAP * LS :draft/chathistory batch");
        feed(&mut s, ":srv CAP * ACK :draft/chathistory batch");
        feed(&mut s, ":srv 001 ada :Welcome");
        assert_eq!(sent(&feed(&mut s, ":ada!a@h JOIN #x")), ["CHATHISTORY LATEST #x * 50"]);
    }
}
