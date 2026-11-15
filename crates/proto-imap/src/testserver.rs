//! An in-memory IMAP server, enough for clients' tests: LOGIN and AUTHENTICATE PLAIN,
//! LIST with special-use flags, STATUS, SELECT, UID SEARCH/FETCH/STORE/MOVE/EXPUNGE, APPEND
//! and IDLE. Not a real server.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use base64::Engine;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;

#[derive(Debug, Clone)]
pub struct Message {
    pub uid: u32,
    pub flags: Vec<String>,
    pub raw: Vec<u8>,
}

#[derive(Default)]
pub struct Store {
    /// Mailbox name to (special-use flag, messages, next uid).
    pub boxes: BTreeMap<String, (Option<String>, Vec<Message>, u32)>,
}

impl Store {
    pub fn with_boxes() -> Self {
        let mut s = Store::default();
        for (n, f) in [
            ("INBOX", None),
            ("Sent", Some("\\Sent")),
            ("Drafts", Some("\\Drafts")),
            ("Trash", Some("\\Trash")),
            ("Archive", Some("\\Archive")),
        ] {
            s.boxes.insert(n.into(), (f.map(String::from), vec![], 1));
        }
        s
    }
    pub fn add(&mut self, mailbox: &str, raw: &[u8], flags: &[&str]) -> u32 {
        let b = self.boxes.entry(mailbox.into()).or_insert((None, vec![], 1));
        let uid = b.2;
        b.2 += 1;
        b.1.push(Message { uid, flags: flags.iter().map(|f| f.to_string()).collect(), raw: raw.to_vec() });
        uid
    }
}

#[derive(Clone)]
pub struct Server {
    pub user: String,
    pub password: String,
    pub store: Arc<Mutex<Store>>,
    /// Woken when a message is added, so IDLE can report it.
    pub news: Arc<Notify>,
}

impl Server {
    pub fn new(user: &str, password: &str) -> Self {
        Self {
            user: user.into(),
            password: password.into(),
            store: Arc::new(Mutex::new(Store::with_boxes())),
            news: Arc::default(),
        }
    }
    pub fn deliver(&self, raw: &[u8]) -> u32 {
        let uid = self.store.lock().unwrap().add("INBOX", raw, &[]);
        self.news.notify_waiters();
        uid
    }
}

pub async fn start(server: Server) -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((s, _)) = l.accept().await {
            let srv = server.clone();
            tokio::spawn(async move {
                let _ = session(s, srv).await;
            });
        }
    });
    addr
}

const CAPS: &str = "IMAP4rev1 SASL-IR AUTH=PLAIN IDLE MOVE UIDPLUS SPECIAL-USE LITERAL+";

/// Turn a UID set like `1:3,7,9:*` into a test.
fn in_set(set: &str, uid: u32, max: u32) -> bool {
    set.split(',').any(|part| {
        let num = |s: &str| if s == "*" { max } else { s.parse().unwrap_or(0) };
        match part.split_once(':') {
            Some((a, b)) => {
                let (a, b) = (num(a), num(b));
                (a.min(b)..=a.max(b)).contains(&uid)
            }
            None => num(part) == uid,
        }
    })
}

async fn read_line(r: &mut BufReader<TcpStream>) -> std::io::Result<Option<Vec<u8>>> {
    let mut line = vec![];
    if r.read_until(b'\n', &mut line).await? == 0 {
        return Ok(None);
    }
    Ok(Some(line))
}

