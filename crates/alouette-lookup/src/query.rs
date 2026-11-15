//! Working out what someone typed into Inquire.

use std::net::IpAddr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryKind {
    Ip(IpAddr),
    /// An autonomous system number, from `AS13335` or `asn 13335`.
    Asn(u32),
    Domain(String),
    /// `user@host`, which may be an email address, a fediverse account or a finger target.
    Address {
        user: String,
        host: String,
    },
    Url(url::Url),
    /// Anything else: words to define or search for.
    Words(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub raw: String,
    pub kind: QueryKind,
}

impl Query {
    pub fn parse(input: &str) -> Self {
        let raw = input.trim().to_string();
        let s = raw.as_str();
        let lower = s.to_ascii_lowercase();
        let kind = if let Ok(ip) = s.trim_start_matches('[').trim_end_matches(']').parse::<IpAddr>() {
            QueryKind::Ip(ip)
        } else if let Some(n) =
            ["asn ", "asn", "as"].iter().find_map(|p| lower.strip_prefix(p)?.trim().parse::<u32>().ok())
        {
            QueryKind::Asn(n)
        } else if s.contains("://")
            && let Ok(u) = url::Url::parse(s)
        {
            QueryKind::Url(u)
        } else if !s.contains(char::is_whitespace)
            && let Some((user, host)) = s.trim_start_matches('@').trim_start_matches("acct:").rsplit_once('@')
            && is_domain(host)
        {
            QueryKind::Address { user: user.to_string(), host: host.to_ascii_lowercase() }
        } else if is_domain(s) {
            QueryKind::Domain(lower.trim_end_matches('.').to_string())
        } else {
            QueryKind::Words(raw.clone())
        };
        Self { raw, kind }
    }

    /// The host a query is about, if any.
    pub fn host(&self) -> Option<String> {
        match &self.kind {
            QueryKind::Domain(d) => Some(d.clone()),
            QueryKind::Address { host, .. } => Some(host.clone()),
            QueryKind::Url(u) => u.host_str().map(str::to_string),
            _ => None,
        }
    }
}

/// A plausible DNS name with at least one dot and a letter in the last label.
pub fn is_domain(s: &str) -> bool {
    let s = s.trim_end_matches('.');
    if s.len() > 253 || !s.contains('.') {
        return false;
    }
    let labels: Vec<&str> = s.split('.').collect();
    labels
        .iter()
        .all(|l| !l.is_empty() && l.len() <= 63 && l.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_'))
        && labels.last().is_some_and(|t| t.chars().any(char::is_alphabetic))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_kinds() {
        assert!(matches!(Query::parse("192.0.2.7").kind, QueryKind::Ip(_)));
        assert!(matches!(Query::parse("[2001:db8::1]").kind, QueryKind::Ip(_)));
        assert_eq!(Query::parse("AS13335").kind, QueryKind::Asn(13335));
        assert_eq!(Query::parse("asn 15169").kind, QueryKind::Asn(15169));
        assert_eq!(Query::parse("Example.ORG.").kind, QueryKind::Domain("example.org".into()));
        assert_eq!(
            Query::parse("@kev@mastodon.social").kind,
            QueryKind::Address { user: "kev".into(), host: "mastodon.social".into() }
        );
        assert!(matches!(Query::parse("gemini://geminiprotocol.net/").kind, QueryKind::Url(_)));
        assert_eq!(Query::parse("minuet").kind, QueryKind::Words("minuet".into()));
        assert_eq!(Query::parse("university of minnesota").kind, QueryKind::Words("university of minnesota".into()));
        assert!(matches!(Query::parse("1.2").kind, QueryKind::Words(_)));
        assert!(matches!(Query::parse("asparagus").kind, QueryKind::Words(_)));
    }
}
