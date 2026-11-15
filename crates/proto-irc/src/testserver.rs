//! A small IRC server for tests and live checks: CAP 302, SASL PLAIN (account `me`,
//! password `pw`), server-time, echo-message, multi-prefix, batch and chathistory,
//! channels with topics and ops, private messages, and a bot called `echo` that answers
//! whatever it is told.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use crate::message::Message;

const CAPS: &str = "sasl=PLAIN server-time echo-message multi-prefix message-tags batch draft/chathistory";
const HOST: &str = "irc.test";

struct Client {
    nick: String,
    user: String,
    caps: Vec<String>,
    registered: bool,
    capping: bool,
    account: Option<String>,
    tx: mpsc::UnboundedSender<String>,
}

#[derive(Default)]
struct Chan {
    name: String,
    topic: String,
    members: HashMap<u64, String>,
    history: Vec<(String, String, i64)>,
}

#[derive(Default)]
struct State {
    next: u64,
    clients: HashMap<u64, Client>,
    chans: HashMap<String, Chan>,
}

pub type Shared = Arc<Mutex<StateHandle>>;

/// What tests can look at.
#[derive(Default)]
pub struct StateHandle {
    state: State,
}

impl StateHandle {
    pub fn nicks(&self) -> Vec<String> {
        self.state.clients.values().map(|c| c.nick.clone()).collect()
    }
    pub fn members(&self, chan: &str) -> Vec<String> {
        let s = &self.state;
        s.chans
            .get(&chan.to_lowercase())
            .map(|c| c.members.keys().filter_map(|id| s.clients.get(id)).map(|c| c.nick.clone()).collect())
            .unwrap_or_default()
    }
    pub fn account(&self, nick: &str) -> Option<String> {
        self.state.clients.values().find(|c| c.nick == nick).and_then(|c| c.account.clone())
    }
}

fn now_ms() -> i64 {
    crate::session::now_ms()
}

fn stamp(ms: i64) -> String {
    let t = time::OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    t.format(&time::format_description::well_known::Rfc3339).unwrap_or_default()
}

impl State {
    fn prefix(&self, id: u64) -> String {
        let c = &self.clients[&id];
        format!("{}!{}@{HOST}", c.nick, c.user)
    }
    fn send(&self, id: u64, m: &Message) {
        if let Some(c) = self.clients.get(&id) {
            let mut m = m.clone();
            if !c.caps.iter().any(|x| x == "server-time" || x == "message-tags") {
                m.tags.retain(|(k, _)| k != "time" && k != "msgid");
            }
            if !c.caps.iter().any(|x| x == "batch") {
                m.tags.retain(|(k, _)| k != "batch");
            }
            let _ = c.tx.send(format!("{}\r\n", m.to_line()));
        }
    }
    fn num(&self, id: u64, code: &str, params: &[&str]) {
        let nick =
            self.clients.get(&id).map(|c| c.nick.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| "*".into());
        let mut p = vec![nick.as_str()];
        p.extend_from_slice(params);
        self.send(id, &Message::new(code, &p).with_source(HOST));
    }
    fn find(&self, nick: &str) -> Option<u64> {
        self.clients.iter().find(|(_, c)| c.nick.eq_ignore_ascii_case(nick)).map(|(id, _)| *id)
    }
    /// Everyone who shares a channel with `id`, and `id`.
    fn neighbours(&self, id: u64) -> Vec<u64> {
        let mut v: Vec<u64> = self
            .chans
            .values()
            .filter(|c| c.members.contains_key(&id))
            .flat_map(|c| c.members.keys().copied())
            .collect();
        v.push(id);
        v.sort();
        v.dedup();
        v
    }
    fn names(&self, id: u64, chan: &str) {
        let Some(c) = self.chans.get(&chan.to_lowercase()) else { return };
        let multi = self.clients[&id].caps.iter().any(|x| x == "multi-prefix");
        let list: Vec<String> = c
            .members
            .iter()
            .filter_map(|(m, pre)| {
                let pre = if multi { pre.clone() } else { pre.chars().take(1).collect() };
                self.clients.get(m).map(|cl| format!("{pre}{}", cl.nick))
            })
            .collect();
        self.num(id, "353", &["=", &c.name, &list.join(" ")]);
        self.num(id, "366", &[&c.name, "End of /NAMES list."]);
    }
}

