//! The NetTools command line: `ping example.org`, `dns MX example.org`, `ports host 22,80`.

use alouette_lookup::sources::COMMON_PORTS;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Ping { host: String, count: u32, tcp_port: Option<u16> },
    Trace { host: String },
    Dns { rtype: String, name: String },
    Whois { query: String },
    Rdap { query: String },
    Asn { query: String },
    Ports { host: String, ports: Vec<u16> },
    Tls { host: String, port: u16 },
    Mdns,
}

pub const HELP: &[(&str, &str)] = &[
    ("ping HOST [COUNT] [tcp [PORT]]", "ICMP echo, or TCP handshake time"),
    ("trace HOST", "the route packets take (system traceroute)"),
    ("dns [TYPE] NAME", "records of one type: A, AAAA, MX, TXT, NS, CAA, HTTPS…"),
    ("whois QUERY", "WHOIS with referrals from IANA"),
    ("rdap QUERY", "registration data for a domain, IP or AS"),
    ("asn IP|ASn", "AS number, BGP prefix and owner"),
    ("ports HOST [22,80,8000-8010]", "which TCP ports answer"),
    ("tls HOST[:PORT]", "protocol, key exchange and certificate chain"),
    ("mdns", "devices and services on the local network"),
];

impl Command {
    pub fn parse(input: &str) -> Result<Self, String> {
        let mut words = input.split_whitespace();
        let verb = words.next().ok_or("type a command, such as: ping example.org")?.to_ascii_lowercase();
        let rest: Vec<&str> = words.collect();
        let need = |what: &str| -> Result<String, String> {
            rest.first().map(|s| s.to_string()).ok_or_else(|| format!("{verb} needs {what}"))
        };
        Ok(match verb.as_str() {
            "ping" => {
                let host = need("a host")?;
                let mut count = 5;
                let mut tcp_port = None;
                let mut it = rest.iter().skip(1).peekable();
                while let Some(w) = it.next() {
                    if w.eq_ignore_ascii_case("tcp") {
                        tcp_port = Some(it.peek().and_then(|p| p.parse().ok()).unwrap_or(443));
                        if it.peek().is_some_and(|p| p.parse::<u16>().is_ok()) {
                            it.next();
                        }
                    } else if let Ok(n) = w.parse::<u32>() {
                        count = n.clamp(1, 1000);
                    }
                }
                Command::Ping { host, count, tcp_port }
            }
            "trace" | "traceroute" | "tracert" => Command::Trace { host: need("a host")? },
            "dns" | "dig" | "lookup" | "nslookup" => match rest.as_slice() {
                [] => return Err("dns needs a name".into()),
                [name] => Command::Dns { rtype: "A".into(), name: name.to_string() },
                [a, b, ..] if alouette_net::dns::RECORD_TYPES.contains(&a.to_ascii_uppercase().as_str()) => {
                    Command::Dns { rtype: a.to_ascii_uppercase(), name: b.to_string() }
                }
                [a, b, ..] => Command::Dns { rtype: b.to_ascii_uppercase(), name: a.to_string() },
            },
            "whois" => Command::Whois { query: need("a domain, IP or AS")? },
            "rdap" => Command::Rdap { query: need("a domain, IP or AS")? },
            "asn" | "bgp" => Command::Asn { query: need("an IP or AS number")? },
            "ports" | "scan" | "port" => {
                let host = need("a host")?;
                let ports = match rest.get(1) {
                    Some(spec) => parse_ports(spec)?,
                    None => COMMON_PORTS.iter().map(|(p, _)| *p).collect(),
                };
                Command::Ports { host, ports }
            }
            "tls" | "cert" | "ssl" => {
                let target = need("a host")?;
                let (host, port) = split_host_port(&target, rest.get(1).and_then(|p| p.parse().ok()).unwrap_or(443));
                Command::Tls { host, port }
            }
            "mdns" | "bonjour" | "discover" => Command::Mdns,
            other => {
                return Err(format!(
                    "unknown command {other}; try ping, trace, dns, whois, rdap, asn, ports, tls or mdns"
                ));
            }
        })
    }

    /// The `net:` URL for this command.
    pub fn url(&self) -> String {
        match self {
            Command::Ping { host, .. } => format!("net:ping/{host}"),
            Command::Trace { host } => format!("net:trace/{host}"),
            Command::Dns { rtype, name } => format!("net:dns/{rtype}/{name}"),
            Command::Whois { query } => format!("net:whois/{query}"),
            Command::Rdap { query } => format!("net:rdap/{query}"),
            Command::Asn { query } => format!("net:asn/{query}"),
            Command::Ports { host, .. } => format!("net:ports/{host}"),
            Command::Tls { host, port } => format!("net:tls/{host}:{port}"),
            Command::Mdns => "net:mdns".into(),
        }
    }
}

/// `net:ping/example.org` becomes `ping example.org`.
pub fn from_url_path(path: &str) -> String {
    let path = path.trim_matches('/');
    let decoded: String = url::form_urlencoded::parse(path.as_bytes())
        .map(|(k, v)| if v.is_empty() { k.into_owned() } else { format!("{k}={v}") })
        .collect();
    decoded.replace('/', " ")
}

pub fn split_host_port(s: &str, default: u16) -> (String, u16) {
    if let Some(rest) = s.strip_prefix('[')
        && let Some((h, p)) = rest.split_once(']')
    {
        return (h.to_string(), p.trim_start_matches(':').parse().unwrap_or(default));
    }
    match s.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => (h.to_string(), p.parse().unwrap_or(default)),
        _ => (s.to_string(), default),
    }
}

pub fn parse_ports(spec: &str) -> Result<Vec<u16>, String> {
    let mut out = vec![];
    for part in spec.split(',').filter(|p| !p.is_empty()) {
        match part.split_once('-') {
            Some((a, b)) => {
                let (a, b): (u16, u16) =
                    (a.parse().map_err(|_| format!("bad port {a}"))?, b.parse().map_err(|_| format!("bad port {b}"))?);
                if b < a || b - a > 2048 {
                    return Err(format!("range {part} is too wide (2048 ports at most)"));
                }
                out.extend(a..=b);
            }
            None => out.push(part.parse().map_err(|_| format!("bad port {part}"))?),
        }
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands() {
        assert_eq!(
            Command::parse("ping example.org").unwrap(),
            Command::Ping { host: "example.org".into(), count: 5, tcp_port: None }
        );
        assert_eq!(
            Command::parse("ping example.org 3 tcp 80").unwrap(),
            Command::Ping { host: "example.org".into(), count: 3, tcp_port: Some(80) }
        );
        assert_eq!(
            Command::parse("dns MX umn.edu").unwrap(),
            Command::Dns { rtype: "MX".into(), name: "umn.edu".into() }
        );
        assert_eq!(
            Command::parse("dig umn.edu txt").unwrap(),
            Command::Dns { rtype: "TXT".into(), name: "umn.edu".into() }
        );
        assert_eq!(Command::parse("tls [::1]:8443").unwrap(), Command::Tls { host: "::1".into(), port: 8443 });
        assert_eq!(Command::parse("tls example.org").unwrap(), Command::Tls { host: "example.org".into(), port: 443 });
        assert_eq!(
            Command::parse("ports h 22,80,8000-8002").unwrap(),
            Command::Ports { host: "h".into(), ports: vec![22, 80, 8000, 8001, 8002] }
        );
        assert!(Command::parse("ports h 1-60000").is_err());
        assert!(Command::parse("frobnicate").is_err());
        assert_eq!(from_url_path("/dns/MX/umn.edu"), "dns MX umn.edu");
    }
}
