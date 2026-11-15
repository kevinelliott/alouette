//! S3 and S3-compatible object stores. Folders are key prefixes, as the S3 console shows.

use alouette_net::Net;
use alouette_net::http::{Body, Sink};
use url::Url;

use crate::sigv4::{self, Credentials};
use crate::xml::{Ev, events};
use crate::{BoxFut, Entry, Fs, Progress, Reader, Writer};

pub struct S3 {
    net: Net,
    endpoint: Url,
    region: String,
    bucket: String,
    creds: Credentials,
    /// `https://bucket.s3.amazonaws.com/key` rather than `https://endpoint/bucket/key`.
    virtual_host: bool,
}

#[derive(Clone)]
pub struct S3Site {
    /// Such as `https://s3.us-east-1.amazonaws.com` or `http://localhost:9000`.
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub creds: Credentials,
}

/// Keys from the environment or `~/.aws/credentials`, as the AWS tools find them.
pub fn credentials_from_env(profile: Option<&str>) -> Option<Credentials> {
    if let (Ok(a), Ok(s)) = (std::env::var("AWS_ACCESS_KEY_ID"), std::env::var("AWS_SECRET_ACCESS_KEY")) {
        return Some(Credentials {
            access_key: a,
            secret_key: s,
            session_token: std::env::var("AWS_SESSION_TOKEN").ok(),
        });
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let text = std::fs::read_to_string(std::path::Path::new(&home).join(".aws").join("credentials")).ok()?;
    let want = profile.map(str::to_string).or_else(|| std::env::var("AWS_PROFILE").ok()).unwrap_or("default".into());
    parse_credentials(&text, &want)
}

pub fn parse_credentials(text: &str, profile: &str) -> Option<Credentials> {
    let mut in_profile = false;
    let (mut a, mut s, mut t) = (None, None, None);
    for line in text.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_profile = name.trim() == profile;
            continue;
        }
        if !in_profile {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            match k.trim() {
                "aws_access_key_id" => a = Some(v.trim().to_string()),
                "aws_secret_access_key" => s = Some(v.trim().to_string()),
                "aws_session_token" => t = Some(v.trim().to_string()),
                _ => {}
            }
        }
    }
    Some(Credentials { access_key: a?, secret_key: s?, session_token: t })
}

fn err(p: &str, e: impl std::fmt::Display) -> String {
    format!("{p}: {e}")
}

impl S3 {
    pub fn new(net: Net, site: S3Site) -> Result<Self, String> {
        let endpoint = Url::parse(&site.endpoint).map_err(|e| format!("{}: {e}", site.endpoint))?;
        let host = endpoint.host_str().unwrap_or_default();
        // AWS prefers virtual-hosted buckets; other stores mostly want path style. Dots in a
        // bucket name break TLS names, so those stay path style too.
        let virtual_host = host.ends_with("amazonaws.com") && !site.bucket.contains('.');
        Ok(Self { net, endpoint, region: site.region, bucket: site.bucket, creds: site.creds, virtual_host })
    }

    /// The object key for a path: `/photos/a.jpg` is `photos/a.jpg`.
    fn key(path: &str) -> String {
        path.trim_start_matches('/').to_string()
    }

    fn url_for(&self, key: &str, query: &[(String, String)]) -> (Url, String, String) {
        let mut u = self.endpoint.clone();
        let (host, path) = if self.virtual_host {
            let h = format!("{}.{}", self.bucket, self.endpoint.host_str().unwrap_or_default());
            let _ = u.set_host(Some(&h));
            (h, format!("/{}", sigv4::uri_encode(key, true)))
        } else {
            let h = match self.endpoint.port() {
                Some(p) => format!("{}:{p}", self.endpoint.host_str().unwrap_or_default()),
                None => self.endpoint.host_str().unwrap_or_default().to_string(),
            };
            (h, format!("/{}/{}", sigv4::uri_encode(&self.bucket, false), sigv4::uri_encode(key, true)))
        };
        let host = match (self.virtual_host, self.endpoint.port()) {
            (true, Some(p)) => format!("{host}:{p}"),
            _ => host,
        };
        let mut q: Vec<String> = query
            .iter()
            .map(|(k, v)| format!("{}={}", sigv4::uri_encode(k, false), sigv4::uri_encode(v, false)))
            .collect();
        q.sort();
        let full = if q.is_empty() { path.clone() } else { format!("{path}?{}", q.join("&")) };
        // set_path would re-encode; build the URL text directly.
        let base = format!("{}://{host}", u.scheme());
        let url = Url::parse(&format!("{base}{full}")).unwrap_or(u);
        (url, host, path)
    }