async fn session(s: TcpStream, srv: Server) -> std::io::Result<()> {
    let mut r = BufReader::new(s);
    r.get_mut().write_all(format!("* OK [CAPABILITY {CAPS}] test imapd ready\r\n").as_bytes()).await?;
    let mut authed = false;
    let mut selected: Option<String> = None;
    while let Some(mut line) = read_line(&mut r).await? {
        // Collect literals: synchronizing ones get a go-ahead.
        while let Some(n) = crate::parse::literal_len(&line) {
            if !line.ends_with(b"+}\r\n") {
                r.get_mut().write_all(b"+ go ahead\r\n").await?;
            }
            let mut lit = vec![0; n];
            r.read_exact(&mut lit).await?;
            line.extend_from_slice(&lit);
            match read_line(&mut r).await? {
                Some(more) => line.extend_from_slice(&more),
                None => return Ok(()),
            }
        }
        let toks = crate::parse::tokens(&line).unwrap_or_default();
        let tag = toks.first().and_then(|v| v.as_str()).unwrap_or_default();
        let mut words: Vec<String> =
            toks.iter().skip(1).take(2).filter_map(|v| v.as_str()).map(|s| s.to_ascii_uppercase()).collect();
        if words.first().map(String::as_str) != Some("UID") {
            words.truncate(1);
        }
        let cmd = words.join(" ");
        let args: Vec<crate::Value> = toks.iter().skip(1 + words.len()).cloned().collect();
        let sargs: Vec<String> = args.iter().map(|v| v.as_str().unwrap_or_default()).collect();
        let arg = |i: usize| sargs.get(i).map(String::as_str).unwrap_or("");
        let mut out = String::new();
        let mut status = "OK done".to_string();
        let w = r.get_mut();
        match cmd.as_str() {
            "CAPABILITY" => out.push_str(&format!("* CAPABILITY {CAPS}\r\n")),
            "NOOP" => {}
            "LOGOUT" => {
                w.write_all(format!("* BYE bye\r\n{tag} OK LOGOUT done\r\n").as_bytes()).await?;
                return Ok(());
            }
            "LOGIN" => {
                authed = arg(0) == srv.user && arg(1) == srv.password;
                if !authed {
                    status = "NO [AUTHENTICATIONFAILED] wrong".into();
                }
            }
            "AUTHENTICATE" => {
                let ir = base64::engine::general_purpose::STANDARD.decode(arg(1)).unwrap_or_default();
                let parts: Vec<&[u8]> = ir.split(|&c| c == 0).collect();
                authed = arg(0).eq_ignore_ascii_case("PLAIN")
                    && parts.len() == 3
                    && parts[1] == srv.user.as_bytes()
                    && parts[2] == srv.password.as_bytes();
                if !authed {
                    status = "NO [AUTHENTICATIONFAILED] wrong".into();
                }
            }
            _ if !authed => status = "NO log in first".into(),
            "LIST" => {
                for (name, (flag, _, _)) in &srv.store.lock().unwrap().boxes {
                    let f = flag.clone().map(|f| format!("\\HasNoChildren {f}")).unwrap_or("\\HasNoChildren".into());
                    out.push_str(&format!("* LIST ({f}) \"/\" \"{name}\"\r\n"));
                }
            }
            "STATUS" => {
                let st = srv.store.lock().unwrap();
                match st.boxes.get(arg(0)) {
                    Some((_, m, _)) => {
                        let unseen = m.iter().filter(|m| !m.flags.iter().any(|f| f == "\\Seen")).count();
                        out.push_str(&format!("* STATUS \"{}\" (MESSAGES {} UNSEEN {unseen})\r\n", arg(0), m.len()));
                    }
                    None => status = "NO no such mailbox".into(),
                }
            }
            "SELECT" | "EXAMINE" => {
                let st = srv.store.lock().unwrap();
                match st.boxes.get(arg(0)) {
                    Some((_, m, next)) => {
                        out.push_str(&format!(
                            "* {} EXISTS\r\n* OK [UIDVALIDITY 42] ok\r\n* OK [UIDNEXT {next}] ok\r\n",
                            m.len()
                        ));
                        selected = Some(arg(0).to_string());
                        if cmd == "EXAMINE" {
                            status = "OK [READ-ONLY] done".into();
                        }
                    }
                    None => status = "NO no such mailbox".into(),
                }
            }
            "CREATE" => {
                srv.store.lock().unwrap().boxes.entry(arg(0).into()).or_insert((None, vec![], 1));
            }
            "APPEND" => {
                let name = arg(0).to_string();
                let flags: Vec<String> = args
                    .iter()
                    .find_map(|v| match v {
                        crate::Value::List(l) => Some(l.iter().filter_map(|f| f.as_str()).collect()),
                        _ => None,
                    })
                    .unwrap_or_default();
                let raw = args.last().and_then(|v| v.as_bytes()).unwrap_or_default().to_vec();
                let fl: Vec<&str> = flags.iter().map(String::as_str).collect();
                let uid = srv.store.lock().unwrap().add(&name, &raw, &fl);
                status = format!("OK [APPENDUID 42 {uid}] done");
            }
            "IDLE" => {
                w.write_all(b"+ idling\r\n").await?;
                let before = selected
                    .as_ref()
                    .map(|b| srv.store.lock().unwrap().boxes.get(b).map(|x| x.1.len()).unwrap_or(0))
                    .unwrap_or(0);
                let woke = {
                    let done = read_line(&mut r);
                    tokio::pin!(done);
                    tokio::select! {
                        _ = srv.news.notified() => true,
                        _ = &mut done => false,
                    }
                };
                if woke {
                    let now = selected
                        .as_ref()
                        .map(|b| srv.store.lock().unwrap().boxes.get(b).map(|x| x.1.len()).unwrap_or(0))
                        .unwrap_or(0);
                    if now != before {
                        r.get_mut().write_all(format!("* {now} EXISTS\r\n").as_bytes()).await?;
                    }
                    let _ = read_line(&mut r).await?;
                }
                r.get_mut().write_all(format!("{tag} OK IDLE done\r\n").as_bytes()).await?;
                continue;
            }
            c if c.starts_with("UID") => {
                let Some(name) = selected.clone() else {
                    w.write_all(format!("{tag} BAD select first\r\n").as_bytes()).await?;
                    continue;
                };
                let mut st = srv.store.lock().unwrap();
                let max = st.boxes.get(&name).and_then(|b| b.1.iter().map(|m| m.uid).max()).unwrap_or(0);
                let mut moved = vec![];
                {
                    let msgs = &mut st.boxes.get_mut(&name).unwrap().1;
                    match c {
                        "UID SEARCH" => {
                            let unseen = args.iter().any(|v| v.is_atom("UNSEEN"));
                            let uids: Vec<String> = msgs
                                .iter()
                                .filter(|m| !unseen || !m.flags.iter().any(|f| f == "\\Seen"))
                                .map(|m| m.uid.to_string())
                                .collect();
                            out.push_str(&format!("* SEARCH {}\r\n", uids.join(" ")));
                        }
                        "UID FETCH" => {
                            let items = String::from_utf8_lossy(&line).to_ascii_uppercase();
                            for (i, m) in msgs.iter_mut().enumerate() {
                                if !in_set(arg(0), m.uid, max) {
                                    continue;
                                }
                                let mut parts = vec![format!("UID {}", m.uid)];
                                if items.contains("FLAGS") {
                                    parts.push(format!("FLAGS ({})", m.flags.join(" ")));
                                }
                                if items.contains("RFC822.SIZE") {
                                    parts.push(format!("RFC822.SIZE {}", m.raw.len()));
                                }
                                if items.contains("INTERNALDATE") {
                                    parts.push("INTERNALDATE \"17-Jul-2024 02:44:25 +0000\"".into());
                                }
                                let mut lit = None;
                                if items.contains("BODY.PEEK[HEADER") || items.contains("BODY[HEADER") {
                                    let end = m
                                        .raw
                                        .windows(4)
                                        .position(|w| w == b"\r\n\r\n")
                                        .map(|p| p + 4)
                                        .unwrap_or(m.raw.len());
                                    lit = Some(("BODY[HEADER]", m.raw[..end].to_vec()));
                                } else if items.contains("BODY.PEEK[]") || items.contains("BODY[]") {
                                    lit = Some(("BODY[]", m.raw.clone()));
                                    if items.contains("BODY[]") && !m.flags.iter().any(|f| f == "\\Seen") {
                                        m.flags.push("\\Seen".into());
                                    }
                                }
                                out.push_str(&format!("* {} FETCH ({}", i + 1, parts.join(" ")));
                                if let Some((k, b)) = lit {
                                    out.push_str(&format!(" {k} {{{}}}\r\n", b.len()));
                                    out.push_str(&String::from_utf8_lossy(&b));
                                }
                                out.push_str(")\r\n");
                            }
                        }
                        "UID STORE" => {
                            let add = arg(1).starts_with('+');
                            let flags: Vec<String> = args
                                .get(2)
                                .map(|v| v.as_list().iter().filter_map(|f| f.as_str()).collect())
                                .unwrap_or_default();
                            for m in msgs.iter_mut().filter(|m| in_set(arg(0), m.uid, max)) {
                                for f in &flags {
                                    m.flags.retain(|x| x != f);
                                    if add {
                                        m.flags.push(f.clone());
                                    }
                                }
                            }
                        }
                        "UID MOVE" | "UID COPY" => {
                            for m in msgs.iter().filter(|m| in_set(arg(0), m.uid, max)) {
                                moved.push(m.clone());
                            }
                            if c == "UID MOVE" {
                                msgs.retain(|m| !in_set(arg(0), m.uid, max));
                            }
                        }
                        "UID EXPUNGE" => {
                            msgs.retain(|m| !(in_set(arg(0), m.uid, max) && m.flags.iter().any(|f| f == "\\Deleted")))
                        }
                        _ => status = "BAD unknown".into(),
                    }
                }
                if !moved.is_empty() {
                    let to = arg(1).to_string();
                    for m in moved {
                        let fl: Vec<&str> = m.flags.iter().map(String::as_str).collect();
                        st.add(&to, &m.raw, &fl);
                    }
                }
            }
            _ => status = "BAD unknown command".into(),
        }
        out.push_str(&format!("{tag} {status}\r\n"));
        r.get_mut().write_all(out.as_bytes()).await?;
    }
    Ok(())
}
