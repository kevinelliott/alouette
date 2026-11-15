//! Loading a URL in the background: connect through the shared network layer, speak the
//! protocol, follow Gemini redirects, check pinned certificates and save downloads.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use alouette_core::TaskCtx;
use alouette_net::{ConnInfo, Net, TlsOptions, Verify};
use proto_gemini::Kind as GKind;
use proto_gopher::{GopherUrl, ItemType};
use url::Url;

use crate::doc::{Doc, Kind, host_title};

/// The largest page shown in the browser.
pub const MAX_PAGE: usize = 16 * 1024 * 1024;
/// The largest download.
pub const MAX_DOWNLOAD: usize = 512 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;

/// Certificate pins for trust on first use, keyed by `host:port`.
#[derive(Clone, Default)]
pub struct Pins(pub Arc<Mutex<HashMap<String, String>>>);

impl Pins {
    pub fn load(text: &str) -> Self {
        let map = text
            .lines()
            .filter_map(|l| l.split_once(' ').map(|(k, v)| (k.to_string(), v.trim().to_string())))
            .collect();
        Self(Arc::new(Mutex::new(map)))
    }

    pub fn dump(&self) -> String {
        let map = self.0.lock().expect("pins");
        let mut lines: Vec<_> = map.iter().map(|(k, v)| format!("{k} {v}")).collect();
        lines.sort();
        lines.join("\n")
    }

    pub fn get(&self, key: &str) -> Option<String> {
        self.0.lock().expect("pins").get(key).cloned()
    }

    pub fn set(&self, key: &str, fp: &str) {
        self.0.lock().expect("pins").insert(key.to_string(), fp.to_string());
    }
}

/// What came back.
#[derive(Debug, Clone)]
pub enum Outcome {
    Page {
        url: String,
        doc: Doc,
    },
    /// The server wants input (Gemini 1x, or a Gopher search without a query).
    Input {
        url: String,
        prompt: String,
        sensitive: bool,
    },
    Saved {
        url: String,
        path: PathBuf,
        bytes: usize,
    },
    /// A pinned certificate changed. Loading again with `trust_new` accepts the new one.
    CertChanged {
        url: String,
        host: String,
        old: String,
        new: String,
    },
    /// Hand this URL to another module (a redirect or link to another scheme).
    Elsewhere(String),
    Failed {
        url: String,
        error: String,
    },
}

/// One finished request, sent back to the window as a module event.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub req: u64,
    pub outcome: Outcome,
    pub info: Option<ConnInfo>,
    /// A pin was added or replaced and should be saved.
    pub pins_changed: bool,
}

pub struct Request {
    pub url: String,
    pub trust_new: bool,
    pub download: bool,
}

pub struct Loader {
    pub net: Net,
    pub pins: Pins,
    pub downloads: PathBuf,
}

impl Loader {
    pub async fn load(&self, req: &Request, ctx: &TaskCtx) -> Loaded {
        let mut url = req.url.clone();
        let mut pins_changed = false;
        for _ in 0..=MAX_REDIRECTS {
            let scheme = url.split(':').next().unwrap_or("").to_ascii_lowercase();
            let r = match scheme.as_str() {
                "gemini" => self.gemini(&url, req, &mut pins_changed, ctx).await,
                "gopher" | "gophers" => self.gopher(&url, req, &mut pins_changed, ctx).await,
                "finger" => self.finger(&url).await,
                _ => Ok((Outcome::Elsewhere(url.clone()), None)),
            };
            match r {
                Ok((Outcome::Elsewhere(next), _)) if next != url && is_ours(&next) => {
                    ctx.detail(format!("redirected to {next}"));
                    url = next;
                }
                Ok((outcome, info)) => return Loaded { req: 0, outcome, info, pins_changed },
                Err(error) => {
                    return Loaded { req: 0, outcome: Outcome::Failed { url, error }, info: None, pins_changed };
                }
            }
        }
        Loaded {
            req: 0,
            outcome: Outcome::Failed { url, error: "too many redirects".into() },
            info: None,
            pins_changed,
        }
    }

