//! From the DOM to blocks.

use std::collections::HashSet;

use html5ever::tendril::TendrilSink;
use html5ever::{ParseOpts, parse_document, tree_builder::TreeBuilderOpts};
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use url::Url;

use crate::{Block, Field, FieldKind, Form, Fx, Image, Link, Page, Seg};

pub(crate) fn parse_dom(html: &str) -> RcDom {
    let opts = ParseOpts {
        // Without scripts, <noscript> holds markup, which is what we want to show.
        tree_builder: TreeBuilderOpts { scripting_enabled: false, ..Default::default() },
        ..Default::default()
    };
    parse_document(RcDom::default(), opts).one(html)
}

pub(crate) fn name(h: &Handle) -> Option<&str> {
    match &h.data {
        NodeData::Element { name, .. } => Some(&name.local),
        _ => None,
    }
}

pub(crate) fn attr(h: &Handle, key: &str) -> Option<String> {
    match &h.data {
        NodeData::Element { attrs, .. } => {
            attrs.borrow().iter().find(|a| &*a.name.local == key).map(|a| a.value.to_string())
        }
        _ => None,
    }
}

pub(crate) fn children(h: &Handle) -> Vec<Handle> {
    h.children.borrow().clone()
}

pub(crate) fn text_content(h: &Handle) -> String {
    let mut s = String::new();
    fn go(h: &Handle, s: &mut String) {
        match &h.data {
            NodeData::Text { contents } => s.push_str(&contents.borrow()),
            NodeData::Element { name, .. } if matches!(&*name.local, "script" | "style" | "template") => {}
            _ => {
                for c in h.children.borrow().iter() {
                    go(c, s);
                }
            }
        }
    }
    go(h, &mut s);
    s
}

pub(crate) fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn key(h: &Handle) -> usize {
    std::rc::Rc::as_ptr(h) as usize
}

/// Hidden from everyone, by attribute or inline style.
pub(crate) fn hidden(h: &Handle) -> bool {
    if attr(h, "hidden").is_some() || attr(h, "aria-hidden").as_deref() == Some("true") {
        return true;
    }
    attr(h, "style").is_some_and(|s| {
        let s = s.replace(' ', "").to_ascii_lowercase();
        s.contains("display:none") || s.contains("visibility:hidden")
    })
}

fn find(h: &Handle, tag: &str) -> Option<Handle> {
    if name(h) == Some(tag) {
        return Some(h.clone());
    }
    for c in h.children.borrow().iter() {
        if let Some(f) = find(c, tag) {
            return Some(f);
        }
    }
    None
}

fn find_all(h: &Handle, tag: &str, out: &mut Vec<Handle>) {
    if name(h) == Some(tag) {
        out.push(h.clone());
    }
    for c in h.children.borrow().iter() {
        find_all(c, tag, out);
    }
}

const BLOCKS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "body",
    "center",
    "details",
    "dialog",
    "dd",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hgroup",
    "hr",
    "html",
    "li",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "summary",
    "table",
    "ul",
    "tr",
    "caption",
    "legend",
    "menu",
    "search",
    "noscript",
];

const SKIP: &[&str] =
    &["script", "style", "template", "head", "svg", "math", "canvas", "map", "object", "embed", "datalist", "dialog"];

struct Cx {
    page: Page,
    base: Url,
    cur: Vec<Block>,
    inline: Vec<Seg>,
    fx: Fx,
    link: Option<usize>,
    form: Option<usize>,
    /// Inside a heading or table cell: block breaks become spaces.
    no_flush: usize,
    /// Nodes reader mode leaves out.
    skip: HashSet<usize>,
    reader: bool,
}

