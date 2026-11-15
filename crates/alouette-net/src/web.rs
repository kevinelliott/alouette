//! The web fetcher behind the Web module: HTTP/2 when the server offers it (HTTP/1.1
//! otherwise), gzip, deflate and Brotli, a cookie jar, redirects and character sets.

use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use url::Url;

use crate::http::{Body, Sink, USER_AGENT};
use crate::{ConnInfo, Net, NetError, TlsOptions};

const MAX_REDIRECTS: usize = 10;

/// A request from the browser.
#[derive(Debug, Clone, Default)]
pub struct WebRequest {
    pub url: String,
    /// GET when empty.
    pub method: String,
    pub body: Option<Vec<u8>>,
    pub content_type: Option<String>,
    pub referer: Option<String>,
    /// Extra headers, such as Accept.
    pub headers: Vec<(String, String)>,
}

impl WebRequest {
    pub fn get(url: impl Into<String>) -> Self {
        Self { url: url.into(), ..Default::default() }
    }
}

#[derive(Debug, Clone)]
pub struct WebResponse {
    /// The final URL, after redirects.
    pub url: String,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    /// The body, decompressed.
    pub body: Vec<u8>,
    /// "h3", "h2" or "http/1.1".
    pub protocol: &'static str,
    pub info: ConnInfo,
    /// Every URL that redirected here, in order.
    pub redirects: Vec<String>,
}

impl WebResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
    /// The media type without parameters, lowercased.
    pub fn mime(&self) -> String {
        self.header("content-type").unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase()
    }
    pub fn is_html(&self) -> bool {
        let m = self.mime();
        m == "text/html" || m == "application/xhtml+xml" || (m.is_empty() && sniff_html(&self.body))
    }
    /// The body as text, decoded from its character set.
    pub fn text(&self) -> String {
        decode(&self.body, self.header("content-type"))
    }
}

fn sniff_html(b: &[u8]) -> bool {
    let head = String::from_utf8_lossy(&b[..b.len().min(512)]).to_ascii_lowercase();
    head.contains("<html") || head.contains("<!doctype html")
}

/// Decode text by the charset in Content-Type, a BOM, or `<meta charset>`, else UTF-8.
pub fn decode(body: &[u8], content_type: Option<&str>) -> String {
    let from_header = content_type.and_then(|ct| {
        ct.split(';').find_map(|p| p.trim().strip_prefix("charset=").map(|c| c.trim_matches('"').to_string()))
    });
    let label = from_header.or_else(|| {
        let head = String::from_utf8_lossy(&body[..body.len().min(2048)]).to_ascii_lowercase();
        let i = head.find("charset=")?;
        let rest = &head[i + 8..];
        let rest = rest.trim_start_matches(['"', '\'']);
        let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_')).unwrap_or(rest.len());
        Some(rest[..end].to_string())
    });
    let enc = label.and_then(|l| encoding_rs::Encoding::for_label(l.as_bytes())).unwrap_or(encoding_rs::UTF_8);
    let (text, _, _) = enc.decode(body);
    text.into_owned()
}

/// Cookies kept for the session.
#[derive(Debug, Default, Clone)]
pub struct Jar {
    cookies: Vec<Cookie>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Cookie {
    name: String,
    value: String,
    domain: String,
    /// Domain cookies match subdomains; host-only cookies do not.
    host_only: bool,
    path: String,
    secure: bool,
}

impl Jar {
    pub fn store(&mut self, url: &Url, header: &str) {
        let host = url.host_str().unwrap_or("").to_ascii_lowercase();
        let mut parts = header.split(';');
        let Some((name, value)) = parts.next().and_then(|nv| nv.split_once('=')) else { return };
        let mut c = Cookie {
            name: name.trim().to_string(),
            value: value.trim().to_string(),
            domain: host.clone(),
            host_only: true,
            path: default_path(url),
            secure: false,
        };
        let mut delete = false;
        for p in parts {
            let (k, v) = p.split_once('=').map(|(k, v)| (k.trim(), v.trim())).unwrap_or((p.trim(), ""));
            match k.to_ascii_lowercase().as_str() {
                "domain" => {
                    let d = v.trim_start_matches('.').to_ascii_lowercase();
                    // Only for this host or a parent of it, and never a bare suffix.
                    if (host == d || host.ends_with(&format!(".{d}"))) && d.contains('.') {
                        c.domain = d;
                        c.host_only = false;
                    } else {
                        return;
                    }
                }
                "path" if v.starts_with('/') => c.path = v.to_string(),
                "secure" => c.secure = true,
                "max-age" => delete = v.parse::<i64>().is_ok_and(|n| n <= 0),
                _ => {}
            }
        }
        if c.name.is_empty() {
            return;
        }
        self.cookies.retain(|o| !(o.name == c.name && o.domain == c.domain && o.path == c.path));
        if !delete {
            self.cookies.push(c);
        }
    }

