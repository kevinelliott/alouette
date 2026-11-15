//! The individual lookups.

use std::net::IpAddr;
use std::time::{Duration, Instant};

use alouette_net::{Net, TlsOptions, Verify};
use serde_json::Value;

use crate::{Out, Section, Style};

const API_MAX: usize = 4 * 1024 * 1024;

// ------------------------------------------------------------------------- DNS

/// The common records for a name, looked up together.
pub async fn dns_report(net: &Net, name: &str) -> Section {
    let mut s = Section::new(format!("DNS ({})", net.dns().label()), format!("DNS records for {name}"));
    let types = ["A", "AAAA", "CNAME", "MX", "NS", "TXT", "CAA", "HTTPS"];
    let results = futures::future::join_all(types.iter().map(|t| net.dns().lookup(name, t))).await;
    let mut any = false;
    for (t, r) in types.iter().zip(results) {
        match r {
            Ok(answers) => {
                for a in answers.iter().filter(|a| a.rtype == *t || *t == "CNAME") {
                    any = true;
                    s.text(Style::Field, format!("{:<6} {:>6}  {}", a.rtype, a.ttl, a.data));
                }
            }
            Err(e) => {
                s.text(Style::Bad, format!("{t}: {e}"));
            }
        }
    }
    if !any && s.lines.is_empty() {
        return Section::nothing(&s.source, &s.title, "no records");
    }
    s
}

pub async fn dns_lookup(net: &Net, name: &str, rtype: &str) -> Section {
    let mut s = Section::new(format!("DNS ({})", net.dns().label()), format!("{rtype} {name}"));
    let start = Instant::now();
    match net.dns().lookup(name, rtype).await {
        Ok(a) if a.is_empty() => {
            s.text(Style::Dim, "no records");
        }
        Ok(a) => {
            for r in a {
                s.text(Style::Field, format!("{:<28} {:>6}  {:<6} {}", r.name, r.ttl, r.rtype, r.data));
            }
        }
        Err(e) => {
            s.text(Style::Bad, e.to_string());
        }
    }
    s.text(Style::Dim, format!("answered in {} ms", start.elapsed().as_millis()));
    s
}

pub async fn reverse_dns(net: &Net, ip: IpAddr) -> Section {
    let title = format!("Reverse DNS for {ip}");
    match net.dns().lookup(&ip.to_string(), "PTR").await {
        Ok(a) if a.is_empty() => Section::nothing("DNS", &title, "no PTR record"),
        Ok(a) => {
            let mut s = Section::new("DNS", title);
            for r in a {
                s.text(Style::Field, format!("PTR    {}", r.data));
            }
            s
        }
        Err(e) => Section::error("DNS", &title, e),
    }
}

// ------------------------------------------------------------------------- WHOIS

async fn whois_raw(net: &Net, server: &str, q: &str) -> Result<String, String> {
    let mut c = net.tcp(server, proto_whois::PORT).await.map_err(|e| e.to_string())?;
    proto_whois::query(&mut c.stream, q).await.map_err(|e| e.to_string())
}

/// Ask IANA, then follow referrals (at most two) to the authoritative server.
pub async fn whois(net: &Net, q: &str) -> Section {
    let title = format!("WHOIS {q}");
    let mut server = proto_whois::IANA.to_string();
    let mut answer = match whois_raw(net, &server, q).await {
        Ok(a) => a,
        Err(e) => return Section::error("WHOIS", &title, e),
    };
    for _ in 0..2 {
        match proto_whois::referral(&answer) {
            Some(next) if next != server => match whois_raw(net, &next, q).await {
                Ok(a) => {
                    server = next;
                    answer = a;
                }
                Err(_) => break,
            },
            _ => break,
        }
    }
    let mut s = Section::new("WHOIS", title);
    s.text(Style::Dim, format!("from {server}"));
    let useful: String = answer
        .lines()
        .filter(|l| {
            !l.trim().is_empty()
                && !l.starts_with('%')
                && !l.starts_with('#')
                && !l.starts_with(">>>")
                && !l.contains("TERMS OF USE")
        })
        .take(60)
        .collect::<Vec<_>>()
        .join("\n");
    s.block(&useful, 60);
    s
}

