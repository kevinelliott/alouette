//! HTTP/3 against a local QUIC server.

use std::sync::Arc;

use alouette_net::{Net, Verify};
use bytes::Bytes;
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};

async fn h3_server() -> u16 {
    let ck = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let der = ck.cert.der().clone();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(ck.signing_key.serialize_der()));
    let mut cfg = rustls::ServerConfig::builder_with_provider(alouette_net::tls::provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![der], key)
        .unwrap();
    cfg.alpn_protocols = vec![b"h3".to_vec()];
    let quic = quinn::crypto::rustls::QuicServerConfig::try_from(Arc::new(cfg)).unwrap();
    let endpoint =
        quinn::Endpoint::server(quinn::ServerConfig::with_crypto(Arc::new(quic)), "127.0.0.1:0".parse().unwrap())
            .unwrap();
    let port = endpoint.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            tokio::spawn(async move {
                let conn = incoming.await.unwrap();
                let mut h3 = h3::server::Connection::<_, Bytes>::new(h3_quinn::Connection::new(conn)).await.unwrap();
                while let Ok(Some(resolver)) = h3.accept().await {
                    let (req, mut stream) = resolver.resolve_request().await.unwrap();
                    let mut body = Vec::new();
                    while let Ok(Some(mut chunk)) = stream.recv_data().await {
                        use bytes::Buf;
                        while chunk.has_remaining() {
                            let c = chunk.chunk().to_vec();
                            chunk.advance(c.len());
                            body.extend(c);
                        }
                    }
                    let text = format!("{} {} {}", req.method(), req.uri().path(), String::from_utf8_lossy(&body));
                    let resp =
                        http::Response::builder().status(200).header("content-type", "text/plain").body(()).unwrap();
                    stream.send_response(resp).await.unwrap();
                    stream.send_data(Bytes::from(text)).await.unwrap();
                    stream.finish().await.unwrap();
                }
            });
        }
    });
    port
}

#[tokio::test]
async fn http3_requests() {
    let port = h3_server().await;
    let net = Net::new(&Default::default()).unwrap();
    let mut conn = net.h3_connect("localhost", port, None, Verify::Tofu).await.unwrap();
    assert_eq!(conn.info.tls.as_ref().unwrap().alpn.as_deref(), Some("h3"));
    let url = url::Url::parse(&format!("https://localhost:{port}/hello")).unwrap();
    let r = alouette_net::quic::h3_request(
        &mut conn.sender,
        &url,
        "GET",
        &[("user-agent".into(), "test".into())],
        None,
        1 << 20,
    )
    .await
    .unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(String::from_utf8_lossy(&r.body), "GET /hello ");
    // The same connection carries a second request, with a body.
    let r = alouette_net::quic::h3_request(&mut conn.sender, &url, "POST", &[], Some(b"x=1"), 1 << 20).await.unwrap();
    assert_eq!(String::from_utf8_lossy(&r.body), "POST /hello x=1");
    // The system trust store rejects the self-signed certificate.
    assert!(net.h3_connect("localhost", port, None, Verify::System).await.is_err());
}
