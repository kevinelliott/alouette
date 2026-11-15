//! An async NNTP client for reading and posting news (RFC 3977): CAPABILITIES, MODE
//! READER, STARTTLS (RFC 4642), AUTHINFO USER/PASS (RFC 4643), LIST ACTIVE and
//! NEWSGROUPS, GROUP, OVER (or XOVER), ARTICLE, HEAD and POST. The connection is any
//! stream, so TLS (NNTPS on port 563, or STARTTLS) is the caller's choice.

#[cfg(any(test, feature = "test-server"))]
pub mod testserver;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("the server said {code} {text}")]
    Reply { code: u16, text: String },
    #[error("bad reply from the server: {0}")]
    Protocol(String),
    #[error("{0}")]
    Unsupported(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// The most a multi-line reply may hold, so a broken server cannot fill memory.
const MAX_BLOCK: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub code: u16,
    pub text: String,
}

impl Reply {
    fn ok(self, want: &[u16]) -> Result<Reply> {
        if want.contains(&self.code) { Ok(self) } else { Err(Error::Reply { code: self.code, text: self.text }) }
    }
}

/// A newsgroup from LIST ACTIVE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub name: String,
    pub high: u64,
    pub low: u64,
    /// `y` posting allowed, `n` not, `m` moderated.
    pub status: char,
}

/// The answer to GROUP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selected {
    pub count: u64,
    pub low: u64,
    pub high: u64,
}

/// One line of the overview database.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overview {
    pub number: u64,
    pub subject: String,
    pub from: String,
    pub date: String,
    pub message_id: String,
    pub references: String,
    pub bytes: u64,
    pub lines: u64,
}

impl Overview {
    /// Parse a tab-separated OVER line.
    pub fn parse(line: &str) -> Option<Overview> {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 8 {
            return None;
        }
        Some(Overview {
            number: f[0].trim().parse().ok()?,
            subject: f[1].to_string(),
            from: f[2].to_string(),
            date: f[3].to_string(),
            message_id: f[4].to_string(),
            references: f[5].to_string(),
            bytes: f[6].trim().parse().unwrap_or(0),
            lines: f[7].trim().parse().unwrap_or(0),
        })
    }

    /// The message IDs in References, oldest first.
    pub fn parents(&self) -> Vec<&str> {
        self.references.split_whitespace().filter(|r| r.starts_with('<')).collect()
    }
}

pub struct Nntp {
    io: BufReader<Box<dyn Io>>,
    pub greeting: String,
    /// True when the greeting (or MODE READER) said posting is allowed.
    pub posting: bool,
    /// Capability lines, upper-cased first words.
    pub caps: Vec<(String, String)>,
}

impl Nntp {
    pub async fn start(stream: Box<dyn Io>) -> Result<Self> {
        let mut s = Self { io: BufReader::new(stream), greeting: String::new(), posting: false, caps: vec![] };
        let r = s.read().await?.ok(&[200, 201])?;
        s.posting = r.code == 200;
        s.greeting = r.text;
        Ok(s)
    }

    pub fn has(&self, k: &str) -> bool {
        self.caps.iter().any(|(c, _)| c.eq_ignore_ascii_case(k))
    }

    fn cap_args(&self, k: &str) -> &str {
        self.caps.iter().find(|(c, _)| c.eq_ignore_ascii_case(k)).map(|(_, a)| a.as_str()).unwrap_or("")
    }

    /// Ask what the server can do. Old servers without CAPABILITIES leave the list empty.
    pub async fn capabilities(&mut self) -> Result<()> {
        let r = self.cmd("CAPABILITIES").await?;
        if r.code != 101 {
            self.caps.clear();
            return Ok(());
        }
        self.caps = self
            .lines()
            .await?
            .into_iter()
            .map(|l| {
                let (k, a) = l.split_once(' ').unwrap_or((&l, ""));
                (k.to_ascii_uppercase(), a.to_string())
            })
            .collect();
        Ok(())
    }

    /// Switch a mode-switching server to reading. Harmless on others.
    pub async fn mode_reader(&mut self) -> Result<()> {
        if self.has("READER") && !self.has("MODE-READER") {
            return Ok(());
        }
        let r = self.cmd("MODE READER").await?;
        match r.code {
            200 => self.posting = true,
            201 => self.posting = false,
            // Not every server knows MODE READER.
            500..=502 => {}
            _ => return Err(Error::Reply { code: r.code, text: r.text }),
        }
        Ok(())
    }

