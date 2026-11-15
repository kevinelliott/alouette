//! Gopher (RFC 1436) with Gopher+ awareness and `gophers://` (Gopher over TLS) URLs.
//!
//! This crate parses URLs, menus and text, and runs one request over any async stream.
//! Connecting is up to the caller, so it works with plain TCP, TLS or a proxy.

use std::fmt;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use url::Url;

pub const DEFAULT_PORT: u16 = 70;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a gopher URL: {0}")]
    Url(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("the response is larger than {0} bytes")]
    TooLarge(usize),
}

/// A Gopher item type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemType {
    Text,
    Menu,
    CsoPh,
    Error,
    BinHex,
    Dos,
    Uuencoded,
    Search,
    Telnet,
    Binary,
    Mirror,
    Gif,
    Image,
    Tn3270,
    Info,
    Html,
    Sound,
    Document,
    Other(char),
}

impl ItemType {
    pub fn from_char(c: char) -> Self {
        match c {
            '0' => Self::Text,
            '1' => Self::Menu,
            '2' => Self::CsoPh,
            '3' => Self::Error,
            '4' => Self::BinHex,
            '5' => Self::Dos,
            '6' => Self::Uuencoded,
            '7' => Self::Search,
            '8' => Self::Telnet,
            '9' => Self::Binary,
            '+' => Self::Mirror,
            'g' => Self::Gif,
            'I' | 'p' | ':' => Self::Image,
            'T' => Self::Tn3270,
            'i' => Self::Info,
            'h' => Self::Html,
            's' | '<' => Self::Sound,
            'd' => Self::Document,
            other => Self::Other(other),
        }
    }

    pub fn to_char(self) -> char {
        match self {
            Self::Text => '0',
            Self::Menu => '1',
            Self::CsoPh => '2',
            Self::Error => '3',
            Self::BinHex => '4',
            Self::Dos => '5',
            Self::Uuencoded => '6',
            Self::Search => '7',
            Self::Telnet => '8',
            Self::Binary => '9',
            Self::Mirror => '+',
            Self::Gif => 'g',
            Self::Image => 'I',
            Self::Tn3270 => 'T',
            Self::Info => 'i',
            Self::Html => 'h',
            Self::Sound => 's',
            Self::Document => 'd',
            Self::Other(c) => c,
        }
    }

    /// A short label for menus, in the spirit of Minuet's item icons.
    pub fn label(self) -> &'static str {
        match self {
            Self::Text => "TXT",
            Self::Menu => "DIR",
            Self::CsoPh => "PH",
            Self::Error => "ERR",
            Self::Search => "?",
            Self::Telnet | Self::Tn3270 => "TEL",
            Self::Html => "WWW",
            Self::Gif | Self::Image => "IMG",
            Self::Sound => "SND",
            Self::Document => "DOC",
            Self::Info => "",
            Self::BinHex | Self::Dos | Self::Uuencoded | Self::Binary => "BIN",
            Self::Mirror => "DUP",
            Self::Other(_) => "???",
        }
    }

    /// Whether this type is shown as text in the browser rather than downloaded.
    pub fn is_textual(self) -> bool {
        matches!(self, Self::Text | Self::Menu | Self::Search | Self::Error | Self::CsoPh)
    }
}

/// One line of a Gopher menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub kind: ItemType,
    pub display: String,
    pub selector: String,
    pub host: String,
    pub port: u16,
    /// The server marked this item as Gopher+.
    pub plus: bool,
}

impl Item {
    /// The URL this item points to. `h` items with a `URL:` selector point at the web,
    /// and telnet items at `telnet://`.
    pub fn url(&self, tls: bool) -> Option<String> {
        match self.kind {
            ItemType::Info | ItemType::Error => None,
            ItemType::Html => {
                match self.selector.strip_prefix("URL:").or_else(|| self.selector.strip_prefix("/URL:")) {
                    Some(u) => Some(u.to_string()),
                    None => Some(self.gopher_url(tls)),
                }
            }
            ItemType::Telnet => {
                Some(format!("telnet://{}{}", host_port(&self.host, self.port, 23), user_part(&self.selector)))
            }
            ItemType::Tn3270 => Some(format!("tn3270://{}", host_port(&self.host, self.port, 23))),
            _ => Some(self.gopher_url(tls)),
        }
    }

    fn gopher_url(&self, tls: bool) -> String {
        let scheme = if tls { "gophers" } else { "gopher" };
        format!(
            "{scheme}://{}/{}{}",
            host_port(&self.host, self.port, DEFAULT_PORT),
            self.kind.to_char(),
            encode_selector(&self.selector)
        )
    }
}

fn user_part(selector: &str) -> String {
    if selector.is_empty() { String::new() } else { format!("/?user={}", encode_selector(selector)) }
}

