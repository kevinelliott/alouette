//! Inspect a server's TLS setup and certificate chain.

use alouette_net::{Net, TlsOptions, Verify};
use x509_parser::prelude::*;

use crate::{Out, Section, Style};

pub fn date(ts: i64) -> String {
    ::time::OffsetDateTime::from_unix_timestamp(ts)
        .ok()
        .and_then(|d| d.format(&::time::macros::format_description!("[year]-[month]-[day]")).ok())
        .unwrap_or_else(|| ts.to_string())
}

/// Connect with full verification; if that fails, connect again without it so the chain can
/// still be shown, and say why it is not trusted.
pub async fn inspect(net: &Net, host: &str, port: u16) -> Section {
    let mut s = Section::new("TLS", format!("TLS for {host}:{port}"));
    let opts = TlsOptions { alpn: vec![b"h2", b"http/1.1"], ..TlsOptions::default() };
    let (conn, problem) = match net.tls(host, port, &opts).await {
        Ok(c) => (c, None),
        Err(e) => {
            let tofu = TlsOptions { verify: Verify::Tofu, ..opts.clone() };
            match net.tls(host, port, &tofu).await {
                Ok(c) => (c, Some(e.to_string())),
                Err(_) => return Section::error("TLS", &s.title, e),
            }
        }
    };
    let info = conn.info;
    let Some(t) = info.tls.clone() else { return Section::error("TLS", &s.title, "no TLS session") };
    match &problem {
        None => s.text(Style::Good, "Trusted by this computer's certificate store"),
        Some(p) => s.text(Style::Bad, format!("Not trusted: {p}")),
    };
    s.field("Connection", info.badge());
    s.field("Protocol", &t.version);
    s.field("Cipher", &t.cipher);
    s.push(Out::new(
        if t.post_quantum { Style::Good } else { Style::Normal },
        format!(
            "{:<14} {}{}",
            "Key exchange",
            t.key_exchange,
            if t.post_quantum { " (post-quantum hybrid)" } else { "" }
        ),
    ));
    if let Some(a) = &t.alpn {
        s.field("ALPN", a);
    }
    if let Some(p) = info.peer {
        s.field("Address", p);
    }
    let now = ::time::OffsetDateTime::now_utc().unix_timestamp();
    for (i, der) in t.chain.iter().enumerate() {
        s.text(
            Style::Heading,
            if i == 0 { "Server certificate".to_string() } else { format!("Chain certificate {i}") },
        );
        let Ok((_, cert)) = parse_x509_certificate(der) else {
            s.text(Style::Bad, "could not parse this certificate");
            continue;
        };
        s.field("Subject", cert.subject());
        s.field("Issuer", cert.issuer());
        let nb = cert.validity().not_before.timestamp();
        let na = cert.validity().not_after.timestamp();
        let days = (na - now) / 86400;
        s.push(Out::new(
            if na < now || nb > now || days < 14 { Style::Bad } else { Style::Field },
            format!(
                "{:<14} {} to {} ({})",
                "Valid",
                date(nb),
                date(na),
                if na < now { "expired".into() } else { format!("{days} days left") }
            ),
        ));
        if i == 0
            && let Ok(Some(san)) = cert.subject_alternative_name()
        {
            let names: Vec<String> = san
                .value
                .general_names
                .iter()
                .filter_map(|g| match g {
                    GeneralName::DNSName(d) => Some(d.to_string()),
                    GeneralName::IPAddress(b) if b.len() == 4 => {
                        Some(std::net::Ipv4Addr::new(b[0], b[1], b[2], b[3]).to_string())
                    }
                    _ => None,
                })
                .collect();
            let shown: Vec<&str> = names.iter().take(12).map(String::as_str).collect();
            let more = if names.len() > 12 { format!(" and {} more", names.len() - 12) } else { String::new() };
            s.field("Names", format!("{}{more}", shown.join(", ")));
        }
        let key = match cert.public_key().parsed() {
            Ok(x509_parser::public_key::PublicKey::RSA(r)) => format!("RSA {} bits", r.key_size()),
            Ok(x509_parser::public_key::PublicKey::EC(e)) => format!("EC {} bits", e.key_size()),
            Ok(_) => "other".into(),
            Err(_) => "unknown".into(),
        };
        s.field("Key", key);
        let sig =
            x509_parser::objects::oid2sn(&cert.signature_algorithm.algorithm, x509_parser::objects::oid_registry())
                .map(str::to_string)
                .unwrap_or_else(|_| cert.signature_algorithm.algorithm.to_id_string());
        s.field("Signature", sig);
        s.field("SHA-256", alouette_net::tls::fingerprint(der));
    }
    s
}
