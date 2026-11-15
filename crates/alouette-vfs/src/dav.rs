//! WebDAV (RFC 4918): Nextcloud, ownCloud, Apache mod_dav, NAS boxes, and more.

use alouette_net::Net;
use alouette_net::http::{Body, Sink};
use base64::Engine;
use url::Url;

use crate::sigv4::uri_encode;
use crate::xml::{Ev, events};
use crate::{BoxFut, Entry, Fs, Progress, Reader, Writer};

pub struct Dav {
    net: Net,
    /// The server root; paths are joined onto its path.
    base: Url,
    auth: Option<String>,
    user: String,
}

const PROPFIND: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/><d:getcontentlength/><d:getlastmodified/></d:prop></d:propfind>"#;

fn err(p: &str, e: impl std::fmt::Display) -> String {
    format!("{p}: {e}")
}

impl Dav {
    /// `base` is `https://host/remote.php/dav/files/me/` or similar.
    pub fn new(net: Net, base: Url, user: &str, password: &str) -> Self {
        let auth = (!user.is_empty()).then(|| {
            format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}")))
        });
        Self { net, base, auth, user: user.to_string() }
    }

    fn url(&self, path: &str) -> Result<Url, String> {
        // Paths here are decoded and absolute; encode each segment.
        let mut u = self.base.clone();
        u.set_path(&uri_encode(path, true));
        u.set_query(None);
        Ok(u)
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        extra: &[(&str, &str)],
        body: Body<'_>,
        sink: Sink<'_>,
    ) -> Result<alouette_net::http::Response, String> {
        let url = self.url(path)?;
        let mut headers: Vec<(&str, &str)> = extra.to_vec();
        if let Some(a) = &self.auth {
            headers.push(("Authorization", a.as_str()));
        }
        let r = self.net.request(&url, method, &headers, body, sink).await.map_err(|e| e.to_string())?;
        if !r.ok() {
            return Err(match r.status {
                401 => "the server wants a user name and password (HTTP 401)".into(),
                403 => "not allowed (HTTP 403)".into(),
                404 => "not found (HTTP 404)".into(),
                405 => "the server does not allow that here (HTTP 405)".into(),
                s => format!("the server answered HTTP {s}"),
            });
        }
        Ok(r)
    }
}

