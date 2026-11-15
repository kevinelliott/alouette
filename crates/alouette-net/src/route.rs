//! Which way a connection goes: direct, or through a SOCKS5 or HTTP proxy.

use std::fmt;

use alouette_core::config::Network;

use crate::NetError;

/// How to reach a host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Via {
    Direct,
    Socks5 { proxy: HostPort, remote_dns: bool, auth: Option<(String, String)> },
    Http { proxy: HostPort, auth: Option<(String, String)> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPort {
    pub host: String,
    pub port: u16,
}

impl fmt::Display for HostPort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.host.contains(':') {
            write!(f, "[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{}:{}", self.host, self.port)
        }
    }
}

impl Via {
    /// Parse `direct`, `socks5://`, `socks5h://` or `http://` URLs.
    pub fn parse(s: &str) -> Result<Self, NetError> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("direct") || s.is_empty() {
            return Ok(Via::Direct);
        }
        let url = url::Url::parse(s).map_err(|e| NetError::Config(format!("proxy `{s}`: {e}")))?;
        let host = url.host_str().ok_or_else(|| NetError::Config(format!("proxy `{s}` has no host")))?;
        let host = host.trim_start_matches('[').trim_end_matches(']').to_string();
        let auth = (!url.username().is_empty()).then(|| {
            let dec =
                |x: &str| url::form_urlencoded::parse(x.as_bytes()).map(|(k, _)| k.into_owned()).collect::<String>();
            (dec(url.username()), dec(url.password().unwrap_or("")))
        });
        match url.scheme() {
            "socks5" | "socks5h" | "socks" => Ok(Via::Socks5 {
                proxy: HostPort { host, port: url.port().unwrap_or(1080) },
                remote_dns: url.scheme() != "socks5",
                auth,
            }),
            "http" => Ok(Via::Http { proxy: HostPort { host, port: url.port().unwrap_or(8080) }, auth }),
            other => Err(NetError::Config(format!("unsupported proxy scheme `{other}` (use socks5, socks5h or http)"))),
        }
    }

    /// A short label for the connection badge.
    pub fn label(&self) -> String {
        match self {
            Via::Direct => "direct".into(),
            Via::Socks5 { proxy, .. } if proxy.port == 9050 || proxy.port == 9150 => format!("tor {proxy}"),
            Via::Socks5 { proxy, .. } => format!("socks5 {proxy}"),
            Via::Http { proxy, .. } => format!("http proxy {proxy}"),
        }
    }
}

/// Ordered routing rules plus the default.
#[derive(Debug, Clone)]
pub struct Routes {
    rules: Vec<(Vec<String>, Via)>,
    default: Via,
}

impl Routes {
    pub fn from_config(cfg: &Network) -> Result<Self, NetError> {
        let default = match &cfg.proxy {
            Some(p) => Via::parse(p)?,
            None => Via::Direct,
        };
        let rules = cfg
            .routes
            .iter()
            .map(|r| Ok((r.hosts.iter().map(|h| h.to_ascii_lowercase()).collect(), Via::parse(&r.via)?)))
            .collect::<Result<_, NetError>>()?;
        Ok(Self { rules, default })
    }

    /// The route for `host`. `.onion` names never go direct: without a matching rule
    /// they use the default proxy, or fail.
    pub fn pick(&self, host: &str) -> Result<&Via, NetError> {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        let via = self
            .rules
            .iter()
            .find(|(pats, _)| pats.iter().any(|p| host_matches(p, &host)))
            .map(|(_, v)| v)
            .unwrap_or(&self.default);
        if host.ends_with(".onion") && !matches!(via, Via::Socks5 { remote_dns: true, .. }) {
            return Err(NetError::Config(format!(
                "{host} is a Tor address; add a route such as hosts = [\"*.onion\"], via = \"socks5h://127.0.0.1:9050\""
            )));
        }
        Ok(via)
    }
}

/// `*` matches anything, `*.example.org` matches subdomains and the domain itself.
pub fn host_matches(pattern: &str, host: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(suffix) = pattern.strip_prefix("*.") {
        return host == suffix || host.ends_with(&format!(".{suffix}"));
    }
    pattern == host
}

#[cfg(test)]
mod tests {
    use super::*;
    use alouette_core::config::Route;

    #[test]
    fn parses_proxies() {
        assert_eq!(Via::parse("direct").unwrap(), Via::Direct);
        match Via::parse("socks5h://user:p%40ss@127.0.0.1:9050").unwrap() {
            Via::Socks5 { proxy, remote_dns, auth } => {
                assert_eq!(proxy.to_string(), "127.0.0.1:9050");
                assert!(remote_dns);
                assert_eq!(auth, Some(("user".into(), "p@ss".into())));
            }
            v => panic!("{v:?}"),
        }
        assert!(matches!(Via::parse("socks5://[::1]:1080").unwrap(), Via::Socks5 { remote_dns: false, .. }));
        assert!(matches!(
            Via::parse("http://proxy.local").unwrap(),
            Via::Http { proxy: HostPort { port: 8080, .. }, .. }
        ));
        assert!(Via::parse("ftp://x").is_err());
    }

    #[test]
    fn matches_hosts() {
        assert!(host_matches("*.onion", "abc.onion"));
        assert!(host_matches("*.example.org", "example.org"));
        assert!(host_matches("*.example.org", "a.b.example.org"));
        assert!(!host_matches("*.example.org", "badexample.org"));
        assert!(host_matches("*", "anything"));
    }

    #[test]
    fn routes_in_order_and_guards_onion() {
        let cfg = Network {
            routes: vec![
                Route { hosts: vec!["*.onion".into()], via: "socks5h://127.0.0.1:9050".into() },
                Route { hosts: vec!["*.corp".into()], via: "http://10.0.0.1:3128".into() },
            ],
            ..Network::default()
        };
        let r = Routes::from_config(&cfg).unwrap();
        assert_eq!(r.pick("x.onion").unwrap().label(), "tor 127.0.0.1:9050");
        assert!(matches!(r.pick("wiki.corp").unwrap(), Via::Http { .. }));
        assert_eq!(r.pick("example.org").unwrap(), &Via::Direct);

        let bare = Routes::from_config(&Network::default()).unwrap();
        assert!(bare.pick("x.onion").is_err());
    }
}
