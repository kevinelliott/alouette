//! Shared-host mode. When Alouette serves several people over SSH (`alouette serve`), they
//! must not reach the server's own shell, files or programs. Modules check [`restricted`]
//! before doing anything local: a local shell, the local file system, a program such as
//! Mosh or an AI CLI, or opening a browser on the server.

use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};

static RESTRICTED: AtomicBool = AtomicBool::new(false);
static PRIVATE_OK: AtomicBool = AtomicBool::new(false);

/// Turn shared-host mode on or off for the whole process.
pub fn set_restricted(on: bool) {
    RESTRICTED.store(on, Ordering::SeqCst);
}

/// True when local shells, files and programs are off.
pub fn restricted() -> bool {
    RESTRICTED.load(Ordering::SeqCst)
}

/// The error to show when something local is refused.
pub const REFUSED: &str = "not available on a shared Alouette server";

/// Let a shared server's users reach its own loopback and private networks (`serve.allow_private`).
pub fn set_private_ok(on: bool) {
    PRIVATE_OK.store(on, Ordering::SeqCst);
}

/// True when a shared server must not connect to `ip`: it is the server itself or on its
/// private network, which the people using it should not reach through it.
pub fn blocked_ip(ip: IpAddr) -> bool {
    restricted() && !PRIVATE_OK.load(Ordering::SeqCst) && !is_public(ip)
}

/// Whether an address is on the public internet.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => {
            let o = a.octets();
            !(a.is_loopback()
                || a.is_private()
                || a.is_link_local()
                || a.is_unspecified()
                || a.is_broadcast()
                || a.is_multicast()
                || a.is_documentation()
                || o[0] == 0
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
                || (o[0] == 198 && (o[1] & 0xfe) == 18)
                || o[0] >= 240)
        }
        IpAddr::V6(a) => {
            if let Some(v4) = a.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            let s = a.segments();
            !(a.is_loopback()
                || a.is_unspecified()
                || a.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80
                || (s[0] == 0x2001 && s[1] == 0x0db8)
                || (s[0] == 0x64 && s[1] == 0xff9b && nat64_inner_private(&a)))
        }
    }
}

/// True when the IPv4 address inside a NAT64 address is not public.
fn nat64_inner_private(a: &std::net::Ipv6Addr) -> bool {
    let o = a.octets();
    !is_public(IpAddr::V4(std::net::Ipv4Addr::new(o[12], o[13], o[14], o[15])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_addresses() {
        for a in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.0.1",
            "172.16.5.4",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fe80::1",
            "fd00::1",
            "::ffff:127.0.0.1",
            "64:ff9b::a00:1",
        ] {
            assert!(!is_public(a.parse().unwrap()), "{a} is not public");
        }
        for a in ["1.1.1.1", "134.84.119.25", "2606:4700::1111", "::ffff:8.8.8.8", "64:ff9b::808:808"] {
            assert!(is_public(a.parse().unwrap()), "{a} is public");
        }
    }
}
