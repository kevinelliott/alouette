//! Reader mode: find the article in a page and leave out the rest.
//!
//! The scoring follows the well-known readability approach: paragraphs give points to the
//! elements around them, class names nudge the score, link-heavy parts lose it, and
//! siblings that look like more of the article are kept.

use std::collections::{HashMap, HashSet};

use markup5ever_rcdom::{Handle, NodeData};

use crate::convert::{attr, children, collapse, key, name, text_content};

pub(crate) struct Pick {
    pub root: Handle,
    pub skip: HashSet<usize>,
    pub title: Option<String>,
}

const UNLIKELY: &[&str] = &[
    "banner",
    "breadcrumb",
    "combx",
    "comment",
    "community",
    "cookie",
    "disqus",
    "extra",
    "footer",
    "gdpr",
    "header",
    "legends",
    "menu",
    "related",
    "remark",
    "replies",
    "rss",
    "shoutbox",
    "sidebar",
    "skyscraper",
    "social",
    "sponsor",
    "supplemental",
    "ad-break",
    "agegate",
    "pagination",
    "pager",
    "popup",
    "yom-remote",
    "share",
    "newsletter",
    "subscribe",
    "promo",
    "modal",
    "masthead",
    "toolbar",
    "skip",
    "signup",
    "navbar",
    "byline-share",
    "outbrain",
    "taboola",
];
const MAYBE: &[&str] = &["and", "article", "body", "column", "content", "main", "shadow", "post", "entry", "story"];
const POSITIVE: &[&str] =
    &["article", "body", "content", "entry", "hentry", "h-entry", "main", "page", "post", "text", "blog", "story"];
const NEGATIVE: &[&str] = &[
    "hidden",
    "banner",
    "combx",
    "comment",
    "com-",
    "contact",
    "foot",
    "footer",
    "footnote",
    "gdpr",
    "masthead",
    "media",
    "meta",
    "outbrain",
    "promo",
    "related",
    "scroll",
    "share",
    "shoutbox",
    "sidebar",
    "skyscraper",
    "sponsor",
    "shopping",
    "tags",
    "tool",
    "widget",
    "nav",
    "menu",
];

fn class_id(h: &Handle) -> String {
    format!("{} {}", attr(h, "class").unwrap_or_default(), attr(h, "id").unwrap_or_default()).to_ascii_lowercase()
}

fn class_weight(h: &Handle) -> f64 {
    let ci = class_id(h);
    let mut w = 0.0;
    if NEGATIVE.iter().any(|n| ci.contains(n)) {
        w -= 25.0;
    }
    if POSITIVE.iter().any(|p| ci.contains(p)) {
        w += 25.0;
    }
    w
}

fn base_score(h: &Handle) -> f64 {
    let tag_score = match name(h).unwrap_or("") {
        "article" => 10.0,
        "main" | "div" => 5.0,
        "pre" | "td" | "blockquote" | "section" => 3.0,
        "address" | "ol" | "ul" | "dl" | "dd" | "dt" | "li" | "form" => -3.0,
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "th" => -5.0,
        _ => 0.0,
    };
    let itemprop = attr(h, "itemprop").is_some_and(|v| v.contains("articleBody"));
    tag_score + class_weight(h) + if itemprop { 50.0 } else { 0.0 }
}

fn link_density(h: &Handle) -> f64 {
    let total = collapse(&text_content(h)).len();
    if total == 0 {
        return 0.0;
    }
    let mut links = 0;
    fn go(h: &Handle, links: &mut usize) {
        if name(h) == Some("a") {
            *links += collapse(&text_content(h)).len();
            return;
        }
        for c in h.children.borrow().iter() {
            go(c, links);
        }
    }
    go(h, &mut links);
    links as f64 / total as f64
}

fn parent(h: &Handle) -> Option<Handle> {
    let weak = h.parent.take();
    let p = weak.as_ref().and_then(|w| w.upgrade());
    h.parent.set(weak);
    p
}

fn is_unlikely(h: &Handle) -> bool {
    let tag = name(h).unwrap_or("");
    if matches!(tag, "nav" | "aside" | "footer" | "form" | "dialog") {
        return true;
    }
    if matches!(tag, "body" | "article" | "main" | "a" | "html") {
        return false;
    }
    if attr(h, "role").is_some_and(|r| {
        matches!(r.as_str(), "navigation" | "complementary" | "banner" | "contentinfo" | "dialog" | "menu" | "menubar")
    }) {
        return true;
    }
    let ci = class_id(h);
    UNLIKELY.iter().any(|u| ci.contains(u)) && !MAYBE.iter().any(|m| ci.contains(m))
}

