//! Reading and writing messages: summaries from headers, a readable text body (HTML turned
//! into text, remote images never fetched), and drafts built into MIME.

use mail_parser::{Address, MessageParser, MimeHeaders};

use crate::Flags;

/// A line in a message list.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Summary {
    /// The backend's id (an IMAP UID or a JMAP id).
    pub id: String,
    pub from: String,
    pub from_addr: String,
    pub to: String,
    pub subject: String,
    /// Seconds since 1970.
    pub date: Option<i64>,
    pub flags: Flags,
    pub size: Option<u64>,
}

fn first(a: Option<&Address<'_>>) -> (String, String) {
    a.and_then(|a| a.first())
        .map(|x| {
            let addr = x.address().unwrap_or_default().to_string();
            let name = x.name().map(str::to_string).filter(|n| !n.trim().is_empty()).unwrap_or_else(|| addr.clone());
            (name, addr)
        })
        .unwrap_or_default()
}

fn list(a: Option<&Address<'_>>) -> String {
    a.map(|a| {
        a.iter()
            .map(|x| match (x.name(), x.address()) {
                (Some(n), Some(e)) if !n.is_empty() => format!("{n} <{e}>"),
                (_, Some(e)) => e.to_string(),
                (Some(n), None) => n.to_string(),
                _ => String::new(),
            })
            .collect::<Vec<_>>()
            .join(", ")
    })
    .unwrap_or_default()
}

/// A summary from a message's headers (the body may be missing).
pub fn summarize(id: String, headers: &[u8], flags: Flags, size: Option<u64>) -> Summary {
    let Some(m) = MessageParser::default().parse_headers(headers) else {
        return Summary { id, subject: "(unreadable)".into(), flags, size, ..Default::default() };
    };
    let (from, from_addr) = first(m.from());
    Summary {
        id,
        from,
        from_addr,
        to: list(m.to()),
        subject: m.subject().unwrap_or("(no subject)").to_string(),
        date: m.date().map(|d| d.to_timestamp()),
        flags,
        size,
    }
}

/// IMAP flag names to [`Flags`].
pub fn flags_from_imap(f: &[String]) -> Flags {
    let has = |n: &str| f.iter().any(|x| x.eq_ignore_ascii_case(n));
    Flags { seen: has("\\Seen"), flagged: has("\\Flagged"), answered: has("\\Answered"), draft: has("\\Draft") }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub name: String,
    pub mime: String,
    pub size: usize,
}

/// A message ready to read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Parsed {
    pub from: String,
    pub from_addr: String,
    pub reply_to: String,
    pub to: String,
    pub cc: String,
    pub subject: String,
    pub date: Option<i64>,
    pub message_id: Option<String>,
    pub references: Vec<String>,
    /// The text to show: text/plain if there is one, otherwise the HTML as text.
    pub text: String,
    /// True when the text came from HTML.
    pub from_html: bool,
    pub attachments: Vec<Attachment>,
    /// List-Unsubscribe, when present.
    pub unsubscribe: Option<String>,
}

pub fn parse(raw: &[u8]) -> Parsed {
    let Some(m) = MessageParser::default().parse(raw) else {
        return Parsed {
            text: String::from_utf8_lossy(raw).into_owned(),
            subject: "(unreadable)".into(),
            ..Default::default()
        };
    };
    let (from, from_addr) = first(m.from());
    let mut refs: Vec<String> =
        m.references().as_text_list().map(|l| l.iter().map(|s| s.to_string()).collect()).unwrap_or_default();
    if refs.is_empty()
        && let Some(t) = m.references().as_text()
    {
        refs.push(t.to_string());
    }
    let (text, from_html) = match m.body_text(0) {
        Some(t) if m.text_body.first().is_some_and(|&i| m.parts.get(i as usize).is_some_and(|p| !p.is_text_html())) => {
            (t.into_owned(), false)
        }
        _ => match m.body_html(0) {
            Some(h) => (html_to_text(&h), true),
            None => (m.body_text(0).map(|t| t.into_owned()).unwrap_or_default(), false),
        },
    };
    let attachments = m
        .attachments()
        .map(|a| Attachment {
            name: a.attachment_name().unwrap_or("unnamed").to_string(),
            mime: a
                .content_type()
                .map(|c| format!("{}/{}", c.ctype(), c.subtype().unwrap_or("octet-stream")))
                .unwrap_or_else(|| "application/octet-stream".into()),
            size: a.contents().len(),
        })
        .collect();
    Parsed {
        from,
        from_addr,
        reply_to: list(m.reply_to()),
        to: list(m.to()),
        cc: list(m.cc()),
        subject: m.subject().unwrap_or("(no subject)").to_string(),
        date: m.date().map(|d| d.to_timestamp()),
        message_id: m.message_id().map(str::to_string),
        references: refs,
        text,
        from_html,
        attachments,
        unsubscribe: m.header_raw("List-Unsubscribe").map(|s| s.trim().to_string()),
    }
}