pub(crate) fn convert(html: &str, base: &str, reader: Option<()>) -> Page {
    let dom = parse_dom(html);
    let doc = dom.document.clone();
    let mut base_url = Url::parse(base).unwrap_or_else(|_| Url::parse("about:blank").unwrap());
    if let Some(b) = find(&doc, "base").and_then(|b| attr(&b, "href"))
        && let Ok(u) = base_url.join(&b)
    {
        base_url = u;
    }
    let mut page = Page { url: base.to_string(), ..Default::default() };
    if let Some(t) = find(&doc, "title") {
        page.title = collapse(&text_content(&t));
    }
    if let Some(h) = find(&doc, "html") {
        page.lang = attr(&h, "lang");
    }
    let mut links = vec![];
    find_all(&doc, "link", &mut links);
    for l in links {
        let rel = attr(&l, "rel").unwrap_or_default().to_ascii_lowercase();
        let ty = attr(&l, "type").unwrap_or_default().to_ascii_lowercase();
        if rel.split_whitespace().any(|r| r == "alternate")
            && (ty.contains("rss") || ty.contains("atom") || ty.contains("feed+json"))
            && let Some(href) = attr(&l, "href")
            && let Ok(u) = base_url.join(&href)
        {
            page.feeds.push((ty, u.to_string()));
        }
    }
    let mut metas = vec![];
    find_all(&doc, "meta", &mut metas);
    for m in &metas {
        if attr(m, "http-equiv").is_some_and(|v| v.eq_ignore_ascii_case("refresh"))
            && let Some(c) = attr(m, "content")
        {
            let (secs, rest) = c.split_once([';', ',']).unwrap_or((&c, ""));
            let target = rest.trim().trim_start_matches(|c: char| c.is_ascii_alphabetic() || c == '=' || c == ' ');
            let target = rest
                .trim()
                .strip_prefix("url=")
                .or_else(|| rest.trim().strip_prefix("URL="))
                .unwrap_or(target)
                .trim_matches(['\'', '"']);
            let secs = secs.trim().parse().unwrap_or(0);
            if !target.is_empty()
                && let Ok(u) = base_url.join(target)
            {
                page.refresh = Some((secs, u.to_string()));
            }
        }
    }
    let mut scripts = vec![];
    find_all(&doc, "script", &mut scripts);

    let mut cx = Cx {
        page,
        base: base_url,
        cur: vec![],
        inline: vec![],
        fx: Fx::default(),
        link: None,
        form: None,
        no_flush: 0,
        skip: HashSet::new(),
        reader: false,
    };
    let body = find(&doc, "body").unwrap_or(doc.clone());
    let mut root = body.clone();
    if reader.is_some()
        && let Some(pick) = crate::reader::pick(&body, &cx.page.title)
    {
        root = pick.root;
        cx.skip = pick.skip;
        cx.reader = true;
        if let Some(t) = pick.title {
            cx.page.title = t;
        }
    }
    cx.walk(&root);
    cx.flush();
    let mut blocks = std::mem::take(&mut cx.cur);
    if cx.reader && !cx.page.title.is_empty() {
        let starts_with_title = blocks.iter().take(3).any(|b| match b {
            Block::Heading(_, s) => {
                let h = collapse(&s.iter().map(|s| s.text.as_str()).collect::<String>()).to_lowercase();
                let t = cx.page.title.to_lowercase();
                h == t || h.starts_with(&t)
            }
            _ => false,
        });
        if !starts_with_title {
            blocks.insert(
                0,
                Block::Heading(1, vec![Seg { text: cx.page.title.clone(), fx: Fx::HEADING, ..Default::default() }]),
            );
        }
    }
    cx.page.blocks = blocks;
    let text_len: usize = collapse(&text_content(&body)).len();
    cx.page.needs_script = text_len < 400 && !scripts.is_empty();
    cx.page
}

impl Cx {
    fn abs(&self, href: &str) -> Option<String> {
        let href = href.trim();
        if href.is_empty() || href.to_ascii_lowercase().starts_with("javascript:") {
            return None;
        }
        self.base.join(href).ok().map(|u| u.to_string())
    }

