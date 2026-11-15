//! Content lines, as vCard and iCalendar share them: folding, parameters, escaping and
//! BEGIN/END nesting.

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Prop {
    /// Upper case, without any group prefix.
    pub name: String,
    /// Parameter names in upper case, each with its values.
    pub params: Vec<(String, Vec<String>)>,
    /// The raw value, still escaped.
    pub value: String,
}

impl Prop {
    pub fn new(name: &str, value: impl Into<String>) -> Self {
        Prop { name: name.to_ascii_uppercase(), params: vec![], value: value.into() }
    }
    /// A text property, escaped.
    pub fn text(name: &str, value: &str) -> Self {
        Prop::new(name, escape(value))
    }
    pub fn with_param(mut self, k: &str, v: &str) -> Self {
        self.params.push((k.to_ascii_uppercase(), vec![v.to_string()]));
        self
    }
    pub fn param(&self, k: &str) -> Option<&str> {
        self.params.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).and_then(|(_, v)| v.first()).map(String::as_str)
    }
    /// Every value of a parameter, split on commas, such as TYPE=work,voice.
    pub fn param_all(&self, k: &str) -> Vec<String> {
        self.params
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(k))
            .flat_map(|(_, v)| v.iter().flat_map(|x| x.split(',')).map(|x| x.trim().to_ascii_lowercase()))
            .collect()
    }
    /// The value unescaped, as text.
    pub fn text_value(&self) -> String {
        unescape(&self.value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Component {
    pub name: String,
    pub props: Vec<Prop>,
    pub children: Vec<Component>,
}

impl Component {
    pub fn new(name: &str) -> Self {
        Component { name: name.to_ascii_uppercase(), ..Default::default() }
    }
    pub fn get(&self, name: &str) -> Option<&Prop> {
        self.props.iter().find(|p| p.name.eq_ignore_ascii_case(name))
    }
    pub fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Prop> + 'a {
        self.props.iter().filter(move |p| p.name.eq_ignore_ascii_case(name))
    }
    pub fn text(&self, name: &str) -> Option<String> {
        self.get(name).map(Prop::text_value).filter(|s| !s.is_empty())
    }
    pub fn set(&mut self, p: Prop) {
        self.props.retain(|o| o.name != p.name);
        self.props.push(p);
    }
    pub fn remove(&mut self, name: &str) {
        self.props.retain(|o| !o.name.eq_ignore_ascii_case(name));
    }
    pub fn child(&self, name: &str) -> impl Iterator<Item = &Component> {
        let name = name.to_ascii_uppercase();
        self.children.iter().filter(move |c| c.name == name)
    }

    /// Write the component with CRLF line ends and 75-octet folding.
    pub fn write(&self) -> String {
        let mut out = String::new();
        self.write_into(&mut out);
        out
    }

    fn write_into(&self, out: &mut String) {
        fold_into(out, &format!("BEGIN:{}", self.name));
        for p in &self.props {
            let mut line = p.name.clone();
            for (k, vs) in &p.params {
                line.push(';');
                line.push_str(k);
                line.push('=');
                let vals: Vec<String> =
                    vs.iter()
                        .map(|v| {
                            if v.contains([':', ';', ',']) { format!("\"{}\"", v.replace('"', "'")) } else { v.clone() }
                        })
                        .collect();
                line.push_str(&vals.join(","));
            }
            line.push(':');
            line.push_str(&p.value);
            fold_into(out, &line);
        }
        for c in &self.children {
            c.write_into(out);
        }
        fold_into(out, &format!("END:{}", self.name));
    }
}

fn fold_into(out: &mut String, line: &str) {
    let mut count = 0;
    for ch in line.chars() {
        let n = ch.len_utf8();
        if count + n > 75 {
            out.push_str("\r\n ");
            count = 1;
        }
        out.push(ch);
        count += n;
    }
    out.push_str("\r\n");
}

/// Escape text for a TEXT value.
pub fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            ';' => o.push_str("\\;"),
            ',' => o.push_str("\\,"),
            '\n' => o.push_str("\\n"),
            '\r' => {}
            c => o.push(c),
        }
    }
    o
}

pub fn unescape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') | Some('N') => o.push('\n'),
                Some(x) => o.push(x),
                None => {}
            }
        } else {
            o.push(c);
        }
    }
    o
}

