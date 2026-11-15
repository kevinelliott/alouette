//! An async SMTP client for submitting mail (RFC 5321, 6409): EHLO, STARTTLS, AUTH PLAIN,
//! LOGIN and XOAUTH2, SIZE, 8BITMIME and SMTPUTF8, with dot-stuffing done for you. The
//! connection is any stream, so TLS is the caller's choice.

#[cfg(any(test, feature = "test-server"))]
pub mod testserver;

use base64::Engine;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub code: u16,
    pub text: String,
}

impl Reply {
    fn ok(self, f: impl Fn(u16) -> bool) -> Result<Reply> {
        if f(self.code) { Ok(self) } else { Err(Error::Reply { code: self.code, text: self.text }) }
    }
}

pub struct Smtp {
    io: BufReader<Box<dyn Io>>,
    pub greeting: String,
    /// EHLO keywords and their parameters, upper-cased keywords.
    pub ext: Vec<(String, String)>,
}

impl Smtp {
    pub async fn start(stream: Box<dyn Io>) -> Result<Self> {
        let mut s = Self { io: BufReader::new(stream), greeting: String::new(), ext: vec![] };
        let r = s.read().await?.ok(|c| c == 220)?;
        s.greeting = r.text;
        Ok(s)
    }

    pub fn has(&self, k: &str) -> bool {
        self.ext.iter().any(|(e, _)| e.eq_ignore_ascii_case(k))
    }

    fn param(&self, k: &str) -> Option<&str> {
        self.ext.iter().find(|(e, _)| e.eq_ignore_ascii_case(k)).map(|(_, p)| p.as_str())
    }

    /// Say hello with `name` (a host name or an address literal), learning the extensions.
    pub async fn ehlo(&mut self, name: &str) -> Result<()> {
        let r = self.cmd(&format!("EHLO {name}")).await?;
        if r.code != 250 {
            self.cmd(&format!("HELO {name}")).await?.ok(|c| c == 250)?;
            self.ext.clear();
            return Ok(());
        }
        self.ext = r
            .text
            .lines()
            .skip(1)
            .map(|l| {
                let (k, p) = l.split_once(' ').unwrap_or((l, ""));
                (k.to_ascii_uppercase(), p.to_string())
            })
            .collect();
        Ok(())
    }

    /// STARTTLS. Then take the stream with [`Smtp::into_inner`], do the handshake, continue
    /// with [`Smtp::resume`] and say EHLO again.
    pub async fn starttls(&mut self) -> Result<()> {
        if !self.has("STARTTLS") {
            return Err(Error::Unsupported("the server does not offer STARTTLS".into()));
        }
        self.cmd("STARTTLS").await?.ok(|c| c == 220).map(|_| ())
    }

    pub fn into_inner(self) -> Box<dyn Io> {
        self.io.into_inner()
    }

    pub fn resume(stream: Box<dyn Io>, greeting: String) -> Self {
        Self { io: BufReader::new(stream), greeting, ext: vec![] }
    }

    fn mechs(&self) -> Vec<String> {
        self.param("AUTH").unwrap_or_default().split_whitespace().map(|m| m.to_ascii_uppercase()).collect()
    }

    /// Log in with a password, by PLAIN or LOGIN, whichever the server offers.
    pub async fn login(&mut self, user: &str, pass: &str) -> Result<()> {
        let mechs = self.mechs();
        let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
        if mechs.iter().any(|m| m == "PLAIN") || mechs.is_empty() {
            let ir = b64(&format!("\0{user}\0{pass}"));
            return self.cmd(&format!("AUTH PLAIN {ir}")).await?.ok(|c| c == 235).map(|_| ());
        }
        if mechs.iter().any(|m| m == "LOGIN") {
            self.cmd("AUTH LOGIN").await?.ok(|c| c == 334)?;
            self.cmd(&b64(user)).await?.ok(|c| c == 334)?;
            return self.cmd(&b64(pass)).await?.ok(|c| c == 235).map(|_| ());
        }
        Err(Error::Unsupported(format!("the server only offers {}", mechs.join(", "))))
    }

