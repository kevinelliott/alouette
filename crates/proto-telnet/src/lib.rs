//! Telnet as a pure state machine: feed it bytes from the server, get back the data to show
//! and the replies to send. No I/O, so it is easy to test and fuzz.
//!
//! It agrees to echo, suppress-go-ahead, binary, terminal type and window size, and refuses
//! everything else, which is what an interactive client needs.

pub const IAC: u8 = 255;
pub const DONT: u8 = 254;
pub const DO: u8 = 253;
pub const WONT: u8 = 252;
pub const WILL: u8 = 251;
pub const SB: u8 = 250;
pub const SE: u8 = 240;

pub const BINARY: u8 = 0;
pub const ECHO: u8 = 1;
pub const SGA: u8 = 3;
pub const TTYPE: u8 = 24;
pub const NAWS: u8 = 31;

const TTYPE_IS: u8 = 0;
const TTYPE_SEND: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Data,
    Iac,
    Verb(u8),
    Sub,
    SubIac,
    /// A CR arrived; a following NUL or LF belongs to it.
    Cr,
}

/// One side of a Telnet connection.
#[derive(Debug, Clone)]
pub struct Telnet {
    state: State,
    term: String,
    size: (u16, u16),
    /// Options the server does for us (WILL accepted).
    them: [bool; 256],
    /// Options we do for the server (DO accepted).
    us: [bool; 256],
    sub: Vec<u8>,
}

/// What came out of [`Telnet::receive`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Output {
    /// Bytes for the terminal.
    pub data: Vec<u8>,
    /// Bytes to send to the server.
    pub reply: Vec<u8>,
}

impl Telnet {
    /// `term` is what TTYPE reports, such as `xterm-256color`; `cols` and `rows` go to NAWS.
    pub fn new(term: &str, cols: u16, rows: u16) -> Self {
        Self {
            state: State::Data,
            term: term.to_ascii_uppercase(),
            size: (cols, rows),
            them: [false; 256],
            us: [false; 256],
            sub: Vec::new(),
        }
    }

    /// What a client says first: it would like to report its type and size, and to skip
    /// go-aheads.
    pub fn greeting(&self) -> Vec<u8> {
        vec![IAC, WILL, TTYPE, IAC, WILL, NAWS, IAC, DO, SGA]
    }

    /// True when the server echoes what we type (so the terminal should not).
    pub fn server_echoes(&self) -> bool {
        self.them[ECHO as usize]
    }

    pub fn receive(&mut self, input: &[u8]) -> Output {
        let mut out = Output::default();
        for &b in input {
            self.state = match (self.state, b) {
                (State::Data, IAC) => State::Iac,
                (State::Data, b'\r') => {
                    out.data.push(b'\r');
                    State::Cr
                }
                (State::Data, b) => {
                    out.data.push(b);
                    State::Data
                }
                (State::Cr, 0) => State::Data,
                (State::Cr, IAC) => State::Iac,
                (State::Cr, b) => {
                    out.data.push(b);
                    if b == b'\r' { State::Cr } else { State::Data }
                }
                (State::Iac, IAC) => {
                    out.data.push(IAC);
                    State::Data
                }
                (State::Iac, v @ (DO | DONT | WILL | WONT)) => State::Verb(v),
                (State::Iac, SB) => {
                    self.sub.clear();
                    State::Sub
                }
                // NOP, GA, data mark and the rest carry nothing for us.
                (State::Iac, _) => State::Data,
                (State::Verb(v), opt) => {
                    self.negotiate(v, opt, &mut out.reply);
                    State::Data
                }
                (State::Sub, IAC) => State::SubIac,
                (State::Sub, b) => {
                    if self.sub.len() < 1024 {
                        self.sub.push(b);
                    }
                    State::Sub
                }
                (State::SubIac, SE) => {
                    self.subnegotiation(&mut out.reply);
                    State::Data
                }
                (State::SubIac, IAC) => {
                    if self.sub.len() < 1024 {
                        self.sub.push(IAC);
                    }
                    State::Sub
                }
                (State::SubIac, _) => State::Sub,
            };
        }
        out
    }

    fn negotiate(&mut self, verb: u8, opt: u8, reply: &mut Vec<u8>) {
        let i = opt as usize;
        match verb {
            WILL => {
                let ok = matches!(opt, ECHO | SGA | BINARY);
                if ok != self.them[i] || !ok {
                    reply.extend_from_slice(&[IAC, if ok { DO } else { DONT }, opt]);
                }
                self.them[i] = ok;
            }
            WONT => {
                if self.them[i] {
                    reply.extend_from_slice(&[IAC, DONT, opt]);
                }
                self.them[i] = false;
            }
            DO => {
                let ok = matches!(opt, TTYPE | NAWS | SGA | BINARY);
                let was = self.us[i];
                if !ok {
                    reply.extend_from_slice(&[IAC, WONT, opt]);
                } else if !was && !matches!(opt, TTYPE | NAWS) {
                    reply.extend_from_slice(&[IAC, WILL, opt]);
                }
                self.us[i] = ok;
                if ok && opt == NAWS {
                    reply.extend_from_slice(&self.naws_bytes());
                }
            }
            DONT => {
                if self.us[i] {
                    reply.extend_from_slice(&[IAC, WONT, opt]);
                }
                self.us[i] = false;
            }
            _ => {}
        }
    }