    /// Send a signed request.
    async fn send(
        &self,
        method: &str,
        key: &str,
        query: &[(String, String)],
        extra: &[(String, String)],
        body: Body<'_>,
        sink: Sink<'_>,
    ) -> Result<alouette_net::http::Response, String> {
        let (url, host, path) = self.url_for(key, query);
        let date = sigv4::amz_now();
        let mut headers: Vec<(String, String)> = vec![
            ("host".into(), host),
            ("x-amz-content-sha256".into(), sigv4::UNSIGNED.into()),
            ("x-amz-date".into(), date.clone()),
        ];
        if let Some(t) = &self.creds.session_token {
            headers.push(("x-amz-security-token".into(), t.clone()));
        }
        headers.extend(extra.iter().cloned());
        let auth = sigv4::authorization(
            &self.creds,
            &self.region,
            "s3",
            method,
            &path,
            query,
            &headers,
            sigv4::UNSIGNED,
            &date,
        );
        let mut send: Vec<(&str, &str)> =
            headers.iter().filter(|(k, _)| k != "host").map(|(k, v)| (k.as_str(), v.as_str())).collect();
        send.push(("Authorization", auth.as_str()));
        let r = self.net.request(&url, method, &send, body, sink).await.map_err(|e| e.to_string())?;
        if !r.ok() {
            return Err(s3_error(r.status, &r.text()));
        }
        Ok(r)
    }

    /// Everything directly inside `prefix`: objects and "folders" (common prefixes).
    pub async fn list_prefix(&self, prefix: &str) -> Result<Vec<Entry>, String> {
        let mut out = vec![];
        let mut token: Option<String> = None;
        loop {
            let mut q = vec![
                ("list-type".to_string(), "2".to_string()),
                ("delimiter".to_string(), "/".to_string()),
                ("prefix".to_string(), prefix.to_string()),
            ];
            if let Some(t) = &token {
                q.push(("continuation-token".into(), t.clone()));
            }
            let r = self.send("GET", "", &q, &[], Body::Empty, Sink::Memory(32 * 1024 * 1024)).await?;
            let (mut entries, next) = parse_list(&r.text(), prefix)?;
            out.append(&mut entries);
            match next {
                Some(t) => token = Some(t),
                None => break,
            }
        }
        Ok(out)
    }
}

/// Parse a ListObjectsV2 answer. Returns entries and the continuation token, if truncated.
pub fn parse_list(xml: &str, prefix: &str) -> Result<(Vec<Entry>, Option<String>), String> {
    let mut out = vec![];
    let mut stack: Vec<String> = vec![];
    let (mut key, mut size, mut modified) = (String::new(), None, None);
    let mut truncated = false;
    let mut next = None;
    for ev in events(xml)? {
        match ev {
            Ev::Start(n) => {
                if n == "Contents" {
                    key.clear();
                    size = None;
                    modified = None;
                }
                stack.push(n);
            }
            Ev::Text(t) => {
                let parent = stack.len().checked_sub(2).map(|i| stack[i].as_str());
                match (parent, stack.last().map(String::as_str)) {
                    (Some("Contents"), Some("Key")) => key = t,
                    (Some("Contents"), Some("Size")) => size = t.parse().ok(),
                    (Some("Contents"), Some("LastModified")) => modified = parse_iso8601(&t),
                    (Some("CommonPrefixes"), Some("Prefix")) => {
                        let name = t.strip_prefix(prefix).unwrap_or(&t).trim_end_matches('/').to_string();
                        if !name.is_empty() {
                            out.push(Entry { name, is_dir: true, ..Entry::default() });
                        }
                    }
                    (_, Some("IsTruncated")) => truncated = t == "true",
                    (_, Some("NextContinuationToken")) => next = Some(t),
                    _ => {}
                }
            }
            Ev::End(n) => {
                stack.pop();
                if n == "Contents" {
                    let name = key.strip_prefix(prefix).unwrap_or(&key).to_string();
                    // The folder marker itself ("photos/") lists as an empty name.
                    if !name.is_empty() && !name.contains('/') {
                        out.push(Entry { name, is_dir: false, is_link: false, size, modified });
                    }
                }
            }
        }
    }
    Ok((out, if truncated { next } else { None }))
}

