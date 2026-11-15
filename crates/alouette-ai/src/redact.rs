//! Replace personal details with placeholders before text leaves the machine: email
//! addresses, phone numbers, IP addresses, card numbers and things that look like keys.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Counts {
    pub emails: usize,
    pub phones: usize,
    pub ips: usize,
    pub cards: usize,
    pub secrets: usize,
}

impl Counts {
    pub fn total(&self) -> usize {
        self.emails + self.phones + self.ips + self.cards + self.secrets
    }

    /// Such as `2 emails, 1 phone number`.
    pub fn describe(&self) -> String {
        let mut v = vec![];
        let mut add = |n: usize, one: &str, many: &str| {
            if n > 0 {
                v.push(format!("{n} {}", if n == 1 { one } else { many }));
            }
        };
        add(self.emails, "email address", "email addresses");
        add(self.phones, "phone number", "phone numbers");
        add(self.ips, "IP address", "IP addresses");
        add(self.cards, "card number", "card numbers");
        add(self.secrets, "key or token", "keys or tokens");
        if v.is_empty() { "nothing".into() } else { v.join(", ") }
    }
}

/// Redact `text`. Returns the new text and what was replaced.
pub fn redact(text: &str) -> (String, Counts) {
    let mut counts = Counts::default();
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // Only start a match at a word boundary.
        let boundary = i == 0 || !is_word(chars[i - 1]);
        if boundary {
            if let Some(n) = card(&chars[i..]) {
                out.push_str("[card]");
                counts.cards += 1;
                i += n;
                continue;
            }
            if let Some(n) = ipv4(&chars[i..]) {
                out.push_str("[ip]");
                counts.ips += 1;
                i += n;
                continue;
            }
            if let Some(n) = ipv6(&chars[i..]) {
                out.push_str("[ip]");
                counts.ips += 1;
                i += n;
                continue;
            }
            if let Some(n) = phone(&chars[i..]) {
                out.push_str("[phone]");
                counts.phones += 1;
                i += n;
                continue;
            }
            // A token of word characters, dots, @ and so on.
            let end = (i..chars.len()).find(|&j| !is_token(chars[j])).unwrap_or(chars.len());
            if end > i {
                let tok: String = chars[i..end].iter().collect();
                let tok_trim = tok.trim_end_matches(['.', '-']);
                let tlen = tok_trim.chars().count();
                if tlen == 0 {
                    out.push(chars[i]);
                    i += 1;
                    continue;
                }
                if is_email(tok_trim) {
                    out.push_str("[email]");
                    counts.emails += 1;
                    i += tlen;
                    continue;
                }
                if is_secret(tok_trim) {
                    out.push_str("[secret]");
                    counts.secrets += 1;
                    i += tlen;
                    continue;
                }
                out.push_str(tok_trim);
                i += tlen;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    (out, counts)
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_token(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '@' | '+' | '%')
}

fn is_email(t: &str) -> bool {
    let Some((local, domain)) = t.split_once('@') else { return false };
    !local.is_empty()
        && !domain.contains('@')
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && domain.rsplit('.').next().is_some_and(|tld| tld.len() >= 2 && tld.chars().all(|c| c.is_alphabetic()))
}

fn is_secret(t: &str) -> bool {
    const PREFIXES: &[&str] =
        &["sk-", "sk_live_", "sk_test_", "ghp_", "gho_", "github_pat_", "xoxb-", "xoxp-", "AKIA", "AIza", "glpat-"];
    if PREFIXES.iter().any(|p| t.starts_with(p) && t.len() >= p.len() + 12) {
        return true;
    }
    // Long, random-looking runs: letters and digits mixed, few repeats.
    if t.len() < 32 || !t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return false;
    }
    let digits = t.chars().filter(|c| c.is_ascii_digit()).count();
    let upper = t.chars().filter(|c| c.is_ascii_uppercase()).count();
    let lower = t.chars().filter(|c| c.is_ascii_lowercase()).count();
    let classes = [digits, upper, lower].iter().filter(|n| **n > 0).count();
    let mut distinct: Vec<char> = t.chars().collect();
    distinct.sort_unstable();
    distinct.dedup();
    classes >= 2 && digits >= 2 && distinct.len() >= 12
}

/// Digits with optional spaces or dashes, 13 to 19 of them, passing the Luhn check.
fn card(c: &[char]) -> Option<usize> {
    let mut digits = Vec::new();
    let mut end = 0;
    for (i, ch) in c.iter().enumerate() {
        if ch.is_ascii_digit() {
            digits.push(ch.to_digit(10)?);
            end = i + 1;
            if digits.len() == 19 {
                break;
            }
        } else if (*ch == ' ' || *ch == '-') && i + 1 < c.len() && c[i + 1].is_ascii_digit() && !digits.is_empty() {
            continue;
        } else {
            break;
        }
    }
    if end < c.len() && is_word(c[end]) {
        return None;
    }
    if (13..=19).contains(&digits.len()) && luhn(&digits) { Some(end) } else { None }
}

fn luhn(d: &[u32]) -> bool {
    let sum: u32 = d
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &x)| {
            if i % 2 == 1 {
                let y = x * 2;
                if y > 9 { y - 9 } else { y }
            } else {
                x
            }
        })
        .sum();
    sum.is_multiple_of(10)
}

