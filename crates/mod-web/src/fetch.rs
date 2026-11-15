//! Loading pages in the background.

use std::path::{Path, PathBuf};

use alouette_html::Page;
use alouette_net::ConnInfo;
use alouette_net::web::{Browser, WebRequest, WebResponse};

/// What a load produced.
#[derive(Debug, Clone)]
pub enum Body {
    /// A web page: the HTML, parsed whole and (if asked) in reader mode.
    Html { html: String, page: Box<Page>, reader: Option<Box<Page>> },
    /// Plain text, shown as is.
    Text(String),
    /// Something else, saved to the downloads folder.
    Saved(PathBuf, usize),
    /// A redirect to a scheme another module handles.
    Elsewhere(String),
}

#[derive(Debug, Clone)]
pub struct Fetched {
    pub url: String,
    pub status: u16,
    pub protocol: &'static str,
    pub info: ConnInfo,
    pub body: Body,
    pub redirects: usize,
}

/// A finished load, sent back to the window that asked for it.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub window: u64,
    pub req: u64,
    pub result: Result<Fetched, String>,
}

/// Fetch and parse. `reader` asks for the reader-mode parse as well. `save` forces a
/// download whatever the type.
pub async fn load(
    browser: &Browser,
    req: WebRequest,
    reader: bool,
    save: bool,
    downloads: &Path,
) -> Result<Fetched, String> {
    let resp = browser.fetch(req).await.map_err(|e| e.to_string())?;
    let redirects = resp.redirects.len();
    let base = |body| Fetched {
        url: resp.url.clone(),
        status: resp.status,
        protocol: resp.protocol,
        info: resp.info.clone(),
        body,
        redirects,
    };
    if let Some(other) = resp.header("x-alouette-redirect") {
        return Ok(base(Body::Elsewhere(other.to_string())));
    }
    let attachment = resp
        .header("content-disposition")
        .is_some_and(|d| d.trim_start().to_ascii_lowercase().starts_with("attachment"));
    if !save && !attachment && resp.is_html() {
        let html = resp.text();
        let page = Page::parse(&html, &resp.url);
        let rp = reader.then(|| Box::new(Page::parse_reader(&html, &resp.url)));
        return Ok(base(Body::Html { html, page: Box::new(page), reader: rp }));
    }
    let mime = resp.mime();
    let textual = mime.starts_with("text/")
        || mime.ends_with("+json")
        || mime == "application/json"
        || mime.ends_with("+xml")
        || mime == "application/xml";
    if !save
        && !attachment
        && (textual || mime.is_empty() && resp.body.len() < 1 << 20 && std::str::from_utf8(&resp.body).is_ok())
    {
        return Ok(base(Body::Text(resp.text())));
    }
    if !resp.ok() {
        return Err(format!("the server said {}", resp.status));
    }
    let path = save_body(&resp, downloads).await?;
    Ok(base(Body::Saved(path, resp.body.len())))
}

/// A file name for a download: from Content-Disposition, else the URL.
pub fn file_name(resp_url: &str, disposition: Option<&str>) -> String {
    let from_header = disposition.and_then(|d| {
        d.split(';').find_map(|p| {
            let p = p.trim();
            p.strip_prefix("filename*=")
                .map(|v| {
                    let v = v.rsplit("''").next().unwrap_or(v);
                    url::form_urlencoded::parse(format!("x={v}").as_bytes())
                        .next()
                        .map(|(_, v)| v.into_owned())
                        .unwrap_or_default()
                })
                .or_else(|| p.strip_prefix("filename=").map(|v| v.trim_matches('"').to_string()))
        })
    });
    let from_url = url::Url::parse(resp_url).ok().and_then(|u| {
        u.path_segments().and_then(|mut s| s.next_back().map(|s| s.to_string())).filter(|s| !s.is_empty())
    });
    let raw = from_header.or(from_url).unwrap_or_else(|| "download".into());
    let decoded =
        url::form_urlencoded::parse(format!("x={raw}").as_bytes()).next().map(|(_, v)| v.into_owned()).unwrap_or(raw);
    // Never let a name climb out of the downloads folder.
    let clean: String =
        decoded.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '\0') || c.is_control() { '_' } else { c }).collect();
    let clean = clean.trim_start_matches('.').trim().to_string();
    if clean.is_empty() { "download".into() } else { clean }
}

/// A path in `dir` that does not exist yet: name, name (1), name (2)…
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        return p;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    (1..10_000).map(|i| dir.join(format!("{stem} ({i}){ext}"))).find(|p| !p.exists()).unwrap_or(p)
}

async fn save_body(resp: &WebResponse, dir: &Path) -> Result<PathBuf, String> {
    tokio::fs::create_dir_all(dir).await.map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = unique_path(dir, &file_name(&resp.url, resp.header("content-disposition")));
    tokio::fs::write(&path, &resp.body).await.map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_names_are_safe() {
        assert_eq!(file_name("https://x.org/a/report%20final.pdf?x=1", None), "report final.pdf");
        assert_eq!(file_name("https://x.org/", None), "download");
        assert_eq!(file_name("https://x.org/f", Some("attachment; filename=\"../../etc/passwd\"")), "_.._etc_passwd");
        assert_eq!(file_name("https://x.org/f", Some("attachment; filename*=UTF-8''caf%C3%A9.txt")), "café.txt");
        let dir = std::env::temp_dir().join(format!("alouette-web-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "").unwrap();
        assert_eq!(unique_path(&dir, "a.txt"), dir.join("a (1).txt"));
        assert_eq!(unique_path(&dir, "b.txt"), dir.join("b.txt"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
