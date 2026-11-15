//! RPL_ISUPPORT (005): what this server supports, and how it compares names.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Casemap {
    Ascii,
    Rfc1459,
    StrictRfc1459,
}

#[derive(Debug, Clone)]
pub struct Support {
    pub network: Option<String>,
    pub chantypes: String,
    /// (mode, symbol) from highest to lowest, such as ('o', '@').
    pub prefixes: Vec<(char, char)>,
    pub casemap: Casemap,
    /// CHANMODES: lists, always-parameter, parameter-when-set, never-parameter.
    pub chanmodes: [String; 4],
    pub statusmsg: String,
    pub raw: HashMap<String, String>,
}

impl Default for Support {
    fn default() -> Self {
        Self {
            network: None,
            chantypes: "#&".into(),
            prefixes: vec![('o', '@'), ('v', '+')],
            casemap: Casemap::Rfc1459,
            chanmodes: ["beI".into(), "k".into(), "l".into(), "imnpst".into()],
            statusmsg: String::new(),
            raw: HashMap::new(),
        }
    }
}

impl Support {
    /// Read the tokens of one 005 line (without the nick and the trailing text).
    pub fn apply(&mut self, tokens: &[String]) {
        for t in tokens {
            let (k, v) = t.split_once('=').unwrap_or((t, ""));
            if let Some(k) = k.strip_prefix('-') {
                self.raw.remove(k);
                continue;
            }
            self.raw.insert(k.to_string(), v.to_string());
            match k {
                "NETWORK" => self.network = Some(v.to_string()),
                "CHANTYPES" => self.chantypes = v.to_string(),
                "STATUSMSG" => self.statusmsg = v.to_string(),
                "CASEMAPPING" => {
                    self.casemap = match v {
                        "ascii" => Casemap::Ascii,
                        "strict-rfc1459" => Casemap::StrictRfc1459,
                        _ => Casemap::Rfc1459,
                    }
                }
                "PREFIX" => {
                    if let Some((modes, syms)) = v.strip_prefix('(').and_then(|r| r.split_once(')')) {
                        self.prefixes = modes.chars().zip(syms.chars()).collect();
                    } else if v.is_empty() {
                        self.prefixes.clear();
                    }
                }
                "CHANMODES" => {
                    let mut parts = v.split(',').map(str::to_string);
                    for slot in &mut self.chanmodes {
                        *slot = parts.next().unwrap_or_default();
                    }
                }
                _ => {}
            }
        }
    }

    /// Lower-case a nick or channel the way this server compares them.
    pub fn fold(&self, s: &str) -> String {
        s.chars()
            .map(|c| match (c, self.casemap) {
                ('[', Casemap::Rfc1459 | Casemap::StrictRfc1459) => '{',
                (']', Casemap::Rfc1459 | Casemap::StrictRfc1459) => '}',
                ('\\', Casemap::Rfc1459 | Casemap::StrictRfc1459) => '|',
                ('~', Casemap::Rfc1459) => '^',
                (c, _) => c.to_ascii_lowercase(),
            })
            .collect()
    }

    pub fn eq(&self, a: &str, b: &str) -> bool {
        self.fold(a) == self.fold(b)
    }

    pub fn is_channel(&self, s: &str) -> bool {
        let s = s.trim_start_matches(|c| self.statusmsg.contains(c));
        s.chars().next().is_some_and(|c| self.chantypes.contains(c))
    }

    /// The symbol for a mode such as 'o', if it is a prefix mode.
    pub fn symbol(&self, mode: char) -> Option<char> {
        self.prefixes.iter().find(|(m, _)| *m == mode).map(|(_, s)| *s)
    }

    /// Rank of a prefix symbol: 0 is highest.
    pub fn rank(&self, sym: char) -> usize {
        self.prefixes.iter().position(|(_, s)| *s == sym).unwrap_or(usize::MAX)
    }

    /// Whether a channel mode takes a parameter when set (`+`) or unset (`-`).
    pub fn takes_param(&self, mode: char, set: bool) -> bool {
        self.symbol(mode).is_some()
            || self.chanmodes[0].contains(mode)
            || self.chanmodes[1].contains(mode)
            || (set && self.chanmodes[2].contains(mode))
    }

    /// Split a NAMES entry like `@+nick!u@h` into its prefixes and nick.
    pub fn split_prefixes<'a>(&self, entry: &'a str) -> (String, &'a str) {
        let rest = entry.trim_start_matches(|c| self.prefixes.iter().any(|(_, s)| *s == c));
        let pre = entry[..entry.len() - rest.len()].to_string();
        (pre, rest.split('!').next().unwrap_or(rest))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_tokens() {
        let mut s = Support::default();
        let toks: Vec<String> = ["NETWORK=Libera.Chat", "PREFIX=(qaohv)~&@%+", "CASEMAPPING=ascii", "STATUSMSG=@+"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        s.apply(&toks);
        assert_eq!(s.network.as_deref(), Some("Libera.Chat"));
        assert_eq!(s.symbol('h'), Some('%'));
        assert!(s.is_channel("@#ops"));
        assert!(!s.is_channel("nick"));
        assert_eq!(s.split_prefixes("~@ada!a@h"), ("~@".to_string(), "ada"));
        assert!(!s.eq("a[b", "a{b"));
        s.casemap = Casemap::Rfc1459;
        assert!(s.eq("Nick[Away]", "nick{away}"));
        assert!(s.takes_param('k', false));
        assert!(s.takes_param('l', true) && !s.takes_param('l', false));
    }
}
