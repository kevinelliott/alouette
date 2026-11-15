//! Happy Eyeballs v2 (RFC 8305): race IPv6 and IPv4 so a broken family never stalls a connect.

use std::net::SocketAddr;
use std::time::Duration;

use tokio::net::TcpStream;
use tokio::task::JoinSet;

/// The pause before starting the next attempt, as RFC 8305 recommends.
pub const ATTEMPT_DELAY: Duration = Duration::from_millis(250);

/// Order addresses by alternating families, starting with the preferred one.
pub fn interleave(addrs: &[SocketAddr], prefer_v6: bool) -> Vec<SocketAddr> {
    let (mut first, mut second): (Vec<SocketAddr>, Vec<SocketAddr>) =
        addrs.iter().copied().partition(|a| a.is_ipv6() == prefer_v6);
    first.dedup();
    second.dedup();
    let mut out = Vec::with_capacity(addrs.len());
    let (mut a, mut b) = (first.into_iter(), second.into_iter());
    loop {
        match (a.next(), b.next()) {
            (None, None) => break,
            (x, y) => out.extend(x.into_iter().chain(y)),
        }
    }
    out
}

/// Connect to the first address that answers. Attempts start [`ATTEMPT_DELAY`] apart, or
/// sooner when the previous one fails.
pub async fn connect(addrs: &[SocketAddr], prefer_v6: bool) -> std::io::Result<(TcpStream, SocketAddr)> {
    let order = interleave(addrs, prefer_v6);
    if order.is_empty() {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "no addresses to connect to"));
    }
    let mut pending = order.into_iter();
    let mut attempts: JoinSet<(SocketAddr, std::io::Result<TcpStream>)> = JoinSet::new();
    let mut last_err = None;
    let start = |set: &mut JoinSet<_>, addr: SocketAddr| {
        set.spawn(async move { (addr, TcpStream::connect(addr).await) });
    };
    if let Some(a) = pending.next() {
        start(&mut attempts, a);
    }
    loop {
        let more = pending.len() > 0;
        tokio::select! {
            done = attempts.join_next(), if !attempts.is_empty() => {
                match done {
                    Some(Ok((addr, Ok(stream)))) => {
                        let _ = stream.set_nodelay(true);
                        attempts.abort_all();
                        return Ok((stream, addr));
                    }
                    Some(Ok((addr, Err(e)))) => {
                        tracing::debug!("connect {addr} failed: {e}");
                        last_err = Some(e);
                        // A failure starts the next attempt at once.
                        if let Some(a) = pending.next() {
                            start(&mut attempts, a);
                        }
                    }
                    Some(Err(e)) => last_err = Some(std::io::Error::other(e)),
                    None => {}
                }
            }
            _ = tokio::time::sleep(ATTEMPT_DELAY), if more => {
                if let Some(a) = pending.next() {
                    start(&mut attempts, a);
                }
            }
        }
        if attempts.is_empty() && pending.len() == 0 {
            return Err(last_err.unwrap_or_else(|| std::io::Error::other("every connection attempt failed")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn interleaves_families() {
        let addrs = [a("1.1.1.1:1"), a("2.2.2.2:1"), a("[::1]:1"), a("[::2]:1"), a("[::3]:1")];
        let v6 = interleave(&addrs, true);
        assert_eq!(v6, vec![a("[::1]:1"), a("1.1.1.1:1"), a("[::2]:1"), a("2.2.2.2:1"), a("[::3]:1")]);
        let v4 = interleave(&addrs, false);
        assert_eq!(v4[0], a("1.1.1.1:1"));
        assert_eq!(v4[1], a("[::1]:1"));
    }

    #[tokio::test]
    async fn falls_back_past_a_dead_address() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let good = listener.local_addr().unwrap();
        // A port nothing listens on: refused at once, so the next attempt starts immediately.
        let dead = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap()
        };
        let accept = tokio::spawn(async move { listener.accept().await.map(|_| ()) });
        let (_s, used) = connect(&[dead, good], false).await.unwrap();
        assert_eq!(used, good);
        accept.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn reports_failure_when_all_fail() {
        let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        assert!(connect(&[dead], true).await.is_err());
    }
}
