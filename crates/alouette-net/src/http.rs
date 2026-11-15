//! A small HTTP/1.1 client over the shared connector, for the APIs that lookups use (RDAP,
//! certificate transparency, WebFinger, search APIs). It follows redirects, decodes chunked
//! bodies and always verifies certificates. The full web stack arrives with the Web module.

use url::Url;

use crate::{ConnInfo, Io, Net, NetError, TlsOptions};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const USER_AGENT: &str =
    concat!("Alouette/", env!("CARGO_PKG_VERSION"), " (+https://github.com/kevinelliott/alouette)");
const MAX_REDIRECTS: usize = 5;

#[derive(Debug, Clone)]
pub struct Response {
    pub url: String,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub info: ConnInfo,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// What a request sends.
pub enum Body<'a> {
    Empty,
    Bytes(&'a [u8]),
    /// A stream of known length, such as a file being uploaded.
    Reader(&'a mut (dyn AsyncRead + Unpin + Send), u64),
}

/// Where a response body goes.
pub enum Sink<'a> {
    /// Into `Response::body`, up to this many bytes.
    Memory(usize),
    /// Into a writer, reporting bytes so far. The body is only written for 2xx answers;
    /// other answers land in `Response::body` (up to 64 KiB) so errors can be read.
    Writer(&'a mut (dyn AsyncWrite + Unpin + Send), &'a (dyn Fn(u64) + Sync)),
}

impl Net {
    /// GET a URL, following up to five redirects. `max` caps the body size.
    pub async fn get(&self, url: &str, headers: &[(&str, &str)], max: usize) -> Result<Response, NetError> {
        let mut url = Url::parse(url).map_err(|e| NetError::Config(format!("{url}: {e}")))?;
        for _ in 0..=MAX_REDIRECTS {
            let resp = self.request(&url, "GET", headers, Body::Empty, Sink::Memory(max)).await?;
            if matches!(resp.status, 301 | 302 | 303 | 307 | 308)
                && let Some(loc) = resp.header("location")
            {
                url = url.join(loc).map_err(|e| NetError::Proxy(format!("bad redirect {loc}: {e}")))?;
                continue;
            }
            return Ok(resp);
        }
        Err(NetError::Proxy(format!("too many redirects from {url}")))
    }

    /// One HTTP/1.1 request, without following redirects. For WebDAV, S3 and other APIs.
    pub async fn request(
        &self,
        url: &Url,
        method: &str,
        headers: &[(&str, &str)],
        body: Body<'_>,
        sink: Sink<'_>,
    ) -> Result<Response, NetError> {
        let https = match url.scheme() {
            "https" => true,
            "http" => false,
            s => return Err(NetError::Config(format!("not an http URL: {s}"))),
        };
        let host = url.host_str().ok_or_else(|| NetError::Config(format!("{url}: no host")))?.to_string();
        let port = url.port_or_known_default().unwrap_or(if https { 443 } else { 80 });
        let (mut stream, info): (Box<dyn Io>, ConnInfo) = if https {
            let opts = TlsOptions { alpn: vec![b"http/1.1"], ..TlsOptions::default() };
            let c = self.tls(&host, port, &opts).await?;
            (Box::new(c.stream), c.info)
        } else {
            let c = self.tcp(&host, port).await?;
            (c.stream, c.info)
        };
        let mut target = url.path().to_string();
        if let Some(q) = url.query() {
            target.push('?');
            target.push_str(q);
        }
        let host_header = match url.port() {
            Some(p) => format!("{}:{p}", url.host_str().unwrap_or("")),
            None => url.host_str().unwrap_or("").to_string(),
        };
        let mut req = format!("{method} {target} HTTP/1.1\r\nHost: {host_header}\r\nConnection: close\r\n");
        if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("user-agent")) {
            req.push_str(&format!("User-Agent: {USER_AGENT}\r\n"));
        }
        if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("accept-encoding")) {
            req.push_str("Accept-Encoding: identity\r\n");
        }
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        let len = match &body {
            Body::Empty => None,
            Body::Bytes(b) => Some(b.len() as u64),
            Body::Reader(_, n) => Some(*n),
        };
        if let Some(n) = len.filter(|_| !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-length"))) {
            req.push_str(&format!("Content-Length: {n}\r\n"));
        }
        req.push_str("\r\n");
        stream.write_all(req.as_bytes()).await?;
        match body {
            Body::Empty => {}
            Body::Bytes(b) => stream.write_all(b).await?,
            Body::Reader(r, n) => {
                let copied = tokio::io::copy(&mut r.take(n), &mut stream).await?;
                if copied != n {
                    return Err(NetError::Proxy(format!("upload ended after {copied} of {n} bytes")));
                }
            }
        }
        stream.flush().await?;
        let (status, headers, rest) = read_head(&mut stream).await?;
        let no_body = method.eq_ignore_ascii_case("HEAD") || status == 204 || status == 304 || status < 200;
        let mut body = Vec::new();
        if !no_body {
            match sink {
                Sink::Memory(max) => {
                    read_body(&mut stream, rest, &headers, &mut body, max, &|_| {}).await?;
                }
                Sink::Writer(w, progress) if (200..300).contains(&status) => {
                    read_body(&mut stream, rest, &headers, w, u64::MAX as usize, progress).await?;
                    w.flush().await?;
                }
                Sink::Writer(..) => {
                    read_body(&mut stream, rest, &headers, &mut body, 64 * 1024, &|_| {}).await?;
                }
            }
        }
        Ok(Response { url: url.to_string(), status, headers, body, info })
    }
}