/// Parse a PROPFIND multistatus. Returns (decoded path, entry) for each response.
pub fn parse_multistatus(xml: &str) -> Result<Vec<(String, Entry)>, String> {
    let mut out = vec![];
    let mut stack: Vec<String> = vec![];
    let mut href = String::new();
    let mut e = Entry::default();
    for ev in events(xml)? {
        match ev {
            Ev::Start(n) => {
                if n == "response" {
                    href.clear();
                    e = Entry::default();
                }
                if n == "collection" && stack.iter().any(|s| s == "resourcetype") {
                    e.is_dir = true;
                }
                stack.push(n);
            }
            Ev::Text(t) => match stack.last().map(String::as_str) {
                Some("href") => href = t,
                Some("getcontentlength") => e.size = t.trim().parse().ok(),
                Some("getlastmodified") => {
                    e.modified = time::OffsetDateTime::parse(t.trim(), &time::format_description::well_known::Rfc2822)
                        .ok()
                        .map(|d| d.unix_timestamp())
                }
                _ => {}
            },
            Ev::End(n) => {
                stack.pop();
                if n == "response" {
                    // href may be a full URL or a path; keep the path, decoded.
                    let path = Url::parse(&href).map(|u| u.path().to_string()).unwrap_or(href.clone());
                    let path = percent_decode(&path);
                    e.name = crate::file_name(&path).to_string();
                    if e.is_dir {
                        e.size = None;
                    }
                    out.push((path, e.clone()));
                }
            }
        }
    }
    Ok(out)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn same_dir(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

impl Fs for Dav {
    fn label(&self) -> String {
        let scheme = if self.base.scheme() == "https" { "davs" } else { "dav" };
        let host = self.base.host_str().unwrap_or_default();
        if self.user.is_empty() { format!("{scheme}://{host}") } else { format!("{scheme}://{}@{host}", self.user) }
    }
    fn badge(&self) -> Option<String> {
        Some(if self.base.scheme() == "https" { "WebDAV · TLS".into() } else { "WebDAV · plain".into() })
    }
    fn home(&self) -> BoxFut<'_, String> {
        Box::pin(async move { Ok(percent_decode(self.base.path())) })
    }
    fn list<'a>(&'a self, path: &'a str) -> BoxFut<'a, Vec<Entry>> {
        Box::pin(async move {
            let dir = if path.ends_with('/') { path.to_string() } else { format!("{path}/") };
            let r = self
                .call(
                    "PROPFIND",
                    &dir,
                    &[("Depth", "1"), ("Content-Type", "application/xml; charset=utf-8")],
                    Body::Bytes(PROPFIND.as_bytes()),
                    Sink::Memory(32 * 1024 * 1024),
                )
                .await
                .map_err(|e| err(path, e))?;
            let all = parse_multistatus(&r.text()).map_err(|e| err(path, e))?;
            Ok(all.into_iter().filter(|(p, _)| !same_dir(p, &dir)).map(|(_, e)| e).collect())
        })
    }
    fn download<'a>(&'a self, path: &'a str, to: Writer<'a>, progress: Progress<'a>) -> BoxFut<'a, u64> {
        Box::pin(async move {
            let n = std::sync::atomic::AtomicU64::new(0);
            let counted = |b: u64| {
                n.store(b, std::sync::atomic::Ordering::Relaxed);
                progress(b);
            };
            self.call("GET", path, &[], Body::Empty, Sink::Writer(to, &counted)).await.map_err(|e| err(path, e))?;
            Ok(n.load(std::sync::atomic::Ordering::Relaxed))
        })
    }
    fn upload<'a>(&'a self, path: &'a str, from: Reader<'a>, size: u64, progress: Progress<'a>) -> BoxFut<'a, u64> {
        Box::pin(async move {
            self.call("PUT", path, &[], Body::Reader(from, size), Sink::Memory(64 * 1024))
                .await
                .map_err(|e| err(path, e))?;
            progress(size);
            Ok(size)
        })
    }
    fn mkdir<'a>(&'a self, path: &'a str) -> BoxFut<'a, ()> {
        Box::pin(async move {
            self.call("MKCOL", path, &[], Body::Empty, Sink::Memory(64 * 1024))
                .await
                .map(|_| ())
                .map_err(|e| err(path, e))
        })
    }
    fn remove<'a>(&'a self, path: &'a str, is_dir: bool) -> BoxFut<'a, ()> {
        Box::pin(async move {
            let p = if is_dir && !path.ends_with('/') { format!("{path}/") } else { path.to_string() };
            self.call("DELETE", &p, &[], Body::Empty, Sink::Memory(64 * 1024))
                .await
                .map(|_| ())
                .map_err(|e| err(path, e))
        })
    }
    fn rename<'a>(&'a self, from: &'a str, to: &'a str) -> BoxFut<'a, ()> {
        Box::pin(async move {
            let dest = self.url(to)?.to_string();
            self.call("MOVE", from, &[("Destination", &dest), ("Overwrite", "F")], Body::Empty, Sink::Memory(64 * 1024))
                .await
                .map(|_| ())
                .map_err(|e| err(from, e))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multistatus() {
        let xml = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:">
 <d:response><d:href>/dav/docs/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response>
 <d:response><d:href>https://h/dav/docs/My%20Notes.txt</d:href><d:propstat><d:prop>
   <d:resourcetype/><d:getcontentlength>42</d:getcontentlength>
   <d:getlastmodified>Mon, 12 Jan 2024 10:00:00 GMT</d:getlastmodified></d:prop></d:propstat></d:response>
 <d:response><d:href>/dav/docs/sub/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype><d:getcontentlength>0</d:getcontentlength></d:prop></d:propstat></d:response>
</d:multistatus>"#;
        let r = parse_multistatus(xml).unwrap();
        assert_eq!(r.len(), 3);
        assert!(r[0].1.is_dir);
        assert_eq!(r[1].0, "/dav/docs/My Notes.txt");
        assert_eq!((r[1].1.name.as_str(), r[1].1.size, r[1].1.is_dir), ("My Notes.txt", Some(42), false));
        assert!(r[1].1.modified.is_some());
        assert!(r[2].1.is_dir && r[2].1.name == "sub" && r[2].1.size.is_none());
    }

    #[test]
    fn decoding() {
        assert_eq!(percent_decode("/a%20b/%C3%A9"), "/a b/é");
        assert_eq!(percent_decode("100%"), "100%");
    }
}