    /// TLS with trust on first use against the pin store.
    async fn tofu(
        &self,
        host: &str,
        port: u16,
        url: &str,
        req: &Request,
        pins_changed: &mut bool,
    ) -> Result<Result<alouette_net::TlsConn, Outcome>, String> {
        let opts = TlsOptions { verify: Verify::Tofu, alpn: vec![], server_name: None };
        let conn = self.net.tls(host, port, &opts).await.map_err(|e| e.to_string())?;
        let fp = conn.info.tls.as_ref().and_then(|t| t.cert_sha256.clone()).unwrap_or_default();
        let key = format!("{host}:{port}");
        match self.pins.get(&key) {
            Some(old) if old != fp && !req.trust_new => {
                return Ok(Err(Outcome::CertChanged { url: url.to_string(), host: key, old, new: fp }));
            }
            Some(old) if old == fp => {}
            _ => {
                self.pins.set(&key, &fp);
                *pins_changed = true;
            }
        }
        Ok(Ok(conn))
    }

    async fn gemini(
        &self,
        url: &str,
        req: &Request,
        pins_changed: &mut bool,
        ctx: &TaskCtx,
    ) -> Result<(Outcome, Option<ConnInfo>), String> {
        let parsed = proto_gemini::parse_url(url).map_err(|e| e.to_string())?;
        let host = parsed.host_str().unwrap_or("").trim_start_matches('[').trim_end_matches(']').to_string();
        let port = parsed.port().unwrap_or(proto_gemini::DEFAULT_PORT);
        ctx.detail(format!("connecting to {host}"));
        let mut conn = match self.tofu(&host, port, url, req, pins_changed).await? {
            Ok(c) => c,
            Err(outcome) => return Ok((outcome, None)),
        };
        ctx.detail("waiting for the server");
        let max = if req.download { MAX_DOWNLOAD } else { MAX_PAGE };
        let resp = proto_gemini::fetch(&mut conn.stream, &parsed, max).await.map_err(|e| e.to_string())?;
        let info = Some(conn.info.clone());
        let h = &resp.header;
        let outcome = match h.kind() {
            GKind::Input { sensitive } => Outcome::Input { url: parsed.to_string(), prompt: h.meta.clone(), sensitive },
            GKind::Redirect { .. } => {
                let next = parsed.join(h.meta.trim()).map_err(|e| format!("bad redirect {}: {e}", h.meta))?;
                Outcome::Elsewhere(next.to_string())
            }
            GKind::Success => {
                let mime = h.mime().to_ascii_lowercase();
                if req.download || !mime.starts_with("text/") {
                    let (path, bytes) = save(&self.downloads, &parsed, &mime, &resp.body).await?;
                    Outcome::Saved { url: parsed.to_string(), path, bytes }
                } else if mime == "text/gemini" {
                    Outcome::Page { url: parsed.to_string(), doc: Doc::gemtext(&parsed, &resp.text()) }
                } else {
                    Outcome::Page { url: parsed.to_string(), doc: Doc::plain(&host_title(&parsed), &resp.text()) }
                }
            }
            GKind::ClientCertificate => Outcome::Page {
                url: parsed.to_string(),
                doc: Doc::message(
                    "Client certificate needed",
                    Kind::Text,
                    &[&h.describe(), "", "Client certificates arrive in a later Burrow update."],
                ),
            },
            GKind::TemporaryFailure | GKind::PermanentFailure => {
                Outcome::Failed { url: parsed.to_string(), error: h.describe() }
            }
        };
        Ok((outcome, info))
    }

    async fn gopher(
        &self,
        url: &str,
        req: &Request,
        pins_changed: &mut bool,
        ctx: &TaskCtx,
    ) -> Result<(Outcome, Option<ConnInfo>), String> {
        let g = GopherUrl::parse(url).map_err(|e| e.to_string())?;
        if g.kind == ItemType::Search && g.search.is_none() {
            return Ok((Outcome::Input { url: g.to_string(), prompt: "Search for".into(), sensitive: false }, None));
        }
        ctx.detail(format!("connecting to {}", g.host));
        let max = if req.download || !g.kind.is_textual() { MAX_DOWNLOAD } else { MAX_PAGE };
        let (body, info) = if g.tls {
            let mut conn = match self.tofu(&g.host, g.port, url, req, pins_changed).await? {
                Ok(c) => c,
                Err(outcome) => return Ok((outcome, None)),
            };
            let b = proto_gopher::fetch(&mut conn.stream, &g, max).await.map_err(|e| e.to_string())?;
            (b, conn.info)
        } else {
            let mut conn = self.net.tcp(&g.host, g.port).await.map_err(|e| e.to_string())?;
            let b = proto_gopher::fetch(&mut conn.stream, &g, max).await.map_err(|e| e.to_string())?;
            (b, conn.info)
        };
        let title = if g.selector.is_empty() || g.selector == "/" {
            g.host.clone()
        } else {
            format!("{}{}", g.host, g.selector)
        };
        let outcome = match g.kind {
            _ if req.download || !g.kind.is_textual() => {
                let u = Url::parse(&g.to_string()).map_err(|e| e.to_string())?;
                let (path, bytes) = save(&self.downloads, &u, "", &body).await?;
                Outcome::Saved { url: g.to_string(), path, bytes }
            }
            ItemType::Menu | ItemType::Search => Outcome::Page {
                url: g.to_string(),
                doc: Doc::gopher_menu(&title, &String::from_utf8_lossy(&body), g.tls),
            },
            _ => Outcome::Page { url: g.to_string(), doc: Doc::plain(&title, &proto_gopher::text_body(&body)) },
        };
        Ok((outcome, Some(info)))
    }

