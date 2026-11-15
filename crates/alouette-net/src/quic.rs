//! HTTP/3 over QUIC, through Quinn. Only used for direct connections: proxies and Tor
//! carry TCP, not UDP.

use std::net::SocketAddr;
use std::sync::Arc;

use bytes::{Buf, Bytes};

use crate::route::Via;
use crate::tls::{TlsInfo, Verify};
use crate::{ConnInfo, Net, NetError};

pub type H3Sender = h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>;

/// An open HTTP/3 connection: send requests on it until it closes.
pub struct H3Conn {
    pub sender: H3Sender,
    pub info: ConnInfo,
    /// Kept so the connection stays open while we hold it.
    _endpoint: quinn::Endpoint,
}

impl Clone for H3Conn {
    fn clone(&self) -> Self {
        Self { sender: self.sender.clone(), info: self.info.clone(), _endpoint: self._endpoint.clone() }
    }
}

/// A finished HTTP/3 exchange.
pub struct H3Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Net {
    /// Whether this host would be reached directly (so QUIC can be used).
    pub fn is_direct(&self, host: &str) -> bool {
        matches!(self.inner.routes.pick(host), Ok(Via::Direct))
    }

    /// Open an HTTP/3 connection to `host:port`.
    pub async fn h3_connect(
        &self,
        host: &str,
        port: u16,
        ech: Option<&[u8]>,
        verify: Verify,
    ) -> Result<H3Conn, NetError> {
        let label = format!("{host}:{port}");
        tokio::time::timeout(self.inner.timeout, self.h3_inner(host, port, ech, verify))
            .await
            .map_err(|_| NetError::Timeout(label))?
    }

    async fn h3_inner(&self, host: &str, port: u16, ech: Option<&[u8]>, verify: Verify) -> Result<H3Conn, NetError> {
        if !self.is_direct(host) {
            return Err(NetError::Proxy("HTTP/3 needs a direct connection".into()));
        }
        let mut addrs = self.resolve(host, port).await?;
        // Prefer the configured family, as Happy Eyeballs does for TCP.
        addrs.sort_by_key(|a| a.is_ipv6() != self.inner.prefer_v6);
        let mut cfg = crate::tls::client_config_ech(verify, true, &[b"h3"], ech, None)?;
        cfg.resumption = rustls::client::Resumption::store(self.inner.sessions.clone());
        let quic = quinn::crypto::rustls::QuicClientConfig::try_from(Arc::new(cfg))
            .map_err(|e| NetError::Tls(format!("QUIC: {e}")))?;
        let client_cfg = quinn::ClientConfig::new(Arc::new(quic));
        let name = host.trim_start_matches('[').trim_end_matches(']');
        let mut last = None;
        let tries: Vec<SocketAddr> = addrs.iter().take(4).copied().collect();
        for (n, addr) in tries.iter().enumerate() {
            // QUIC has no refusal to wait for: an address that drops packets would hold
            // the whole attempt, so give each but the last a short turn.
            let more = n + 1 < tries.len();
            let bind: SocketAddr =
                if addr.is_ipv6() { "[::]:0".parse().unwrap() } else { "0.0.0.0:0".parse().unwrap() };
            let mut endpoint = match quinn::Endpoint::client(bind) {
                Ok(e) => e,
                Err(e) => {
                    last = Some(NetError::Io(e));
                    continue;
                }
            };
            endpoint.set_default_client_config(client_cfg.clone());
            let connecting = match endpoint.connect(*addr, name) {
                Ok(c) => c,
                Err(e) => {
                    last = Some(NetError::Proxy(format!("QUIC: {e}")));
                    continue;
                }
            };
            let res = if more {
                match tokio::time::timeout(std::time::Duration::from_millis(2500), connecting).await {
                    Ok(r) => r,
                    Err(_) => {
                        last = Some(NetError::Timeout(addr.to_string()));
                        continue;
                    }
                }
            } else {
                connecting.await
            };
            let conn = match res {
                Ok(c) => c,
                Err(e) => {
                    last = Some(NetError::Proxy(format!("QUIC: {e}")));
                    continue;
                }
            };
            let tls = quic_tls_info(&conn, verify);
            let (mut driver, sender) = h3::client::new(h3_quinn::Connection::new(conn))
                .await
                .map_err(|e| NetError::Proxy(format!("HTTP/3: {e}")))?;
            tokio::spawn(async move {
                let _ = std::future::poll_fn(|cx| driver.poll_close(cx)).await;
            });
            let info = ConnInfo {
                host: host.to_string(),
                port,
                peer: Some(*addr),
                via: "direct".into(),
                dns: self.inner.dns.label(),
                tls: Some(tls),
            };
            return Ok(H3Conn { sender, info, _endpoint: endpoint });
        }
        Err(last.unwrap_or_else(|| NetError::Dns(host.to_string(), "no addresses".into())))
    }
}

