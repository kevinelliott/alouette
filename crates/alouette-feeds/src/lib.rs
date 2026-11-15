//! Feeds: RSS 0.9x/2.0, RSS 1.0 (RDF), Atom 1.0 and JSON Feed 1.x, read into one shape.
//! Also OPML lists of feeds, and finding a site's feed from its HTML.

pub mod xml;

use serde::{Deserialize, Serialize};
use url::Url;
use xml::Node;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Feed {
    pub title: String,
    /// The site the feed belongs to.
    pub link: Option<String>,
    pub description: String,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// A stable id: the guid, Atom id, JSON id, or else the link or title.
    pub id: String,
    pub title: String,
    pub link: Option<String>,
    /// Seconds since 1970.
    pub date: Option<i64>,
    pub author: Option<String>,
    /// HTML or plain text, the longest the feed gives (content over summary).
    pub body: String,
    /// Podcast audio and the like: (url, mime type, bytes).
    pub enclosures: Vec<(String, String, u64)>,
}

/// Parse a feed of any supported kind. `base` resolves relative links.
pub fn parse(body: &[u8], base: Option<&Url>) -> Result<Feed, String> {
    let text = String::from_utf8_lossy(body);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    let mut feed = if trimmed.starts_with('{') {
        json(trimmed)?
    } else {
        let root = xml::parse(trimmed)?;
        match root.name.as_str() {
            "rss" => rss(root.child("channel").ok_or("an RSS feed with no channel")?, Some(&root)),
            "RDF" => rss(root.child("channel").unwrap_or(&root), Some(&root)),
            "feed" => atom(&root),
            other => return Err(format!("not a feed (the document is <{other}>)")),
        }
    };
    if let Some(b) = base {
        let fix = |u: &mut Option<String>| {
            if let Some(s) = u
                && let Ok(abs) = b.join(s)
            {
                *s = abs.to_string();
            }
        };
        fix(&mut feed.link);
        for it in &mut feed.items {
            fix(&mut it.link);
        }
    }
    for it in &mut feed.items {
        if it.id.is_empty() {
            it.id = it.link.clone().unwrap_or_else(|| it.title.clone());
        }
        it.title = it.title.trim().to_string();
    }
    feed.title = feed.title.trim().to_string();
    Ok(feed)
}

/// RFC 2822 (RSS) or RFC 3339 (Atom) dates, and the usual near-misses.
pub fn parse_date(s: &str) -> Option<i64> {
    use time::format_description::well_known::{Rfc2822, Rfc3339};
    let s = s.trim();
    if let Ok(d) = time::OffsetDateTime::parse(s, &Rfc3339) {
        return Some(d.unix_timestamp());
    }
    if let Ok(d) = time::OffsetDateTime::parse(s, &Rfc2822) {
        return Some(d.unix_timestamp());
    }
    // Named zones RFC 2822 parsing does not take, such as "GMT" spelled out or "EST".
    let zones = [
        ("GMT", "+0000"),
        ("UTC", "+0000"),
        ("UT", "+0000"),
        ("Z", "+0000"),
        ("EST", "-0500"),
        ("EDT", "-0400"),
        ("CST", "-0600"),
        ("CDT", "-0500"),
        ("MST", "-0700"),
        ("MDT", "-0600"),
        ("PST", "-0800"),
        ("PDT", "-0700"),
    ];
    for (z, off) in zones {
        if let Some(rest) = s.strip_suffix(z) {
            let fixed = format!("{}{off}", rest);
            if let Ok(d) = time::OffsetDateTime::parse(&fixed, &Rfc2822) {
                return Some(d.unix_timestamp());
            }
        }
    }
    // A date with no time.
    let fmt = time::macros::format_description!("[year]-[month]-[day]");
    time::Date::parse(s.get(..10)?, &fmt).ok().map(|d| d.midnight().assume_utc().unix_timestamp())
}