fn host_port(host: &str, port: u16, default: u16) -> String {
    let h = if host.contains(':') && !host.starts_with('[') { format!("[{host}]") } else { host.to_string() };
    if port == default || port == 0 { h } else { format!("{h}:{port}") }
}

/// Percent-encode a selector for a URL path, keeping `/` readable.
pub fn encode_selector(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'.'
            | b'_'
            | b'~'
            | b'/'
            | b':'
            | b'@'
            | b'!'
            | b'$'
            | b'&'
            | b'\''
            | b'('
            | b')'
            | b'*'
            | b'+'
            | b','
            | b';'
            | b'=' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(h), Some(l)) = (hex(bytes.get(i + 1)), hex(bytes.get(i + 2)))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: Option<&u8>) -> Option<u8> {
    match b? {
        c @ b'0'..=b'9' => Some(c - b'0'),
        c @ b'a'..=b'f' => Some(c - b'a' + 10),
        c @ b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// A parsed `gopher://` or `gophers://` URL (RFC 4266).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GopherUrl {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    pub kind: ItemType,
    pub selector: String,
    /// The search string for type 7 items, after a `%09` (tab) in the URL.
    pub search: Option<String>,
}

impl GopherUrl {
    pub fn parse(input: &str) -> Result<Self, Error> {
        let url = Url::parse(input).map_err(|e| Error::Url(format!("{input}: {e}")))?;
        let tls = match url.scheme() {
            "gopher" => false,
            "gophers" => true,
            s => return Err(Error::Url(format!("scheme {s}"))),
        };
        let host = url.host_str().ok_or_else(|| Error::Url(format!("{input}: no host")))?.to_string();
        let host = host.trim_start_matches('[').trim_end_matches(']').to_string();
        let port = url.port().unwrap_or(DEFAULT_PORT);
        // The path is "/" + type + selector; a query, if any, is part of the selector.
        let mut path = url.path().to_string();
        if let Some(q) = url.query() {
            path.push('?');
            path.push_str(q);
        }
        let path = path.strip_prefix('/').unwrap_or(&path);
        let mut chars = path.chars();
        let (kind, rest) = match chars.next() {
            None => (ItemType::Menu, String::new()),
            Some(c) => (ItemType::from_char(c), chars.as_str().to_string()),
        };
        let rest = decode(&rest);
        let (selector, search) = match rest.split_once('\t') {
            Some((sel, s)) => {
                // Gopher+ strings follow a second tab; drop them.
                let s = s.split('\t').next().unwrap_or("");
                (sel.to_string(), Some(s.to_string()))
            }
            None => (rest, None),
        };
        Ok(Self { tls, host, port, kind, selector, search })
    }

    /// The line sent to the server.
    pub fn request(&self) -> String {
        match &self.search {
            Some(q) => format!("{}\t{}\r\n", self.selector, q),
            None => format!("{}\r\n", self.selector),
        }
    }

    /// This URL with a search string set, for type 7 items.
    pub fn with_search(&self, query: &str) -> Self {
        Self { search: Some(query.replace(['\t', '\r', '\n'], " ")), ..self.clone() }
    }
}

impl fmt::Display for GopherUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let scheme = if self.tls { "gophers" } else { "gopher" };
        write!(
            f,
            "{scheme}://{}/{}{}",
            host_port(&self.host, self.port, DEFAULT_PORT),
            self.kind.to_char(),
            encode_selector(&self.selector)
        )?;
        if let Some(q) = &self.search {
            write!(f, "%09{}", encode_selector(q))?;
        }
        Ok(())
    }
}

/// Parse a menu. Lenient: blank lines, missing fields and stray CRs are tolerated, and the
/// terminating `.` line ends the menu.
pub fn parse_menu(text: &str) -> Vec<Item> {
    let mut items = Vec::new();
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line == "." {
            break;
        }
        if line.is_empty() {
            continue;
        }
        let mut chars = line.chars();
        let Some(t) = chars.next() else { continue };
        let mut fields = chars.as_str().split('\t');
        let display = fields.next().unwrap_or("").to_string();
        let selector = fields.next().unwrap_or("").to_string();
        let host = fields.next().unwrap_or("").trim().to_string();
        let port = fields.next().and_then(|p| p.trim().parse().ok()).unwrap_or(DEFAULT_PORT);
        let plus = fields.next().is_some_and(|p| p.starts_with('+'));
        let mut kind = ItemType::from_char(t);
        // Lines without a tab are not menu items; show them as information.
        if !line.contains('\t') {
            items.push(Item {
                kind: ItemType::Info,
                display: line.to_string(),
                selector: String::new(),
                host: String::new(),
                port: 0,
                plus: false,
            });
            continue;
        }
        if kind != ItemType::Info && host.is_empty() && !matches!(kind, ItemType::Error) {
            kind = ItemType::Info;
        }
        items.push(Item { kind, display, selector, host, port, plus });
    }
    items
}

