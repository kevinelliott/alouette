//! IRC messages: `@tags :source COMMAND params :trailing`, as RFC 1459 and IRCv3 define them.

use std::fmt::Write;

/// One IRC message.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Message {
    /// IRCv3 message tags, values unescaped. A tag without a value has `None`.
    pub tags: Vec<(String, Option<String>)>,
    /// `nick!user@host` or a server name.
    pub source: Option<String>,
    /// The command, upper case, or a three-digit numeric.
    pub command: String,
    pub params: Vec<String>,
}

impl Message {
    pub fn new(command: &str, params: &[&str]) -> Self {
        Self {
            tags: vec![],
            source: None,
            command: command.to_string(),
            params: params.iter().map(|p| p.to_string()).collect(),
        }
    }

    /// Parse one line, without its CR LF. Returns `None` for an empty or malformed line.
    pub fn parse(line: &str) -> Option<Self> {
        let mut rest = line.trim_end_matches(['\r', '\n']).trim_start_matches(' ');
        let mut msg = Message::default();
        if let Some(r) = rest.strip_prefix('@') {
            let (tags, r) = r.split_once(' ')?;
            for t in tags.split(';').filter(|t| !t.is_empty()) {
                match t.split_once('=') {
                    Some((k, v)) => msg.tags.push((k.to_string(), Some(unescape_tag(v)))),
                    None => msg.tags.push((t.to_string(), None)),
                }
            }
            rest = r.trim_start_matches(' ');
        }
        if let Some(r) = rest.strip_prefix(':') {
            let (src, r) = r.split_once(' ')?;
            msg.source = Some(src.to_string());
            rest = r.trim_start_matches(' ');
        }
        let (cmd, mut r) = match rest.split_once(' ') {
            Some((c, r)) => (c, r),
            None => (rest, ""),
        };
        if cmd.is_empty() {
            return None;
        }
        msg.command = cmd.to_ascii_uppercase();
        loop {
            r = r.trim_start_matches(' ');
            if r.is_empty() {
                break;
            }
            if let Some(t) = r.strip_prefix(':') {
                msg.params.push(t.to_string());
                break;
            }
            match r.split_once(' ') {
                Some((p, more)) => {
                    msg.params.push(p.to_string());
                    r = more;
                }
                None => {
                    msg.params.push(r.to_string());
                    break;
                }
            }
        }
        Some(msg)
    }

    /// The wire form, without CR LF. Line breaks inside parameters become spaces, so a
    /// message can never smuggle a second command.
    pub fn to_line(&self) -> String {
        let mut out = String::new();
        if !self.tags.is_empty() {
            out.push('@');
            for (i, (k, v)) in self.tags.iter().enumerate() {
                if i > 0 {
                    out.push(';');
                }
                out.push_str(k);
                if let Some(v) = v {
                    out.push('=');
                    out.push_str(&escape_tag(v));
                }
            }
            out.push(' ');
        }
        if let Some(s) = &self.source {
            let _ = write!(out, ":{s} ");
        }
        out.push_str(&self.command);
        let n = self.params.len();
        for (i, p) in self.params.iter().enumerate() {
            let p = p.replace(['\r', '\n', '\0'], " ");
            out.push(' ');
            if i + 1 == n && (p.is_empty() || p.contains(' ') || p.starts_with(':')) {
                out.push(':');
            }
            out.push_str(&p);
        }
        out
    }

    /// The nick part of the source.
    pub fn nick(&self) -> Option<&str> {
        self.source.as_deref().map(|s| s.split(['!', '@']).next().unwrap_or(s))
    }

    /// `user@host` from the source, if it carries one.
    pub fn userhost(&self) -> Option<&str> {
        self.source.as_deref().and_then(|s| s.split_once('!')).map(|(_, uh)| uh)
    }

    pub fn tag(&self, name: &str) -> Option<&str> {
        self.tags.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_deref().unwrap_or(""))
    }

    pub fn param(&self, i: usize) -> &str {
        self.params.get(i).map(String::as_str).unwrap_or("")
    }

    /// The last parameter, which is usually the text.
    pub fn trailing(&self) -> &str {
        self.params.last().map(String::as_str).unwrap_or("")
    }

    pub fn with_tag(mut self, k: &str, v: Option<&str>) -> Self {
        self.tags.push((k.to_string(), v.map(str::to_string)));
        self
    }

    pub fn with_source(mut self, s: &str) -> Self {
        self.source = Some(s.to_string());
        self
    }
}

pub fn escape_tag(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            ';' => out.push_str("\\:"),
            ' ' => out.push_str("\\s"),
            '\\' => out.push_str("\\\\"),
            '\r' => out.push_str("\\r"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out
}

pub fn unescape_tag(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut it = v.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some(':') => out.push(';'),
            Some('s') => out.push(' '),
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some(c) => out.push(c),
            None => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tags_source_and_trailing() {
        let m = Message::parse(
            "@time=2026-01-02T03:04:05.000Z;msgid=a\\sb;+draft/react :nick!u@h PRIVMSG #rust :hello there",
        )
        .unwrap();
        assert_eq!(m.tag("time"), Some("2026-01-02T03:04:05.000Z"));
        assert_eq!(m.tag("msgid"), Some("a b"));
        assert_eq!(m.tag("+draft/react"), Some(""));
        assert_eq!(m.nick(), Some("nick"));
        assert_eq!(m.userhost(), Some("u@h"));
        assert_eq!(m.command, "PRIVMSG");
        assert_eq!(m.params, ["#rust", "hello there"]);
    }

    #[test]
    fn parses_odd_shapes() {
        assert_eq!(Message::parse("PING").unwrap().params, Vec::<String>::new());
        assert_eq!(Message::parse("ping :x y").unwrap().command, "PING");
        let m = Message::parse(":srv 005 me CHANTYPES=# PREFIX=(ov)@+ :are supported").unwrap();
        assert_eq!(m.params.len(), 4);
        assert_eq!(Message::parse("  CAP  * LS :a b").unwrap().params, ["*", "LS", "a b"]);
        assert!(Message::parse("").is_none());
        assert!(Message::parse(":onlysource").is_none());
    }

    #[test]
    fn writes_round_trip() {
        let m = Message::new("PRIVMSG", &["#a", "hi there"]).with_tag("+reply", Some("x;y z"));
        assert_eq!(m.to_line(), "@+reply=x\\:y\\sz PRIVMSG #a :hi there");
        assert_eq!(Message::parse(&m.to_line()).unwrap(), m);
        assert_eq!(Message::new("NICK", &["me"]).to_line(), "NICK me");
        assert_eq!(Message::new("PRIVMSG", &["#a", ":)"]).to_line(), "PRIVMSG #a ::)");
        assert_eq!(Message::new("PRIVMSG", &["#a", ""]).to_line(), "PRIVMSG #a :");
        // No smuggled commands.
        assert_eq!(Message::new("PRIVMSG", &["#a", "x\r\nQUIT"]).to_line(), "PRIVMSG #a :x  QUIT");
    }
}
