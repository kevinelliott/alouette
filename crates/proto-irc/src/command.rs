//! What a person types: `/join #rust`, `/me waves`, or plain text for the current buffer.

use crate::message::Message;
use crate::session::{Kind, Output, Session};

/// Something the window handles itself rather than the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Local {
    /// Open (and maybe say something in) a private buffer.
    Query {
        nick: String,
        text: Option<String>,
    },
    /// Close the current buffer, parting a channel.
    Close,
    Clear,
    Help,
    /// Disconnect, with a reason.
    Quit(String),
    /// Connect again.
    Reconnect,
}

/// The result of one typed line.
#[derive(Debug, Default)]
pub struct Typed {
    pub out: Output,
    pub local: Option<Local>,
}

pub const HELP: &str = "/join #chan [key], /part [reason], /msg nick text, /query nick, /me action, \
/notice target text, /nick name, /topic [text], /whois nick, /mode [args], /kick nick [reason], \
/invite nick [#chan], /away [text], /back, /names, /ctcp nick what, /raw line, /close, /clear, \
/reconnect, /quit [reason]. Other /commands go to the server as typed; // starts a line with /.";

/// Turn one typed line into messages. `buffer` is the channel or nick being shown, `None`
/// for the server buffer.
pub fn typed(s: &Session, buffer: Option<&str>, line: &str) -> Result<Typed, String> {
    let mut t = Typed::default();
    let line = line.trim_end_matches(['\r', '\n']);
    if line.is_empty() {
        return Ok(t);
    }
    let here = || buffer.map(str::to_string).ok_or_else(|| "Switch to a channel or a person first".to_string());
    let (cmd, args) = match line.strip_prefix('/') {
        Some(r) if !r.starts_with('/') => {
            let (c, a) = r.split_once(' ').unwrap_or((r, ""));
            (c.to_ascii_lowercase(), a.trim())
        }
        _ => {
            let text = line.strip_prefix('/').unwrap_or(line);
            t.out = s.say(&here()?, text, Kind::Message);
            return Ok(t);
        }
    };
    let words: Vec<&str> = args.split_whitespace().collect();
    let first_rest = || args.split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((args, ""));
    let send = |t: &mut Typed, cmd: &str, p: &[&str]| t.out.send.push(Message::new(cmd, p));
    match cmd.as_str() {
        "j" | "join" => {
            let (chans, key) = first_rest();
            if chans.is_empty() {
                return Err("Which channel? /join #name".into());
            }
            let chans: Vec<String> = chans
                .split(',')
                .map(|c| if s.support.is_channel(c) { c.to_string() } else { format!("#{c}") })
                .collect();
            let chans = chans.join(",");
            if key.is_empty() { send(&mut t, "JOIN", &[&chans]) } else { send(&mut t, "JOIN", &[&chans, key]) }
        }
        "part" | "leave" => {
            let (chan, why) = match words.first() {
                Some(w) if s.support.is_channel(w) => (w.to_string(), first_rest().1),
                _ => (here()?, args),
            };
            if why.is_empty() { send(&mut t, "PART", &[&chan]) } else { send(&mut t, "PART", &[&chan, why]) }
        }
        "msg" | "privmsg" | "m" => {
            let (to, text) = first_rest();
            if to.is_empty() || text.is_empty() {
                return Err("/msg nick text".into());
            }
            t.out = s.say(to, text, Kind::Message);
        }
        "notice" => {
            let (to, text) = first_rest();
            if to.is_empty() || text.is_empty() {
                return Err("/notice target text".into());
            }
            t.out = s.say(to, text, Kind::Notice);
        }
        "q" | "query" => {
            let (nick, text) = first_rest();
            if nick.is_empty() {
                return Err("/query nick".into());
            }
            if !text.is_empty() {
                t.out = s.say(nick, text, Kind::Message);
            }
            t.local = Some(Local::Query { nick: nick.into(), text: (!text.is_empty()).then(|| text.into()) });
        }
        "me" => t.out = s.say(&here()?, args, Kind::Action),
        "nick" => match words.first() {
            Some(n) => send(&mut t, "NICK", &[n]),
            None => return Err("/nick newname".into()),
        },
        "topic" => {
            let (chan, text) = match words.first() {
                Some(w) if s.support.is_channel(w) => (w.to_string(), first_rest().1),
                _ => (here()?, args),
            };
            if text.is_empty() { send(&mut t, "TOPIC", &[&chan]) } else { send(&mut t, "TOPIC", &[&chan, text]) }
        }
        "whois" | "wi" => match words.first() {
            Some(n) => send(&mut t, "WHOIS", &[n]),
            None => send(&mut t, "WHOIS", &[&here()?]),
        },
        "mode" => {
            if words.is_empty() {
                send(&mut t, "MODE", &[&here()?])
            } else if s.support.is_channel(words[0]) || s.is_me(words[0]) {
                send(&mut t, "MODE", &words)
            } else {
                let chan = here()?;
                let mut p = vec![chan.as_str()];
                p.extend(&words);
                send(&mut t, "MODE", &p)
            }
        }
        "op" | "deop" | "voice" | "devoice" if !words.is_empty() => {
            let chan = here()?;
            let m = match cmd.as_str() {
                "op" => "+o",
                "deop" => "-o",
                "voice" => "+v",
                _ => "-v",
            };
            let modes = format!("{}{}", &m[..1], m[1..].repeat(words.len()));
            let mut p = vec![chan.as_str(), modes.as_str()];
            p.extend(&words);
            send(&mut t, "MODE", &p)
        }
        "kick" => {
            let (nick, why) = first_rest();
            if nick.is_empty() {
                return Err("/kick nick [reason]".into());
            }
            let chan = here()?;
            if why.is_empty() {
                send(&mut t, "KICK", &[&chan, nick])
            } else {
                send(&mut t, "KICK", &[&chan, nick, why])
            }
        }
        "invite" => {
            let nick = words.first().ok_or("/invite nick [#channel]")?;
            let chan = match words.get(1) {
                Some(c) => c.to_string(),
                None => here()?,
            };
            send(&mut t, "INVITE", &[nick, &chan])
        }
        "away" => {
            if args.is_empty() {
                send(&mut t, "AWAY", &[])
            } else {
                send(&mut t, "AWAY", &[args])
            }
        }
        "back" => send(&mut t, "AWAY", &[]),
        "names" => send(&mut t, "NAMES", &[&words.first().map(|w| w.to_string()).map_or_else(here, Ok)?]),
        "ctcp" => {
            let (nick, what) = first_rest();
            if nick.is_empty() || what.is_empty() {
                return Err("/ctcp nick VERSION".into());
            }
            let (c, a) = what.split_once(' ').unwrap_or((what, ""));
            let body = if a.is_empty() {
                format!("\x01{}\x01", c.to_uppercase())
            } else {
                format!("\x01{} {a}\x01", c.to_uppercase())
            };
            send(&mut t, "PRIVMSG", &[nick, &body])
        }
        "raw" | "quote" => match Message::parse(args) {
            Some(m) => t.out.send.push(m),
            None => return Err("/raw COMMAND params".into()),
        },
        "close" | "wc" => t.local = Some(Local::Close),
        "clear" => t.local = Some(Local::Clear),
        "help" => t.local = Some(Local::Help),
        "reconnect" | "connect" | "server" => t.local = Some(Local::Reconnect),
        "quit" | "disconnect" => t.local = Some(Local::Quit(args.to_string())),
        other => match Message::parse(&format!("{} {args}", other.to_ascii_uppercase())) {
            Some(m) => t.out.send.push(m),
            None => return Err(format!("Unknown command /{other}")),
        },
    }
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Config;

    fn lines(s: &Session, buffer: Option<&str>, line: &str) -> Vec<String> {
        typed(s, buffer, line).unwrap().out.send.iter().map(|m| m.to_line()).collect()
    }

    #[test]
    fn commands() {
        let s = Session::new(Config { nick: "ada".into(), ..Default::default() });
        assert_eq!(lines(&s, None, "/join rust,#go key"), ["JOIN #rust,#go key"]);
        assert_eq!(lines(&s, Some("#rust"), "/part"), ["PART #rust"]);
        assert_eq!(lines(&s, Some("#rust"), "/part #go bye now"), ["PART #go :bye now"]);
        assert_eq!(lines(&s, Some("#rust"), "hello"), ["PRIVMSG #rust hello"]);
        assert_eq!(lines(&s, Some("#rust"), "//slash"), ["PRIVMSG #rust /slash"]);
        assert_eq!(lines(&s, Some("#rust"), "/me waves"), ["PRIVMSG #rust :\x01ACTION waves\x01"]);
        assert_eq!(lines(&s, None, "/msg bob hi there"), ["PRIVMSG bob :hi there"]);
        assert_eq!(lines(&s, Some("#rust"), "/topic New topic"), ["TOPIC #rust :New topic"]);
        assert_eq!(lines(&s, Some("#rust"), "/op bob carol"), ["MODE #rust +oo bob carol"]);
        assert_eq!(lines(&s, Some("#rust"), "/mode +m"), ["MODE #rust +m"]);
        assert_eq!(lines(&s, Some("#rust"), "/kick bob be nice"), ["KICK #rust bob :be nice"]);
        assert_eq!(lines(&s, None, "/ctcp bob version"), ["PRIVMSG bob \x01VERSION\x01"]);
        assert_eq!(lines(&s, None, "/raw PRIVMSG x :y z"), ["PRIVMSG x :y z"]);
        assert_eq!(lines(&s, None, "/knock #secret"), ["KNOCK #secret"]);
        assert_eq!(lines(&s, None, "/away lunch"), ["AWAY lunch"]);
        let t = typed(&s, None, "/query bob hey").unwrap();
        assert_eq!(t.local, Some(Local::Query { nick: "bob".into(), text: Some("hey".into()) }));
        assert!(typed(&s, None, "hello").is_err());
        assert!(typed(&s, None, "/msg bob").is_err());
        assert_eq!(typed(&s, None, "/quit gone").unwrap().local, Some(Local::Quit("gone".into())));
    }
}