fn rss(ch: &Node, root: Option<&Node>) -> Feed {
    // RSS 1.0 keeps items beside the channel; 2.0 inside it.
    let items: Vec<&Node> = {
        let inside: Vec<&Node> = ch.children_named("item").collect();
        if inside.is_empty() { root.map(|r| r.children_named("item").collect()).unwrap_or_default() } else { inside }
    };
    Feed {
        title: ch.text_of("title"),
        link: ch
            .children_named("link")
            .find(|l| l.ns.is_empty() || l.ns == "rss")
            .map(|l| l.text.trim().to_string())
            .filter(|s| !s.is_empty()),
        description: ch.text_of("description"),
        items: items
            .into_iter()
            .map(|i| {
                let content = i.text_of("encoded");
                let desc = i.text_of("description");
                Item {
                    id: i
                        .child("guid")
                        .map(|g| g.text.trim().to_string())
                        .or_else(|| i.attr("about").map(str::to_string))
                        .unwrap_or_default(),
                    title: i.text_of("title"),
                    link: Some(i.text_of("link")).filter(|s| !s.is_empty()),
                    date: i.child("pubDate").or_else(|| i.child("date")).and_then(|d| parse_date(&d.text)),
                    author: Some(i.text_of("creator"))
                        .filter(|s| !s.is_empty())
                        .or_else(|| Some(i.text_of("author")).filter(|s| !s.is_empty())),
                    body: if content.len() > desc.len() { content } else { desc },
                    enclosures: i
                        .children_named("enclosure")
                        .filter_map(|e| {
                            Some((
                                e.attr("url")?.to_string(),
                                e.attr("type").unwrap_or("").to_string(),
                                e.attr("length").and_then(|l| l.parse().ok()).unwrap_or(0),
                            ))
                        })
                        .collect(),
                }
            })
            .collect(),
    }
}

fn atom_link(n: &Node) -> Option<String> {
    let links: Vec<&Node> = n.children_named("link").collect();
    links
        .iter()
        .find(|l| l.attr("rel").is_none_or(|r| r == "alternate"))
        .or(links.first())
        .and_then(|l| l.attr("href"))
        .map(str::to_string)
}

fn atom(f: &Node) -> Feed {
    Feed {
        title: f.text_of("title"),
        link: atom_link(f),
        description: f.text_of("subtitle"),
        items: f
            .children_named("entry")
            .map(|e| {
                let content = e.text_of("content");
                let summary = e.text_of("summary");
                Item {
                    id: e.text_of("id"),
                    title: e.text_of("title"),
                    link: atom_link(e),
                    date: e.child("updated").or_else(|| e.child("published")).and_then(|d| parse_date(&d.text)),
                    author: e
                        .child("author")
                        .or_else(|| f.child("author"))
                        .map(|a| a.text_of("name"))
                        .filter(|s| !s.is_empty()),
                    body: if content.len() > summary.len() { content } else { summary },
                    enclosures: e
                        .children_named("link")
                        .filter(|l| l.attr("rel") == Some("enclosure"))
                        .filter_map(|l| {
                            Some((
                                l.attr("href")?.to_string(),
                                l.attr("type").unwrap_or("").to_string(),
                                l.attr("length").and_then(|x| x.parse().ok()).unwrap_or(0),
                            ))
                        })
                        .collect(),
                }
            })
            .collect(),
    }
}