    fn subnegotiation(&mut self, reply: &mut Vec<u8>) {
        if self.sub.first() == Some(&TTYPE) && self.sub.get(1) == Some(&TTYPE_SEND) {
            reply.extend_from_slice(&[IAC, SB, TTYPE, TTYPE_IS]);
            reply.extend_from_slice(self.term.as_bytes());
            reply.extend_from_slice(&[IAC, SE]);
        }
    }

    fn naws_bytes(&self) -> Vec<u8> {
        let mut v = vec![IAC, SB, NAWS];
        for n in [self.size.0, self.size.1] {
            for b in n.to_be_bytes() {
                v.push(b);
                if b == IAC {
                    v.push(IAC);
                }
            }
        }
        v.extend_from_slice(&[IAC, SE]);
        v
    }

    /// The window changed size. Returns bytes to send if the server asked for sizes.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Vec<u8> {
        self.size = (cols, rows);
        if self.us[NAWS as usize] { self.naws_bytes() } else { Vec::new() }
    }

    /// Prepare typed bytes for the wire: double IAC, and send CR as CR NUL outside binary mode.
    pub fn encode(&self, data: &[u8]) -> Vec<u8> {
        let binary = self.us[BINARY as usize];
        let mut out = Vec::with_capacity(data.len() + 4);
        for &b in data {
            out.push(b);
            if b == IAC {
                out.push(IAC);
            } else if b == b'\r' && !binary {
                out.push(0);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_data_passes_through() {
        let mut t = Telnet::new("xterm", 80, 24);
        let o = t.receive(b"hello\r\nworld\r\0!");
        assert_eq!(o.data, b"hello\r\nworld\r!");
        assert!(o.reply.is_empty());
    }

    #[test]
    fn escaped_iac_is_data() {
        let mut t = Telnet::new("xterm", 80, 24);
        assert_eq!(t.receive(&[b'a', IAC, IAC, b'b']).data, [b'a', IAC, b'b']);
    }

    #[test]
    fn accepts_echo_and_refuses_others() {
        let mut t = Telnet::new("xterm", 80, 24);
        let o = t.receive(&[IAC, WILL, ECHO, IAC, WILL, 99, IAC, DO, 42]);
        assert_eq!(o.reply, [IAC, DO, ECHO, IAC, DONT, 99, IAC, WONT, 42]);
        assert!(t.server_echoes());
        // Repeating WILL ECHO must not loop.
        assert!(t.receive(&[IAC, WILL, ECHO]).reply.is_empty());
    }

    #[test]
    fn reports_terminal_type() {
        let mut t = Telnet::new("xterm-256color", 80, 24);
        t.receive(&[IAC, DO, TTYPE]);
        let o = t.receive(&[IAC, SB, TTYPE, TTYPE_SEND, IAC, SE]);
        let mut want = vec![IAC, SB, TTYPE, TTYPE_IS];
        want.extend_from_slice(b"XTERM-256COLOR");
        want.extend_from_slice(&[IAC, SE]);
        assert_eq!(o.reply, want);
    }

    #[test]
    fn window_size() {
        let mut t = Telnet::new("xterm", 80, 24);
        assert!(t.resize(100, 30).is_empty());
        let o = t.receive(&[IAC, DO, NAWS]);
        assert_eq!(o.reply, [IAC, SB, NAWS, 0, 100, 0, 30, IAC, SE]);
        // 255 columns must be escaped.
        assert_eq!(t.resize(255, 30), [IAC, SB, NAWS, 0, 255, 255, 0, 30, IAC, SE]);
    }

    #[test]
    fn encodes_input() {
        let t = Telnet::new("xterm", 80, 24);
        assert_eq!(t.encode(&[b'a', b'\r', IAC]), [b'a', b'\r', 0, IAC, IAC]);
    }

    #[test]
    fn split_sequences_survive() {
        let mut t = Telnet::new("xterm", 80, 24);
        let mut reply = t.receive(&[b'x', IAC]).reply;
        reply.extend(t.receive(&[WILL]).reply);
        let o = t.receive(&[SGA, b'y']);
        reply.extend(o.reply);
        assert_eq!(reply, [IAC, DO, SGA]);
        assert_eq!(o.data, b"y");
    }
}