    fn push_text(&mut self, s: &str) {
        let mut text = String::with_capacity(s.len());
        let mut space = self.ends_in_space();
        for c in s.chars() {
            if c.is_whitespace() {
                if !space {
                    text.push(' ');
                    space = true;
                }
            } else {
                text.push(c);
                space = false;
            }
        }
        self.push_seg(text, self.fx, self.link, None);
    }

    fn ends_in_space(&self) -> bool {
        match self.inline.last() {
            None => true,
            Some(s) => s.text.ends_with([' ', '\n']),
        }
    }

    fn push_seg(&mut self, text: String, fx: Fx, link: Option<usize>, field: Option<usize>) {
        if text.is_empty() {
            return;
        }
        if let Some(last) = self.inline.last_mut()
            && last.fx == fx
            && last.link == link
            && last.field.is_none()
            && field.is_none()
        {
            last.text.push_str(&text);
            return;
        }
        self.inline.push(Seg { text, fx, link, field, image: None });
    }

    fn take_inline(&mut self) -> Vec<Seg> {
        let mut segs = std::mem::take(&mut self.inline);
        while let Some(last) = segs.last_mut() {
            let t = last.text.trim_end().to_string();
            if t.is_empty() && last.field.is_none() {
                segs.pop();
            } else {
                last.text = t;
                break;
            }
        }
        while let Some(first) = segs.first_mut() {
            let t = first.text.trim_start().to_string();
            if t.is_empty() && first.field.is_none() {
                segs.remove(0);
            } else {
                first.text = t;
                break;
            }
        }
        segs
    }

    fn flush(&mut self) {
        if self.no_flush > 0 {
            if !self.ends_in_space() {
                self.push_seg(" ".into(), Fx::default(), None, None);
            }
            return;
        }
        let segs = self.take_inline();
        if !segs.is_empty() {
            self.cur.push(Block::Para(segs));
        }
    }

    /// Run `f` into a fresh container and return what it produced.
    fn collect(&mut self, f: impl FnOnce(&mut Self)) -> Vec<Block> {
        self.flush();
        let saved_cur = std::mem::take(&mut self.cur);
        let saved_nf = std::mem::replace(&mut self.no_flush, 0);
        f(self);
        self.flush();
        self.no_flush = saved_nf;
        std::mem::replace(&mut self.cur, saved_cur)
    }

    /// Run `f` and return its text as one run of segments.
    fn collect_inline(&mut self, f: impl FnOnce(&mut Self)) -> Vec<Seg> {
        let saved = std::mem::take(&mut self.inline);
        self.no_flush += 1;
        f(self);
        self.no_flush -= 1;
        let segs = self.take_inline();
        self.inline = saved;
        segs
    }

    fn walk_children(&mut self, h: &Handle) {
        for c in children(h) {
            self.walk(&c);
        }
    }

    fn with_fx(&mut self, h: &Handle, fx: Fx) {
        let saved = self.fx;
        self.fx = self.fx.with(fx);
        self.walk_children(h);
        self.fx = saved;
    }

