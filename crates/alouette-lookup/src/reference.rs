//! Web, reference, archive and library sources: Wikipedia, the Wayback Machine, arXiv,
//! Crossref, SearXNG, OpenSearch engines and library catalogues over SRU.

use alouette_net::Net;
use serde_json::Value;

use crate::sources::urlenc;
use crate::{Out, Section, Style};

const MAX: usize = 4 * 1024 * 1024;

async fn get_json(net: &Net, url: &str) -> Result<Value, String> {
    let r = net.get(url, &[("Accept", "application/json")], MAX).await.map_err(|e| e.to_string())?;
    if !r.ok() {
        return Err(format!("HTTP {}", r.status));
    }
    serde_json::from_slice(&r.body).map_err(|e| format!("unexpected answer: {e}"))
}

async fn get_text(net: &Net, url: &str, accept: &str) -> Result<String, String> {
    let r = net.get(url, &[("Accept", accept)], MAX).await.map_err(|e| e.to_string())?;
    if !r.ok() {
        return Err(format!("HTTP {}", r.status));
    }
    Ok(r.text())
}

fn wrap(s: &str, width: usize) -> String {
    let mut out = String::new();
    let mut col = 0;
    for w in s.split_whitespace() {
        if col > 0 && col + 1 + w.chars().count() > width {
            out.push('\n');
            col = 0;
        } else if col > 0 {
            out.push(' ');
            col += 1;
        }
        out.push_str(w);
        col += w.chars().count();
    }
    out
}

fn clip(s: &str, n: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= n { s } else { format!("{}…", s.chars().take(n - 1).collect::<String>()) }
}

// ------------------------------------------------------------------------- Wikipedia

/// Search Wikipedia and summarise the best match.
pub async fn wikipedia(net: &Net, lang: &str, words: &str) -> Section {
    let lang = if lang.is_empty() { "en" } else { lang };
    let title = format!("Wikipedia: {words}");
    let api = format!(
        "https://{lang}.wikipedia.org/w/api.php?action=query&list=search&format=json&srlimit=8&srprop=snippet&srsearch={}",
        urlenc(words)
    );
    let v = match get_json(net, &api).await {
        Ok(v) => v,
        Err(e) => return Section::error("Wikipedia", &title, e),
    };
    let hits = parse_wikipedia_search(&v);
    if hits.is_empty() {
        return Section::nothing("Wikipedia", &title, "no articles");
    }
    let mut s = Section::new("Wikipedia", title);
    // The first article's summary.
    let first = &hits[0];
    let sum_url =
        format!("https://{lang}.wikipedia.org/api/rest_v1/page/summary/{}", urlenc(&first.0.replace(' ', "_")));
    if let Ok(sum) = get_json(net, &sum_url).await
        && let Some(extract) = sum.get("extract").and_then(Value::as_str)
    {
        s.text(Style::Heading, first.0.clone());
        if let Some(d) = sum.get("description").and_then(Value::as_str) {
            s.text(Style::Dim, d.to_string());
        }
        s.block(&wrap(extract, 96), 12);
    }
    for (t, snippet) in &hits {
        let url = format!("https://{lang}.wikipedia.org/wiki/{}", urlenc(&t.replace(' ', "_")));
        let label = if snippet.is_empty() { t.clone() } else { format!("{t}: {}", clip(snippet, 90)) };
        s.push(Out::link(label, url));
    }
    s
}

/// (title, snippet as plain text) from a search answer.
pub fn parse_wikipedia_search(v: &Value) -> Vec<(String, String)> {
    v.pointer("/query/search")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|h| {
                    let t = h.get("title")?.as_str()?.to_string();
                    let sn = h.get("snippet").and_then(Value::as_str).unwrap_or("");
                    Some((t, strip_tags(sn)))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&quot;", "\"").replace("&amp;", "&").replace("&#039;", "'").replace("&lt;", "<").replace("&gt;", ">")
}

// ------------------------------------------------------------------------- Wayback

