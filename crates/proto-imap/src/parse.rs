//! Reading IMAP responses: lines with literals spliced in, then a small tokenizer.

/// A value in a response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Nil,
    Num(u64),
    /// An atom, such as `FLAGS`, `\Seen` or `BODY[HEADER]`.
    Atom(String),
    /// A quoted string or a literal.
    Str(Vec<u8>),
    List(Vec<Value>),
}

impl Value {
    pub fn as_str(&self) -> Option<String> {
        match self {
            Value::Str(b) => Some(String::from_utf8_lossy(b).into_owned()),
            Value::Atom(a) => Some(a.clone()),
            Value::Num(n) => Some(n.to_string()),
            _ => None,
        }
    }
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Str(b) => Some(b),
            Value::Atom(a) => Some(a.as_bytes()),
            _ => None,
        }
    }
    pub fn as_num(&self) -> Option<u64> {
        match self {
            Value::Num(n) => Some(*n),
            Value::Atom(a) => a.parse().ok(),
            _ => None,
        }
    }
    pub fn as_list(&self) -> &[Value] {
        match self {
            Value::List(l) => l,
            _ => &[],
        }
    }
    pub fn is_atom(&self, s: &str) -> bool {
        matches!(self, Value::Atom(a) if a.eq_ignore_ascii_case(s))
    }
}

/// Tokenize one response (literals already inline, as `{n}\r\n` followed by n bytes).
pub fn tokens(b: &[u8]) -> Result<Vec<Value>, String> {
    let mut p = P { b, i: 0 };
    let mut out = vec![];
    loop {
        p.skip_sp();
        if p.i >= b.len() || b[p.i] == b'\r' || b[p.i] == b'\n' {
            return Ok(out);
        }
        out.push(p.value()?);
    }
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn skip_sp(&mut self) {
        while self.i < self.b.len() && self.b[self.i] == b' ' {
            self.i += 1;
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        let b = self.b;
        match b.get(self.i) {
            None => Err("response ended early".into()),
            Some(b'(') => {
                self.i += 1;
                let mut l = vec![];
                loop {
                    self.skip_sp();
                    match b.get(self.i) {
                        None => return Err("unclosed (".into()),
                        Some(b')') => {
                            self.i += 1;
                            return Ok(Value::List(l));
                        }
                        Some(b'\r' | b'\n') => return Err("unclosed (".into()),
                        _ => l.push(self.value()?),
                    }
                }
            }
            Some(b'"') => {
                self.i += 1;
                let mut s = vec![];
                while let Some(&c) = b.get(self.i) {
                    self.i += 1;
                    match c {
                        b'\\' => {
                            if let Some(&n) = b.get(self.i) {
                                s.push(n);
                                self.i += 1;
                            }
                        }
                        b'"' => return Ok(Value::Str(s)),
                        _ => s.push(c),
                    }
                }
                Err("unclosed quote".into())
            }
            Some(b'{') => {
                let end = b[self.i..].iter().position(|&c| c == b'}').ok_or("bad literal")? + self.i;
                let n: usize = std::str::from_utf8(&b[self.i + 1..end])
                    .ok()
                    .and_then(|s| s.trim_end_matches('+').parse().ok())
                    .ok_or("bad literal length")?;
                let start = end + 3; // }\r\n
                if start + n > b.len() {
                    return Err("literal ran past the response".into());
                }
                self.i = start + n;
                Ok(Value::Str(b[start..start + n].to_vec()))
            }
            Some(_) => {
                // An atom; brackets nest, so BODY[HEADER.FIELDS (FROM TO)] stays one atom.
                let start = self.i;
                let mut depth = 0;
                while let Some(&c) = b.get(self.i) {
                    match c {
                        b'[' => depth += 1,
                        b']' => depth -= 1,
                        b' ' | b'(' | b')' | b'\r' | b'\n' if depth <= 0 => break,
                        _ => {}
                    }
                    self.i += 1;
                }
                let a = String::from_utf8_lossy(&b[start..self.i]).into_owned();
                if a.eq_ignore_ascii_case("NIL") {
                    Ok(Value::Nil)
                } else if !a.is_empty() && a.bytes().all(|c| c.is_ascii_digit()) {
                    Ok(a.parse().map(Value::Num).unwrap_or(Value::Atom(a)))
                } else {
                    Ok(Value::Atom(a))
                }
            }
        }
    }
}

/// If a line ends with a literal marker `{n}` or `{n+}`, its length.
pub fn literal_len(line: &[u8]) -> Option<usize> {
    let l = line.strip_suffix(b"\r\n").or_else(|| line.strip_suffix(b"\n"))?;
    let l = l.strip_suffix(b"}")?;
    let open = l.iter().rposition(|&c| c == b'{')?;
    std::str::from_utf8(&l[open + 1..]).ok()?.trim_end_matches('+').parse().ok()
}

/// Quote a string for a command, or say it needs a literal.
pub fn quote(s: &str) -> Option<String> {
    if s.bytes().any(|c| c == b'\r' || c == b'\n' || c == 0 || c >= 0x80) {
        return None;
    }
    Some(format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")))
}

/// Modified UTF-7 (RFC 3501 5.1.3) to UTF-8, for mailbox names.
pub fn decode_mutf7(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i + 1..];
        let Some(j) = rest.find('-') else {
            out.push('&');
            out.push_str(rest);
            return out;
        };
        let chunk = &rest[..j];
        rest = &rest[j + 1..];
        if chunk.is_empty() {
            out.push('&');
            continue;
        }
        use base64::Engine;
        let b64 = chunk.replace(',', "/");
        match base64::engine::general_purpose::STANDARD_NO_PAD.decode(b64.trim_end_matches('=')) {
            Ok(bytes) => {
                let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
                out.push_str(&String::from_utf16_lossy(&units));
            }
            Err(_) => {
                out.push('&');
                out.push_str(chunk);
                out.push('-');
            }
        }
    }
    out.push_str(rest);
    out
}