    /// STARTTLS. Then take the stream with [`Nntp::into_inner`], do the handshake, carry on
    /// with [`Nntp::resume`] and ask for capabilities again.
    pub async fn starttls(&mut self) -> Result<()> {
        if !self.has("STARTTLS") {
            return Err(Error::Unsupported("the server does not offer STARTTLS".into()));
        }
        self.cmd("STARTTLS").await?.ok(&[382]).map(|_| ())
    }

    pub fn into_inner(self) -> Box<dyn Io> {
        self.io.into_inner()
    }

    pub fn resume(stream: Box<dyn Io>, greeting: String, posting: bool) -> Self {
        Self { io: BufReader::new(stream), greeting, posting, caps: vec![] }
    }

    /// AUTHINFO USER and PASS.
    pub async fn login(&mut self, user: &str, pass: &str) -> Result<()> {
        let r = self.cmd(&format!("AUTHINFO USER {user}")).await?;
        match r.code {
            281 => return Ok(()),
            381 => {}
            _ => return Err(Error::Reply { code: r.code, text: r.text }),
        }
        self.cmd(&format!("AUTHINFO PASS {pass}")).await?.ok(&[281]).map(|_| ())
    }

    /// Every group matching `wildmat` (such as `comp.lang.*`), or all of them.
    pub async fn list_active(&mut self, wildmat: Option<&str>) -> Result<Vec<Group>> {
        let c = match wildmat {
            Some(w) => format!("LIST ACTIVE {w}"),
            None => "LIST ACTIVE".into(),
        };
        let r = self.cmd(&c).await?;
        // Very old servers know only plain LIST.
        if r.code != 215 {
            if wildmat.is_some() {
                return Err(Error::Reply { code: r.code, text: r.text });
            }
            self.cmd("LIST").await?.ok(&[215])?;
        }
        Ok(self
            .lines()
            .await?
            .iter()
            .filter_map(|l| {
                let mut p = l.split_whitespace();
                Some(Group {
                    name: p.next()?.to_string(),
                    high: p.next()?.parse().ok()?,
                    low: p.next()?.parse().ok()?,
                    status: p.next().and_then(|s| s.chars().next()).unwrap_or('y'),
                })
            })
            .collect())
    }

    /// Group descriptions, as (name, description).
    pub async fn list_newsgroups(&mut self, wildmat: Option<&str>) -> Result<Vec<(String, String)>> {
        let c = match wildmat {
            Some(w) => format!("LIST NEWSGROUPS {w}"),
            None => "LIST NEWSGROUPS".into(),
        };
        self.cmd(&c).await?.ok(&[215])?;
        Ok(self
            .lines()
            .await?
            .into_iter()
            .filter_map(|l| {
                let (n, d) = l.split_once(['\t', ' '])?;
                Some((n.to_string(), d.trim().to_string()))
            })
            .collect())
    }

    pub async fn group(&mut self, name: &str) -> Result<Selected> {
        let r = self.cmd(&format!("GROUP {name}")).await?.ok(&[211])?;
        let n: Vec<u64> = r.text.split_whitespace().take(3).filter_map(|x| x.parse().ok()).collect();
        match n[..] {
            [count, low, high] => Ok(Selected { count, low, high }),
            _ => Err(Error::Protocol(r.text)),
        }
    }

    /// Overview lines for articles `from..=to` in the selected group.
    pub async fn over(&mut self, from: u64, to: u64) -> Result<Vec<Overview>> {
        let range = format!("{from}-{to}");
        let r = if self.caps.is_empty() || self.has("OVER") {
            self.cmd(&format!("OVER {range}")).await?
        } else {
            Reply { code: 500, text: String::new() }
        };
        let r = if matches!(r.code, 500 | 501) { self.cmd(&format!("XOVER {range}")).await? } else { r };
        // 423: no articles in that range.
        if r.code == 423 || r.code == 420 {
            return Ok(vec![]);
        }
        r.ok(&[224])?;
        Ok(self.lines().await?.iter().filter_map(|l| Overview::parse(l)).collect())
    }

