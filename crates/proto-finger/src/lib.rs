//! Finger (RFC 1288) and `finger://` URLs.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use url::Url;

pub const DEFAULT_PORT: u16 = 79;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a finger address: {0}")]
    Address(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("the response is larger than {0} bytes")]
    TooLarge(usize),
}

/// A finger query: an optional user at a host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub user: String,
    pub host: String,
    pub port: u16,
    /// Ask for the long form (`/W`).
    pub verbose: bool,
}

impl Query {
    /// Accepts `finger://host/user`, `finger://user@host` and `user@host`.
    pub fn parse(input: &str) -> Result<Self, Error> {
        let input = input.trim();
        if let Ok(url) = Url::parse(input)
            && url.scheme() == "finger"
        {
            let host = url.host_str().ok_or_else(|| Error::Address(input.into()))?;
            let host = host.trim_start_matches('[').trim_end_matches(']').to_string();
            let path = url.path().trim_start_matches('/');
            let user = if !url.username().is_empty() { url.username().to_string() } else { decode(path) };
            return Ok(Self { user, host, port: url.port().unwrap_or(DEFAULT_PORT), verbose: false });
        }
        match input.rsplit_once('@') {
            Some((user, host)) if !host.is_empty() && !host.contains(['/', ' ']) => {
                Ok(Self { user: user.to_string(), host: host.to_string(), port: DEFAULT_PORT, verbose: false })
            }
            _ => Err(Error::Address(input.into())),
        }
    }

    pub fn url(&self) -> String {
        let port = if self.port == DEFAULT_PORT { String::new() } else { format!(":{}", self.port) };
        format!("finger://{}{port}/{}", self.host, self.user)
    }

    pub fn request(&self) -> String {
        let user: String = self.user.chars().filter(|c| !c.is_control()).collect();
        if self.verbose { format!("/W {user}\r\n") } else { format!("{user}\r\n") }
    }
}

fn decode(s: &str) -> String {
    url::form_urlencoded::parse(s.as_bytes())
        .map(|(k, v)| if v.is_empty() { k.into_owned() } else { format!("{k}={v}") })
        .collect()
}

/// Send the query and read the answer as text. Control characters other than tab and
/// newline are removed, as RFC 1288 asks of clients.
pub async fn fetch<S: AsyncRead + AsyncWrite + Unpin>(stream: &mut S, q: &Query, max: usize) -> Result<String, Error> {
    stream.write_all(q.request().as_bytes()).await?;
    stream.flush().await?;
    let mut body = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = match stream.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        };
        if body.len() + n > max {
            return Err(Error::TooLarge(max));
        }
        body.extend_from_slice(&buf[..n]);
    }
    Ok(clean(&String::from_utf8_lossy(&body)))
}

pub fn clean(s: &str) -> String {
    s.replace("\r\n", "\n").chars().filter(|c| !c.is_control() || *c == '\n' || *c == '\t').collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_addresses() {
        let q = Query::parse("finger://graph.no/oslo").unwrap();
        assert_eq!((q.user.as_str(), q.host.as_str(), q.port), ("oslo", "graph.no", 79));
        assert_eq!(Query::parse("kev@example.org").unwrap().url(), "finger://example.org/kev");
        assert_eq!(Query::parse("finger://kev@example.org:7979").unwrap().url(), "finger://example.org:7979/kev");
        assert_eq!(Query::parse("@example.org").unwrap().user, "");
        assert!(Query::parse("nope").is_err());
    }

    #[tokio::test]
    async fn fetches_and_cleans() {
        let (mut c, mut s) = tokio::io::duplex(1024);
        tokio::spawn(async move {
            let mut buf = [0u8; 32];
            let n = s.read(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], b"kev\r\n");
            s.write_all(b"Login: kev\r\n\x1b[31mPlan:\x07 build\r\n").await.unwrap();
        });
        let q = Query::parse("kev@h").unwrap();
        assert_eq!(fetch(&mut c, &q, 1024).await.unwrap(), "Login: kev\n[31mPlan: build\n");
    }
}
