//! An async IMAP client (RFC 3501 and 9051) with the extensions mail clients lean on:
//! IDLE, MOVE, UIDPLUS, LITERAL+, SPECIAL-USE, CONDSTORE-free UID sync, SASL PLAIN and
//! XOAUTH2. The connection is any stream, so TLS (and STARTTLS) is the caller's choice.

pub mod parse;
#[cfg(any(test, feature = "test-server"))]
pub mod testserver;

use std::collections::BTreeMap;
use std::time::Duration;

use base64::Engine;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

pub use parse::{Value, decode_mutf7, encode_mutf7};

pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// The server answered NO or BAD.
    #[error("the server said {status}: {text}")]
    No { status: String, text: String },
    #[error("bad response from the server: {0}")]
    Protocol(String),
    #[error("the server said goodbye: {0}")]
    Bye(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// One untagged or tagged response, tokenized.
#[derive(Debug, Clone)]
pub struct Response {
    pub tokens: Vec<Value>,
}

/// A mailbox from LIST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mailbox {
    /// The name as the server knows it (modified UTF-7).
    pub raw: String,
    /// The name for people.
    pub name: String,
    pub delimiter: Option<char>,
    pub flags: Vec<String>,
}

impl Mailbox {
    /// `\Sent`, `\Trash` and friends (RFC 6154), or INBOX.
    pub fn special_use(&self) -> Option<&str> {
        if self.raw.eq_ignore_ascii_case("INBOX") {
            return Some("\\Inbox");
        }
        self.flags.iter().map(String::as_str).find(|f| {
            ["\\All", "\\Archive", "\\Drafts", "\\Flagged", "\\Junk", "\\Sent", "\\Trash"]
                .iter()
                .any(|s| s.eq_ignore_ascii_case(f))
        })
    }
    pub fn selectable(&self) -> bool {
        !self.flags.iter().any(|f| f.eq_ignore_ascii_case("\\Noselect") || f.eq_ignore_ascii_case("\\NonExistent"))
    }
}

/// What SELECT or EXAMINE said.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selected {
    pub exists: u32,
    pub uidvalidity: u32,
    pub uidnext: u32,
    pub read_only: bool,
}

/// One message from FETCH.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fetched {
    pub seq: u32,
    pub uid: u32,
    pub flags: Vec<String>,
    pub size: Option<u64>,
    /// INTERNALDATE as the server wrote it.
    pub internal_date: Option<String>,
    /// Body sections by name, such as `BODY[]` or `BODY[HEADER]`.
    pub sections: BTreeMap<String, Vec<u8>>,
}

impl Fetched {
    /// The first body section, whatever it was called.
    pub fn body(&self) -> Option<&[u8]> {
        self.sections.values().next().map(Vec::as_slice)
    }
}

/// Something IDLE heard about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Exists(u32),
    Expunge(u32),
    Flags(u32),
    Timeout,
}

pub struct Imap {
    io: BufReader<Box<dyn Io>>,
    tag: u32,
    pub greeting: String,
    pub caps: Vec<String>,
    /// Untagged responses that arrived during the last command.
    pub untagged: Vec<Response>,
}

fn proto(e: impl Into<String>) -> Error {
    Error::Protocol(e.into())
}

/// A string argument: quoted if possible.
fn astring(s: &str) -> Arg {
    match parse::quote(s) {
        Some(q) => Arg::Text(q),
        None => Arg::Literal(s.as_bytes().to_vec()),
    }
}

enum Arg {
    Text(String),
    Literal(Vec<u8>),
}

impl Imap {
    /// Read the greeting. A PREAUTH greeting means no login is needed.
    pub async fn start(stream: Box<dyn Io>) -> Result<Self> {
        let mut c =
            Self { io: BufReader::new(stream), tag: 0, greeting: String::new(), caps: vec![], untagged: vec![] };
        let (line, r) = c.read_response().await?;
        let t = &r.tokens;
        if t.get(1).is_some_and(|v| v.is_atom("BYE")) {
            return Err(Error::Bye(line));
        }
        if !t.get(1).is_some_and(|v| v.is_atom("OK") || v.is_atom("PREAUTH")) {
            return Err(proto(line));
        }
        c.greeting = line.trim().to_string();
        c.take_caps_from_code(&r);
        if c.caps.is_empty() {
            c.capability().await?;
        }
        Ok(c)
    }

