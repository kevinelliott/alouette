//! TLS client setup. TLS 1.3 with the hybrid post-quantum X25519MLKEM768 key exchange is
//! offered first; TLS 1.2 stays available unless `tls_min = "1.3"`, because many Gemini and
//! Gopher-over-TLS servers still use it.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use rustls_platform_verifier::BuilderVerifierExt;
use sha2::{Digest, Sha256};

use crate::NetError;

/// How to check the server's certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verify {
    /// The operating system's trust store, as browsers do.
    System,
    /// Trust on first use: accept any certificate and report its fingerprint, so the caller
    /// can pin it. Gemini works this way.
    Tofu,
}

/// aws-lc-rs with the hybrid post-quantum group first, then the classic ones.
pub fn provider() -> Arc<CryptoProvider> {
    use rustls::crypto::aws_lc_rs::{default_provider, kx_group};
    Arc::new(CryptoProvider {
        kx_groups: vec![kx_group::X25519MLKEM768, kx_group::X25519, kx_group::SECP256R1, kx_group::SECP384R1],
        ..default_provider()
    })
}

pub fn client_config(verify: Verify, tls13_only: bool, alpn: &[&[u8]]) -> Result<ClientConfig, NetError> {
    let versions: &[&rustls::SupportedProtocolVersion] =
        if tls13_only { &[&rustls::version::TLS13] } else { &[&rustls::version::TLS13, &rustls::version::TLS12] };
    let builder = ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(versions)
        .map_err(|e| NetError::Tls(e.to_string()))?;
    let mut cfg = match verify {
        Verify::System => {
            builder.with_platform_verifier().map_err(|e| NetError::Tls(e.to_string()))?.with_no_client_auth()
        }
        Verify::Tofu => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AnyCert { algs: provider().signature_verification_algorithms }))
            .with_no_client_auth(),
    };
    cfg.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    Ok(cfg)
}

/// Accepts any certificate but still checks that the server owns its key, so a TOFU pin
/// on the fingerprint means something.
#[derive(Debug)]
struct AnyCert {
    algs: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for AnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algs)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algs)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

/// What was negotiated, for the connection badge and TOFU checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsInfo {
    pub version: String,
    pub cipher: String,
    pub key_exchange: String,
    pub post_quantum: bool,
    pub alpn: Option<String>,
    /// SHA-256 of the server's leaf certificate, as lowercase hex.
    pub cert_sha256: Option<String>,
    /// True when the certificate was checked against the system trust store.
    pub verified: bool,
}

impl TlsInfo {
    pub fn from_conn(conn: &rustls::ClientConnection, verify: Verify) -> Self {
        let version = match conn.protocol_version() {
            Some(rustls::ProtocolVersion::TLSv1_3) => "TLS 1.3".to_string(),
            Some(rustls::ProtocolVersion::TLSv1_2) => "TLS 1.2".to_string(),
            Some(v) => format!("{v:?}"),
            None => "TLS".into(),
        };
        let cipher = conn.negotiated_cipher_suite().map(|c| format!("{:?}", c.suite())).unwrap_or_default();
        let key_exchange = conn.negotiated_key_exchange_group().map(|g| format!("{:?}", g.name())).unwrap_or_default();
        let post_quantum = key_exchange.contains("MLKEM");
        let alpn = conn.alpn_protocol().map(|a| String::from_utf8_lossy(a).into_owned());
        let cert_sha256 = conn.peer_certificates().and_then(|c| c.first()).map(|c| fingerprint(c.as_ref()));
        Self { version, cipher, key_exchange, post_quantum, alpn, cert_sha256, verified: verify == Verify::System }
    }
}

pub fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der).iter().map(|b| format!("{b:02x}")).collect()
}
