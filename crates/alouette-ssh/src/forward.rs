//! Local port forwarding, as `ssh -L`.

use std::sync::Arc;

use tokio::net::TcpListener;

use crate::connect::Session;

/// A parsed `-L` spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalForward {
    pub bind: String,
    pub port: u16,
    pub host: String,
    pub host_port: u16,
}

impl LocalForward {
    /// `8080:localhost:80`, `127.0.0.1:8080:db:5432` or `[::1]:8080:db:5432`.
    pub fn parse(s: &str) -> Result<Self, String> {
        let parts = split_spec(s);
        let bad = || format!("{s:?} is not a forward like 8080:localhost:80");
        let (bind, port, host, hp) = match parts.as_slice() {
            [p, h, hp] => ("127.0.0.1".to_string(), p, h, hp),
            [b, p, h, hp] => (b.clone(), p, h, hp),
            _ => return Err(bad()),
        };
        Ok(Self {
            bind,
            port: port.parse().map_err(|_| bad())?,
            host: host.clone(),
            host_port: hp.parse().map_err(|_| bad())?,
        })
    }
}

fn split_spec(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut bracket = false;
    for c in s.chars() {
        match c {
            '[' => bracket = true,
            ']' => bracket = false,
            ':' if !bracket => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// Listen locally and carry each connection through the session. Runs until the listener
/// fails or the session closes; `on_conn` hears about each connection.
pub async fn run_local(
    session: Arc<Session>,
    fwd: LocalForward,
    on_conn: impl Fn(String) + Send + Sync + 'static,
) -> Result<(), String> {
    let listener = TcpListener::bind((fwd.bind.as_str(), fwd.port))
        .await
        .map_err(|e| format!("{}:{}: {e}", fwd.bind, fwd.port))?;
    let on_conn = Arc::new(on_conn);
    loop {
        let (mut sock, peer) = listener.accept().await.map_err(|e| e.to_string())?;
        let session = session.clone();
        let fwd = fwd.clone();
        let on_conn = on_conn.clone();
        tokio::spawn(async move {
            let ch = match session
                .handle
                .channel_open_direct_tcpip(
                    fwd.host.clone(),
                    u32::from(fwd.host_port),
                    peer.ip().to_string(),
                    u32::from(peer.port()),
                )
                .await
            {
                Ok(c) => c,
                Err(e) => {
                    on_conn(format!("{peer}: could not open {}:{}: {e}", fwd.host, fwd.host_port));
                    return;
                }
            };
            on_conn(format!("{peer} → {}:{}", fwd.host, fwd.host_port));
            let mut stream = ch.into_stream();
            let _ = tokio::io::copy_bidirectional(&mut sock, &mut stream).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_specs() {
        assert_eq!(
            LocalForward::parse("8080:localhost:80").unwrap(),
            LocalForward { bind: "127.0.0.1".into(), port: 8080, host: "localhost".into(), host_port: 80 }
        );
        assert_eq!(LocalForward::parse("[::1]:5432:db:5432").unwrap().bind, "::1");
        assert_eq!(LocalForward::parse("0.0.0.0:1:[fe80::1]:2").unwrap().host, "fe80::1");
        assert!(LocalForward::parse("8080").is_err());
        assert!(LocalForward::parse("x:y:z").is_err());
    }
}