fn welcome(st: &mut State, id: u64) {
    let c = st.clients.get_mut(&id).unwrap();
    if c.registered || c.capping || c.nick.is_empty() || c.user.is_empty() {
        return;
    }
    c.registered = true;
    st.num(id, "001", &["Welcome to the Alouette test network"]);
    st.num(id, "002", &["Your host is irc.test"]);
    st.num(
        id,
        "005",
        &["NETWORK=TestNet", "CHANTYPES=#", "PREFIX=(ov)@+", "CASEMAPPING=ascii", "are supported by this server"],
    );
    st.num(id, "375", &["- irc.test Message of the day -"]);
    st.num(id, "372", &["- Be kind. The bot called echo answers private messages."]);
    st.num(id, "376", &["End of /MOTD command."]);
}

fn deliver(st: &mut State, from: u64, cmd: &str, target: &str, text: &str) {
    let src = st.prefix(from);
    let t = now_ms();
    let m = Message::new(cmd, &[target, text]).with_source(&src).with_tag("time", Some(&stamp(t)));
    if target.starts_with('#') {
        let key = target.to_lowercase();
        let Some(c) = st.chans.get_mut(&key) else {
            st.num(from, "403", &[target, "No such channel"]);
            return;
        };
        if !c.members.contains_key(&from) {
            st.num(from, "404", &[target, "Cannot send to channel"]);
            return;
        }
        if cmd == "PRIVMSG" {
            c.history.push((src.clone(), text.to_string(), t));
        }
        let members: Vec<u64> = c.members.keys().copied().collect();
        for m_id in members {
            let echo = st.clients[&from].caps.iter().any(|x| x == "echo-message");
            if m_id != from || echo {
                st.send(m_id, &m);
            }
        }
    } else if target.eq_ignore_ascii_case("echo") {
        if st.clients[&from].caps.iter().any(|x| x == "echo-message") {
            st.send(from, &m);
        }
        if cmd == "PRIVMSG" && !text.starts_with('\x01') {
            let me = st.clients[&from].nick.clone();
            let reply = Message::new("PRIVMSG", &[&me, &format!("You said: {text}")])
                .with_source(&format!("echo!bot@{HOST}"))
                .with_tag("time", Some(&stamp(now_ms())));
            st.send(from, &reply);
        }
    } else if let Some(to) = st.find(target) {
        st.send(to, &m);
        if st.clients[&from].caps.iter().any(|x| x == "echo-message") {
            st.send(from, &m);
        }
    } else {
        st.num(from, "401", &[target, "No such nick/channel"]);
    }
}