fn ipv4(c: &[char]) -> Option<usize> {
    let mut i = 0;
    for part in 0..4 {
        let start = i;
        while i < c.len() && c[i].is_ascii_digit() && i - start < 3 {
            i += 1;
        }
        if i == start {
            return None;
        }
        let n: u32 = c[start..i].iter().collect::<String>().parse().ok()?;
        if n > 255 {
            return None;
        }
        if part < 3 {
            if c.get(i) != Some(&'.') {
                return None;
            }
            i += 1;
        }
    }
    if c.get(i)
        .is_some_and(|ch| ch.is_ascii_digit() || (*ch == '.' && c.get(i + 1).is_some_and(|d| d.is_ascii_digit())))
    {
        return None;
    }
    Some(i)
}

fn ipv6(c: &[char]) -> Option<usize> {
    let end = c.iter().position(|ch| !(ch.is_ascii_hexdigit() || *ch == ':')).unwrap_or(c.len());
    let s: String = c[..end].iter().collect();
    let s = s.trim_end_matches(':');
    if s.matches(':').count() >= 2
        && s.parse::<std::net::Ipv6Addr>().is_ok()
        && s.chars().any(|c| c.is_ascii_hexdigit())
    {
        Some(s.chars().count())
    } else {
        None
    }
}

/// A phone number: 7 to 15 digits with separators, starting with `+`, `(` or a digit.
fn phone(c: &[char]) -> Option<usize> {
    let first = *c.first()?;
    if !(first == '+' || first == '(' || first.is_ascii_digit()) {
        return None;
    }
    let mut digits = 0;
    let mut seps = 0;
    let mut end = 0;
    let mut groups = vec![0usize];
    for (i, ch) in c.iter().enumerate() {
        match ch {
            '0'..='9' => {
                digits += 1;
                *groups.last_mut()? += 1;
                end = i + 1;
            }
            '+' if i == 0 => {}
            '(' | ')' | ' ' | '-' | '.' => {
                if c.get(i + 1).is_some_and(|n| n.is_ascii_digit() || *n == '(' || *n == ' ') {
                    seps += 1;
                    if *groups.last()? > 0 {
                        groups.push(0);
                    }
                } else if *ch == ')' {
                    end = i + 1;
                } else {
                    break;
                }
            }
            _ => break,
        }
    }
    if end < c.len() && is_word(c[end]) {
        return None;
    }
    groups.retain(|g| *g > 0);
    // A date such as 2026-11-15 or a time is not a phone number.
    if groups.len() == 3 && groups[0] == 4 && groups[1] <= 2 && groups[2] <= 2 {
        return None;
    }
    if !(7..=15).contains(&digits) || (seps == 0 && first != '+') || groups.iter().any(|g| *g > 10) {
        return None;
    }
    if first != '+' && first != '(' && groups.len() < 2 {
        return None;
    }
    Some(end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts() {
        let (s, c) = redact(
            "Mail ann.lee@example.co.uk or call +1 (555) 123-4567 from 192.168.1.20; card 4111 1111 1111 1111. Key sk-abcdefghijklmnop1234.",
        );
        assert_eq!(s, "Mail [email] or call [phone] from [ip]; card [card]. Key [secret].");
        assert_eq!(c.total(), 5);
        assert_eq!(c.describe(), "1 email address, 1 phone number, 1 IP address, 1 card number, 1 key or token");
    }

    #[test]
    fn leaves_ordinary_text() {
        let t = "On 2026-11-15 at 10:30 version 1.2.3 had 42 users and 3.5% growth, see RFC 9110 and user@localhost.";
        assert_eq!(redact(t).0, t);
        assert_eq!(redact("ünïcode — fine").0, "ünïcode — fine");
        assert_eq!(redact("a 1234567890123 b").0, "a 1234567890123 b");
    }

    #[test]
    fn ipv6_and_tokens() {
        let (s, _) = redact("from 2001:db8::1 and ghp_ABCDEFGHIJKLMNOPQRSTUVWX0123");
        assert_eq!(s, "from [ip] and [secret]");
        let (s, c) = redact("id 3f9a8b7c6d5e4f3a2b1c0d9e8f7a6b5c4d3e2f1a");
        assert_eq!((s.as_str(), c.secrets), ("id [secret]", 1));
    }
}