/// Turn a text item's bytes into text: drop the closing `.` line and undo dot-stuffing.
pub fn text_body(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line == "." {
            break;
        }
        out.push_str(line.strip_prefix("..").map(|l| format!(".{l}")).as_deref().unwrap_or(line));
        out.push('\n');
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

/// Send one request and read the whole response, up to `max` bytes.
pub async fn fetch<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    url: &GopherUrl,
    max: usize,
) -> Result<Vec<u8>, Error> {
    stream.write_all(url.request().as_bytes()).await?;
    stream.flush().await?;
    read_to_limit(stream, max).await
}

pub async fn read_to_limit<S: AsyncRead + Unpin>(stream: &mut S, max: usize) -> Result<Vec<u8>, Error> {
    let mut body = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        let n = match stream.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            // Many servers close without a TLS close_notify; treat that as the end.
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        };
        if body.len() + n > max {
            return Err(Error::TooLarge(max));
        }
        body.extend_from_slice(&buf[..n]);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_urls() {
        let u = GopherUrl::parse("gopher://gopher.floodgap.com").unwrap();
        assert_eq!((u.kind, u.selector.as_str(), u.port), (ItemType::Menu, "", 70));
        let u = GopherUrl::parse("gopher://example.org:7070/0/docs/read%20me.txt").unwrap();
        assert_eq!((u.kind, u.selector.as_str(), u.port), (ItemType::Text, "/docs/read me.txt", 7070));
        assert_eq!(u.to_string(), "gopher://example.org:7070/0/docs/read%20me.txt");
        let u = GopherUrl::parse("gopher://gopher.floodgap.com/7/v2/vs%09minuet").unwrap();
        assert_eq!(u.search.as_deref(), Some("minuet"));
        assert_eq!(u.request(), "/v2/vs\tminuet\r\n");
        let u = GopherUrl::parse("gophers://[2001:db8::1]/1/").unwrap();
        assert!(u.tls);
        assert_eq!(u.host, "2001:db8::1");
        assert_eq!(u.to_string(), "gophers://[2001:db8::1]/1/");
        let u = GopherUrl::parse("gopher://h/1/cgi?x=1").unwrap();
        assert_eq!(u.selector, "/cgi?x=1");
        assert!(GopherUrl::parse("http://x").is_err());
    }

    #[test]
    fn parses_menus() {
        let menu = "iWelcome to Gopherspace\tfake\t(NULL)\t0\r\n\
                    1Floodgap\t/\tgopher.floodgap.com\t70\t+\r\n\
                    0About\t/about.txt\tgopher.floodgap.com\t70\r\n\
                    7Veronica-2\t/v2/vs\tgopher.floodgap.com\t70\r\n\
                    hWeb\tURL:https://example.org/\tgopher.floodgap.com\t70\r\n\
                    8Library\tguest\tlibrary.example.edu\t23\r\n\
                    a line with no tabs\r\n\
                    3Error\t\terror.host\t1\r\n\
                    .\r\n\
                    1after the end\t/\th\t70\r\n";
        let items = parse_menu(menu);
        assert_eq!(items.len(), 8);
        assert_eq!(items[0].kind, ItemType::Info);
        assert!(items[1].plus);
        assert_eq!(items[1].url(false).unwrap(), "gopher://gopher.floodgap.com/1/");
        assert_eq!(items[3].kind, ItemType::Search);
        assert_eq!(items[4].url(false).unwrap(), "https://example.org/");
        assert_eq!(items[5].url(false).unwrap(), "telnet://library.example.edu/?user=guest");
        assert_eq!(items[6].kind, ItemType::Info);
        assert_eq!(items[7].kind, ItemType::Error);
        assert_eq!(items[0].url(false), None);
    }

    #[test]
    fn unstuffs_text() {
        assert_eq!(text_body(b"hello\r\n..dots\r\n.\r\nignored\r\n"), "hello\n.dots\n");
        assert_eq!(text_body(b"no terminator"), "no terminator\n");
    }

    #[tokio::test]
    async fn fetches_and_limits() {
        let (mut c, mut s) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let mut buf = [0u8; 64];
            let n = s.read(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], b"/x\r\n");
            s.write_all(b"0hi\t/hi\th\t70\r\n.\r\n").await.unwrap();
        });
        let u = GopherUrl::parse("gopher://h/1/x").unwrap();
        let body = fetch(&mut c, &u, 1024).await.unwrap();
        assert_eq!(parse_menu(&String::from_utf8_lossy(&body)).len(), 1);

        let (mut c, mut s) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let mut buf = [0u8; 64];
            let _ = s.read(&mut buf).await;
            let _ = s.write_all(&[b'x'; 2000]).await;
        });
        assert!(matches!(fetch(&mut c, &u, 100).await, Err(Error::TooLarge(100))));
    }
}