    /// XOAUTH2 for Gmail and Microsoft 365.
    pub async fn login_xoauth2(&mut self, user: &str, token: &str) -> Result<()> {
        let ir =
            base64::engine::general_purpose::STANDARD.encode(format!("user={user}\x01auth=Bearer {token}\x01\x01"));
        let r = self.cmd(&format!("AUTH XOAUTH2 {ir}")).await?;
        if r.code == 334 {
            // An error in base64 JSON; an empty line ends the exchange.
            let detail = base64::engine::general_purpose::STANDARD
                .decode(r.text.trim())
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default();
            let end = self.cmd("").await?;
            return Err(Error::Reply { code: end.code, text: format!("{} {detail}", end.text) });
        }
        r.ok(|c| c == 235).map(|_| ())
    }

    /// Send one message. `message` is the whole RFC 5322 text; line endings are made CRLF
    /// and lines starting with a dot are escaped.
    pub async fn send(&mut self, from: &str, to: &[&str], message: &[u8]) -> Result<()> {
        if to.is_empty() {
            return Err(Error::Unsupported("no recipients".into()));
        }
        let utf8 = !from.is_ascii() || to.iter().any(|t| !t.is_ascii());
        if utf8 && !self.has("SMTPUTF8") {
            return Err(Error::Unsupported("the server cannot take non-ASCII addresses (no SMTPUTF8)".into()));
        }
        if let Some(max) = self.param("SIZE").and_then(|p| p.trim().parse::<u64>().ok())
            && max > 0
            && message.len() as u64 > max
        {
            return Err(Error::Unsupported(format!(
                "the message is {} bytes and the server takes at most {max}",
                message.len()
            )));
        }
        let mut mail = format!("MAIL FROM:<{from}>");
        if self.has("SIZE") {
            mail.push_str(&format!(" SIZE={}", message.len()));
        }
        if self.has("8BITMIME") && !message.is_ascii() {
            mail.push_str(" BODY=8BITMIME");
        }
        if utf8 {
            mail.push_str(" SMTPUTF8");
        }
        self.cmd(&mail).await?.ok(|c| c == 250)?;
        for t in to {
            self.cmd(&format!("RCPT TO:<{t}>")).await?.ok(|c| c == 250 || c == 251)?;
        }
        self.cmd("DATA").await?.ok(|c| c == 354)?;
        let body = stuff(message);
        let w = self.io.get_mut();
        w.write_all(&body).await?;
        w.write_all(b".\r\n").await?;
        w.flush().await?;
        self.read().await?.ok(|c| c == 250).map(|_| ())
    }

    pub async fn quit(&mut self) {
        let _ = self.cmd("QUIT").await;
    }

    async fn cmd(&mut self, line: &str) -> Result<Reply> {
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
        let mut text = String::new();
        for _ in 0..1000 {
            let mut line = String::new();
            if self.io.read_line(&mut line).await? == 0 {
                return Err(Error::Protocol("the server closed the connection".into()));
            }
            let code: u16 =
                line.get(..3).and_then(|c| c.parse().ok()).ok_or_else(|| Error::Protocol(line.trim().into()))?;
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(line.get(4..).unwrap_or("").trim_end());
            if line.as_bytes().get(3) != Some(&b'-') {
                return Ok(Reply { code, text });
            }
        }
        Err(Error::Protocol("a reply over 1000 lines".into()))
    }
}

/// CRLF line endings, dot-stuffed, ending in CRLF.
pub fn stuff(m: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(m.len() + 64);
    let mut at_start = true;
    let mut i = 0;
    while i < m.len() {
        let c = m[i];
        if at_start && c == b'.' {
            out.push(b'.');
        }
        at_start = false;
        match c {
            b'\r' if m.get(i + 1) == Some(&b'\n') => {
                out.extend_from_slice(b"\r\n");
                i += 1;
                at_start = true;
            }
            b'\n' | b'\r' => {
                out.extend_from_slice(b"\r\n");
                at_start = true;
            }
            _ => out.push(c),
        }
        i += 1;
    }
    if !out.ends_with(b"\r\n") {
        out.extend_from_slice(b"\r\n");
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn stuffing() {
        assert_eq!(super::stuff(b"a\n.b\r\n..c\rd"), b"a\r\n..b\r\n...c\r\nd\r\n");
        assert_eq!(super::stuff(b".\r\n"), b"..\r\n");
    }
}
