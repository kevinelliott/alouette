//! WHOIS (RFC 3912): queries, IANA referrals and Team Cymru's IP-to-ASN service.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const PORT: u16 = 43;
pub const IANA: &str = "whois.iana.org";
pub const CYMRU: &str = "whois.cymru.com";
pub const MAX: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("the answer is larger than {0} bytes")]
    TooLarge(usize),
}

/// Send one query and read the answer.
pub async fn query<S: AsyncRead + AsyncWrite + Unpin>(s: &mut S, q: &str) -> Result<String, Error> {
    let q: String = q.chars().filter(|c| !c.is_control()).collect();
    s.write_all(format!("{q}\r\n").as_bytes()).await?;
    s.flush().await?;
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = match s.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        };
        if out.len() + n > MAX {
            return Err(Error::TooLarge(MAX));
        }
        out.extend_from_slice(&buf[..n]);
    }
    Ok(String::from_utf8_lossy(&out).replace("\r\n", "\n"))
}

/// The server a WHOIS answer refers the query on to, if any (`refer:`, `whois:` or
/// `Registrar WHOIS Server:` lines).
pub fn referral(answer: &str) -> Option<String> {
    for line in answer.lines() {
        let l = line.trim();
        let Some((k, v)) = l.split_once(':') else { continue };
        let k = k.trim().to_ascii_lowercase();
        if matches!(k.as_str(), "refer" | "whois" | "registrar whois server" | "referralserver") {
            let v = v.trim().trim_start_matches("whois://").trim_end_matches('/');
            let v = v.split(':').next().unwrap_or(v).trim();
            if !v.is_empty() && v.contains('.') {
                return Some(v.to_ascii_lowercase());
            }
        }
    }
    None
}

/// The query to send to Team Cymru for an IP address or `AS` number, in verbose form.
pub fn cymru_query(target: &str) -> String {
    format!(" -v {}", target.trim())
}

/// One row of a Team Cymru answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsnInfo {
    pub asn: String,
    pub prefix: Option<String>,
    pub country: String,
    pub registry: String,
    pub allocated: String,
    pub name: String,
}

/// Parse Team Cymru's `|`-separated verbose output, for either an IP or an AS query.
pub fn parse_cymru(answer: &str) -> Vec<AsnInfo> {
    let mut out = vec![];
    for line in answer.lines() {
        let cols: Vec<&str> = line.split('|').map(str::trim).collect();
        if cols.first().is_some_and(|c| c.eq_ignore_ascii_case("AS") || c.starts_with("Bulk")) {
            continue;
        }
        match cols.len() {
            // IP: AS | IP | BGP Prefix | CC | Registry | Allocated | AS Name
            7 => out.push(AsnInfo {
                asn: cols[0].into(),
                prefix: Some(cols[2].into()),
                country: cols[3].into(),
                registry: cols[4].into(),
                allocated: cols[5].into(),
                name: cols[6].into(),
            }),
            // AS: AS | CC | Registry | Allocated | AS Name
            5 => out.push(AsnInfo {
                asn: cols[0].into(),
                prefix: None,
                country: cols[1].into(),
                registry: cols[2].into(),
                allocated: cols[3].into(),
                name: cols[4].into(),
            }),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_referrals() {
        assert_eq!(
            referral("% IANA WHOIS server\nrefer:        whois.verisign-grs.com\n"),
            Some("whois.verisign-grs.com".into())
        );
        assert_eq!(
            referral("   Registrar WHOIS Server: whois.markmonitor.com\n"),
            Some("whois.markmonitor.com".into())
        );
        assert_eq!(referral("ReferralServer:  whois://whois.ripe.net\n"), Some("whois.ripe.net".into()));
        assert_eq!(referral("Domain Name: EXAMPLE.COM\n"), None);
    }

    #[test]
    fn parses_cymru() {
        let ip = "AS      | IP               | BGP Prefix          | CC | Registry | Allocated  | AS Name\n\
                  15169   | 8.8.8.8          | 8.8.8.0/24          | US | arin     | 2023-12-28 | GOOGLE, US\n";
        let r = parse_cymru(ip);
        assert_eq!(r[0].asn, "15169");
        assert_eq!(r[0].prefix.as_deref(), Some("8.8.8.0/24"));
        let asn = "AS      | CC | Registry | Allocated  | AS Name\n13335   | US | arin     | 2010-07-14 | CLOUDFLARENET, US\n";
        assert_eq!(parse_cymru(asn)[0].name, "CLOUDFLARENET, US");
    }

    #[tokio::test]
    async fn queries() {
        let (mut c, mut s) = tokio::io::duplex(1024);
        tokio::spawn(async move {
            let mut buf = [0u8; 64];
            let n = s.read(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], b"example.org\r\n");
            s.write_all(b"Domain: EXAMPLE.ORG\r\n").await.unwrap();
        });
        assert_eq!(query(&mut c, "example.org").await.unwrap(), "Domain: EXAMPLE.ORG\n");
    }
}
