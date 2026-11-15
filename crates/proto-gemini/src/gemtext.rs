//! The gemtext line format.

/// One gemtext line, or a preformatted block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Text(String),
    Link { url: String, label: Option<String> },
    Heading { level: u8, text: String },
    ListItem(String),
    Quote(String),
    Preformatted { alt: String, lines: Vec<String> },
}

/// Parse gemtext. An unclosed preformatted block runs to the end of the document.
pub fn parse(text: &str) -> Vec<Line> {
    let mut out = Vec::new();
    let mut pre: Option<(String, Vec<String>)> = None;
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(rest) = line.strip_prefix("```") {
            match pre.take() {
                Some((alt, lines)) => out.push(Line::Preformatted { alt, lines }),
                None => pre = Some((rest.trim().to_string(), Vec::new())),
            }
            continue;
        }
        if let Some((_, lines)) = &mut pre {
            lines.push(line.to_string());
            continue;
        }
        out.push(if let Some(rest) = line.strip_prefix("=>") {
            let rest = rest.trim_start();
            let (url, label) = match rest.find([' ', '\t']) {
                Some(i) => {
                    let label = rest[i..].trim();
                    (rest[..i].to_string(), (!label.is_empty()).then(|| label.to_string()))
                }
                None => (rest.trim().to_string(), None),
            };
            if url.is_empty() { Line::Text(line.to_string()) } else { Line::Link { url, label } }
        } else if let Some(rest) = line.strip_prefix("###") {
            Line::Heading { level: 3, text: rest.trim().to_string() }
        } else if let Some(rest) = line.strip_prefix("##") {
            Line::Heading { level: 2, text: rest.trim().to_string() }
        } else if let Some(rest) = line.strip_prefix('#') {
            Line::Heading { level: 1, text: rest.trim().to_string() }
        } else if let Some(rest) = line.strip_prefix("* ") {
            Line::ListItem(rest.to_string())
        } else if let Some(rest) = line.strip_prefix('>') {
            Line::Quote(rest.trim_start().to_string())
        } else {
            Line::Text(line.to_string())
        });
    }
    if let Some((alt, lines)) = pre {
        out.push(Line::Preformatted { alt, lines });
    }
    // Drop the empty line a trailing newline produces.
    if out.last() == Some(&Line::Text(String::new())) && text.ends_with('\n') {
        out.pop();
    }
    out
}

/// The first level-one heading, used as the window title.
pub fn title(lines: &[Line]) -> Option<&str> {
    lines.iter().find_map(|l| match l {
        Line::Heading { level: 1, text } if !text.is_empty() => Some(text.as_str()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_line_type() {
        let doc = "# Title\r\n## Sub\n### Third\nplain\n=> gemini://x/ X marks\n=>/rel\n=>\n* item\n> quote\n```ascii art\n# not a heading\n```\nafter\n";
        let lines = parse(doc);
        assert_eq!(lines[0], Line::Heading { level: 1, text: "Title".into() });
        assert_eq!(lines[2], Line::Heading { level: 3, text: "Third".into() });
        assert_eq!(lines[4], Line::Link { url: "gemini://x/".into(), label: Some("X marks".into()) });
        assert_eq!(lines[5], Line::Link { url: "/rel".into(), label: None });
        assert_eq!(lines[6], Line::Text("=>".into()));
        assert_eq!(lines[7], Line::ListItem("item".into()));
        assert_eq!(lines[8], Line::Quote("quote".into()));
        assert_eq!(lines[9], Line::Preformatted { alt: "ascii art".into(), lines: vec!["# not a heading".into()] });
        assert_eq!(lines[10], Line::Text("after".into()));
        assert_eq!(lines.len(), 11);
        assert_eq!(title(&lines), Some("Title"));
    }

    #[test]
    fn unclosed_preformatted_runs_to_end() {
        assert_eq!(
            parse("```\na\nb"),
            vec![Line::Preformatted { alt: String::new(), lines: vec!["a".into(), "b".into()] }]
        );
    }
}