/// Read a status line, headers and body (by length, chunked or until close).
pub async fn read_response<S: AsyncRead + Unpin>(
    s: &mut S,
    max: usize,
) -> Result<(u16, Vec<(String, String)>, Vec<u8>), NetError> {
    let (status, headers, rest) = read_head(s).await?;
    let mut body = Vec::new();
    read_body(s, rest, &headers, &mut body, max, &|_| {}).await?;
    Ok((status, headers, body))
}

/// Read the status line and headers. Returns bytes already read past them.
pub async fn read_head<S: AsyncRead + Unpin>(s: &mut S) -> Result<(u16, Vec<(String, String)>, Vec<u8>), NetError> {
    let mut buf = Vec::with_capacity(8192);
    let mut tmp = [0u8; 8192];
    let head_end = loop {
        if let Some(i) = find(&buf, b"\r\n\r\n") {
            break i;
        }
        if buf.len() > 64 * 1024 {
            return Err(NetError::Proxy("HTTP headers are too long".into()));
        }
        let n = read_some(s, &mut tmp).await?;
        if n == 0 {
            return Err(NetError::Proxy("the server closed the connection before answering".into()));
        }
        buf.extend_from_slice(&tmp[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let rest = buf[head_end + 4..].to_vec();
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| NetError::Proxy(format!("bad HTTP status line: {status_line}")))?;
    let headers: Vec<(String, String)> =
        lines.filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))).collect();
    Ok((status, headers, rest))
}

