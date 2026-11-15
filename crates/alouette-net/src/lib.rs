//! Alouette's network layer. Every module connects through [`Net`], so they all get the same
//! modern transport: dual-stack Happy Eyeballs, encrypted DNS, TLS 1.3 with post-quantum key
//! exchange, SOCKS5 and HTTP proxies, Tor routing for `.onion` names and per-host rules.
//! Each connection reports a [`ConnInfo`] that the status line shows as a badge.

pub mod dns;
pub mod happy;
pub mod http;
pub mod proxy;
pub mod quic;
pub mod route;
pub mod tls;
pub mod web;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use alouette_core::config::Network;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

pub use dns::Dns;
pub use route::{Routes, Via};
pub use tls::{ClientCert, TlsInfo, Verify};

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("network settings: {0}")]
    Config(String),
    #[error("could not look up {0}: {1}")]
    Dns(String, String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Proxy(String),
    #[error("TLS: {0}")]
    Tls(String),
    #[error("timed out connecting to {0}")]
    Timeout(String),
}

/// Anything a module can read from and write to.
pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// How a connection was made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnInfo {
    pub host: String,
    pub port: u16,
    /// The address we connected to: the server, or the proxy.
    pub peer: Option<SocketAddr>,
    pub via: String,
    pub dns: &'static str,
    pub tls: Option<TlsInfo>,
}

impl ConnInfo {
    /// A short summary such as `IPv6 · TLS 1.3 · PQ · DoH`.
    pub fn badge(&self) -> String {
        let mut parts: Vec<String> = vec![];
        if self.via == "direct" {
            match self.peer {
                Some(SocketAddr::V6(_)) => parts.push("IPv6".into()),
                Some(SocketAddr::V4(_)) => parts.push("IPv4".into()),
                None => {}
            }
        } else if self.via.starts_with("tor") {
            parts.push("Tor".into());
        } else {
            parts.push("proxy".into());
        }
        match &self.tls {
            Some(t) => {
                parts.push(t.version.clone());
                if t.post_quantum {
                    parts.push("PQ".into());
                }
                if t.ech {
                    parts.push("ECH".into());
                }
                if !t.verified {
                    parts.push("TOFU".into());
                }
            }
            None => parts.push("plain".into()),
        }
        if self.dns != "system DNS" && self.via == "direct" {
            parts.push(self.dns.into());
        }
        parts.join(" · ")
    }
}

/// A plain TCP connection, maybe through a proxy.
pub struct Conn {
    pub stream: Box<dyn Io>,
    pub info: ConnInfo,
}

/// A TLS connection.
pub struct TlsConn {
    pub stream: TlsStream<Box<dyn Io>>,
    pub info: ConnInfo,
}

/// Options for a TLS connection.
#[derive(Debug, Clone)]
pub struct TlsOptions {
    pub verify: Verify,
    pub alpn: Vec<&'static [u8]>,
    /// Name to send in SNI and check the certificate against. Defaults to the host.
    pub server_name: Option<String>,
    /// An ECHConfigList from the host's HTTPS record, to hide the name with ECH.
    pub ech: Option<Vec<u8>>,
    /// A certificate to present to the server.
    pub client_cert: Option<tls::ClientCert>,
}

impl Default for TlsOptions {
    fn default() -> Self {
        Self { verify: Verify::System, alpn: vec![], server_name: None, ech: None, client_cert: None }
    }
}

/// The shared connector. Cheap to clone.
#[derive(Clone)]
pub struct Net {
    inner: Arc<Inner>,
}

struct Inner {
    dns: Dns,
    routes: Routes,
    prefer_v6: bool,
    timeout: Duration,
    tls13_only: bool,
    sessions: Arc<rustls::client::ClientSessionMemoryCache>,
    http3: bool,
    ech: bool,
}

impl Net {
    /// Build from the `[network]` config. Must run inside a tokio runtime when encrypted DNS
    /// is configured.
    pub fn new(cfg: &Network) -> Result<Self, NetError> {
        let tls13_only = match cfg.tls_min.trim() {
            "1.3" => true,
            "1.2" | "" => false,
            other => return Err(NetError::Config(format!("tls_min `{other}` (expected 1.2 or 1.3)"))),
        };
        Ok(Self {
            inner: Arc::new(Inner {
                dns: Dns::from_config(cfg)?,
                routes: Routes::from_config(cfg)?,
                prefer_v6: cfg.prefer_ipv6,
                timeout: Duration::from_secs(cfg.connect_timeout_secs.clamp(1, 600)),
                tls13_only,
                sessions: Arc::new(rustls::client::ClientSessionMemoryCache::new(256)),
                http3: cfg.http3,
                ech: cfg.ech,
            }),
        })
    }

