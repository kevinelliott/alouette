//! Name resolution: the system resolver, or encrypted DNS (DoH, DoT, DoQ) through Hickory.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use alouette_core::config::Network;
use hickory_resolver::TokioResolver;
use hickory_resolver::config::{
    CLOUDFLARE, GOOGLE, LookupIpStrategy, NameServerConfig, QUAD9, ResolverConfig, ServerGroup,
};
use hickory_resolver::net::runtime::TokioRuntimeProvider;

use crate::NetError;

#[derive(Clone)]
pub enum Dns {
    System,
    Encrypted { resolver: Arc<TokioResolver>, label: &'static str },
}

impl std::fmt::Debug for Dns {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

impl Dns {
    pub fn from_config(cfg: &Network) -> Result<Self, NetError> {
        let mode = cfg.dns.to_ascii_lowercase();
        let label = match mode.as_str() {
            "system" | "" => return Ok(Dns::System),
            "doh" | "https" => "DoH",
            "dot" | "tls" => "DoT",
            "doq" | "quic" => "DoQ",
            other => return Err(NetError::Config(format!("unknown dns mode `{other}` (system, doh, dot or doq)"))),
        };
        let servers = name_servers(label, cfg.dns_server.as_deref().unwrap_or("cloudflare"))?;
        let config = ResolverConfig::from_parts(None, vec![], servers);
        let mut builder = TokioResolver::builder_with_config(config, TokioRuntimeProvider::default());
        builder.options_mut().ip_strategy =
            if cfg.prefer_ipv6 { LookupIpStrategy::Ipv6AndIpv4 } else { LookupIpStrategy::Ipv4AndIpv6 };
        let resolver = builder.build().map_err(|e| NetError::Config(format!("encrypted DNS: {e}")))?;
        Ok(Dns::Encrypted { resolver: Arc::new(resolver), label })
    }

    pub fn label(&self) -> &'static str {
        match self {
            Dns::System => "system DNS",
            Dns::Encrypted { label, .. } => label,
        }
    }

    /// Every address for `host`, IPv6 and IPv4. An IP literal resolves to itself.
    pub async fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, NetError> {
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        if let Ok(ip) = bare.parse::<IpAddr>() {
            return Ok(vec![SocketAddr::new(ip, port)]);
        }
        let addrs: Vec<SocketAddr> = match self {
            Dns::System => tokio::net::lookup_host((bare, port))
                .await
                .map_err(|e| NetError::Dns(host.to_string(), e.to_string()))?
                .collect(),
            Dns::Encrypted { resolver, .. } => resolver
                .lookup_ip(bare)
                .await
                .map_err(|e| NetError::Dns(host.to_string(), e.to_string()))?
                .iter()
                .map(|ip| SocketAddr::new(ip, port))
                .collect(),
        };
        if addrs.is_empty() {
            return Err(NetError::Dns(host.to_string(), "no addresses".into()));
        }
        Ok(addrs)
    }
}

fn name_servers(kind: &str, server: &str) -> Result<Vec<NameServerConfig>, NetError> {
    let group: Option<&ServerGroup<'static>> = match server.to_ascii_lowercase().as_str() {
        "cloudflare" => Some(&CLOUDFLARE),
        "google" => Some(&GOOGLE),
        "quad9" => Some(&QUAD9),
        _ => None,
    };
    if let Some(g) = group {
        return Ok(match kind {
            "DoH" => g.https().collect(),
            "DoT" => g.tls().collect(),
            _ => g.quic().collect(),
        });
    }
    // IP#name, such as 9.9.9.9#dns.quad9.net
    let (ip, name) = server
        .split_once('#')
        .ok_or_else(|| NetError::Config(format!("dns_server `{server}`: use cloudflare, quad9, google or IP#name")))?;
    let ip: IpAddr = ip.trim().parse().map_err(|_| NetError::Config(format!("dns_server `{server}`: bad IP")))?;
    let name: Arc<str> = Arc::from(name.trim());
    Ok(vec![match kind {
        "DoH" => NameServerConfig::https(ip, name, None),
        "DoT" => NameServerConfig::tls(ip, name),
        _ => NameServerConfig::quic(ip, name),
    }])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn literals_skip_lookup() {
        let d = Dns::System;
        assert_eq!(d.resolve("127.0.0.1", 70).await.unwrap(), vec!["127.0.0.1:70".parse().unwrap()]);
        assert_eq!(d.resolve("[::1]", 1965).await.unwrap(), vec!["[::1]:1965".parse().unwrap()]);
    }

    #[test]
    fn builds_encrypted_resolvers() {
        for (mode, server) in [("doh", "quad9"), ("dot", "cloudflare"), ("doq", "9.9.9.9#dns.quad9.net")] {
            let cfg = Network { dns: mode.into(), dns_server: Some(server.into()), ..Network::default() };
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            let d = rt.block_on(async { Dns::from_config(&cfg) }).unwrap();
            assert_ne!(d.label(), "system DNS");
        }
        let bad = Network { dns: "doh".into(), dns_server: Some("nonsense".into()), ..Network::default() };
        assert!(Dns::from_config(&bad).is_err());
    }
}
