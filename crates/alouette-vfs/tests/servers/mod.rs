//! Tiny FTP, WebDAV and S3 servers over a folder or a map, just enough for the backend tests.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

pub fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("alouette-vfs-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn real(root: &Path, p: &str) -> PathBuf {
    let rel = p.trim_start_matches('/');
    if rel.is_empty() { root.to_path_buf() } else { root.join(rel) }
}

fn mtime(m: &std::fs::Metadata) -> u64 {
    m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0)
}

// ---------- FTP ----------

pub async fn ftp(root: PathBuf, user: &'static str, pass: &'static str) -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((s, _)) = l.accept().await {
            let root = root.clone();
            tokio::spawn(async move {
                let _ = ftp_session(s, root, user, pass).await;
            });
        }
    });
    addr
}

async fn ftp_session(s: TcpStream, root: PathBuf, user: &str, pass: &str) -> std::io::Result<()> {
    let (r, mut w) = s.into_split();
    let mut r = BufReader::new(r);
    w.write_all(b"220 test ftpd\r\n").await?;
    let mut data: Option<TcpListener> = None;
    let mut rnfr = None;
    let mut authed = false;
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        let l = line.trim_end_matches(['\r', '\n']);
        let (cmd, arg) = l.split_once(' ').unwrap_or((l, ""));
        let cmd = cmd.to_ascii_uppercase();
        let reply: String = match cmd.as_str() {
            "USER" => "331 password please".into(),
            "PASS" => {
                authed = arg == pass;
                let _ = user;
                if authed { "230 in".into() } else { "530 no".into() }
            }
            _ if !authed => "530 log in first".into(),
            "TYPE" | "OPTS" => "200 ok".into(),
            "FEAT" => "211-Features\r\n MLST type*;size*;modify*;\r\n UTF8\r\n211 End".into(),
            "PWD" => "257 \"/\" is home".into(),
            "EPSV" => {
                let dl = TcpListener::bind("127.0.0.1:0").await?;
                let p = dl.local_addr()?.port();
                data = Some(dl);
                format!("229 Entering Extended Passive Mode (|||{p}|)")
            }
            "MLSD" | "RETR" | "STOR" => {
                let Some(dl) = data.take() else {
                    w.write_all(b"425 use EPSV first\r\n").await?;
                    continue;
                };
                let path = real(&root, arg);
                if cmd == "RETR" && !path.is_file() || cmd == "MLSD" && !path.is_dir() {
                    w.write_all(b"550 no such file\r\n").await?;
                    continue;
                }
                let (mut d, _) = dl.accept().await?;
                w.write_all(b"150 here it comes\r\n").await?;
                match cmd.as_str() {
                    "MLSD" => {
                        let mut out = String::new();
                        for e in std::fs::read_dir(&path)? {
                            let e = e?;
                            let m = e.metadata()?;
                            let t = time_str(mtime(&m));
                            let name = e.file_name().to_string_lossy().into_owned();
                            if m.is_dir() {
                                out.push_str(&format!("type=dir;modify={t}; {name}\r\n"));
                            } else {
                                out.push_str(&format!("type=file;size={};modify={t}; {name}\r\n", m.len()));
                            }
                        }
                        d.write_all(out.as_bytes()).await?;
                    }
                    "RETR" => d.write_all(&std::fs::read(&path)?).await?,
                    _ => {
                        let mut buf = vec![];
                        d.read_to_end(&mut buf).await?;
                        std::fs::write(&path, buf)?;
                    }
                }
                d.shutdown().await.ok();
                drop(d);
                "226 done".into()
            }
            "MKD" => ok_or(std::fs::create_dir(real(&root, arg)), "257 made"),
            "RMD" => ok_or(std::fs::remove_dir(real(&root, arg)), "250 gone"),
            "DELE" => ok_or(std::fs::remove_file(real(&root, arg)), "250 gone"),
            "RNFR" => {
                rnfr = Some(real(&root, arg));
                "350 and to?".into()
            }
            "RNTO" => match rnfr.take() {
                Some(from) => ok_or(std::fs::rename(from, real(&root, arg)), "250 moved"),
                None => "503 RNFR first".into(),
            },
            "QUIT" => {
                w.write_all(b"221 bye\r\n").await?;
                return Ok(());
            }
            _ => "502 not here".into(),
        };
        w.write_all(format!("{reply}\r\n").as_bytes()).await?;
    }
}

