//! Search the local cache (mail kept offline, when the person turned that on) for "Ask
//! Alouette". Search runs entirely here; only the best few matches go to the model.

use std::path::{Path, PathBuf};

/// One matching message.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub path: PathBuf,
    pub subject: String,
    pub from: String,
    pub date: String,
    pub score: usize,
    /// The lines around the matches.
    pub excerpt: String,
}

fn words(q: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "the", "a", "an", "and", "or", "of", "to", "in", "on", "for", "is", "are", "was", "what", "when", "who", "did",
        "does", "do", "my", "me", "i", "about", "with", "from", "that", "this", "it", "how", "which", "any",
    ];
    q.split(|c: char| !c.is_alphanumeric())
        .map(|w| w.to_lowercase())
        .filter(|w| w.len() > 1 && !STOP.contains(&w.as_str()))
        .collect()
}

fn header(head: &str, name: &str) -> String {
    let mut out = String::new();
    let mut on = false;
    for line in head.lines() {
        if on && (line.starts_with(' ') || line.starts_with('\t')) {
            out.push(' ');
            out.push_str(line.trim());
            continue;
        }
        on = false;
        if let Some((k, v)) = line.split_once(':')
            && k.eq_ignore_ascii_case(name)
        {
            out = v.trim().to_string();
            on = true;
        }
    }
    out
}

/// Score one message's text against the query words.
fn score(raw: &str, q: &[String], path: &Path) -> Option<Hit> {
    let (head, body) = raw.split_once("\r\n\r\n").or_else(|| raw.split_once("\n\n")).unwrap_or(("", raw));
    let subject = header(head, "subject");
    let from = header(head, "from");
    let date = header(head, "date");
    let hay = format!("{subject}\n{from}\n{body}").to_lowercase();
    let mut total = 0;
    let mut matched = 0;
    for w in q {
        let n = hay.matches(w.as_str()).count();
        if n > 0 {
            matched += 1;
            total += n.min(10);
        }
    }
    if matched == 0 {
        return None;
    }
    // Every word found counts most; subject matches count extra.
    let subj = subject.to_lowercase();
    let bonus: usize = q.iter().filter(|w| subj.contains(w.as_str())).count() * 5;
    let score = matched * 20 + total + bonus;
    let mut excerpt = String::new();
    for line in body.lines().filter(|l| {
        let l = l.to_lowercase();
        q.iter().any(|w| l.contains(w.as_str()))
    }) {
        if excerpt.len() > 600 {
            break;
        }
        excerpt.push_str(line.trim());
        excerpt.push('\n');
    }
    Some(Hit { path: path.to_path_buf(), subject, from, date, score, excerpt })
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 6 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() => walk(&p, out, depth + 1),
            Ok(t) if t.is_file() && p.extension().is_some_and(|x| x == "eml") => out.push(p),
            _ => {}
        }
    }
}

/// Search every cached message under `root` and return the best `max`.
pub fn search(root: &Path, query: &str, max: usize) -> Vec<Hit> {
    let q = words(query);
    if q.is_empty() {
        return Vec::new();
    }
    let mut files = Vec::new();
    walk(root, &mut files, 0);
    let mut hits: Vec<Hit> = files
        .iter()
        .filter_map(|p| {
            let raw = std::fs::read(p).ok()?;
            score(&String::from_utf8_lossy(&raw[..raw.len().min(256 * 1024)]), &q, p)
        })
        .collect();
    hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.path.cmp(&b.path)));
    hits.truncate(max);
    hits
}

/// The hits as content for the model.
pub fn as_content(hits: &[Hit]) -> String {
    let mut s = String::new();
    for (i, h) in hits.iter().enumerate() {
        s.push_str(&format!(
            "[{}] Subject: {}\nFrom: {}\nDate: {}\n{}\n",
            i + 1,
            h.subject,
            h.from,
            h.date,
            h.excerpt.trim_end()
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds() {
        let dir = std::env::temp_dir().join(format!("alouette-ai-search-{}", std::process::id()));
        let inbox = dir.join("mail/me/INBOX");
        std::fs::create_dir_all(&inbox).unwrap();
        std::fs::write(
            inbox.join("1.eml"),
            "Subject: Flight to Lisbon\r\nFrom: Air <a@air.example>\r\n\r\nYour flight leaves at 09:40 from gate 12.\r\n",
        )
        .unwrap();
        std::fs::write(inbox.join("2.eml"), "Subject: Lunch\r\nFrom: Bo\r\n\r\nLunch on Friday?\r\n").unwrap();
        std::fs::write(inbox.join("3.txt"), "flight flight flight").unwrap();
        let hits = search(&dir, "When does my flight to Lisbon leave?", 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].subject, "Flight to Lisbon");
        assert!(hits[0].excerpt.contains("09:40"));
        assert!(as_content(&hits).starts_with("[1] Subject: Flight to Lisbon"));
        assert!(search(&dir, "the of", 5).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
