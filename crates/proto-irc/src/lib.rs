//! IRC for Alouette: messages with IRCv3 tags, capability negotiation, SASL PLAIN and
//! EXTERNAL, ISUPPORT, CTCP, mIRC formatting, chathistory and a client state machine.

pub mod client;
pub mod command;
pub mod format;
pub mod message;
pub mod session;
pub mod support;
#[cfg(feature = "test-server")]
pub mod testserver;

pub use message::Message;
pub use session::{Config, Event, Kind, Line, Sasl, Session};

/// `host[:port]` and whether to use TLS. No port means TLS on 6697; `+port` and 6697
/// mean TLS; any other port is plain text, as IRC convention has it. `ircs://` is TLS.
pub fn endpoint(host: &str, tls_scheme: Option<bool>) -> (String, u16, bool) {
    let h = host.trim();
    let (name, port) = match h.rsplit_once(':') {
        Some((n, p))
            if (!n.contains(':') || n.starts_with('[')) && p.trim_start_matches('+').parse::<u16>().is_ok() =>
        {
            (n.trim_matches(['[', ']']).to_string(), Some(p))
        }
        _ => (h.trim_matches(['[', ']']).to_string(), None),
    };
    let plus = port.is_some_and(|p| p.starts_with('+'));
    let port: Option<u16> = port.and_then(|p| p.trim_start_matches('+').parse().ok());
    let tls = match tls_scheme {
        Some(t) => t || plus,
        None => plus || port.is_none_or(|p| p == 6697),
    };
    let port = port.unwrap_or(if tls { 6697 } else { 6667 });
    (name, port, tls)
}

#[cfg(test)]
mod tests {
    #[test]
    fn endpoints() {
        assert_eq!(super::endpoint("irc.libera.chat", None), ("irc.libera.chat".into(), 6697, true));
        assert_eq!(super::endpoint("irc.example.org:6667", None), ("irc.example.org".into(), 6667, false));
        assert_eq!(super::endpoint("irc.example.org:6697", None), ("irc.example.org".into(), 6697, true));
        assert_eq!(super::endpoint("127.0.0.1:4000", None), ("127.0.0.1".into(), 4000, false));
        assert_eq!(super::endpoint("irc.example.org:+7000", None), ("irc.example.org".into(), 7000, true));
        assert_eq!(super::endpoint("irc.example.org", Some(false)), ("irc.example.org".into(), 6667, false));
        assert_eq!(super::endpoint("[::1]:6667", None), ("::1".into(), 6667, false));
    }
}