/// `<Error><Code>NoSuchBucket</Code><Message>…</Message></Error>` into a sentence.
fn s3_error(status: u16, body: &str) -> String {
    let ev = events(body).unwrap_or_default();
    let mut code = None;
    let mut msg = None;
    let mut cur = String::new();
    for e in ev {
        match e {
            Ev::Start(n) => cur = n,
            Ev::Text(t) if cur == "Code" => code = Some(t),
            Ev::Text(t) if cur == "Message" => msg = Some(t),
            _ => {}
        }
    }
    match (code, msg) {
        (Some(c), Some(m)) => format!("{c}: {m}"),
        (Some(c), None) => c,
        _ => format!("the server answered HTTP {status}"),
    }
}

/// `2009-10-12T17:50:30.000Z` to seconds since 1970.
pub fn parse_iso8601(s: &str) -> Option<i64> {
    let fmt = time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::parse(s, &fmt).ok().map(|t| t.unix_timestamp())
}

fn dir_prefix(path: &str) -> String {
    let k = S3::key(path);
    if k.is_empty() || k.ends_with('/') { k } else { format!("{k}/") }
}

impl Fs for S3 {
    fn label(&self) -> String {
        format!("s3://{}", self.bucket)
    }
    fn badge(&self) -> Option<String> {
        Some(format!("S3 · {}", self.endpoint.host_str().unwrap_or_default()))
    }
    fn home(&self) -> BoxFut<'_, String> {
        Box::pin(async { Ok("/".to_string()) })
    }
    fn list<'a>(&'a self, path: &'a str) -> BoxFut<'a, Vec<Entry>> {
        Box::pin(async move { self.list_prefix(&dir_prefix(path)).await.map_err(|e| err(path, e)) })
    }
    fn download<'a>(&'a self, path: &'a str, to: Writer<'a>, progress: Progress<'a>) -> BoxFut<'a, u64> {
        Box::pin(async move {
            let n = std::sync::atomic::AtomicU64::new(0);
            let counted = |b: u64| {
                n.store(b, std::sync::atomic::Ordering::Relaxed);
                progress(b);
            };
            let r = self.send("GET", &Self::key(path), &[], &[], Body::Empty, Sink::Writer(to, &counted)).await;
            r.map_err(|e| err(path, e))?;
            Ok(n.load(std::sync::atomic::Ordering::Relaxed))
        })
    }
    fn upload<'a>(&'a self, path: &'a str, from: Reader<'a>, size: u64, progress: Progress<'a>) -> BoxFut<'a, u64> {
        Box::pin(async move {
            if size > 5 * 1024 * 1024 * 1024 {
                return Err(err(path, "files over 5 GB need multipart upload, which is not supported yet"));
            }
            self.send("PUT", &Self::key(path), &[], &[], Body::Reader(from, size), Sink::Memory(64 * 1024))
                .await
                .map_err(|e| err(path, e))?;
            progress(size);
            Ok(size)
        })
    }
    fn mkdir<'a>(&'a self, path: &'a str) -> BoxFut<'a, ()> {
        Box::pin(async move {
            self.send("PUT", &dir_prefix(path), &[], &[], Body::Bytes(&[]), Sink::Memory(64 * 1024))
                .await
                .map(|_| ())
                .map_err(|e| err(path, e))
        })
    }
    fn remove<'a>(&'a self, path: &'a str, is_dir: bool) -> BoxFut<'a, ()> {
        Box::pin(async move {
            let key = if is_dir { dir_prefix(path) } else { Self::key(path) };
            self.send("DELETE", &key, &[], &[], Body::Empty, Sink::Memory(64 * 1024))
                .await
                .map(|_| ())
                .map_err(|e| err(path, e))
        })
    }
    fn rename<'a>(&'a self, from: &'a str, to: &'a str) -> BoxFut<'a, ()> {
        Box::pin(async move {
            // S3 has no rename: copy, then delete the original.
            let source = format!("/{}/{}", self.bucket, sigv4::uri_encode(&Self::key(from), true));
            self.send(
                "PUT",
                &Self::key(to),
                &[],
                &[("x-amz-copy-source".into(), source)],
                Body::Empty,
                Sink::Memory(1024 * 1024),
            )
            .await
            .map_err(|e| err(from, e))?;
            self.send("DELETE", &Self::key(from), &[], &[], Body::Empty, Sink::Memory(64 * 1024))
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
    fn parses_a_listing() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
  <Name>bucket</Name><Prefix>photos/</Prefix><IsTruncated>true</IsTruncated>
  <NextContinuationToken>tok==</NextContinuationToken>
  <Contents><Key>photos/</Key><Size>0</Size></Contents>
  <Contents><Key>photos/cat &amp; dog.jpg</Key><LastModified>2009-10-12T17:50:30.000Z</LastModified><Size>434234</Size></Contents>
  <CommonPrefixes><Prefix>photos/2024/</Prefix></CommonPrefixes>
</ListBucketResult>"#;
        let (e, next) = parse_list(xml, "photos/").unwrap();
        assert_eq!(next.as_deref(), Some("tok=="));
        assert_eq!(e.len(), 2);
        assert_eq!((e[0].name.as_str(), e[0].size, e[0].modified), ("cat & dog.jpg", Some(434234), Some(1255369830)));
        assert!(e[1].is_dir && e[1].name == "2024");
    }

    #[test]
    fn reads_aws_credentials_file() {
        let c = parse_credentials(
            "[default]\naws_access_key_id = A\naws_secret_access_key=B\n[work]\naws_access_key_id=C\naws_secret_access_key=D\n",
            "work",
        )
        .unwrap();
        assert_eq!((c.access_key.as_str(), c.secret_key.as_str()), ("C", "D"));
        assert!(parse_credentials("[x]\n", "default").is_none());
    }

    #[test]
    fn errors_read_well() {
        assert_eq!(
            s3_error(404, "<Error><Code>NoSuchKey</Code><Message>The key does not exist</Message></Error>"),
            "NoSuchKey: The key does not exist"
        );
        assert_eq!(s3_error(500, ""), "the server answered HTTP 500");
    }

    #[test]
    fn urls() {
        let net = Net::new(&Default::default()).unwrap();
        let mk = |ep: &str, b: &str| {
            S3::new(
                net.clone(),
                S3Site {
                    endpoint: ep.into(),
                    region: "us-east-1".into(),
                    bucket: b.into(),
                    creds: Credentials { access_key: "a".into(), secret_key: "b".into(), session_token: None },
                },
            )
            .unwrap()
        };
        let (u, h, p) = mk("https://s3.us-east-1.amazonaws.com", "bkt").url_for("a b/c.txt", &[]);
        assert_eq!(u.as_str(), "https://bkt.s3.us-east-1.amazonaws.com/a%20b/c.txt");
        assert_eq!((h.as_str(), p.as_str()), ("bkt.s3.us-east-1.amazonaws.com", "/a%20b/c.txt"));
        let (u, h, _) = mk("http://localhost:9000", "bkt").url_for("k", &[("prefix".into(), "x/".into())]);
        assert_eq!(u.as_str(), "http://localhost:9000/bkt/k?prefix=x%2F");
        assert_eq!(h, "localhost:9000");
    }
}