/// HTML as readable text: paragraphs and line breaks kept, links shown after their text,
/// scripts, styles and images dropped (an image's alt text is kept).
pub fn html_to_text(html: &str) -> String {
    let mut out = String::new();
    let mut i = 0;
    let b = html.as_bytes();
    let mut skip_until: Option<&str> = None;
    let mut href: Vec<Option<String>> = vec![];
    let mut pre = 0;
    while i < b.len() {
        if b[i] == b'<' {
            let end = match html[i..].find('>') {
                Some(e) => i + e,
                None => break,
            };
            let tag = &html[i + 1..end];
            i = end + 1;
            let lower = tag.to_ascii_lowercase();
            let name: String =
                lower.trim_start_matches('/').chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
            let closing = lower.starts_with('/');
            if let Some(stop) = skip_until {
                if closing && name == stop {
                    skip_until = None;
                }
                continue;
            }
            match (name.as_str(), closing) {
                ("script" | "style" | "head" | "title", false) => {
                    skip_until = Some(match name.as_str() {
                        "script" => "script",
                        "style" => "style",
                        "head" => "head",
                        _ => "title",
                    })
                }
                ("br", _) => out.push('\n'),
                (
                    "p" | "div" | "tr" | "table" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "blockquote" | "ul" | "ol",
                    _,
                ) => {
                    if !out.ends_with("\n\n") {
                        out.push_str(if out.ends_with('\n') { "\n" } else { "\n\n" });
                    }
                }
                ("li", false) => {
                    if !out.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push_str("• ");
                }
                ("td" | "th", true) => out.push_str("  "),
                ("hr", _) => out.push_str("\n────────\n"),
                ("pre", false) => pre += 1,
                ("pre", true) => pre = (pre - 1).max(0),
                ("img", false) => {
                    if let Some(alt) = attr(tag, "alt").filter(|a| !a.trim().is_empty()) {
                        out.push_str(&format!("[{}]", alt.trim()));
                    }
                }
                ("a", false) => href.push(attr(tag, "href")),
                ("a", true) => {
                    if let Some(Some(h)) = href.pop()
                        && (h.starts_with("http") || h.starts_with("mailto:"))
                        && !out.trim_end().ends_with(h.trim_start_matches("mailto:"))
                    {
                        out.push_str(&format!(" <{h}>"));
                    }
                }
                _ => {}
            }
            continue;
        }
        let next = html[i..].find('<').map(|n| i + n).unwrap_or(b.len());
        if skip_until.is_none() {
            let chunk = decode_entities(&html[i..next]);
            if pre > 0 {
                out.push_str(&chunk);
            } else {
                for w in chunk.split_whitespace() {
                    if !out.is_empty() && !out.ends_with([' ', '\n']) {
                        out.push(' ');
                    }
                    out.push_str(w);
                }
                if chunk.ends_with(char::is_whitespace) && !out.ends_with([' ', '\n']) && !chunk.trim().is_empty() {
                    out.push(' ');
                }
            }
        }
        i = next;
    }
    let lines: Vec<&str> = out.lines().map(str::trim_end).collect();
    let mut text = String::new();
    let mut blank = 0;
    for l in lines {
        if l.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        text.push_str(l.trim_start_matches(' '));
        text.push('\n');
    }
    text.trim().to_string()
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let at = lower.find(&format!("{name}="))? + name.len() + 1;
    let rest = &tag[at..];
    let v = if let Some(r) = rest.strip_prefix('"') {
        r.split('"').next()
    } else if let Some(r) = rest.strip_prefix('\'') {
        r.split('\'').next()
    } else {
        rest.split([' ', '>']).next()
    }?;
    Some(decode_entities(v))
}

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest[..rest.len().min(12)].find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..end];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            "mdash" => Some('—'),
            "ndash" => Some('–'),
            "hellip" => Some('…'),
            "rsquo" => Some('’'),
            "lsquo" => Some('‘'),
            "rdquo" => Some('”'),
            "ldquo" => Some('“'),
            "copy" => Some('©'),
            _ if ent.starts_with("#x") || ent.starts_with("#X") => {
                u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32)
            }
            _ if ent.starts_with('#') => ent[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// A message being written.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Draft {
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    pub body: String,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
}

