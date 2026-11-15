//! A small element tree, enough for feeds: names without prefixes, attributes, text.

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

#[derive(Debug, Clone, Default)]
pub struct Node {
    pub name: String,
    /// The prefix as written, such as `dc` or `content`.
    pub ns: String,
    pub attrs: Vec<(String, String)>,
    pub text: String,
    pub children: Vec<Node>,
}

impl Node {
    pub fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }
    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
    /// The text of a child, or empty.
    pub fn text_of(&self, name: &str) -> String {
        self.child(name).map(|c| c.text.trim().to_string()).unwrap_or_default()
    }
}

fn split(name: &[u8]) -> (String, String) {
    let s = String::from_utf8_lossy(name);
    match s.split_once(':') {
        Some((p, l)) => (l.to_string(), p.to_string()),
        None => (s.to_string(), String::new()),
    }
}

fn node(e: &BytesStart) -> Node {
    let (name, ns) = split(e.name().as_ref());
    let attrs = e
        .attributes()
        .flatten()
        .map(|a| {
            let (k, _) = split(a.key.as_ref());
            let raw = String::from_utf8_lossy(&a.value).into_owned();
            let v = quick_xml::escape::unescape(&raw).map(|c| c.into_owned()).unwrap_or(raw);
            (k, v)
        })
        .collect();
    Node { name, ns, attrs, ..Default::default() }
}

/// Entities HTML uses that XML does not define, which feeds leak all the time.
fn entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "mdash" => '—',
        "ndash" => '–',
        "hellip" => '…',
        "rsquo" => '’',
        "lsquo" => '‘',
        "rdquo" => '”',
        "ldquo" => '“',
        "copy" => '©',
        _ => {
            let n = name.strip_prefix('#')?;
            let code = match n.strip_prefix(['x', 'X']) {
                Some(h) => u32::from_str_radix(h, 16).ok()?,
                None => n.parse().ok()?,
            };
            return char::from_u32(code);
        }
    })
}

/// Parse a document and return its root element.
pub fn parse(text: &str) -> Result<Node, String> {
    let mut r = Reader::from_str(text);
    r.config_mut().check_end_names = false;
    let mut stack: Vec<Node> = vec![Node::default()];
    loop {
        let ev = r.read_event().map_err(|e| format!("bad XML at byte {}: {e}", r.buffer_position()))?;
        match ev {
            Event::Start(e) => stack.push(node(&e)),
            Event::Empty(e) => {
                let n = node(&e);
                if let Some(top) = stack.last_mut() {
                    top.children.push(n);
                }
            }
            Event::End(_) => {
                if stack.len() > 1 {
                    let n = stack.pop().unwrap_or_default();
                    if let Some(top) = stack.last_mut() {
                        top.children.push(n);
                    }
                }
            }
            Event::Text(t) => {
                let raw = String::from_utf8_lossy(t.as_ref()).into_owned();
                let s = quick_xml::escape::unescape(&raw).map(|c| c.into_owned()).unwrap_or(raw);
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&s);
                }
            }
            Event::CData(t) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&String::from_utf8_lossy(t.as_ref()));
                }
            }
            Event::GeneralRef(g) => {
                let name = String::from_utf8_lossy(g.as_ref()).into_owned();
                if let Some(top) = stack.last_mut() {
                    match entity(&name) {
                        Some(c) => top.text.push(c),
                        None => top.text.push_str(&format!("&{name};")),
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    // Close anything left open (truncated feeds).
    while stack.len() > 1 {
        let n = stack.pop().unwrap_or_default();
        if let Some(top) = stack.last_mut() {
            top.children.push(n);
        }
    }
    stack.pop().and_then(|d| d.children.into_iter().next()).ok_or_else(|| "an empty document".into())
}
