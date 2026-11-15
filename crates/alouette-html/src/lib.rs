//! HTML for the terminal. [`Page::parse`] reads a document into blocks of styled text with
//! its links, images and forms; [`reader`] picks out the article the way reader modes do;
//! [`layout`] wraps blocks to a width; [`markdown`] writes them back out as Markdown.
//!
//! Used by the Web module, HTML mail and anything else that shows a web page.

mod convert;
pub mod layout;
pub mod markdown;
pub mod reader;

use url::Url;

pub use layout::{Line, layout};

/// Text effects, combined as bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Fx(pub u8);

impl Fx {
    pub const BOLD: Fx = Fx(1);
    pub const ITALIC: Fx = Fx(2);
    pub const CODE: Fx = Fx(4);
    pub const UNDERLINE: Fx = Fx(8);
    pub const STRIKE: Fx = Fx(16);
    pub const MARK: Fx = Fx(32);
    pub const HEADING: Fx = Fx(64);
    pub const DIM: Fx = Fx(128);

    pub fn has(self, f: Fx) -> bool {
        self.0 & f.0 != 0
    }
    pub fn with(self, f: Fx) -> Fx {
        Fx(self.0 | f.0)
    }
}

/// A run of text with one style.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Seg {
    pub text: String,
    pub fx: Fx,
    /// Index into [`Page::links`].
    pub link: Option<usize>,
    /// Index into [`Page::fields`].
    pub field: Option<usize>,
    /// Index into [`Page::images`].
    pub image: Option<usize>,
}

impl Seg {
    pub fn plain(text: impl Into<String>) -> Self {
        Seg { text: text.into(), ..Default::default() }
    }
}

