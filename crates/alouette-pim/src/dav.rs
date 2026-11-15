//! CardDAV (RFC 6352) and CalDAV (RFC 4791): finding a user's address books and
//! calendars, and keeping a local folder in step with one.

use std::collections::{BTreeMap, HashMap};

use alouette_net::Net;
use alouette_net::http::{Body, Response, Sink};
use alouette_vfs::xml::{Ev, events};
use base64::Engine;
use url::Url;

use crate::store::{Folder, SyncEntry, file_for};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Contacts,
    Calendars,
}

impl Kind {
    fn well_known(self) -> &'static str {
        match self {
            Kind::Contacts => "/.well-known/carddav",
            Kind::Calendars => "/.well-known/caldav",
        }
    }
    fn home_set(self) -> &'static str {
        match self {
            Kind::Contacts => "addressbook-home-set",
            Kind::Calendars => "calendar-home-set",
        }
    }
    fn collection_type(self) -> &'static str {
        match self {
            Kind::Contacts => "addressbook",
            Kind::Calendars => "calendar",
        }
    }
    fn content_type(self) -> &'static str {
        match self {
            Kind::Contacts => "text/vcard; charset=utf-8",
            Kind::Calendars => "text/calendar; charset=utf-8",
        }
    }
    fn ext(self) -> &'static str {
        match self {
            Kind::Contacts => "vcf",
            Kind::Calendars => "ics",
        }
    }
}

/// One `<response>` of a multistatus answer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DavResponse {
    pub href: String,
    /// Properties that came back with a 200 status, by local name. A property holding an
    /// `<href>` gives that href.
    pub props: HashMap<String, String>,
    /// The element names inside `<resourcetype>`.
    pub types: Vec<String>,
}