    /// The Cookie header for a request, if any cookie applies.
    pub fn header(&self, url: &Url) -> Option<String> {
        let host = url.host_str().unwrap_or("").to_ascii_lowercase();
        let path = url.path();
        let https = url.scheme() == "https";
        let mut v: Vec<&Cookie> = self
            .cookies
            .iter()
            .filter(|c| {
                let dom = if c.host_only {
                    host == c.domain
                } else {
                    host == c.domain || host.ends_with(&format!(".{}", c.domain))
                };
                let p = path == c.path
                    || path.starts_with(&c.path) && (c.path.ends_with('/') || path[c.path.len()..].starts_with('/'));
                dom && p && (https || !c.secure)
            })
            .collect();
        v.sort_by_key(|c| std::cmp::Reverse(c.path.len()));
        if v.is_empty() {
            return None;
        }
        Some(v.iter().map(|c| format!("{}={}", c.name, c.value)).collect::<Vec<_>>().join("; "))
    }

    pub fn clear(&mut self) {
        self.cookies.clear();
    }

    pub fn len(&self) -> usize {
        self.cookies.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cookies.is_empty()
    }
}

fn default_path(url: &Url) -> String {
    let p = url.path();
    match p.rfind('/') {
        Some(0) | None => "/".into(),
        Some(i) => p[..i].to_string(),
    }
}

/// Open HTTP/2 connections by origin.
type H2Pool = HashMap<String, (h2::client::SendRequest<Bytes>, ConnInfo)>;

/// A browser session: shared cookies and open HTTP/2 connections.
#[derive(Clone)]
pub struct Browser {
    net: Net,
    pub jar: Arc<Mutex<Jar>>,
    h2: Arc<tokio::sync::Mutex<H2Pool>>,
    h3: Arc<tokio::sync::Mutex<HashMap<String, crate::quic::H3Conn>>>,
    /// HTTP/3 ports origins offered by Alt-Svc; None once HTTP/3 failed there.
    alt: Arc<Mutex<HashMap<String, Option<u16>>>>,
    /// HTTPS records by host.
    svcb: Arc<tokio::sync::Mutex<HashMap<String, Option<crate::dns::HttpsInfo>>>>,
    /// The largest body accepted, after decompression.
    pub max_body: usize,
}

impl Browser {
    pub fn new(net: Net) -> Self {
        Self {
            net,
            jar: Arc::default(),
            h2: Arc::default(),
            h3: Arc::default(),
            alt: Arc::default(),
            svcb: Arc::default(),
            max_body: 16 << 20,
        }
    }

    pub fn net(&self) -> &Net {
        &self.net
    }