/// The Wayback Machine's copies of a page or site.
pub async fn wayback(net: &Net, target: &str) -> Section {
    let title = format!("Archived copies of {target}");
    let cdx = format!(
        "https://web.archive.org/cdx/search/cdx?url={}&output=json&fl=timestamp,original,statuscode&collapse=timestamp:6&limit=-12",
        urlenc(target)
    );
    let v = match get_json(net, &cdx).await {
        Ok(v) => v,
        Err(e) => return Section::error("Wayback", &title, e),
    };
    let rows = parse_cdx(&v);
    if rows.is_empty() {
        let mut s = Section::nothing("Wayback", &title, "never archived");
        s.push(Out::link("Ask the Wayback Machine to save it", format!("https://web.archive.org/save/{target}")));
        return s;
    }
    let mut s = Section::new("Wayback", title);
    let first = format!("https://web.archive.org/web/0/{target}");
    s.push(Out::link("Oldest copy", first));
    s.push(Out::link("Newest copy", format!("https://web.archive.org/web/{target}")));
    s.text(Style::Heading, "Recent months");
    for (ts, orig, status) in rows.iter().rev() {
        let when = if ts.len() >= 8 { format!("{}-{}-{}", &ts[0..4], &ts[4..6], &ts[6..8]) } else { ts.clone() };
        s.push(Out::link(format!("{when}  {status}  {orig}"), format!("https://web.archive.org/web/{ts}/{orig}")));
    }
    s
}

