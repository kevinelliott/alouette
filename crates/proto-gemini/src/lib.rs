//! Gemini protocol client pieces: requests, response headers, and the gemtext format.
//!
//! Connecting (TLS, trust on first use) is the caller's job; this crate speaks the protocol
//! over any async stream.

pub mod gemtext;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use url::Url;

pub const DEFAULT_PORT: u16 = 1965;
/// The longest request URL the spec allows, in bytes.
pub const MAX_URL: usize = 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a gemini URL: {0}")]
    Url(String),
    #[error("the URL is longer than 1024 bytes")]
    UrlTooLong,
    #[error("bad response header: {0}")]
    Header(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("the response is larger than {0} bytes")]
    TooLarge(usize),
}

/// What a status code asks the client to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 10, or 11 for sensitive input such as a password.
    Input {
        sensitive: bool,
    },
    Success,
    Redirect {
        permanent: bool,
    },
    TemporaryFailure,
    PermanentFailure,
    ClientCertificate,
}

/// A response header: two-digit status and its meta text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub status: u8,
    pub meta: String,
}

impl Header {
    pub fn parse(line: &str) -> Result<Self, Error> {
        let line = line.strip_suffix("\r\n").or_else(|| line.strip_suffix('\n')).unwrap_or(line);
        let (code, meta) = match line.split_once([' ', '\t']) {
            Some((c, m)) => (c, m.trim_start()),
            None => (line, ""),
        };
        if code.len() != 2 || !code.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Error::Header(line.chars().take(80).collect()));
        }
        let status: u8 = code.parse().map_err(|_| Error::Header(line.into()))?;
        if !(10..70).contains(&status) {
            return Err(Error::Header(format!("unknown status {status}")));
        }
        Ok(Self { status, meta: meta.to_string() })
    }

    pub fn kind(&self) -> Kind {
        match self.status / 10 {
            1 => Kind::Input { sensitive: self.status == 11 },
            2 => Kind::Success,
            3 => Kind::Redirect { permanent: self.status == 31 },
            4 => Kind::TemporaryFailure,
            5 => Kind::PermanentFailure,
            _ => Kind::ClientCertificate,
        }
    }

    /// The MIME type of a success response, defaulting to gemtext.
    pub fn mime(&self) -> &str {
        let m = self.meta.split(';').next().unwrap_or("").trim();
        if m.is_empty() { "text/gemini" } else { m }
    }

    /// A readable explanation of a failure status.
    pub fn describe(&self) -> String {
        let what = match self.status {
            40 => "temporary failure",
            41 => "server unavailable",
            42 => "CGI error",
            43 => "proxy error",
            44 => "slow down",
            50 => "permanent failure",
            51 => "not found",
            52 => "gone",
            53 => "proxy request refused",
            59 => "bad request",
            60 => "client certificate required",
            61 => "certificate not authorized",
            62 => "certificate not valid",
            _ => "error",
        };
        if self.meta.is_empty() {
            format!("{} {what}", self.status)
        } else {
            format!("{} {what}: {}", self.status, self.meta)
        }
    }
}

/// A complete response.
#[derive(Debug, Clone)]
pub struct Response {
    pub header: Header,
    pub body: Vec<u8>,
}

impl Response {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Parse and normalize a gemini URL, filling in the scheme for bare `host/path` input.
pub fn parse_url(input: &str) -> Result<Url, Error> {
    let input = input.trim();
    let url = Url::parse(input)
        .or_else(|_| Url::parse(&format!("gemini://{input}")))
        .map_err(|e| Error::Url(format!("{input}: {e}")))?;
    if url.scheme() != "gemini" {
        return Err(Error::Url(format!("scheme {}", url.scheme())));
    }
    if url.host_str().is_none() {
        return Err(Error::Url(format!("{input}: no host")));
    }
    Ok(url)
}

/// `url` with `query` as its percent-encoded query, for answering an input prompt.
pub fn with_query(url: &Url, query: &str) -> Url {
    let mut u = url.clone();
    let enc: String = query
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    u.set_query(Some(&enc));
    u
}

/// Send a request and read the header and body (up to `max` body bytes).
pub async fn fetch<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    url: &Url,
    max: usize,
) -> Result<Response, Error> {
    let req = format!("{url}\r\n");
    if req.len() > MAX_URL + 2 {
        return Err(Error::UrlTooLong);
    }
    stream.write_all(req.as_bytes()).await?;
    stream.flush().await?;

    // Header: at most 2 + 1 + 1024 + 2 bytes, ending in CRLF.
    let mut head = Vec::with_capacity(64);
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte).await {
            Ok(0) => break,
            Ok(_) => {
                head.push(byte[0]);
                if head.ends_with(b"\n") {
                    break;
                }
                if head.len() > 1100 {
                    return Err(Error::Header("header too long".into()));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
    }
    let header = Header::parse(&String::from_utf8_lossy(&head))?;
    let mut body = Vec::new();
    if header.kind() == Kind::Success {
        let mut buf = [0u8; 16 * 1024];
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
    }
    Ok(Response { header, body })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_headers() {
        let h = Header::parse("20 text/gemini; lang=en\r\n").unwrap();
        assert_eq!((h.kind(), h.mime()), (Kind::Success, "text/gemini"));
        assert_eq!(Header::parse("20\r\n").unwrap().mime(), "text/gemini");
        assert_eq!(Header::parse("11 Password\r\n").unwrap().kind(), Kind::Input { sensitive: true });
        assert_eq!(Header::parse("31 gemini://x/\r\n").unwrap().kind(), Kind::Redirect { permanent: true });
        assert_eq!(Header::parse("51 nope").unwrap().describe(), "51 not found: nope");
        assert!(Header::parse("2 text").is_err());
        assert!(Header::parse("HTTP/1.1 200 OK").is_err());
        assert!(Header::parse("99 x").is_err());
    }

    #[test]
    fn urls() {
        assert_eq!(parse_url("geminiprotocol.net/docs/").unwrap().as_str(), "gemini://geminiprotocol.net/docs/");
        assert!(parse_url("https://x").is_err());
        let u = parse_url("gemini://kennedy.gemi.dev/search").unwrap();
        assert_eq!(
            with_query(&u, "small web & you").as_str(),
            "gemini://kennedy.gemi.dev/search?small%20web%20%26%20you"
        );
    }

    #[tokio::test]
    async fn fetches() {
        let (mut c, mut s) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let mut buf = [0u8; 128];
            let n = s.read(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], b"gemini://h/\r\n");
            s.write_all(b"20 text/gemini\r\n# Hi\n=> /a A\n").await.unwrap();
        });
        let r = fetch(&mut c, &parse_url("gemini://h/").unwrap(), 1024).await.unwrap();
        assert_eq!(r.header.status, 20);
        assert_eq!(r.text(), "# Hi\n=> /a A\n");
    }
}