fn quic_tls_info(conn: &quinn::Connection, verify: Verify) -> TlsInfo {
    let chain: Vec<Vec<u8>> = conn
        .peer_identity()
        .and_then(|i| i.downcast::<Vec<rustls::pki_types::CertificateDer<'static>>>().ok())
        .map(|v| v.iter().map(|c| c.as_ref().to_vec()).collect())
        .unwrap_or_default();
    TlsInfo {
        version: "TLS 1.3".into(),
        cipher: String::new(),
        // Quinn does not say which group was used; ours offers the hybrid one first.
        key_exchange: String::new(),
        post_quantum: false,
        alpn: Some("h3".into()),
        cert_sha256: chain.first().map(|c| crate::tls::fingerprint(c)),
        verified: verify == Verify::System,
        chain,
        ech: false,
    }
}

/// Send one request on an HTTP/3 connection.
pub async fn h3_request(
    sender: &mut H3Sender,
    url: &url::Url,
    method: &str,
    headers: &[(String, String)],
    body: Option<&[u8]>,
    max: usize,
) -> Result<H3Response, NetError> {
    let err = |e: &dyn std::fmt::Display| NetError::Proxy(format!("HTTP/3: {e}"));
    let mut b = http::Request::builder().method(method).uri(url.as_str());
    for (k, v) in headers {
        b = b.header(k.to_ascii_lowercase(), v);
    }
    if let Some(body) = body {
        b = b.header("content-length", body.len());
    }
    let req = b.body(()).map_err(|e| err(&e))?;
    let mut stream = sender.send_request(req).await.map_err(|e| err(&e))?;
    if let Some(body) = body {
        stream.send_data(Bytes::copy_from_slice(body)).await.map_err(|e| err(&e))?;
    }
    stream.finish().await.map_err(|e| err(&e))?;
    let resp = stream.recv_response().await.map_err(|e| err(&e))?;
    let mut data = Vec::new();
    while let Some(mut chunk) = stream.recv_data().await.map_err(|e| err(&e))? {
        while chunk.has_remaining() {
            let c = chunk.chunk();
            data.extend_from_slice(c);
            let n = c.len();
            chunk.advance(n);
        }
        if data.len() > max {
            return Err(NetError::Proxy(format!("the page is larger than {} MB", max >> 20)));
        }
    }
    let headers = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
        .collect();
    Ok(H3Response { status: resp.status().as_u16(), headers, body: data })
}

/// The HTTP/3 port an Alt-Svc header offers for the same host, if any.
pub fn alt_svc_h3(header: &str) -> Option<u16> {
    for part in header.split(',') {
        let part = part.trim();
        let Some(rest) = part.strip_prefix("h3=") else { continue };
        let value = rest.split(';').next().unwrap_or("").trim().trim_matches('"');
        // Only ":port", the same host; other hosts would need their own checks.
        if let Some(p) = value.strip_prefix(':') {
            return p.parse().ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alt_svc() {
        assert_eq!(alt_svc_h3(r#"h3=":443"; ma=86400, h3-29=":443""#), Some(443));
        assert_eq!(alt_svc_h3(r#"h2=":443", h3=":8443""#), Some(8443));
        assert_eq!(alt_svc_h3(r#"h3="other.example:443""#), None);
        assert_eq!(alt_svc_h3("clear"), None);
    }
}
