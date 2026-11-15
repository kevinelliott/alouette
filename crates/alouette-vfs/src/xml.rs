//! Just enough XML for WebDAV and S3 answers: element starts, text and ends, by local name
//! (namespace prefixes dropped).

use quick_xml::Reader;
use quick_xml::events::Event;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ev {
    Start(String),
    Text(String),
    End(String),
}

fn local(name: &[u8]) -> String {
    let s = String::from_utf8_lossy(name);
    s.rsplit(':').next().unwrap_or(&s).to_string()
}

/// Every event in document order. Malformed XML ends the walk with an error.
pub fn events(xml: &str) -> Result<Vec<Ev>, String> {
    let mut r = Reader::from_str(xml);
    let mut out = vec![];
    loop {
        match r.read_event().map_err(|e| format!("bad XML: {e}"))? {
            Event::Start(e) => out.push(Ev::Start(local(e.name().as_ref()))),
            Event::Empty(e) => {
                let n = local(e.name().as_ref());
                out.push(Ev::Start(n.clone()));
                out.push(Ev::End(n));
            }
            Event::End(e) => out.push(Ev::End(local(e.name().as_ref()))),
            Event::Text(t) => {
                let raw = String::from_utf8_lossy(t.as_ref()).into_owned();
                let s = quick_xml::escape::unescape(&raw).map(|c| c.into_owned()).unwrap_or(raw);
                if !s.is_empty() {
                    out.push(Ev::Text(s));
                }
            }
            Event::CData(t) => out.push(Ev::Text(String::from_utf8_lossy(t.as_ref()).into_owned())),
            Event::GeneralRef(r) => {
                // Entity references arrive separately; fold them into text.
                let name = String::from_utf8_lossy(r.as_ref()).into_owned();
                let ch = match name.as_str() {
                    "amp" => "&".to_string(),
                    "lt" => "<".into(),
                    "gt" => ">".into(),
                    "quot" => "\"".into(),
                    "apos" => "'".into(),
                    n if n.starts_with("#x") => u32::from_str_radix(&n[2..], 16)
                        .ok()
                        .and_then(char::from_u32)
                        .map(String::from)
                        .unwrap_or_default(),
                    n if n.starts_with('#') => {
                        n[1..].parse().ok().and_then(char::from_u32).map(String::from).unwrap_or_default()
                    }
                    _ => String::new(),
                };
                match out.last_mut() {
                    Some(Ev::Text(t)) => t.push_str(&ch),
                    _ => out.push(Ev::Text(ch)),
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    // Merge text split around entity references.
    let mut merged: Vec<Ev> = Vec::with_capacity(out.len());
    for e in out {
        match (merged.last_mut(), e) {
            (Some(Ev::Text(a)), Ev::Text(b)) => a.push_str(&b),
            (_, e) => merged.push(e),
        }
    }
    // Whitespace between elements is layout, not content.
    Ok(merged
        .into_iter()
        .filter_map(|e| match e {
            Ev::Text(t) => {
                let t = t.trim();
                (!t.is_empty()).then(|| Ev::Text(t.to_string()))
            }
            e => Some(e),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_with_namespaces_and_entities() {
        let ev = events(r#"<d:a xmlns:d="DAV:"><d:b>x &amp; y</d:b><d:c/></d:a>"#).unwrap();
        assert_eq!(
            ev,
            vec![
                Ev::Start("a".into()),
                Ev::Start("b".into()),
                Ev::Text("x & y".into()),
                Ev::End("b".into()),
                Ev::Start("c".into()),
                Ev::End("c".into()),
                Ev::End("a".into()),
            ]
        );
    }
}