    /// Fetch a page, following redirects and keeping cookies.
    pub async fn fetch(&self, req: WebRequest) -> Result<WebResponse, NetError> {
        let mut url = Url::parse(&req.url).map_err(|e| NetError::Config(format!("{}: {e}", req.url)))?;
        let mut method = if req.method.is_empty() { "GET".to_string() } else { req.method.to_ascii_uppercase() };
        let mut body = req.body.clone();
        let mut redirects = vec![];
        let mut referer = req.referer.clone();
        for _ in 0..=MAX_REDIRECTS {
            let mut headers: Vec<(String, String)> = vec![
                ("User-Agent".into(), USER_AGENT.into()),
                ("Accept".into(), "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8".into()),
                ("Accept-Encoding".into(), "gzip, deflate, br".into()),
                ("Accept-Language".into(), accept_language()),
            ];
            for (k, v) in &req.headers {
                headers.retain(|(h, _)| !h.eq_ignore_ascii_case(k));
                headers.push((k.clone(), v.clone()));
            }
            if let Some(r) = &referer {
                // Never leak an https referer to plain http.
                if !(r.starts_with("https:") && url.scheme() == "http") {
                    headers.push(("Referer".into(), r.clone()));
                }
            }
            if let Some(c) = self.jar.lock().unwrap().header(&url) {
                headers.push(("Cookie".into(), c));
            }
            if body.is_some() {
                headers.push((
                    "Content-Type".into(),
                    req.content_type.clone().unwrap_or_else(|| "application/x-www-form-urlencoded".into()),
                ));
            }
            let mut resp = self.once(&url, &method, &headers, body.as_deref()).await?;
            if resp.protocol != "h3"
                && url.scheme() == "https"
                && let Some(port) = resp.header("alt-svc").and_then(crate::quic::alt_svc_h3)
            {
                let origin = url.origin().ascii_serialization();
                self.alt.lock().unwrap().entry(origin).or_insert(Some(port));
            }
            for (k, v) in &resp.headers {
                if k.eq_ignore_ascii_case("set-cookie") {
                    self.jar.lock().unwrap().store(&url, v);
                }
            }
            if matches!(resp.status, 301 | 302 | 303 | 307 | 308)
                && let Some(loc) = resp.header("location")
            {
                let next = url.join(loc).map_err(|e| NetError::Proxy(format!("bad redirect {loc}: {e}")))?;
                if !matches!(next.scheme(), "http" | "https") {
                    // Let the caller decide about other schemes.
                    resp.redirects = redirects;
                    resp.headers.push(("x-alouette-redirect".into(), next.to_string()));
                    return Ok(resp);
                }
                redirects.push(url.to_string());
                if resp.status == 303 || (matches!(resp.status, 301 | 302) && method == "POST") {
                    method = "GET".into();
                    body = None;
                }
                referer = Some(url.to_string());
                url = next;
                continue;
            }
            let enc = resp.header("content-encoding").map(str::to_string);
            resp.body = decompress(enc.as_deref(), std::mem::take(&mut resp.body), self.max_body)?;
            resp.redirects = redirects;
            return Ok(resp);
        }
        Err(NetError::Proxy(format!("too many redirects from {}", req.url)))
    }

    async fn once(
        &self,
        url: &Url,
        method: &str,
        headers: &[(String, String)],
        body: Option<&[u8]>,
    ) -> Result<WebResponse, NetError> {
        if url.scheme() == "https" {
            let origin = url.origin().ascii_serialization();
            let host = url.host_str().unwrap_or("").to_string();
            let direct = self.net.is_direct(&host);
            let svcb = if direct && (self.net.ech_enabled() || self.net.http3_enabled()) {
                self.https_record(&host).await
            } else {
                None
            };
            let ech = svcb.as_ref().and_then(|s| s.ech.clone()).filter(|_| self.net.ech_enabled());
            // HTTP/3 when the site offered it, by Alt-Svc or its HTTPS record.
            let alt = self.alt.lock().unwrap().get(&origin).cloned();
            let h3_port = match alt {
                Some(p) => p,
                None => svcb
                    .as_ref()
                    .filter(|s| s.alpn.iter().any(|a| a == "h3"))
                    .map(|s| s.port.unwrap_or(url.port_or_known_default().unwrap_or(443))),
            };
            if direct
                && self.net.http3_enabled()
                && let Some(port) = h3_port
            {
                match self.h3_once(&origin, &host, port, ech.as_deref(), url, method, headers, body).await {
                    Ok(r) => return Ok(r),
                    Err(e) => {
                        tracing::debug!("HTTP/3 to {origin} failed, using TCP: {e}");
                        self.alt.lock().unwrap().insert(origin.clone(), None);
                    }
                }
            }
            // Reuse an open HTTP/2 connection.
            let cached = self.h2.lock().await.get(&origin).cloned();
            if let Some((sender, info)) = cached {
                match self.h2_request(sender, info, url, method, headers, body).await {
                    Ok(r) => return Ok(r),
                    Err(_) => {
                        self.h2.lock().await.remove(&origin);
                    }
                }
            }
            let port = url.port_or_known_default().unwrap_or(443);
            let opts = TlsOptions { alpn: vec![b"h2", b"http/1.1"], ech, ..TlsOptions::default() };
            let conn = self.net.tls(&host, port, &opts).await?;
            if conn.stream.get_ref().1.alpn_protocol() == Some(b"h2") {
                let info = conn.info.clone();
                let (sender, connection) =
                    h2::client::handshake(conn.stream).await.map_err(|e| NetError::Proxy(format!("HTTP/2: {e}")))?;
                tokio::spawn(async move {
                    let _ = connection.await;
                });
                self.h2.lock().await.insert(origin, (sender.clone(), info.clone()));
                return self.h2_request(sender, info, url, method, headers, body).await;
            }
            // HTTP/1.1 on this connection.
            drop(conn);
        }
        let hs: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let b = body.map(Body::Bytes).unwrap_or(Body::Empty);
        let r = self.net.request(url, method, &hs, b, Sink::Memory(self.max_body)).await?;
        Ok(WebResponse {
            url: r.url,
            status: r.status,
            headers: r.headers,
            body: r.body,
            protocol: "http/1.1",
            info: r.info,
            redirects: vec![],
        })
    }

