//! An in-memory NNTP server for tests: CAPABILITIES, MODE READER, AUTHINFO, LIST ACTIVE
//! and NEWSGROUPS, GROUP, OVER, ARTICLE, HEAD and POST. Not a real server.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

#[derive(Default)]
pub struct Store {
    /// Group name to (description, articles by number).
    pub groups: BTreeMap<String, (String, BTreeMap<u64, Vec<u8>>)>,
}

impl Store {
    pub fn add(&mut self, group: &str, raw: &[u8]) -> u64 {
        let g = self.groups.entry(group.to_string()).or_default();
        let n = g.1.keys().next_back().copied().unwrap_or(0) + 1;
        g.1.insert(n, raw.to_vec());
        n
    }
}

#[derive(Clone)]
pub struct Server {
    /// When set, reading needs AUTHINFO with these.
    pub login: Option<(String, String)>,
    pub store: Arc<Mutex<Store>>,
}

impl Server {
    pub fn new() -> Self {
        Self { login: None, store: Arc::default() }
    }
    pub fn group(&self, name: &str, description: &str) {
        self.store.lock().unwrap().groups.entry(name.into()).or_default().0 = description.into();
    }
    pub fn deliver(&self, group: &str, raw: &[u8]) -> u64 {
        self.store.lock().unwrap().add(group, raw)
    }
}

impl Default for Server {
    fn default() -> Self {
        Self::new()
    }
}

pub async fn start(srv: Server) -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((s, _)) = l.accept().await {
            let srv = srv.clone();
            tokio::spawn(async move {
                let _ = run(BufReader::new(s), srv).await;
            });
        }
    });
    addr
}

fn header(raw: &[u8], name: &str) -> String {
    let text = String::from_utf8_lossy(raw);
    let head = text.split("\r\n\r\n").next().unwrap_or("");
    let mut out = String::new();
    let mut on = false;
    for l in head.split("\r\n") {
        if l.starts_with([' ', '\t']) {
            if on {
                out.push_str(l);
            }
            continue;
        }
        on = l.split_once(':').is_some_and(|(k, _)| k.eq_ignore_ascii_case(name));
        if on {
            out = l.split_once(':').map(|(_, v)| v.trim().to_string()).unwrap_or_default();
        }
    }
    out
}

