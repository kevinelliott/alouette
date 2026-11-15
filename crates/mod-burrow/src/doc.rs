//! One document model for every protocol Burrow speaks, and its layout into screen rows.

use proto_gemini::gemtext;
use proto_gopher::{Item, ItemType};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Text,
    Heading(u8),
    /// A link; the tag is a short type label such as `DIR` (Gopher) or empty (Gemini).
    Link(&'static str),
    Quote,
    ListItem,
    Pre,
    Info,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocLine {
    pub kind: Kind,
    pub text: String,
    pub link: Option<String>,
}

impl DocLine {
    fn new(kind: Kind, text: impl Into<String>) -> Self {
        Self { kind, text: text.into(), link: None }
    }
}

/// How the page is laid out and navigated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    /// A list of items: the arrow keys move between links.
    Menu,
    /// Running text: the arrow keys scroll, Tab moves between links.
    Page,
}

#[derive(Debug, Clone)]
pub struct Doc {
    pub title: String,
    pub lines: Vec<DocLine>,
    pub flavor: Flavor,
    /// The raw text, for saving.
    pub source: String,
}

impl Doc {
    pub fn links(&self) -> impl Iterator<Item = usize> + '_ {
        self.lines.iter().enumerate().filter(|(_, l)| l.link.is_some()).map(|(i, _)| i)
    }

    pub fn gopher_menu(title: &str, text: &str, tls: bool) -> Self {
        let lines = proto_gopher::parse_menu(text)
            .into_iter()
            .map(|it: Item| match it.kind {
                ItemType::Info => DocLine::new(Kind::Info, it.display),
                ItemType::Error => DocLine::new(Kind::Error, it.display),
                k => DocLine { kind: Kind::Link(k.label()), link: it.url(tls), text: it.display },
            })
            .collect();
        Self { title: title.to_string(), lines, flavor: Flavor::Menu, source: text.to_string() }
    }

    pub fn plain(title: &str, text: &str) -> Self {
        let lines = text.lines().map(|l| DocLine::new(Kind::Pre, expand_tabs(l))).collect();
        Self { title: title.to_string(), lines, flavor: Flavor::Page, source: text.to_string() }
    }

    pub fn gemtext(base: &Url, text: &str) -> Self {
        let parsed = gemtext::parse(text);
        let title = gemtext::title(&parsed).map(str::to_string).unwrap_or_else(|| host_title(base));
        let mut lines = vec![];
        for l in parsed {
            match l {
                gemtext::Line::Text(t) => lines.push(DocLine::new(Kind::Text, t)),
                gemtext::Line::Heading { level, text } => lines.push(DocLine::new(Kind::Heading(level), text)),
                gemtext::Line::ListItem(t) => lines.push(DocLine::new(Kind::ListItem, t)),
                gemtext::Line::Quote(t) => lines.push(DocLine::new(Kind::Quote, t)),
                gemtext::Line::Link { url, label } => {
                    let target = base.join(&url).map(|u| u.to_string()).unwrap_or(url.clone());
                    let tag = scheme_tag(&target);
                    lines.push(DocLine { kind: Kind::Link(tag), text: label.unwrap_or(url), link: Some(target) });
                }
                gemtext::Line::Preformatted { lines: pre, .. } => {
                    lines.extend(pre.into_iter().map(|p| DocLine::new(Kind::Pre, expand_tabs(&p))));
                }
            }
        }
        Self { title, lines, flavor: Flavor::Page, source: text.to_string() }
    }

    pub fn message(title: &str, kind: Kind, body: &[&str]) -> Self {
        let mut lines = vec![DocLine::new(Kind::Heading(1), title)];
        lines.push(DocLine::new(Kind::Text, ""));
        lines.extend(body.iter().map(|b| DocLine::new(kind.clone(), *b)));
        Self { title: title.to_string(), lines, flavor: Flavor::Page, source: body.join("\n") }
    }

    pub fn with_link(mut self, label: &str, url: &str) -> Self {
        self.lines.push(DocLine { kind: Kind::Link(""), text: label.to_string(), link: Some(url.to_string()) });
        self
    }
}

/// A short tag for links that leave Burrow, so readers know before they follow them.
fn scheme_tag(url: &str) -> &'static str {
    match url.split(':').next().unwrap_or("") {
        "gemini" => "",
        "gopher" | "gophers" => "GPH",
        "http" | "https" => "WWW",
        "finger" => "FNG",
        "mailto" => "MAIL",
        "telnet" | "ssh" => "TEL",
        _ => "EXT",
    }
}