    pub fn http3_enabled(&self) -> bool {
        self.inner.http3
    }

    pub fn ech_enabled(&self) -> bool {
        self.inner.ech
    }

    pub fn dns(&self) -> &Dns {
        &self.inner.dns
    }

    /// Resolve a name with the configured DNS.
    pub async fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, NetError> {
        self.inner.dns.resolve(host, port).await
    }

    /// Open a TCP connection to `host:port`, following the routing rules.
    pub async fn tcp(&self, host: &str, port: u16) -> Result<Conn, NetError> {
        let label = format!("{host}:{port}");
        tokio::time::timeout(self.inner.timeout, self.tcp_inner(host, port))
            .await
            .map_err(|_| NetError::Timeout(label))?
    }

    async fn tcp_inner(&self, host: &str, port: u16) -> Result<Conn, NetError> {
        let via = self.inner.routes.pick(host)?.clone();
        let (stream, peer) = match &via {
            Via::Direct => {
                let addrs = self.resolve(host, port).await?;
                happy::connect(&addrs, self.inner.prefer_v6).await?
            }
            Via::Socks5 { proxy, remote_dns, auth } => {
                let addrs = self.resolve(&proxy.host, proxy.port).await?;
                let (mut s, peer) = happy::connect(&addrs, self.inner.prefer_v6).await?;
                let dns = self.inner.dns.clone();
                proxy::socks5(&mut s, host, port, *remote_dns, auth.as_ref(), async |h| {
                    Ok(dns.resolve(h, port).await?[0].ip())
                })
                .await?;
                (s, peer)
            }
            Via::Http { proxy, auth } => {
                let addrs = self.resolve(&proxy.host, proxy.port).await?;
                let (mut s, peer) = happy::connect(&addrs, self.inner.prefer_v6).await?;
                proxy::http_connect(&mut s, host, port, auth.as_ref()).await?;
                (s, peer)
            }
        };
        Ok(Conn {
            stream: Box::new(stream) as Box<dyn Io>,
            info: ConnInfo {
                host: host.to_string(),
                port,
                peer: Some(peer),
                via: via.label(),
                dns: self.inner.dns.label(),
                tls: None,
            },
        })
    }

    /// Open a TLS connection to `host:port`.
    pub async fn tls(&self, host: &str, port: u16, opts: &TlsOptions) -> Result<TlsConn, NetError> {
        let conn = self.tcp(host, port).await?;
        let label = format!("{host}:{port}");
        tokio::time::timeout(self.inner.timeout, self.handshake(conn, opts))
            .await
            .map_err(|_| NetError::Timeout(label))?
    }

    /// Start TLS on an existing connection, as STARTTLS does.
    pub async fn handshake(&self, conn: Conn, opts: &TlsOptions) -> Result<TlsConn, NetError> {
        let mut cfg = tls::client_config_ech(
            opts.verify,
            self.inner.tls13_only,
            &opts.alpn,
            opts.ech.as_deref(),
            opts.client_cert.as_ref(),
        )?;
        // Shared, so FTPS data connections can resume the control connection's session.
        cfg.resumption = rustls::client::Resumption::store(self.inner.sessions.clone());
        let name = opts.server_name.clone().unwrap_or_else(|| conn.info.host.clone());
        let name = name.trim_start_matches('[').trim_end_matches(']').to_string();
        let server_name = rustls::pki_types::ServerName::try_from(name.clone())
            .map_err(|_| NetError::Tls(format!("`{name}` is not a valid server name")))?;
        let stream = TlsConnector::from(Arc::new(cfg))
            .connect(server_name, conn.stream)
            .await
            .map_err(|e| NetError::Tls(e.to_string()))?;
        let mut info = conn.info;
        info.tls = Some(TlsInfo::from_conn(stream.get_ref().1, opts.verify));
        Ok(TlsConn { stream, info })
    }
}

/// Keep `TcpStream` in the public API surface for modules that need raw sockets.
pub type RawTcp = TcpStream;