    async fn https_record(&self, host: &str) -> Option<crate::dns::HttpsInfo> {
        if let Some(c) = self.svcb.lock().await.get(host) {
            return c.clone();
        }
        let r = tokio::time::timeout(std::time::Duration::from_millis(1500), self.net.dns().https_record(host))
            .await
            .ok()
            .flatten();
        self.svcb.lock().await.insert(host.to_string(), r.clone());
        r
    }

    #[allow(clippy::too_many_arguments)]
    async fn h3_once(
        &self,
        origin: &str,
        host: &str,
        port: u16,
        ech: Option<&[u8]>,
        url: &Url,
        method: &str,
        headers: &[(String, String)],
        body: Option<&[u8]>,
    ) -> Result<WebResponse, NetError> {
        let cached = self.h3.lock().await.get(origin).cloned();
        let mut conn = match cached {
            Some(c) => c,
            None => {
                let c = self.net.h3_connect(host, port, ech, crate::Verify::System).await?;
                self.h3.lock().await.insert(origin.to_string(), c.clone());
                c
            }
        };
        let r = match crate::quic::h3_request(&mut conn.sender, url, method, headers, body, self.max_body).await {
            Ok(r) => r,
            Err(e) => {
                self.h3.lock().await.remove(origin);
                return Err(e);
            }
        };
        Ok(WebResponse {
            url: url.to_string(),
            status: r.status,
            headers: r.headers,
            body: r.body,
            protocol: "h3",
            info: conn.info.clone(),
            redirects: vec![],
        })
    }

    async fn h2_request(
        &self,
        sender: h2::client::SendRequest<Bytes>,
        info: ConnInfo,
        url: &Url,
        method: &str,
        headers: &[(String, String)],
        body: Option<&[u8]>,
    ) -> Result<WebResponse, NetError> {
        let h2err = |e: h2::Error| NetError::Proxy(format!("HTTP/2: {e}"));
        let mut sender = sender.ready().await.map_err(h2err)?;
        let mut b = http::Request::builder().method(method).uri(url.as_str());
        for (k, v) in headers {
            b = b.header(k.to_ascii_lowercase(), v);
        }
        if let Some(body) = body {
            b = b.header("content-length", body.len());
        }
        let req = b.body(()).map_err(|e| NetError::Proxy(format!("HTTP/2 request: {e}")))?;
        let (resp, mut send) = sender.send_request(req, body.is_none()).map_err(h2err)?;
        if let Some(body) = body {
            send.send_data(Bytes::copy_from_slice(body), true).map_err(h2err)?;
        }
        let resp = resp.await.map_err(h2err)?;
        let (parts, mut stream) = resp.into_parts();
        let mut data = Vec::new();
        if method != "HEAD" {
            while let Some(chunk) = stream.data().await {
                let chunk = chunk.map_err(h2err)?;
                let _ = stream.flow_control().release_capacity(chunk.len());
                data.extend_from_slice(&chunk);
                if data.len() > self.max_body {
                    return Err(NetError::Proxy(format!("the page is larger than {} MB", self.max_body >> 20)));
                }
            }
        }
        let headers = parts
            .headers
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
            .collect();
        Ok(WebResponse {
            url: url.to_string(),
            status: parts.status.as_u16(),
            headers,
            body: data,
            protocol: "h2",
            info,
            redirects: vec![],
        })
    }
}

fn accept_language() -> String {
    let lang = std::env::var("LC_ALL")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var("LANG").ok())
        .unwrap_or_default();
    let tag = lang.split(['.', '@']).next().unwrap_or("").replace('_', "-");
    if tag.is_empty() || tag == "C" || tag == "POSIX" {
        "en;q=0.9, *;q=0.5".into()
    } else {
        let primary = tag.split('-').next().unwrap_or("en");
        if primary == tag { format!("{tag}, *;q=0.5") } else { format!("{tag}, {primary};q=0.9, *;q=0.5") }
    }
}

