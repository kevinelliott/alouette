//! Read `~/.ssh/config`, the way OpenSSH does for the options Alouette uses.
//!
//! The first value for an option wins, except `IdentityFile` and `LocalForward`, which add
//! up. `Include` is followed (with `*` patterns), `Match` blocks are skipped, and host
//! patterns support `*`, `?` and `!`.

use std::path::{Path, PathBuf};

/// One `Host` block.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Block {
    patterns: Vec<String>,
    /// Lowercased option name and its value.
    opts: Vec<(String, String)>,
}

/// A parsed ssh_config.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SshConfig {
    blocks: Vec<Block>,
}

/// Settings for one host after applying every matching block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostConfig {
    /// What was asked for, such as `web`.
    pub alias: String,
    /// What to connect to.
    pub hostname: String,
    pub user: Option<String>,
    pub port: u16,
    pub identity_files: Vec<PathBuf>,
    pub identities_only: bool,
    /// `ProxyJump` hops, nearest first.
    pub jumps: Vec<String>,
    pub local_forwards: Vec<String>,
    pub keepalive_secs: Option<u64>,
    pub known_hosts: Option<PathBuf>,
    /// `StrictHostKeyChecking`: `yes` refuses unknown hosts, `accept-new` takes them.
    pub strict: Option<String>,
}

impl SshConfig {
    /// Read `~/.ssh/config`. A missing file gives an empty config.
    pub fn load_default() -> Self {
        match home() {
            Some(h) => Self::load(&h.join(".ssh").join("config")),
            None => Self::default(),
        }
    }

    pub fn load(path: &Path) -> Self {
        let mut cfg = Self::default();
        let base = path.parent().map(Path::to_path_buf).unwrap_or_default();
        if let Ok(text) = std::fs::read_to_string(path) {
            cfg.parse_into(&text, &base, 0);
        }
        cfg
    }

    pub fn parse(text: &str) -> Self {
        let mut cfg = Self::default();
        cfg.parse_into(text, Path::new("."), 0);
        cfg
    }

    fn parse_into(&mut self, text: &str, base: &Path, depth: u8) {
        // Options before the first Host line apply to every host.
        let mut current = Block { patterns: vec!["*".into()], opts: vec![] };
        let mut skipping = false;
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = split_option(line);
            let key = key.to_ascii_lowercase();
            match key.as_str() {
                "host" => {
                    self.push(std::mem::replace(&mut current, Block { patterns: words(&value), opts: vec![] }));
                    skipping = false;
                }
                "match" => {
                    self.push(std::mem::replace(&mut current, Block { patterns: vec![], opts: vec![] }));
                    skipping = true;
                }
                "include" if depth < 8 => {
                    let patterns = current.patterns.clone();
                    let pending = std::mem::replace(&mut current, Block { patterns, opts: vec![] });
                    self.push(pending);
                    for pat in words(&value) {
                        for file in expand_include(&pat, base) {
                            if let Ok(t) = std::fs::read_to_string(&file) {
                                self.parse_into(&t, base, depth + 1);
                            }
                        }
                    }
                }
                _ if !skipping => current.opts.push((key, value)),
                _ => {}
            }
        }
        self.push(current);
    }

    fn push(&mut self, b: Block) {
        if !b.opts.is_empty() && !b.patterns.is_empty() {
            self.blocks.push(b);
        }
    }

    /// Add a host ahead of the file's entries, so its settings win.
    pub fn add_host(&mut self, name: &str, opts: Vec<(&str, String)>) {
        let opts: Vec<(String, String)> = opts.into_iter().map(|(k, v)| (k.to_ascii_lowercase(), v)).collect();
        if !opts.is_empty() {
            self.blocks.insert(0, Block { patterns: vec![name.to_string()], opts });
        }
    }

    /// Named hosts (no wildcards), for a host list.
    pub fn hosts(&self) -> Vec<String> {
        let mut out: Vec<String> = vec![];
        for b in &self.blocks {
            for p in &b.patterns {
                if !p.contains(['*', '?', '!']) && !out.contains(p) {
                    out.push(p.clone());
                }
            }
        }
        out
    }

    /// Apply every block that matches `alias`.
    pub fn resolve(&self, alias: &str) -> HostConfig {
        let mut h = HostConfig { alias: alias.into(), port: 22, ..HostConfig::default() };
        let mut seen: Vec<&str> = vec![];
        let mut hostname = None;
        let mut port = None;
        for b in self.blocks.iter().filter(|b| matches(&b.patterns, alias)) {
            for (k, v) in &b.opts {
                let first = !seen.contains(&k.as_str());
                match k.as_str() {
                    "identityfile" => h.identity_files.push(PathBuf::from(v)),
                    "localforward" => h.local_forwards.push(v.split_whitespace().collect::<Vec<_>>().join(":")),
                    _ if !first => {}
                    "hostname" => hostname = Some(v.clone()),
                    "user" => h.user = Some(v.clone()),
                    "port" => port = v.parse().ok(),
                    "identitiesonly" => h.identities_only = v.eq_ignore_ascii_case("yes"),
                    "proxyjump" if !v.eq_ignore_ascii_case("none") => {
                        h.jumps = v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                    }
                    "serveraliveinterval" => h.keepalive_secs = v.parse().ok(),
                    "userknownhostsfile" => {
                        h.known_hosts = Some(PathBuf::from(v.split_whitespace().next().unwrap_or(v)))
                    }
                    "stricthostkeychecking" => h.strict = Some(v.to_ascii_lowercase()),
                    _ => {}
                }
                if first {
                    seen.push(k);
                }
            }
        }
        h.port = port.unwrap_or(22);
        h.hostname = hostname.map(|n| n.replace("%h", alias).replace("%%", "%")).unwrap_or_else(|| alias.into());
        let user = h.user.clone().unwrap_or_else(local_user);
        let expand = |p: &PathBuf| -> PathBuf {
            let s = p.to_string_lossy();
            let s = s
                .replace("%d", &home().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default())
                .replace("%h", &h.hostname)
                .replace("%r", &user)
                .replace("%u", &local_user())
                .replace("%p", &h.port.to_string())
                .replace("%%", "%");
            tilde(&s)
        };
        h.identity_files = h.identity_files.iter().map(expand).collect();
        h.known_hosts = h.known_hosts.as_ref().map(expand);
        h
    }
}