/// Read a body framed by Content-Length, chunked encoding, or the connection closing, into
/// `out`. Fails past `max` bytes.
pub async fn read_body<S: AsyncRead + Unpin, W: AsyncWrite + Unpin + ?Sized>(
    s: &mut S,
    mut rest: Vec<u8>,
    headers: &[(String, String)],
    out: &mut W,
    max: usize,
    progress: &(dyn Fn(u64) + Sync),
) -> Result<u64, NetError> {
    let mut tmp = vec![0u8; 64 * 1024];
    let get = |name: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone());
    let too_big = || NetError::Proxy(format!("the response is larger than {max} bytes"));
    let mut total: u64 = 0;
    if get("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked")) {
        loop {
            let line_end = loop {
                if let Some(i) = find(&rest, b"\r\n") {
                    break i;
                }
                let n = read_some(s, &mut tmp).await?;
                if n == 0 {
                    return Err(NetError::Proxy("chunked body ended early".into()));
                }
                rest.extend_from_slice(&tmp[..n]);
            };
            let size_str = String::from_utf8_lossy(&rest[..line_end]).into_owned();
            let size = usize::from_str_radix(size_str.split(';').next().unwrap_or("").trim(), 16)
                .map_err(|_| NetError::Proxy(format!("bad chunk size {size_str}")))?;
            rest.drain(..line_end + 2);
            if size == 0 {
                break;
            }
            if total as usize + size > max {
                return Err(too_big());
            }
            let mut left = size;
            while left > 0 {
                if rest.is_empty() {
                    let n = read_some(s, &mut tmp).await?;
                    if n == 0 {
                        return Err(NetError::Proxy("chunked body ended early".into()));
                    }
                    rest.extend_from_slice(&tmp[..n]);
                }
                let take = left.min(rest.len());
                out.write_all(&rest[..take]).await?;
                rest.drain(..take);
                left -= take;
                total += take as u64;
                progress(total);
            }
            // The CRLF after the chunk.
            while rest.len() < 2 {
                let n = read_some(s, &mut tmp).await?;
                if n == 0 {
                    return Err(NetError::Proxy("chunked body ended early".into()));
                }
                rest.extend_from_slice(&tmp[..n]);
            }
            rest.drain(..2);
        }
    } else if let Some(len) = get("content-length").and_then(|v| v.parse::<u64>().ok()) {
        if len as usize > max {
            return Err(too_big());
        }
        let first = rest.len().min(len as usize);
        out.write_all(&rest[..first]).await?;
        total = first as u64;
        progress(total);
        while total < len {
            let n = read_some(s, &mut tmp).await?;
            if n == 0 {
                return Err(NetError::Proxy(format!("the body ended after {total} of {len} bytes")));
            }
            let take = (n as u64).min(len - total) as usize;
            out.write_all(&tmp[..take]).await?;
            total += take as u64;
            progress(total);
        }
    } else {
        if rest.len() > max {
            return Err(too_big());
        }
        out.write_all(&rest).await?;
        total = rest.len() as u64;
        loop {
            let n = read_some(s, &mut tmp).await?;
            if n == 0 {
                break;
            }
            if total as usize + n > max {
                return Err(too_big());
            }
            out.write_all(&tmp[..n]).await?;
            total += n as u64;
            progress(total);
        }
    }
    Ok(total)
}

async fn read_some<S: AsyncRead + Unpin>(s: &mut S, buf: &mut [u8]) -> Result<usize, NetError> {
    match s.read(buf).await {
        Ok(n) => Ok(n),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(0),
        Err(e) => Err(e.into()),
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reads_length_chunked_and_close() {
        let mut r: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhelloEXTRA";
        let (st, _, body) = read_response(&mut r, 100).await.unwrap();
        assert_eq!((st, &body[..]), (200, &b"hello"[..]));

        let mut r: &[u8] =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nWiki\r\n5;x=y\r\npedia\r\n0\r\n\r\n";
        let (_, _, body) = read_response(&mut r, 100).await.unwrap();
        assert_eq!(body, b"Wikipedia");

        let mut r: &[u8] = b"HTTP/1.0 404 Not Found\r\nX: y\r\n\r\nuntil close";
        let (st, h, body) = read_response(&mut r, 100).await.unwrap();
        assert_eq!((st, h[0].0.as_str(), &body[..]), (404, "X", &b"until close"[..]));

        let mut r: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 500\r\n\r\n";
        assert!(read_response(&mut r, 100).await.is_err());
    }

    #[tokio::test]
    async fn gets_and_follows_redirects_over_plain_http() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            for _ in 0..2 {
                let (mut s, _) = l.accept().await.unwrap();
                let mut buf = vec![0u8; 1024];
                let n = s.read(&mut buf).await.unwrap();
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let reply = if req.starts_with("GET /old ") {
                    "HTTP/1.1 301 Moved\r\nLocation: /new?x=1\r\nContent-Length: 0\r\n\r\n".to_string()
                } else {
                    assert!(req.starts_with("GET /new?x=1 HTTP/1.1\r\n"), "{req}");
                    assert!(req.contains("Accept: application/json\r\n"));
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}".to_string()
                };
                s.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        let net = Net::new(&Default::default()).unwrap();
        let r =
            net.get(&format!("http://127.0.0.1:{port}/old"), &[("Accept", "application/json")], 1024).await.unwrap();
        assert!(r.ok());
        assert_eq!(r.text(), "{}");
        assert!(r.url.ends_with("/new?x=1"));
        assert_eq!(r.header("content-type"), Some("application/json"));
    }
}
