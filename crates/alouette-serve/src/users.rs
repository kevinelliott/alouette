//! Serve-mode users: one folder each under `users/`, holding an OpenSSH `authorized_keys`
//! file and that user's own Alouette config, data and cache.

use std::path::{Path, PathBuf};

use russh::keys::PublicKey;

pub const GUEST: &str = "guest";

#[derive(Clone, Debug)]
pub struct Users {
    root: PathBuf,
}

/// Letters, digits, `-` and `_`, up to 32, starting with a letter. Not `guest`.
pub fn valid_name(name: &str) -> bool {
    let mut c = name.chars();
    c.next().is_some_and(|f| f.is_ascii_lowercase())
        && name.len() <= 32
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && name != GUEST
}

/// Parse `authorized_keys` text. Options before the key type are not supported and such lines
/// are skipped, as are comments and blank lines.
pub fn parse_keys(text: &str) -> Vec<PublicKey> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| PublicKey::from_openssh(l).ok())
        .collect()
}

impl Users {
    pub fn new(root: &Path) -> Self {
        Self { root: root.join("users") }
    }

    pub fn dir(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    pub fn exists(&self, name: &str) -> bool {
        valid_name(name) && self.dir(name).join("authorized_keys").is_file()
    }

    pub fn list(&self) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(&self.root)
            .map(|rd| {
                rd.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| self.exists(n)).collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    pub fn keys(&self, name: &str) -> Vec<PublicKey> {
        if !valid_name(name) {
            return Vec::new();
        }
        std::fs::read_to_string(self.dir(name).join("authorized_keys")).map(|t| parse_keys(&t)).unwrap_or_default()
    }

    /// True when `key` may sign in as `name`.
    pub fn allows(&self, name: &str, key: &PublicKey) -> bool {
        self.keys(name).iter().any(|k| k.key_data() == key.key_data())
    }

    /// Create the user if needed and add keys from `authorized_keys` text. Returns how many
    /// keys were new.
    pub fn add(&self, name: &str, keys: &str) -> Result<usize, String> {
        if !valid_name(name) {
            return Err(format!(
                "`{name}` is not a usable name: use lowercase letters, digits, - and _, starting with a letter"
            ));
        }
        let new = parse_keys(keys);
        if new.is_empty() {
            return Err("no SSH public keys found".into());
        }
        let have = self.keys(name);
        let mut text = std::fs::read_to_string(self.dir(name).join("authorized_keys")).unwrap_or_default();
        let mut added = 0;
        for k in new {
            if have.iter().any(|h| h.key_data() == k.key_data()) {
                continue;
            }
            if let Ok(line) = k.to_openssh() {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&line);
                text.push('\n');
                added += 1;
            }
        }
        crate::secrets::write_private(&self.dir(name).join("authorized_keys"), text.as_bytes())
            .map_err(|e| e.to_string())?;
        Ok(added)
    }

    /// How much the user's folder holds, in bytes.
    pub fn usage(&self, name: &str) -> u64 {
        fn walk(p: &Path, depth: usize) -> u64 {
            if depth > 12 {
                return 0;
            }
            let Ok(rd) = std::fs::read_dir(p) else { return 0 };
            rd.flatten()
                .map(|e| match e.file_type() {
                    Ok(t) if t.is_dir() => walk(&e.path(), depth + 1),
                    Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
                    _ => 0,
                })
                .sum()
        }
        walk(&self.dir(name), 0)
    }
}

/// The public keys GitHub publishes for a login, as `authorized_keys` text.
pub async fn github_keys(net: &alouette_net::Net, login: &str) -> Result<String, String> {
    if login.is_empty() || !login.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') || login.len() > 39 {
        return Err(format!("`{login}` is not a GitHub login"));
    }
    let resp = net
        .get(&format!("https://github.com/{login}.keys"), &[], 256 * 1024)
        .await
        .map_err(|e| format!("could not reach GitHub: {e}"))?;
    if !resp.ok() {
        return Err(format!("GitHub answered {} for {login}", resp.status));
    }
    let text = resp.text();
    if parse_keys(&text).is_empty() {
        return Err(format!("{login} has no public SSH keys on GitHub"));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl ann@laptop";

    #[test]
    fn names() {
        assert!(valid_name("ann"));
        assert!(valid_name("bo-2_x"));
        assert!(!valid_name("Ann"));
        assert!(!valid_name("../x"));
        assert!(!valid_name("guest"));
        assert!(!valid_name("1ann"));
        assert!(!valid_name(""));
    }

    #[test]
    fn add_and_check() {
        let dir = std::env::temp_dir().join(format!("alouette-users-{}", std::process::id()));
        let u = Users::new(&dir);
        assert_eq!(u.add("ann", &format!("# mine\n{KEY}\n")).unwrap(), 1);
        assert_eq!(u.add("ann", KEY).unwrap(), 0);
        assert!(u.add("ann", "nonsense").is_err());
        assert_eq!(u.list(), vec!["ann"]);
        let k = parse_keys(KEY).remove(0);
        assert!(u.allows("ann", &k));
        assert!(!u.allows("bo", &k));
        let _ = std::fs::remove_dir_all(dir);
    }
}
