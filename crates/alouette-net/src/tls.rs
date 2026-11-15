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
    client_config_ech(verify, tls13_only, alpn, None, None)
}

/// A client certificate and its private key, both PEM, for servers that identify people
/// by certificate (IRC's SASL EXTERNAL and CertFP, some Gemini capsules).
#[derive(Clone)]
pub struct ClientCert {
    pub cert_pem: Vec<u8>,
    pub key_pem: Vec<u8>,
}

impl std::fmt::Debug for ClientCert {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ClientCert(..)")
    }
}

impl ClientCert {
    /// Read one PEM file holding both the certificate and the key, or two files.
    pub fn load(cert: &std::path::Path, key: Option<&std::path::Path>) -> Result<Self, NetError> {
        let read = |p: &std::path::Path| std::fs::read(p).map_err(|e| NetError::Tls(format!("{}: {e}", p.display())));
        let cert_pem = read(cert)?;
        let key_pem = match key {
            Some(k) => read(k)?,
            None => cert_pem.clone(),
        };
        Ok(Self { cert_pem, key_pem })
    }

    fn parts(&self) -> Result<(Vec<CertificateDer<'static>>, rustls::pki_types::PrivateKeyDer<'static>), NetError> {
        use rustls::pki_types::PrivateKeyDer;
        use rustls::pki_types::pem::PemObject;
        let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(&self.cert_pem)
            .collect::<Result<_, _>>()
            .map_err(|e| NetError::Tls(format!("client certificate: {e}")))?;
        if certs.is_empty() {
            return Err(NetError::Tls("the client certificate file holds no certificate".into()));
        }
        let key = PrivateKeyDer::from_pem_slice(&self.key_pem)
            .map_err(|e| NetError::Tls(format!("client certificate key: {e}")))?;
        Ok((certs, key))
    }

    /// The SHA-256 fingerprint of the certificate, as IRC services show for CertFP.
    pub fn fingerprint(&self) -> Option<String> {
        let (certs, _) = self.parts().ok()?;
        Some(Sha256::digest(certs[0].as_ref()).iter().map(|b| format!("{b:02x}")).collect())
    }
}

/// An ECH configuration from an ECHConfigList (as DNS HTTPS records carry), if one of its
/// configs suits our HPKE suites.
pub fn ech_config(list: &[u8]) -> Option<rustls::client::EchConfig> {
    rustls::client::EchConfig::new(
        rustls::pki_types::EchConfigListBytes::from(list.to_vec()),
        rustls::crypto::aws_lc_rs::hpke::ALL_SUPPORTED_SUITES,
    )
    .ok()
}

/// Like [`client_config`], with Encrypted Client Hello when `ech` holds a usable config
/// list. ECH needs TLS 1.3.
pub fn client_config_ech(
    verify: Verify,
    tls13_only: bool,
    alpn: &[&[u8]],
    ech: Option<&[u8]>,
    client: Option<&ClientCert>,
) -> Result<ClientConfig, NetError> {
    let versions: &[&rustls::SupportedProtocolVersion] =
        if tls13_only { &[&rustls::version::TLS13] } else { &[&rustls::version::TLS13, &rustls::version::TLS12] };
    let builder = ClientConfig::builder_with_provider(provider());
    let builder = match ech.and_then(ech_config) {
        Some(cfg) => builder.with_ech(rustls::client::EchMode::Enable(cfg)),
        None => builder.with_protocol_versions(versions),
    }
    .map_err(|e| NetError::Tls(e.to_string()))?;
    let builder = match verify {
        Verify::System => builder.with_platform_verifier().map_err(|e| NetError::Tls(e.to_string()))?,
        Verify::Tofu => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AnyCert { algs: provider().signature_verification_algorithms })),
    };
    let mut cfg = match client {
        Some(c) => {
            let (certs, key) = c.parts()?;
            builder.with_client_auth_cert(certs, key).map_err(|e| NetError::Tls(e.to_string()))?
        }
        None => builder.with_no_client_auth(),
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
    /// The certificates the server sent, leaf first, as DER.
    pub chain: Vec<Vec<u8>>,
    /// The server accepted Encrypted Client Hello, so the name was hidden.
    pub ech: bool,
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
        let chain =
            conn.peer_certificates().map(|c| c.iter().map(|d| d.as_ref().to_vec()).collect()).unwrap_or_default();
        Self {
            version,
            cipher,
            key_exchange,
            post_quantum,
            alpn,
            cert_sha256,
            verified: verify == Verify::System,
            chain,
            ech: conn.ech_status() == rustls::client::EchStatus::Accepted,
        }
    }
}

pub fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der).iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn bad_ech_lists_are_ignored() {
        assert!(super::ech_config(b"not an ECHConfigList").is_none());
        assert!(super::client_config_ech(super::Verify::System, false, &[b"h2"], Some(b"junk"), None).is_ok());
    }

    #[test]
    fn client_certificates() {
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec!["me".into()]).unwrap().self_signed(&key).unwrap();
        let c = super::ClientCert { cert_pem: cert.pem().into_bytes(), key_pem: key.serialize_pem().into_bytes() };
        assert_eq!(c.fingerprint().unwrap().len(), 64);
        assert!(super::client_config_ech(super::Verify::Tofu, false, &[], None, Some(&c)).is_ok());
        // One file holding both works too.
        let both = super::ClientCert {
            cert_pem: [c.cert_pem.clone(), c.key_pem.clone()].concat(),
            key_pem: [c.cert_pem.clone(), c.key_pem.clone()].concat(),
        };
        assert!(both.fingerprint().is_some());
        let bad = super::ClientCert { cert_pem: b"nothing".to_vec(), key_pem: vec![] };
        assert!(super::client_config_ech(super::Verify::Tofu, false, &[], None, Some(&bad)).is_err());
    }
}