/// Split a structured value on unescaped `sep`, unescaping each part.
pub fn split_structured(s: &str, sep: char) -> Vec<String> {
    let mut parts = vec![];
    let mut cur = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            cur.push(c);
            if let Some(n) = chars.next() {
                cur.push(n);
            }
        } else if c == sep {
            parts.push(unescape(&cur));
            cur.clear();
        } else {
            cur.push(c);
        }
    }
    parts.push(unescape(&cur));
    parts
}

fn unfold(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = vec![];
    for raw in text.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        if (raw.starts_with(' ') || raw.starts_with('\t')) && !lines.is_empty() {
            lines.last_mut().unwrap().push_str(&raw[1..]);
        } else if !raw.is_empty() {
            lines.push(raw.to_string());
        }
    }
    lines
}

/// Parse one content line.
pub fn parse_line(line: &str) -> Option<Prop> {
    // The name and parameters end at the first colon outside quotes.
    let mut in_q = false;
    let mut colon = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_q = !in_q,
            ':' if !in_q => {
                colon = Some(i);
                break;
            }
            _ => {}
        }
    }
    let colon = colon?;
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let mut parts = vec![];
    let mut cur = String::new();
    in_q = false;
    for c in head.chars() {
        match c {
            '"' => in_q = !in_q,
            ';' if !in_q => parts.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    parts.push(cur);
    let mut it = parts.into_iter();
    let name = it.next()?;
    // Drop a vCard group such as "item1.".
    let name = name.rsplit('.').next().unwrap_or(&name).to_ascii_uppercase();
    if name.is_empty() {
        return None;
    }
    let params = it
        .map(|p| {
            let (k, v) = match p.split_once('=') {
                Some((k, v)) => (k.to_string(), v.to_string()),
                // vCard 2.1 bare types: "TEL;WORK;VOICE:".
                None => ("TYPE".to_string(), p.clone()),
            };
            (k.to_ascii_uppercase(), v.split(',').map(|x| x.trim_matches('"').to_string()).collect())
        })
        .collect();
    Some(Prop { name, params, value: value.to_string() })
}

/// Parse every top-level component in a file.
pub fn parse(text: &str) -> Vec<Component> {
    let mut stack: Vec<Component> = vec![];
    let mut out = vec![];
    for line in unfold(text) {
        let Some(p) = parse_line(&line) else { continue };
        match p.name.as_str() {
            "BEGIN" => stack.push(Component::new(p.value.trim())),
            "END" => {
                let Some(c) = stack.pop() else { continue };
                match stack.last_mut() {
                    Some(parent) => parent.children.push(c),
                    None => out.push(c),
                }
            }
            _ => {
                if let Some(c) = stack.last_mut() {
                    c.props.push(p);
                }
            }
        }
    }
    // Unclosed components still count.
    while let Some(c) = stack.pop() {
        match stack.last_mut() {
            Some(parent) => parent.children.push(c),
            None => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding_params_and_escapes() {
        let text = "BEGIN:VCARD\r\nVERSION:4.0\r\nitem1.EMAIL;TYPE=work,pref:ada@example.org\r\nNOTE:Line one\\nwith a comma\\, and a \r\n long folded tail\r\nX-THING;LABEL=\"a:b;c\":v\r\nEND:VCARD\r\n";
        let c = &parse(text)[0];
        assert_eq!(c.name, "VCARD");
        let e = c.get("email").unwrap();
        assert_eq!(e.param_all("type"), ["work", "pref"]);
        assert_eq!(c.text("NOTE").unwrap(), "Line one\nwith a comma, and a long folded tail");
        assert_eq!(c.get("X-THING").unwrap().param("label"), Some("a:b;c"));
        // Round trip.
        let again = &parse(&c.write())[0];
        assert_eq!(again, c);
        let long = Component { name: "X".into(), props: vec![Prop::text("NOTE", &"é".repeat(60))], children: vec![] };
        assert!(long.write().lines().all(|l| l.len() <= 76));
        assert_eq!(parse(&long.write())[0].text("NOTE").unwrap(), "é".repeat(60));
        assert_eq!(split_structured("Lovelace;Ada;;Countess\\;x;", ';'), ["Lovelace", "Ada", "", "Countess;x", ""]);
    }
}