    async fn finger(&self, url: &str) -> Result<(Outcome, Option<ConnInfo>), String> {
        let q = proto_finger::Query::parse(url).map_err(|e| e.to_string())?;
        let mut conn = self.net.tcp(&q.host, q.port).await.map_err(|e| e.to_string())?;
        let text = proto_finger::fetch(&mut conn.stream, &q, MAX_PAGE).await.map_err(|e| e.to_string())?;
        let title =
            if q.user.is_empty() { format!("finger @{}", q.host) } else { format!("finger {}@{}", q.user, q.host) };
        Ok((Outcome::Page { url: q.url(), doc: Doc::plain(&title, &text) }, Some(conn.info)))
    }
}

fn is_ours(url: &str) -> bool {
    matches!(url.split(':').next(), Some("gemini" | "gopher" | "gophers" | "finger"))
}

/// Save a body into the downloads folder under a name taken from the URL, never
/// overwriting an existing file.
async fn save(dir: &Path, url: &Url, mime: &str, body: &[u8]) -> Result<(PathBuf, usize), String> {
    tokio::fs::create_dir_all(dir).await.map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let path = unique_path(dir, &file_name(url, mime));
    tokio::fs::write(&path, body).await.map_err(|e| format!("saving {}: {e}", path.display()))?;
    Ok((path, body.len()))
}

pub fn file_name(url: &Url, mime: &str) -> String {
    let last = url.path_segments().and_then(|mut s| s.next_back().map(str::to_string)).unwrap_or_default();
    let last = percent_decode(&last);
    let clean: String = last
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .collect();
    let clean = clean.trim_matches('.').to_string();
    let mut name = if clean.is_empty() { url.host_str().unwrap_or("download").to_string() } else { clean };
    if !name.contains('.') {
        let ext = match mime {
            "text/gemini" => Some("gmi"),
            "text/plain" => Some("txt"),
            "image/png" => Some("png"),
            "image/jpeg" => Some("jpg"),
            "image/gif" => Some("gif"),
            "application/pdf" => Some("pdf"),
            _ => None,
        };
        if let Some(e) = ext {
            name = format!("{name}.{e}");
        }
    }
    name
}

fn percent_decode(s: &str) -> String {
    url::form_urlencoded::parse(s.as_bytes())
        .map(|(k, v)| if v.is_empty() { k.into_owned() } else { format!("{k}={v}") })
        .collect()
}

fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        return p;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    (1..).map(|n| dir.join(format!("{stem} ({n}){ext}"))).find(|p| !p.exists()).expect("a free name")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_round_trip() {
        let p = Pins::load("a:1965 aa\nb:1965 bb\n");
        p.set("c:70", "cc");
        assert_eq!(p.dump(), "a:1965 aa\nb:1965 bb\nc:70 cc");
    }

    #[test]
    fn file_names_are_safe() {
        let u = Url::parse("gemini://h/files/My%20Report.pdf").unwrap();
        assert_eq!(file_name(&u, "application/pdf"), "My Report.pdf");
        let u = Url::parse("gemini://h/").unwrap();
        assert_eq!(file_name(&u, "image/png"), "h.png");
        let u = Url::parse("gopher://h/9/../..%2Fetc%2Fpasswd").unwrap();
        assert!(!file_name(&u, "").contains('/'));
    }
}