/// IP or AS number details from Team Cymru.
pub async fn asn(net: &Net, target: &str) -> Section {
    let title = format!("ASN and BGP for {target}");
    match whois_raw(net, proto_whois::CYMRU, &proto_whois::cymru_query(target)).await {
        Ok(a) => {
            let rows = proto_whois::parse_cymru(&a);
            if rows.is_empty() {
                return Section::nothing("Team Cymru", &title, "not announced in BGP");
            }
            let mut s = Section::new("Team Cymru", title);
            for r in rows {
                s.field("AS", format!("AS{} {}", r.asn, r.name));
                if let Some(p) = r.prefix {
                    s.field("BGP prefix", p);
                }
                s.field(
                    "Registry",
                    format!("{} ({}), allocated {}", r.registry.to_uppercase(), r.country, r.allocated),
                );
            }
            s
        }
        Err(e) => Section::error("Team Cymru", &title, e),
    }
}

// ------------------------------------------------------------------------- RDAP

/// `kind` is `domain`, `ip` or `autnum`. Uses the rdap.org bootstrap redirector.
pub async fn rdap(net: &Net, kind: &str, q: &str) -> Section {
    let title = format!("RDAP {q}");
    let url = format!("https://rdap.org/{kind}/{q}");
    let resp = match net.get(&url, &[("Accept", "application/rdap+json, application/json")], API_MAX).await {
        Ok(r) => r,
        Err(e) => return Section::error("RDAP", &title, e),
    };
    if resp.status == 404 {
        return Section::nothing("RDAP", &title, "not found in RDAP");
    }
    if !resp.ok() {
        return Section::error("RDAP", &title, format!("HTTP {}", resp.status));
    }
    let v: Value = match serde_json::from_slice(&resp.body) {
        Ok(v) => v,
        Err(e) => return Section::error("RDAP", &title, format!("bad JSON: {e}")),
    };
    let mut s = Section::new("RDAP", title);
    rdap_fields(&v, &mut s);
    s.text(Style::Dim, format!("from {}", host_of(&resp.url)));
    s
}