/// Parse a 207 multistatus.
pub fn parse_multistatus(xml: &str) -> Result<Vec<DavResponse>, String> {
    let mut out = vec![];
    let mut cur: Option<DavResponse> = None;
    let mut stack: Vec<String> = vec![];
    // Within one propstat: the props seen so far, and its status.
    let mut ps_props: Vec<(String, String)> = vec![];
    let mut ps_types: Vec<String> = vec![];
    let mut ps_status = String::new();
    let mut prop_name: Option<String> = None;
    let mut prop_text = String::new();
    for ev in events(xml)? {
        match ev {
            Ev::Start(n) => {
                let parent = stack.last().map(String::as_str);
                match (parent, n.as_str()) {
                    (_, "response") => cur = Some(DavResponse::default()),
                    (_, "propstat") => {
                        ps_props.clear();
                        ps_types.clear();
                        ps_status.clear();
                    }
                    (Some("prop"), _) => {
                        prop_name = Some(n.clone());
                        prop_text.clear();
                    }
                    (Some("resourcetype"), t) => ps_types.push(t.to_string()),
                    _ => {}
                }
                stack.push(n);
            }
            Ev::Text(t) => {
                let here = stack.last().map(String::as_str).unwrap_or("");
                let parent = stack.len().checked_sub(2).map(|i| stack[i].as_str()).unwrap_or("");
                match (parent, here) {
                    ("response", "href") => {
                        if let Some(c) = &mut cur {
                            c.href = t.clone();
                        }
                    }
                    ("propstat", "status") => ps_status = t.clone(),
                    _ if prop_name.is_some() => {
                        // An <href> inside a property wins over other text.
                        if here == "href" || prop_text.is_empty() {
                            prop_text = t.clone();
                        }
                    }
                    _ => {}
                }
            }
            Ev::End(n) => {
                stack.pop();
                let parent = stack.last().map(String::as_str);
                match n.as_str() {
                    "response" => {
                        if let Some(c) = cur.take() {
                            out.push(c);
                        }
                    }
                    "propstat" => {
                        let ok = ps_status.is_empty() || ps_status.contains(" 200");
                        if ok && let Some(c) = &mut cur {
                            for (k, v) in ps_props.drain(..) {
                                c.props.insert(k, v);
                            }
                            c.types.append(&mut ps_types);
                        }
                    }
                    _ if parent == Some("prop") => {
                        if let Some(p) = prop_name.take() {
                            ps_props.push((p, std::mem::take(&mut prop_text)));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(out)
}

/// A collection found on the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collection {
    pub url: String,
    pub name: String,
    pub color: Option<String>,
}

pub struct Client {
    net: Net,
    auth: Option<String>,
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

impl Client {
    pub fn new(net: Net, user: &str, password: &str) -> Self {
        let auth = (!user.is_empty()).then(|| {
            format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}")))
        });
        Client { net, auth }
    }

    async fn call(
        &self,
        url: &Url,
        method: &str,
        headers: &[(&str, &str)],
        body: Option<&str>,
    ) -> Result<Response, String> {
        let mut h: Vec<(&str, &str)> = headers.to_vec();
        if let Some(a) = &self.auth {
            h.push(("Authorization", a));
        }
        let b = match body {
            Some(s) => Body::Bytes(s.as_bytes()),
            None => Body::Empty,
        };
        self.net.request(url, method, &h, b, Sink::Memory(32 << 20)).await.map_err(|e| e.to_string())
    }

    fn check(r: &Response) -> Result<(), String> {
        match r.status {
            200..=299 => Ok(()),
            401 => Err("the server wants a user name and password (HTTP 401)".into()),
            403 => Err("not allowed (HTTP 403)".into()),
            404 => Err("not found (HTTP 404)".into()),
            412 => Err("changed on the server since the last sync (HTTP 412)".into()),
            s => Err(format!("the server answered HTTP {s}")),
        }
    }

    /// PROPFIND, following redirects, returning the final URL and the responses.
    async fn propfind(&self, url: &Url, depth: &str, props: &str) -> Result<(Url, Vec<DavResponse>), String> {
        let body = format!(
            r#"<?xml version="1.0" encoding="utf-8"?><d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:card="urn:ietf:params:xml:ns:carddav" xmlns:cs="http://calendarserver.org/ns/" xmlns:a="http://apple.com/ns/ical/"><d:prop>{props}</d:prop></d:propfind>"#
        );
        let mut url = url.clone();
        for _ in 0..5 {
            let r = self
                .call(
                    &url,
                    "PROPFIND",
                    &[("Depth", depth), ("Content-Type", "application/xml; charset=utf-8")],
                    Some(&body),
                )
                .await?;
            if matches!(r.status, 301 | 302 | 303 | 307 | 308)
                && let Some(loc) = r.header("location")
            {
                url = url.join(loc).map_err(|e| format!("bad redirect {loc}: {e}"))?;
                continue;
            }
            Self::check(&r)?;
            return Ok((url, parse_multistatus(&r.text())?));
        }
        Err("too many redirects".into())
    }

    /// The address books or calendars of the user that `base` belongs to. `base` may be
    /// the server, a principal, a home set, or a collection itself.
    pub async fn discover(&self, base: &Url, kind: Kind) -> Result<Vec<Collection>, String> {
        let list_props = "<d:resourcetype/><d:displayname/><a:calendar-color/><d:current-user-principal/>";
        // A collection given directly.
        if let Ok((u, rs)) = self.propfind(base, "0", list_props).await
            && let Some(r) = rs.first()
            && r.types.iter().any(|t| t == kind.collection_type())
        {
            return Ok(vec![collection(&u, r)]);
        }
        // Find the principal: from the URL itself, else the well-known address.
        let mut principal = None;
        for start in [base.clone(), base.join(kind.well_known()).map_err(|e| e.to_string())?] {
            if let Ok((u, rs)) = self.propfind(&start, "0", "<d:current-user-principal/>").await
                && let Some(p) =
                    rs.first().and_then(|r| r.props.get("current-user-principal")).filter(|p| !p.is_empty())
            {
                principal = Some(u.join(p).map_err(|e| e.to_string())?);
                break;
            }
        }
        let principal = principal.unwrap_or_else(|| base.clone());
        let home_prop = match kind {
            Kind::Contacts => "<card:addressbook-home-set/>",
            Kind::Calendars => "<c:calendar-home-set/>",
        };
        let (pu, rs) = self.propfind(&principal, "0", home_prop).await?;
        let home = rs
            .first()
            .and_then(|r| r.props.get(kind.home_set()))
            .filter(|h| !h.is_empty())
            .map(|h| pu.join(h))
            .transpose()
            .map_err(|e| e.to_string())?
            .unwrap_or(pu);
        let (hu, rs) = self.propfind(&home, "1", list_props).await?;
        let mut out: Vec<Collection> = rs
            .iter()
            .filter(|r| r.types.iter().any(|t| t == kind.collection_type()))
            .filter_map(|r| Some(collection(&hu.join(&r.href).ok()?, r)))
            .collect();
        out.dedup_by(|a, b| a.url == b.url);
        if out.is_empty() {
            return Err(format!("no {}s were found for this account", kind.collection_type()));
        }
        Ok(out)
    }

    /// Every item in a collection: href to ETag.
    pub async fn etags(&self, coll: &Url) -> Result<BTreeMap<String, String>, String> {
        let (u, rs) = self.propfind(coll, "1", "<d:getetag/><d:resourcetype/><d:getcontenttype/>").await?;
        let own = u.path().trim_end_matches('/').to_string();
        Ok(rs
            .into_iter()
            .filter(|r| r.types.iter().all(|t| t != "collection"))
            .filter_map(|r| {
                let abs = u.join(&r.href).ok()?;
                (abs.path().trim_end_matches('/') != own)
                    .then(|| (abs.path().to_string(), r.props.get("getetag").cloned().unwrap_or_default()))
            })
            .collect())
    }

    /// Fetch items by href (paths on the server), with their ETags.
    pub async fn multiget(
        &self,
        coll: &Url,
        kind: Kind,
        hrefs: &[String],
    ) -> Result<Vec<(String, String, String)>, String> {
        if hrefs.is_empty() {
            return Ok(vec![]);
        }
        let (tag, data) = match kind {
            Kind::Contacts => ("card:addressbook-multiget", "card:address-data"),
            Kind::Calendars => ("c:calendar-multiget", "c:calendar-data"),
        };
        let hs: String = hrefs.iter().map(|h| format!("<d:href>{}</d:href>", xml_escape(h))).collect();
        let body = format!(
            r#"<?xml version="1.0" encoding="utf-8"?><{tag} xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:card="urn:ietf:params:xml:ns:carddav"><d:prop><d:getetag/><{data}/></d:prop>{hs}</{tag}>"#
        );
        let r = self
            .call(coll, "REPORT", &[("Depth", "1"), ("Content-Type", "application/xml; charset=utf-8")], Some(&body))
            .await?;
        if r.status == 405 || r.status == 501 || r.status == 400 {
            // No REPORT here: fetch them one by one.
            let mut out = vec![];
            for h in hrefs {
                let u = coll.join(h).map_err(|e| e.to_string())?;
                let g = self.call(&u, "GET", &[], None).await?;
                Self::check(&g)?;
                out.push((h.clone(), g.header("etag").unwrap_or("").to_string(), g.text()));
            }
            return Ok(out);
        }
        Self::check(&r)?;
        let key = data.split(':').nth(1).unwrap_or("");
        Ok(parse_multistatus(&r.text())?
            .into_iter()
            .filter_map(|x| {
                let d = x.props.get(key)?.clone();
                let path = coll.join(&x.href).ok()?.path().to_string();
                Some((path, x.props.get("getetag").cloned().unwrap_or_default(), d))
            })
            .collect())
    }

    /// Send an item. `etag` is the version we last saw (None for a new item). Returns the
    /// new ETag, if the server gave one.
    pub async fn put(&self, url: &Url, kind: Kind, text: &str, etag: Option<&str>) -> Result<String, String> {
        let mut h = vec![("Content-Type", kind.content_type())];
        match etag.filter(|e| !e.is_empty()) {
            Some(e) => h.push(("If-Match", e)),
            None => h.push(("If-None-Match", "*")),
        }
        let r = self.call(url, "PUT", &h, Some(text)).await?;
        Self::check(&r)?;
        Ok(r.header("etag").unwrap_or("").to_string())
    }

    pub async fn delete(&self, url: &Url, etag: &str) -> Result<(), String> {
        let h: Vec<(&str, &str)> = if etag.is_empty() { vec![] } else { vec![("If-Match", etag)] };
        let r = self.call(url, "DELETE", &h, None).await?;
        if r.status == 404 {
            return Ok(());
        }
        Self::check(&r)
    }
}

fn collection(u: &Url, r: &DavResponse) -> Collection {
    let name = r.props.get("displayname").cloned().filter(|n| !n.is_empty()).unwrap_or_else(|| {
        u.path_segments()
            .and_then(|mut s| s.rfind(|x| !x.is_empty()).map(str::to_string))
            .unwrap_or_else(|| "default".into())
    });
    Collection { url: u.to_string(), name, color: r.props.get("calendar-color").cloned().filter(|c| !c.is_empty()) }
}

/// What a sync did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub sent: usize,
    pub deleted_there: usize,
    pub fetched: usize,
    pub deleted_here: usize,
    /// Items the server refused, with why.
    pub errors: Vec<String>,
}

impl SyncReport {
    pub fn summary(&self) -> String {
        let mut parts = vec![];
        if self.fetched > 0 {
            parts.push(format!("{} new or changed", self.fetched));
        }
        if self.deleted_here > 0 {
            parts.push(format!("{} removed", self.deleted_here));
        }
        if self.sent > 0 {
            parts.push(format!("{} sent", self.sent));
        }
        if self.deleted_there > 0 {
            parts.push(format!("{} deleted on the server", self.deleted_there));
        }
        if !self.errors.is_empty() {
            parts.push(format!("{} failed", self.errors.len()));
        }
        if parts.is_empty() { "up to date".into() } else { parts.join(", ") }
    }
}

/// Bring a local folder and a server collection into step: local changes go up first,
/// then whatever changed on the server comes down. The server wins a conflict.
pub async fn sync(client: &Client, coll: &Url, kind: Kind, folder: &Folder) -> Result<SyncReport, String> {
    folder.mark_synced().map_err(|e| e.to_string())?;
    let mut idx = folder.index();
    let mut rep = SyncReport::default();
    let coll = if coll.path().ends_with('/') {
        coll.clone()
    } else {
        let mut c = coll.clone();
        c.set_path(&format!("{}/", coll.path()));
        c
    };

    // Up: changed and deleted here.
    let files: Vec<String> = idx.keys().cloned().collect();
    for file in files {
        let e = idx[&file].clone();
        if e.deleted {
            let u = coll.join(&e.href).map_err(|x| x.to_string())?;
            match client.delete(&u, &e.etag).await {
                Ok(()) => {
                    rep.deleted_there += 1;
                    idx.remove(&file);
                }
                Err(x) => {
                    rep.errors.push(format!("{file}: {x}"));
                    // Fetch it again below.
                    idx.remove(&file);
                }
            }
        } else if e.dirty {
            let Some(text) = folder.read(&file) else {
                idx.remove(&file);
                continue;
            };
            let href = if e.href.is_empty() {
                coll.join(&file).map_err(|x| x.to_string())?.path().to_string()
            } else {
                e.href.clone()
            };
            let u = coll.join(&href).map_err(|x| x.to_string())?;
            match client.put(&u, kind, &text, (!e.etag.is_empty()).then_some(e.etag.as_str())).await {
                Ok(tag) => {
                    rep.sent += 1;
                    idx.insert(file.clone(), SyncEntry { href, etag: tag, dirty: false, deleted: false });
                }
                Err(x) => {
                    rep.errors.push(format!("{file}: {x}"));
                    idx.insert(file.clone(), SyncEntry { href, etag: String::new(), dirty: false, deleted: false });
                }
            }
        }
    }
    // Local files not in the index yet are new here.
    for item in folder.list() {
        if idx.contains_key(&item.file) {
            continue;
        }
        let u = coll.join(&item.file).map_err(|x| x.to_string())?;
        match client.put(&u, kind, &item.text, None).await {
            Ok(tag) => {
                rep.sent += 1;
                idx.insert(
                    item.file.clone(),
                    SyncEntry { href: u.path().to_string(), etag: tag, ..Default::default() },
                );
            }
            Err(x) => rep.errors.push(format!("{}: {x}", item.file)),
        }
    }

    // Down.
    let remote = client.etags(&coll).await?;
    let by_href: HashMap<String, String> = idx.iter().map(|(f, e)| (e.href.clone(), f.clone())).collect();
    let mut want = vec![];
    for (href, etag) in &remote {
        match by_href.get(href) {
            Some(f) if !idx[f].etag.is_empty() && idx[f].etag == *etag => {}
            _ => want.push(href.clone()),
        }
    }
    for chunk in want.chunks(50) {
        for (href, etag, data) in client.multiget(&coll, kind, chunk).await? {
            let file = by_href.get(&href).cloned().unwrap_or_else(|| {
                let last = href.rsplit('/').find(|s| !s.is_empty()).unwrap_or("item");
                let stem = last.rsplit_once('.').map_or(last, |(s, _)| s);
                let stem = url::form_urlencoded::parse(format!("x={stem}").as_bytes())
                    .next()
                    .map(|(_, v)| v.into_owned())
                    .unwrap_or_default();
                file_for(&stem, kind.ext())
            });
            folder.write(&file, &data).map_err(|e| e.to_string())?;
            idx.insert(file, SyncEntry { href, etag, ..Default::default() });
            rep.fetched += 1;
        }
    }
    // Gone from the server.
    let gone: Vec<String> = idx
        .iter()
        .filter(|(_, e)| !e.href.is_empty() && !remote.contains_key(&e.href))
        .map(|(f, _)| f.clone())
        .collect();
    for f in gone {
        folder.remove_file(&f).map_err(|e| e.to_string())?;
        idx.remove(&f);
        rep.deleted_here += 1;
    }
    folder.save_index(&idx).map_err(|e| e.to_string())?;
    Ok(rep)
}

/// Fetch an ICS subscription (`webcal://` or `https://`) into a folder, replacing what
/// was there. Returns how many events it holds.
pub async fn subscribe(net: &Net, url: &str, folder: &Folder) -> Result<usize, String> {
    let url = match url.strip_prefix("webcal://").or_else(|| url.strip_prefix("webcals://")) {
        Some(rest) => format!("https://{rest}"),
        None => url.to_string(),
    };
    let r = net.get(&url, &[("Accept", "text/calendar")], 32 << 20).await.map_err(|e| e.to_string())?;
    if !r.ok() {
        return Err(format!("the server answered HTTP {}", r.status));
    }
    let text = r.text();
    let cal = crate::event::Calendar::parse(&text);
    if cal.raw.name.is_empty() {
        return Err("that is not an iCalendar file".into());
    }
    folder.write("subscription.ics", &text).map_err(|e| e.to_string())?;
    Ok(cal.events.len())
}

pub mod testserver {
    //! A small CardDAV and CalDAV server for tests and trying things out. It keeps items
    //! in memory under `/dav/<user>/contacts/` and `/dav/<user>/calendar/`, and answers
    //! the well-known addresses, PROPFIND, multiget REPORT, GET, PUT and DELETE.

    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[derive(Default)]
    pub struct State {
        pub items: BTreeMap<String, (String, u64)>,
        pub next: u64,
    }

    pub type Shared = Arc<Mutex<State>>;

    /// Start on a free local port. Returns the base URL (`http://127.0.0.1:port`) and the
    /// state. The user is "me", with any password.
    pub async fn start() -> (String, Shared) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let state: Shared = Arc::default();
        let s2 = state.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = l.accept().await else { break };
                let st = s2.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 8192];
                    let (head, body) = loop {
                        let Ok(n) = sock.read(&mut tmp).await else { return };
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&buf[..p]).into_owned();
                            let len: usize = head
                                .lines()
                                .find_map(|l| {
                                    let (k, v) = l.split_once(':')?;
                                    k.eq_ignore_ascii_case("content-length").then(|| v.trim().parse().ok())?
                                })
                                .unwrap_or(0);
                            let mut body = buf[p + 4..].to_vec();
                            while body.len() < len {
                                let Ok(n) = sock.read(&mut tmp).await else { return };
                                if n == 0 {
                                    break;
                                }
                                body.extend_from_slice(&tmp[..n]);
                            }
                            break (head, String::from_utf8_lossy(&body).into_owned());
                        }
                    };
                    let (status, headers, out) = handle(&st, &head, &body);
                    let mut resp =
                        format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n", out.len());
                    for (k, v) in headers {
                        resp.push_str(&format!("{k}: {v}\r\n"));
                    }
                    resp.push_str("\r\n");
                    resp.push_str(&out);
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });
        (format!("http://{addr}"), state)
    }

    fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
        head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case(name).then(|| v.trim())
        })
    }

    fn ms(inner: &str) -> String {
        format!(
            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:card="urn:ietf:params:xml:ns:carddav">{inner}</d:multistatus>"#
        )
    }

    fn resp(href: &str, props: &str) -> String {
        format!(
            "<d:response><d:href>{href}</d:href><d:propstat><d:prop>{props}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"
        )
    }

    fn esc(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
    }

    fn handle(st: &Shared, head: &str, body: &str) -> (&'static str, Vec<(String, String)>, String) {
        let first = head.lines().next().unwrap_or("");
        let mut parts = first.split(' ');
        let method = parts.next().unwrap_or("");
        let path = parts.next().unwrap_or("/").to_string();
        let xml = vec![("Content-Type".to_string(), "application/xml".to_string())];
        let books = "/dav/me/contacts/";
        let cals = "/dav/me/calendar/";
        match (method, path.as_str()) {
            ("PROPFIND", "/.well-known/carddav") | ("PROPFIND", "/.well-known/caldav") => {
                ("301 Moved Permanently", vec![("Location".into(), "/dav/".into())], String::new())
            }
            ("PROPFIND", "/" | "/dav/" | "/dav/me/") => {
                let p = "<d:current-user-principal><d:href>/dav/me/</d:href></d:current-user-principal><card:addressbook-home-set><d:href>/dav/me/</d:href></card:addressbook-home-set><c:calendar-home-set><d:href>/dav/me/</d:href></c:calendar-home-set><d:resourcetype><d:collection/></d:resourcetype>";
                let mut inner = resp(&path, p);
                if header(head, "depth") == Some("1") {
                    inner.push_str(&resp(books, "<d:resourcetype><d:collection/><card:addressbook/></d:resourcetype><d:displayname>Contacts</d:displayname>"));
                    inner.push_str(&resp(cals, "<d:resourcetype><d:collection/><c:calendar/></d:resourcetype><d:displayname>Work</d:displayname>"));
                }
                ("207 Multi-Status", xml, ms(&inner))
            }
            ("PROPFIND", p) if p == books || p == cals => {
                let kind = if p == books { "<card:addressbook/>" } else { "<c:calendar/>" };
                let mut inner = resp(p, &format!("<d:resourcetype><d:collection/>{kind}</d:resourcetype>"));
                if header(head, "depth") == Some("1") {
                    for (h, (_, tag)) in st.lock().unwrap().items.iter().filter(|(h, _)| h.starts_with(p)) {
                        inner.push_str(&resp(h, &format!("<d:getetag>\"{tag}\"</d:getetag><d:resourcetype/>")));
                    }
                }
                ("207 Multi-Status", xml, ms(&inner))
            }
            ("REPORT", p) => {
                let data = if p == books { "card:address-data" } else { "c:calendar-data" };
                let s = st.lock().unwrap();
                let mut inner = String::new();
                for part in body.split("<d:href>").skip(1) {
                    let h = part.split("</d:href>").next().unwrap_or("");
                    if let Some((text, tag)) = s.items.get(h) {
                        inner.push_str(&resp(
                            h,
                            &format!("<d:getetag>\"{tag}\"</d:getetag><{data}>{}</{data}>", esc(text)),
                        ));
                    }
                }
                ("207 Multi-Status", xml, ms(&inner))
            }
            ("GET", p) => match st.lock().unwrap().items.get(p) {
                Some((t, tag)) => ("200 OK", vec![("ETag".into(), format!("\"{tag}\""))], t.clone()),
                None => ("404 Not Found", vec![], String::new()),
            },
            ("PUT", p) => {
                let mut s = st.lock().unwrap();
                let current = s.items.get(p).map(|(_, t)| format!("\"{t}\""));
                if let Some(m) = header(head, "if-match")
                    && current.as_deref() != Some(m)
                {
                    return ("412 Precondition Failed", vec![], String::new());
                }
                if header(head, "if-none-match") == Some("*") && current.is_some() {
                    return ("412 Precondition Failed", vec![], String::new());
                }
                s.next += 1;
                let tag = s.next;
                s.items.insert(p.to_string(), (body.to_string(), tag));
                ("201 Created", vec![("ETag".into(), format!("\"{tag}\""))], String::new())
            }
            ("DELETE", p) => match st.lock().unwrap().items.remove(p) {
                Some(_) => ("204 No Content", vec![], String::new()),
                None => ("404 Not Found", vec![], String::new()),
            },
            _ => ("405 Method Not Allowed", vec![], String::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multistatus() {
        let x = r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
<d:response><d:href>/cal/</d:href>
<d:propstat><d:prop><d:resourcetype><d:collection/><c:calendar/></d:resourcetype><d:displayname>Home &amp; away</d:displayname>
<d:current-user-principal><d:href>/p/me/</d:href></d:current-user-principal></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
<d:propstat><d:prop><d:getetag/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat>
</d:response></d:multistatus>"#;
        let r = parse_multistatus(x).unwrap();
        assert_eq!(r[0].href, "/cal/");
        assert_eq!(r[0].types, ["collection", "calendar"]);
        assert_eq!(r[0].props["displayname"], "Home & away");
        assert_eq!(r[0].props["current-user-principal"], "/p/me/");
        assert!(!r[0].props.contains_key("getetag"));
    }
}