/// UTF-8 to modified UTF-7.
pub fn encode_mutf7(s: &str) -> String {
    use base64::Engine;
    let mut out = String::new();
    let mut pending: Vec<u16> = vec![];
    let flush = |pending: &mut Vec<u16>, out: &mut String| {
        if !pending.is_empty() {
            let bytes: Vec<u8> = pending.iter().flat_map(|u| u.to_be_bytes()).collect();
            out.push('&');
            out.push_str(&base64::engine::general_purpose::STANDARD_NO_PAD.encode(bytes).replace('/', ","));
            out.push('-');
            pending.clear();
        }
    };
    for c in s.chars() {
        if (' '..='~').contains(&c) {
            flush(&mut pending, &mut out);
            if c == '&' { out.push_str("&-") } else { out.push(c) }
        } else {
            let mut buf = [0u16; 2];
            pending.extend_from_slice(c.encode_utf16(&mut buf));
        }
    }
    flush(&mut pending, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_fetch() {
        let r = b"* 12 FETCH (UID 7 FLAGS (\\Seen $Junk) RFC822.SIZE 44 BODY[HEADER.FIELDS (FROM)] {11}\r\nFrom: a@b\r\n INTERNALDATE \"17-Jul-1996 02:44:25 -0700\")\r\n";
        let t = tokens(r).unwrap();
        assert_eq!(t[0], Value::Atom("*".into()));
        assert_eq!(t[1], Value::Num(12));
        assert!(t[2].is_atom("fetch"));
        let l = t[3].as_list();
        assert_eq!(l[1], Value::Num(7));
        assert_eq!(l[3].as_list()[0], Value::Atom("\\Seen".into()));
        assert_eq!(l[6], Value::Atom("BODY[HEADER.FIELDS (FROM)]".into()));
        assert_eq!(l[7], Value::Str(b"From: a@b\r\n".to_vec()));
        assert_eq!(l[9].as_str().unwrap(), "17-Jul-1996 02:44:25 -0700");
    }

    #[test]
    fn nil_and_quotes() {
        let t = tokens(b"* LIST (\\HasNoChildren) \"/\" \"a \\\"b\\\"\" NIL\r\n").unwrap();
        assert_eq!(t[4], Value::Str(b"a \"b\"".to_vec()));
        assert_eq!(t[5], Value::Nil);
    }

    #[test]
    fn literals() {
        assert_eq!(literal_len(b"* 1 FETCH (BODY[] {42}\r\n"), Some(42));
        assert_eq!(literal_len(b"A1 APPEND x {7+}\r\n"), Some(7));
        assert_eq!(literal_len(b"* OK done\r\n"), None);
    }

    #[test]
    fn mutf7() {
        assert_eq!(decode_mutf7("~peter/mail/&U,BTFw-/&ZeVnLIqe-"), "~peter/mail/台北/日本語");
        assert_eq!(decode_mutf7("Sent &- Received"), "Sent & Received");
        assert_eq!(encode_mutf7("台北/日本語"), "&U,BTFw-/&ZeVnLIqe-");
        assert_eq!(encode_mutf7("A&B"), "A&-B");
        for s in ["Entwürfe", "Papierkorb", "😀 fun", "Gesendete Objekte"] {
            assert_eq!(decode_mutf7(&encode_mutf7(s)), s);
        }
    }
}