fn handle(shared: &Shared, id: u64, m: Message) -> bool {
    let mut h = shared.lock().unwrap();
    let st = &mut h.state;
    match m.command.as_str() {
        "CAP" => match m.param(0) {
            "LS" => {
                st.clients.get_mut(&id).unwrap().capping = true;
                st.send(id, &Message::new("CAP", &["*", "LS", CAPS]).with_source(HOST));
            }
            "REQ" => {
                let want: Vec<String> = m.trailing().split(' ').map(str::to_string).collect();
                let offered: Vec<&str> = CAPS.split(' ').map(|c| c.split('=').next().unwrap()).collect();
                if want.iter().all(|w| offered.contains(&w.as_str())) {
                    st.clients.get_mut(&id).unwrap().caps.extend(want);
                    st.send(id, &Message::new("CAP", &["*", "ACK", m.trailing()]).with_source(HOST));
                } else {
                    st.send(id, &Message::new("CAP", &["*", "NAK", m.trailing()]).with_source(HOST));
                }
            }
            "END" => {
                st.clients.get_mut(&id).unwrap().capping = false;
                welcome(st, id);
            }
            _ => {}
        },
        "AUTHENTICATE" => {
            if m.param(0) == "PLAIN" {
                st.send(id, &Message::new("AUTHENTICATE", &["+"]));
            } else {
                use base64::Engine;
                let raw = base64::engine::general_purpose::STANDARD.decode(m.param(0)).unwrap_or_default();
                let parts: Vec<&[u8]> = raw.split(|b| *b == 0).collect();
                if parts.len() == 3 && parts[1] == b"me" && parts[2] == b"pw" {
                    st.clients.get_mut(&id).unwrap().account = Some("me".into());
                    let src = format!("{}!{}@{HOST}", st.clients[&id].nick, st.clients[&id].user);
                    st.num(id, "900", &[&src, "me", "You are now logged in as me"]);
                    st.num(id, "903", &["SASL authentication successful"]);
                } else {
                    st.num(id, "904", &["SASL authentication failed"]);
                }
            }
        }
        "NICK" => {
            let new = m.param(0).to_string();
            if st.find(&new).is_some_and(|o| o != id) || new.eq_ignore_ascii_case("echo") {
                st.num(id, "433", &[&new, "Nickname is already in use"]);
            } else if st.clients[&id].registered {
                let src = st.prefix(id);
                let msg = Message::new("NICK", &[&new]).with_source(&src);
                for n in st.neighbours(id) {
                    st.send(n, &msg);
                }
                st.clients.get_mut(&id).unwrap().nick = new;
            } else {
                st.clients.get_mut(&id).unwrap().nick = new;
                welcome(st, id);
            }
        }
        "USER" => {
            st.clients.get_mut(&id).unwrap().user = m.param(0).to_string();
            welcome(st, id);
        }
        "PING" => st.send(id, &Message::new("PONG", &[HOST, m.param(0)]).with_source(HOST)),
        "JOIN" => {
            for name in m.param(0).split(',') {
                let key = name.to_lowercase();
                let c = st
                    .chans
                    .entry(key.clone())
                    .or_insert_with(|| Chan { name: name.to_string(), ..Default::default() });
                if c.members.contains_key(&id) {
                    continue;
                }
                let pre = if c.members.is_empty() { "@" } else { "" };
                c.members.insert(id, pre.to_string());
                let (cname, topic) = (c.name.clone(), c.topic.clone());
                let msg = Message::new("JOIN", &[&cname]).with_source(&st.prefix(id));
                let members: Vec<u64> = st.chans[&key].members.keys().copied().collect();
                for mm in members {
                    st.send(mm, &msg);
                }
                if !topic.is_empty() {
                    st.num(id, "332", &[&cname, &topic]);
                }
                st.names(id, &cname);
            }
        }
        "CHATHISTORY" if m.param(0) == "LATEST" => {
            let key = m.param(1).to_lowercase();
            let n: usize = m.param(3).parse().unwrap_or(50);
            if let Some(c) = st.chans.get(&key) {
                let items: Vec<(String, String, i64)> = c.history.iter().rev().take(n).rev().cloned().collect();
                let name = c.name.clone();
                st.send(id, &Message::new("BATCH", &["+hist", "chathistory", &name]).with_source(HOST));
                for (src, text, t) in items {
                    let msg = Message::new("PRIVMSG", &[&name, &text])
                        .with_source(&src)
                        .with_tag("time", Some(&stamp(t)))
                        .with_tag("batch", Some("hist"));
                    st.send(id, &msg);
                }
                st.send(id, &Message::new("BATCH", &["-hist"]).with_source(HOST));
            }
        }
        "PART" => {
            let key = m.param(0).to_lowercase();
            let src = st.prefix(id);
            if let Some(c) = st.chans.get(&key) {
                let msg = Message { params: m.params.clone(), ..Message::new("PART", &[]) }.with_source(&src);
                let members: Vec<u64> = c.members.keys().copied().collect();
                for mm in members {
                    st.send(mm, &msg);
                }
                st.chans.get_mut(&key).unwrap().members.remove(&id);
            }
        }
        "TOPIC" if m.params.len() >= 2 => {
            let key = m.param(0).to_lowercase();
            let src = st.prefix(id);
            if let Some(c) = st.chans.get_mut(&key) {
                c.topic = m.param(1).to_string();
                let msg = Message::new("TOPIC", &[&c.name, m.param(1)]).with_source(&src);
                let members: Vec<u64> = c.members.keys().copied().collect();
                for mm in members {
                    st.send(mm, &msg);
                }
            }
        }
        "MODE" if m.param(0).starts_with('#') && m.params.len() >= 3 => {
            let key = m.param(0).to_lowercase();
            let src = st.prefix(id);
            let target = st.find(m.param(2));
            let modes = m.param(1).to_string();
            if let (Some(c), Some(t)) = (st.chans.get_mut(&key), target) {
                if !c.members.get(&id).is_some_and(|p| p.contains('@')) {
                    st.num(id, "482", &[m.param(0), "You're not a channel operator"]);
                    return true;
                }
                let sym = if modes.contains('o') { '@' } else { '+' };
                if let Some(p) = c.members.get_mut(&t) {
                    if modes.starts_with('+') {
                        if !p.contains(sym) {
                            p.push(sym);
                        }
                    } else {
                        p.retain(|x| x != sym);
                    }
                }
                let msg = Message { params: m.params.clone(), ..Message::new("MODE", &[]) }.with_source(&src);
                let members: Vec<u64> = c.members.keys().copied().collect();
                for mm in members {
                    st.send(mm, &msg);
                }
            }
        }
        "PRIVMSG" | "NOTICE" => {
            let text = m.param(1).to_string();
            for t in m.param(0).split(',') {
                deliver(st, id, &m.command, t, &text);
            }
        }
        "WHOIS" => {
            match st.find(m.param(0)) {
                Some(t) => {
                    let (n, u) = (st.clients[&t].nick.clone(), st.clients[&t].user.clone());
                    st.num(id, "311", &[&n, &u, HOST, "*", "Test user"]);
                    if let Some(a) = st.clients[&t].account.clone() {
                        st.num(id, "330", &[&n, &a, "is logged in as"]);
                    }
                }
                None => st.num(id, "401", &[m.param(0), "No such nick"]),
            }
            st.num(id, "318", &[m.param(0), "End of /WHOIS list."]);
        }
        "QUIT" => {
            let src = st.prefix(id);
            let msg = Message::new("QUIT", &[m.param(0)]).with_source(&src);
            for n in st.neighbours(id) {
                if n != id {
                    st.send(n, &msg);
                }
            }
            st.send(id, &Message::new("ERROR", &["Closing link"]));
            return false;
        }
        _ => {}
    }
    true
}