    fn walk(&mut self, h: &Handle) {
        match &h.data {
            NodeData::Text { contents } => {
                let t = contents.borrow().to_string();
                self.push_text(&t);
                return;
            }
            NodeData::Document => return self.walk_children(h),
            NodeData::Element { .. } => {}
            _ => return,
        }
        let tag = name(h).unwrap_or("").to_string();
        if SKIP.contains(&tag.as_str()) || hidden(h) || self.skip.contains(&key(h)) {
            return;
        }
        match tag.as_str() {
            "br" => {
                if self.no_flush > 0 {
                    self.push_seg(" ".into(), Fx::default(), None, None);
                } else {
                    self.push_seg("\n".into(), Fx::default(), None, None);
                }
            }
            "hr" => {
                self.flush();
                if self.no_flush == 0 {
                    self.cur.push(Block::Rule);
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = tag.as_bytes()[1] - b'0';
                if self.no_flush > 0 {
                    return self.with_fx(h, Fx::BOLD);
                }
                self.flush();
                let saved = self.fx;
                self.fx = self.fx.with(Fx::HEADING);
                let segs = self.collect_inline(|cx| cx.walk_children(h));
                self.fx = saved;
                if !segs.is_empty() {
                    self.cur.push(Block::Heading(level, segs));
                }
            }
            "p" | "div" | "section" | "article" | "main" | "header" | "footer" | "nav" | "aside" | "address"
            | "center" | "hgroup" | "fieldset" | "details" | "search" | "html" | "body" | "noscript" | "figure"
            | "menu" | "legend" | "caption" => {
                self.flush();
                self.walk_children(h);
                self.flush();
            }
            "summary" => {
                self.flush();
                self.with_fx(h, Fx::BOLD);
                self.flush();
            }
            "figcaption" => {
                self.flush();
                self.with_fx(h, Fx::ITALIC.with(Fx::DIM));
                self.flush();
            }
            "pre" | "listing" | "xmp" | "plaintext" => {
                if self.no_flush > 0 {
                    return self.with_fx(h, Fx::CODE);
                }
                self.flush();
                let t = text_content(h);
                let t = t.strip_prefix('\n').unwrap_or(&t).trim_end().to_string();
                if !t.is_empty() {
                    self.cur.push(Block::Pre(t.replace('\t', "    ")));
                }
            }
            "blockquote" => {
                if self.no_flush > 0 {
                    return self.walk_children(h);
                }
                let inner = self.collect(|cx| cx.walk_children(h));
                if !inner.is_empty() {
                    self.cur.push(Block::Quote(inner));
                }
            }
            "ul" | "ol" | "dir" => {
                if self.no_flush > 0 {
                    return self.walk_children(h);
                }
                let ordered = tag == "ol";
                let start = attr(h, "start").and_then(|s| s.parse().ok()).unwrap_or(1);
                let mut items = vec![];
                self.flush();
                let mut stray = vec![];
                for c in children(h) {
                    if name(&c) == Some("li") {
                        if !stray.is_empty() {
                            let s = std::mem::take(&mut stray);
                            let b = self.collect(|cx| s.iter().for_each(|n| cx.walk(n)));
                            if !b.is_empty() {
                                items.push(b);
                            }
                        }
                        if hidden(&c) || self.skip.contains(&key(&c)) {
                            continue;
                        }
                        items.push(self.collect(|cx| cx.walk_children(&c)));
                    } else {
                        stray.push(c);
                    }
                }
                if !stray.is_empty() {
                    let b = self.collect(|cx| stray.iter().for_each(|n| cx.walk(n)));
                    if !b.is_empty() {
                        items.push(b);
                    }
                }
                if !items.is_empty() {
                    self.cur.push(Block::List { ordered, start, items });
                }
            }
            "li" => {
                self.flush();
                self.push_seg("• ".into(), Fx::default(), None, None);
                self.walk_children(h);
                self.flush();
            }
            "dl" => {
                if self.no_flush > 0 {
                    return self.walk_children(h);
                }
                self.flush();
                let mut term: Vec<Seg> = vec![];
                for c in children(h) {
                    match name(&c) {
                        Some("dt") => {
                            let saved = self.fx;
                            self.fx = self.fx.with(Fx::BOLD);
                            let t = self.collect_inline(|cx| cx.walk_children(&c));
                            self.fx = saved;
                            if !term.is_empty() {
                                self.cur.push(Block::Def(std::mem::take(&mut term), vec![]));
                            }
                            term = t;
                        }
                        Some("dd") => {
                            let d = self.collect(|cx| cx.walk_children(&c));
                            self.cur.push(Block::Def(std::mem::take(&mut term), d));
                        }
                        Some("div") => {
                            // <dl><div><dt/><dd/></div></dl> is allowed too.
                            self.walk(&c);
                        }
                        _ => self.walk(&c),
                    }
                }
                if !term.is_empty() {
                    self.cur.push(Block::Def(term, vec![]));
                }
            }
            "dt" => {
                self.flush();
                self.with_fx(h, Fx::BOLD);
                self.flush();
            }
            "dd" => {
                let d = self.collect(|cx| cx.walk_children(h));
                self.cur.push(Block::Def(vec![], d));
            }
            "table" => self.table(h),
            "a" => {
                let href = attr(h, "href").and_then(|u| self.abs(&u));
                let saved = self.link;
                if let Some(url) = href {
                    self.page.links.push(Link { url, title: attr(h, "title") });
                    self.link = Some(self.page.links.len() - 1);
                }
                let fx = if self.link.is_some() && saved != self.link { Fx::UNDERLINE } else { Fx::default() };
                let before = self.inline.len();
                self.with_fx(h, fx);
                // A link with nothing to show still gets something to click.
                if self.link != saved && self.inline.len() == before {
                    let url = &self.page.links[self.link.unwrap()].url;
                    let label = attr(h, "aria-label").or(attr(h, "title")).unwrap_or_else(|| short_url(url));
                    self.push_seg(label, self.fx.with(Fx::UNDERLINE), self.link, None);
                }
                self.link = saved;
            }
            "strong" | "b" => self.with_fx(h, Fx::BOLD),
            "em" | "i" | "cite" | "var" | "dfn" => self.with_fx(h, Fx::ITALIC),
            "code" | "kbd" | "samp" | "tt" => self.with_fx(h, Fx::CODE),
            "u" | "ins" => self.with_fx(h, Fx::UNDERLINE),
            "s" | "del" | "strike" => self.with_fx(h, Fx::STRIKE),
            "mark" => self.with_fx(h, Fx::MARK),
            "small" | "sub" | "sup" => self.with_fx(h, Fx::DIM),
            "q" => {
                self.push_text("“");
                self.walk_children(h);
                self.push_seg("”".into(), self.fx, self.link, None);
            }
            "img" => self.image(h),
            "picture" => {
                if let Some(img) = children(h).into_iter().find(|c| name(c) == Some("img")) {
                    self.image(&img);
                }
            }
            "iframe" | "frame" | "video" | "audio" => {
                let src = attr(h, "src")
                    .or_else(|| children(h).iter().find(|c| name(c) == Some("source")).and_then(|s| attr(s, "src")));
                if let Some(url) = src.and_then(|s| self.abs(&s)) {
                    let what = match tag.as_str() {
                        "video" => "video",
                        "audio" => "audio",
                        _ => "frame",
                    };
                    let label = attr(h, "title").unwrap_or_else(|| short_url(&url));
                    self.page.links.push(Link { url, title: None });
                    let idx = self.page.links.len() - 1;
                    self.flush();
                    self.push_seg(format!("[{what}: {label}]"), Fx::DIM.with(Fx::UNDERLINE), Some(idx), None);
                    self.flush();
                }
            }
            "form" => {
                let action = attr(h, "action").and_then(|a| self.abs(&a)).unwrap_or_default();
                self.page.forms.push(Form {
                    action,
                    method: attr(h, "method").unwrap_or_else(|| "get".into()).to_ascii_lowercase(),
                    enctype: attr(h, "enctype").unwrap_or_default(),
                });
                let saved = self.form.replace(self.page.forms.len() - 1);
                self.flush();
                self.walk_children(h);
                self.flush();
                self.form = saved;
            }
            "input" => self.input(h),
            "textarea" => {
                let f = self.field(h, FieldKind::TextArea, text_content(h));
                self.show_field(f);
            }
            "select" => {
                let mut opts = vec![];
                let mut selected = None;
                let mut all = vec![];
                find_all(h, "option", &mut all);
                for o in all {
                    let label = collapse(&text_content(&o));
                    let value = attr(&o, "value").unwrap_or_else(|| label.clone());
                    if attr(&o, "selected").is_some() {
                        selected = Some(value.clone());
                    }
                    opts.push((value, label));
                }
                let value = selected.or_else(|| opts.first().map(|o| o.0.clone())).unwrap_or_default();
                let f = self.field(h, FieldKind::Select(opts), value);
                self.show_field(f);
            }
            "button" => {
                let ty = attr(h, "type").unwrap_or_else(|| "submit".into()).to_ascii_lowercase();
                let kind = match ty.as_str() {
                    "reset" => FieldKind::Reset,
                    "button" => FieldKind::Button,
                    _ => FieldKind::Submit,
                };
                if kind == FieldKind::Button {
                    // Buttons that only run scripts do nothing here.
                    return;
                }
                let mut f = self.field(h, kind, attr(h, "value").unwrap_or_default());
                f.placeholder = collapse(&text_content(h));
                if f.placeholder.is_empty() {
                    f.placeholder = attr(h, "aria-label").or_else(|| attr(h, "title")).unwrap_or_default();
                }
                self.show_field(f);
            }
            _ => {
                if BLOCKS.contains(&tag.as_str()) {
                    self.flush();
                    self.walk_children(h);
                    self.flush();
                } else {
                    self.walk_children(h);
                }
            }
        }
    }

    fn image(&mut self, h: &Handle) {
        let tiny =
            |a: &str| attr(h, a).and_then(|v| v.trim_end_matches("px").parse::<u32>().ok()).is_some_and(|v| v <= 2);
        if tiny("width") || tiny("height") {
            return;
        }
        let src = attr(h, "src")
            .filter(|s| !s.starts_with("data:"))
            .or_else(|| attr(h, "data-src"))
            .or_else(|| {
                attr(h, "srcset")
                    .and_then(|s| s.split(',').next().map(|p| p.split_whitespace().next().unwrap_or("").to_string()))
            })
            .and_then(|s| self.abs(&s));
        let alt = collapse(&attr(h, "alt").unwrap_or_default());
        let Some(src) = src else {
            if !alt.is_empty() {
                if !self.ends_in_space() {
                    self.push_seg(" ".into(), self.fx, self.link, None);
                }
                self.push_seg(alt, self.fx, self.link, None);
            }
            return;
        };
        self.page.images.push(Image { src, alt: alt.clone(), link: self.link });
        let idx = self.page.images.len() - 1;
        let inline_ctx = self.no_flush > 0 || self.inline.iter().any(|s| !s.text.trim().is_empty());
        if inline_ctx || self.link.is_some() && alt.len() < 40 {
            if alt.is_empty() && self.link.is_none() {
                return;
            }
            let label = if alt.is_empty() { "image".to_string() } else { alt };
            self.inline.push(Seg {
                text: format!("[{label}]"),
                fx: self.fx.with(Fx::DIM),
                link: self.link,
                field: None,
                image: Some(idx),
            });
        } else {
            self.flush();
            self.cur.push(Block::Image(idx));
        }
    }

    fn field(&mut self, h: &Handle, kind: FieldKind, value: String) -> Field {
        Field {
            form: self.form,
            kind,
            name: attr(h, "name").unwrap_or_default(),
            value,
            checked: attr(h, "checked").is_some(),
            placeholder: attr(h, "placeholder").or_else(|| attr(h, "aria-label")).unwrap_or_default(),
        }
    }

    fn show_field(&mut self, f: Field) {
        let shown = f.shown();
        self.page.fields.push(f);
        let idx = self.page.fields.len() - 1;
        if !shown.is_empty() {
            if !self.ends_in_space() {
                self.push_seg(" ".into(), Fx::default(), None, None);
            }
            self.inline.push(Seg { text: shown, field: Some(idx), ..Default::default() });
            self.push_seg(" ".into(), Fx::default(), None, None);
        }
    }

    fn input(&mut self, h: &Handle) {
        let ty = attr(h, "type").unwrap_or_else(|| "text".into()).to_ascii_lowercase();
        let value = attr(h, "value").unwrap_or_default();
        let kind = match ty.as_str() {
            "hidden" => FieldKind::Hidden,
            "password" => FieldKind::Password,
            "checkbox" => FieldKind::Checkbox,
            "radio" => FieldKind::Radio,
            "submit" | "image" => FieldKind::Submit,
            "reset" => FieldKind::Reset,
            "button" => return,
            "file" => FieldKind::File,
            _ => FieldKind::Text,
        };
        let mut f = self.field(h, kind.clone(), value);
        if kind == FieldKind::Submit {
            f.placeholder = if ty == "image" { attr(h, "alt").unwrap_or_default() } else { f.value.clone() };
        }
        self.show_field(f);
    }

    fn table(&mut self, h: &Handle) {
        if self.no_flush > 0 {
            return self.walk_children(h);
        }
        // Rows, skipping into thead/tbody/tfoot but not into nested tables.
        let mut rows: Vec<(bool, Handle)> = vec![];
        let mut caption = None;
        for c in children(h) {
            match name(&c) {
                Some("tr") => rows.push((false, c)),
                Some(g @ ("thead" | "tbody" | "tfoot")) => {
                    for r in children(&c) {
                        if name(&r) == Some("tr") {
                            rows.push((g == "thead", r));
                        }
                    }
                }
                Some("caption") => caption = Some(c),
                _ => {}
            }
        }
        let cells =
            |r: &Handle| children(r).into_iter().filter(|c| matches!(name(c), Some("td" | "th"))).collect::<Vec<_>>();
        // Tables used for layout: nested tables, long block content, or one column.
        let mut layout = rows.iter().all(|(_, r)| cells(r).len() <= 1);
        let mut nested = vec![];
        for (_, r) in &rows {
            for c in cells(r) {
                find_all(&c, "table", &mut nested);
                let mut blocks = vec![];
                for t in ["p", "div", "ul", "ol", "h1", "h2", "h3", "blockquote", "pre"] {
                    find_all(&c, t, &mut blocks);
                }
                if !blocks.is_empty() && collapse(&text_content(&c)).len() > 120 {
                    layout = true;
                }
            }
        }
        if !nested.is_empty() || layout {
            self.flush();
            if let Some(c) = &caption {
                self.walk(c);
            }
            for (_, r) in &rows {
                if hidden(r) {
                    continue;
                }
                for c in cells(r) {
                    if hidden(&c) {
                        continue;
                    }
                    self.walk_children(&c);
                    if !self.ends_in_space() {
                        self.push_seg(" ".into(), Fx::default(), None, None);
                    }
                }
                self.flush();
            }
            return;
        }
        self.flush();
        if let Some(c) = &caption {
            let segs = self.collect_inline(|cx| cx.with_fx(c, Fx::ITALIC));
            if !segs.is_empty() {
                self.cur.push(Block::Para(segs));
            }
        }
        let mut out = vec![];
        for (head, r) in rows {
            if hidden(&r) {
                continue;
            }
            let cs = cells(&r);
            let is_head = head || (!cs.is_empty() && cs.iter().all(|c| name(c) == Some("th")));
            let mut row = vec![];
            for c in cs {
                let fx = if name(&c) == Some("th") { Fx::BOLD } else { Fx::default() };
                let segs = self.collect_inline(|cx| cx.with_fx(&c, fx));
                let span = attr(&c, "colspan").and_then(|s| s.parse::<usize>().ok()).unwrap_or(1).clamp(1, 20);
                row.push(segs);
                for _ in 1..span {
                    row.push(vec![]);
                }
            }
            if row.iter().any(|c| !c.is_empty()) {
                out.push((is_head, row));
            }
        }
        if !out.is_empty() {
            self.cur.push(Block::Table(out));
        }
    }
}

pub(crate) fn short_url(url: &str) -> String {
    match Url::parse(url) {
        Ok(u) => {
            let p = u.path().trim_end_matches('/');
            let last = p.rsplit('/').next().unwrap_or("");
            if last.is_empty() { u.host_str().unwrap_or(url).to_string() } else { last.to_string() }
        }
        Err(_) => url.to_string(),
    }
}