fn ok_or(r: std::io::Result<()>, ok: &str) -> String {
    match r {
        Ok(()) => ok.into(),
        Err(e) => format!("550 {e}"),
    }
}

fn time_str(secs: u64) -> String {
    let t = time::OffsetDateTime::from_unix_timestamp(secs as i64).unwrap();
    format!("{:04}{:02}{:02}{:02}{:02}{:02}", t.year(), t.month() as u8, t.day(), t.hour(), t.minute(), t.second())
}

// ---------- HTTP ----------

pub struct Req {
    pub method: String,
    pub path: String,
    pub query: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Req {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

pub struct Resp {
    pub status: u16,
    pub body: Vec<u8>,
}

fn resp(status: u16, body: impl Into<Vec<u8>>) -> Resp {
    Resp { status, body: body.into() }
}

pub async fn http<F>(handler: F) -> SocketAddr
where
    F: Fn(Req) -> Resp + Send + Sync + 'static,
{
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let handler = Arc::new(handler);
    tokio::spawn(async move {
        while let Ok((s, _)) = l.accept().await {
            let h = handler.clone();
            tokio::spawn(async move {
                let _ = http_one(s, h.as_ref()).await;
            });
        }
    });
    addr
}

async fn http_one(s: TcpStream, h: &(dyn Fn(Req) -> Resp + Send + Sync)) -> std::io::Result<()> {
    let mut r = BufReader::new(s);
    let mut line = String::new();
    r.read_line(&mut line).await?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    let mut headers = vec![];
    loop {
        line.clear();
        r.read_line(&mut line).await?;
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let len: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; len];
    r.read_exact(&mut body).await?;
    let (path, query) =
        target.split_once('?').map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or((target, String::new()));
    let res = h(Req { method, path: decode(&path), query, headers, body });
    let mut s = r.into_inner();
    s.write_all(
        format!("HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", res.status, res.body.len())
            .as_bytes(),
    )
    .await?;
    s.write_all(&res.body).await?;
    s.shutdown().await
}

pub fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = vec![];
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

fn encode(s: &str) -> String {
    s.bytes()
        .map(|c| match c {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => (c as char).to_string(),
            _ => format!("%{c:02X}"),
        })
        .collect()
}

// ---------- WebDAV ----------

/// Serves `root` under `/dav/`, with Basic auth for user:pass.
pub async fn dav(root: PathBuf, user: &str, pass: &str) -> SocketAddr {
    use base64::Engine;
    let auth = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}")));
    http(move |q| {
        if q.header("authorization") != Some(auth.as_str()) {
            return resp(401, "");
        }
        let Some(rel) = q.path.strip_prefix("/dav") else { return resp(404, "") };
        let p = real(&root, rel);
        let r = match q.method.as_str() {
            "PROPFIND" => {
                let Ok(m) = std::fs::metadata(&p) else { return resp(404, "") };
                let mut xml = String::from(r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:">"#);
                let entry = |href: &str, m: &std::fs::Metadata| {
                    let t = time::OffsetDateTime::from_unix_timestamp(mtime(m) as i64)
                        .unwrap()
                        .format(&time::format_description::well_known::Rfc2822)
                        .unwrap();
                    if m.is_dir() {
                        format!("<d:response><d:href>{}</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype><d:getlastmodified>{t}</d:getlastmodified></d:prop></d:propstat></d:response>", encode(href))
                    } else {
                        format!("<d:response><d:href>{}</d:href><d:propstat><d:prop><d:resourcetype/><d:getcontentlength>{}</d:getcontentlength><d:getlastmodified>{t}</d:getlastmodified></d:prop></d:propstat></d:response>", encode(href), m.len())
                    }
                };
                xml.push_str(&entry(&q.path, &m));
                if m.is_dir() && q.header("depth") == Some("1") {
                    for e in std::fs::read_dir(&p).unwrap() {
                        let e = e.unwrap();
                        let m = e.metadata().unwrap();
                        let name = e.file_name().to_string_lossy().into_owned();
                        let base = q.path.trim_end_matches('/');
                        let href = if m.is_dir() { format!("{base}/{name}/") } else { format!("{base}/{name}") };
                        xml.push_str(&entry(&href, &m));
                    }
                }
                xml.push_str("</d:multistatus>");
                return resp(207, xml);
            }
            "GET" => return std::fs::read(&p).map(|b| resp(200, b)).unwrap_or_else(|_| resp(404, "")),
            "PUT" => std::fs::write(&p, &q.body),
            "MKCOL" => std::fs::create_dir(&p),
            "DELETE" => {
                if p.is_dir() {
                    std::fs::remove_dir_all(&p)
                } else {
                    std::fs::remove_file(&p)
                }
            }
            "MOVE" => {
                let dest = q.header("destination").unwrap_or_default();
                let path = url::Url::parse(dest).map(|u| u.path().to_string()).unwrap_or_default();
                let to = real(&root, decode(&path).strip_prefix("/dav").unwrap_or_default());
                if to.exists() && q.header("overwrite") == Some("F") {
                    return resp(412, "");
                }
                std::fs::rename(&p, to)
            }
            _ => return resp(405, ""),
        };
        match r {
            Ok(()) => resp(201, ""),
            Err(_) => resp(409, ""),
        }
    })
    .await
}

// ---------- S3 ----------

/// Path-style S3 over an in-memory map: ListObjectsV2, GET, PUT (and copy), DELETE.
pub async fn s3(bucket: &'static str) -> (SocketAddr, Arc<Mutex<BTreeMap<String, Vec<u8>>>>) {
    let store: Arc<Mutex<BTreeMap<String, Vec<u8>>>> = Arc::default();
    let st = store.clone();
    let addr = http(move |q| {
        if !q.header("authorization").is_some_and(|a| a.starts_with("AWS4-HMAC-SHA256 Credential=AKID/")) {
            return resp(403, "<Error><Code>AccessDenied</Code><Message>no</Message></Error>");
        }
        let Some(rest) = q.path.strip_prefix(&format!("/{bucket}")) else { return resp(404, "") };
        let key = rest.trim_start_matches('/').to_string();
        let mut m = st.lock().unwrap();
        match q.method.as_str() {
            "GET" if key.is_empty() => {
                let params: BTreeMap<String, String> =
                    url::form_urlencoded::parse(q.query.as_bytes()).map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
                let prefix = params.get("prefix").cloned().unwrap_or_default();
                let mut xml = String::from("<ListBucketResult><IsTruncated>false</IsTruncated>");
                let mut dirs = std::collections::BTreeSet::new();
                for (k, v) in m.range(prefix.clone()..) {
                    let Some(rest) = k.strip_prefix(&prefix) else { break };
                    if rest.is_empty() {
                        continue;
                    }
                    match rest.find('/') {
                        Some(i) => {
                            dirs.insert(format!("{prefix}{}", &rest[..=i]));
                        }
                        None => xml.push_str(&format!(
                            "<Contents><Key>{k}</Key><LastModified>2024-01-02T03:04:05.000Z</LastModified><Size>{}</Size></Contents>",
                            v.len()
                        )),
                    }
                }
                for d in dirs {
                    xml.push_str(&format!("<CommonPrefixes><Prefix>{d}</Prefix></CommonPrefixes>"));
                }
                xml.push_str("</ListBucketResult>");
                resp(200, xml)
            }
            "GET" => m.get(&key).map(|b| resp(200, b.clone())).unwrap_or_else(|| {
                resp(404, "<Error><Code>NoSuchKey</Code><Message>The specified key does not exist.</Message></Error>")
            }),
            "PUT" => {
                if let Some(src) = q.header("x-amz-copy-source") {
                    let src = decode(src);
                    let src = src.trim_start_matches('/').strip_prefix(&format!("{bucket}/")).unwrap_or_default().to_string();
                    let Some(b) = m.get(&src).cloned() else { return resp(404, "") };
                    m.insert(key, b);
                    return resp(200, "<CopyObjectResult/>");
                }
                m.insert(key, q.body);
                resp(200, "")
            }
            "DELETE" => {
                m.remove(&key);
                resp(204, "")
            }
            _ => resp(405, ""),
        }
    })
    .await;
    (addr, store)
}