pub fn rdap_fields(v: &Value, s: &mut Section) {
    let str_of = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    if let Some(n) = str_of("ldhName").or_else(|| str_of("name")) {
        s.field("Name", n);
    }
    if let (Some(a), Some(b)) = (str_of("startAddress"), str_of("endAddress")) {
        s.field("Range", format!("{a} – {b}"));
    }
    if let (Some(a), Some(b)) =
        (v.get("startAutnum").and_then(Value::as_u64), v.get("endAutnum").and_then(Value::as_u64))
    {
        s.field("AS numbers", if a == b { format!("AS{a}") } else { format!("AS{a} – AS{b}") });
    }
    if let Some(c) = str_of("country") {
        s.field("Country", c);
    }
    if let Some(t) = str_of("type") {
        s.field("Type", t);
    }
    if let Some(st) = v.get("status").and_then(Value::as_array) {
        let st: Vec<&str> = st.iter().filter_map(Value::as_str).collect();
        s.field("Status", st.join(", "));
    }
    if let Some(events) = v.get("events").and_then(Value::as_array) {
        for e in events {
            let action = e.get("eventAction").and_then(Value::as_str).unwrap_or("");
            let date = e.get("eventDate").and_then(Value::as_str).unwrap_or("");
            let label = match action {
                "registration" => "Registered",
                "expiration" => "Expires",
                "last changed" => "Changed",
                _ => continue,
            };
            s.field(label, date.split('T').next().unwrap_or(date));
        }
    }
    if let Some(ents) = v.get("entities").and_then(Value::as_array) {
        for e in ents {
            let roles: Vec<&str> = e
                .get("roles")
                .and_then(Value::as_array)
                .map(|r| r.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            if let Some(name) = vcard_name(e) {
                let role = roles.first().copied().unwrap_or("entity");
                let mut role = role.to_string();
                if let Some(f) = role.get_mut(..1) {
                    f.make_ascii_uppercase();
                }
                s.field(&role, name);
            }
        }
    }
    if let Some(ns) = v.get("nameservers").and_then(Value::as_array) {
        let names: Vec<String> =
            ns.iter().filter_map(|n| n.get("ldhName").and_then(Value::as_str)).map(str::to_lowercase).collect();
        if !names.is_empty() {
            s.field("Nameservers", names.join(", "));
        }
    }
    if let Some(d) = v.get("secureDNS").and_then(|d| d.get("delegationSigned")).and_then(Value::as_bool) {
        s.field("DNSSEC", if d { "signed" } else { "unsigned" });
    }
}

fn vcard_name(entity: &Value) -> Option<String> {
    let arr = entity.get("vcardArray")?.as_array()?.get(1)?.as_array()?;
    arr.iter().find_map(|p| {
        let p = p.as_array()?;
        (p.first()?.as_str()? == "fn").then(|| p.get(3)?.as_str().map(str::to_string)).flatten()
    })
}

fn host_of(url: &str) -> String {
    url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_else(|| url.to_string())
}

// ------------------------------------------------------------------------- CT

/// Names seen in certificate transparency logs, through crt.sh.
pub async fn certificate_transparency(net: &Net, domain: &str) -> Section {
    let title = format!("Certificates issued for {domain}");
    let url = format!("https://crt.sh/?q={}&output=json&exclude=expired", urlenc(&format!("%.{domain}")));
    let resp = match net.get(&url, &[("Accept", "application/json")], 16 * 1024 * 1024).await {
        Ok(r) if r.ok() => r,
        Ok(r) => return Section::error("crt.sh", &title, format!("HTTP {}", r.status)),
        Err(e) => return Section::error("crt.sh", &title, e),
    };
    let v: Value = serde_json::from_slice(&resp.body).unwrap_or(Value::Null);
    let Some(entries) = v.as_array() else { return Section::nothing("crt.sh", &title, "no certificates") };
    let mut names: Vec<String> = entries
        .iter()
        .filter_map(|e| e.get("name_value").and_then(Value::as_str))
        .flat_map(|n| n.split('\n').map(|x| x.trim().to_lowercase()))
        .collect();
    names.sort();
    names.dedup();
    let mut issuers: Vec<String> = entries
        .iter()
        .filter_map(|e| e.get("issuer_name").and_then(Value::as_str))
        .map(|i| i.split(", ").find_map(|p| p.strip_prefix("O=")).unwrap_or(i).to_string())
        .collect();
    issuers.sort();
    issuers.dedup();
    let mut s = Section::new("crt.sh", title);
    s.field("Certificates", entries.len());
    s.field("Issuers", issuers.join(", "));
    s.text(Style::Heading, format!("{} names", names.len()));
    for n in names.iter().take(40) {
        s.text(Style::Normal, n.clone());
    }
    if names.len() > 40 {
        s.text(Style::Dim, format!("… {} more", names.len() - 40));
    }
    s
}

pub fn urlenc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

// ------------------------------------------------------------------------- people

/// WebFinger (RFC 7033), as fediverse servers use.
pub async fn webfinger(net: &Net, user: &str, host: &str) -> Section {
    let title = format!("WebFinger {user}@{host}");
    let url = format!("https://{host}/.well-known/webfinger?resource={}", urlenc(&format!("acct:{user}@{host}")));
    let resp = match net.get(&url, &[("Accept", "application/jrd+json, application/json")], API_MAX).await {
        Ok(r) => r,
        Err(e) => return Section::error("WebFinger", &title, e),
    };
    if !resp.ok() {
        return Section::nothing("WebFinger", &title, &format!("no account (HTTP {})", resp.status));
    }
    let Ok(v) = serde_json::from_slice::<Value>(&resp.body) else {
        return Section::error("WebFinger", &title, "bad JSON");
    };
    let mut s = Section::new("WebFinger", title);
    if let Some(sub) = v.get("subject").and_then(Value::as_str) {
        s.field("Subject", sub);
    }
    if let Some(aliases) = v.get("aliases").and_then(Value::as_array) {
        for a in aliases.iter().filter_map(Value::as_str) {
            s.push(Out::link(format!("Alias          {a}"), a));
        }
    }
    if let Some(links) = v.get("links").and_then(Value::as_array) {
        for l in links {
            let rel = l.get("rel").and_then(Value::as_str).unwrap_or("");
            let rel = rel.rsplit('/').next().unwrap_or(rel);
            if let Some(h) = l.get("href").and_then(Value::as_str) {
                s.push(Out::link(format!("{rel:<14} {h}"), h));
            }
        }
    }
    s
}

pub async fn finger(net: &Net, user: &str, host: &str) -> Section {
    let title = format!("Finger {user}@{host}");
    let q =
        proto_finger::Query { user: user.into(), host: host.into(), port: proto_finger::DEFAULT_PORT, verbose: false };
    let mut c = match net.tcp(host, q.port).await {
        Ok(c) => c,
        Err(e) => return Section::nothing("Finger", &title, &format!("no finger server ({e})")),
    };
    match proto_finger::fetch(&mut c.stream, &q, 256 * 1024).await {
        Ok(t) if t.trim().is_empty() => Section::nothing("Finger", &title, "empty answer"),
        Ok(t) => {
            let mut s = Section::new("Finger", title);
            s.block(&t, 40);
            s.push(Out::link("Open in Burrow", q.url()));
            s
        }
        Err(e) => Section::error("Finger", &title, e),
    }
}

/// z-base-32, as OpenPGP WKD uses for hashed local parts.
pub fn zbase32(data: &[u8]) -> String {
    const A: &[u8; 32] = b"ybndrfg8ejkmcpqxot1uwisza345h769";
    let mut out = String::new();
    let mut buf: u32 = 0;
    let mut bits = 0;
    for &b in data {
        buf = (buf << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            out.push(A[((buf >> (bits - 5)) & 31) as usize] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        out.push(A[((buf << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// Look for an OpenPGP key by Web Key Directory, advanced method first.
pub async fn openpgp_wkd(net: &Net, user: &str, host: &str) -> Section {
    use sha1::{Digest, Sha1};
    let title = format!("OpenPGP key for {user}@{host}");
    let hash = zbase32(&Sha1::digest(user.to_lowercase().as_bytes()));
    let local = urlenc(user);
    let urls = [
        format!("https://openpgpkey.{host}/.well-known/openpgpkey/{host}/hu/{hash}?l={local}"),
        format!("https://{host}/.well-known/openpgpkey/hu/{hash}?l={local}"),
    ];
    for u in urls {
        if let Ok(r) = net.get(&u, &[], 1024 * 1024).await
            && r.ok()
            && !r.body.is_empty()
        {
            let mut s = Section::new("OpenPGP WKD", title);
            s.text(Style::Good, format!("Key found ({} bytes)", r.body.len()));
            s.field("From", host_of(&u));
            return s;
        }
    }
    Section::nothing("OpenPGP WKD", &title, "no key published")
}

// ------------------------------------------------------------------------- words

pub async fn dict(net: &Net, word: &str) -> Section {
    let title = format!("Definitions of {word}");
    let c = match net.tcp(proto_dict::DEFAULT_SERVER, proto_dict::PORT).await {
        Ok(c) => c,
        Err(e) => return Section::error("DICT", &title, e),
    };
    match proto_dict::define(c.stream, word, "*").await {
        Ok(d) if d.is_empty() => Section::nothing("DICT", &title, "no definitions"),
        Ok(defs) => {
            let mut s = Section::new("DICT", title);
            for d in defs.iter().take(4) {
                s.text(Style::Heading, d.database_name.clone());
                s.block(&d.text, 25);
            }
            if defs.len() > 4 {
                s.text(Style::Dim, format!("… {} more dictionaries", defs.len() - 4));
            }
            s
        }
        Err(e) => Section::error("DICT", &title, e),
    }
}

/// Search all of Gopherspace with Veronica-2.
pub async fn veronica(net: &Net, words: &str) -> Section {
    let title = format!("Gopherspace: {words}");
    let url =
        proto_gopher::GopherUrl::parse("gopher://gopher.floodgap.com/7/v2/vs").expect("static url").with_search(words);
    let mut c = match net.tcp(&url.host, url.port).await {
        Ok(c) => c,
        Err(e) => return Section::error("Veronica-2", &title, e),
    };
    let body = match proto_gopher::fetch(&mut c.stream, &url, 2 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => return Section::error("Veronica-2", &title, e),
    };
    let items: Vec<_> = proto_gopher::parse_menu(&String::from_utf8_lossy(&body))
        .into_iter()
        .filter(|i| !matches!(i.kind, proto_gopher::ItemType::Info | proto_gopher::ItemType::Error))
        .collect();
    if items.is_empty() {
        return Section::nothing("Veronica-2", &title, "no matches");
    }
    let mut s = Section::new("Veronica-2", title);
    for i in items.iter().take(25) {
        if let Some(u) = i.url(false) {
            s.push(Out::link(format!("{:<4} {}", i.kind.label(), i.display), u));
        }
    }
    s.push(Out::link(format!("All {} results in Burrow", items.len()), url.to_string()));
    s
}

/// Search Geminispace with Kennedy.
pub async fn gemini_search(net: &Net, words: &str) -> Section {
    let title = format!("Geminispace: {words}");
    let base = proto_gemini::parse_url("gemini://kennedy.gemi.dev/search").expect("static url");
    let url = proto_gemini::with_query(&base, words);
    let host = "kennedy.gemi.dev";
    let opts = TlsOptions { verify: Verify::Tofu, ..TlsOptions::default() };
    let mut c = match net.tls(host, proto_gemini::DEFAULT_PORT, &opts).await {
        Ok(c) => c,
        Err(e) => return Section::error("Kennedy", &title, e),
    };
    let r = match proto_gemini::fetch(&mut c.stream, &url, 2 * 1024 * 1024).await {
        Ok(r) if r.header.status / 10 == 2 => r,
        Ok(r) => return Section::error("Kennedy", &title, r.header.describe()),
        Err(e) => return Section::error("Kennedy", &title, e),
    };
    let lines = proto_gemini::gemtext::parse(&r.text());
    let links: Vec<(String, String)> = lines
        .into_iter()
        .filter_map(|l| match l {
            proto_gemini::gemtext::Line::Link { url: u, label } => {
                let abs = url.join(&u).ok()?.to_string();
                (abs.starts_with("gemini://") && !abs.contains("kennedy.gemi.dev")).then(|| (label.unwrap_or(u), abs))
            }
            _ => None,
        })
        .collect();
    if links.is_empty() {
        return Section::nothing("Kennedy", &title, "no matches");
    }
    let mut s = Section::new("Kennedy", title);
    for (label, u) in links.iter().take(20) {
        s.push(Out::link(label.clone(), u.clone()));
    }
    s.push(Out::link("All results in Burrow", url.to_string()));
    s
}

// ------------------------------------------------------------------------- probes

/// How long a TCP handshake takes, which works everywhere and through firewalls that
/// drop ICMP.
pub async fn tcp_ping(net: &Net, host: &str, port: u16) -> Result<(Duration, Option<std::net::SocketAddr>), String> {
    let start = Instant::now();
    let c = net.tcp(host, port).await.map_err(|e| e.to_string())?;
    Ok((start.elapsed(), c.info.peer))
}

/// Well-known ports a port check tries by default.
pub const COMMON_PORTS: &[(u16, &str)] = &[
    (21, "ftp"),
    (22, "ssh"),
    (23, "telnet"),
    (25, "smtp"),
    (53, "dns"),
    (70, "gopher"),
    (79, "finger"),
    (80, "http"),
    (110, "pop3"),
    (119, "nntp"),
    (143, "imap"),
    (443, "https"),
    (465, "smtps"),
    (587, "submission"),
    (853, "dns-over-tls"),
    (993, "imaps"),
    (995, "pop3s"),
    (1965, "gemini"),
    (2628, "dict"),
    (3306, "mysql"),
    (5432, "postgres"),
    (6697, "ircs"),
    (8080, "http-alt"),
    (8443, "https-alt"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortState {
    Open(Duration),
    Closed,
    /// No answer before the timeout: usually a firewall.
    Filtered,
}

pub async fn check_port(net: &Net, host: &str, port: u16, timeout: Duration) -> PortState {
    let start = Instant::now();
    match tokio::time::timeout(timeout, net.tcp(host, port)).await {
        Ok(Ok(_)) => PortState::Open(start.elapsed()),
        Ok(Err(e)) if e.to_string().to_lowercase().contains("refused") => PortState::Closed,
        Ok(Err(_)) => PortState::Closed,
        Err(_) => PortState::Filtered,
    }
}

pub fn port_name(port: u16) -> &'static str {
    COMMON_PORTS.iter().find(|(p, _)| *p == port).map(|(_, n)| *n).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zbase32_matches_wkd_spec() {
        // draft-koch-openpgp-webkey-service: "Joe.Doe" lowercased -> iy9q119eutrkn8s1mk4r39qejnbu3n5q
        use sha1::{Digest, Sha1};
        assert_eq!(zbase32(&Sha1::digest(b"joe.doe")), "iy9q119eutrkn8s1mk4r39qejnbu3n5q");
    }

    #[test]
    fn rdap_fields_read_domains() {
        let v: Value = serde_json::from_str(
            r#"{"ldhName":"EXAMPLE.ORG","status":["client transfer prohibited"],
                "events":[{"eventAction":"registration","eventDate":"1995-08-31T04:00:00Z"},
                          {"eventAction":"expiration","eventDate":"2030-08-30T04:00:00Z"}],
                "entities":[{"roles":["registrar"],"vcardArray":["vcard",[["version",{},"text","4.0"],["fn",{},"text","Example Registrar"]]]}],
                "nameservers":[{"ldhName":"A.IANA-SERVERS.NET"}],
                "secureDNS":{"delegationSigned":true}}"#,
        )
        .unwrap();
        let mut s = Section::new("RDAP", "t");
        rdap_fields(&v, &mut s);
        let text: Vec<&str> = s.lines.iter().map(|l| l.text.as_str()).collect();
        assert!(text.iter().any(|t| t.contains("Registered") && t.contains("1995-08-31")));
        assert!(text.iter().any(|t| t.starts_with("Registrar") && t.contains("Example Registrar")));
        assert!(text.iter().any(|t| t.contains("a.iana-servers.net")));
        assert!(text.iter().any(|t| t.contains("signed")));
    }

    #[tokio::test]
    async fn port_states() {
        let net = Net::new(&Default::default()).unwrap();
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let open = l.local_addr().unwrap().port();
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        assert!(matches!(check_port(&net, "127.0.0.1", open, Duration::from_secs(2)).await, PortState::Open(_)));
        assert_eq!(check_port(&net, "127.0.0.1", closed, Duration::from_secs(2)).await, PortState::Closed);
        assert!(tcp_ping(&net, "127.0.0.1", open).await.is_ok());
    }
}