/// Block-level content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Heading(u8, Vec<Seg>),
    Para(Vec<Seg>),
    /// Preformatted text, kept as is.
    Pre(String),
    Quote(Vec<Block>),
    List {
        ordered: bool,
        start: u32,
        items: Vec<Vec<Block>>,
    },
    /// Rows of cells; the bool marks header rows.
    Table(Vec<(bool, Vec<Vec<Seg>>)>),
    Rule,
    /// Index into [`Page::images`].
    Image(usize),
    /// A term and its definition.
    Def(Vec<Seg>, Vec<Block>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub url: String,
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub src: String,
    pub alt: String,
    pub link: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    Password,
    Hidden,
    TextArea,
    Checkbox,
    Radio,
    Submit,
    Reset,
    Button,
    File,
    /// The options as (value, label).
    Select(Vec<(String, String)>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub form: Option<usize>,
    pub kind: FieldKind,
    pub name: String,
    pub value: String,
    pub checked: bool,
    pub placeholder: String,
}

impl Field {
    /// Whether a person can change or press it.
    pub fn interactive(&self) -> bool {
        !matches!(self.kind, FieldKind::Hidden)
    }
    /// The text shown in place of the field.
    pub fn shown(&self) -> String {
        match &self.kind {
            FieldKind::Text | FieldKind::TextArea | FieldKind::File => {
                let v = if self.value.is_empty() { self.placeholder.as_str() } else { self.value.as_str() };
                let v: String = v.chars().take(24).collect();
                format!("[{v:_<16}]")
            }
            FieldKind::Password => format!("[{:_<16}]", "•".repeat(self.value.chars().count().min(24))),
            FieldKind::Hidden => String::new(),
            FieldKind::Checkbox => if self.checked { "[x]" } else { "[ ]" }.into(),
            FieldKind::Radio => if self.checked { "(•)" } else { "( )" }.into(),
            FieldKind::Submit | FieldKind::Reset | FieldKind::Button => {
                let v = if !self.placeholder.is_empty() {
                    self.placeholder.as_str()
                } else if !self.value.is_empty() {
                    self.value.as_str()
                } else {
                    match self.kind {
                        FieldKind::Reset => "Reset",
                        FieldKind::Submit => "Submit",
                        _ => "Button",
                    }
                };
                format!("[ {v} ]")
            }
            FieldKind::Select(opts) => {
                let label = opts.iter().find(|(v, _)| *v == self.value).or(opts.first()).map(|(_, l)| l.as_str());
                format!("[{} ▾]", label.unwrap_or(""))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    pub action: String,
    pub method: String,
    pub enctype: String,
}

/// A parsed page.
#[derive(Debug, Clone, Default)]
pub struct Page {
    pub url: String,
    pub title: String,
    pub blocks: Vec<Block>,
    pub links: Vec<Link>,
    pub images: Vec<Image>,
    pub forms: Vec<Form>,
    pub fields: Vec<Field>,
    /// `<link rel=alternate>` feeds, as (type, absolute URL).
    pub feeds: Vec<(String, String)>,
    /// A `<meta http-equiv=refresh>` target, with its delay in seconds.
    pub refresh: Option<(u32, String)>,
    /// Whether the page seems to need JavaScript to show anything.
    pub needs_script: bool,
    pub lang: Option<String>,
}

impl Page {
    /// Parse a whole document. Links and images are made absolute against `base` (or the
    /// page's `<base href>`).
    pub fn parse(html: &str, base: &str) -> Page {
        convert::convert(html, base, None)
    }

    /// Parse and keep only the main article, as reader modes do. Falls back to the whole
    /// page when no article stands out.
    pub fn parse_reader(html: &str, base: &str) -> Page {
        convert::convert(html, base, Some(()))
    }

    /// All the text, one block per line, for search and summaries.
    pub fn plain_text(&self) -> String {
        layout(self, 10_000, &Default::default()).iter().map(|l| l.text()).collect::<Vec<_>>().join("\n")
    }

    /// Fill a form's fields into a URL (GET) or a body (POST). `pressed` is the submit
    /// button used, if any. Returns (method, url, body, content type).
    pub fn submit(&self, form: usize, pressed: Option<usize>) -> Option<(String, String, Option<String>, String)> {
        let f = self.forms.get(form)?;
        let mut pairs: Vec<(String, String)> = vec![];
        for (i, fl) in self.fields.iter().enumerate() {
            if fl.form != Some(form) || fl.name.is_empty() {
                continue;
            }
            match fl.kind {
                FieldKind::Checkbox | FieldKind::Radio if !fl.checked => continue,
                FieldKind::Checkbox | FieldKind::Radio if fl.value.is_empty() => {
                    pairs.push((fl.name.clone(), "on".into()))
                }
                FieldKind::Submit | FieldKind::Button if Some(i) != pressed => continue,
                FieldKind::Reset | FieldKind::File => continue,
                FieldKind::Select(ref opts) if fl.value.is_empty() => {
                    pairs.push((fl.name.clone(), opts.first().map(|o| o.0.clone()).unwrap_or_default()))
                }
                _ => pairs.push((fl.name.clone(), fl.value.clone())),
            }
        }
        let encoded = url::form_urlencoded::Serializer::new(String::new()).extend_pairs(pairs).finish();
        let action = if f.action.is_empty() { self.url.clone() } else { f.action.clone() };
        if f.method.eq_ignore_ascii_case("post") {
            Some(("POST".into(), action, Some(encoded), "application/x-www-form-urlencoded".into()))
        } else {
            let mut u = Url::parse(&action).ok()?;
            u.set_query(Some(&encoded));
            u.set_fragment(None);
            Some(("GET".into(), u.to_string(), None, String::new()))
        }
    }

    /// Fields that a person can move between, in page order.
    pub fn focusable_fields(&self) -> impl Iterator<Item = usize> + '_ {
        self.fields.iter().enumerate().filter(|(_, f)| f.interactive()).map(|(i, _)| i)
    }

    /// Choose a radio button, clearing the others in its group.
    pub fn check_radio(&mut self, idx: usize) {
        let Some(f) = self.fields.get(idx).cloned() else { return };
        for g in self.fields.iter_mut() {
            if g.kind == FieldKind::Radio && g.form == f.form && g.name == f.name {
                g.checked = false;
            }
        }
        self.fields[idx].checked = true;
    }
}

/// How [`to_text`] shows links.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkStyle {
    /// `text <url>`, as mail readers do.
    Inline,
    /// `text[1]`, with the list of links at the end.
    Numbered,
    /// Just the text.
    Hidden,
}

/// Turn HTML into plain text for mail, posts and feed items. `width` wraps long lines;
/// pass a large width to keep paragraphs on one line. Images without a description are left
/// out.
pub fn to_text(html: &str, width: usize, links: LinkStyle) -> String {
    let mut page = Page::parse(html, "about:blank");
    fn fix(page_links: &[Link], images: &[Image], blocks: &mut Vec<Block>, inline: bool) {
        blocks.retain(|b| !matches!(b, Block::Image(i) if images.get(*i).is_none_or(|im| im.alt.is_empty())));
        for b in blocks.iter_mut() {
            match b {
                Block::Heading(_, s) | Block::Para(s) => fix_segs(page_links, s, inline),
                Block::Def(t, d) => {
                    fix_segs(page_links, t, inline);
                    fix(page_links, images, d, inline);
                }
                Block::Quote(v) => fix(page_links, images, v, inline),
                Block::List { items, .. } => items.iter_mut().for_each(|i| fix(page_links, images, i, inline)),
                Block::Table(rows) => {
                    rows.iter_mut().for_each(|(_, r)| r.iter_mut().for_each(|c| fix_segs(page_links, c, inline)))
                }
                _ => {}
            }
        }
    }
    fn fix_segs(page_links: &[Link], segs: &mut Vec<Seg>, inline: bool) {
        if !inline {
            return;
        }
        let mut i = 0;
        while i < segs.len() {
            if let Some(l) = segs[i].link
                && segs.get(i + 1).is_none_or(|n| n.link != Some(l))
                && let Some(link) = page_links.get(l)
            {
                let text: String = segs.iter().filter(|s| s.link == Some(l)).map(|s| s.text.as_str()).collect();
                let url = link.url.strip_prefix("mailto:").unwrap_or(&link.url);
                if text.trim() != url && !link.url.starts_with('#') {
                    segs.insert(i + 1, Seg::plain(format!(" <{url}>")));
                    i += 1;
                }
            }
            i += 1;
        }
    }
    let inline = links == LinkStyle::Inline;
    let (pl, im) = (page.links.clone(), page.images.clone());
    fix(&pl, &im, &mut page.blocks, inline);
    let opts = layout::Options { link_numbers: links == LinkStyle::Numbered };
    let mut text = layout(&page, width.max(20), &opts)
        .iter()
        .map(|l| l.text().trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();
    if links == LinkStyle::Numbered && !page.links.is_empty() {
        text.push_str("\n\n");
        for (i, l) in page.links.iter().enumerate() {
            text.push_str(&format!("[{}] {}\n", i + 1, l.url));
        }
    }
    text.trim_end().to_string()
}

#[cfg(test)]
mod tests;