    pub fn has(&self, cap: &str) -> bool {
        self.caps.iter().any(|c| c.eq_ignore_ascii_case(cap))
    }

    /// Send STARTTLS. Afterwards take the stream with [`Imap::into_inner`], do the TLS
    /// handshake, and continue with [`Imap::resume`].
    pub async fn starttls(&mut self) -> Result<()> {
        self.run("STARTTLS").await.map(|_| ())
    }

    pub fn into_inner(self) -> Box<dyn Io> {
        self.io.into_inner()
    }

    /// Continue on a new stream (after STARTTLS); capabilities are asked again.
    pub async fn resume(stream: Box<dyn Io>, tag: u32, greeting: String) -> Result<Self> {
        let mut c = Self { io: BufReader::new(stream), tag, greeting, caps: vec![], untagged: vec![] };
        c.capability().await?;
        Ok(c)
    }

    pub fn tag_counter(&self) -> u32 {
        self.tag
    }

    pub async fn capability(&mut self) -> Result<()> {
        self.run("CAPABILITY").await?;
        let found: Vec<String> = self
            .untagged
            .iter()
            .filter(|r| r.tokens.get(1).is_some_and(|v| v.is_atom("CAPABILITY")))
            .flat_map(|r| r.tokens[2..].iter().filter_map(Value::as_str))
            .collect();
        if !found.is_empty() {
            self.caps = found;
        }
        Ok(())
    }

    fn take_caps_from_code(&mut self, r: &Response) {
        // * OK [CAPABILITY IMAP4rev1 ...] text
        if let Some(c) = code(r, "CAPABILITY") {
            self.caps = c.split(' ').map(str::to_string).collect();
        }
    }

    pub async fn login(&mut self, user: &str, pass: &str) -> Result<()> {
        if self.has("LOGINDISABLED") {
            return Err(Error::No {
                status: "NO".into(),
                text: "the server does not allow plain logins here (try TLS)".into(),
            });
        }
        self.run_args("LOGIN", vec![astring(user), astring(pass)]).await?;
        self.after_auth().await
    }

    /// SASL PLAIN, which handles any characters in the password.
    pub async fn authenticate_plain(&mut self, user: &str, pass: &str) -> Result<()> {
        let ir = base64::engine::general_purpose::STANDARD.encode(format!("\0{user}\0{pass}"));
        self.authenticate("PLAIN", &ir).await
    }

    /// SASL XOAUTH2 for Gmail and Microsoft 365.
    pub async fn authenticate_xoauth2(&mut self, user: &str, token: &str) -> Result<()> {
        let ir =
            base64::engine::general_purpose::STANDARD.encode(format!("user={user}\x01auth=Bearer {token}\x01\x01"));
        self.authenticate("XOAUTH2", &ir).await
    }

    async fn authenticate(&mut self, mech: &str, ir: &str) -> Result<()> {
        let tag = self.next_tag();
        if self.has("SASL-IR") {
            self.write(format!("{tag} AUTHENTICATE {mech} {ir}\r\n").as_bytes()).await?;
        } else {
            self.write(format!("{tag} AUTHENTICATE {mech}\r\n").as_bytes()).await?;
        }
        loop {
            let (line, r) = self.read_response().await?;
            if line.starts_with('+') {
                // The challenge: send the response, or (after a failure, for XOAUTH2) an empty line.
                if self.has("SASL-IR") {
                    self.write(b"\r\n").await?;
                } else {
                    self.write(format!("{ir}\r\n").as_bytes()).await?;
                }
                continue;
            }
            if self.tagged(&tag, &line, r)?.is_some() {
                return self.after_auth().await;
            }
        }
    }

    async fn after_auth(&mut self) -> Result<()> {
        // Servers often send new capabilities after login.
        let codes: Vec<Response> = self.untagged.clone();
        let before = self.caps.clone();
        for r in &codes {
            self.take_caps_from_code(r);
        }
        if self.caps == before {
            self.capability().await?;
        }
        if self.has("ENABLE") && self.has("UTF8=ACCEPT") {
            let _ = self.run("ENABLE UTF8=ACCEPT").await;
        }
        Ok(())
    }