/// Undo Content-Encoding.
pub fn decompress(encoding: Option<&str>, body: Vec<u8>, max: usize) -> Result<Vec<u8>, NetError> {
    let enc = encoding.unwrap_or("").trim().to_ascii_lowercase();
    let mut out = Vec::new();
    let limit = max as u64 + 1;
    let res = match enc.as_str() {
        "" | "identity" => return Ok(body),
        "gzip" | "x-gzip" => flate2::read::MultiGzDecoder::new(&body[..]).take(limit).read_to_end(&mut out),
        "deflate" => {
            // Servers send either zlib-wrapped or raw deflate.
            match flate2::read::ZlibDecoder::new(&body[..]).take(limit).read_to_end(&mut out) {
                Ok(n) => Ok(n),
                Err(_) => {
                    out.clear();
                    flate2::read::DeflateDecoder::new(&body[..]).take(limit).read_to_end(&mut out)
                }
            }
        }
        "br" => brotli::Decompressor::new(&body[..], 4096).take(limit).read_to_end(&mut out),
        other => return Err(NetError::Proxy(format!("the server used an encoding Alouette does not know: {other}"))),
    };
    res.map_err(|e| NetError::Proxy(format!("could not decompress the page: {e}")))?;
    if out.len() > max {
        return Err(NetError::Proxy(format!("the page is larger than {} MB", max >> 20)));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookies_match_hosts_and_paths() {
        let mut j = Jar::default();
        let u = Url::parse("https://www.example.org/app/login").unwrap();
        j.store(&u, "sid=1; Path=/; Secure; HttpOnly");
        j.store(&u, "pref=dark; Domain=example.org; Path=/");
        j.store(&u, "local=x");
        j.store(&u, "evil=1; Domain=org");
        j.store(&u, "other=1; Domain=example.com");
        assert_eq!(j.len(), 3);
        assert_eq!(j.header(&u).unwrap(), "local=x; sid=1; pref=dark");
        let sub = Url::parse("http://cdn.example.org/").unwrap();
        assert_eq!(j.header(&sub).unwrap(), "pref=dark");
        assert_eq!(j.header(&Url::parse("https://www.example.org/").unwrap()).unwrap(), "sid=1; pref=dark");
        j.store(&u, "sid=; Path=/; Max-Age=0");
        assert_eq!(j.header(&Url::parse("https://www.example.org/").unwrap()).unwrap(), "pref=dark");
    }

    #[test]
    fn decodes_charsets_and_compression() {
        assert_eq!(decode(b"caf\xe9", Some("text/html; charset=ISO-8859-1")), "café");
        assert_eq!(decode(b"<meta charset=\"windows-1252\">\x93hi\x94", None), "<meta charset=\"windows-1252\">“hi”");
        assert_eq!(decode("café".as_bytes(), None), "café");
        use std::io::Write;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(b"hello gzip").unwrap();
        assert_eq!(decompress(Some("gzip"), gz.finish().unwrap(), 100).unwrap(), b"hello gzip");
        let mut br = Vec::new();
        brotli::CompressorWriter::new(&mut br, 4096, 5, 22).write_all(b"hello brotli").unwrap();
        assert_eq!(decompress(Some("br"), br, 100).unwrap(), b"hello brotli");
        assert!(decompress(Some("zz"), vec![], 10).is_err());
        let mut big = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        big.write_all(&[0u8; 5000]).unwrap();
        assert!(decompress(Some("gzip"), big.finish().unwrap(), 1000).is_err());
    }
}