/// Rows of (timestamp, original URL, status) from a CDX JSON answer, header skipped.
pub fn parse_cdx(v: &Value) -> Vec<(String, String, String)> {
    v.as_array()
        .map(|rows| {
            rows.iter()
                .skip(1)
                .filter_map(|r| {
                    let r = r.as_array()?;
                    Some((
                        r.first()?.as_str()?.to_string(),
                        r.get(1)?.as_str()?.to_string(),
                        r.get(2).and_then(Value::as_str).unwrap_or("").to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

// ------------------------------------------------------------------------- arXiv

/// The arXiv API URL for a query: an id, or words to search.
pub fn arxiv_url(words: &str) -> String {
    let w = words.trim();
    let id = w.strip_prefix("arXiv:").or_else(|| w.strip_prefix("arxiv:")).unwrap_or(w);
    if id.len() >= 9 && id.chars().next().is_some_and(|c| c.is_ascii_digit()) && id.contains('.') && !id.contains(' ') {
        return format!("https://export.arxiv.org/api/query?id_list={}", urlenc(id));
    }
    format!(
        "https://export.arxiv.org/api/query?search_query=all:{}&sortBy=relevance&max_results=10",
        urlenc(&w.split_whitespace().collect::<Vec<_>>().join(" AND "))
    )
}

/// Papers on arXiv. The feed URL can be followed as a saved search.
pub async fn arxiv(net: &Net, words: &str) -> Section {
    let title = format!("arXiv: {words}");
    let url = arxiv_url(words);
    let body = match get_text(net, &url, "application/atom+xml").await {
        Ok(b) => b,
        Err(e) => return Section::error("arXiv", &title, e),
    };
    let feed = match alouette_feeds::parse(body.as_bytes(), url::Url::parse(&url).ok().as_ref()) {
        Ok(f) => f,
        Err(e) => return Section::error("arXiv", &title, e),
    };
    let items: Vec<_> = feed.items.iter().filter(|i| !i.title.trim().eq_ignore_ascii_case("error")).collect();
    if items.is_empty() {
        return Section::nothing("arXiv", &title, "no papers");
    }
    let mut s = Section::new("arXiv", title);
    for it in items.iter().take(10) {
        let year = it
            .date
            .and_then(|d| time::OffsetDateTime::from_unix_timestamp(d).ok())
            .map(|d| d.year().to_string())
            .unwrap_or_default();
        let who = it.author.clone().unwrap_or_default();
        let label =
            format!("{} ({}{}{})", clip(&it.title, 90), clip(&who, 30), if who.is_empty() { "" } else { ", " }, year);
        match &it.link {
            Some(l) => s.push(Out::link(label, l.clone())),
            None => s.text(Style::Normal, label),
        };
    }
    s.push(Out::link("Follow this search as a feed", format!("feed:{url}")));
    s
}

// ------------------------------------------------------------------------- Crossref

/// Whether text looks like a DOI.
pub fn doi(s: &str) -> Option<String> {
    let s = s.trim().trim_start_matches("https://doi.org/").trim_start_matches("doi:");
    (s.starts_with("10.") && s.contains('/') && !s.contains(' ')).then(|| s.to_string())
}

/// Scholarly works from Crossref, or one work by DOI.
pub async fn crossref(net: &Net, words: &str) -> Section {
    let title = format!("Crossref: {words}");
    let url = match doi(words) {
        Some(d) => format!("https://api.crossref.org/works/{}", urlenc(&d)),
        None => format!(
            "https://api.crossref.org/works?rows=8&select=DOI,title,author,issued,container-title,type&query.bibliographic={}",
            urlenc(words)
        ),
    };
    let v = match get_json(net, &url).await {
        Ok(v) => v,
        Err(e) => return Section::error("Crossref", &title, e),
    };
    let works = parse_crossref(&v);
    if works.is_empty() {
        return Section::nothing("Crossref", &title, "no works");
    }
    let mut s = Section::new("Crossref", title);
    for w in works {
        s.push(Out::link(w.label(), format!("https://doi.org/{}", w.doi)));
    }
    s
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Work {
    pub doi: String,
    pub title: String,
    pub authors: Vec<String>,
    pub year: Option<i64>,
    pub venue: String,
}

impl Work {
    pub fn label(&self) -> String {
        let who = match self.authors.len() {
            0 => String::new(),
            1 => self.authors[0].clone(),
            2 => format!("{} and {}", self.authors[0], self.authors[1]),
            _ => format!("{} et al.", self.authors[0]),
        };
        let mut l = clip(&self.title, 90);
        let mut extra = vec![];
        if !who.is_empty() {
            extra.push(who);
        }
        if let Some(y) = self.year {
            extra.push(y.to_string());
        }
        if !self.venue.is_empty() {
            extra.push(clip(&self.venue, 40));
        }
        if !extra.is_empty() {
            l.push_str(&format!(" ({})", extra.join(", ")));
        }
        l
    }
}

pub fn parse_crossref(v: &Value) -> Vec<Work> {
    let msg = &v["message"];
    let items: Vec<&Value> = match msg.get("items").and_then(Value::as_array) {
        Some(a) => a.iter().collect(),
        None if msg.get("DOI").is_some() => vec![msg],
        None => vec![],
    };
    items
        .into_iter()
        .filter_map(|w| {
            let doi = w.get("DOI")?.as_str()?.to_string();
            let title = w["title"].get(0).and_then(Value::as_str).unwrap_or("(untitled)").to_string();
            let authors = w["author"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|p| p.get("family").or(p.get("name")).and_then(Value::as_str).map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let year = w.pointer("/issued/date-parts/0/0").and_then(Value::as_i64);
            let venue = w["container-title"].get(0).and_then(Value::as_str).unwrap_or("").to_string();
            Some(Work { doi, title: strip_tags(&title), authors, year, venue })
        })
        .collect()
}

// ------------------------------------------------------------------------- SearXNG

/// Results from a SearXNG instance (which must allow the JSON format).
pub async fn searxng(net: &Net, instance: &str, words: &str) -> Section {
    let title = format!("SearXNG: {words}");
    let base = instance.trim_end_matches('/');
    let url = format!("{base}/search?format=json&q={}", urlenc(words));
    let v = match get_json(net, &url).await {
        Ok(v) => v,
        Err(e) => return Section::error("SearXNG", &title, e),
    };
    let results = v["results"].as_array().cloned().unwrap_or_default();
    if results.is_empty() {
        return Section::nothing("SearXNG", &title, "no results");
    }
    let mut s = Section::new("SearXNG", title);
    for r in results.iter().take(12) {
        let (Some(t), Some(u)) = (r["title"].as_str(), r["url"].as_str()) else { continue };
        s.push(Out::link(clip(t, 90), u.to_string()));
        if let Some(c) = r["content"].as_str().filter(|c| !c.is_empty()) {
            s.text(Style::Dim, format!("  {}", clip(c, 110)));
        }
    }
    s.push(Out::link("All results", format!("{base}/search?q={}", urlenc(words))));
    s
}

// ------------------------------------------------------------------------- OpenSearch

/// Fill an OpenSearch URL template.
pub fn opensearch_url(template: &str, words: &str) -> String {
    template
        .replace("{searchTerms}", &urlenc(words))
        .replace("{count?}", "10")
        .replace("{startIndex?}", "")
        .replace("{startPage?}", "")
        .replace("{language?}", "")
        .replace("{inputEncoding?}", "UTF-8")
        .replace("{outputEncoding?}", "UTF-8")
}

/// Any engine with an OpenSearch RSS or Atom template.
pub async fn opensearch(net: &Net, name: &str, template: &str, words: &str) -> Section {
    let title = format!("{name}: {words}");
    let url = opensearch_url(template, words);
    let body = match get_text(net, &url, "application/rss+xml, application/atom+xml, */*;q=0.5").await {
        Ok(b) => b,
        Err(e) => return Section::error(name, &title, e),
    };
    let feed = match alouette_feeds::parse(body.as_bytes(), url::Url::parse(&url).ok().as_ref()) {
        Ok(f) => f,
        Err(e) => return Section::error(name, &title, e),
    };
    if feed.items.is_empty() {
        return Section::nothing(name, &title, "no results");
    }
    let mut s = Section::new(name, title);
    for it in feed.items.iter().take(12) {
        match &it.link {
            Some(l) => s.push(Out::link(clip(&it.title, 100), l.clone())),
            None => s.text(Style::Normal, clip(&it.title, 100)),
        };
    }
    s.push(Out::link("Follow this search as a feed", format!("feed:{url}")));
    s
}

// ------------------------------------------------------------------------- SRU

/// Library catalogue records over SRU, in Dublin Core.
pub async fn sru(net: &Net, endpoint: &str, words: &str) -> Section {
    let title = format!("Library catalogue: {words}");
    let sep = if endpoint.contains('?') { '&' } else { '?' };
    let cql = words.split_whitespace().collect::<Vec<_>>().join(" and ");
    let url = format!(
        "{endpoint}{sep}version=1.1&operation=searchRetrieve&recordSchema=dc&maximumRecords=8&query={}",
        urlenc(&cql)
    );
    let body = match get_text(net, &url, "application/xml, text/xml").await {
        Ok(b) => b,
        Err(e) => return Section::error("SRU", &title, e),
    };
    let (total, records) = parse_sru(&body);
    if records.is_empty() {
        return Section::nothing("SRU", &title, "no records");
    }
    let mut s = Section::new("SRU", title);
    if let Some(n) = total {
        s.field("Records", n);
    }
    for r in records {
        let mut label = clip(&r.title, 90);
        let mut extra = vec![];
        if let Some(c) = r.creator {
            extra.push(clip(&c, 40));
        }
        if let Some(d) = r.date {
            extra.push(d);
        }
        if !extra.is_empty() {
            label.push_str(&format!(" ({})", extra.join(", ")));
        }
        match r.link {
            Some(l) => s.push(Out::link(label, l)),
            None => s.text(Style::Normal, label),
        };
    }
    s
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Record {
    pub title: String,
    pub creator: Option<String>,
    pub date: Option<String>,
    pub link: Option<String>,
}

/// Total hits and Dublin Core records from an SRU answer. Namespaces are ignored.
pub fn parse_sru(xml: &str) -> (Option<u64>, Vec<Record>) {
    fn tags<'a>(xml: &'a str, local: &str) -> Vec<&'a str> {
        let mut out = vec![];
        let mut rest = xml;
        while let Some(i) = rest.find('<') {
            rest = &rest[i + 1..];
            let name_end = rest.find(|c: char| c == '>' || c.is_whitespace() || c == '/').unwrap_or(rest.len());
            let name = &rest[..name_end];
            let bare = name.rsplit(':').next().unwrap_or(name);
            if bare != local || name.starts_with('/') {
                continue;
            }
            let Some(gt) = rest.find('>') else { break };
            if rest[..gt].ends_with('/') {
                continue;
            }
            let body = &rest[gt + 1..];
            let close = format!("</{name}>");
            if let Some(end) = body.find(&close) {
                out.push(&body[..end]);
                rest = &body[end..];
            }
        }
        out
    }
    let unescape = |s: &str| {
        s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
    };
    let total = tags(xml, "numberOfRecords").first().and_then(|n| n.trim().parse().ok());
    let records = tags(xml, "record")
        .into_iter()
        .filter_map(|r| {
            let title = tags(r, "title").first().map(|t| unescape(t.trim()))?;
            let creator = tags(r, "creator").first().map(|t| unescape(t.trim()));
            let date = tags(r, "date").first().map(|t| unescape(t.trim()));
            let link = tags(r, "identifier").into_iter().map(|t| unescape(t.trim())).find(|i| i.starts_with("http"));
            Some(Record { title, creator, date, link })
        })
        .collect();
    (total, records)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_answers() {
        let v: Value = serde_json::from_str(
            r#"{"query":{"search":[{"title":"Minuet (software)","snippet":"the <span class=\"searchmatch\">Minnesota</span> Internet &quot;Users&quot;"}]}}"#,
        )
        .unwrap();
        assert_eq!(
            parse_wikipedia_search(&v),
            [("Minuet (software)".to_string(), "the Minnesota Internet \"Users\"".to_string())]
        );

        let v: Value = serde_json::from_str(
            r#"[["timestamp","original","statuscode"],["20010401000000","http://minuet.example/","200"]]"#,
        )
        .unwrap();
        assert_eq!(parse_cdx(&v), [("20010401000000".into(), "http://minuet.example/".into(), "200".into())]);

        let v: Value = serde_json::from_str(
            r#"{"message":{"items":[{"DOI":"10.1000/x","title":["A <i>Paper</i>"],"author":[{"family":"Lovelace"},{"family":"Babbage"},{"name":"Team"}],"issued":{"date-parts":[[1843]]},"container-title":["Notes"]}]}}"#,
        )
        .unwrap();
        let w = parse_crossref(&v);
        assert_eq!(w[0].label(), "A Paper (Lovelace et al., 1843, Notes)");
        let one: Value = serde_json::from_str(r#"{"message":{"DOI":"10.1/y","title":["T"]}}"#).unwrap();
        assert_eq!(parse_crossref(&one)[0].doi, "10.1/y");
        assert_eq!(doi("https://doi.org/10.1145/3290605.3300234").as_deref(), Some("10.1145/3290605.3300234"));
        assert_eq!(doi("ten things"), None);
    }

    #[test]
    fn arxiv_and_opensearch_urls() {
        assert_eq!(arxiv_url("arXiv:2401.01234"), "https://export.arxiv.org/api/query?id_list=2401.01234");
        assert!(arxiv_url("text user interface").contains("search_query=all:text%20AND%20user%20AND%20interface"));
        assert_eq!(opensearch_url("https://x.org/s?q={searchTerms}&n={count?}", "a b"), "https://x.org/s?q=a%20b&n=10");
    }

    #[test]
    fn parses_sru_dublin_core() {
        let xml = r#"<zs:searchRetrieveResponse xmlns:zs="http://www.loc.gov/zing/srw/"><zs:numberOfRecords>42</zs:numberOfRecords>
        <zs:records><zs:record><zs:recordData><srw_dc:dc xmlns:dc="http://purl.org/dc/elements/1.1/">
        <dc:title>Gopher &amp; friends</dc:title><dc:creator>Anklesaria, Farhad</dc:creator><dc:date>1993</dc:date>
        <dc:identifier>https://lccn.loc.gov/93000001</dc:identifier></srw_dc:dc></zs:recordData></zs:record>
        <zs:record><zs:recordData><dc><title>Second</title></dc></zs:recordData></zs:record></zs:records></zs:searchRetrieveResponse>"#;
        let (n, r) = parse_sru(xml);
        assert_eq!(n, Some(42));
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].title, "Gopher & friends");
        assert_eq!(r[0].creator.as_deref(), Some("Anklesaria, Farhad"));
        assert_eq!(r[0].link.as_deref(), Some("https://lccn.loc.gov/93000001"));
        assert_eq!(r[1].title, "Second");
    }
}
