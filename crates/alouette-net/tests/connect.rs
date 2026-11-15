//! End-to-end: connect to local servers through the real connector.

use std::sync::Arc;

use alouette_core::config::{Network, Route};
use alouette_net::{Net, TlsOptions, Verify};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A TLS echo-once server with a fresh self-signed certificate. Returns its port and the
/// certificate's SHA-256 fingerprint.
async fn tls_server(versions: &[&'static rustls::SupportedProtocolVersion]) -> (u16, String) {
    let ck = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let der = ck.cert.der().clone();
    let fp = alouette_net::tls::fingerprint(der.as_ref());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(ck.signing_key.serialize_der()));
    let cfg = rustls::ServerConfig::builder_with_provider(alouette_net::tls::provider())
        .with_protocol_versions(versions)
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![der], key)
        .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(cfg));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Ok(mut tls) = acceptor.accept(sock).await {
                    let mut buf = [0u8; 64];
                    if let Ok(n) = tls.read(&mut buf).await {
                        let _ = tls.write_all(&buf[..n]).await;
                        let _ = tls.shutdown().await;
                    }
                }
            });
        }
    });
    (port, fp)
}

#[tokio::test]
async fn tls13_with_post_quantum_key_exchange() {
    let (port, fp) = tls_server(&[&rustls::version::TLS13]).await;
    let net = Net::new(&Network::default()).unwrap();
    let opts = TlsOptions { verify: Verify::Tofu, server_name: Some("localhost".into()), ..Default::default() };
    let mut conn = net.tls("127.0.0.1", port, &opts).await.unwrap();
    let tls = conn.info.tls.clone().unwrap();
    assert_eq!(tls.version, "TLS 1.3");
    assert!(tls.post_quantum, "key exchange was {}", tls.key_exchange);
    assert_eq!(tls.cert_sha256.as_deref(), Some(fp.as_str()));
    assert_eq!(conn.info.badge(), "IPv4 · TLS 1.3 · PQ · TOFU");

    conn.stream.write_all(b"20 text/gemini\r\n").await.unwrap();
    let mut got = String::new();
    conn.stream.read_to_string(&mut got).await.unwrap();
    assert_eq!(got, "20 text/gemini\r\n");
}

#[tokio::test]
async fn system_verification_rejects_self_signed() {
    let (port, _) = tls_server(&[&rustls::version::TLS13]).await;
    let net = Net::new(&Network::default()).unwrap();
    let opts = TlsOptions { server_name: Some("localhost".into()), ..Default::default() };
    assert!(net.tls("127.0.0.1", port, &opts).await.is_err());
}

#[tokio::test]
async fn tls_min_is_enforced() {
    let (port, _) = tls_server(&[&rustls::version::TLS12]).await;
    let opts = TlsOptions { verify: Verify::Tofu, server_name: Some("localhost".into()), ..Default::default() };

    let lenient = Net::new(&Network::default()).unwrap();
    let c = lenient.tls("127.0.0.1", port, &opts).await.unwrap();
    assert_eq!(c.info.tls.unwrap().version, "TLS 1.2");

    let strict = Net::new(&Network { tls_min: "1.3".into(), ..Network::default() }).unwrap();
    assert!(strict.tls("127.0.0.1", port, &opts).await.is_err());
}

/// A minimal SOCKS5 proxy that only supports CONNECT to a domain name and forwards to
/// 127.0.0.1, recording the name it was asked for.
async fn socks_proxy() -> (u16, tokio::sync::mpsc::UnboundedReceiver<String>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut c, _)) = listener.accept().await {
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut h = [0u8; 3];
                c.read_exact(&mut h).await.unwrap();
                c.write_all(&[5, 0]).await.unwrap();
                let mut r = [0u8; 5];
                c.read_exact(&mut r).await.unwrap();
                assert_eq!(r[3], 3, "expected a domain name");
                let mut name = vec![0u8; r[4] as usize];
                c.read_exact(&mut name).await.unwrap();
                let port = c.read_u16().await.unwrap();
                tx.send(String::from_utf8(name).unwrap()).unwrap();
                let mut up = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
                c.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]).await.unwrap();
                let _ = tokio::io::copy_bidirectional(&mut c, &mut up).await;
            });
        }
    });
    (port, rx)
}

#[tokio::test]
async fn onion_names_route_through_socks_with_remote_dns() {
    let (tls_port, _) = tls_server(&[&rustls::version::TLS13]).await;
    let (proxy_port, mut asked) = socks_proxy().await;
    let cfg = Network {
        routes: vec![Route { hosts: vec!["*.onion".into()], via: format!("socks5h://127.0.0.1:{proxy_port}") }],
        ..Network::default()
    };
    let net = Net::new(&cfg).unwrap();
    let opts = TlsOptions { verify: Verify::Tofu, server_name: Some("localhost".into()), ..Default::default() };
    let conn = net.tls("examplexyz.onion", tls_port, &opts).await.unwrap();
    assert_eq!(asked.recv().await.unwrap(), "examplexyz.onion");
    assert!(conn.info.badge().starts_with("proxy · TLS 1.3"), "{}", conn.info.badge());

    // Without the rule, a .onion name is refused rather than leaked to DNS.
    let plain = Net::new(&Network::default()).unwrap();
    assert!(plain.tcp("examplexyz.onion", 80).await.is_err());
}

#[tokio::test]
async fn happy_eyeballs_over_ipv6_loopback_when_available() {
    let Ok(listener) = TcpListener::bind("[::1]:0").await else { return };
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { while listener.accept().await.is_ok() {} });
    let net = Net::new(&Network::default()).unwrap();
    let c = net.tcp("::1", port).await.unwrap();
    assert_eq!(c.info.badge(), "IPv6 · plain");
}