    /// Every mailbox. Uses LIST-EXTENDED's SPECIAL-USE return when offered.
    pub async fn list(&mut self) -> Result<Vec<Mailbox>> {
        let cmd = if self.has("SPECIAL-USE") { r#"LIST "" "*" RETURN (SPECIAL-USE)"# } else { r#"LIST "" "*""# };
        self.run(cmd).await?;
        let mut out = vec![];
        for r in &self.untagged {
            let t = &r.tokens;
            if !t.get(1).is_some_and(|v| v.is_atom("LIST")) || t.len() < 5 {
                continue;
            }
            let flags = t[2].as_list().iter().filter_map(Value::as_str).collect();
            let delimiter = t[3].as_str().and_then(|s| s.chars().next());
            let raw = t[4].as_str().unwrap_or_default();
            let name = if self.has("UTF8=ACCEPT") { raw.clone() } else { decode_mutf7(&raw) };
            out.push(Mailbox { raw, name, delimiter, flags });
        }
        Ok(out)
    }

    /// Unseen and total counts for a mailbox not selected.
    pub async fn status(&mut self, raw: &str) -> Result<(u32, u32)> {
        let args = vec![astring(raw), Arg::Text("(MESSAGES UNSEEN)".into())];
        self.run_args("STATUS", args).await?;
        for r in &self.untagged {
            let t = &r.tokens;
            if t.get(1).is_some_and(|v| v.is_atom("STATUS"))
                && let Some(l) = t.get(3)
            {
                let l = l.as_list();
                let get = |k: &str| {
                    l.iter().position(|v| v.is_atom(k)).and_then(|i| l.get(i + 1)).and_then(Value::as_num).unwrap_or(0)
                        as u32
                };
                return Ok((get("UNSEEN"), get("MESSAGES")));
            }
        }
        Ok((0, 0))
    }

    pub async fn select(&mut self, raw: &str) -> Result<Selected> {
        self.open_box("SELECT", raw).await
    }

    pub async fn examine(&mut self, raw: &str) -> Result<Selected> {
        self.open_box("EXAMINE", raw).await
    }

    async fn open_box(&mut self, cmd: &str, raw: &str) -> Result<Selected> {
        let ok = self.run_args(cmd, vec![astring(raw)]).await?;
        let mut s = Selected { read_only: ok.contains("[READ-ONLY]"), ..Default::default() };
        for r in &self.untagged {
            let t = &r.tokens;
            if t.get(2).is_some_and(|v| v.is_atom("EXISTS")) {
                s.exists = t[1].as_num().unwrap_or(0) as u32;
            }
            if let Some(v) = code(r, "UIDVALIDITY") {
                s.uidvalidity = v.parse().unwrap_or(0);
            }
            if let Some(v) = code(r, "UIDNEXT") {
                s.uidnext = v.parse().unwrap_or(0);
            }
        }
        Ok(s)
    }

    /// UID SEARCH with raw criteria, such as `ALL`, `UNSEEN` or `UID 100:*`.
    pub async fn uid_search(&mut self, criteria: &str) -> Result<Vec<u32>> {
        let cmd = if criteria.is_ascii() {
            format!("UID SEARCH {criteria}")
        } else {
            format!("UID SEARCH CHARSET UTF-8 {criteria}")
        };
        self.run(&cmd).await?;
        let mut out: Vec<u32> = self
            .untagged
            .iter()
            .filter(|r| r.tokens.get(1).is_some_and(|v| v.is_atom("SEARCH")))
            .flat_map(|r| r.tokens[2..].iter().filter_map(Value::as_num).map(|n| n as u32))
            .collect();
        out.sort_unstable();
        Ok(out)
    }

    /// UID FETCH `set` with raw items, such as `(UID FLAGS RFC822.SIZE BODY.PEEK[HEADER])`.
    pub async fn uid_fetch(&mut self, set: &str, items: &str) -> Result<Vec<Fetched>> {
        self.run(&format!("UID FETCH {set} {items}")).await?;
        let mut out = vec![];
        for r in &self.untagged {
            let t = &r.tokens;
            if !t.get(2).is_some_and(|v| v.is_atom("FETCH")) {
                continue;
            }
            let mut f = Fetched { seq: t[1].as_num().unwrap_or(0) as u32, ..Default::default() };
            let l = t.get(3).map(Value::as_list).unwrap_or(&[]);
            for pair in l.chunks(2) {
                let [k, v] = pair else { continue };
                let Value::Atom(k) = k else { continue };
                let key = k.to_ascii_uppercase();
                match key.as_str() {
                    "UID" => f.uid = v.as_num().unwrap_or(0) as u32,
                    "FLAGS" => f.flags = v.as_list().iter().filter_map(Value::as_str).collect(),
                    "RFC822.SIZE" => f.size = v.as_num(),
                    "INTERNALDATE" => f.internal_date = v.as_str(),
                    _ if key.starts_with("BODY[") || key.starts_with("BINARY[") || key.starts_with("RFC822") => {
                        f.sections.insert(key, v.as_bytes().map(<[u8]>::to_vec).unwrap_or_default());
                    }
                    _ => {}
                }
            }
            if f.uid != 0 {
                out.push(f);
            }
        }
        Ok(out)
    }

    /// Add (`+FLAGS`) or remove (`-FLAGS`) flags, quietly.
    pub async fn uid_store(&mut self, set: &str, add: bool, flags: &[&str]) -> Result<()> {
        let op = if add { "+FLAGS.SILENT" } else { "-FLAGS.SILENT" };
        self.run(&format!("UID STORE {set} {op} ({})", flags.join(" "))).await.map(|_| ())
    }

    pub async fn uid_copy(&mut self, set: &str, to: &str) -> Result<()> {
        self.run_args(&format!("UID COPY {set}"), vec![astring(to)]).await.map(|_| ())
    }

    /// Move messages: MOVE where offered, otherwise copy, mark deleted and expunge.
    pub async fn uid_move(&mut self, set: &str, to: &str) -> Result<()> {
        if self.has("MOVE") {
            return self.run_args(&format!("UID MOVE {set}"), vec![astring(to)]).await.map(|_| ());
        }
        self.uid_copy(set, to).await?;
        self.uid_store(set, true, &["\\Deleted"]).await?;
        self.uid_expunge(set).await
    }

    /// Expunge just these messages where UIDPLUS allows, otherwise everything marked deleted.
    pub async fn uid_expunge(&mut self, set: &str) -> Result<()> {
        if self.has("UIDPLUS") {
            self.run(&format!("UID EXPUNGE {set}")).await.map(|_| ())
        } else {
            self.run("EXPUNGE").await.map(|_| ())
        }
    }

    /// Store a message, such as a sent copy or a draft.
    pub async fn append(&mut self, raw: &str, flags: &[&str], message: &[u8]) -> Result<()> {
        let mut args = vec![astring(raw)];
        if !flags.is_empty() {
            args.push(Arg::Text(format!("({})", flags.join(" "))));
        }
        args.push(Arg::Literal(message.to_vec()));
        self.run_args("APPEND", args).await.map(|_| ())
    }

    pub async fn create(&mut self, raw: &str) -> Result<()> {
        self.run_args("CREATE", vec![astring(raw)]).await.map(|_| ())
    }

    pub async fn noop(&mut self) -> Result<()> {
        self.run("NOOP").await.map(|_| ())
    }

    pub async fn logout(&mut self) {
        let _ = self.run("LOGOUT").await;
    }

    /// Wait for news in the selected mailbox (RFC 2177), up to `max`. Servers drop idle
    /// connections after 30 minutes, so keep `max` under that.
    pub async fn idle(&mut self, max: Duration) -> Result<Change> {
        if !self.has("IDLE") {
            tokio::time::sleep(max.min(Duration::from_secs(60))).await;
            self.noop().await?;
            return Ok(self.change_from_untagged().unwrap_or(Change::Timeout));
        }
        let tag = self.next_tag();
        self.write(format!("{tag} IDLE\r\n").as_bytes()).await?;
        let (line, _) = self.read_response().await?;
        if !line.starts_with('+') {
            return Err(Error::No { status: "BAD".into(), text: line });
        }
        let heard = match tokio::time::timeout(max, self.read_response()).await {
            Ok(r) => {
                let (_, r) = r?;
                change_of(&r).or(Some(Change::Timeout))
            }
            Err(_) => None,
        };
        self.write(b"DONE\r\n").await?;
        self.untagged.clear();
        loop {
            let (line, r) = self.read_response().await?;
            if self.tagged(&tag, &line, r)?.is_some() {
                break;
            }
        }
        Ok(heard.or_else(|| self.change_from_untagged()).unwrap_or(Change::Timeout))
    }

    fn change_from_untagged(&self) -> Option<Change> {
        self.untagged.iter().find_map(change_of)
    }

    /// Run a command with no arguments that need literals. Returns the tagged OK's text.
    pub async fn run(&mut self, cmd: &str) -> Result<String> {
        self.run_args(cmd, vec![]).await
    }

    async fn run_args(&mut self, cmd: &str, args: Vec<Arg>) -> Result<String> {
        self.untagged.clear();
        let tag = self.next_tag();
        let mut buf = format!("{tag} {cmd}").into_bytes();
        let plus = self.has("LITERAL+") || self.has("LITERAL-");
        for a in args {
            buf.push(b' ');
            match a {
                Arg::Text(t) => buf.extend_from_slice(t.as_bytes()),
                Arg::Literal(b) => {
                    if plus {
                        buf.extend_from_slice(format!("{{{}+}}\r\n", b.len()).as_bytes());
                        buf.extend_from_slice(&b);
                    } else {
                        buf.extend_from_slice(format!("{{{}}}\r\n", b.len()).as_bytes());
                        self.write(&buf).await?;
                        buf.clear();
                        // Wait for the go-ahead.
                        loop {
                            let (line, r) = self.read_response().await?;
                            if line.starts_with('+') {
                                break;
                            }
                            if self.tagged(&tag, &line, r)?.is_some() {
                                return Err(proto("the server answered before the literal"));
                            }
                        }
                        buf.extend_from_slice(&b);
                    }
                }
            }
        }
        buf.extend_from_slice(b"\r\n");
        self.write(&buf).await?;
        loop {
            let (line, r) = self.read_response().await?;
            if let Some(text) = self.tagged(&tag, &line, r)? {
                return Ok(text);
            }
        }
    }

    /// Handle one response for command `tag`: Some(text) when it is the tagged OK.
    fn tagged(&mut self, tag: &str, line: &str, r: Response) -> Result<Option<String>> {
        if line.starts_with("* ") {
            self.untagged.push(r);
            return Ok(None);
        }
        if let Some(rest) = line.strip_prefix(tag).and_then(|l| l.strip_prefix(' ')) {
            let (status, text) = rest.split_once(' ').unwrap_or((rest, ""));
            let text = text.trim().to_string();
            return match status.to_ascii_uppercase().as_str() {
                "OK" => Ok(Some(text)),
                s => Err(Error::No { status: s.to_string(), text }),
            };
        }
        Ok(None)
    }

    fn next_tag(&mut self) -> String {
        self.tag += 1;
        format!("A{:04}", self.tag)
    }

    async fn write(&mut self, b: &[u8]) -> Result<()> {
        let s = self.io.get_mut();
        s.write_all(b).await?;
        s.flush().await?;
        Ok(())
    }

    /// One whole response, literals included. Returns its first line (for messages) and tokens.
    async fn read_response(&mut self) -> Result<(String, Response)> {
        let mut all = Vec::new();
        loop {
            let mut line = Vec::new();
            let n = self.io.read_until(b'\n', &mut line).await?;
            if n == 0 {
                return Err(Error::Bye("the server closed the connection".into()));
            }
            if line.len() > 1 << 20 {
                return Err(proto("a response line over 1 MB"));
            }
            all.extend_from_slice(&line);
            match parse::literal_len(&line) {
                Some(len) if len <= 256 << 20 => {
                    let start = all.len();
                    all.resize(start + len, 0);
                    self.io.read_exact(&mut all[start..]).await?;
                }
                Some(_) => return Err(proto("a literal over 256 MB")),
                None => break,
            }
        }
        let first = all.split(|&c| c == b'\n').next().unwrap_or_default();
        let first = String::from_utf8_lossy(first).trim_end().to_string();
        let tokens = if first.starts_with('+') { vec![] } else { parse::tokens(&all).map_err(proto)? };
        Ok((first, Response { tokens }))
    }
}

/// The value of a response code such as `[UIDNEXT 7]` (one atom to the tokenizer).
fn code(r: &Response, key: &str) -> Option<String> {
    r.tokens.iter().find_map(|v| {
        let Value::Atom(a) = v else { return None };
        let inner = a.strip_prefix('[')?.strip_suffix(']')?;
        let (k, rest) = inner.split_once(' ').unwrap_or((inner, ""));
        k.eq_ignore_ascii_case(key).then(|| rest.to_string())
    })
}

fn change_of(r: &Response) -> Option<Change> {
    let t = &r.tokens;
    let n = t.get(1)?.as_num()? as u32;
    let kind = t.get(2)?;
    if kind.is_atom("EXISTS") {
        Some(Change::Exists(n))
    } else if kind.is_atom("EXPUNGE") {
        Some(Change::Expunge(n))
    } else if kind.is_atom("FETCH") {
        Some(Change::Flags(n))
    } else {
        None
    }
}