pub fn host_title(url: &Url) -> String {
    let host = url.host_str().unwrap_or("");
    let path = url.path();
    if path.is_empty() || path == "/" { host.to_string() } else { format!("{host}{path}") }
}

fn expand_tabs(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut col = 0;
    for c in s.chars() {
        if c == '\t' {
            let n = 8 - col % 8;
            out.extend(std::iter::repeat_n(' ', n));
            col += n;
        } else {
            out.push(c);
            col += c.width().unwrap_or(0);
        }
    }
    out
}

/// One screen row: which document line it belongs to, its prefix and its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub line: usize,
    pub prefix: String,
    pub text: String,
}

/// Lay a document out for a width: wrap running text with a hanging indent, keep
/// preformatted text as is (clipped by the renderer).
pub fn layout(doc: &Doc, width: usize) -> Vec<Row> {
    let width = width.max(10);
    let mut rows = vec![];
    for (i, l) in doc.lines.iter().enumerate() {
        let (first, rest): (String, String) = match &l.kind {
            Kind::Link(tag) if doc.flavor == Flavor::Menu => (format!(" {tag:<4} "), "      ".into()),
            Kind::Info | Kind::Error if doc.flavor == Flavor::Menu => ("      ".into(), "      ".into()),
            Kind::Link("") => ("→ ".into(), "  ".into()),
            Kind::Link(tag) => (format!("→ {tag} "), " ".repeat(tag.width() + 3)),
            Kind::ListItem => ("• ".into(), "  ".into()),
            Kind::Quote => ("│ ".into(), "│ ".into()),
            _ => (String::new(), String::new()),
        };
        if matches!(l.kind, Kind::Pre) || (doc.flavor == Flavor::Menu && matches!(l.kind, Kind::Info)) {
            rows.push(Row { line: i, prefix: first, text: l.text.clone() });
            continue;
        }
        let avail = width.saturating_sub(first.width()).max(4);
        let wrapped = wrap(&l.text, avail);
        for (n, w) in wrapped.into_iter().enumerate() {
            rows.push(Row { line: i, prefix: if n == 0 { first.clone() } else { rest.clone() }, text: w });
        }
    }
    rows
}

/// Word wrap by display width. Words longer than the width are broken.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut out = vec![];
    let mut line = String::new();
    let mut w = 0;
    for word in text.split(' ') {
        let ww = word.width();
        if w > 0 && w + 1 + ww > width {
            out.push(std::mem::take(&mut line));
            w = 0;
        }
        if ww > width {
            for c in word.chars() {
                let cw = c.width().unwrap_or(0);
                if w + cw > width {
                    out.push(std::mem::take(&mut line));
                    w = 0;
                }
                line.push(c);
                w += cw;
            }
            continue;
        }
        if w > 0 {
            line.push(' ');
            w += 1;
        }
        line.push_str(word);
        w += ww;
    }
    out.push(line);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_words() {
        assert_eq!(wrap("the quick brown fox", 9), vec!["the quick", "brown fox"]);
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap("", 5), vec![""]);
    }

    #[test]
    fn gemtext_links_resolve_against_the_base() {
        let base = Url::parse("gemini://example.org/dir/page.gmi").unwrap();
        let d = Doc::gemtext(&base, "# Hello\n=> other.gmi Other\n=> https://w.example/ Web\n=> gopher://g/ \n");
        assert_eq!(d.title, "Hello");
        let links: Vec<_> = d.links().map(|i| d.lines[i].link.clone().unwrap()).collect();
        assert_eq!(links, vec!["gemini://example.org/dir/other.gmi", "https://w.example/", "gopher://g/"]);
        assert_eq!(d.lines[2].kind, Kind::Link("WWW"));
        assert_eq!(d.lines[3].text, "gopher://g/");
    }

    #[test]
    fn menu_layout_has_type_column() {
        let d = Doc::gopher_menu("t", "iHello\t\t\t0\r\n1Dir\t/d\th\t70\r\n", false);
        let rows = layout(&d, 40);
        assert_eq!(rows[0].prefix, "      ");
        assert_eq!(rows[1].prefix, " DIR  ");
        assert_eq!(d.flavor, Flavor::Menu);
    }
}