fn dot(block: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for l in String::from_utf8_lossy(block).split("\r\n") {
        if l.starts_with('.') {
            out.push(b'.');
        }
        out.extend_from_slice(l.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    if block.ends_with(b"\r\n") {
        out.truncate(out.len() - 2);
    }
    out.extend_from_slice(b".\r\n");
    out
}

async fn run(mut r: BufReader<tokio::net::TcpStream>, srv: Server) -> std::io::Result<()> {
    r.get_mut().write_all(b"200 test nntpd ready, posting ok\r\n").await?;
    let mut authed = srv.login.is_none();
    let mut user = None;
    let mut cur: Option<String> = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        let l = line.trim_end();
        let (verb, rest) = l.split_once(' ').unwrap_or((l, ""));
        let verb = verb.to_ascii_uppercase();
        let mut out = Vec::new();
        let needs_auth = !matches!(verb.as_str(), "CAPABILITIES" | "MODE" | "AUTHINFO" | "QUIT");
        if needs_auth && !authed {
            r.get_mut().write_all(b"480 authentication required\r\n").await?;
            continue;
        }
        match verb.as_str() {
            "CAPABILITIES" => {
                out.extend_from_slice(b"101 caps\r\nVERSION 2\r\nREADER\r\nPOST\r\nOVER\r\nLIST ACTIVE NEWSGROUPS\r\n");
                if srv.login.is_some() && !authed {
                    out.extend_from_slice(b"AUTHINFO USER\r\n");
                }
                out.extend_from_slice(b".\r\n");
            }
            "MODE" => out.extend_from_slice(b"200 reader\r\n"),
            "AUTHINFO" => {
                let (k, v) = rest.split_once(' ').unwrap_or((rest, ""));
                match k.to_ascii_uppercase().as_str() {
                    "USER" => {
                        user = Some(v.to_string());
                        out.extend_from_slice(b"381 password please\r\n");
                    }
                    "PASS" => {
                        let ok = srv.login.as_ref().is_some_and(|(u, p)| user.as_deref() == Some(u.as_str()) && v == p);
                        if ok {
                            authed = true;
                            out.extend_from_slice(b"281 welcome\r\n");
                        } else {
                            out.extend_from_slice(b"481 no\r\n");
                        }
                    }
                    _ => out.extend_from_slice(b"501 eh\r\n"),
                }
            }
            "LIST" => {
                let st = srv.store.lock().unwrap();
                let kind = rest.split_whitespace().next().unwrap_or("ACTIVE").to_ascii_uppercase();
                out.extend_from_slice(b"215 list follows\r\n");
                for (name, (desc, arts)) in &st.groups {
                    let line = if kind == "NEWSGROUPS" {
                        format!("{name}\t{desc}\r\n")
                    } else {
                        let lo = arts.keys().next().copied().unwrap_or(1);
                        let hi = arts.keys().next_back().copied().unwrap_or(0);
                        format!("{name} {hi} {lo} y\r\n")
                    };
                    out.extend_from_slice(line.as_bytes());
                }
                out.extend_from_slice(b".\r\n");
            }
            "GROUP" => {
                let st = srv.store.lock().unwrap();
                match st.groups.get(rest) {
                    Some((_, arts)) => {
                        let lo = arts.keys().next().copied().unwrap_or(1);
                        let hi = arts.keys().next_back().copied().unwrap_or(0);
                        out.extend_from_slice(format!("211 {} {lo} {hi} {rest}\r\n", arts.len()).as_bytes());
                        cur = Some(rest.to_string());
                    }
                    None => out.extend_from_slice(b"411 no such group\r\n"),
                }
            }
            "OVER" | "XOVER" => {
                let st = srv.store.lock().unwrap();
                if let Some((_, arts)) = cur.as_ref().and_then(|g| st.groups.get(g)) {
                    let (a, b) = rest.split_once('-').unwrap_or((rest, rest));
                    let a: u64 = a.parse().unwrap_or(0);
                    let b: u64 = b.parse().unwrap_or(u64::MAX);
                    out.extend_from_slice(b"224 overview\r\n");
                    for (n, raw) in arts.range(a..=b) {
                        let h = |k| header(raw, k).replace('\t', " ");
                        let lines = String::from_utf8_lossy(raw)
                            .split("\r\n\r\n")
                            .nth(1)
                            .map(|b| b.lines().count())
                            .unwrap_or(0);
                        out.extend_from_slice(
                            format!(
                                "{n}\t{}\t{}\t{}\t{}\t{}\t{}\t{lines}\r\n",
                                h("Subject"),
                                h("From"),
                                h("Date"),
                                h("Message-ID"),
                                h("References"),
                                raw.len()
                            )
                            .as_bytes(),
                        );
                    }
                    out.extend_from_slice(b".\r\n");
                } else {
                    out.extend_from_slice(b"412 no group\r\n");
                }
            }
            "ARTICLE" | "HEAD" => {
                let st = srv.store.lock().unwrap();
                let found = if rest.starts_with('<') {
                    st.groups
                        .values()
                        .flat_map(|(_, a)| a.iter())
                        .find(|(_, raw)| header(raw, "Message-ID") == rest)
                        .map(|(n, raw)| (*n, raw.clone()))
                } else {
                    let n: u64 = rest.parse().unwrap_or(0);
                    cur.as_ref().and_then(|g| st.groups.get(g)).and_then(|(_, a)| a.get(&n)).map(|raw| (n, raw.clone()))
                };
                match found {
                    Some((n, raw)) => {
                        let id = header(&raw, "Message-ID");
                        let (code, body) = if verb == "HEAD" {
                            let text = String::from_utf8_lossy(&raw);
                            let h = text.split("\r\n\r\n").next().unwrap_or("").to_string() + "\r\n";
                            (221, h.into_bytes())
                        } else {
                            (220, raw)
                        };
                        out.extend_from_slice(format!("{code} {n} {id}\r\n").as_bytes());
                        out.extend_from_slice(&dot(&body));
                    }
                    None => out.extend_from_slice(b"430 no such article\r\n"),
                }
            }
            "POST" => {
                r.get_mut().write_all(b"340 send it\r\n").await?;
                let mut raw = Vec::new();
                loop {
                    let mut l = String::new();
                    if r.read_line(&mut l).await? == 0 {
                        return Ok(());
                    }
                    let t = l.trim_end_matches(['\r', '\n']);
                    if t == "." {
                        break;
                    }
                    raw.extend_from_slice(t.strip_prefix('.').unwrap_or(t).as_bytes());
                    raw.extend_from_slice(b"\r\n");
                }
                let groups = header(&raw, "Newsgroups");
                let mut st = srv.store.lock().unwrap();
                let mut any = false;
                for g in groups.split(',').map(str::trim).filter(|g| !g.is_empty()) {
                    if st.groups.contains_key(g) {
                        st.add(g, &raw);
                        any = true;
                    }
                }
                out.extend_from_slice(if any { b"240 posted\r\n" } else { b"441 no such group\r\n" });
            }
            "QUIT" => {
                r.get_mut().write_all(b"205 bye\r\n").await?;
                return Ok(());
            }
            _ => out.extend_from_slice(b"500 what\r\n"),
        }
        r.get_mut().write_all(&out).await?;
    }
}