/// `user@host:port` or `host`, as in ProxyJump and the ssh command line.
pub fn parse_destination(s: &str) -> (Option<String>, String, Option<u16>) {
    let s = s.trim().trim_start_matches("ssh://");
    let (user, rest) = match s.rsplit_once('@') {
        Some((u, r)) => (Some(u.to_string()), r),
        None => (None, s),
    };
    // [v6]:port, host:port, or a bare v6 address.
    if let Some(r) = rest.strip_prefix('[')
        && let Some((h, p)) = r.split_once(']')
    {
        return (user, h.to_string(), p.strip_prefix(':').and_then(|p| p.parse().ok()));
    }
    match rest.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => (user, h.to_string(), p.parse().ok()),
        _ => (user, rest.to_string(), None),
    }
}

fn split_option(line: &str) -> (String, String) {
    let line = line.trim();
    let idx = line.find(|c: char| c.is_whitespace() || c == '=').unwrap_or(line.len());
    let key = line[..idx].to_string();
    let rest = line[idx..].trim_start_matches(|c: char| c.is_whitespace() || c == '=').trim();
    let value = rest.strip_prefix('"').and_then(|r| r.strip_suffix('"')).unwrap_or(rest);
    (key, value.to_string())
}

fn words(s: &str) -> Vec<String> {
    s.split_whitespace().map(|w| w.trim_matches('"').to_string()).collect()
}

fn matches(patterns: &[String], host: &str) -> bool {
    let mut hit = false;
    for p in patterns {
        if let Some(neg) = p.strip_prefix('!') {
            if glob(neg, host) {
                return false;
            }
        } else if glob(p, host) {
            hit = true;
        }
    }
    hit
}

/// `*` and `?` matching, case-insensitive as OpenSSH does for host names.
pub fn glob(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut ti, mut star, mut mark) = (0, 0, None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

fn expand_include(pat: &str, base: &Path) -> Vec<PathBuf> {
    let p = tilde(pat);
    let p = if p.is_absolute() { p } else { base.join(p) };
    let Some(name) = p.file_name().map(|n| n.to_string_lossy().into_owned()) else { return vec![] };
    if !name.contains(['*', '?']) {
        return vec![p];
    }
    let dir = p.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut out: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|rd| rd.flatten().filter(|e| glob(&name, &e.file_name().to_string_lossy())).map(|e| e.path()).collect())
        .unwrap_or_default();
    out.sort();
    out
}

pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from)
}

pub fn tilde(s: &str) -> PathBuf {
    match s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\")) {
        Some(rest) => home().map(|h| h.join(rest)).unwrap_or_else(|| PathBuf::from(s)),
        None => PathBuf::from(s),
    }
}

pub fn local_user() -> String {
    std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_else(|_| "root".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
# global
ServerAliveInterval 15

Host web web2
    HostName web.example.org
    User deploy
    Port 2222
    IdentityFile ~/.ssh/web_ed25519
    ProxyJump bastion,admin@inner:2200
    LocalForward 8080 localhost:80

Host *.internal !secret.internal
    User ops
    IdentitiesOnly yes

Match host foo
    User nobody

Host *
    User fallback
    IdentityFile ~/.ssh/id_ed25519
    StrictHostKeyChecking accept-new
"#;

    #[test]
    fn resolves_a_named_host() {
        let c = SshConfig::parse(SAMPLE);
        let h = c.resolve("web");
        assert_eq!(h.hostname, "web.example.org");
        assert_eq!(h.user.as_deref(), Some("deploy"));
        assert_eq!(h.port, 2222);
        assert_eq!(h.jumps, vec!["bastion", "admin@inner:2200"]);
        assert_eq!(h.local_forwards, vec!["8080:localhost:80"]);
        assert_eq!(h.keepalive_secs, Some(15));
        assert_eq!(h.strict.as_deref(), Some("accept-new"));
        // IdentityFile adds up, in order.
        assert_eq!(h.identity_files.len(), 2);
        assert!(h.identity_files[0].ends_with(".ssh/web_ed25519"));
    }

    #[test]
    fn patterns_and_negation() {
        let c = SshConfig::parse(SAMPLE);
        assert_eq!(c.resolve("db.internal").user.as_deref(), Some("ops"));
        assert!(c.resolve("db.internal").identities_only);
        assert_eq!(c.resolve("secret.internal").user.as_deref(), Some("fallback"));
        // Match blocks are skipped, not merged into the previous Host.
        assert_eq!(c.resolve("foo").user.as_deref(), Some("fallback"));
        assert_eq!(c.resolve("other").hostname, "other");
        assert_eq!(c.resolve("other").port, 22);
    }

    #[test]
    fn lists_named_hosts() {
        assert_eq!(SshConfig::parse(SAMPLE).hosts(), vec!["web", "web2"]);
    }

    #[test]
    fn equals_and_quotes() {
        let c = SshConfig::parse("Host=a\n  HostName = \"b.example\"\n  Port=10");
        let h = c.resolve("a");
        assert_eq!((h.hostname.as_str(), h.port), ("b.example", 10));
    }

    #[test]
    fn globbing() {
        assert!(glob("*.example.org", "a.example.org"));
        assert!(glob("web?", "web1"));
        assert!(!glob("web?", "web"));
        assert!(glob("*", ""));
        assert!(glob("A*", "abc"));
    }

    #[test]
    fn destinations() {
        assert_eq!(parse_destination("me@host:2200"), (Some("me".into()), "host".into(), Some(2200)));
        assert_eq!(parse_destination("host"), (None, "host".into(), None));
        assert_eq!(parse_destination("[::1]:22"), (None, "::1".into(), Some(22)));
        assert_eq!(parse_destination("::1"), (None, "::1".into(), None));
        assert_eq!(parse_destination("ssh://a@b"), (Some("a".into()), "b".into(), None));
    }

    #[test]
    fn includes() {
        let dir = std::env::temp_dir().join(format!("alouette-ssh-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("conf.d")).unwrap();
        std::fs::write(dir.join("conf.d/a.conf"), "Host inc\n  HostName included.example\n").unwrap();
        std::fs::write(dir.join("config"), "Include conf.d/*.conf\nHost x\n  User y\n").unwrap();
        let c = SshConfig::load(&dir.join("config"));
        assert_eq!(c.resolve("inc").hostname, "included.example");
        assert_eq!(c.resolve("x").user.as_deref(), Some("y"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