    /// A whole article by number or `<message-id>`.
    pub async fn article(&mut self, which: &str) -> Result<Vec<u8>> {
        self.cmd(&format!("ARTICLE {which}")).await?.ok(&[220])?;
        self.block().await
    }

    /// Just the headers.
    pub async fn head(&mut self, which: &str) -> Result<Vec<u8>> {
        self.cmd(&format!("HEAD {which}")).await?.ok(&[221])?;
        self.block().await
    }

    /// Post an article: the whole RFC 5536 text, with Newsgroups, From and Subject.
    pub async fn post(&mut self, article: &[u8]) -> Result<()> {
        self.cmd("POST").await?.ok(&[340])?;
        let w = self.io.get_mut();
        w.write_all(&stuff(article)).await?;
        w.write_all(b".\r\n").await?;
        w.flush().await?;
        self.read().await?.ok(&[240]).map(|_| ())
    }

    /// The server's posting limits, if it says (from CAPABILITIES).
    pub fn implementation(&self) -> &str {
        self.cap_args("IMPLEMENTATION")
    }

    pub async fn quit(&mut self) {
        let _ = self.cmd("QUIT").await;
    }

    pub async fn cmd(&mut self, line: &str) -> Result<Reply> {
        if line.contains(['\r', '\n']) {
            return Err(Error::Protocol("a command cannot contain a line break".into()));
        }
        let w = self.io.get_mut();
        w.write_all(line.as_bytes()).await?;
        w.write_all(b"\r\n").await?;
        w.flush().await?;
        self.read().await
    }

    async fn read(&mut self) -> Result<Reply> {
        let mut line = String::new();
        if (&mut self.io).take(4096).read_line(&mut line).await? == 0 {
            return Err(Error::Protocol("the server closed the connection".into()));
        }
        let code: u16 =
            line.get(..3).and_then(|c| c.parse().ok()).ok_or_else(|| Error::Protocol(line.trim().into()))?;
        let r = Reply { code, text: line.get(4..).unwrap_or("").trim_end().to_string() };
        if code == 480 {
            return Err(Error::Reply { code, text: format!("{} (sign in first)", r.text) });
        }
        Ok(r)
    }

    /// A dot-terminated block, un-stuffed, with CRLF line endings.
    async fn block(&mut self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        loop {
            let mut line = Vec::new();
            if self.io.read_until(b'\n', &mut line).await? == 0 {
                return Err(Error::Protocol("the connection closed in the middle of a reply".into()));
            }
            let body = line.strip_suffix(b"\n").unwrap_or(&line);
            let body = body.strip_suffix(b"\r").unwrap_or(body);
            if body == b"." {
                return Ok(out);
            }
            let body = body.strip_prefix(b".").unwrap_or(body);
            out.extend_from_slice(body);
            out.extend_from_slice(b"\r\n");
            if out.len() > MAX_BLOCK {
                return Err(Error::Protocol("the reply is too large".into()));
            }
        }
    }

    async fn lines(&mut self) -> Result<Vec<String>> {
        let b = self.block().await?;
        Ok(String::from_utf8_lossy(&b).lines().map(str::to_string).collect())
    }
}

/// CRLF line endings, dot-stuffed, ending in CRLF.
pub fn stuff(m: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(m.len() + 64);
    for line in m.split(|&c| c == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.starts_with(b".") {
            out.push(b'.');
        }
        out.extend_from_slice(line);
        out.extend_from_slice(b"\r\n");
    }
    // split leaves an empty last piece after a final newline.
    if m.ends_with(b"\n") {
        out.truncate(out.len() - 2);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stuffing() {
        assert_eq!(stuff(b"a\n.b\r\n..c"), b"a\r\n..b\r\n...c\r\n");
        assert_eq!(stuff(b"x\r\n"), b"x\r\n");
    }

    #[test]
    fn overview_lines() {
        let o = Overview::parse("12\tRe: hi\tAda <a@x>\tTue, 2 Jan 2024 03:04:05 +0000\t<b@x>\t<a@x> <c@x>\t1200\t20")
            .unwrap();
        assert_eq!(o.number, 12);
        assert_eq!(o.parents(), ["<a@x>", "<c@x>"]);
        assert!(Overview::parse("bad").is_none());
    }
}
