//! SOCKS5 (RFC 1928, with RFC 1929 passwords) and HTTP CONNECT tunnels.

use std::net::IpAddr;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use crate::NetError;

/// Ask a SOCKS5 proxy to connect to `host:port`. With `remote_dns` the proxy resolves the
/// name, which Tor requires and which keeps lookups private.
pub async fn socks5<S: AsyncRead + AsyncWrite + Unpin>(
    s: &mut S,
    host: &str,
    port: u16,
    remote_dns: bool,
    auth: Option<&(String, String)>,
    resolve: impl AsyncFnOnce(&str) -> Result<IpAddr, NetError>,
) -> Result<(), NetError> {
    let err = |m: &str| NetError::Proxy(format!("socks5: {m}"));
    let methods: &[u8] = if auth.is_some() { &[0x00, 0x02] } else { &[0x00] };
    let mut hello = vec![0x05, methods.len() as u8];
    hello.extend_from_slice(methods);
    s.write_all(&hello).await?;
    let mut reply = [0u8; 2];
    s.read_exact(&mut reply).await?;
    if reply[0] != 0x05 {
        return Err(err("not a SOCKS5 proxy"));
    }
    match (reply[1], auth) {
        (0x00, _) => {}
        (0x02, Some((user, pass))) => {
            if user.len() > 255 || pass.len() > 255 {
                return Err(err("user name or password is too long"));
            }
            let mut msg = vec![0x01, user.len() as u8];
            msg.extend_from_slice(user.as_bytes());
            msg.push(pass.len() as u8);
            msg.extend_from_slice(pass.as_bytes());
            s.write_all(&msg).await?;
            let mut r = [0u8; 2];
            s.read_exact(&mut r).await?;
            if r[1] != 0x00 {
                return Err(err("the proxy rejected the user name or password"));
            }
        }
        (0xff, _) => return Err(err("the proxy accepts none of our login methods")),
        (m, _) => return Err(err(&format!("unexpected login method {m}"))),
    }

    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let target = match bare.parse::<IpAddr>() {
        Ok(ip) => ip_bytes(ip),
        Err(_) if remote_dns => {
            if bare.len() > 255 {
                return Err(err("host name is too long"));
            }
            let mut v = vec![0x03, bare.len() as u8];
            v.extend_from_slice(bare.as_bytes());
            v
        }
        Err(_) => ip_bytes(resolve(bare).await?),
    };
    let mut req = vec![0x05, 0x01, 0x00];
    req.extend_from_slice(&target);
    req.extend_from_slice(&port.to_be_bytes());
    s.write_all(&req).await?;

    let mut head = [0u8; 4];
    s.read_exact(&mut head).await?;
    if head[1] != 0x00 {
        return Err(err(match head[1] {
            0x01 => "general failure",
            0x02 => "connection not allowed by the proxy's rules",
            0x03 => "network unreachable",
            0x04 => "host unreachable",
            0x05 => "connection refused",
            0x06 => "TTL expired",
            0x07 => "command not supported",
            0x08 => "address type not supported",
            _ => "unknown error",
        }));
    }
    // Skip the bound address the proxy reports.
    let skip = match head[3] {
        0x01 => 4,
        0x04 => 16,
        0x03 => s.read_u8().await? as usize,
        _ => return Err(err("bad reply")),
    };
    let mut rest = vec![0u8; skip + 2];
    s.read_exact(&mut rest).await?;
    Ok(())
}

fn ip_bytes(ip: IpAddr) -> Vec<u8> {
    match ip {
        IpAddr::V4(v4) => [&[0x01][..], &v4.octets()].concat(),
        IpAddr::V6(v6) => [&[0x04][..], &v6.octets()].concat(),
    }
}

/// Open a tunnel through an HTTP proxy with `CONNECT host:port`.
pub async fn http_connect<S: AsyncRead + AsyncWrite + Unpin>(
    s: &mut S,
    host: &str,
    port: u16,
    auth: Option<&(String, String)>,
) -> Result<(), NetError> {
    let authority = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let mut req = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if let Some((u, p)) = auth {
        req.push_str(&format!("Proxy-Authorization: Basic {}\r\n", base64(format!("{u}:{p}").as_bytes())));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).await?;

    // Read the status line and headers one byte at a time so nothing past them is consumed.
    let mut reader = BufReader::with_capacity(1, s);
    let mut status = String::new();
    reader.read_line(&mut status).await?;
    let code = status.split_whitespace().nth(1).and_then(|c| c.parse::<u16>().ok());
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
    }
    match code {
        Some(200..=299) => Ok(()),
        Some(407) => Err(NetError::Proxy("http proxy: login required (407)".into())),
        Some(c) => Err(NetError::Proxy(format!("http proxy refused the tunnel ({c})"))),
        None => Err(NetError::Proxy("http proxy sent a bad reply".into())),
    }
}

fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[test]
    fn base64_matches_rfc4648() {
        for (i, o) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foobar", "Zm9vYmFy")] {
            assert_eq!(base64(i.as_bytes()), o);
        }
    }

    #[tokio::test]
    async fn socks5_with_password_and_remote_dns() {
        let (mut client, mut server) = duplex(1024);
        let srv = tokio::spawn(async move {
            let mut b = [0u8; 4];
            server.read_exact(&mut b).await.unwrap();
            assert_eq!(b, [5, 2, 0, 2]);
            server.write_all(&[5, 2]).await.unwrap();
            let mut a = [0u8; 2];
            server.read_exact(&mut a).await.unwrap();
            let mut user = vec![0u8; a[1] as usize];
            server.read_exact(&mut user).await.unwrap();
            let plen = server.read_u8().await.unwrap();
            let mut pass = vec![0u8; plen as usize];
            server.read_exact(&mut pass).await.unwrap();
            assert_eq!((&user[..], &pass[..]), (&b"kev"[..], &b"pw"[..]));
            server.write_all(&[1, 0]).await.unwrap();
            let mut h = [0u8; 5];
            server.read_exact(&mut h).await.unwrap();
            assert_eq!(&h[..4], &[5, 1, 0, 3]);
            let mut name = vec![0u8; h[4] as usize + 2];
            server.read_exact(&mut name).await.unwrap();
            assert_eq!(&name[..name.len() - 2], b"example.onion");
            assert_eq!(&name[name.len() - 2..], &70u16.to_be_bytes());
            server.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
            server.write_all(b"hello").await.unwrap();
        });
        let creds = ("kev".to_string(), "pw".to_string());
        socks5(&mut client, "example.onion", 70, true, Some(&creds), async |_| unreachable!()).await.unwrap();
        let mut got = [0u8; 5];
        client.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"hello");
        srv.await.unwrap();
    }

    #[tokio::test]
    async fn socks5_reports_refusal() {
        let (mut client, mut server) = duplex(1024);
        tokio::spawn(async move {
            let mut b = [0u8; 3];
            server.read_exact(&mut b).await.unwrap();
            server.write_all(&[5, 0]).await.unwrap();
            let mut req = [0u8; 10];
            server.read_exact(&mut req).await.unwrap();
            assert_eq!(&req[3..8], &[1, 10, 0, 0, 1]);
            server.write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
        });
        let e = socks5(&mut client, "host.lan", 22, false, None, async |_| Ok("10.0.0.1".parse().unwrap())).await;
        assert!(e.unwrap_err().to_string().contains("refused"));
    }

    #[tokio::test]
    async fn http_connect_tunnels_and_leaves_payload() {
        let (mut client, mut server) = duplex(4096);
        tokio::spawn(async move {
            let mut buf = vec![0u8; 512];
            let n = server.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            assert!(req.starts_with("CONNECT example.org:443 HTTP/1.1\r\n"));
            assert!(req.contains("Proxy-Authorization: Basic a2V2OnB3\r\n"));
            server.write_all(b"HTTP/1.1 200 Connection established\r\nVia: test\r\n\r\npayload").await.unwrap();
        });
        let creds = ("kev".to_string(), "pw".to_string());
        http_connect(&mut client, "example.org", 443, Some(&creds)).await.unwrap();
        let mut got = [0u8; 7];
        client.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"payload");
    }

    #[tokio::test]
    async fn http_connect_reports_status() {
        let (mut client, mut server) = duplex(4096);
        tokio::spawn(async move {
            let mut buf = vec![0u8; 512];
            let _ = server.read(&mut buf).await.unwrap();
            server.write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n").await.unwrap();
        });
        let e = http_connect(&mut client, "example.org", 443, None).await.unwrap_err();
        assert!(e.to_string().contains("407"));
    }
}