fn leave(shared: &Shared, id: u64) {
    let mut h = shared.lock().unwrap();
    let st = &mut h.state;
    for c in st.chans.values_mut() {
        c.members.remove(&id);
    }
    st.clients.remove(&id);
}

/// Start the server on a free local port. Returns the port and a handle to its state.
pub async fn start() -> (u16, Shared) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let shared: Shared = Default::default();
    let sh = shared.clone();
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else { return };
            let shared = sh.clone();
            tokio::spawn(async move {
                let (tx, mut rx) = mpsc::unbounded_channel::<String>();
                let id = {
                    let mut h = shared.lock().unwrap();
                    h.state.next += 1;
                    let id = h.state.next;
                    h.state.clients.insert(
                        id,
                        Client {
                            nick: String::new(),
                            user: String::new(),
                            caps: vec![],
                            registered: false,
                            capping: false,
                            account: None,
                            tx,
                        },
                    );
                    id
                };
                let (r, mut w) = sock.into_split();
                let writer = tokio::spawn(async move {
                    while let Some(l) = rx.recv().await {
                        if w.write_all(l.as_bytes()).await.is_err() {
                            break;
                        }
                    }
                });
                let mut lines = BufReader::new(r).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    let Some(m) = Message::parse(&l) else { continue };
                    if !handle(&shared, id, m) {
                        break;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                leave(&shared, id);
                writer.abort();
            });
        }
    });
    (port, shared)
}