fn has_block_child(h: &Handle) -> bool {
    children(h).iter().any(|c| {
        matches!(
            name(c),
            Some(
                "p" | "div"
                    | "section"
                    | "article"
                    | "table"
                    | "ul"
                    | "ol"
                    | "pre"
                    | "blockquote"
                    | "h1"
                    | "h2"
                    | "h3"
                    | "h4"
                    | "figure"
                    | "dl"
            )
        )
    })
}

pub(crate) fn pick(body: &Handle, page_title: &str) -> Option<Pick> {
    let mut skip = HashSet::new();
    let mut paras = vec![];
    let mut h1s = vec![];
    fn walk(h: &Handle, skip: &mut HashSet<usize>, paras: &mut Vec<Handle>, h1s: &mut Vec<Handle>) {
        if !matches!(h.data, NodeData::Element { .. } | NodeData::Document) {
            return;
        }
        if is_unlikely(h) {
            skip.insert(key(h));
            return;
        }
        match name(h) {
            Some("p" | "pre" | "td") => paras.push(h.clone()),
            Some("div" | "section") if !has_block_child(h) => paras.push(h.clone()),
            Some("h1") => h1s.push(h.clone()),
            _ => {}
        }
        for c in children(h) {
            walk(&c, skip, paras, h1s);
        }
    }
    walk(body, &mut skip, &mut paras, &mut h1s);

    let mut scores: HashMap<usize, (f64, Handle)> = HashMap::new();
    for p in &paras {
        let text = collapse(&text_content(p));
        if text.len() < 25 {
            continue;
        }
        let score = 1.0 + text.matches([',', '，', '、']).count() as f64 + (text.len() as f64 / 100.0).min(3.0);
        let mut node = parent(p);
        for level in 0..3 {
            let Some(n) = node else { break };
            if name(&n).is_none() {
                break;
            }
            let entry = scores.entry(key(&n)).or_insert_with(|| (base_score(&n), n.clone()));
            entry.0 += score / [1.0, 2.0, 3.0][level];
            node = parent(&n);
        }
    }
    let mut best: Option<(f64, Handle)> = None;
    for (score, h) in scores.values() {
        let s = score * (1.0 - link_density(h));
        if best.as_ref().is_none_or(|(b, _)| s > *b) {
            best = Some((s, h.clone()));
        }
    }
    let (top_score, top) = best?;
    if matches!(name(&top), Some("body" | "html")) || top_score < 20.0 {
        return None;
    }
    if collapse(&text_content(&top)).len() < 250 {
        return None;
    }

    // Keep siblings that look like more of the article.
    let threshold = (top_score * 0.2).max(10.0);
    let mut root = top.clone();
    if let Some(par) = parent(&top)
        && !matches!(name(&par), Some("html") | None)
    {
        let mut extra = 0;
        let mut drop = vec![];
        for sib in children(&par) {
            if key(&sib) == key(&top) {
                continue;
            }
            if name(&sib).is_none() {
                continue;
            }
            let score = scores.get(&key(&sib)).map(|s| s.0 * (1.0 - link_density(&sib))).unwrap_or(0.0);
            let text = collapse(&text_content(&sib));
            let ld = link_density(&sib);
            let keep = score >= threshold
                || (name(&sib) == Some("p") && (text.len() > 80 && ld < 0.25 || ld == 0.0 && text.contains(". ")));
            if keep {
                extra += 1;
            } else {
                drop.push(key(&sib));
            }
        }
        if extra > 0 {
            root = par;
            skip.extend(drop);
        }
    }

    // A title: the h1 that the page title names, else the article's own h1.
    let t = page_title.to_lowercase();
    let title = h1s
        .iter()
        .map(|h| collapse(&text_content(h)))
        .find(|h| !h.is_empty() && t.contains(&h.to_lowercase()))
        .or_else(|| {
            let mut own = vec![];
            fn first_h1(h: &Handle, out: &mut Vec<String>) {
                if name(h) == Some("h1") {
                    out.push(collapse(&text_content(h)));
                }
                for c in h.children.borrow().iter() {
                    first_h1(c, out);
                }
            }
            first_h1(&root, &mut own);
            own.into_iter().find(|s| !s.is_empty())
        });
    Some(Pick { root, skip, title })
}