/// Split `a@x, "B, C" <b@y>; c@z` into addresses (with display names kept apart).
pub fn parse_addresses(s: &str) -> Vec<(Option<String>, String)> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut quoted = false;
    let mut angle = false;
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            '<' if !quoted => {
                angle = true;
                cur.push(c);
            }
            '>' if !quoted => {
                angle = false;
                cur.push(c);
            }
            ',' | ';' if !quoted && !angle => {
                out.extend(one_address(&cur));
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    out.extend(one_address(&cur));
    out
}

fn one_address(s: &str) -> Option<(Option<String>, String)> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let (Some(a), Some(b)) = (s.rfind('<'), s.rfind('>'))
        && a < b
    {
        let name = s[..a].trim().trim_matches('"').trim();
        return Some(((!name.is_empty()).then(|| name.to_string()), s[a + 1..b].trim().to_string()));
    }
    Some((None, s.to_string()))
}

impl Draft {
    /// Every recipient address (to, cc and bcc).
    pub fn recipients(&self) -> Vec<String> {
        [&self.to, &self.cc, &self.bcc].iter().flat_map(|s| parse_addresses(s)).map(|(_, a)| a).collect()
    }

    /// The MIME message. Bcc is left out of the headers.
    pub fn build(&self, from_name: Option<&str>, from: &str, message_id_domain: &str) -> Result<Vec<u8>, String> {
        use mail_builder::MessageBuilder;
        use mail_builder::headers::address::Address as A;
        let to_list = |s: &str| -> Vec<A<'static>> {
            parse_addresses(s).into_iter().map(|(n, a)| A::new_address(n, a)).collect()
        };
        let to = to_list(&self.to);
        if to.is_empty() && self.cc.trim().is_empty() && self.bcc.trim().is_empty() {
            return Err("add someone to send it to".into());
        }
        for (_, a) in parse_addresses(&format!("{},{},{}", self.to, self.cc, self.bcc)) {
            if !a.contains('@') || a.contains(char::is_whitespace) {
                return Err(format!("{a} is not an email address"));
            }
        }
        let id = format!(
            "{}.{}@{}",
            time::OffsetDateTime::now_utc().unix_timestamp_nanos(),
            std::process::id(),
            message_id_domain
        );
        let mut m = MessageBuilder::new()
            .from(A::new_address(from_name.map(str::to_string), from.to_string()))
            .subject(self.subject.clone())
            .message_id(id)
            .text_body(self.body.replace("\r\n", "\n"));
        if !to.is_empty() {
            m = m.to(A::new_list(to));
        }
        let cc = to_list(&self.cc);
        if !cc.is_empty() {
            m = m.cc(A::new_list(cc));
        }
        if let Some(r) = &self.in_reply_to {
            m = m.in_reply_to(r.clone());
        }
        if !self.references.is_empty() {
            m = m.references(self.references.clone());
        }
        m = m.header(
            "User-Agent",
            mail_builder::headers::text::Text::new(format!("Alouette/{}", env!("CARGO_PKG_VERSION"))),
        );
        m.write_to_vec().map_err(|e| e.to_string())
    }

    /// A reply to `p`. `all` adds the other recipients (except `me`).
    pub fn reply(p: &Parsed, all: bool, me: &str) -> Draft {
        let to = if p.reply_to.is_empty() { format!("{} <{}>", p.from, p.from_addr) } else { p.reply_to.clone() };
        let cc = if all {
            parse_addresses(&format!("{},{}", p.to, p.cc))
                .into_iter()
                .filter(|(_, a)| !a.eq_ignore_ascii_case(me) && !a.eq_ignore_ascii_case(&p.from_addr))
                .map(|(n, a)| n.map(|n| format!("{n} <{a}>")).unwrap_or(a))
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            String::new()
        };
        let subject = if p.subject.to_ascii_lowercase().starts_with("re:") {
            p.subject.clone()
        } else {
            format!("Re: {}", p.subject)
        };
        let when = p
            .date
            .and_then(|d| time::OffsetDateTime::from_unix_timestamp(d).ok())
            .map(|d| format!("{:04}-{:02}-{:02}", d.year(), d.month() as u8, d.day()));
        let mut body = format!("\n\nOn {}, {} wrote:\n", when.unwrap_or_else(|| "an earlier day".into()), p.from);
        for l in p.text.lines() {
            body.push_str(if l.starts_with('>') { ">" } else { "> " });
            body.push_str(l);
            body.push('\n');
        }
        let mut references = p.references.clone();
        if let Some(id) = &p.message_id {
            references.push(id.clone());
        }
        Draft { to, cc, subject, body, in_reply_to: p.message_id.clone(), references, ..Default::default() }
    }

    pub fn forward(p: &Parsed) -> Draft {
        let subject = if p.subject.to_ascii_lowercase().starts_with("fwd:") {
            p.subject.clone()
        } else {
            format!("Fwd: {}", p.subject)
        };
        let body = format!(
            "\n\n---------- Forwarded message ----------\nFrom: {} <{}>\nSubject: {}\nTo: {}\n\n{}\n",
            p.from, p.from_addr, p.subject, p.to, p.text
        );
        Draft { subject, body, ..Default::default() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MULTI: &[u8] = b"From: \"Ada L.\" <ada@example.org>\r\nTo: me@example.org, Bob <bob@example.net>\r\nSubject: =?UTF-8?Q?Caf=C3=A9?=\r\nDate: Tue, 2 Jan 2024 03:04:05 +0000\r\nMessage-ID: <m1@example.org>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=X\r\n\r\n--X\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<html><head><style>p{}</style></head><body><p>Hello &amp; welcome</p><p>Read <a href=\"https://x.example/a\">this</a><br>now</p><img src=\"https://track.example/p.gif\"><ul><li>one</li><li>two</li></ul></body></html>\r\n--X\r\nContent-Type: application/pdf; name=report.pdf\r\nContent-Disposition: attachment; filename=report.pdf\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERi0=\r\n--X--\r\n";

    #[test]
    fn parses_html_mail_with_attachment() {
        let p = parse(MULTI);
        assert_eq!((p.from.as_str(), p.from_addr.as_str()), ("Ada L.", "ada@example.org"));
        assert_eq!(p.subject, "Café");
        assert_eq!(p.to, "me@example.org, Bob <bob@example.net>");
        assert_eq!(p.message_id.as_deref(), Some("m1@example.org"));
        assert!(p.from_html);
        assert_eq!(p.text, "Hello & welcome\n\nRead this <https://x.example/a>\nnow\n\n• one\n• two");
        assert!(!p.text.contains("track.example"));
        assert_eq!(p.attachments, [Attachment { name: "report.pdf".into(), mime: "application/pdf".into(), size: 5 }]);
        let s = summarize("1".into(), MULTI, Flags::default(), None);
        assert_eq!(s.subject, "Café");
        assert_eq!(s.date, Some(1704164645));
    }

    #[test]
    fn addresses() {
        let a = parse_addresses("a@x.org; \"Smith, Jo\" <jo@y.org>, Bo <bo@z.org>");
        assert_eq!(
            a,
            [
                (None, "a@x.org".into()),
                (Some("Smith, Jo".into()), "jo@y.org".into()),
                (Some("Bo".into()), "bo@z.org".into())
            ]
        );
    }

    #[test]
    fn reply_builds_and_round_trips() {
        let p = parse(MULTI);
        let mut d = Draft::reply(&p, true, "me@example.org");
        assert_eq!(d.to, "Ada L. <ada@example.org>");
        assert_eq!(d.cc, "Bob <bob@example.net>");
        assert_eq!(d.subject, "Re: Café");
        assert!(d.body.contains("> Hello & welcome"));
        d.body = format!("Thanks!{}", d.body);
        d.bcc = "secret@example.com".into();
        let raw = d.build(Some("Me"), "me@example.org", "example.org").unwrap();
        let back = parse(&raw);
        assert_eq!(back.subject, "Re: Café");
        assert_eq!(back.to, "Ada L. <ada@example.org>");
        assert!(back.text.starts_with("Thanks!"));
        assert_eq!(back.references, ["m1@example.org"]);
        assert!(!String::from_utf8_lossy(&raw).contains("secret@example.com"));
        assert_eq!(d.recipients().len(), 3);
        assert!(Draft { to: "not an address".into(), ..Default::default() }.build(None, "me@x.org", "x.org").is_err());
    }
}