fn json(text: &str) -> Result<Feed, String> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("bad JSON Feed: {e}"))?;
    if !v["version"].as_str().unwrap_or("").contains("jsonfeed.org") {
        return Err("not a JSON Feed".into());
    }
    let s = |x: &serde_json::Value| x.as_str().unwrap_or("").to_string();
    let author =
        |x: &serde_json::Value| x["authors"][0]["name"].as_str().or(x["author"]["name"].as_str()).map(str::to_string);
    Ok(Feed {
        title: s(&v["title"]),
        link: v["home_page_url"].as_str().map(str::to_string),
        description: s(&v["description"]),
        items: v["items"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|i| Item {
                        id: i["id"]
                            .as_str()
                            .map(str::to_string)
                            .or_else(|| i["id"].as_i64().map(|n| n.to_string()))
                            .unwrap_or_default(),
                        title: s(&i["title"]),
                        link: i["url"].as_str().map(str::to_string),
                        date: i["date_modified"].as_str().or(i["date_published"].as_str()).and_then(parse_date),
                        author: author(i).or_else(|| author(&v)),
                        body: i["content_html"]
                            .as_str()
                            .or(i["content_text"].as_str())
                            .or(i["summary"].as_str())
                            .unwrap_or("")
                            .to_string(),
                        enclosures: i["attachments"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|x| {
                                        Some((
                                            x["url"].as_str()?.to_string(),
                                            s(&x["mime_type"]),
                                            x["size_in_bytes"].as_u64().unwrap_or(0),
                                        ))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// One feed in an OPML list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outline {
    pub title: String,
    pub url: String,
    /// The folder it sat in, if any.
    pub folder: Option<String>,
}

/// Read an OPML subscription list, flattening folders.
pub fn parse_opml(text: &str) -> Result<Vec<Outline>, String> {
    let root = xml::parse(text)?;
    let body = root.child("body").ok_or("an OPML file with no body")?;
    let mut out = vec![];
    fn walk(n: &Node, folder: Option<&str>, out: &mut Vec<Outline>) {
        for o in n.children_named("outline") {
            let title = o.attr("title").or(o.attr("text")).unwrap_or("").to_string();
            match o.attr("xmlUrl") {
                Some(u) => out.push(Outline { title, url: u.to_string(), folder: folder.map(str::to_string) }),
                None => walk(o, Some(&title), out),
            }
        }
    }
    walk(body, None, &mut out);
    Ok(out)
}

/// Write an OPML subscription list.
pub fn write_opml(title: &str, feeds: &[Outline]) -> String {
    let esc = |s: &str| quick_xml::escape::escape(s).into_owned();
    let mut s = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<opml version=\"2.0\">\n  <head><title>{}</title></head>\n  <body>\n",
        esc(title)
    );
    for f in feeds {
        s.push_str(&format!(
            "    <outline type=\"rss\" text=\"{0}\" title=\"{0}\" xmlUrl=\"{1}\"/>\n",
            esc(&f.title),
            esc(&f.url)
        ));
    }
    s.push_str("  </body>\n</opml>\n");
    s
}

/// Feed links a web page announces with `<link rel="alternate" type="application/rss+xml">`.
pub fn discover(html: &str, base: &Url) -> Vec<(String, String)> {
    let mut out = vec![];
    let lower = html.to_ascii_lowercase();
    let mut at = 0;
    while let Some(i) = lower[at..].find("<link") {
        let start = at + i;
        let end = lower[start..].find('>').map(|e| start + e).unwrap_or(lower.len());
        let tag = &html[start..end];
        let attr = |name: &str| -> Option<String> {
            let t = tag.to_ascii_lowercase();
            let p = t.find(&format!("{name}="))? + name.len() + 1;
            let rest = &tag[p..];
            let (q, rest) = match rest.chars().next()? {
                c @ ('"' | '\'') => (c, &rest[1..]),
                _ => (' ', rest),
            };
            Some(rest.split(q).next()?.trim_end_matches('/').to_string())
        };
        let rel = attr("rel").unwrap_or_default().to_ascii_lowercase();
        let ty = attr("type").unwrap_or_default().to_ascii_lowercase();
        if rel.split_whitespace().any(|r| r == "alternate")
            && (ty.contains("rss") || ty.contains("atom") || ty.contains("feed+json"))
            && let Some(href) = attr("href")
            && let Ok(u) = base.join(&href)
        {
            out.push((u.to_string(), attr("title").unwrap_or_else(|| ty.clone())));
        }
        at = end;
    }
    out
}

#[cfg(test)]
mod tests;
